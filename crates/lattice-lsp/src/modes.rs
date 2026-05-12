//! LSP modes.
//!
//! - `lsp-mode` (minor; M.5.0) -- the umbrella gate. When active
//!   on a buffer, LSP traffic flows: requests are issued,
//!   diagnostics are applied, document sync runs. When inactive,
//!   every LSP entry point is a silent no-op for that buffer.
//!   Activation lifecycle (didOpen / didClose) wired in M.5.3.
//!
//! - LSP **sub-modes** (minors; M.6.0) -- one per LSP feature
//!   surface. Each is independently toggleable on top of
//!   `lsp-mode`; the umbrella is the gate, the sub-modes are
//!   per-feature switches. M.6.0 ships pure declarations +
//!   registration; M.6.1 wires capability-driven auto-activation
//!   (when the umbrella turns on, sub-modes whose capability the
//!   attached server advertises auto-activate); M.6.2/M.6.3 wire
//!   per-feature gates at the request entry points and the
//!   diagnostic / completion-source pipelines.
//!
//!   The nine sub-modes:
//!   - `lsp-completion-mode` -- LSP-driven insert-mode completion +
//!     palette `:complete` issuing.
//!   - `lsp-diagnostics-mode` -- inline + gutter diagnostic paint;
//!     `:diag-next` / `:diag-prev` navigation.
//!   - `lsp-hover-mode` -- `K` hover popup.
//!   - `lsp-signature-mode` -- auto signature help on `(` / `,`.
//!   - `lsp-format-mode` -- `:lsp-format` / `:lsp-format-range` +
//!     `textDocument/onTypeFormatting` + format-on-save.
//!   - `lsp-rename-mode` -- `:lsp-rename` + workspaceEdit apply.
//!   - `lsp-symbols-mode` -- `:lsp-symbols`,
//!     `:lsp-workspace-symbol`.
//!   - `lsp-code-action-mode` -- `:lsp-code-action`.
//!   - `lsp-nav-mode` -- go-to definition / declaration / type-def
//!     / implementation; references.
//!
//! - `lsp-log-mode` / `lsp-trace-log-mode` / `lsp-server-log-mode`
//!   (majors; M.3.0) -- the read-only buffers backing the LSP
//!   observability surfaces:
//!   - `lsp-log-mode` -- the per-server `*lsp:<server>*` log
//!     (records produced by `LspLogger::log`).
//!   - `lsp-trace-log-mode` -- per-server JSON-RPC wire trace
//!     (`*lsp:<server>:trace*`); only populated when
//!     `:lsp-trace <server>` is on.
//!   - `lsp-server-log-mode` -- per-server stderr feed
//!     (`*lsp:<server>:server*`).
//!
//! Each log major's `decorations()` impl will become non-empty
//! when M.4's renderer pipeline consumes them; today they're
//! pure declarations.

use lattice_mode::{
    CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry,
    OptionOverrideSet,
};

/// All three LSP log majors are read-only buffers (records
/// stream in from the LSP subsystem; the user navigates but
/// doesn't edit). Each contributes `ReadOnly = true` via its
/// declarative options.
macro_rules! lsp_log_mode {
    ($struct_name:ident, $mode_name:literal) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn mode_id() -> ModeId {
                ModeId::new($mode_name)
            }
        }

        impl Mode for $struct_name {
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Major
            }
            fn options(&self) -> OptionOverrideSet {
                lattice_config::overrides! {
                    lattice_config::ReadOnly = true,
                }
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
            fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
        }
    };
}

lsp_log_mode!(LspLogMode, "lsp-log-mode");
lsp_log_mode!(LspTraceLogMode, "lsp-trace-log-mode");
lsp_log_mode!(LspServerLogMode, "lsp-server-log-mode");

