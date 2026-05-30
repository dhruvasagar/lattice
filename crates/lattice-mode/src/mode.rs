//! The `Mode` trait, plus `ModeId`, `ModeKind`, and the
//! [`LifecycleFuture`] type alias.

use std::any::Any;
use std::future::Future;
use std::pin::Pin;

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
/// a distribution unit").
///
/// Naming convention: mode names always end in `-mode`. Group
/// names (M.2) never end in `-mode`. The disambiguation rule
/// in mode-architecture.md §6.7.1 depends on this convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModeId(Intern<String>);

impl ModeId {
    /// Intern `name`. Two calls with equal strings produce equal
    /// `ModeId`s; the underlying allocation is shared.
    pub fn new(name: &str) -> Self {
        Self(Intern::new(name.to_string()))
    }

    /// Borrow the canonical name. Stable for the program's
    /// lifetime.
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

/// Pinned, boxed, send-able future for `Mode::on_activate`.
///
/// The explicit `Pin<Box<dyn Future + Send>>` desugaring (rather
/// than `async fn` in trait) is needed because:
///
/// 1. **Object safety.** [`Mode`] has an associated type
///    ([`Mode::Guard`]) and is not directly object-safe. The
///    dispatcher stores modes as `Arc<dyn DynMode>` via the
///    [`DynMode`](crate::DynMode) adapter; the adapter's
///    `on_activate_dyn` returns a future whose output is
///    type-erased to `Box<dyn Any + Send>`.
/// 2. **`Send` bound.** Lifecycle futures may be scheduled across
///    threads (M-async.2 swaps `poll_now` for runtime-spawned
///    `.await`); the future itself must be `Send` so the executor
///    can move it between worker threads.
/// 3. **Explicit lifetime.** Modes capture their `&self` and the
///    [`ModeContext`] (owned, `Send + 'static`); the future's
///    lifetime is tied to `&self` via `'a`.
///
/// The default type parameter `T = ()` lets marker modes write
/// `LifecycleFuture<'_>` without naming the unit type.
pub type LifecycleFuture<'a, T = ()> =
    Pin<Box<dyn Future<Output = Result<T, ModeActivationError>> + Send + 'a>>;

/// Declarative mode contract.
///
/// Per mode-architecture.md §5.2 + §7.1, this trait splits into
/// three concerns:
///
/// 1. **Declarative methods** (`options`, `keymap`,
///    `subscriptions`, `decorations`, `required_capabilities`,
///    `conflicts_with`, `implies`, `completion_sources`,
///    `mirrors_option`) return read-only data. The registry
///    applies these to the layer stack on activation and removes
///    them on deactivation. The mode can never leak contributions
///    past its lifetime by construction.
/// 2. **Lifecycle hook** ([`Mode::on_activate`]) returns an
///    owned [`Guard`](Mode::Guard) value carrying every resource
///    the mode allocated (subscription IDs, prior option values
///    to restore, supervisor handles, etc.). The dispatcher
///    stashes the Guard in a [`GuardStore`](crate::GuardStore)
///    keyed by `(BufferId, ModeId)`.
/// 3. **Deactivation cleanup.** There is **no `on_deactivate`**.
///    On deactivation the dispatcher drops the stashed Guard;
///    the Guard's `Drop` impl performs every cleanup action.
///    This makes cleanup mandatory (compiler-enforced via
///    Rust ownership), bug-resistant (a forgotten cleanup step
///    becomes a compile-time leak rather than a runtime resource
///    leak), and uniform (marker modes use `()` as Guard).
///
/// Validated against Zed's `Subscription` / `Task<T>` cancel-on-
/// drop pattern and helix's Rust-ownership-based cleanup; see
/// mode-architecture.md §7.1.
///
/// `Send + Sync + 'static` so a single trait object can be shared
/// across threads (the registry runs on whatever task drives
/// activation; subscribers can be on any task).
pub trait Mode: Send + Sync + 'static {
    /// Owned cleanup token returned by [`Self::on_activate`].
    ///
    /// The mode allocates whatever resources it needs (event
    /// subscriptions, supervisor handles, prior option values
    /// to restore) and packages them in a Guard struct with a
    /// `Drop` impl that performs cleanup. Marker modes that
    /// have no cleanup work use `()`.
    ///
    /// `Send + 'static` so the dispatcher can stash the Guard
    /// in a typed-erased `Box<dyn Any + Send>` and move it
    /// across threads if needed.
    type Guard: Send + 'static;

