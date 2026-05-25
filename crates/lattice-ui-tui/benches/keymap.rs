#![allow(clippy::unwrap_used, clippy::panic)]
//! Criterion benchmarks for the keymap dispatch path
//! (audit slice 8 / M3).
//!
//! The keystroke path runs `KeyEvent → KeyChord → trie lookup
//! → CommandInvocation` at human typing rates (10s of events
//! per second on the input thread). Each stage has a budget:
//!
//! - **`KeyEvent → KeyChord`** -- input-thread normalisation;
//!   target sub-100ns.
//! - **`KeyChord → String`** -- `:describe-key`, macro
//!   recording, future config dump. Off the keystroke path
//!   per se but called for every key during chord-capture.
//!   Sub-microsecond is plenty.
//! - **`&str → KeyChord` / `&str → Vec<KeyChord>`** -- runs
//!   at startup (built-in catalog enumeration in slice 8.c)
//!   and on `:bind` invocations. Not hot, but catastrophic
//!   regression here would slow startup; bench so we notice.
//!
//! Slice 8.b will add trie-lookup benches once the
//! `KeymapTrie` type lands.

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use lattice_grammar::CommandInvocation;
use lattice_grammar::SourceLocation;
use lattice_protocol::ids::CommandId;
use lattice_ui_tui::buffers::BufferKind;
use lattice_ui_tui::chord::{KeyChord, parse_chord_sequence};
use lattice_ui_tui::keymap::BindingMode;
use lattice_ui_tui::keymap_registry::KeymapHandle;
use lattice_ui_tui::keymap_trie::{
    BoundCommand, ChordPattern, KeymapLayer, KeymapTrie, LookupResult,
};
use lattice_ui_tui::{TranslateContext, translate};

fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: mods,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

/// `KeyEvent → KeyChord` for the most common case: a plain
/// printable letter (`j`, `w`, `a`...). Should be a few
/// register operations.
fn keychord_from_event_plain_letter(c: &mut Criterion) {
    let event = ev(KeyCode::Char('j'), KeyModifiers::NONE);
    c.bench_function("keychord_from_event_plain_letter", |b| {
        b.iter(|| {
            let chord = lattice_ui_tui::chord::from_event(black_box(&event));
            black_box(chord);
        });
    });
}

/// `KeyEvent → KeyChord` for a Ctrl-modified letter. The
/// canonicalisation step (lowercase the letter, strip
/// terminal-redundant shift) is the costliest in the
/// path; this bench guards against regression there.
fn keychord_from_event_ctrl_letter(c: &mut Criterion) {
    let event = ev(KeyCode::Char('c'), KeyModifiers::CONTROL);
    c.bench_function("keychord_from_event_ctrl_letter", |b| {
        b.iter(|| {
            let chord = lattice_ui_tui::chord::from_event(black_box(&event));
            black_box(chord);
        });
    });
}

/// `KeyEvent → KeyChord` for a special key with a modifier
/// (`<S-Tab>` via `BackTab`). Exercises the special-key
/// canonicalisation branch.
fn keychord_from_event_back_tab(c: &mut Criterion) {
    let event = ev(KeyCode::BackTab, KeyModifiers::NONE);
    c.bench_function("keychord_from_event_back_tab", |b| {
        b.iter(|| {
            let chord = lattice_ui_tui::chord::from_event(black_box(&event));
            black_box(chord);
        });
    });
}

/// `KeyChord → String` for a representative bare-letter chord.
fn keychord_to_string_plain_letter(c: &mut Criterion) {
    let chord = KeyChord::char('j');
    c.bench_function("keychord_to_string_plain_letter", |b| {
        b.iter(|| {
            let s = black_box(&chord).to_string();
            black_box(s);
        });
    });
}

/// `KeyChord → String` for a multi-modifier chord. Exercises
/// the `<C-S-c>`-shaped path that needs to allocate a small
/// String and write modifier prefixes.
fn keychord_to_string_ctrl_shift_letter(c: &mut Criterion) {
    let chord = lattice_ui_tui::chord::from_event(&ev(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ))
    .unwrap();
    c.bench_function("keychord_to_string_ctrl_shift_letter", |b| {
        b.iter(|| {
            let s = black_box(&chord).to_string();
            black_box(s);
        });
    });
}

