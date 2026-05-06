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

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

use lattice_ui_tui::chord::{KeyChord, parse_chord_sequence};

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
            let chord = KeyChord::from_event(black_box(&event));
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
            let chord = KeyChord::from_event(black_box(&event));
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
            let chord = KeyChord::from_event(black_box(&event));
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
    let chord = KeyChord::from_event(&ev(
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
);
criterion_main!(benches);
