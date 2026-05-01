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
| 1     | Modal Editing                         | ✅ done                                                           | Modal engine, full chord routing, motions / operators / text objects / counts / registers / marks / macros / dot-repeat (incl. insert-replay) / search (incl. hlsearch) / folds / ex-commands (every command -- including `:s` / `:g` / `:v` via `Args::List` -- registered as `ExCommandSpec` peers, dispatched through unified `grammar::execute()` per §5.2.1, §B.2). Blockwise visual: per-row dispatch for `d` / `y` / `c` plus blockwise paste; `I` / `A` / `>` / `<` block forms still fall back to charwise. |
| 2     | Terminal UI Bootstrap                 | ✅ done                                                           | crossterm + ratatui; modal cursor; mode line; gutter                                          |
| 3     | Tree-Sitter                           | ✅ done (Rust/Python/JS)                                          | Highlights wired; grammar extension API used by builtins, not yet by plugins                  |
| 4     | LSP                                   | ⛔ not started                                                    | Phase 4 still waiting; depends on async-actor work and the Pending->Effect plumbing in §5.2.1 |
| 5     | GPU Rendering Foundation              | ⛔ not started                                                    | TUI is the live renderer for v1; GPU is a separate paint surface                              |
| 6     | Document Renderer + UI Components     | ⛔ not started                                                    | Popups, pickers, panels-as-buffers all live in §5.9                                           |
| 7     | Plugin Host                           | ⛔ not started                                                    | wasmtime + Component Model + WIT scaffolding                                                  |
| 8     | Major/Minor Modes + Reference Plugins | ⛔ not started                                                    | Major / minor modes are themselves plugins (§5.8.3)                                           |
| 9     | Rich Buffer Rendering                 | ⛔ not started                                                    | Per-line shaped path, Fenwick height index                                                    |
| 10    | Polish + v1.0                         | ⛔ not started                                                    | `*scratch:rust*` live-eval workflow (§10), accessibility, packaging, themes                   |

Active focus: **Phase 4 (LSP) is the next major chunk. It depends on the
§5.2.1 async-dispatcher refactor (`execute → Pending<Effect>`) and the
§5.6.8 render-snapshot model, neither of which is implemented yet --
the TUI runs a synchronous dispatcher today. Those two together are
the gating prerequisite for Phases 4-7.**

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
| Visual (Blockwise) | ✅ d/y/c + paste | §15:18 | Ctrl-V (or Ctrl-Q on terminals that hijack Ctrl-V) enters; render highlights the rectangle; `d` / `y` / `c` dispatch per-row in the dispatcher with merged Edits + one Blockwise yank. `YankKind::Blockwise` paste replays each row at the same column. `I` / `A` / `>` / `<` block forms still post-1.0. |
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
| jump_history_back / forward       | Ctrl-O, Ctrl-I | ✅     | §5.1.1 unified ring (filtered to AutoJump+PluginPush) |
| mark_history_back / forward       | g;, g,         | ✅     | §5.1.1 unified ring (filtered to NamedMark) |
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

Unification status (DESIGN.md §5.2.1, §B.2): every ex-command is now a
registered `ExCommandSpec` peer of motions / operators / text objects.
The `:` parser front-end resolves aliases, looks up by name, calls the
spec's `parse_args` (or builds an `Args::List` directly for the
delimiter-syntax forms `:s/.../`, `:%s/.../`, `:g/.../`, `:v/.../`),
and dispatches through the unified `grammar::execute()`. Apply closures
emit `Effect` variants (`SaveBuffer`, `QuitEditor`, `OpenBuffer`,
`SetOption`, `Substitute`, `Global`, `Echo`, ...); `App::apply_effect`
owns the side effects.

