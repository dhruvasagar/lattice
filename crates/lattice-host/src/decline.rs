//! AP.0.2 — the layer peel behind `Effect::Declined`, in one place.
//!
//! A mode can bind a chord it only sometimes wants. auto-pair binds `(` and
//! `<BS>`; table-mode binds `<Tab>`. When the situation is not theirs, the
//! action returns [`Effect::Declined`](lattice_grammar::Effect::Declined) —
//! "I did nothing" — and the chord must be re-resolved **as if that mode's
//! layer were not there**, so it reaches whatever is underneath. A declined
//! `(` types a bracket; a declined `<Tab>` folds.
//!
//! ## Why this is a type and not a loop at each call site
//!
//! It was a loop at each call site, twice, and the third caller — the GPUI
//! peer — had neither. Every key auto-pair binds (`( [ { ) ] } " ' \``, and
//! backspace) was therefore dead in that renderer, and `<Tab>` never fell
//! through to org's fold cycle. The TUI's copy carried the comment "mirrors
//! the host peel exactly (GPUI rides that path)", which was not true and
//! could not be checked.
//!
//! What varies between callers is real: each builds its translate context
//! from a different place and applies the resulting action through its own
//! path (the peers have renderer-coupled intercepts the host does not). What
//! does NOT vary is the walk — which layer to drop, what the prefix is, and
//! when to stop — and that is what lives here. Two bugs came out of getting
//! exactly those wrong:
//!
//! - **Dropping every layer at once.** `active_minor_modes: &[]` means a
//!   second declining layer never sees the chord, so table-mode's `<Tab>`
//!   declining outside a table skipped org-mode's fold cycle entirely and
//!   landed on the builtin jump-list. Layers compose; a chain of length two
//!   is the first case anyone writes.
//! - **Losing the prefix.** Re-translating the final chord alone turned a
//!   declined `<leader>oJ` into vim's `J`, which joined two lines. The peel
//!   re-resolves the SEQUENCE.

use lattice_grammar::ModalState;
use lattice_mode::ModeId;

use crate::chord::KeyChord;
use crate::keymap::BindingMode;
use crate::keymap_registry::KeymapHandle;

/// The [`BindingMode`] a modal state resolves chords in.
///
/// One definition: the host's dispatcher, the TUI's runtime and this walk all
/// used to carry their own copy of this match, and a mode added to one of
/// them would silently resolve against the wrong keymap in the others.
pub fn binding_mode_for(modal: ModalState) -> BindingMode {
    match modal {
        ModalState::Insert => BindingMode::Insert,
        ModalState::Visual(_) | ModalState::Select(_) => BindingMode::Visual,
        ModalState::OperatorPending => BindingMode::OperatorPending,
        ModalState::Replace => BindingMode::Replace,
        _ => BindingMode::Normal,
    }
}

/// The layer-peeling walk behind a declined chord.
///
/// Hold one across the re-resolution loop: each [`peel`](Self::peel) removes
/// the single layer that produced the declining binding and hands back the
/// reduced set to re-translate against. The caller translates and applies;
/// this decides what to translate against and when to stop.
#[derive(Debug, Clone)]
pub struct DeclinePeel {
    binding_mode: BindingMode,
    prefix: Vec<KeyChord>,
    chord: KeyChord,
    layers: Vec<ModeId>,
}

impl DeclinePeel {
    /// `prefix` is the partial chord as it stood **before** the declined
    /// dispatch — the sequence this chord completes, not what the dispatch
    /// left behind.
    pub fn new(
        modal: ModalState,
        prefix: Vec<KeyChord>,
        chord: KeyChord,
        layers: Vec<ModeId>,
    ) -> Self {
        Self {
            binding_mode: binding_mode_for(modal),
            prefix,
            chord,
            layers,
        }
    }

    /// Drop the layer that produced the declining binding, and yield the
    /// reduced layer set to re-translate against.
    ///
    /// `None` means there is nothing left to peel: the winning binding came
    /// from the always-on Builtin / User layers, which cannot decline. The
    /// walk is bounded — every pass removes exactly one layer.
    pub fn peel(&mut self, keymap: &KeymapHandle) -> Option<&[ModeId]> {
        let declining = crate::keymap_normal::binding_layer_mode(
            keymap,
            self.binding_mode,
            &self.prefix,
            &self.chord,
            &self.layers,
        )?;
        self.layers.retain(|m| *m != declining);
        Some(&self.layers)
    }

    /// The sequence this chord completes. Re-translation uses it, so a
    /// declined multi-key chord re-resolves as the sequence it was.
    pub fn prefix(&self) -> &[KeyChord] {
        &self.prefix
    }

    pub fn chord(&self) -> KeyChord {
        self.chord
    }

    pub fn layers(&self) -> &[ModeId] {
        &self.layers
    }

    pub fn binding_mode(&self) -> BindingMode {
        self.binding_mode
    }

    /// True when no mode layers remain, so a further decline has nowhere to
    /// go and the caller should stop.
    pub fn exhausted(&self) -> bool {
        self.layers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_modal_state_maps_to_the_keymap_it_resolves_in() {
        assert_eq!(binding_mode_for(ModalState::Insert), BindingMode::Insert);
        assert_eq!(binding_mode_for(ModalState::Replace), BindingMode::Replace);
        assert_eq!(
            binding_mode_for(ModalState::OperatorPending),
            BindingMode::OperatorPending
        );
        assert_eq!(binding_mode_for(ModalState::Normal), BindingMode::Normal);
    }

    /// The prefix is what the chord COMPLETES, and it survives every pass —
    /// re-resolving the trailing key alone is what turned a declined
    /// `<leader>oJ` into vim's `J`.
    #[test]
    fn the_prefix_and_chord_are_held_across_the_walk() {
        let prefix = vec![KeyChord::char('g')];
        let peel = DeclinePeel::new(
            ModalState::Normal,
            prefix.clone(),
            KeyChord::char('J'),
            vec![ModeId::new("a-mode")],
        );
        assert_eq!(peel.prefix(), prefix.as_slice());
        assert_eq!(peel.chord(), KeyChord::char('J'));
        assert_eq!(peel.binding_mode(), BindingMode::Normal);
    }

    /// A walk with no layers is already finished — the caller must not spin.
    #[test]
    fn a_walk_with_no_layers_is_exhausted() {
        let peel = DeclinePeel::new(
            ModalState::Insert,
            Vec::new(),
            KeyChord::char('('),
            Vec::new(),
        );
        assert!(peel.exhausted());
        assert!(peel.layers().is_empty());
    }
}
