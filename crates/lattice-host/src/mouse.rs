//! MO.2: editor-body mouse hit-testing.
//!
//! `ui.mouse` has existed since MO.1, but only modeline elements
//! listened to it — the TUI's event handler returned early on anything
//! that was not a left-press on a modeline zone, and the GPUI peer had
//! hit-test primitives with no listener at all. This module is the
//! shared half of giving the editor body scroll, click-to-position and
//! drag-to-select.
//!
//! ## What lives here, and what does not
//!
//! Here: the pane hit map (which pane owns a screen cell, and where its
//! text starts) and the semantic target a resolved gesture produces.
//! Both are renderer-neutral.
//!
//! Not here: how a renderer arrives at a cell. The TUI reads
//! `(column, row)` straight off a crossterm event; GPUI divides pixels
//! by a glyph advance. That geometry belongs to each peer, and the
//! `ModelineHitMap` beside this one draws the line in the same place.
//!
//! ## Recorded, not re-derived
//!
//! The zones are pushed by the renderer **during paint**, and cleared at
//! the top of every frame — exactly like [`crate::modeline::ModelineHitMap`].
//! A map rebuilt from layout inputs after the fact is a second
//! implementation of the layout, free to disagree with the one on screen;
//! the symptom of a disagreement is a click landing a pane away, which
//! reads as a broken feature rather than as stale geometry. A pane that
//! stops painting stops being clickable, because nothing pushed a zone
//! for it.

use lattice_core::BufferId;
use lattice_core::ui::pane::PaneId;

/// Vim's `mousescroll` default (`ver:3`): one wheel notch moves three
/// lines. Named rather than inlined so the two call sites (wheel up and
/// wheel down) cannot drift, and so the eventual option has an obvious
/// thing to replace.
pub const MOUSE_SCROLL_LINES: u32 = 3;

/// One pane's painted body, recorded for hit-testing.
///
/// The rect is the pane's **content** area — the status footer is
/// excluded, so a click on a status line is not a click in the buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneHitZone {
    pub pane_id: PaneId,
    pub buffer_id: BufferId,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    /// Columns occupied by the gutter (line numbers, sign column, pad)
    /// before the first text cell, relative to `x`.
    ///
    /// Recorded rather than recomputed because the gutter width depends
    /// on the buffer's line count, `number` / `signcolumn` resolution
    /// and the centring pad — four inputs the painter has already
    /// resolved, and a click one cell off is exactly what a fifth
    /// resolution of them produces.
    pub text_left: u16,
    /// The pane's first visible source line at paint time.
    pub scroll: u32,
    /// First visible display column (horizontal scroll). Always 0
    /// under soft wrap, which is why the column arithmetic can add it
    /// unconditionally.
    pub leftcol: u32,
}

impl PaneHitZone {
    /// Does this zone cover `(col, row)`?
    pub fn covers(&self, col: u16, row: u16) -> bool {
        col >= self.x
            && col < self.x.saturating_add(self.width)
            && row >= self.y
            && row < self.y.saturating_add(self.height)
    }
}

/// Which part of the buffer a painted row came from.
///
/// `segment` is the soft-wrap segment index: the second visual row of a
/// wrapped line is `segment: 1`, and its columns start
/// `segment * body_width` into the logical line. Without it every
/// wrapped row would resolve to the line's first `body_width` columns,
/// so clicking the tail of a wrapped paragraph would land near its
/// start — wrong in a way that looks like an off-by-a-lot rather than
/// like a missing concept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowOrigin {
    pub source_line: u32,
    pub segment: u32,
}

/// Every pane body painted this frame.
///
/// Small (one entry per visible pane, so single digits) and walked
/// linearly — a click is a human gesture, and an index would cost more
/// to maintain than it saves.
#[derive(Debug, Clone, Default)]
pub struct PaneHitMap {
    zones: Vec<PaneHitZone>,
    /// Per-pane row → buffer origin, in painted order.
    ///
    /// Recorded by the compose loop rather than derived from scroll and
    /// the fold list, because a painted row is not `scroll + n`: soft
    /// wrap splits one line across several, closed folds skip interiors,
    /// and virtual rows (sticky context, inline diagnostics) occupy rows
    /// that mirror no line at all. Re-deriving all three is a second
    /// implementation of the compose loop, and the one on screen is the
    /// one that is right.
    ///
    /// `None` marks a row with no source line — a virtual row, or the
    /// `~` filler past the end of the buffer.
    rows: std::collections::HashMap<PaneId, Vec<Option<RowOrigin>>>,
}

