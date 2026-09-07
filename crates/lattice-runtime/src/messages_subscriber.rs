//! `MessagesLayer`: a `tracing::Layer` that fans every event
//! into the App's `MessagesRing` + publishes a typed
//! `MessagePushed` on the editor event bus.
//!
//! Per `docs/dev/architecture/design.md` §5.10.6, `*messages*` is
//! the editor's audit log: every record `App::set_message`
//! produces, *plus* every `tracing::*` event from the editor +
//! plugins, flows through this single subscriber into one
//! buffer. The subscriber:
//!
//! - **Captures every event**, irrespective of where it
//!   originated (App code, `lattice-lsp`, future plugin host).
//! - **Translates `tracing::Level` to
//!   `lattice_grammar::EchoLevel`** for parity with the legacy
//!   `set_message` records (same wire enum either way).
//! - **Pushes to `MessagesRing`** for backlog seeding when the
//!   user opens `*messages*` mid-session.
//! - **Publishes `MessagePushed`** so per-tick drains in the
//!   App (`drain_message_events`) can append to the buffer.
//!
//! ## Install once at App boot
//!
//! [`install_messages_subscriber`] calls
//! `tracing_subscriber::registry().with(MessagesLayer { ...
//! }).set_global_default()`. The global default can only be
//! installed once per process — multi-App test setups must use
//! the no-install path (a `MessagesLayer` can still be
//! constructed and exercised directly for unit tests; only the
//! global install is gated).
//!
//! ## Hot-path cost
//!
//! When no subscriber is installed, `tracing::info!` is a
//! const-time atomic load → ~10ns per call (the tracing crate's
//! commitment). When the layer is installed:
//! `record_debug` visitor allocs ~one string + a `Mutex::lock`
//! on `MessagesRing` + bus publish. Total ~hundreds of ns per
//! event — acceptable because LSP / mode events fire at human
//! cadence, not keystroke cadence (per §8.2 Background-class).

use std::sync::{Arc, Mutex, OnceLock};

use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::filter::EnvFilter;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::reload;

use crate::events::EventBus;
use crate::messages::{MessagePushed, MessageRecord, MessagesRing};

/// `tracing::Layer` that bridges every event into the
/// `MessagesRing` + bus. Cheap to construct; cloning the layer
/// shares the underlying ring + bus handles via `Arc`.
#[derive(Clone)]
pub struct MessagesLayer {
    ring: Arc<Mutex<MessagesRing>>,
    bus: Arc<EventBus>,
}

impl MessagesLayer {
    /// New layer bound to the given ring + bus handles. The
    /// layer captures every event the installed subscriber
    /// receives.
    pub fn new(ring: Arc<Mutex<MessagesRing>>, bus: Arc<EventBus>) -> Self {
        Self { ring, bus }
    }

    /// Push a record + publish the bus event. Public so unit
    /// tests can drive the layer without standing up a global
    /// subscriber.
    pub fn emit(&self, record: MessageRecord) {
        if let Ok(mut ring) = self.ring.lock() {
            ring.push(record.clone());
        }
        self.bus.publish_typed(MessagePushed { record });
    }
}

impl<S> Layer<S> for MessagesLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = lattice_grammar::EchoLevel::from(*meta.level());

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        let text = visitor.finish();
        // Skip events with NOTHING to show. Most `tracing` macros set the
        // implicit `message` field, but a structured-only event would
        // otherwise produce an empty entry.
        if text.is_empty() {
            return;
        }

        self.emit(MessageRecord {
            timestamp: std::time::SystemTime::now(),
            level,
            text,
        });
    }
}

