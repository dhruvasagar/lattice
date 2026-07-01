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

use crate::WitBoundary;
use crate::lattice::plugin_host::types::{
    AppliedEdit as WitAppliedEdit, Edit as WitEdit, EditDelta as WitEditDelta,
    EditKind as WitEditKind, ModalState as WitModalState, Position as WitPosition,
    Range as WitRange, Register as WitRegister, SearchDirection as WitSearchDirection,
    Selection as WitSelection, SelectionSet as WitSelectionSet, VisualKind as WitVisualKind,
    VisualMode as WitVisualMode, YankKind as WitYankKind,
};
use lattice_core::buffer::AppliedEdit as NativeAppliedEdit;
use lattice_grammar::effect::YankKind as NativeYankKind;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn pos(line: u32, byte: u32) -> NativePosition {
        NativePosition { line, byte }
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
}
