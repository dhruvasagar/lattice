//! MG.18c bench: resolving the hunk under the cursor.
//!
//! One question, asked twice: **does the cost of `s` depend on how big
//! the buffer is?** It must not. `magit_hunk_at_*` places the cursor
//! the same distance below its `@@` header in a 200-line diff and in a
//! 50,000-line one; the two numbers should be within noise of each
//! other, because the parser reads lines through an accessor and stops
//! at the hunk's declared counts.
//!
//! The shape this guards against is the obvious "simplification":
//! collecting the buffer into a `Vec<String>` and slicing it. That
//! version is shorter, passes every correctness test, and turns one
//! keypress in a large `*magit:diff*` into an O(document) copy on the
//! actor thread — paramount goal #1, visible to the user as the editor
//! going quiet after `s`.
//!
//! Runs on the actor thread in production (an action handler), not the
//! UI thread, so the bar is "imperceptible per chord press", not the
//! frame budget.

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use lattice_magit::hunk::hunk_at_with;

/// A diff of `files` files, each with one three-line hunk. The first
/// hunk always sits at the same rows, so cursor distance-to-header is
/// constant and the only variable is how much buffer lies below.
fn diff_with(files: usize) -> Vec<String> {
    let mut lines = Vec::with_capacity(files * 8);
    for i in 0..files {
        lines.push(format!("diff --git a/f{i}.rs b/f{i}.rs"));
        lines.push("index 1111111..2222222 100644".to_string());
        lines.push(format!("--- a/f{i}.rs"));
        lines.push(format!("+++ b/f{i}.rs"));
        lines.push("@@ -1,2 +1,2 @@".to_string());
        lines.push(" fn main() {".to_string());
        lines.push("-    old();".to_string());
        lines.push("+    new();".to_string());
    }
    lines
}

fn bench_hunk_at(c: &mut Criterion) {
    // Cursor on the `+` line of the FIRST hunk in both cases.
    const CURSOR: usize = 7;

    for (label, files) in [("small", 25usize), ("large", 6_250usize)] {
        let lines = diff_with(files);
        // 25 files → 200 lines; 6250 → 50,000.
        let name = format!("magit_hunk_at_{label}_{}_lines", lines.len());
        c.bench_function(&name, |b| {
            b.iter(|| {
                let patch = hunk_at_with(|i| lines.get(i).cloned(), black_box(CURSOR))
                    .expect("cursor is inside the first hunk");
                black_box(patch.to_patch())
            })
        });
    }

    // The cursor at the END of a large buffer: the backward scan to the
    // file header is the only distance that legitimately grows, and it
    // is bounded by ONE file's diff, not by the document.
    let lines = diff_with(6_250);
    let cursor = lines.len() - 1;
    c.bench_function("magit_hunk_at_last_hunk_of_50000_lines", |b| {
        b.iter(|| {
            let patch = hunk_at_with(|i| lines.get(i).cloned(), black_box(cursor))
                .expect("cursor is inside the last hunk");
            black_box(patch.to_patch())
        })
    });
}

criterion_group!(benches, bench_hunk_at);
criterion_main!(benches);
