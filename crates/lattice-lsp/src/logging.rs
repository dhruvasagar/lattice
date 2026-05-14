//! LSP logging facade -- the producer side of the
//! `*lsp*` / `*lsp:<server>*` / `*lsp:<server>:trace*`
//! buffer views (Phase 4.1.f).
//!
//! ## Inspiration
//!
//! emacs's `lsp-mode` puts subsystem events in `*lsp-log*` and
//! per-server stderr in `*<server> stderr*`. eglot's
//! `*EGLOT (project/lang) events*` captures the JSON-RPC trace
//! when toggled. lattice combines both ideas under
//! everything-is-a-buffer (§5.9):
//!
//! - **Subsystem buffer** (`*lsp*`) -- supervisor events
//!   (spawn / handshake / crash / restart) plus cross-server
//!   messages.
//! - **Per-server buffer** (`*lsp:<server-id>*`) -- stderr +
//!   `window/logMessage` + `window/showMessage` + lifecycle
//!   for one server.
//! - **Per-server trace** (`*lsp:<server-id>:trace*`) -- every
//!   inbound / outbound JSON-RPC message. Off by default;
//!   toggle with `:lsp-trace <server>`.
//!
//! ## Producer / consumer split
//!
//! This module is the **producer**: actors and the supervisor
//! emit `LogRecord`s through [`LspLogger::log`]. Records land
//! in bounded `LogRing`s held inside the logger.
//!
//! The **consumer** -- buffer-backed log views in
//! `lattice-ui-tui` -- snapshots a ring on demand via
//! [`LspLogger::snapshot_global`] /
//! [`LspLogger::snapshot_server`] and renders the records.
//! Auto-scroll-to-tail and live tail-follow are buffer-side
//! concerns; the producer just appends.
//!
//! ## Tracing crate fan-out
//!
//! Every [`LspLogger::log`] call also emits a `tracing::*` event
//! at the matching level so users who prefer
//! `RUST_LOG=lattice_lsp=debug ./lattice` still see
//! everything. The two paths are independent: the in-memory
//! rings survive even when no `tracing` subscriber is
//! installed, and `tracing` users see the same events whether
//! or not the buffer views are open.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use std::sync::Mutex;

use crate::events::LspLogPushed;

/// Composite key for the per-instance log ring -- the
/// `(server_id, workspace_root)` pair that the supervisor uses
/// to key actors. Two `rust-analyzer` processes against
/// different workspaces stay distinct in the logger and in
/// the buffer name. Cheap to clone (two `Arc`s).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct InstanceKey {
    pub server_id: Arc<str>,
    pub workspace: Arc<Path>,
}

impl InstanceKey {
    /// Construct an instance key from owned server id + workspace
    /// path. Accepts anything that converts into `Arc<str>` /
    /// `Arc<Path>` so callers don't need to pre-Arc.
    pub fn new(server_id: impl Into<Arc<str>>, workspace: impl Into<Arc<Path>>) -> Self {
        Self {
            server_id: server_id.into(),
            workspace: workspace.into(),
        }
    }
}

/// Format one log record line for append to a synthetic LSP log
/// buffer (B'.3: hoisted out of `lattice_ui_tui::app::lsp_log_buffers`
/// so log modes in this crate can reuse it from a tokio task).
/// Shape: `HH:MM:SS.mmm [<server>] <level> <source>: <message>`.
/// Trailing newline is the caller's responsibility (the drain
/// batches many records into one buffer-append).
pub fn format_log_event_line(
    server_id: Option<&str>,
    level: &str,
    source: &str,
    message: &str,
) -> String {
    let elapsed = SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok();
    let secs = elapsed.map(|d| d.as_secs()).unwrap_or(0);
    let ms = elapsed.map(|d| d.subsec_millis()).unwrap_or(0);
    let hh = (secs / 3600) % 24;
    let mm = (secs / 60) % 60;
    let ss = secs % 60;
    let prefix = server_id.map(|id| format!("[{id}] ")).unwrap_or_default();
    let msg = one_line(message);
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03} {prefix}{level} {source:>6}: {msg}")
}

/// Collapse newlines / carriage returns / tabs into spaces so the
/// formatted record fits on one buffer line.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
}

