//! S4.final.a (2026-05-27): per-codepoint glyph-id cache.
//!
//! GPUI 0.2.2's [`Window::paint_glyph`] takes a `(FontId,
//! GlyphId)` pair. To avoid `WindowTextSystem::shape_line` on
//! the active-pane document hot path, we cache the `char →
//! GlyphId` mapping per resolved [`FontId`]. The miss path
//! (added in S4.final.b together with the `paint_cells` wiring)
//! lays out a single-char line via
//! [`WindowTextSystem::layout_line`] and extracts the resulting
//! glyph id — paid for once per `(font_id, char)` per window
//! lifetime, not once per paint.
//!
//! ## Why a cache, not GPUI's own atlas
//!
//! GPUI already has a per-window sprite atlas keyed by
//! `RenderGlyphParams { font_id, glyph_id, font_size,
//! subpixel_variant, scale_factor, is_emoji }` — that's the GPU
//! texture cache. The atlas misses by GlyphId. To skip
//! `shape_line` we need to know the GlyphId without shaping —
//! and there's no public `char → GlyphId` API on `TextSystem`.
//!
//! [`GlyphResolver`] is the missing piece: a software cache
//! that converts `(FontId, char)` → `Option<ResolvedGlyph>` so
//! the cell-paint loop can hand `(font_id, glyph_id)` straight
//! to `paint_glyph` without re-running the text layout
//! pipeline.
//!
//! ## Cache states
//!
//! The cache stores `Option<ResolvedGlyph>` rather than
//! `ResolvedGlyph`:
//!
//! - `Some(Some(resolved))` — a previous query returned a real
//!   glyph (possibly via font fallback, see [`ResolvedGlyph`]).
//! - `Some(None)` — a previous query confirmed the codepoint is
//!   genuinely unresolvable (no fallback font carries it; the
//!   `.notdef` glyph would be the only output). Sticky: future
//!   lookups stay `None` and the paint loop is free to draw a
//!   tofu / box placeholder without re-querying.
//! - `None` — never queried.
//!
//! Wiring this resolver into [`crate::GpuiApp`] and producing
//! the miss path that calls `layout_line` lands in S4.final.b
//! alongside the `paint_cells` function that consumes it.

#![cfg(feature = "window")]

use std::collections::HashMap;

use gpui::{Font, FontId, GlyphId, Pixels, Rgba, TextRun, Window};

/// Cache key for [`GlyphResolver`]. A resolved [`FontId`]
/// already encodes font family + weight + style + features +
/// fallbacks (see [`gpui::TextSystem::resolve_font`]), so only
/// the codepoint is needed alongside it.
#[derive(Clone, Copy, Hash, Eq, PartialEq, Debug)]
pub struct GlyphKey {
    /// The font the caller intends to render in. Comes from
    /// `TextSystem::resolve_font(&font_with_weight_and_style)`,
    /// so two cells with the same family but different BOLD /
    /// ITALIC bits hash to different keys (the resolver returns
    /// different FontIds for them).
    pub font_id: FontId,
    /// The codepoint to resolve. `cell.codepoint` is `u32`; the
    /// caller maps invalid scalars to a placeholder ('?') or
    /// skips them before reaching the cache.
    pub ch: char,
}

/// A successful resolution returns the font the glyph was
/// actually drawn from (which may differ from `key.font_id`
/// when the system selected a fallback for non-Latin / emoji
/// codepoints — handled in S4.final.e) plus the
/// font-internal `GlyphId` and whether the glyph is colour
/// (emoji vs monochrome — selects between `Window::paint_emoji`
/// and `Window::paint_glyph`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResolvedGlyph {
    pub font_id: FontId,
    pub glyph_id: GlyphId,
    /// `true` when the renderer should use `paint_emoji`
    /// (colour glyph), `false` for `paint_glyph` (monochrome,
    /// tinted by the cell's fg colour).
    pub is_emoji: bool,
}

