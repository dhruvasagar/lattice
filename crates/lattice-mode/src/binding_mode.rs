//! Vim-modal binding-mode enum.
//!
//! K.2.2 (2026-06-01): moved from `lattice-host::keymap` into
//! `lattice-mode`. The enum names the modal state a chord
//! resolves in (Normal, Insert, Visual, …), and is reached by
//! the `Keymap` contribution type that `Mode::keymap()` returns
//! (per `keymap-architecture.md` §11.2). Living in
//! `lattice-mode` lets mode crates name their bindings'
//! binding-mode without depending on `lattice-host`.
//!
//! `lattice-host::keymap::BindingMode` is retained as a
//! re-export shim for the existing matcher / dispatcher / TUI
//! call sites; this is the canonical home.

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
