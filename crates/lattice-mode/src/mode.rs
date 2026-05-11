//! The `Mode` trait, plus `ModeId` and `ModeKind`.

use internment::Intern;

use crate::capability::CapabilitySet;
use crate::context::ModeContext;
use crate::contributions::{DecorationProvider, Keymap, Subscription};
use crate::error::ModeActivationError;
use lattice_config::OptionOverrideSet;

/// Canonical identity of a mode. Interned-string for `Copy + Eq +
/// Hash` at zero allocation cost on the hot path.
///
/// Two `ModeId`s are equal iff their names are equal; equality is
/// pointer-cheap because `internment::Intern<String>` deduplicates
/// at construction. Across crates, the identity is the *string*,
/// not a Rust type -- this is intentional, since lifecycle events
/// and registry lookups are uniform across built-in and plugin
/// modes (mode-architecture.md §1, "modes are an interface, not
/// a distribution unit"). Compile-time uniqueness for *option*
/// types (the §6.4 types-as-keys model) is a separate concern
/// landed in M.2.
///
/// Naming convention (enforced at registration in M.3+, not in
/// the type itself): mode names always end in `-mode`. Group
/// names (M.2) never end in `-mode`. The disambiguation rule
/// in mode-architecture.md §6.7.1 depends on this convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModeId(Intern<String>);

impl ModeId {
    /// Intern `name`. Two calls with equal strings produce equal
    /// `ModeId`s; the underlying allocation is shared.
    pub fn new(name: &str) -> Self {
        Self(Intern::new(name.to_string()))
    }

    /// Borrow the canonical name. Stable for the program's
    /// lifetime (interned strings live forever per the leak
    /// semantics of `internment::Intern`).
    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl std::fmt::Display for ModeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Major / minor distinction. A buffer has exactly one major and
/// any number of minors active simultaneously
/// (mode-architecture.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeKind {
    Major,
    Minor,
}

