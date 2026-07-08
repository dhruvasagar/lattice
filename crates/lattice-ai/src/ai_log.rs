//! AI agent logging facade -- the producer side of the
//! `*ai:<provider>:<index>*` buffer views (AI-1b).
//!
//! ## Inspiration
//!
//! This module is a direct port of `lattice-lsp`'s
//! [`crate::ai_log`]-analogue, `lattice_lsp::logging`. LSP puts
//! per-server stderr / protocol events in `*lsp:<server-id>*`
//! buffers keyed by `(server_id, workspace)`; AI agents are the
//! same shape one level simpler -- a `SessionKey { provider,
//! index }` distinguishes a second `opencode` session (its own
//! ring/buffer) from the first, exactly as two `rust-analyzer`
//! instances against different workspaces stay distinct.
//!
//! ## Producer / consumer split
//!
//! This module is the **producer**: the ACP connection and
//! session machinery emit [`AiLogRecord`]s through
//! [`AiLogger::log`]. Records land in bounded [`LogRing`]s held
//! inside the logger.
//!
//! The **consumer** -- buffer-backed log views in
//! `lattice-ui-tui` (AI-1b, later slice) -- snapshots a ring on
//! demand via [`AiLogger::snapshot_global`] /
//! [`AiLogger::snapshot_session`] and renders the records.
//! Auto-scroll-to-tail and live tail-follow are buffer-side
//! concerns; the producer just appends.
//!
//! ## Tracing crate fan-out
//!
//! Every [`AiLogger::log`] call also emits a `tracing::*` event
//! at the matching level (target `"ai"`) so users who prefer
//! `RUST_LOG=ai=debug ./lattice` still see everything. The two
//! paths are independent: the in-memory rings survive even when
//! no `tracing` subscriber is installed, and `tracing` users see
//! the same events whether or not the buffer views are open.
//!
//! ## Deviation from the LSP template
//!
//! `lattice_lsp::logging::LspLogger` also gates a per-instance
//! JSON-RPC wire trace (`instance_trace` / `enable_trace` /
//! `is_tracing` / `toggle_trace` / `disable_trace`). AI-1b has no
//! per-message wire trace to toggle -- streamed agent text is a
//! first-class `AiLogSource::AgentText`, not an opt-in trace --
//! so that machinery is intentionally omitted here. Everything
//! else mirrors the LSP logger verbatim.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

use std::sync::Mutex;

/// Per-process key for the per-session log ring -- the
/// `(provider, index)` pair that distinguishes concurrent agent
/// processes of the same provider. A second `opencode` session
/// (`index = 2`) is a distinct ring/buffer from the first,
/// analogous to LSP's `InstanceKey(server_id, workspace)`. Cheap
/// to clone (one `Arc<str>` + a `Copy` integer).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub provider: Arc<str>,
    pub index: u32,
}

impl SessionKey {
    /// Construct a session key from an owned/borrowed provider
    /// name + process index. Accepts anything that converts into
    /// `Arc<str>` so callers don't need to pre-Arc.
    pub fn new(provider: impl Into<Arc<str>>, index: u32) -> Self {
        Self {
            provider: provider.into(),
            index,
        }
    }
}

/// Format one log record line for append to a synthetic AI log
/// buffer. Shape: `HH:MM:SS.mmm [<provider>:<index>] <level>
/// <source>: <message>`. Trailing newline is the caller's
/// responsibility (the drain batches many records into one
/// buffer-append).
pub fn format_ai_log_line(
    session: Option<&SessionKey>,
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
    let prefix = session
        .map(|key| format!("[{}:{}] ", key.provider, key.index))
        .unwrap_or_default();
    let msg = one_line(message);
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03} {prefix}{level} {source:>6}: {msg}")
}

/// Collapse newlines / carriage returns / tabs into spaces so the
/// formatted record fits on one buffer line.
fn one_line(s: &str) -> String {
    s.replace(['\n', '\r', '\t'], " ")
}

/// Closure invoked on every successful append. Wired by the App
/// (or test harness) to publish [`AiLogPushed`] onto the runtime
/// event bus, which lets log buffers refresh live as records
/// arrive. Optional: `AiLogger::with_defaults()` starts with no
/// publisher; `set_event_publisher` installs one.
pub type AiLogEventPublisher = Arc<dyn Fn(AiLogPushed) + Send + Sync>;

