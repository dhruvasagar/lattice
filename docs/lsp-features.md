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

| Method                                 | Direction | Phase | Status | Notes                                                                                                                                                                                                                                             |
|----------------------------------------|-----------|-------|--------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `textDocument/didOpen`                 | C → S     | 4.1   | ✅     | `DocSync::open` -- carries `languageId`, `version=1`, full text.                                                                                                                                                                                  |
| `textDocument/didChange` (Incremental) | C → S     | 4.1   | ✅     | `DocSync::record_edit` queues; `DocSync::flush` sends. utf-8 / utf-16 / utf-32 conversion via `position` module.                                                                                                                                  |
| `textDocument/didChange` (Full)        | C → S     | 4.1   | ✅     | Auto-selected when server advertises `Full` sync mode.                                                                                                                                                                                            |
| `textDocument/didChange` (None)        | C → S     | 4.1   | ✅     | No-op when server advertises `None`.                                                                                                                                                                                                              |
| `textDocument/willSave`                | C → S     | 4.3   | ✅     | `App::save_blocking` fan-out via `fire_will_save_notifications`; only servers advertising `wants_will_save` receive it. `reason: Manual` for now -- AfterDelay / FocusOut land when those triggers exist.                                         |
| `textDocument/willSaveWaitUntil`       | C → S     | 4.3   | ✅     | `App::save_blocking` runs `run_will_save_wait_until_blocking` between willSave and the disk write; per-server `Pending::await` with a 500ms timeout; collected TextEdits apply pre-save as one undo unit. Format-on-save flows through this path. |
| `textDocument/didSave`                 | C → S     | 4.3   | ✅     | Post-save fan-out via `fire_did_save_notifications`; attaches the rope's text when the server's `did_save_include_text` says so.                                                                                                                  |
| `textDocument/didClose`                | C → S     | 4.1   | ✅     | `DocSync::close` flushes pending then sends.                                                                                                                                                                                                      |

## Server → client notifications

| Method                            | Phase | Status | Notes                                                                                                                                                                                  |
|-----------------------------------|-------|--------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `textDocument/publishDiagnostics` | 4.1   | ✅     | `DiagnosticsBus` broadcast (4.1.d.i) + `DiagnosticsLayer` per-URI state with version gating + multi-server merge (4.1.d.ii). Renderer overlay + `:diagnostics` buffer in 4.1.d.iii–iv. |
| `window/showMessage`              | 4.4   | 🚧     | Logged today; routed into a non-blocking minibuffer notification in 4.4.                                                                                                               |
| `window/showMessageRequest`       | 4.4   | ⏹️      | Modal picker (action-button list) -- 4.4.                                                                                                                                              |
| `window/showDocument`             | 4.4   | ⏹️      | Open the requested URI (file or external) in a buffer / browser.                                                                                                                       |
| `window/logMessage`               | 4.4   | 🚧     | Logged today; routed to `:messages` buffer in 4.4.                                                                                                                                     |
| `telemetry/event`                 | 4.4   | 🚧     | Logged today; opt-in subscription point for plugins in 4.4.                                                                                                                            |

## Server-initiated requests

