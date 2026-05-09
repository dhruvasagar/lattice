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

## 3. Submodule layout (after R.1.65)

| File                   | Lines | Theme                                                              |
|------------------------|-------|--------------------------------------------------------------------|
| `app.rs`               | 17143 | App struct + remaining methods + types + free helpers + tests      |
| `app/lsp.rs`           |  3296 | every `lattice-lsp` consumer (requests, drains, log buffers)       |
| `app/folds.rs`         |  1344 | fold compute / open / close / auto-open                            |
| `app/completion.rs`    |  1250 | popup state machine, ranker, ghost text, snippets, refilter        |
| `app/search.rs`        |  1125 | `/`, `?`, `:s`, `:%s`, find / find-next / find-reverse             |
| `app/lifecycle.rs`     |   839 | activate-buffer, pane tree, `:e` / `:w` / `:q` / `:bn` / `:ls`, `<C-l>`, document swap |
| `app/edit.rs`          |   729 | actor-bridge mutation wrappers, yank / paste / register store / Insert+Replace primitives / `:d` / block-insert |
| `app/options.rs`       |   690 | `:set`, typed options, customize machinery                         |
| `app/picker.rs`        |   665 | picker state machine + buffer/LSP-instance candidate builders      |
| `app/motions.rs`       |   598 | bracket match, history walkers + writer, mark jump, viewport / scroll, cursor clamp, viewport sizing |
| `app/cmdline.rs`       |   581 | `:` minibuffer + missing-arg prompt + chord-capture gate + cmdline completion |
| `app/help.rs`          |   389 | `:help`, `:describe-*`, `:apropos`, `:keymap`                       |
| `app/visual.rs`        |   295 | charwise / linewise / blockwise selection state                    |
| `app/file_tree.rs`     |   203 | file-tree buffer ops                                               |
| `app/macros.rs`        |   194 | `q` recording / `@` replay                                          |
| `app/oil.rs`           |   185 | oil buffer ops (incl. navigate-up)                                  |
| `app/test_helpers.rs`  |   129 | shared test fixtures                                                |
| `app/syntax.rs`        |    75 | `maybe_reparse_syntax`                                              |
| `app/mode.rs`          |    50 | `modal_label`; remaining mode-transition entries deferred           |
| `app/state.rs`         |    23 | small state accessors                                               |
| `app/operators.rs`     |    22 | operator-pending plumbing                                           |

## 4. Slices done (R.1.0 -- R.1.65)

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

## 5. Pending candidates

No formal slice plan exists past R.1.65; each slice is picked
when picked. The clusters below are the visible candidates
from a survey of `app.rs` after R.1.65. Numbers are
heuristic -- some clusters fragment into 2--3 sub-slices
(the way M.3.2 did in `mode-architecture.md`); some collapse
into one. Rough envelope: **15--25 slices** to fully drain
App-impl methods from `app.rs`.

Listed by destination, not priority. Within each destination,
cohesion to existing residents is the strongest signal for
which slice to pick first.

### → `app/lifecycle.rs`

- **Save family.** `save_blocking`, `save_as_blocking`,
  `fire_will_save_notifications`,
  `run_will_save_wait_until_blocking`,
  `fire_did_save_notifications`. Big and self-contained;
  pairs with `:w` already living there. Could land as one
  slice or split write/notify.
- **Boot / config.** `new`, `build_lsp_subsystem`,
  `load_persistent_config`, `apply_per_language_toml_overrides`,
  `sync_keymap_overlays`, `sync_theme_from_config`. Largest
  remaining cluster; needs its own naming -- maybe
  `app/boot.rs` rather than overloading lifecycle.
- **Selection-set wrapper.** `set_selections_blocking` --
  the only remaining `*_blocking` actor-bridge in `app.rs`.
  Used everywhere a selection commits; `render.rs` is the
  outside caller (so it stays `pub`). No obvious feature
  home -- candidates are visual.rs (heaviest user) or stay
  as a primitive. Pick when its placement is clear.

### → `app/highlights.rs` (new)

- `refresh_highlights`, `refresh_pane_highlights`,
  `highlights_for_viewport_row`,
  `highlights_for_buffer_line`, `shift_highlights_for_edit`,
  `shift_spans_within_line`, `visible_buffer_line_extent`.
  Cluster is large; introducing the file is itself the
  first slice, with the actual moves as follow-ups.

### → `app/dispatch.rs` (new) or split across existing files

- The big router family: `apply` (the Action dispatcher),
  `apply_app_effect`, `dispatch_blocking`, `handle_edits`,
  `run_oil_invocation`, `run_file_tree_invocation`,
  `run_help_invocation`, `run_read_only_motion`,
  `run_document_invocation`. Open question: is this one
  module or do `run_*_invocation` migrate to their
  respective feature files? Most likely split: routers go
  to their kind, central `apply` stays in `app.rs` as the
  one place all kinds meet.

### → `app/help.rs`

- **Help-flow finishers** (deferred per `app/help.rs` header):
  `do_help_follow_link`, `open_help_in_pane`,
  `seed_help_locals`. Entangled with lifecycle and the help-
  popup overlay; pick when the State A / State B split
  stabilises.

### → `app/cmdline.rs`

- `execute_ex_line` (parser + dispatcher entry for the `:`
  line). Big and entangled with every command surface;
  worth a careful look but not a small slice.

### → `app/motions.rs`

- `active_cursor`, `active_text`, `active_buffer_id`,
  `active_pane_buffer_id` (the buffer-kind-aware
  accessors). Small slice; could also live in
  `app/state.rs`.

### → `app/state.rs` or `app/mode.rs`

- `modal_label`, `set_message`, `set_viewport_height`,
  `active_pane_content_height`, `help_popup_inner_height`,
  `do_redraw_screen`. Small accessors / commands; some
  belong with `mode.rs`, some with `state.rs`. Will land
  as one or two cleanup slices near the end.

### Documentation / tests pass (R.1.x.final)

- One slice that walks the test module in `app.rs` and
  relocates each `#[test]` to the file whose method it
  exercises. Pure motion; high churn; do it last.
- One slice that updates `docs/IMPLEMENTATION.md` to point at
  the new module homes.

## 6. End-state target

When R.1.x finishes, `app.rs` should hold:

- The `App` struct definition + `impl Default` /
  `impl Debug for App`.
- The `OptionCache`, `PositionEntry`, `EffectiveCompletionConfig`,
  `LspNavKind`, `CompletionComputeError` data types.
- Cross-module free helpers (`line_byte_len`,
  `last_addressable_line`, `is_word_char_byte`,
  `is_path_byte`, `is_blank_line`, `preview_register`,
  `is_valid_mark_name`).
- `effect_mutates`, `effect_mutates_or_yanks` -- effect
  classifiers used by both the dispatcher and visual mode.
- The central `App::apply` dispatcher (one place all
  feature kinds meet).
- A `mod tests` block that contains only the cross-feature
  integration tests; per-feature tests live next to their
  feature module.

Everything else moves out. A reasonable end-state target
is **`app.rs` under 2000 lines**.

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