    /// Canonical identity. Same value every call.
    fn id(&self) -> ModeId;

    /// Major / minor.
    fn kind(&self) -> ModeKind;

    /// Option overrides this mode contributes. Pure declarative
    /// (same return value every call); the registry merges these
    /// into the resolution layer stack on activation.
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }

    /// Keymap chord -> command additions / overrides. Layered
    /// into the existing keymap registry at this mode's priority
    /// slot.
    fn keymap(&self) -> Keymap {
        Keymap::default()
    }

    /// Typed event subscriptions registered alongside the mode;
    /// deregistered on deactivation.
    fn subscriptions(&self) -> Vec<Subscription> {
        Vec::new()
    }

    /// Decoration providers (gutter / inline / overlay /
    /// statusline).
    fn decorations(&self) -> Vec<DecorationProvider> {
        Vec::new()
    }

    /// Insert-mode completion sources this mode contributes while
    /// active on a buffer. Empty by default; minors that own a
    /// completion source (`lsp-completion-mode`,
    /// `snippet-completion-mode`, `buffer-words-mode`,
    /// `tree-sitter-completion-mode`, `path-completion-mode`,
    /// plugin sources) override.
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

    /// Declarative mirror hint for "this mode is the on/off
    /// switch for a typed option of the same observable state".
    /// `Some(canonical_name)` ⇒ a host-driven cascade keeps the
    /// mode's active state and the option's value in sync.
    fn mirrors_option(&self) -> Option<&'static str> {
        None
    }

    /// 2026-05-26: invocation-runner discovery. Modes that own
    /// command-invocation dispatch for their buffer kind
    /// (terminal-mode, oil-mode, file-tree-mode, help-mode, …)
    /// return their canonical [`ModeId`]; the host registers a
    /// runner function under that id at boot, and
    /// `Editor::run_invocation` looks it up by walking the
    /// active modes on the active pane's buffer (minors first,
    /// then major) before falling back to the central grammar
    /// Action gate.
    ///
    /// Returning `None` (the default) means the mode doesn't
    /// claim invocation dispatch — the keymap / decorations /
    /// completion-source contributions still apply.
    ///
    /// Replaces the hardcoded `match BufferKind` block that
    /// previously lived in `Editor::run_invocation`. Plugin-
    /// installed modes for plugin-installed buffer kinds now
    /// extend the dispatcher without touching host code.
    fn invocation_runner(&self) -> Option<ModeId> {
        None
    }

    /// Lifecycle. Called once per (buffer, activation) cycle
    /// after the registry has applied the declarative
    /// contributions. Returns an owned [`Guard`](Self::Guard)
    /// carrying every resource the mode allocated. The
    /// dispatcher stashes the Guard until deactivation, at which
    /// point dropping it performs cleanup via the Guard's `Drop`
    /// impl.
    ///
    /// Marker modes whose `Guard = ()` typically write:
    ///
    /// ```ignore
    /// type Guard = ();
    /// fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
    ///     Box::pin(async { Ok(()) })
    /// }
    /// ```
    ///
    /// Stateful modes return a Guard struct whose `Drop` impl
    /// performs cleanup (unsubscribe, restore prior option,
    /// drop supervisor handle, etc.).
    ///
    /// Errors propagate as [`ModeActivationError`]; do not panic.
    ///
    /// Idempotent setup contract: `on_activate` may run more
    /// than once in a buffer's lifetime (each preceded by a
    /// Guard-drop if previously active). Implementations must
    /// produce a fresh Guard every time.
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard>;
}

