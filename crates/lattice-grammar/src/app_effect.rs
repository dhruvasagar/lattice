//! `AppEffect` -- the typed App-side effects produced by free-form
//! `CommandKind::Action` registrations.
//!
//! Background: the keymap-bound `Action` enum in `lattice-ui-tui`
//! historically encoded the App's response to non-grammar chords
//! (`<Esc>`, `o`, `<C-w>v`, ...). Slice 8.i (see
//! [`docs/8i-approach.md`](../../../docs/8i-approach.md)) retires
//! the per-binding `Action` bridge and routes those chords through
//! the unified dispatcher's `CommandKind::Action` branch instead.
//! Action-kind registry entries return `Effect::AppAction(AppEffect)`;
//! the App's `apply_effect` then matches on the inner `AppEffect`
//! variant.
//!
//! Why a sibling type to `Effect` rather than a flat extension:
//!
//! - `Effect`'s existing variants describe *core / dispatcher-native*
//!   work (`Edits`, `SelectionChange`, `Yank`, `EnterMode`,
//!   `Substitute`, ...) and *ex-command-emitted* App work
//!   (`SaveBuffer`, `OpenBuffer`, `LspRestart`, ...). Both are
//!   produced by code paths that *resolve* a typed grammar concern
//!   into a typed effect.
//! - `AppEffect` is for chord bindings that historically had no
//!   grammar concept attached -- `<Esc>` exits Visual, `<C-w>v`
//!   splits a pane, `o` opens a line below. Treating those as
//!   first-class "free-form actions" keeps the dispatcher contract
//!   ("everything returns Effect") honest without fusing two
//!   conceptually different surfaces into one giant enum.
//!
//! 8.i.0 ships the carrier with `Quit` only -- the smallest
//! variant that proves the dispatcher path. Slices 8.i.1-3 grow
//! `AppEffect` as the per-mode bindings migrate; slice 8.i.4
//! retires the `bind_legacy` bridge entirely.

use serde::{Deserialize, Serialize};

