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
        }
    }
}

/// One row in the catalog. Ordering of fields matches the rendering
/// order in `:describe-key` so reading one field at a time still tells
/// a coherent story.
#[derive(Debug, Clone, Copy)]
pub struct KeymapEntry {
    pub chord: &'static str,
    pub mode: BindingMode,
    pub doc: &'static str,
    /// Canonical name in the `CommandRegistry`. `None` for synthetic
    /// actions (`PushDigit`, `SetPending`, `StartMacroRecord`, ...) that
    /// don't bind a registered command.
    pub command: Option<&'static str>,
}

/// The full vim default keymap. Order matters for tests: chords
/// that share a prefix (e.g. `g` vs `gg`) appear adjacent, so a
/// reader scanning the table sees the prefix relationship.
pub static DEFAULT_KEYMAP: &[KeymapEntry] = &[
    // ---- Normal: motions ----
    KeymapEntry { chord: "h", mode: BindingMode::Normal, doc: "Move cursor left", command: Some("motion:char-left") },
    KeymapEntry { chord: "<Left>", mode: BindingMode::Normal, doc: "Move cursor left", command: Some("motion:char-left") },
    KeymapEntry { chord: "j", mode: BindingMode::Normal, doc: "Move cursor down", command: Some("motion:line-down") },
    KeymapEntry { chord: "<Down>", mode: BindingMode::Normal, doc: "Move cursor down", command: Some("motion:line-down") },
    KeymapEntry { chord: "k", mode: BindingMode::Normal, doc: "Move cursor up", command: Some("motion:line-up") },
    KeymapEntry { chord: "<Up>", mode: BindingMode::Normal, doc: "Move cursor up", command: Some("motion:line-up") },
    KeymapEntry { chord: "l", mode: BindingMode::Normal, doc: "Move cursor right", command: Some("motion:char-right") },
    KeymapEntry { chord: "<Right>", mode: BindingMode::Normal, doc: "Move cursor right", command: Some("motion:char-right") },
    KeymapEntry { chord: "0", mode: BindingMode::Normal, doc: "Jump to start of line (column 0)", command: Some("motion:line-start") },
    KeymapEntry { chord: "<Home>", mode: BindingMode::Normal, doc: "Jump to start of line", command: Some("motion:line-start") },
    KeymapEntry { chord: "$", mode: BindingMode::Normal, doc: "Jump to end of line", command: Some("motion:line-end") },
    KeymapEntry { chord: "<End>", mode: BindingMode::Normal, doc: "Jump to end of line", command: Some("motion:line-end") },
    KeymapEntry { chord: "^", mode: BindingMode::Normal, doc: "Jump to first non-blank on line", command: Some("motion:first-non-blank") },
    KeymapEntry { chord: "w", mode: BindingMode::Normal, doc: "Word forward (start of next word)", command: Some("motion:word-forward") },
    KeymapEntry { chord: "b", mode: BindingMode::Normal, doc: "Word backward (start of previous word)", command: Some("motion:word-backward") },
    KeymapEntry { chord: "e", mode: BindingMode::Normal, doc: "Word end (last byte of current/next word)", command: Some("motion:word-end") },
    KeymapEntry { chord: "W", mode: BindingMode::Normal, doc: "WORD forward (whitespace-delimited)", command: Some("motion:big-word-forward") },
    KeymapEntry { chord: "B", mode: BindingMode::Normal, doc: "WORD backward", command: Some("motion:big-word-backward") },
    KeymapEntry { chord: "E", mode: BindingMode::Normal, doc: "WORD end", command: Some("motion:big-word-end") },
    KeymapEntry { chord: "}", mode: BindingMode::Normal, doc: "Next paragraph boundary", command: Some("motion:paragraph-forward") },
    KeymapEntry { chord: "{", mode: BindingMode::Normal, doc: "Previous paragraph boundary", command: Some("motion:paragraph-backward") },
    KeymapEntry { chord: ")", mode: BindingMode::Normal, doc: "Next sentence", command: Some("motion:sentence-forward") },
    KeymapEntry { chord: "(", mode: BindingMode::Normal, doc: "Previous sentence", command: Some("motion:sentence-backward") },
    KeymapEntry { chord: "G", mode: BindingMode::Normal, doc: "Jump to last line", command: Some("motion:goto-last-line") },
    KeymapEntry { chord: "gg", mode: BindingMode::Normal, doc: "Jump to first line", command: Some("motion:goto-first-line") },

    // ---- Normal: viewport jumps ----
    KeymapEntry { chord: "H", mode: BindingMode::Normal, doc: "Cursor to top of viewport", command: None },
    KeymapEntry { chord: "M", mode: BindingMode::Normal, doc: "Cursor to middle of viewport", command: None },
    KeymapEntry { chord: "L", mode: BindingMode::Normal, doc: "Cursor to bottom of viewport", command: None },

    // ---- Normal: scrolling ----
    KeymapEntry { chord: "<C-d>", mode: BindingMode::Normal, doc: "Scroll half-page down (10 lines)", command: Some("motion:line-down") },
    KeymapEntry { chord: "<C-u>", mode: BindingMode::Normal, doc: "Scroll half-page up (10 lines)", command: Some("motion:line-up") },
    KeymapEntry { chord: "<C-f>", mode: BindingMode::Normal, doc: "Page down", command: None },
    KeymapEntry { chord: "<C-b>", mode: BindingMode::Normal, doc: "Page up", command: None },
    KeymapEntry { chord: "<C-e>", mode: BindingMode::Normal, doc: "Scroll viewport down one line", command: None },
    KeymapEntry { chord: "<C-y>", mode: BindingMode::Normal, doc: "Scroll viewport up one line", command: None },
    KeymapEntry { chord: "<PageDown>", mode: BindingMode::Normal, doc: "Page down (10 lines)", command: Some("motion:line-down") },
    KeymapEntry { chord: "<PageUp>", mode: BindingMode::Normal, doc: "Page up (10 lines)", command: Some("motion:line-up") },

    // ---- Normal: undo/redo, dot, jump-list ----
    KeymapEntry { chord: "u", mode: BindingMode::Normal, doc: "Undo last change", command: None },
    KeymapEntry { chord: "<C-r>", mode: BindingMode::Normal, doc: "Redo (reverse undo)", command: None },
    KeymapEntry { chord: ".", mode: BindingMode::Normal, doc: "Repeat last change (dot-repeat)", command: None },
    KeymapEntry { chord: "<C-o>", mode: BindingMode::Normal, doc: "Jump-list back (previous AutoJump position)", command: None },
    KeymapEntry { chord: "<C-i>", mode: BindingMode::Normal, doc: "Jump-list forward", command: None },
    KeymapEntry { chord: "<Tab>", mode: BindingMode::Normal, doc: "Jump-list forward (terminal alias for Ctrl-I)", command: None },

    // ---- Normal: pending-key prefixes ----
    KeymapEntry { chord: "g", mode: BindingMode::Normal, doc: "Pending: second key resolves to gg/gU/gu/g~/gv/gJ/g;/g,", command: None },
    KeymapEntry { chord: "z", mode: BindingMode::Normal, doc: "Pending: scroll/fold sub-commands", command: None },
    KeymapEntry { chord: "d", mode: BindingMode::Normal, doc: "Delete operator -- use with motion/text-object; doubled (`dd`) deletes current line", command: Some("operator:delete") },
    KeymapEntry { chord: "c", mode: BindingMode::Normal, doc: "Change operator -- delete then enter Insert", command: Some("operator:change") },
    KeymapEntry { chord: "y", mode: BindingMode::Normal, doc: "Yank operator -- copy without modifying", command: Some("operator:yank") },
    KeymapEntry { chord: ">", mode: BindingMode::Normal, doc: "Indent-right operator", command: Some("operator:indent-right") },
    KeymapEntry { chord: "<", mode: BindingMode::Normal, doc: "Indent-left operator", command: Some("operator:indent-left") },

    // ---- Normal: standalone deletes/changes ----
    KeymapEntry { chord: "x", mode: BindingMode::Normal, doc: "Delete one char to the right", command: Some("operator:delete") },
    KeymapEntry { chord: "D", mode: BindingMode::Normal, doc: "Delete to end of line (== d$)", command: Some("operator:delete") },
    KeymapEntry { chord: "C", mode: BindingMode::Normal, doc: "Change to end of line (== c$)", command: Some("operator:change") },
    KeymapEntry { chord: "S", mode: BindingMode::Normal, doc: "Substitute current line (== cc)", command: Some("operator:change") },
    KeymapEntry { chord: "Y", mode: BindingMode::Normal, doc: "Yank current line (== yy)", command: Some("operator:yank") },
    KeymapEntry { chord: "J", mode: BindingMode::Normal, doc: "Join current line with next (insert space at boundary)", command: None },

    // ---- Normal: paste ----
    KeymapEntry { chord: "p", mode: BindingMode::Normal, doc: "Paste after cursor / below current line", command: None },
    KeymapEntry { chord: "P", mode: BindingMode::Normal, doc: "Paste before cursor / above current line", command: None },

    // ---- Normal: case ----
    KeymapEntry { chord: "~", mode: BindingMode::Normal, doc: "Toggle case at cursor and advance", command: Some("operator:toggle-case") },

    // ---- Normal: mode entry ----
    KeymapEntry { chord: "i", mode: BindingMode::Normal, doc: "Enter Insert mode at cursor", command: None },
    KeymapEntry { chord: "a", mode: BindingMode::Normal, doc: "Enter Insert mode after cursor", command: None },
    KeymapEntry { chord: "o", mode: BindingMode::Normal, doc: "Open new line below + Insert", command: None },
    KeymapEntry { chord: "O", mode: BindingMode::Normal, doc: "Open new line above + Insert", command: None },
    KeymapEntry { chord: "v", mode: BindingMode::Normal, doc: "Enter Visual (charwise)", command: None },
    KeymapEntry { chord: "V", mode: BindingMode::Normal, doc: "Enter Visual (linewise)", command: None },
    KeymapEntry { chord: "<C-v>", mode: BindingMode::Normal, doc: "Enter Visual (blockwise)", command: None },
    KeymapEntry { chord: "<C-q>", mode: BindingMode::Normal, doc: "Enter Visual (blockwise) -- alternate when terminal hijacks Ctrl-V", command: None },
    KeymapEntry { chord: "R", mode: BindingMode::Normal, doc: "Enter Replace mode", command: None },
    KeymapEntry { chord: ":", mode: BindingMode::Normal, doc: "Enter command-line", command: None },

    // ---- Normal: search ----
    KeymapEntry { chord: "/", mode: BindingMode::Normal, doc: "Forward search", command: None },
    KeymapEntry { chord: "?", mode: BindingMode::Normal, doc: "Backward search", command: None },
    KeymapEntry { chord: "n", mode: BindingMode::Normal, doc: "Next search match (same direction)", command: None },
    KeymapEntry { chord: "N", mode: BindingMode::Normal, doc: "Previous search match (reverse direction)", command: None },
    KeymapEntry { chord: "*", mode: BindingMode::Normal, doc: "Search word under cursor forward", command: None },
    KeymapEntry { chord: "#", mode: BindingMode::Normal, doc: "Search word under cursor backward", command: None },
    KeymapEntry { chord: "%", mode: BindingMode::Normal, doc: "Jump to matching bracket", command: None },

    // ---- Normal: find-char prefixes ----
    KeymapEntry { chord: "f", mode: BindingMode::Normal, doc: "Find char forward (waits for target char)", command: Some("motion:find-char-forward") },
    KeymapEntry { chord: "F", mode: BindingMode::Normal, doc: "Find char backward", command: Some("motion:find-char-backward") },
    KeymapEntry { chord: "t", mode: BindingMode::Normal, doc: "Till char forward (one before)", command: Some("motion:till-char-forward") },
    KeymapEntry { chord: "T", mode: BindingMode::Normal, doc: "Till char backward (one after)", command: Some("motion:till-char-backward") },
    KeymapEntry { chord: ";", mode: BindingMode::Normal, doc: "Repeat last find/till in same direction", command: None },
    KeymapEntry { chord: ",", mode: BindingMode::Normal, doc: "Repeat last find/till in reverse direction", command: None },

    // ---- Normal: marks ----
    KeymapEntry { chord: "m", mode: BindingMode::Normal, doc: "Set named mark (next key is mark name)", command: None },
    KeymapEntry { chord: "'", mode: BindingMode::Normal, doc: "Jump to mark line (next key is mark name)", command: None },
    KeymapEntry { chord: "`", mode: BindingMode::Normal, doc: "Jump to mark exact position (next key is mark name)", command: None },

    // ---- Normal: registers, macros ----
    KeymapEntry { chord: "\"", mode: BindingMode::Normal, doc: "Select register for next operator/paste (next key is register name)", command: None },
    KeymapEntry { chord: "q", mode: BindingMode::Normal, doc: "Start macro recording (next key is register; press q again to stop)", command: None },
    KeymapEntry { chord: "@", mode: BindingMode::Normal, doc: "Play macro from register (next key is register, or @ for last)", command: None },

    // ---- After-g sub-commands ----
    KeymapEntry { chord: "gU", mode: BindingMode::AfterG, doc: "Uppercase operator -- prefix to motion/text-object; doubled = current line", command: Some("operator:upper") },
    KeymapEntry { chord: "gu", mode: BindingMode::AfterG, doc: "Lowercase operator", command: Some("operator:lower") },
    KeymapEntry { chord: "g~", mode: BindingMode::AfterG, doc: "Toggle-case operator", command: Some("operator:toggle-case") },
    KeymapEntry { chord: "gv", mode: BindingMode::AfterG, doc: "Re-enter Visual with last selection", command: None },
    KeymapEntry { chord: "gJ", mode: BindingMode::AfterG, doc: "Join lines without inserting a space", command: None },
    KeymapEntry { chord: "g;", mode: BindingMode::AfterG, doc: "Walk named-mark history backward", command: None },
    KeymapEntry { chord: "g,", mode: BindingMode::AfterG, doc: "Walk named-mark history forward", command: None },

    // ---- After-z sub-commands (scroll + folds) ----
    KeymapEntry { chord: "zz", mode: BindingMode::AfterZ, doc: "Center cursor in viewport", command: None },
    KeymapEntry { chord: "z.", mode: BindingMode::AfterZ, doc: "Center cursor in viewport (alias of zz)", command: None },
    KeymapEntry { chord: "zt", mode: BindingMode::AfterZ, doc: "Cursor to top of viewport", command: None },
    KeymapEntry { chord: "z<CR>", mode: BindingMode::AfterZ, doc: "Cursor to top of viewport (alias of zt)", command: None },
    KeymapEntry { chord: "zb", mode: BindingMode::AfterZ, doc: "Cursor to bottom of viewport", command: None },
    KeymapEntry { chord: "z-", mode: BindingMode::AfterZ, doc: "Cursor to bottom of viewport (alias of zb)", command: None },
    KeymapEntry { chord: "zf", mode: BindingMode::AfterZ, doc: "Create fold from current Visual selection", command: None },
    KeymapEntry { chord: "zo", mode: BindingMode::AfterZ, doc: "Open fold at cursor", command: None },
    KeymapEntry { chord: "zc", mode: BindingMode::AfterZ, doc: "Close fold at cursor", command: None },
    KeymapEntry { chord: "za", mode: BindingMode::AfterZ, doc: "Toggle fold at cursor", command: None },
    KeymapEntry { chord: "zR", mode: BindingMode::AfterZ, doc: "Open all folds", command: None },
    KeymapEntry { chord: "zM", mode: BindingMode::AfterZ, doc: "Close all folds", command: None },
    KeymapEntry { chord: "zd", mode: BindingMode::AfterZ, doc: "Delete fold at cursor", command: None },
    KeymapEntry { chord: "zj", mode: BindingMode::AfterZ, doc: "Jump to next fold", command: None },
    KeymapEntry { chord: "zk", mode: BindingMode::AfterZ, doc: "Jump to previous fold", command: None },

    // ---- Visual mode (motions extend, operators dispatch on Range::Selection) ----
    KeymapEntry { chord: "<Esc>", mode: BindingMode::Visual, doc: "Exit to Normal", command: None },
    KeymapEntry { chord: "v", mode: BindingMode::Visual, doc: "Toggle: exit Visual", command: None },
    KeymapEntry { chord: "V", mode: BindingMode::Visual, doc: "Toggle: exit Visual", command: None },
    KeymapEntry { chord: "h", mode: BindingMode::Visual, doc: "Extend selection left", command: Some("motion:char-left") },
    KeymapEntry { chord: "j", mode: BindingMode::Visual, doc: "Extend selection down", command: Some("motion:line-down") },
    KeymapEntry { chord: "k", mode: BindingMode::Visual, doc: "Extend selection up", command: Some("motion:line-up") },
    KeymapEntry { chord: "l", mode: BindingMode::Visual, doc: "Extend selection right", command: Some("motion:char-right") },
    KeymapEntry { chord: "0", mode: BindingMode::Visual, doc: "Extend to start of line", command: Some("motion:line-start") },
    KeymapEntry { chord: "$", mode: BindingMode::Visual, doc: "Extend to end of line", command: Some("motion:line-end") },
    KeymapEntry { chord: "^", mode: BindingMode::Visual, doc: "Extend to first non-blank", command: Some("motion:first-non-blank") },
    KeymapEntry { chord: "w", mode: BindingMode::Visual, doc: "Extend by word forward", command: Some("motion:word-forward") },
    KeymapEntry { chord: "b", mode: BindingMode::Visual, doc: "Extend by word backward", command: Some("motion:word-backward") },
    KeymapEntry { chord: "e", mode: BindingMode::Visual, doc: "Extend to word end", command: Some("motion:word-end") },
    KeymapEntry { chord: "G", mode: BindingMode::Visual, doc: "Extend to last line", command: Some("motion:goto-last-line") },
    KeymapEntry { chord: "d", mode: BindingMode::Visual, doc: "Delete selection", command: Some("operator:delete") },
    KeymapEntry { chord: "x", mode: BindingMode::Visual, doc: "Delete selection (alias of d)", command: Some("operator:delete") },
    KeymapEntry { chord: "c", mode: BindingMode::Visual, doc: "Change selection (delete + Insert)", command: Some("operator:change") },
    KeymapEntry { chord: "s", mode: BindingMode::Visual, doc: "Change selection (alias of c)", command: Some("operator:change") },
    KeymapEntry { chord: "y", mode: BindingMode::Visual, doc: "Yank selection", command: Some("operator:yank") },

    // ---- Insert mode ----
    KeymapEntry { chord: "<Esc>", mode: BindingMode::Insert, doc: "Exit to Normal", command: None },
    KeymapEntry { chord: "<BS>", mode: BindingMode::Insert, doc: "Delete char to the left", command: None },
    KeymapEntry { chord: "<CR>", mode: BindingMode::Insert, doc: "Insert newline", command: None },
    KeymapEntry { chord: "<Tab>", mode: BindingMode::Insert, doc: "Insert tab character", command: None },

    // ---- Replace mode ----
    KeymapEntry { chord: "<Esc>", mode: BindingMode::Replace, doc: "Exit to Normal", command: None },
    KeymapEntry { chord: "<BS>", mode: BindingMode::Replace, doc: "Restore last overwritten byte", command: None },
    KeymapEntry { chord: "<CR>", mode: BindingMode::Replace, doc: "Insert newline", command: None },

    // ---- Command (`:`) ----
    KeymapEntry { chord: "<Esc>", mode: BindingMode::Command, doc: "Cancel command line", command: None },
    KeymapEntry { chord: "<CR>", mode: BindingMode::Command, doc: "Submit command line", command: None },
    KeymapEntry { chord: "<BS>", mode: BindingMode::Command, doc: "Delete previous char", command: None },
    KeymapEntry { chord: "<Up>", mode: BindingMode::Command, doc: "Walk command-history backward", command: None },
    KeymapEntry { chord: "<Down>", mode: BindingMode::Command, doc: "Walk command-history forward", command: None },

    // ---- Search (`/` `?`) ----
    KeymapEntry { chord: "<Esc>", mode: BindingMode::Search, doc: "Cancel search", command: None },
    KeymapEntry { chord: "<CR>", mode: BindingMode::Search, doc: "Submit search", command: None },
    KeymapEntry { chord: "<BS>", mode: BindingMode::Search, doc: "Delete previous char", command: None },

    // ---- Help overlay ----
    KeymapEntry { chord: "<Esc>", mode: BindingMode::Help, doc: "Dismiss help", command: None },
    KeymapEntry { chord: "q", mode: BindingMode::Help, doc: "Dismiss help", command: None },
    KeymapEntry { chord: "j", mode: BindingMode::Help, doc: "Scroll down one line", command: None },
    KeymapEntry { chord: "k", mode: BindingMode::Help, doc: "Scroll up one line", command: None },
    KeymapEntry { chord: "<Down>", mode: BindingMode::Help, doc: "Scroll down one line", command: None },
    KeymapEntry { chord: "<Up>", mode: BindingMode::Help, doc: "Scroll up one line", command: None },
    KeymapEntry { chord: "<C-d>", mode: BindingMode::Help, doc: "Scroll down one page", command: None },
    KeymapEntry { chord: "<C-u>", mode: BindingMode::Help, doc: "Scroll up one page", command: None },
    KeymapEntry { chord: "<PageDown>", mode: BindingMode::Help, doc: "Scroll down one page", command: None },
    KeymapEntry { chord: "<PageUp>", mode: BindingMode::Help, doc: "Scroll up one page", command: None },
    KeymapEntry { chord: "g", mode: BindingMode::Help, doc: "Jump to top", command: None },
    KeymapEntry { chord: "G", mode: BindingMode::Help, doc: "Jump to bottom", command: None },
];

