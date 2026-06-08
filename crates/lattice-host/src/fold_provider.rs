//! D.3.f.0 (2026-05-29): `FoldProvider` substrate.
//!
//! See `docs/dev/architecture/fold-architecture.md` for the
//! full design; capsule:
//!
//! - One primary provider runs per `recompute_folds()` call,
//!   keyed on `:set foldmethod=`.
//! - All overlay providers run on every `recompute_folds()`.
//! - Providers are pure functions of [`FoldContext`]; the
//!   registry pre-loads inputs.
//!
//! This module ships the trait + the registry; the five
//! built-in primary providers wrap the existing
//! [`crate::folds`] helpers. The first overlay consumer
//! (`HunkFoldProvider`) lands in D.3.f.1.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use lattice_core::{Buffer, BufferId, Fold, FoldMethod, ProviderId, ProviderKind};
use lattice_diff::HunkIndex;
use lattice_syntax::SyntaxSnapshot;

/// D.3.f.0: per-recompute inputs passed to every provider.
///
/// The registry pre-loads what providers might need so each
/// `FoldProvider::compute` is a pure function — no
/// back-references to `Editor`, no `&mut` state. Fields are
/// `Option` because not every provider needs every input
/// (the indent provider ignores `syntax`; the LSP provider
/// reads only `lsp_folds`; etc.).
pub struct FoldContext<'a> {
    pub buffer: &'a Buffer,
    pub buffer_id: BufferId,
    pub path: Option<&'a Path>,
    pub syntax: Option<&'a SyntaxSnapshot>,
    pub lsp_folds: Option<&'a [Fold]>,
    pub diff_hunks: Option<&'a HunkIndex>,
}

/// D.3.f.0: a registered source of folds.
///
/// `id()` is stable across recomputes and namespaces the
/// provider's identity hashes (so two providers that emit
/// folds with the same `(start_line, end_line)` don't
/// collide on `Fold::identity`).
pub trait FoldProvider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn kind(&self) -> ProviderKind;
    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold>;
}

/// D.3.f.0: per-`Editor` fold-provider registry.
///
/// One primary per `FoldMethod` (built-ins registered at
/// construction); overlays added/removed by their owning
/// subsystems (`DiffSubsystem` for hunk folds, multibuffer
/// subsystem for excerpt / file-boundary folds in M.7 / M.8).
pub struct FoldRegistry {
    primaries: HashMap<FoldMethod, Arc<dyn FoldProvider>>,
    overlays: Vec<Arc<dyn FoldProvider>>,
}

