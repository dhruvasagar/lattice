//! K.4.1.a — Multibuffer-is-a-regular-buffer integration tests
//! (foundation slice).
//!
//! Drives a real `Editor::boot` through the
//! `create_multibuffer_view` + `activate_document` path against
//! a synthetic two-excerpt multibuffer view. The 35-seam K.4.0
//! audit identified that the existing inline lib tests + the
//! `MockActivator`-based `m2b2_integration.rs` cover
//! `create_multibuffer_view`'s registration contract but NOT
//! the end-to-end "view exists in an Editor, becomes active,
//! cursor/active_buffer state reflects the switch" pipeline —
//! which is where K.4.2 / K.4.3 / K.4.4 silently regressed
//! before user testing surfaced them.
//!
//! ## What this slice covers (and what's deferred)
//!
//! **Covered (K.4.1.a, this commit):**
//!
//! - Boot a real `Editor`, create a synthetic two-excerpt
//!   multibuffer view, `activate_document` it. Assert
//!   `active_buffer == BufferKind::Multibuffer` and the
//!   view's BufferId is reachable through the buffer
//!   registry.
//! - K.4.5 / K.4.6 / K.4.7 dependency markers as `#[ignore]`'d
//!   tests that document the contract each upcoming slice
//!   will satisfy.
//!
//! **K.4.1.b — chord-dispatch tests:** the `Editor::dispatch_chord`
//! public API (added 2026-06-02) closes the verification gap.
//! Motion (`j`, `k`, `gg`, `G`, `w`), visual mode (`v`), and the
//! partial-chord lifecycle are now exercised end-to-end on a
//! multibuffer view. The API builds a `TranslateContext` from
//! Editor state, calls host `translate`, manages `partial_chord`,
//! and routes through `handle_action` — same pipeline the TUI's
//! input layer uses, minus App-only surface state (picker
//! overlay, completion popup, snippet, terminal) which defaults
//! to "not active" for programmatic dispatch.

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, BufferKind, Document as CoreDocument};
use lattice_grammar::CommandRegistry;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_multibuffer::{Excerpt, create_multibuffer_view};
use lattice_runtime::spawn_document;

// ─────────────────────────────────────────────────────────────
//  Test scaffold
// ─────────────────────────────────────────────────────────────

/// Boot an Editor with one synthetic multibuffer view layered
/// on top. The view spans two source documents
/// (`source_a` and `source_b`), each with 4 short rows, mapped
/// to two excerpts (one per source). Returns the editor + the
/// multibuffer's BufferId.
///
/// Matches the shape K.4.6 / K.4.7 need to exercise (two
/// distinct source buffers → two excerpts → file-boundary
/// motions between them).
fn boot_with_multibuffer() -> (Editor, BufferId) {
    // Boot with a scratch document — the multibuffer is added
    // separately. Editor::boot is synchronous; the shared
    // runtime it acquires handles every spawn_document call
    // below.
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));

    // Build two source documents we'll embed into the
    // multibuffer view.
    let cmd_registry: Arc<CommandRegistry> = editor.registry.clone();
    let source_a = spawn_document(
        BufferId(101),
        CoreDocument::from_text("a-line-0\na-line-1\na-line-2\na-line-3\n"),
        cmd_registry.clone(),
    );
    let source_b = spawn_document(
        BufferId(102),
        CoreDocument::from_text("b-line-0\nb-line-1\nb-line-2\nb-line-3\n"),
        cmd_registry,
    );

    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(
        BufferId(101),
        Arc::new(source_a) as Arc<dyn lattice_runtime::Document>,
    );
    sources.insert(
        BufferId(102),
        Arc::new(source_b) as Arc<dyn lattice_runtime::Document>,
    );

    let excerpts = vec![
        Excerpt::new(BufferId(101), 0, 4),
        Excerpt::new(BufferId(102), 0, 4),
    ];

    let registry_for_view = editor.registry.clone();
    let view_id = create_multibuffer_view(
        &mut editor,
        sources,
        excerpts,
        Some("*test:multibuffer*".into()),
        BufferFlags::default(),
        registry_for_view,
        None,
    );

    (editor, view_id)
}