| Method                             | Phase | Status | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
|------------------------------------|-------|--------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `client/registerCapability`        | 4.1   | ✅     | Accepted with `null` result. Dynamic-capability tracking lands in 4.4.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `client/unregisterCapability`      | 4.1   | ✅     | Accepted with `null` result.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `workspace/configuration`          | 4.1   | ✅     | Inbound channel (`lattice-lsp::ConfigurationBus` mpsc + per-request oneshot) ferries each request to the App. The loader caches the merged user + project TOML tree (deep-merge; project wins per scalar; sibling keys preserved); `App::drain_inbound_configuration_requests` looks each requested `section` up at `lsp.<section>` and replies with one `serde_json::Value` per item. Empty `section` returns the whole `lsp.*` sub-tree; missing sections come back as `null`. User puts server-namespaced settings under `[lsp.<server-id>]` (e.g. `[lsp.rust-analyzer.cargo] features = ["foo"]`). |
| `workspace/applyEdit`              | 4.3   | ✅     | Inbound channel (`lattice-lsp::ApplyEditBus` mpsc + per-request oneshot) ferries each request to the App. `App::drain_inbound_apply_edits` runs once per main-loop iteration; reuses `flatten_workspace_edit` + `apply_lsp_text_edits` (the rename path) per file; replies via the oneshot with `applied` + optional `failure_reason`. Empty edits reply `applied: true`. v1 is non-atomic (per-file batches); `failed_change` is queued for the future `apply_workspace_edit_atomic`.                                                                                                                 |
| `workspace/codeLens/refresh`       | 4.5   | ⏹️      | Re-fetch all visible code lenses.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `workspace/inlayHint/refresh`      | 4.4   | ⏹️      | Re-fetch all visible inlay hints.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `workspace/inlineValue/refresh`    | 4.5   | ⏹️      | Re-fetch all visible inline values.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `workspace/semanticTokens/refresh` | 4.4   | ⏹️      | Invalidate semantic-token cache + re-request.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `workspace/diagnostic/refresh`     | 4.4   | ⏹️      | Re-fetch diagnostics across the workspace (pull-based diagnostics).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `window/workDoneProgress/create`   | 4.1   | ✅     | Accepted with `null` result.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `window/workDoneProgress/cancel`   | 4.4   | ⏹️      | Will cancel a created token's progress; needs the progress map.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |

## Workspace operations

| Method                                | Phase | Status | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
|---------------------------------------|-------|--------|--------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `workspace/didChangeConfiguration`    | 4.4   | ⏹️      | Pushes config deltas to the server -- needs §5.12 hooks.                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `workspace/didChangeWatchedFiles`     | 4.4   | ⏹️      | Driven by a native file-watcher (notify / fsevents / inotify).                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `workspace/didChangeWorkspaceFolders` | post  | ⏹️      | Multi-root workspaces -- post-1.0 feature; v1 is single-root.                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `workspace/willCreateFiles`           | 4.4   | ⏹️      | Pre-create hook -- server may return edits to apply.                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `workspace/didCreateFiles`            | 4.4   | ⏹️      | Post-create notification.                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `workspace/willRenameFiles`           | 4.4   | ⏹️      | Pre-rename hook (e.g. update imports).                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `workspace/didRenameFiles`            | 4.4   | ⏹️      | Post-rename notification.                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `workspace/willDeleteFiles`           | 4.4   | ⏹️      | Pre-delete hook (e.g. cascading edits).                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `workspace/didDeleteFiles`            | 4.4   | ⏹️      | Post-delete notification.                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `workspace/executeCommand`            | 4.3   | ✅     | Wired through the codeAction Command-payload arm. `:code-actions` accept fires it via the originating server; capability-gated on `executeCommandProvider`.                                                                                                                                                                                                                                                                                                                          |
| `workspace/symbol`                    | 4.2   | ✅     | `:lsp-workspace-symbol [query]` (Phase 4.2.f). Fans out across every running server (workspace-scoped); dedups by `(path, line, col, name)`; opens the merged list as a vertico `PickerSource::LspLocations` picker. Empty query → server's idea of "every workspace symbol".                                                                                                                                                                                                        |
| `workspaceSymbol/resolve`             | 4.2   | ✅     | LSP 3.17+ lazy-location path. `workspace/symbol` response upgrades from the legacy `Vec<SymbolInformation>` to `WorkspaceSymbolResponse` (`Flat \| Nested` union); `Nested` symbols whose `location` is `WorkspaceLocation` (URI only) eager-resolve at fan-out via `workspace_symbol_to_row` so the picker's row shape stays uniform. Falls back to `(path, 0, 0)` when the server doesn't advertise `resolveProvider` or resolve fails -- the user can still navigate to the file. |

## Language features (text document)

### Hover / signatures / completion

