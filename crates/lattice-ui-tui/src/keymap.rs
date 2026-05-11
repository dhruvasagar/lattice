//! Keymap registry (DESIGN.md §5.2.3).
//!
//! Vim's default keymap as a typed catalog rather than scattered
//! `match` arms. Each [`KeymapEntry`] records the chord, the mode it
//! applies in, a one-line doc, and an optional canonical command name
//! that links the binding to its `CommandRegistry` entry. `:describe-key`
//! and `:keymap` consume this table; the v1 input layer (`input.rs`)
//! still owns the chord-to-Action translation, but a drift test (in
//! `input.rs`'s test module) verifies that every descriptor's chord
//! produces a non-`None` Action -- so adding a binding here without
//! wiring it in `input.rs` (or vice-versa) fails CI.
//!
//! Promoting `input.rs` to consume this table directly (so dispatch is
//! registry-driven and plugins can add chords) is post-1.0 -- the
//! v1.0 priority is the metadata surface for §5.11 introspection.
//!
//! Notation:
//! - Plain chars: `j`, `dw`, `gg`.
//! - Modifier-prefixed: `<C-d>`, `<C-v>`, `<C-r>`.
//! - Special keys: `<Esc>`, `<CR>`, `<Tab>`, `<Up>`, `<Down>`, `<Left>`,
//!   `<Right>`, `<Home>`, `<End>`, `<PageUp>`, `<PageDown>`, `<BS>`.
//! - Multi-key sequences are concatenated: `gg`, `dw`, `zt`.

/// Where a binding takes effect. Multi-key sequences (e.g. `gg`)
/// resolve atomically; intermediate single-key prefixes (`g`, `z`) get
/// their own descriptor entries so `:describe-key g` explains the
/// pending substate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindingMode {
    Normal,
    Insert,
    /// Charwise / Linewise / Blockwise visual share the same chord
    /// table; differences are in the operator dispatch (Range::Selection
    /// resolution).
    Visual,
    Replace,
    /// `:` minibuffer.
    Command,
    /// `/` `?` minibuffer.
    Search,
    /// After `d` / `y` / `c` / `>` / `<` / `gU` / `gu` / `g~` -- waiting
    /// for a motion or text-object target.
    OperatorPending,
    /// After `g` -- waiting for the second key.
    AfterG,
    /// After `z` -- waiting for the second key.
    AfterZ,
    /// After `m` -- waiting for the mark name.
    AfterMark,
    /// After `'` (jump to mark line) -- waiting for mark name.
    AfterJumpMarkLine,
    /// After `` ` `` (jump to mark exact) -- waiting for mark name.
    AfterJumpMarkExact,
    /// After `"` -- waiting for register name.
    AfterRegister,
    /// After `q` (when not already recording) -- waiting for register
    /// name to record into.
    AfterMacroStart,
    /// After `@` -- waiting for register name to play (or `@` for last).
    AfterMacroPlay,
    /// After `f` / `F` / `t` / `T` -- waiting for the target char.
    AfterFindChar,
    /// After `i<x>` / `a<x>` in operator-pending -- waiting for the
    /// text-object key.
    AfterTextObject,
    /// While the §5.11 help overlay is active.
    Help,
    /// After `<C-w>` -- waiting for the window-management
    /// resolution key.
    AfterCtrlW,
    /// After `<C-x>` in Insert mode -- waiting for the
    /// expansion-prefix resolution key (`<C-x><C-o>` ->
    /// completion trigger; future siblings: `<C-x><C-s>`
    /// snippet expand, `<C-x><C-f>` filename completion).
    AfterCtrlX,
    /// **Insert-mode completion popup minor mode** (Phase
    /// 4.2.g.1). Active only while
    /// `App.insert_completion.is_some()`. Bindings inside this
    /// layer override Insert-mode + Normal-mode meanings for
    /// the popup's lifetime; closing the popup deactivates the
    /// layer.
    CompletionPopup,
    /// **Active-snippet minor mode** (Phase 4.2.g.4). Active
    /// only while `App.active_snippet.is_some()`. Bindings
    /// inside this layer override Insert-mode meanings for the
    /// snippet's lifetime: `<Tab>` jumps to the next
    /// placeholder (instead of inserting a literal tab),
    /// `<S-Tab>` to the previous, `<Esc>` exits the snippet
    /// and Insert mode. Closing the snippet (reaching `$0`,
    /// pressing `<Esc>`, or `:snippet-leave`) deactivates the
    /// layer.
    Snippet,
}

impl BindingMode {
    pub fn label(self) -> &'static str {
        match self {
            BindingMode::Normal => "Normal",
            BindingMode::Insert => "Insert",
            BindingMode::Visual => "Visual",
            BindingMode::Replace => "Replace",
            BindingMode::Command => "Command",
            BindingMode::Search => "Search",
            BindingMode::OperatorPending => "Operator-Pending",
            BindingMode::AfterG => "After-g",
            BindingMode::AfterZ => "After-z",
            BindingMode::AfterMark => "After-m",
            BindingMode::AfterJumpMarkLine => "After-'",
            BindingMode::AfterJumpMarkExact => "After-`",
            BindingMode::AfterRegister => "After-\"",
            BindingMode::AfterMacroStart => "After-q (record)",
            BindingMode::AfterMacroPlay => "After-@",
            BindingMode::AfterFindChar => "After-f/F/t/T",
            BindingMode::AfterTextObject => "After-i/a (text-object)",
            BindingMode::Help => "Help-overlay",
            BindingMode::AfterCtrlW => "After-<C-w> (window-management)",
            BindingMode::AfterCtrlX => "After-<C-x> (Insert expansion-prefix)",
            BindingMode::CompletionPopup => "Completion popup (minor mode)",
            BindingMode::Snippet => "Active-snippet (minor mode)",
        }
    }
}

