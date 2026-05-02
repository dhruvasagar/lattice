// SAFETY policy: this module opts into `unsafe` for one specific
// call -- `std::str::from_utf8_unchecked` on the streaming search
// window. Justified inline at the call site (`find_in_window`)
// with the UTF-8 invariant the rope and drain logic preserve. No
// other `unsafe` is permitted in this file; new uses must
// document the invariant and pass review.
#![allow(unsafe_code)]

//! Buffer-level regex search.
//!
//! Uses [`fancy_regex::Regex`] -- a hybrid engine that delegates to
//! the `regex` crate's RE2-style DFA for patterns without
//! backreferences/lookarounds and falls back to a bounded NFA only
//! for the patterns that need it. So:
//!
//! - Plain literals (`/foo`) hit memmem's SIMD prefilter
//!   transparently. Same speed as the prior B-β literal-only path.
//! - Standard regex (`/(foo|bar)+`) compiles through the lazy DFA.
//!   Linear-time guarantee.
//! - Backref patterns (`/(\w+)\s+\1/`) use the NFA with a
//!   configurable recursion limit -- catastrophic-backtracking
//!   patterns abort cleanly instead of locking the editor.
//!
//! Replacement-side backrefs (the more common request) are handled
//! by the `fancy_regex::Regex::replace_all` template syntax (`$1`,
//! `${name}`); see the substitute path in `lattice-ui-tui::app`.
//!
//! ## Streaming model
//!
//! The search walks the rope's chunk iterator. A sliding `window:
//! Vec<u8>` holds the current chunk plus a `MAX_MATCH_LEN`-byte
//! tail from the previous chunk so cross-chunk matches are caught
//! in exactly one iteration. Match lengths longer than
//! `MAX_MATCH_LEN` that span a chunk boundary will be missed --
//! acceptable for v1 (no editor-search workflow needs >8KB matches).
//!
//! ## API shape
//!
//! Callers compile the [`fancy_regex::Regex`] once and pass it by
//! reference. Hlsearch / live-preview consumers call
//! [`find_all`] each keystroke; the regex is compiled when the
//! pattern changes, not per call.
//!
//! Semantics (unchanged from the prior literal engine):
//!
//! - `Forward`: smallest match-start byte >= `from`. If none, wrap
//!   and return the smallest match-start byte in `[0, from)`.
//! - `Backward`: largest match-start byte <= `from`. If none,
//!   wrap and return the largest match-start byte > `from`.
//!
//! Both directions are inclusive of `from`. To skip the match at
//! the cursor (vim's `n` after `/`), the caller advances `from` by
//! one byte before calling.

use fancy_regex::Regex;
use lattice_protocol::CancellationToken;
use lattice_protocol::position::{Position, Range};

use crate::buffer::Buffer;
use crate::error::{CoreError, CoreResult};

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

/// Maximum match length we expect to span a window boundary. After
/// a search-window finishes, we keep this many trailing bytes for
/// the next iteration so a match starting in window N and ending
/// in window N+1 is caught. Matches longer than this AND spanning
/// a boundary are missed; 8KB is generous for editor patterns.
const MAX_MATCH_LEN: usize = 8 * 1024;

/// Bytes accumulated from rope chunks before the next regex
/// `find` call. ropey emits chunks in the 1-16KB range; the regex
/// engine has ~5µs of per-call setup. Coalescing into ~128KB
/// windows amortises that to ~one call per 8 chunks. Tuning down
/// memory (`MAX_MATCH_LEN`-sized windows) trades back to
/// ~5µs/chunk × N chunks. 128KB hits the L1/L2 boundary on
/// typical hardware -- bigger windows spill to L3 and the extend
/// cost exceeds the regex setup savings.
const SCAN_WINDOW_BYTES: usize = 128 * 1024;

/// Find every occurrence of the regex in the buffer. Returns the
/// positional ranges in left-to-right order. Empty for matchless
/// patterns and patterns whose minimum length exceeds the buffer.
///
/// Walks the rope's chunk iterator -- never allocates the whole
/// buffer text. Per-call cost on literal patterns is dominated by
/// the SIMD prefilter via the `regex` crate's literal extraction.
///
/// `cancel` is polled between matches and at chunk boundaries
/// inside the inner walks. A flipped token short-circuits with
/// [`CoreError::Cancelled`]; partial results are discarded.
pub fn find_all(
    buffer: &Buffer,
    regex: &Regex,
    cancel: &CancellationToken,
) -> CoreResult<Vec<Range>> {
    let total = buffer.byte_len() as usize;
    if total == 0 {
        return Ok(Vec::new());
    }
    let mut hits = Vec::new();
    let mut next = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        match find_forward_in_rope(buffer.rope(), regex, next, cancel)? {
            Some((start_b, end_b)) => {
                let start = buffer.byte_to_position(start_b)?;
                let end = buffer.byte_to_position(end_b)?;
                hits.push(Range::new(start, end));
                // Advance past the match to avoid overlapping. If the
                // match was zero-width (e.g. `^`, `\b`), still advance
                // one byte so we make progress.
                next = if end_b > start_b { end_b } else { start_b + 1 };
                if next >= total {
                    break;
                }
            }
            None => break,
        }
    }
    Ok(hits)
}

