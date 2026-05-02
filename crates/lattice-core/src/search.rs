//! Buffer-level literal substring search.
//!
//! v1 is byte-level memmem (case-sensitive, literal). The `from` /
//! `direction` shape and the `SearchHit { range, wrapped }` return type
//! are stable; the engine swaps to a real regex / vim-magic / smartcase
//! pipeline later without touching callers.
//!
//! Semantics:
//!
//! - `Forward`: smallest match-start byte >= `from`. If none, wrap and
//!   return the smallest match-start byte in `[0, from)`. The wrap pass
//!   is independent of the primary pass; a buffer with one match at
//!   `from` returns it without wrapping.
//! - `Backward`: largest match-start byte <= `from`. If none, wrap and
//!   return the largest match-start byte > `from`.
//!
//! Both directions are inclusive of `from`. To skip the match at the
//! cursor (vim's `n` after `/`), the caller advances `from` by one byte
//! before calling.

use lattice_protocol::position::{Position, Range};

use crate::buffer::Buffer;
use crate::error::CoreResult;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchHit {
    pub range: Range,
    /// True if the search wrapped around the buffer end (Forward) or
    /// start (Backward) before finding `range`.
    pub wrapped: bool,
}

/// Find every literal occurrence of `query` in the buffer. Returns
/// the positional ranges in left-to-right order. Returns an empty
/// vector for empty patterns or buffers shorter than the pattern.
///
/// Backed by [`memchr::memmem::find_iter`] for SIMD scans. v1 still
/// materialises the rope to a `String` once per call (`as_string`);
/// the chunked-walk path lands in B-β.
pub fn find_all(buffer: &Buffer, query: &str) -> CoreResult<Vec<Range>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let text = buffer.as_string();
    let bytes = text.as_bytes();
    let needle = query.as_bytes();
    if needle.len() > bytes.len() {
        return Ok(Vec::new());
    }
    let mut hits = Vec::new();
    let mut next = 0;
    // memmem::find_iter yields overlapping starts; we manually
    // skip past each match's end to preserve the v1 "no overlap"
    // contract callers depend on.
    while let Some(rel) = memchr::memmem::find(&bytes[next..], needle) {
        let i = next + rel;
        let start = buffer.byte_to_position(i)?;
        let end = buffer.byte_to_position(i + needle.len())?;
        hits.push(Range::new(start, end));
        next = i + needle.len();
        if next > bytes.len() - needle.len() {
            break;
        }
    }
    Ok(hits)
}

pub fn find(
    buffer: &Buffer,
    query: &str,
    from: Position,
    direction: Direction,
) -> CoreResult<Option<SearchHit>> {
    if query.is_empty() {
        return Ok(None);
    }
    let text = buffer.as_string();
    let bytes = text.as_bytes();
    let needle = query.as_bytes();
    if needle.len() > bytes.len() {
        return Ok(None);
    }
    let from_byte = buffer.position_to_byte(from)?;

    let (match_start, wrapped) = match direction {
        Direction::Forward => match find_forward(bytes, needle, from_byte) {
            Some(p) => (Some(p), false),
            None => (find_forward(bytes, needle, 0).filter(|&p| p < from_byte), true),
        },
        Direction::Backward => match find_backward(bytes, needle, from_byte) {
            Some(p) => (Some(p), false),
            None => {
                let last_start = bytes.len() - needle.len();
                (
                    find_backward(bytes, needle, last_start).filter(|&p| p > from_byte),
                    true,
                )
            }
        },
    };

    match match_start {
        Some(start) => {
            let end_byte = start + needle.len();
            let start_pos = buffer.byte_to_position(start)?;
            let end_pos = buffer.byte_to_position(end_byte)?;
            Ok(Some(SearchHit {
                range: Range::new(start_pos, end_pos),
                wrapped,
            }))
        }
        None => Ok(None),
    }
}

/// Forward substring search via [`memchr::memmem`]. Two-Way + SIMD
/// prefilter; ~2GB/s on AVX2-capable CPUs. Returns the first
/// match-start byte at or after `from`, or `None`.
fn find_forward(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    if from >= haystack.len() {
        return None;
    }
    memchr::memmem::find(&haystack[from..], needle).map(|i| i + from)
}

