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
//! window, `register_core_options` runs `init_from_linkme` and
//! returns a `CoreOptions` struct populated with the typed
//! handles. Existing callers (`config.get(core.tabstop)`)
//! continue to work unchanged. M.2.0c migrates the callers to
//! `config.get_typed::<Tabstop>()` and retires `CoreOptions`.

use crate::signcolumn::SignColumn;
use lattice_core::FoldMethod;

// Validators referenced by `#[validate(...)]` on the options
// below. Plain Rust functions; the macro just records the path.
// `&String` is required by the typed-option machinery's
// validator signature; clippy's `ptr_arg` lint flags this as
// "use `&str` instead", but doing so breaks the macro-emitted
// caller.
#[allow(clippy::ptr_arg)]
fn validate_log_level(s: &String) -> Result<(), String> {
    match s.as_str() {
        "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
        other => Err(format!(
            "lsp.log_level must be one of `error`/`warn`/`info`/`debug`/`trace`, got `{other}`"
        )),
    }
}

/// AI-1b: dedicated validator for `ai.log_level`, mirroring
/// `validate_log_level` above but naming `ai.log_level` (not
/// `lsp.log_level`) in the error so `:set ai.log_level=bogus`
/// points at the right option.
#[allow(clippy::ptr_arg)]
fn validate_ai_log_level(s: &String) -> Result<(), String> {
    match s.as_str() {
        "error" | "warn" | "info" | "debug" | "trace" => Ok(()),
        other => Err(format!(
            "ai.log_level must be one of `error`/`warn`/`info`/`debug`/`trace`, got `{other}`"
        )),
    }
}

fn validate_log_capacity(i: &i64) -> Result<(), String> {
    if *i < 0 {
        Err(format!("lsp.log_capacity must be >= 0, got {i}"))
    } else {
        Ok(())
    }
}

/// Validate a `tracing-subscriber::EnvFilter` directive. Accepts
/// the standard syntax: a level name (`info`), per-target
/// directives (`lsp=debug`), or a comma-separated list
/// (`editor=info,lsp=debug,grammar=trace`).
#[allow(clippy::ptr_arg)]
fn validate_messages_filter(s: &String) -> Result<(), String> {
    // The parse itself is the validation -- tracing-subscriber
    // returns a typed error on bad syntax (unknown level,
    // unparseable target, etc.).
    tracing_subscriber::EnvFilter::try_new(s.as_str())
        .map(|_| ())
        .map_err(|e| format!("messages.filter: {e}"))
}

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

fn validate_sidescroll(i: &i64) -> Result<(), String> {
    // 0 = jump-scroll (cursor to the middle of the window), matching
    // vim's default. Positive values step that many columns at a
    // time. Upper bound mirrors vim's practical ceiling.
    if (0..=1024).contains(i) {
        Ok(())
    } else {
        Err(format!("sidescroll out of range [0, 1024]: {i}"))
    }
}

fn validate_sidescrolloff(i: &i64) -> Result<(), String> {
    if (0..=1024).contains(i) {
        Ok(())
    } else {
        Err(format!("sidescrolloff out of range [0, 1024]: {i}"))
    }
}

fn validate_modeline_padding(i: &i64) -> Result<(), String> {
    if (0..=16).contains(i) {
        Ok(())
    } else {
        Err(format!("ui.modeline.padding out of range [0, 16]: {i}"))
    }
}

fn validate_terminal_scrollback_lines(i: &i64) -> Result<(), String> {
    if *i < 0 {
        Err(format!(
            "terminal.scrollback-lines must be >= 0 (use 0 to disable scrollback), got {i}"
        ))
    } else if *i > 1_000_000 {
        Err(format!(
            "terminal.scrollback-lines capped at 1_000_000 (≈ 80 MB of cells); got {i}"
        ))
    } else {
        Ok(())
    }
}

fn validate_completion_priority(i: &i64) -> Result<(), String> {
    if (0..=9999).contains(i) {
        Ok(())
    } else {
        Err(format!("priority out of range [0, 9999]: {i}"))
    }
}