/// `lsp-mode` -- the umbrella minor that gates LSP traffic on a
/// buffer. M.5.0 ships the pure declaration; M.5.3 wires the
/// activation / deactivation lifecycle (attach / didOpen on
/// activate; detach / didClose on deactivate). Subsequent M.5
/// slices route every LSP entry point (request issuing, document
/// sync, diagnostic rendering, completion source) through the
/// gate.
///
/// Capabilities are intentionally empty: standalone-server use
/// cases (snippets, scratch buffers without a backing file) want
/// to activate `lsp-mode` on un-named buffers. The capability
/// lattice can tighten in a later slice once we have a clearer
/// per-server minimum-requirement story.
/// `lsp-mode` -- the umbrella minor. Stores the sub-mode id
/// list so `Mode::implies()` can return a slice that lives
/// for `&self`'s lifetime (Phase 3: cascade activation lives
/// in the registry now, driven by `implies()`).
pub struct LspMode {
    /// The 13 LSP sub-modes the umbrella cascades to. Built
    /// once at `LspMode::new()`; `implies()` returns a slice
    /// against this Vec.
    sub_modes: Vec<ModeId>,
}

impl LspMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-mode")
    }

    pub fn new() -> Self {
        Self {
            sub_modes: vec![
                LspCompletionMode::mode_id(),
                LspDiagnosticsMode::mode_id(),
                LspHoverMode::mode_id(),
                LspSignatureMode::mode_id(),
                LspFormatMode::mode_id(),
                LspRenameMode::mode_id(),
                LspSymbolsMode::mode_id(),
                LspCodeActionMode::mode_id(),
                LspNavMode::mode_id(),
                LspProgressMode::mode_id(),
                LspDocumentHighlightMode::mode_id(),
                LspSelectionRangeMode::mode_id(),
                LspFoldingMode::mode_id(),
                LspInlayHintMode::mode_id(),
            ],
        }
    }
}

impl Default for LspMode {
    fn default() -> Self {
        Self::new()
    }
}

impl Mode for LspMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn implies(&self) -> &[ModeId] {
        // Phase 3: the umbrella cascade lives in
        // `Mode::implies()` -- the registry walks this on
        // `activate_minor` and (with the matching extension)
        // on `deactivate_minor`. Eliminates the App-side
        // `activate_lsp_sub_modes_for` / `deactivate_lsp_sub_modes_for`.
        &self.sub_modes
    }
    fn options(&self) -> OptionOverrideSet {
        // `lsp-mode` doesn't contribute any typed options today.
        // The gate is checked via direct mode-state lookup
        // (`App::lsp_mode_enabled_for`) rather than through a
        // resolved option, since the "is this mode active?"
        // question is the gate itself, not a knob the mode
        // happens to flip.
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        // Phase 2: publish `LspBufferAttached` from the mode's
        // own lifecycle hook via `ctx.events()`. The App's
        // `on_lsp_mode_activated` used to do this; now it just
        // cascades sub-mode activation (Phase 3 will move the
        // cascade in here too once `ModeContext` exposes a
        // cascade primitive).
        ctx.events().publish_typed(crate::events::LspBufferAttached {
            id: lattice_protocol::ids::DocumentId::new(
                ctx.buffer_id().0 as u64,
            ),
        });
        Ok(())
    }
    fn on_deactivate(&self, ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        // Phase 2: symmetric to `on_activate`. Wire-level
        // `didClose` still runs from the App (Phase 3 needs
        // `ctx.service::<LspSupervisorHandle>()`).
        ctx.events().publish_typed(crate::events::LspBufferDetached {
            id: lattice_protocol::ids::DocumentId::new(
                ctx.buffer_id().0 as u64,
            ),
        });
        Ok(())
    }
}

/// M.6.0: declare an LSP sub-mode. Each sub-mode is a minor with
/// no contributed options, no capability requirements, and no-op
/// lifecycle hooks. The hooks stay no-op even after M.6.1 lands
/// auto-activation -- the *cascade* (umbrella → sub-modes) lives
/// on the App, not in `Mode::on_activate` (which only sees a
/// `ModeContext`, not the LSP servers + capabilities). Sub-modes
/// are pure markers; the gating logic lives at the request
/// entry points and the publish-diagnostics / completion-source
/// sites that consult `App::<feature>_mode_enabled_for`.
macro_rules! lsp_sub_mode {
    ($struct_name:ident, $mode_name:literal) => {
        pub struct $struct_name;

        impl $struct_name {
            pub fn mode_id() -> ModeId {
                ModeId::new($mode_name)
            }
        }

        impl Mode for $struct_name {
            fn id(&self) -> ModeId {
                Self::mode_id()
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn options(&self) -> OptionOverrideSet {
                OptionOverrideSet::default()
            }
            fn required_capabilities(&self) -> CapabilitySet {
                CapabilitySet::empty()
            }
            fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
            fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
                Ok(())
            }
        }
    };
}

