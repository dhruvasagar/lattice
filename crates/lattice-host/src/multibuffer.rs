//! M.2 (2026-05-31): host-side multibuffer rendering primitives.
//!
//! Lives in `lattice-host` because the `VirtualRowProvider`
//! trait is in `lattice-cells` (which `lattice-runtime` doesn't
//! depend on) and the registration sites are the same
//! `Editor.virtual_row_providers` registry used by the diff
//! filler / overlay providers. Multibuffer data model
//! (`MultibufferDocumentHandle`, `Excerpt`, `RowTranslation`)
//! continues to live in `lattice-runtime::multibuffer`.
//!
//! ## What this slice ships (M.2)
//!
//! * `MultibufferHeaderProvider` — a `VirtualRowProvider`
//!   impl that emits one virtual row per excerpt header,
//!   anchored above the excerpt's first composed row.
//! * `multibuffer_header_provider_id(BufferId) -> ProviderId`
//!   — stable id derivation in a dedicated namespace
//!   (`0x_BBBB_0001_*`) parallel to the diff-system
//!   namespaces.
//! * Pure helper `compose_header_rows` taking the excerpt
//!   list + a header-renderer callback so tests can verify
//!   header geometry without exercising the full provider
//!   machinery.
//!
//! ## Anchor row geometry
//!
//! Headers sit `AnchorPosition::Above` the first composed
//! row of their excerpt. For an excerpt list
//! `[e1(rows 0..=2), e2(rows 3..=4), e3(rows 5..=5)]` the
//! header anchors are:
//! ```text
//! e1.header  -> anchor_line = 0, Above
//! e2.header  -> anchor_line = 3, Above
//! e3.header  -> anchor_line = 5, Above
//! ```
//! The renderer interleaves these between the composed-buffer
//! source rows via the existing `DisplaySliceIter` pipeline —
//! the multibuffer pane reads the snapshot's composed `buffer`
//! exactly like a regular document and gets virtual rows from
//! `virtual_rows_matrix_cell` exactly like a regular document
//! does for diff fillers. No multibuffer-specific renderer
//! code; the everything-is-a-buffer principle holds at the
//! rendering boundary too.
//!
//! ## What lands later
//!
//! * **M.7** — excerpt-fold provider on the same anchor lines.
//! * **M.8** — file-boundary fold provider grouping excerpts
//!   per source.
//! * Header-style flourishes (path decoration, severity tags
//!   for the diagnostics provider) — provider-specific
//!   subclasses set `Excerpt::header.title` to the rendered
//!   form; this module just paints whatever string they
//!   produced.

use std::sync::Arc;

use lattice_cells::cell::Cell;
use lattice_cells::virtual_rows::{
    AnchorPosition, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};
use lattice_core::BufferId;
use lattice_runtime::{Document, Excerpt, MultibufferDocumentHandle};

/// Namespace prefix for multibuffer header provider ids.
/// Distinct from the diff filler / overlay namespaces
/// (`0xD1FF_*`) so the two coexist in the global provider
/// registry without collision.
const MULTIBUFFER_HEADER_NAMESPACE: u64 = 0xBBBB_0001_0000_0000;

/// Reproducible `ProviderId` for a multibuffer's header
/// provider. Encodes the multibuffer's own `BufferId` in the
/// low bits so callers can derive the id without holding the
/// provider Arc — the same shape as the diff system's
/// `diff_filler_provider_id`.
pub fn multibuffer_header_provider_id(buffer_id: BufferId) -> ProviderId {
    MULTIBUFFER_HEADER_NAMESPACE | u64::from(buffer_id.0)
}

/// M.2 (2026-05-31): emits one virtual row per excerpt
/// header, anchored above the excerpt's first composed row.
/// The provider holds a cheap-clone reference to the
/// multibuffer handle and re-reads excerpts on each
/// `collect()` — version() is tied to the published
/// snapshot's `version` so the worker short-circuits on
/// cache-hit when nothing's changed.
#[derive(Debug)]
pub struct MultibufferHeaderProvider {
    /// The multibuffer this provider renders headers for.
    multibuffer: MultibufferDocumentHandle,
}