/// `&str → KeyChord` for a single bare-letter chord.
fn keychord_parse_plain_letter(c: &mut Criterion) {
    c.bench_function("keychord_parse_plain_letter", |b| {
        b.iter(|| {
            let chord: KeyChord = black_box("a").parse().unwrap();
            black_box(chord);
        });
    });
}

/// `&str → KeyChord` for the busiest single-chord parse path
/// -- modifier-prefixed special key (`<C-S-Tab>`).
fn keychord_parse_modifier_special(c: &mut Criterion) {
    c.bench_function("keychord_parse_modifier_special", |b| {
        b.iter(|| {
            let chord: KeyChord = black_box("<C-S-Tab>").parse().unwrap();
            black_box(chord);
        });
    });
}

/// `parse_chord_sequence` for a typical multi-key chord. Runs
/// once per `KeymapEntry` at startup (slice 8.c), so total
/// startup cost = bindings × this bench.
fn parse_chord_sequence_multi_key(c: &mut Criterion) {
    c.bench_function("parse_chord_sequence_multi_key", |b| {
        b.iter(|| {
            let seq = parse_chord_sequence(black_box("<C-w>j")).unwrap();
            black_box(seq);
        });
    });
}

/// `parse_chord_sequence` for the canonical "two bare chars"
/// shape (`gg`, `dw`, `zt`).
fn parse_chord_sequence_two_letters(c: &mut Criterion) {
    c.bench_function("parse_chord_sequence_two_letters", |b| {
        b.iter(|| {
            let seq = parse_chord_sequence(black_box("gg")).unwrap();
            black_box(seq);
        });
    });
}

// ---------------------------------------------------------------
// KeymapTrie benches (audit slice 8.b)
//
// The trie sits behind the `Arc<ArcSwap<KeymapTrie>>` cell the
// registry handle (slice 8.c) exposes. These benches measure
// just the trie path -- ArcSwap load + lookup walk.
// ---------------------------------------------------------------

/// Build a representative trie containing the chord shapes we
/// care about: single-chord motions, two-chord prefix motions
/// (`gd`, `gg`), three-chord operator+motion (`d` `i` `w`),
/// and a wildcard for find-char (`f X`). Sized to ~16
/// bindings, which is roughly half a real mode's worth and
/// fits the lookup-path measurement we want.
fn populate_trie() -> KeymapTrie {
    let mut t = KeymapTrie::new();
    let bound = || -> Arc<BoundCommand> {
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(CommandId::new(0)),
            SourceLocation::synthetic("bench"),
            KeymapLayer::Builtin,
        ))
    };
    let lit = |c: char| ChordPattern::Literal(KeyChord::char(c));
    // single-chord
    for c in ['j', 'k', 'h', 'l', 'w', 'b', 'e'] {
        t.insert(&[lit(c)], bound());
    }
    // two-chord
    t.insert(&[lit('g'), lit('d')], bound());
    t.insert(&[lit('g'), lit('g')], bound());
    t.insert(&[lit('d'), lit('d')], bound());
    t.insert(&[lit('y'), lit('y')], bound());
    // three-chord (operator + i/a + text-object)
    t.insert(&[lit('d'), lit('i'), lit('w')], bound());
    t.insert(&[lit('c'), lit('i'), lit('w')], bound());
    // wildcard (find-char)
    t.insert(&[lit('f'), ChordPattern::CharLiteral], bound());
    t.insert(&[lit('F'), ChordPattern::CharLiteral], bound());
    t
}

/// Lookup a single-chord binding (`j`). The hot path: the
/// keystroke loop walks one descent and returns Bound.
fn keymap_trie_lookup_single(c: &mut Criterion) {
    let trie = populate_trie();
    let path = vec![KeyChord::char('j')];
    c.bench_function("keymap_trie_lookup_single", |b| {
        b.iter(|| {
            let r = trie.lookup(black_box(&path));
            black_box(r);
        });
    });
}

