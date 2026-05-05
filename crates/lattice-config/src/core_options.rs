//! Renderer-agnostic options. Every renderer (TUI today, GUI / web
//! tomorrow) registers these by calling [`register_core_options`]
//! at App startup and holds the returned [`CoreOptions`] for typed
//! reads.
//!
//! What lives here vs. in a renderer crate: an option belongs in
//! `core_options` if its *semantics* are renderer-independent --
//! i.e. any sane renderer will respect it (`number`, `tabstop`,
//! `wrap`, `foldmethod`, ...). Renderer-specific styling
//! (`ui.separator_color`, `ui.statusline_active_fg`) registers
//! from the renderer's own `register_*_options` function.

use lattice_core::FoldMethod;

use crate::option::{Option, OptionHandle};
use crate::registry::ConfigRegistry;

/// Typed handles to every core option. Returned by
/// [`register_core_options`]; the App holds one of these and
/// reads via `config.get(handles.foo)` on hot paths.
pub struct CoreOptions {
    pub number: OptionHandle<bool>,
    pub relativenumber: OptionHandle<bool>,
    pub wrap: OptionHandle<bool>,
    pub ignorecase: OptionHandle<bool>,
    pub tabstop: OptionHandle<i64>,
    pub foldenable: OptionHandle<bool>,
    pub foldmethod: OptionHandle<FoldMethod>,
    pub scrolloff: OptionHandle<i64>,
    pub completion_auto_insert_single: OptionHandle<bool>,
    /// Priority bucket for the LSP source. Higher buckets sort
    /// above lower; ties broken by the matcher score + frequency
    /// bonus per `docs/insert-completion.md` §3.6. Defaults
    /// follow §3.4: lsp 200, snippet 150, buffer-words 100.
    pub completion_source_lsp_priority: OptionHandle<i64>,
    pub completion_source_snippet_priority: OptionHandle<i64>,
    pub completion_source_buffer_words_priority: OptionHandle<i64>,
    pub completion_source_tree_sitter_priority: OptionHandle<i64>,
    pub completion_source_path_priority: OptionHandle<i64>,
    pub completion_extra_commit_chars: OptionHandle<String>,
}