/// One row in the catalog. Ordering of fields matches the rendering
/// order in `:describe-key` so reading one field at a time still tells
/// a coherent story.
///
/// The `source` field is `pub(crate)` so external callers cannot
/// construct a `KeymapEntry` literal -- the `keymap_entry!` macro is
/// the only construction path. This enforces forgery-prevention per
/// DESIGN.md §5.11.1: every binding's source is captured at the row's
/// own `file!()` + `line!()`, not supplied by the caller.
#[derive(Debug, Clone)]
pub struct KeymapEntry {
    pub chord: &'static str,
    pub mode: BindingMode,
    pub doc: &'static str,
    /// Canonical name in the `CommandRegistry`. `None` for synthetic
    /// actions (`PushDigit`, `SetPending`, `StartMacroRecord`, ...) that
    /// don't bind a registered command.
    pub command: Option<&'static str>,
    /// Where this binding was registered. For static entries built by
    /// the [`keymap_entry!`] macro this is the row's own `file:line`;
    /// for runtime user binds this is the config-loader / dispatcher
    /// source. The field is `pub(crate)` so the macro is the only
    /// construction path -- see DESIGN.md §5.11.1.
    pub(crate) source: lattice_grammar::SourceLocation,
}

/// Helper used by the [`keymap_entry!`] macro to construct a
/// `Builtin` source from `file!()` + `line!()`. `pub(crate)` so the
/// macro is the only construction path -- external callers can't
/// fabricate a `Builtin` source by calling this helper directly.
#[doc(hidden)]
pub(crate) fn __builtin_source(file: &'static str, line: u32) -> lattice_grammar::SourceLocation {
    lattice_grammar::SourceLocation::builtin_file(file, line)
}

impl lattice_grammar::Introspectable for KeymapEntry {
    fn kind_label(&self) -> &'static str {
        "key"
    }

    fn identifier(&self) -> String {
        format!("{}  ({})", self.chord, self.mode.label())
    }

    fn doc(&self) -> &str {
        self.doc
    }

    fn sources(&self) -> Vec<lattice_grammar::SourceEntry<'_>> {
        vec![lattice_grammar::SourceEntry {
            label: lattice_grammar::SourceLabel::BoundAt,
            source: &self.source,
        }]
    }

    fn extra_sections(&self) -> Vec<lattice_grammar::HelpSection> {
        let mut lines = Vec::new();
        if let Some(name) = self.command {
            // Cross-reference: `[name](command:name)` markdown
            // link follows to :describe-command.
            lines.push(format!("Invokes: [{name}](command:{name})"));
        }
        if lines.is_empty() {
            return Vec::new();
        }
        vec![lattice_grammar::HelpSection {
            heading: "Action:".to_string(),
            lines,
            anchor: Some("action".to_string()),
        }]
    }
}

/// Construct a [`KeymapEntry`] with the row's source location captured
/// at the macro invocation site. Three forms:
///
/// - `keymap_entry! { mode: Normal, chord: "j", doc: "Move down", cmd: "motion:line-down" }`
/// - `keymap_entry! { mode: Help, chord: "j", doc: "Scroll down" }`  (no cmd, defaults to None)
/// - `keymap_entry! { mode: Normal, chord: "x", doc: "Custom", cmd: Some("plugin:foo") }`  (explicit Option)
///
/// The `source` field on `KeymapEntry` is `pub(crate)` so this macro
/// is the only construction path -- callers cannot bypass capture by
/// hand-rolling a literal. `file!()` and `line!()` expand at the macro
/// invocation site, so each row in a static slice records its own
/// distinct line.
#[macro_export]
macro_rules! keymap_entry {
    // No-cmd form: defaults command to None.
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr $(,)? } => {
        $crate::keymap_entry! { mode: $mode, chord: $chord, doc: $doc, cmd: None }
    };
    // String-literal sugar: `cmd: "name"` -> `cmd: Some("name")`.
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr, cmd: $cmd:literal $(,)? } => {
        $crate::keymap_entry! { mode: $mode, chord: $chord, doc: $doc, cmd: Some($cmd) }
    };
    // Explicit form: `cmd: None` or `cmd: Some(...)`.
    { mode: $mode:ident, chord: $chord:expr, doc: $doc:expr, cmd: $cmd:expr $(,)? } => {
        $crate::keymap::KeymapEntry {
            mode: $crate::keymap::BindingMode::$mode,
            chord: $chord,
            doc: $doc,
            command: $cmd,
            source: $crate::keymap::__builtin_source(file!(), line!()),
        }
    };
}