| Command                 | Status | Anchor |
|-------------------------|--------|--------|
| :w / :write [path]      | ✅ registry | §5.2.1 |
| :q / :q!                | ✅ registry | §5.2.1 |
| :wq / :x / :wq! / :x!   | ✅ registry | §5.2.1 (Effect::Many) |
| :e / :edit [path] / :e! | ✅ registry | §5.2.1 |
| :d / :delete            | ✅ registry | §5.2.1 |
| :noh / :nohl / :nohlsearch | ✅ registry | §5.2.1 |
| :reg / :registers       | ✅ registry | §5.2.1 |
| :marks                  | ✅ registry | §5.2.1 |
| :set option=value       | ✅ registry; ⚠️ option set | §5.12. v1 only honors number/relativenumber toggles; full typed-options post-1.0. |
| :s/.../.../[g]          | ✅ registry (literal substring) | §5.2.1 / §B.2; `Args::List([pattern, replacement, flags])`, scope via Range::CurrentLine/Whole. Regex deferred. |
| :g/pattern/cmd          | ✅ registry | §B.2; `Args::List([pattern, false, body])`. Body re-parsed per match. |
| :v/pattern/cmd          | ✅ registry | §B.2; `Args::List([pattern, true, body])` -- same command as `:g`, inverted flag set. |
| :describe-command       | ✅ buffer (popup) | §5.11; renders `CommandSpec.doc` + each `args_schema` entry's name/kind/doc/default. |
| :describe-buffer        | ✅ buffer (popup) | §5.11; path / language / modal / cursor / dirty / line-count / registers / marks / position-history / macros / folds / view options. |
| :describe-key <chord>   | ✅ buffer (popup) | §5.11; renders every `KeymapEntry` for the chord, grouped by mode. Cross-references the bound command via `[[command:...]]` link markup. |
| :keymap                 | ✅ buffer (popup) | §5.11; lists all default bindings grouped by mode, every chord linked via `[[key:...]]` for follow-up `:describe-key`. |
| :apropos <pattern>      | ✅ buffer (popup) | §5.11; case-insensitive substring over every `CommandSpec.name` + `doc`. Picker UI (§5.9.7) is post-1.0. |
| :describe-option, :describe-event, :describe-mode | ⛔ | §5.11; each lands when its registry does (typed options §5.12 / event bus §5.10 / modes Phase 8). |
| Command-line history (Up/Down)  | ✅     | §B.3 |
| :history-*              | ⛔     | §B.3 (picker UI; Up/Down already works) |
| :customize              | ⛔     | §5.12  |
| :autocmd / :add-hook    | ⛔     | §5.10  |

---

## Introspection architecture (DESIGN.md §5.11)

Help is **buffer-backed from day one**, modeled after emacs's `*Help*`.
A `HelpBuffer` (in `lattice-ui-tui::help`) holds a real
`lattice_core::Buffer` (rope) plus the title, scroll offset, and an
extracted `Vec<HelpLink>`. The current display strategy is the centred
popup overlay; **`HelpDisplayMode` enumerates Popup / Split / Tab /
Window** so when multi-buffer support arrives (Phase 6 / §5.9) the
display target swaps without touching the help-content layer. The
target is configurable per-user (eventually via `:set
help.display-mode=...`).

### Provenance (§5.11.1)

Every registration / binding / set captures a `SourceLocation`
(`lattice-grammar::source`). `:describe-*` output always includes a
`[[file:...]]` link to where the thing came from -- vim's
`:verbose set` semantics, applied uniformly across commands, keys,
options, events, modes.

| Capture mechanism | Used for | Forgery resistance |
|---|---|---|
| `#[track_caller]` on `register_*` | built-in command registrations in `builtins.rs` / `ex_commands.rs` -- zero call-site burden | compiler-captured, caller cannot supply or override |
| `keymap_entry!` declarative macro | static keymap rows (per-row `file!()` + `line!()`) | macro is the only construction path; `source` field is `pub(crate)` |
| Trusted subsystem builds value | config loader, plugin host bridge, runtime dispatcher | reaches `pub(crate) insert_*` registry methods directly; sibling crates use sealed-trait re-exports when needed |
| `SourceLocation::synthetic` (cfg-test only) | test fixtures | invisible outside tests |

**Public API invariant**: there is no `pub fn` that takes a
`SourceLocation` parameter and stores it. Verified by:
- The `register_*` methods are all `#[track_caller]` and don't
  accept a source.
- The `pub(crate) insert_*` companions are visibility-gated to
  `lattice-grammar`.
- The `keymap_entry!` macro is the only path that constructs a
  `KeymapEntry`; its `source` field is `pub(crate)`.

**Determinism**: a unit test (`track_caller_captures_register_motion_call_site`) registers a sentinel at a known line and asserts the captured `SourceLocation` matches exactly. Six related tests cover propagation through `#[track_caller]` helpers, the negative case where unmarked helpers shift the captured line inward, and per-row line distinguishing across adjacent registrations. Any refactor that breaks call-site capture (a `dyn Fn` dispatcher around `register_*`, removed `#[track_caller]` on a helper in the chain) fails CI.

### Generic renderer (§5.11.2)

Every `:describe-*` target implements `Introspectable`
(`lattice-grammar::introspect`). One generic
`render_introspection(&dyn Introspectable) -> Vec<String>` produces
the help body in a uniform shape: identifier + kind + doc +
type-specific `extra_sections` + one `[[file:...]]` link per labelled
source (`Defined at:`, `Bound at:`, `Subscribed at:`, `Last set at:`,
`Overridden at:`, `Activated at:`). Adding a new introspection
target -- when typed options / events / modes land -- is one trait
impl plus one new ex-command; the renderer doesn't change.

Link markup -- forward-compatible reference syntax in help bodies:

| Markup                  | Resolution                              |
|-------------------------|-----------------------------------------|
| `[[command:NAME]]`      | re-dispatch `:describe-command NAME`    |
| `[[key:CHORD]]`         | re-dispatch `:describe-key CHORD`       |
| `[[file:PATH:LINE]]`    | open PATH at LINE                       |
| `[[anything-else]]`     | Unresolved (preserved verbatim)         |