/// Visitor that renders an event's implicit `message` field **and its
/// structured fields** into one line.
///
/// ## The fields were being thrown away
///
/// This used to keep `message` and discard every other field, so a
/// diagnostic written as
///
/// ```ignore
/// debug!(?axes, want_syntax, have_syntax, "cells_matrix_invalidated");
/// ```
///
/// reached `*messages*` as the bare string `cells_matrix_invalidated`.
/// The message name is the least informative part of such an event —
/// the whole point of that line is *which axis differed and by how
/// much* — so the instrumentation was answering a question nobody
/// could read the answer to.
///
/// That is not hypothetical. The stale-highlight investigation added
/// exactly that event to name the invalidation axis behind a wrong-colour
/// render, then read a `*messages*` capture that could not show it, and
/// stalled. `*messages*` is documented as "the canonical surface" for
/// inspecting events without leaving the editor; it has to carry what the
/// events actually say.
///
/// ## Cost
///
/// Formatting is proportional to field count and happens on the thread
/// that logged. That is acceptable because the events carrying many
/// fields are `debug!` / `trace!`, which the level filter drops before
/// this layer sees them unless the user opted in (`--log-level debug`).
/// At the default level the common case is an `info!` with no extra
/// fields, which costs one `is_empty` check.
#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: String,
}

impl MessageVisitor {
    /// `message field=value field2=value2` — `tracing`'s own fmt layout,
    /// so a `*messages*` line and a stderr line read the same way.
    fn finish(self) -> String {
        if self.fields.is_empty() {
            return self.message;
        }
        if self.message.is_empty() {
            return self.fields;
        }
        format!("{} {}", self.message, self.fields)
    }

    fn push_field(&mut self, name: &str, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        if !self.fields.is_empty() {
            self.fields.push(' ');
        }
        // `write!` to a String is infallible; the result is ignored rather
        // than unwrapped so a logging path can never panic.
        let _ = write!(self.fields, "{name}={value:?}");
    }
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            // Strings render bare — `path=/tmp/x.org`, not `path="/tmp/x.org"`
            // — because the quotes are noise in a transcript a human reads.
            self.push_field(field.name(), &format_args!("{value}"));
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            if self.message.is_empty() {
                // `format_args!` Debug-formats to the rendered
                // output without quotes (its Debug impl is
                // identical to its Display).
                self.message = format!("{value:?}");
            }
        } else {
            self.push_field(field.name(), value);
        }
    }
}

/// Tracks whether `install_messages_subscriber` has installed
/// the global tracing subscriber for this process. Idempotent:
/// repeated calls (e.g. multi-App tests sharing the process)
/// no-op after the first install instead of panicking.
static GLOBAL_INSTALLED: OnceLock<()> = OnceLock::new();

/// Process-wide log-level override set by the CLI before
/// `install_messages_subscriber` runs. `lattice-cli::main`
/// computes the level from `-v`/`-q`/`--log-level` flags and
/// calls [`set_boot_log_level`]; `editor_boot.rs` calls
/// [`boot_log_level`] inside the install path.
///
/// `OnceLock<String>` (not an env var) because the
/// `lattice-cli` crate denies `unsafe_code` and modern Rust
/// flags `std::env::set_var` as unsafe (env mutations aren't
/// thread-safe). A process-wide `OnceLock` is the
/// equivalent-but-safe mechanism for set-once boot config.
static BOOT_LOG_LEVEL: OnceLock<String> = OnceLock::new();

/// CLI sets the boot-time log level before constructing the
/// editor. Idempotent — second call no-ops. The first set
/// wins (matching `tokio::main` -> `App::new` ordering).
pub fn set_boot_log_level(level: impl Into<String>) {
    let _ = BOOT_LOG_LEVEL.set(level.into());
}

/// `editor_boot` reads the boot-time log level when calling
/// [`install_messages_subscriber`]. Returns `None` when the
/// CLI didn't set one (typical for tests + library callers);
/// boot falls back to `"info"`.
pub fn boot_log_level() -> Option<String> {
    BOOT_LOG_LEVEL.get().cloned()
}

/// Process-wide flag set by the CLI before App::new to tell
/// the runtime whether to enable the fmt-to-stderr layer.
/// `Some(true)` ⇒ enable; `Some(false)` ⇒ disable;
/// `None` ⇒ runtime falls back to its default (currently
/// `true` to preserve previous behaviour for library callers
/// that don't set it).
///
/// Issue #36 (2026-05-22): TUI sets this to `false` because
/// stderr IS the terminal it paints into. GPUI sets to
/// `true` (stderr is a separate stream).
static BOOT_STDERR_ENABLED: OnceLock<bool> = OnceLock::new();