#[allow(clippy::ptr_arg)]
fn validate_picker_display(s: &String) -> Result<(), String> {
    match s.as_str() {
        "popup" | "minibuffer" => Ok(()),
        other => Err(format!(
            "picker.display must be one of `popup`/`minibuffer`, got `{other}`"
        )),
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

    /// Whether the renderer reserves the gutter sign columns
    /// (diagnostics severity + diff sign). `yes` (default) always
    /// reserves them so layout never shifts when a sign appears;
    /// `no` hides them. Help / synthetic buffers set `no` for clean
    /// gutterless rendering — the renderer derives the column layout
    /// from this option alone, never from buffer kind / popup / pane.
    #[aliases("scl")]
    #[name("signcolumn")]
    pub SignColumnOption: SignColumn = SignColumn::Yes;

    /// When `true` (default), an explicit **yank** (`y`, `yy`, Visual
    /// `y`) also copies to the system clipboard, and paste of the
    /// unnamed register reads it — the clipboard is the default yank
    /// target. Delete / change / `x` stay in registers and never touch
    /// the clipboard (the *yank-only* rule — no incidental clobber).
    /// `false` = pure registers; only the explicit `"+` / `"*`
    /// registers reach the clipboard.
    ///
    /// A plain boolean by deliberate design (`clipboard.md` §5):
    /// lattice rejects vim's crude `unnamed` / `unnamedplus` string
    /// names in favour of a self-documenting on/off.
    #[name("clipboard")]
    pub ClipboardEnabled: bool = true;

    /// Ignore case in search patterns.
    #[aliases("ic")]
    #[name("ignorecase")]
    pub IgnoreCase: bool = false;

    /// Number of columns a hard tab character renders as. The
    /// cells builder expands each `\t` to the next multiple of this
    /// width (W.4.t). Default 4 (Lattice's house style; vim's
    /// historical default is 8).
    #[aliases("ts")]
    #[validate(validate_tabstop)]
    pub Tabstop: i64 = 4;

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

    /// Whether the buffer corresponds to an on-disk file the
    /// editor can save and that should be tracked for unsaved
    /// changes. `true` (the default) means `:q` warns on dirty,
    /// `:w` writes to disk, and the modeline shows `[+]` for
    /// modified state. `false` (vim's `&buftype = nofile`) means
    /// the buffer is a transcript / log / overlay whose content
    /// is owned by a subsystem; the dirty guard skips it and
    /// `:w` is a no-op. `customizable = false` — modes contribute
    /// the override (`messages-mode`, `lsp-log-mode`, `help-mode`,
    /// `terminal-mode` set `NoFile = true`); users don't `:set`
    /// it directly.
    #[customizable(false)]
    #[name("no-file")]
    pub NoFile: bool = false;

    /// When `true` (the default), a file-backed buffer refreshes when its
    /// on-disk content changes out from under the editor (vim's
    /// `autoread`): an unmodified buffer reloads silently; a buffer with
    /// unsaved edits opens a diff resolver rather than clobbering either
    /// side. `false` disables external-change watching for the buffer.
    /// Non-file buffers (oil, help, synthetic) are never watched
    /// regardless. See `docs/dev/architecture/autoread.md`.
    #[name("autoread")]
    pub Autoread: bool = true;

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

    /// Columns to scroll horizontally when the cursor moves off the
    /// edge with `wrap` off. `0` (vim default) jumps so the cursor
    /// lands in the middle of the window; a positive value scrolls
    /// that many columns at a time. No effect when `wrap` is on.
    #[aliases("ss")]
    #[validate(validate_sidescroll)]
    pub Sidescroll: i64 = 0;

    /// Minimum columns kept to the left and right of the cursor when
    /// the view scrolls horizontally (`wrap` off). Horizontal analog
    /// of `scrolloff`. Clamped to half the body width at use.
    #[aliases("siso")]
    #[validate(validate_sidescrolloff)]
    pub Sidescrolloff: i64 = 0;

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

    /// Synchronise scrolling across all panes that have
    /// `scrollbind=true`. The option-change handler rebuilds the
    /// singleton identity-mapper `PaneGroup` to contain exactly
    /// the current panes with `scrollbind=true`. Vim's
    /// `:set scrollbind` / `scb`. D.0b.
    #[aliases("scb")]
    #[name("scrollbind")]
    pub Scrollbind: bool = false;

    /// Enable the `emacs-keys` `<C-x>` leader tribute. Default on.
    /// `:set noemacs-keys` rebuilds the leader layer empty (live),
    /// reclaiming `<C-x>` for vanilla Normal-mode resolution. See
    /// `docs/dev/architecture/emacs-keys.md`.
    #[name("emacs-keys")]
    pub EmacsKeys: bool = true;

    /// The `emacs-keys` leader prefix — the chord that opens the
    /// `<C-x>` tribute map (`docs/dev/architecture/emacs-keys.md`).
    /// Default `<C-x>`. Each binding is `prefix + suffix`, parsed via
    /// `parse_chord_sequence`; a malformed value degrades to an empty
    /// tribute (warn, no panic). Live: `:set emacs-keys-prefix=…`
    /// re-pushes the layer.
    #[name("emacs-keys-prefix")]
    pub EmacsKeysPrefix: String = "<C-x>".into();
}

// ---- Completion group: insert-completion knobs ----

/// SN.3g: single source for the `gen:snippet` source default priority,
/// shared by `completion.source.snippet.priority`'s default (below) and
/// `lattice-snippet`'s `SnippetCompletionMode` contribution, so the two
/// can't drift. Above buffer-words, below LSP.
pub const COMPLETION_SOURCE_SNIPPET_DEFAULT_PRIORITY: i64 = 150;

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
    /// (`docs/dev/architecture/insert-completion.md` §3.4 / §3.6). Default 200;
    /// LSP-driven IDE completions usually want to win against
    /// local buffer words and snippets at tied score.
    #[name("completion.source.lsp.priority")]
    #[validate(validate_completion_priority)]
    pub CompletionSourceLspPriority: i64 = 200;

    /// Priority bucket for the `gen:snippet` insert-mode source.
    /// Default [`COMPLETION_SOURCE_SNIPPET_DEFAULT_PRIORITY`] (150) --
    /// above buffer-words, below LSP. Per-language overrides land in
    /// 4.2.g.5 (3/3); today the value is global.
    #[name("completion.source.snippet.priority")]
    #[validate(validate_completion_priority)]
    pub CompletionSourceSnippetPriority: i64 = COMPLETION_SOURCE_SNIPPET_DEFAULT_PRIORITY;

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

    /// Where `:messages` opens. Default `active-pane` (a
    /// transcript that streams new entries lives best in a
    /// pane the user can split / scroll independently).
    #[name("messages.display")]
    pub MessagesDisplay: lattice_core::ui::display::BufferDisplayPreference =
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

    // ---- M.7.3 whitespace decoration glyphs ----
    //
    // Five discrete typed glyphs, replacing vim's encoded
    // `listchars=tab:→\ ,trail:·,space:·,...` blob with a
    // properly typed surface that's discoverable through
    // `:options` and `:describe-option`. Each option is a
    // String (single visible glyph in v1; future combining
    // sequences can land without an option-shape change).
    // Empty string ⇒ that whitespace category is not decorated.
    //
    // Defaults follow emacs `whitespace-mode`'s canonical
    // visible set: tabs + trailing + leading. Mid-text spaces
    // and end-of-line markers are opt-in (most users find
    // them noisy).
    //
    // All five flow through the renderer's whitespace pre-pass
    // when `whitespace-show-mode` is active (M.7.2 minor) /
    // `:set list` (M.7.1 cascade); see `Whitespace` in the
    // Editor group.

    /// Glyph rendered in place of the tab character when
    /// whitespace decoration is active. Followed by a
    /// space-pad to the next tabstop column. Empty string ⇒
    /// tabs render bare. Default `→`.
    #[name("display.whitespace.tab")]
    pub WhitespaceTab: String = "→".into();

    /// Glyph for trailing whitespace (spaces or tabs at
    /// end-of-line). Rendered with a `trailing` style (red
    /// by default; theme-driven). Empty ⇒ no decoration.
    /// Default `·`.
    #[name("display.whitespace.trailing")]
    pub WhitespaceTrailing: String = "·".into();

    /// Glyph for leading whitespace -- non-tab indentation
    /// at the start of a line. Mirrors emacs
    /// `whitespace-mode`'s `indentation` highlight.
    /// Default `·`.
    #[name("display.whitespace.leading")]
    pub WhitespaceLeading: String = "·".into();

    /// Glyph for plain spaces in the middle of text
    /// (between non-whitespace characters). Most users find
    /// this loud; default empty. Set to `·` to mirror
    /// emacs's `space-mark`.
    #[name("display.whitespace.space")]
    pub WhitespaceSpace: String = String::new();

    /// Glyph at end-of-line (vim's `eol` listchar). Default
    /// empty; `¬` is the conventional choice.
    #[name("display.whitespace.eol")]
    pub WhitespaceEol: String = String::new();
}