/// Declarative mode contract.
///
/// Per mode-architecture.md §5.2, this trait splits into two
/// halves:
///
/// 1. **Declarative methods** (`options`, `keymap`,
///    `subscriptions`, `decorations`, `required_capabilities`,
///    `conflicts_with`, `implies`) return read-only data. The
///    registry, not the mode, applies these to the layer stack on
///    activation and removes them on deactivation. The mode can
///    never leak contributions past its lifetime by construction.
/// 2. **Lifecycle hooks** (`on_activate`, `on_deactivate`) are
///    for side effects only -- spawning a server connection,
///    opening a watcher, allocating a buffer-side cache. They
///    receive a *read-only* [`ModeContext`]: no `&mut Config`,
///    no `&mut Keymap`, no direct LSP / actor access.
///
/// All declarative methods have default impls returning empty;
/// real modes in M.3+ override the ones they care about.
///
/// `Send + Sync + 'static` so a single trait object can be shared
/// across threads (the registry runs on whatever task drives
/// activation; subscribers can be on any task).
pub trait Mode: Send + Sync + 'static {
    /// Canonical identity. Same value every call.
    fn id(&self) -> ModeId;

    /// Major / minor.
    fn kind(&self) -> ModeKind;

    /// Option overrides this mode contributes. Pure declarative
    /// (same return value every call); the registry merges these
    /// into the resolution layer stack on activation. Stub type
    /// in M.1; real type lands with M.2 option resolution.
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }

    /// Keymap chord -> command additions / overrides. Layered
    /// into the existing keymap registry
    /// (`keymap-architecture.md` §5-6) at this mode's priority
    /// slot. Stub type in M.1.
    fn keymap(&self) -> Keymap {
        Keymap::default()
    }

    /// Typed event subscriptions registered alongside the mode;
    /// deregistered on deactivation. Stub type in M.1.
    fn subscriptions(&self) -> Vec<Subscription> {
        Vec::new()
    }

    /// Decoration providers (gutter / inline / overlay /
    /// statusline). Stub type in M.1.
    fn decorations(&self) -> Vec<DecorationProvider> {
        Vec::new()
    }

    /// Insert-mode completion sources this mode contributes while
    /// active on a buffer. Empty by default; minors that own a
    /// completion source (`lsp-completion-mode`,
    /// `snippet-completion-mode`, `buffer-words-mode`,
    /// `tree-sitter-completion-mode`, `path-completion-mode`,
    /// plugin sources) override.
    ///
    /// The host resolves and caches the active source set per
    /// buffer at mode-activation / -deactivation transitions (see
    /// `insert-completion.md` §12.4); the keystroke-frequency
    /// refilter pays an O(1) buffer-local lookup, never a walk
    /// over every active mode. Implementations are therefore
    /// allowed to allocate inside this method -- it runs at mode-
    /// transition rate, not keystroke rate.
    ///
    /// CSM.1 lands the trait method (default empty); CSM.4 --
    /// CSM.8 migrate the existing hardcoded sources into mode
    /// contributions, one source at a time.
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution> {
        Vec::new()
    }

    /// Capabilities the mode requires. Validated at activation;
    /// missing capability ⇒
    /// [`ModeActivationError::MissingCapability`], never silent
    /// skip.
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// Conflicts. Activating this mode auto-deactivates the
    /// listed minor modes, OR fails if a conflicting major is
    /// active.
    fn conflicts_with(&self) -> &[ModeId] {
        &[]
    }

    /// Implies. Activating this mode auto-activates these.
    /// Used by `relative-line-numbers-mode` ⇒ `line-numbers-mode`.
    fn implies(&self) -> &[ModeId] {
        &[]
    }

    /// Lifecycle. Called once per (buffer, activation) cycle
    /// after the registry has applied the declarative contributions.
    /// May start side effects (spawn a server connection, register
    /// a watcher) and may write its own buffer-locals via
    /// [`ModeContext::set_local`]; may NOT mutate the config
    /// registry, keymap registry, or another mode's locals.
    /// Errors propagated as [`ModeActivationError`]; do not panic.
    ///
    /// Idempotent setup contract: `on_activate` may run more
    /// than once in a buffer's lifetime (each preceded by
    /// `on_deactivate` if previously active). Implementations
    /// must check existing state before allocating.
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }

    /// Inverse of `on_activate`. Synchronous from the user's
    /// perspective; resource teardown can continue async
    /// post-event (`mode-architecture.md` §7.1). Subscribers to
    /// `MinorDeactivated` / `MajorExiting` must handle "the
    /// resource the mode managed may still be in mid-shutdown".
    /// Implementations should remove any buffer-locals they
    /// installed during `on_activate` via
    /// [`ModeContext::remove_local`].
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn mode_id_interns_equal_names() {
        let a = ModeId::new("rust-mode");
        let b = ModeId::new("rust-mode");
        assert_eq!(a, b);
        // Pointer-equal because Intern dedups.
        assert_eq!(a.as_str().as_ptr(), b.as_str().as_ptr());
    }

    #[test]
    fn mode_id_distinguishes_different_names() {
        let a = ModeId::new("rust-mode");
        let b = ModeId::new("python-mode");
        assert_ne!(a, b);
    }

    #[test]
    fn mode_id_display_is_the_name() {
        assert_eq!(format!("{}", ModeId::new("lsp-mode")), "lsp-mode");
    }

    /// A bare `Mode` impl that doesn't override
    /// `completion_sources()` gets the default empty list. Mode
    /// crates that don't own a completion source (the majority --
    /// `text-mode`, `rust-mode`, `file-tree-mode`, etc.) rely on
    /// this default and never see the completion machinery.
    #[test]
    fn completion_sources_defaults_to_empty() {
        struct BareMode;
        impl Mode for BareMode {
            fn id(&self) -> ModeId {
                ModeId::new("bare-mode")
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
        }
        assert!(BareMode.completion_sources().is_empty());
    }

    /// A mode that DOES contribute a source returns it through
    /// the new trait method. CSM.4 -- CSM.8 will replace the
    /// stub source with the real `Sync`/`AsyncCompletionSource`
    /// impls from each owning crate.
    #[test]
    fn mode_can_contribute_a_completion_source() {
        use lattice_completion::{
            CompletionSourceContribution, CompletionSourceKind, RawCandidate, SyncCompletionSource,
            candidate::CandidateKind,
        };
        use std::sync::Arc;

        #[derive(Debug)]
        struct StubSource;
        impl SyncCompletionSource for StubSource {
            fn produce(
                &self,
                _ctx: &lattice_completion::InsertContext<'_>,
            ) -> Vec<RawCandidate> {
                vec![RawCandidate::plain("stub", CandidateKind::Plain)]
            }
        }
        struct StubMode;
        impl Mode for StubMode {
            fn id(&self) -> ModeId {
                ModeId::new("stub-mode")
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
                vec![CompletionSourceContribution {
                    id: lattice_completion::SourceId::new("gen:stub"),
                    default_priority: 100,
                    auto_trigger: true,
                    trigger_chars: Vec::new(),
                    kind: CompletionSourceKind::Sync(Arc::new(StubSource)),
                }]
            }
        }
        let sources = StubMode.completion_sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id.as_str(), "gen:stub");
        assert_eq!(sources[0].kind.kind_label(), "sync");
    }
}
