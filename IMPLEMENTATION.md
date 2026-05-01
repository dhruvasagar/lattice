# Lattice — Implementation Tracker

This doc is the **current-state ledger** for the v1.0 build. It maps every
feature back to its anchor in DESIGN.md / CLAUDE.md and shows what's done,
what's in flight, and what's still pending.

Commit history is the authoritative log of *what changed when*. This file is
the authoritative answer to *where are we against the spec*.

When closing a session, update the status of every row touched. When opening
a session, read the **In-progress** and **Up next** sections — those are the
session pickup points.

---

## v1.0 scope at a glance

The four paramount goals from CLAUDE.md (in priority order when they
conflict):

1. **Performance.** Sub-frame keystroke-to-glyph (§8.2 commitments).
2. **Extensibility.** WebAssembly Component Model plugin host (§5.5, §9).
3. **Strict vim modal editing** with one deviation: unified command/grammar
   dispatch (§5.2.1).
4. **Asynchronous, multi-threaded core** that never blocks the UI (§3, §5.7).

The roadmap (§13) is divided into 11 phases (Phase 0 through Phase 10).
Phases 0–3 land the foundation, modal engine, terminal UI, and tree-sitter.
Phases 4–10 add LSP, GPU rendering, plugin host, modes, rich buffer
rendering, and v1.0 polish.

---

## Phase status

| Phase | Title                                 | Status                                                            | Notes                                                                                         |
|-------|---------------------------------------|-------------------------------------------------------------------|-----------------------------------------------------------------------------------------------|
| 0     | Foundation                            | ✅ done                                                           | Workspace, lattice-core, document/buffer/undo, file I/O, protocol enums                       |
| 1     | Modal Editing                         | ✅ done (engine), 🟡 catalog (most builtins land + chord routing) | Vim grammar fully dispatchable; remaining builtin gaps tracked below                          |
| 2     | Terminal UI Bootstrap                 | ✅ done                                                           | crossterm + ratatui; modal cursor; mode line; gutter                                          |
| 3     | Tree-Sitter                           | ✅ done (Rust/Python/JS)                                          | Highlights wired; grammar extension API used by builtins, not yet by plugins                  |
| 4     | LSP                                   | ⛔ not started                                                    | Phase 4 still waiting; depends on async-actor work and the Pending->Effect plumbing in §5.2.1 |
| 5     | GPU Rendering Foundation              | ⛔ not started                                                    | TUI is the live renderer for v1; GPU is a separate paint surface                              |
| 6     | Document Renderer + UI Components     | ⛔ not started                                                    | Popups, pickers, panels-as-buffers all live in §5.9                                           |
| 7     | Plugin Host                           | ⛔ not started                                                    | wasmtime + Component Model + WIT scaffolding                                                  |
| 8     | Major/Minor Modes + Reference Plugins | ⛔ not started                                                    | Major / minor modes are themselves plugins (§5.8.3)                                           |
| 9     | Rich Buffer Rendering                 | ⛔ not started                                                    | Per-line shaped path, Fenwick height index                                                    |
| 10    | Polish + v1.0                         | ⛔ not started                                                    | `*scratch:rust*` live-eval workflow (§10), accessibility, packaging, themes                   |

Active focus: **closing the remaining vim grammar gaps (Phase 1 catalog) and
tightening the §15 open questions.**

---

## Vim grammar coverage (Phase 1 catalog)

This section enumerates every named primitive in vim's grammar against its
status here. Anchor: DESIGN.md §5.2 + the seven unifications in §5.10–§5.12.

### Modal states

| State              | Status | Anchor    | Notes                                                       |
|--------------------|--------|-----------|-------------------------------------------------------------|
| Normal             | ✅     | §5.2      | Plus block cursor in TUI                                    |
| Insert             | ✅     | §5.2      | Plus bar cursor                                             |
| Visual (Charwise)  | ✅     | §5.2, B.1 | Selection extends, operators on Range::Selection            |
| Visual (Linewise)  | ✅     | §5.2      |                                                             |
| Visual (Blockwise) | ⚠️     | §15:18    | Ctrl-V enters; render highlights the rectangle. Operators fall back to charwise (proper per-line block dispatch is post-1.0). |
| Operator-Pending   | ✅     | §5.2      | Resolved through translate_normal pending state             |
| Command (`:`)      | ✅     | §5.9.10   | Rich minibuffer scope is partial; full spec is post-Phase-1 |
| Search (`/`, `?`)  | ✅     | §5.9.10   | Live preview decoration not yet wired                       |
| Replace (`R`)      | ✅     | §5.2      | Backspace-restore wired                                     |

