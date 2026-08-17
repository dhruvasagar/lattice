//! The `document` resource backing + the `buffer-snapshot` projection
//! (plugin-host.md §4.2 / §9.6, PH7.3c).
//!
//! §4.2's borrows (`&Buffer`, `&Path`, `&str` inside `ActiveBufferSnapshot`)
//! cannot cross the WASM boundary. The host projects the *metadata* into an
//! owned [`buffer::BufferSnapshot`] record ([`project_buffer_snapshot`]) and
//! hands the guest a `document` **resource handle** for the text. Bulk rope
//! text never rides the snapshot: the guest calls `get-text-range(range)` and
//! the host slices only that range out of the rope ("zero-copy at the slice
//! level" — the bytes still cross into guest linear memory, but the whole
//! document never does).
//!
//! **Decision A (locked):** the resource is backed by an
//! `Arc<DocumentSnapshot>` — a point-in-time immutable view. Edits landing
//! after the handle is minted never shift byte ranges under the guest mid-read.
//!
//! The end-to-end guest→host call through the canonical ABI is exercised at
//! PH7.3d/PH7.4 (the call machinery + the <500ns bench live there); this slice
//! proves the design at the host layer — the resource is real (wired into the
//! linker via bindgen's generated `add_to_linker`) and its methods + projection
//! are unit-tested directly.

use std::sync::Arc;

use lattice_picker::context::ActiveBufferSnapshot;
use lattice_protocol::position::Range as NativeRange;
use lattice_runtime::snapshot::DocumentSnapshot;

use crate::WitBoundary;
use crate::boundary::path_to_wit;
use crate::lattice::plugin_host::buffer::BufferSnapshot as WitBufferSnapshot;

/// Host-side backing for the `document` WIT resource (decision A): a
/// point-in-time immutable snapshot. The bindgen `with:` mapping makes this the
/// resource representation, so the `Store`'s `ResourceTable` stores it directly
/// and [`HostDocument`] methods receive `Resource<DocumentResource>`.
pub struct DocumentResource {
    snapshot: Arc<DocumentSnapshot>,
}

impl DocumentResource {
    /// Wrap a document snapshot as a resource backing.
    pub fn new(snapshot: Arc<DocumentSnapshot>) -> Self {
        Self { snapshot }
    }

    /// The text of the `[start, end)` byte range. Slices only the requested
    /// range out of the rope; the whole document is never materialised. `Err`
    /// on an out-of-range or `end < start` range (mirrors `Buffer::slice`).
    pub fn get_text_range(&self, range: NativeRange) -> Result<String, String> {
        self.snapshot.buffer.slice(range).map_err(|e| e.to_string())
    }

    /// Lines the document has, in the sense the WIT contract means:
    /// `"a\nb\n"` is two lines.
    ///
    /// CV.3: content space. This surfaced ropey's raw count, so a
    /// guest iterating `0..line-count` and calling `line(n)` got one
    /// phantom empty line at the end of every normal file — a
    /// rope-implementation detail leaking across the plugin boundary,
    /// which every guest author would then have to rediscover and
    /// correct for.
    pub fn line_count(&self) -> u32 {
        self.snapshot.buffer.content_line_count()
    }

    /// Total byte length.
    pub fn byte_len(&self) -> u64 {
        self.snapshot.buffer.byte_len()
    }

    /// Line `n` (0-based) without its trailing newline (matching
    /// `Buffer::line`), or `None` past EOF.
    pub fn line_at(&self, n: u32) -> Option<String> {
        self.snapshot.buffer.line(n)
    }
}

/// Project the borrow-carrying [`ActiveBufferSnapshot`] into the owned WIT
/// [`buffer::BufferSnapshot`] metadata record (§4.2). Bulk text is NOT copied
/// here — it rides the `document` handle. A non-UTF-8 path is a typed error
/// (never lossy), matching the boundary convention.
pub fn project_buffer_snapshot(snap: &ActiveBufferSnapshot) -> Result<WitBufferSnapshot, String> {
    Ok(WitBufferSnapshot {
        buffer_id: snap.buffer_id,
        path: snap.path.map(path_to_wit).transpose()?,
        language: snap.language.map(str::to_string),
        cursor: snap.cursor.to_wit()?,
        selection: snap
            .selection
            .map(|(anchor, head)| Ok::<_, String>((anchor.to_wit()?, head.to_wit()?)))
            .transpose()?,
    })
}

