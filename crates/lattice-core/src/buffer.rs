//! Buffer: a thin, edit-aware wrapper around `ropey::Rope`.
//!
//! All edits flow through `apply_edit`, which:
//! - validates the range against current bounds
//! - mutates the rope in O(log n)
//! - returns enough information for the caller to record an inverse for undo
//!   and an `AppliedEdit` for change events
//!
//! This crate does not own the actor that serializes edits; that lives in the
//! dispatcher (later phase). Callers are expected to hold the buffer behind
//! whatever single-writer discipline they prefer.

use ropey::Rope;

use lattice_protocol::edit::{Edit, EditDelta, EditKind};
use lattice_protocol::error::ProtocolError;
use lattice_protocol::position::{Position, Range};

use crate::error::CoreResult;

#[derive(Debug, Clone)]
pub struct Buffer {
    rope: Rope,
}

/// What an `apply_edit` produced. The caller uses this to log change events
/// and to construct an inverse `Edit` for the undo stack.
///
/// `inserted_text` is the text that was placed into `inserted_range`. We
/// keep it on the struct so subscribers (notably the LSP fan-in, which
/// needs `range + text` per change) don't have to re-read the buffer
/// after every applied edit.
///
/// `delta` is the tree-sitter-shaped sibling: byte/position deltas
/// the syntax worker uses to drive incremental reparse via
/// `Tree::edit` + `Parser::parse(_, Some(&old_tree))`. Constructed
/// from values already computed during the rope mutation -- no
/// extra rope reads. See [`EditDelta`] for field semantics.
#[derive(Debug, Clone)]
pub struct AppliedEdit {
    pub original_range: Range,
    pub inserted_range: Range,
    pub replaced_text: String,
    pub inserted_text: String,
    pub delta: EditDelta,
}

