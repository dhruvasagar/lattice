# Multibuffer Header Polish — Slice Plan (MH)

Sequencing + status for the excerpt-header visual rework and
the streaming incremental-append fix.

- **Design:** [`multibuffer-views.md`](../../architecture/multibuffer-views.md)
  §3.8 (excerpt-header visual model + incremental-append contract).
- **Authoritative status:** `implementation.md` `## multibuffer-views`.

Status icons: 🗒 planned · 🚧 in progress · ✅ landed.

---

## Thread A — rich excerpt headers

### MH.A1 — `VirtualRowKind::Header` + theme fields 🗒

Add `VirtualRowKind::Header` to `lattice-cells`. Add header
theme fields to `host_theme`
(`multibuffer_header_bg/_fg/_path_fg/_count_fg`). Wire the
backdrop arm for the new kind in **both** renderers in the
same patch:

- TUI `render.rs` (`render_virtual_row` backdrop match).
- GPUI `editor_element.rs` (`push_virtual_row` backdrop quads).

Stops the diff-deletion-red reuse for headers. End-of-slice
grep audit: `grep -rn "VirtualRowKind::Header" crates/lattice-ui-gpui/`
must be non-empty.

### MH.A2 — enrich `ExcerptHeader` 🗒

Add `path: Option<PathBuf>` + `match_count: Option<u32>` to
`ExcerptHeader`. `compose_header_rows` tags emitted rows
`VirtualRowKind::Header` (was `Generic`). Existing constructors
default the new fields to `None` (back-compat for diff /
diagnostics providers until they opt in).

### MH.A3 — rich `header_cells` renderer 🗒

Replace `default_header_cells(excerpt)` with
`header_cells(header, nerd_fonts, colors)`: file-type icon
(`entry_visual`, nerd-font + BMP fallback) + basename
(`_fg`) + dir path (`_path_fg`) + ` · N matches` badge
(`_count_fg`). `MultibufferExcerptHeaderProvider::collect()`
reads `ui.nerd_fonts` + header colours via
`FrameView::for_buffer`. Per-segment fg via `Cell` fg field.

### MH.A4 — search mode supplies data + theme status colours 🗒

Search mode (`providers/search.rs`) sets `path` +
`match_count` on the excerpts it builds (count = hits per
source). Move the §3.7 status-row colours
(`0x999999`/`0x44cc88`/`0xff4444` in `render_multibuffer_status`)
into the new theme fields.

### MH.A5 — tests + bench 🗒

- Header-cells golden: nerd on/off (BMP fallback), count
  present/absent, path dim segment, empty-title fallback.
- `multibuffer_is_a_regular_buffer.rs` stays green.
- Both-renderer header backdrop parity assertion.
- Bench: header build stays O(viewport), not O(excerpts).

---

## Thread B — streaming incremental append

### MH.B1 — incremental `append_excerpts` 🗒

`append_excerpts` extends `composed_doc` (append new sources'
rope segments) + `RowTranslation` (push new `RowEntry`s)
incrementally — O(batch). `replace_excerpts` stays
full-rebuild for `gr` refresh. Higher-risk: touches the
composed-rope / row-translation invariant edit-forwarding (§4)
depends on; lands behind the equivalence test below.

### MH.B2 — equivalence test + bench 🗒

- Equivalence: streamed N-batch append produces a composed
  rope + `RowTranslation` byte-identical to a single
  full-build over the same excerpt list.
- Bench: per-batch append cost is flat as accumulated total
  grows (vs today's linear-in-total recompose).

---

## Sequencing

Thread A first (cosmetic, low-risk, lands incrementally),
then Thread B behind its equivalence test. TUI/GPUI lockstep
within every slice that touches a renderer.
