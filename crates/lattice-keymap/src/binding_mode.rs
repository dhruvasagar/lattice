//! `BindingMode` — the vim-modal state a chord resolves in.
//!
//! K.3 (2026-06-07): moved from `lattice-mode` into `lattice-keymap`
//! so the keymap trie, `KeymapLayer`, and `resolve_trace` can reference
//! the mode enum without a dep cycle back to `lattice-mode`.
//!
//! `lattice-mode::BindingMode` and `lattice-host::keymap::BindingMode`
//! are retained as re-export shims.

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
    /// Human-readable label used in `:describe-key` output and diagnostics.
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

    /// All variants in declaration order. Used by
    /// `KeymapHandle::resolve_trace_all_modes` to iterate every mode.
    pub fn all() -> &'static [BindingMode] {
        use BindingMode::*;
        &[
            Normal, Insert, Visual, Replace, Command, Search,
            OperatorPending, AfterG, AfterZ, AfterMark,
            AfterJumpMarkLine, AfterJumpMarkExact, AfterRegister,
            AfterMacroStart, AfterMacroPlay, AfterFindChar,
            AfterTextObject, Help, AfterCtrlW, AfterCtrlX,
            CompletionPopup, Snippet,
        ]
    }
}

impl std::fmt::Display for BindingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_covers_all_variants() {
        // Exhaustive check: every variant must return a non-empty label.
        let modes = [
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
            BindingMode::AfterCtrlW,
            BindingMode::AfterCtrlX,
            BindingMode::CompletionPopup,
            BindingMode::Snippet,
        ];
        for m in modes {
            assert!(!m.label().is_empty(), "empty label for {m:?}");
        }
    }

    #[test]
    fn display_equals_label() {
        assert_eq!(format!("{}", BindingMode::Normal), "Normal");
        assert_eq!(format!("{}", BindingMode::Insert), "Insert");
        assert_eq!(format!("{}", BindingMode::OperatorPending), "Operator-Pending");
    }

    #[test]
    fn all_covers_every_variant() {
        // Must match the count in `label_covers_all_variants`.
        assert_eq!(BindingMode::all().len(), 22);
        // Every variant must appear exactly once (no duplicates).
        let mut seen = std::collections::HashSet::new();
        for m in BindingMode::all() {
            assert!(seen.insert(*m), "duplicate: {m:?}");
        }
    }
}