// ---- Picker group: file finder / command palette / grep ----

crate::options! {
    group = crate::Picker;

    /// Backend binary `:picker grep` shells out to. `"auto"`
    /// picks the first available of `rg`, `ag`, `grep` in
    /// PATH at invocation time. Explicit names (`"rg"`,
    /// `"ag"`, `"grep"`) force a specific binary and surface
    /// an error if it's not on PATH. Future plugin-shipped
    /// backends can register additional names; the matching
    /// logic lives in `lattice_picker::picker_sources::grep`.
    #[name("picker.grep.backend")]
    pub PickerGrepBackend: String = String::from("auto");

    /// Maximum number of grep hits to surface in one
    /// `:picker grep` invocation. Bounds memory + render
    /// time on huge codebases; users hit this rarely (typical
    /// pattern matches hundreds of lines, not thousands).
    #[name("picker.grep.max-hits")]
    pub PickerGrepMaxHits: i64 = 2000;

    /// Whether picker MRU (frecency) scoring fires at all.
    /// `false` disables both the bonus snapshot on
    /// picker-open and the record-on-accept path -- pickers
    /// rank by pure match score, ignore prior usage.
    /// Persistence keeps working independently
    /// (`picker.mru.persist`); flipping enabled back on
    /// resumes ranking using whatever's still in the cache.
    #[name("picker.mru.enabled")]
    pub PickerMruEnabled: bool = true;

    /// Recency half-life for the frecency formula, in
    /// **days**. Stored as `i64` so `:set
    /// picker.mru.recency-half-life-days=14` Just Works
    /// through the parse-int path; the picker's
    /// `Duration` machinery converts. Smaller values bias
    /// strongly toward "today's choices"; larger values
    /// keep historical usage relevant.
    #[name("picker.mru.recency-half-life-days")]
    pub PickerMruRecencyHalfLifeDays: i64 = 7;

    /// Maximum number of MRU entries per (source_id) namespace.
    /// On insert past this cap the lowest-frecency entry in
    /// the namespace is evicted (prescient-style). Larger
    /// values keep long-tail usage history; smaller values
    /// keep the cache lean.
    #[name("picker.mru.cap-per-namespace")]
    pub PickerMruCapPerNamespace: i64 = 1000;

    /// Whether the MRU index persists to disk between runs.
    /// `false` keeps MRU in-memory only; helpful for ephemeral
    /// sessions or for users who deliberately want a clean
    /// slate each launch. Default `true` preserves vertico-
    /// style "yesterday's picks still float."
    #[name("picker.mru.persist")]
    pub PickerMruPersist: bool = true;

    /// Where the picker UI is drawn. `"minibuffer"` renders
    /// vertico-style: prompt sits on the cmdline row and the
    /// candidate list fans above it (TUI) / above the status
    /// line (GPUI), keeping the buffer fully visible.
    /// `"popup"` renders a centred overlay floating over the
    /// buffer area (terminal-friendly when narrow, common in
    /// IDE-style editors). Behaviour is identical between the
    /// TUI and GPUI peers so users carry the same muscle
    /// memory across them. Future variants (`"split"`,
    /// `"sidebar"`) may be added without breaking this key.
    #[name("picker.display")]
    #[validate(validate_picker_display)]
    pub PickerDisplay: String = String::from("minibuffer");
}

