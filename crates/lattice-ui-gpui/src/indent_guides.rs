//! IG.4: the GPU peer's half of indentation guides.
//!
//! See `docs/dev/architecture/indent-guides.md`.
//!
//! The host publishes which columns carry a guide on which rows
//! ([`lattice_host::indent_guides::IndentGuides`]); this module turns that
//! into paint geometry. It holds no rule about *whether* a column may be
//! painted — that predicate lives once, in the producer, so the two renderer
//! peers cannot drift into disagreeing about it.
//!
//! What does differ from the TUI peer is the mechanism, and it has to: a
//! terminal cell holds one glyph and can only approximate a rule with `│`,
//! while here the guide is a one-pixel quad that joins across rows into a
//! continuous line. Same columns, same active block, different means.
//!
//! Deliberately not `#[cfg(feature = "window")]`, unlike its `paint_cells` /
//! `hit_test` / `glyph_resolver` neighbours: CI runs this crate's tests with
//! `window` **off** and only *builds* with it on (`.github/workflows/ci.yml`,
//! `gpui-window-build`). Gating the module would mean its tests never run in
//! the job that runs tests, which is worse than not having them.
//!
//! The consequence is the `allow(dead_code)` below: without `window` the only
//! consumer of these items — `editor_element`'s paint body — is compiled out,
//! so every one of them looks orphaned. The attribute is scoped to exactly
//! that configuration, so with `window` on a genuinely dead item still warns.
#![cfg_attr(not(feature = "window"), allow(dead_code))]

use lattice_host::indent_guides::GuideMark;

/// Logical width of an ordinary guide rule.
pub(crate) const GUIDE_WIDTH_PX: f32 = 1.0;
/// Logical width of the guide for the block enclosing the cursor. Wider
/// rather than merely brighter so the active block reads at a glance even in
/// a low-contrast theme.
pub(crate) const ACTIVE_GUIDE_WIDTH_PX: f32 = 2.0;

/// One guide to paint on one row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GuideQuad {
    /// Body-local display column, already panned by `leftcol`.
    pub col: u32,
    /// This guide belongs to the block enclosing the cursor.
    pub active: bool,
}

/// Translate a row's marks into paint-space guides.
///
/// Marks scrolled off the left edge are dropped rather than clamped to column
/// zero — a guide pinned to the edge would claim a nesting level that is not
/// on screen. Right-edge clipping is left to the caller, which knows the
/// pane's pixel bounds.
pub(crate) fn visible_guides(
    marks: &[GuideMark],
    active_block: Option<u16>,
    leftcol: u32,
) -> impl Iterator<Item = GuideQuad> + '_ {
    marks.iter().filter_map(move |m| {
        Some(GuideQuad {
            col: (m.col as u32).checked_sub(leftcol)?,
            active: active_block == Some(m.block),
        })
    })
}

/// Width of a guide's quad, in logical pixels.
pub(crate) fn guide_width(active: bool) -> f32 {
    if active {
        ACTIVE_GUIDE_WIDTH_PX
    } else {
        GUIDE_WIDTH_PX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's gutter reservation must equal what THIS peer paints,
    /// for every combination of the two options that change it.
    ///
    /// Lives here rather than in `editor_element` because that module is
    /// `#[cfg(feature = "window")]` and CI runs this crate's tests with
    /// the feature off — a parity test that cannot run in the job that
    /// runs tests is not a parity test. The formula is duplicated in the
    /// assertion for the same reason, and that is exactly what it is
    /// checking: if `gutter_chars` changes without this, they diverge.
    ///
    /// 2026-08-16: this peer painted `sign + digits + 3`, one column
    /// narrower than the TUI's `digits + 1 + 3 + sign`. It was
    /// self-consistent only because the host under-reserved by the same
    /// column; correcting the host would have left GPUI wrapping one
    /// column narrower than its own body.
    #[test]
    fn gutter_cols_matches_the_gpui_gutter() {
        for lines in [1u32, 9, 10, 99, 100, 219, 1000, 12345] {
            for numbers in [true, false] {
                for signs in [true, false] {
                    // Mirrors `EditorElement::prepaint`'s `gutter_chars`:
                    // the digit slot is `to_string().len()`, blank when
                    // `number` is off, plus the leading pad, the trailing
                    // three, and the two sign cells when reserved.
                    let digits = if numbers {
                        lines.max(1).to_string().len() as u32
                    } else {
                        0
                    };
                    let num_pad = u32::from(digits > 0);
                    let painted = digits + num_pad + 3 + if signs { 2 } else { 0 };
                    let reserved = lattice_host::cells_worker::gutter_cols(lines, numbers, signs);
                    assert_eq!(
                        reserved, painted,
                        "lines={lines} number={numbers} signcolumn={signs}: host \
                         reserves {reserved}, this peer paints {painted}"
                    );
                }
            }
        }
    }

    fn marks(cols: &[u16]) -> Vec<GuideMark> {
        cols.iter()
            .enumerate()
            .map(|(i, c)| GuideMark {
                col: *c,
                block: i as u16,
            })
            .collect()
    }

    #[test]
    fn unscrolled_guides_keep_their_columns() {
        let m = marks(&[0, 4, 8]);
        let out: Vec<GuideQuad> = visible_guides(&m, None, 0).collect();
        assert_eq!(out.iter().map(|g| g.col).collect::<Vec<_>>(), vec![0, 4, 8]);
        assert!(out.iter().all(|g| !g.active));
    }

    #[test]
    fn leftcol_pans_guides_and_drops_the_ones_off_screen() {
        let m = marks(&[0, 4, 8]);
        let out: Vec<GuideQuad> = visible_guides(&m, None, 4).collect();
        assert_eq!(
            out.iter().map(|g| g.col).collect::<Vec<_>>(),
            vec![0, 4],
            "the column-0 guide has scrolled off; it is not clamped to the edge"
        );
    }

    #[test]
    fn the_active_block_is_flagged_by_index_not_by_column() {
        // Two blocks can share a column across different line ranges, so the
        // flag has to come from the block index the producer assigned.
        let m = marks(&[0, 4]);
        let out: Vec<GuideQuad> = visible_guides(&m, Some(1), 0).collect();
        assert!(!out[0].active, "the outer block is not the cursor's");
        assert!(out[1].active, "block 1 encloses the cursor");
    }

    #[test]
    fn an_active_block_that_is_scrolled_off_flags_nothing() {
        let m = marks(&[0, 4]);
        let out: Vec<GuideQuad> = visible_guides(&m, Some(0), 4).collect();
        assert_eq!(out.len(), 1);
        assert!(!out[0].active, "block 0 panned off; block 1 is not it");
    }

    #[test]
    fn the_active_guide_is_wider() {
        assert!(guide_width(true) > guide_width(false));
    }
}
