// `linkme`'s distributed slices use `link_section` to aggregate
// items at link time. The macro expansions in this file emit
// such declarations; allow the workspace's `unsafe_code = "deny"`
// lint locally with the same safety rationale documented in
// `option_decl.rs` and `group.rs`.
#![allow(unsafe_code)]

//! Renderer-agnostic options. M.2.0b migrates these from the
//! pre-typed-keys imperative `Option::builder()` form to the
//! macro-driven declarative form (Design B + D from the
//! `mode-architecture.md` discussion).
//!
//! Each option is a unique Rust type emitted by [`crate::options!`].
//! The macro generates the [`crate::OptionDecl`] / [`crate::HasGroup`]
//! impls, a `build_spec()` helper that constructs the runtime
//! `Option<T>` via the existing builder, and a `linkme`
//! self-registration thunk submitted to [`crate::OPTION_DECLS`].
//! At App boot the registry's [`crate::ConfigRegistry::init_from_linkme`]
//! walks the slice and registers every option without a central
//! `register_core_options` body.
//!
//! For backwards compatibility during the transitional M.2.0b/c
//! window, [`register_core_options`] runs `init_from_linkme` and
//! returns a [`CoreOptions`] struct populated with the typed
//! handles. Existing callers (`config.get(core.tabstop)`)
//! continue to work unchanged. M.2.0c migrates the callers to
//! `config.get_typed::<Tabstop>()` and retires `CoreOptions`.

use lattice_core::FoldMethod;

// Validators referenced by `#[validate(...)]` on the options
// below. Plain Rust functions; the macro just records the path.
fn validate_tabstop(i: &i64) -> Result<(), String> {
    if (1..=32).contains(i) {
        Ok(())
    } else {
        // Migration constraint: error wording matches the
        // pre-typed-options setter (test
        // `tabstop_validate_rejects_out_of_range_with_legacy_message`).
        Err(format!("tabstop out of range [1, 32]: {i}"))
    }
}

fn validate_scrolloff(i: &i64) -> Result<(), String> {
    if (0..=64).contains(i) {
        Ok(())
    } else {
        Err(format!("scrolloff out of range [0, 64]: {i}"))
    }
}

fn validate_completion_priority(i: &i64) -> Result<(), String> {
    if (0..=9999).contains(i) {
        Ok(())
    } else {
        Err(format!("priority out of range [0, 9999]: {i}"))
    }
}

// ---- Editor group: bare-named editor options ----
//
// Reserved namespace per `mode-architecture.md` §6.5.2 / §6.8.
// Bare names (no prefix) are the user's first-class options;
// plugins must use their own `<plugin-id>.` prefix.

crate::options! {
    group = crate::Editor;

    /// Show absolute line numbers in the gutter.
    #[aliases("nu")]
    #[name("number")]
    pub Number: bool = true;

    /// Gutter shows distance from the cursor; the cursor's line
    /// shows its absolute number.
    #[aliases("rnu")]
    #[name("relativenumber")]
    pub RelativeNumber: bool = false;

    /// Wrap long lines visually instead of horizontal scrolling.
    pub Wrap: bool = false;

    /// Ignore case in search patterns.
    #[aliases("ic")]
    #[name("ignorecase")]
    pub IgnoreCase: bool = false;

    /// Number of spaces a hard tab character renders as.
    #[aliases("ts")]
    #[validate(validate_tabstop)]
    pub Tabstop: i64 = 8;

    /// Whether the buffer is read-only (mutating operators
    /// reject; `:w` still permits explicit writes if a path
    /// exists). `customizable = false` because this is
    /// mode-driven, not a user-typed config: major modes like
    /// `help-mode`, `file-tree-mode`, and the LSP log modes
    /// contribute `ReadOnly = true` via `Mode::options()` (per
    /// `mode-architecture.md` §6.5.3 read-only-mode pattern).
    /// Users who want to flip it on / off for a particular
    /// buffer use `:enable read-only-mode` (when that minor
    /// mode lands in M.7), not `:set read-only=true`.
    #[customizable(false)]
    #[name("read-only")]
    pub ReadOnly: bool = false;

    /// When false (`:set nofoldenable`, `zi`), every fold renders
    /// as open regardless of its closed flag. Closed-state is
    /// preserved -- toggling back restores the previous
    /// distribution.
    #[aliases("fen")]
    #[name("foldenable")]
    pub FoldEnable: bool = true;

    /// How folds are produced: `manual` (zf only), `indent` (auto
    /// from indentation), `markdown` (ATX heading nesting), or
    /// `syntax` (tree-sitter cascade -- markdown for `.md`,
    /// indent otherwise).
    #[aliases("fdm")]
    #[name("foldmethod")]
    pub FoldMethodOption: FoldMethod = FoldMethod::Manual;

    /// Minimum visual lines kept above and below the cursor when
    /// scrolling.
    #[aliases("so")]
    #[validate(validate_scrolloff)]
    pub Scrolloff: i64 = 0;

    /// Show whitespace glyphs (trailing spaces, tabs, leading
    /// indentation) as visible markers. Vim's `:set list`.
    /// Backing option for `whitespace-show-mode` (M.7.2). The
    /// renderer's whitespace-painting plumbing lands in M.7.3 --
    /// today this option is read by the cascade and the mode
    /// machinery, but the renderer doesn't yet emit decorations.
    #[aliases("list")]
    pub Whitespace: bool = false;

    /// Highlight the cursor's current line with a different
    /// background style. Vim's `:set cursorline`. Backing option
    /// for `current-line-highlight-mode` (M.7.2). The renderer's
    /// current-line-highlight pipeline lands in M.7.3.
    #[aliases("cul", "cursorline")]
    #[name("current-line-highlight")]
    pub CursorLine: bool = false;
}

