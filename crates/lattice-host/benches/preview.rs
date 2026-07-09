#![allow(clippy::unwrap_used, clippy::panic)]
//! Preview-isolation (PI) per-move bench.
//!
//! Measures the cost of a picker preview *selection move* under the
//! isolated-projection model (PI.3) against the old swap-and-restore
//! baseline it replaced:
//!
//! - `preview_reseat_same_buffer` — the `gr` / grep hot case: several
//!   hits in one file. Moving the selection just re-seats the pane
//!   override (`set_preview_override`) — no mode work, no option
//!   recompute. This should be the cheapest row (near-O(1)).
//!
//! - `preview_enter_exit` — mount a preview of buffer B into the active
//!   pane, then unmount it. `mount_preview` activates `preview-mode` on
//!   B's OWN stack (recomputing only `resolved_options[B]`, never the
//!   global `option_cache`); `unmount_preview` strips it. No global
//!   cache rebuild, no re-activation of the origin.
//!
//! - `activate_swap_baseline` — the pre-PI cost: `activate_buffer(B)`
//!   then `activate_buffer(A)`. Each activation runs the major-mode
//!   lifecycle + a FULL global `option_cache` rebuild + cursor/scroll
//!   reset. This is what preview used to pay on every selection move,
//!   twice (swap + restore). The two projection rows above should be a
//!   fraction of this.
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench preview
//!
//! Results are recorded in `docs/dev/operations/benchmarks.md` (PI).

use std::sync::Arc;

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use lattice_core::{BufferFlags, BufferId, Document};
use lattice_host::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
use lattice_host::editor::Editor;
use lattice_host::preview::PreviewOverride;
use lattice_protocol::position::Position;

/// Boot an editor on buffer A and add a second document buffer B (a
/// distinct rust-flavoured file) to the registry without activating it.
fn editor_with_two_buffers() -> (Editor, BufferId, BufferId) {
    let e = Editor::boot(Document::from_text("a-line-0\na-line-1\na-line-2\n"));
    let a = e.document_buffer_id;
    let b = BufferId(90_001);
    let handle = lattice_runtime::spawn_document(
        b,
        Document::from_text("fn main() {\n    let x = 1;\n    println!(\"{x}\");\n}\n"),
        e.registry.clone(),
    );
    let arc: Arc<dyn lattice_runtime::Document> = Arc::new(handle);
    e.buffers.insert(BufferEntry {
        id: b,
        flags: BufferFlags {
            listed: true,
            hidden: false,
            ephemeral: false,
        },
        data: BufferData::Document(DocumentEntry {
            id: b,
            handle: Arc::clone(&arc),
        }),
        name: Some("*B.rs*".to_string()),
    });
    (e, a, b)
}

fn bench_preview(c: &mut Criterion) {
    c.bench_function("preview_reseat_same_buffer", |bencher| {
        let (mut e, _a, b) = editor_with_two_buffers();
        let pane = e.pane_tree.active().id;
        // Seat an initial preview of B; the measured op is a subsequent
        // move to a NEW line in the same buffer (the `gr`/grep case).
        let _ = e.preview_in_active_pane(b, Some(0));
        let mut line = 0u32;
        bencher.iter(|| {
            line = line.wrapping_add(1) % 3;
            e.set_preview_override(
                pane,
                PreviewOverride {
                    buffer_id: b,
                    buffer: lattice_core::BufferKind::Document,
                    cursor: Position::new(line, 0),
                    scroll: line,
                },
            );
            black_box(e.preview_override_for(pane));
        });
    });

    c.bench_function("preview_enter_exit", |bencher| {
        let (mut e, _a, b) = editor_with_two_buffers();
        let pane = e.pane_tree.active().id;
        bencher.iter(|| {
            black_box(e.mount_preview(pane, b, Position::ZERO, 0));
            black_box(e.unmount_preview(pane));
        });
    });

    c.bench_function("activate_swap_baseline", |bencher| {
        let (mut e, a, b) = editor_with_two_buffers();
        bencher.iter(|| {
            // The pre-PI swap-and-restore: activate B, then restore A.
            black_box(e.activate_buffer(b));
            black_box(e.activate_buffer(a));
        });
    });
}

criterion_group!(benches, bench_preview);
criterion_main!(benches);
