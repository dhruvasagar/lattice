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
    }
}

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
        // PH7.8b.2/3: declare a plugin-defined event via the `register-event`
        // host-service, using the SDK-derived `NAME` + `DOC` (the doc-comment).
        // It self-registers into the host's runtime event registry under this
        // plugin's provenance; `on-event` handler 1 emits it on save.
        host_services::register_event(SavedEcho::NAME, SavedEcho::DOC);
    }

    /// Deliver one matching event. Handler 3 traps, handler 4 is a no-op (the
    /// perf-ratchet dispatch path); the rest append their kind to the data-dir
    /// log so the test can observe end-to-end delivery.
    fn on_event(handler: u32, ev: Event) {
        if handler == 3 {
            // Deliberate trap: the host catches it, logs, and skips this
            // delivery — the plugin stays subscribed (§8).
            unreachable!("fixture poison handler traps on delivery");
        }
        if handler == 4 {
            // No-op: pure dispatch, no side effect (perf measurement).
            return;
        }
        use std::io::Write;
        let line = format!("{handler}:{}\n", label(&ev));
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/data/received.log")
        {
            let _ = f.write_all(line.as_bytes());
        }
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
}

export!(Component);