/// Object-safe adapter for `Mode`. The registry stores modes
/// as `Arc<dyn DynMode>`; the blanket impl below box-erases
/// each `Mode`'s typed `Guard` into `Box<dyn Any + Send>` so
/// the dispatcher can stash heterogeneous Guards in a single
/// [`GuardStore`](crate::GuardStore) and drop them on
/// deactivation.
///
/// Public (not sealed): the trait is implemented automatically
/// for every `Mode`; consumers never implement `DynMode`
/// directly. Exposed in `pub` form because the registry's
/// public API (`Arc<dyn DynMode>`) leaks it.
pub trait DynMode: Send + Sync + 'static {
    fn id(&self) -> ModeId;
    fn kind(&self) -> ModeKind;
    fn options(&self) -> OptionOverrideSet;
    fn keymap(&self) -> Keymap;
    fn subscriptions(&self) -> Vec<Subscription>;
    fn decorations(&self) -> Vec<DecorationProvider>;
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution>;
    fn required_capabilities(&self) -> CapabilitySet;
    fn conflicts_with(&self) -> &[ModeId];
    fn implies(&self) -> &[ModeId];
    fn mirrors_option(&self) -> Option<&'static str>;
    fn invocation_runner(&self) -> Option<ModeId>;

    /// Type-erased lifecycle entry. Returns a future whose
    /// output is the typed Guard erased to `Box<dyn Any + Send>`.
    /// The dispatcher stashes this box keyed by
    /// `(BufferId, ModeId)`; deactivation drops it.
    fn on_activate_dyn<'a>(
        &'a self,
        ctx: ModeContext,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Any + Send>, ModeActivationError>> + Send + 'a>>;
}

impl<M: Mode> DynMode for M {
    fn id(&self) -> ModeId {
        <M as Mode>::id(self)
    }
    fn kind(&self) -> ModeKind {
        <M as Mode>::kind(self)
    }
    fn options(&self) -> OptionOverrideSet {
        <M as Mode>::options(self)
    }
    fn keymap(&self) -> Keymap {
        <M as Mode>::keymap(self)
    }
    fn subscriptions(&self) -> Vec<Subscription> {
        <M as Mode>::subscriptions(self)
    }
    fn decorations(&self) -> Vec<DecorationProvider> {
        <M as Mode>::decorations(self)
    }
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution> {
        <M as Mode>::completion_sources(self)
    }
    fn required_capabilities(&self) -> CapabilitySet {
        <M as Mode>::required_capabilities(self)
    }
    fn conflicts_with(&self) -> &[ModeId] {
        <M as Mode>::conflicts_with(self)
    }
    fn implies(&self) -> &[ModeId] {
        <M as Mode>::implies(self)
    }
    fn mirrors_option(&self) -> Option<&'static str> {
        <M as Mode>::mirrors_option(self)
    }
    fn invocation_runner(&self) -> Option<ModeId> {
        <M as Mode>::invocation_runner(self)
    }

    fn on_activate_dyn<'a>(
        &'a self,
        ctx: ModeContext,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Any + Send>, ModeActivationError>> + Send + 'a>>
    {
        let fut = <M as Mode>::on_activate(self, ctx);
        Box::pin(async move {
            let guard = fut.await?;
            Ok(Box::new(guard) as Box<dyn Any + Send>)
        })
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

    /// A bare `Mode` impl with `Guard = ()` and a trivial
    /// `on_activate`. Confirms `completion_sources()` defaults
    /// to empty.
    #[test]
    fn completion_sources_defaults_to_empty() {
        struct BareMode;
        impl Mode for BareMode {
            type Guard = ();
            fn id(&self) -> ModeId {
                ModeId::new("bare-mode")
            }
            fn kind(&self) -> ModeKind {
                ModeKind::Minor
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }
        assert!(<BareMode as Mode>::completion_sources(&BareMode).is_empty());
    }

    /// A mode that DOES contribute a source returns it through
    /// the new trait method.
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
            fn produce(&self, _ctx: &lattice_completion::InsertContext<'_>) -> Vec<RawCandidate> {
                vec![RawCandidate::plain("stub", CandidateKind::Plain)]
            }
        }
        struct StubMode;
        impl Mode for StubMode {
            type Guard = ();
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
                    popup_filter_chord: None,
                    kind: CompletionSourceKind::Sync(Arc::new(StubSource)),
                }]
            }
            fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
                Box::pin(async { Ok(()) })
            }
        }
        let sources = <StubMode as Mode>::completion_sources(&StubMode);
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id.as_str(), "gen:stub");
        assert_eq!(sources[0].kind.kind_label(), "sync");
    }
}
