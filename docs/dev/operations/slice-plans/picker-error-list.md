# Slice plan — generic picker → error-list send (PE series)

Extends the archived LR.5 (`slice-plans/archive/lsp-references-view.md`),
which added `<C-q>` as telescope's `send_to_qflist` but wired the
row→location translation as a host-side `match … _ => continue`, generic
in name only. The PE series makes it generic *in fact*, carries the row
text, and shows the result.

Design / contract: [`architecture/error-list.md`](../../architecture/error-list.md)
§3.2c (picker producer) and §3.4 (the programmatic producer API).
User docs: [`user/picker.md`](../../../user/picker.md),
[`user/error-list.md`](../../../user/error-list.md).

| Slice | What | Status | Commit |
|-------|------|--------|--------|
| PE.1 | Payload-owned `RoutingPayload::error_location(&dyn BufferPathResolver)`, EXHAUSTIVE (no `_` arm) — coverage is a compile-time property; closes the silently-dropped `FileLocation`/plugin-picker gap. Host `do_picker_bulk_accept` iterates it via a thin resolver. | ✅ | `d034577f` |
| PE.2 | `Picker::filtered_entries()` pairs each filtered row's `display` with its routing; `ErrorEntry.message` carries the row text (grep match, symbol, preview) so the list reads as the picker did, not blank `file:line`s. | ✅ | `8adc23b7` |
| PE.3 | `picker.send-opens-problems` (bool, default on): a send opens `*problems*` over the result (telescope parity); off populates silently for `:copen`. Extract `Editor::open_problems_view`, shared with `:problems` so the two openers cannot drift. | ✅ | `2d927047` |
| PE.4 | User docs — cross-picker `<C-q>` in `picker.md` (keymap row + prose) and `error-list.md` (producer, per-slice isolation, LSP opt-in accuracy). | ✅ | `213b099c` |
| PE.5 | Developer doc — `error-list.md` §3.2c (picker producer) + §3.4 (the programmatic producer API: `write_error_list` / `set_error_list`, `ErrorEntry`/`ErrorSource`/`ErrorWrite`, the off-thread `InboundBus → AppEffect::SetErrorList` seam, `NewRun` vs `Refresh`); corrected the stale `ErrorSource { Compilation, Lsp }` enum. | ✅ | (this commit) |

## Paramount-goal alignment

- **#2 Extensibility** — PE.1's exhaustive, payload-owned translation is
  the load-bearing win: a plugin picker's `FileLocation` rows (across the
  WIT boundary) get error-list support with zero host change, and a new
  routing variant will not compile until it declares its mapping.
- **#3 Vim grammar** — the result is vim's quickfix, fed from a fuzzy
  finder; `:cnext` / `]q` / `*problems*` navigation is inherited unchanged.
- **UX (convention-first)** — default-on `*problems*` matches telescope's
  `<C-q>`; the option honours the vim `:grep` two-step.

## Verification

Per-slice: `cargo test -p lattice-picker error_location` (PE.1) and
`cargo test -p lattice-host --test references_view_terminus` (PE.1–PE.3,
16 tests incl. `bulk_accept_opens_problems_by_default`,
`bulk_accept_off_populates_without_opening`); fmt clean; compile
warning-clean. No renderer/effect-enum change, so no TUI/GPUI parity work.

## Deferred

- Real severity threading for the diagnostics picker (entries send as
  `Info`). Needs `LspLocationRow` to carry severity; diagnostics already
  have their own error-list feed (`lsp.diagnostics-to-error-list`), so the
  gap is cosmetic. Revisit if a severity-aware picked list is wanted.