/// Switch the active pane to the multibuffer view. Mirrors what
/// `:b <view-name>` or the search provider's open path does at
/// runtime — flips `active_buffer` to `Multibuffer`, points the
/// cursor at row 0, and triggers downstream render-state
/// republishes.
///
/// `activate_document` takes `lattice_core::BufferId` (the
/// `lattice-core`'s u32-shaped id, not the protocol's u64).
fn activate_pane(editor: &mut Editor, view_id: BufferId) {
    editor.activate_document(view_id);
}

// ─────────────────────────────────────────────────────────────
//  Foundation tests (K.4.1.a) — Editor + view + activation
// ─────────────────────────────────────────────────────────────

#[test]
fn create_multibuffer_view_returns_a_valid_buffer_id() {
    let (_editor, view_id) = boot_with_multibuffer();
    // BufferId(0) is the sentinel `create_multibuffer_view`
    // returns when BufferStoreHandle isn't registered. If we
    // see it, the boot-order audit went wrong.
    assert_ne!(
        view_id,
        BufferId(0),
        "create_multibuffer_view returned the sentinel BufferId(0) — \
         BufferStoreHandle service was not registered at boot"
    );
}

#[test]
fn opening_a_file_does_not_clobber_multibuffer_snapshot() {
    // K.4.x bug user reported 2026-06-02:
    // > "As soon as I open a file the entire buffer
    // >  (virtual rows remain) gets replaced"
    //
    // Repro: boot Editor with a multibuffer view, snapshot its
    // composed body, simulate the user flow (split active pane,
    // then `:e <fresh_file>`), and assert the multibuffer's
    // body text is unchanged. Headers are known intact (the
    // user said "virtual rows remain"); we're testing the
    // body-text path only.
    //
    // If this test PASSES, the bug is downstream of host state
    // (renderer / TUI app picker pipeline). If it FAILS, the
    // bug is in `do_edit` / `open_fresh_into_active_slot` /
    // `activate_buffer_state` writing to the multibuffer.
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);

    // Snapshot the multibuffer's composed body BEFORE the
    // file-open. This is what the user expects to remain
    // intact.
    let mb_handle_pre = editor
        .buffers
        .document_handle(view_id)
        .expect("multibuffer is in registry");
    let composed_pre = mb_handle_pre.snapshot().buffer.as_string();
    assert!(
        composed_pre.contains("a-line-0") && composed_pre.contains("b-line-0"),
        "sanity: multibuffer should compose source content; got: {composed_pre:?}"
    );

    // User's repro: <C-w>v then open a file.
    editor.do_split_pane(lattice_core::ui::pane::SplitOrientation::Vertical);

    // Write a fresh file outside the working tree so do_edit's
    // `Document::open(path)` succeeds.
    let temp_path = std::env::temp_dir().join(format!(
        "lattice_multibuffer_clobber_test_{}.txt",
        std::process::id()
    ));
    std::fs::write(&temp_path, "FILE-LINE-0\nFILE-LINE-1\nFILE-LINE-2\n").expect("write tmp file");

    // The actual call the picker / `:e` end up making.
    let outcome = editor.do_edit(Some(temp_path.clone()), false);
    let _ = outcome;

    // After file open: assert multibuffer's body is UNCHANGED.
    // We re-fetch the handle through the registry because that's
    // what the renderer does for inactive panes.
    let mb_handle_post = editor
        .buffers
        .document_handle(view_id)
        .expect("multibuffer should still be in registry after file open");
    let composed_post = mb_handle_post.snapshot().buffer.as_string();

    // Clean up before assertion so a panic doesn't leak the tmp.
    let _ = std::fs::remove_file(&temp_path);

    assert_eq!(
        composed_post, composed_pre,
        "BUG: opening a file clobbered the multibuffer's composed body.\n\
         Pre-open composed text:\n{composed_pre}\n\
         Post-open composed text:\n{composed_post}"
    );

    // Also check that the registry's handle for view_id still
    // resolves to a Multibuffer (not a Document).
    assert_eq!(
        editor.buffers.kind_of(view_id),
        Some(BufferKind::Multibuffer),
        "BUG: multibuffer's registry entry got swapped to a different kind"
    );

    // Now check the PUBLISHED render state — what the renderer
    // actually reads from. Force a publish and re-query.
    editor.publish_render_state();
    let rs = editor.render_state.load();
    let published_kind = rs.buffers.registry.kind_of(view_id);
    assert_eq!(
        published_kind,
        Some(BufferKind::Multibuffer),
        "BUG: published render state's buffer registry lost the \
         multibuffer's BufferKind"
    );
    let published_handle = rs
        .buffers
        .registry
        .document_handle(view_id)
        .expect("multibuffer should be in published registry");
    let published_composed = published_handle.snapshot().buffer.as_string();
    assert_eq!(
        published_composed, composed_pre,
        "BUG: published render state's multibuffer handle returned \
         different body text than expected.\nPre-open:\n{composed_pre}\
         \nPost-publish:\n{published_composed}"
    );
}

