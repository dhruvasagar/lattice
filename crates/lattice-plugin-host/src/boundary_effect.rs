//! `WitBoundary` mirrors for the `Effect` payload types (plugin-host.md §4.4).
//!
//! Slice PH7.3b1a: the ~12 nested payload records/enums the `effect` variant
//! (PH7.3b1b) composes — `Position`/`Range`/`Edit`/`EditDelta`/`AppliedEdit`,
//! the selection model (`Selection`/`SelectionSet`/`VisualMode`), the modal
//! model (`ModalState`/`VisualKind`/`SearchDirection`), `Register`, `YankKind`.
//! Each is pure data and its conversion is infallible, but still returns
//! `Result<_, String>` to satisfy the uniform [`WitBoundary`] contract.
//!
//! `SelectionSet` is reconstructed via `SelectionSet::from_parts` (added to
//! `lattice-protocol` for exactly this boundary-projection need — the
//! counterpart to its `all()` + `primary_index()` readers).

use std::path::PathBuf;

use crate::WitBoundary;
use crate::boundary::path_to_wit;
use crate::lattice::plugin_host::types::{
    AppliedEdit as WitAppliedEdit, ApplyEditPayload as WitApplyEditPayload,
    CloseSessionDiffsPayload as WitCloseSessionDiffsPayload, ConfirmPayload as WitConfirmPayload,
    DescribeCommandPayload as WitDescribeCommandPayload, DiffsplitPayload as WitDiffsplitPayload,
    EchoLevel as WitEchoLevel, EchoPayload as WitEchoPayload, Edit as WitEdit,
    EditDelta as WitEditDelta, EditKind as WitEditKind, Effect as WitEffect,
    LspRequest as WitLspRequest, ModalState as WitModalState,
    OpenBufferAtColumnPayload as WitOpenBufferAtColumnPayload,
    OpenBufferAtPayload as WitOpenBufferAtPayload, OpenBufferPayload as WitOpenBufferPayload,
    OpenPickerPayload as WitOpenPickerPayload, OpenPopupPayload as WitOpenPopupPayload,
    OpenPromptPayload as WitOpenPromptPayload,
    OpenSyntheticBufferPayload as WitOpenSyntheticBufferPayload, PopupFocus as WitPopupFocus,
    PopupPlacement as WitPopupPlacement, Position as WitPosition, QuitPayload as WitQuitPayload,
    QuitScope as WitQuitScope, Range as WitRange, Register as WitRegister,
    SearchDirection as WitSearchDirection, Selection as WitSelection,
    SelectionSet as WitSelectionSet, SetLspLogLevelPayload as WitSetLspLogLevelPayload,
    SpawnTerminalPayload as WitSpawnTerminalPayload, SubstitutePayload as WitSubstitutePayload,
    SubstituteScope as WitSubstituteScope, Utf16Pos as WitUtf16Pos, VisualKind as WitVisualKind,
    VisualMode as WitVisualMode, YankKind as WitYankKind, YankPayload as WitYankPayload,
};
use lattice_core::BufferId;
use lattice_core::buffer::AppliedEdit as NativeAppliedEdit;
use lattice_core::ui::popup::{
    PopupFocus as NativePopupFocus, PopupPlacement as NativePopupPlacement,
};
use lattice_grammar::app_effect::AppEffect as NativeAppEffect;
use lattice_grammar::args::Args as NativeArgs;
use lattice_grammar::effect::{
    EchoLevel as NativeEchoLevel, Effect as NativeEffect, LspRequest as NativeLspRequest,
    QuitScope as NativeQuitScope, SubstituteScope as NativeSubstituteScope,
    Utf16Pos as NativeUtf16Pos, YankKind as NativeYankKind,
};
use lattice_grammar::modal::{
    ModalState as NativeModalState, SearchDirection as NativeSearchDirection,
    VisualKind as NativeVisualKind,
};
use lattice_grammar::register::Register as NativeRegister;
use lattice_protocol::edit::{
    Edit as NativeEdit, EditDelta as NativeEditDelta, EditKind as NativeEditKind,
};
use lattice_protocol::position::{Position as NativePosition, Range as NativeRange};
use lattice_protocol::selection::{
    Selection as NativeSelection, SelectionSet as NativeSelectionSet,
    VisualMode as NativeVisualMode,
};

impl WitBoundary for NativePosition {
    type Wit = WitPosition;
    fn to_wit(&self) -> Result<WitPosition, String> {
        Ok(WitPosition {
            line: self.line,
            byte: self.byte,
        })
    }
    fn from_wit(w: WitPosition) -> Result<Self, String> {
        Ok(NativePosition {
            line: w.line,
            byte: w.byte,
        })
    }
}

impl WitBoundary for NativeRange {
    type Wit = WitRange;
    fn to_wit(&self) -> Result<WitRange, String> {
        Ok(WitRange {
            start: self.start.to_wit()?,
            end: self.end.to_wit()?,
        })
    }
    fn from_wit(w: WitRange) -> Result<Self, String> {
        Ok(NativeRange {
            start: NativePosition::from_wit(w.start)?,
            end: NativePosition::from_wit(w.end)?,
        })
    }
}

impl WitBoundary for NativeEditKind {
    type Wit = WitEditKind;
    fn to_wit(&self) -> Result<WitEditKind, String> {
        Ok(match self {
            NativeEditKind::Replace { text } => WitEditKind::Replace(text.clone()),
        })
    }
    fn from_wit(w: WitEditKind) -> Result<Self, String> {
        Ok(match w {
            WitEditKind::Replace(text) => NativeEditKind::Replace { text },
        })
    }
}

impl WitBoundary for NativeEdit {
    type Wit = WitEdit;
    fn to_wit(&self) -> Result<WitEdit, String> {
        Ok(WitEdit {
            range: self.range.to_wit()?,
            kind: self.kind.to_wit()?,
        })
    }
    fn from_wit(w: WitEdit) -> Result<Self, String> {
        Ok(NativeEdit {
            range: NativeRange::from_wit(w.range)?,
            kind: NativeEditKind::from_wit(w.kind)?,
        })
    }
}

impl WitBoundary for NativeEditDelta {
    type Wit = WitEditDelta;
    fn to_wit(&self) -> Result<WitEditDelta, String> {
        Ok(WitEditDelta {
            start_byte: self.start_byte,
            old_end_byte: self.old_end_byte,
            new_end_byte: self.new_end_byte,
            start_position: self.start_position.to_wit()?,
            old_end_position: self.old_end_position.to_wit()?,
            new_end_position: self.new_end_position.to_wit()?,
        })
    }
    fn from_wit(w: WitEditDelta) -> Result<Self, String> {
        Ok(NativeEditDelta {
            start_byte: w.start_byte,
            old_end_byte: w.old_end_byte,
            new_end_byte: w.new_end_byte,
            start_position: NativePosition::from_wit(w.start_position)?,
            old_end_position: NativePosition::from_wit(w.old_end_position)?,
            new_end_position: NativePosition::from_wit(w.new_end_position)?,
        })
    }
}

impl WitBoundary for NativeAppliedEdit {
    type Wit = WitAppliedEdit;
    fn to_wit(&self) -> Result<WitAppliedEdit, String> {
        Ok(WitAppliedEdit {
            original_range: self.original_range.to_wit()?,
            inserted_range: self.inserted_range.to_wit()?,
            replaced_text: self.replaced_text.clone(),
            inserted_text: self.inserted_text.clone(),
            delta: self.delta.to_wit()?,
        })
    }
    fn from_wit(w: WitAppliedEdit) -> Result<Self, String> {
        Ok(NativeAppliedEdit {
            original_range: NativeRange::from_wit(w.original_range)?,
            inserted_range: NativeRange::from_wit(w.inserted_range)?,
            replaced_text: w.replaced_text,
            inserted_text: w.inserted_text,
            delta: NativeEditDelta::from_wit(w.delta)?,
        })
    }
}

impl WitBoundary for NativeVisualMode {
    type Wit = WitVisualMode;
    fn to_wit(&self) -> Result<WitVisualMode, String> {
        Ok(match self {
            NativeVisualMode::Charwise => WitVisualMode::Charwise,
            NativeVisualMode::Linewise => WitVisualMode::Linewise,
            NativeVisualMode::Blockwise => WitVisualMode::Blockwise,
        })
    }
    fn from_wit(w: WitVisualMode) -> Result<Self, String> {
        Ok(match w {
            WitVisualMode::Charwise => NativeVisualMode::Charwise,
            WitVisualMode::Linewise => NativeVisualMode::Linewise,
            WitVisualMode::Blockwise => NativeVisualMode::Blockwise,
        })
    }
}

impl WitBoundary for NativeSelection {
    type Wit = WitSelection;
    fn to_wit(&self) -> Result<WitSelection, String> {
        Ok(WitSelection {
            anchor: self.anchor.to_wit()?,
            head: self.head.to_wit()?,
            visual: self.visual.map(|v| v.to_wit()).transpose()?,
        })
    }
    fn from_wit(w: WitSelection) -> Result<Self, String> {
        Ok(NativeSelection {
            anchor: NativePosition::from_wit(w.anchor)?,
            head: NativePosition::from_wit(w.head)?,
            visual: w.visual.map(NativeVisualMode::from_wit).transpose()?,
        })
    }
}