/// Compact severity tag for [`AiLogPushed`]. Mirrors
/// [`AiLogSource::tag`]'s shape for the level discriminator.
pub fn level_tag(l: AiLogLevel) -> &'static str {
    match l {
        AiLogLevel::Trace => "trace",
        AiLogLevel::Debug => "debug",
        AiLogLevel::Info => "info",
        AiLogLevel::Warn => "warn",
        AiLogLevel::Error => "error",
    }
}

/// Internal helper: lock with consistent expect-message. Logger
/// mutexes are never held across `.await` and never panic in the
/// critical section, so `PoisonError` is unreachable in safe
/// usage.
fn lock<'a, T>(m: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
    m.lock().expect("AiLogger mutex poisoned")
}

/// Severity levels. Ordered low-to-high so the derived
/// `PartialOrd` matches "Error > Warn > ... > Trace". Per-session
/// min level filters records BELOW it (i.e. `level < min` drops;
/// `level >= min` keeps).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AiLogLevel {
    /// Finest-grained detail (raw wire payload fragments, mailbox
    /// commands).
    Trace,
    /// Per-message, non-trace detail (tool-call argument dumps,
    /// permission-request bookkeeping).
    Debug,
    /// Lifecycle milestones (session started, agent attached,
    /// turn completed, ...).
    Info,
    /// Recoverable problems / unexpected protocol behaviour.
    Warn,
    /// Unrecoverable failures.
    Error,
}

impl AiLogLevel {
    /// Parse a string like `"info"` / `"debug"` (case-insensitive).
    /// Used by config loaders.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "error" => Some(AiLogLevel::Error),
            "warn" | "warning" => Some(AiLogLevel::Warn),
            "info" => Some(AiLogLevel::Info),
            "debug" => Some(AiLogLevel::Debug),
            "trace" => Some(AiLogLevel::Trace),
            _ => None,
        }
    }

    /// Iterate over all levels, low to high.
    pub fn all() -> &'static [AiLogLevel] {
        &[
            AiLogLevel::Trace,
            AiLogLevel::Debug,
            AiLogLevel::Info,
            AiLogLevel::Warn,
            AiLogLevel::Error,
        ]
    }

    /// Single-letter compact form for log rendering.
    pub fn short(self) -> char {
        match self {
            AiLogLevel::Error => 'E',
            AiLogLevel::Warn => 'W',
            AiLogLevel::Info => 'I',
            AiLogLevel::Debug => 'D',
            AiLogLevel::Trace => 'T',
        }
    }
}

/// Where a log record originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AiLogSource {
    /// Lattice-side (session spawn, handshake, permission
    /// requests, shutdown sequence, decode failures).
    Client,
    /// Streamed assistant text from `session/update`.
    AgentText,
    /// Streamed assistant reasoning / thinking content.
    Reasoning,
    /// Tool-call request / result content.
    ToolCall,
    /// Lifecycle milestones (session started, agent attached,
    /// turn completed, process exited).
    Lifecycle,
}

impl AiLogSource {
    /// Compact tag used in log rendering.
    pub fn tag(self) -> &'static str {
        match self {
            AiLogSource::Client => "client",
            AiLogSource::AgentText => "agent",
            AiLogSource::Reasoning => "reason",
            AiLogSource::ToolCall => "tool",
            AiLogSource::Lifecycle => "life",
        }
    }
}

/// One log entry. Cheap to clone (`Arc<str>` for the session's
/// provider name; the message is a plain `String`).
#[derive(Debug, Clone)]
pub struct AiLogRecord {
    /// Wall-clock time of emission. Used for buffer rendering;
    /// not load-bearing for ordering (records preserve insertion
    /// order in their ring).
    pub timestamp: SystemTime,
    /// `None` for subsystem-wide records (supervisor events).
    pub session: Option<SessionKey>,
    pub level: AiLogLevel,
    pub source: AiLogSource,
    pub message: String,
}

/// Bounded ring of log records. Append-only; oldest is evicted
/// when capacity is reached.
#[derive(Debug)]
pub struct LogRing {
    buf: VecDeque<AiLogRecord>,
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
    pub fn push(&mut self, record: AiLogRecord) {
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

    /// Snapshot every record. Cheap clone (each `AiLogRecord`'s
    /// heavy field is the message `String`; we're optimising for
    /// correctness here, not for log-view paint cost).
    pub fn snapshot(&self) -> Vec<AiLogRecord> {
        self.buf.iter().cloned().collect()
    }

    /// Drop everything. Used by `:ai-log clear`.
    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Capacity setter. Used when the user adjusts
    /// `ai.log_capacity` at runtime.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity;
        while self.buf.len() > self.capacity {
            self.buf.pop_front();
        }
    }
}