// ---- Completion group: insert-completion knobs ----

crate::options! {
    group = crate::Completion;

    /// When the completion pipeline returns exactly one candidate
    /// at popup-open time, insert it directly instead of showing a
    /// one-row popup. Only fires at popup-open; narrowing an
    /// already-open popup to one candidate while typing does not
    /// auto-insert. Disable with `:set nocompletion.auto_insert_single`
    /// to always require an explicit confirm.
    #[name("completion.auto_insert_single")]
    pub CompletionAutoInsertSingle: bool = true;

    /// Priority bucket for the `gen:lsp-completion` insert-mode
    /// completion source. Higher numbers float that source's items
    /// above ties from lower-priority sources
    /// (`docs/insert-completion.md` §3.4 / §3.6). Default 200;
    /// LSP-driven IDE completions usually want to win against
    /// local buffer words and snippets at tied score.
    #[name("completion.source.lsp.priority")]
    #[validate(validate_completion_priority)]
    pub CompletionSourceLspPriority: i64 = 200;

    /// Priority bucket for the `gen:snippet` insert-mode source.
    /// Default 150 -- above buffer-words, below LSP. Per-language
    /// overrides land in 4.2.g.5 (3/3); today the value is global.
    #[name("completion.source.snippet.priority")]
    #[validate(validate_completion_priority)]
    pub CompletionSourceSnippetPriority: i64 = 150;

    /// Priority bucket for the `gen:buffer-words` insert-mode
    /// source. Default 100 -- baseline; LSP and snippets both
    /// outrank it at tied matcher score.
    #[name("completion.source.buffer-words.priority")]
    #[validate(validate_completion_priority)]
    pub CompletionSourceBufferWordsPriority: i64 = 100;

    /// Priority bucket for the `gen:tree-sitter-symbol`
    /// insert-mode source -- definition-position identifiers
    /// pulled from the buffer's syntax tree. Default 80, below
    /// buffer-words: when LSP is attached for the language, the
    /// LSP source has the same names with richer metadata.
    #[name("completion.source.tree-sitter.priority")]
    #[validate(validate_completion_priority)]
    pub CompletionSourceTreeSitterPriority: i64 = 80;

    /// Priority bucket for the `gen:path` insert-mode source --
    /// filesystem entries surfaced when the cursor sits inside a
    /// string literal. Default 90 per spec §3.4: below
    /// buffer-words 100 (which often matches partial paths too)
    /// and above tree-sitter 80.
    #[name("completion.source.path.priority")]
    #[validate(validate_completion_priority)]
    pub CompletionSourcePathPriority: i64 = 90;

    /// Editor-side commit characters unioned with each LSP
    /// server's per-item `commitCharacters`. When the
    /// insert-completion popup is open and the user types one of
    /// these characters, the focused candidate is accepted before
    /// the character is inserted. Default empty -- only
    /// LSP-supplied commit chars fire. Set to e.g. `".,;"` to
    /// accept on any of those keys globally.
    #[name("completion.extra_commit_chars")]
    pub CompletionExtraCommitChars: String = String::new();

    /// Render the top-ranked candidate's suffix as a dimmed
    /// inline overlay after the cursor while the popup is open
    /// (Phase 4.2.g.7 polish). Off by default to keep the live
    /// buffer visually quiet; turn on for a vscode-style preview
    /// of the most likely completion. Only fires when the cursor
    /// sits at end-of-line and the top candidate is a
    /// case-insensitive prefix of the typed query.
    #[name("completion.ghost_text")]
    pub CompletionGhostText: bool = false;
}