/// CLI sets the boot-time stderr-enabled flag before
/// constructing the editor. Idempotent — second call no-ops.
pub fn set_boot_stderr_enabled(enabled: bool) {
    let _ = BOOT_STDERR_ENABLED.set(enabled);
}

/// `editor_boot` reads the boot-time stderr-enabled flag.
/// Returns `None` when the CLI didn't set one (test paths
/// and library callers); editor_boot falls back to `false`
/// (safe — never accidentally corrupt a TUI screen).
pub fn boot_stderr_enabled() -> Option<bool> {
    BOOT_STDERR_ENABLED.get().copied()
}

/// Reload-handle for the `EnvFilter` that gates which events
/// the `MessagesLayer` captures. Stored at install time so
/// `:set messages.filter=...` can swap the filter live via
/// [`reload_messages_filter`] without restarting the editor.
/// `OnceLock<Option<...>>` so the "no filter wired" case (test
/// paths that construct the layer directly) is observable.
type FilterReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
static FILTER_HANDLE: OnceLock<FilterReloadHandle> = OnceLock::new();

/// Install `MessagesLayer` as the global tracing subscriber,
/// gated by an `EnvFilter` whose initial directive is
/// `initial_filter`. The filter is reloadable -- live edits
/// via [`reload_messages_filter`] swap the directive without
/// re-installing the subscriber.
///
/// Idempotent: only the first call wins; subsequent calls
/// return `false`. The first call's `ring` + `bus` are the
/// ones every later event flows into; the first call's
/// `initial_filter` seeds the filter handle.
///
/// `initial_filter` accepts the standard
/// `tracing_subscriber::EnvFilter` directive syntax (`info`,
/// `editor=info,lsp=debug`, ...). On parse failure the
/// install falls back to `info`.
///
/// **Why a global subscriber:** `tracing` can only have one
/// global default per process. Test isolation is handled by
/// the layer's `ring`/`bus` Arcs — every test that wants its
/// own messages stream constructs its own `MessagesLayer`
/// (without installing globally) and exercises `on_event` /
/// `emit` directly.
pub fn install_messages_subscriber(
    ring: Arc<Mutex<MessagesRing>>,
    bus: Arc<EventBus>,
    initial_filter: &str,
    stderr_enabled: bool,
) -> bool {
    if GLOBAL_INSTALLED.get().is_some() {
        return false;
    }
    let env_filter = EnvFilter::try_new(initial_filter)
        .unwrap_or_else(|_| EnvFilter::try_new("info").expect("`info` is a valid EnvFilter spec"));
    let (filter_layer, handle) = reload::Layer::new(env_filter);
    let messages_layer = MessagesLayer::new(ring, bus);
    // The `*messages*` buffer ALWAYS captures every event.
    // The fmt layer (stderr writer) is OPTIONAL.
    //
    // Issue #36 (2026-05-22): TUI peers must NOT enable
    // stderr — stderr IS the terminal ratatui paints into,
    // so every `tracing::*` event blits a stray line over
    // the screen until the next full redraw. The caller
    // passes `stderr_enabled = false` for TUI; GPUI passes
    // `true` (its stderr is a separate stream).
    //
    // `LATTICE_STDERR=1` (CLI flag) forces fmt ON regardless
    // for users running TUI with `2>tracing.log` redirection.
    //
    // Either way, `*messages*` is the canonical surface —
    // open with `:messages` to inspect events without leaving
    // the editor.
    let install_result = if stderr_enabled {
        let fmt_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);
        let subscriber = tracing_subscriber::registry()
            .with(filter_layer)
            .with(messages_layer)
            .with(fmt_layer);
        tracing::subscriber::set_global_default(subscriber)
    } else {
        let subscriber = tracing_subscriber::registry()
            .with(filter_layer)
            .with(messages_layer);
        tracing::subscriber::set_global_default(subscriber)
    };
    match install_result {
        Ok(()) => {
            let _ = GLOBAL_INSTALLED.set(());
            let _ = FILTER_HANDLE.set(handle);
            true
        }
        Err(_) => {
            // Another subscriber is already installed (perhaps
            // by `RUST_LOG=...` env_logger setup in a downstream
            // test or by a parallel App). Treat as success in
            // the sense that we don't try again.
            let _ = GLOBAL_INSTALLED.set(());
            false
        }
    }
}

