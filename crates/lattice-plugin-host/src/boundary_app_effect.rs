//! `WitBoundary` mirror for `AppEffect` (plugin-host.md §4.4, PH7.3b2).
//!
//! `AppEffect` is the App-side typed effect carried by `Effect::AppAction` —
//! chord-bound work with no grammar concept attached (`<Esc>` exits Visual,
//! `<C-w>v` splits a pane, `o` opens a line below). Mirroring it unblocks the
//! `effect::app-action` arm (which crossed as a typed error at PH7.3b1b).
//!
//! Every arm is pure, flat, non-recursive data and reuses the shared payload
//! mirrors (`ModalState`/`VisualKind`/`SearchDirection`/`Register`, PH7.3b1a)
//! plus four `app-effect`-only helper enums (`ViewportPos`/`ScrollPos`/
//! `PaneDirection`/`HScroll`). One arm is absent by design:
//! `AppEffect::NarrowTrigger { range: Option<Range> }` carries the recursive
//! ex-command `Range` (`RangeBound::Offset { base: Box<RangeBound> }`) + a
//! plugin `RangeId`, which WIT cannot express — it crosses as a typed
//! `WitBoundary` error until a range mirror lands (the `Effect::Global`
//! precedent). `NarrowLines` (a pre-resolved line span) crosses fine.

use crate::WitBoundary;
use crate::lattice::plugin_host::types::{
    AppEffect as WitAppEffect, Hscroll as WitHscroll, InsertLineEdit as WitInsertLineEdit,
    NarrowLinesPayload as WitNarrowLinesPayload,
    OpenProviderViewPayload as WitOpenProviderViewPayload, PaneDirection as WitPaneDirection,
    ScrollPos as WitScrollPos, ViewportPos as WitViewportPos,
};
use lattice_grammar::app_effect::{
    AppEffect as NativeAppEffect, HScroll as NativeHScroll, InsertLineEdit as NativeInsertLineEdit,
    PaneDirection as NativePaneDirection, ScrollPos as NativeScrollPos,
    ViewportPos as NativeViewportPos,
};
use lattice_grammar::modal::{
    ModalState as NativeModalState, SearchDirection as NativeSearchDirection,
    VisualKind as NativeVisualKind,
};
use lattice_grammar::register::Register as NativeRegister;
use lattice_grammar::registry::OperatorId;
use lattice_protocol::ids::CommandId;

impl WitBoundary for NativeViewportPos {
    type Wit = WitViewportPos;
    fn to_wit(&self) -> Result<WitViewportPos, String> {
        Ok(match self {
            NativeViewportPos::Top => WitViewportPos::Top,
            NativeViewportPos::Middle => WitViewportPos::Middle,
            NativeViewportPos::Bottom => WitViewportPos::Bottom,
        })
    }
    fn from_wit(w: WitViewportPos) -> Result<Self, String> {
        Ok(match w {
            WitViewportPos::Top => NativeViewportPos::Top,
            WitViewportPos::Middle => NativeViewportPos::Middle,
            WitViewportPos::Bottom => NativeViewportPos::Bottom,
        })
    }
}

impl WitBoundary for NativeScrollPos {
    type Wit = WitScrollPos;
    fn to_wit(&self) -> Result<WitScrollPos, String> {
        Ok(match self {
            NativeScrollPos::Top => WitScrollPos::Top,
            NativeScrollPos::Center => WitScrollPos::Center,
            NativeScrollPos::Bottom => WitScrollPos::Bottom,
        })
    }
    fn from_wit(w: WitScrollPos) -> Result<Self, String> {
        Ok(match w {
            WitScrollPos::Top => NativeScrollPos::Top,
            WitScrollPos::Center => NativeScrollPos::Center,
            WitScrollPos::Bottom => NativeScrollPos::Bottom,
        })
    }
}

impl WitBoundary for NativePaneDirection {
    type Wit = WitPaneDirection;
    fn to_wit(&self) -> Result<WitPaneDirection, String> {
        Ok(match self {
            NativePaneDirection::Left => WitPaneDirection::Left,
            NativePaneDirection::Down => WitPaneDirection::Down,
            NativePaneDirection::Up => WitPaneDirection::Up,
            NativePaneDirection::Right => WitPaneDirection::Right,
        })
    }
    fn from_wit(w: WitPaneDirection) -> Result<Self, String> {
        Ok(match w {
            WitPaneDirection::Left => NativePaneDirection::Left,
            WitPaneDirection::Down => NativePaneDirection::Down,
            WitPaneDirection::Up => NativePaneDirection::Up,
            WitPaneDirection::Right => NativePaneDirection::Right,
        })
    }
}

impl WitBoundary for NativeInsertLineEdit {
    type Wit = WitInsertLineEdit;
    fn to_wit(&self) -> Result<WitInsertLineEdit, String> {
        Ok(match self {
            NativeInsertLineEdit::CursorLineStart => WitInsertLineEdit::CursorLineStart,
            NativeInsertLineEdit::CursorLineEnd => WitInsertLineEdit::CursorLineEnd,
            NativeInsertLineEdit::CursorCharLeft => WitInsertLineEdit::CursorCharLeft,
            NativeInsertLineEdit::CursorCharRight => WitInsertLineEdit::CursorCharRight,
            NativeInsertLineEdit::DeleteWordBackward => WitInsertLineEdit::DeleteWordBackward,
            NativeInsertLineEdit::DeleteToLineStart => WitInsertLineEdit::DeleteToLineStart,
            NativeInsertLineEdit::KillToLineEnd => WitInsertLineEdit::KillToLineEnd,
            NativeInsertLineEdit::IndentLine => WitInsertLineEdit::IndentLine,
            NativeInsertLineEdit::DedentLine => WitInsertLineEdit::DedentLine,
        })
    }
    fn from_wit(w: WitInsertLineEdit) -> Result<Self, String> {
        Ok(match w {
            WitInsertLineEdit::CursorLineStart => NativeInsertLineEdit::CursorLineStart,
            WitInsertLineEdit::CursorLineEnd => NativeInsertLineEdit::CursorLineEnd,
            WitInsertLineEdit::CursorCharLeft => NativeInsertLineEdit::CursorCharLeft,
            WitInsertLineEdit::CursorCharRight => NativeInsertLineEdit::CursorCharRight,
            WitInsertLineEdit::DeleteWordBackward => NativeInsertLineEdit::DeleteWordBackward,
            WitInsertLineEdit::DeleteToLineStart => NativeInsertLineEdit::DeleteToLineStart,
            WitInsertLineEdit::KillToLineEnd => NativeInsertLineEdit::KillToLineEnd,
            WitInsertLineEdit::IndentLine => NativeInsertLineEdit::IndentLine,
            WitInsertLineEdit::DedentLine => NativeInsertLineEdit::DedentLine,
        })
    }
}

