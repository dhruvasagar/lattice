//! PO.1 — the plugin **boundary-trace** substrate (Layer 1 of the observability
//! stack, design fragment `docs/dev/architecture/plugin-observability.md`).
//!
//! The host owns the whole Component-Model boundary, so it can record every
//! host↔guest call **independent of the guest's source language**. This module
//! is the sink for those records: a per-plugin ring + a boot-wired event
//! publisher, mirroring the LSP logging substrate (`LspLogger` / `LspLogPushed`)
//! that already proves the ring + publisher + off-thread-drain shape.
//!
//! **This slice is the pipe only** — no seam is instrumented yet (PO.2 / PO.3).
//! Emission stays OFF the editor hot path by contract (design §4): `trace` is a
//! cheap gate + push + publish; all formatting + buffer append happen off-thread
//! on the drain side (PO.4), never on the UI/actor thread.

use std::collections::HashMap;
use std::collections::VecDeque;
use std::sync::Mutex;

use crate::PluginSeam;

/// Trace verbosity. Ordered least→most verbose; a record at `record.level` is
/// kept iff `record.level <= gate`. Records always carry `Error..=Trace`; `Off`
/// is a *gate-only* value (a gate of `Off` keeps nothing).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum TraceLevel {
    /// Gate-only: silence this plugin entirely.
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Which way a boundary call crossed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Direction {
    /// The guest called a host import (e.g. `wasi:logging`, a host-service).
    HostImport,
    /// The host called a guest export (e.g. `apply-motion`, `on-event`).
    GuestExport,
}

/// How a boundary call ended.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TraceOutcome {
    /// Completed: wall-clock `micros` and the `fuel_delta` it consumed.
    Ok { micros: u64, fuel_delta: u64 },
    /// Trapped: the stable trap `kind` (`"fuel"` / `"epoch"` / `"trap"`) and the
    /// `func` that trapped — the `Event::PluginCrashed` provenance shape.
    Trap { kind: String, func: String },
    /// A capability the call needed was withheld by the trust tier.
    Denied { capability: String },
}

/// One boundary event. Structured (not a pre-formatted line) so the view owns
/// presentation and filters key on fields. `plugin` is the host-issued id (the
/// same id `SourceLayer::Plugin` / `Event::PluginCrashed.plugin` carry), so
/// Layer-0 crash rows and Layer-1 trace rows join cleanly.
#[derive(Clone, Debug)]
pub struct PluginTraceRecord {
    pub plugin: u32,
    pub seam: PluginSeam,
    pub direction: Direction,
    /// The WIT function name (e.g. `"apply-motion"`).
    pub call: std::borrow::Cow<'static, str>,
    pub level: TraceLevel,
    pub outcome: TraceOutcome,
    /// Arg / result summary, populated only above a verbosity threshold (kept
    /// `None` on the hot path when tracing is off — design §4).
    pub detail: Option<String>,
}

/// Fired when [`PluginTracer::trace`] appends a record. A `register_event!`
/// typed event streamed via `publish_typed`; the trace-buffer views (PO.4)
/// subscribe and drain it off-thread, the `LspLogPushed` precedent.
#[derive(Clone, Debug)]
pub struct PluginTracePushed {
    pub record: PluginTraceRecord,
}

lattice_protocol::register_event!(
    PluginTracePushed,
    "plugin.trace-pushed",
    "Fired when the PluginTracer appends a boundary-trace record.",
    "lattice-plugin-host",
);

/// The publisher fired on every successful append — wired at boot to
/// `EventBus::publish_typed`. `None` (test paths / pre-wire) → no events, the
/// ring still fills.
type TracePublisher = Box<dyn Fn(PluginTraceRecord) + Send + Sync>;

/// Shared handle alias (ServiceRegistry Arc/TypeId rule: register **and** look
/// up as `PluginTracerHandle`).
pub type PluginTracerHandle = std::sync::Arc<PluginTracer>;

