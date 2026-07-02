//! The branding block (DB.4): the Lattice mark + wordmark, rendered as
//! custom-colored virtual rows anchored above the first document line.
//!
//! The mark is the interlocking "L" bracket from `assets/lattice-mark.svg`
//! (brand blue) with the amber cursor bar inside it, drawn with BMP block
//! glyphs so it renders in every terminal font (the graceful-degradation
//! default per the icon-palette rule; a Nerd-Font variant is a later polish
//! — blocks already degrade perfectly). The "Lattice" wordmark + tagline sit
//! to the right, vertically centred against the mark with a tight gap — the
//! `banner-dark.svg` symmetry.
//!
//! Colours resolve from the `dashboard.*` theme elements (DB.3) at
//! `collect()` time, so themes restyle the banner. Horizontal pane-centring
//! (which needs live pane width fed to the provider) is deferred; the block
//! is left-aligned with a small indent for now (design §5.3 — the icon↔
//! wordmark symmetry, the stated priority, is internal to the block and needs
//! no width).

use std::sync::Arc;

use lattice_cells::{
    AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowKind,
    VirtualRowProvider, BASE_SCALE,
};
use lattice_theme::{Color, ElementId, ResolvedTheme, ThemeRegistryHandle};

use crate::theme::{DashboardElementIds, BRAND_AMBER, BRAND_BLUE};

/// XOR tag mixed into the per-buffer [`ProviderId`] so the dashboard provider
/// never collides with other providers on the same buffer.
pub const DASHBOARD_BRANDING_TAG: u64 = 0xDA5B_0A2D_0000_0000;

/// The tagline (matches the brand assets).
const TAGLINE: &str = "A modal, GPU-accelerated, plugin-first text editor in Rust";
/// Gap (cells) between the mark and the wordmark.
const GAP: usize = 2;
/// The "Lattice" wordmark.
const WORDMARK: &str = "Lattice";
/// F.3 (Thread F): the wordmark's font scale in hundredths (`100` =
/// base). The GPUI peer renders the "Lattice" run larger than the mark
/// blocks and tagline via the per-token virtual-row scaling primitive
/// (`VirtualRow::scales`); the TUI peer ignores it (terminal cells can't
/// vary size) and renders every cell base-size. Kept modest so the shared
/// row's growth stays subtle.
const WORDMARK_SCALE: u16 = 150;

/// The mark as a glyph grid: `L` = logo block, `C` = cursor block, ` ` =
/// empty. Eight columns × five rows: wide-and-short so it renders roughly
/// square (terminal cells are ~2:1 tall, and 8×5 cells ≈ 8×10 visual ≈ the
/// SVG's 100×120). Mirrors the SVG paths — top bar, left + right columns,
/// bottom bar — with the two opposite corners cut (the interlocking look:
/// top-left and bottom-right are open). The amber cursor bar sits in the
/// middle with a gap row above it (the SVG cursor starts below the top bar).
const MARK: [&str; 5] = [
    " LLLLLL", //  top bar (top-left corner cut)
    "L     L", //  gap row ABOVE the cursor
    "L  C  L", //  cursor bar
    "L     L", //  gap row BELOW the cursor
    "LLLLLL ", //  bottom bar (bottom-right corner cut)
];
/// Full block glyph for the mark segments.
const BLOCK: char = '█';

/// A provider that emits the dashboard branding rows for one buffer.
pub struct DashboardBrandingProvider {
    provider_id: ProviderId,
    theme: Option<ThemeRegistryHandle>,
    ids: DashboardElementIds,
}

impl std::fmt::Debug for DashboardBrandingProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DashboardBrandingProvider")
            .field("provider_id", &self.provider_id)
            .field("has_theme", &self.theme.is_some())
            .finish()
    }
}

impl DashboardBrandingProvider {
    pub fn new(
        provider_id: ProviderId,
        theme: Option<ThemeRegistryHandle>,
        ids: DashboardElementIds,
    ) -> Self {
        Self {
            provider_id,
            theme,
            ids,
        }
    }

    /// Derive a stable per-buffer provider id.
    pub fn provider_id_for(buffer_id: u64) -> ProviderId {
        buffer_id ^ DASHBOARD_BRANDING_TAG
    }
}

/// Resolve an element's foreground to a packed `0xRRGGBB`, or `fallback`.
fn fg_or(resolved: Option<&ResolvedTheme>, id: ElementId, fallback: Color) -> u32 {
    resolved
        .map(|r| r.get(id))
        .and_then(|s| s.fg)
        .unwrap_or(fallback)
        .to_rgb_u32(fallback.to_rgb_u32(0))
}

/// Build one row's cells from styled runs. Each run is `(text, fg)`; a blank
/// run uses `fg = 0`.
fn row_cells(runs: &[(String, u32)]) -> Vec<Cell> {
    let mut cells: Vec<Cell> = Vec::new();
    for (text, fg) in runs {
        for ch in text.chars() {
            cells.push(Cell::new(ch as u32, *fg, 0, 0));
        }
    }
    cells
}