/// Lookup a two-chord binding (`gd`). Two descents.
fn keymap_trie_lookup_two_chord(c: &mut Criterion) {
    let trie = populate_trie();
    let path = vec![KeyChord::char('g'), KeyChord::char('d')];
    c.bench_function("keymap_trie_lookup_two_chord", |b| {
        b.iter(|| {
            let r = trie.lookup(black_box(&path));
            black_box(r);
        });
    });
}

/// Lookup a three-chord binding (`diw`). Three descents.
fn keymap_trie_lookup_three_chord(c: &mut Criterion) {
    let trie = populate_trie();
    let path = vec![
        KeyChord::char('d'),
        KeyChord::char('i'),
        KeyChord::char('w'),
    ];
    c.bench_function("keymap_trie_lookup_three_chord", |b| {
        b.iter(|| {
            let r = trie.lookup(black_box(&path));
            black_box(r);
        });
    });
}

/// Lookup that ends at a Partial (`g` -- wait for second).
/// Walks one descent; the input ends at an internal node.
fn keymap_trie_lookup_partial(c: &mut Criterion) {
    let trie = populate_trie();
    let path = vec![KeyChord::char('g')];
    c.bench_function("keymap_trie_lookup_partial", |b| {
        b.iter(|| {
            let r = trie.lookup(black_box(&path));
            black_box(r);
        });
    });
}

/// Lookup that ends at Unbound (`q` -- not in this trie).
/// Worst-case dispatch shape: HashMap miss at root, return.
fn keymap_trie_lookup_unbound(c: &mut Criterion) {
    let trie = populate_trie();
    let path = vec![KeyChord::char('q')];
    c.bench_function("keymap_trie_lookup_unbound", |b| {
        b.iter(|| {
            let r = trie.lookup(black_box(&path));
            black_box(r);
        });
    });
}

/// Lookup that crosses a wildcard (`f x` -> bound, captured
/// `['x']`). Branches to the `char_wildcard` slot after the
/// exact-match miss + allocates the small `Vec<char>` of
/// captures.
fn keymap_trie_lookup_wildcard(c: &mut Criterion) {
    let trie = populate_trie();
    let path = vec![KeyChord::char('f'), KeyChord::char('x')];
    c.bench_function("keymap_trie_lookup_wildcard", |b| {
        b.iter(|| {
            let r = trie.lookup(black_box(&path));
            // black_box the result variant so the optimizer
            // doesn't elide the captured-vec construction.
            match r {
                LookupResult::Bound { captured, .. } => {
                    black_box(captured);
                }
                _ => unreachable!("populated trie has wildcard f"),
            }
        });
    });
}

/// `merge_over` cost for a base trie + a small overlay.
/// Models the registry's layer-stack collapse on minor-mode
/// push -- happens off the keystroke path, but should stay
/// sub-millisecond.
fn keymap_trie_merge_overlay(c: &mut Criterion) {
    let base = populate_trie();
    let mut over = KeymapTrie::new();
    let bound = Arc::new(BoundCommand::from_invocation(
        CommandInvocation::of(CommandId::new(0)),
        SourceLocation::synthetic("bench-overlay"),
        KeymapLayer::User,
    ));
    let lit = |c: char| ChordPattern::Literal(KeyChord::char(c));
    over.insert(&[lit('d'), lit('d')], Arc::clone(&bound));
    over.insert(&[lit('y'), lit('y')], Arc::clone(&bound));
    c.bench_function("keymap_trie_merge_overlay", |b| {
        b.iter(|| {
            let mut merged = base.clone();
            merged.merge_over(black_box(&over));
            black_box(merged);
        });
    });
}

// ---------------------------------------------------------------
// KeymapRegistry benches (audit slice 8.c)
//
// End-to-end keystroke path: `ArcSwap::load` + per-mode trie
// lookup. This is the actual cost the input dispatcher will
// pay on every keypress -- everything below is a sub-step
// already benched separately.
// ---------------------------------------------------------------

