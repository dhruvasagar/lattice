//! The event/hook guest world (PH7.8b).
//!
//! An event-observing plugin implements the `events-plugin` world: it **imports**
//! the `events` subscription API (host-provided) and `host-services`, and
//! **exports** `register-events` (the host calls it once to drive subscription
//! registration) and `on-event` (host→guest delivery). This module holds the
//! **fifth `bindgen!`** (after `plugin`, `picker-source-plugin`,
//! `completion-source-plugin`, `grammar-plugin`) for that world — the
//! shared-types trick (`with:` points
//! `types` + `host-services` at the `plugin` world's generated modules so a
//! crossed `event` value is the SAME Rust type `WitBoundary` round-trips,
//! `boundary_event.rs`; PH7.3d precedent).
//!
//! **Async (unlike grammar).** Event delivery is OFF the keystroke path: the host
//! owns an mpsc and pushes each `event` to `on-event` on the plugin's own task
//! (§5.10.4). So the `bindgen!` sets `exports: { default: async }` — an `on-event`
//! call suspends the guest stack, never pins the caller's thread, and a slow
//! handler can never freeze a keystroke or another subscriber (paramount #4). The
//! per-plugin actor that drains the bus channel and drives `on-event` is the
//! `event_task` bridge (PH7.8c).
//!
//! Registration flow (the grammar `register-grammar` precedent): the host calls
//! the guest's `register-events` export; the guest calls the imported
//! `events.subscribe(filter, handler)` host function; that records the
//! declaration into the Store's [`EventContributions`] (via the `events::Host`
//! impl on `PluginState`, `lib.rs`); after the export returns, the host drains the
//! subscriptions and wires each to the native `EventBus` (PH7.8c).

use crate::lattice::plugin_host::types::EventFilter as WitEventFilter;

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "events-plugin",
        path: "../../wit",
        // Event delivery (`on-event`) is async — off the keystroke path, a
        // delivery suspends the guest, never pins the caller's thread.
        exports: { default: async },
        with: {
            // Reuse the `plugin` world's generated mirrors so a crossed value is
            // the same Rust type `WitBoundary` round-trips; `host-services` reuses
            // the already-wired `Host` impl (the completion/picker precedent).
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            "lattice:plugin-host/host-services": crate::lattice::plugin_host::host_services,
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}

/// One subscription a plugin declared through `events.subscribe`, recorded
/// verbatim (the guest-chosen `handler` id + the WIT `filter`). The host drains
/// these after `register-events` returns and wires each to the native
/// `EventBus` (PH7.8c); the filter projects to native at wire time via
/// [`boundary_event::project_event_filter`](crate::boundary_event::project_event_filter).
pub struct RecordedSubscription {
    /// The guest's own dispatch key — passed back to `on-event(handler, ev)`.
    pub handler: u32,
    /// The declarative filter the plugin subscribed with (WIT form; projected to
    /// the native `EventFilter` when wired to the bus).
    pub filter: WitEventFilter,
}

/// The per-plugin accumulator the `events::Host` impl records into during
/// `register-events` (`lib.rs`). Held in `PluginState`; drained by the host
/// after the registration export returns (PH7.8c). `record` is the sync
/// host-func body (it only pushes — it cannot trap), factored here (the
/// [`GrammarContributions`](crate::grammar_host::GrammarContributions) precedent)
/// so recording is unit-testable without a `PluginState` / guest.
#[derive(Default)]
pub struct EventContributions {
    recorded: Vec<RecordedSubscription>,
}

impl EventContributions {
    /// Record a subscription (the `events.subscribe` host-func body).
    pub fn record(&mut self, filter: WitEventFilter, handler: u32) {
        self.recorded.push(RecordedSubscription { handler, filter });
    }

    /// How many subscriptions were recorded.
    pub fn len(&self) -> usize {
        self.recorded.len()
    }

    /// True when the plugin subscribed to nothing (the degenerate case — a
    /// non-observing plugin, or one whose `register-events` is empty).
    pub fn is_empty(&self) -> bool {
        self.recorded.is_empty()
    }

    /// Drain the recorded subscriptions, leaving the accumulator empty. Called by
    /// the host after `register-events` returns (PH7.8c) to wire the bus.
    pub fn take(&mut self) -> Vec<RecordedSubscription> {
        std::mem::take(&mut self.recorded)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::lattice::plugin_host::types::EventKind as WitEventKind;

    fn filter(kind: WitEventKind) -> WitEventFilter {
        WitEventFilter {
            kinds: Some(vec![kind]),
            path_globs: None,
            major_modes: None,
        }
    }

    #[test]
    fn records_subscriptions_and_preserves_handler_ids() {
        let mut e = EventContributions::default();
        assert!(e.is_empty());

        e.record(filter(WitEventKind::DocumentSaved), 1);
        e.record(filter(WitEventKind::BeforeQuit), 7);
        assert_eq!(e.len(), 2);

        let drained = e.take();
        assert!(e.is_empty(), "take() leaves the accumulator empty");
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].handler, 1);
        assert!(matches!(
            drained[0].filter.kinds.as_deref(),
            Some([WitEventKind::DocumentSaved])
        ));
        assert_eq!(drained[1].handler, 7);
    }
}
