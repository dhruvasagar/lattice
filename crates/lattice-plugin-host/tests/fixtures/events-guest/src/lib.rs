//! PH7.8c event fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `events-plugin` world,
//! driving the event-delivery actor (`event_task.rs`) through a real host→guest
//! `on-event` call:
//!   - `register-events` (the world export the host calls once) subscribes three
//!     handlers via the imported `events.subscribe`: handler 1 → `DocumentSaved`,
//!     handler 2 → `BeforeQuit`, handler 3 → `ModalModeChanged`.
//!   - `on-event(handler, ev)` appends `"<handler>:<kind>\n"` to
//!     `/data/received.log` (the writable data-dir mount, PH7.2) so the host test
//!     can observe that delivery reached the guest end to end.
//!   - Handler 3 is a **poison** handler: it traps (`unreachable!`) instead of
//!     writing, exercising graceful degradation — the host logs + skips the
//!     delivery and never crashes, and every other subscriber (a native bus
//!     channel in the test) is untouched (§8 isolation). A trap taints the
//!     instance, so this plugin's later deliveries also fail; that is fine (it is
//!     dead until re-instantiation, PH7.12).

wit_bindgen::generate!({
    world: "events-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::events;
use lattice::plugin_host::host_services;
use lattice::plugin_host::types::{EventFilter, EventKind};
use lattice_plugin_sdk::PluginEvent;
use serde::{Deserialize, Serialize};

/// The plugin-defined event this fixture declares (`register-event`) and emits
/// (`emit-event`) when it observes a save. Authored via the PH7.8b.3 SDK derive:
/// `NAME` / `DOC` come from the derive (the `///` doc-comment IS the doc), and
/// `encode()` gives a type-safe MessagePack payload over the opaque wire. A
/// shared-crate copy of this exact type is the cross-plugin contract the host-side
/// consumer decodes in the e2e test.
#[derive(Serialize, Deserialize, PluginEvent)]
#[event(name = "events-fixture.saved-echo")]
struct SavedEcho {
    /// The path that was saved (echoed back on the bus).
    path: String,
}
// `Event` is world-`use`d, so wit-bindgen surfaces it at the crate root (in
// scope here without an import — importing it from `types` would collide).

struct Component;

/// Append one line to the log the host test reads. Factored out at OR.2, when a
/// second and third writer appeared; the behaviour is unchanged.
fn record(line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/data/received.log")
    {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// OC.2 wake state. A component is single-threaded, but `RefCell` says so
/// without `unsafe` and costs nothing at this call rate.
mod wake_state {
    use std::cell::Cell;

    thread_local! {
        /// The periodic wake armed from `register-events` — the "org's clock
        /// re-renders once a minute" shape.
        pub static TICKER: Cell<u32> = const { Cell::new(0) };
        /// How many times it has fired. The guest cancels itself at
        /// [`CANCEL_AFTER`] so a test can prove `cancel-wake` actually stops the
        /// timer rather than merely being callable.
        pub static FIRES: Cell<u32> = const { Cell::new(0) };
        /// A wake armed from *inside* `on-event` (org clocks in from a chord's
        /// event, not from registration) whose `on-wake` traps — the
        /// quarantine-without-wedging arm.
        pub static POISON: Cell<u32> = const { Cell::new(0) };
    }

    /// Fires after which the ticker cancels itself.
    pub const CANCEL_AFTER: u32 = 3;
}

/// A one-kind declarative filter (the common `:autocmd <kind>` shape).
fn kind_filter(kind: EventKind) -> EventFilter {
    EventFilter {
        kinds: Some(vec![kind]),
        path_globs: None,
        major_modes: None,
    }
}

/// A stable label per event kind — what the guest records so the host test can
/// assert which events were delivered.
fn label(ev: &Event) -> &'static str {
    match ev {
        Event::DocumentOpened(_) => "opened",
        Event::DocumentClosed(_) => "closed",
        Event::BeforeSave(_) => "before-save",
        Event::DocumentSaved(_) => "saved",
        Event::DocumentChanged(_) => "changed",
        Event::SelectionsChanged(_) => "selections",
        Event::ModalModeChanged(_) => "modal",
        Event::BeforeQuit => "quit",
        Event::OptionChanged(_) => "option",
        Event::MajorEntered(_) => "major-entered",
        Event::MajorExiting(_) => "major-exiting",
        Event::MinorActivated(_) => "minor-activated",
        Event::MinorDeactivated(_) => "minor-deactivated",
        Event::Plugin(_) => "plugin",
        Event::PrePluginLoaded(_) => "pre-plugin-loaded",
        Event::PluginLoaded(_) => "plugin-loaded",
        Event::PluginUnloaded(_) => "plugin-unloaded",
        Event::FilesChanged(_) => "files-changed",
    }
}

/// OR.2. The host path the guest watches, handed in through the data-dir mount
/// because a guest cannot know where its own `/data` lives on the host — and,
/// more to the point, because a real plugin learns its corpus root from
/// configuration too (`org.roam-directory`). Absent → no watch is armed, which
/// is what every pre-OR.2 test gets.
const WATCH_TARGET: &str = "/data/watch-target";

impl Guest for Component {
    /// The host calls this once; the guest subscribes through the imported
    /// `events.subscribe` host function.
    fn register_events() {
        events::subscribe(&kind_filter(EventKind::DocumentSaved), 1);
        events::subscribe(&kind_filter(EventKind::BeforeQuit), 2);
        // Poison handler: traps on delivery (graceful-skip exercise).
        events::subscribe(&kind_filter(EventKind::ModalModeChanged), 3);
        // No-op handler: returns immediately (no fs) — the clean per-delivery
        // dispatch path the perf ratchet (PH7.8d) measures.
        events::subscribe(&kind_filter(EventKind::DocumentChanged), 4);
        // OC.2: arms the poison wake when it fires (see `on_event`). A separate
        // kind so the existing delivery assertions are untouched.
        events::subscribe(&kind_filter(EventKind::DocumentOpened), 5);
        // PH7.8b.2/3: declare a plugin-defined event via the `register-event`
        // host-service, using the SDK-derived `NAME` + `DOC` (the doc-comment).
        // It self-registers into the host's runtime event registry under this
        // plugin's provenance; `on-event` handler 1 emits it on save.
        host_services::register_event(SavedEcho::NAME, SavedEcho::DOC);
        // OC.2: arm a periodic wake from registration. 50 ms is the seam's
        // floor — fast enough that a test does not sit on a real clock, and the
        // guest cancels itself after a few fires so it cannot run away.
        wake_state::TICKER.with(|t| t.set(events::wake_every(50)));
        // OR.2: arm a directory watch, if the test handed us one. Handler 6
        // records each batch — the point being that it records it with NO
        // action dispatched afterwards, which is the failure mode this seam is
        // most likely to have.
        if let Ok(target) = std::fs::read_to_string(WATCH_TARGET) {
            let target = target.trim();
            events::subscribe(&kind_filter(EventKind::FilesChanged), 6);
            let outcome = match host_services::watch(target) {
                Ok(()) => "watch:ok".to_string(),
                Err(e) => format!("watch:err({e})"),
            };
            record(&outcome);
            // …and a path the plugin was NOT granted. Recording the refusal
            // beside the success is what makes the grant check observable
            // rather than assumed: a seam that permitted everything would
            // produce the same first line.
            let denied = match host_services::watch("/") {
                Ok(()) => "denied:ok".to_string(),
                Err(e) => format!("denied:err({e})"),
            };
            record(&denied);
        }
    }

    /// Deliver one matching event. Handler 3 traps, handler 4 is a no-op (the
    /// perf-ratchet dispatch path); the rest append their kind to the data-dir
    /// log so the test can observe end-to-end delivery.
    fn on_event(handler: u32, ev: Event) {
        // OC.2: a wake armed from inside a handler — the shape org's clock-in
        // uses (a chord fires, the mode's actor arms the minute tick). This one's
        // `on-wake` traps, so a test can prove a trapping wake quarantines the
        // plugin without wedging the actor for everyone else.
        if handler == 5 {
            wake_state::POISON.with(|p| p.set(events::wake_every(50)));
            return;
        }
        if handler == 3 {
            // Deliberate trap: the host catches it, logs, and skips this
            // delivery — the plugin stays subscribed (§8).
            unreachable!("fixture poison handler traps on delivery");
        }
        if handler == 4 {
            // No-op: pure dispatch, no side effect (perf measurement).
            return;
        }
        // OR.2: a watch batch. Record how many paths arrived and their
        // basenames, so the host test can assert BOTH that the change crossed
        // and that a burst arrived as one batch rather than as N.
        if handler == 6 {
            let Event::FilesChanged(paths) = &ev else {
                record("6:not-files-changed");
                return;
            };
            let names: Vec<&str> = paths
                .iter()
                .filter_map(|p| p.rsplit('/').next())
                .filter(|n| !n.is_empty())
                .collect();
            record(&format!("6:files-changed:{}:{}", names.len(), names.join(",")));
            // A batch naming `stop.org` disarms the watch, so a test can prove
            // `unwatch` reaches a live watcher rather than merely being
            // callable.
            if names.contains(&"stop.org") {
                if let Ok(target) = std::fs::read_to_string(WATCH_TARGET) {
                    let outcome = match host_services::unwatch(target.trim()) {
                        Ok(()) => "unwatch:ok".to_string(),
                        Err(e) => format!("unwatch:err({e})"),
                    };
                    record(&outcome);
                }
            }
            return;
        }
        record(&format!("{handler}:{}", label(&ev)));
        // PH7.8b.2/3: on a save, EMIT a plugin-defined event. The SDK derive
        // MessagePack-encodes a typed struct (`SavedEcho`) into the opaque
        // payload; it crosses to the bus verbatim and a consumer sharing the type
        // decodes it (the e2e test). The host never parses the bytes.
        if handler == 1 {
            let echo = SavedEcho {
                path: match &ev {
                    Event::DocumentSaved(p) => p.path.clone(),
                    _ => String::new(),
                },
            };
            host_services::emit_event(SavedEcho::NAME, &echo.encode());
        }
    }

    /// OC.2: an armed wake came due.
    ///
    /// Two arms. The **ticker** appends `wake:<n>` to the same log the event
    /// deliveries write, so a test can see it advance with no event published at
    /// all — the whole point of the seam — and then cancels itself, so the log
    /// stops growing and a test can prove `cancel-wake` reached a live timer.
    /// The **poison** wake traps, exercising the same graceful-degradation
    /// contract `on-event`'s handler 3 does.
    fn on_wake(id: u32) {
        if id != 0 && wake_state::POISON.with(|p| p.get()) == id {
            unreachable!("fixture poison wake traps on delivery");
        }
        let n = wake_state::FIRES.with(|f| {
            let n = f.get() + 1;
            f.set(n);
            n
        });
        record(&format!("wake:{n}"));
        if n >= wake_state::CANCEL_AFTER {
            events::cancel_wake(id);
        }
    }
}

export!(Component);