#[test]
fn active_buffer_is_multibuffer_after_activation() {
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    assert_eq!(
        editor.active_buffer,
        BufferKind::Multibuffer,
        "after activate_document on a multibuffer view, \
         active_buffer must be BufferKind::Multibuffer"
    );
}

// ─────────────────────────────────────────────────────────────
//  K.4.1.b — chord-dispatch tests via Editor::dispatch_chord
// ─────────────────────────────────────────────────────────────
//
// Uses the public Editor::dispatch_chord API (added 2026-06-02)
// which builds a TranslateContext from editor state, calls
// host translate, manages the partial-chord buffer, and routes
// the resulting Action through handle_action. Same path the
// TUI's input layer uses, minus App-only surface state.

use lattice_protocol::KeyChord;

/// Dispatch a chord through the host's public API, maintaining
/// a partial-chord buffer for multi-chord sequences (gg, dw,
/// ]e, etc.).
fn dispatch_chord(editor: &mut Editor, chord: KeyChord, partial: &mut Vec<KeyChord>) {
    let _ = editor.dispatch_chord(chord, partial);
}

#[test]
fn motion_j_advances_cursor() {
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();
    let start = editor.cursor.line;
    dispatch_chord(&mut editor, KeyChord::char('j'), &mut partial);
    assert_eq!(
        editor.cursor.line,
        start + 1,
        "`j` on a multibuffer view must advance cursor.line by one \
         (cursor: {:?})",
        editor.cursor
    );
}

#[test]
fn motion_k_retreats_cursor() {
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();
    dispatch_chord(&mut editor, KeyChord::char('j'), &mut partial);
    dispatch_chord(&mut editor, KeyChord::char('j'), &mut partial);
    let before_k = editor.cursor.line;
    dispatch_chord(&mut editor, KeyChord::char('k'), &mut partial);
    assert_eq!(
        editor.cursor.line,
        before_k - 1,
        "`k` on a multibuffer view must retreat cursor.line by one"
    );
}

#[test]
fn motion_gg_jumps_to_top() {
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();
    for _ in 0..3 {
        dispatch_chord(&mut editor, KeyChord::char('j'), &mut partial);
    }
    // First `g` returns Partial → AbsorbPartialChord; second
    // `g` matches `[g, g]` → motion:goto-first-line.
    dispatch_chord(&mut editor, KeyChord::char('g'), &mut partial);
    assert_eq!(partial.len(), 1, "first `g` should absorb into partial");
    dispatch_chord(&mut editor, KeyChord::char('g'), &mut partial);
    assert!(
        partial.is_empty(),
        "second `g` should resolve `gg` and clear"
    );
    assert_eq!(
        editor.cursor.line, 0,
        "`gg` on a multibuffer view must land cursor.line at 0"
    );
}