impl Buffer {
    pub fn empty() -> Self {
        Self { rope: Rope::new() }
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
        }
    }

    pub fn line_count(&self) -> u32 {
        // ropey's `len_lines` counts the trailing implicit empty line for any
        // rope ending in a newline. For an empty rope it returns 1. We surface
        // ropey's count directly; callers compose semantics they need.
        u32::try_from(self.rope.len_lines()).unwrap_or(u32::MAX)
    }

    pub fn byte_len(&self) -> u64 {
        self.rope.len_bytes() as u64
    }

    pub fn as_string(&self) -> String {
        self.rope.to_string()
    }

    /// Borrow the underlying rope for crate-internal callers that
    /// need streaming access (chunk iteration, byte slicing) without
    /// the O(n) `as_string` allocation. Kept `pub(crate)` so the
    /// rope abstraction stays internal -- if the storage ever
    /// changes (sled, sumtree, ...) we want a single point of
    /// adaptation.
    pub(crate) fn rope(&self) -> &Rope {
        &self.rope
    }

    /// D.3.a (2026-05-29): clone the underlying rope. `Rope::clone`
    /// is `Arc`-share of the underlying chunks (no deep copy), so
    /// the cost is one refcount bump per chunk. Used by the
    /// diff subsystem's `BufferTextProvider` impl to hand a
    /// snapshot rope to `BufferSource`
    /// from inside the worker's `spawn_blocking` body. Exposes
    /// only `Rope` (not the internal `Rope` field), so the
    /// abstraction barrier the `pub(crate) rope()` method
    /// protects stays in place.
    pub fn to_rope(&self) -> Rope {
        self.rope.clone()
    }

    /// Materialise one logical line (without its trailing `\n`).
    /// Returns `None` if `line` is past the end. Locating the line
    /// is `O(log n)`; copying its bytes is `O(line_len)`.
    ///
    /// Why this exists: the renderer's hot path needs only the
    /// visible window of lines per frame. Calling [`Self::as_string`]
    /// followed by `split('\n').collect::<Vec<_>>()` once per frame
    /// allocates `O(buffer size)` bytes; a 100MB log blows the §8.2
    /// frame budget on every paint. Iterating one line at a time
    /// keeps the per-frame work proportional to the viewport, not
    /// the document.
    pub fn line(&self, line: u32) -> Option<String> {
        if line >= self.line_count() {
            return None;
        }
        let slice = self.rope.line(line as usize);
        let text = slice.to_string();
        // ropey includes the trailing newline in line slices; the
        // renderer wants the line content without it.
        Some(match text.strip_suffix('\n') {
            Some(s) => s.to_string(),
            None => text,
        })
    }

    /// Byte length of one line excluding the trailing newline.
    /// O(log n). Cheap helper for renderers that want to know a
    /// line's width without materialising its text.
    pub fn line_byte_len(&self, line: u32) -> u32 {
        if line >= self.line_count() {
            return 0;
        }
        let slice = self.rope.line(line as usize);
        let bytes = slice.len_bytes();
        // Subtract 1 if the slice ends in a newline (ropey
        // includes trailing `\n` in its line slice). ropey's
        // `Chars` isn't double-ended so we peek the last byte
        // directly.
        let has_trailing_newline = bytes > 0 && slice.byte(bytes - 1) == b'\n';
        let len = if has_trailing_newline {
            bytes - 1
        } else {
            bytes
        };
        u32::try_from(len).unwrap_or(u32::MAX)
    }

    pub fn slice(&self, range: Range) -> CoreResult<String> {
        let start = snap_to_char_boundary(&self.rope, self.position_to_byte(range.start)?);
        let end = snap_to_char_boundary(&self.rope, self.position_to_byte(range.end)?);
        if end < start {
            return Err(ProtocolError::InvalidRange("end < start").into());
        }
        Ok(self.rope.byte_slice(start..end).to_string())
    }

    /// Apply an edit and return what was applied. The returned
    /// `AppliedEdit::replaced_text` is exactly what the caller needs to push
    /// onto the undo stack as the inverse.
    pub fn apply_edit(&mut self, edit: &Edit) -> CoreResult<AppliedEdit> {
        let start_byte =
            snap_to_char_boundary(&self.rope, self.position_to_byte(edit.range.start)?);
        let end_byte = snap_to_char_boundary(&self.rope, self.position_to_byte(edit.range.end)?);
        if end_byte < start_byte {
            return Err(ProtocolError::InvalidRange("end < start").into());
        }

        let replaced_text = self.rope.byte_slice(start_byte..end_byte).to_string();

        match &edit.kind {
            EditKind::Replace { text } => {
                if start_byte != end_byte {
                    self.rope.remove(
                        byte_to_char(&self.rope, start_byte)..byte_to_char(&self.rope, end_byte),
                    );
                }
                if !text.is_empty() {
                    let char_idx = byte_to_char(&self.rope, start_byte);
                    self.rope.insert(char_idx, text);
                }
            }
        }

        let inserted_end_byte = match &edit.kind {
            EditKind::Replace { text } => start_byte + text.len(),
        };
        let inserted_end = self.byte_to_position(inserted_end_byte)?;

        let inserted_text = match &edit.kind {
            EditKind::Replace { text } => text.clone(),
        };
        // Tree-sitter-shaped delta. Every field is already
        // computed above; this is a six-cast struct literal --
        // ~ns regime, no rope reads. The casts to u32 honour
        // lattice's existing Position-byte cap (Position::byte
        // is u32, so documents are already capped at 4 GB on
        // the line-byte axis); a saturating cast keeps wildly
        // oversized buffers from panicking on cast overflow.
        let delta = EditDelta {
            start_byte: u32::try_from(start_byte).unwrap_or(u32::MAX),
            old_end_byte: u32::try_from(end_byte).unwrap_or(u32::MAX),
            new_end_byte: u32::try_from(inserted_end_byte).unwrap_or(u32::MAX),
            start_position: edit.range.start,
            old_end_position: edit.range.end,
            new_end_position: inserted_end,
        };
        Ok(AppliedEdit {
            original_range: edit.range,
            inserted_range: Range::new(edit.range.start, inserted_end),
            replaced_text,
            inserted_text,
            delta,
        })
    }

    pub fn position_to_byte(&self, pos: Position) -> CoreResult<usize> {
        let line_count = self.line_count();
        if pos.line >= line_count {
            return Err(ProtocolError::PositionOutOfBounds {
                position: pos,
                line_count,
            }
            .into());
        }
        let line_start = self.rope.line_to_byte(pos.line as usize);
        let line_end = if (pos.line as usize + 1) < self.rope.len_lines() {
            self.rope.line_to_byte(pos.line as usize + 1)
        } else {
            self.rope.len_bytes()
        };
        let line_byte_len = line_end - line_start;
        if pos.byte as usize > line_byte_len {
            return Err(ProtocolError::PositionOutOfBounds {
                position: pos,
                line_count,
            }
            .into());
        }
        Ok(line_start + pos.byte as usize)
    }

    pub fn byte_to_position(&self, byte: usize) -> CoreResult<Position> {
        let line = self.rope.byte_to_line(byte);
        let line_start = self.rope.line_to_byte(line);
        Ok(Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            byte: u32::try_from(byte - line_start).unwrap_or(u32::MAX),
        })
    }
}

