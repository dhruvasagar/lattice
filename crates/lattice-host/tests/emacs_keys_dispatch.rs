//! Integration coverage for the emacs-keys `<C-x>` leader through the
//! REAL dispatch path: `Editor::dispatch_chord` → `input::translate` →
//! `compute_normal_action`. The unit tests in `emacs_keys.rs` only
//! exercise the trie in isolation; those passed even while `<C-x>2` was
//! broken at runtime, because the digit hoist (slice 8.i.4.f) swallowed
//! the `2` as a vim count before the prefix continuation ran.
//!
//! This file is the regression guard for the S2 fix. It boots a real
//! editor and activates the emacs-keys minor mode the way a running
//! editor does — by draining the `MajorEntered` resolver (the app does
//! this per-tick; the trie-only tests bypass it) — then asserts the
//! leader's pane chords resolve to their pane `Action`s, including the
//! digit suffixes `2` / `3`, while a bare digit still starts a count.

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_protocol::{KeyChord, parse_chord_sequence};

/// First `KeyChord` of a parsed sequence (every string here is a single
/// chord, e.g. `"<C-x>"` or `"2"`).
fn chord(s: &str) -> KeyChord {
    parse_chord_sequence(s)
        .expect("parseable chord")
        .into_iter()
        .next()
        .expect("one chord")
}

/// Boot an editor and bring the emacs-keys minor mode live on the scratch
/// buffer. emacs-keys uses `ActivationPolicy::Global`, so it activates
/// when the buffer's `MajorEntered` is drained by the generic resolver —
/// exactly what the running app does on its first tick. Without this the
/// mode is registered but not in `active_modes`, so the `<C-x>` leader
/// layer is out of scope and the chord falls through.
fn boot_with_leader() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text(
        "alpha beta gamma\nsecond line\nthird line\nfourth line\n",
    ));
    let _ = editor.drain_minor_activation(); // clear boot-queued events
    let proto = lattice_protocol::ids::BufferId::new(editor.document_buffer_id.0 as u64);
    editor
        .event_bus
        .publish(lattice_protocol::Event::MajorEntered {
            buffer: proto,
            major: "text-mode".into(),
        });
    let _ = editor.drain_minor_activation();
    editor
}

/// Boot with the leader active, press `<C-x>` then `suffix`, and return
/// the editor plus the `Action` the suffix keystroke resolved to. The
/// `<C-x>` press absorbs into the partial-chord stack; the suffix
/// completes the leader chord.
fn leader_action(suffix: &str) -> (Editor, Action) {
    let mut editor = boot_with_leader();
    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(chord("<C-x>"), &mut partial);
    let action = editor.dispatch_chord(chord(suffix), &mut partial);
    (editor, action)
}

/// Assert an `Action::Invoke` targets the command registered under
/// `name`. Proves the leader chord routed to the intended pane action
/// rather than being swallowed by count parsing or falling through.
fn assert_invokes(editor: &Editor, action: &Action, name: &str) {
    let Action::Invoke(inv) = action else {
        panic!("expected Action::Invoke({name}), got {action:?}");
    };
    let expected = editor
        .registry
        .id_by_name(name)
        .unwrap_or_else(|| panic!("command `{name}` is registered"));
    assert_eq!(inv.command, expected, "leader chord must target `{name}`");
}

#[tokio::test]
async fn leader_2_splits_horizontally_not_count() {
    let (editor, action) = leader_action("2");
    // The digit `2` after the `<C-x>` partial must resolve the pane
    // split, NOT push a count. This is the exact runtime path the
    // trie-only unit test could not see.
    assert_invokes(&editor, &action, "action:split-pane-horizontal");
    // And it actually executed end-to-end: the pane tree grew.
    assert_eq!(editor.pane_tree.len(), 2, "`<C-x>2` should split the pane");
}

#[tokio::test]
async fn leader_3_splits_vertically() {
    let (editor, action) = leader_action("3");
    assert_invokes(&editor, &action, "action:split-pane-vertical");
    assert_eq!(editor.pane_tree.len(), 2, "`<C-x>3` should split the pane");
}

#[tokio::test]
async fn leader_0_closes_pane() {
    // `0` with no pending count is the one digit that fell through even
    // before the fix; assert it resolves the close action by design.
    let (editor, action) = leader_action("0");
    assert_invokes(&editor, &action, "action:close-pane");
}

#[tokio::test]
async fn leader_o_focuses_next_pane() {
    let (editor, action) = leader_action("o");
    assert_invokes(&editor, &action, "action:next-pane");
}

#[tokio::test]
async fn bare_digit_still_starts_a_count() {
    // With an empty partial-chord stack the digit hoist must still fire:
    // `2` begins a count (vim `2j`), it is NOT a leader chord. Guards
    // against the S2 fix over-reaching into ordinary count parsing.
    let mut editor = boot_with_leader();
    let mut partial: Vec<KeyChord> = Vec::new();
    let action = editor.dispatch_chord(chord("2"), &mut partial);
    assert!(
        matches!(action, Action::PushDigit(2)),
        "bare `2` must start a count, got {action:?}"
    );
}

// ---- Regression guards for the digit-precedence fix --------------------
// The fix gates the digit->count hoist on `[prefix + digit]` being BOUND
// in the trie. These prove it does NOT make prefixes greedily swallow
// digits: a prefix that doesn't bind the digit still starts a count, and
// only a genuinely-bound `[prefix, digit]` (literal or wildcard) routes to
// the trie. This is the surface that could have regressed count flows.

/// Boot (leader active), press `prefix` then `suffix`; return the editor
/// plus the `Action` the suffix keystroke resolved to.
fn chord_after(prefix: &str, suffix: &str) -> (Editor, Action) {
    let mut editor = boot_with_leader();
    let mut partial: Vec<KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(chord(prefix), &mut partial);
    let action = editor.dispatch_chord(chord(suffix), &mut partial);
    (editor, action)
}

#[tokio::test]
async fn window_prefix_then_digit_still_counts() {
    // `<C-w>` has an enumerated sub-tree (no digit child), so `<C-w>5`
    // must still start a count — vim's `<C-w>5+` grows the window by 5.
    // The fix must not let the window prefix swallow the digit.
    let (_editor, action) = chord_after("<C-w>", "5");
    assert!(
        matches!(action, Action::PushDigit(5)),
        "`<C-w>5` must start a count, got {action:?}"
    );
}

#[tokio::test]
async fn g_prefix_then_digit_still_counts() {
    // `g` binds enumerated continuations (`gg`, `g0`, ...) but no
    // `g<1-9>`, so `g5` still starts a count.
    let (_editor, action) = chord_after("g", "5");
    assert!(
        matches!(action, Action::PushDigit(5)),
        "`g5` must start a count, got {action:?}"
    );
}

#[tokio::test]
async fn register_prefix_then_digit_selects_register_not_count() {
    // `"` captures its register name via a `CharLiteral` wildcard, so
    // `"5` selects numbered register 5 (vim `"5p`) — the digit is the
    // register ARGUMENT, never a count. Before the fix the digit hoist
    // mis-counted it; this guards the correction AND that select-register
    // accepts a digit char without panicking.
    let (editor, action) = chord_after("\"", "5");
    assert!(
        !matches!(action, Action::PushDigit(_)),
        "`\"5` must select a register, not count, got {action:?}"
    );
    assert_invokes(&editor, &action, "action:select-register");
}
