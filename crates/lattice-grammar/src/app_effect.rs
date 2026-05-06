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

use crate::modal::{ModalState, SearchDirection, VisualKind};

/// Vim's `H` / `M` / `L` target positions: where in the visible
/// viewport the cursor lands. App-side concept hosted here so
/// `AppEffect::JumpViewport(ViewportPos)` can carry the typed
/// payload without `lattice-ui-tui` having to dance through a
/// dependency cycle. Slice 8.i.2.c hoist; the App's previous
/// `crate::app::ViewportPos` becomes a `pub use` re-export of
/// this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewportPos {
    /// `H` -- first visible line of the viewport.
    Top,
    /// `M` -- middle visible line of the viewport.
    Middle,
    /// `L` -- last visible line of the viewport.
    Bottom,
}

/// Vim's `zz` / `zt` / `zb` target positions: where in the
/// viewport the cursor's current line should sit after the
/// scroll. App-side concept hosted alongside [`ViewportPos`] for
/// the same dependency reason. Slice 8.i.2.c hoist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScrollPos {
    /// `zt` -- cursor's line lands at the top of the viewport.
    Top,
    /// `zz` -- cursor's line lands at the vertical centre.
    Center,
    /// `zb` -- cursor's line lands at the bottom of the viewport.
    Bottom,
}

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
    /// Vim's `<C-l>`. Force a full screen redraw. Promoted from
    /// `Action::RedrawScreen` in slice 8.i.1.e.
    RedrawScreen,
    /// Vim's `:`. Enter the command-line minibuffer. Promoted
    /// from `Action::EnterCommandLine` in slice 8.i.1.e.
    EnterCommandLine,
    /// Lattice's `-`. Open / step up in the oil-style directory
    /// view (DESIGN.md §5.9.4). Promoted from
    /// `Action::OilNavigateUp` in slice 8.i.1.e.
    OilNavigateUp,
    /// Vim's `gv`. Reselect the last Visual selection (same
    /// kind, anchor, head). Promoted from
    /// `Action::ReselectLastVisual` in slice 8.i.1.e.
    ReselectLastVisual,
    /// Vim's `p`. Paste the unnamed register's contents after
    /// the cursor. Promoted from `Action::PasteAfter` in slice
    /// 8.i.1.e.
    PasteAfter,
    /// Vim's `P`. Paste the unnamed register's contents before
    /// the cursor. Promoted from `Action::PasteBefore` in slice
    /// 8.i.1.e.
    PasteBefore,
    /// `gd` (Phase 4.2.c). `textDocument/definition` -- jump to
    /// the symbol's definition. Promoted from
    /// `Action::LspDefinitionRequest` in slice 8.i.1.f.
    LspDefinitionRequest,
    /// `gD`. `textDocument/declaration` -- declaration ≠
    /// definition for header / forward-declaration languages.
    /// Promoted from `Action::LspDeclarationRequest` in slice
    /// 8.i.1.f.
    LspDeclarationRequest,
    /// `gy`. `textDocument/typeDefinition` -- jump from a value
    /// to its type's declaration site. Promoted from
    /// `Action::LspTypeDefinitionRequest` in slice 8.i.1.f.
    LspTypeDefinitionRequest,
    /// `gI`. `textDocument/implementation` -- jump from a trait
    /// or interface to its implementations. Promoted from
    /// `Action::LspImplementationRequest` in slice 8.i.1.f.
    LspImplementationRequest,
    /// `gr`. `textDocument/references` -- list every reference
    /// to the symbol at the cursor. Promoted from
    /// `Action::LspReferencesRequest` in slice 8.i.1.f.
    LspReferencesRequest,
    /// Vim's `a`. Move cursor one byte right (clamped) and enter
    /// Insert. Promoted from `Action::EnterAppend` in slice
    /// 8.i.1.g.
    EnterAppend,
    /// Vim's `zf`. Create a fold from the most recent Visual
    /// selection. Promoted from `Action::CreateFoldFromVisual`
    /// in slice 8.i.1.g.
    CreateFoldFromVisual,
    /// Insert mode's `<BS>`. Delete the byte before the cursor.
    /// Promoted from `Action::DeleteCharBackward` in slice
    /// 8.i.1.g.
    DeleteCharBackward,
    /// Insert mode's `<C-Space>` and `<C-x><C-o>`. Trigger the
    /// completion popup (omni-completion alias). Promoted from
    /// `Action::CompletionTrigger` in slice 8.i.1.g. The
    /// completion-popup minor-mode layer's `<C-Space>` binding
    /// keeps its legacy `bind_action` registration until that
    /// helper picks up `CommandInvocation` (separate scope).
    CompletionTrigger,
    /// Insert mode's `<C-x><C-s>`. Direct snippet expansion at
    /// the cursor (matches the longest snippet prefix without
    /// surfacing the completion popup). Promoted from
    /// `Action::SnippetExpand` in slice 8.i.1.g.
    SnippetExpand,
    /// Visual mode's `<Esc>` / `v` / `V`. Exit Visual to Normal,
    /// collapsing the selection. Promoted from `Action::ExitVisual`
    /// in slice 8.i.1.h.
    ExitVisual,
    /// Replace mode's `<BS>`. Undo the last overwritten char.
    /// Promoted from `Action::ReplaceUndoLast` in slice 8.i.1.h.
    ReplaceUndoLast,
    /// Vim's `i` / `R` / `<Esc>` (from Insert / Replace) -- enter
    /// the named modal state. Promoted from
    /// `Action::EnterMode(_)` in slice 8.i.2.a. Each chord in the
    /// keymap binds a *distinct* `CommandId` whose `ActionSpec`
    /// returns the right `AppEffect::EnterMode(state)` constant
    /// -- the `ModalState` rides in the AppEffect rather than in
    /// `CommandInvocation::args` so the App's `apply_app_effect`
    /// matches a single `EnterMode(state)` arm instead of N
    /// param-flat variants.
    EnterMode(ModalState),
    /// Vim's `v` / `V` / `<C-v>` -- enter Visual with the named
    /// kind anchored at the current cursor. Promoted from
    /// `Action::EnterVisual(_)` in slice 8.i.2.a. Same encoding
    /// as [`Self::EnterMode`]: distinct `CommandId` per kind,
    /// payload rides in the AppEffect.
    EnterVisual(VisualKind),
    /// Vim's `/` / `?` -- enter the Search minibuffer in the
    /// named direction. Promoted from `Action::EnterSearch(_)`
    /// in slice 8.i.2.b. Same encoding as [`Self::EnterMode`]:
    /// distinct `CommandId` per direction.
    EnterSearch(SearchDirection),
    /// Vim's `*` / `#` -- search for the word under the cursor
    /// in the named direction. Promoted from
    /// `Action::SearchWordUnderCursor(_)` in slice 8.i.2.b.
    SearchWordUnderCursor(SearchDirection),
    /// Vim's `H` / `M` / `L` -- jump cursor to the named
    /// position within the visible viewport. Promoted from
    /// `Action::JumpViewport(_)` in slice 8.i.2.c.
    JumpViewport(ViewportPos),
    /// Vim's `zz` / `zt` / `zb` -- scroll the viewport so the
    /// cursor's current line sits at the named position.
    /// Promoted from `Action::ScrollCursorTo(_)` in slice 8.i.2.c.
    ScrollCursorTo(ScrollPos),
    /// Vim's `J` (with-space) / `gJ` (no-space). Joins the
    /// current line with the next, replacing the joining
    /// newline with a single space (`with_space: true`) or
    /// nothing (`false`). Promoted from `Action::JoinLines` in
    /// slice 8.i.2.d. Bool payload rides in the AppEffect:
    /// distinct `CommandId` per binding (`J` -> with-space=true,
    /// `gJ` -> with-space=false).
    JoinLines { with_space: bool },
    /// Vim's `;` (forward) / `,` (reverse). Repeat the most
    /// recent `f` / `F` / `t` / `T` find on the current line
    /// in the originally-typed direction (`reverse: false`) or
    /// the opposite direction (`reverse: true`). Promoted from
    /// `Action::FindRepeat` in slice 8.i.2.d.
    FindRepeat { reverse: bool },
    /// Insert / Replace mode's `<CR>`. Inserts a literal newline
    /// at the cursor. Promoted from
    /// `Action::Insert("\n".into())` in slice 8.i.2.e. Distinct
    /// flat variant rather than a `String`-payload `Insert(_)`
    /// because the keymap-bound forms always pin a fixed
    /// literal; the wildcard "type any printable char" path
    /// stays on `Action::Insert(c.to_string())` since it isn't
    /// keymap-bound.
    InsertNewline,
    /// Insert mode's `<Tab>`. Inserts a literal tab at the
    /// cursor. Promoted from `Action::Insert("\t".into())` in
    /// slice 8.i.2.e.
    InsertTab,
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