impl WitBoundary for NativeSelectionSet {
    type Wit = WitSelectionSet;
    fn to_wit(&self) -> Result<WitSelectionSet, String> {
        Ok(WitSelectionSet {
            selections: self
                .all()
                .iter()
                .map(WitBoundary::to_wit)
                .collect::<Result<Vec<_>, _>>()?,
            primary: self.primary_index() as u32,
        })
    }
    fn from_wit(w: WitSelectionSet) -> Result<Self, String> {
        let selections = w
            .selections
            .into_iter()
            .map(NativeSelection::from_wit)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(NativeSelectionSet::from_parts(
            selections,
            w.primary as usize,
        ))
    }
}

impl WitBoundary for NativeVisualKind {
    type Wit = WitVisualKind;
    fn to_wit(&self) -> Result<WitVisualKind, String> {
        Ok(match self {
            NativeVisualKind::Charwise => WitVisualKind::Charwise,
            NativeVisualKind::Linewise => WitVisualKind::Linewise,
            NativeVisualKind::Blockwise => WitVisualKind::Blockwise,
        })
    }
    fn from_wit(w: WitVisualKind) -> Result<Self, String> {
        Ok(match w {
            WitVisualKind::Charwise => NativeVisualKind::Charwise,
            WitVisualKind::Linewise => NativeVisualKind::Linewise,
            WitVisualKind::Blockwise => NativeVisualKind::Blockwise,
        })
    }
}

impl WitBoundary for NativeSearchDirection {
    type Wit = WitSearchDirection;
    fn to_wit(&self) -> Result<WitSearchDirection, String> {
        Ok(match self {
            NativeSearchDirection::Forward => WitSearchDirection::Forward,
            NativeSearchDirection::Backward => WitSearchDirection::Backward,
        })
    }
    fn from_wit(w: WitSearchDirection) -> Result<Self, String> {
        Ok(match w {
            WitSearchDirection::Forward => NativeSearchDirection::Forward,
            WitSearchDirection::Backward => NativeSearchDirection::Backward,
        })
    }
}

impl WitBoundary for NativeModalState {
    type Wit = WitModalState;
    fn to_wit(&self) -> Result<WitModalState, String> {
        Ok(match self {
            NativeModalState::Normal => WitModalState::Normal,
            NativeModalState::Insert => WitModalState::Insert,
            NativeModalState::Visual(k) => WitModalState::Visual(k.to_wit()?),
            NativeModalState::Select(k) => WitModalState::Select(k.to_wit()?),
            NativeModalState::OperatorPending => WitModalState::OperatorPending,
            NativeModalState::Command => WitModalState::Command,
            NativeModalState::Search(d) => WitModalState::Search(d.to_wit()?),
            NativeModalState::Replace => WitModalState::Replace,
            NativeModalState::Prompt => WitModalState::Prompt,
        })
    }
    fn from_wit(w: WitModalState) -> Result<Self, String> {
        Ok(match w {
            WitModalState::Normal => NativeModalState::Normal,
            WitModalState::Insert => NativeModalState::Insert,
            WitModalState::Visual(k) => NativeModalState::Visual(NativeVisualKind::from_wit(k)?),
            WitModalState::Select(k) => NativeModalState::Select(NativeVisualKind::from_wit(k)?),
            WitModalState::OperatorPending => NativeModalState::OperatorPending,
            WitModalState::Command => NativeModalState::Command,
            WitModalState::Search(d) => {
                NativeModalState::Search(NativeSearchDirection::from_wit(d)?)
            }
            WitModalState::Replace => NativeModalState::Replace,
            WitModalState::Prompt => NativeModalState::Prompt,
        })
    }
}

impl WitBoundary for NativeRegister {
    type Wit = WitRegister;
    fn to_wit(&self) -> Result<WitRegister, String> {
        Ok(match self {
            NativeRegister::Unnamed => WitRegister::Unnamed,
            NativeRegister::Named(c) => WitRegister::Named(*c),
            NativeRegister::System => WitRegister::System,
            NativeRegister::BlackHole => WitRegister::BlackHole,
            NativeRegister::Expression => WitRegister::Expression,
            NativeRegister::ReadOnly(c) => WitRegister::ReadOnly(*c),
            NativeRegister::Numbered(n) => WitRegister::Numbered(*n),
        })
    }
    fn from_wit(w: WitRegister) -> Result<Self, String> {
        Ok(match w {
            WitRegister::Unnamed => NativeRegister::Unnamed,
            WitRegister::Named(c) => NativeRegister::Named(c),
            WitRegister::System => NativeRegister::System,
            WitRegister::BlackHole => NativeRegister::BlackHole,
            WitRegister::Expression => NativeRegister::Expression,
            WitRegister::ReadOnly(c) => NativeRegister::ReadOnly(c),
            WitRegister::Numbered(n) => NativeRegister::Numbered(n),
        })
    }
}

impl WitBoundary for NativeYankKind {
    type Wit = WitYankKind;
    fn to_wit(&self) -> Result<WitYankKind, String> {
        Ok(match self {
            NativeYankKind::Charwise => WitYankKind::Charwise,
            NativeYankKind::Linewise => WitYankKind::Linewise,
            NativeYankKind::Blockwise => WitYankKind::Blockwise,
        })
    }
    fn from_wit(w: WitYankKind) -> Result<Self, String> {
        Ok(match w {
            WitYankKind::Charwise => NativeYankKind::Charwise,
            WitYankKind::Linewise => NativeYankKind::Linewise,
            WitYankKind::Blockwise => NativeYankKind::Blockwise,
        })
    }
}

// ---- The small `effect`-only helper enums/records (PH7.3b1b) ----

impl WitBoundary for NativeQuitScope {
    type Wit = WitQuitScope;
    fn to_wit(&self) -> Result<WitQuitScope, String> {
        Ok(match self {
            NativeQuitScope::Pane => WitQuitScope::Pane,
            NativeQuitScope::All => WitQuitScope::All,
        })
    }
    fn from_wit(w: WitQuitScope) -> Result<Self, String> {
        Ok(match w {
            WitQuitScope::Pane => NativeQuitScope::Pane,
            WitQuitScope::All => NativeQuitScope::All,
        })
    }
}

impl WitBoundary for NativeEchoLevel {
    type Wit = WitEchoLevel;
    fn to_wit(&self) -> Result<WitEchoLevel, String> {
        Ok(match self {
            NativeEchoLevel::Trace => WitEchoLevel::Trace,
            NativeEchoLevel::Debug => WitEchoLevel::Debug,
            NativeEchoLevel::Info => WitEchoLevel::Info,
            NativeEchoLevel::Warn => WitEchoLevel::Warn,
            NativeEchoLevel::Error => WitEchoLevel::Error,
        })
    }
    fn from_wit(w: WitEchoLevel) -> Result<Self, String> {
        Ok(match w {
            WitEchoLevel::Trace => NativeEchoLevel::Trace,
            WitEchoLevel::Debug => NativeEchoLevel::Debug,
            WitEchoLevel::Info => NativeEchoLevel::Info,
            WitEchoLevel::Warn => NativeEchoLevel::Warn,
            WitEchoLevel::Error => NativeEchoLevel::Error,
        })
    }
}

impl WitBoundary for NativeSubstituteScope {
    type Wit = WitSubstituteScope;
    fn to_wit(&self) -> Result<WitSubstituteScope, String> {
        Ok(match self {
            NativeSubstituteScope::CurrentLine => WitSubstituteScope::CurrentLine,
            NativeSubstituteScope::Whole => WitSubstituteScope::Whole,
        })
    }
    fn from_wit(w: WitSubstituteScope) -> Result<Self, String> {
        Ok(match w {
            WitSubstituteScope::CurrentLine => NativeSubstituteScope::CurrentLine,
            WitSubstituteScope::Whole => NativeSubstituteScope::Whole,
        })
    }
}

impl WitBoundary for NativeUtf16Pos {
    type Wit = WitUtf16Pos;
    fn to_wit(&self) -> Result<WitUtf16Pos, String> {
        Ok(WitUtf16Pos {
            line: self.line,
            col: self.col,
        })
    }
    fn from_wit(w: WitUtf16Pos) -> Result<Self, String> {
        Ok(NativeUtf16Pos {
            line: w.line,
            col: w.col,
        })
    }
}

impl WitBoundary for NativeLspRequest {
    type Wit = WitLspRequest;
    fn to_wit(&self) -> Result<WitLspRequest, String> {
        Ok(match self {
            NativeLspRequest::Hover => WitLspRequest::Hover,
            NativeLspRequest::Definition => WitLspRequest::Definition,
            NativeLspRequest::Declaration => WitLspRequest::Declaration,
            NativeLspRequest::TypeDefinition => WitLspRequest::TypeDefinition,
            NativeLspRequest::Implementation => WitLspRequest::Implementation,
            NativeLspRequest::References => WitLspRequest::References,
            NativeLspRequest::FollowLink => WitLspRequest::FollowLink,
        })
    }
    fn from_wit(w: WitLspRequest) -> Result<Self, String> {
        Ok(match w {
            WitLspRequest::Hover => NativeLspRequest::Hover,
            WitLspRequest::Definition => NativeLspRequest::Definition,
            WitLspRequest::Declaration => NativeLspRequest::Declaration,
            WitLspRequest::TypeDefinition => NativeLspRequest::TypeDefinition,
            WitLspRequest::Implementation => NativeLspRequest::Implementation,
            WitLspRequest::References => NativeLspRequest::References,
            WitLspRequest::FollowLink => NativeLspRequest::FollowLink,
        })
    }
}

/// `Option<PathBuf>` → `option<string>` — each present path must be UTF-8
/// (§4.4); a non-UTF-8 path is a typed error, never lossy.
fn opt_path_to_wit(path: &Option<PathBuf>) -> Result<Option<String>, String> {
    path.as_ref().map(|p| path_to_wit(p)).transpose()
}