impl PaneHitMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every recorded zone. Called at the top of each frame; a
    /// stale map would route clicks against a layout no longer painted.
    pub fn clear(&mut self) {
        self.zones.clear();
        self.rows.clear();
    }

    /// Record what the compose loop painted into `pane`, row by row.
    ///
    /// Called from the composer rather than from the layout pass, which
    /// is why it is a separate call from [`Self::push`]: the zone's rect
    /// is known before the pane's contents are composed, and its rows
    /// only after.
    pub fn set_rows(&mut self, pane_id: PaneId, rows: Vec<Option<RowOrigin>>) {
        self.rows.insert(pane_id, rows);
    }

    /// The buffer origin of `row_offset` within `pane`, if it has one.
    ///
    /// A row past the end of the buffer, or a virtual row, falls back to
    /// the **last row above it that does** have an origin. Clicking the
    /// blank space below a short buffer puts the cursor on its last line,
    /// which is what every editor does and what a terminal makes the
    /// common case — it reports a cell for every row on screen, not only
    /// the ones with text under them.
    pub fn origin_at(&self, pane_id: PaneId, row_offset: u16) -> Option<RowOrigin> {
        let rows = self.rows.get(&pane_id)?;
        let upto = rows.get(..=(row_offset as usize)).unwrap_or(rows);
        upto.iter().rev().find_map(|r| *r)
    }

    pub fn push(&mut self, zone: PaneHitZone) {
        // A zero-area pane can never be hit and would only lengthen the
        // walk. Mirrors `ModelineHitMap::push`'s inverted-region guard.
        if zone.width > 0 && zone.height > 0 {
            self.zones.push(zone);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    pub fn len(&self) -> usize {
        self.zones.len()
    }

    /// The pane under `(col, row)`, if any.
    ///
    /// Last match wins, so a pane painted over another — a popup body
    /// above a document — takes the click. Panes are pushed in paint
    /// order, which makes later-painted mean on-top without the map
    /// needing a z-index.
    pub fn hit(&self, col: u16, row: u16) -> Option<PaneHitZone> {
        self.zones
            .iter()
            .rev()
            .find(|z| z.covers(col, row))
            .copied()
    }

    /// Resolve a screen cell to a position inside a pane's body.
    ///
    /// `None` when the cell is outside every pane. A cell on the gutter
    /// resolves to its pane with `text_col: None` — the pane is still
    /// the right scroll target for a wheel event there, and a click on a
    /// line number is a real gesture with its own meaning (fold toggle,
    /// eventually) rather than a miss.
    pub fn resolve(&self, col: u16, row: u16) -> Option<BodyHit> {
        let zone = self.hit(col, row)?;
        let within = col - zone.x;
        let row_offset = row - zone.y;
        Some(BodyHit {
            zone,
            row_offset,
            text_col: within.checked_sub(zone.text_left),
            origin: self.origin_at(zone.pane_id, row_offset),
        })
    }
}

/// A screen cell resolved against the painted layout.
///
/// Still in *display* space: `row_offset` counts painted rows, which
/// under soft wrap or a closed fold is not a source line, and `text_col`
/// counts display columns, which inlays and conceals shift away from
/// source positions. Turning those into a buffer position is the
/// renderer's next step, and it does it by inverting the same forward
/// maps the caret is drawn with — see `docs/dev/architecture/mouse.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BodyHit {
    pub zone: PaneHitZone,
    /// Rows below the top of the pane's content area.
    pub row_offset: u16,
    /// Display columns right of the first text cell, or `None` when the
    /// cell is on the gutter.
    pub text_col: Option<u16>,
    /// Which logical line (and wrap segment) this row was painted from.
    /// `None` when the pane recorded no rows at all — a pane composed
    /// before this frame's paint, or one whose content path does not go
    /// through the shared composer.
    pub origin: Option<RowOrigin>,
}

