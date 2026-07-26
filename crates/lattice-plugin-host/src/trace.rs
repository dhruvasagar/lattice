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
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::PluginSeam;

/// Trace verbosity. Ordered least→most verbose; a record at `record.level` is
/// kept iff `record.level <= gate`. Records always carry `Error..=Trace`; `Off`
/// is a *gate-only* value (a gate of `Off` keeps nothing).
///
/// `#[repr(u8)]` with explicit discriminants so the value round-trips through the
/// hot-path [`HotGate`] atomic ([`as_u8`](Self::as_u8) / [`from_u8`](Self::from_u8))
/// without changing the derived `Ord` (declaration order == numeric order).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum TraceLevel {
    /// Gate-only: silence this plugin entirely.
    Off = 0,
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl TraceLevel {
    /// The lowercase wire / display label (`off` / `error` / … / `trace`).
    pub fn as_str(self) -> &'static str {
        match self {
            TraceLevel::Off => "off",
            TraceLevel::Error => "error",
            TraceLevel::Warn => "warn",
            TraceLevel::Info => "info",
            TraceLevel::Debug => "debug",
            TraceLevel::Trace => "trace",
        }
    }

    /// The six levels least→most verbose — the canonical order for enumeration
    /// (`:set` completion, the customize view) and cycling.
    pub const ALL: [TraceLevel; 6] = [
        TraceLevel::Off,
        TraceLevel::Error,
        TraceLevel::Warn,
        TraceLevel::Info,
        TraceLevel::Debug,
        TraceLevel::Trace,
    ];

    /// The next level in [`ALL`](Self::ALL) order, wrapping `Trace → Off` — the
    /// manager view's per-plugin cycle chord (PO.4.3).
    pub fn cycle_next(self) -> TraceLevel {
        TraceLevel::from_u8((self.as_u8() + 1) % Self::ALL.len() as u8)
    }

    /// Parse a lowercase label (inverse of [`as_str`](Self::as_str)), `None` for
    /// an unknown word. The loader uses this to bridge a `plugin.trace-level`
    /// `OptionChanged` (whose value arrives as a string) into a level (PO.4.3).
    pub fn parse(s: &str) -> Option<TraceLevel> {
        TraceLevel::ALL.into_iter().find(|l| l.as_str() == s)
    }

    /// The numeric encoding stored in the [`HotGate`] atomic.
    #[inline]
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// Decode a [`HotGate`] atomic byte. An out-of-range byte (never written by
    /// this crate) decodes to `Off` — observability fails closed, never panics.
    #[inline]
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => TraceLevel::Error,
            2 => TraceLevel::Warn,
            3 => TraceLevel::Info,
            4 => TraceLevel::Debug,
            5 => TraceLevel::Trace,
            _ => TraceLevel::Off,
        }
    }
}

/// The hot-path verbosity gate — a per-plugin atomic the tracer publishes and the
/// sync grammar trampoline reads once per guest call (design §4: "a single
/// relaxed-atomic per-plugin gate load and a predicted-not-taken branch when
/// off"). The `Arc<AtomicU8>` is owned by the [`PluginTracer`] (its single source
/// of truth for verbosity) and cloned into the trampoline; a `:set
/// plugin.trace-level` write updates the atomic, so the very next keystroke reads
/// the new level with no lock and no cross-plugin cost.
#[derive(Clone)]
pub struct HotGate {
    level: Arc<AtomicU8>,
}

impl HotGate {
    /// The current effective gate level — the relaxed hot-path load.
    #[inline]
    pub fn level(&self) -> TraceLevel {
        TraceLevel::from_u8(self.level.load(Ordering::Relaxed))
    }

    /// Will a per-call success record (emitted at `Debug`) be kept? The hot-path
    /// predicate: `false` at the default `Info` gate, so the trampoline does zero
    /// timing / allocation / formatting on the common keystroke.
    #[inline]
    pub fn records_calls(&self) -> bool {
        self.level() >= TraceLevel::Debug
    }

    /// A permanently-off gate for the no-tracer paths (tests / benches) — reads as
    /// `Off`, so `records_calls()` is always `false`.
    pub fn disabled() -> Self {
        Self {
            level: Arc::new(AtomicU8::new(TraceLevel::Off.as_u8())),
        }
    }