pub fn find(
    buffer: &Buffer,
    regex: &Regex,
    from: Position,
    direction: Direction,
    cancel: &CancellationToken,
) -> CoreResult<Option<SearchHit>> {
    let total = buffer.byte_len() as usize;
    if total == 0 {
        return Ok(None);
    }
    let from_byte = buffer.position_to_byte(from)?;
    let rope = buffer.rope();

    let (match_range, wrapped) = match direction {
        Direction::Forward => match find_forward_in_rope(rope, regex, from_byte, cancel)? {
            Some(r) => (Some(r), false),
            None => {
                // Wrap: search [0, from). Filter out matches at
                // or past `from` -- those would have been seen
                // by the primary pass.
                let wrap =
                    find_forward_in_rope(rope, regex, 0, cancel)?.filter(|&(s, _)| s < from_byte);
                (wrap, true)
            }
        },
        Direction::Backward => match find_backward_in_rope(rope, regex, from_byte, cancel)? {
            Some(r) => (Some(r), false),
            None => {
                // Wrap: largest match in (from, total]. Run a
                // backward scan from end-of-buffer, filter out
                // matches at or before from.
                let last = total.saturating_sub(1);
                let wrap = find_backward_in_rope(rope, regex, last, cancel)?
                    .filter(|&(s, _)| s > from_byte);
                (wrap, true)
            }
        },
    };

    match match_range {
        Some((start_b, end_b)) => {
            let start_pos = buffer.byte_to_position(start_b)?;
            let end_pos = buffer.byte_to_position(end_b)?;
            Ok(Some(SearchHit {
                range: Range::new(start_pos, end_pos),
                wrapped,
            }))
        }
        None => Ok(None),
    }
}

/// Streaming forward regex search. Returns `Ok(Some((start, end)))`
/// for the leftmost match at or after `from`, `Ok(None)` if no
/// match exists. Returns `Err(CoreError::Cancelled)` on a flipped
/// `cancel` token, or a regex runtime error (e.g. recursion-limit
/// exceeded on a pathological backref pattern).
fn find_forward_in_rope(
    rope: &ropey::Rope,
    regex: &Regex,
    from: usize,
    cancel: &CancellationToken,
) -> CoreResult<Option<(usize, usize)>> {
    let total = rope.len_bytes();
    if from >= total {
        return Ok(None);
    }
    let slice = rope.byte_slice(from..total);
    let mut window: Vec<u8> = Vec::with_capacity(SCAN_WINDOW_BYTES + MAX_MATCH_LEN);
    let mut window_start_abs = from;

    for chunk in slice.chunks() {
        // Poll once per chunk: ~1-16KB of work per check, cheaper
        // than a regex call's ~5µs setup. A flipped token bails
        // before we extend the window or enter regex.
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        window.extend_from_slice(chunk.as_bytes());
        // Only call into the regex engine once we've accumulated a
        // full SCAN_WINDOW_BYTES of data. Reduces per-call setup
        // overhead from once-per-chunk (~5µs × ~800 calls) to
        // once-per-window (~10µs × ~100 calls) on a 13MB scan.
        if window.len() < SCAN_WINDOW_BYTES {
            continue;
        }
        if let Some(m) = find_in_window(&window, regex).map_err(regex_to_core_err)? {
            return Ok(Some((window_start_abs + m.0, window_start_abs + m.1)));
        }
        // Slide forward: keep the last MAX_MATCH_LEN bytes so a
        // match spanning the window boundary is caught next round.
        // Drain at a UTF-8 char boundary.
        let drain_target = window.len().saturating_sub(MAX_MATCH_LEN);
        if drain_target > 0 {
            let drain_n = round_down_utf8_boundary(&window, drain_target);
            if drain_n > 0 {
                window.drain(..drain_n);
                window_start_abs += drain_n;
            }
        }
    }
    // Final flush: search whatever remains in the window after the
    // last chunk (in particular, when the rope ends mid-window).
    if !window.is_empty()
        && let Some(m) = find_in_window(&window, regex).map_err(regex_to_core_err)?
    {
        return Ok(Some((window_start_abs + m.0, window_start_abs + m.1)));
    }
    Ok(None)
}

