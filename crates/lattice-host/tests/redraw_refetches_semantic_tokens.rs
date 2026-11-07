//! `<C-l>` / `:redraw` refetches a buffer's semantic tokens, in full.
//!
//! `<C-l>` is the escape hatch for a display that has gone wrong, and it
//! already re-derives syntax, decorations and every pane's matrix from a
//! clean slate. Semantic tokens were the one layer it left alone: they are
//! cached against the text version, and a redraw moves no version, so
//! wrong semantic colours — the keyword colours inside comments that a
//! request overtaking its edit used to produce — survived the key pressed
//! to fix them.
//!
//! The refetch is a full one, not a delta: a delta is spliced onto the
//! cached tokens, so it would carry whatever was wrong with them forward.
//! The old tokens keep rendering until the new ones land, so a redraw of a
//! correct buffer changes nothing on screen.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_host::per_buffer_cache::PerBufferCacheExt;
use lattice_lsp::cache::LspSemanticTokensCache;

fn token() -> lattice_lsp::lsp_types::SemanticToken {
    lattice_lsp::lsp_types::SemanticToken {
        delta_line: 0,
        delta_start: 0,
        length: 2,
        token_type: 0,
        token_modifiers_bitset: 0,
    }
}

fn cached_tokens(editor: &Editor) -> LspSemanticTokensCache {
    LspSemanticTokensCache {
        // Current, so an ordinary tick would see "already fetched".
        document_version: editor.document.snapshot().text_version,
        result_id: Some("7".to_string()),
        raw_data: vec![token()],
        tokens: Vec::new(),
    }
}

#[tokio::test]
async fn redraw_marks_the_buffer_for_a_full_semantic_token_refetch() {
    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let buffer = editor.document_buffer_id;
    let tokens = cached_tokens(&editor);
    editor.lsp_semantic_tokens_cache.insert_for(buffer, tokens);
    assert!(!editor.semantic_tokens_refresh_pending.contains(&buffer));

    editor.do_redraw_screen();

    assert!(
        editor.semantic_tokens_refresh_pending.contains(&buffer),
        "the next tick must re-request tokens even though the text did not change"
    );
    let cache = editor
        .lsp_semantic_tokens_cache
        .get_for(buffer)
        .expect("the old tokens stay until the new ones land — no flash");
    assert_eq!(
        cache.result_id, None,
        "without a result id to diff from, the refetch is full, not a delta"
    );
    assert_eq!(cache.raw_data, vec![token()]);
}

/// A buffer with no tokens cached has nothing wrong to replace; the redraw
/// marks it all the same, and creates no cache entry for it.
#[tokio::test]
async fn redraw_of_a_buffer_without_tokens_creates_no_cache_entry() {
    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let buffer = editor.document_buffer_id;

    editor.do_redraw_screen();

    assert!(editor.semantic_tokens_refresh_pending.contains(&buffer));
    assert!(editor.lsp_semantic_tokens_cache.get_for(buffer).is_none());
}
