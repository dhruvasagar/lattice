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