#[test]
fn motion_capital_g_jumps_to_bottom() {
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();
    dispatch_chord(&mut editor, KeyChord::char('G'), &mut partial);
    assert!(
        editor.cursor.line > 0,
        "`G` on a multibuffer view must advance cursor past 0 \
         (cursor: {:?})",
        editor.cursor
    );
}

#[test]
fn motion_w_advances_word() {
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();
    let start_byte = editor.cursor.byte;
    let start_line = editor.cursor.line;
    dispatch_chord(&mut editor, KeyChord::char('w'), &mut partial);
    assert!(
        editor.cursor.byte != start_byte || editor.cursor.line != start_line,
        "`w` must advance the cursor (start byte {start_byte}, line \
         {start_line}; current cursor {:?})",
        editor.cursor
    );
}

#[test]
fn visual_mode_enter_works() {
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();
    dispatch_chord(&mut editor, KeyChord::char('v'), &mut partial);
    assert!(
        matches!(editor.modal, lattice_grammar::ModalState::Visual(_)),
        "pressing `v` on a multibuffer view must enter Visual \
         modal state (modal is {:?})",
        editor.modal
    );
}

#[test]
fn partial_chord_resets_on_unbound_follow_up() {
    // First `g` absorbs into partial. `!` has no `[g, !]`
    // binding in Normal, so partial must clear and cursor must
    // not move. Guards the partial-chord lifecycle on the
    // dispatch_chord path.
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();
    let start = editor.cursor;
    dispatch_chord(&mut editor, KeyChord::char('g'), &mut partial);
    assert_eq!(partial.len(), 1, "`g` should absorb");
    dispatch_chord(&mut editor, KeyChord::char('!'), &mut partial);
    assert!(partial.is_empty(), "unbound follow-up must clear partial");
    assert_eq!(editor.cursor, start, "unbound `g!` should not move cursor");
}

// ─────────────────────────────────────────────────────────────
//  K.4.5 / K.4.6 / K.4.7 dependencies (ignored markers)
// ─────────────────────────────────────────────────────────────
//
// These tests document the contract the upcoming slices will
// satisfy. Each is `#[ignore]`'d with the slice that will
// flip the attribute off. When the slice lands, the ignore
// drops and the test joins the run.
//
// Today they `panic!` so the test framework registers them
// as ignored-but-known-broken; once the K.4.1.b chord-dispatch
// helper exists, these can pull the chord-dispatch path back
// in and become real assertions.

#[test]
fn visual_selection_renders_for_multibuffer() {
    // K.4.5 (2026-06-02): after entering Visual mode +
    // extending the selection on a multibuffer view, the
    // editor's `visual_selection_range()` must return Some
    // range whose head reflects the user's motion. Before
    // K.4.5 the multibuffer's `set_selections` was a
    // ReadOnly no-op, so the snapshot's selections stayed at
    // `SelectionSet::default()` and visual_selection_range
    // returned a degenerate `(0,0)..(0,1)` range regardless
    // of the actual cursor position.
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);
    let mut partial = Vec::new();

    // Enter Visual mode at row 0.
    dispatch_chord(&mut editor, KeyChord::char('v'), &mut partial);
    // Extend the selection: 3 × `l` (right) followed by `j`
    // (down). After K.4.5 the snapshot's selection follows.
    for _ in 0..3 {
        dispatch_chord(&mut editor, KeyChord::char('l'), &mut partial);
    }
    dispatch_chord(&mut editor, KeyChord::char('j'), &mut partial);

    let range = editor
        .visual_selection_range()
        .expect("Visual mode active → visual_selection_range must be Some");
    assert!(
        range.end.line >= 1 || range.end.byte >= 3,
        "K.4.5: extended Visual on multibuffer must surface a non-degenerate range \
         (got {range:?}); pre-fix the multibuffer's set_selections no-op left this \
         pinned at the origin"
    );
}