    /// Overwrite the published level (the tracer calls this on a verbosity change).
    #[inline]
    fn store(&self, level: TraceLevel) {
        self.level.store(level.as_u8(), Ordering::Relaxed);
    }
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
    /// Published per-plugin hot-path gates (PO.3) — the atomic mirror of the
    /// effective level (`plugin_levels` override, else `default_level`) that the
    /// sync grammar trampoline reads without locking. Kept in sync by
    /// [`PluginTracer::set_plugin_level`] / [`set_default_level`].
    gates: Mutex<HashMap<u32, HotGate>>,
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
                gates: Mutex::new(HashMap::new()),
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

    /// Set the default verbosity gate (`:set plugin.trace-level=…`). Republishes
    /// the new level to every hot-path gate whose plugin has no override, so the
    /// grammar trampoline picks it up on the next keystroke.
    pub fn set_default_level(&self, level: TraceLevel) {
        *lock(&self.state.default_level) = level;
        let overrides = lock(&self.state.plugin_levels);
        for (plugin, gate) in lock(&self.state.gates).iter() {
            if !overrides.contains_key(plugin) {
                gate.store(level);
            }
        }
    }

    /// Override one plugin's verbosity gate (a per-plugin `:set` / manager-view
    /// toggle). The hot-path seam reads this, so raising one plugin's verbosity
    /// never touches another's cost. Republishes to that plugin's hot-path gate.
    pub fn set_plugin_level(&self, plugin: u32, level: TraceLevel) {
        lock(&self.state.plugin_levels).insert(plugin, level);
        if let Some(gate) = lock(&self.state.gates).get(&plugin) {
            gate.store(level);
        }
    }

    /// The published hot-path gate for `plugin` — created (seeded to the plugin's
    /// current effective level) on first request and cached, so repeated calls
    /// return clones of the same atomic. The sync grammar trampoline holds one and
    /// reads it once per guest call (design §4).
    pub fn hot_gate(&self, plugin: u32) -> HotGate {
        let effective = self.plugin_level(plugin);
        lock(&self.state.gates)
            .entry(plugin)
            .or_insert_with(|| HotGate {
                level: Arc::new(AtomicU8::new(effective.as_u8())),
            })
            .clone()
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
        push_bounded(
            &mut lock(&self.state.global),
            record.clone(),
            self.state.capacity,
        );
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
        lock(&self.state.gates).remove(&plugin);
    }
}

