//! Position-encoding conversion (LSP 3.17 §3.17 / §General).
//!
//! Lattice uses UTF-8 byte offsets internally
//! (`lattice_protocol::Position { line, byte }`). LSP sends:
//!
//! - **utf-8** (`PositionEncodingKind::UTF8`): column is the
//!   UTF-8 byte offset within the line. Identical to ours -- no
//!   conversion needed.
//! - **utf-16** (`PositionEncodingKind::UTF16`, the LSP 3.16
//!   default): column is the UTF-16 code-unit offset within the
//!   line. We convert when the negotiated encoding is utf-16.
//! - **utf-32** (`PositionEncodingKind::UTF32`): column is the
//!   Unicode codepoint count. Practically unused; we don't
//!   advertise support, but the converter is here in case a
//!   server demands it.
//!
//! The converters work line-by-line: callers pass the line text
//! plus an offset and get back the offset in the target
//! encoding. Lattice's `Position::byte` is always within a
//! single line, so we never have to walk multiple lines.
//!
//! ## Performance
//!
//! `utf8_byte_to_utf16_column` is `O(byte)` -- it walks the
//! prefix and counts UTF-16 code units. For ASCII lines this
//! collapses to `byte` (no multi-byte chars). The bench
//! `lsp::position::utf8_to_utf16` measures the worst case
//! (line of CJK glyphs) at sub-microsecond.

use lsp_types::PositionEncodingKind;

/// Convert a UTF-8 byte offset within `line` to the offset in
/// the negotiated `encoding`. Used when constructing LSP
/// `Position::character` from lattice's `Position::byte`.
///
/// Bounds: `byte` may equal `line.len()` (one-past-the-end);
/// callers passing larger values get the count for the whole
/// line plus their over-shoot, matching Rust's `&str` slicing
/// semantics.
pub fn byte_to_lsp_character(line: &str, byte: u32, encoding: &PositionEncodingKind) -> u32 {
    if encoding == &PositionEncodingKind::UTF8 {
        return byte;
    }
    if encoding == &PositionEncodingKind::UTF32 {
        return utf8_byte_to_utf32_column(line, byte);
    }
    // utf-16 is the spec default and our fallback.
    utf8_byte_to_utf16_column(line, byte)
}

/// Convert an LSP `character` value (in the negotiated encoding)
/// to a UTF-8 byte offset within `line`. Used for ranges that
/// arrive FROM the server (definitions, diagnostics, etc.).
pub fn lsp_character_to_byte(line: &str, character: u32, encoding: &PositionEncodingKind) -> u32 {
    if encoding == &PositionEncodingKind::UTF8 {
        // Clamp to line length so a server reporting an offset
        // past EOL doesn't yield an out-of-bounds byte index.
        return character.min(line.len() as u32);
    }
    if encoding == &PositionEncodingKind::UTF32 {
        return utf32_column_to_utf8_byte(line, character);
    }
    utf16_column_to_utf8_byte(line, character)
}

/// UTF-8 byte offset → UTF-16 code-unit offset within `line`.
/// `byte` is treated as a position within the line text;
/// if it's past the end the function returns the line's full
/// utf-16 length plus the over-shoot in bytes (a useful approximation
/// for clamped callers, but production paths shouldn't pass
/// beyond `line.len()`).
pub fn utf8_byte_to_utf16_column(line: &str, byte: u32) -> u32 {
    let cap = line.len() as u32;
    if byte == 0 {
        return 0;
    }
    if byte >= cap {
        // Whole line fits.
        return line.encode_utf16().count() as u32 + byte.saturating_sub(cap);
    }
    // SAFETY: callers pass a byte offset that lies on a UTF-8
    // boundary -- guaranteed by `lattice_core::Buffer` (Position
    // .byte is always at a char boundary). If a buggy caller
    // breaks this, `is_char_boundary` catches it and we fall
    // back to a safe approximation: walk char-by-char.
    if !line.is_char_boundary(byte as usize) {
        return line
            .char_indices()
            .take_while(|(i, _)| (*i as u32) < byte)
            .fold(0u32, |acc, (_, c)| acc + c.len_utf16() as u32);
    }
    let prefix = &line[..byte as usize];
    prefix.encode_utf16().count() as u32
}

/// UTF-16 code-unit offset → UTF-8 byte offset within `line`.
/// Walks chars accumulating utf-16 units; stops when the running
/// count reaches `character`.
pub fn utf16_column_to_utf8_byte(line: &str, character: u32) -> u32 {
    let mut units_seen: u32 = 0;
    let mut byte: u32 = 0;
    for c in line.chars() {
        if units_seen >= character {
            return byte;
        }
        units_seen = units_seen.saturating_add(c.len_utf16() as u32);
        byte = byte.saturating_add(c.len_utf8() as u32);
    }
    // Past end of line: clamp to the line's byte length.
    byte
}

/// UTF-8 byte offset → UTF-32 codepoint offset within `line`.
pub fn utf8_byte_to_utf32_column(line: &str, byte: u32) -> u32 {
    let cap = line.len() as u32;
    if byte == 0 {
        return 0;
    }
    if byte >= cap {
        return line.chars().count() as u32 + byte.saturating_sub(cap);
    }
    if !line.is_char_boundary(byte as usize) {
        return line
            .char_indices()
            .take_while(|(i, _)| (*i as u32) < byte)
            .count() as u32;
    }
    line[..byte as usize].chars().count() as u32
}