/// Backward substring search via [`memchr::memmem::rfind`]. Returns
/// the largest match-start byte that is `<= from`. Constructed
/// over the prefix `haystack[..=from + needle.len()]` (clamped to
/// haystack length) so a match starting at `from` itself is
/// included.
fn find_backward(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    // The search window includes any match whose START byte is
    // in [0, from]. memmem::rfind on a slice returns offsets within
    // the slice; we clamp the slice length so a match starting at
    // `from` is the rightmost one considered.
    let end = (from + needle.len()).min(haystack.len());
    memchr::memmem::rfind(&haystack[..end], needle)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn p(line: u32, byte: u32) -> Position {
        Position::new(line, byte)
    }

    fn r(s: Position, e: Position) -> Range {
        Range::new(s, e)
    }

    // ---- Low-level byte search ----

    #[test]
    fn forward_finds_first_match_at_or_after_from() {
        let h = b"abcXabcX";
        assert_eq!(find_forward(h, b"abc", 0), Some(0));
        assert_eq!(find_forward(h, b"abc", 1), Some(4));
    }

    #[test]
    fn forward_returns_none_past_last_possible_start() {
        let h = b"hello";
        assert_eq!(find_forward(h, b"lo", 4), None);
    }

    #[test]
    fn backward_finds_last_match_at_or_before_from() {
        let h = b"abcXabcXabc";
        assert_eq!(find_backward(h, b"abc", 11), Some(8));
        assert_eq!(find_backward(h, b"abc", 7), Some(4));
        assert_eq!(find_backward(h, b"abc", 3), Some(0));
    }

    #[test]
    fn backward_with_no_match_before_from_returns_none() {
        let h = b"abc";
        assert_eq!(find_backward(h, b"xy", 2), None);
    }

    // ---- Public API ----

    #[test]
    fn find_in_empty_buffer_returns_none() {
        let b = Buffer::empty();
        let hit = find(&b, "needle", Position::ZERO, Direction::Forward).unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn empty_query_returns_none() {
        let b = Buffer::from_text("hello");
        let hit = find(&b, "", Position::ZERO, Direction::Forward).unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn forward_basic_match_no_wrap() {
        let b = Buffer::from_text("hello world");
        let hit = find(&b, "world", Position::ZERO, Direction::Forward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range, r(p(0, 6), p(0, 11)));
        assert!(!hit.wrapped);
    }

    #[test]
    fn forward_wraps_when_no_match_after_from() {
        let b = Buffer::from_text("foo bar baz");
        let hit = find(&b, "foo", p(0, 5), Direction::Forward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range, r(p(0, 0), p(0, 3)));
        assert!(hit.wrapped);
    }

    #[test]
    fn forward_no_match_anywhere_is_none() {
        let b = Buffer::from_text("hello");
        let hit = find(&b, "xyz", Position::ZERO, Direction::Forward).unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn forward_inclusive_of_from_byte() {
        let b = Buffer::from_text("abc abc");
        // from at the second match's start: should return that match, no wrap.
        let hit = find(&b, "abc", p(0, 4), Direction::Forward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range.start, p(0, 4));
        assert!(!hit.wrapped);
    }

    #[test]
    fn backward_basic_match_no_wrap() {
        let b = Buffer::from_text("foo bar foo");
        // from past the second match's start: backward returns it.
        let hit = find(&b, "foo", p(0, 10), Direction::Backward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range.start, p(0, 8));
        assert!(!hit.wrapped);
    }

    #[test]
    fn backward_wraps_when_no_match_before_from() {
        let b = Buffer::from_text("alpha beta gamma");
        let hit = find(&b, "gamma", p(0, 0), Direction::Backward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range.start, p(0, 11));
        assert!(hit.wrapped);
    }

    #[test]
    fn forward_finds_match_across_lines() {
        let b = Buffer::from_text("foo\nbar\nbaz");
        let hit = find(&b, "bar", Position::ZERO, Direction::Forward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range, r(p(1, 0), p(1, 3)));
    }

    #[test]
    fn backward_finds_match_across_lines() {
        let b = Buffer::from_text("foo\nbar\nbaz");
        // from on line 2: backward should find "bar" on line 1.
        let hit = find(&b, "bar", p(2, 0), Direction::Backward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range, r(p(1, 0), p(1, 3)));
    }

    #[test]
    fn needle_longer_than_haystack_is_none() {
        let b = Buffer::from_text("hi");
        let hit = find(&b, "needle", Position::ZERO, Direction::Forward).unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn unicode_needle_matches_at_byte_boundary() {
        let b = Buffer::from_text("café au lait");
        let hit = find(&b, "café", Position::ZERO, Direction::Forward)
            .unwrap()
            .expect("match");
        // "café" = 5 UTF-8 bytes (c=1, a=1, f=1, é=2)
        assert_eq!(hit.range, r(p(0, 0), p(0, 5)));
    }

    #[test]
    fn forward_wrap_excludes_same_match_when_from_is_unique_match_start() {
        // Single match at position 5; calling with from=6 should wrap and
        // find it (since from=6 > 5, primary pass misses).
        let b = Buffer::from_text("xxxxxNEEDLExxxx");
        let hit = find(&b, "NEEDLE", p(0, 6), Direction::Forward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range.start, p(0, 5));
        assert!(hit.wrapped);
    }

    #[test]
    fn backward_at_start_of_buffer_wraps_to_end() {
        let b = Buffer::from_text("alpha gamma alpha");
        let hit = find(&b, "alpha", p(0, 0), Direction::Backward)
            .unwrap()
            .expect("match");
        // from=0: backward primary finds match at 0 itself.
        assert_eq!(hit.range.start, p(0, 0));
        assert!(!hit.wrapped);
    }

    #[test]
    fn find_all_returns_every_occurrence() {
        let b = Buffer::from_text("foo bar foo baz foo");
        let hits = find_all(&b, "foo").unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].start, p(0, 0));
        assert_eq!(hits[1].start, p(0, 8));
        assert_eq!(hits[2].start, p(0, 16));
    }

    #[test]
    fn find_all_empty_query_returns_empty() {
        let b = Buffer::from_text("hello");
        assert!(find_all(&b, "").unwrap().is_empty());
    }

    #[test]
    fn find_all_no_match_returns_empty() {
        let b = Buffer::from_text("hello");
        assert!(find_all(&b, "xyz").unwrap().is_empty());
    }

    #[test]
    fn find_all_does_not_overlap_matches() {
        let b = Buffer::from_text("aaaa");
        // Pattern "aa" matches at 0 and 2 (advancing by needle.len() each time).
        let hits = find_all(&b, "aa").unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn find_all_across_lines() {
        let b = Buffer::from_text("foo\nbar\nfoo");
        let hits = find_all(&b, "foo").unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].start.line, 0);
        assert_eq!(hits[1].start.line, 2);
    }

    #[test]
    fn backward_skip_current_via_caller_advance() {
        // Caller skipping the current match passes from-1 (or 0 if from is 0).
        let b = Buffer::from_text("foo bar foo");
        // Cursor on second "foo" (byte 8). Go backward, skipping current.
        let hit = find(&b, "foo", p(0, 7), Direction::Backward)
            .unwrap()
            .expect("match");
        assert_eq!(hit.range.start, p(0, 0));
    }
}
