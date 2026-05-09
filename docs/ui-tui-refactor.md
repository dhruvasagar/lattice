# lattice-ui-tui App Module Refactor (R.1.x)

Tracking doc for the long-running slice sequence that drains
`crates/lattice-ui-tui/src/app.rs` into per-concern submodules
under `crates/lattice-ui-tui/src/app/`. Each slice is one
commit; green tests gate every commit. Read this before
picking the next slice.

Authoritative slice ledger; the commit log is the source of
truth, this doc is the index.

## 1. Goal

`app.rs` started as the place where the App impl grew. It
crossed 17k lines, which makes review slow and natural seams
invisible. R.1.x is a mechanical move-only refactor that:

- Cuts the file along feature lines (visual, search, picker,
  folds, LSP, motions, edits, lifecycle, ...) so each
  cohesive surface lives next to its peers.
- Tightens visibility -- `pub fn` becomes `pub(super) fn`
  whenever no caller outside the crate exists.
- Surfaces deferred decisions (every module's header
  documents what stayed in `app.rs` and why), so future
  slices can pick up exactly where the previous one stopped.
- Prepares ground for the M.* mode-architecture migration
  (`docs/mode-architecture.md` §10) -- once an App method is
  in the right per-concern file, moving it across the
  `lattice-mode` boundary later is one local change, not a
  cross-file untangle.

Non-goals: no behaviour change, no API redesign, no test
rewrites. If a slice is tempted to refactor logic, it stops
and files a follow-up instead.

## 2. Conventions

These are the patterns the existing 56 slices have settled on.
New slices match these unless there's a documented reason.

- **One coherent surface per slice.** A method or a tight
  cluster of methods that share state and callers. Examples:
  the four LSP completion drains landed as one slice (R.1.38),
  but the read side and write side of the position-history
  ring landed in two slices (R.1.46 walkers, R.1.55 writer).
- **Move-only.** No logic edits in the same commit. If a bug
  shows up during the move, file it; don't bundle.
- **Visibility tightens.** `pub fn` → `pub(super) fn` when no
  out-of-crate caller exists. Tests inside `app.rs` count as
  same-module callers (visibility from `app::motions` is
  visible in `app`).
- **Helpers travel with their sole user.** A constant or
  free function used only by the moving method moves with it
  (e.g. R.1.55 took `POSITION_HISTORY_CAP` along).
- **Module header is part of the slice.** `app/<file>.rs`
  opens with a doc-comment listing (a) what lives here, (b)
  what's deferred and why. Updates are part of every slice
  that touches the module.
- **Commit message style:**
  `refactor(ui-tui): R.1.NN -- <summary> move to <file>.rs`.
  Body is two or three lines explaining the *why* (paired
  with R.1.MM, reunites read+write side, etc.) when it isn't
  obvious from the summary.
- **Tests follow methods.** When a method moves, its
  unit tests can move with it; relocations are part of the
  slice. Tests left in `app.rs::tests` for now will migrate
  during the test-cleanup pass at the end.
- **`app.rs` keeps:** the `App` struct definition, free
  helpers used cross-module (`line_byte_len`,
  `last_addressable_line`, `is_word_char_byte`,
  `is_path_byte`, `is_blank_line`, `preview_register`,
  `is_valid_mark_name`), the `OptionCache` /
  `PositionEntry` / `EffectiveCompletionConfig` / etc.
  data types, and `impl Debug for App`.

## 3. Submodule layout (after R.1.88)