#[test]
fn m11_insert_mode_on_multibuffer_produces_visible_text() {
    // M.11 (2026-06-02): insert-mode keystrokes on a
    // multibuffer view must land in the composed snapshot
    // synchronously — byte-identical to insert mode on a
    // regular Document. Reproduces the user's interactive bug
    // ("after going in insert mode, key presses do nothing")
    // through the full host pipeline: activate the multibuffer
    // pane, call `do_insert_text` (the exact path
    // `Action::Insert(s)` routes through at dispatch.rs:1923),
    // assert the composed snapshot reflects the char AND
    // `editor.cursor` advanced.
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);

    // Pre-insert sanity. The multibuffer's composed text is the
    // union of two 4-row excerpts (one per source); cursor sits
    // at (0, 0) — the start of source_a row 0 = "a-line-0".
    let pre_text = editor.document.snapshot().buffer.as_string();
    assert!(
        pre_text.starts_with("a-line-0"),
        "expected composed view to start with first source's content; got: {pre_text:?}"
    );
    let pre_cursor = editor.cursor;
    assert_eq!(pre_cursor.line, 0);
    assert_eq!(pre_cursor.byte, 0);

    // Insert "X" at the cursor — same code path the keystroke
    // dispatcher uses when the user types a character in
    // insert mode (do_insert_text → apply_edit_blocking →
    // self.document.apply_edit).
    let mut out = DispatchOutcome::default();
    editor.do_insert_text("X", &mut out);

    // Composed snapshot must reflect the insert immediately.
    let post_text = editor.document.snapshot().buffer.as_string();
    assert!(
        post_text.starts_with("Xa-line-0"),
        "M.11 insert must land in composed snapshot synchronously; \
         pre-fix composed view stayed pre-edit. Got: {post_text:?}"
    );

    // Cursor must have advanced by one byte.
    assert_eq!(
        editor.cursor.line, 0,
        "cursor.line should stay on row 0 after a single-char insert"
    );
    assert_eq!(
        editor.cursor.byte, 1,
        "cursor.byte must advance by 1 per char; got {:?}",
        editor.cursor
    );

    // Second insert — verify it accumulates, mirroring the
    // user's "first char landed, second didn't" symptom.
    editor.do_insert_text("Y", &mut out);
    let post_text_2 = editor.document.snapshot().buffer.as_string();
    assert!(
        post_text_2.starts_with("XYa-line-0"),
        "second insert must accumulate on top of the first; got: {post_text_2:?}"
    );
    assert_eq!(
        editor.cursor.byte, 2,
        "cursor.byte must be 2 after two char inserts; got {:?}",
        editor.cursor
    );
}

#[test]
fn virtual_row_matrix_carries_excerpt_headers() {
    // K.4.6 (2026-06-02): MultibufferHeaderProvider is
    // registered against the multibuffer's BufferId in
    // `create_multibuffer_view` via the ModeActivator trait's
    // new `register_virtual_row_provider` method. The provider
    // emits one VirtualRow per excerpt at AnchorPosition::Above
    // of the excerpt's first composed row, content = excerpt
    // header label (default rendering: `── <title> ──`).
    //
    // This test verifies the provider is registered + its
    // `collect()` output is correct. The worker's async wake +
    // publish path is not exercised here (the test runtime
    // doesn't drive the editor actor's tokio runtime); the
    // pure-function `collect()` is sufficient to verify the
    // K.4.6 contract — that excerpt headers are present in the
    // matrix the worker would publish.
    let (mut editor, view_id) = boot_with_multibuffer();
    activate_pane(&mut editor, view_id);

    // After create_multibuffer_view, the excerpt-header provider
    // should be registered against view_id. Confirm via the
    // public snapshot API.
    //
    // M.6.5 (2026-06-08): create_multibuffer_view also registers a
    // view-status headerline provider, so there are two providers
    // against the view now. Select the K.4.6 excerpt-header provider
    // by its deterministic id rather than asserting a single
    // registration.
    let providers = editor.virtual_row_providers.snapshot(view_id);
    assert_eq!(
        providers.len(),
        2,
        "create_multibuffer_view should register the excerpt-header \
         provider + the view-status headerline provider — got {}",
        providers.len()
    );
    let header_id = lattice_multibuffer::multibuffer_excerpt_header_provider_id(view_id);
    let header_provider = providers
        .iter()
        .find(|p| p.id() == header_id)
        .expect("the excerpt-header provider must be registered against the view");

    // The provider's collect() emits one VirtualRow per excerpt.
    // The test's synthetic multibuffer has two excerpts so
    // exactly two header rows should land in the matrix.
    let rows = header_provider.collect();
    assert_eq!(
        rows.len(),
        2,
        "two-excerpt multibuffer should produce two header rows \
         (got {})",
        rows.len()
    );

    // Each header row is anchored Above its excerpt's first
    // composed row. With 5-row excerpts (Excerpt::new(_, 0, 4)
    // is inclusive on both ends, so 0..=4 = 5 rows), the
    // anchors land at composed lines 0 and 5.
    use lattice_cells::AnchorPosition;
    assert_eq!(rows[0].anchor_line, 0);
    assert!(matches!(rows[0].position, AnchorPosition::Above));
    assert_eq!(rows[1].anchor_line, 5);
    assert!(matches!(rows[1].position, AnchorPosition::Above));
}