/// Live-swap the messages-layer filter directive. Returns
/// `Err` when the directive fails to parse, when the global
/// subscriber wasn't installed (test paths), or when the
/// reload handle has been dropped. The App's option-change
/// cascade calls this on `:set messages.filter=<spec>`.
pub fn reload_messages_filter(spec: &str) -> Result<(), MessagesFilterReloadError> {
    let new_filter = EnvFilter::try_new(spec).map_err(|e| MessagesFilterReloadError::Parse {
        spec: spec.to_string(),
        reason: e.to_string(),
    })?;
    let handle = FILTER_HANDLE
        .get()
        .ok_or(MessagesFilterReloadError::SubscriberNotInstalled)?;
    handle
        .modify(|f| *f = new_filter)
        .map_err(|e| MessagesFilterReloadError::Reload(e.to_string()))
}

/// Why a [`reload_messages_filter`] call failed.
#[derive(Debug, thiserror::Error)]
pub enum MessagesFilterReloadError {
    /// The directive didn't parse as `EnvFilter` syntax. The
    /// typed-option validator already rejects bad strings at
    /// `:set` time; reaching this variant means the validator
    /// missed something.
    #[error("messages.filter `{spec}` is not a valid filter directive: {reason}")]
    Parse { spec: String, reason: String },
    /// `install_messages_subscriber` was never called for this
    /// process. Production boot always installs; test paths
    /// that exercise the App without going through `App::new`
    /// can observe this.
    #[error("messages-mode tracing subscriber not installed; cannot reload filter")]
    SubscriberNotInstalled,
    /// `tracing_subscriber::reload::Handle::modify` returned
    /// an error (typically because the underlying layer was
    /// dropped). Should not happen in production but the
    /// error type carries the message for diagnostics.
    #[error("messages.filter reload failed: {0}")]
    Reload(String),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// Layer captures `tracing::info!` text + level + emits a
    /// `MessagePushed` event on the bus. Exercised against a
    /// per-test subscriber via `with_default`, so this test
    /// doesn't depend on `install_messages_subscriber` (which
    /// can only run once per process).
    #[test]
    fn layer_captures_info_event_to_ring_and_bus() {
        let ring = Arc::new(Mutex::new(MessagesRing::default()));
        let bus = Arc::new(EventBus::new());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<MessagePushed>();
        bus.subscribe_typed(tx);
        let layer = MessagesLayer::new(ring.clone(), bus);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello world");
        });
        // Ring captured the record.
        let ring_records = ring.lock().unwrap();
        assert_eq!(ring_records.len(), 1);
        let r = ring_records.records().front().unwrap();
        assert_eq!(r.level, lattice_grammar::EchoLevel::Info);
        assert_eq!(r.text, "hello world");
        drop(ring_records);
        // Bus delivered the event.
        let evt = rx.try_recv().unwrap();
        assert_eq!(evt.record.text, "hello world");
        assert_eq!(evt.record.level, lattice_grammar::EchoLevel::Info);
    }

    /// A structured event's FIELDS reach `*messages*`, not just its name.
    ///
    /// They used to be discarded. A diagnostic written to name *which*
    /// invalidation axis moved arrived as the bare string
    /// `cells_matrix_invalidated`, which is the least informative part of
    /// it — and `*messages*` is the surface the docs point at for reading
    /// events without leaving the editor, so the answer existed and could
    /// not be read. The stale-highlight investigation stalled on exactly
    /// that.
    #[test]
    fn layer_renders_structured_fields_beside_the_message() {
        let ring = Arc::new(Mutex::new(MessagesRing::default()));
        let bus = Arc::new(EventBus::new());
        let layer = MessagesLayer::new(ring.clone(), bus);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                want_syntax = 7u64,
                have_syntax = 6u64,
                "cells_matrix_invalidated"
            );
        });
        let ring_records = ring.lock().unwrap();
        let r = ring_records.records().front().unwrap();
        assert!(
            r.text.contains("want_syntax=7") && r.text.contains("have_syntax=6"),
            "the fields are the whole diagnostic; got {:?}",
            r.text
        );
        assert!(
            r.text.starts_with("cells_matrix_invalidated"),
            "the message still leads the line: {:?}",
            r.text
        );
    }

    /// A `&str` field renders bare. Quotes are noise in a transcript, and
    /// paths are the common case (`path=/tmp/notes.org`).
    #[test]
    fn string_fields_render_without_quotes() {
        let ring = Arc::new(Mutex::new(MessagesRing::default()));
        let bus = Arc::new(EventBus::new());
        let layer = MessagesLayer::new(ring.clone(), bus);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(lang = "org", "attached");
        });
        let ring_records = ring.lock().unwrap();
        let r = ring_records.records().front().unwrap();
        assert_eq!(r.text, "attached lang=org");
    }

    /// An event with fields and NO message still lands, carrying its
    /// fields.
    ///
    /// Supersedes `layer_drops_messageless_events`, which asserted the
    /// opposite. Its stated reason was "don't pollute `*messages*` with
    /// EMPTY rows" — a consequence of fields being discarded, not an
    /// independent requirement. Now that fields render, the row is
    /// `buffer_id=7` rather than blank, so the reason no longer holds and
    /// dropping the event would be throwing away the only thing it said.
    #[test]
    fn a_field_only_event_is_not_dropped() {
        let ring = Arc::new(Mutex::new(MessagesRing::default()));
        let bus = Arc::new(EventBus::new());
        let layer = MessagesLayer::new(ring.clone(), bus);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(axis = "syntax");
        });
        let ring_records = ring.lock().unwrap();
        assert_eq!(ring_records.len(), 1, "a field-only event must not vanish");
        assert_eq!(ring_records.records().front().unwrap().text, "axis=syntax");
    }

    /// `*messages*` captures EVERY level the `EnvFilter` admits.
    ///
    /// This is a product requirement, not an implementation detail:
    /// `--log-level debug` exists so debug output can be read **inside
    /// the editor**, and `--stderr-logs` only ADDS the fmt layer — it
    /// never diverts events away from here (`install_messages_subscriber`
    /// installs this layer in both branches).
    ///
    /// 2026-08-16: I briefly filtered this layer to INFO+ to break a
    /// feedback loop (a `debug!` per render became an edit to the rendered
    /// `*messages*` buffer, which republished, which re-woke the workers).
    /// That fixed the loop by deleting the feature. The loop is now fixed
    /// where it belongs — the render cycle no longer logs per render, see
    /// the worker `run` loops — and this test exists so the shortcut is
    /// not taken again.
    #[test]
    fn layer_translates_every_level() {
        let ring = Arc::new(Mutex::new(MessagesRing::default()));
        let bus = Arc::new(EventBus::new());
        let layer = MessagesLayer::new(ring.clone(), bus);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!("t");
            tracing::debug!("d");
            tracing::info!("i");
            tracing::warn!("w");
            tracing::error!("e");
        });
        let ring = ring.lock().unwrap();
        let levels: Vec<lattice_grammar::EchoLevel> =
            ring.records().iter().map(|r| r.level).collect();
        assert_eq!(
            levels,
            vec![
                lattice_grammar::EchoLevel::Trace,
                lattice_grammar::EchoLevel::Debug,
                lattice_grammar::EchoLevel::Info,
                lattice_grammar::EchoLevel::Warn,
                lattice_grammar::EchoLevel::Error,
            ]
        );
    }

    /// Formatted-args messages render to their final string
    /// (no quotes, no Debug noise).
    #[test]
    fn layer_renders_format_args_without_debug_quotes() {
        let ring = Arc::new(Mutex::new(MessagesRing::default()));
        let bus = Arc::new(EventBus::new());
        let layer = MessagesLayer::new(ring.clone(), bus);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("opened {} buffers", 42);
        });
        let ring = ring.lock().unwrap();
        assert_eq!(ring.records().front().unwrap().text, "opened 42 buffers");
    }
}
