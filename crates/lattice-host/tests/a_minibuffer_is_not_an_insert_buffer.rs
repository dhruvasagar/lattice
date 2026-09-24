//! The `:` line, the `/`·`?` line and the generic prompt resolve keys in
//! their OWN binding context — not Insert's.
//!
//! ## The bug this closes
//!
//! `ModalState::Command` (and `Search`, and `Prompt`) routed through
//! `dispatch_insert`, which resolved against `BindingMode::Insert`. So every
//! globally-active minor's Insert bindings applied on a minibuffer.
//!
//! auto-pair is the one that bit. It declares `ActivationPolicy::Global` and
//! binds `<BS>` in Insert, so on the `:` line its handler shadowed the builtin
//! backspace — and backspace did nothing while typing a command, in BOTH
//! renderers, for as long as the minibuffer has been a buffer. No renderer fix
//! could have touched it: the leak is in which table the key is looked up in.
//!
//! It cut the other way too. `BindingMode::Command` existed, `:describe-key`
//! reported `c_` bindings against it (vim's convention), and the WIT layer
//! mapped a plugin's `Command` binding onto it — and none of them could ever
//! fire, because the live path asked the Insert table.
//!
//! ## What these assert
//!
//! The readline set a one-line buffer cannot do without — backspace, word
//! erase, line edits, cursor keys — must resolve in every readline surface.
//! Miss one in the migration and that key dies on that surface, which is the
//! same failure with a different victim, so it is pinned rather than trusted.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_keymap::BindingMode;
use lattice_protocol::chord::{KeyChord, SpecialKey};

/// Every readline surface, and the binding context it must resolve in.
const READLINE_SURFACES: &[(BindingMode, &str)] = &[
    (BindingMode::Command, "the `:` line"),
    (BindingMode::Search, "the `/`·`?` line"),
    (BindingMode::Prompt, "a generic prompt"),
];

/// The keys a one-line editing surface cannot do without.
fn readline_chords() -> Vec<(KeyChord, &'static str)> {
    vec![
        (KeyChord::special(SpecialKey::Backspace), "<BS>"),
        (KeyChord::ctrl('w'), "<C-w>"),
        (KeyChord::ctrl('u'), "<C-u>"),
        (KeyChord::ctrl('a'), "<C-a>"),
        (KeyChord::ctrl('e'), "<C-e>"),
        (KeyChord::ctrl('k'), "<C-k>"),
        (KeyChord::special(SpecialKey::Left), "<Left>"),
        (KeyChord::special(SpecialKey::Right), "<Right>"),
        (KeyChord::special(SpecialKey::Home), "<Home>"),
        (KeyChord::special(SpecialKey::End), "<End>"),
    ]
}

/// The builtin readline set resolves on every minibuffer surface.
///
/// They used to get these by borrowing the Insert table wholesale. They have
/// their own now, and "their own" has to mean complete.
#[test]
fn every_readline_key_resolves_on_every_minibuffer_surface() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let mut missing: Vec<String> = Vec::new();
    for (mode, what) in READLINE_SURFACES {
        for (chord, name) in readline_chords() {
            let hit = editor
                .keymap
                .resolve_trace(*mode, std::slice::from_ref(&chord), &[])
                .hits
                .into_iter()
                .any(|h| h.active);
            if !hit {
                missing.push(format!("{name} does not resolve in {what}"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "a minibuffer surface is missing readline keys:\n  {}",
        missing.join("\n  ")
    );
}

/// The point of the split: an Insert binding is an INSERT binding.
///
/// A minor that binds a chord in Insert must not have it apply on the `:`
/// line. Asserted against the builtin backspace: on a minibuffer surface the
/// winning binding must come from the always-on Builtin layer, not from
/// whatever minor happens to be globally active.
#[test]
fn a_minor_modes_insert_binding_does_not_reach_a_minibuffer() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let bs = [KeyChord::special(SpecialKey::Backspace)];
    // auto-pair is not loaded here, so this asserts the STRUCTURE: the
    // minibuffer tables are distinct from Insert's, which is what keeps a
    // Global minor's Insert bindings out of them.
    for (mode, what) in READLINE_SURFACES {
        let trace = editor.keymap.resolve_trace(*mode, &bs, &[]);
        assert!(
            trace.hits.iter().any(|h| h.active),
            "backspace must resolve in {what}"
        );
        assert!(
            trace
                .hits
                .iter()
                .all(|h| !matches!(h.layer, lattice_keymap::KeymapLayer::MinorMode(_))),
            "{what} resolved backspace on a minor-mode layer; an Insert-bound \
             minor has leaked into a readline surface again"
        );
    }
}