struct TracerState {
    /// Subsystem-wide ring (every plugin, interleaved) → `*plugin-trace*`.
    global: Mutex<VecDeque<PluginTraceRecord>>,
    /// Per-plugin rings → `*plugin-trace:<name>*`.
    per_plugin: Mutex<HashMap<u32, VecDeque<PluginTraceRecord>>>,
    /// Default verbosity gate (records above it are dropped).
    default_level: Mutex<TraceLevel>,
    /// Per-plugin gate overrides; falls back to `default_level`.
    plugin_levels: Mutex<HashMap<u32, TraceLevel>>,
    /// Ring capacity (records; bounded so memory stays flat).
    capacity: usize,
    publisher: Mutex<Option<TracePublisher>>,
}

/// The boundary-trace sink — the `LspLogger` analogue. Cheap to `trace` into
/// (gate → push → publish); everything expensive is deferred to the off-thread
/// drain.
pub struct PluginTracer {
    state: std::sync::Arc<TracerState>,
}

impl PluginTracer {
    /// Construct a tracer with `default_level` and a per-ring `capacity`.
    pub fn new(default_level: TraceLevel, capacity: usize) -> Self {
        Self {
            state: std::sync::Arc::new(TracerState {
                global: Mutex::new(VecDeque::with_capacity(capacity.min(1024))),
                per_plugin: Mutex::new(HashMap::new()),
                default_level: Mutex::new(default_level),
                plugin_levels: Mutex::new(HashMap::new()),
                capacity,
                publisher: Mutex::new(None),
            }),
        }
    }

    /// Sensible defaults: `Info` (no per-call trace by default — design §7),
    /// 10k records / ring.
    pub fn with_defaults() -> Self {
        Self::new(TraceLevel::Info, 10_000)
    }

    /// Install / replace the event publisher (wired at boot to the runtime bus).
    pub fn set_event_publisher(&self, publisher: TracePublisher) {
        *lock(&self.state.publisher) = Some(publisher);
    }

    /// Set the default verbosity gate (`:set plugin.trace-level=…`).
    pub fn set_default_level(&self, level: TraceLevel) {
        *lock(&self.state.default_level) = level;
    }

    /// Override one plugin's verbosity gate (a per-plugin `:set` / manager-view
    /// toggle). The hot-path seam reads this, so raising one plugin's verbosity
    /// never touches another's cost.
    pub fn set_plugin_level(&self, plugin: u32, level: TraceLevel) {
        lock(&self.state.plugin_levels).insert(plugin, level);
    }

    /// The effective gate for `plugin` — its override, else the default.
    pub fn plugin_level(&self, plugin: u32) -> TraceLevel {
        lock(&self.state.plugin_levels)
            .get(&plugin)
            .copied()
            .unwrap_or_else(|| *lock(&self.state.default_level))
    }

    /// Append a record: gate by the plugin's effective level, push to the
    /// per-plugin + global rings (bounded), then fire the publisher. A record
    /// above the gate is dropped before any ring work.
    pub fn trace(&self, record: PluginTraceRecord) {
        if record.level > self.plugin_level(record.plugin) {
            return;
        }
        push_bounded(
            lock(&self.state.per_plugin)
                .entry(record.plugin)
                .or_insert_with(|| VecDeque::with_capacity(self.state.capacity.min(1024))),
            record.clone(),
            self.state.capacity,
        );
        push_bounded(&mut lock(&self.state.global), record.clone(), self.state.capacity);
        if let Some(publisher) = lock(&self.state.publisher).as_ref() {
            publisher(record);
        }
    }

