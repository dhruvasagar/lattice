# LSP Feature Tracking

Single source of truth for which LSP 3.17 capabilities `lattice`
implements and which are planned. Updated every commit that
moves a row.

Status legend:

| Status | Meaning                                        |
|--------|------------------------------------------------|
| ✅     | Done; tested; benchmarked where relevant.      |
| 🚧     | In progress; partial implementation landed.    |
| ⏹️      | Planned for the indicated phase.               |
| ⛔     | Deferred past v1 (rationale in the row).       |
| n/a    | Not applicable (server-side, not client-side). |

Phases:

| Phase        | Scope                                                                                                                                                         |
|--------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------|
| **4.1**      | Foundation: wire layer + actor + handshake + sync + diagnostics routing.                                                                                      |
| **4.2**      | Navigation: hover, definition family, references, symbols, completion.                                                                                        |
| **4.3**      | Edits: code actions, rename, formatting, signature help, will-save hooks.                                                                                     |
| **4.4**      | Polish: semantic tokens, inlay hints, folding, document highlight, dynamic registration, file watchers, workspace configuration, progress / messages routing. |
| **4.5**      | Expansion: call/type hierarchy, code lens, document links, color, moniker, linked editing, selection ranges, inline values, inline completion.                |
| **post-1.0** | Notebooks, multi-root workspace folders, server-side LSP, etc.                                                                                                |

Capability columns:

- **Spec § / method** — LSP 3.17 spec section + method name.
- **Client cap** — what we advertise in `initialize.capabilities`.
- **Server cap gating** — which `ServerCapabilities` field gates the feature.
- **Phase** — when it lands.
- **Status** — current implementation state.
- **Notes** — implementation pointers + caveats.

---

## Lifecycle

| Method            | Direction | Phase | Status | Notes                                                                                                                       |
|-------------------|-----------|-------|--------|-----------------------------------------------------------------------------------------------------------------------------|
| `initialize`      | C → S     | 4.1   | ✅     | `actor::actor_main`. Runs before `ServerHandle` is returned to the caller; failure surfaces as `LspError::HandshakeFailed`. |
| `initialized`     | C → S     | 4.1   | ✅     | Sent immediately after `initialize` response is decoded.                                                                    |
| `shutdown`        | C → S     | 4.1   | ✅     | `ServerHandle::shutdown()` runs the protocol-mandated `shutdown` request before `exit`.                                     |
| `exit`            | C → S     | 4.1   | ✅     | Sent after `shutdown` response (or 5s timeout).                                                                             |
| `$/setTrace`      | C → S     | 4.4   | ⏹️      | Honours `trace` value from `initialize` for now (we set `Off`); switching dynamically is a 4.4 polish item.                 |
| `$/logTrace`      | S → C     | 4.4   | ⏹️      | Will route into `:messages` buffer alongside `window/logMessage`.                                                           |
| `$/cancelRequest` | both      | 4.1   | ✅     | `ServerHandle::cancel(id)` emits the notification + resolves the matching `Pending` with `LspError::Cancelled`.             |
| `$/progress`      | S → C     | 4.4   | ⏹️      | Currently logged; routed to the modeline progress slot in 4.4.                                                              |

## Document synchronisation

| Method                                 | Direction | Phase | Status | Notes                                                                                                            |
|----------------------------------------|-----------|-------|--------|------------------------------------------------------------------------------------------------------------------|
| `textDocument/didOpen`                 | C → S     | 4.1   | ✅     | `DocSync::open` -- carries `languageId`, `version=1`, full text.                                                 |
| `textDocument/didChange` (Incremental) | C → S     | 4.1   | ✅     | `DocSync::record_edit` queues; `DocSync::flush` sends. utf-8 / utf-16 / utf-32 conversion via `position` module. |
| `textDocument/didChange` (Full)        | C → S     | 4.1   | ✅     | Auto-selected when server advertises `Full` sync mode.                                                           |
| `textDocument/didChange` (None)        | C → S     | 4.1   | ✅     | No-op when server advertises `None`.                                                                             |
| `textDocument/willSave`                | C → S     | 4.3   | ⏹️      | Will fire from the App's Save hook before disk write.                                                            |
| `textDocument/willSaveWaitUntil`       | C → S     | 4.3   | ⏹️      | For format-on-save; waits for the server's response (timeout-bounded).                                           |
| `textDocument/didSave`                 | C → S     | 4.3   | ⏹️      | Sent after a successful disk write; carries text iff server requested it via `save.includeText`.                 |
| `textDocument/didClose`                | C → S     | 4.1   | ✅     | `DocSync::close` flushes pending then sends.                                                                     |