/// Run `regex.find` on a window we know is valid UTF-8 (because it
/// came from rope chunks and our drain logic preserves the
/// invariant). Skipping the runtime UTF-8 revalidation that
/// `std::str::from_utf8` would do saves ~µs per call on large
/// windows -- material at 128KB+ window sizes.
#[allow(clippy::result_large_err)]
fn find_in_window(
    window: &[u8],
    regex: &Regex,
) -> Result<Option<(usize, usize)>, fancy_regex::Error> {
    // SAFETY: rope chunks are typed `&str` (valid UTF-8). The
    // window is a concat of chunks plus the bridge tail kept by
    // `round_down_utf8_boundary`, which never splits a codepoint.
    // The invariant is therefore: `window` is always valid UTF-8.
    let s = unsafe { std::str::from_utf8_unchecked(window) };
    match regex.find(s)? {
        Some(m) => Ok(Some((m.start(), m.end()))),
        None => Ok(None),
    }
}

/// Streaming backward regex search. Returns the rightmost match in
/// `[0, from + MAX_MATCH_LEN)` clamped to buffer bounds. Walks the
/// rope's chunks right-to-left. Polls `cancel` once per chunk.
fn find_backward_in_rope(
    rope: &ropey::Rope,
    regex: &Regex,
    from: usize,
    cancel: &CancellationToken,
) -> CoreResult<Option<(usize, usize)>> {
    let total = rope.len_bytes();
    if total == 0 {
        return Ok(None);
    }
    // Search window includes any match whose START byte is in
    // [0, from]. Allow up to MAX_MATCH_LEN past `from` for the
    // match to extend.
    let end = (from + MAX_MATCH_LEN).min(total);
    let slice = rope.byte_slice(0..end);

    // ropey's `Chunks` is forward-only; collect to reverse-iterate.
    // Pointer list only -- chunk content isn't copied.
    let chunks: Vec<&str> = slice.chunks().collect();
    let mut window: Vec<u8> = Vec::new();
    let mut window_end_abs = end;

    for chunk in chunks.iter().rev() {
        if cancel.is_cancelled() {
            return Err(CoreError::Cancelled);
        }
        let cb = chunk.as_bytes();
        // Frame: this chunk + bridge from the just-processed (more-
        // rightward) chunk. The bridge holds at most MAX_MATCH_LEN
        // bytes so a match spanning into that chunk is reachable.
        let mut frame = Vec::with_capacity(cb.len() + window.len());
        frame.extend_from_slice(cb);
        frame.extend_from_slice(&window);
        let frame_start_abs = window_end_abs - cb.len();

        // SAFETY: frame is a concat of valid-UTF-8 chunks at
        // codepoint-aligned boundaries (the `keep` we computed last
        // iteration was rounded via `round_down_utf8_boundary`).
        let s = unsafe { std::str::from_utf8_unchecked(&frame) };

        // For "rightmost match starting at or before `from`",
        // iterate all matches and keep the rightmost whose start
        // does not exceed `from`. Bounded by total scan cost since
        // we stop at the chunk granularity.
        let mut best: Option<(usize, usize)> = None;
        for m in regex.find_iter(s) {
            let m = m.map_err(regex_to_core_err)?;
            let abs_start = frame_start_abs + m.start();
            // Constrain to the [0, from] start range. Since we
            // process chunks right-to-left, in the rightmost
            // chunks `abs_start <= from` always holds (if the
            // chunk's bytes are all <= from). For the chunk that
            // straddles `from`, we filter.
            if abs_start <= from {
                best = Some((abs_start, frame_start_abs + m.end()));
            } else {
                break; // matches are in left-to-right order; later starts > from
            }
        }
        if let Some(found) = best {
            return Ok(Some(found));
        }

        // For the next (more-leftward) iteration: keep the first
        // MAX_MATCH_LEN bytes of `frame` as the new window. They're
        // the boundary bytes that might end a match starting in
        // the next chunk we'll process. Round to UTF-8 boundary.
        let keep_target = frame.len().min(MAX_MATCH_LEN);
        let keep = round_down_utf8_boundary(&frame, keep_target);
        window.clear();
        window.extend_from_slice(&frame[..keep]);
        window_end_abs -= cb.len();
    }
    Ok(None)
}

