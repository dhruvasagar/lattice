#![allow(clippy::unwrap_used, clippy::panic)]
//! CB.5 (`docs/dev/architecture/clipboard.md`): yank-path cost bench.
//!
//! `store_yank` runs on the keystroke path (dispatch of `y` / `yy` /
//! Visual `y`), and under `clipboard=true` (the default) it also mirrors to
//! the system clipboard. This bench guards paramount #1: the clipboard
//! mirror must add only a cheap, constant cost to the yank path — an
//! option read + a fire-and-forget `Clipboard::write` — never work
//! proportional to the document or anything that would push a keystroke
//! past a frame.
//!
//! It times `store_yank` in three configurations against the same payload:
//!
//!   - `clipboard=false`     — pure register write (no mirror).
//!   - `clipboard=true`      — register write + mirror to the default
//!     `FakeClipboard` (instant, in-memory).
//!   - `"+`-register yank    — always mirrors regardless of the option.
//!
//! The three rows should sit within noise of each other: the mirror is a
//! `services.get::<ClipboardHandle>()` + `write`, both O(1).
//!
//! **What this does NOT bench (by construction, not omission):** that a
//! *real* backend's `write` is non-blocking. That property lives in the
//! impl — `ArboardClipboard::write` (`lattice-host/src/clipboard.rs`)
//! spawns onto the shared runtime and returns immediately — not in the
//! trait, so it can't be measured through the `Clipboard` seam with the
//! synchronous `FakeClipboard`. It's guaranteed by the `spawn_task` shape
//! and the CB.2/CB.4 bounded-read timeout, reviewed at those sites.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench clipboard_yank

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_core::Document;
use lattice_grammar::YankKind;
use lattice_grammar::register::Register;
use lattice_host::editor::Editor;

fn boot(clipboard_on: bool) -> Editor {
    let editor = Editor::boot(Document::from_text("payload line for yank\n"));
    editor
        .config
        .parse_and_set_command(if clipboard_on {
            "clipboard=true"
        } else {
            "clipboard=false"
        })
        .expect("clipboard is a registered bool option");
    editor
}

fn bench_store_yank(c: &mut Criterion) {
    let content = "the quick brown fox jumps over the lazy dog".to_string();

    let mut group = c.benchmark_group("clipboard_store_yank");

    group.bench_function("register_only_clipboard_off", |b| {
        let mut editor = boot(false);
        b.iter(|| {
            editor.store_yank(
                black_box(Register::Unnamed),
                black_box(content.clone()),
                YankKind::Charwise,
                true,
            );
        });
    });

    group.bench_function("mirror_clipboard_on", |b| {
        let mut editor = boot(true);
        b.iter(|| {
            editor.store_yank(
                black_box(Register::Unnamed),
                black_box(content.clone()),
                YankKind::Charwise,
                true,
            );
        });
    });

    group.bench_function("system_register_always_mirrors", |b| {
        let mut editor = boot(false);
        b.iter(|| {
            editor.store_yank(
                black_box(Register::System),
                black_box(content.clone()),
                YankKind::Charwise,
                true,
            );
        });
    });

    group.finish();
}

criterion_group!(benches, bench_store_yank);
criterion_main!(benches);
