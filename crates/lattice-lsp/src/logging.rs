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
use std::sync::Arc;
use std::time::SystemTime;

use std::sync::Mutex;

use crate::events::LspLogPushed;

/// Closure invoked on every successful append. Wired by the App
/// (or test harness) to publish [`LspLogPushed`] onto the runtime
/// event bus via `EventBus::publish_typed`, which lets log
/// buffers refresh live as records arrive. Optional:
/// `LspLogger::with_defaults()` starts with no publisher;
/// `set_event_publisher` installs one.
pub type LogEventPublisher = Arc<dyn Fn(LspLogPushed) + Send + Sync>;

/// Compact severity tag for [`LspLogPushed`]. Mirrors
/// [`LogSource::tag`]'s shape for the level discriminator.
fn level_tag(l: LogLevel) -> &'static str {
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

/// One log entry. Cheap to clone (`Arc<str>` for server id;
/// the message is a plain `String`).
#[derive(Debug, Clone)]
pub struct LogRecord {
    /// Wall-clock time of emission. Used for buffer rendering;
    /// not load-bearing for ordering (records preserve insertion
    /// order in their ring).
    pub timestamp: SystemTime,
    /// `None` for subsystem-wide records (supervisor events).
    pub server_id: Option<Arc<str>>,
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
    /// Subsystem-wide ring (server_id = None records).
    global: Mutex<LogRing>,
    /// Per-server rings (server_id = Some(_) records).
    per_server: Mutex<HashMap<Arc<str>, LogRing>>,
    /// Optional publisher fired on every successful append.
    /// Wired by the App at boot to feed the runtime event bus.
    /// `None` -> no events emitted (test paths, or pre-wire).
    event_publisher: Mutex<Option<LogEventPublisher>>,
    /// Default capacity for new per-server rings.
    default_capacity: Mutex<usize>,
    /// Default min level (records below are dropped).
    default_min_level: Mutex<LogLevel>,
    /// Per-server overrides for min level. Falls back to the
    /// default when absent.
    server_levels: Mutex<HashMap<Arc<str>, LogLevel>>,
    /// Per-server trace toggle. When absent / false, Trace-level
    /// records are dropped before the ring lookup.
    server_trace: Mutex<HashSet<Arc<str>>>,
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
                per_server: Mutex::new(HashMap::new()),
                default_capacity: Mutex::new(default_capacity),
                default_min_level: Mutex::new(default_min_level),
                server_levels: Mutex::new(HashMap::new()),
                server_trace: Mutex::new(HashSet::new()),
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