/// Round `target` DOWN to the largest index `<= target` that lands
/// on a UTF-8 codepoint boundary in `bytes`. Assumes `bytes` is
/// valid UTF-8. Used by the streaming search to ensure the window
/// drain doesn't split a multi-byte char.
fn round_down_utf8_boundary(bytes: &[u8], target: usize) -> usize {
    let target = target.min(bytes.len());
    let mut i = target;
    // UTF-8 char boundary: byte is either ASCII (top bit clear) or
    // a leading byte (top two bits 0b11). Continuation bytes are
    // 0b10xxxxxx -- step back over them. The end-of-buffer index
    // (i == bytes.len()) is always a valid boundary; skip indexing
    // it to avoid OOB.
    while i > 0 && i < bytes.len() && (bytes[i] & 0b1100_0000) == 0b1000_0000 {
        i -= 1;
    }
    i
}

/// Bridge fancy-regex's runtime error type into `CoreError`.
/// Pattern compilation errors surface to the App via a different
/// path (the App compiles up front); this is for runtime errors
/// like recursion-limit exceeded on a pathological backref pattern.
fn regex_to_core_err(e: fancy_regex::Error) -> crate::error::CoreError {
    use lattice_protocol::ProtocolError;
    crate::error::CoreError::Protocol(ProtocolError::InvalidRange(match e {
        fancy_regex::Error::ParseError(_, _) => "regex parse error",
        fancy_regex::Error::CompileError(_) => "regex compile error",
        fancy_regex::Error::RuntimeError(_) => "regex runtime error (recursion limit?)",
        _ => "regex error",
    }))
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

    fn re(pattern: &str) -> Regex {
        Regex::new(pattern).expect("test pattern compiles")
    }

    // ---- Public find() ----

    #[test]
    fn find_in_empty_buffer_returns_none() {
        let b = Buffer::empty();
        let hit = find(
            &b,
            &re("needle"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn forward_basic_match_no_wrap() {
        let b = Buffer::from_text("hello world");
        let hit = find(
            &b,
            &re("world"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range, r(p(0, 6), p(0, 11)));
        assert!(!hit.wrapped);
    }

    #[test]
    fn forward_wraps_when_no_match_after_from() {
        let b = Buffer::from_text("foo bar baz");
        let hit = find(
            &b,
            &re("foo"),
            p(0, 5),
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range, r(p(0, 0), p(0, 3)));
        assert!(hit.wrapped);
    }

    #[test]
    fn forward_no_match_anywhere_is_none() {
        let b = Buffer::from_text("hello");
        let hit = find(
            &b,
            &re("xyz"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(hit.is_none());
    }

    #[test]
    fn forward_inclusive_of_from_byte() {
        let b = Buffer::from_text("abc abc");
        let hit = find(
            &b,
            &re("abc"),
            p(0, 4),
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range.start, p(0, 4));
        assert!(!hit.wrapped);
    }

    #[test]
    fn backward_basic_match_no_wrap() {
        let b = Buffer::from_text("foo bar foo");
        let hit = find(
            &b,
            &re("foo"),
            p(0, 10),
            Direction::Backward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range.start, p(0, 8));
        assert!(!hit.wrapped);
    }

    #[test]
    fn backward_wraps_when_no_match_before_from() {
        let b = Buffer::from_text("alpha beta gamma");
        let hit = find(
            &b,
            &re("gamma"),
            p(0, 0),
            Direction::Backward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range.start, p(0, 11));
        assert!(hit.wrapped);
    }

    #[test]
    fn forward_finds_match_across_lines() {
        let b = Buffer::from_text("foo\nbar\nbaz");
        let hit = find(
            &b,
            &re("bar"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range, r(p(1, 0), p(1, 3)));
    }

    #[test]
    fn backward_at_start_of_buffer_finds_match_at_zero() {
        let b = Buffer::from_text("alpha gamma alpha");
        let hit = find(
            &b,
            &re("alpha"),
            p(0, 0),
            Direction::Backward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        // from=0: backward primary finds match at 0 itself.
        assert_eq!(hit.range.start, p(0, 0));
        assert!(!hit.wrapped);
    }

    #[test]
    fn unicode_pattern_matches_at_codepoint_boundary() {
        let b = Buffer::from_text("café au lait");
        let hit = find(
            &b,
            &re("café"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        // "café" = 5 UTF-8 bytes (c=1, a=1, f=1, é=2)
        assert_eq!(hit.range, r(p(0, 0), p(0, 5)));
    }

    // ---- Regex-specific behaviour ----

    #[test]
    fn regex_alternation_matches_either_branch() {
        let b = Buffer::from_text("foo bar baz");
        let hit = find(
            &b,
            &re("(bar|baz)"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range.start, p(0, 4));
    }

    #[test]
    fn regex_character_class_matches() {
        let b = Buffer::from_text("abc123def");
        let hit = find(
            &b,
            &re(r"\d+"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range, r(p(0, 3), p(0, 6)));
    }

    #[test]
    fn regex_with_pattern_backref_matches_repeated_word() {
        // fancy-regex's defining feature vs. plain `regex` crate.
        let b = Buffer::from_text("the cat the dog");
        let hit = find(
            &b,
            &re(r"(\w+) \w+ \1"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range, r(p(0, 0), p(0, 11))); // "the cat the"
    }

    #[test]
    fn regex_anchor_matches_start_of_string() {
        let b = Buffer::from_text("hello world");
        let hit = find(
            &b,
            &re("^hello"),
            Position::ZERO,
            Direction::Forward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range.start, p(0, 0));
    }

    // ---- find_all ----

    #[test]
    fn find_all_returns_every_occurrence() {
        let b = Buffer::from_text("foo bar foo baz foo");
        let hits = find_all(&b, &re("foo"), &CancellationToken::never()).unwrap();
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].start, p(0, 0));
        assert_eq!(hits[1].start, p(0, 8));
        assert_eq!(hits[2].start, p(0, 16));
    }

    #[test]
    fn find_all_no_match_returns_empty() {
        let b = Buffer::from_text("hello");
        assert!(
            find_all(&b, &re("xyz"), &CancellationToken::never())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn find_all_does_not_overlap_matches() {
        let b = Buffer::from_text("aaaa");
        // Pattern "aa" matches at 0 and 2 (advancing by match.end each time).
        let hits = find_all(&b, &re("aa"), &CancellationToken::never()).unwrap();
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn find_all_across_lines() {
        let b = Buffer::from_text("foo\nbar\nfoo");
        let hits = find_all(&b, &re("foo"), &CancellationToken::never()).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].start.line, 0);
        assert_eq!(hits[1].start.line, 2);
    }

    #[test]
    fn find_all_with_regex_class() {
        let b = Buffer::from_text("a1 b2 c3");
        let hits = find_all(&b, &re(r"\d"), &CancellationToken::never()).unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn backward_skip_current_via_caller_advance() {
        // Caller skipping the current match passes from-1.
        let b = Buffer::from_text("foo bar foo");
        let hit = find(
            &b,
            &re("foo"),
            p(0, 7),
            Direction::Backward,
            &CancellationToken::never(),
        )
        .unwrap()
        .expect("match");
        assert_eq!(hit.range.start, p(0, 0));
    }

    // ---- Cancellation ----

    #[test]
    fn pre_flipped_token_short_circuits_find() {
        let b = Buffer::from_text("foo bar baz");
        let token = CancellationToken::new();
        token.cancel();
        let result = find(&b, &re("foo"), Position::ZERO, Direction::Forward, &token);
        assert!(matches!(result, Err(CoreError::Cancelled)));
    }

    #[test]
    fn pre_flipped_token_short_circuits_find_all() {
        let b = Buffer::from_text("foo bar foo baz foo");
        let token = CancellationToken::new();
        token.cancel();
        let result = find_all(&b, &re("foo"), &token);
        assert!(matches!(result, Err(CoreError::Cancelled)));
    }

    #[test]
    fn pre_flipped_token_short_circuits_find_backward() {
        let b = Buffer::from_text("foo bar foo");
        let token = CancellationToken::new();
        token.cancel();
        let result = find(&b, &re("foo"), p(0, 10), Direction::Backward, &token);
        assert!(matches!(result, Err(CoreError::Cancelled)));
    }

    #[test]
    fn round_down_utf8_boundary_handles_multibyte() {
        // "café" = 0x63 0x61 0x66 0xC3 0xA9 (5 bytes).
        let s = "café";
        let b = s.as_bytes();
        // Target index 4 lands on the continuation byte 0xA9; round
        // down to 3 (the start of the 'é' two-byte sequence).
        assert_eq!(round_down_utf8_boundary(b, 4), 3);
        // Target index 3 lands on the leading byte of 'é' -- valid.
        assert_eq!(round_down_utf8_boundary(b, 3), 3);
        // Past end clamps to len.
        assert_eq!(round_down_utf8_boundary(b, 99), 5);
    }
}