// ---- LSP group: log + trace knobs ----

crate::options! {
    group = crate::Lsp;

    /// Default LSP log-record minimum level at startup.
    /// Accepted values: `error` / `warn` / `info` / `debug`
    /// / `trace`. The runtime `:lsp-log-level` command
    /// adjusts this live; this option sets the boot value.
    ///
    /// 4.4.o: seeded into `LspLogger::new` so users who
    /// always run with debug-level LSP traces don't have to
    /// `:set` it post-boot.
    #[name("lsp.log_level")]
    #[validate(validate_log_level)]
    pub LspLogLevel: String = String::from("info");

    /// Per-server log ring capacity at boot. Each LSP server
    /// gets its own bounded ring; smaller values shed older
    /// records sooner. `0` is allowed (drops every record at
    /// the ring boundary -- useful for tests / sandboxed
    /// runs) but typically users keep the 10k default.
    ///
    /// 4.4.o: the runtime path
    /// (`LspLogger::set_default_capacity`) stays for live
    /// resizing; this option just seeds the boot value.
    #[name("lsp.log_capacity")]
    #[validate(validate_log_capacity)]
    pub LspLogCapacity: i64 = 10_000;
}

// AI-1b: AI group -- log knobs for the per-process `AiLogger` log
// rings (mirrors the LSP group above; `lattice-ai`'s `AiLogger`
// producer already exists (Task 6), boot-time wiring of these
// options into it is a later task).