/// Per-window glyph-id cache. Cheap to construct (empty
/// HashMap) and intended to live on [`crate::GpuiApp`] from
/// S4.final.b onward.
///
/// Single-window editors share one resolver across panes. The
/// cache is not LRU-bounded today: a typical buffer touches at
/// most a few thousand distinct codepoints, each entry is two
/// `usize`-class fields plus a `char`, so the working-set
/// memory is on the order of low hundreds of KB. If the cache
/// ever grows pathologically (locale-mixed buffers, repeated
/// font switches), an LRU cap can land as a follow-up — the
/// public surface is designed to allow it.
#[derive(Default, Debug)]
pub struct GlyphResolver {
    cache: HashMap<GlyphKey, Option<ResolvedGlyph>>,
}

impl GlyphResolver {
    /// Construct an empty resolver. Equivalent to
    /// `GlyphResolver::default()`; provided as a constructor for
    /// API symmetry with `Vec::new` etc.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up `key` in the cache.
    ///
    /// Return-value semantics:
    /// - `Some(Some(resolved))` — cache hit, glyph is known.
    /// - `Some(None)` — cache hit, codepoint is known
    ///   unresolvable (sticky).
    /// - `None` — cache miss; caller should run the resolve
    ///   path and then [`Self::insert`] the result.
    ///
    /// Returns by value because [`ResolvedGlyph`] is `Copy` (16
    /// bytes — two `usize` plus a `bool` plus padding), so
    /// borrowing the cache entry is more typing for no win.
    pub fn get_cached(&self, key: GlyphKey) -> Option<Option<ResolvedGlyph>> {
        self.cache.get(&key).copied()
    }

    /// Record a resolution. `value: Some(...)` for a successful
    /// resolve; `value: None` to mark the codepoint
    /// unresolvable. Re-inserting the same key overwrites — a
    /// later resolve attempt that finds a glyph supersedes a
    /// prior `None`, in the rare case a fallback font is added
    /// mid-session.
    pub fn insert(&mut self, key: GlyphKey, value: Option<ResolvedGlyph>) {
        self.cache.insert(key, value);
    }

    /// Number of distinct `(FontId, char)` keys observed so
    /// far. Useful for diagnostics / bench (S5) where we want
    /// to track resolver coverage growth.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    /// `true` when no keys have been observed.
    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    /// Drop all cached resolutions. Reserved for theme / font
    /// reloads where the underlying [`FontId`] mapping may have
    /// shifted; not used on the hot path.
    pub fn clear(&mut self) {
        self.cache.clear();
    }