/// Closure invoked on every successful append. Wired by the App
/// (or test harness) to publish [`LspLogPushed`] onto the runtime
/// event bus via `EventBus::publish_typed`, which lets log
/// buffers refresh live as records arrive. Optional:
/// `LspLogger::with_defaults()` starts with no publisher;
/// `set_event_publisher` installs one.
pub type LogEventPublisher = Arc<dyn Fn(LspLogPushed) + Send + Sync>;

/// Compact severity tag for [`LspLogPushed`]. Mirrors
/// [`LogSource::tag`]'s shape for the level discriminator.
pub fn level_tag(l: LogLevel) -> &'static str {
    match l {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    }
}

/// Internal helper: lock with consistent expect-message. Logger
/// mutexes are never held across `.await` and never panic in
/// the critical section, so `PoisonError` is unreachable in
/// safe usage.
fn lock<'a, T>(m: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
    m.lock().expect("LspLogger mutex poisoned")
}

/// Severity levels. Ordered low-to-high so the derived
/// `PartialOrd` matches "Error > Warn > ... > Trace". Per-server
/// min level filters records BELOW it (i.e. `level < min`
/// drops; `level >= min` keeps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    /// Wire trace -- per-message in/out at the codec boundary.
    /// Only emitted when trace mode is on for the relevant
    /// server.
    Trace,
    /// Per-message, non-trace detail (mailbox commands, debounce
    /// flushes, capability gating decisions).
    Debug,
    /// Lifecycle milestones (handshake done, server attached, ...).
    Info,
    /// Recoverable problems / unexpected protocol behaviour.
    Warn,
    /// Unrecoverable failures.
    Error,
}

impl LogLevel {
    /// Parse a string like `"info"` / `"debug"` (case-insensitive).
    /// Used by config loaders.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Some(LogLevel::Error),
            "warn" | "warning" => Some(LogLevel::Warn),
            "info" => Some(LogLevel::Info),
            "debug" => Some(LogLevel::Debug),
            "trace" => Some(LogLevel::Trace),
            _ => None,
        }
    }

    /// Iterate over all levels, low to high.
    pub fn all() -> &'static [LogLevel] {
        &[
            LogLevel::Trace,
            LogLevel::Debug,
            LogLevel::Info,
            LogLevel::Warn,
            LogLevel::Error,
        ]
    }

    /// Single-letter compact form for log rendering.
    pub fn short(self) -> char {
        match self {
            LogLevel::Error => 'E',
            LogLevel::Warn => 'W',
            LogLevel::Info => 'I',
            LogLevel::Debug => 'D',
            LogLevel::Trace => 'T',
        }
    }
}

/// Where a log record originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogSource {
    /// LSP `telemetry/event` notification (spec §3.16). Routed
    /// here in 4.4.a so plugin subscribers can filter on
    /// `source == "telemetry"` rather than parsing free-form
    /// log-message text. Payload still goes through the
    /// shared `LspLogPushed.message` String -- plugin code
    /// that needs structured JSON can parse the suffix.
    Telemetry,
    /// Lattice-side (handshake start / done, supervisor restart,
    /// shutdown sequence, decode failures, capability gating).
    Client,
    /// One line from the server's stderr stream.
    Stderr,
    /// `window/logMessage` notification.
    LspMessage,
    /// `window/showMessage` notification (also surfaced to
    /// the editor's notification slot).
    LspShowMessage,
    /// JSON-RPC wire trace -- one inbound or outbound message.
    Trace,
}

impl LogSource {
    /// Compact tag used in log rendering.
    pub fn tag(self) -> &'static str {
        match self {
            LogSource::Client => "client",
            LogSource::Stderr => "stderr",
            LogSource::LspMessage => "log",
            LogSource::LspShowMessage => "show",
            LogSource::Trace => "trace",
            LogSource::Telemetry => "telemetry",
        }
    }
}

