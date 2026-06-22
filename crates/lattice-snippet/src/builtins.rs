//! Built-in snippet packs embedded in the binary.
//!
//! Bundled friendly-snippets-compatible JSON packs (the same
//! TextMate format [`crate::load::load_pack`] reads) so a fresh
//! editor has a useful starter set without the user installing
//! anything. They load at boot into the shared
//! [`SnippetRegistry`]; user packs in `~/.config/lattice/snippets`
//! overlay them via `:reload-snippets`.
//!
//! Adding a language: drop `<language>.json` under
//! `crates/lattice-snippet/snippets/` and add an `include_str!`
//! line to [`builtin_packs`]. `_global.json` maps to the `"*"`
//! all-languages bucket.

use crate::load::load_pack_from_str;
use crate::registry::SnippetRegistry;

/// `(language, pack-json)` for every embedded pack. `"*"` is the
/// all-languages bucket (the snippet source walks it alongside the
/// active language).
pub fn builtin_packs() -> &'static [(&'static str, &'static str)] {
    &[
        ("rust", include_str!("../snippets/rust.json")),
        ("*", include_str!("../snippets/_global.json")),
    ]
}

/// Build a fresh registry holding every built-in pack.
pub fn load_builtins() -> SnippetRegistry {
    let mut registry = SnippetRegistry::new();
    load_builtins_into(&mut registry);
    registry
}

/// Overlay the built-in packs onto an existing registry. Used by
/// `:reload-snippets` to seed the rebuild before user packs (so
/// user packs augment / override the built-ins rather than the
/// reload wiping them).
///
/// A pack that fails to parse is skipped: it cannot happen in a
/// shipped binary — `builtin_packs_all_parse` guards every pack at
/// test time — but the runtime path stays defensive rather than
/// panicking on the hot-adjacent boot path.
pub fn load_builtins_into(registry: &mut SnippetRegistry) {
    for (language, json) in builtin_packs() {
        if let Ok(snippets) = load_pack_from_str(json) {
            for snippet in snippets {
                registry.insert(language, snippet);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every embedded pack must parse — a malformed bundled pack
    /// is a build-time bug, caught here instead of silently
    /// dropping snippets in the user's editor.
    #[test]
    fn builtin_packs_all_parse() {
        for (language, json) in builtin_packs() {
            load_pack_from_str(json)
                .unwrap_or_else(|e| panic!("builtin pack `{language}` must parse: {e}"));
        }
    }

    #[test]
    fn load_builtins_indexes_rust_and_global() {
        let reg = load_builtins();
        assert!(!reg.is_empty());
        // A known rust prefix resolves.
        assert!(
            !reg.lookup("rust", "for").is_empty(),
            "rust `for` snippet present"
        );
        // A known global prefix resolves under the "*" bucket.
        assert!(
            !reg.lookup("*", "todo").is_empty(),
            "global `todo` snippet present"
        );
    }
}
