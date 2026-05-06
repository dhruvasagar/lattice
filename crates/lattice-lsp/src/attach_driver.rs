//! `DocumentOpened` → `LspSupervisorHandle::open_buffer` driver.
//!
//! Subscribes one channel to the editor's event bus (filter:
//! [`EventKind::DocumentOpened`]) and spawns a tokio task that
//! forwards every event with a path-bearing payload to the
//! supervisor's mailbox. The supervisor's open path is async --
//! the LSP `initialize` round-trip with a real server takes
//! hundreds of milliseconds to multiple seconds -- so doing it
//! off the UI thread is what keeps the editor's input latency
//! under the paramount-goal-#1 budget (sub-frame keystroke ->
//! glyph).
//!
//! ## Single driver task, serial recv
//!
//! Opens are serialised at the supervisor's mailbox (one
//! command queue per supervisor task), so a single driver task
//! that awaits each `open_buffer` in turn doesn't lose any
//! parallelism that the supervisor would have allowed anyway.
//! Spawning a fresh tokio task per event would create
//! short-lived tasks that all funnel into the same supervisor
//! mailbox -- pure overhead. The serial loop is the right
//! shape.
//!
//! ## What "fire-and-forget" means here
//!
//! The publish call site (`App::new`, `App::do_edit`) returns
//! the moment the event is queued on the bus. The driver task
//! then awaits the supervisor reply on the LSP runtime, NOT
//! the UI thread. Failures log to the per-server / global
//! logger; nothing surfaces back to the publisher.
//!
//! Subscribers that need confirmation of attach (e.g., a
//! future "fly-in diagnostic banner when LSP first attaches")
//! should subscribe to a separate event the driver publishes
//! on success / failure. That event isn't part of v1; the
//! current design treats LSP attach as best-effort background
//! work.

use std::sync::Arc;

use lattice_protocol::event::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionId, SubscriptionTarget};

use crate::logging::{LogLevel, LogSource, LspLogger};
use crate::supervisor::LspSupervisorHandle;

/// Subscribe to `EventKind::DocumentOpened` on `bus` and spawn
/// the attach-driver task on `runtime_handle`. The driver task
/// runs for the editor's lifetime; the returned
/// [`SubscriptionId`] keeps the bus's bucket clean if the App
/// ever needs to tear the subscription down explicitly (today
/// it doesn't -- the supervisor outlives the App, the bus
/// drops with the App).
///
/// `lsp` is cloned into the driver task so the App keeps its
/// own handle (the App still issues hover / definition / etc.
/// directly). `logger` is cloned for failure reporting.
pub fn spawn(
    bus: Arc<EventBus>,
    runtime_handle: &tokio::runtime::Handle,
    lsp: LspSupervisorHandle,
    logger: LspLogger,
) -> SubscriptionId {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    let sub_id = bus.subscribe(
        EventFilter::kinds(vec![EventKind::DocumentOpened]),
        SubscriptionTarget::Channel(tx),
    );

    runtime_handle.spawn(async move {
        while let Some(event) = rx.recv().await {
            let Event::DocumentOpened { path, text, .. } = event else {
                // The filter is `DocumentOpened`-only; any other
                // variant here means the bus contract is
                // violated. Defensive ignore.
                continue;
            };
            let Some(path) = path else {
                // Path-less buffers (scratch / unsaved) don't
                // attach LSP. Publishers may still emit the
                // event so other subscribers (project watcher,
                // completion warmer) can react.
                continue;
            };
            if let Err(e) = lsp.open_buffer(path.clone(), text).await {
                logger.log(
                    None,
                    LogLevel::Warn,
                    LogSource::Client,
                    format!(
                        "lsp attach driver: open_buffer({}) failed: {e}",
                        path.display()
                    ),
                );
            }
        }
        // rx returned None -- the bus dropped the sender. The
        // driver exits cleanly; the supervisor stays alive.
    });

    sub_id
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::ids::DocumentId;

    /// The driver subscribes to DocumentOpened and ignores
    /// path-less events without surfacing an error. Logger
    /// stays empty.
    #[tokio::test]
    async fn pathless_event_is_a_noop() {
        let bus = Arc::new(EventBus::new());
        let logger = LspLogger::with_defaults();
        // Build a minimal supervisor that doesn't spawn real
        // servers (configs empty -> matches.is_empty -> Ok).
        let mut sup = crate::LspSupervisor::new(logger.clone());
        sup.set_event_bus(bus.clone());
        let lsp = sup.spawn(&tokio::runtime::Handle::current());
        let _sub = spawn(
            bus.clone(),
            &tokio::runtime::Handle::current(),
            lsp,
            logger.clone(),
        );

        bus.publish(Event::DocumentOpened {
            id: DocumentId::new(1),
            path: None,
            version: 0,
            text: String::new(),
        });
        // Yield once so the driver task gets a chance to recv.
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;

        // No log records emitted for path-less events.
        assert_eq!(logger.snapshot_global().len(), 0);
    }

    /// Path-bearing event with no matching server config (no
    /// `*.rs` patterns registered) routes through the
    /// supervisor and quietly returns Ok with no servers.
    /// Driver doesn't log anything; the supervisor's "opened"
    /// info log isn't emitted either because no server was
    /// attached.
    #[tokio::test]
    async fn path_event_with_no_matching_config_does_not_log_failure() {
        let bus = Arc::new(EventBus::new());
        let logger = LspLogger::with_defaults();
        let mut sup = crate::LspSupervisor::new(logger.clone());
        sup.set_event_bus(bus.clone());
        let lsp = sup.spawn(&tokio::runtime::Handle::current());
        let _sub = spawn(
            bus.clone(),
            &tokio::runtime::Handle::current(),
            lsp,
            logger.clone(),
        );

        bus.publish(Event::DocumentOpened {
            id: DocumentId::new(1),
            path: Some(std::path::PathBuf::from("/tmp/empty.unknown")),
            version: 0,
            text: "hello".into(),
        });
        // Wait for the supervisor task to process the open.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // No matching config -> Ok(empty), no failure record.
        let warns = logger
            .snapshot_global()
            .into_iter()
            .filter(|r| r.level == LogLevel::Warn)
            .count();
        assert_eq!(warns, 0);
    }
}