| File                   | Lines | Theme                                                              |
|------------------------|-------|--------------------------------------------------------------------|
| `app.rs`               | 12957 | App struct + types + free helpers + tests (~10k tests)             |
| `app/lsp.rs`           |  3378 | every `lattice-lsp` consumer + intro helpers + log openers         |
| `app/completion.rs`    |  1521 | popup state machine, ranker, ghost text, snippets, refilter, `EffectiveCompletionConfig`, `active_language_id` |
| `app/lifecycle.rs`     |  1421 | activate-buffer, pane tree, `:e` / `:w` / `:q` / `:bn` / `:ls`, `<C-l>`, document swap, save family, help adoption, buffer-state, jump_to_file_line_col, publish_*_changed |
| `app/dispatch.rs`      |  1389 | the dispatch core: `apply` / `apply_effect` / `apply_app_effect` / `handle_edits` / `dispatch_blocking` / `run_*_invocation` / `execute_ex_line` / Effect classifiers |
| `app/folds.rs`         |  1344 | fold compute / open / close / auto-open                            |
| `app/search.rs`        |  1125 | `/`, `?`, `:s`, `:%s`, find / find-next / find-reverse             |
| `app/options.rs`       |   922 | `:set`, typed options, customize machinery, per-language overrides + pending-section drainers + 9 typed-option getters + 3 test setters |
| `app/edit.rs`          |   729 | actor-bridge mutation wrappers, yank / paste / register store / Insert+Replace primitives / `:d` / block-insert |
| `app/boot.rs`          |   698 | `App::new`, `build_lsp_subsystem`, `load_persistent_config`, `sync_keymap_overlays`, `sync_theme_from_config` |
| `app/motions.rs`       |   671 | bracket match, history walkers + writer, mark jump, viewport / scroll, cursor clamp, viewport sizing, active-buffer accessors |
| `app/picker.rs`        |   665 | picker state machine + buffer/LSP-instance candidate builders      |
| `app/cmdline.rs`       |   613 | `:` minibuffer + missing-arg prompt + chord-capture gate + cmdline completion (incl. open-popup) |
| `app/help.rs`          |   575 | `:help`, `:describe-*`, `:apropos`, `:keymap`, `do_help_follow_link` |
| `app/highlights.rs`    |   485 | tree-sitter highlight cache + per-frame refresh + post-edit shift  |
| `app/visual.rs`        |   303 | charwise / linewise / blockwise selection state, `set_selections_blocking` |
| `app/file_tree.rs`     |   203 | file-tree buffer ops                                               |
| `app/mode.rs`          |   195 | `modal_label` + `enter_mode` + `activate_major_for_buffer_kind`    |
| `app/macros.rs`        |   194 | `q` recording / `@` replay                                          |
| `app/oil.rs`           |   185 | oil buffer ops (incl. navigate-up)                                  |
| `app/test_helpers.rs`  |   129 | shared test fixtures                                                |
| `app/syntax.rs`        |    75 | `maybe_reparse_syntax`                                              |
| `app/state.rs`         |    23 | type-only stub (per its header)                                    |
| `app/operators.rs`     |    22 | operator-pending plumbing                                           |

## 4. Slices done (R.1.0 -- R.1.88)