/// Push `record`, evicting the oldest if at `capacity` (a bounded ring).
fn push_bounded(
    ring: &mut VecDeque<PluginTraceRecord>,
    record: PluginTraceRecord,
    capacity: usize,
) {
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
    fn trace_level_round_trips_through_the_gate_byte() {
        for lvl in [
            TraceLevel::Off,
            TraceLevel::Error,
            TraceLevel::Warn,
            TraceLevel::Info,
            TraceLevel::Debug,
            TraceLevel::Trace,
        ] {
            assert_eq!(TraceLevel::from_u8(lvl.as_u8()), lvl);
        }
        // An out-of-range byte fails closed to `Off`.
        assert_eq!(TraceLevel::from_u8(200), TraceLevel::Off);
    }

    #[test]
    fn cycle_next_walks_all_and_wraps() {
        let mut seen = Vec::new();
        let mut lvl = TraceLevel::Off;
        for _ in 0..TraceLevel::ALL.len() {
            seen.push(lvl);
            lvl = lvl.cycle_next();
        }
        assert_eq!(
            seen,
            TraceLevel::ALL.to_vec(),
            "cycle visits every level in order"
        );
        assert_eq!(lvl, TraceLevel::Off, "Trace wraps back to Off");
        assert_eq!(TraceLevel::Info.as_str(), "info");
    }

    #[test]
    fn parse_is_the_inverse_of_as_str() {
        for lvl in TraceLevel::ALL {
            assert_eq!(TraceLevel::parse(lvl.as_str()), Some(lvl));
        }
        assert_eq!(TraceLevel::parse("loud"), None);
    }

    #[test]
    fn a_hot_gate_seeds_to_the_effective_level() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        // No override → the default.
        assert_eq!(t.hot_gate(1).level(), TraceLevel::Info);
        // Info gate does not admit per-call Debug records.
        assert!(!t.hot_gate(1).records_calls());
        // A plugin raised before its gate is created seeds raised.
        t.set_plugin_level(2, TraceLevel::Trace);
        let g2 = t.hot_gate(2);
        assert_eq!(g2.level(), TraceLevel::Trace);
        assert!(g2.records_calls());
    }

    #[test]
    fn setting_a_plugin_level_republishes_to_its_live_gate() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        let gate = t.hot_gate(1);
        assert!(!gate.records_calls(), "starts off at the Info default");
        t.set_plugin_level(1, TraceLevel::Debug);
        assert!(
            gate.records_calls(),
            "the already-handed-out gate sees the raise"
        );
        t.set_plugin_level(1, TraceLevel::Off);
        assert_eq!(gate.level(), TraceLevel::Off, "and the lowering");
    }

    #[test]
    fn raising_the_default_republishes_only_to_unoverridden_gates() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        let g1 = t.hot_gate(1); // no override — tracks the default
        let g2 = t.hot_gate(2);
        t.set_plugin_level(2, TraceLevel::Off); // g2 pinned Off
        t.set_default_level(TraceLevel::Trace);
        assert_eq!(
            g1.level(),
            TraceLevel::Trace,
            "the unoverridden gate follows the default"
        );
        assert_eq!(
            g2.level(),
            TraceLevel::Off,
            "the overridden gate is untouched"
        );
    }

    #[test]
    fn hot_gate_is_cached_so_clones_share_the_atomic() {
        let t = PluginTracer::new(TraceLevel::Info, 8);
        let a = t.hot_gate(1);
        let b = t.hot_gate(1);
        t.set_plugin_level(1, TraceLevel::Trace);
        assert!(a.records_calls());
        assert!(b.records_calls(), "the second handle sees the same store");
    }

    #[test]
    fn a_disabled_gate_never_records() {
        let g = HotGate::disabled();
        assert_eq!(g.level(), TraceLevel::Off);
        assert!(!g.records_calls());
    }

    #[test]
    fn a_poisoned_publisher_mutex_still_records_and_never_panics() {
        // Observability must never crash the editor: a panic while a trace guard is
        // held poisons the mutex, and the next tracer call must recover the guard
        // (lock() → into_inner) rather than re-panic. Drive it via a panicking
        // publisher (the closure runs while the publisher mutex is held).
        let t = PluginTracer::new(TraceLevel::Info, 8);
        t.set_event_publisher(Box::new(|_| panic!("boom in the publisher closure")));
        // The push to the rings happens BEFORE the publisher fires, so record #1
        // lands even though the publisher then panics + poisons its mutex.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            t.trace(rec(1, TraceLevel::Info));
        }));
        assert!(poisoned.is_err(), "the panicking publisher unwound");
        assert_eq!(
            t.snapshot_plugin(1).len(),
            1,
            "record #1 landed pre-publish"
        );
        // Replace the publisher (locks the POISONED publisher mutex — must recover),
        // then trace again (locks it again to publish). Neither may panic.
        t.set_event_publisher(Box::new(|_| {}));
        t.trace(rec(1, TraceLevel::Info));
        assert_eq!(
            t.snapshot_plugin(1).len(),
            2,
            "the tracer recovered the poisoned mutex and kept recording"
        );
    }

    #[test]
    fn forget_plugin_drops_its_ring_and_override() {
        let t = PluginTracer::new(TraceLevel::Trace, 8);
        t.set_plugin_level(1, TraceLevel::Trace);
        t.trace(rec(1, TraceLevel::Trace));
        assert_eq!(t.snapshot_plugin(1).len(), 1);
        // A live gate override reverts to the default once the plugin is forgotten.
        let gate = t.hot_gate(1);
        assert_eq!(gate.level(), TraceLevel::Trace);
        t.forget_plugin(1);
        assert!(t.snapshot_plugin(1).is_empty());
        // The global ring keeps history.
        assert_eq!(t.snapshot_global().len(), 1);
        // The old gate handle is orphaned; a fresh gate seeds from the default.
        assert_eq!(
            t.hot_gate(1).level(),
            TraceLevel::Trace,
            "default is Trace here"
        );
    }
}
