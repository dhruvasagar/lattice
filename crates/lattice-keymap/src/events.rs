//! WK.4: typed events about chords.
//!
//! Declared here rather than in `lattice-host` because every field is a
//! `lattice-keymap` or `lattice-protocol` type — the crate that owns the
//! chord vocabulary is the honest home for an event about chords. The
//! host publishes; which-key is one subscriber among possible future
//! ones ("did you mean…", macro-recording HUDs, plugin observers).

use lattice_protocol::KeyChord;

use crate::{BindingMode, ModeId};

/// Published on every keystroke whose outcome changes the pending-chord
/// state: a prefix was entered, extended, resolved, or aborted.
/// `chords` is empty when nothing is pending, which is the dismissal
/// signal subscribers key off.
///
/// ## The payload rides on the event
///
/// It would be smaller to publish a bare "something changed" ping and
/// have subscribers read the published render state — and it would be
/// wrong. Tick callbacks run *before* the publish in both actor arms, so
/// a subscriber reading published state would observe the PREVIOUS
/// keystroke's prefix and arm one keystroke late. Carrying the payload
/// makes that class of bug unrepresentable.
///
/// ## `pane_width`
///
/// Carried so the grid can be laid out in `lattice-keymap` (the width is
/// known at build time), which keeps the popup's content ordinary buffer
/// text: everything-is-a-buffer holds, no new render model crosses into
/// either renderer, and the column algorithm is unit-testable with no
/// renderer at all. The cost is that a resize while a popup is up leaves
/// a stale grid, so subscribers dismiss on resize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialChordPending {
    /// The chords pressed so far. Empty = nothing pending.
    pub chords: Vec<KeyChord>,
    /// The binding mode the next keystroke will resolve in.
    pub binding_mode: BindingMode,
    /// The active buffer's keymap-gated modes, in the order
    /// `lookup_with_context` wants them (active major first, then minors
    /// in activation order).
    pub active_modes: Vec<ModeId>,
    /// Text width of the pane the chord is pending in.
    pub pane_width: u16,
}

lattice_protocol::register_event!(
    PartialChordPending,
    "keymap.partial-chord-pending",
    "Fired when a multi-key chord is partway through (or has just \
     resolved, with an empty chord list).",
    "lattice-keymap",
);