fn populated_handle() -> KeymapHandle {
    let h = KeymapHandle::new();
    let lit = |c: char| ChordPattern::Literal(KeyChord::char(c));
    let invocation = || CommandInvocation::of(CommandId::new(0));
    let src = || SourceLocation::synthetic("bench");
    // Spread bindings across modes the way a real boot does:
    // some Normal, some Visual, some Insert. ~16 entries per
    // mode is below the eventual ~70 in real Normal but enough
    // to exercise HashMap lookup + ArcSwap load realistically.
    for c in [
        'j', 'k', 'h', 'l', 'w', 'b', 'e', 'g', 'd', 'y', 'c', 'p', 'P', 'u', 'r',
    ] {
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Normal,
            &[lit(c)],
            invocation(),
            src(),
        );
    }
    h.bind(
        KeymapLayer::Builtin,
        BindingMode::Normal,
        &[lit('g'), lit('d')],
        invocation(),
        src(),
    );
    h.bind(
        KeymapLayer::Builtin,
        BindingMode::Normal,
        &[lit('g'), lit('g')],
        invocation(),
        src(),
    );
    h.bind(
        KeymapLayer::Builtin,
        BindingMode::Normal,
        &[lit('d'), lit('i'), lit('w')],
        invocation(),
        src(),
    );
    for c in ['j', 'k', 'h', 'l', 'd', 'y', 'c', 'v'] {
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Visual,
            &[lit(c)],
            invocation(),
            src(),
        );
    }
    for c in ['<', '>'] {
        h.bind(
            KeymapLayer::Builtin,
            BindingMode::Insert,
            &[lit(c)],
            invocation(),
            src(),
        );
    }
    h
}

/// Hot path. Single-chord lookup through the registry handle:
/// `ArcSwap::load` + per-mode `HashMap::get` + the trie walk
/// from slice 8.b. The number a real keystroke pays.
fn keymap_handle_lookup_single(c: &mut Criterion) {
    let h = populated_handle();
    let path = vec![KeyChord::char('j')];
    c.bench_function("keymap_handle_lookup_single", |b| {
        b.iter(|| {
            let r = h.lookup(BindingMode::Normal, black_box(&path));
            black_box(r);
        });
    });
}

/// Hot path. Two-chord lookup through the handle (`gd`).
fn keymap_handle_lookup_two_chord(c: &mut Criterion) {
    let h = populated_handle();
    let path = vec![KeyChord::char('g'), KeyChord::char('d')];
    c.bench_function("keymap_handle_lookup_two_chord", |b| {
        b.iter(|| {
            let r = h.lookup(BindingMode::Normal, black_box(&path));
            black_box(r);
        });
    });
}

/// Hot path. Three-chord lookup through the handle (`diw`)
/// -- the deepest realistic lookup. End-to-end keystroke
/// path including ArcSwap load.
fn keymap_handle_lookup_three_chord(c: &mut Criterion) {
    let h = populated_handle();
    let path = vec![
        KeyChord::char('d'),
        KeyChord::char('i'),
        KeyChord::char('w'),
    ];
    c.bench_function("keymap_handle_lookup_three_chord", |b| {
        b.iter(|| {
            let r = h.lookup(BindingMode::Normal, black_box(&path));
            black_box(r);
        });
    });
}

// ---------------------------------------------------------------
// Slice 8.i.4 dispatcher end-to-end benches
//
// Slices 8.i.0-8.i.4 retired the per-`Pending` `match` body in
// `compute_normal_action` in favour of a `partial_chord` stack
// + trie lookup driven by the catalog's chord notation. The
// trie-lookup half is already covered above; these benches
// pin the **full `translate()` round-trip** -- i.e. the cost a
// real keystroke pays through the App's input thread, including
// the ArcSwap load, the per-mode dispatch fan-out, and any
// `AbsorbPartialChord` / `AbsorbOperatorPrefix` short-circuit
// that the new dispatch shape introduces. If a future slice
// regresses dispatch latency, these rows surface it before the
// trie-only rows do.
// ---------------------------------------------------------------