/// Borrow the full vim default keymap. Lazily initialised on first
/// call (the entries embed `SourceLocation`s which carry `PathBuf`s
/// and therefore can't live in a `static`).
///
/// Order matters: chords that share a prefix (`g` vs `gg`) appear
/// adjacent so a reader scanning the table sees the prefix
/// relationship. The drift test in `input.rs` and `:keymap`'s render
/// both walk this slice in declaration order.
pub fn default_keymap() -> &'static [KeymapEntry] {
    static CACHE: std::sync::OnceLock<Vec<KeymapEntry>> = std::sync::OnceLock::new();
    CACHE.get_or_init(build_default_keymap)
}

fn build_default_keymap() -> Vec<KeymapEntry> {
    vec![
        // ---- Normal: motions ----
        keymap_entry! { mode: Normal, chord: "h", doc: "Move cursor left", cmd: "motion:char-left" },
        keymap_entry! { mode: Normal, chord: "<Left>", doc: "Move cursor left", cmd: "motion:char-left" },
        keymap_entry! { mode: Normal, chord: "j", doc: "Move cursor down", cmd: "motion:line-down" },
        keymap_entry! { mode: Normal, chord: "<Down>", doc: "Move cursor down", cmd: "motion:line-down" },
        keymap_entry! { mode: Normal, chord: "k", doc: "Move cursor up", cmd: "motion:line-up" },
        keymap_entry! { mode: Normal, chord: "<Up>", doc: "Move cursor up", cmd: "motion:line-up" },
        keymap_entry! { mode: Normal, chord: "l", doc: "Move cursor right", cmd: "motion:char-right" },
        keymap_entry! { mode: Normal, chord: "<Right>", doc: "Move cursor right", cmd: "motion:char-right" },
        keymap_entry! { mode: Normal, chord: "0", doc: "Jump to start of line (column 0)", cmd: "motion:line-start" },
        keymap_entry! { mode: Normal, chord: "<Home>", doc: "Jump to start of line", cmd: "motion:line-start" },
        keymap_entry! { mode: Normal, chord: "$", doc: "Jump to end of line", cmd: "motion:line-end" },
        keymap_entry! { mode: Normal, chord: "<End>", doc: "Jump to end of line", cmd: "motion:line-end" },
        keymap_entry! { mode: Normal, chord: "^", doc: "Jump to first non-blank on line", cmd: "motion:first-non-blank" },
        keymap_entry! { mode: Normal, chord: "w", doc: "Word forward (start of next word)", cmd: "motion:word-forward" },
        keymap_entry! { mode: Normal, chord: "b", doc: "Word backward (start of previous word)", cmd: "motion:word-backward" },
        keymap_entry! { mode: Normal, chord: "e", doc: "Word end (last byte of current/next word)", cmd: "motion:word-end" },
        keymap_entry! { mode: Normal, chord: "W", doc: "WORD forward (whitespace-delimited)", cmd: "motion:big-word-forward" },
        keymap_entry! { mode: Normal, chord: "B", doc: "WORD backward", cmd: "motion:big-word-backward" },
        keymap_entry! { mode: Normal, chord: "E", doc: "WORD end", cmd: "motion:big-word-end" },
        keymap_entry! { mode: Normal, chord: "}", doc: "Next paragraph boundary", cmd: "motion:paragraph-forward" },
        keymap_entry! { mode: Normal, chord: "{", doc: "Previous paragraph boundary", cmd: "motion:paragraph-backward" },
        keymap_entry! { mode: Normal, chord: ")", doc: "Next sentence", cmd: "motion:sentence-forward" },
        keymap_entry! { mode: Normal, chord: "(", doc: "Previous sentence", cmd: "motion:sentence-backward" },
        keymap_entry! { mode: Normal, chord: "G", doc: "Jump to last line", cmd: "motion:goto-last-line" },
        keymap_entry! { mode: Normal, chord: "gg", doc: "Jump to first line", cmd: "motion:goto-first-line" },
        // ---- Normal: viewport jumps ----
        keymap_entry! { mode: Normal, chord: "H", doc: "Cursor to top of viewport" },
        keymap_entry! { mode: Normal, chord: "M", doc: "Cursor to middle of viewport" },
        keymap_entry! { mode: Normal, chord: "L", doc: "Cursor to bottom of viewport" },
        // ---- Normal: scrolling ----
        keymap_entry! { mode: Normal, chord: "<C-d>", doc: "Scroll half-page down (10 lines)", cmd: "motion:line-down" },
        keymap_entry! { mode: Normal, chord: "<C-u>", doc: "Scroll half-page up (10 lines)", cmd: "motion:line-up" },
        keymap_entry! { mode: Normal, chord: "<C-f>", doc: "Page down" },
        keymap_entry! { mode: Normal, chord: "<C-b>", doc: "Page up" },
        keymap_entry! { mode: Normal, chord: "<C-e>", doc: "Scroll viewport down one line" },
        keymap_entry! { mode: Normal, chord: "<C-y>", doc: "Scroll viewport up one line" },
        keymap_entry! { mode: Normal, chord: "<PageDown>", doc: "Page down (10 lines)", cmd: "motion:line-down" },
        keymap_entry! { mode: Normal, chord: "<PageUp>", doc: "Page up (10 lines)", cmd: "motion:line-up" },
        // ---- Normal: undo/redo, dot, jump-list ----
        keymap_entry! { mode: Normal, chord: "u", doc: "Undo last change" },
        keymap_entry! { mode: Normal, chord: "<C-r>", doc: "Redo (reverse undo)" },
        keymap_entry! { mode: Normal, chord: ".", doc: "Repeat last change (dot-repeat)" },
        keymap_entry! { mode: Normal, chord: "<C-o>", doc: "Jump-list back (previous AutoJump position)" },
        keymap_entry! { mode: Normal, chord: "<C-i>", doc: "Jump-list forward" },
        keymap_entry! { mode: Normal, chord: "<Tab>", doc: "Jump-list forward (terminal alias for Ctrl-I)" },
        keymap_entry! { mode: Normal, chord: "<C-l>", doc: "Force redraw (clear terminal, reparse syntax, reset highlight cache)" },
        // ---- Normal: pending-key prefixes ----
        keymap_entry! { mode: Normal, chord: "g", doc: "Pending: second key resolves to gg/gU/gu/g~/gv/gJ/g;/g," },
        keymap_entry! { mode: Normal, chord: "z", doc: "Pending: scroll/fold sub-commands" },
        keymap_entry! { mode: Normal, chord: "d", doc: "Delete operator -- use with motion/text-object; doubled (`dd`) deletes current line", cmd: "operator:delete" },
        keymap_entry! { mode: Normal, chord: "c", doc: "Change operator -- delete then enter Insert", cmd: "operator:change" },
        keymap_entry! { mode: Normal, chord: "y", doc: "Yank operator -- copy without modifying", cmd: "operator:yank" },
        keymap_entry! { mode: Normal, chord: ">", doc: "Indent-right operator", cmd: "operator:indent-right" },
        keymap_entry! { mode: Normal, chord: "<", doc: "Indent-left operator", cmd: "operator:indent-left" },
        // ---- Normal: standalone deletes/changes ----
        keymap_entry! { mode: Normal, chord: "x", doc: "Delete one char to the right", cmd: "operator:delete" },
        keymap_entry! { mode: Normal, chord: "D", doc: "Delete to end of line (== d$)", cmd: "operator:delete" },
        keymap_entry! { mode: Normal, chord: "C", doc: "Change to end of line (== c$)", cmd: "operator:change" },
        keymap_entry! { mode: Normal, chord: "S", doc: "Substitute current line (== cc)", cmd: "operator:change" },
        keymap_entry! { mode: Normal, chord: "Y", doc: "Yank current line (== yy)", cmd: "operator:yank" },
        keymap_entry! { mode: Normal, chord: "J", doc: "Join current line with next (insert space at boundary)" },
        // ---- Normal: paste ----
        keymap_entry! { mode: Normal, chord: "p", doc: "Paste after cursor / below current line" },
        keymap_entry! { mode: Normal, chord: "P", doc: "Paste before cursor / above current line" },
        // ---- Normal: case ----
        keymap_entry! { mode: Normal, chord: "~", doc: "Toggle case at cursor and advance", cmd: "operator:toggle-case" },
        // ---- Normal: mode entry ----
        keymap_entry! { mode: Normal, chord: "i", doc: "Enter Insert mode at cursor" },
        keymap_entry! { mode: Normal, chord: "a", doc: "Enter Insert mode after cursor" },
        keymap_entry! { mode: Normal, chord: "o", doc: "Open new line below + Insert" },
        keymap_entry! { mode: Normal, chord: "O", doc: "Open new line above + Insert" },
        keymap_entry! { mode: Normal, chord: "v", doc: "Enter Visual (charwise)" },
        keymap_entry! { mode: Normal, chord: "V", doc: "Enter Visual (linewise)" },
        keymap_entry! { mode: Normal, chord: "<C-v>", doc: "Enter Visual (blockwise)" },
        keymap_entry! { mode: Normal, chord: "<C-q>", doc: "Enter Visual (blockwise) -- alternate when terminal hijacks Ctrl-V" },
        keymap_entry! { mode: Normal, chord: "R", doc: "Enter Replace mode" },
        keymap_entry! { mode: Normal, chord: ":", doc: "Enter command-line" },
        // ---- Normal: search ----
        keymap_entry! { mode: Normal, chord: "/", doc: "Forward search" },
        keymap_entry! { mode: Normal, chord: "?", doc: "Backward search" },
        keymap_entry! { mode: Normal, chord: "n", doc: "Next search match (same direction)" },
        keymap_entry! { mode: Normal, chord: "N", doc: "Previous search match (reverse direction)" },
        keymap_entry! { mode: Normal, chord: "*", doc: "Search word under cursor forward" },
        keymap_entry! { mode: Normal, chord: "#", doc: "Search word under cursor backward" },
        keymap_entry! { mode: Normal, chord: "%", doc: "Jump to matching bracket" },
        // ---- Normal: find-char prefixes ----
        keymap_entry! { mode: Normal, chord: "f", doc: "Find char forward (waits for target char)", cmd: "motion:find-char-forward" },
        keymap_entry! { mode: Normal, chord: "F", doc: "Find char backward", cmd: "motion:find-char-backward" },
        keymap_entry! { mode: Normal, chord: "t", doc: "Till char forward (one before)", cmd: "motion:till-char-forward" },
        keymap_entry! { mode: Normal, chord: "T", doc: "Till char backward (one after)", cmd: "motion:till-char-backward" },
        keymap_entry! { mode: Normal, chord: ";", doc: "Repeat last find/till in same direction" },
        keymap_entry! { mode: Normal, chord: ",", doc: "Repeat last find/till in reverse direction" },
        // ---- Normal: marks ----
        keymap_entry! { mode: Normal, chord: "m", doc: "Set named mark (next key is mark name)" },
        keymap_entry! { mode: Normal, chord: "'", doc: "Jump to mark line (next key is mark name)" },
        keymap_entry! { mode: Normal, chord: "`", doc: "Jump to mark exact position (next key is mark name)" },
        // ---- Normal: registers, macros ----
        keymap_entry! { mode: Normal, chord: "\"", doc: "Select register for next operator/paste (next key is register name)" },
        keymap_entry! { mode: Normal, chord: "q", doc: "Start macro recording (next key is register; press q again to stop)" },
        keymap_entry! { mode: Normal, chord: "@", doc: "Play macro from register (next key is register, or @ for last)" },
        // ---- Normal: window management (DESIGN.md §5.9 -- B.1.b) ----
        keymap_entry! { mode: Normal, chord: "<C-w>", doc: "Pending: window-management chord (split / close / navigate)" },
        // ---- After-<C-w> sub-commands ----
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>s", doc: "Split active pane horizontally (new pane below)" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>v", doc: "Split active pane vertically (new pane right)" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>c", doc: "Close active pane" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>q", doc: "Close active pane (alias of <C-w>c)" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>h", doc: "Navigate to pane on the left" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>j", doc: "Navigate to pane below" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>k", doc: "Navigate to pane above" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>l", doc: "Navigate to pane on the right" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>w", doc: "Cycle to next pane" },
        keymap_entry! { mode: AfterCtrlW, chord: "<C-w>W", doc: "Cycle to previous pane" },
        // ---- After-g sub-commands ----
        keymap_entry! { mode: AfterG, chord: "gU", doc: "Uppercase operator -- prefix to motion/text-object; doubled = current line", cmd: "operator:upper" },
        keymap_entry! { mode: AfterG, chord: "gu", doc: "Lowercase operator", cmd: "operator:lower" },
        keymap_entry! { mode: AfterG, chord: "g~", doc: "Toggle-case operator", cmd: "operator:toggle-case" },
        keymap_entry! { mode: AfterG, chord: "gv", doc: "Re-enter Visual with last selection" },
        keymap_entry! { mode: AfterG, chord: "gJ", doc: "Join lines without inserting a space" },
        keymap_entry! { mode: AfterG, chord: "g;", doc: "Walk named-mark history backward" },
        keymap_entry! { mode: AfterG, chord: "g,", doc: "Walk named-mark history forward" },
        keymap_entry! { mode: AfterG, chord: "gd", doc: "LSP: go to definition (textDocument/definition)" },
        keymap_entry! { mode: AfterG, chord: "gD", doc: "LSP: go to declaration (textDocument/declaration)" },
        keymap_entry! { mode: AfterG, chord: "gy", doc: "LSP: go to type definition (textDocument/typeDefinition)" },
        keymap_entry! { mode: AfterG, chord: "gI", doc: "LSP: go to implementation (textDocument/implementation)" },
        keymap_entry! { mode: AfterG, chord: "gr", doc: "LSP: list references (textDocument/references)" },
        keymap_entry! { mode: Normal, chord: "<C-t>", doc: "Tag stack: pop -- walk back through the LIFO chain of LSP go-to-definition drill-downs (independent of <C-o> jump list)" },
        // ---- LSP top-level keys ----
        keymap_entry! { mode: Normal, chord: "K", doc: "LSP: hover documentation popup at cursor (textDocument/hover)" },
        // ---- After-z sub-commands (scroll + folds) ----
        keymap_entry! { mode: AfterZ, chord: "zz", doc: "Center cursor in viewport" },
        keymap_entry! { mode: AfterZ, chord: "z.", doc: "Center cursor in viewport (alias of zz)" },
        keymap_entry! { mode: AfterZ, chord: "zt", doc: "Cursor to top of viewport" },
        keymap_entry! { mode: AfterZ, chord: "z<CR>", doc: "Cursor to top of viewport (alias of zt)" },
        keymap_entry! { mode: AfterZ, chord: "zb", doc: "Cursor to bottom of viewport" },
        keymap_entry! { mode: AfterZ, chord: "z-", doc: "Cursor to bottom of viewport (alias of zb)" },
        keymap_entry! { mode: AfterZ, chord: "zf", doc: "Create fold from current Visual selection" },
        keymap_entry! { mode: AfterZ, chord: "zo", doc: "Open fold at cursor" },
        keymap_entry! { mode: AfterZ, chord: "zc", doc: "Close fold at cursor" },
        keymap_entry! { mode: AfterZ, chord: "za", doc: "Toggle fold at cursor" },
        keymap_entry! { mode: AfterZ, chord: "zR", doc: "Open all folds" },
        keymap_entry! { mode: AfterZ, chord: "zM", doc: "Close all folds" },
        keymap_entry! { mode: AfterZ, chord: "zd", doc: "Delete fold at cursor" },
        keymap_entry! { mode: AfterZ, chord: "zj", doc: "Jump to next fold" },
        keymap_entry! { mode: AfterZ, chord: "zk", doc: "Jump to previous fold" },
        keymap_entry! { mode: AfterZ, chord: "zi", doc: "Toggle foldenable" },
        // ---- Visual mode (motions extend, operators dispatch on Range::Selection) ----
        keymap_entry! { mode: Visual, chord: "<Esc>", doc: "Exit to Normal" },
        keymap_entry! { mode: Visual, chord: "v", doc: "Toggle: exit Visual" },
        keymap_entry! { mode: Visual, chord: "V", doc: "Toggle: exit Visual" },
        keymap_entry! { mode: Visual, chord: "h", doc: "Extend selection left", cmd: "motion:char-left" },
        keymap_entry! { mode: Visual, chord: "j", doc: "Extend selection down", cmd: "motion:line-down" },
        keymap_entry! { mode: Visual, chord: "k", doc: "Extend selection up", cmd: "motion:line-up" },
        keymap_entry! { mode: Visual, chord: "l", doc: "Extend selection right", cmd: "motion:char-right" },
        keymap_entry! { mode: Visual, chord: "0", doc: "Extend to start of line", cmd: "motion:line-start" },
        keymap_entry! { mode: Visual, chord: "$", doc: "Extend to end of line", cmd: "motion:line-end" },
        keymap_entry! { mode: Visual, chord: "^", doc: "Extend to first non-blank", cmd: "motion:first-non-blank" },
        keymap_entry! { mode: Visual, chord: "w", doc: "Extend by word forward", cmd: "motion:word-forward" },
        keymap_entry! { mode: Visual, chord: "b", doc: "Extend by word backward", cmd: "motion:word-backward" },
        keymap_entry! { mode: Visual, chord: "e", doc: "Extend to word end", cmd: "motion:word-end" },
        keymap_entry! { mode: Visual, chord: "G", doc: "Extend to last line", cmd: "motion:goto-last-line" },
        keymap_entry! { mode: Visual, chord: "d", doc: "Delete selection", cmd: "operator:delete" },
        keymap_entry! { mode: Visual, chord: "x", doc: "Delete selection (alias of d)", cmd: "operator:delete" },
        keymap_entry! { mode: Visual, chord: "c", doc: "Change selection (delete + Insert)", cmd: "operator:change" },
        keymap_entry! { mode: Visual, chord: "s", doc: "Change selection (alias of c)", cmd: "operator:change" },
        keymap_entry! { mode: Visual, chord: "y", doc: "Yank selection", cmd: "operator:yank" },
        // ---- Insert mode ----
        keymap_entry! { mode: Insert, chord: "<Esc>", doc: "Exit to Normal" },
        keymap_entry! { mode: Insert, chord: "<BS>", doc: "Delete char to the left" },
        keymap_entry! { mode: Insert, chord: "<CR>", doc: "Insert newline" },
        keymap_entry! { mode: Insert, chord: "<Tab>", doc: "Insert tab character" },
        // ---- Insert-mode completion (Phase 4.2.g.1; CSM.K1
        // retired the `<C-x><C-o>` alias -- `<C-Space>` is the
        // sole popup trigger; per-source filter chords live
        // inside `completion-popup-mode`, contributed via each
        // source mode's `popup_filter_chord` field).
        keymap_entry! { mode: Insert, chord: "<C-Space>", doc: "Manual completion trigger -- opens the popup with sources matching the prefix at the cursor" },
        keymap_entry! { mode: Insert, chord: "<C-x>", doc: "Pending: vim's expansion-prefix family. `<C-x><C-s>` (snippet-expand-at-cursor) is the only live chord today." },
        // Completion-popup minor mode -- bindings active only
        // while `App.insert_completion.is_some()`. Override
        // Insert-mode + Normal-mode meanings (notably <C-d>)
        // for the popup's lifetime; closing the popup
        // restores the original bindings.
        keymap_entry! { mode: CompletionPopup, chord: "<C-n>", doc: "Completion popup: select next candidate" },
        keymap_entry! { mode: CompletionPopup, chord: "<C-p>", doc: "Completion popup: select previous candidate" },
        keymap_entry! { mode: CompletionPopup, chord: "<C-y>", doc: "Completion popup: accept selected candidate (vim convention)" },
        keymap_entry! { mode: CompletionPopup, chord: "<Tab>", doc: "Completion popup: accept selected candidate" },
        keymap_entry! { mode: CompletionPopup, chord: "<CR>", doc: "Completion popup: accept selected candidate" },
        keymap_entry! { mode: CompletionPopup, chord: "<C-e>", doc: "Completion popup: cancel popup, stay in Insert (vim convention)" },
        keymap_entry! { mode: CompletionPopup, chord: "<Esc>", doc: "Completion popup: cancel popup AND exit Insert (vim convention)" },
        keymap_entry! { mode: CompletionPopup, chord: "<C-Space>", doc: "Completion popup: re-trigger / refresh (LSP isIncomplete path)" },
        keymap_entry! { mode: CompletionPopup, chord: "<C-d>", doc: "Completion popup: toggle side documentation popup for the focused candidate" },
        keymap_entry! { mode: CompletionPopup, chord: "<C-f>", doc: "Completion popup: page docs popup forward" },
        keymap_entry! { mode: CompletionPopup, chord: "<C-b>", doc: "Completion popup: page docs popup backward" },
        // ---- Snippets (Phase 4.2.g.4) ----
        keymap_entry! { mode: AfterCtrlX, chord: "<C-x><C-s>", doc: "Direct snippet expansion: look up the word at the cursor in the snippet registry; expand without surfacing the popup" },
        // Active-snippet minor mode -- bindings active only
        // while `App.active_snippet.is_some()`. Override
        // Insert-mode `<Tab>` / `<Esc>` for placeholder
        // navigation; deactivates when the snippet exits.
        keymap_entry! { mode: Snippet, chord: "<Tab>", doc: "Snippet: jump to next placeholder (or exit on $0)" },
        keymap_entry! { mode: Snippet, chord: "<S-Tab>", doc: "Snippet: jump to previous placeholder" },
        keymap_entry! { mode: Snippet, chord: "<Esc>", doc: "Snippet: exit the snippet (placeholders become plain text) and return to Normal" },
        // ---- Replace mode ----
        keymap_entry! { mode: Replace, chord: "<Esc>", doc: "Exit to Normal" },
        keymap_entry! { mode: Replace, chord: "<BS>", doc: "Restore last overwritten byte" },
        keymap_entry! { mode: Replace, chord: "<CR>", doc: "Insert newline" },
        // ---- Command (`:`) ----
        keymap_entry! { mode: Command, chord: "<Esc>", doc: "Cancel command line (or dismiss completion popup if open)" },
        keymap_entry! { mode: Command, chord: "<CR>", doc: "Submit command line (or accept completion if popup is open)" },
        keymap_entry! { mode: Command, chord: "<BS>", doc: "Delete previous char" },
        keymap_entry! { mode: Command, chord: "<Up>", doc: "Walk command-history backward" },
        keymap_entry! { mode: Command, chord: "<Down>", doc: "Walk command-history forward" },
        keymap_entry! { mode: Command, chord: "<Tab>", doc: "Trigger completion / advance to next candidate" },
        keymap_entry! { mode: Command, chord: "<C-h>", doc: "Describe command word / arg under cursor", cmd: "ex:describe-command" },
        keymap_entry! { mode: Command, chord: "<C-u>", doc: "Clear the command line" },
        keymap_entry! { mode: Command, chord: "<C-w>", doc: "Delete the trailing word" },
        // ---- Search (`/` `?`) ----
        keymap_entry! { mode: Search, chord: "<Esc>", doc: "Cancel search" },
        keymap_entry! { mode: Search, chord: "<CR>", doc: "Submit search" },
        keymap_entry! { mode: Search, chord: "<BS>", doc: "Delete previous char" },
        // ---- Help buffer (DESIGN.md §5.11, §5.9) ----
        //
        // Help is a regular buffer routed through Normal-mode chord
        // grammar. Only three buffer-local bindings differ -- they
        // appear here. Motions, page motions, viewport jumps, marks,
        // `<C-o>` / `<C-i>`, etc. all inherit from the Normal-mode
        // entries above; describe-key in Help mode reports them
        // through the `Normal` rows.
        keymap_entry! { mode: Help, chord: "<Esc>", doc: "Dismiss help" },
        keymap_entry! { mode: Help, chord: "q", doc: "Dismiss help" },
        keymap_entry! { mode: Help, chord: "<CR>", doc: "Follow link under cursor" },
    ]
}

