//! M.2.b.3 (2026-06-01): keymap layer for `multibuffer-mode`.
//!
//! Binds the four excerpt-jump motions registered in
//! `lattice_multibuffer::motions` to their canonical chords:
//!
//! - `]e` → `multibuffer.next-excerpt-start`
//! - `[e` → `multibuffer.prev-excerpt-start`
//! - `]E` → `multibuffer.next-file-boundary`
//! - `[E` → `multibuffer.prev-file-boundary`
//!
//! Pushed at boot under `KeymapLayer::MajorMode(multibuffer-mode)`
//! so the bindings are visible only on multibuffer views.
//!
//! Mirrors the shape `crate::diff::mode::diff_mode_layer_bindings`
//! uses for `diff-mode`.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_grammar::CommandInvocation;
use lattice_grammar::source::SourceLocation;
use lattice_multibuffer::MultibufferMotionIds;

use crate::chord::{KeyChord, KeyKind, KeyMods};
use crate::keymap::BindingMode;
use crate::keymap_trie::{BoundCommand, ChordPattern, KeymapLayer, KeymapTrie};

fn lit(c: char) -> ChordPattern {
    ChordPattern::Literal(KeyChord {
        key: KeyKind::Char(c),
        mods: KeyMods::NONE,
    })
}

fn lit_shift(c: char) -> ChordPattern {
    ChordPattern::Literal(KeyChord {
        key: KeyKind::Char(c),
        mods: KeyMods::SHIFT,
    })
}

/// Chord → motion bindings for `multibuffer-mode`. Lives under
/// `KeymapLayer::MajorMode(multibuffer-mode)` so the bindings
/// only fire when `multibuffer-mode` is the active major.
pub fn multibuffer_mode_layer_bindings(
    motion_ids: &MultibufferMotionIds,
) -> HashMap<BindingMode, KeymapTrie> {
    // K.1.b convention: bindings keyed by ModeId go on a
    // `MinorMode(ModeId)` layer regardless of whether the mode is
    // a major or minor — K.1.c's per-keystroke filter checks
    // `ActiveModes` membership, not the major/minor kind. The
    // bindings fire whenever `multibuffer-mode` is the active
    // major.
    let layer = KeymapLayer::MinorMode(lattice_multibuffer::MultibufferMode::mode_id());
    let mut trie = KeymapTrie::new();

    // `]e` → next excerpt start
    trie.insert(
        &[lit(']'), lit('e')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.next_excerpt_start.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    // `[e` → prev excerpt start
    trie.insert(
        &[lit('['), lit('e')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.prev_excerpt_start.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    // `]E` → next file boundary (uppercase E via SHIFT modifier)
    trie.insert(
        &[lit(']'), lit_shift('E')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.next_file_boundary.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    // `[E` → prev file boundary
    trie.insert(
        &[lit('['), lit_shift('E')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(motion_ids.prev_file_boundary.0),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    let mut modes = HashMap::new();
    modes.insert(BindingMode::Normal, trie);
    modes
}
