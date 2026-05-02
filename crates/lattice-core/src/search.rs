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
/// Walks the rope's chunk iterator + memmem -- never allocates the
/// whole buffer text. Per-call cost is dominated by the SIMD scan
/// itself; ~2GB/s on AVX2.
pub fn find_all(buffer: &Buffer, query: &str) -> CoreResult<Vec<Range>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let needle = query.as_bytes();
    let total = buffer.byte_len() as usize;
    if needle.len() > total {
        return Ok(Vec::new());
    }
    let mut hits = Vec::new();
    let mut next = 0;
    // Find each match by scanning the rope from the byte AFTER the
    // previous match's end. Each call to `find_forward_in_rope` is
    // a chunked memmem walk; we never materialise the whole buffer.
    while let Some(i) = find_forward_in_rope(buffer.rope(), needle, next) {
        let start = buffer.byte_to_position(i)?;
        let end = buffer.byte_to_position(i + needle.len())?;
        hits.push(Range::new(start, end));
        next = i + needle.len();
        if next + needle.len() > total {
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
    let needle = query.as_bytes();
    let total = buffer.byte_len() as usize;
    if needle.len() > total {
        return Ok(None);
    }
    let from_byte = buffer.position_to_byte(from)?;
    let rope = buffer.rope();

    let (match_start, wrapped) = match direction {
        Direction::Forward => match find_forward_in_rope(rope, needle, from_byte) {
            Some(p) => (Some(p), false),
            None => (
                find_forward_in_rope(rope, needle, 0).filter(|&p| p < from_byte),
                true,
            ),
        },
        Direction::Backward => match find_backward_in_rope(rope, needle, from_byte) {
            Some(p) => (Some(p), false),
            None => {
                let last_start = total - needle.len();
                (
                    find_backward_in_rope(rope, needle, last_start).filter(|&p| p > from_byte),
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

/// In-memory substring search exposed to unit tests so the
/// algorithm can be exercised independently of the rope traversal.
/// The public `find` path uses [`find_forward_in_rope`].
#[cfg(test)]
fn find_forward(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    if from >= haystack.len() {
        return None;
    }
    memchr::memmem::find(&haystack[from..], needle).map(|i| i + from)
}

/// In-memory backward search exposed to unit tests; the public
/// `find` path uses [`find_backward_in_rope`].
#[cfg(test)]
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

/// Streaming forward search over a `ropey::Rope`. Walks the rope's
/// chunk iterator; never materialises the whole buffer.
///
/// Algorithm: maintain a sliding `window: Vec<u8>` whose first byte
/// has absolute offset `window_start_abs`. For each chunk, append
/// it to the window, run `memmem::find` on the result. On miss,
/// drain everything except the last `needle.len() - 1` bytes
/// (boundary bytes that might start a match continuing into the
/// next chunk) and advance `window_start_abs`. Capacity-reuse
/// across iterations keeps allocation bounded by chunk size.
///
/// Sequential is faster than rayon-parallel here: memmem's SIMD
/// prefilter scans rare-prefix needles at ~30GB/s, which makes a
/// 13MB sequential scan ~450µs -- inside rayon's spawn overhead
/// (~500µs). Parallel partition scans were tried (B-γ) and
/// regressed every bench measurably; reverted. Sub-millisecond
/// full-buffer scans on 200k-line corpora would require a
/// fundamentally different algorithm (suffix array, FM-index).
fn find_forward_in_rope(rope: &ropey::Rope, needle: &[u8], from: usize) -> Option<usize> {
    let total = rope.len_bytes();
    if needle.is_empty() || total < needle.len() || from >= total {
        return None;
    }
    let bridge_keep = needle.len() - 1;
    let slice = rope.byte_slice(from..total);
    let mut window: Vec<u8> = Vec::with_capacity(bridge_keep + 16 * 1024);
    let mut window_start_abs = from;

    for chunk in slice.chunks() {
        window.extend_from_slice(chunk.as_bytes());
        if let Some(rel) = memchr::memmem::find(&window, needle) {
            return Some(window_start_abs + rel);
        }
        // Slide forward: keep only the last `bridge_keep` bytes so
        // a match spanning the chunk boundary is caught next round.
        let drain_n = window.len().saturating_sub(bridge_keep);
        if drain_n > 0 {
            window.drain(..drain_n);
            window_start_abs += drain_n;
        }
    }
    None
}

/// Streaming backward search. Returns the rightmost match whose
/// start is `<= from`. Walks the rope's chunks right-to-left.
///
/// Each iteration's search window is `chunk + first_bridge_keep_of_window`
/// where `window` carries the leading bytes from the
/// just-processed (more-rightward) chunk. `memmem::rfind` returns
/// the rightmost match in this window; on hit we return immediately.
fn find_backward_in_rope(rope: &ropey::Rope, needle: &[u8], from: usize) -> Option<usize> {
    let total = rope.len_bytes();
    if needle.is_empty() || total < needle.len() {
        return None;
    }
    let bridge_keep = needle.len() - 1;
    let end = (from + needle.len()).min(total);
    let slice = rope.byte_slice(0..end);

    // ropey's `Chunks` is forward-only (not `DoubleEndedIterator`),
    // so we collect a Vec of `&str` pointers and reverse-iterate
    // it. The chunks themselves aren't copied -- only the pointer
    // list. For a 200k-line buffer (~16KB chunks) this is ~800
    // pointers (~13KB), allocated once per backward search.
    let chunks: Vec<&str> = slice.chunks().collect();
    let mut window: Vec<u8> = Vec::new();
    let mut window_end_abs = end;

    for chunk in chunks.iter().rev() {
        let cb = chunk.as_bytes();
        let mut frame = Vec::with_capacity(cb.len() + window.len());
        frame.extend_from_slice(cb);
        frame.extend_from_slice(&window);
        let frame_start_abs = window_end_abs - cb.len();
        if let Some(rel) = memchr::memmem::rfind(&frame, needle) {
            return Some(frame_start_abs + rel);
        }
        let keep = frame.len().min(bridge_keep);
        window.clear();
        window.extend_from_slice(&frame[..keep]);
        window_end_abs -= cb.len();
    }
    None
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