/// The typed event fired on every successful append. Analogue of
/// `lattice_lsp::events::LspLogPushed`.
#[derive(Debug, Clone)]
pub struct AiLogPushed {
    /// `None` for subsystem-wide records; `Some(key)` per-session.
    pub session: Option<SessionKey>,
    /// Severity tag (`"trace"`, `"debug"`, `"info"`, `"warn"`,
    /// `"error"`).
    pub level: String,
    /// Source tag (`"client"`, `"agent"`, `"reason"`, `"tool"`,
    /// `"life"`).
    pub source: String,
    /// The record's message text.
    pub message: String,
}

/// Producer-side log facade. One per AI subsystem; each agent
/// connection gets a clone (cheap -- internal state is
/// `Arc<Mutex<...>>`).
///
/// Cheap-by-design: append is O(1) amortized, level gating is one
/// HashMap lookup. Logging is **not** a hot path -- per-chunk cost
/// is the `session/update` decode + apply, not the log emission.
#[derive(Clone)]
pub struct AiLogger {
    state: Arc<LoggerState>,
}

struct LoggerState {
    /// Subsystem-wide ring (records where `session` is `None`).
    global: Mutex<LogRing>,
    /// Per-session rings, keyed by `(provider, index)`. A second
    /// `opencode` session is a different ring than the first.
    per_session: Mutex<HashMap<SessionKey, LogRing>>,
    /// Optional publisher fired on every successful append. Wired
    /// by the App at boot to feed the runtime event bus. `None` ->
    /// no events emitted (test paths, or pre-wire).
    event_publisher: Mutex<Option<AiLogEventPublisher>>,
    /// Default capacity for new per-session rings.
    default_capacity: Mutex<usize>,
    /// Default min level (records below are dropped).
    default_min_level: Mutex<AiLogLevel>,
    /// Per-session overrides for min level. Falls back to the
    /// default when absent.
    session_levels: Mutex<HashMap<SessionKey, AiLogLevel>>,
}

impl AiLogger {
    /// Construct a logger with the given default min level and
    /// default per-session ring capacity (e.g. `AiLogLevel::Info`,
    /// 10_000). Conservative defaults: Info filters out debug/trace
    /// noise; 10k records ≈ a few MB of memory at most.
    pub fn new(default_min_level: AiLogLevel, default_capacity: usize) -> Self {
        Self {
            state: Arc::new(LoggerState {
                global: Mutex::new(LogRing::new(default_capacity)),
                per_session: Mutex::new(HashMap::new()),
                default_capacity: Mutex::new(default_capacity),
                default_min_level: Mutex::new(default_min_level),
                session_levels: Mutex::new(HashMap::new()),
                event_publisher: Mutex::new(None),
            }),
        }
    }

    /// Install / replace the event publisher. Subsequent `log`
    /// calls fire the closure with [`AiLogPushed`] after the
    /// record lands in its ring. The App wires this at boot so the
    /// runtime event bus sees every append; subscribers (live log
    /// views) drain the bus on tick.
    pub fn set_event_publisher(&self, publisher: AiLogEventPublisher) {
        *lock(&self.state.event_publisher) = Some(publisher);
    }

    /// Sensible defaults: Info level, 10k records / ring.
    pub fn with_defaults() -> Self {
        Self::new(AiLogLevel::Info, 10_000)
    }