    /// Append a record. Gated by per-server min level (or the
    /// default). Subsystem-wide records (server_id = None) use
    /// the default min level. Trace-level records are
    /// additionally gated by the per-server trace toggle.
    pub fn log(
        &self,
        server_id: Option<&Arc<str>>,
        level: LogLevel,
        source: LogSource,
        message: impl Into<String>,
    ) {
        // Trace gating: per-server trace toggle decides whether
        // Trace records reach a ring at all. When the toggle is
        // ON, Trace records bypass the per-server min-level
        // filter (the user opted in deliberately; the default
        // Info filter would otherwise drop them on the floor).
        // When the toggle is OFF, Trace records are dropped here
        // and the level filter never sees them.
        let trace_bypass = if level == LogLevel::Trace {
            match server_id {
                Some(id) if self.is_tracing(id) => true,
                Some(_) => return, // Trace, toggle off -> drop.
                None => false,     // Subsystem-wide trace honours level filter.
            }
        } else {
            false
        };

        if !trace_bypass {
            let min = self.effective_min_level(server_id);
            if level < min {
                return;
            }
        }

        let message = message.into();

        // tracing fan-out -- always fires, regardless of buffer
        // views being open. RUST_LOG users see the same events.
        let id_disp = server_id.map(|id| id.to_string());
        match level {
            LogLevel::Error => tracing::error!(
                server_id = id_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Warn => tracing::warn!(
                server_id = id_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Info => tracing::info!(
                server_id = id_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Debug => tracing::debug!(
                server_id = id_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
            LogLevel::Trace => tracing::trace!(
                server_id = id_disp.as_deref(),
                source = source.tag(),
                "{}",
                message
            ),
        }

        let record = LogRecord {
            timestamp: SystemTime::now(),
            server_id: server_id.cloned(),
            level,
            source,
            message,
        };

        // Fan out to the runtime event bus before / after the
        // ring push. We snapshot primitive fields so the publisher
        // closure (which lives in the App via `lattice-protocol`)
        // M.5.3.b: payload is now a typed `LspLogPushed` struct.
        // `server_id` is `Arc<str>` (cheap clone, matches the
        // record's own type) rather than the prior `String`
        // round-trip.
        let publish_payload = LspLogPushed {
            server_id: record.server_id.clone(),
            level: level_tag(record.level).to_string(),
            source: record.source.tag().to_string(),
            message: record.message.clone(),
        };

        match server_id {
            None => {
                lock(&self.state.global).push(record);
            }
            Some(id) => {
                let cap = *lock(&self.state.default_capacity);
                let mut per = lock(&self.state.per_server);
                per.entry(Arc::clone(id))
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

    /// Resolve the min level for a server (or subsystem-wide).
    fn effective_min_level(&self, server_id: Option<&Arc<str>>) -> LogLevel {
        if let Some(id) = server_id
            && let Some(level) = lock(&self.state.server_levels).get(id).copied()
        {
            return level;
        }
        *lock(&self.state.default_min_level)
    }

    /// True iff trace mode is enabled for `server_id`. Cheap;
    /// the trace interceptors call this once per message and
    /// short-circuit the record build when false.
    pub fn is_tracing(&self, server_id: &Arc<str>) -> bool {
        lock(&self.state.server_trace).contains(server_id)
    }

    /// Enable JSON-RPC trace for a server. Trace records start
    /// landing in the per-server ring on the next emission.
    pub fn enable_trace(&self, server_id: Arc<str>) {
        lock(&self.state.server_trace).insert(server_id);
    }

    /// Disable JSON-RPC trace for a server.
    pub fn disable_trace(&self, server_id: &Arc<str>) {
        lock(&self.state.server_trace).remove(server_id);
    }

    /// Toggle JSON-RPC trace; returns the new state (true = on).
    pub fn toggle_trace(&self, server_id: Arc<str>) -> bool {
        let mut guard = lock(&self.state.server_trace);
        if guard.contains(&server_id) {
            guard.remove(&server_id);
            false
        } else {
            guard.insert(server_id);
            true
        }
    }

    /// Set per-server min level. `None` removes the override and
    /// reverts to the default.
    pub fn set_server_level(&self, server_id: Arc<str>, level: Option<LogLevel>) {
        let mut guard = lock(&self.state.server_levels);
        match level {
            Some(l) => {
                guard.insert(server_id, l);
            }
            None => {
                guard.remove(&server_id);
            }
        }
    }

    /// Set the default min level (applies to subsystem-wide
    /// records and to servers without an override).
    pub fn set_default_level(&self, level: LogLevel) {
        *lock(&self.state.default_min_level) = level;
    }

    /// Set the default ring capacity. Existing rings are
    /// resized; future per-server rings inherit the new value.
    pub fn set_default_capacity(&self, capacity: usize) {
        *lock(&self.state.default_capacity) = capacity;
        lock(&self.state.global).set_capacity(capacity);
        for (_, ring) in lock(&self.state.per_server).iter_mut() {
            ring.set_capacity(capacity);
        }
    }

    /// Snapshot the subsystem-wide ring. Used by the `*lsp*`
    /// buffer view to populate its body.
    pub fn snapshot_global(&self) -> Vec<LogRecord> {
        lock(&self.state.global).snapshot()
    }

    /// Snapshot a server's ring (empty if the server has never
    /// logged anything).
    pub fn snapshot_server(&self, server_id: &Arc<str>) -> Vec<LogRecord> {
        lock(&self.state.per_server)
            .get(server_id)
            .map(LogRing::snapshot)
            .unwrap_or_default()
    }

    /// List every server with a per-server ring. Useful for
    /// `:lsp-status` and the `*lsp*` buffer's "servers" header.
    pub fn known_servers(&self) -> Vec<Arc<str>> {
        lock(&self.state.per_server).keys().cloned().collect()
    }

    /// Drop the subsystem-wide ring's contents.
    pub fn clear_global(&self) {
        lock(&self.state.global).clear();
    }

    /// Drop a server's ring contents.
    pub fn clear_server(&self, server_id: &Arc<str>) {
        if let Some(ring) = lock(&self.state.per_server).get_mut(server_id) {
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
        let n_servers = lock(&self.state.per_server).len();
        f.debug_struct("LspLogger")
            .field("global_records", &global_len)
            .field("server_count", &n_servers)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_id(s: &str) -> Arc<str> {
        Arc::from(s)
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
            level: LogLevel::Info,
            source: LogSource::Client,
            message: "lost".into(),
        });
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn log_routes_to_correct_ring() {
        let logger = LspLogger::with_defaults();
        let rust = make_id("rust");
        let py = make_id("python");

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

        let r = logger.snapshot_server(&rust);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].message, "rust evt");

        let p = logger.snapshot_server(&py);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].message, "python evt");

        // Snapshotting an unknown server returns empty, not a
        // panic.
        let unknown = make_id("zzz");
        assert!(logger.snapshot_server(&unknown).is_empty());
    }

    #[test]
    fn log_below_min_level_is_dropped() {
        let logger = LspLogger::new(LogLevel::Warn, 100);
        let id = make_id("rust");
        logger.log(Some(&id), LogLevel::Info, LogSource::Client, "below");
        logger.log(Some(&id), LogLevel::Warn, LogSource::Client, "at");
        logger.log(Some(&id), LogLevel::Error, LogSource::Client, "above");
        let snap = logger.snapshot_server(&id);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].message, "at");
        assert_eq!(snap[1].message, "above");
    }

    #[test]
    fn per_server_level_overrides_default() {
        let logger = LspLogger::new(LogLevel::Info, 100);
        let rust = make_id("rust");
        let py = make_id("python");
        logger.set_server_level(Arc::clone(&rust), Some(LogLevel::Debug));

        logger.log(Some(&rust), LogLevel::Debug, LogSource::Client, "rust dbg");
        logger.log(Some(&py), LogLevel::Debug, LogSource::Client, "py dbg");

        // rust override: Debug accepted.
        assert_eq!(logger.snapshot_server(&rust).len(), 1);
        // python default: Debug below Info, dropped.
        assert_eq!(logger.snapshot_server(&py).len(), 0);
    }

    #[test]
    fn trace_records_gated_by_per_server_toggle() {
        let logger = LspLogger::new(LogLevel::Trace, 100); // Trace-permissive default
        let id = make_id("rust");
        // Trace toggle off by default -- record dropped.
        logger.log(Some(&id), LogLevel::Trace, LogSource::Trace, "t1");
        assert_eq!(logger.snapshot_server(&id).len(), 0);
        // Enable trace, emit, observe.
        logger.enable_trace(Arc::clone(&id));
        assert!(logger.is_tracing(&id));
        logger.log(Some(&id), LogLevel::Trace, LogSource::Trace, "t2");
        let snap = logger.snapshot_server(&id);
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].message, "t2");
        // Disable.
        logger.disable_trace(&id);
        assert!(!logger.is_tracing(&id));
        logger.log(Some(&id), LogLevel::Trace, LogSource::Trace, "t3");
        assert_eq!(logger.snapshot_server(&id).len(), 1, "no new records");
    }