crate::options! {
    group = crate::Ai;

    /// Enables capture of AI-agent output into the per-process log
    /// rings. `:set ai.log=false` disables capture.
    #[name("ai.log")]
    pub AiLog: bool = true;

    /// Default minimum log level for AI-agent log records.
    /// Accepted values: `error` / `warn` / `info` / `debug` /
    /// `trace`.
    #[name("ai.log_level")]
    #[validate(validate_ai_log_level)]
    pub AiLogLevel: String = String::from("info");
}

// msg-mode.2: messages group — `messages.filter` directives
// the `*messages*` buffer's tracing bridge layer.
crate::options! {
    group = crate::Messages;

    /// `tracing-subscriber::EnvFilter` directive controlling
    /// which `tracing::*` events the boot-installed
    /// `MessagesLayer` captures into `*messages*`. Accepts:
    ///
    /// - A single level (`info` / `warn` / `error` / `debug` /
    ///   `trace`).
    /// - Per-target directives (`lsp=debug`).
    /// - Comma-separated combinations
    ///   (`editor=info,lsp=debug,grammar=trace`).
    ///
    /// Live-editable via `:set messages.filter=...`; the
    /// runtime's reload-handle (installed at boot alongside
    /// the layer) swaps the filter without restarting the
    /// editor. Default `info`: every `info!` / `warn!` /
    /// `error!` event in the editor flows into `*messages*`,
    /// `debug!` and `trace!` are dropped at the filter.
    #[name("messages.filter")]
    #[validate(validate_messages_filter)]
    pub MessagesFilter: String = String::from("info");
}

// Issue #29 (2026-05-22): tabline group — `tabline.show`
// controls when the tab strip is visible. Mirrors vim's
// `:set showtabline` (0/1/2) but uses readable labels.
crate::options! {
    group = crate::Tabline;

    /// When to paint the tabline at the top of the screen.
    ///
    /// - `never`  — never show the tabline (no row reserved).
    /// - `auto`   — show only when more than one tab is open
    ///              (default; matches vim's `showtabline=1`).
    /// - `always` — always show, even for one tab.
    ///
    /// Live-editable via `:set tabline.show=<value>`. The
    /// renderer's per-frame layout pass reads the published
    /// value when deciding how much vertical space to reserve.
    #[name("tabline.show")]
    pub TablineShowOption: lattice_core::ui::tab::TablineShow =
        lattice_core::ui::tab::TablineShow::Auto;
}