| Method                          | Phase | Status | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
|---------------------------------|-------|--------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `textDocument/hover`            | 4.2   | ✅     | `K` keystroke (Phase 4.2.b). Spawns the request on the LSP runtime; first non-empty body across attached servers wins; relay's cancellation token flips on a follow-up `K` so a stale response can't drop a popup over a moved cursor. Markdown body feeds the existing `HoverPopup` pipeline. Multi-server merge with `--- {name} ---` separators is a polish item.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `textDocument/signatureHelp`    | 4.3   | ✅     | `:signature-help` (alias `:sighelp`). Fan-out across attached servers; first non-empty wins. `signature_help_to_markdown` renders the active signature + active parameter (fenced-code label + `**param:**` highlight + parameter docs); body feeds `do_open_hover` so the popup pipeline (markdown highlight, anchored placement, State A/B focus model) is shared with `K` hover. Trigger-character autopilot (`(` / `,` etc.) in Insert mode is queued.                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `textDocument/completion`       | 4.2   | ✅     | Insert-mode popup via `<C-x><C-o>` / `<C-Space>` / smart-tab (Phase 4.2.g full surface: shell + buffer-words + LSP source + docs popup + lazy `completionItem/resolve` + snippets + frequency ranking + per-source priority + per-language overrides + tree-sitter symbols + path source + commit chars + ghost text + cross-source visual dedup + typed picker routing). Item adaptation: `filterText` / `sortText` / `detail` / `documentation` / `commitCharacters` / `additionalTextEdits` / `textEdit.range` / `insertTextFormat`; `isIncomplete: true` re-fires per keystroke. Multi-server fan-out + dedup by `(label, kind)`. Sidecar metadata via `App.insert_completion_lsp_meta` keyed by `CandidateData::Extension { kind_id: LSP_COMPLETION_KIND_ID, payload }`. Picker bridge `:complete` (alias `:lspcomplete`) survives as the cmdline-driven peer. Behavioural spec at `docs/insert-completion.md`. |
| `completionItem/resolve`        | 4.2   | ✅     | `completionItem/resolve` fires lazily when the user opens the docs popup for a candidate that arrived without `documentation`. Round-trips the original `CompletionItem` JSON to the originating server (preserves the opaque `data` blob); response fills in `documentation` / `detail` / `additionalTextEdits` / `command` on the host's `LspCompletionMeta` sidecar. Cancellation token rides on selection changes so a slow server's stale resolve never overwrites the popup body. Phase 4.2.g.3.                                                                                                                                                                                                                                                                                                                                                                                                               |
| `textDocument/inlineCompletion` | 4.5   | ⏹️      | LSP 3.18 / pre-spec. Inline ghost-text suggestions; integrates with copilot-like flows once landed.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |

### Navigation