impl MultibufferHeaderProvider {
    pub fn new(multibuffer: MultibufferDocumentHandle) -> Self {
        Self { multibuffer }
    }
}

impl VirtualRowProvider for MultibufferHeaderProvider {
    fn id(&self) -> ProviderId {
        multibuffer_header_provider_id(self.multibuffer.buffer_id())
    }

    fn version(&self) -> u64 {
        // Snapshot.version bumps on every recompose, which is
        // the only thing that can change the excerpt geometry
        // in M.2. M.6 (when providers re-emit excerpts) gains
        // a separate excerpt-list revision counter; for M.2
        // the snapshot version is sufficient.
        self.multibuffer.snapshot().version
    }

    fn collect(&self) -> Vec<VirtualRow> {
        compose_header_rows(self.multibuffer.excerpts(), default_header_cells)
    }
}

/// Pure function from excerpt list → header virtual rows.
/// Each excerpt contributes one row, anchored `Above` its
/// first composed line. Exposed for direct unit testing
/// without round-tripping through a `MultibufferDocument
/// Handle`.
///
/// `render_cells` is the title-to-cells renderer; the
/// default ([`default_header_cells`]) paints
/// `── <title> ──` with box-drawing rules. Tests can pass a
/// stub renderer to exercise geometry without depending on
/// cell-shape assertions.
pub fn compose_header_rows(
    excerpts: &[Excerpt],
    mut render_cells: impl FnMut(&Excerpt) -> Arc<[Cell]>,
) -> Vec<VirtualRow> {
    let mut rows = Vec::with_capacity(excerpts.len());
    let mut composed_cursor: u32 = 0;
    for excerpt in excerpts {
        let cells = render_cells(excerpt);
        rows.push(VirtualRow {
            anchor_line: composed_cursor,
            position: AnchorPosition::Above,
            cells,
            height: 1,
            kind: VirtualRowKind::Generic,
        });
        composed_cursor = composed_cursor.saturating_add(excerpt.line_count());
    }
    rows
}