// Terminal-mode T2.b.0 (2026-05-25): terminal group — knobs that
// affect every PTY-backed buffer. `terminal.esc-exits` is the
// first; T2.b/T4 grow the group with `terminal.shell`,
// `terminal.scrollback-lines`, `terminal.refresh-hz`, etc.
crate::options! {
    group = crate::Terminal;

    /// When `true`, pressing `<Esc>` inside Terminal-Insert exits
    /// back to Normal-in-terminal (so `:q`, motions, and the rest
    /// of the vim grammar are reachable without the `<C-\><C-n>`
    /// chord). When `false`, `<Esc>` encodes to `\x1b` and goes
    /// to the PTY — nested programs (vim, htop, less) keep their
    /// own Esc semantics.
    ///
    /// Default `true`: matches the table-stakes terminal UX of
    /// modern editors (VS Code, Helix). Power users running vim
    /// inside `:terminal` flip it off and use `<C-\><C-n>` to
    /// exit (added by T2.c).
    #[name("terminal.esc-exits")]
    pub TerminalEscExits: bool = true;

    /// Maximum scrollback ring size (lines). Set to `0` to
    /// disable scrollback entirely (saves RAM on long-running
    /// terminals with chatty output). Default `10000` matches
    /// the user-facing `docs/user/terminal.md` table and what
    /// most modern terminal emulators ship with.
    ///
    /// Capped at 1_000_000 — beyond that the ring's memory
    /// footprint dwarfs every other editor allocation and the
    /// search hot path slows to a crawl. Users who genuinely
    /// want unbounded history should pipe the output to a file
    /// instead and `:e` it as a Document buffer.
    #[name("terminal.scrollback-lines")]
    #[validate(validate_terminal_scrollback_lines)]
    pub TerminalScrollbackLines: i64 = 10_000;
}

// ML.5 (2026-06-21): modeline group — per-zone element layout +
// separator for the configurable element-system modeline
// (`docs/dev/architecture/modeline.md` §11). The three zone options
// hold a `ModelineZone` (the first list-valued option): `Auto` (the
// default — descriptor-driven placement, so a newly-registered mode
// element auto-appears) or an explicit ordered element-id list.
// TOML uses Helix-shaped arrays (`left = ["core.mode", "core.path"]`);
// `:set ui.modeline.left=core.mode,core.path` uses the comma form.
crate::options! {
    group = crate::Modeline;

    /// Left-zone element layout — ordered element ids assigned to the
    /// left (flush-left) zone, e.g. `["core.mode", "core.path"]`.
    /// `auto` (the default) uses each registered element's own
    /// descriptor placement. An explicit list shows exactly those ids,
    /// in order; unknown ids are skipped + logged. An empty list
    /// (`[]`) is an explicitly-blank zone.
    #[name("ui.modeline.left")]
    pub ModelineLeft: crate::ModelineZone = crate::ModelineZone::Auto;

    /// Center-zone element layout (centered in the gap between Left and
    /// Right). `auto` (default) is descriptor-driven; built-ins place
    /// nothing here, so the effective default is empty. Custom / plugin
    /// elements live here.
    #[name("ui.modeline.center")]
    pub ModelineCenter: crate::ModelineZone = crate::ModelineZone::Auto;

    /// Right-zone element layout (the block is right-aligned, ids in
    /// left→right order), e.g. `["lsp", "core.position", "core.lang"]`.
    /// `auto` (default) is descriptor-driven.
    #[name("ui.modeline.right")]
    pub ModelineRight: crate::ModelineZone = crate::ModelineZone::Auto;

    /// Separator inserted between elements within a zone. A non-blank
    /// value is auto-padded with a space on each side at render time
    /// (so `:set ui.modeline.separator=|` shows ` | ` — you give the
    /// glyph, the renderer owns the spacing). Blank (the default) ⇒ a
    /// single space between elements.
    #[name("ui.modeline.separator")]
    pub ModelineSeparator: String = " ".into();

    /// Columns of blank margin at the start (before the Left zone) and
    /// end (after the Right zone) of the modeline row — the row's
    /// left/right breathing room. Default 1; `0` flushes content to the
    /// pane edges.
    #[name("ui.modeline.padding")]
    #[validate(validate_modeline_padding)]
    pub ModelinePadding: i64 = 1;
}