| #      | Slice                                                                 |
|--------|-----------------------------------------------------------------------|
| R.1.0  | create `app/` submodule skeleton                                      |
| R.1.1  | `app/macros.rs`                                                       |
| R.1.2  | `app/visual.rs`                                                       |
| R.1.3  | `app/picker.rs`                                                       |
| R.1.4  | `app/search.rs` (`/`, `?`, `:s`, find)                                |
| R.1.5  | `app/options.rs`                                                      |
| R.1.6  | `app/folds.rs`                                                        |
| R.1.7  | `app/cmdline.rs`                                                      |
| R.1.8  | `app/completion.rs` popup state                                       |
| R.1.9  | `app/help.rs` writers (`:describe-*`, `:apropos`, `:help`, `:keymap`) |
| R.1.10 | `app/file_tree.rs`                                                    |
| R.1.11 | `app/oil.rs`                                                          |
| R.1.12 | `app/syntax.rs` (`maybe_reparse_syntax`)                              |
| R.1.13 | LSP admin commands → `app/lsp.rs`                                     |
| R.1.14 | `%` bracket motion → `app/motions.rs`                                 |
| R.1.15 | `J` / `gJ` / `~` → `app/edit.rs`                                      |
| R.1.16 | pane navigation → `app/lifecycle.rs`                                  |
| R.1.17 | hover popup → `app/help.rs`                                           |
| R.1.18 | diagnostics commands → `app/lsp.rs`                                   |
| R.1.19 | `:bn` / `:bp` / `:bd` → `app/lifecycle.rs`                            |
| R.1.20 | snippets → `app/completion.rs`                                        |
| R.1.21 | `activate_*` family → `app/lifecycle.rs`                              |
| R.1.22 | pane-tree mutations → `app/lifecycle.rs`                              |
| R.1.23 | `:e` / `:w` / `:q` → `app/lifecycle.rs`                               |
| R.1.24 | hover request → `app/lsp.rs`                                          |
| R.1.25 | LSP nav request family → `app/lsp.rs`                                 |
| R.1.26 | signature help → `app/lsp.rs`                                         |
| R.1.27 | LSP symbol requests → `app/lsp.rs`                                    |
| R.1.28 | LSP format request → `app/lsp.rs`                                     |
| R.1.29 | `drain_pending_references` + `jump_to_lsp_location` → `app/lsp.rs`    |
| R.1.30 | LSP completion request → `app/lsp.rs`                                 |
| R.1.31 | LSP code action + apply paths → `app/lsp.rs`                          |
| R.1.32 | LSP rename request → `app/lsp.rs`                                     |
| R.1.33 | LSP buffer helpers + `apply_lsp_text_edits` → `app/lsp.rs`            |
| R.1.34 | on-type formatting + trigger-char helpers → `app/lsp.rs`              |
| R.1.35 | `publish_document_opened` + `lsp_completion_meta_for` → `app/lsp.rs`  |
| R.1.36 | inbound LSP drains → `app/lsp.rs`                                     |
| R.1.37 | LSP log buffer refresh → `app/lsp.rs`                                 |
| R.1.38 | LSP completion drains → `app/lsp.rs`                                  |
| R.1.39 | LSP insert completion request + apply → `app/lsp.rs`                  |
| R.1.40 | docs: refresh `app/lsp.rs` module header                              |
| R.1.41 | `do_completion_trigger` → `app/completion.rs`                         |
| R.1.42 | completion accept + ghost text → `app/completion.rs`                  |
| R.1.43 | completion docs popup + post-edit refresh → `app/completion.rs`       |
| R.1.44 | completion populate / refilter / ranker → `app/completion.rs`         |
| R.1.45 | paste family → `app/edit.rs`                                          |
| R.1.46 | position-history walkers → `app/motions.rs`                           |
| R.1.47 | insert-mode entry methods → `app/edit.rs`                             |
| R.1.48 | viewport + scroll family → `app/motions.rs`                           |
| R.1.49 | tag-stack pop + mark jump → `app/motions.rs`                          |
| R.1.50 | list ex-commands → `app/lifecycle.rs`                                 |
| R.1.51 | `:d` / `:g` / `:v` ex-command bodies → `app/edit.rs`                  |
| R.1.52 | Insert/Replace edit primitives → `app/edit.rs`                        |
| R.1.53 | `do_oil_navigate_up` → `app/oil.rs`                                   |
| R.1.54 | `store_yank` + `read_register` → `app/edit.rs`                        |
| R.1.55 | `push_position_history` → `app/motions.rs`                            |
| R.1.56 | cursor-clamp + `ensure_cursor_visible` → `app/motions.rs`             |
| R.1.57 | `replicate_block_insert` → `app/edit.rs`                              |
| R.1.58 | `snippet_variable_context` → `app/completion.rs`                      |
| R.1.59 | cmdline-submit helpers (`try_resolve_missing_arg_prompt` + `chord_capture_active` + `MissingArgPrompt`) → `app/cmdline.rs` |
| R.1.60 | actor-bridge edit wrappers (`apply_edit_blocking` + `apply_edit_batch_blocking` + `undo_blocking` + `redo_blocking`) → `app/edit.rs` |
| R.1.61 | `replace_document_blocking` → `app/lifecycle.rs`                      |
| R.1.62 | viewport-sizing accessors (`set_viewport_height` + `active_pane_content_height` + `help_popup_inner_height`) → `app/motions.rs` |
| R.1.63 | `modal_label` → `app/mode.rs`                                         |
| R.1.64 | `do_redraw_screen` (`<C-l>`) → `app/lifecycle.rs`                     |
| R.1.65 | cmdline-completion engine (`compute_completion_state` + `refresh_completion_popup` + `CompletionComputeError` + `prefer_aliases_for_command_candidates` + `subsequence_match_ranges`) → `app/cmdline.rs` |
| R.1.66 | per-language overrides loader (`apply_per_language_toml_overrides` + `parse_per_language_overrides_table`) → `app/options.rs` |
| R.1.67 | save family (`save_blocking` + `save_as_blocking` + `fire_will_save_notifications` + `run_will_save_wait_until_blocking` + `fire_did_save_notifications`) → `app/lifecycle.rs` |
| R.1.68 | snippet expansion (`snippet_meta_for` + `expand_snippet_with_lsp_edits` + `expand_snippet`) → `app/completion.rs` |
| R.1.69 | LSP-log-in-pane openers (`open_lsp_log_in_pane` + `open_lsp_trace_log_in_pane`) → `app/lsp.rs` |
| R.1.70 | pending-structural-section drainers (`take_pending_structural_section` + `pending_structural_section_paths`) → `app/options.rs` |
| R.1.71 | effective completion config (`EffectiveCompletionConfig` + `source_enabled` + `effective_completion_for`) → `app/completion.rs` |
| R.1.72 | `do_help_follow_link` → `app/help.rs`                                 |
| R.1.73 | help-buffer adoption (`open_help` + `open_help_in_pane` + `seed_help_locals`) → `app/lifecycle.rs` |
| R.1.74 | `set_selections_blocking` → `app/visual.rs`                           |
| R.1.75 | `jump_to_file_line_col` → `app/lifecycle.rs`                          |
| R.1.76 | `open_completion_popup` → `app/cmdline.rs`                            |
| R.1.77 | `active_language_id` → `app/completion.rs`                            |
| R.1.78 | buffer-state lifecycle accessors (`find_document_by_path` + `snapshot_active_document` + `activate_buffer_state` + `active_pane_buffer_id`) → `app/lifecycle.rs` |
| R.1.79 | pane-snapshot + buffer-area-rect (`snapshot_active_pane` + `buffer_area_rect`) → `app/lifecycle.rs` |
| R.1.80 | `enter_mode` → `app/mode.rs`                                          |
| R.1.81 | highlights pipeline (`refresh_highlights` family + `VisibleHighlightsKey` + `shift_*` cache shifters + `visible_buffer_line_extent` + accessors) → new `app/highlights.rs` |
| R.1.82 | boot/sync methods (`sync_keymap_overlays` + `sync_theme_from_config` + `load_persistent_config`) → new `app/boot.rs` |
| R.1.83 | `App::new` + `build_lsp_subsystem` → `app/boot.rs`                    |
| R.1.84 | typed-option getters (9 accessors + 3 test setters) → `app/options.rs`; `activate_major_for_buffer_kind` → `app/mode.rs` |
| R.1.85 | LSP intro helpers (`publish_position_change` + `resolve_server_id` + `running_server_ids`) → `app/lsp.rs`; event publishers (`publish_document_changed` + `publish_selections_changed`) → `app/lifecycle.rs` |
| R.1.86 | dispatch core (`apply` + `apply_effect` + `apply_app_effect` + `handle_edits` + `dispatch_blocking` + 6 `run_*` routers + `execute_ex_line` + 5 helpers) → new `app/dispatch.rs` |
| R.1.87 | active-buffer accessors (`active_buffer_id` + `active_cursor` + `active_text`) → `app/motions.rs` |
| R.1.88 | shrink the App-impl block to `set_message` only (cleanup; no method moves) |