impl BodyHit {
    /// The display column within the LOGICAL line, undoing soft wrap and
    /// horizontal scroll.
    ///
    /// `None` on the gutter or on a row with no source line. The two
    /// terms are exclusive in practice — `leftcol` is forced to 0 under
    /// wrap — so adding both is correct rather than merely convenient.
    pub fn logical_col(&self, body_width: u32) -> Option<u32> {
        let text_col = self.text_col? as u32;
        let origin = self.origin?;
        Some(origin.segment * body_width + text_col + self.zone.leftcol)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn zone(pane: u32, x: u16, y: u16, w: u16, h: u16) -> PaneHitZone {
        PaneHitZone {
            pane_id: PaneId(pane),
            buffer_id: BufferId(pane),
            x,
            y,
            width: w,
            height: h,
            text_left: 4,
            scroll: 0,
            leftcol: 0,
        }
    }

    #[test]
    fn a_cell_outside_every_pane_resolves_to_nothing() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 10));
        assert!(map.resolve(50, 5).is_none(), "right of the pane");
        assert!(map.resolve(10, 20).is_none(), "below the pane");
    }

    /// A vertical split: the column decides which pane takes the click.
    /// This is the whole reason the map is keyed on a rect rather than
    /// on "the active pane".
    #[test]
    fn side_by_side_panes_split_on_the_column() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 10));
        map.push(zone(2, 40, 0, 40, 10));

        assert_eq!(map.resolve(10, 5).unwrap().zone.pane_id, PaneId(1));
        assert_eq!(map.resolve(50, 5).unwrap().zone.pane_id, PaneId(2));
        assert_eq!(
            map.resolve(39, 5).unwrap().zone.pane_id,
            PaneId(1),
            "the boundary column belongs to the left pane"
        );
        assert_eq!(
            map.resolve(40, 5).unwrap().zone.pane_id,
            PaneId(2),
            "…and the next one to the right pane"
        );
    }

    /// The gutter is inside the pane but outside the text. It resolves
    /// to the pane — a wheel event there still scrolls it — with no text
    /// column, so a click cannot be mistaken for one on column 0.
    #[test]
    fn a_cell_on_the_gutter_has_no_text_column() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 10));

        let on_gutter = map.resolve(2, 3).unwrap();
        assert_eq!(on_gutter.zone.pane_id, PaneId(1));
        assert_eq!(on_gutter.text_col, None);

        let first_text_cell = map.resolve(4, 3).unwrap();
        assert_eq!(first_text_cell.text_col, Some(0));
        assert_eq!(map.resolve(9, 3).unwrap().text_col, Some(5));
    }

    /// Row and column are both relative to the pane, not the screen —
    /// a split pane's second row is row 1 of that pane.
    #[test]
    fn offsets_are_relative_to_the_pane_not_the_screen() {
        let mut map = PaneHitMap::new();
        map.push(zone(2, 40, 12, 40, 10));

        let hit = map.resolve(48, 15).unwrap();
        assert_eq!(hit.row_offset, 3);
        assert_eq!(hit.text_col, Some(4));
    }

    /// A pane painted later sits on top, so it takes the click. That is
    /// what makes a popup body clickable without the map carrying a
    /// z-index.
    #[test]
    fn a_later_pane_wins_an_overlap() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 80, 24));
        map.push(zone(2, 10, 5, 20, 8));

        assert_eq!(map.resolve(15, 7).unwrap().zone.pane_id, PaneId(2));
        assert_eq!(
            map.resolve(5, 7).unwrap().zone.pane_id,
            PaneId(1),
            "outside the overlay, the pane underneath still answers"
        );
    }

    /// Clearing is what makes a pane that stops painting stop being
    /// clickable. Without it a closed split keeps taking clicks.
    #[test]
    fn clearing_makes_a_vanished_pane_unclickable() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 10));
        assert!(map.resolve(10, 5).is_some());

        map.clear();

        assert!(map.is_empty());
        assert!(map.resolve(10, 5).is_none());
    }

    #[test]
    fn a_zero_area_pane_is_not_recorded() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 0, 10));
        map.push(zone(2, 0, 0, 40, 0));
        assert_eq!(map.len(), 0);
    }

    // ── row origins ──────────────────────────────────────────────────

    fn origin(line: u32, segment: u32) -> Option<RowOrigin> {
        Some(RowOrigin {
            source_line: line,
            segment,
        })
    }

    /// The straightforward case: one painted row per source line.
    #[test]
    fn a_row_resolves_to_the_line_painted_on_it() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 4));
        map.set_rows(
            PaneId(1),
            vec![origin(10, 0), origin(11, 0), origin(12, 0), origin(13, 0)],
        );

        assert_eq!(map.resolve(6, 2).unwrap().origin, origin(12, 0));
    }

    /// **Soft wrap: the second row of a wrapped line is the same line,
    /// segment 1.** The segment is what puts a click on the tail of a
    /// wrapped paragraph near its tail instead of near its head.
    #[test]
    fn a_wrapped_line_keeps_its_line_and_advances_its_segment() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 4));
        map.set_rows(
            PaneId(1),
            vec![origin(7, 0), origin(7, 1), origin(7, 2), origin(8, 0)],
        );

        assert_eq!(map.resolve(6, 0).unwrap().origin, origin(7, 0));
        assert_eq!(map.resolve(6, 2).unwrap().origin, origin(7, 2));
        assert_eq!(map.resolve(6, 3).unwrap().origin, origin(8, 0));
    }

    /// …and the column arithmetic uses it: segment 2 of a 30-column body
    /// starts 60 columns into the logical line.
    #[test]
    fn the_logical_column_accounts_for_the_wrap_segment() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 34, 4));
        map.set_rows(PaneId(1), vec![origin(7, 0), origin(7, 1), origin(7, 2)]);

        // text_left is 4, so a click at screen column 9 is text column 5.
        assert_eq!(map.resolve(9, 0).unwrap().logical_col(30), Some(5));
        assert_eq!(map.resolve(9, 1).unwrap().logical_col(30), Some(35));
        assert_eq!(map.resolve(9, 2).unwrap().logical_col(30), Some(65));
    }

    /// Horizontal scroll shifts the same way. `leftcol` is forced to 0
    /// under wrap, so the two terms never both apply and adding both is
    /// correct rather than merely convenient.
    #[test]
    fn the_logical_column_accounts_for_horizontal_scroll() {
        let mut map = PaneHitMap::new();
        let mut z = zone(1, 0, 0, 34, 4);
        z.leftcol = 100;
        map.push(z);
        map.set_rows(PaneId(1), vec![origin(7, 0)]);

        assert_eq!(map.resolve(9, 0).unwrap().logical_col(30), Some(105));
    }

    /// A virtual row — sticky context, an inline diagnostic — mirrors no
    /// source line, so it falls back to the last row that does rather
    /// than resolving to whatever line happens to be recorded next.
    #[test]
    fn a_virtual_row_falls_back_to_the_line_above_it() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 4));
        map.set_rows(
            PaneId(1),
            vec![origin(4, 0), None, origin(5, 0), origin(6, 0)],
        );

        assert_eq!(
            map.resolve(6, 1).unwrap().origin,
            origin(4, 0),
            "clicking a virtual row acts on the line it is anchored below"
        );
    }

    /// **A click below the end of the buffer lands on its last line.**
    /// A terminal reports a cell for every row on screen, so clicking
    /// the `~` filler is the common case, not a defensive one, and an
    /// inert click there would read as the feature being broken.
    #[test]
    fn a_click_past_the_end_of_the_buffer_lands_on_the_last_line() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 6));
        map.set_rows(
            PaneId(1),
            vec![origin(0, 0), origin(1, 0), None, None, None, None],
        );

        assert_eq!(map.resolve(6, 5).unwrap().origin, origin(1, 0));
    }

    /// A pane whose rows were never recorded resolves its rect but no
    /// origin — the scroll target is still right, and a click is inert
    /// rather than landing somewhere invented.
    #[test]
    fn a_pane_with_no_recorded_rows_has_no_origin() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 4));

        let hit = map.resolve(6, 2).unwrap();
        assert_eq!(hit.zone.pane_id, PaneId(1));
        assert_eq!(hit.origin, None);
        assert_eq!(hit.logical_col(30), None);
    }

    /// …and a buffer whose first rows are all virtual has nothing above
    /// to fall back to, so it stays inert rather than guessing line 0.
    #[test]
    fn a_row_with_nothing_above_it_has_no_origin() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 4));
        map.set_rows(PaneId(1), vec![None, None, origin(0, 0)]);

        assert_eq!(map.resolve(6, 1).unwrap().origin, None);
    }

    /// Rows are per-pane: one pane's listing must not answer for
    /// another's, which is what keying on `PaneId` rather than on a
    /// single vector buys.
    #[test]
    fn each_pane_keeps_its_own_rows() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 4));
        map.push(zone(2, 40, 0, 40, 4));
        map.set_rows(PaneId(1), vec![origin(100, 0), origin(101, 0)]);
        map.set_rows(PaneId(2), vec![origin(7, 0), origin(8, 0)]);

        assert_eq!(map.resolve(6, 1).unwrap().origin, origin(101, 0));
        assert_eq!(map.resolve(46, 1).unwrap().origin, origin(8, 0));
    }

    /// Clearing drops the rows with the zones, so a stale listing cannot
    /// answer for a layout that is no longer painted.
    #[test]
    fn clearing_drops_the_recorded_rows_too() {
        let mut map = PaneHitMap::new();
        map.push(zone(1, 0, 0, 40, 4));
        map.set_rows(PaneId(1), vec![origin(3, 0)]);

        map.clear();

        assert_eq!(map.origin_at(PaneId(1), 0), None);
    }
}
