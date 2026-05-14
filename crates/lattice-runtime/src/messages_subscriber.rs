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
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;

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
        let text = visitor.message;
        // Skip events with no `message` field. Most `tracing`
        // macros set one (the implicit format string), but
        // structured-only events without a message body would
        // produce empty entries -- not useful in `*messages*`.
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

/// Visitor that extracts the implicit `message` field from a
/// `tracing::Event`. Falls back to `record_debug` which
/// produces the rendered `format_args!` output (no quotes for
/// `info!("..")`-style calls).
#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" && self.message.is_empty() {
            // `format_args!` Debug-formats to the rendered
            // output without quotes (its Debug impl is
            // identical to its Display).
            self.message = format!("{value:?}");
        }
    }
}

/// Tracks whether `install_messages_subscriber` has installed
/// the global tracing subscriber for this process. Idempotent:
/// repeated calls (e.g. multi-App tests sharing the process)
/// no-op after the first install instead of panicking.
static GLOBAL_INSTALLED: OnceLock<()> = OnceLock::new();

/// Install `MessagesLayer` as the global tracing subscriber.
/// Idempotent: only the first call wins; subsequent calls
/// return `false`. The first call's `ring` + `bus` are the
/// ones every later event flows into.
///
/// **Why a global subscriber:** `tracing` can only have one
/// global default per process. Test isolation is handled by
/// the layer's `ring`/`bus` Arcs — every test that wants its
/// own messages stream constructs its own `MessagesLayer`
/// (without installing globally) and exercises `on_event` /
/// `emit` directly.
pub fn install_messages_subscriber(ring: Arc<Mutex<MessagesRing>>, bus: Arc<EventBus>) -> bool {
    if GLOBAL_INSTALLED.get().is_some() {
        return false;
    }
    let layer = MessagesLayer::new(ring, bus);
    let subscriber = tracing_subscriber::registry().with(layer);
    match tracing::subscriber::set_global_default(subscriber) {
        Ok(()) => {
            let _ = GLOBAL_INSTALLED.set(());
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

    /// Events with no message body are dropped (don't pollute
    /// `*messages*` with empty rows).
    #[test]
    fn layer_drops_messageless_events() {
        let ring = Arc::new(Mutex::new(MessagesRing::default()));
        let bus = Arc::new(EventBus::new());
        let layer = MessagesLayer::new(ring.clone(), bus);
        let subscriber = tracing_subscriber::registry().with(layer);
        tracing::subscriber::with_default(subscriber, || {
            // Structured-only event -- no implicit `message` arg.
            tracing::info!(buffer_id = 7);
        });
        assert_eq!(ring.lock().unwrap().len(), 0);
    }
}