/// Register every renderer-agnostic option against `registry` and
/// hand back the typed handle struct. Idempotent only by
/// duplication — calling twice panics on the first duplicate name
/// (registry's invariant).
pub fn register_core_options(registry: &ConfigRegistry) -> CoreOptions {
    let number = registry.register(
        Option::<bool>::builder("number", true, "Show absolute line numbers in the gutter.")
            .aliases(&["nu"])
            .build(),
    );
    let relativenumber = registry.register(
        Option::<bool>::builder(
            "relativenumber",
            false,
            "Gutter shows distance from the cursor; the cursor's line shows its absolute number.",
        )
        .aliases(&["rnu"])
        .build(),
    );
    let wrap = registry.register(Option::<bool>::new(
        "wrap",
        false,
        "Wrap long lines visually instead of horizontal scrolling.",
    ));
    let ignorecase = registry.register(
        Option::<bool>::builder("ignorecase", false, "Ignore case in search patterns.")
            .aliases(&["ic"])
            .build(),
    );
    let tabstop = registry.register(
        Option::<i64>::builder(
            "tabstop",
            8,
            "Number of spaces a hard tab character renders as.",
        )
        .aliases(&["ts"])
        .validate(|i| {
            if (1..=32).contains(i) {
                Ok(())
            } else {
                // Migration constraint: error wording matches the
                // pre-typed-options setter.
                Err(format!("tabstop out of range [1, 32]: {i}"))
            }
        })
        .build(),
    );
    let foldenable = registry.register(
        Option::<bool>::builder(
            "foldenable",
            true,
            "When false (`:set nofoldenable`, `zi`), every fold renders as open \
             regardless of its closed flag. Closed-state is preserved -- toggling \
             back restores the previous distribution.",
        )
        .aliases(&["fen"])
        .build(),
    );
    let foldmethod = registry.register(
        Option::<FoldMethod>::builder(
            "foldmethod",
            FoldMethod::Manual,
            "How folds are produced: `manual` (zf only), `indent` (auto from \
             indentation), `markdown` (ATX heading nesting), or `syntax` \
             (tree-sitter cascade -- markdown for `.md`, indent otherwise).",
        )
        .aliases(&["fdm"])
        .build(),
    );
    let scrolloff = registry.register(
        Option::<i64>::builder(
            "scrolloff",
            0,
            "Minimum visual lines kept above and below the cursor when scrolling.",
        )
        .aliases(&["so"])
        .validate(|i| {
            if (0..=64).contains(i) {
                Ok(())
            } else {
                Err(format!("scrolloff out of range [0, 64]: {i}"))
            }
        })
        .build(),
    );
    let completion_auto_insert_single = registry.register(Option::<bool>::new(
        "completion.auto_insert_single",
        true,
        "When the completion pipeline returns exactly one candidate at \
         popup-open time, insert it directly instead of showing a one-row \
         popup. Only fires at popup-open; narrowing an already-open popup \
         to one candidate while typing does not auto-insert. Disable with \
         `:set nocompletion.auto_insert_single` to always require an \
         explicit confirm.",
    ));
    let priority_validate = |i: &i64| {
        if (0..=9999).contains(i) {
            Ok(())
        } else {
            Err(format!("priority out of range [0, 9999]: {i}"))
        }
    };
    let completion_source_lsp_priority = registry.register(
        Option::<i64>::builder(
            "completion.source.lsp.priority",
            200,
            "Priority bucket for the `gen:lsp-completion` insert-mode \
             completion source. Higher numbers float that source's \
             items above ties from lower-priority sources \
             (`docs/insert-completion.md` §3.4 / §3.6). Default 200; \
             LSP-driven IDE completions usually want to win against \
             local buffer words and snippets at tied score.",
        )
        .validate(priority_validate)
        .build(),
    );
    let completion_source_snippet_priority = registry.register(
        Option::<i64>::builder(
            "completion.source.snippet.priority",
            150,
            "Priority bucket for the `gen:snippet` insert-mode source. \
             Default 150 -- above buffer-words, below LSP. Per-language \
             overrides land in 4.2.g.5 (3/3); today the value is \
             global.",
        )
        .validate(priority_validate)
        .build(),
    );
    let completion_source_buffer_words_priority = registry.register(
        Option::<i64>::builder(
            "completion.source.buffer-words.priority",
            100,
            "Priority bucket for the `gen:buffer-words` insert-mode \
             source. Default 100 -- baseline; LSP and snippets both \
             outrank it at tied matcher score.",
        )
        .validate(priority_validate)
        .build(),
    );
    let completion_source_tree_sitter_priority = registry.register(
        Option::<i64>::builder(
            "completion.source.tree-sitter.priority",
            80,
            "Priority bucket for the `gen:tree-sitter-symbol` \
             insert-mode source -- definition-position identifiers \
             pulled from the buffer's syntax tree. Default 80, \
             below buffer-words: when LSP is attached for the \
             language, the LSP source has the same names with \
             richer metadata.",
        )
        .validate(priority_validate)
        .build(),
    );
    let completion_source_path_priority = registry.register(
        Option::<i64>::builder(
            "completion.source.path.priority",
            90,
            "Priority bucket for the `gen:path` insert-mode \
             source -- filesystem entries surfaced when the \
             cursor sits inside a string literal. Default 90 \
             per spec §3.4: below buffer-words 100 (which often \
             matches partial paths too) and above tree-sitter 80.",
        )
        .validate(priority_validate)
        .build(),
    );
    let completion_extra_commit_chars = registry.register(Option::<String>::new(
        "completion.extra_commit_chars",
        String::new(),
        "Editor-side commit characters unioned with each LSP \
         server's per-item `commitCharacters`. When the \
         insert-completion popup is open and the user types \
         one of these characters, the focused candidate is \
         accepted before the character is inserted. Default \
         empty -- only LSP-supplied commit chars fire. Set \
         to e.g. `\".,;\"` to accept on any of those keys \
         globally.",
    ));
    CoreOptions {
        number,
        relativenumber,
        wrap,
        ignorecase,
        tabstop,
        foldenable,
        foldmethod,
        scrolloff,
        completion_auto_insert_single,
        completion_source_lsp_priority,
        completion_source_snippet_priority,
        completion_source_buffer_words_priority,
        completion_source_tree_sitter_priority,
        completion_source_path_priority,
        completion_extra_commit_chars,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn register_core_options_returns_handles_to_all_options() {
        let r = ConfigRegistry::new();
        let h = register_core_options(&r);
        assert!(*r.get(h.number));
        assert!(!*r.get(h.relativenumber));
        assert!(!*r.get(h.wrap));
        assert!(!*r.get(h.ignorecase));
        assert_eq!(*r.get(h.tabstop), 8);
        assert!(*r.get(h.foldenable));
        assert_eq!(*r.get(h.foldmethod), FoldMethod::Manual);
        assert_eq!(*r.get(h.scrolloff), 0);
        assert!(*r.get(h.completion_auto_insert_single));
        assert_eq!(*r.get(h.completion_source_lsp_priority), 200);
        assert_eq!(*r.get(h.completion_source_snippet_priority), 150);
        assert_eq!(*r.get(h.completion_source_buffer_words_priority), 100);
        assert_eq!(*r.get(h.completion_source_tree_sitter_priority), 80);
        assert_eq!(*r.get(h.completion_source_path_priority), 90);
        assert_eq!(r.get(h.completion_extra_commit_chars).as_str(), "");
    }

    #[test]
    fn completion_source_priority_validate_rejects_out_of_range() {
        let r = ConfigRegistry::new();
        let h = register_core_options(&r);
        assert!(r.set(h.completion_source_lsp_priority, -1).is_err());
        assert!(r.set(h.completion_source_lsp_priority, 10_000).is_err());
        assert!(r.set(h.completion_source_lsp_priority, 0).is_ok());
        assert!(r.set(h.completion_source_lsp_priority, 9999).is_ok());
    }

    #[test]
    fn registered_options_are_lookable_by_alias() {
        let r = ConfigRegistry::new();
        register_core_options(&r);
        assert_eq!(r.lookup("nu").unwrap().name(), "number");
        assert_eq!(r.lookup("rnu").unwrap().name(), "relativenumber");
        assert_eq!(r.lookup("ic").unwrap().name(), "ignorecase");
        assert_eq!(r.lookup("ts").unwrap().name(), "tabstop");
        assert_eq!(r.lookup("fen").unwrap().name(), "foldenable");
        assert_eq!(r.lookup("fdm").unwrap().name(), "foldmethod");
        assert_eq!(r.lookup("so").unwrap().name(), "scrolloff");
    }

    #[test]
    fn tabstop_validate_rejects_out_of_range_with_legacy_message() {
        let r = ConfigRegistry::new();
        register_core_options(&r);
        let err = r.parse_and_set_command("tabstop=99").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("tabstop out of range [1, 32]: 99"));
    }

    #[test]
    fn foldmethod_parse_error_preserves_legacy_wording() {
        let r = ConfigRegistry::new();
        register_core_options(&r);
        let err = r.parse_and_set_command("foldmethod=xyz").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("expected `manual`, `indent`, `markdown`, or `syntax`"));
    }
}