// NB: the generated `buffer::HostDocument` host trait (the guest's `document`
// method calls) is wired to `PluginState` at **AP.0.1** — the grammar
// `apply-action(…, doc: borrow<document>)` signature is the first world function
// to reference the resource (bindgen only binds a `with`-mapped resource a world
// function uses). The impl + `add_to_linker` (on the SYNC grammar linker) + the
// `with`-mapping (`"lattice:plugin-host/buffer.document"`) live in `lib.rs` /
// `grammar_host.rs`; the trampoline (`grammar_trampoline.rs`) mints + lends the
// handle per dispatch. The backing type below stays the single source of the
// slice/metadata logic both this and any future consumer (picker `init(doc)`)
// forward to.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_core::buffer::Buffer;
    use lattice_protocol::position::Position;

    fn snapshot(text: &str) -> Arc<DocumentSnapshot> {
        Arc::new(DocumentSnapshot {
            buffer: Buffer::from_text(text),
            ..Default::default()
        })
    }

    fn pos(line: u32, byte: u32) -> Position {
        Position { line, byte }
    }

    #[test]
    fn get_text_range_slices_only_the_requested_span() {
        let doc = DocumentResource::new(snapshot("hello\nworld\n"));
        // "world" is line 1, bytes 0..5.
        let got = doc
            .get_text_range(NativeRange {
                start: pos(1, 0),
                end: pos(1, 5),
            })
            .unwrap();
        assert_eq!(got, "world");
    }

    #[test]
    fn get_text_range_out_of_range_is_a_typed_error() {
        let doc = DocumentResource::new(snapshot("hi\n"));
        // Line 9 doesn't exist.
        let err = doc
            .get_text_range(NativeRange {
                start: pos(9, 0),
                end: pos(9, 1),
            })
            .expect_err("out-of-range must be a typed error");
        assert!(!err.is_empty(), "error carries a message: {err}");
    }

    #[test]
    fn get_text_range_end_before_start_is_a_typed_error() {
        let doc = DocumentResource::new(snapshot("abcdef\n"));
        let err = doc
            .get_text_range(NativeRange {
                start: pos(0, 4),
                end: pos(0, 1),
            })
            .expect_err("end < start must be a typed error");
        assert!(!err.is_empty(), "error carries a message: {err}");
    }

    #[test]
    fn metadata_readers_match_the_buffer() {
        let doc = DocumentResource::new(snapshot("a\nbb\nccc\n"));
        // Three lines. The trailing newline TERMINATES the third line rather
        // than opening a fourth empty one — this asserted 4 until the buffer's
        // line counting was corrected, and the stale expectation outlived the
        // fix. A plugin addressing `line_count - 1` as "the last line" must
        // land on `ccc`, not on a phantom line past the end.
        assert_eq!(doc.line_count(), 3);
        assert_eq!(doc.byte_len(), 9);
        // `Buffer::line` strips the trailing newline.
        assert_eq!(doc.line_at(1).as_deref(), Some("bb"));
        assert_eq!(doc.line_at(2).as_deref(), Some("ccc"));
        // KNOWN INCONSISTENCY, pinned so it is visible rather than tolerated:
        // `line_count()` says 3, yet index 3 still reads as an empty line. The
        // two disagree about whether the trailing newline opens a line. A
        // plugin that iterates `0..line_count()` is unaffected, which is why
        // this has gone unnoticed; one that probes `line_at` directly sees a
        // line the count denies. If this is ever reconciled, expect `None`
        // here and delete this comment.
        assert_eq!(doc.line_at(3).as_deref(), Some(""));
        assert_eq!(doc.line_at(99), None);
    }

    /// Decision A: the resource is a point-in-time snapshot. A later, unrelated
    /// snapshot of "the same document" does not shift ranges under a handle
    /// minted from the earlier one.
    #[test]
    fn snapshot_backing_is_immutable_under_later_edits() {
        let original = snapshot("original text\n");
        let doc = DocumentResource::new(original);
        // A subsequent edit produces a *new* snapshot; the handle still reads
        // the one it was minted from.
        let _later = snapshot("edited!\n");
        let got = doc
            .get_text_range(NativeRange {
                start: pos(0, 0),
                end: pos(0, 8),
            })
            .unwrap();
        assert_eq!(got, "original");
    }

    #[test]
    fn project_buffer_snapshot_projects_metadata_not_text() {
        let buffer = Buffer::from_text("fn main() {}\n");
        let path = std::path::PathBuf::from("/proj/src/main.rs");
        let snap = ActiveBufferSnapshot {
            buffer_id: 7,
            path: Some(path.as_path()),
            language: Some("rust"),
            cursor: pos(0, 3),
            selection: Some((pos(0, 0), pos(0, 7))),
            buffer: &buffer,
            syntax_symbols: Vec::new(),
            syntax_highlights: Vec::new(),
        };
        let wit = project_buffer_snapshot(&snap).unwrap();
        assert_eq!(wit.buffer_id, 7);
        assert_eq!(wit.path.as_deref(), Some("/proj/src/main.rs"));
        assert_eq!(wit.language.as_deref(), Some("rust"));
        assert_eq!(wit.cursor.line, 0);
        assert_eq!(wit.cursor.byte, 3);
        let (a, b) = wit.selection.expect("selection present");
        assert_eq!((a.byte, b.byte), (0, 7));
    }

    #[test]
    fn project_buffer_snapshot_handles_absent_optionals() {
        let buffer = Buffer::from_text("scratch\n");
        let snap = ActiveBufferSnapshot {
            buffer_id: 1,
            path: None,
            language: None,
            cursor: pos(0, 0),
            selection: None,
            buffer: &buffer,
            syntax_symbols: Vec::new(),
            syntax_highlights: Vec::new(),
        };
        let wit = project_buffer_snapshot(&snap).unwrap();
        assert!(wit.path.is_none());
        assert!(wit.language.is_none());
        assert!(wit.selection.is_none());
    }
}
