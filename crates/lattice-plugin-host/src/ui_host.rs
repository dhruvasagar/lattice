//! The `ui` guest→host contribution seam (OC.3 / ML.6) — a plugin-owned
//! modeline element.
//!
//! **The canonical API is the WIT** (`ui.wit`); this module is only the host
//! side of it. The rule it implements is `modeline.md` §6: whoever registers an
//! element owns it end to end — the descriptor, the content, and (later) the
//! interaction handlers. The host exposes generic primitives and branches on
//! nothing, so the acid test that fragment states holds: a provider adding a
//! modeline element needs zero `Editor::` methods and zero new host `Action`
//! variants.
//!
//! ## Three things that are load-bearing and easy to get wrong
//!
//! **Content goes on the bus, not straight into the service.** `ModelineService`
//! is interior-mutable, so a host function *could* just call `apply` and the
//! content store would be correct — and nothing would repaint until the user
//! next pressed a key. The repaint comes from the bus forwarder waking the actor
//! (`editor_boot.rs`'s `ModelineElementUpdate` subscription → `async_landed`),
//! which is the same reason `lattice-lsp::modeline` and `lattice-ai::mcp::status`
//! publish rather than write. Writing directly is the "it works, but only after I
//! hit something" bug with a plausible-looking implementation.
//!
//! **The descriptor goes straight into the service, not on the bus.** It is not
//! per-frame state and has no wake to earn; the renderer reads the registry
//! through the snapshot it takes each frame.
//!
//! **Ids are namespaced with the plugin's name**, like config options. Without
//! that, `register-segment("mode")` would resolve to the id `mode`, and a plugin
//! could register `core.mode` and silently take over a built-in element —
//! `ModelineService::register` is last-write-wins by design.

use lattice_mode::ModelineServiceHandle;
use lattice_mode::modeline::{
    ElementContent, ElementId, ModelineElement, ModelineElementUpdate, ModelineKey, ModelineRole,
    Scope, Span, Zone,
};
use lattice_runtime::EventBus;
use std::sync::Arc;

use crate::lattice::plugin_host::types::UiZone;

/// The role every plugin span carries.
///
/// Not a parameter, and the WIT says why: both renderers match role names
/// against a closed five-constant set and disagree on the fallback (TUI renders
/// unstyled, GPUI renders in the path colour), so an arbitrary role from a
/// plugin would be a silent cross-renderer difference. `modeline.mode_item` is
/// the one role both peers resolve identically, and it is the only role either
/// native modeline producer uses.
const PLUGIN_ROLE: &str = "modeline.mode_item";

/// What a plugin's `ui` calls act on. `Some` only on the async spawn paths that
/// are handed a modeline; `None` on the sync grammar store, which is what keeps
/// the modeline off the keystroke path once the Component Model forces the
/// import to be linked there anyway (see `ui.wit`).
#[derive(Clone)]
pub(crate) struct UiCtx {
    /// The registry descriptors are registered into and removed from.
    pub(crate) modeline: ModelineServiceHandle,
    /// The bus content updates are published on, so the repaint wake fires.
    pub(crate) bus: Arc<EventBus>,
}

/// Namespace a plugin's element id with its own name (`org` + `clock` →
/// `org.clock`).
///
/// A plugin with no recorded name — only the minimal test constructor — gets its
/// id unprefixed, which is fine there and reachable nowhere else.
pub(crate) fn namespaced_id(plugin_name: Option<&str>, id: &str) -> String {
    match plugin_name {
        Some(name) => format!("{name}.{id}"),
        None => id.to_string(),
    }
}

/// True if `id` would land in the built-in namespace the renderers compute
/// host-side (`core.*`).
///
/// After namespacing this can only happen for a plugin literally named `core`,
/// which is a name a bundled plugin could plausibly be given. The check costs
/// one comparison and closes a hijack that would otherwise be silent —
/// registration is last-write-wins, so the built-in element would simply stop
/// rendering with no error anywhere.
pub(crate) fn is_builtin_namespace(id: &str) -> bool {
    id.starts_with("core.")
}