    /// Append a record. Gated by per-session min level (or the
    /// default). Records with `session = None` route to the
    /// subsystem-wide ring (`*ai*`) and use the default min level.
    pub fn log(
        &self,
        session: Option<&SessionKey>,
        level: AiLogLevel,
        source: AiLogSource,
        message: impl Into<String>,
    ) {
        let min = self.effective_min_level(session);
        if level < min {
            return;
        }

        let message = message.into();

        // tracing fan-out -- always fires, regardless of buffer
        // views being open. RUST_LOG users see the same events.
        let provider_disp = session.map(|key| key.provider.to_string());
        let index_disp = session.map(|key| key.index);
        match level {
            AiLogLevel::Error => tracing::error!(
                target: "ai",
                provider = provider_disp.as_deref(),
                index = index_disp,
                source = source.tag(),
                "{}",
                message
            ),
            AiLogLevel::Warn => tracing::warn!(
                target: "ai",
                provider = provider_disp.as_deref(),
                index = index_disp,
                source = source.tag(),
                "{}",
                message
            ),
            AiLogLevel::Info => tracing::info!(
                target: "ai",
                provider = provider_disp.as_deref(),
                index = index_disp,
                source = source.tag(),
                "{}",
                message
            ),
            AiLogLevel::Debug => tracing::debug!(
                target: "ai",
                provider = provider_disp.as_deref(),
                index = index_disp,
                source = source.tag(),
                "{}",
                message
            ),
            AiLogLevel::Trace => tracing::trace!(
                target: "ai",
                provider = provider_disp.as_deref(),
                index = index_disp,
                source = source.tag(),
                "{}",
                message
            ),
        }

        let record = AiLogRecord {
            timestamp: SystemTime::now(),
            session: session.cloned(),
            level,
            source,
            message,
        };

        // Fan out to the runtime event bus before / after the ring
        // push. Payload is a typed `AiLogPushed`; subscribers use
        // `session` to route to the correct per-process buffer.
        let publish_payload = AiLogPushed {
            session: record.session.clone(),
            level: level_tag(record.level).to_string(),
            source: record.source.tag().to_string(),
            message: record.message.clone(),
        };

        match session {
            None => {
                lock(&self.state.global).push(record);
            }
            Some(key) => {
                let cap = *lock(&self.state.default_capacity);
                let mut per = lock(&self.state.per_session);
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

    /// Resolve the min level for a session (or subsystem-wide).
    fn effective_min_level(&self, session: Option<&SessionKey>) -> AiLogLevel {
        if let Some(key) = session
            && let Some(level) = lock(&self.state.session_levels).get(key).copied()
        {
            return level;
        }
        *lock(&self.state.default_min_level)
    }

    /// Set per-session min level. `None` removes the override and
    /// reverts to the default.
    pub fn set_session_level(&self, session: SessionKey, level: Option<AiLogLevel>) {
        let mut guard = lock(&self.state.session_levels);
        match level {
            Some(l) => {
                guard.insert(session, l);
            }
            None => {
                guard.remove(&session);
            }
        }
    }

    /// Set the default min level (applies to subsystem-wide
    /// records and to sessions without an override).
    pub fn set_default_level(&self, level: AiLogLevel) {
        *lock(&self.state.default_min_level) = level;
    }

    /// Set the default ring capacity. Existing rings are resized;
    /// future per-session rings inherit the new value.
    pub fn set_default_capacity(&self, capacity: usize) {
        *lock(&self.state.default_capacity) = capacity;
        lock(&self.state.global).set_capacity(capacity);
        for (_, ring) in lock(&self.state.per_session).iter_mut() {
            ring.set_capacity(capacity);
        }
    }

    /// Snapshot the subsystem-wide ring. Used by the `*ai*` buffer
    /// view to populate its body.
    pub fn snapshot_global(&self) -> Vec<AiLogRecord> {
        lock(&self.state.global).snapshot()
    }

    /// Snapshot a session's ring (empty if the session has never
    /// logged anything).
    pub fn snapshot_session(&self, session: &SessionKey) -> Vec<AiLogRecord> {
        lock(&self.state.per_session)
            .get(session)
            .map(LogRing::snapshot)
            .unwrap_or_default()
    }

    /// List every session with a per-session ring. Useful for
    /// `:ai-status` and the `*ai*` buffer's "sessions" header.
    pub fn known_sessions(&self) -> Vec<SessionKey> {
        lock(&self.state.per_session).keys().cloned().collect()
    }

    /// Drop the subsystem-wide ring's contents.
    pub fn clear_global(&self) {
        lock(&self.state.global).clear();
    }

    /// Drop a session's ring contents.
    pub fn clear_session(&self, session: &SessionKey) {
        if let Some(ring) = lock(&self.state.per_session).get_mut(session) {
            ring.clear();
        }
    }
}

impl Default for AiLogger {
    fn default() -> Self {
        Self::with_defaults()
    }
}

impl std::fmt::Debug for AiLogger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let global_len = lock(&self.state.global).len();
        let n_sessions = lock(&self.state.per_session).len();
        f.debug_struct("AiLogger")
            .field("global_records", &global_len)
            .field("session_count", &n_sessions)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(provider: &str, index: u32) -> SessionKey {
        SessionKey::new(Arc::<str>::from(provider), index)
    }

    #[test]
    fn ai_log_level_parse_round_trips() {
        assert_eq!(AiLogLevel::parse("error"), Some(AiLogLevel::Error));
        assert_eq!(AiLogLevel::parse("WARN"), Some(AiLogLevel::Warn));
        assert_eq!(AiLogLevel::parse("warning"), Some(AiLogLevel::Warn));
        assert_eq!(AiLogLevel::parse("Info"), Some(AiLogLevel::Info));
        assert_eq!(AiLogLevel::parse("debug"), Some(AiLogLevel::Debug));
        assert_eq!(AiLogLevel::parse("trace"), Some(AiLogLevel::Trace));
        assert_eq!(AiLogLevel::parse("nope"), None);
    }

    #[test]
    fn level_ordering_matches_severity() {
        // Error is the highest -- a min-level filter of Error means
        // "only show errors".
        assert!(AiLogLevel::Error > AiLogLevel::Warn);
        assert!(AiLogLevel::Warn > AiLogLevel::Info);
        assert!(AiLogLevel::Info > AiLogLevel::Debug);
        assert!(AiLogLevel::Debug > AiLogLevel::Trace);
    }

    #[test]
    fn ring_evicts_oldest_at_capacity() {
        let mut ring = LogRing::new(3);
        for i in 0..5 {
            ring.push(AiLogRecord {
                timestamp: SystemTime::now(),
                session: None,
                level: AiLogLevel::Info,
                source: AiLogSource::Client,
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
        ring.push(AiLogRecord {
            timestamp: SystemTime::now(),
            session: None,
            level: AiLogLevel::Info,
            source: AiLogSource::Client,
            message: "lost".into(),
        });
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn log_routes_to_correct_ring() {
        let logger = AiLogger::with_defaults();
        let oc1 = key("opencode", 1);
        let oc2 = key("claude-code", 1);

        logger.log(None, AiLogLevel::Info, AiLogSource::Client, "subsys event");
        logger.log(
            Some(&oc1),
            AiLogLevel::Info,
            AiLogSource::AgentText,
            "opencode evt",
        );
        logger.log(
            Some(&oc2),
            AiLogLevel::Warn,
            AiLogSource::Lifecycle,
            "claude-code evt",
        );

        let g = logger.snapshot_global();
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].message, "subsys event");

        let r = logger.snapshot_session(&oc1);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].message, "opencode evt");

        let p = logger.snapshot_session(&oc2);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].message, "claude-code evt");

        // Snapshotting an unknown session returns empty, not a
        // panic.
        let unknown = key("zzz", 1);
        assert!(logger.snapshot_session(&unknown).is_empty());
    }