impl WitBoundary for NativeHScroll {
    type Wit = WitHscroll;
    fn to_wit(&self) -> Result<WitHscroll, String> {
        Ok(match self {
            NativeHScroll::Columns { right } => WitHscroll::Columns(*right),
            NativeHScroll::HalfScreen { right } => WitHscroll::HalfScreen(*right),
            NativeHScroll::CursorToEdge { end } => WitHscroll::CursorToEdge(*end),
        })
    }
    fn from_wit(w: WitHscroll) -> Result<Self, String> {
        Ok(match w {
            WitHscroll::Columns(right) => NativeHScroll::Columns { right },
            WitHscroll::HalfScreen(right) => NativeHScroll::HalfScreen { right },
            WitHscroll::CursorToEdge(end) => NativeHScroll::CursorToEdge { end },
        })
    }
}

impl WitBoundary for NativeAppEffect {
    type Wit = WitAppEffect;

    fn to_wit(&self) -> Result<WitAppEffect, String> {
        Ok(match self {
            NativeAppEffect::Quit => WitAppEffect::Quit,
            // CG.1: foreground cancellation is the *user's* escape hatch —
            // it flips the token of whatever the user triggered and snaps
            // the editor back to Normal. A plugin emitting it would be
            // yanking the user out of their mode from a background task,
            // which is the opposite of the contract. Deliberately no WIT
            // surface; typed error, never lossy (the `*Line*` precedent
            // below). CG.4 threads the token INTO plugin calls so the host
            // can cancel a plugin — the direction that does make sense.
            NativeAppEffect::Cancel => {
                return Err(
                    "AppEffect::Cancel is the user's foreground-cancel hatch (`<C-c>`, \
                     cancellation.md CG.1); no plugin (WIT) surface — a plugin must not \
                     reset the user's mode"
                        .to_string(),
                );
            }
            NativeAppEffect::MatchBracket => WitAppEffect::MatchBracket,
            NativeAppEffect::ToggleCaseAtCursor => WitAppEffect::ToggleCaseAtCursor,
            NativeAppEffect::OpenLineBelow => WitAppEffect::OpenLineBelow,
            NativeAppEffect::OpenLineAbove => WitAppEffect::OpenLineAbove,
            NativeAppEffect::SearchNext => WitAppEffect::SearchNext,
            NativeAppEffect::SearchPrevious => WitAppEffect::SearchPrevious,
            NativeAppEffect::JumpHistoryBack => WitAppEffect::JumpHistoryBack,
            NativeAppEffect::JumpHistoryForward => WitAppEffect::JumpHistoryForward,
            NativeAppEffect::PaneHistoryBack => WitAppEffect::PaneHistoryBack,
            NativeAppEffect::PaneHistoryForward => WitAppEffect::PaneHistoryForward,
            NativeAppEffect::WalkMarkHistoryBack => WitAppEffect::WalkMarkHistoryBack,
            NativeAppEffect::WalkMarkHistoryForward => WitAppEffect::WalkMarkHistoryForward,
            NativeAppEffect::TagStackPop => WitAppEffect::TagStackPop,
            NativeAppEffect::OpenFoldAtCursor => WitAppEffect::OpenFoldAtCursor,
            NativeAppEffect::CloseFoldAtCursor => WitAppEffect::CloseFoldAtCursor,
            NativeAppEffect::ToggleFoldAtCursor => WitAppEffect::ToggleFoldAtCursor,
            NativeAppEffect::OpenAllFolds => WitAppEffect::OpenAllFolds,
            NativeAppEffect::CloseAllFolds => WitAppEffect::CloseAllFolds,
            NativeAppEffect::CycleFoldAtCursor => WitAppEffect::CycleFoldAtCursor,
            NativeAppEffect::CycleFoldsGlobal => WitAppEffect::CycleFoldsGlobal,
            NativeAppEffect::GotoParentFold => WitAppEffect::GotoParentFold,
            NativeAppEffect::DeleteFoldAtCursor => WitAppEffect::DeleteFoldAtCursor,
            NativeAppEffect::GotoNextFold => WitAppEffect::GotoNextFold,
            NativeAppEffect::GotoPrevFold => WitAppEffect::GotoPrevFold,
            NativeAppEffect::ToggleFoldEnable => WitAppEffect::ToggleFoldEnable,
            NativeAppEffect::Undo => WitAppEffect::Undo,
            NativeAppEffect::Redo => WitAppEffect::Redo,
            NativeAppEffect::RepeatLastChange => WitAppEffect::RepeatLastChange,
            NativeAppEffect::PageDown => WitAppEffect::PageDown,
            NativeAppEffect::PageUp => WitAppEffect::PageUp,
            NativeAppEffect::ScrollLineUp => WitAppEffect::ScrollLineUp,
            NativeAppEffect::ScrollLineDown => WitAppEffect::ScrollLineDown,
            NativeAppEffect::RedrawScreen => WitAppEffect::RedrawScreen,
            NativeAppEffect::OpenCommandPicker => WitAppEffect::OpenCommandPicker,
            // MB.3: `q:` opens the command-line *history* picker — a
            // host-internal command-line affordance (its accept seeds the
            // `:` line). No plugin surface; plugins reach the same picker
            // via the `:history` ex-command. Typed error, never lossy
            // (mirrors the `CommandLine*` host-internal group below).
            NativeAppEffect::OpenHistoryPicker => {
                return Err(
                    "AppEffect::OpenHistoryPicker is a host-internal command-line-history \
                     affordance (rich-minibuffer MB.3); no plugin (WIT) surface — use the \
                     `:history` ex-command"
                        .to_string(),
                );
            }
            NativeAppEffect::OpenSearchHistoryPicker => {
                return Err(
                    "AppEffect::OpenSearchHistoryPicker is a host-internal search-line-history \
                     affordance (rich-minibuffer MB.5); no plugin (WIT) surface — use the \
                     `:history search` ex-command"
                        .to_string(),
                );
            }
            NativeAppEffect::EnterCommandLine => WitAppEffect::EnterCommandLine,
            // MB.1: the `command-line-mode` chord effects (submit / cancel
            // / history / completion / describe) are host-internal — the
            // `:` line's readline machinery lives entirely host-side and
            // plugins never bind these. No WIT variant; typed error, never
            // lossy (NarrowTrigger / CompileRun precedent).
            NativeAppEffect::CommandLineSubmit
            | NativeAppEffect::CommandLineCancel
            | NativeAppEffect::CommandLineHistoryPrev
            | NativeAppEffect::CommandLineHistoryNext
            | NativeAppEffect::CommandLineComplete
            | NativeAppEffect::CommandLineCompletePrev
            | NativeAppEffect::CommandLineDescribeUnderCursor
            | NativeAppEffect::CommandLineToggleExpand
            | NativeAppEffect::SearchLineSubmit
            | NativeAppEffect::SearchLineCancel
            | NativeAppEffect::SearchLineBackspace
            | NativeAppEffect::SearchLineHistoryPrev
            | NativeAppEffect::SearchLineHistoryNext
            | NativeAppEffect::SearchLineToggleExpand
            | NativeAppEffect::PromptLineSubmit
            | NativeAppEffect::PromptLineCancel => {
                return Err(
                    "AppEffect::*Line* are host-internal minibuffer-prompt effects \
                     (rich-minibuffer MB.1–MB.5); no plugin (WIT) surface"
                        .to_string(),
                );
            }
            NativeAppEffect::OilNavigateUp => WitAppEffect::OilNavigateUp,
            NativeAppEffect::ReselectLastVisual => WitAppEffect::ReselectLastVisual,
            NativeAppEffect::SwapVisualEnds => WitAppEffect::SwapVisualEnds,
            NativeAppEffect::PasteAfter => WitAppEffect::PasteAfter,
            NativeAppEffect::PasteBefore => WitAppEffect::PasteBefore,
            NativeAppEffect::EnterAppend => WitAppEffect::EnterAppend,
            NativeAppEffect::EnterInsertFirstNonBlank => WitAppEffect::EnterInsertFirstNonBlank,
            NativeAppEffect::EnterAppendEndOfLine => WitAppEffect::EnterAppendEndOfLine,
            NativeAppEffect::DisplayLineDown => WitAppEffect::DisplayLineDown,
            NativeAppEffect::DisplayLineUp => WitAppEffect::DisplayLineUp,
            NativeAppEffect::DisplayLineStart => WitAppEffect::DisplayLineStart,
            NativeAppEffect::DisplayLineEnd => WitAppEffect::DisplayLineEnd,
            NativeAppEffect::CreateFoldFromVisual => WitAppEffect::CreateFoldFromVisual,
            NativeAppEffect::DeleteCharBackward => WitAppEffect::DeleteCharBackward,
            NativeAppEffect::CompletionTrigger => WitAppEffect::CompletionTrigger,
            NativeAppEffect::ExitVisual => WitAppEffect::ExitVisual,
            NativeAppEffect::ReplaceUndoLast => WitAppEffect::ReplaceUndoLast,
            NativeAppEffect::EnterMode(state) => WitAppEffect::EnterMode(state.to_wit()?),
            NativeAppEffect::EnterVisual(k) => WitAppEffect::EnterVisual(k.to_wit()?),
            NativeAppEffect::EnterSelect(k) => WitAppEffect::EnterSelect(k.to_wit()?),
            NativeAppEffect::EnterSearch(d) => WitAppEffect::EnterSearch(d.to_wit()?),
            NativeAppEffect::SearchWordUnderCursor(d) => {
                WitAppEffect::SearchWordUnderCursor(d.to_wit()?)
            }
            NativeAppEffect::JumpViewport(p) => WitAppEffect::JumpViewport(p.to_wit()?),
            NativeAppEffect::ScrollCursorTo(p) => WitAppEffect::ScrollCursorTo(p.to_wit()?),
            NativeAppEffect::HorizontalScroll(h) => WitAppEffect::HorizontalScroll(h.to_wit()?),
            NativeAppEffect::JoinLines { with_space } => WitAppEffect::JoinLines(*with_space),
            NativeAppEffect::FindRepeat { reverse } => WitAppEffect::FindRepeat(*reverse),
            NativeAppEffect::InsertNewline => WitAppEffect::InsertNewline,
            NativeAppEffect::InsertTab => WitAppEffect::InsertTab,
            NativeAppEffect::OverwriteChar(c) => WitAppEffect::OverwriteChar(*c),
            NativeAppEffect::SetMark(c) => WitAppEffect::SetMark(*c),
            NativeAppEffect::JumpToMarkLine(c) => WitAppEffect::JumpToMarkLine(*c),
            NativeAppEffect::JumpToMarkExact(c) => WitAppEffect::JumpToMarkExact(*c),
            NativeAppEffect::SelectRegister(r) => WitAppEffect::SelectRegister(r.to_wit()?),
            NativeAppEffect::StartMacroRecord(c) => WitAppEffect::StartMacroRecord(*c),
            NativeAppEffect::PlayMacro(c) => WitAppEffect::PlayMacro(*c),
            NativeAppEffect::PlayLastMacro => WitAppEffect::PlayLastMacro,
            NativeAppEffect::AbsorbOperatorPrefix(op) => {
                WitAppEffect::AbsorbOperatorPrefix(op.0.raw())
            }
            NativeAppEffect::SplitPaneHorizontal => WitAppEffect::SplitPaneHorizontal,
            NativeAppEffect::SplitPaneVertical => WitAppEffect::SplitPaneVertical,
            NativeAppEffect::ClosePane => WitAppEffect::ClosePane,
            NativeAppEffect::OnlyPane => WitAppEffect::OnlyPane,
            NativeAppEffect::NavigatePane(d) => WitAppEffect::NavigatePane(d.to_wit()?),
            NativeAppEffect::NextPane => WitAppEffect::NextPane,
            NativeAppEffect::PrevPane => WitAppEffect::PrevPane,
            NativeAppEffect::NextTab => WitAppEffect::NextTab,
            NativeAppEffect::PrevTab => WitAppEffect::PrevTab,
            NativeAppEffect::GoToTab(n) => WitAppEffect::GoToTab(*n),
            NativeAppEffect::NewTab => WitAppEffect::NewTab,
            NativeAppEffect::NewTabAt(path) => WitAppEffect::NewTabAt(path.clone()),
            NativeAppEffect::TerminalSpawn(cmd) => WitAppEffect::TerminalSpawn(cmd.clone()),
            NativeAppEffect::TerminalSpawnInNewTab(cmd) => {
                WitAppEffect::TerminalSpawnInNewTab(cmd.clone())
            }
            NativeAppEffect::MovePaneToNewTab => WitAppEffect::MovePaneToNewTab,
            NativeAppEffect::CloseTab => WitAppEffect::CloseTab,
            NativeAppEffect::OnlyTab => WitAppEffect::OnlyTab,
            NativeAppEffect::MoveTab(n) => WitAppEffect::MoveTab(*n),
            NativeAppEffect::PickerAcceptInSplit => WitAppEffect::PickerAcceptInSplit,
            NativeAppEffect::PickerAcceptInVSplit => WitAppEffect::PickerAcceptInVsplit,
            NativeAppEffect::PickerAcceptInTab => WitAppEffect::PickerAcceptInTab,
            NativeAppEffect::EqualizePanes => WitAppEffect::EqualizePanes,
            NativeAppEffect::GrowPaneHeight => WitAppEffect::GrowPaneHeight,
            NativeAppEffect::ShrinkPaneHeight => WitAppEffect::ShrinkPaneHeight,
            NativeAppEffect::GrowPaneWidth => WitAppEffect::GrowPaneWidth,
            NativeAppEffect::ShrinkPaneWidth => WitAppEffect::ShrinkPaneWidth,
            NativeAppEffect::CompletionNext => WitAppEffect::CompletionNext,
            NativeAppEffect::CompletionPrev => WitAppEffect::CompletionPrev,
            NativeAppEffect::CompletionAccept => WitAppEffect::CompletionAccept,
            NativeAppEffect::CompletionCancel => WitAppEffect::CompletionCancel,
            NativeAppEffect::CompletionCancelAndExitInsert => {
                WitAppEffect::CompletionCancelAndExitInsert
            }
            NativeAppEffect::CompletionToggleDocs => WitAppEffect::CompletionToggleDocs,
            NativeAppEffect::CompletionDocsScrollDown => WitAppEffect::CompletionDocsScrollDown,
            NativeAppEffect::CompletionDocsScrollUp => WitAppEffect::CompletionDocsScrollUp,
            NativeAppEffect::CompletionAcceptThenInsert(c) => {
                WitAppEffect::CompletionAcceptThenInsert(*c)
            }
            // YR.5: both put text into, or open a picker over, whichever
            // HOST surface is focused — a place a plugin cannot name.
            // Representable once the fill targets themselves cross.
            // YR.6 joins them: the argument picker names a `:`-line slot,
            // which is the same unnameable-host-surface problem. A plugin
            // that wants an argument picker declares one in its
            // `args_schema` (`ArgSpec.picker`, which DOES cross) rather
            // than emitting this effect.
            NativeAppEffect::InsertRegister(_)
            | NativeAppEffect::OpenYankPicker
            | NativeAppEffect::OpenArgPicker => {
                return Err(
                    "insert-register / open-yank-picker / open-arg-picker are host-internal, \
                     not representable over WIT"
                        .into(),
                );
            }
            NativeAppEffect::SnippetNextPlaceholder => WitAppEffect::SnippetNextPlaceholder,
            NativeAppEffect::SnippetPrevPlaceholder => WitAppEffect::SnippetPrevPlaceholder,
            NativeAppEffect::CompletionFilterToSource(s) => {
                WitAppEffect::CompletionFilterToSource(s.clone())
            }
            NativeAppEffect::CompletionFilterClear => WitAppEffect::CompletionFilterClear,
            NativeAppEffect::DiffGet => WitAppEffect::DiffGet,
            NativeAppEffect::DiffPut => WitAppEffect::DiffPut,
            NativeAppEffect::TutorAdvance => WitAppEffect::TutorAdvance,
            NativeAppEffect::TutorRetreat => WitAppEffect::TutorRetreat,
            NativeAppEffect::MultibufferExpand { delta } => WitAppEffect::MultibufferExpand(*delta),
            NativeAppEffect::NarrowWiden => WitAppEffect::NarrowWiden,
            NativeAppEffect::NarrowLines {
                start_line,
                end_line,
            } => WitAppEffect::NarrowLines(WitNarrowLinesPayload {
                start_line: *start_line,
                end_line: *end_line,
            }),
            NativeAppEffect::SearchTrigger { query } => WitAppEffect::SearchTrigger(query.clone()),
            NativeAppEffect::SearchRefresh => WitAppEffect::SearchRefresh,
            // Carries the recursive ex-command `Range` (§4.4); crosses with the
            // range mirror. Typed error until then, never lossy.
            NativeAppEffect::NarrowTrigger { .. } => {
                return Err(
                    "AppEffect::NarrowTrigger carries a recursive ex-command Range; it crosses \
                     with the range mirror (fragment §4.4)"
                        .to_string(),
                );
            }
            NativeAppEffect::InsertLineEdit(edit) => WitAppEffect::InsertLineEdit(edit.to_wit()?),
            // CM.1: `:compile`/`:recompile`/`:make` are a native
            // built-in; the compilation WIT surface is deferred with
            // the plugin host (compilation-mode.md §8 #2). No WIT
            // variant yet — typed error, never lossy (NarrowTrigger
            // precedent).
            NativeAppEffect::CompileRun { .. } => {
                return Err(
                    "AppEffect::CompileRun is a native built-in; its plugin (WIT) surface is \
                     deferred with the plugin host (compilation-mode.md §8)"
                        .to_string(),
                );
            }
            // CM.3b: the `<CR>`-jump from a `*compilation*` location line
            // jumps + syncs core error list index; like CompileRun /
            // ErrorNav a native built-in, WIT surface deferred with
            // the plugin host. Typed error, never lossy.
            NativeAppEffect::CompileJumpToLocation { .. } => {
                return Err(
                    "AppEffect::CompileJumpToLocation jumps to a source location + syncs core \
                     error state; its plugin (WIT) surface is deferred with the plugin host \
                     (compilation-mode.md §5)"
                        .to_string(),
                );
            }
            // CM.2: error navigation walks core/host state; like
            // CompileRun, its plugin (WIT) surface is deferred with
            // the plugin host. Typed error, never lossy.
            NativeAppEffect::ErrorNav { .. } => {
                return Err(
                    "AppEffect::ErrorNav walks core error state; its plugin (WIT) surface \
                     is deferred with the plugin host (compilation-mode.md §3)"
                        .to_string(),
                );
            }
            // CM.3a: parsed error entries feed core error state
            // from the native compilation parser; like CompileRun /
            // ErrorNav a native built-in, WIT surface deferred with
            // the plugin host. Typed error, never lossy.
            NativeAppEffect::SetErrorList { .. } => {
                return Err(
                    "AppEffect::SetErrorList feeds core error state from the native compilation \
                     parser; its plugin (WIT) surface is deferred with the plugin host \
                     (compilation-mode.md §5)"
                        .to_string(),
                );
            }
            // CM.3c: the per-buffer severity gutter index feeds the
            // `*compilation*` buffer's in-buffer severity marks from the
            // native compilation parser; like SetErrorList a native built-in,
            // WIT surface deferred with the plugin host. Typed error, never
            // lossy.
            NativeAppEffect::CompilationGutterSet { .. } => {
                return Err(
                    "AppEffect::CompilationGutterSet feeds the *compilation* buffer's severity \
                     gutter marks from the native compilation parser; its plugin (WIT) surface is \
                     deferred with the plugin host (compilation-mode.md §5)"
                        .to_string(),
                );
            }
            // CM.3c: location-line index for theme-based highlighting of
            // navigable file-location lines in the *compilation* buffer.
            // Same reasoning as CompilationGutterSet — native built-in,
            // WIT surface deferred.
            NativeAppEffect::CompilationLocationLines { .. } => {
                return Err(
                    "AppEffect::CompilationLocationLines marks navigable file-location lines in the \
                     *compilation* buffer for theme-based highlighting; its plugin (WIT) surface is \
                     deferred with the plugin host"
                        .to_string(),
                );
            }
            // CM.3d: resolved theme colours for compilation
            // location lines — same as above, native built-in.
            NativeAppEffect::CompilationThemeColors { .. } => {
                return Err(
                    "AppEffect::CompilationThemeColors feeds theme-resolved compilation location \
                     colours to the renderer; its plugin (WIT) surface is deferred with the \
                     plugin host"
                        .to_string(),
                );
            }
            NativeAppEffect::CompilationKill => {
                return Err(
                    "AppEffect::CompilationKill kills the running compilation child process; \
                     its plugin (WIT) surface is deferred with the plugin host"
                        .to_string(),
                );
            }
            // CM.4 / RV.3: `:copen` / `:cclose` / `gr` open, close and
            // rebuild the `*problems*` multibuffer over core error
            // state; like CompileRun / ErrorNav a native built-in, WIT
            // surface deferred with the plugin host. Typed error,
            // never lossy.
            NativeAppEffect::ProblemsOpen
            | NativeAppEffect::ProblemsClose
            | NativeAppEffect::ProblemsRefresh => {
                return Err(
                    "AppEffect::Problems{Open,Close,Refresh} open, close and rebuild the \
                     *problems* view over core error state; their plugin (WIT) surface is \
                     deferred with the plugin host (compilation-mode.md §4)"
                        .to_string(),
                );
            }
            // AG.1: the generic provider-view seam, no longer deferred.
            //
            // PV.1 withheld it as a capability question — which providers may a
            // plugin trigger? — to be answered with the host's capability model
            // rather than ahead of it. The precedent had already answered it:
            // `Effect::OpenPicker` and `Effect::OpenTransient` both cross
            // UNGATED and let a guest open any registered source by name. This
            // is the same authority in the same shape, so withholding it never
            // withheld the capability; it only stopped the one seam that needed
            // it from naming its own trigger.
            //
            // That cost was concrete: the agenda's ex-command lived in the host
            // because the plugin had no way to open the view, so a feature every
            // user calls `org-agenda` shipped as `:agenda` and the plugin could
            // not fix it from its own side.
            //
            // The argument crosses as `option<string>` — the host-interpreted
            // parameter, of which every provider view takes at most one (a
            // root, a query). Mirroring the whole recursive `Args` enum would
            // add a second args encoding to the boundary for cases no provider
            // has, so anything it cannot express is a typed error rather than a
            // silent flattening — the `NarrowTrigger` precedent two arms up.
            //
            // OA.11a: `Args::List` is now expressible, as `argument` plus
            // `scan-args`. That is not the recursive mirror this comment used
            // to refuse; it is the flat positional case, and it exists because
            // a scan source needs a channel the host does NOT read. Position 0
            // is the host's argument, the rest are the guest's. An empty
            // position 0 means "no root override", which is how a caller sends
            // scan args without one.
            NativeAppEffect::OpenProviderView { provider, args } => {
                let (argument, scan_args) = match args {
                    lattice_grammar::args::Args::None => (None, Vec::new()),
                    lattice_grammar::args::Args::String(s) => (Some(s.clone()), Vec::new()),
                    lattice_grammar::args::Args::List(values) => {
                        let mut strings = Vec::with_capacity(values.len());
                        for v in values {
                            match v {
                                lattice_grammar::args::ArgValue::String(s) => {
                                    strings.push(s.clone())
                                }
                                // Only the string form crosses. A provider
                                // view's arguments are free text on both sides
                                // of the boundary, and quietly stringifying an
                                // `Int` or an `Invocation` here would invent a
                                // spelling the guest never agreed to.
                                other => {
                                    return Err(format!(
                                        "AppEffect::OpenProviderView carries a non-string list \
                                         argument the provider-view boundary does not mirror \
                                         ({other:?}); provider-view arguments are free text"
                                    ));
                                }
                            }
                        }
                        let mut it = strings.into_iter();
                        let first = it.next().unwrap_or_default();
                        // An empty first element is "no root override" rather
                        // than a root of "", which is not a path anyone means.
                        let argument = (!first.is_empty()).then_some(first);
                        (argument, it.collect())
                    }
                    other => {
                        return Err(format!(
                            "AppEffect::OpenProviderView carries args the provider-view boundary \
                             does not mirror ({other:?}); a provider view takes at most one \
                             free-text argument"
                        ));
                    }
                };
                WitAppEffect::OpenProviderView(WitOpenProviderViewPayload {
                    provider: provider.clone(),
                    argument,
                    scan_args,
                })
            }
        })
    }