/// Look up every binding for a chord across modes. The chord is
/// matched case-sensitively. Used by `:describe-key`.
pub fn lookup(chord: &str) -> Vec<&'static KeymapEntry> {
    DEFAULT_KEYMAP
        .iter()
        .filter(|e| e.chord == chord)
        .collect()
}

/// Every entry in mode-grouped order. Used by `:keymap`.
pub fn entries() -> &'static [KeymapEntry] {
    DEFAULT_KEYMAP
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn lookup_returns_every_mode_a_chord_appears_in() {
        // `j` is bound in Normal (line down), Visual (extend down),
        // and Help (scroll down). lookup() returns all three.
        let hits = lookup("j");
        assert_eq!(hits.len(), 3);
        let modes: HashSet<_> = hits.iter().map(|e| e.mode).collect();
        assert!(modes.contains(&BindingMode::Normal));
        assert!(modes.contains(&BindingMode::Visual));
        assert!(modes.contains(&BindingMode::Help));
    }

    #[test]
    fn lookup_unknown_chord_is_empty() {
        assert!(lookup("nope-not-a-chord").is_empty());
    }

    #[test]
    fn motion_chords_link_to_registered_command_names() {
        // Every entry whose `command` is Some must point at a name
        // that the registry actually registers. Drift-test against
        // the builtin populator.
        let mut registry = lattice_grammar::CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut registry);
        for entry in DEFAULT_KEYMAP {
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
        for entry in DEFAULT_KEYMAP {
            assert!(
                seen.insert((entry.chord, entry.mode)),
                "duplicate keymap entry: chord={} mode={:?}",
                entry.chord,
                entry.mode
            );
        }
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