    #[test]
    fn different_sessions_stay_distinct() {
        // The per-process requirement: two `opencode` sessions
        // (index 1 vs index 2) must NOT share a ring -- otherwise
        // `*ai:opencode:1*` and `*ai:opencode:2*` would surface
        // each other's records.
        let logger = AiLogger::with_defaults();
        let oc1 = key("opencode", 1);
        let oc2 = key("opencode", 2);
        logger.log(
            Some(&oc1),
            AiLogLevel::Info,
            AiLogSource::Client,
            "msg from session 1",
        );
        logger.log(
            Some(&oc2),
            AiLogLevel::Info,
            AiLogSource::Client,
            "msg from session 2",
        );
        let a = logger.snapshot_session(&oc1);
        let b = logger.snapshot_session(&oc2);
        assert_eq!(a.len(), 1, "session 1 has its own record");
        assert_eq!(a[0].message, "msg from session 1");
        assert_eq!(b.len(), 1, "session 2 has its own record");
        assert_eq!(b[0].message, "msg from session 2");
        // The record carries the session so renderers can verify
        // which process produced it.
        assert_eq!(a[0].session.as_ref(), Some(&oc1));
        assert_eq!(b[0].session.as_ref(), Some(&oc2));
    }

    #[test]
    fn log_below_min_level_is_dropped() {
        let logger = AiLogger::new(AiLogLevel::Warn, 100);
        let id = key("opencode", 1);
        logger.log(Some(&id), AiLogLevel::Info, AiLogSource::Client, "below");
        logger.log(Some(&id), AiLogLevel::Warn, AiLogSource::Client, "at");
        logger.log(Some(&id), AiLogLevel::Error, AiLogSource::Client, "above");
        let snap = logger.snapshot_session(&id);
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].message, "at");
        assert_eq!(snap[1].message, "above");
    }

    #[test]
    fn per_session_level_overrides_default() {
        let logger = AiLogger::new(AiLogLevel::Info, 100);
        let oc = key("opencode", 1);
        let cc = key("claude-code", 1);
        logger.set_session_level(oc.clone(), Some(AiLogLevel::Debug));

        logger.log(Some(&oc), AiLogLevel::Debug, AiLogSource::Client, "oc dbg");
        logger.log(Some(&cc), AiLogLevel::Debug, AiLogSource::Client, "cc dbg");

        // opencode override: Debug accepted.
        assert_eq!(logger.snapshot_session(&oc).len(), 1);
        // claude-code default: Debug below Info, dropped.
        assert_eq!(logger.snapshot_session(&cc).len(), 0);
    }

    #[test]
    fn known_sessions_lists_only_seen() {
        let logger = AiLogger::with_defaults();
        let oc = key("opencode", 1);
        let cc = key("claude-code", 1);
        logger.log(Some(&oc), AiLogLevel::Info, AiLogSource::Client, "x");
        logger.log(Some(&cc), AiLogLevel::Info, AiLogSource::Client, "y");
        let mut known: Vec<String> = logger
            .known_sessions()
            .into_iter()
            .map(|k| k.provider.to_string())
            .collect();
        known.sort();
        assert_eq!(
            known,
            vec!["claude-code".to_string(), "opencode".to_string()]
        );
    }

    #[test]
    fn clearing_drops_records() {
        let logger = AiLogger::with_defaults();
        let id = key("opencode", 1);
        logger.log(None, AiLogLevel::Info, AiLogSource::Client, "a");
        logger.log(Some(&id), AiLogLevel::Info, AiLogSource::Client, "b");
        logger.clear_global();
        assert!(logger.snapshot_global().is_empty());
        // Per-session ring still has the entry until we clear it.
        assert_eq!(logger.snapshot_session(&id).len(), 1);
        logger.clear_session(&id);
        assert!(logger.snapshot_session(&id).is_empty());
    }

    #[test]
    fn set_default_capacity_resizes_existing_rings() {
        let logger = AiLogger::new(AiLogLevel::Info, 100);
        let id = key("opencode", 1);
        for i in 0..50 {
            logger.log(
                Some(&id),
                AiLogLevel::Info,
                AiLogSource::Client,
                format!("r{i}"),
            );
        }
        assert_eq!(logger.snapshot_session(&id).len(), 50);
        // Shrink to 10 -- the most recent 10 survive.
        logger.set_default_capacity(10);
        let snap = logger.snapshot_session(&id);
        assert_eq!(snap.len(), 10);
        assert_eq!(snap[0].message, "r40");
        assert_eq!(snap[9].message, "r49");
    }

    #[test]
    fn cheap_clone_shares_state() {
        let a = AiLogger::with_defaults();
        let b = a.clone();
        a.log(None, AiLogLevel::Info, AiLogSource::Client, "from a");
        // Clone sees the same ring.
        assert_eq!(b.snapshot_global().len(), 1);
    }

    #[test]
    fn format_ai_log_line_shape() {
        let session = key("opencode", 2);
        let line = format_ai_log_line(Some(&session), "info", "agent", "hello\nworld");
        // HH:MM:SS.mmm [opencode:2] info  agent: hello world
        assert!(
            line.contains("[opencode:2]"),
            "line should carry the session tag: {line}"
        );
        assert!(line.contains("info"), "line should carry the level: {line}");
        assert!(
            line.contains("agent"),
            "line should carry the source: {line}"
        );
        assert!(
            line.contains("hello world"),
            "newline should collapse to a space: {line}"
        );
        assert!(!line.contains('\n'), "no embedded newline: {line}");

        let global_line = format_ai_log_line(None, "warn", "client", "no session");
        assert!(
            !global_line.contains('['),
            "subsystem-wide line has no session prefix: {global_line}"
        );
    }
}