    /// Resolve `ch` for the given `font` at `font_size`, hitting
    /// the cache when possible and falling back to
    /// `WindowTextSystem::layout_line` on miss. S4.final.b.
    ///
    /// The miss path lays out a single-char string in a
    /// throwaway [`TextRun`] (colour irrelevant — GPUI's layout
    /// cache keys by font + size + text, not run colour), then
    /// reads the first glyph from the resulting [`LineLayout`]:
    /// - `runs[0].font_id` may differ from
    ///   `text_system.resolve_font(font)` if the system chose a
    ///   fallback face for the codepoint. We cache that
    ///   resolved id on [`ResolvedGlyph`] so paint can dispatch
    ///   to the right font without re-laying-out.
    /// - `runs[0].glyphs[0].id` is the per-font glyph index
    ///   `paint_glyph` / `paint_emoji` need.
    /// - `runs[0].glyphs[0].is_emoji` selects between
    ///   `paint_glyph` (monochrome, tinted by fg) and
    ///   `paint_emoji` (colour glyph, fg ignored).
    ///
    /// Codepoints with no glyph in any fallback font (`layout`'s
    /// `runs` empty or `glyphs` empty) cache as `None` and
    /// future lookups stay sticky-`None` — see [`Self::insert`].
    pub fn resolve(
        &mut self,
        ch: char,
        font: &Font,
        font_size: Pixels,
        window: &mut Window,
    ) -> Option<ResolvedGlyph> {
        let font_id = window.text_system().resolve_font(font);
        let key = GlyphKey { font_id, ch };
        if let Some(cached) = self.cache.get(&key) {
            return *cached;
        }
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        let run = TextRun {
            len: s.len(),
            font: font.clone(),
            color: Rgba::default().into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let layout = window
            .text_system()
            .layout_line(s, font_size, &[run], None);
        let resolved = layout.runs.first().and_then(|r| {
            r.glyphs.first().map(|g| ResolvedGlyph {
                font_id: r.font_id,
                glyph_id: g.id,
                is_emoji: g.is_emoji,
            })
        });
        self.cache.insert(key, resolved);
        resolved
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// FontId / GlyphId are tuple-struct wrappers around
    /// `usize` / `u32`. Tests synthesise raw ids rather than
    /// going through `TextSystem::resolve_font` (which needs a
    /// live GPUI window) — the cache is a pure data structure.
    fn font(id: usize) -> FontId {
        FontId(id)
    }

    fn key(font_id: usize, ch: char) -> GlyphKey {
        GlyphKey {
            font_id: font(font_id),
            ch,
        }
    }

    fn resolved(font_id: usize, glyph: u32, is_emoji: bool) -> ResolvedGlyph {
        ResolvedGlyph {
            font_id: font(font_id),
            glyph_id: glyph_id(glyph),
            is_emoji,
        }
    }

    /// `GlyphId` is `#[repr(C)] pub struct GlyphId(pub(crate)
    /// u32);` — we can't construct it directly from outside the
    /// crate, so tests round-trip through a transmute. Safe
    /// because `GlyphId` is `#[repr(C)]` with a single `u32`
    /// field and `Copy`. The crate-level `-D unsafe-code` is
    /// scoped down with `#[allow(unsafe_code)]` for this single
    /// test helper.
    #[allow(unsafe_code)]
    fn glyph_id(raw: u32) -> GlyphId {
        // SAFETY: `GlyphId` is `#[repr(C)]` with a single
        // `pub(crate) u32` field; this is a no-op
        // reinterpretation of the same bit pattern.
        unsafe { std::mem::transmute::<u32, GlyphId>(raw) }
    }

    /// Empty resolver — every lookup is a miss.
    #[test]
    fn empty_resolver_misses_every_key() {
        let r = GlyphResolver::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert_eq!(r.get_cached(key(1, 'a')), None);
    }

    /// Insert then get — round-trip is exact.
    #[test]
    fn insert_then_get_returns_same_resolved_glyph() {
        let mut r = GlyphResolver::new();
        let k = key(1, 'a');
        let v = resolved(1, 42, false);
        r.insert(k, Some(v));
        assert_eq!(r.get_cached(k), Some(Some(v)));
        assert_eq!(r.len(), 1);
    }

    /// `None` is sticky — a recorded unresolvable lookup stays
    /// `Some(None)` on re-query.
    #[test]
    fn none_resolution_is_sticky() {
        let mut r = GlyphResolver::new();
        let k = key(1, '\u{f00d}'); // private-use codepoint
        r.insert(k, None);
        assert_eq!(r.get_cached(k), Some(None));
        // Re-querying does not flip back to a fresh miss.
        assert_eq!(r.get_cached(k), Some(None));
    }

    /// Different chars at the same FontId are distinct keys.
    #[test]
    fn different_chars_are_distinct_keys() {
        let mut r = GlyphResolver::new();
        let a = key(1, 'a');
        let b = key(1, 'b');
        r.insert(a, Some(resolved(1, 1, false)));
        r.insert(b, Some(resolved(1, 2, false)));
        assert_eq!(r.get_cached(a), Some(Some(resolved(1, 1, false))));
        assert_eq!(r.get_cached(b), Some(Some(resolved(1, 2, false))));
        assert_eq!(r.len(), 2);
    }

    /// Same char at different FontIds is distinct keys (e.g. a
    /// BOLD variant resolves to a different FontId than the
    /// regular face). Locks the
    /// "FontId-discriminates-bold-italic" property the
    /// `cells_paint::cell_to_text_run` flow depends on.
    #[test]
    fn different_font_ids_are_distinct_keys_for_same_char() {
        let mut r = GlyphResolver::new();
        let regular = key(1, 'a');
        let bold = key(2, 'a');
        r.insert(regular, Some(resolved(1, 100, false)));
        r.insert(bold, Some(resolved(2, 200, false)));
        assert_eq!(r.get_cached(regular), Some(Some(resolved(1, 100, false))));
        assert_eq!(r.get_cached(bold), Some(Some(resolved(2, 200, false))));
    }

    /// Re-insert overwrites. A later resolve that finds a
    /// glyph supersedes a prior `None` — the rare case of a
    /// fallback font being added mid-session.
    #[test]
    fn re_insert_overwrites() {
        let mut r = GlyphResolver::new();
        let k = key(1, '\u{1f600}'); // emoji codepoint
        r.insert(k, None);
        assert_eq!(r.get_cached(k), Some(None));
        let v = resolved(7, 99, true);
        r.insert(k, Some(v));
        assert_eq!(r.get_cached(k), Some(Some(v)));
        assert_eq!(r.len(), 1, "overwrite keeps the same key, not adds a new one");
    }

    /// `ResolvedGlyph` carries `is_emoji` so the paint loop can
    /// dispatch to `paint_emoji` vs `paint_glyph`. The flag
    /// round-trips through the cache.
    #[test]
    fn resolved_glyph_preserves_is_emoji_flag() {
        let mut r = GlyphResolver::new();
        let mono = key(1, 'a');
        let emoji = key(1, '\u{1f600}');
        r.insert(mono, Some(resolved(1, 1, false)));
        r.insert(emoji, Some(resolved(1, 2, true)));
        let r_mono = r.get_cached(mono).flatten().unwrap();
        let r_emoji = r.get_cached(emoji).flatten().unwrap();
        assert!(!r_mono.is_emoji);
        assert!(r_emoji.is_emoji);
    }

    /// `ResolvedGlyph.font_id` may differ from the lookup
    /// `key.font_id` when font fallback lands on a different
    /// face. Locks the type's ability to record that.
    #[test]
    fn resolved_glyph_records_fallback_font_id() {
        let mut r = GlyphResolver::new();
        let requested = key(1, '\u{4e2d}'); // CJK ideograph
        // Suppose the system fell back to FontId 42 (e.g. a CJK
        // font in the fallback chain) and resolved to glyph 7.
        r.insert(requested, Some(resolved(42, 7, false)));
        let resolved_glyph = r.get_cached(requested).flatten().unwrap();
        assert_ne!(resolved_glyph.font_id, requested.font_id);
        assert_eq!(resolved_glyph.font_id, font(42));
        assert_eq!(resolved_glyph.glyph_id, glyph_id(7));
    }

    /// `clear` empties the cache without affecting subsequent
    /// inserts.
    #[test]
    fn clear_empties_the_cache() {
        let mut r = GlyphResolver::new();
        r.insert(key(1, 'a'), Some(resolved(1, 1, false)));
        r.insert(key(1, 'b'), Some(resolved(1, 2, false)));
        assert_eq!(r.len(), 2);
        r.clear();
        assert!(r.is_empty());
        // Subsequent inserts continue to work.
        r.insert(key(1, 'a'), Some(resolved(1, 99, false)));
        assert_eq!(r.len(), 1);
        assert_eq!(
            r.get_cached(key(1, 'a')),
            Some(Some(resolved(1, 99, false)))
        );
    }
}
