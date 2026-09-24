# Per-picker help on `<C-h>` — slice plan (PH)

> Design: [`../../architecture/picker.md`](../../architecture/picker.md)
> §4.2quinquies (the key, the three-rung page resolution, the guard test) and
> §4.2bis (depth, whose "up" key this moved).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

**Status:** 🚧 in progress (2026-09-24).

## Why

Every picker's keys lived in one shared table in `docs/user/picker.md`, and
what a key means depends on the source: `<C-d>` forgets a project and does
nothing in `files`; `<C-l>` drills into a directory in `dir-pick` and nowhere
else. The user had no way to ask *this* picker what it does, and the shared
table could not answer without listing every exception.

## Decisions taken

- **`<C-h>` opens help; ascend moves to `<C-w>`.** `<C-h>` is the editor's help
  prefix in Normal mode and emacs's in the minibuffer. `<C-w>` is `c_CTRL-W`,
  which on a path is "drop the last component"; where a source has no depth it
  falls through to delete-word, so it never does nothing. `<C-j>` / `<C-k>`
  were rejected: `j` / `k` are the vertical pair, and fzf binds them to select
  next / prev.
- **Pages are `docs/user/picker-<id>.md`, flat.** The file stem is the topic
  name, so the `picker-<id>` convention finds every builtin page with no spec
  declaration; only a page shared by several sources is declared. Flat, not a
  `pickers/` subdirectory: `docs/user/` is a flat corpus on purpose, and both
  the site sync (`sync-docs.sh`) and the `:help`-index guard glob only its top
  level — a subdirectory page would register as a topic and reach neither the
  website nor the index check. (PH.1 wrote `pickers/`; corrected in PH.2.)
- **Transient menus are not in this batch** (PH.6).

## Slices

| Slice | Description | Status |
|---|---|---|
| PH.1 | `PickerSourceSpec::help_topic`, `Action::PickerHelp` on `<C-h>`, three-rung resolution, ascend → `<C-w>` with delete-word fallback (`Picker::delete_word_backward`), `PickerSource::help_topic` for id-less pickers | ✅ |
| PH.2 | Pages for the file / navigation sources: `files`, `file-pick`, `dir-pick`, `recent`, `buffers`, `projects`, `grep`, `lines`, `outline`. `dir-pick` documents `<C-l>` / `<C-w>` / `<Tab>` | 📝 |
| PH.3 | Pages for history / editing sources: `jumps`, `marks`, `registers`, `yank`, `commands`, `history`, `search-history`, `pane-buffer-history`, `snippets`, `colorscheme` | 📝 |
| PH.4 | Pages for magit (one shared `picker-magit`, declared), LSP (`picker-lsp-locations`, `picker-lsp-instances`, …), AI sessions, org-roam | 📝 |
| PH.5 | Guard test (every builtin source resolves a page; its *Keys* table names every key its spec enables), `picker.md` links out, README index, `nav.toml` | 📝 |
| PH.6 | `<C-h>` in transient menus opens a page for the menu | ⛔ deferred — transients have no registry id to key a page on; revisit with a `TransientSpec` help field when the magit transient docs are next reworked |

## PH.1 notes

- Tests: `crates/lattice-host/tests/picker_help.rs` (each rung asserts the
  TITLE of the page that opened, so an always-general fallback cannot pass),
  `picker_descend.rs` (ascend on `<C-w>`, delete-word where there is no depth),
  and `delete_word_backward_follows_vim_ctrl_w` in `lattice-picker`, whose
  cases were checked against vim 9.2's `:` line.
- The delete-word fallback is decided by whether the source *answers* `ascend`,
  not by whether the query moved. `dir-pick` at `/` answers `Some("/")`; a
  moved-query test deleted the `/` — caught by
  `ascending_stops_at_the_filesystem_root`.
- No bench: nothing on the per-keystroke path changes. The page is resolved on
  the `<C-h>` press, from two `ArcSwap` loads.
