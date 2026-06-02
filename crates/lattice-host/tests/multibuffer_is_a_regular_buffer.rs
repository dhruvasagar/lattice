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
    assert!(partial.is_empty(), "second `g` should resolve `gg` and clear");
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
        matches!(
            editor.modal,
            lattice_grammar::ModalState::Visual(_)
        ),
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

    // After create_multibuffer_view, the header provider
    // should be registered against view_id. Confirm via the
    // public snapshot API.
    let providers = editor.virtual_row_providers.snapshot(view_id);
    assert_eq!(
        providers.len(),
        1,
        "create_multibuffer_view should have registered \
         exactly one virtual-row provider (the multibuffer \
         header provider) — got {}",
        providers.len()
    );

    // The provider's collect() emits one VirtualRow per excerpt.
    // The test's synthetic multibuffer has two excerpts so
    // exactly two header rows should land in the matrix.
    let rows = providers[0].collect();
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

#[test]
#[ignore = "K.4.7 dependency — per-excerpt syntax highlighting not yet wired"]
fn syntax_highlights_per_excerpt_use_source_language() {
    // K.4.7 contract: an excerpt sourced from a `.rs` buffer
    // should carry rust tree-sitter spans on its rows; an
    // excerpt from a `.md` buffer should carry markdown spans.
    // Today every row in a multibuffer view falls back to
    // `Lang::Plain` and renders unstyled because the composed
    // snapshot's filename is `*test:multibuffer*` which
    // detects as Plain.
    panic!("K.4.7 unimplemented — per-excerpt syntax not in place");
}
