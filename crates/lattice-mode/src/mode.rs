//! The `Mode` trait, plus `ModeId` and `ModeKind`.

use internment::Intern;

use crate::capability::CapabilitySet;
use crate::context::ModeContext;
use crate::contributions::{DecorationProvider, Keymap, Subscription};
use crate::error::ModeActivationError;
use crate::overrides::OptionOverrideSet;

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
}