## Server → client notifications

| Method                            | Phase | Status | Notes                                                                                                                                                  |
|-----------------------------------|-------|--------|--------------------------------------------------------------------------------------------------------------------------------------------------------|
| `textDocument/publishDiagnostics` | 4.1   | ✅     | `DiagnosticsBus` broadcast; subscribers via `ServerHandle::subscribe_diagnostics()`. Decoration layer + gutter + `:diagnostics` buffer in 4.1.d.ii–iv. |
| `window/showMessage`              | 4.4   | 🚧     | Logged today; routed into a non-blocking minibuffer notification in 4.4.                                                                               |
| `window/showMessageRequest`       | 4.4   | ⏹️      | Modal picker (action-button list) -- 4.4.                                                                                                              |
| `window/showDocument`             | 4.4   | ⏹️      | Open the requested URI (file or external) in a buffer / browser.                                                                                       |
| `window/logMessage`               | 4.4   | 🚧     | Logged today; routed to `:messages` buffer in 4.4.                                                                                                     |
| `telemetry/event`                 | 4.4   | 🚧     | Logged today; opt-in subscription point for plugins in 4.4.                                                                                            |

## Server-initiated requests

| Method                             | Phase | Status | Notes                                                                                                       |
|------------------------------------|-------|--------|-------------------------------------------------------------------------------------------------------------|
| `client/registerCapability`        | 4.1   | ✅     | Accepted with `null` result. Dynamic-capability tracking lands in 4.4.                                      |
| `client/unregisterCapability`      | 4.1   | ✅     | Accepted with `null` result.                                                                                |
| `workspace/configuration`          | 4.1   | 🚧     | Returns one `null` per requested item today. Real values land when §5.12 (typed options + `init.rs`) lands. |
| `workspace/applyEdit`              | 4.3   | ⏹️      | Multi-file edit pipeline -- 4.3 alongside rename + code actions.                                            |
| `workspace/codeLens/refresh`       | 4.5   | ⏹️      | Re-fetch all visible code lenses.                                                                           |
| `workspace/inlayHint/refresh`      | 4.4   | ⏹️      | Re-fetch all visible inlay hints.                                                                           |
| `workspace/inlineValue/refresh`    | 4.5   | ⏹️      | Re-fetch all visible inline values.                                                                         |
| `workspace/semanticTokens/refresh` | 4.4   | ⏹️      | Invalidate semantic-token cache + re-request.                                                               |
| `workspace/diagnostic/refresh`     | 4.4   | ⏹️      | Re-fetch diagnostics across the workspace (pull-based diagnostics).                                         |
| `window/workDoneProgress/create`   | 4.1   | ✅     | Accepted with `null` result.                                                                                |
| `window/workDoneProgress/cancel`   | 4.4   | ⏹️      | Will cancel a created token's progress; needs the progress map.                                             |

## Workspace operations

| Method                                | Phase | Status | Notes                                                          |
|---------------------------------------|-------|--------|----------------------------------------------------------------|
| `workspace/didChangeConfiguration`    | 4.4   | ⏹️      | Pushes config deltas to the server -- needs §5.12 hooks.       |
| `workspace/didChangeWatchedFiles`     | 4.4   | ⏹️      | Driven by a native file-watcher (notify / fsevents / inotify). |
| `workspace/didChangeWorkspaceFolders` | post  | ⏹️      | Multi-root workspaces -- post-1.0 feature; v1 is single-root.  |
| `workspace/willCreateFiles`           | 4.4   | ⏹️      | Pre-create hook -- server may return edits to apply.           |
| `workspace/didCreateFiles`            | 4.4   | ⏹️      | Post-create notification.                                      |
| `workspace/willRenameFiles`           | 4.4   | ⏹️      | Pre-rename hook (e.g. update imports).                         |
| `workspace/didRenameFiles`            | 4.4   | ⏹️      | Post-rename notification.                                      |
| `workspace/willDeleteFiles`           | 4.4   | ⏹️      | Pre-delete hook (e.g. cascading edits).                        |
| `workspace/didDeleteFiles`            | 4.4   | ⏹️      | Post-delete notification.                                      |
| `workspace/executeCommand`            | 4.3   | ⏹️      | Used by code-action `command` payloads.                        |
| `workspace/symbol`                    | 4.2   | ⏹️      | Workspace-wide symbol picker.                                  |
| `workspaceSymbol/resolve`             | 4.2   | ⏹️      | Resolve location of a workspace-symbol entry.                  |

