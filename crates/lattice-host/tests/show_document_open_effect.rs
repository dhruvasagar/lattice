//! BC.8c — host-applied `window/showDocument` open effects.
//!
//! `window/showDocument` arrives server-initiated, off-keystroke: it drains
//! through the generic inbound tick-callback, where peer-applied effects
//! (`OpenBuffer` / `OpenBufferAt`) are NOT forwarded to the renderer peer. So
//! the show-document opens are modelled as **host-applied** effects
//! ([`Effect::OpenBufferAtColumn`], [`Effect::OpenExternalUri`]) whose bodies
//! run in `Editor::handle_effect` (calling `do_edit` host-side, exactly as the
//! retired `drain_inbound_show_documents` did).
//!
//! These pins prove the host-applied path actually opens the buffer AND that
//! the UTF-16 column the handler couldn't convert pre-open is resolved to a
//! byte offset against the *opened* line — the byte-vs-column distinction that
//! only matters on a line with a multi-byte char before the column.

#![allow(clippy::unwrap_used)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::{Effect, Utf16Pos};
use lattice_host::editor::Editor;
use lattice_protocol::Position;

/// A `file://` showDocument with a selection: the host-applied
/// `OpenBufferAtColumn` opens the file AND converts the UTF-16 column to a
/// byte offset against the opened line.
///
/// The line `let café = 1;` puts `é` (U+00E9 — **2** UTF-8 bytes, **1** UTF-16
/// code unit) before the `=`. The server (UTF-16) points at the `=` as column
/// **9**; Lattice needs byte **10**. Landing on byte 10 proves the conversion
/// ran post-open (a no-op pass-through would have left the cursor at byte 9,
/// one short, on the space).
#[test]
fn open_buffer_at_column_opens_and_converts_utf16_to_byte() {
    let path = std::env::temp_dir().join("lattice-bc8c-open-at-column.rs");
    std::fs::write(&path, "let café = 1;\n").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let _outcome = editor.handle_effect(Effect::OpenBufferAtColumn {
        path: Some(path.clone()),
        // The `=` sits at UTF-16 column 9 on the café line.
        column: Some(Utf16Pos { line: 0, col: 9 }),
        force: false,
    });

    // The open swapped the active doc to the requested file.
    assert_eq!(editor.document.snapshot().path(), Some(path.as_path()));
    // UTF-16 column 9 → byte 10 (`é` consumed an extra byte): the `=`.
    assert_eq!(editor.cursor, Position { line: 0, byte: 10 });

    let _ = std::fs::remove_file(&path);
}

/// A `file://` showDocument with no selection: `column = None` opens only,
/// without forcing the cursor to a column (the no-selection case).
#[test]
fn open_buffer_at_column_none_opens_without_forcing_cursor() {
    let path = std::env::temp_dir().join("lattice-bc8c-open-no-column.rs");
    std::fs::write(&path, "fn main() {}\n").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    editor.handle_effect(Effect::OpenBufferAtColumn {
        path: Some(path.clone()),
        column: None,
        force: false,
    });

    assert_eq!(editor.document.snapshot().path(), Some(path.as_path()));

    let _ = std::fs::remove_file(&path);
}

// NB: the `Effect::OpenExternalUri` arm is intentionally NOT exercised here —
// it spawns a real OS handler (`xdg-open` / `open` / `explorer`), which would
// be a side-effecting, flaky test (could pop a browser; differs per host). Its
// request → effect mapping is pinned in `lattice-lsp`'s show_document handler
// tests; the spawn itself is a thin fire-and-forget OS call.

/// I3 / BC.8c follow-up: `Effect::SaveBuffer` is HOST-applied in `handle_effect`
/// (reusing the existing `Editor::do_write`, joining `BufferDelete`), so
/// claude-code's `saveDocument` actually saves on the off-keystroke inbound
/// tick path — where peer-applied effects are discarded. Proof: emitting
/// `SaveBuffer` with a target path writes the active buffer's content to disk
/// with no renderer peer involved.
#[test]
fn save_buffer_is_host_applied_and_writes_to_disk() {
    let path = std::env::temp_dir().join("lattice-savebuffer-host-applied.txt");
    let _ = std::fs::remove_file(&path);

    let mut editor = Editor::boot(CoreDocument::from_text("agent wrote this\n"));
    // `:w <path>` semantics — save the active (scratch) buffer to `path`.
    editor.handle_effect(Effect::SaveBuffer {
        path: Some(path.clone()),
    });

    let on_disk =
        std::fs::read_to_string(&path).expect("SaveBuffer wrote the file host-side (no peer)");
    assert_eq!(on_disk, "agent wrote this\n");

    let _ = std::fs::remove_file(&path);
}