/// The `register-segment` body: build the descriptor and hand it to the service.
///
/// Global scope, not per-pane, per the WIT — a plugin has no buffer to scope to
/// and no plugin needs per-buffer yet.
pub(crate) fn register_segment(
    ctx: &UiCtx,
    id: String,
    zone: UiZone,
    priority: i32,
) -> Result<(), String> {
    if is_builtin_namespace(&id) {
        return Err(format!(
            "{id} is in the built-in `core.` namespace; registration refused"
        ));
    }
    ctx.modeline.register(
        ModelineElement::new(ElementId::new(id), zone_from_wit(zone), priority)
            .with_scope(Scope::Global),
    );
    Ok(())
}

/// The `emit-segment` body: publish content on the bus so the repaint wake fires.
///
/// Empty text produces empty content, which the renderer treats as "hidden" —
/// so a plugin with nothing to say this minute needs no separate call.
pub(crate) fn emit_segment(ctx: &UiCtx, id: String, text: String) {
    let content = if text.is_empty() {
        ElementContent::default()
    } else {
        ElementContent {
            spans: vec![Span {
                text,
                role: ModelineRole::new(PLUGIN_ROLE),
            }],
        }
    };
    ctx.bus.publish_typed(ModelineElementUpdate {
        key: ModelineKey::Global,
        id: ElementId::new(id),
        content,
    });
}

/// The `clear-segment` body — `emit-segment` with nothing to show. Idempotent,
/// including for an id that was never registered: the update lands in the
/// content store keyed by an id no descriptor names, and the renderer, which
/// iterates descriptors, never looks at it.
pub(crate) fn clear_segment(ctx: &UiCtx, id: String) {
    emit_segment(ctx, id, String::new());
}

fn zone_from_wit(z: UiZone) -> Zone {
    match z {
        UiZone::Left => Zone::Left,
        UiZone::Center => Zone::Center,
        UiZone::Right => Zone::Right,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_mode::ModelineService;

    fn ctx() -> (UiCtx, Arc<EventBus>) {
        let bus = Arc::new(EventBus::new());
        (
            UiCtx {
                modeline: Arc::new(ModelineService::new()),
                bus: Arc::clone(&bus),
            },
            bus,
        )
    }

    #[test]
    fn ids_are_namespaced_by_plugin() {
        assert_eq!(namespaced_id(Some("org"), "clock"), "org.clock");
        assert_eq!(namespaced_id(None, "clock"), "clock");
    }

    #[test]
    fn the_builtin_namespace_is_refused() {
        let (c, _bus) = ctx();
        let err = register_segment(&c, "core.mode".into(), UiZone::Left, 0)
            .expect_err("a core.* id must not register");
        assert!(err.contains("core."), "the refusal names the reason: {err}");
        assert!(
            c.modeline
                .snapshot()
                .registry
                .zone_ordered(Zone::Left)
                .is_empty(),
            "nothing was registered — registration is last-write-wins, so a \
             silent success here would unregister the real `core.mode`"
        );
    }

    #[test]
    fn a_registered_segment_is_global_scoped() {
        let (c, _bus) = ctx();
        register_segment(&c, "org.clock".into(), UiZone::Right, 7).unwrap();
        let snap = c.modeline.snapshot();
        let els = snap.registry.zone_ordered(Zone::Right);
        assert_eq!(els.len(), 1);
        assert_eq!(els[0].priority, 7);
        assert!(
            matches!(els[0].scope, Scope::Global),
            "a plugin segment shows in every pane; it has no buffer to scope to"
        );
    }

    #[test]
    fn emit_publishes_on_the_bus_rather_than_writing_the_store() {
        let (c, bus) = ctx();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ModelineElementUpdate>();
        bus.subscribe_typed(tx);
        emit_segment(&c, "org.clock".into(), "◷ 0:14".into());

        let update = rx.try_recv().expect(
            "the update must reach the BUS — writing the service directly leaves \
             the content correct and the screen stale until the next keystroke",
        );
        assert_eq!(update.id.as_str(), "org.clock");
        assert!(matches!(update.key, ModelineKey::Global));
        assert_eq!(update.content.spans.len(), 1);
        assert_eq!(update.content.spans[0].text, "◷ 0:14");
        assert_eq!(update.content.spans[0].role.as_str(), PLUGIN_ROLE);
    }

    #[test]
    fn clearing_publishes_empty_content_which_is_how_an_element_hides() {
        let (c, bus) = ctx();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ModelineElementUpdate>();
        bus.subscribe_typed(tx);
        clear_segment(&c, "org.clock".into());
        let update = rx.try_recv().unwrap();
        assert!(update.content.is_empty());
    }
}
