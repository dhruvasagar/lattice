# Workspace edits: applied in the background, saved (WE-series)

Design: [`lsp-architecture.md` §18](../../architecture/lsp-architecture.md#18-applying-a-workspaceedit).

## Why

`:lsp-rename` across files opened each one in the active pane (so the user
ended on the last), applied the edit, and saved nothing: a rename left a
trail of dirty buffers and moved focus. Code actions shared the function; a
server's `workspace/applyEdit` had a near-copy of it. Edits to a buffer other
than the active one also never reached the language server as `didChange`,
so a server holding one of those files open kept the old text.

## Slices

| Slice | What | Status |
|---|---|---|
| WE.1 | `save_buffer_blocking(id)`: save any buffer, not only the active one — `BeforeSave` / `DocumentSaved` / autoread stamp / `didSave`, no `willSaveWaitUntil` for a background buffer. org capture's `save_target_to_disk` uses it. | ✅ |
| WE.2 | One applier, `apply_workspace_edit`: per file, the open buffer (found by real path) or a hidden one opened for the edit; one undo unit per file; `DocumentChanged` + `LspDocumentChanged` for peers; focus never moves. Rename, code actions and inbound `applyEdit` all use it. | ✅ |
| WE.3 | `lsp.save-workspace-edits` (default on): save every file the edit touched and close the hidden buffers once saved. Off, or a save failing, the touched files stay open, listed and dirty for review. | ✅ |
| WE.4 | `:wa` / `:wall`: write every modified file-backed buffer through WE.1. | ⛔ deferred: it needs a new `Effect` variant, and every `Effect` crosses the plugin boundary, so it is a WIT change (`@0.4.0`). Comes with the next WIT bump. |

Not in scope, named so they are not lost:

- **Resource operations** (`CreateFile` / `RenameFile` / `DeleteFile` in
  `documentChanges`) are still dropped by `flatten_workspace_edit`. A rename
  that moves a module file loses the move.
- **No rollback.** A file that fails mid-edit leaves the earlier files edited
  (and, with WE.3, saved). The summary names the failures; git is the undo.
- **Preview** is A.8 (refactor preview), not this.