The popup renderer is dumb today: links render verbatim. The
follow-link motion + styled link ranges + `[[file:...]]` source
navigation arrive incrementally:

| Capability                                | Status | Notes |
|-------------------------------------------|--------|-------|
| Buffer-backed help (rope content)         | ✅     | `HelpBuffer.content: Buffer` |
| Link markup defined + parsed              | ✅     | `parse_help_links` returns `Vec<HelpLink>` with byte ranges |
| Help formatters emit links                | ✅     | `:describe-key`, `:apropos`, `:keymap` reference cross-targets |
| Display: Popup overlay                    | ✅     | v1 default |
| Display: Split / Tab / Window             | ⛔     | post-multi-buffer (Phase 6) |
| Styled link ranges in renderer            | ⛔     | renderer ignores `links` today |
| Follow-link motion (e.g. `<CR>` on link)  | ⛔     | needs tree-sitter help grammar + link motion |
| Help major mode + tree-sitter grammar     | ⛔     | post-Phase-3-extension; sections / code-blocks / link-targets |
| `SourceLocation` on `CommandSpec`         | ⛔     | needs `register_*` API extension; powers `[[file:...]]` auto-emit |
| `:source-of <command>`                    | ⛔     | depends on `SourceLocation` |
| `:describe-key`                           | ✅     | keymap registry §5.2.3 -- see below |
| `:keymap`                                 | ✅     | full default keymap, grouped by mode |
| `:describe-option`                        | ⛔     | needs typed options registry §5.12 |
| `:describe-event`                         | ⛔     | needs event bus §5.10 |
| `:describe-mode`                          | ⛔     | needs major/minor modes (Phase 8) |

### Keymap registry (DESIGN.md §5.2.3)

`KeymapEntry { chord, mode, doc, command }` in `lattice-ui-tui::keymap`,
populated as a `&'static [KeymapEntry] DEFAULT_KEYMAP` covering every
chord in `input.rs`. v1 is a *descriptor table* the introspection layer
queries; the input layer (`input.rs::translate`) still owns the
chord-to-Action translation. A drift test
(`keymap_descriptors_dont_drift_from_translate`) walks every descriptor
through `translate()` and asserts a non-`None` Action -- catches
removed/moved bindings before they ship.

Registry-driven dispatch (the input layer *consuming* the keymap
registry rather than running a parallel `match`) is post-1.0 -- it
needs the layered keymap walker (built-in / major-mode / minor-modes /
user / per-buffer) from §5.2.3. The descriptor table is the migration
seed.

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

1. **`SourceLocation` on `CommandSpec`** + `:source-of <command>` —
   `register_*` capture `concat!(file!(), ":", line!())` at registration
   sites; `:describe-command` emits `[[file:...]]` automatically;
   `:source-of` (or link-following on a file link) opens the file. Small
   change with high payoff for the introspection surface.
2. **Block-visual `I` / `A` / `>` / `<`** — extend the per-row block
   dispatch with the remaining vim affordances: insert-at-block-start,
   append-at-block-end, indent-block-right/left.
3. **Computed folds** (syntax-driven, indent-based) — manual folds via
   zf/zo/zc/za/zR/zM/zd are done; computed folds need tree-sitter integration
   and an indent-based fall-back.
4. **`:set option=value` + typed options** (§5.12) — also unblocks
   `:describe-option`.
5. **Multi-buffer foundations** (§5.9) — the trigger for `HelpDisplayMode`
   beyond `Popup`. Until this lands, all introspection is overlay-rendered.
6. **Help major mode + tree-sitter grammar** — defines sections,
   link-targets, code-blocks. Needs the help mode registered as a major
   mode, which depends on the modes registry (Phase 8) but the *grammar*
   can be drafted earlier.
7. **Substitute live preview** — decorations on the target buffer while
   the user types `:s/foo/bar/...`. The hlsearch now lights up matches
   when the search minibuffer is open; substitute should do the same.
8. **Tag text object** (`it`, `at`) — XML/HTML tags.
9. **Promote `:g` body to a parsed CommandInvocation** — currently the
   body is `ArgValue::Raw(String)` and re-parsed per match.
10. **Interactive arg-prompts via `args_schema`** (§B.1 phase 2) — when a
    command has a missing required arg, drop the user into the minibuffer
    with the schema's prompt text + completion source.
11. **Async dispatcher** — replace `execute(...) -> Effect` with `execute
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

823 tests across the workspace as of the last commit. Coverage by crate:

| Crate                            | Tests |
|----------------------------------|-------|
| lattice-protocol                 | 30    |
| lattice-core (incl. integration) | 78    |
| lattice-grammar                  | 171   |
| lattice-syntax                   | 23    |
| lattice-ui-tui                   | 516   |

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
