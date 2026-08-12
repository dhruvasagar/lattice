//! BC.7 (2026-06-24): the crate-owned `install(boot)` entry point.
//!
//! The multibuffer subsystem registers its own **modes + commands + services +
//! off-keystroke wake** through the generic [`SubsystemBoot`] surface,
//! collapsing the host's ~6 scattered `editor_boot` sites into one Phase-B
//! line (`lattice_multibuffer::install(&mut boot)`) — the terminal /
//! claude-code / diff shape.
//!
//! ## What collapses here
//!
//! - **The registry handle** ([`InMemoryMultibufferRegistry::handle`]) is
//!   created *inside* `install` — multibuffer-owned, because it carries **no
//!   host-state dependency** (unlike diff's resolver-backed
//!   `DiffSubsystemHandle` or terminal's host-`BufferRegistry`-backed
//!   `TerminalStoreHandle`, both of which the host must construct). It is used
//!   for mode registration + the excerpt-jump motion handlers, and published
//!   as a service the host reads back at dispatch time
//!   (`Editor::resolve_narrow_target`) via `services.get::<MultibufferRegistryHandle>()`.
//! - **Modes** — `multibuffer-mode` (+ its `DocumentClosed` cleanup
//!   subscriber), `narrow-mode`, and the `search` provider mode.
//! - **Commands** — the excerpt-jump motions (`]e`/`[e`/`]E`/`[E`), the
//!   `:multibuffer-*` ex-commands, `:narrow`/`:widen`, the `zn` narrow
//!   operator SPEC, and the `:search` ex-command.
//! - **Services** — the registry handle + the project-search service.
//! - **The off-keystroke wake** — `boot.wake_on_event::<MultibufferExcerptsReady>()`,
//!   replacing the host's hand-rolled mpsc→`async_landed` forwarder so streamed
//!   search excerpts repaint without a keypress. The wake is now a *property of
//!   the primitive* (BC design §3), not by-discipline.
//!
//! ## What stays host-side (documented residue, NOT mode-ownership violations)
//!
//! - **The `zn` narrow-operator *binding*** at the universal operator-pending
//!   (`Builtin`) layer. The operator *spec/handler* lives here
//!   ([`crate::providers::narrow::register_narrow_operator`]); only the
//!   *binding* is host-side, because `zn` is **universal grammar** — it
//!   composes with the universal motion/text-object vocabulary and needs the
//!   host-resolved `Builtins`. This is the same category as `lattice-syntax`'s
//!   structural text objects (N.1.4c): registered-in-crate, bound-at-`Builtin`.
//!   There is no single "owning mode" for a universal operator. BC.7 decision
//!   (A): the host resolves the operator by name
//!   (`registry.id_by_name("operator:narrow")`), so the registration *return
//!   value* no longer threads through the host — the K.2.5 motion
//!   name-resolution pattern.
//! - **Host `Effect` appliers** — `Editor::resolve_narrow_target` + the
//!   `AppEffect::{SearchTrigger,NarrowTrigger,NarrowLines,NarrowWiden,MultibufferExpand}`
//!   dispatch arms mutate `&mut Editor` (open buffers, pane tree, splits) — the
//!   Effect-vocabulary-is-the-host-boundary rule (diff's `do_diff_*` precedent).
//!   The trigger substrate fns (`project_search`, `create_narrow_view`,
//!   `multibuffer_expand_excerpt_at`) are crate-owned helpers those arms call.
//! - **The generic `event_bus` *service*** stays a Phase-A host primitive —
//!   many subsystems consume it; it is not multibuffer-owned.

use lattice_mode::SubsystemBoot;