fn byte_to_char(rope: &Rope, byte: usize) -> usize {
    rope.byte_to_char(byte)
}

/// Round `byte` DOWN to the start of the UTF-8 scalar it falls within, so it is
/// always a valid char boundary. A mid-scalar byte offset would panic ropey's
/// `byte_slice` / `byte_to_char` on the hot path (paramount #1: never panic on
/// a keystroke). The char motions produce boundary-aligned offsets after the
/// scalar-step fix; this is the defensive net for any residual byte-off offset
/// from other motions (word/find/till) or future plugin-contributed motions.
/// `byte_to_char` is monotonic, so snapping both ends of a range preserves
/// `start <= end`.
fn snap_to_char_boundary(rope: &Rope, byte: usize) -> usize {
    rope.char_to_byte(rope.byte_to_char(byte))
}

/// Transform a position across an edit (§4.1 of owner-write-caret.md).
///
/// Pure, total function: every position has a well-defined result after any
/// edit. Composes left-to-right across batches.
///
/// | Where `p` is relative to `edit.original_range` | Result |
/// |---|---|
/// | strictly before `original_range.start` | unchanged |
/// | at or after `original_range.end` | shifted by `inserted_range.end - original_range.end` |
/// | strictly inside `original_range` | clamped to `inserted_range.end` |
/// | at `original_range.start` (non-empty range) | stays at `inserted_range.start` |
pub fn transform_position(pos: Position, edit: &AppliedEdit) -> Position {
    let original = edit.original_range;
    let inserted = edit.inserted_range;

    if pos >= original.end {
        let d_line = inserted.end.line as i64 - original.end.line as i64;
        let d_byte = inserted.end.byte as i64 - original.end.byte as i64;
        let new_line = if d_line >= 0 {
            pos.line.saturating_add(d_line as u32)
        } else {
            pos.line.saturating_sub((-d_line) as u32)
        };
        let new_byte = if d_line == 0 {
            if d_byte >= 0 {
                pos.byte.saturating_add(d_byte as u32)
            } else {
                pos.byte.saturating_sub((-d_byte) as u32)
            }
        } else {
            pos.byte
        };
        return Position::new(new_line, new_byte);
    }

    if pos < original.start {
        return pos;
    }

    if pos == original.start {
        inserted.start
    } else {
        inserted.end
    }
}