## Language features (text document)

### Hover / signatures / completion

| Method                                 | Phase | Status | Notes |
|----------------------------------------|-------|--------|-------|
| `textDocument/hover`                   | 4.2   | ⏹️    | Reuses existing hover-popup primitive; `K` keybinding. |
| `textDocument/signatureHelp`           | 4.3   | ⏹️    | Trigger characters (`,`, `(`) drive auto-popup. |
| `textDocument/completion`              | 4.2   | ⏹️    | Provider for `lattice-completion`. Snippet support, label details, insertReplace, resolveSupport advertised. |
| `completionItem/resolve`               | 4.2   | ⏹️    | Lazy-resolve documentation / detail / additionalTextEdits when an item gains focus. |
| `textDocument/inlineCompletion`        | 4.5   | ⏹️    | LSP 3.18 / pre-spec. Inline ghost-text suggestions; integrates with copilot-like flows once landed. |

### Navigation

| Method                                    | Phase | Status | Notes |
|-------------------------------------------|-------|--------|-------|
| `textDocument/declaration`                | 4.2   | ⏹️    | `gd` family. Pushes a `PluginPush` entry to the position-history (§5.1.1). |
| `textDocument/definition`                 | 4.2   | ⏹️    | `gD` / `gd` per keymap. |
| `textDocument/typeDefinition`             | 4.2   | ⏹️    | `gy` |
| `textDocument/implementation`             | 4.2   | ⏹️    | `gI` |
| `textDocument/references`                 | 4.2   | ⏹️    | `gr` -- opens a buffer-backed list view (everything-is-a-buffer). |
| `textDocument/documentHighlight`          | 4.4   | ⏹️    | Renderer overlay highlighting matches under cursor. |
| `textDocument/documentSymbol`             | 4.2   | ⏹️    | Outline pane buffer. |
| `textDocument/prepareCallHierarchy`       | 4.5   | ⏹️    | Required by callHierarchy/* below. |
| `callHierarchy/incomingCalls`             | 4.5   | ⏹️    | Caller-side traversal. |
| `callHierarchy/outgoingCalls`             | 4.5   | ⏹️    | Callee-side traversal. |
| `textDocument/prepareTypeHierarchy`       | 4.5   | ⏹️    | Required by typeHierarchy/*. |
| `typeHierarchy/supertypes`                | 4.5   | ⏹️    | Up the type tree. |
| `typeHierarchy/subtypes`                  | 4.5   | ⏹️    | Down the type tree. |
| `textDocument/moniker`                    | 4.5   | ⏹️    | Stable cross-project symbol id. Useful for cross-repo navigation; niche. |
| `textDocument/selectionRange`             | 4.4   | ⏹️    | Hierarchical selection expansion (`+` / `-` keys). |

### Edits

| Method                                    | Phase | Status | Notes |
|-------------------------------------------|-------|--------|-------|
| `textDocument/codeAction`                 | 4.3   | ⏹️    | Picker popup; supports `WorkspaceEdit` + `Command` payloads. |
| `codeAction/resolve`                      | 4.3   | ⏹️    | Lazy-resolve `edit` from the action's `data`. |
| `textDocument/rename`                     | 4.3   | ⏹️    | Multi-file edit applied atomically (one undo step per doc). |
| `textDocument/prepareRename`              | 4.3   | ⏹️    | Validates the cursor is on a renameable identifier; returns the placeholder. |
| `textDocument/formatting`                 | 4.3   | ⏹️    | Whole-buffer formatter; `=G` mapped. |
| `textDocument/rangeFormatting`            | 4.3   | ⏹️    | Range formatter; `=` operator on motions / objects. |
| `textDocument/onTypeFormatting`           | 4.3   | ⏹️    | Trigger-character driven (e.g. `;` / `}` in C-family). |
| `textDocument/linkedEditingRange`         | 4.5   | ⏹️    | Multi-cursor mode for linked identifiers (HTML tag pairs, etc.). |

### Decorations / inline information

| Method                                    | Phase | Status | Notes |
|-------------------------------------------|-------|--------|-------|
| `textDocument/publishDiagnostics`         | 4.1   | 🚧     | Routing done (4.1.d.i); decoration layer + gutter + `:diagnostics` buffer in 4.1.d.ii–iv. |
| `textDocument/diagnostic`                 | 4.4   | ⏹️    | Pull-based diagnostics (LSP 3.17). Used when server prefers pull over push. |
| `workspace/diagnostic`                    | 4.4   | ⏹️    | Workspace-wide pull. |
| `textDocument/inlayHint`                  | 4.4   | ⏹️    | Renderer overlay; type/parameter hints inline. |
| `inlayHint/resolve`                       | 4.4   | ⏹️    | Lazy-resolve tooltip / textEdits when the hint gains focus. |
| `textDocument/inlineValue`                | 4.5   | ⏹️    | Debug-flow value-at-line. Niche outside debugger integration. |
| `textDocument/codeLens`                   | 4.5   | ⏹️    | Above-line clickable annotations (run / debug / references). |
| `codeLens/resolve`                        | 4.5   | ⏹️    | Lazy-resolve the lens command. |
| `textDocument/documentLink`               | 4.5   | ⏹️    | Hyperlinks inside text (URLs, imports). `gx` follows them. |
| `documentLink/resolve`                    | 4.5   | ⏹️    | Lazy-resolve target. |
| `textDocument/documentColor`              | 4.5   | ⏹️    | Color swatches at hex literals; `colorPresentation` returns named alternatives. |
| `textDocument/colorPresentation`          | 4.5   | ⏹️    | Companion to documentColor. |
| `textDocument/foldingRange`               | 4.4   | ⏹️    | New `FoldMethod::Lsp` -- merges with tree-sitter fold provider via priority. |
| `textDocument/semanticTokens/full`        | 4.4   | ⏹️    | Per-doc semantic-token list; merges with tree-sitter highlight as a layer. |
| `textDocument/semanticTokens/full/delta`  | 4.4   | ⏹️    | Delta encoding for re-requests after edits. |
| `textDocument/semanticTokens/range`       | 4.4   | ⏹️    | Viewport-bounded request; cheaper for large files. |

## Notebook documents

| Method                       | Phase | Status | Notes                                                                  |
|------------------------------|-------|--------|------------------------------------------------------------------------|
| `notebookDocument/didOpen`   | post  | ⛔     | Lattice's notebook story is post-1.0 (rich-buffer rendering required). |
| `notebookDocument/didChange` | post  | ⛔     | --                                                                     |
| `notebookDocument/didSave`   | post  | ⛔     | --                                                                     |
| `notebookDocument/didClose`  | post  | ⛔     | --                                                                     |

## Tracking summary

Counts as of 2026-05-04:

| State          | Count |
|----------------|-------|
| ✅ Done        | 11    |
| 🚧 In progress | 4     |
| ⏹️ Planned      | 50    |
| ⛔ Deferred    | 4     |

Phase rollup:

- **4.1** Foundation: 11/15 done (4 still in flight under 4.1.d–e: decoration layer, gutter, `:diagnostics` buffer, doc completion).
- **4.2** Navigation: 0/12 -- queued.
- **4.3** Edits: 0/9 -- queued.
- **4.4** Polish: 0/19 -- queued.
- **4.5** Expansion: 0/14 -- queued.
- **post-1.0**: notebooks + multi-root workspaces.

When you finish a feature, flip its Status column and update the
phase rollup. The matrix is the single source of truth: every
LSP feature in lattice has a row here, and every row reflects
reality.
