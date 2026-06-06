//! `ModeId` — canonical interned-string identity for a mode.
//!
//! Lives in `lattice-keymap` so that `KeymapLayer::MajorMode(ModeId)` and
//! `KeymapLayer::MinorMode(ModeId)` can be defined here without creating a
//! circular dependency back up to `lattice-mode`.

use internment::Intern;

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