| Method                              | Phase | Status | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
|-------------------------------------|-------|--------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `textDocument/declaration`          | 4.2   | ✅     | `gD` keystroke (Phase 4.2.c follow-up). Routes through the unified `do_lsp_nav_request(LspNavKind::Declaration)` -- same per-server merge + jump-or-picker dispatch as definition.                                                                                                                                                                                                                                                                                                              |
| `textDocument/definition`           | 4.2   | ✅     | `gd` keystroke (Phase 4.2.c). Spawns the request on the LSP runtime; merged + deduped (by uri+range.start) across attached servers. Single result jumps in-place (or via `:e <path>` if cross-file); multiple results open a vertico `PickerSource::LspLocations` picker (Phase 4.2.d.picker). Pre-jump cursor pushed onto position history with `PluginPush` source so `<C-o>` walks back. Cancellation token rides on follow-up `gd` so a slow server can't drop a popup over a moved cursor. |
| `textDocument/typeDefinition`       | 4.2   | ✅     | `gy` keystroke. Same dispatch as definition.                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `textDocument/implementation`       | 4.2   | ✅     | `gI` keystroke (capital `I`; lowercase `gi` reserved for vim's "go to last insert position"). Same dispatch as definition.                                                                                                                                                                                                                                                                                                                                                                      |
| `textDocument/references`           | 4.2   | ✅     | `gr` keystroke (Phase 4.2.d). Fans out per attached server with `include_declaration: true`; sort+dedup by (uri, range.start); opens the merged result as a vertico picker. Empty result echoes "no references for X".                                                                                                                                                                                                                                                                          |
| `textDocument/documentHighlight`    | 4.4   | ⏹️      | Renderer overlay highlighting matches under cursor.                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `textDocument/documentSymbol`       | 4.2   | ✅     | `:lsp-symbols` (Phase 4.2.e). Walks both Flat and Nested response shapes; Nested DFS preserves outline order and assigns indent depth in the picker display. Multi-server merge dedups by `(path, line, col, name)`.                                                                                                                                                                                                                                                                            |
| `textDocument/prepareCallHierarchy` | 4.5   | ⏹️      | Required by callHierarchy/* below.                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `callHierarchy/incomingCalls`       | 4.5   | ⏹️      | Caller-side traversal.                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `callHierarchy/outgoingCalls`       | 4.5   | ⏹️      | Callee-side traversal.                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `textDocument/prepareTypeHierarchy` | 4.5   | ⏹️      | Required by typeHierarchy/*.                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `typeHierarchy/supertypes`          | 4.5   | ⏹️      | Up the type tree.                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| `typeHierarchy/subtypes`            | 4.5   | ⏹️      | Down the type tree.                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `textDocument/moniker`              | 4.5   | ⏹️      | Stable cross-project symbol id. Useful for cross-repo navigation; niche.                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `textDocument/selectionRange`       | 4.4   | ⏹️      | Hierarchical selection expansion (`+` / `-` keys).                                                                                                                                                                                                                                                                                                                                                                                                                                              |

### Edits

| Method                            | Phase | Status | Notes                                                                                                                                                                                                                                                                                                                                                             |
|-----------------------------------|-------|--------|-------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `textDocument/codeAction`         | 4.3   | ✅     | `:code-actions` (alias `:ca`). Picks first server with `codeActionProvider`; context carries overlapping diagnostics + `InvokedTriggerKind`. Visual selection / point cursor range. Vertico picker; accept routes `Command` payloads through `executeCommand` and `WorkspaceEdit` payloads through the rename apply path (per-file one-undo-unit).                |
| `codeAction/resolve`              | 4.3   | ✅     | Lazy-resolve fires when the chosen action arrived without both `edit` and `command`. Resolve response feeds the same apply path; the Resolved arm of `CodeActionOutcome` distinguishes from fresh-fetch.                                                                                                                                                          |
| `textDocument/rename`             | 4.3   | ✅     | `:rename <new-name>` (alias `:rn`). Single highest-priority server with `renameProvider`. WorkspaceEdit flattens both legacy `changes` map and modern `document_changes` Edits arm; `AnnotatedTextEdit` unwraps to plain TextEdit. Active buffer applies as one undo unit; cross-file edits open via `:e` then apply (cross-file atomic rollback is a follow-up). |
| `textDocument/prepareRename`      | 4.3   | ✅     | Runs before `rename` when the server advertises `prepare_provider`. RangeWithPlaceholder-shape responses pre-populate the new-name when the user types `:rename` with no arg; Range / DefaultBehavior responses fall through. NotRenameable echoes the server's reason.                                                                                           |
| `textDocument/formatting`         | 4.3   | ✅     | `:format` (alias `:fmt`). Highest-priority server with `documentFormattingProvider`; returned `Vec<TextEdit>` applies as one undo unit. TextEdits sorted in REVERSE by start position before apply (LSP convention: non-overlapping edits relative to original document). Single-server strategy per architecture doc.                                            |
| `textDocument/rangeFormatting`    | 4.3   | ✅     | `:format-range`. Active Visual selection (when in Visual) or whole buffer; same dispatch as `formatting`. `=` operator on motions / objects -- queued.                                                                                                                                                                                                            |
| `textDocument/onTypeFormatting`   | 4.3   | ✅     | Insert-mode trigger-character autopilot. `do_insert_text` fires the request when the typed char matches the server's `documentOnTypeFormattingProvider.first_trigger_character` / `more_trigger_character`. Edits land via the format-channel apply path (one undo unit).                                                                                         |
| `textDocument/linkedEditingRange` | 4.5   | ⏹️      | Multi-cursor mode for linked identifiers (HTML tag pairs, etc.).                                                                                                                                                                                                                                                                                                  |

### Decorations / inline information

| Method                                   | Phase | Status | Notes                                                                                                                                                                                                                                                                                                                             |
|------------------------------------------|-------|--------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `textDocument/publishDiagnostics`        | 4.1   | ✅     | Routing (4.1.d.i) ✅, per-URI state w/ version gating + multi-server merge (4.1.d.ii) ✅, renderer integration: gutter severity column + inline underline overlay (4.1.d.iii) ✅, `:diagnostics` help-style buffer w/ clickable Source links + `:diag-next` / `:diag-prev` / `:cnext` / `:cprev` cursor navigation (4.1.d.iv) ✅. |
| `textDocument/diagnostic`                | 4.4   | ⏹️      | Pull-based diagnostics (LSP 3.17). Used when server prefers pull over push.                                                                                                                                                                                                                                                       |
| `workspace/diagnostic`                   | 4.4   | ⏹️      | Workspace-wide pull.                                                                                                                                                                                                                                                                                                              |
| `textDocument/inlayHint`                 | 4.4   | ⏹️      | Renderer overlay; type/parameter hints inline.                                                                                                                                                                                                                                                                                    |
| `inlayHint/resolve`                      | 4.4   | ⏹️      | Lazy-resolve tooltip / textEdits when the hint gains focus.                                                                                                                                                                                                                                                                       |
| `textDocument/inlineValue`               | 4.5   | ⏹️      | Debug-flow value-at-line. Niche outside debugger integration.                                                                                                                                                                                                                                                                     |
| `textDocument/codeLens`                  | 4.5   | ⏹️      | Above-line clickable annotations (run / debug / references).                                                                                                                                                                                                                                                                      |
| `codeLens/resolve`                       | 4.5   | ⏹️      | Lazy-resolve the lens command.                                                                                                                                                                                                                                                                                                    |
| `textDocument/documentLink`              | 4.5   | ⏹️      | Hyperlinks inside text (URLs, imports). `gx` follows them.                                                                                                                                                                                                                                                                        |
| `documentLink/resolve`                   | 4.5   | ⏹️      | Lazy-resolve target.                                                                                                                                                                                                                                                                                                              |
| `textDocument/documentColor`             | 4.5   | ⏹️      | Color swatches at hex literals; `colorPresentation` returns named alternatives.                                                                                                                                                                                                                                                   |
| `textDocument/colorPresentation`         | 4.5   | ⏹️      | Companion to documentColor.                                                                                                                                                                                                                                                                                                       |
| `textDocument/foldingRange`              | 4.4   | ⏹️      | New `FoldMethod::Lsp` -- merges with tree-sitter fold provider via priority.                                                                                                                                                                                                                                                      |
| `textDocument/semanticTokens/full`       | 4.4   | ⏹️      | Per-doc semantic-token list; merges with tree-sitter highlight as a layer.                                                                                                                                                                                                                                                        |
| `textDocument/semanticTokens/full/delta` | 4.4   | ⏹️      | Delta encoding for re-requests after edits.                                                                                                                                                                                                                                                                                       |
| `textDocument/semanticTokens/range`      | 4.4   | ⏹️      | Viewport-bounded request; cheaper for large files.                                                                                                                                                                                                                                                                                |

## Logging & introspection (lattice-side)

These aren't LSP methods -- they're lattice's debugging
surface around LSP traffic. Tracked here so the matrix is the
single source of truth.

| Feature                                                                                                                                                               | Phase   | Status | Notes                                                                                                                                                                                                                                                                                                                                                                                                          |
|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------|--------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `LspLogger` (rings + level gating + trace toggle)                                                                                                                     | 4.1.f   | ✅     | `crate::logging`. ~9ns trace-off, ~100ns per record.                                                                                                                                                                                                                                                                                                                                                           |
| `tracing` crate fan-out                                                                                                                                               | 4.1.f   | ✅     | Every `LspLogger::log` also fires `tracing::*`. `RUST_LOG=lattice_lsp=debug` works.                                                                                                                                                                                                                                                                                                                            |
| `LspSupervisor` (per-buffer attachment + per-(workspace, server-id) actor reuse)                                                                                      | 4.1.h   | ✅     | `crate::supervisor`. open_buffer / close_buffer / record_edit / flush / flush_all / servers_for / shutdown. attach_handle for tests + custom transports. Supports multi-server-per-buffer + multi-buffer reuse of one actor.                                                                                                                                                                                   |
| App-side wiring: `App` holds `LspSupervisor`, `App::initialize_lsp` async boot, `lsp_record_edit` / `lsp_flush` / `lsp_close_buffer` sync hooks, `BufferId ↔ Uri` map | 4.1.i   | ✅     | `lattice-ui-tui::app`. App::new builds dormant supervisor with builtin_servers; runtime calls initialize_lsp before main loop. do_buffer_delete fires lsp_close_buffer.                                                                                                                                                                                                                                        |
| Edit dispatch: apply_edit_blocking → lsp_record_edit + debounced flush + open-on-`:e`                                                                                 | 4.1.i.2 | ✅     | `Arc<tokio::sync::Mutex<LspSupervisor>>` for shared async/sync access; cloned `DiagnosticsLayer` + `LspLogger` for lock-free renderer reads. apply_edit_blocking + apply_edit_batch_blocking call lsp_record_edit on success. Spawned debounce task wakes 50ms after the last edit signal, locks supervisor, calls flush_all. `:e <path>` queues an attachment; runtime drains the queue between input events. |
| `*lsp*` subsystem buffer                                                                                                                                              | 4.1.g   | ✅     | `:lsp-log` (no arg). Help-style buffer; one row per record from `LspLogger::snapshot_global()`. Format: `HH:MM:SS.mmm <level> <source>: <message>`.                                                                                                                                                                                                                                                            |
| `*lsp:<server>*` per-server buffer                                                                                                                                    | 4.1.g   | ✅     | `:lsp-log <server>`. Per-server records (stderr + window/logMessage + window/showMessage + lifecycle), trace records filtered out.                                                                                                                                                                                                                                                                             |
| `*lsp:<server>:trace*` JSON-RPC trace buffer                                                                                                                          | 4.1.g   | ✅     | `:lsp-trace <server>`. Filtered to `LogSource::Trace`; toggle inbound/outbound recording on/off; `←` / `→` markers in body.                                                                                                                                                                                                                                                                                    |
| `:lsp-log [server]` ex-command                                                                                                                                        | 4.1.g   | ✅     | `lsplog` alias.                                                                                                                                                                                                                                                                                                                                                                                                |
| `:lsp-trace <server>` ex-command                                                                                                                                      | 4.1.g   | ✅     | `lsptrace` alias. Flips toggle + opens trace buffer.                                                                                                                                                                                                                                                                                                                                                           |
| `:lsp-status` ex-command                                                                                                                                              | 4.1.g   | ✅     | One row per running server with workspace root + capability summary + diagnostic-subscriber count.                                                                                                                                                                                                                                                                                                             |
| `:lsp-restart <server>` ex-command                                                                                                                                    | 4.4     | 🚧     | Wired but no-op until the supervisor restart-with-backoff path lands in 4.4. Echoes a tidy "wiring in 4.4" message today.                                                                                                                                                                                                                                                                                      |
| `:lsp-log-level` / `:lsp-log-clear`                                                                                                                                   | 4.1.g   | ✅     | Runtime level: `:lsp-log-level <level>` (subsystem) or `:lsp-log-level <server> <level>` (per-server). Clear: `:lsp-log-clear [server]`.                                                                                                                                                                                                                                                                       |
| `lsp.toml` log keys (`log_level`, `log_capacity`, `trace_io`)                                                                                                         | 4.4     | ⏹️      | Lands when the §5.12 typed-options layer arrives. Runtime knobs work today via `:lsp-log-level` / `:lsp-trace`.                                                                                                                                                                                                                                                                                                |

## Notebook documents

| Method                       | Phase | Status | Notes                                                                  |
|------------------------------|-------|--------|------------------------------------------------------------------------|
| `notebookDocument/didOpen`   | post  | ⛔     | Lattice's notebook story is post-1.0 (rich-buffer rendering required). |
| `notebookDocument/didChange` | post  | ⛔     | --                                                                     |
| `notebookDocument/didSave`   | post  | ⛔     | --                                                                     |
| `notebookDocument/didClose`  | post  | ⛔     | --                                                                     |

## Tracking summary

Counts as of 2026-05-05:

| State          | Count |
|----------------|-------|
| ✅ Done        | 30    |
| 🚧 In progress | 5     |
| ⏹️ Planned      | 30    |
| ⛔ Deferred    | 4     |

Phase rollup:

- **4.1** Foundation: **complete**. Wire layer + actor/handshake + sync + diagnostics (broadcast → layer → renderer → buffer view + nav) + logging (rings + tracing fan-out + buffer views + commands) + supervisor + App-side wiring + edit-dispatch + open-on-`:e` all shipped. `workspace/configuration` surfaces real values from the merged user + project TOML tree via the `ConfigurationBus` mpsc + oneshot channel and the `App::drain_inbound_configuration_requests` drain; users place server-namespaced settings under `[lsp.<server-id>]`.
- **4.2** Navigation: **complete**. ✅ hover (`K`), definition (`gd`), declaration (`gD`), typeDefinition (`gy`), implementation (`gI`), references (`gr`), documentSymbol (`:lsp-symbols`), workspaceSymbol (`:lsp-workspace-symbol`), workspaceSymbol/resolve (eager-resolve at fan-out for LSP 3.17+ Nested-WorkspaceLocation symbols), Insert-mode completion (the full 4.2.g surface: shell + buffer-words + LSP source + docs popup + lazy completionItem/resolve + snippets + frequency / per-source priority / per-language overrides + tree-sitter + path source + commit chars + ghost text + cross-source dedup + typed picker routing). All multi-result lookups + `:diagnostics` route through the unified vertico picker (`PickerSource::LspLocations` + `PickerAction::JumpToLspLocation`); picker routing payload is now typed (`RoutingPayload` enum) so accept dispatch reads variants instead of parsing tab-encoded strings. Tag stack `<C-t>` pops `gd`-family drill-downs LIFO; jump list `<C-o>`/`<C-i>` walks every cursor jump chronologically. The `:complete` picker bridge stays as the cmdline-driven peer of the inline popup.
- **4.3** Edits: **complete**. formatting + rangeFormatting; signatureHelp + Insert-mode autopilot; rename + prepareRename; willSave / didSave notifications; willSaveWaitUntil format-on-save (500ms per-server bound); codeAction + resolve + executeCommand; onTypeFormatting Insert-mode autopilot; `workspace/applyEdit` (server-initiated) ferries through the `ApplyEditBus` mpsc + per-request oneshot, drained per-frame by `App::drain_inbound_apply_edits`. codeAction Commands keep routing through `executeCommand` (fire-and-forget); applyEdit is the inbound complement servers use after `executeCommand` callbacks.
- **4.4** Polish: 0/19 -- queued.
- **4.5** Expansion: 0/14 -- queued.
- **post-1.0**: notebooks + multi-root workspaces.

When you finish a feature, flip its Status column and update the
phase rollup. The matrix is the single source of truth: every
LSP feature in lattice has a row here, and every row reflects
reality.

## 4.4 slicing plan

Phase 4.4's items split into six affinity groups. Recommended
order is depth-first by group; rationale beside each. Slice
ids (`4.4.a`, `4.4.b`, ...) follow the same scheme as 4.2.g.

### Group 1 — server → editor messaging

`window/showMessage`, `window/showMessageRequest`, `window/showDocument`,
`window/logMessage`, `telemetry/event`, `$/setTrace` (C → S),
`$/logTrace` (S → C). All map onto primitives that already exist
(minibuffer notifications, modal pickers, buffer-open, the
`*lsp:<server>:trace*` buffer). Several are partially logged
today; flipping them to first-class UI is the highest UX-per-line
ratio of the phase.

- **4.4.a** -- `window/showMessage` + `window/logMessage` +
  `telemetry/event`. The three currently-logged items get a
  proper UI surface (minibuffer for show, opt-in plugin
  subscription for telemetry).
- **4.4.b** -- `window/showMessageRequest` (modal action-button
  picker) + `window/showDocument` (URI open via existing buffer
  / browser path) + `$/setTrace` + `$/logTrace`. The trace pair
  flips `:lsp-trace` to drive `$/setTrace` on the wire; logTrace
  routes into the existing trace buffer.

### Group 2 — server → editor live state

`$/progress` → modeline progress slot, `window/workDoneProgress/cancel`,
plus the supervisor work needed for `:lsp-restart` to actually
shut down + respawn an actor and replay `didOpen` to every attached
buffer. Stays close to the per-actor DocSync work that's fresh in
mind.

- **4.4.c** -- `$/progress` accumulator + modeline slot +
  `workDoneProgress/cancel`.
- **4.4.d** -- supervisor restart-with-backoff + `:lsp-restart`
  wiring + crash-detection auto-restart (the supervisor's "today
  the actor detects pipe close" comment becomes "and the
  supervisor responds with a graceful respawn").

### Group 3 — new renderer overlays

`textDocument/documentHighlight`, `textDocument/selectionRange`,
`textDocument/inlayHint` + `inlayHint/resolve` + workspace refresh,
`textDocument/foldingRange`. Each adds a renderer layer; can land
in any order. Suggested grouping is by infra reuse:

- **4.4.e** -- `documentHighlight` + `selectionRange`. Both
  cursor-driven, both small; share the "request-on-cursor-move"
  plumbing we'll need anyway for inlay hint hover-resolve.
- **4.4.f** -- `foldingRange`. New `FoldMethod::Lsp` provider
  feeding the existing fold infra (recompute, identity hash,
  gutter glyphs all reusable).
- **4.4.g** -- inlay hints (`textDocument/inlayHint` +
  `inlayHint/resolve` + `workspace/inlayHint/refresh`). Renderer
  virtual-text overlay is new infrastructure; landing it once
  here pays for future inline-value / inline-completion work too.

### Group 4 — semantic tokens

`textDocument/semanticTokens/full` + `/delta` + `/range`,
`workspace/semanticTokens/refresh`. Most complex single feature in
4.4: relative-position varint encoding, modifier bitmask, layered
highlight resolution against tree-sitter. Stays standalone so the
tree-sitter highlight fast paths don't churn under unrelated
changes.

- **4.4.h** -- `semanticTokens/full` + highlight-layer integration
  (LSP layer ranks above tree-sitter when both produce a token at
  the same range).
- **4.4.i** -- `semanticTokens/full/delta` + `semanticTokens/range`
  + `workspace/semanticTokens/refresh` (delta encoding + viewport
  request + invalidation trigger).

### Group 5 — pull-based + workspace integration

Pull diagnostics, configuration push, file watchers. The largest
single dependency surface in 4.4 (the `notify` crate) lands here.

- **4.4.j** -- pull diagnostics (`textDocument/diagnostic`,
  `workspace/diagnostic`, `workspace/diagnostic/refresh`).
  Capability-gated; when a server advertises pull-only,
  `DiagnosticsLayer` switches its source from the push subscription
  to a periodic + on-edit-debounce pull request.
- **4.4.k** -- `workspace/didChangeConfiguration` driven by §5.12
  `OptionChanged` events on `lsp.*` keys. Pairs naturally with the
  inbound `workspace/configuration` path that already exists.
- **4.4.l** -- file watchers: `notify` crate dep, per-server
  glob-pattern subscriptions matched against
  `client/registerCapability`'s file-watcher registrations,
  `workspace/didChangeWatchedFiles` fan-out.
- **4.4.m** -- workspace file lifecycle (`willCreateFiles` /
  `didCreateFiles` / `willRenameFiles` / `didRenameFiles` /
  `willDeleteFiles` / `didDeleteFiles`). Hooks into our buffer
  save / `:bd` / `:saveas` paths; pre-events may return
  `WorkspaceEdit`s to apply (reuses the rename apply pipeline).

### Group 6 — capability + config polish

- **4.4.n** -- dynamic registration tracking. Today
  `client/registerCapability` accepts and drops; this slice gives
  `Capabilities` a "static + dynamic" snapshot model so feature
  dispatch reads from the union.
- **4.4.o** -- `lsp.toml` log keys (`log_level`, `log_capacity`,
  `trace_io`) wired through the §5.12 typed-options layer at
  startup; today only the runtime `:lsp-log-level` / `:lsp-trace`
  paths exist.

### Trade-offs flagged

- **Semantic tokens (Group 4) ahead of overlays (Group 3)** is
  defensible if you'd rather get the highest-impact polish in
  first, but it's also the highest churn risk -- highlight
  layering interacts with tree-sitter and the renderer's fast
  paths. Default order keeps it after the overlays so the
  renderer changes that group introduces are stable before
  semantic tokens layer on top.
- **File watchers (Group 5) earlier as standalone infra** would
  unlock non-LSP features (markdown live-preview, etc.) sooner,
  but they're standalone enough to stay where they are without
  blocking anything.
- **`:lsp-restart` (4.4.d) could fold into 4.4.c** as a single
  "supervisor lifecycle" slice, but the two touch different
  surfaces (modeline rendering vs. actor lifecycle) and read
  cleaner as separate commits.