/// One log entry. Cheap to clone (`Arc<str>` for server id,
/// `Arc<Path>` for workspace; the message is a plain `String`).
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Wall-clock time of emission. Used for buffer rendering;
    /// not load-bearing for ordering (records preserve insertion
    /// order in their ring).
    pub timestamp: SystemTime,
    /// `None` for subsystem-wide records (supervisor events).
    /// Pre-B'.2 records may have `server_id` without `workspace`;
    /// post-B'.2 the two travel together (both `Some` => per-instance,
    /// both `None` => subsystem-wide). Mixed states route to global.
    pub server_id: Option<Arc<str>>,
    /// `None` for subsystem-wide records. The workspace root the
    /// `(server_id, workspace)` actor was spawned against. Two
    /// `rust-analyzer` instances on different workspaces stay
    /// distinct via this field.
    pub workspace: Option<Arc<Path>>,
    pub level: LogLevel,
    pub source: LogSource,
    pub message: String,
}

/// Bounded ring of log records. Append-only; oldest is evicted
/// when capacity is reached.
#[derive(Debug)]
pub struct LogRing {
    buf: VecDeque<LogRecord>,
    capacity: usize,
}

impl LogRing {
    /// Construct with capacity. `capacity` of 0 produces a ring
    /// that drops everything (useful for "logging disabled").
    pub fn new(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity.min(1024)),
            capacity,
        }
    }

    /// Append a record, evicting the oldest if at capacity.
    pub fn push(&mut self, record: LogRecord) {
        if self.capacity == 0 {
            return;
        }
        while self.buf.len() >= self.capacity {
            self.buf.pop_front();
        }
        self.buf.push_back(record);
    }

    /// Number of records currently stored.
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Snapshot every record. Cheap clone (each `LogRecord`'s
    /// heavy field is the message `String`; we're optimising
    /// for correctness here, not for log-view paint cost).
    pub fn snapshot(&self) -> Vec<LogRecord> {
        self.buf.iter().cloned().collect()
    }

    /// Drop everything. Used by `:lsp-log clear` (4.1.g).
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Capacity setter. Used when the user adjusts
    /// `lsp.log_capacity` at runtime.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        while self.buf.len() > self.capacity {
            self.buf.pop_front();
        }
    }
}

/// Producer-side log facade. One per LSP subsystem; each actor
/// gets a clone (cheap -- internal state is `Arc<Mutex<...>>`).
///
/// Cheap-by-design: append is O(1) amortized, level gating is
/// one HashMap lookup, trace gating is one HashSet lookup.
/// Logging is **not** a hot path -- per-keystroke cost is the
/// `didChange` encode + send, not the log emission.
#[derive(Clone)]
pub struct LspLogger {
    state: Arc<LoggerState>,
}

struct LoggerState {
    /// Subsystem-wide ring (records where both server_id and
    /// workspace are None).
    global: Mutex<LogRing>,
    /// Per-instance rings, keyed by `(server_id, workspace)`.
    /// One `rust-analyzer` against `/path/A` is a different ring
    /// than `rust-analyzer` against `/path/B`.
    per_instance: Mutex<HashMap<InstanceKey, LogRing>>,
    /// Optional publisher fired on every successful append.
    /// Wired by the App at boot to feed the runtime event bus.
    /// `None` -> no events emitted (test paths, or pre-wire).
    event_publisher: Mutex<Option<LogEventPublisher>>,
    /// Default capacity for new per-instance rings.
    default_capacity: Mutex<usize>,
    /// Default min level (records below are dropped).
    default_min_level: Mutex<LogLevel>,
    /// Per-instance overrides for min level. Falls back to the
    /// default when absent.
    instance_levels: Mutex<HashMap<InstanceKey, LogLevel>>,
    /// Per-instance trace toggle. When absent / false, Trace-level
    /// records for that instance are dropped before the ring
    /// lookup.
    instance_trace: Mutex<HashSet<InstanceKey>>,
}

impl LspLogger {
    /// Construct a logger with the given default min level and
    /// default per-server ring capacity (e.g. `LogLevel::Info`,
    /// 10_000). Conservative defaults: Info filters out trace
    /// noise; 10k records ≈ a few MB of memory at most.
    pub fn new(default_min_level: LogLevel, default_capacity: usize) -> Self {
        Self {
            state: Arc::new(LoggerState {
                global: Mutex::new(LogRing::new(default_capacity)),
                per_instance: Mutex::new(HashMap::new()),
                default_capacity: Mutex::new(default_capacity),
                default_min_level: Mutex::new(default_min_level),
                instance_levels: Mutex::new(HashMap::new()),
                instance_trace: Mutex::new(HashSet::new()),
                event_publisher: Mutex::new(None),
            }),
        }
    }

