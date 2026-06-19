# Multibuffer Header Polish — Slice Plan (MH)

Sequencing + status for the excerpt-header visual rework and
the streaming incremental-append fix.

- **Design:** [`multibuffer-views.md`](../../architecture/multibuffer-views.md)
  §3.8 (excerpt-header visual model + incremental-append contract).
- **Authoritative status:** `implementation.md` `## multibuffer-views`.

Status icons: 🗒 planned · 🚧 in progress · ✅ landed.

---

## Status — COMPLETE (2026-06-20)

All MH slices landed. Headers render rich (file-type icon + basename +
dim dir + `· N matches` badge, per-segment themed colours); the
project-search provider supplies path + match count; headerline status
colours are themed; and `append_excerpts` is incremental (O(batch)) with
an equivalence gate. Both renderers green; the
`multibuffer_is_a_regular_buffer` edit-forwarding invariant held across
every slice.

| Slice | Status | Commit |
|---|---|---|
| MH.A1 — header kind + theme fields | ✅ **subsumed by T.7** | `5c69ab21` (theme-system) |
| MH.A2 — enrich `ExcerptHeader` | ✅ | `4795dd71` |
| MH.A3 — rich `header_cells` renderer | ✅ | `4795dd71` |
| MH.A4 — search data + themed status | ✅ | `986c8ac6` |
| MH.A5 — tests + bench | ✅ | `4795dd71` / `986c8ac6` |
| MH.B1 — incremental `append_excerpts` | ✅ | `ac69dd5b` |
| MH.B2 — equivalence test + bench | ✅ | `ac69dd5b` |

---

## Thread A — rich excerpt headers

### MH.A1 — header kind + theme fields ✅ (subsumed by theme-system T.7)

**Done differently — superseded.** The original plan added
`VirtualRowKind::Header` + bespoke `multibuffer_header_*` `host_theme`
fields. The theme-system redesign (T.7) instead **registered theme
elements** `multibuffer.excerpt_header[.path|.count]` (the mode owns
them) and headers stay `VirtualRowKind::Generic` rows carrying a baked
`bg` from the backdrop element — no new kind, no `host_theme` fields
(`host_theme` was deleted entirely in T.6.t). The diff-deletion-red
reuse the plan worried about is gone: the header has its own element.
See `theme-system.md` §4 + slice plan T.7.

### MH.A2 — enrich `ExcerptHeader` ✅

`ExcerptHeader` gained `path: Option<PathBuf>` + `match_count:
Option<u32>` (back-compat `None`; all callers use `::new`/`::default`/
`with_header`, so no struct-literal churn).

### MH.A3 — rich `header_cells` renderer ✅

`header_cells(header, nerd_fonts, header_fg, path_fg, count_fg)` replaced
the single-colour `themed_header_cells`: leading file-type icon
(`lattice_core::ui::icons::glyph_for_entry`, Nerd v3 on / BMP fallback
off, same 2-cell width — [[feedback_icon_palette]]) + basename
(`header_fg`) + dim parent dir (`path_fg`) + ` · N matches` badge
(`count_fg`), `[untitled]` fallback; per-segment fg baked into each
`Cell`. The provider's `collect()` resolves all three element fgs;
`nerd_fonts` is captured at construction (global `ui.nerd_fonts` via a
newly-registered `ConfigRegistry` service) and folded into `version()`.
**Follow-on:** live per-buffer `ui.nerd_fonts` toggle for an *open*
multibuffer (needs the `FrameView::for_buffer` seam).

### MH.A4 — search data + themed status colours ✅

The project-search provider sets `path` + `match_count` on each file's
header via a shared `search_excerpt_header` helper (count =
`fh.rows.len()`; `scan_file` emits one `FileHits` per file per scan, so
no cross-batch split). The §3.7 headerline status colours moved off
hardcoded hex into theme elements `multibuffer.status.{in_progress,
complete,failed}` (→ `subtext`/`green`/`red` role-keys); the status
provider gained theme access (mirrors T.7) + folds theme version, with
hex fallback when theme is `None`.

### MH.A5 — tests + bench ✅

Header-cell goldens landed with A3 (basename/dir split, count
plural/singular, Nerd-vs-BMP same-width, `[untitled]` fallback) + A4
(search header path/count + badge; status resolves from elements).
Both-renderer parity is **structural** — headers are pure substrate
`Generic` rows; neither renderer has header-specific code, and
`multibuffer_is_a_regular_buffer.rs` (14 tests) covers the
render-like-any-buffer invariant. Header-build perf: production is
off-thread O(sources) (the per-source dedup in `compose_header_rows`);
the per-frame renderer read is viewport-windowed (virtual-row
machinery) — no UI-thread O(excerpts) term.

---

## Thread B — streaming incremental append

### MH.B1 — incremental `append_excerpts` ✅

`append_excerpts` extends rather than rebuilds: composes ONLY the added
batch (`compose_text_from_sources` has no cross-excerpt state), appends
it at the end of `composed_doc` via `Document::apply_edit(Edit::insert(
end_pos, batch))` (byte-identical to `from_text(old + batch)`), and
extends `row_translation` via the new `RowTranslation::append` — all
O(batch). `replace_excerpts` stays full-rebuild (the `gr`/refresh path).
Bonus: appending preserves local edits (the old rebuild clobbered them).

### MH.B2 — equivalence test + bench ✅

`incremental_append_matches_full_build`: streaming N batches yields a
composed rope byte-identical to a single full build + an identical
row-translation (count + `source_row` order). Plus
`incremental_append_skips_unknown_source_like_full_build` (dropped
excerpt) + `append_excerpts_grows_by_exactly_the_batch` (structural
O(batch) pin). Bench `bench_append_excerpts` baseline widened to
`[50, 500, 5000]` to surface flat-vs-linear.

---

## Sequencing

Thread A first (cosmetic, low-risk, lands incrementally), then Thread B
behind its equivalence test. TUI/GPUI lockstep within every renderer-
touching slice — here, none needed: every change is substrate (cells +
bg baked off-thread; both peers consume `VirtualRow` generically).