impl Default for Buffer {
    fn default() -> Self {
        Self::empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::edit::Edit;

    #[test]
    fn from_str_roundtrips() {
        let b = Buffer::from_text("hello\nworld\n");
        assert_eq!(b.as_string(), "hello\nworld\n");
    }

    #[test]
    fn insert_at_origin() {
        let mut b = Buffer::empty();
        let applied = b.apply_edit(&Edit::insert(Position::ZERO, "hi")).unwrap();
        assert_eq!(b.as_string(), "hi");
        assert_eq!(applied.replaced_text, "");
        assert_eq!(applied.inserted_range.end, Position::new(0, 2));
    }

    #[test]
    fn insert_appends_at_end() {
        let mut b = Buffer::from_text("ab");
        b.apply_edit(&Edit::insert(Position::new(0, 2), "cd"))
            .unwrap();
        assert_eq!(b.as_string(), "abcd");
    }

    #[test]
    fn delete_replaces_with_empty() {
        let mut b = Buffer::from_text("abcdef");
        let range = Range::new(Position::new(0, 1), Position::new(0, 4));
        let applied = b.apply_edit(&Edit::delete(range)).unwrap();
        assert_eq!(b.as_string(), "aef");
        assert_eq!(applied.replaced_text, "bcd");
    }

    #[test]
    fn replace_returns_replaced_text() {
        let mut b = Buffer::from_text("hello");
        let range = Range::new(Position::new(0, 0), Position::new(0, 5));
        let applied = b.apply_edit(&Edit::replace(range, "world!")).unwrap();
        assert_eq!(b.as_string(), "world!");
        assert_eq!(applied.replaced_text, "hello");
    }

    #[test]
    fn position_out_of_bounds_is_an_error() {
        let mut b = Buffer::from_text("abc");
        let bad = Position::new(5, 0);
        assert!(b.apply_edit(&Edit::insert(bad, "x")).is_err());
    }

    #[test]
    fn slice_extracts_substring() {
        let b = Buffer::from_text("hello\nworld");
        let r = Range::new(Position::new(0, 1), Position::new(1, 3));
        assert_eq!(b.slice(r).unwrap(), "ello\nwor");
    }

    #[test]
    fn slice_snaps_mid_scalar_byte_offset_without_panicking() {
        // Defensive hot-path guard: a range whose end lands mid-UTF-8-scalar
        // (byte 1 inside `│` U+2502, bytes 0..3) must not panic ropey's
        // byte_slice. It snaps DOWN to the nearest boundary (byte 0 here), so
        // the slice is empty rather than a crash.
        let b = Buffer::from_text("│x");
        let r = Range::new(Position::new(0, 0), Position::new(0, 1));
        assert_eq!(b.slice(r).unwrap(), "");
    }

    #[test]
    fn apply_edit_snaps_mid_scalar_range_without_panicking() {
        // Same guard on the edit path: a delete range ending mid-scalar must
        // not panic; it snaps to a boundary before touching the rope.
        let mut b = Buffer::from_text("│x");
        let edit = Edit::delete(Range::new(Position::new(0, 0), Position::new(0, 1)));
        // No panic; the mid-scalar end snaps down so nothing is removed.
        assert!(b.apply_edit(&edit).is_ok());
        assert_eq!(b.as_string(), "│x");
    }

    #[test]
    fn line_returns_one_line_without_newline() {
        let b = Buffer::from_text("hello\nworld\nfoo");
        assert_eq!(b.line(0), Some("hello".into()));
        assert_eq!(b.line(1), Some("world".into()));
        assert_eq!(b.line(2), Some("foo".into()));
    }

    #[test]
    fn line_out_of_range_is_none() {
        let b = Buffer::from_text("a\nb");
        assert_eq!(b.line(99), None);
    }

    #[test]
    fn line_handles_trailing_newline() {
        // Trailing newline -> ropey reports an extra empty line;
        // it must materialise as the empty string.
        let b = Buffer::from_text("a\nb\n");
        assert_eq!(b.line(0), Some("a".into()));
        assert_eq!(b.line(1), Some("b".into()));
        assert_eq!(b.line(2), Some(String::new()));
    }

    #[test]
    fn line_byte_len_excludes_newline() {
        let b = Buffer::from_text("hello\nworld");
        assert_eq!(b.line_byte_len(0), 5);
        assert_eq!(b.line_byte_len(1), 5);
    }

    #[test]
    fn line_byte_len_for_empty_line_is_zero() {
        let b = Buffer::from_text("\n");
        assert_eq!(b.line_byte_len(0), 0);
    }

    #[test]
    fn empty_buffer_default_is_equivalent() {
        let b: Buffer = Default::default();
        assert_eq!(b.as_string(), "");
        assert_eq!(b.byte_len(), 0);
    }

    #[test]
    fn empty_buffer_has_one_logical_line() {
        // Ropey's convention: an empty rope still presents one (empty) line.
        let b = Buffer::empty();
        assert_eq!(b.line_count(), 1);
    }

    #[test]
    fn line_count_counts_trailing_empty_line() {
        // Ropey's convention: a rope ending with `\n` reports one extra (empty)
        // trailing line. Documenting current behavior so changes are intentional.
        let b = Buffer::from_text("a\nb\n");
        assert_eq!(b.line_count(), 3);
    }

    #[test]
    fn byte_len_reports_utf8_byte_count() {
        let b = Buffer::from_text("café");
        // café = 5 UTF-8 bytes (c=1, a=1, f=1, é=2)
        assert_eq!(b.byte_len(), 5);
    }

    #[test]
    fn unicode_insert_preserves_byte_count() {
        let mut b = Buffer::from_text("c");
        b.apply_edit(&Edit::insert(Position::new(0, 1), "é"))
            .unwrap();
        assert_eq!(b.as_string(), "cé");
        assert_eq!(b.byte_len(), 3);
    }

    #[test]
    fn cross_line_replace_works() {
        let mut b = Buffer::from_text("ab\ncd\nef");
        let range = Range::new(Position::new(0, 1), Position::new(2, 1));
        let applied = b.apply_edit(&Edit::replace(range, "X")).expect("replace");
        assert_eq!(b.as_string(), "aXf");
        assert_eq!(applied.replaced_text, "b\ncd\ne");
        assert_eq!(applied.inserted_range.end, Position::new(0, 2));
    }

    #[test]
    fn delete_then_insert_via_replace_yields_correct_inserted_range() {
        let mut b = Buffer::from_text("hello\nworld");
        let range = Range::new(Position::new(0, 0), Position::new(0, 5));
        let applied = b
            .apply_edit(&Edit::replace(range, "BIG\nNEW"))
            .expect("replace");
        assert_eq!(b.as_string(), "BIG\nNEW\nworld");
        assert_eq!(applied.inserted_range.start, Position::new(0, 0));
        // "BIG\nNEW" -> 7 bytes, line 1 byte 3 (NEW = 3 bytes).
        assert_eq!(applied.inserted_range.end, Position::new(1, 3));
    }

    #[test]
    fn insert_at_end_of_final_line_works() {
        let mut b = Buffer::from_text("xy");
        let pos = Position::new(0, 2);
        let applied = b.apply_edit(&Edit::insert(pos, "z")).expect("insert");
        assert_eq!(b.as_string(), "xyz");
        assert_eq!(applied.inserted_range.end, Position::new(0, 3));
    }

    #[test]
    fn end_before_start_is_an_error_via_apply() {
        let mut b = Buffer::from_text("abc");
        let range = Range::new(Position::new(0, 2), Position::new(0, 1));
        let err = b.apply_edit(&Edit::delete(range));
        assert!(err.is_err(), "expected error for inverted range");
    }

    #[test]
    fn end_before_start_is_an_error_via_slice() {
        let b = Buffer::from_text("abc");
        let range = Range::new(Position::new(0, 2), Position::new(0, 1));
        assert!(b.slice(range).is_err());
    }

    #[test]
    fn slice_at_zero_width_returns_empty_string() {
        let b = Buffer::from_text("hello");
        let range = Range::new(Position::new(0, 2), Position::new(0, 2));
        assert_eq!(b.slice(range).unwrap(), "");
    }

    #[test]
    fn applied_edit_round_trip_via_inverse_restores_buffer() {
        // Apply an edit, then apply the inverse implied by the AppliedEdit, and
        // verify the buffer returns to its original content. This is the
        // contract the Document layer relies on for undo.
        let original = "abcdef";
        let mut b = Buffer::from_text(original);
        let range = Range::new(Position::new(0, 1), Position::new(0, 4));
        let applied = b.apply_edit(&Edit::replace(range, "X")).expect("replace");
        // Now apply: replace inserted_range with replaced_text.
        b.apply_edit(&Edit::replace(
            applied.inserted_range,
            applied.replaced_text,
        ))
        .expect("invert");
        assert_eq!(b.as_string(), original);
    }

    // ---- Slice B.1: edit delta plumbing -------------------------
    //
    // `AppliedEdit::delta` carries the six tree-sitter-shaped
    // fields the syntax worker needs to drive incremental
    // reparse via `Tree::edit`. These tests pin the field
    // semantics across the four edit shapes (insert, delete,
    // replace, multi-line) plus invariants that catch field
    // drift at the construction site.

    #[test]
    fn delta_shape_for_simple_insert() {
        // Insert "hello" at the start of "world" -- pure insert,
        // no bytes deleted. old_end == start; new_end ==
        // start + len(text).
        let mut b = Buffer::from_text("world");
        let applied = b
            .apply_edit(&Edit::insert(Position::new(0, 0), "hello"))
            .expect("insert");
        let d = applied.delta;
        assert_eq!(d.start_byte, 0);
        assert_eq!(d.old_end_byte, 0);
        assert_eq!(d.new_end_byte, 5);
        assert_eq!(d.start_position, Position::new(0, 0));
        assert_eq!(d.old_end_position, Position::new(0, 0));
        assert_eq!(d.new_end_position, Position::new(0, 5));
    }

    #[test]
    fn delta_shape_for_pure_delete() {
        // Delete "bcd" from "abcdef" -- pure delete, no bytes
        // inserted. new_end == start; old_end captures the
        // deleted span's end.
        let mut b = Buffer::from_text("abcdef");
        let range = Range::new(Position::new(0, 1), Position::new(0, 4));
        let applied = b.apply_edit(&Edit::delete(range)).expect("delete");
        let d = applied.delta;
        assert_eq!(d.start_byte, 1);
        assert_eq!(d.old_end_byte, 4);
        assert_eq!(d.new_end_byte, 1);
        assert_eq!(d.start_position, Position::new(0, 1));
        assert_eq!(d.old_end_position, Position::new(0, 4));
        assert_eq!(d.new_end_position, Position::new(0, 1));
    }

    #[test]
    fn delta_shape_for_multiline_replace() {
        // Replace `world\nbar` (spans lines 1-2) with a single
        // `Y`. The range starts at (1,0) -- line 1's first byte
        // -- and ends at (2,3), the end of "bar". The result is
        // `hello\nY\nfoo`. new_end_byte=7 lands at line 1 byte 1
        // (cursor right after the 'Y', before the trailing \n);
        // line 0 is unchanged by byte_to_position's mapping.
        let mut b = Buffer::from_text("hello\nworld\nbar\nfoo");
        let range = Range::new(Position::new(1, 0), Position::new(2, 3));
        let applied = b.apply_edit(&Edit::replace(range, "Y")).expect("replace");
        assert_eq!(b.as_string(), "hello\nY\nfoo");
        let d = applied.delta;
        assert_eq!(d.start_byte, 6);
        assert_eq!(d.old_end_byte, 15); // "hello\n" + "world\nbar" = 6+9
        assert_eq!(d.new_end_byte, 7); // "hello\n" + "Y"
        assert_eq!(d.start_position, Position::new(1, 0));
        assert_eq!(d.old_end_position, Position::new(2, 3));
        assert_eq!(d.new_end_position, Position::new(1, 1));
    }

    #[test]
    fn delta_byte_invariants_hold_for_replace() {
        // The two key invariants tree-sitter relies on:
        //   new_end_byte - start_byte == inserted_text.len()
        //   old_end_byte - start_byte == replaced_text.len()
        // Catches any future field drift at the construction
        // site without naming specific values.
        let mut b = Buffer::from_text("aaa\nbbb\nccc");
        let range = Range::new(Position::new(0, 1), Position::new(1, 2));
        let applied = b.apply_edit(&Edit::replace(range, "XYZ")).expect("replace");
        let d = applied.delta;
        assert_eq!(
            (d.new_end_byte - d.start_byte) as usize,
            applied.inserted_text.len(),
        );
        assert_eq!(
            (d.old_end_byte - d.start_byte) as usize,
            applied.replaced_text.len(),
        );
    }

    #[test]
    fn delta_positions_match_inserted_and_original_ranges() {
        // start_position == original_range.start; old_end_position
        // == original_range.end; new_end_position ==
        // inserted_range.end. Pins the AppliedEdit's existing
        // range fields and the new delta to a consistent shape.
        let mut b = Buffer::from_text("line0\nline1\nline2");
        let range = Range::new(Position::new(0, 2), Position::new(1, 3));
        let applied = b.apply_edit(&Edit::replace(range, "AB")).expect("replace");
        assert_eq!(applied.delta.start_position, applied.original_range.start);
        assert_eq!(applied.delta.old_end_position, applied.original_range.end);
        assert_eq!(applied.delta.new_end_position, applied.inserted_range.end);
    }

    #[test]
    fn delta_for_inverse_edit_swaps_old_and_new() {
        // Apply an insert, then apply its inverse (delete what
        // was inserted). The inverse's delta has start_byte
        // unchanged but old_end and new_end swapped relative to
        // the original. Pins undo/redo correctness without
        // exercising the Document layer.
        let mut b = Buffer::from_text("abc");
        let forward = b
            .apply_edit(&Edit::insert(Position::new(0, 1), "XY"))
            .expect("forward");
        // After forward: "aXYbc". Inverse: delete the "XY" we
        // just inserted -- range [(0,1), (0,3)).
        let inverse = b
            .apply_edit(&Edit::delete(Range::new(
                Position::new(0, 1),
                Position::new(0, 3),
            )))
            .expect("inverse");
        // Forward: start=1, old_end=1, new_end=3 (insert 2 bytes).
        // Inverse: start=1, old_end=3, new_end=1 (delete 2 bytes).
        assert_eq!(forward.delta.start_byte, 1);
        assert_eq!(forward.delta.old_end_byte, 1);
        assert_eq!(forward.delta.new_end_byte, 3);
        assert_eq!(inverse.delta.start_byte, 1);
        assert_eq!(inverse.delta.old_end_byte, 3);
        assert_eq!(inverse.delta.new_end_byte, 1);
    }

    // ---- owner-write-caret.md §8: transform_position tests ----

    #[test]
    fn transform_pos_before_is_unchanged() {
        let edit = AppliedEdit {
            original_range: Range::new(Position::new(1, 0), Position::new(1, 5)),
            inserted_range: Range::new(Position::new(1, 0), Position::new(1, 3)),
            replaced_text: "hello".into(),
            inserted_text: "hi".into(),
            // delta is irrelevant for transform
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        assert_eq!(
            transform_position(Position::new(0, 10), &edit),
            Position::new(0, 10)
        );
    }

    #[test]
    fn transform_pos_at_or_after_end_shifts_by_delta() {
        // Insert at (1, 0) with delta +2 lines +0 byte
        let edit = AppliedEdit {
            original_range: Range::new(Position::new(1, 0), Position::new(1, 0)),
            inserted_range: Range::new(Position::new(1, 0), Position::new(3, 0)),
            replaced_text: String::new(),
            inserted_text: "a\nb\nc".into(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        // Caret at (5, 3) → shifted by +2 lines → (7, 3)
        assert_eq!(
            transform_position(Position::new(5, 3), &edit),
            Position::new(7, 3)
        );
    }

    #[test]
    fn transform_pos_at_or_after_end_shifts_byte_on_same_line() {
        let edit = AppliedEdit {
            original_range: Range::new(Position::new(0, 2), Position::new(0, 2)),
            inserted_range: Range::new(Position::new(0, 2), Position::new(0, 5)),
            replaced_text: String::new(),
            inserted_text: "abc".into(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        // Insert "abc" at byte 2. Caret at byte 4 → shifted to byte 7
        assert_eq!(
            transform_position(Position::new(0, 4), &edit),
            Position::new(0, 7)
        );
    }

    #[test]
    fn transform_pos_strictly_inside_clamps_to_end() {
        let edit = AppliedEdit {
            original_range: Range::new(Position::new(0, 1), Position::new(0, 5)),
            inserted_range: Range::new(Position::new(0, 1), Position::new(0, 2)),
            replaced_text: "ello".into(),
            inserted_text: "x".into(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        // Caret inside "ello" → clamped to end of "x" = (0, 2)
        assert_eq!(
            transform_position(Position::new(0, 3), &edit),
            Position::new(0, 2)
        );
    }

    #[test]
    fn transform_pos_at_start_of_non_empty_range_stays() {
        let edit = AppliedEdit {
            original_range: Range::new(Position::new(0, 1), Position::new(0, 5)),
            inserted_range: Range::new(Position::new(0, 1), Position::new(0, 2)),
            replaced_text: "ello".into(),
            inserted_text: "x".into(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        // Caret at start of "ello" → stays at start of "x" = (0, 1)
        assert_eq!(
            transform_position(Position::new(0, 1), &edit),
            Position::new(0, 1)
        );
    }

    #[test]
    fn transform_pos_insertion_at_cursor_rides_forward() {
        // Insert at the caret position (empty range, pos == original.start == original.end)
        let edit = AppliedEdit {
            original_range: Range::new(Position::new(0, 2), Position::new(0, 2)),
            inserted_range: Range::new(Position::new(0, 2), Position::new(0, 5)),
            replaced_text: String::new(),
            inserted_text: "XYZ".into(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        // Caret at (0, 2) — the insertion point — rides forward to (0, 5)
        assert_eq!(
            transform_position(Position::new(0, 2), &edit),
            Position::new(0, 5)
        );
    }

    #[test]
    fn transform_pos_deletion_spanning_caret_clamps_to_end() {
        let edit = AppliedEdit {
            original_range: Range::new(Position::new(0, 1), Position::new(0, 10)),
            inserted_range: Range::new(Position::new(0, 1), Position::new(0, 1)),
            replaced_text: "bcdefghij".into(),
            inserted_text: String::new(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        // Caret at (0, 5) inside deleted range → clamped to (0, 1)
        assert_eq!(
            transform_position(Position::new(0, 5), &edit),
            Position::new(0, 1)
        );
    }

    #[test]
    fn transform_pos_batch_applies_left_to_right() {
        let edit1 = AppliedEdit {
            original_range: Range::new(Position::new(0, 5), Position::new(0, 5)),
            inserted_range: Range::new(Position::new(0, 5), Position::new(0, 8)),
            replaced_text: String::new(),
            inserted_text: "abc".into(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        let edit2 = AppliedEdit {
            original_range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            inserted_range: Range::new(Position::new(0, 0), Position::new(0, 2)),
            replaced_text: String::new(),
            inserted_text: "XY".into(),
            delta: EditDelta {
                start_byte: 0,
                old_end_byte: 0,
                new_end_byte: 0,
                start_position: Position::ZERO,
                old_end_position: Position::ZERO,
                new_end_position: Position::ZERO,
            },
        };
        // Caret at (0, 3) before batch.
        // After edit1 (insert "abc" at byte 5 → pos unaffected since pos < 5).
        // After edit2 (insert "XY" at byte 0 → pos shifts by +2 bytes).
        let pos = transform_position(Position::new(0, 3), &edit1);
        let pos = transform_position(pos, &edit2);
        assert_eq!(pos, Position::new(0, 5));
    }
}