// `LspCompletionMode` lives in `crate::completion` (CSM.8a) --
// it's source-contributing rather than a pure marker, so the
// macro-generated unit struct + Mode impl don't suffice. The
// hand-written impl + `register_lsp_completion_mode` helper
// take its place.
pub use crate::completion::LspCompletionMode;
lsp_sub_mode!(LspDiagnosticsMode, "lsp-diagnostics-mode");
lsp_sub_mode!(LspHoverMode, "lsp-hover-mode");
lsp_sub_mode!(LspSignatureMode, "lsp-signature-mode");
lsp_sub_mode!(LspFormatMode, "lsp-format-mode");
lsp_sub_mode!(LspRenameMode, "lsp-rename-mode");
lsp_sub_mode!(LspSymbolsMode, "lsp-symbols-mode");
lsp_sub_mode!(LspCodeActionMode, "lsp-code-action-mode");
lsp_sub_mode!(LspNavMode, "lsp-nav-mode");
// 4.4.c: progress is a workspace-wide concern (one bar per
// server token, surfaced in the modeline). It's still a per-
// buffer sub-mode because the activation cascade matches the
// rest of the LSP family — turning `lsp-mode` off on a buffer
// quiets every channel, progress included; `:lsp-progress-mode`
// (toggle) keeps everything else but stops accumulating
// progress for that buffer.
lsp_sub_mode!(LspProgressMode, "lsp-progress-mode");
// 4.4.e: `documentHighlight` references at the cursor +
// `selectionRange` smart-expansion. Both are
// position-driven and on by default; toggle off when their
// overlays / expansion clobber other plugins.
lsp_sub_mode!(LspDocumentHighlightMode, "lsp-document-highlight-mode");
lsp_sub_mode!(LspSelectionRangeMode, "lsp-selection-range-mode");
// 4.4.g: `textDocument/inlayHint` virtual-text overlay --
// type / parameter annotations rendered inline with the
// buffer's actual characters. Per-buffer cache lives on
// the App keyed by (BufferId, doc_version); renderer
// splices each hint label as a virtual span.
lsp_sub_mode!(LspInlayHintMode, "lsp-inlay-hint-mode");
// 4.4.f: `textDocument/foldingRange` feeding `FoldMethod::Lsp`.
// Coupled to the `foldmethod` option: activating the mode
// stashes the prior value and swaps `foldmethod` to `lsp`;
// deactivating restores. The toggle command is the bare mode
// name (`:lsp-folding-mode`); there's no separate `:disable`.
//
// Hand-written (not macro-generated) because the lifecycle
// hooks do real work -- they read the config registry and
// the buffer-local stash via [`ModeContext`]. Anyone who
// activates `lsp-folding-mode` -- direct toggle, the
// `lsp-mode` cascade, a plugin via the registry API -- gets
// the foldmethod sync for free; the mode is responsible for
// its own work, not the App.
pub struct LspFoldingMode;

impl LspFoldingMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-folding-mode")
    }
}

impl Mode for LspFoldingMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(
        &self,
        ctx: &mut ModeContext<'_>,
    ) -> Result<(), ModeActivationError> {
        if let Some(prior) = crate::folding_sync::on_activate(ctx.config()) {
            // Stash the prior `foldmethod` so deactivate can
            // restore it. `set_local` enforces the
            // `OWNER_MODE = "lsp-folding-mode"` rule.
            //
            // Idempotent: when this mode is already active and
            // someone re-activates (the registry's
            // `activate_minor` short-circuits, so we wouldn't
            // be here, but be defensive anyway), we DO NOT
            // overwrite the stash -- the `on_activate` helper
            // already returned `None` if the option was already
            // `Lsp`, so reaching this branch means we just did
            // the swap.
            ctx.set_local(crate::folding_sync::PriorFoldmethod(prior))?;
        }
        Ok(())
    }
    fn on_deactivate(
        &self,
        ctx: &mut ModeContext<'_>,
    ) -> Result<(), ModeActivationError> {
        // Take the stash; restore via the helper. No-op when
        // the stash is missing (mode was never activated, or
        // activate skipped the stash because the option was
        // already `Lsp`).
        let prior = ctx
            .remove_local::<crate::folding_sync::PriorFoldmethod>()?;
        if let Some(p) = prior {
            crate::folding_sync::on_deactivate(ctx.config(), p.0);
        }
        Ok(())
    }
}

