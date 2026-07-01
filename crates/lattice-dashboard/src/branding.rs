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
    AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowAlign, VirtualRowKind,
    VirtualRowProvider,
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

        // Build each row's cells. The wordmark block (name + tagline) is
        // vertically centred against the five-row mark: name on row 2,
        // tagline on row 3.
        let mut cell_rows: Vec<Vec<Cell>> = Vec::with_capacity(MARK.len() + 1);
        for (i, mark_row) in MARK.iter().enumerate() {
            let mut runs = mark_runs_for(mark_row, logo_fg, cursor_fg);
            match i {
                2 => {
                    runs.push(gap.clone());
                    runs.push(("Lattice".to_string(), title_fg));
                }
                3 => {
                    runs.push(gap.clone());
                    runs.push((TAGLINE.to_string(), tagline_fg));
                }
                _ => {}
            }
            cell_rows.push(row_cells(&runs));
        }
        // Trailing blank spacer between the banner and the body.
        cell_rows.push(Vec::new());

        // Pad every row to the block's max width so per-row centring insets
        // them all equally — the block centres as a unit and the mark's
        // columns stay aligned vertically.
        let block_width = cell_rows.iter().map(|c| c.len()).max().unwrap_or(0);
        cell_rows
            .into_iter()
            .map(|mut cells| {
                cells.resize(block_width, Cell::new(b' ' as u32, 0, 0, 0));
                VirtualRow {
                    anchor_line: 0,
                    position: AnchorPosition::Above,
                    cells: Arc::from(cells.into_boxed_slice()),
                    height: 1,
                    kind: VirtualRowKind::Filler,
                    bg: None,
                    align: VirtualRowAlign::Center,
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

/// The number of display rows the branding block occupies (mark rows + one
/// spacer). Used by tests and future layout.
pub const BRANDING_ROW_COUNT: usize = MARK.len() + 1;

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
    fn wordmark_is_vertically_centered_on_rows_2_and_3() {
        let p = provider_with_theme();
        let rows = p.collect();
        let text_of = |row: &VirtualRow| -> String {
            row.cells
                .iter()
                .filter_map(|c| char::from_u32(c.codepoint))
                .collect()
        };
        assert!(text_of(&rows[2]).contains("Lattice"), "wordmark on row 2");
        assert!(text_of(&rows[3]).contains("modal"), "tagline on row 3");
        // Rows 0/1/4/5 carry no wordmark text.
        for i in [0usize, 1, 4, 5] {
            assert!(!text_of(&rows[i]).contains("Lattice"));
        }
    }

    #[test]
    fn rows_are_padded_to_one_block_width_and_centered() {
        // For the block to centre as a unit (not per-row), every row must be
        // the same width and tagged Center.
        let p = provider_with_theme();
        let rows = p.collect();
        let widths: std::collections::HashSet<usize> =
            rows.iter().map(|r| r.cells.len()).collect();
        assert_eq!(widths.len(), 1, "all rows must share one block width: {widths:?}");
        assert!(rows.iter().all(|r| r.align == VirtualRowAlign::Center));
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