/// Default header-rendering: `── <title> ──` (box-drawing
/// horizontal rules around the title). Empty title yields a
/// row of box rules only (separator-like).
///
/// M.2 keeps decoration minimal — provider-specific
/// flourishes (severity icons, hunk decoration) compose by
/// setting `Excerpt::header.title` to a pre-decorated string;
/// this renderer just paints whatever's there.
pub fn default_header_cells(excerpt: &Excerpt) -> Arc<[Cell]> {
    let title = &excerpt.header.title;
    let mut cells = Vec::new();
    // Leading rule.
    for _ in 0..2 {
        cells.push(Cell::with_codepoint('─' as u32));
    }
    if !title.is_empty() {
        cells.push(Cell::with_codepoint(' ' as u32));
        for ch in title.chars() {
            cells.push(Cell::with_codepoint(ch as u32));
        }
        cells.push(Cell::with_codepoint(' ' as u32));
    }
    // Trailing rule (short — full-width fill is the
    // renderer's job, padding to pane width).
    for _ in 0..2 {
        cells.push(Cell::with_codepoint('─' as u32));
    }
    Arc::from(cells)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_runtime::{Excerpt, ExcerptHeader, MultibufferDocumentHandle, spawn_document};
    use std::collections::HashMap;

    fn make_sources(
        texts: &[&str],
    ) -> (
        HashMap<BufferId, Arc<dyn Document>>,
        Vec<BufferId>,
    ) {
        let registry = Arc::new(lattice_grammar::CommandRegistry::new());
        let mut map: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
        let mut ids = Vec::new();
        for text in texts {
            let handle = spawn_document(lattice_core::BufferId(0), 
                lattice_core::Document::from_text(*text),
                registry.clone(),
            );
            let id = BufferId::next();
            map.insert(id, Arc::new(handle));
            ids.push(id);
        }
        (map, ids)
    }

    /// `compose_header_rows` produces one row per excerpt,
    /// anchored above the first composed row of that excerpt.
    #[test]
    fn header_rows_anchor_at_each_excerpts_first_composed_row() {
        let mb_source = BufferId::next();
        let excerpts = vec![
            Excerpt::new(mb_source, 0, 2).with_header(ExcerptHeader::new("a")),
            Excerpt::new(mb_source, 0, 1).with_header(ExcerptHeader::new("b")),
            Excerpt::new(mb_source, 0, 0).with_header(ExcerptHeader::new("c")),
        ];
        let rows = compose_header_rows(&excerpts, |_| Arc::from(Vec::<Cell>::new()));

        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].anchor_line, 0);
        assert_eq!(rows[1].anchor_line, 3); // e1 covered rows 0,1,2 → next anchor is 3
        assert_eq!(rows[2].anchor_line, 5); // e2 covered rows 3,4 → next anchor is 5
        for row in &rows {
            assert_eq!(row.position, AnchorPosition::Above);
            assert_eq!(row.height, 1);
            assert_eq!(row.kind, VirtualRowKind::Generic);
        }
    }

    /// `default_header_cells` paints the title surrounded by
    /// box-drawing rules. An empty title still emits rule
    /// cells (separator-like row).
    #[test]
    fn default_header_paints_box_rules_around_title() {
        let mb_source = BufferId::next();
        let with_title = Excerpt::new(mb_source, 0, 0)
            .with_header(ExcerptHeader::new("hi"));
        let cells = default_header_cells(&with_title);
        // ── + space + h + i + space + ──  = 2 + 1 + 2 + 1 + 2 = 8 cells
        assert_eq!(cells.len(), 8);
        assert_eq!(cells[0].codepoint, '─' as u32);
        assert_eq!(cells[3].codepoint, 'h' as u32);
        assert_eq!(cells[4].codepoint, 'i' as u32);

        // Empty title → 4 rule cells only.
        let without_title = Excerpt::new(mb_source, 0, 0);
        let cells = default_header_cells(&without_title);
        assert_eq!(cells.len(), 4);
        for cell in cells.iter() {
            assert_eq!(cell.codepoint, '─' as u32);
        }
    }

    /// End-to-end: build a multibuffer, wrap in the provider,
    /// `collect()` returns the expected header rows.
    #[tokio::test(flavor = "multi_thread")]
    async fn provider_collects_one_row_per_excerpt() {
        let (sources, ids) = make_sources(&["alpha\nbeta\ngamma\n"]);
        let excerpts = vec![
            Excerpt::new(ids[0], 0, 1).with_header(ExcerptHeader::new("first")),
            Excerpt::new(ids[0], 2, 2).with_header(ExcerptHeader::new("second")),
        ];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
        let provider = MultibufferHeaderProvider::new(mb);
        let rows = provider.collect();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].anchor_line, 0);
        // First excerpt covers composed rows 0..=1 (2 rows),
        // so the second header anchors at composed row 2.
        assert_eq!(rows[1].anchor_line, 2);
    }

    /// Provider id derivation is stable and namespace-segregated.
    #[test]
    fn provider_id_namespace_is_stable() {
        let buffer_id = BufferId(42);
        let id = multibuffer_header_provider_id(buffer_id);
        // Low 32 bits = buffer id, high bits = namespace.
        assert_eq!(id & 0xFFFF_FFFF, 42);
        // Distinct from diff namespaces (0xD1FF_*).
        assert!(id < 0xD1FF_0000_0000_0000 || id >= 0xD200_0000_0000_0000);
    }

    /// `version()` follows the multibuffer's snapshot version.
    /// Manual `recompose()` after a source edit bumps the
    /// reported version.
    #[tokio::test(flavor = "multi_thread")]
    async fn provider_version_bumps_with_recompose() {
        let (sources, ids) = make_sources(&["alpha\nbeta\n"]);
        // Pull the source handle so we can drive it.
        let source = sources.get(&ids[0]).unwrap().clone();
        let excerpts = vec![Excerpt::new(ids[0], 0, 0)];
        let mb = MultibufferDocumentHandle::new(sources, excerpts).unwrap();
        let provider = MultibufferHeaderProvider::new(mb.clone());

        let v_before = provider.version();
        source
            .apply_edit(lattice_protocol::edit::Edit::insert(
                lattice_protocol::position::Position::ZERO,
                "X",
            ))
            .await
            .unwrap();
        mb.recompose();
        let v_after = provider.version();
        assert!(
            v_after > v_before,
            "version must bump after recompose; before={v_before} after={v_after}"
        );
    }
}