/// Wire the multibuffer subsystem's modes + commands + services + wake into the
/// editor at boot. One Phase-B line in `editor_boot.rs`.
pub fn install(boot: &mut impl SubsystemBoot) {
    // The registry handle: crate-owned (no host-state dependency). Captured for
    // mode registration + the motion handlers + published as a service below.
    let registry_handle = crate::registry::InMemoryMultibufferRegistry::handle();
    // Own a clone of the event bus up front: `register_multibuffer_modes` needs
    // `&Arc<EventBus>` *and* `boot.modes_mut()` in one call, so borrowing
    // `boot.event_bus()` across the `&mut` would conflict. The owned local
    // sidesteps it.
    let event_bus = boot.event_bus().clone();

    // ── Modes ───────────────────────────────────────────────────────────────
    // `multibuffer-mode` (H.2 kind-bound to `BufferKind::Multibuffer`) + its
    // `DocumentClosed` cleanup subscriber, which needs the event bus + the
    // registry handle. The subscriber spawns only when a tokio runtime is in
    // scope (production boot); test paths skip it gracefully.
    crate::mode::register_multibuffer_modes(boot.modes_mut(), &event_bus, registry_handle.clone());
    // N.1.1: narrow provider-minor mode (marker for narrow views). First-class.
    crate::providers::narrow::register_narrow_mode(boot.modes_mut());
    // CM.4: problems provider-minor mode (marker for `*problems*` views). First-class.
    crate::providers::problems::register_problems_mode(boot.modes_mut());
    // M.6: project-search provider-minor mode (feature-gated with its provider).
    #[cfg(feature = "search")]
    crate::providers::search::register_project_search_mode(boot.modes_mut());

    // ── Commands ────────────────────────────────────────────────────────────
    // M.2.b.3 / K.2.5: excerpt-jump motions (`]e`/`[e`/`]E`/`[E`). The returned
    // `MultibufferMotionIds` is discarded — `MultibufferMode::keymap()`
    // references the motions by canonical name, resolved at the host's K.2.4
    // translation pass; the registration side-effect (the names in the
    // registry) is what keeps that lookup successful.
    let _ =
        crate::motions::register_multibuffer_motions(boot.commands_mut(), registry_handle.clone());
    crate::mode::register_multibuffer_ex_commands(boot.commands_mut());
    // N.1.1: `:narrow` + `:widen`. First-class — no feature gate.
    crate::providers::narrow::register_narrow_ex_commands(boot.commands_mut());
    // CM.4: `:copen` + `:cclose`. First-class — no feature gate.
    crate::providers::problems::register_problems_ex_commands(boot.commands_mut());
    // N.1.3 / BC.7 (A): register the `zn` narrow operator SPEC. The returned
    // `OperatorId` is discarded — the host's universal operator-pending binding
    // resolves it by name (`operator:narrow`), the motion name-resolution
    // pattern. The binding lives host-side because `zn` is universal grammar
    // that composes with the resolved `Builtins`; only the spec is mode-owned.
    let _ = crate::providers::narrow::register_narrow_operator(boot.commands_mut());
    // M.6: `:search` ex-command (feature-gated with its provider).
    #[cfg(feature = "search")]
    crate::providers::search::register_search_ex_command(boot.commands_mut());

    // ── Services ────────────────────────────────────────────────────────────
    // M.2.b.2: expose the typed multibuffer-handle lookup so providers
    // (`create_multibuffer_view`, the `:search` minor) AND the host's
    // `resolve_narrow_target` reach it via `services.get::<MultibufferRegistryHandle>()`.
    boot.register_service(registry_handle);
    // M.6: the project-search service so `project_search` triggers find it.
    #[cfg(feature = "search")]
    crate::providers::search::register_project_search_service(boot.services_mut());

    // ── Off-keystroke wake ──────────────────────────────────────────────────
    // `MultibufferExcerptsReady` (published by any provider after appending a
    // batch) wakes `async_landed` so the actor republishes render state and the
    // cells worker picks up the new excerpt syntax — without a keypress.
    // Replaces the host's hand-rolled mpsc→notify forwarder; the wake is baked
    // into the primitive (can't-forget). Ordering with the
    // `AsyncRenderStatePublished` → cells bridge is unchanged (that bridge stays
    // host-side, downstream of this wake).
    //
    // PV.1 (2026-08-12): NO LONGER `#[cfg(feature = "search")]`. The event is a
    // property of multibuffer views, not of searching; gating it meant a
    // `--no-default-features` build — and any provider living outside this
    // crate, like magit's project-diff — appended excerpts that only appeared
    // on the next keypress.
    boot.wake_on_event::<crate::events::MultibufferExcerptsReady>();
}