    /// Snapshot one plugin's ring (cheap clone), newest last — the seed for a
    /// per-plugin trace view.
    pub fn snapshot_plugin(&self, plugin: u32) -> Vec<PluginTraceRecord> {
        lock(&self.state.per_plugin)
            .get(&plugin)
            .map(|ring| ring.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Snapshot the subsystem-wide ring — the seed for the shared trace view.
    pub fn snapshot_global(&self) -> Vec<PluginTraceRecord> {
        lock(&self.state.global).iter().cloned().collect()
    }

    /// Drop a plugin's ring + gate override (on unload — reclaim its trace
    /// memory). The global ring keeps its historical records.
    pub fn forget_plugin(&self, plugin: u32) {
        lock(&self.state.per_plugin).remove(&plugin);
        lock(&self.state.plugin_levels).remove(&plugin);
    }
}

/// Push `record`, evicting the oldest if at `capacity` (a bounded ring).
fn push_bounded(ring: &mut VecDeque<PluginTraceRecord>, record: PluginTraceRecord, capacity: usize) {
    if ring.len() >= capacity {
        ring.pop_front();
    }
    ring.push_back(record);
}

/// Lock helper — a poisoned trace mutex is non-fatal (observability must never
/// crash the editor), so recover the guard rather than propagate the panic.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use super::*;

    fn rec(plugin: u32, level: TraceLevel) -> PluginTraceRecord {
        PluginTraceRecord {
            plugin,
            seam: PluginSeam::Grammar,
            direction: Direction::GuestExport,
            call: "apply-motion".into(),
            level,
            outcome: TraceOutcome::Ok {
                micros: 12,
                fuel_delta: 100,
            },
            detail: None,
        }
    }

    #[test]
    fn a_record_at_or_below_the_gate_is_kept() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        t.trace(rec(1, TraceLevel::Error)); // <= Info
        t.trace(rec(1, TraceLevel::Info)); // == Info
        assert_eq!(t.snapshot_plugin(1).len(), 2);
        assert_eq!(t.snapshot_global().len(), 2);
    }

    #[test]
    fn a_record_above_the_gate_is_dropped() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        t.trace(rec(1, TraceLevel::Debug)); // > Info → dropped
        t.trace(rec(1, TraceLevel::Trace)); // > Info → dropped
        assert!(t.snapshot_plugin(1).is_empty());
        assert!(t.snapshot_global().is_empty());
    }

    #[test]
    fn a_per_plugin_override_raises_verbosity_for_only_that_plugin() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        t.set_plugin_level(1, TraceLevel::Trace);
        t.trace(rec(1, TraceLevel::Debug)); // kept — plugin 1 raised to Trace
        t.trace(rec(2, TraceLevel::Debug)); // dropped — plugin 2 still at Info default
        assert_eq!(t.snapshot_plugin(1).len(), 1);
        assert!(t.snapshot_plugin(2).is_empty());
    }

    #[test]
    fn off_gate_silences_everything() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        t.set_plugin_level(1, TraceLevel::Off);
        t.trace(rec(1, TraceLevel::Error)); // even an error is dropped when Off
        assert!(t.snapshot_plugin(1).is_empty());
    }

    #[test]
    fn rings_are_bounded_evicting_oldest() {
        let t = PluginTracer::new(TraceLevel::Trace, 3);
        for _ in 0..5 {
            t.trace(rec(1, TraceLevel::Info));
        }
        assert_eq!(t.snapshot_plugin(1).len(), 3, "per-plugin ring capped at 3");
        assert_eq!(t.snapshot_global().len(), 3, "global ring capped at 3");
    }

    #[test]
    fn publish_fires_on_every_kept_append() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = count.clone();
        t.set_event_publisher(Box::new(move |_rec| {
            c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }));
        t.trace(rec(1, TraceLevel::Info)); // kept → publishes
        t.trace(rec(1, TraceLevel::Debug)); // dropped → no publish
        assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn forget_plugin_drops_its_ring_and_override() {
        let t = PluginTracer::new(TraceLevel::Trace, 8);
        t.set_plugin_level(1, TraceLevel::Trace);
        t.trace(rec(1, TraceLevel::Trace));
        assert_eq!(t.snapshot_plugin(1).len(), 1);
        t.forget_plugin(1);
        assert!(t.snapshot_plugin(1).is_empty());
        // The global ring keeps history.
        assert_eq!(t.snapshot_global().len(), 1);
    }
}