// ---- Display group: per-feature buffer-display preferences ----
//
// One option per `BufferDisplayCategory` variant, each typed
// `BufferDisplayPreference`. Default is `Default`, which means
// "use the category's built-in default" -- the resolver in
// `App::resolve_display` falls through to `default_display()`
// when an option resolves to `Default`. Setting a non-default
// value (e.g. `:set lsp.log.display = split-h`) overrides the
// dispatch for that category.
crate::options! {
    group = crate::Display;

    /// Where `:lsp-status` opens.
    #[name("lsp.status.display")]
    pub LspStatusDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where `:lsp-log` / `:lsp-trace-log` open. Default
    /// `active-pane` (live-tailed log buffers want to live in
    /// a real pane).
    #[name("lsp.log.display")]
    pub LspLogDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where `:help <topic>` opens.
    #[name("help.topic.display")]
    pub HelpTopicDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where `:describe-command` / `:describe-buffer` /
    /// `:describe-key` / `:describe-option` /
    /// `:describe-event` open.
    #[name("help.describe.display")]
    pub HelpDescribeDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where `:apropos <pattern>` opens.
    #[name("help.apropos.display")]
    pub HelpAproposDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where state-listing help views open (`:ls`, `:keymap`,
    /// `:options`, `:describe-events`, ...).
    #[name("help.list.display")]
    pub HelpListDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where the hover popup (`K`) renders. Default
    /// `floating-cursor` (popup floats on the doc; the doc
    /// keeps focus); user can flip to `popup-cursor` (focused
    /// popup) or `active-pane`.
    #[name("hover.display")]
    pub HoverDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where signature help renders. Same default-shape as
    /// hover (cursor-anchored floating).
    #[name("signature.display")]
    pub SignatureDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;

    /// Where the *selected* buffer / location lands after a
    /// picker accept (`:diagnostics`, `:references`,
    /// `:symbol`, `:Files`, `:buffers`, `:lsp-server-log`).
    /// Default `active-pane`; `split-h` / `split-v` open the
    /// pick in a new sibling pane.
    #[name("picker.result.display")]
    pub PickerResultDisplay: lattice_core::ui::display::BufferDisplayPreference =
        lattice_core::ui::display::BufferDisplayPreference::Default;
}

// M.2.0c: `CoreOptions` struct and `register_core_options`
// helper retired. Built-in options self-register via the
// macro-generated `register_fn` thunks (`OPTION_DECLS` linkme
// slice); consumers boot via `ConfigRegistry::init_from_linkme()`
// and read via `config.get_typed::<Tabstop>()`.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic, unsafe_code)]
    use super::*;
    use crate::option_decl::OptionDecl;
    use crate::registry::ConfigRegistry;

    #[test]
    fn type_keyed_reads_after_init_from_linkme() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert_eq!(*r.get_typed::<Tabstop>().unwrap(), 8);
        assert!(*r.get_typed::<Number>().unwrap());
        assert!(!*r.get_typed::<RelativeNumber>().unwrap());
        assert!(!*r.get_typed::<Wrap>().unwrap());
        assert!(!*r.get_typed::<ReadOnly>().unwrap());
        assert_eq!(*r.get_typed::<FoldMethodOption>().unwrap(), FoldMethod::Manual);
        assert_eq!(*r.get_typed::<Scrolloff>().unwrap(), 0);
        assert!(*r.get_typed::<CompletionAutoInsertSingle>().unwrap());
        assert_eq!(*r.get_typed::<CompletionSourceLspPriority>().unwrap(), 200);
    }

    #[test]
    fn read_only_is_not_customizable() {
        // ReadOnly is mode-driven, not user-typed config -- it
        // should be hidden from `:set` autocomplete and the
        // future `:customize` form.
        assert!(!ReadOnly::CUSTOMIZABLE);
    }

    #[test]
    fn completion_source_priority_validate_rejects_out_of_range() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert!(r.set_typed::<CompletionSourceLspPriority>(-1).is_err());
        assert!(r.set_typed::<CompletionSourceLspPriority>(10_000).is_err());
        assert!(r.set_typed::<CompletionSourceLspPriority>(0).is_ok());
        assert!(r.set_typed::<CompletionSourceLspPriority>(9999).is_ok());
    }

    #[test]
    fn registered_options_are_lookable_by_alias() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
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
        r.init_from_linkme();
        let err = r.parse_and_set_command("tabstop=99").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("tabstop out of range [1, 32]: 99"));
    }

    #[test]
    fn foldmethod_parse_error_preserves_legacy_wording() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        let err = r.parse_and_set_command("foldmethod=xyz").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("expected `manual`, `indent`, `markdown`, or `syntax`"));
    }
}