/// UTF-32 codepoint offset → UTF-8 byte offset.
pub fn utf32_column_to_utf8_byte(line: &str, character: u32) -> u32 {
    let mut chars_seen: u32 = 0;
    let mut byte: u32 = 0;
    for c in line.chars() {
        if chars_seen >= character {
            return byte;
        }
        chars_seen = chars_seen.saturating_add(1);
        byte = byte.saturating_add(c.len_utf8() as u32);
    }
    byte
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_byte_equals_utf16_column() {
        let line = "hello world";
        assert_eq!(utf8_byte_to_utf16_column(line, 0), 0);
        assert_eq!(utf8_byte_to_utf16_column(line, 5), 5);
        assert_eq!(utf8_byte_to_utf16_column(line, 11), 11);
    }

    #[test]
    fn latin1_two_byte_one_utf16_unit() {
        // `é` is U+00E9, 2 bytes in UTF-8, 1 UTF-16 code unit.
        let line = "café";
        assert_eq!(line.len(), 5); // "caf" + 2 bytes for é
        // byte 4 is start of é, byte 5 is past it.
        assert_eq!(utf8_byte_to_utf16_column(line, 3), 3); // "caf"
        assert_eq!(utf8_byte_to_utf16_column(line, 5), 4); // "café"
    }

    #[test]
    fn cjk_three_byte_one_utf16_unit() {
        // CJK ideograph U+4E2D '中': 3 UTF-8 bytes, 1 UTF-16 unit.
        let line = "中文";
        assert_eq!(line.len(), 6);
        assert_eq!(utf8_byte_to_utf16_column(line, 0), 0);
        assert_eq!(utf8_byte_to_utf16_column(line, 3), 1); // after '中'
        assert_eq!(utf8_byte_to_utf16_column(line, 6), 2); // after '文'
    }

    #[test]
    fn supplementary_plane_four_bytes_two_utf16_units() {
        // U+1F600 '😀': 4 UTF-8 bytes, 2 UTF-16 code units (surrogate pair).
        let line = "x😀y";
        assert_eq!(line.len(), 6); // 1 + 4 + 1
        assert_eq!(utf8_byte_to_utf16_column(line, 1), 1); // after 'x'
        assert_eq!(utf8_byte_to_utf16_column(line, 5), 3); // after '😀' = 1 + 2
        assert_eq!(utf8_byte_to_utf16_column(line, 6), 4); // after 'y'
    }

    #[test]
    fn utf16_to_byte_round_trips_ascii() {
        let line = "hello";
        for byte in 0..=line.len() as u32 {
            let col = utf8_byte_to_utf16_column(line, byte);
            assert_eq!(utf16_column_to_utf8_byte(line, col), byte);
        }
    }

    #[test]
    fn utf16_to_byte_round_trips_unicode() {
        let line = "x😀y中z";
        for byte in [0, 1, 5, 6, 9, 10] {
            let col = utf8_byte_to_utf16_column(line, byte);
            assert_eq!(utf16_column_to_utf8_byte(line, col), byte, "byte={byte}");
        }
    }

    #[test]
    fn utf16_to_byte_clamps_past_eol() {
        // Server says character=999; we clamp to line length.
        let line = "abc";
        assert_eq!(utf16_column_to_utf8_byte(line, 999), 3);
    }

    #[test]
    fn utf32_byte_round_trip() {
        let line = "x😀y";
        // 😀 is one codepoint in utf-32.
        // After x: byte=1, col=1
        // After 😀: byte=5, col=2
        // After y: byte=6, col=3
        assert_eq!(utf8_byte_to_utf32_column(line, 0), 0);
        assert_eq!(utf8_byte_to_utf32_column(line, 1), 1);
        assert_eq!(utf8_byte_to_utf32_column(line, 5), 2);
        assert_eq!(utf8_byte_to_utf32_column(line, 6), 3);

        for col in 0..=3 {
            let byte = utf32_column_to_utf8_byte(line, col);
            assert_eq!(utf8_byte_to_utf32_column(line, byte), col);
        }
    }

    #[test]
    fn dispatch_utf8_short_circuits() {
        let line = "x😀y";
        assert_eq!(
            byte_to_lsp_character(line, 5, &PositionEncodingKind::UTF8),
            5,
            "utf-8 mode preserves byte offset"
        );
    }

    #[test]
    fn dispatch_utf16_routes_to_utf16_converter() {
        let line = "x😀y";
        assert_eq!(
            byte_to_lsp_character(line, 5, &PositionEncodingKind::UTF16),
            3
        );
    }

    #[test]
    fn dispatch_utf32_routes_to_utf32_converter() {
        let line = "x😀y";
        assert_eq!(
            byte_to_lsp_character(line, 5, &PositionEncodingKind::UTF32),
            2
        );
    }

    #[test]
    fn one_past_end_is_handled() {
        let line = "ab";
        // byte=2 is one-past-the-end of "ab".
        assert_eq!(utf8_byte_to_utf16_column(line, 2), 2);
        // Past the end -- approximation.
        assert_eq!(utf8_byte_to_utf16_column(line, 5), 5);
    }

    #[test]
    fn empty_line_returns_zero() {
        assert_eq!(utf8_byte_to_utf16_column("", 0), 0);
        assert_eq!(utf16_column_to_utf8_byte("", 0), 0);
    }

    #[test]
    fn non_char_boundary_falls_back_to_safe_walk() {
        // byte=2 inside the multi-byte CJK char '中' (3 bytes).
        // Production code shouldn't pass this, but if it does we
        // mustn't panic. The fallback walks char_indices and
        // counts every char whose start index is < 2; '中' starts
        // at 0 so it counts -> 1 utf-16 unit.
        let line = "中";
        assert_eq!(utf8_byte_to_utf16_column(line, 2), 1);
    }
}