    #[test]
    fn toggle_trace_returns_new_state() {
        let logger = LspLogger::with_defaults();
        let id = make_id("rust");
        assert!(logger.toggle_trace(Arc::clone(&id)));
        assert!(!logger.toggle_trace(Arc::clone(&id)));
    }

    #[test]
    fn known_servers_lists_only_seen_servers() {
        let logger = LspLogger::with_defaults();
        let rust = make_id("rust");
        let py = make_id("python");
        logger.log(Some(&rust), LogLevel::Info, LogSource::Client, "x");
        logger.log(Some(&py), LogLevel::Info, LogSource::Client, "y");
        let mut known: Vec<String> = logger
            .known_servers()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        known.sort();
        assert_eq!(known, vec!["python".to_string(), "rust".to_string()]);
    }

    #[test]
    fn clearing_drops_records() {
        let logger = LspLogger::with_defaults();
        let id = make_id("rust");
        logger.log(None, LogLevel::Info, LogSource::Client, "a");
        logger.log(Some(&id), LogLevel::Info, LogSource::Client, "b");
        logger.clear_global();
        assert!(logger.snapshot_global().is_empty());
        // Per-server ring still has the entry until we clear it.
        assert_eq!(logger.snapshot_server(&id).len(), 1);
        logger.clear_server(&id);
        assert!(logger.snapshot_server(&id).is_empty());
    }

    #[test]
    fn set_default_capacity_resizes_existing_rings() {
        let logger = LspLogger::new(LogLevel::Info, 100);
        let id = make_id("rust");
        for i in 0..50 {
            logger.log(
                Some(&id),
                LogLevel::Info,
                LogSource::Client,
                format!("r{i}"),
            );
        }
        assert_eq!(logger.snapshot_server(&id).len(), 50);
        // Shrink to 10 -- the most recent 10 survive.
        logger.set_default_capacity(10);
        let snap = logger.snapshot_server(&id);
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