// L4 (2026-06-21): diagnostics group — inline end-of-line diagnostic
// summary presentation (`lsp-architecture.md` §15). `inline` scopes the
// summary (off / cursor-line / all); `inline-min-severity` filters which
// diagnostics count. Both are read host-side to gate + compute the
// cursor-line summary (L4a.2).
crate::options! {
    group = crate::Diagnostics;

    /// Where the inline (end-of-line virtual-text) diagnostic summary
    /// renders: `off`, `cursor-line` (the default — cursor line only,
    /// idle-gated, Insert-suppressed), or `all` (every viewport line).
    #[name("ui.diagnostics.inline")]
    pub DiagnosticsInlineOption: crate::DiagnosticsInline = crate::DiagnosticsInline::CursorLine;

    /// Least-severe diagnostic level included in the inline summary:
    /// `error`, `warning`, `info`, or `hint` (the default — include
    /// everything). A diagnostic shows when it is as-or-more severe.
    #[name("ui.diagnostics.inline-min-severity")]
    pub DiagnosticsMinSeverityOption: crate::DiagnosticsSeverity =
        crate::DiagnosticsSeverity::Hint;
}

// M.2.0c: `CoreOptions` struct and `register_core_options`
// helper retired. Built-in options self-register via the
// macro-generated `register_fn` thunks (`OPTION_DECLS` linkme
// slice); consumers boot via `ConfigRegistry::init_from_linkme()`
// and read via `config.get_typed::<Tabstop>()`.

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        clippy::assertions_on_constants,
        unsafe_code
    )]
    use super::*;
    use crate::option_decl::OptionDecl;
    use crate::registry::ConfigRegistry;

    #[test]
    fn type_keyed_reads_after_init_from_linkme() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert_eq!(*r.get_typed::<Tabstop>().unwrap(), 4);
        assert!(*r.get_typed::<Number>().unwrap());
        assert!(!*r.get_typed::<RelativeNumber>().unwrap());
        assert!(!*r.get_typed::<Wrap>().unwrap());
        assert!(!*r.get_typed::<ReadOnly>().unwrap());
        assert_eq!(
            *r.get_typed::<FoldMethodOption>().unwrap(),
            FoldMethod::Manual
        );
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
    fn picker_display_default_is_minibuffer() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert_eq!(
            r.get_typed::<PickerDisplay>().unwrap().as_str(),
            "minibuffer"
        );
    }

    #[test]
    fn picker_display_accepts_popup_and_minibuffer() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert!(r.set_typed::<PickerDisplay>(String::from("popup")).is_ok());
        assert_eq!(r.get_typed::<PickerDisplay>().unwrap().as_str(), "popup");
        assert!(
            r.set_typed::<PickerDisplay>(String::from("minibuffer"))
                .is_ok()
        );
    }

    #[test]
    fn picker_display_rejects_unknown_value() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        let err = r
            .set_typed::<PickerDisplay>(String::from("sidebar"))
            .unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("picker.display must be one of"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn foldmethod_parse_error_preserves_legacy_wording() {
        // Wording grew the `lsp` option in 4.4.f; the test
        // pins the new shape (legacy bytes-identical
        // constraint dropped because the option list itself
        // grew).
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        let err = r.parse_and_set_command("foldmethod=xyz").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("expected `manual`, `indent`, `markdown`, `syntax`, or `lsp`"));
    }

    // AI-1b: `ai.log` / `ai.log_level` config options. Registration
    // only -- the boot-time `AiLogger` wiring is Task 12.

    #[test]
    fn ai_log_default_is_true() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert!(*r.get_typed::<AiLog>().unwrap());
    }

    #[test]
    fn ai_log_level_default_is_info() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert_eq!(r.get_typed::<AiLogLevel>().unwrap().as_str(), "info");
    }

    #[test]
    fn ai_log_level_rejects_invalid() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert!(r.set_typed::<AiLogLevel>(String::from("bogus")).is_err());
        assert!(r.set_typed::<AiLogLevel>(String::from("debug")).is_ok());
    }

    #[test]
    fn ai_options_lookable_by_name() {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        assert_eq!(r.lookup("ai.log").unwrap().name(), "ai.log");
        assert_eq!(r.lookup("ai.log_level").unwrap().name(), "ai.log_level");
    }
}