### Motions (Reflex-class)

| Motion                            | Key            | Status | Anchor                            |
|-----------------------------------|----------------|--------|-----------------------------------|
| char_left / char_right            | h, l           | ✅     | §5.2.2                            |
| line_up / line_down               | k, j           | ✅     | §5.2.2                            |
| line_start / line_end             | 0, $           | ✅     | §5.2.2                            |
| first_non_blank                   | ^              | ✅     | §5.2.2                            |
| word_forward                      | w              | ✅     | §5.2.2                            |
| word_backward                     | b              | ✅     | §5.2.2                            |
| word_end                          | e              | ✅     | §5.2.2                            |
| WORD_forward / backward / end     | W, B, E        | ✅     | Whitespace-delimited variants     |
| paragraph_forward / backward      | }, {           | ✅     | §5.2.2                            |
| sentence_forward / backward       | ), (           | ✅     |                                   |
| goto_first_line / goto_last_line  | gg, G          | ✅     | §5.2.2                            |
| find_char_forward / backward      | f, F           | ✅     | §5.2.2                            |
| till_char_forward / backward      | t, T           | ✅     | §5.2.2                            |
| find_repeat / find_repeat_reverse | ;, ,           | ✅     |                                   |
| viewport_top / middle / bottom    | H, M, L        | ✅     | App-level (needs viewport_height) |
| word_search_forward / backward    | *, #           | ✅     | §B.3 informally                   |
| match_bracket                     | %              | ✅     | App-level                         |
| jump_history_back / forward       | Ctrl-O, Ctrl-I | ✅     | §5.1.1 (position history)         |
| page_down / page_up               | Ctrl-F, Ctrl-B | ✅     | App-level                         |
| scroll_line_up / down             | Ctrl-Y, Ctrl-E | ✅     | App-level                         |
| half_page_down / up               | Ctrl-D, Ctrl-U | ✅     | Hardcoded count 10                |
| mark jumps                        | 'a, \`a        | ✅     | §5.1.1                            |

### Operators (Reflex-class for sync prelude)

| Operator     | Key      | Status           | Anchor                                     |
|--------------|----------|------------------|--------------------------------------------|
| delete       | d, dd, D | ✅               | §5.2.2                                     |
| change       | c, cc, C | ✅               | §5.2.2                                     |
| yank         | y, yy, Y | ✅               | §5.2.2                                     |
| indent_left  | <        | ✅               | §5.2.2                                     |
| indent_right | >        | ✅               | §5.2.2                                     |
| upper        | gU       | ✅               | §5.2.2                                     |
| lower        | gu       | ✅               | §5.2.2                                     |
| toggle_case  | g~       | ✅               | §5.2.2                                     |
| filter       | !        | ⛔               | Subprocess pipe; depends on `:!cmd` (§B.6) |
| format       | gq       | ⛔               | Depends on plugin / formatter              |
| join_lines   | J, gJ    | ✅               | App-level (not a grammar operator)         |

### Text objects

| Text object              | Key       | Status | Anchor          |
|--------------------------|-----------|--------|-----------------|
| inner_word / around_word | iw / aw   | ✅     | §5.2.2          |
| inner_WORD / around_WORD | iW / aW   | ⛔     | Whitespace word |
| inner_quote_dbl / around | i" / a"   | ✅     |                 |
| inner_quote_sgl / around | i' / a'   | ✅     |                 |
| inner_quote_btk / around | i\` / a\` | ✅     |                 |
| inner_paren / around     | i( / a(   | ✅     |                 |
| inner_bracket / around   | i[ / a[   | ✅     |                 |
| inner_brace / around     | i{ / a{   | ✅     |                 |
| inner_angle / around     | i< / a<   | ⛔     |                 |
| inner_tag / around       | it / at   | ✅     | XML/HTML tags   |
| inner_paragraph / around | ip / ap   | ✅     |                 |
| inner_sentence / around  | is / as   | ✅     |                 |

### Counts, registers, ranges, marks, macros, dot-repeat

| Feature                                        | Status                                | Anchor         |
|------------------------------------------------|---------------------------------------|----------------|
| Numeric prefix counts (3w, 5j, 2dw, 2d3w)      | ✅                                    | §5.2.2         |
| Register prefix `"<reg>`                       | ✅                                    | §5.2.2         |
| Named registers `"a-z`, `"A-Z`                 | ✅ (uppercase replaces, append TBD)   | §5.2.2         |
| Numbered registers `"0-"9`                     | ✅ (storage; `"0` auto-populate TBD)  | §5.2.2         |
| Black hole register `"_`                       | ✅                                    |                |
| System / clipboard `"+`, `"*`                  | ⚠️ storage only; no clipboard wire-up  | §15 (deferred) |
| Last-yank `"0`                                 | ⚠️ readable; auto-populate TBD         |                |
| Ex-command ranges (1,5 / % / 'a,'b / patterns) | 🟡 % and CurrentLine work             | §5.2.2         |
| Marks (m / ' / \`)                             | ✅                                    | §5.1.1         |
| Mark for last visual (`'<`, `'>`)              | ⛔                                    |                |
| gv (reselect last visual)                      | ✅                                    |                |
| Macros (q, @, @@)                              | ✅                                    | §5.2.4         |
| Dot-repeat (.)                                 | ✅                                    | §5.2.4         |
| Insert-mode replay in dot-repeat               | ✅                                    | §5.2.4         |

### Search and substitution

| Feature                                  | Status | Anchor                                |
|------------------------------------------|--------|---------------------------------------|
| `/` `?` `n` `N` literal search with wrap | ✅     | §5.9.10                               |
| `*` / `#` word-search                    | ✅     |                                       |
| Search highlight in buffer               | ✅     | §5.6.2                                |
| `:s/foo/bar/[g]` substitute              | ✅ (literal) | §5.2.1 worked example uses substitute. Regex deferred. |
| Regex search                             | ⛔     | Currently literal substring; §15      |
| Search-as-you-type live preview (hlsearch) | ✅   | every match highlighted; persists after submit |

### Ex commands

| Command                 | Status | Anchor |
|-------------------------|--------|--------|
| :w / :write [path]      | ✅     |        |
| :q / :q!                | ✅     |        |
| :wq                     | ✅     |        |
| :s/.../.../[g]          | ✅ (literal) |        |
| :g/pattern/cmd          | ✅     | §B.2   |
| :v/pattern/cmd          | ✅     | §B.2   |
| :d / :delete            | ✅     |        |
| :noh / :nohlsearch      | ✅     |        |
| :reg / :registers       | ✅     |        |
| :marks                  | ✅     |        |
| :set option=value       | ⛔     | §5.12  |
| :describe-command, etc. | ⛔     | §5.11  |
| :history-*              | ⛔     | §B.3   |
| :customize              | ⛔     | §5.12  |
| :autocmd / :add-hook    | ⛔     | §5.10  |

---

## Async / actor architecture (DESIGN.md §5.2.1, §5.6.8, §5.7)

Currently the dispatcher is synchronous: `execute(registry, document, cursor,
inv) -> Effect`. The async-Pending API spec'd in §5.2.1 has not yet replaced
it. The render-snapshot coherence model (§5.6.8) is also unimplemented.

| Concern                                                    | Status               | Anchor                     |
|------------------------------------------------------------|----------------------|----------------------------|
| Sync dispatcher                                            | ✅                   | DESIGN.md §5.2.1 (current) |
| Async `execute -> Pending`                                 | ⛔                   | §5.2.1 (spec)              |
| Document actor / mailbox                                   | ⛔                   | §5.7                       |
| `arc-swap` published `DocumentSnapshot`                    | ⛔                   | §5.6.8                     |
| Veto-class hooks (1ms p99)                                 | ⛔                   | §5.2.1                     |
| Cancellation token contract                                | ⛔                   | §5.2.5                     |
| Latency-class declarations (Reflex / Display / Background) | ⛔                   | §5.2.5                     |
| Events-over-invocation rule                                | ⛔                   | §5.2.5                     |
| Multi-pane selection transformation                        | n/a (single-pane v1) | §5.6.8                     |

These land in Phase 4–7 alongside LSP and the plugin host. The v1 TUI runs
synchronously in `runtime.rs`; the spec is followed in §5.2.1 / §5.6.8 /
§5.2.5 but the implementation hasn't caught up.

---

## Performance posture

| Concern                           | Status | Anchor |
|-----------------------------------|--------|--------|
| Criterion bench harness           | ✅     | §8.2   |
| Per-class budget assertions in CI | ⛔     | §5.2.5 |
| Allocation discipline check in CI | ⛔     | §A.6   |
| Long-running session benches      | ⛔     | §A.6   |
| Cross-platform acceptance suite   | ⛔     | §A.6   |

Current bench coverage: motions (word_forward / backward / end /
first_non_blank / counted), operators (dw / dd / d_whole / yw / cw / diw /
di_paren), search (forward first / last / no-match-with-wrap / backward),
buffer (insert at origin / middle, delete one byte, position round-trip).

---

## In-progress

(none — pick the next item from "Up next" below.)

Update this section when picking up the in-flight item.

---

## Up next (priority order)

1. **Blockwise visual operators** (delete-block, yank-block, change-block) —
   currently the rectangle is highlighted but operators still cover a single
   contiguous range. Proper per-line dispatch needs the multi-range plumbing
   in `Range::Selection` resolution.
2. **Computed folds** (syntax-driven, indent-based) — manual folds via
   zf/zo/zc/za/zR/zM/zd are done; computed folds need tree-sitter integration
   and an indent-based fall-back.
3. **`:set option=value` + typed options** — full §5.12 system; v1 has no
   options at all today.
4. **`:describe-command` / `:describe-key` / `:apropos`** — introspection
   needs a key→action registry first (§5.11).
5. **Substitute live preview** — decorations on the target buffer while the
   user types `:s/foo/bar/...`. The hlsearch now lights up matches when the
   search minibuffer is open; substitute should do the same.
3. **Tag text object** (`it`, `at`) — XML/HTML tags.
7. **`:set option=value`** + the typed-options system. §5.12.
8. **`:describe-command` / `:describe-key` / `:apropos`** — introspection
   (§5.11).
9. **Async dispatcher** — replace `execute(...) -> Effect` with `execute
   -> Pending<Effect>` per §5.2.1.

---

## §15 open questions still load-bearing

These are tracked in DESIGN.md §15. Items the implementation has resolved
are crossed out there. Items that influence active tasks:

- §15:18 Folds storage / interaction — feeds task #3 above.
- §15:19 Replace mode dispatch (resolved by current Replace impl).
- §15:20 Live evaluation (deferred per §10).
- §15:21 File watcher / auto-revert — unaddressed.
- §15:22 Bookmarks / cross-file marks — current marks are buffer-local.
- §15:23 Function rebinding / advice — unaddressed.
- §15:24 Narrow-to-region — unaddressed.
- §15:25 Snippets / abbrev — unaddressed.
- §15:26 Frames (multi-OS-window) — unaddressed.
- §15:27 Session save / restore — unaddressed.

---

## Test counts (snapshot)

673 tests across the workspace as of the last commit. Coverage by crate:

| Crate                            | Tests |
|----------------------------------|-------|
| lattice-protocol                 | 30    |
| lattice-core (incl. integration) | 75    |
| lattice-grammar                  | 125   |
| lattice-syntax                   | 23    |
| lattice-ui-tui                   | 420   |

Plus criterion benches for hot paths (search, buffer, motions, operators).

---

## Conventions for updating this doc

- Update the **Phase status** table whenever a phase advances.
- Update the **Vim grammar coverage** table when a primitive lands; the
  status column uses ✅ done, 🔄 in progress, 🟡 partial, ⛔ pending,
  ⚠️ usable-with-caveats.
- Update **In-progress** before each commit that lands the in-flight item.
- Move completed items from **Up next** into the appropriate coverage table.
- Update **Test counts** at the end of each session.
- Don't write per-session log entries here — `git log --oneline` is the log.