impl std::fmt::Debug for FoldRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FoldRegistry")
            .field("primaries", &self.primaries.keys().collect::<Vec<_>>())
            .field(
                "overlays",
                &self.overlays.iter().map(|p| p.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl FoldRegistry {
    /// Construct with the five built-in primary providers
    /// already registered. Caller adds overlays as their
    /// subsystems come online.
    pub fn with_builtins() -> Self {
        use crate::folds::{
            IndentPrimary, LspPrimary, ManualPrimary, MarkdownPrimary, SyntaxPrimary,
        };
        let mut primaries: HashMap<FoldMethod, Arc<dyn FoldProvider>> = HashMap::new();
        primaries.insert(FoldMethod::Manual, Arc::new(ManualPrimary));
        primaries.insert(FoldMethod::Indent, Arc::new(IndentPrimary));
        primaries.insert(FoldMethod::Markdown, Arc::new(MarkdownPrimary));
        primaries.insert(FoldMethod::Syntax, Arc::new(SyntaxPrimary));
        primaries.insert(FoldMethod::Lsp, Arc::new(LspPrimary));
        // D.3.f.1 (2026-05-29): the hunk fold overlay is
        // always-on — `compute()` is gated by `ctx.diff_hunks`
        // being `Some`, so it's inert when no diff session
        // backs the active buffer. Pre-seeding here means
        // `Editor::default()` (used by tests) gets the same
        // composition as the editor-boot path without a
        // manual `add_overlay` call.
        let overlays: Vec<Arc<dyn FoldProvider>> =
            vec![Arc::new(crate::diff::fold::HunkFoldProvider)];
        Self {
            primaries,
            overlays,
        }
    }

    /// Look up the primary provider for `method`. Always
    /// returns `Some` for the five built-in methods; future
    /// plugin-registered methods would populate this map
    /// too.
    pub fn primary(&self, method: FoldMethod) -> Option<&Arc<dyn FoldProvider>> {
        self.primaries.get(&method)
    }

    /// Register an overlay provider. Called by subsystems
    /// when they come online (e.g. `DiffSubsystem` registers
    /// `HunkFoldProvider` on `open_session`). Returns the
    /// provider's `ProviderId` so the caller can later
    /// remove it.
    pub fn add_overlay(&mut self, provider: Arc<dyn FoldProvider>) -> ProviderId {
        let id = provider.id();
        // Symmetric remove path uses id; an existing
        // registration with the same id is a programming
        // bug — replace it rather than duplicate.
        if let Some(slot) = self.overlays.iter_mut().find(|p| p.id() == id) {
            *slot = provider;
        } else {
            self.overlays.push(provider);
        }
        id
    }

    /// Remove the overlay with the given id. No-op if not
    /// registered.
    pub fn remove_overlay(&mut self, id: ProviderId) {
        self.overlays.retain(|p| p.id() != id);
    }

    /// Iterate registered overlays in registration order.
    pub fn overlays(&self) -> impl Iterator<Item = &Arc<dyn FoldProvider>> {
        self.overlays.iter()
    }
}

impl Default for FoldRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

/// M.7: wraps a [`lattice_core::FoldSource`] as a `FoldProvider`.
/// `target_buffer_id` scopes this overlay: `compute()` returns
/// empty when `ctx.buffer_id` doesn't match, so providers from
/// multiple simultaneous multibuffers don't bleed into each other.
pub struct FoldSourceAdapter {
    source: Arc<dyn lattice_core::FoldSource>,
    target_buffer_id: BufferId,
}

impl FoldProvider for FoldSourceAdapter {
    fn id(&self) -> ProviderId {
        self.source.id()
    }
    fn kind(&self) -> ProviderKind {
        ProviderKind::Overlay
    }
    fn compute(&self, ctx: &FoldContext<'_>) -> Vec<Fold> {
        if ctx.buffer_id != self.target_buffer_id {
            return Vec::new();
        }
        self.source.compute_folds()
    }
}

/// M.7: [`lattice_core::FoldOverlayService`] impl that wraps the
/// shared `Arc<Mutex<FoldRegistry>>`. `MultibufferMode::on_activate`
/// obtains this via `ctx.service::<FoldOverlayServiceHandle>()` and
/// registers `ExcerptFoldProvider` without depending on
/// `lattice-host`.
pub struct FoldOverlayServiceImpl {
    registry: Arc<std::sync::Mutex<FoldRegistry>>,
}

impl FoldOverlayServiceImpl {
    pub fn new(registry: Arc<std::sync::Mutex<FoldRegistry>>) -> Self {
        Self { registry }
    }
}

impl lattice_core::FoldOverlayService for FoldOverlayServiceImpl {
    fn add_source(
        &self,
        source: Arc<dyn lattice_core::FoldSource>,
        buffer_id: BufferId,
    ) -> ProviderId {
        self.registry
            .lock()
            .expect("fold_registry poisoned")
            .add_overlay(Arc::new(FoldSourceAdapter {
                source,
                target_buffer_id: buffer_id,
            }))
    }

    fn remove_source(&self, id: ProviderId) {
        self.registry
            .lock()
            .expect("fold_registry poisoned")
            .remove_overlay(id);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    struct StaticOverlay {
        id: ProviderId,
        folds: Vec<Fold>,
    }

    impl FoldProvider for StaticOverlay {
        fn id(&self) -> ProviderId {
            self.id
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::Overlay
        }
        fn compute(&self, _ctx: &FoldContext<'_>) -> Vec<Fold> {
            self.folds.clone()
        }
    }

    #[test]
    fn builtins_register_all_five_primaries() {
        let r = FoldRegistry::with_builtins();
        for fm in [
            FoldMethod::Manual,
            FoldMethod::Indent,
            FoldMethod::Markdown,
            FoldMethod::Syntax,
            FoldMethod::Lsp,
        ] {
            assert!(r.primary(fm).is_some(), "missing primary for {:?}", fm);
        }
    }

    #[test]
    fn overlay_add_and_remove_round_trip() {
        let mut r = FoldRegistry::with_builtins();
        // `with_builtins` pre-seeds the always-on
        // HunkFoldProvider (D.3.f.1) — baseline is 1, not 0.
        let baseline = r.overlays().count();
        let id = r.add_overlay(Arc::new(StaticOverlay {
            id: ProviderId(42),
            folds: vec![],
        }));
        assert_eq!(id, ProviderId(42));
        assert_eq!(r.overlays().count(), baseline + 1);
        r.remove_overlay(ProviderId(42));
        assert_eq!(r.overlays().count(), baseline);
    }

    #[test]
    fn add_overlay_with_existing_id_replaces() {
        let mut r = FoldRegistry::with_builtins();
        let baseline = r.overlays().count();
        r.add_overlay(Arc::new(StaticOverlay {
            id: ProviderId(7),
            folds: vec![Fold {
                start_line: 0,
                end_line: 1,
                closed: false,
                identity: None,
            }],
        }));
        r.add_overlay(Arc::new(StaticOverlay {
            id: ProviderId(7),
            folds: vec![],
        }));
        assert_eq!(
            r.overlays().count(),
            baseline + 1,
            "same-id overlay must replace, not duplicate"
        );
    }

    #[test]
    fn with_builtins_pre_seeds_hunk_overlay() {
        // D.3.f.1: HunkFoldProvider lives at id 100.
        let r = FoldRegistry::with_builtins();
        assert!(
            r.overlays()
                .any(|p| p.id() == crate::diff::fold::HUNK_FOLD_PROVIDER_ID),
            "HunkFoldProvider must be pre-seeded so `Editor::default()` matches editor-boot composition"
        );
    }
}
