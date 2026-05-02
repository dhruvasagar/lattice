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

use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::error::ProtocolError;
use lattice_protocol::position::{Position, Range};

use crate::error::CoreResult;

#[derive(Debug, Clone)]
pub struct Buffer {
    rope: Rope,
}

/// What an `apply_edit` produced. The caller uses this to log change events
/// and to construct an inverse `Edit` for the undo stack.
#[derive(Debug, Clone)]
pub struct AppliedEdit {
    pub original_range: Range,
    pub inserted_range: Range,
    pub replaced_text: String,
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
        let start = self.position_to_byte(range.start)?;
        let end = self.position_to_byte(range.end)?;
        if end < start {
            return Err(ProtocolError::InvalidRange("end < start").into());
        }
        Ok(self.rope.byte_slice(start..end).to_string())
    }

    /// Apply an edit and return what was applied. The returned
    /// `AppliedEdit::replaced_text` is exactly what the caller needs to push
    /// onto the undo stack as the inverse.
    pub fn apply_edit(&mut self, edit: &Edit) -> CoreResult<AppliedEdit> {
        let start_byte = self.position_to_byte(edit.range.start)?;
        let end_byte = self.position_to_byte(edit.range.end)?;
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

        Ok(AppliedEdit {
            original_range: edit.range,
            inserted_range: Range::new(edit.range.start, inserted_end),
            replaced_text,
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
}