/// App-side typed effect produced by a `CommandKind::Action`
/// dispatch (DESIGN.md §5.2.1, see also `docs/8i-approach.md`).
///
/// Variants are added incrementally during slice 8.i as each
/// historical `Action` variant is promoted from the legacy
/// `bind_legacy` bridge to a typed `CommandInvocation`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppEffect {
    /// Graceful editor shutdown. The App's `apply_effect` arm
    /// publishes `Event::BeforeQuit` and sets the `should_quit`
    /// flag the runtime polls between frames; matches today's
    /// `Action::Quit` semantics exactly. (8.i.0 smoke variant;
    /// the `<C-c>` binding lives in `input.rs` so the legacy
    /// site doesn't migrate until the input-layer rewrite.)
    Quit,
    /// Vim's `%`. Jumps to the bracket / brace / paren matching
    /// the one at-or-after the cursor on the current line.
    /// Promoted from `Action::MatchBracket` in slice 8.i.1.a.
    MatchBracket,
    /// Vim's `~`. Toggles the case of the char at the cursor and
    /// advances by one byte. Promoted from
    /// `Action::ToggleCaseAtCursor` in slice 8.i.1.a.
    ToggleCaseAtCursor,
    /// Vim's `o`. Opens a new line below the current line and
    /// enters Insert. Promoted from `Action::OpenLineBelow` in
    /// slice 8.i.1.a.
    OpenLineBelow,
    /// Vim's `O`. Opens a new line above the current line and
    /// enters Insert. Promoted from `Action::OpenLineAbove` in
    /// slice 8.i.1.a.
    OpenLineAbove,
    /// `K` (Phase 4.2.b). Sends `textDocument/hover` to every
    /// LSP server attached to the active document; renders the
    /// first non-empty markdown body in the hover popup.
    /// Promoted from `Action::LspHoverRequest` in slice 8.i.1.a.
    LspHoverRequest,
    /// Vim's `n`. Re-runs the last search forward. Promoted from
    /// `Action::SearchNext` in slice 8.i.1.b.
    SearchNext,
    /// Vim's `N`. Re-runs the last search in the reverse
    /// direction. Promoted from `Action::SearchPrevious` in
    /// slice 8.i.1.b.
    SearchPrevious,
    /// Vim's `<C-o>`. Walk one step backward through the position
    /// history (DESIGN.md §5.1.1). Promoted from
    /// `Action::JumpHistoryBack` in slice 8.i.1.b.
    JumpHistoryBack,
    /// Vim's `<C-i>` / `<Tab>`. Walk one step forward through the
    /// position history. Promoted from `Action::JumpHistoryForward`
    /// in slice 8.i.1.b.
    JumpHistoryForward,
    /// Vim's `g;`. Walk one step backward through the
    /// mark history (oldest -> newest cursor positions in this
    /// buffer). Promoted from `Action::WalkMarkHistoryBack` in
    /// slice 8.i.1.b.
    WalkMarkHistoryBack,
    /// Vim's `g,`. Walk one step forward through the mark
    /// history. Promoted from `Action::WalkMarkHistoryForward` in
    /// slice 8.i.1.b.
    WalkMarkHistoryForward,
    /// Vim's `<C-t>`. Pop one entry off the tag stack and jump
    /// back to where the previous `gd` / `<C-]>` originated.
    /// Promoted from `Action::TagStackPop` in slice 8.i.1.b.
    TagStackPop,
    /// Vim's `zo`. Open the fold containing the cursor.
    /// Promoted from `Action::OpenFoldAtCursor` in slice 8.i.1.c.
    OpenFoldAtCursor,
    /// Vim's `zc`. Close the fold containing the cursor.
    /// Promoted from `Action::CloseFoldAtCursor` in slice 8.i.1.c.
    CloseFoldAtCursor,
    /// Vim's `za`. Toggle the fold containing the cursor.
    /// Promoted from `Action::ToggleFoldAtCursor` in slice 8.i.1.c.
    ToggleFoldAtCursor,
    /// Vim's `zR`. Open every fold in the buffer.
    /// Promoted from `Action::OpenAllFolds` in slice 8.i.1.c.
    OpenAllFolds,
    /// Vim's `zM`. Close every fold in the buffer.
    /// Promoted from `Action::CloseAllFolds` in slice 8.i.1.c.
    CloseAllFolds,
    /// Vim's `zd`. Delete the fold containing the cursor (drop
    /// it from the manual fold table; structure-driven folds
    /// reappear on the next reparse). Promoted from
    /// `Action::DeleteFoldAtCursor` in slice 8.i.1.c.
    DeleteFoldAtCursor,
    /// Vim's `zj`. Move cursor to the start of the next fold.
    /// Promoted from `Action::GotoNextFold` in slice 8.i.1.c.
    GotoNextFold,
    /// Vim's `zk`. Move cursor to the end of the previous fold.
    /// Promoted from `Action::GotoPrevFold` in slice 8.i.1.c.
    GotoPrevFold,
    /// Vim's `zi`. Toggle the `foldenable` option (when off,
    /// every line renders flat regardless of any closed flag).
    /// Promoted from `Action::ToggleFoldEnable` in slice 8.i.1.c.
    ToggleFoldEnable,
    /// Vim's `u`. Undo the last buffer change. Promoted from
    /// `Action::Undo` in slice 8.i.1.d.
    Undo,
    /// Vim's `<C-r>`. Redo the last undone change. Promoted from
    /// `Action::Redo` in slice 8.i.1.d.
    Redo,
    /// Vim's `.`. Repeat the last change (operator + motion +
    /// register + count). Promoted from `Action::RepeatLastChange`
    /// in slice 8.i.1.d.
    RepeatLastChange,
    /// Vim's `<C-f>`. Page-down: scroll the viewport down one
    /// page. Promoted from `Action::PageDown` in slice 8.i.1.d.
    PageDown,
    /// Vim's `<C-b>`. Page-up: scroll the viewport up one page.
    /// Promoted from `Action::PageUp` in slice 8.i.1.d.
    PageUp,
    /// Vim's `<C-y>`. Scroll viewport up one line (cursor
    /// stays at the same screen position when possible).
    /// Promoted from `Action::ScrollLineUp` in slice 8.i.1.d.
    ScrollLineUp,
    /// Vim's `<C-e>`. Scroll viewport down one line. Promoted
    /// from `Action::ScrollLineDown` in slice 8.i.1.d.
    ScrollLineDown,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn quit_round_trips_through_serde() {
        let q = AppEffect::Quit;
        let s = serde_json::to_string(&q).unwrap();
        let back: AppEffect = serde_json::from_str(&s).unwrap();
        assert_eq!(q, back);
    }
}