fn opt_path_from_wit(path: Option<String>) -> Option<PathBuf> {
    path.map(PathBuf::from)
}

// ---- `Effect` (§4.4): the whole closed enum crosses as `list<effect>` ----

impl WitBoundary for NativeEffect {
    /// `Effect` is *recursive* (`Many(Vec<Effect>)`), which WIT value types
    /// cannot express. The boundary crosses a **`list<effect>`** instead: a
    /// single effect is a one-element list, and `Many` (associative
    /// composition) is flattened. `from_wit` rebuilds `Many` when the list has
    /// more than one element; an empty list is `Effect::None`. A single-element
    /// `Many([x])` therefore normalises to `x` — a *semantic* identity, since
    /// `Many([x]) ≡ x`.
    type Wit = Vec<WitEffect>;

    fn to_wit(&self) -> Result<Vec<WitEffect>, String> {
        let mut out = Vec::new();
        flatten_effect(self, &mut out)?;
        Ok(out)
    }

    fn from_wit(wit: Vec<WitEffect>) -> Result<Self, String> {
        let mut effects = wit
            .into_iter()
            .map(effect_from_wit)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(if effects.is_empty() {
            NativeEffect::None
        } else if effects.len() == 1 {
            effects.remove(0)
        } else {
            NativeEffect::Many(effects)
        })
    }
}

/// Flatten `Many` (recursively — `Many` is associative) into a list of atoms.
/// A sub-effect that cannot cross (`Global`/`AppAction`) propagates its typed
/// error out of the whole conversion.
fn flatten_effect(e: &NativeEffect, out: &mut Vec<WitEffect>) -> Result<(), String> {
    match e {
        NativeEffect::Many(list) => {
            for sub in list {
                flatten_effect(sub, out)?;
            }
            Ok(())
        }
        atom => {
            out.push(effect_to_wit(atom)?);
            Ok(())
        }
    }
}