/// Build a `KeymapHandle` populated by the production
/// `register_*_bindings` calls -- same wiring `App::default`
/// runs at boot. Used by the `translate_*` benches below so
/// they exercise the actual catalog, not a synthetic stand-in.
fn production_keymap() -> (
    KeymapHandle,
    lattice_grammar::builtins::Builtins,
    lattice_ui_tui::actions::ActionIds,
) {
    let mut r = lattice_grammar::CommandRegistry::new();
    let b = lattice_grammar::builtins::populate(&mut r);
    let _ex = lattice_grammar::ex_commands::populate(&mut r);
    let a = lattice_ui_tui::actions::populate(&mut r, &b);
    let h = KeymapHandle::new();
    lattice_ui_tui::keymap_replace::register_replace_bindings(&h, &a);
    lattice_ui_tui::keymap_visual::register_visual_bindings(&h, &b, &a);
    lattice_ui_tui::keymap_insert::register_insert_bindings(&h, &a);
    lattice_ui_tui::keymap_normal::register_normal_bindings(&h, &b, &a);
    (h, b, a)
}

fn translate_ctx<'a>(
    keymap: &'a KeymapHandle,
    builtins: &'a lattice_grammar::builtins::Builtins,
    partial_chord: &'a [KeyChord],
) -> TranslateContext<'a> {
    TranslateContext {
        modal: lattice_grammar::ModalState::Normal,
        builtins,
        pending_count: 0,
        op_count: 0,
        recording_macro: false,
        active_buffer: BufferKind::Document,
        completion_open: false,
        chord_capture: false,
        picker_open: false,
        insert_completion_open: false,
        snippet_active: false,
        terminal_insert_active: false,
        terminal_esc_exits: false,
        terminal_app_cursor_keys: false,
        terminal_insert_exit_pending: false,
            terminal_visual_active: false,
        keymap,
        partial_chord,
    }
}

/// Hot path. Full `translate()` for the second key of a
/// two-chord sequence (`g` already on the partial stack;
/// the bench resolves `gd`). Exercises the post-8.i.4
/// `partial_chord` dispatch shape: trie lookup with a
/// non-empty prefix, returning a resolved `Action::Invoke`
/// (or `AbsorbPartialChord` for an unbound second key).
fn dispatch_translate_full_two_chord(c: &mut Criterion) {
    let (h, b, _) = production_keymap();
    let partial: [KeyChord; 1] = [KeyChord::char('g')];
    let event = ev(KeyCode::Char('d'), KeyModifiers::NONE);
    c.bench_function("dispatch_translate_full_two_chord", |bench| {
        bench.iter(|| {
            let action = translate(translate_ctx(&h, &b, black_box(&partial)), black_box(event));
            black_box(action);
        });
    });
}

/// Hot path. Full `translate()` for the motion key of an
/// operator-motion (`d` already on the partial stack; the
/// bench resolves `dw`). Exercises the operator-prefix
/// dispatch shape: trie lookup with `d` prefix returning a
/// resolved invocation that latches via the operator-side
/// `op_count` flow that 8.i.4.c rebuilt.
fn dispatch_translate_full_operator_motion(c: &mut Criterion) {
    let (h, b, _) = production_keymap();
    let partial: [KeyChord; 1] = [KeyChord::char('d')];
    let event = ev(KeyCode::Char('w'), KeyModifiers::NONE);
    c.bench_function("dispatch_translate_full_operator_motion", |bench| {
        bench.iter(|| {
            let action = translate(translate_ctx(&h, &b, black_box(&partial)), black_box(event));
            black_box(action);
        });
    });
}

criterion_group!(
    benches,
    keychord_from_event_plain_letter,
    keychord_from_event_ctrl_letter,
    keychord_from_event_back_tab,
    keychord_to_string_plain_letter,
    keychord_to_string_ctrl_shift_letter,
    keychord_parse_plain_letter,
    keychord_parse_modifier_special,
    parse_chord_sequence_multi_key,
    parse_chord_sequence_two_letters,
    keymap_trie_lookup_single,
    keymap_trie_lookup_two_chord,
    keymap_trie_lookup_three_chord,
    keymap_trie_lookup_partial,
    keymap_trie_lookup_unbound,
    keymap_trie_lookup_wildcard,
    keymap_trie_merge_overlay,
    keymap_handle_lookup_single,
    keymap_handle_lookup_two_chord,
    keymap_handle_lookup_three_chord,
    dispatch_translate_full_two_chord,
    dispatch_translate_full_operator_motion,
);
criterion_main!(benches);