/// K.4.7: per-excerpt syntax highlighting uses the source language,
/// not the composed view's name (`*test:multibuffer*` → Plain).
///
/// Contracts tested:
/// 1. When `lang_registry` is wired, `excerpt_syntax_entries()` on the
///    handle returns one entry per source that has a detectable language.
/// 2. A `recompute_pane` call with those entries produces at least one
///    row whose `StyledSpan` list is non-empty — meaning the worker
///    applied source-language highlights, not the plain fallback.
#[test]
fn syntax_highlights_per_excerpt_use_source_language() {
    use std::sync::Arc;
    use arc_swap::ArcSwap;
    use lattice_cells::{CellMatrix, MatrixVersion, VirtualRowMatrix};
    use lattice_core::{Document as CoreDocument, DocumentBuilder};
    use lattice_host::cells_worker::recompute_pane;
    use lattice_host::render_state::{ExcerptSyntax, PaneCellsInputs};
    use lattice_multibuffer::{Excerpt, MultibufferRegistryHandle, create_multibuffer_view};
    use lattice_runtime::{Document as RtDocument, spawn_document};

    let mut editor = lattice_host::editor::Editor::boot(CoreDocument::from_text("scratch\n"));
    let lr = editor.lang_registry.clone();

    // Source A: Rust file — lang_registry will detect Lang::Rust from the path.
    let rust_text = "fn main() {\n    let x = 1;\n}\n";
    let src_a_doc = DocumentBuilder::default()
        .with_text(rust_text)
        .with_path("src/main.rs")
        .build();
    let src_a = spawn_document(BufferId(201), src_a_doc, editor.registry.clone());

    let mut sources: HashMap<BufferId, Arc<dyn lattice_runtime::Document>> = HashMap::new();
    sources.insert(
        BufferId(201),
        Arc::new(src_a) as Arc<dyn lattice_runtime::Document>,
    );

    let excerpts = vec![Excerpt::new(BufferId(201), 0, 3)];
    let registry_for_view = editor.registry.clone();

    let view_id = create_multibuffer_view(
        &mut editor,
        sources,
        excerpts,
        Some("*test:syntax-k47*".into()),
        BufferFlags::default(),
        registry_for_view,
        Some(lr),
    );

    // 1. The typed handle must report at least one excerpt syntax entry
    //    (Rust was detected → SyntaxHandle created for BufferId(201)).
    let mb_reg = editor
        .services
        .get::<MultibufferRegistryHandle>()
        .expect("MultibufferRegistryHandle must be registered");
    let mb = mb_reg
        .handle(view_id)
        .expect("multibuffer view must be in registry");

    // Give the seeded parse a moment to propagate through ArcSwap.
    std::thread::sleep(std::time::Duration::from_millis(20));

    let entries = mb.excerpt_syntax_entries();
    assert!(
        !entries.is_empty(),
        "K.4.7: excerpt_syntax_entries() is empty — SyntaxHandle was not created \
         for the Rust source. Check lang_registry wiring in add_source."
    );
    // Validate tuple field ordering: (composed_start, composed_end, source_start, handle).
    // Before the K.4.7 bug-fix the tuple was (composed_start, source_start, source_end, _),
    // which produced composed_end < composed_start and wildly wrong src_lo in the highlighter.
    let (cs0, ce0, ss0, _) = &entries[0];
    assert_eq!(*cs0, 0, "K.4.7: first excerpt must start at composed row 0");
    assert!(
        *ce0 > *cs0,
        "K.4.7: composed_end ({ce0}) must exceed composed_start ({cs0}) for a multi-line excerpt \
         — likely tuple field ordering bug in excerpt_syntax_entries()"
    );
    assert_eq!(
        *ss0, 0,
        "K.4.7: source_start must be 0 for a full-file excerpt starting at line 0 \
         (got {ss0}) — likely tuple field ordering bug in excerpt_syntax_entries()"
    );

    // 2. Build a minimal PaneCellsInputs with the excerpt handles and
    //    run recompute_pane; at least one row should have non-empty spans.
    let matrix_cell = Arc::new(ArcSwap::from_pointee(CellMatrix::empty()));
    let snap = mb.snapshot();
    let text_v = snap.text_version;
    let excerpt_syntax_arc: Arc<[ExcerptSyntax]> = entries
        .into_iter()
        .map(|(cs, ce, ss, h)| ExcerptSyntax {
            composed_start: cs,
            composed_end: ce,
            source_start: ss,
            handle: h,
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
        .into();

    let pane = PaneCellsInputs {
        pane_id: lattice_core::ui::pane::PaneId::default(),
        buffer_id: view_id,
        matrix: matrix_cell,
        display_matrix: Arc::new(ArcSwap::from_pointee(
            lattice_host::display_matrix::DisplayMatrix::empty(),
        )),
        virtual_rows_matrix: Arc::new(ArcSwap::from_pointee(VirtualRowMatrix::empty())),
        version: MatrixVersion {
            text: text_v,
            syntax: text_v,
            ..MatrixVersion::ZERO
        },
        snapshot: Some(snap),
        syntax_handle: None,
        inlay_hints: Arc::from([]),
        folds: Arc::from([]),
        viewport_height: 10,
        scroll: 0,
        viewport_width: 0,
        wrap: false,
        foldenable: false,
        last_edit: None,
        excerpt_syntax: excerpt_syntax_arc,
        extra_spans: Arc::from([]),
    };

    // T.6.t: the host `Theme` struct is gone; `recompute_pane` reads
    // syntax styles through a `CellTheme { resolved, ids }` (T.5). Build
    // one from the default theme registry — the same construction the
    // renderers use at boot.
    use lattice_host::ui::theme::ThemeRegistry as _;
    let reg = lattice_host::ui::theme::InMemoryThemeRegistry::with_defaults();
    let resolved = reg.resolved();
    let ids = lattice_host::ui::theme::BuiltinElementIds::capture(&reg);
    let ct = lattice_host::cells_worker::CellTheme {
        resolved: &resolved,
        ids: &ids,
    };
    let ws = lattice_host::cells_worker::WhitespaceConfig::default();
    recompute_pane(&pane, ct, &ws);

    let matrix = pane.matrix.load();
    let has_spans = matrix.chunks.iter().any(|chunk| {
        chunk
            .rows
            .iter()
            .any(|row| row.cells.iter().any(|c| c.fg != 0))
    });
    assert!(
        has_spans,
        "K.4.7: all rows rendered with default fg — excerpt syntax highlighting \
         did not apply. Check highlight_range_multibuffer wiring in recompute_pane."
    );
}