/// Map one non-`Many` `Effect` to its WIT mirror. `Global`/`AppAction` cross as
/// typed errors until their mirrors land (§4.1 / PH7.3b2); `Many` never reaches
/// here (flattened by [`flatten_effect`]) but is handled defensively.
fn effect_to_wit(e: &NativeEffect) -> Result<WitEffect, String> {
    Ok(match e {
        NativeEffect::None => WitEffect::None,
        NativeEffect::Declined => WitEffect::Declined,
        NativeEffect::Edits(edits) => WitEffect::Edits(
            edits
                .iter()
                .map(WitBoundary::to_wit)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        NativeEffect::ApplyEdit {
            target,
            edit,
            cursor,
        } => WitEffect::ApplyEdit(WitApplyEditPayload {
            target: target.0,
            edit: edit.to_wit()?,
            cursor: cursor.as_ref().map(|p| p.to_wit()).transpose()?,
        }),
        NativeEffect::CursorMove(pos) => {
            let p = *pos;
            WitEffect::SelectionChange(
                NativeSelectionSet::single(NativeSelection {
                    anchor: p,
                    head: p,
                    visual: None,
                })
                .to_wit()?,
            )
        }
        // MG.18d: no WIT mirror yet, and the `CursorMove` fallback above
        // is NOT available to it — collapsing it to a selection would
        // drop the `target` buffer, which is the entire point of the
        // variant (an async producer's position is only meaningful in
        // the buffer it was computed in). A typed error keeps that
        // loud; the mirror lands with the WIT buffer-handle work.
        NativeEffect::CursorMoveIn { .. } => {
            return Err(
                "Effect::CursorMoveIn addresses a BufferId; it crosses with the buffer-handle \
                 mirror, and must not degrade to a target-less cursor move"
                    .to_string(),
            );
        }
        NativeEffect::SelectionChange(set) => WitEffect::SelectionChange(set.to_wit()?),
        NativeEffect::Yank {
            register,
            content,
            kind,
            explicit_yank,
        } => WitEffect::Yank(WitYankPayload {
            register: register.to_wit()?,
            content: content.clone(),
            kind: kind.to_wit()?,
            explicit_yank: *explicit_yank,
        }),
        NativeEffect::EnterMode(state) => WitEffect::EnterMode(state.to_wit()?),
        NativeEffect::SaveBuffer { path } => WitEffect::SaveBuffer(opt_path_to_wit(path)?),
        NativeEffect::QuitEditor { force, scope } => WitEffect::QuitEditor(WitQuitPayload {
            force: *force,
            scope: scope.to_wit()?,
        }),
        NativeEffect::OpenBuffer { path, force } => WitEffect::OpenBuffer(WitOpenBufferPayload {
            path: opt_path_to_wit(path)?,
            force: *force,
        }),
        NativeEffect::OpenBufferAt {
            path,
            position,
            force,
        } => WitEffect::OpenBufferAt(WitOpenBufferAtPayload {
            path: opt_path_to_wit(path)?,
            position: position.to_wit()?,
            force: *force,
        }),
        NativeEffect::OpenExternalUri { uri } => WitEffect::OpenExternalUri(uri.clone()),
        NativeEffect::OpenBufferAtColumn {
            path,
            column,
            force,
        } => WitEffect::OpenBufferAtColumn(WitOpenBufferAtColumnPayload {
            path: opt_path_to_wit(path)?,
            column: column.map(|c| c.to_wit()).transpose()?,
            force: *force,
        }),
        NativeEffect::SpawnTerminal {
            cmd_line,
            env,
            activate_minor,
        } => WitEffect::SpawnTerminal(WitSpawnTerminalPayload {
            cmd_line: cmd_line.clone(),
            env: env.clone(),
            activate_minor: activate_minor.clone(),
        }),
        NativeEffect::TerminalInput(bytes) => WitEffect::TerminalInput(bytes.clone()),
        NativeEffect::SetOption { spec } => WitEffect::SetOption(spec.clone()),
        NativeEffect::SetLocalOption { spec } => WitEffect::SetLocalOption(spec.clone()),
        NativeEffect::SetGlobalOption { spec } => WitEffect::SetGlobalOption(spec.clone()),
        NativeEffect::ClearSearchHighlight => WitEffect::ClearSearchHighlight,
        NativeEffect::SetColorscheme(name) => WitEffect::SetColorscheme(name.clone()),
        NativeEffect::Echo { level, text } => WitEffect::Echo(WitEchoPayload {
            level: level.to_wit()?,
            text: text.clone(),
        }),
        NativeEffect::ShowDiagnosticsPopup { lines } => {
            WitEffect::ShowDiagnosticsPopup(lines.clone())
        }
        NativeEffect::Lsp(req) => WitEffect::Lsp(req.to_wit()?),
        NativeEffect::EchoRegisters => WitEffect::EchoRegisters,
        NativeEffect::EchoMarks => WitEffect::EchoMarks,
        NativeEffect::Substitute {
            scope,
            pattern,
            replacement,
            global,
        } => WitEffect::Substitute(WitSubstitutePayload {
            scope: scope.to_wit()?,
            pattern: pattern.clone(),
            replacement: replacement.clone(),
            global: *global,
        }),
        NativeEffect::Global { .. } => {
            return Err(
                "Effect::Global carries a Box<CommandInvocation>; it crosses with the \
                        command mirror (fragment §4.1)"
                    .to_string(),
            );
        }
        NativeEffect::DeleteCurrentLine => WitEffect::DeleteCurrentLine,
        NativeEffect::DescribeCommand { name, anchor } => {
            WitEffect::DescribeCommand(WitDescribeCommandPayload {
                name: name.clone(),
                anchor: anchor.clone(),
            })
        }
        NativeEffect::DescribeBuffer => WitEffect::DescribeBuffer,
        NativeEffect::Apropos { pattern } => WitEffect::Apropos(pattern.clone()),
        NativeEffect::DescribeKey { chord } => WitEffect::DescribeKey(chord.clone()),
        NativeEffect::ListKeymap => WitEffect::ListKeymap,
        NativeEffect::BufferNext => WitEffect::BufferNext,
        NativeEffect::BufferPrev => WitEffect::BufferPrev,
        NativeEffect::ListBuffers => WitEffect::ListBuffers,
        NativeEffect::OpenBufferPicker => WitEffect::OpenBufferPicker,
        NativeEffect::OpenPicker { source, args } => WitEffect::OpenPicker(WitOpenPickerPayload {
            source: source.clone(),
            args: args.clone(),
        }),
        NativeEffect::BufferDelete { force } => WitEffect::BufferDelete(*force),
        NativeEffect::OpenFileTree { root } => WitEffect::OpenFileTree(opt_path_to_wit(root)?),
        NativeEffect::CloseFileTree => WitEffect::CloseFileTree,
        NativeEffect::OpenOil { dir } => WitEffect::OpenOil(opt_path_to_wit(dir)?),
        NativeEffect::DescribeOption { name } => WitEffect::DescribeOption(name.clone()),
        NativeEffect::DescribeElement { name } => WitEffect::DescribeElement(name.clone()),
        NativeEffect::ListOptions => WitEffect::ListOptions,
        NativeEffect::DescribePluginApi { seam } => WitEffect::DescribePluginApi(seam.clone()),
        NativeEffect::ListPluginApis => WitEffect::ListPluginApis,
        NativeEffect::ExportPluginApi { format } => WitEffect::ExportPluginApi(format.clone()),
        NativeEffect::ListCommands => WitEffect::ListCommands,
        NativeEffect::DescribePlugin { name } => WitEffect::DescribePlugin(name.clone()),
        NativeEffect::ListPlugins => WitEffect::ListPlugins,
        NativeEffect::OpenHover { markdown } => WitEffect::OpenHover(markdown.clone()),
        NativeEffect::DismissPopup => WitEffect::DismissPopup,
        NativeEffect::BuryBuffer => {
            // No WIT mirror yet. Adding one is a versioned plugin-API
            // change and deserves its own slice rather than riding
            // along with a bug fix; the native modes that need it
            // (magit's `q`) reach it directly. Same typed-error shape
            // `Effect::Global` uses while its mirror is pending.
            return Err(
                "Effect::BuryBuffer has no WIT mirror yet — native-only until the \
                 plugin-API slice adds it"
                    .to_string(),
            );
        }
        NativeEffect::OpenPopup {
            name,
            mode_id,
            placement,
            focus,
        } => WitEffect::OpenPopup(WitOpenPopupPayload {
            name: name.clone(),
            mode_id: mode_id.clone(),
            placement: match placement {
                NativePopupPlacement::Centered => WitPopupPlacement::Centered,
                NativePopupPlacement::CursorAnchored => WitPopupPlacement::CursorAnchored,
            },
            focus: match focus {
                NativePopupFocus::Steal => WitPopupFocus::Steal,
                NativePopupFocus::Passive => WitPopupFocus::Passive,
            },
        }),
        NativeEffect::OpenHelpTopic { topic } => WitEffect::OpenHelpTopic(topic.clone()),
        NativeEffect::ListDiagnostics => WitEffect::ListDiagnostics,
        NativeEffect::NextDiagnostic => WitEffect::NextDiagnostic,
        NativeEffect::PrevDiagnostic => WitEffect::PrevDiagnostic,
        NativeEffect::OpenLspLog { server_id } => WitEffect::OpenLspLog(server_id.clone()),
        NativeEffect::OpenMessages => WitEffect::OpenMessages,
        NativeEffect::OpenDashboard => WitEffect::OpenDashboard,
        NativeEffect::ToggleLspTrace { server_id } => WitEffect::ToggleLspTrace(server_id.clone()),
        NativeEffect::OpenLspTraceLog { server_id } => {
            WitEffect::OpenLspTraceLog(server_id.clone())
        }
        NativeEffect::LspStatus => WitEffect::LspStatus,
        NativeEffect::LspServerLogListing => WitEffect::LspServerLogListing,
        NativeEffect::LspRestart { server_id } => WitEffect::LspRestart(server_id.clone()),
        NativeEffect::LspProgressCancel { server_id } => {
            WitEffect::LspProgressCancel(server_id.clone())
        }
        NativeEffect::LspExpandRegion => WitEffect::LspExpandRegion,
        NativeEffect::LspShrinkRegion => WitEffect::LspShrinkRegion,
        NativeEffect::SetLspLogLevel { server_id, level } => {
            WitEffect::SetLspLogLevel(WitSetLspLogLevelPayload {
                server_id: server_id.clone(),
                level: level.clone(),
            })
        }
        NativeEffect::LspLogClear { server_id } => WitEffect::LspLogClear(server_id.clone()),
        NativeEffect::LspDocumentSymbol => WitEffect::LspDocumentSymbol,
        NativeEffect::LspWorkspaceSymbol { query } => WitEffect::LspWorkspaceSymbol(query.clone()),
        NativeEffect::LspIncomingCalls => WitEffect::LspIncomingCalls,
        NativeEffect::LspOutgoingCalls => WitEffect::LspOutgoingCalls,
        NativeEffect::LspSupertypes => WitEffect::LspSupertypes,
        NativeEffect::LspSubtypes => WitEffect::LspSubtypes,
        NativeEffect::LspMoniker => WitEffect::LspMoniker,
        NativeEffect::LspCodeLens => WitEffect::LspCodeLens,
        NativeEffect::LspColorPresentation => WitEffect::LspColorPresentation,
        NativeEffect::LspFormat => WitEffect::LspFormat,
        NativeEffect::LspFormatRange => WitEffect::LspFormatRange,
        NativeEffect::LspSignatureHelp => WitEffect::LspSignatureHelp,
        NativeEffect::LspComplete => WitEffect::LspComplete,
        NativeEffect::LspRename { new_name } => WitEffect::LspRename(new_name.clone()),
        NativeEffect::LspCodeAction => WitEffect::LspCodeAction,
        NativeEffect::ExpandSnippet { replace_range } => {
            WitEffect::ExpandSnippet(replace_range.to_wit()?)
        }
        NativeEffect::ReloadSnippets => WitEffect::ReloadSnippets,
        NativeEffect::DescribeEvents => WitEffect::DescribeEvents,
        NativeEffect::DescribeDiff => WitEffect::DescribeDiff,
        NativeEffect::DiffOpen => WitEffect::DiffOpen,
        NativeEffect::DiffOff { force } => WitEffect::DiffOff(*force),
        NativeEffect::Diffthis => WitEffect::Diffthis,
        NativeEffect::Diffsplit { path, remote } => WitEffect::Diffsplit(WitDiffsplitPayload {
            path: path_to_wit(path)?,
            remote: opt_path_to_wit(remote)?,
        }),
        NativeEffect::DiffGetCmd { target } => WitEffect::DiffGetCmd(*target),
        NativeEffect::DiffPutCmd { target } => WitEffect::DiffPutCmd(*target),
        NativeEffect::DiffAccept => WitEffect::DiffAccept,
        NativeEffect::DiffReject => WitEffect::DiffReject,
        NativeEffect::DiffAcceptAll => WitEffect::DiffAcceptAll,
        NativeEffect::DiffRejectAll => WitEffect::DiffRejectAll,
        NativeEffect::CloseSessionDiffs {
            origin_session,
            tab_name,
        } => WitEffect::CloseSessionDiffs(WitCloseSessionDiffsPayload {
            origin_session: *origin_session,
            tab_name: tab_name.clone(),
        }),
        NativeEffect::CloseAllSessionDiffs { origin_session } => {
            WitEffect::CloseAllSessionDiffs(*origin_session)
        }
        NativeEffect::NextHunk => WitEffect::NextHunk,
        NativeEffect::PrevHunk => WitEffect::PrevHunk,
        NativeEffect::DescribeEvent { name } => WitEffect::DescribeEvent(name.clone()),
        NativeEffect::ListModes => WitEffect::ListModes,
        NativeEffect::DescribeMode { name } => WitEffect::DescribeMode(name.clone()),
        NativeEffect::DescribeActiveModes => WitEffect::DescribeActiveModes,
        NativeEffect::DescribeActiveBindings => WitEffect::DescribeActiveBindings,
        NativeEffect::DescribeOptionResolution { name } => {
            WitEffect::DescribeOptionResolution(name.clone())
        }
        NativeEffect::Customize { name } => WitEffect::Customize(name.clone()),
        NativeEffect::Tutor { lesson } => WitEffect::Tutor(*lesson),
        NativeEffect::ToggleMode { mode_name } => WitEffect::ToggleMode(mode_name.clone()),
        // PH7.3b2: the AppEffect mirror landed — AppAction now crosses (a
        // NarrowTrigger-carrying AppEffect still propagates its typed error).
        NativeEffect::AppAction(app) => WitEffect::AppAction(app.to_wit()?),
        NativeEffect::RecordJump => WitEffect::RecordJump,
        // Host-only ex-commands (no WIT mirror; plugins never see them).
        // CM.8: `:clist` — the error picker is a host-only surface.
        // IX.3: `confirm` has a mirror now — it no longer joins the
        // silently-dropped group below.
        NativeEffect::Confirm {
            prompt,
            yes_action,
            args,
        } => WitEffect::Confirm(WitConfirmPayload {
            prompt: prompt.clone(),
            yes_action: yes_action.clone(),
            args: args.to_wit()?,
        }),
        // IX.4: no WIT mirror yet — a **typed error**, not `None`.
        //
        // `None` is a lie with the same shape as success: the effect
        // arrives as "do nothing" and no one is told. `Effect::Global`
        // set the precedent of failing loudly instead, and these follow
        // it. `open-transient` and `open-prompt` are the two that most
        // want mirrors (a plugin that cannot prompt cannot collect
        // input at all) — IX.5 / IX.6.
        NativeEffect::OpenPrompt {
            prompt,
            initial,
            on_submit_action,
            buffer_name,
        } => WitEffect::OpenPrompt(WitOpenPromptPayload {
            prompt: prompt.clone(),
            initial: initial.clone(),
            on_submit_action: on_submit_action.clone(),
            buffer_name: buffer_name.clone(),
        }),
        NativeEffect::OpenTransient { source } => WitEffect::OpenTransient(source.clone()),
        // Host-only surfaces: `:cd` / `:pwd` act on the editor process,
        // and `:clist`'s error picker is a host-owned view. They are not
        // blocked on a mirror so much as unmapped by intent; the error
        // still names them rather than pretending they crossed.
        NativeEffect::ChangeDir(_) => {
            return Err(
                "Effect::ChangeDir is host-only (`:cd` acts on the editor process)".to_string(),
            );
        }
        NativeEffect::PrintWorkingDir => {
            return Err("Effect::PrintWorkingDir is host-only (`:pwd`)".to_string());
        }
        NativeEffect::ListErrors => {
            return Err(
                "Effect::ListErrors is host-only (`:clist` opens a host-owned picker)".to_string(),
            );
        }
        NativeEffect::OpenAiLog { session } => WitEffect::OpenAiLog(session.clone()),
        NativeEffect::OpenSyntheticBuffer { name, mode_id } => {
            WitEffect::OpenSyntheticBuffer(WitOpenSyntheticBufferPayload {
                name: name.clone(),
                mode_id: mode_id.clone(),
            })
        }
        NativeEffect::Many(_) => {
            return Err(
                "Effect::Many is flattened to list<effect> at the boundary and must not \
                        reach effect_to_wit"
                    .to_string(),
            );
        }
    })
}

/// Map one WIT `effect` mirror back to native. There is no `Many` arm (it is a
/// list-level concept), so this is a flat, total mapping over the ~101 arms.
fn effect_from_wit(w: WitEffect) -> Result<NativeEffect, String> {
    Ok(match w {
        WitEffect::None => NativeEffect::None,
        WitEffect::Declined => NativeEffect::Declined,
        WitEffect::Edits(edits) => NativeEffect::Edits(
            edits
                .into_iter()
                .map(NativeAppliedEdit::from_wit)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        WitEffect::ApplyEdit(p) => NativeEffect::ApplyEdit {
            target: BufferId(p.target),
            edit: NativeEdit::from_wit(p.edit)?,
            cursor: p.cursor.map(NativePosition::from_wit).transpose()?,
        },
        WitEffect::SelectionChange(set) => {
            NativeEffect::SelectionChange(NativeSelectionSet::from_wit(set)?)
        }
        WitEffect::CursorMove(pos) => NativeEffect::CursorMove(NativePosition::from_wit(pos)?),
        // IX.3: a guest asks the user a yes/no question. The host
        // resolves `yes_action` through the command registry when the
        // answer comes back, so a plugin confirms its own registered
        // action by name exactly as a native mode does.
        WitEffect::Confirm(p) => NativeEffect::Confirm {
            prompt: p.prompt,
            yes_action: p.yes_action,
            args: NativeArgs::from_wit(p.args)?,
        },
        // IX.5: a guest asks for a line of text. The submitted value
        // reaches the action through its context's `prompt_value`, so
        // nothing about the payload needs to carry it back.
        WitEffect::OpenTransient(source) => NativeEffect::OpenTransient { source },
        WitEffect::OpenPrompt(p) => NativeEffect::OpenPrompt {
            prompt: p.prompt,
            initial: p.initial,
            on_submit_action: p.on_submit_action,
            buffer_name: p.buffer_name,
        },
        WitEffect::Yank(p) => NativeEffect::Yank {
            register: NativeRegister::from_wit(p.register)?,
            content: p.content,
            kind: NativeYankKind::from_wit(p.kind)?,
            explicit_yank: p.explicit_yank,
        },
        WitEffect::EnterMode(state) => NativeEffect::EnterMode(NativeModalState::from_wit(state)?),
        WitEffect::SaveBuffer(path) => NativeEffect::SaveBuffer {
            path: opt_path_from_wit(path),
        },
        WitEffect::QuitEditor(p) => NativeEffect::QuitEditor {
            force: p.force,
            scope: NativeQuitScope::from_wit(p.scope)?,
        },
        WitEffect::OpenBuffer(p) => NativeEffect::OpenBuffer {
            path: opt_path_from_wit(p.path),
            force: p.force,
        },
        WitEffect::OpenBufferAt(p) => NativeEffect::OpenBufferAt {
            path: opt_path_from_wit(p.path),
            position: NativePosition::from_wit(p.position)?,
            force: p.force,
        },
        WitEffect::OpenExternalUri(uri) => NativeEffect::OpenExternalUri { uri },
        WitEffect::OpenBufferAtColumn(p) => NativeEffect::OpenBufferAtColumn {
            path: opt_path_from_wit(p.path),
            column: p.column.map(NativeUtf16Pos::from_wit).transpose()?,
            force: p.force,
        },
        WitEffect::SpawnTerminal(p) => NativeEffect::SpawnTerminal {
            cmd_line: p.cmd_line,
            env: p.env,
            activate_minor: p.activate_minor,
        },
        WitEffect::TerminalInput(bytes) => NativeEffect::TerminalInput(bytes),
        WitEffect::SetOption(spec) => NativeEffect::SetOption { spec },
        WitEffect::SetLocalOption(spec) => NativeEffect::SetLocalOption { spec },
        WitEffect::SetGlobalOption(spec) => NativeEffect::SetGlobalOption { spec },
        WitEffect::ClearSearchHighlight => NativeEffect::ClearSearchHighlight,
        WitEffect::SetColorscheme(name) => NativeEffect::SetColorscheme(name),
        WitEffect::Echo(p) => NativeEffect::Echo {
            level: NativeEchoLevel::from_wit(p.level)?,
            text: p.text,
        },
        WitEffect::ShowDiagnosticsPopup(lines) => NativeEffect::ShowDiagnosticsPopup { lines },
        WitEffect::Lsp(req) => NativeEffect::Lsp(NativeLspRequest::from_wit(req)?),
        WitEffect::EchoRegisters => NativeEffect::EchoRegisters,
        WitEffect::EchoMarks => NativeEffect::EchoMarks,
        WitEffect::Substitute(p) => NativeEffect::Substitute {
            scope: NativeSubstituteScope::from_wit(p.scope)?,
            pattern: p.pattern,
            replacement: p.replacement,
            global: p.global,
        },
        WitEffect::DeleteCurrentLine => NativeEffect::DeleteCurrentLine,
        WitEffect::DescribeCommand(p) => NativeEffect::DescribeCommand {
            name: p.name,
            anchor: p.anchor,
        },
        WitEffect::DescribeBuffer => NativeEffect::DescribeBuffer,
        WitEffect::Apropos(pattern) => NativeEffect::Apropos { pattern },
        WitEffect::DescribeKey(chord) => NativeEffect::DescribeKey { chord },
        WitEffect::ListKeymap => NativeEffect::ListKeymap,
        WitEffect::BufferNext => NativeEffect::BufferNext,
        WitEffect::BufferPrev => NativeEffect::BufferPrev,
        WitEffect::ListBuffers => NativeEffect::ListBuffers,
        WitEffect::OpenBufferPicker => NativeEffect::OpenBufferPicker,
        WitEffect::OpenPicker(p) => NativeEffect::OpenPicker {
            source: p.source,
            args: p.args,
        },
        WitEffect::BufferDelete(force) => NativeEffect::BufferDelete { force },
        WitEffect::OpenFileTree(root) => NativeEffect::OpenFileTree {
            root: opt_path_from_wit(root),
        },
        WitEffect::CloseFileTree => NativeEffect::CloseFileTree,
        WitEffect::OpenOil(dir) => NativeEffect::OpenOil {
            dir: opt_path_from_wit(dir),
        },
        WitEffect::DescribeOption(name) => NativeEffect::DescribeOption { name },
        WitEffect::DescribeElement(name) => NativeEffect::DescribeElement { name },
        WitEffect::ListOptions => NativeEffect::ListOptions,
        WitEffect::DescribePluginApi(seam) => NativeEffect::DescribePluginApi { seam },
        WitEffect::ListPluginApis => NativeEffect::ListPluginApis,
        WitEffect::ExportPluginApi(format) => NativeEffect::ExportPluginApi { format },
        WitEffect::ListCommands => NativeEffect::ListCommands,
        WitEffect::DescribePlugin(name) => NativeEffect::DescribePlugin { name },
        WitEffect::ListPlugins => NativeEffect::ListPlugins,
        WitEffect::OpenHover(markdown) => NativeEffect::OpenHover { markdown },
        WitEffect::DismissPopup => NativeEffect::DismissPopup,
        WitEffect::OpenPopup(p) => NativeEffect::OpenPopup {
            name: p.name,
            mode_id: p.mode_id,
            placement: match p.placement {
                WitPopupPlacement::Centered => NativePopupPlacement::Centered,
                WitPopupPlacement::CursorAnchored => NativePopupPlacement::CursorAnchored,
            },
            focus: match p.focus {
                WitPopupFocus::Steal => NativePopupFocus::Steal,
                WitPopupFocus::Passive => NativePopupFocus::Passive,
            },
        },
        WitEffect::OpenHelpTopic(topic) => NativeEffect::OpenHelpTopic { topic },
        WitEffect::ListDiagnostics => NativeEffect::ListDiagnostics,
        WitEffect::NextDiagnostic => NativeEffect::NextDiagnostic,
        WitEffect::PrevDiagnostic => NativeEffect::PrevDiagnostic,
        WitEffect::OpenLspLog(server_id) => NativeEffect::OpenLspLog { server_id },
        WitEffect::OpenMessages => NativeEffect::OpenMessages,
        WitEffect::OpenDashboard => NativeEffect::OpenDashboard,
        WitEffect::ToggleLspTrace(server_id) => NativeEffect::ToggleLspTrace { server_id },
        WitEffect::OpenLspTraceLog(server_id) => NativeEffect::OpenLspTraceLog { server_id },
        WitEffect::LspStatus => NativeEffect::LspStatus,
        WitEffect::LspServerLogListing => NativeEffect::LspServerLogListing,
        WitEffect::LspRestart(server_id) => NativeEffect::LspRestart { server_id },
        WitEffect::LspProgressCancel(server_id) => NativeEffect::LspProgressCancel { server_id },
        WitEffect::LspExpandRegion => NativeEffect::LspExpandRegion,
        WitEffect::LspShrinkRegion => NativeEffect::LspShrinkRegion,
        WitEffect::SetLspLogLevel(p) => NativeEffect::SetLspLogLevel {
            server_id: p.server_id,
            level: p.level,
        },
        WitEffect::LspLogClear(server_id) => NativeEffect::LspLogClear { server_id },
        WitEffect::LspDocumentSymbol => NativeEffect::LspDocumentSymbol,
        WitEffect::LspWorkspaceSymbol(query) => NativeEffect::LspWorkspaceSymbol { query },
        WitEffect::LspIncomingCalls => NativeEffect::LspIncomingCalls,
        WitEffect::LspOutgoingCalls => NativeEffect::LspOutgoingCalls,
        WitEffect::LspSupertypes => NativeEffect::LspSupertypes,
        WitEffect::LspSubtypes => NativeEffect::LspSubtypes,
        WitEffect::LspMoniker => NativeEffect::LspMoniker,
        WitEffect::LspCodeLens => NativeEffect::LspCodeLens,
        WitEffect::LspColorPresentation => NativeEffect::LspColorPresentation,
        WitEffect::LspFormat => NativeEffect::LspFormat,
        WitEffect::LspFormatRange => NativeEffect::LspFormatRange,
        WitEffect::LspSignatureHelp => NativeEffect::LspSignatureHelp,
        WitEffect::LspComplete => NativeEffect::LspComplete,
        WitEffect::LspRename(new_name) => NativeEffect::LspRename { new_name },
        WitEffect::LspCodeAction => NativeEffect::LspCodeAction,
        WitEffect::ExpandSnippet(range) => NativeEffect::ExpandSnippet {
            replace_range: NativeRange::from_wit(range)?,
        },
        WitEffect::ReloadSnippets => NativeEffect::ReloadSnippets,
        WitEffect::DescribeEvents => NativeEffect::DescribeEvents,
        WitEffect::DescribeDiff => NativeEffect::DescribeDiff,
        WitEffect::DiffOpen => NativeEffect::DiffOpen,
        WitEffect::DiffOff(force) => NativeEffect::DiffOff { force },
        WitEffect::Diffthis => NativeEffect::Diffthis,
        WitEffect::Diffsplit(p) => NativeEffect::Diffsplit {
            path: PathBuf::from(p.path),
            remote: opt_path_from_wit(p.remote),
        },
        WitEffect::DiffGetCmd(target) => NativeEffect::DiffGetCmd { target },
        WitEffect::DiffPutCmd(target) => NativeEffect::DiffPutCmd { target },
        WitEffect::DiffAccept => NativeEffect::DiffAccept,
        WitEffect::DiffReject => NativeEffect::DiffReject,
        WitEffect::DiffAcceptAll => NativeEffect::DiffAcceptAll,
        WitEffect::DiffRejectAll => NativeEffect::DiffRejectAll,
        WitEffect::CloseSessionDiffs(p) => NativeEffect::CloseSessionDiffs {
            origin_session: p.origin_session,
            tab_name: p.tab_name,
        },
        WitEffect::CloseAllSessionDiffs(origin_session) => {
            NativeEffect::CloseAllSessionDiffs { origin_session }
        }
        WitEffect::NextHunk => NativeEffect::NextHunk,
        WitEffect::PrevHunk => NativeEffect::PrevHunk,
        WitEffect::DescribeEvent(name) => NativeEffect::DescribeEvent { name },
        WitEffect::ListModes => NativeEffect::ListModes,
        WitEffect::DescribeMode(name) => NativeEffect::DescribeMode { name },
        WitEffect::DescribeActiveModes => NativeEffect::DescribeActiveModes,
        WitEffect::DescribeActiveBindings => NativeEffect::DescribeActiveBindings,
        WitEffect::DescribeOptionResolution(name) => {
            NativeEffect::DescribeOptionResolution { name }
        }
        WitEffect::Customize(name) => NativeEffect::Customize { name },
        WitEffect::Tutor(lesson) => NativeEffect::Tutor { lesson },
        WitEffect::ToggleMode(mode_name) => NativeEffect::ToggleMode { mode_name },
        WitEffect::AppAction(app) => NativeEffect::AppAction(NativeAppEffect::from_wit(app)?),
        WitEffect::RecordJump => NativeEffect::RecordJump,
        WitEffect::OpenAiLog(session) => NativeEffect::OpenAiLog { session },
        WitEffect::OpenSyntheticBuffer(p) => NativeEffect::OpenSyntheticBuffer {
            name: p.name,
            mode_id: p.mode_id,
        },
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn pos(line: u32, byte: u32) -> NativePosition {
        NativePosition { line, byte }
    }

    /// IX.4: an effect with no mirror fails loudly, naming itself.
    ///
    /// `WitEffect::None` was the previous answer, and it is a lie with
    /// the same shape as success — the effect arrives as "do nothing"
    /// and nobody is told. The error text has to name the culprit,
    /// because the reader is a plugin author with no view of host
    /// internals.
    #[test]
    fn an_effect_with_no_mirror_fails_loudly_instead_of_becoming_none() {
        // `OpenPrompt` / `OpenTransient` were here until IX.5 / IX.6
        // gave them mirrors — the remaining three are host-only by
        // intent rather than pending one.
        let unmirrored = vec![
            (NativeEffect::PrintWorkingDir, "PrintWorkingDir"),
            (NativeEffect::ListErrors, "ListErrors"),
            (
                NativeEffect::ChangeDir(Some("/tmp".to_string())),
                "ChangeDir",
            ),
        ];
        for (effect, name) in unmirrored {
            let err = effect_to_wit(&effect)
                .expect_err("an unmirrored effect must not silently become `none`");
            assert!(
                err.contains(name),
                "the error must name the culprit for a plugin author who \
                 cannot see host internals: got {err:?}"
            );
        }
    }

    /// IX.3: a plugin can ask the user a yes/no question.
    ///
    /// Before this, `confirm` was mapped to `WitEffect::None` — a plugin
    /// returning one got no dialog and no error, which is the silent
    /// swallow the boundary is supposed to make impossible. The round
    /// trip is what proves the mirror is real in both directions.
    #[test]
    fn confirm_crosses_the_boundary_in_both_directions() {
        let native = NativeEffect::Confirm {
            prompt: "Delete src/main.rs?".to_string(),
            yes_action: "action:my-plugin-delete".to_string(),
            args: NativeArgs::List(vec![lattice_grammar::args::ArgValue::String(
                "src/main.rs".to_string(),
            )]),
        };
        let back = effect_from_wit(effect_to_wit(&native).unwrap()).unwrap();
        match back {
            NativeEffect::Confirm {
                prompt,
                yes_action,
                args,
            } => {
                assert_eq!(prompt, "Delete src/main.rs?");
                assert_eq!(yes_action, "action:my-plugin-delete");
                // The carried target is the whole point: a confirm that
                // lost its args on the way across would ask about one
                // thing and act on another.
                let list = args.as_list().expect("the target survived");
                assert!(matches!(
                    &list[0],
                    lattice_grammar::args::ArgValue::String(p) if p == "src/main.rs"
                ));
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    /// A confirm carrying nothing is still a confirm — the common case
    /// for a plugin that re-derives its own target.
    #[test]
    fn a_confirm_with_no_args_crosses_too() {
        let native = NativeEffect::Confirm {
            prompt: "Proceed?".to_string(),
            yes_action: "action:my-plugin-go".to_string(),
            args: NativeArgs::None,
        };
        let back = effect_from_wit(effect_to_wit(&native).unwrap()).unwrap();
        assert!(matches!(
            back,
            NativeEffect::Confirm {
                args: NativeArgs::None,
                ..
            }
        ));
    }

    #[test]
    fn position_and_range_round_trip() {
        let p = pos(3, 7);
        assert_eq!(p, NativePosition::from_wit(p.to_wit().unwrap()).unwrap());
        let r = NativeRange {
            start: pos(1, 0),
            end: pos(2, 5),
        };
        assert_eq!(r, NativeRange::from_wit(r.to_wit().unwrap()).unwrap());
    }

    #[test]
    fn plugin_api_help_effects_round_trip() {
        // PI.2: the `:describe-plugin-api` / `:list-plugin-apis` effects cross
        // the WIT boundary losslessly (the whole-enum mirror, §4.4). `Effect`
        // has no `PartialEq`, so match on the variant.
        let rt = |e: &NativeEffect| effect_from_wit(effect_to_wit(e).unwrap()).unwrap();
        match rt(&NativeEffect::DescribePluginApi {
            seam: Some("host-services".into()),
        }) {
            NativeEffect::DescribePluginApi { seam } => {
                assert_eq!(seam.as_deref(), Some("host-services"))
            }
            other => panic!("unexpected: {other:?}"),
        }
        match rt(&NativeEffect::DescribePluginApi { seam: None }) {
            NativeEffect::DescribePluginApi { seam } => assert!(seam.is_none()),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            rt(&NativeEffect::ListPluginApis),
            NativeEffect::ListPluginApis
        ));
        match rt(&NativeEffect::ExportPluginApi {
            format: Some("json".into()),
        }) {
            NativeEffect::ExportPluginApi { format } => assert_eq!(format.as_deref(), Some("json")),
            other => panic!("unexpected: {other:?}"),
        }
        match rt(&NativeEffect::ExportPluginApi { format: None }) {
            NativeEffect::ExportPluginApi { format } => assert!(format.is_none()),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            rt(&NativeEffect::ListCommands),
            NativeEffect::ListCommands
        ));
        match rt(&NativeEffect::DescribePlugin {
            name: "git-gutter".into(),
        }) {
            NativeEffect::DescribePlugin { name } => assert_eq!(name, "git-gutter"),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(matches!(
            rt(&NativeEffect::ListPlugins),
            NativeEffect::ListPlugins
        ));
    }

    #[test]
    fn edit_and_applied_edit_round_trip() {
        let edit = NativeEdit {
            range: NativeRange {
                start: pos(0, 0),
                end: pos(0, 3),
            },
            kind: NativeEditKind::Replace { text: "abc".into() },
        };
        assert_eq!(edit, NativeEdit::from_wit(edit.to_wit().unwrap()).unwrap());

        let applied = NativeAppliedEdit {
            original_range: NativeRange {
                start: pos(0, 0),
                end: pos(0, 1),
            },
            inserted_range: NativeRange {
                start: pos(0, 0),
                end: pos(0, 3),
            },
            replaced_text: "x".into(),
            inserted_text: "abc".into(),
            delta: NativeEditDelta {
                start_byte: 0,
                old_end_byte: 1,
                new_end_byte: 3,
                start_position: pos(0, 0),
                old_end_position: pos(0, 1),
                new_end_position: pos(0, 3),
            },
        };
        // `AppliedEdit` is not `PartialEq`; assert round-trip fidelity at the
        // WIT level (bindgen derives `Debug`): to_wit(from_wit(to_wit(x))) must
        // equal to_wit(x).
        let wit = applied.to_wit().unwrap();
        let back = NativeAppliedEdit::from_wit(wit.clone()).unwrap();
        assert_eq!(format!("{:?}", back.to_wit().unwrap()), format!("{wit:?}"));
    }

    #[test]
    fn selection_set_round_trips_single_and_multi() {
        let single = NativeSelectionSet::single(NativeSelection {
            anchor: pos(0, 0),
            head: pos(0, 4),
            visual: Some(NativeVisualMode::Charwise),
        });
        assert_eq!(
            single,
            NativeSelectionSet::from_wit(single.to_wit().unwrap()).unwrap()
        );

        let multi = NativeSelectionSet::from_parts(
            vec![
                NativeSelection {
                    anchor: pos(0, 0),
                    head: pos(0, 1),
                    visual: None,
                },
                NativeSelection {
                    anchor: pos(2, 0),
                    head: pos(2, 3),
                    visual: Some(NativeVisualMode::Linewise),
                },
            ],
            1,
        );
        let back = NativeSelectionSet::from_wit(multi.to_wit().unwrap()).unwrap();
        assert_eq!(multi, back);
        assert_eq!(back.primary_index(), 1);
    }

    #[test]
    fn modal_state_round_trips_every_variant() {
        for s in [
            NativeModalState::Normal,
            NativeModalState::Insert,
            NativeModalState::Visual(NativeVisualKind::Blockwise),
            NativeModalState::Select(NativeVisualKind::Charwise),
            NativeModalState::OperatorPending,
            NativeModalState::Command,
            NativeModalState::Search(NativeSearchDirection::Backward),
            NativeModalState::Replace,
        ] {
            assert_eq!(s, NativeModalState::from_wit(s.to_wit().unwrap()).unwrap());
        }
    }

    #[test]
    fn register_round_trips_every_variant() {
        for r in [
            NativeRegister::Unnamed,
            NativeRegister::Named('a'),
            NativeRegister::System,
            NativeRegister::BlackHole,
            NativeRegister::Expression,
            NativeRegister::ReadOnly('%'),
            NativeRegister::Numbered(3),
        ] {
            assert_eq!(r, NativeRegister::from_wit(r.to_wit().unwrap()).unwrap());
        }
    }

    #[test]
    fn yank_kind_round_trips() {
        for k in [
            NativeYankKind::Charwise,
            NativeYankKind::Linewise,
            NativeYankKind::Blockwise,
        ] {
            assert_eq!(k, NativeYankKind::from_wit(k.to_wit().unwrap()).unwrap());
        }
    }

    // ---- Effect (PH7.3b1b) ----

    /// `Effect` is not `PartialEq`; compare structural `Debug`. Round-trip is
    /// native → `list<effect>` → native.
    fn assert_effect_round_trips(native: NativeEffect) {
        let wit = native.to_wit().expect("to_wit");
        let back = NativeEffect::from_wit(wit).expect("from_wit");
        assert_eq!(format!("{native:?}"), format!("{back:?}"));
    }

    fn sample_applied_edit() -> NativeAppliedEdit {
        NativeAppliedEdit {
            original_range: NativeRange {
                start: pos(0, 0),
                end: pos(0, 1),
            },
            inserted_range: NativeRange {
                start: pos(0, 0),
                end: pos(0, 3),
            },
            replaced_text: "x".into(),
            inserted_text: "abc".into(),
            delta: NativeEditDelta {
                start_byte: 0,
                old_end_byte: 1,
                new_end_byte: 3,
                start_position: pos(0, 0),
                old_end_position: pos(0, 1),
                new_end_position: pos(0, 3),
            },
        }
    }

    fn sample_edit() -> NativeEdit {
        NativeEdit {
            range: NativeRange {
                start: pos(1, 2),
                end: pos(1, 5),
            },
            kind: NativeEditKind::Replace { text: "yo".into() },
        }
    }

    /// Every payload-bearing arm (exercises all 14 payload records, the 5
    /// helper enums, the path/option/list arms). The unit arms are covered
    /// separately; the compiler already enforces that `effect_to_wit` /
    /// `effect_from_wit` handle *every* variant (both matches are exhaustive),
    /// so a new `Effect` arm cannot land without a mapping — that is the real
    /// exhaustiveness guarantee this test complements.
    #[test]
    fn effect_payload_arms_round_trip() {
        let cases = vec![
            NativeEffect::None,
            NativeEffect::Edits(vec![sample_applied_edit()]),
            NativeEffect::ApplyEdit {
                target: BufferId(2),
                edit: sample_edit(),
                cursor: Some(NativePosition::new(3, 0)),
            },
            NativeEffect::ApplyEdit {
                target: BufferId(0),
                edit: sample_edit(),
                cursor: None,
            },
            NativeEffect::SelectionChange(NativeSelectionSet::single(NativeSelection {
                anchor: pos(0, 0),
                head: pos(0, 4),
                visual: Some(NativeVisualMode::Charwise),
            })),
            NativeEffect::Yank {
                register: NativeRegister::Named('a'),
                content: "text".into(),
                kind: NativeYankKind::Linewise,
                explicit_yank: true,
            },
            NativeEffect::EnterMode(NativeModalState::Visual(NativeVisualKind::Blockwise)),
            NativeEffect::SaveBuffer {
                path: Some(PathBuf::from("/a/b.rs")),
            },
            NativeEffect::SaveBuffer { path: None },
            NativeEffect::QuitEditor {
                force: true,
                scope: NativeQuitScope::All,
            },
            NativeEffect::OpenBuffer {
                path: Some(PathBuf::from("/a/b.rs")),
                force: false,
            },
            NativeEffect::OpenBufferAt {
                path: None,
                position: pos(10, 4),
                force: true,
            },
            NativeEffect::OpenExternalUri {
                uri: "https://example.com".into(),
            },
            NativeEffect::OpenBufferAtColumn {
                path: Some(PathBuf::from("/a/b.rs")),
                column: Some(NativeUtf16Pos { line: 2, col: 8 }),
                force: false,
            },
            NativeEffect::OpenBufferAtColumn {
                path: None,
                column: None,
                force: false,
            },
            NativeEffect::SpawnTerminal {
                cmd_line: Some("claude".into()),
                env: vec![("CLAUDE_CODE_SSE_PORT".into(), "9000".into())],
                activate_minor: Some("claude-code-mode".into()),
            },
            NativeEffect::TerminalInput(vec![0x1b]),
            NativeEffect::SetOption {
                spec: "wrap".into(),
            },
            NativeEffect::SetLocalOption {
                spec: "number".into(),
            },
            NativeEffect::SetGlobalOption {
                spec: "hlsearch".into(),
            },
            NativeEffect::SetColorscheme("nord".into()),
            NativeEffect::Echo {
                level: NativeEchoLevel::Warn,
                text: "careful".into(),
            },
            NativeEffect::ShowDiagnosticsPopup {
                lines: vec![("E0308: mismatched types".into(), 0), ("hint".into(), 3)],
            },
            NativeEffect::Lsp(NativeLspRequest::Definition),
            NativeEffect::Lsp(NativeLspRequest::FollowLink),
            NativeEffect::Substitute {
                scope: NativeSubstituteScope::Whole,
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: true,
            },
            NativeEffect::DescribeCommand {
                name: "write".into(),
                anchor: Some("arg:path".into()),
            },
            NativeEffect::Apropos {
                pattern: "buf".into(),
            },
            NativeEffect::DescribeKey { chord: "gd".into() },
            NativeEffect::OpenPicker {
                source: "files".into(),
                args: vec!["src".into(), "*.rs".into()],
            },
            NativeEffect::BufferDelete { force: true },
            NativeEffect::OpenFileTree {
                root: Some(PathBuf::from("/proj")),
            },
            NativeEffect::OpenOil { dir: None },
            NativeEffect::DescribeOption {
                name: "wrap".into(),
            },
            NativeEffect::DescribeElement {
                name: "modeline.normal".into(),
            },
            NativeEffect::OpenHover {
                markdown: "**hi**".into(),
            },
            NativeEffect::OpenHelpTopic {
                topic: Some("motions".into()),
            },
            NativeEffect::OpenLspLog {
                server_id: Some("rust-analyzer".into()),
            },
            NativeEffect::ToggleLspTrace {
                server_id: "rust-analyzer".into(),
            },
            NativeEffect::OpenLspTraceLog { server_id: None },
            NativeEffect::LspRestart {
                server_id: "rust-analyzer".into(),
            },
            NativeEffect::LspProgressCancel { server_id: None },
            NativeEffect::SetLspLogLevel {
                server_id: Some("rust-analyzer".into()),
                level: "debug".into(),
            },
            NativeEffect::LspLogClear { server_id: None },
            NativeEffect::LspWorkspaceSymbol {
                query: "Foo".into(),
            },
            NativeEffect::LspRename {
                new_name: "renamed".into(),
            },
            NativeEffect::ExpandSnippet {
                replace_range: NativeRange {
                    start: pos(0, 0),
                    end: pos(0, 2),
                },
            },
            NativeEffect::DiffOff { force: true },
            NativeEffect::Diffsplit {
                path: PathBuf::from("/a/base.rs"),
                remote: Some(PathBuf::from("/a/remote.rs")),
            },
            NativeEffect::DiffGetCmd { target: Some(2) },
            NativeEffect::DiffPutCmd { target: None },
            NativeEffect::CloseSessionDiffs {
                origin_session: 7,
                tab_name: "/a/b.rs".into(),
            },
            NativeEffect::CloseAllSessionDiffs { origin_session: 7 },
            NativeEffect::DescribeEvent {
                name: "document-changed".into(),
            },
            NativeEffect::DescribeMode {
                name: "lsp-mode".into(),
            },
            NativeEffect::DescribeOptionResolution {
                name: "wrap".into(),
            },
            NativeEffect::Customize { name: None },
            NativeEffect::Tutor { lesson: Some(3) },
            NativeEffect::ToggleMode {
                mode_name: "diff-mode".into(),
            },
        ];
        for case in cases {
            assert_effect_round_trips(case);
        }
    }

    /// A representative sample of the unit (no-payload) arms.
    #[test]
    fn effect_unit_arms_round_trip() {
        for e in [
            NativeEffect::ClearSearchHighlight,
            NativeEffect::EchoRegisters,
            NativeEffect::EchoMarks,
            NativeEffect::DeleteCurrentLine,
            NativeEffect::DescribeBuffer,
            NativeEffect::ListKeymap,
            NativeEffect::BufferNext,
            NativeEffect::BufferPrev,
            NativeEffect::ListBuffers,
            NativeEffect::OpenBufferPicker,
            NativeEffect::CloseFileTree,
            NativeEffect::ListOptions,
            NativeEffect::DismissPopup,
            NativeEffect::OpenPopup {
                name: "*ai-permission*".to_string(),
                mode_id: "ai-permission-mode".to_string(),
                placement: NativePopupPlacement::Centered,
                focus: NativePopupFocus::Steal,
            },
            NativeEffect::OpenPopup {
                name: "hover".to_string(),
                mode_id: "hover-mode".to_string(),
                placement: NativePopupPlacement::CursorAnchored,
                focus: NativePopupFocus::Passive,
            },
            NativeEffect::ListDiagnostics,
            NativeEffect::NextDiagnostic,
            NativeEffect::PrevDiagnostic,
            NativeEffect::OpenMessages,
            NativeEffect::OpenDashboard,
            NativeEffect::LspStatus,
            NativeEffect::LspServerLogListing,
            NativeEffect::LspExpandRegion,
            NativeEffect::LspShrinkRegion,
            NativeEffect::LspDocumentSymbol,
            NativeEffect::LspIncomingCalls,
            NativeEffect::LspOutgoingCalls,
            NativeEffect::LspSupertypes,
            NativeEffect::LspSubtypes,
            NativeEffect::LspMoniker,
            NativeEffect::LspCodeLens,
            NativeEffect::LspColorPresentation,
            NativeEffect::LspFormat,
            NativeEffect::LspFormatRange,
            NativeEffect::LspSignatureHelp,
            NativeEffect::LspComplete,
            NativeEffect::LspCodeAction,
            NativeEffect::ReloadSnippets,
            NativeEffect::DescribeEvents,
            NativeEffect::DescribeDiff,
            NativeEffect::DiffOpen,
            NativeEffect::Diffthis,
            NativeEffect::DiffAccept,
            NativeEffect::DiffReject,
            NativeEffect::DiffAcceptAll,
            NativeEffect::DiffRejectAll,
            NativeEffect::NextHunk,
            NativeEffect::PrevHunk,
            NativeEffect::ListModes,
            NativeEffect::RecordJump,
        ] {
            assert_effect_round_trips(e);
        }
    }

    /// The 5 `effect`-only helper enums round-trip every variant.
    #[test]
    fn effect_helper_enums_round_trip() {
        for s in [NativeQuitScope::Pane, NativeQuitScope::All] {
            assert_eq!(s, NativeQuitScope::from_wit(s.to_wit().unwrap()).unwrap());
        }
        for l in [
            NativeEchoLevel::Trace,
            NativeEchoLevel::Debug,
            NativeEchoLevel::Info,
            NativeEchoLevel::Warn,
            NativeEchoLevel::Error,
        ] {
            assert_eq!(l, NativeEchoLevel::from_wit(l.to_wit().unwrap()).unwrap());
        }
        for s in [
            NativeSubstituteScope::CurrentLine,
            NativeSubstituteScope::Whole,
        ] {
            assert_eq!(
                s,
                NativeSubstituteScope::from_wit(s.to_wit().unwrap()).unwrap()
            );
        }
        let u = NativeUtf16Pos { line: 4, col: 9 };
        assert_eq!(u, NativeUtf16Pos::from_wit(u.to_wit().unwrap()).unwrap());
        for r in [
            NativeLspRequest::Hover,
            NativeLspRequest::Definition,
            NativeLspRequest::Declaration,
            NativeLspRequest::TypeDefinition,
            NativeLspRequest::Implementation,
            NativeLspRequest::References,
            NativeLspRequest::FollowLink,
        ] {
            assert_eq!(r, NativeLspRequest::from_wit(r.to_wit().unwrap()).unwrap());
        }
    }

    /// `Many` crosses as `list<effect>`: `to_wit` flattens, `from_wit` rebuilds
    /// `Many` when the list has >1 element. Nested `Many` flattens too
    /// (associativity), so `Many([a, Many([b, c])])` normalises to
    /// `Many([a, b, c])` — a semantic identity.
    #[test]
    fn many_flattens_and_rebuilds() {
        let many = NativeEffect::Many(vec![
            NativeEffect::RecordJump,
            NativeEffect::Many(vec![
                NativeEffect::BufferNext,
                NativeEffect::ClearSearchHighlight,
            ]),
        ]);
        let wit = many.to_wit().unwrap();
        assert_eq!(wit.len(), 3, "nested Many flattens to 3 atoms");
        let back = NativeEffect::from_wit(wit).unwrap();
        match back {
            NativeEffect::Many(list) => {
                assert_eq!(list.len(), 3);
                assert!(matches!(list[0], NativeEffect::RecordJump));
                assert!(matches!(list[1], NativeEffect::BufferNext));
                assert!(matches!(list[2], NativeEffect::ClearSearchHighlight));
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    /// A single-element list reconstructs the atom (not a `Many`); an empty
    /// list is `Effect::None`.
    #[test]
    fn single_element_and_empty_list_normalise() {
        let one = NativeEffect::from_wit(vec![WitEffect::BufferNext]).unwrap();
        assert!(matches!(one, NativeEffect::BufferNext));
        let empty = NativeEffect::from_wit(Vec::new()).unwrap();
        assert!(matches!(empty, NativeEffect::None));
    }

    /// `Global` cannot cross yet (§4.1); it surfaces as a typed error, never a
    /// panic or a lossy encoding — even nested inside a `Many`, the error
    /// propagates out of the whole conversion.
    #[test]
    fn global_is_a_typed_error() {
        use lattice_grammar::CommandId;
        use lattice_grammar::command::CommandInvocation;

        let global = NativeEffect::Global {
            pattern: "TODO".into(),
            inverted: false,
            body: Box::new(CommandInvocation::of(CommandId::new(0))),
        };
        let err = global.to_wit().expect_err("Global must not cross yet");
        assert!(err.contains("Global"), "error names the culprit: {err}");

        // Nested inside a Many, the error still propagates.
        let nested = NativeEffect::Many(vec![NativeEffect::RecordJump, global]);
        assert!(
            nested.to_wit().is_err(),
            "nested Global fails the whole list"
        );
    }

    /// PH7.3b2: `AppAction` now crosses (the `AppEffect` mirror landed). A
    /// representable `AppEffect` round-trips through the `effect` boundary; an
    /// `AppEffect::NarrowTrigger` (recursive `Range`) still propagates its typed
    /// error out of the `AppAction` arm.
    #[test]
    fn app_action_crosses_and_narrow_trigger_still_errors() {
        use lattice_grammar::app_effect::AppEffect;

        let app = NativeEffect::AppAction(AppEffect::SplitPaneVertical);
        assert_effect_round_trips(app);

        // A whole Many of AppActions round-trips too.
        assert_effect_round_trips(NativeEffect::Many(vec![
            NativeEffect::AppAction(AppEffect::Quit),
            NativeEffect::RecordJump,
            NativeEffect::AppAction(AppEffect::NextTab),
        ]));

        let narrow = NativeEffect::AppAction(AppEffect::NarrowTrigger { range: None });
        let err = narrow
            .to_wit()
            .expect_err("NarrowTrigger-carrying AppAction must not cross yet");
        assert!(
            err.contains("NarrowTrigger"),
            "error names the culprit: {err}"
        );
    }
}