/// Look up every binding for a chord across modes. The chord is
/// matched case-sensitively. Used by `:describe-key`.
pub fn lookup(chord: &str) -> Vec<&'static KeymapEntry> {
    default_keymap()
        .iter()
        .filter(|e| e.chord == chord)
        .collect()
}

/// Every entry in mode-grouped order. Used by `:keymap`.
pub fn entries() -> &'static [KeymapEntry] {
    default_keymap()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn lookup_returns_every_mode_a_chord_appears_in() {
        // `j` is bound in Normal (line down) and Visual (extend
        // down). Help inherits Normal's `j` via active-buffer
        // routing, so it doesn't appear as a separate descriptor.
        let hits = lookup("j");
        assert_eq!(hits.len(), 2);
        let modes: HashSet<_> = hits.iter().map(|e| e.mode).collect();
        assert!(modes.contains(&BindingMode::Normal));
        assert!(modes.contains(&BindingMode::Visual));
    }

    #[test]
    fn lookup_unknown_chord_is_empty() {
        assert!(lookup("nope-not-a-chord").is_empty());
    }

    #[test]
    fn motion_chords_link_to_registered_command_names() {
        // Every entry whose `command` is Some must point at a name
        // that the registry actually registers. Drift-test against
        // the builtin populator. Includes ex-commands because
        // Command-mode keymap rows reference `ex:describe-command`
        // for `<C-h>`.
        let mut registry = lattice_grammar::CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        let _ = lattice_grammar::ex_commands::populate(&mut registry);
        for entry in default_keymap() {
            if let Some(name) = entry.command {
                assert!(
                    registry.id_by_name(name).is_some(),
                    "binding `{}` ({}) claims `{}` but registry has no such command",
                    entry.chord,
                    entry.mode.label(),
                    name
                );
            }
        }
    }

    #[test]
    fn no_duplicate_chord_mode_pairs() {
        // Two entries with the same (chord, mode) would both match
        // the same lookup -- a bug.
        let mut seen: HashSet<(&str, BindingMode)> = HashSet::new();
        for entry in default_keymap() {
            assert!(
                seen.insert((entry.chord, entry.mode)),
                "duplicate keymap entry: chord={} mode={:?}",
                entry.chord,
                entry.mode
            );
        }
    }

    #[test]
    fn every_entry_has_a_builtin_source_with_a_line() {
        // Per DESIGN.md §5.11.1 -- every binding MUST carry provenance
        // so :describe-key can render a source link. The keymap_entry!
        // macro is the only construction path; verify it captured
        // the row's location.
        for entry in default_keymap() {
            assert_eq!(
                entry.source.layer,
                lattice_grammar::SourceLayer::Builtin,
                "entry `{}` ({}) source layer should be Builtin",
                entry.chord,
                entry.mode.label()
            );
            match &entry.source.kind {
                lattice_grammar::SourceKind::File { path, line } => {
                    assert!(
                        path.to_string_lossy().contains("keymap.rs"),
                        "expected source path to contain `keymap.rs`, got `{}` (entry `{}` {})",
                        path.display(),
                        entry.chord,
                        entry.mode.label(),
                    );
                    assert!(
                        line.is_some(),
                        "entry `{}` ({}) has no captured line",
                        entry.chord,
                        entry.mode.label()
                    );
                }
                other => panic!("expected File source kind, got {other:?}"),
            }
        }
    }

    #[test]
    fn adjacent_entries_capture_distinct_lines() {
        // The macro injects file!() + line!() per row; rows on
        // different lines must record distinct line numbers,
        // otherwise the per-row capture has regressed.
        let entries = default_keymap();
        for window in entries.windows(2) {
            let (a, b) = (&window[0], &window[1]);
            let line_a = match &a.source.kind {
                lattice_grammar::SourceKind::File { line, .. } => *line,
                _ => None,
            };
            let line_b = match &b.source.kind {
                lattice_grammar::SourceKind::File { line, .. } => *line,
                _ => None,
            };
            // We don't require strict ordering (the table has comment
            // gaps) but adjacent entries on different source lines
            // must record different captured lines.
            if let (Some(a_line), Some(b_line)) = (line_a, line_b) {
                assert!(
                    a_line != b_line,
                    "adjacent entries `{}` ({}) and `{}` ({}) both captured line {a_line}",
                    a.chord,
                    a.mode.label(),
                    b.chord,
                    b.mode.label(),
                );
            }
        }
    }

    #[test]
    fn introspectable_impl_emits_bound_at_source() {
        use lattice_grammar::{Introspectable, SourceLabel};
        let entries = default_keymap();
        let entry = entries.first().expect("at least one entry");
        let sources = entry.sources();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].label, SourceLabel::BoundAt);
    }

    #[test]
    fn introspectable_render_includes_source_link_and_invokes_section() {
        // End-to-end: render an entry with a command through the
        // generic renderer; verify the output has both the
        // `(file:...)` source link and the `(command:...)`
        // cross-reference (markdown link form).
        let entries = default_keymap();
        let entry = entries
            .iter()
            .find(|e| e.command.is_some())
            .expect("at least one entry has a command");
        let lines = lattice_grammar::render_introspection_lines(entry);
        let body = lines.join("\n");
        assert!(
            body.contains("Bound at:"),
            "body missing source label: {body}"
        );
        assert!(
            body.contains("(file:") && body.contains("keymap.rs"),
            "body missing source link: {body}"
        );
        assert!(
            body.contains("(command:"),
            "body missing command cross-reference: {body}"
        );
        assert!(
            body.contains("(built-in)"),
            "body missing source layer label: {body}"
        );
    }

    #[test]
    fn binding_mode_label_is_non_empty() {
        for mode in [
            BindingMode::Normal,
            BindingMode::Insert,
            BindingMode::Visual,
            BindingMode::Replace,
            BindingMode::Command,
            BindingMode::Search,
            BindingMode::OperatorPending,
            BindingMode::AfterG,
            BindingMode::AfterZ,
            BindingMode::AfterMark,
            BindingMode::AfterJumpMarkLine,
            BindingMode::AfterJumpMarkExact,
            BindingMode::AfterRegister,
            BindingMode::AfterMacroStart,
            BindingMode::AfterMacroPlay,
            BindingMode::AfterFindChar,
            BindingMode::AfterTextObject,
            BindingMode::Help,
        ] {
            assert!(!mode.label().is_empty());
        }
    }
}