    /// Install / replace the event publisher. Subsequent `log`
    /// calls fire the closure with [`LspLogPushed`] after the
    /// record lands in its ring. The App wires this at boot so
    /// the runtime event bus sees every append; subscribers
    /// (live log views) drain the bus on tick.
    pub fn set_event_publisher(&self, publisher: LogEventPublisher) {
        *lock(&self.state.event_publisher) = Some(publisher);
    }

    /// Sensible defaults: Info level, 10k records / ring.
    pub fn with_defaults() -> Self {
        Self::new(LogLevel::Info, 10_000)
    }

    /// Append a record. Gated by per-instance min level (or the
    /// default). Records with `instance = None` route to the
    /// subsystem-wide ring (`*lsp*`) and use the default min level.
    /// Trace-level records are additionally gated by the
    /// per-instance trace toggle.
    pub fn log(
        &self,
        instance: Option<&InstanceKey>,
        level: LogLevel,
        source: LogSource,
        message: impl Into<String>,
    ) {
        // Trace gating: per-instance trace toggle decides whether
        // Trace records reach a ring at all. When the toggle is
        // ON, Trace records bypass the per-instance min-level
        // filter (the user opted in deliberately; the default
        // Info filter would otherwise drop them on the floor).
        // When the toggle is OFF, Trace records for that instance
        // are dropped here and the level filter never sees them.
        let trace_bypass = if level == LogLevel::Trace {
            match instance {
                Some(key) if self.is_tracing(key) => true,
                Some(_) => return, // Trace, toggle off -> drop.
                None => false,     // Subsystem-wide trace honours level filter.
            }
        } else {
            false
        };

        if !trace_bypass {
            let min = self.effective_min_level(instance);
            if level < min {
                return;
            }
        }

        let message = message.into();

        // tracing fan-out -- always fires, regardless of buffer
        // views being open. RUST_LOG users see the same events.
        let id_disp = instance.map(|key| key.server_id.to_string());
        let ws_disp = instance.map(|key| key.workspace.display().to_string());
        match level {
            LogLevel::Error => tracing::error!(
                server_id = id_disp.as_deref(),
                workspace = ws_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Warn => tracing::warn!(
                server_id = id_disp.as_deref(),
                workspace = ws_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Info => tracing::info!(
                server_id = id_disp.as_deref(),
                workspace = ws_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Debug => tracing::debug!(
                server_id = id_disp.as_deref(),
                workspace = ws_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Trace => tracing::trace!(
                server_id = id_disp.as_deref(),
                workspace = ws_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
        }

        let record = LogRecord {
            timestamp: SystemTime::now(),
            server_id: instance.map(|k| Arc::clone(&k.server_id)),
            workspace: instance.map(|k| Arc::clone(&k.workspace)),
            level,
            source,
            message,
        };

        // Fan out to the runtime event bus before / after the
        // ring push. M.5.3.b: payload is a typed `LspLogPushed`;
        // post-B'.2 it carries the workspace alongside the
        // server_id so subscribers can route to the correct
        // per-instance buffer.
        let publish_payload = LspLogPushed {
            server_id: record.server_id.clone(),
            workspace: record.workspace.clone(),
            level: level_tag(record.level).to_string(),
            source: record.source.tag().to_string(),
            message: record.message.clone(),
        };

        match instance {
            None => {
                lock(&self.state.global).push(record);
            }
            Some(key) => {
                let cap = *lock(&self.state.default_capacity);
                let mut per = lock(&self.state.per_instance);
                per.entry(key.clone())
                    .or_insert_with(|| LogRing::new(cap))
                    .push(record);
            }
        }

        // Snapshot the publisher under the mutex, drop the lock,
        // then call -- the bus's internal mutex is independent of
        // ours and we never hold both at once.
        let publisher = lock(&self.state.event_publisher).clone();
        if let Some(p) = publisher {
            p(publish_payload);
        }
    }

    /// Resolve the min level for an instance (or subsystem-wide).
    fn effective_min_level(&self, instance: Option<&InstanceKey>) -> LogLevel {
        if let Some(key) = instance
            && let Some(level) = lock(&self.state.instance_levels).get(key).copied()
        {
            return level;
        }
        *lock(&self.state.default_min_level)
    }

    /// True iff trace mode is enabled for the instance. Cheap;
    /// the trace interceptors call this once per message and
    /// short-circuit the record build when false.
    pub fn is_tracing(&self, instance: &InstanceKey) -> bool {
        lock(&self.state.instance_trace).contains(instance)
    }

    /// Enable JSON-RPC trace for an instance. Trace records start
    /// landing in the per-instance ring on the next emission.
    pub fn enable_trace(&self, instance: InstanceKey) {
        lock(&self.state.instance_trace).insert(instance);
    }

    /// Disable JSON-RPC trace for an instance.
    pub fn disable_trace(&self, instance: &InstanceKey) {
        lock(&self.state.instance_trace).remove(instance);
    }

    /// Toggle JSON-RPC trace for an instance; returns the new
    /// state (true = on).
    pub fn toggle_trace(&self, instance: InstanceKey) -> bool {
        let mut guard = lock(&self.state.instance_trace);
        if guard.contains(&instance) {
            guard.remove(&instance);
            false
        } else {
            guard.insert(instance);
            true
        }
    }

    /// Set per-instance min level. `None` removes the override
    /// and reverts to the default.
    pub fn set_instance_level(&self, instance: InstanceKey, level: Option<LogLevel>) {
        let mut guard = lock(&self.state.instance_levels);
        match level {
            Some(l) => {
                guard.insert(instance, l);
            }
            None => {
                guard.remove(&instance);
            }
        }
    }

    /// Set the default min level (applies to subsystem-wide
    /// records and to instances without an override).
    pub fn set_default_level(&self, level: LogLevel) {
        *lock(&self.state.default_min_level) = level;
    }

    /// Set the default ring capacity. Existing rings are
    /// resized; future per-instance rings inherit the new value.
    pub fn set_default_capacity(&self, capacity: usize) {
        *lock(&self.state.default_capacity) = capacity;
        lock(&self.state.global).set_capacity(capacity);
        for (_, ring) in lock(&self.state.per_instance).iter_mut() {
            ring.set_capacity(capacity);
        }
    }

    /// Snapshot the subsystem-wide ring. Used by the `*lsp*`
    /// buffer view to populate its body.
    pub fn snapshot_global(&self) -> Vec<LogRecord> {
        lock(&self.state.global).snapshot()
    }

    /// Snapshot an instance's ring (empty if the instance has
    /// never logged anything).
    pub fn snapshot_instance(&self, instance: &InstanceKey) -> Vec<LogRecord> {
        lock(&self.state.per_instance)
            .get(instance)
            .map(LogRing::snapshot)
            .unwrap_or_default()
    }

    /// List every instance with a per-instance ring. Useful for
    /// `:lsp-status` and the `*lsp*` buffer's "servers" header.
    pub fn known_instances(&self) -> Vec<InstanceKey> {
        lock(&self.state.per_instance).keys().cloned().collect()
    }

    /// Drop the subsystem-wide ring's contents.
    pub fn clear_global(&self) {
        lock(&self.state.global).clear();
    }

    /// Drop an instance's ring contents.
    pub fn clear_instance(&self, instance: &InstanceKey) {
        if let Some(ring) = lock(&self.state.per_instance).get_mut(instance) {
            ring.clear();
        }
    }
}

impl Default for LspLogger {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl std::fmt::Debug for LspLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let global_len = lock(&self.state.global).len();
        let n_instances = lock(&self.state.per_instance).len();
        f.debug_struct("LspLogger")
            .field("global_records", &global_len)
            .field("instance_count", &n_instances)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn key(server: &str, workspace: &str) -> InstanceKey {
        InstanceKey::new(
            Arc::<str>::from(server),
            Arc::<Path>::from(PathBuf::from(workspace).as_path()),
        )
    }

    #[test]
    fn log_level_parse_round_trips() {
        assert_eq!(LogLevel::parse("error"), Some(LogLevel::Error));
        assert_eq!(LogLevel::parse("WARN"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("warning"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::parse("Info"), Some(LogLevel::Info));
        assert_eq!(LogLevel::parse("debug"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::parse("trace"), Some(LogLevel::Trace));
        assert_eq!(LogLevel::parse("nope"), None);
    }

    #[test]
    fn log_level_ordering_matches_severity() {
        // Error is the highest -- a min-level filter of Error
        // means "only show errors".
        assert!(LogLevel::Error > LogLevel::Warn);
        assert!(LogLevel::Warn > LogLevel::Info);
        assert!(LogLevel::Info > LogLevel::Debug);
        assert!(LogLevel::Debug > LogLevel::Trace);
    }

    #[test]
    fn ring_evicts_oldest_at_capacity() {
        let mut ring = LogRing::new(3);
        for i in 0..5 {
            ring.push(LogRecord {
                timestamp: SystemTime::now(),
                server_id: None,
                workspace: None,
                level: LogLevel::Info,
                source: LogSource::Client,
                message: format!("msg {i}"),
            });
        }
        assert_eq!(ring.len(), 3);
        let snap = ring.snapshot();
        assert_eq!(snap[0].message, "msg 2");
        assert_eq!(snap[1].message, "msg 3");
        assert_eq!(snap[2].message, "msg 4");
    }

    #[test]
    fn ring_zero_capacity_drops_everything() {
        let mut ring = LogRing::new(0);
        ring.push(LogRecord {
            timestamp: SystemTime::now(),
            server_id: None,
            workspace: None,
            level: LogLevel::Info,
            source: LogSource::Client,
            message: "lost".into(),
        });
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn log_routes_to_correct_ring() {
        let logger = LspLogger::with_defaults();
        let rust = key("rust", "/work/A");
        let py = key("python", "/work/A");

        logger.log(None, LogLevel::Info, LogSource::Client, "subsys event");
        logger.log(
            Some(&rust),
            LogLevel::Info,
            LogSource::LspMessage,
            "rust evt",
        );
        logger.log(Some(&py), LogLevel::Warn, LogSource::Stderr, "python evt");

        let g = logger.snapshot_global();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].message, "subsys event");

        let r = logger.snapshot_instance(&rust);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].message, "rust evt");

        let p = logger.snapshot_instance(&py);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].message, "python evt");

        // Snapshotting an unknown instance returns empty, not a
        // panic.
        let unknown = key("zzz", "/work/A");
        assert!(logger.snapshot_instance(&unknown).is_empty());
    }

    #[test]
    fn same_server_id_different_workspaces_stay_distinct() {
        // B'.2: two `rust-analyzer` instances against different
        // workspaces must NOT share a ring -- otherwise
        // `*lsp:rust-analyzer:/path/A*` and
        // `*lsp:rust-analyzer:/path/B*` would surface each other's
        // records.
        let logger = LspLogger::with_defaults();
        let rust_a = key("rust", "/work/A");
        let rust_b = key("rust", "/work/B");
        logger.log(
            Some(&rust_a),
            LogLevel::Info,
            LogSource::Client,
            "msg from A",
        );
        logger.log(
            Some(&rust_b),
            LogLevel::Info,
            LogSource::Client,
            "msg from B",
        );
        let a = logger.snapshot_instance(&rust_a);
        let b = logger.snapshot_instance(&rust_b);
        assert_eq!(a.len(), 1, "instance A has its own record");
        assert_eq!(a[0].message, "msg from A");
        assert_eq!(b.len(), 1, "instance B has its own record");
        assert_eq!(b[0].message, "msg from B");
        // The record carries the workspace so renderers can verify
        // which instance produced it.
        assert_eq!(a[0].workspace.as_deref(), Some(Path::new("/work/A")));
        assert_eq!(b[0].workspace.as_deref(), Some(Path::new("/work/B")));
    }

    #[test]
    fn log_below_min_level_is_dropped() {
        let logger = LspLogger::new(LogLevel::Warn, 100);
        let id = key("rust", "/work/A");
        logger.log(Some(&id), LogLevel::Info, LogSource::Client, "below");
        logger.log(Some(&id), LogLevel::Warn, LogSource::Client, "at");
        logger.log(Some(&id), LogLevel::Error, LogSource::Client, "above");
        let snap = logger.snapshot_instance(&id);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].message, "at");
        assert_eq!(snap[1].message, "above");
    }

    #[test]
    fn per_instance_level_overrides_default() {
        let logger = LspLogger::new(LogLevel::Info, 100);
        let rust = key("rust", "/work/A");
        let py = key("python", "/work/A");
        logger.set_instance_level(rust.clone(), Some(LogLevel::Debug));

        logger.log(Some(&rust), LogLevel::Debug, LogSource::Client, "rust dbg");
        logger.log(Some(&py), LogLevel::Debug, LogSource::Client, "py dbg");

        // rust override: Debug accepted.
        assert_eq!(logger.snapshot_instance(&rust).len(), 1);
        // python default: Debug below Info, dropped.
        assert_eq!(logger.snapshot_instance(&py).len(), 0);
    }

    #[test]
    fn trace_records_gated_by_per_instance_toggle() {
        let logger = LspLogger::new(LogLevel::Trace, 100); // Trace-permissive default
        let id = key("rust", "/work/A");
        // Trace toggle off by default -- record dropped.
        logger.log(Some(&id), LogLevel::Trace, LogSource::Trace, "t1");
        assert_eq!(logger.snapshot_instance(&id).len(), 0);
        // Enable trace, emit, observe.
        logger.enable_trace(id.clone());
        assert!(logger.is_tracing(&id));
        logger.log(Some(&id), LogLevel::Trace, LogSource::Trace, "t2");
        let snap = logger.snapshot_instance(&id);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].message, "t2");
        // Disable.
        logger.disable_trace(&id);
        assert!(!logger.is_tracing(&id));
        logger.log(Some(&id), LogLevel::Trace, LogSource::Trace, "t3");
        assert_eq!(logger.snapshot_instance(&id).len(), 1, "no new records");
    }

    #[test]
    fn toggle_trace_returns_new_state() {
        let logger = LspLogger::with_defaults();
        let id = key("rust", "/work/A");
        assert!(logger.toggle_trace(id.clone()));
        assert!(!logger.toggle_trace(id.clone()));
    }

    #[test]
    fn known_instances_lists_only_seen_instances() {
        let logger = LspLogger::with_defaults();
        let rust = key("rust", "/work/A");
        let py = key("python", "/work/A");
        logger.log(Some(&rust), LogLevel::Info, LogSource::Client, "x");
        logger.log(Some(&py), LogLevel::Info, LogSource::Client, "y");
        let mut known: Vec<String> = logger
            .known_instances()
            .into_iter()
            .map(|k| k.server_id.to_string())
            .collect();
        known.sort();
        assert_eq!(known, vec!["python".to_string(), "rust".to_string()]);
    }

    #[test]
    fn clearing_drops_records() {
        let logger = LspLogger::with_defaults();
        let id = key("rust", "/work/A");
        logger.log(None, LogLevel::Info, LogSource::Client, "a");
        logger.log(Some(&id), LogLevel::Info, LogSource::Client, "b");
        logger.clear_global();
        assert!(logger.snapshot_global().is_empty());
        // Per-instance ring still has the entry until we clear it.
        assert_eq!(logger.snapshot_instance(&id).len(), 1);
        logger.clear_instance(&id);
        assert!(logger.snapshot_instance(&id).is_empty());
    }

    #[test]
    fn set_default_capacity_resizes_existing_rings() {
        let logger = LspLogger::new(LogLevel::Info, 100);
        let id = key("rust", "/work/A");
        for i in 0..50 {
            logger.log(
                Some(&id),
                LogLevel::Info,
                LogSource::Client,
                format!("r{i}"),
            );
        }
        assert_eq!(logger.snapshot_instance(&id).len(), 50);
        // Shrink to 10 -- the most recent 10 survive.
        logger.set_default_capacity(10);
        let snap = logger.snapshot_instance(&id);
        assert_eq!(snap.len(), 10);
        assert_eq!(snap[0].message, "r40");
        assert_eq!(snap[9].message, "r49");
    }

    #[test]
    fn cheap_clone_shares_state() {
        let a = LspLogger::with_defaults();
        let b = a.clone();
        a.log(None, LogLevel::Info, LogSource::Client, "from a");
        // Clone sees the same ring.
        assert_eq!(b.snapshot_global().len(), 1);
    }
}