## 5. Pending candidates

All App-impl methods are out. What's left in `app.rs` is
about **2,555 lines of production code** (down from 17,849
at R.1.0 -- 85.7% drained) plus **~10,400 lines of tests**.
The production residue is:

- The `App` struct definition itself (~770 lines of fields).
- A `tiny impl App { pub fn set_message }` block.
- Public type definitions used as message-bus payloads
  (`HoverOutcome`, `ReferencesOutcome`, `CompletionOutcome`,
  `RenameOutcome`, `SignatureHelpOutcome`, `FormatOutcome`,
  `CodeActionOutcome`, `SymbolsOutcome`, `LspNavKind`,
  `LspCompletionMeta`, `CompletionItemRow`,
  `CompletionResolveOutcome`, `InsertCompletionLspOutcome`,
  `CodeActionRow`, `SymbolRow`, `SnippetCandidateMeta`,
  `LSP_COMPLETION_KIND_ID`, `SNIPPET_COMPLETION_KIND_ID`,
  `CompletionState`, `PathCompletionCache`, `Fold`, ...).
- Cross-feature data types (`Action`, `FindKind`,
  `EchoMessage`, `EchoLevel`, `SearchLine`, `LastSearch`,
  `UnnamedRegister`, `PrevPaneState`, `MacroRecording`,
  `TagStackEntry`, `LastFind`, `ReplaceEntry`,
  `PendingBlockInsert`, `OptionCache`, `PositionEntry`,
  `PositionSource`, ...).