    fn from_wit(w: WitAppEffect) -> Result<Self, String> {
        Ok(match w {
            WitAppEffect::Quit => NativeAppEffect::Quit,
            WitAppEffect::MatchBracket => NativeAppEffect::MatchBracket,
            WitAppEffect::ToggleCaseAtCursor => NativeAppEffect::ToggleCaseAtCursor,
            WitAppEffect::OpenLineBelow => NativeAppEffect::OpenLineBelow,
            WitAppEffect::OpenLineAbove => NativeAppEffect::OpenLineAbove,
            WitAppEffect::SearchNext => NativeAppEffect::SearchNext,
            WitAppEffect::SearchPrevious => NativeAppEffect::SearchPrevious,
            WitAppEffect::JumpHistoryBack => NativeAppEffect::JumpHistoryBack,
            WitAppEffect::JumpHistoryForward => NativeAppEffect::JumpHistoryForward,
            WitAppEffect::PaneHistoryBack => NativeAppEffect::PaneHistoryBack,
            WitAppEffect::PaneHistoryForward => NativeAppEffect::PaneHistoryForward,
            WitAppEffect::WalkMarkHistoryBack => NativeAppEffect::WalkMarkHistoryBack,
            WitAppEffect::WalkMarkHistoryForward => NativeAppEffect::WalkMarkHistoryForward,
            WitAppEffect::TagStackPop => NativeAppEffect::TagStackPop,
            WitAppEffect::OpenFoldAtCursor => NativeAppEffect::OpenFoldAtCursor,
            WitAppEffect::CloseFoldAtCursor => NativeAppEffect::CloseFoldAtCursor,
            WitAppEffect::ToggleFoldAtCursor => NativeAppEffect::ToggleFoldAtCursor,
            WitAppEffect::OpenAllFolds => NativeAppEffect::OpenAllFolds,
            WitAppEffect::CloseAllFolds => NativeAppEffect::CloseAllFolds,
            WitAppEffect::CycleFoldAtCursor => NativeAppEffect::CycleFoldAtCursor,
            WitAppEffect::CycleFoldsGlobal => NativeAppEffect::CycleFoldsGlobal,
            WitAppEffect::GotoParentFold => NativeAppEffect::GotoParentFold,
            WitAppEffect::DeleteFoldAtCursor => NativeAppEffect::DeleteFoldAtCursor,
            WitAppEffect::GotoNextFold => NativeAppEffect::GotoNextFold,
            WitAppEffect::GotoPrevFold => NativeAppEffect::GotoPrevFold,
            WitAppEffect::ToggleFoldEnable => NativeAppEffect::ToggleFoldEnable,
            WitAppEffect::Undo => NativeAppEffect::Undo,
            WitAppEffect::Redo => NativeAppEffect::Redo,
            WitAppEffect::RepeatLastChange => NativeAppEffect::RepeatLastChange,
            WitAppEffect::PageDown => NativeAppEffect::PageDown,
            WitAppEffect::PageUp => NativeAppEffect::PageUp,
            WitAppEffect::ScrollLineUp => NativeAppEffect::ScrollLineUp,
            WitAppEffect::ScrollLineDown => NativeAppEffect::ScrollLineDown,
            WitAppEffect::RedrawScreen => NativeAppEffect::RedrawScreen,
            WitAppEffect::OpenCommandPicker => NativeAppEffect::OpenCommandPicker,
            WitAppEffect::EnterCommandLine => NativeAppEffect::EnterCommandLine,
            WitAppEffect::OilNavigateUp => NativeAppEffect::OilNavigateUp,
            WitAppEffect::ReselectLastVisual => NativeAppEffect::ReselectLastVisual,
            WitAppEffect::SwapVisualEnds => NativeAppEffect::SwapVisualEnds,
            WitAppEffect::PasteAfter => NativeAppEffect::PasteAfter,
            WitAppEffect::PasteBefore => NativeAppEffect::PasteBefore,
            WitAppEffect::EnterAppend => NativeAppEffect::EnterAppend,
            WitAppEffect::EnterInsertFirstNonBlank => NativeAppEffect::EnterInsertFirstNonBlank,
            WitAppEffect::EnterAppendEndOfLine => NativeAppEffect::EnterAppendEndOfLine,
            WitAppEffect::DisplayLineDown => NativeAppEffect::DisplayLineDown,
            WitAppEffect::DisplayLineUp => NativeAppEffect::DisplayLineUp,
            WitAppEffect::DisplayLineStart => NativeAppEffect::DisplayLineStart,
            WitAppEffect::DisplayLineEnd => NativeAppEffect::DisplayLineEnd,
            WitAppEffect::CreateFoldFromVisual => NativeAppEffect::CreateFoldFromVisual,
            WitAppEffect::DeleteCharBackward => NativeAppEffect::DeleteCharBackward,
            WitAppEffect::CompletionTrigger => NativeAppEffect::CompletionTrigger,
            WitAppEffect::ExitVisual => NativeAppEffect::ExitVisual,
            WitAppEffect::ReplaceUndoLast => NativeAppEffect::ReplaceUndoLast,
            WitAppEffect::EnterMode(state) => {
                NativeAppEffect::EnterMode(NativeModalState::from_wit(state)?)
            }
            WitAppEffect::EnterVisual(k) => {
                NativeAppEffect::EnterVisual(NativeVisualKind::from_wit(k)?)
            }
            WitAppEffect::EnterSelect(k) => {
                NativeAppEffect::EnterSelect(NativeVisualKind::from_wit(k)?)
            }
            WitAppEffect::EnterSearch(d) => {
                NativeAppEffect::EnterSearch(NativeSearchDirection::from_wit(d)?)
            }
            WitAppEffect::SearchWordUnderCursor(d) => {
                NativeAppEffect::SearchWordUnderCursor(NativeSearchDirection::from_wit(d)?)
            }
            WitAppEffect::JumpViewport(p) => {
                NativeAppEffect::JumpViewport(NativeViewportPos::from_wit(p)?)
            }
            WitAppEffect::ScrollCursorTo(p) => {
                NativeAppEffect::ScrollCursorTo(NativeScrollPos::from_wit(p)?)
            }
            WitAppEffect::HorizontalScroll(h) => {
                NativeAppEffect::HorizontalScroll(NativeHScroll::from_wit(h)?)
            }
            WitAppEffect::JoinLines(with_space) => NativeAppEffect::JoinLines { with_space },
            WitAppEffect::FindRepeat(reverse) => NativeAppEffect::FindRepeat { reverse },
            WitAppEffect::InsertNewline => NativeAppEffect::InsertNewline,
            WitAppEffect::InsertTab => NativeAppEffect::InsertTab,
            WitAppEffect::OverwriteChar(c) => NativeAppEffect::OverwriteChar(c),
            WitAppEffect::SetMark(c) => NativeAppEffect::SetMark(c),
            WitAppEffect::JumpToMarkLine(c) => NativeAppEffect::JumpToMarkLine(c),
            WitAppEffect::JumpToMarkExact(c) => NativeAppEffect::JumpToMarkExact(c),
            WitAppEffect::SelectRegister(r) => {
                NativeAppEffect::SelectRegister(NativeRegister::from_wit(r)?)
            }
            WitAppEffect::StartMacroRecord(c) => NativeAppEffect::StartMacroRecord(c),
            WitAppEffect::PlayMacro(c) => NativeAppEffect::PlayMacro(c),
            WitAppEffect::PlayLastMacro => NativeAppEffect::PlayLastMacro,
            WitAppEffect::AbsorbOperatorPrefix(raw) => {
                NativeAppEffect::AbsorbOperatorPrefix(OperatorId(CommandId::new(raw)))
            }
            WitAppEffect::SplitPaneHorizontal => NativeAppEffect::SplitPaneHorizontal,
            WitAppEffect::SplitPaneVertical => NativeAppEffect::SplitPaneVertical,
            WitAppEffect::ClosePane => NativeAppEffect::ClosePane,
            WitAppEffect::OnlyPane => NativeAppEffect::OnlyPane,
            WitAppEffect::NavigatePane(d) => {
                NativeAppEffect::NavigatePane(NativePaneDirection::from_wit(d)?)
            }
            WitAppEffect::NextPane => NativeAppEffect::NextPane,
            WitAppEffect::PrevPane => NativeAppEffect::PrevPane,
            WitAppEffect::NextTab => NativeAppEffect::NextTab,
            WitAppEffect::PrevTab => NativeAppEffect::PrevTab,
            WitAppEffect::GoToTab(n) => NativeAppEffect::GoToTab(n),
            WitAppEffect::NewTab => NativeAppEffect::NewTab,
            WitAppEffect::NewTabAt(path) => NativeAppEffect::NewTabAt(path),
            WitAppEffect::TerminalSpawn(cmd) => NativeAppEffect::TerminalSpawn(cmd),
            WitAppEffect::TerminalSpawnInNewTab(cmd) => NativeAppEffect::TerminalSpawnInNewTab(cmd),
            WitAppEffect::MovePaneToNewTab => NativeAppEffect::MovePaneToNewTab,
            WitAppEffect::CloseTab => NativeAppEffect::CloseTab,
            WitAppEffect::OnlyTab => NativeAppEffect::OnlyTab,
            WitAppEffect::MoveTab(n) => NativeAppEffect::MoveTab(n),
            WitAppEffect::PickerAcceptInSplit => NativeAppEffect::PickerAcceptInSplit,
            WitAppEffect::PickerAcceptInVsplit => NativeAppEffect::PickerAcceptInVSplit,
            WitAppEffect::PickerAcceptInTab => NativeAppEffect::PickerAcceptInTab,
            WitAppEffect::EqualizePanes => NativeAppEffect::EqualizePanes,
            WitAppEffect::GrowPaneHeight => NativeAppEffect::GrowPaneHeight,
            WitAppEffect::ShrinkPaneHeight => NativeAppEffect::ShrinkPaneHeight,
            WitAppEffect::GrowPaneWidth => NativeAppEffect::GrowPaneWidth,
            WitAppEffect::ShrinkPaneWidth => NativeAppEffect::ShrinkPaneWidth,
            WitAppEffect::CompletionNext => NativeAppEffect::CompletionNext,
            WitAppEffect::CompletionPrev => NativeAppEffect::CompletionPrev,
            WitAppEffect::CompletionAccept => NativeAppEffect::CompletionAccept,
            WitAppEffect::CompletionCancel => NativeAppEffect::CompletionCancel,
            WitAppEffect::CompletionCancelAndExitInsert => {
                NativeAppEffect::CompletionCancelAndExitInsert
            }
            WitAppEffect::CompletionToggleDocs => NativeAppEffect::CompletionToggleDocs,
            WitAppEffect::CompletionDocsScrollDown => NativeAppEffect::CompletionDocsScrollDown,
            WitAppEffect::CompletionDocsScrollUp => NativeAppEffect::CompletionDocsScrollUp,
            WitAppEffect::CompletionAcceptThenInsert(c) => {
                NativeAppEffect::CompletionAcceptThenInsert(c)
            }
            WitAppEffect::SnippetNextPlaceholder => NativeAppEffect::SnippetNextPlaceholder,
            WitAppEffect::SnippetPrevPlaceholder => NativeAppEffect::SnippetPrevPlaceholder,
            WitAppEffect::CompletionFilterToSource(s) => {
                NativeAppEffect::CompletionFilterToSource(s)
            }
            WitAppEffect::CompletionFilterClear => NativeAppEffect::CompletionFilterClear,
            WitAppEffect::DiffGet => NativeAppEffect::DiffGet,
            WitAppEffect::DiffPut => NativeAppEffect::DiffPut,
            WitAppEffect::TutorAdvance => NativeAppEffect::TutorAdvance,
            WitAppEffect::TutorRetreat => NativeAppEffect::TutorRetreat,
            WitAppEffect::MultibufferExpand(delta) => NativeAppEffect::MultibufferExpand { delta },
            WitAppEffect::NarrowWiden => NativeAppEffect::NarrowWiden,
            WitAppEffect::NarrowLines(p) => NativeAppEffect::NarrowLines {
                start_line: p.start_line,
                end_line: p.end_line,
            },
            WitAppEffect::SearchTrigger(query) => NativeAppEffect::SearchTrigger { query },
            WitAppEffect::SearchRefresh => NativeAppEffect::SearchRefresh,
            // AG.1: the direction that matters — a plugin's ex-command opening
            // the view. `none` becomes `Args::None`, which is what a bare
            // `:org-agenda` with no root means.
            // OA.11a: with no scan args this is byte-for-byte the old mapping,
            // which is what keeps every existing trigger — `:org-agenda`,
            // `:org-agenda ~/notes` — unchanged. Scan args promote it to the
            // positional list form, with the host's argument still at 0 so the
            // two never contend for one slot.
            WitAppEffect::OpenProviderView(p) => NativeAppEffect::OpenProviderView {
                provider: p.provider,
                args: match (p.argument, p.scan_args.is_empty()) {
                    (Some(s), true) => lattice_grammar::args::Args::String(s),
                    (None, true) => lattice_grammar::args::Args::None,
                    (argument, false) => lattice_grammar::args::Args::List(
                        std::iter::once(argument.unwrap_or_default())
                            .chain(p.scan_args)
                            .map(lattice_grammar::args::ArgValue::String)
                            .collect(),
                    ),
                },
            },
            WitAppEffect::InsertLineEdit(edit) => {
                NativeAppEffect::InsertLineEdit(NativeInsertLineEdit::from_wit(edit)?)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// `AppEffect` derives `PartialEq`/`Eq`, so round-trip is a direct equality.
    fn assert_round_trips(native: NativeAppEffect) {
        let wit = native.to_wit().expect("to_wit");
        let back = NativeAppEffect::from_wit(wit).expect("from_wit");
        assert_eq!(native, back);
    }

    /// Every payload-bearing arm (exercises the 4 helper enums + the shared
    /// ModalState/VisualKind/SearchDirection/Register mirrors + the primitives).
    /// The unit arms are covered separately; `to_wit`/`from_wit` are both
    /// compiler-exhaustive, so a new `AppEffect` arm forces a mapping here.
    #[test]
    fn app_effect_payload_arms_round_trip() {
        for e in [
            NativeAppEffect::EnterMode(NativeModalState::Insert),
            NativeAppEffect::EnterVisual(NativeVisualKind::Linewise),
            NativeAppEffect::EnterSelect(NativeVisualKind::Blockwise),
            NativeAppEffect::EnterSearch(NativeSearchDirection::Backward),
            NativeAppEffect::SearchWordUnderCursor(NativeSearchDirection::Forward),
            NativeAppEffect::JumpViewport(NativeViewportPos::Middle),
            NativeAppEffect::ScrollCursorTo(NativeScrollPos::Center),
            NativeAppEffect::HorizontalScroll(NativeHScroll::Columns { right: true }),
            NativeAppEffect::HorizontalScroll(NativeHScroll::HalfScreen { right: false }),
            NativeAppEffect::HorizontalScroll(NativeHScroll::CursorToEdge { end: true }),
            NativeAppEffect::JoinLines { with_space: true },
            NativeAppEffect::FindRepeat { reverse: false },
            NativeAppEffect::OverwriteChar('z'),
            NativeAppEffect::SetMark('a'),
            NativeAppEffect::JumpToMarkLine('b'),
            NativeAppEffect::JumpToMarkExact('c'),
            NativeAppEffect::SelectRegister(NativeRegister::Named('q')),
            NativeAppEffect::StartMacroRecord('m'),
            NativeAppEffect::PlayMacro('m'),
            NativeAppEffect::AbsorbOperatorPrefix(OperatorId(CommandId::new(17))),
            NativeAppEffect::NavigatePane(NativePaneDirection::Right),
            NativeAppEffect::GoToTab(3),
            NativeAppEffect::MoveTab(2),
            NativeAppEffect::NewTabAt("/a/b.rs".into()),
            NativeAppEffect::TerminalSpawn(Some("bash".into())),
            NativeAppEffect::TerminalSpawnInNewTab(None),
            NativeAppEffect::CompletionAcceptThenInsert('x'),
            NativeAppEffect::CompletionFilterToSource("gen:lsp-completion".into()),
            NativeAppEffect::MultibufferExpand { delta: -2 },
            NativeAppEffect::NarrowLines {
                start_line: 3,
                end_line: 9,
            },
            NativeAppEffect::SearchTrigger {
                query: "TODO".into(),
            },
        ] {
            assert_round_trips(e);
        }
    }

    /// A representative sample of the unit arms.
    #[test]
    fn app_effect_unit_arms_round_trip() {
        for e in [
            NativeAppEffect::Quit,
            NativeAppEffect::MatchBracket,
            NativeAppEffect::OpenLineBelow,
            NativeAppEffect::Undo,
            NativeAppEffect::Redo,
            NativeAppEffect::PageDown,
            NativeAppEffect::ExitVisual,
            NativeAppEffect::SplitPaneVertical,
            NativeAppEffect::OnlyPane,
            NativeAppEffect::NextTab,
            NativeAppEffect::CompletionAccept,
            NativeAppEffect::DiffGet,
            NativeAppEffect::DiffPut,
            NativeAppEffect::TutorAdvance,
            NativeAppEffect::NarrowWiden,
            NativeAppEffect::SearchRefresh,
            NativeAppEffect::PlayLastMacro,
        ] {
            assert_round_trips(e);
        }
    }

    /// The 4 `app-effect`-only helper enums round-trip every variant.
    #[test]
    fn app_effect_helper_enums_round_trip() {
        for p in [
            NativeViewportPos::Top,
            NativeViewportPos::Middle,
            NativeViewportPos::Bottom,
        ] {
            assert_eq!(p, NativeViewportPos::from_wit(p.to_wit().unwrap()).unwrap());
        }
        for p in [
            NativeScrollPos::Top,
            NativeScrollPos::Center,
            NativeScrollPos::Bottom,
        ] {
            assert_eq!(p, NativeScrollPos::from_wit(p.to_wit().unwrap()).unwrap());
        }
        for d in [
            NativePaneDirection::Left,
            NativePaneDirection::Down,
            NativePaneDirection::Up,
            NativePaneDirection::Right,
        ] {
            assert_eq!(
                d,
                NativePaneDirection::from_wit(d.to_wit().unwrap()).unwrap()
            );
        }
        for h in [
            NativeHScroll::Columns { right: true },
            NativeHScroll::HalfScreen { right: false },
            NativeHScroll::CursorToEdge { end: true },
        ] {
            assert_eq!(h, NativeHScroll::from_wit(h.to_wit().unwrap()).unwrap());
        }
    }

    /// OA.11a: a provider view's two argument slots have two owners, and the
    /// boundary keeps them apart.
    ///
    /// `argument` is the root the HOST interprets; `scan-args` are the guest's
    /// own vocabulary, which it does not. Round-tripping both together is what
    /// pins that neither consumes the other — the failure this shape exists to
    /// prevent is a command key landing in the root slot, where the opener
    /// would turn it into a directory that does not exist and the scan would
    /// silently cover nothing.
    #[test]
    fn a_provider_views_root_and_scan_args_do_not_consume_each_other() {
        use lattice_grammar::args::{ArgValue, Args};

        let native = NativeAppEffect::OpenProviderView {
            provider: "agenda".to_string(),
            args: Args::List(vec![
                ArgValue::String("~/notes".to_string()),
                ArgValue::String("waiting".to_string()),
            ]),
        };
        let wit = native.to_wit().expect("to_wit");
        let WitAppEffect::OpenProviderView(p) = &wit else {
            panic!("wrong arm");
        };
        assert_eq!(p.argument.as_deref(), Some("~/notes"), "the host's slot");
        assert_eq!(p.scan_args, vec!["waiting".to_string()], "the guest's");
        assert_round_trips(native);

        // Scan args with NO root: position 0 is empty rather than absent, so
        // the list form stays positional and `~/notes` cannot be mistaken for
        // a command key by arriving first.
        let native = NativeAppEffect::OpenProviderView {
            provider: "agenda".to_string(),
            args: Args::List(vec![
                ArgValue::String(String::new()),
                ArgValue::String("waiting".to_string()),
            ]),
        };
        let wit = native.to_wit().expect("to_wit");
        let WitAppEffect::OpenProviderView(p) = &wit else {
            panic!("wrong arm");
        };
        assert_eq!(
            p.argument, None,
            "an empty root is no root, not a root of \"\""
        );
        assert_eq!(p.scan_args, vec!["waiting".to_string()]);
    }

    /// OA.11a must not have moved the triggers that already existed. With no
    /// scan args the mapping is what it was: `None` ⇄ `Args::None`, one string
    /// ⇄ `Args::String`. `:org-agenda` and `:org-agenda ~/notes` are those two
    /// cases, so this is the regression guard for every pre-existing caller.
    #[test]
    fn a_provider_view_with_no_scan_args_maps_exactly_as_before() {
        use lattice_grammar::args::Args;

        for (args, expect) in [
            (Args::None, None),
            (Args::String("~/notes".to_string()), Some("~/notes")),
        ] {
            let native = NativeAppEffect::OpenProviderView {
                provider: "agenda".to_string(),
                args,
            };
            let wit = native.to_wit().expect("to_wit");
            let WitAppEffect::OpenProviderView(p) = &wit else {
                panic!("wrong arm");
            };
            assert_eq!(p.argument.as_deref(), expect);
            assert!(
                p.scan_args.is_empty(),
                "no scan args means no scan args — not an empty string in a list"
            );
            assert_round_trips(native);
        }
    }

    /// Only free text crosses. Stringifying an `Int` or an `Invocation` here
    /// would invent a spelling the guest never agreed to, so it is a typed
    /// error — the `NarrowTrigger` precedent, applied to the list form.
    #[test]
    fn a_non_string_provider_view_argument_is_a_typed_error() {
        use lattice_grammar::args::{ArgValue, Args};

        let e = NativeAppEffect::OpenProviderView {
            provider: "agenda".to_string(),
            args: Args::List(vec![ArgValue::Int(7)]),
        };
        let err = e
            .to_wit()
            .expect_err("a non-string list argument must not cross");
        assert!(err.contains("free text"), "error says why: {err}");
    }

    /// `NarrowTrigger` carries the recursive ex-command `Range`; it cannot cross
    /// yet and surfaces as a typed error, never a panic or lossy encoding.
    #[test]
    fn narrow_trigger_is_a_typed_error() {
        let e = NativeAppEffect::NarrowTrigger { range: None };
        let err = e.to_wit().expect_err("NarrowTrigger must not cross yet");
        assert!(
            err.contains("NarrowTrigger"),
            "error names the culprit: {err}"
        );
    }
}
