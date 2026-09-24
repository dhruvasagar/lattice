# Per-picker help on `<C-h>` — slice plan (PH)

> Design: [`../../architecture/picker.md`](../../architecture/picker.md)
> §4.2quinquies (the key, the three-rung page resolution, the guard test) and
> §4.2bis (depth, whose "up" key this moved).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

**Status:** 🚧 PH.1–PH.5 ✅ (2026-09-24); PH.6 ⛔ deferred, which is what keeps this plan active.

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
| PH.2 | Pages for the file / navigation sources: `files`, `file-pick`, `dir-pick`, `recent`, `buffers` (both `:b` and `:picker buffers` — `PickerSource::Buffers` now answers `picker-buffers`), `grep`, `lines`, `outline`. `dir-pick` documents `<C-l>` / `<C-w>` / `<Tab>`. README index rows, `nav.toml` "Pickers, one by one" group | ✅ |
| PH.3 | Pages for history / editing sources: `jumps`, `marks`, `registers`, `yank-ring`, `commands`, `history`, `search-history`, `pane-buffer-history`, `snippets`, `colorscheme`. Writing them surfaced that `<CR>` in `pane-buffer-history` walked nowhere (fixed separately, `553c79d4`) and that `picker.md` still called the yank picker unbuilt | ✅ |
| PH.4 | Pages for magit (one shared `picker-magit`, declared by all twelve sources), LSP (`picker-lsp-locations` — every LSP result list incl. `:diagnostics` and the error list — `picker-lsp-instances`, `picker-lsp-message-request`), AI sessions, and the `project` plugin's `projects` / `project-buffers`, registered through its help seam (`project.picker-*`). The loader test against the real plugin found that ownership must compare plugins, not seam ids (`6cd10436`) | ✅ |
| PH.5 | Guard test `picker_help_pages_cover_every_source.rs`: every source in the boot registry resolves a page with a keys table listing `<C-h>`; each page's table ROWS name every key its spec enables (`delete_command` → `<C-d>`, depth → `<C-l>` / `<C-w>` / `<Tab>`, `create_label` → the create row); every id-less `PickerSource` names a registered page. Mutation-checked: dropping `<C-w>` from dir-pick's table fails it (the first cut read the whole section and let the prose paragraph stand in for the row) | ✅ |
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