- `impl Default for App` and `impl Debug for App`.
- Cross-module free helpers (`line_byte_len`,
  `last_addressable_line`, `is_word_char_byte`,
  `is_path_byte`, `is_blank_line`, `preview_register`,
  `is_valid_mark_name`, `dedup_rendered_by_text`,
  `word_under_cursor`, `lsp_position_to_app_byte`,
  `resolve_command_name_or_alias`).

### Optional follow-up slices

The R.1.x sequence's primary goal -- breaking the
monolithic `impl App` block apart -- is complete. The
remaining churn is more of a polish:

- **Type relocation.** LSP outcome types could move to
  `app/lsp.rs`; completion types to `app/completion.rs`.
  This would drain another ~500 lines but the types are
  used as App field types, so the `app.rs` struct
  definition still imports them. Net visibility win is
  modest.
- **Test relocation.** ~10,400 lines of tests in
  `app.rs`'s `mod tests` block. Each test exercises a
  feature that has moved to a per-feature module; the
  tests can migrate next to their target. High churn
  but mechanical. Best done in 5--10 batches grouped by
  feature.


  belong with `mode.rs`, some with `state.rs`. Will land
  as one or two cleanup slices near the end.

### Documentation / tests pass (R.1.x.final)

## 6. End-state achieved (R.1.88)

`app.rs` now holds:

- The `App` struct definition + `impl Default` /
  `impl Debug for App`.
- A single `impl App { pub fn set_message }` block.
- Type definitions used as App field types or message-bus
  payloads (`Action`, the LSP outcome enums + structs,
  `OptionCache`, `PositionEntry`, `LspNavKind`,
  `SnippetCandidateMeta`, `LspCompletionMeta`,
  `CompletionState`, `PathCompletionCache`, `Fold`,
  `EchoMessage`, `EchoLevel`, `SearchLine`, ...).
- The inherent impl methods on those types (`LspNavKind`'s
  `noun_plural` / `noun_singular`, `PositionEntry`'s
  `is_jump` / `is_named_mark`).
- Cross-module free helpers (`line_byte_len`,
  `last_addressable_line`, `is_word_char_byte`,
  `is_path_byte`, `is_blank_line`, `preview_register`,
  `is_valid_mark_name`, `dedup_rendered_by_text`,
  `word_under_cursor`, `lsp_position_to_app_byte`,
  `resolve_command_name_or_alias`).
- A `mod tests` block (~10,400 lines) -- the dominant
  remaining mass; per-feature relocation is the obvious
  follow-up.

Production code line count: **2,555 lines** (down from
17,849 -- a 14.3% residue, **85.7% drained**). The
end-state target of "under 2000 lines" was set at R.1.0
when only data types were expected to remain; the actual
data-type mass is larger than expected (LSP outcome
enums are ~280 lines on their own) and the App struct
field list is ~770 lines.

## 7. After R.1.x

This refactor sets up the M.* mode-architecture migration
(`docs/mode-architecture.md` §10). Specifically: every per-
concern module under `app/` becomes a candidate adapter
boundary when its methods graduate to `Mode::on_activate` /
`Mode::on_deactivate` hooks or to mode-owned `BufferLocal`
data. Without R.1.x, that migration would have to fish each
method out of a 17k-line file before relocating; with R.1.x
done, the M.* slices are local edits.

R.1.x is therefore not a vanity refactor -- it's prep.