impl VirtualRowProvider for DashboardBrandingProvider {
    fn id(&self) -> ProviderId {
        self.provider_id
    }

    fn version(&self) -> u64 {
        // Re-collect when the theme changes (colours) — the row set is
        // otherwise static.
        self.theme
            .as_ref()
            .map(|t| t.resolved().version())
            .unwrap_or(0)
    }

    fn collect(&self) -> Vec<VirtualRow> {
        let resolved = self.theme.as_ref().map(|t| t.resolved());
        let r = resolved.as_deref();
        let logo_fg = fg_or(r, self.ids.logo, BRAND_BLUE);
        let cursor_fg = fg_or(r, self.ids.cursor, BRAND_AMBER);
        let title_fg = fg_or(r, self.ids.title, BRAND_BLUE);
        let tagline_fg = fg_or(r, self.ids.tagline, Color::Rgb(0x93, 0x99, 0xb2));

        let gap = (" ".repeat(GAP), 0u32);

        // Build each row's cells + per-column scales. The wordmark block
        // (name + tagline) is vertically centred against the five-row mark:
        // name on row 2, tagline on row 3. A blank spacer row above AND below
        // the mark gives the banner breathing room. Horizontal centring is
        // handled by the gutter (content_left_pad) — rows are left-aligned
        // within the gutter, so the mark's columns line up.
        //
        // F.3: the "Lattice" run carries `WORDMARK_SCALE` in its per-column
        // `scales` (the mark blocks + gap stay base); GPUI renders it larger
        // via the shared-baseline per-token scaling path, the TUI ignores it.
        let mut cell_rows: Vec<(Vec<Cell>, Option<Vec<u16>>)> =
            Vec::with_capacity(MARK.len() + 2);
        cell_rows.push((Vec::new(), None)); // spacer above
        for (i, mark_row) in MARK.iter().enumerate() {
            let mut runs = mark_runs_for(mark_row, logo_fg, cursor_fg);
            // The wordmark row appends a scaled "Lattice" run; every other
            // run (mark blocks, gap, tagline) stays base size.
            let mut wordmark_at: Option<usize> = None;
            match i {
                2 => {
                    runs.push(gap.clone());
                    wordmark_at = Some(runs.iter().map(|(t, _)| t.chars().count()).sum());
                    runs.push((WORDMARK.to_string(), title_fg));
                }
                3 => {
                    runs.push(gap.clone());
                    runs.push((TAGLINE.to_string(), tagline_fg));
                }
                _ => {}
            }
            let cells = row_cells(&runs);
            let scales = wordmark_at.map(|start| {
                let mut s = vec![BASE_SCALE; cells.len()];
                for slot in s.iter_mut().skip(start).take(WORDMARK.chars().count()) {
                    *slot = WORDMARK_SCALE;
                }
                s
            });
            cell_rows.push((cells, scales));
        }
        cell_rows.push((Vec::new(), None)); // spacer below

        cell_rows
            .into_iter()
            .map(|(cells, scales)| {
                VirtualRow {
                    anchor_line: 0,
                    position: AnchorPosition::Above,
                    cells: Arc::from(cells.into_boxed_slice()),
                    height: 1,
                    // BrandingBlock (DB.4-gpui): the GPUI peer intercepts this
                    // row group and paints a 2-D composition (quad mark +
                    // shaped wordmark); the TUI paints the cells as-is. No
                    // backdrop either way (was Filler).
                    kind: VirtualRowKind::BrandingBlock,
                    bg: None,
                    scales: scales.map(|s| Arc::from(s.into_boxed_slice())),
                }
            })
            .collect()
    }
}

/// Split one mark grid row into colored runs (coalescing adjacent same-color
/// glyphs). `L` → logo block, `C` → cursor block, ` ` → blank.
fn mark_runs_for(mark_row: &str, logo_fg: u32, cursor_fg: u32) -> Vec<(String, u32)> {
    let mut runs: Vec<(String, u32)> = Vec::new();
    let push = |ch: char, fg: u32, runs: &mut Vec<(String, u32)>| {
        if let Some(last) = runs.last_mut()
            && last.1 == fg
        {
            last.0.push(ch);
        } else {
            runs.push((ch.to_string(), fg));
        }
    };
    for c in mark_row.chars() {
        match c {
            'L' => push(BLOCK, logo_fg, &mut runs),
            'C' => push(BLOCK, cursor_fg, &mut runs),
            _ => push(' ', 0, &mut runs),
        }
    }
    runs
}

/// The number of display rows the branding block occupies (mark rows + a
/// spacer above and below). Used by tests and future layout.
pub const BRANDING_ROW_COUNT: usize = MARK.len() + 2;