/// Register every LSP mode (the three log majors, the umbrella
/// `lsp-mode` minor, and the nine M.6 sub-mode minors) against
/// `registry`. The name is kept for backwards compatibility --
/// any existing call sites continue to compile, and the function
/// is the single boot-time registration entry point for
/// everything LSP-related.
pub fn register_lsp_log_modes(registry: &mut ModeRegistry) {
    registry
        .register(LspLogMode)
        .expect("lsp-log-mode register");
    registry
        .register(LspTraceLogMode)
        .expect("lsp-trace-log-mode register");
    registry
        .register(LspServerLogMode)
        .expect("lsp-server-log-mode register");
    registry.register(LspMode::new()).expect("lsp-mode register");
    // M.6.0: LSP sub-mode minors. `LspCompletionMode` is
    // source-contributing (CSM.8a) and registered via
    // `register_lsp_completion_mode(registry, lsp_handle)` from
    // the boot path; the rest stay marker minors registered
    // here.
    registry
        .register(LspDiagnosticsMode)
        .expect("lsp-diagnostics-mode register");
    registry
        .register(LspHoverMode)
        .expect("lsp-hover-mode register");
    registry
        .register(LspSignatureMode)
        .expect("lsp-signature-mode register");
    registry
        .register(LspFormatMode)
        .expect("lsp-format-mode register");
    registry
        .register(LspRenameMode)
        .expect("lsp-rename-mode register");
    registry
        .register(LspSymbolsMode)
        .expect("lsp-symbols-mode register");
    registry
        .register(LspCodeActionMode)
        .expect("lsp-code-action-mode register");
    registry
        .register(LspNavMode)
        .expect("lsp-nav-mode register");
    registry
        .register(LspProgressMode)
        .expect("lsp-progress-mode register");
    registry
        .register(LspDocumentHighlightMode)
        .expect("lsp-document-highlight-mode register");
    registry
        .register(LspSelectionRangeMode)
        .expect("lsp-selection-range-mode register");
    registry
        .register(LspFoldingMode)
        .expect("lsp-folding-mode register");
    registry
        .register(LspInlayHintMode)
        .expect("lsp-inlay-hint-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_lsp_mode_has_distinct_id() {
        let ids = [
            LspLogMode::mode_id(),
            LspTraceLogMode::mode_id(),
            LspServerLogMode::mode_id(),
            LspMode::mode_id(),
            // M.6.0 sub-modes.
            LspCompletionMode::mode_id(),
            LspDiagnosticsMode::mode_id(),
            LspHoverMode::mode_id(),
            LspSignatureMode::mode_id(),
            LspFormatMode::mode_id(),
            LspRenameMode::mode_id(),
            LspSymbolsMode::mode_id(),
            LspCodeActionMode::mode_id(),
            LspNavMode::mode_id(),
            LspProgressMode::mode_id(),
            LspDocumentHighlightMode::mode_id(),
            LspSelectionRangeMode::mode_id(),
            LspFoldingMode::mode_id(),
            LspInlayHintMode::mode_id(),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in &ids[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn register_lsp_log_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_lsp_log_modes(&mut registry);
        assert!(registry.is_registered(LspLogMode::mode_id()));
        assert!(registry.is_registered(LspTraceLogMode::mode_id()));
        assert!(registry.is_registered(LspServerLogMode::mode_id()));
        assert!(registry.is_registered(LspMode::mode_id()));
        // M.6.0 sub-modes are picked up by the same registration
        // entry point. `LspCompletionMode` ships via
        // `register_lsp_completion_mode` (CSM.8a) which needs a
        // supervisor handle, so it's *not* asserted here.
        assert!(registry.is_registered(LspDiagnosticsMode::mode_id()));
        assert!(registry.is_registered(LspHoverMode::mode_id()));
        assert!(registry.is_registered(LspSignatureMode::mode_id()));
        assert!(registry.is_registered(LspFormatMode::mode_id()));
        assert!(registry.is_registered(LspRenameMode::mode_id()));
        assert!(registry.is_registered(LspSymbolsMode::mode_id()));
        assert!(registry.is_registered(LspCodeActionMode::mode_id()));
        assert!(registry.is_registered(LspNavMode::mode_id()));
        assert!(registry.is_registered(LspProgressMode::mode_id()));
        assert!(registry.is_registered(LspDocumentHighlightMode::mode_id()));
        assert!(registry.is_registered(LspSelectionRangeMode::mode_id()));
        assert!(registry.is_registered(LspFoldingMode::mode_id()));
        assert!(registry.is_registered(LspInlayHintMode::mode_id()));
    }

    #[test]
    fn each_lsp_sub_mode_is_minor_no_caps_no_options() {
        // M.6.0 invariant: every sub-mode is a pure marker.
        // Capabilities and option contributions are empty; the
        // gating logic lives on the App side at the request
        // entry points + diagnostic / completion-source sites.
        // `LspCompletionMode` is now source-contributing
        // (CSM.8a) -- it's tested separately in
        // `crate::completion`'s own test module.
        let modes: Vec<&dyn Mode> = vec![
            &LspDiagnosticsMode,
            &LspHoverMode,
            &LspSignatureMode,
            &LspFormatMode,
            &LspRenameMode,
            &LspSymbolsMode,
            &LspCodeActionMode,
            &LspNavMode,
            &LspProgressMode,
            &LspDocumentHighlightMode,
            &LspSelectionRangeMode,
            &LspFoldingMode,
            &LspInlayHintMode,
        ];
        for m in modes {
            assert_eq!(m.kind(), ModeKind::Minor, "{} not minor", m.id());
            assert_eq!(
                m.required_capabilities(),
                CapabilitySet::empty(),
                "{} declared caps",
                m.id(),
            );
            assert!(
                m.options().iter().count() == 0,
                "{} contributed options",
                m.id(),
            );
        }
    }

    #[test]
    fn lsp_mode_is_minor_with_no_capability_requirements() {
        // M.5.0: `lsp-mode` is a minor (it overlays the buffer's
        // language major); standalone-server use cases want to
        // activate without a `BUFFER_URI` capability requirement.
        let m = LspMode::new();
        assert_eq!(m.kind(), ModeKind::Minor);
        assert_eq!(m.required_capabilities(), CapabilitySet::empty());
    }

    #[tokio::test]
    async fn lsp_mode_activates_through_registry_as_minor() {
        // Phase 3: activating `lsp-mode` cascades through
        // `implies()` to all 13 sub-modes. The completion
        // sub-mode is hand-written and registered separately
        // via `register_lsp_completion_mode(...)` with a real
        // supervisor handle, so the test needs a tokio
        // runtime to build one.
        use crate::completion::register_lsp_completion_mode;
        use crate::supervisor::LspSupervisor;
        use lattice_mode::{ActiveModes, BufferLocals};
        use lattice_protocol::ids::BufferId;
        let mut registry = ModeRegistry::new();
        register_lsp_log_modes(&mut registry);
        let sup = LspSupervisor::new(crate::LspLogger::with_defaults());
        let lsp_handle = sup.spawn(&tokio::runtime::Handle::current());
        register_lsp_completion_mode(&mut registry, lsp_handle);
        let mut active = ActiveModes::new();
        let mut locals = BufferLocals::new();
        let cfg = lattice_config::ConfigRegistry::new();
        let evt = std::sync::Arc::new(lattice_runtime::EventBus::new());
        let svc = lattice_mode::ServiceRegistry::new();
        registry
            .activate_minor(
                &mut active,
                &mut locals,
                &cfg,
                &evt,
                &svc,
                BufferId::new(1),
                LspMode::mode_id(),
                CapabilitySet::empty(),
            )
            .expect("activate lsp-mode + sub-mode cascade");
        assert!(active.has_minor(LspMode::mode_id()));
        // Phase 3: every sub-mode in `implies()` activated
        // via the registry's cascade. Sample a few.
        assert!(active.has_minor(LspCompletionMode::mode_id()));
        assert!(active.has_minor(LspDiagnosticsMode::mode_id()));
        assert!(active.has_minor(LspFoldingMode::mode_id()));
    }
}