/// The branding block width in cells (mark + gap + the wider of wordmark /
/// tagline). The host feeds this into the content-centring block width so the
/// banner and body share one centred margin.
pub fn branding_block_width() -> u32 {
    let mark_w = MARK.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    let text_w = "Lattice".chars().count().max(TAGLINE.chars().count());
    (mark_w + GAP + text_w) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::register_dashboard_theme_elements;
    use lattice_theme::{default_palette, ElementOwner, InMemoryThemeRegistry};

    fn provider_with_theme() -> DashboardBrandingProvider {
        let reg: ThemeRegistryHandle = Arc::new(InMemoryThemeRegistry::new(default_palette()));
        let ids =
            register_dashboard_theme_elements(reg.as_ref(), ElementOwner::Mode("dashboard-mode".into()));
        DashboardBrandingProvider::new(0xABCD, Some(reg), ids)
    }

    #[test]
    fn emits_all_mark_rows_plus_spacer() {
        let p = provider_with_theme();
        let rows = p.collect();
        assert_eq!(rows.len(), BRANDING_ROW_COUNT);
        for row in &rows {
            assert_eq!(row.anchor_line, 0);
            assert_eq!(row.position, AnchorPosition::Above);
            assert_eq!(row.height, 1);
        }
    }

    #[test]
    fn mark_uses_brand_colors() {
        let p = provider_with_theme();
        let rows = p.collect();
        let all_fgs: std::collections::HashSet<u32> = rows
            .iter()
            .flat_map(|r| r.cells.iter())
            .filter(|c| c.codepoint == BLOCK as u32)
            .map(|c| c.fg)
            .collect();
        // The mark carries both brand blue (logo) and brand amber (cursor).
        assert!(all_fgs.contains(&BRAND_BLUE.to_rgb_u32(0)), "logo blue present");
        assert!(all_fgs.contains(&BRAND_AMBER.to_rgb_u32(0)), "cursor amber present");
    }

    #[test]
    fn wordmark_is_vertically_centered_against_the_mark() {
        // Row 0 is the spacer above, so MARK rows are 1..=5 and the wordmark
        // (MARK row 2 / 3) lands on rows 3 / 4.
        let p = provider_with_theme();
        let rows = p.collect();
        let text_of = |row: &VirtualRow| -> String {
            row.cells
                .iter()
                .filter_map(|c| char::from_u32(c.codepoint))
                .collect()
        };
        assert!(text_of(&rows[3]).contains("Lattice"), "wordmark on row 3");
        assert!(text_of(&rows[4]).contains("modal"), "tagline on row 4");
    }

    #[test]
    fn wordmark_run_carries_scale_over_base_mark_and_gap() {
        // F.3: the "Lattice" run scales (WORDMARK_SCALE); the mark blocks,
        // gap, and everything else on the row stay base. Only row 3 (the
        // wordmark row, after the spacer) carries a `scales` channel.
        let p = provider_with_theme();
        let rows = p.collect();
        let wordmark_row = &rows[3];
        let scales = wordmark_row
            .scales
            .as_ref()
            .expect("wordmark row carries per-column scales");
        assert_eq!(scales.len(), wordmark_row.cells.len());
        // The scaled columns are exactly the "Lattice" run.
        let scaled = scales.iter().filter(|s| **s == WORDMARK_SCALE).count();
        assert_eq!(scaled, WORDMARK.chars().count(), "only the wordmark scales");
        // The leading mark blocks + gap are base size.
        assert_eq!(scales[0], BASE_SCALE, "mark blocks stay base size");
        // The tagline row (row 4) and mark-only rows carry no scale channel.
        assert!(rows[4].scales.is_none(), "tagline stays base (no scale channel)");
        assert!(rows[1].scales.is_none(), "mark-only row has no scale channel");
    }

    #[test]
    fn has_spacer_rows_above_and_below() {
        // Horizontal centring is gutter-based (content_left_pad, host-side);
        // the rows are plain left-aligned cells so the mark's columns line up.
        let p = provider_with_theme();
        let rows = p.collect();
        assert!(rows.first().unwrap().cells.is_empty(), "spacer above");
        assert!(rows.last().unwrap().cells.is_empty(), "spacer below");
    }

    #[test]
    fn falls_back_to_literal_brand_colors_without_theme() {
        // No theme service: still renders with the literal brand colours.
        let ids = DashboardElementIds {
            logo: ElementId::INVALID,
            cursor: ElementId::INVALID,
            title: ElementId::INVALID,
            tagline: ElementId::INVALID,
            section: ElementId::INVALID,
            key: ElementId::INVALID,
            link: ElementId::INVALID,
            body: ElementId::INVALID,
        };
        let p = DashboardBrandingProvider::new(1, None, ids);
        let rows = p.collect();
        let has_blue = rows
            .iter()
            .flat_map(|r| r.cells.iter())
            .any(|c| c.fg == BRAND_BLUE.to_rgb_u32(0));
        assert!(has_blue, "brand blue present even without a theme");
    }
}
