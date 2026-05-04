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

| Phase | Title                                 | Status                   | Notes                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
|-------|---------------------------------------|--------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| 0     | Foundation                            | ✅ done                  | Workspace, lattice-core, document/buffer/undo, file I/O, protocol enums                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| 1     | Modal Editing                         | ✅ done                  | Modal engine, full chord routing, motions / operators / text objects / counts / registers / marks / macros / dot-repeat (incl. insert-replay) / search (incl. hlsearch + substitute live preview) / folds / ex-commands (every command -- including `:s` / `:g` / `:v` via `Args::List` -- registered as `ExCommandSpec` peers, dispatched through unified `grammar::execute()` per §5.2.1, §B.2). Blockwise visual: per-row dispatch for `d` / `y` / `c` plus blockwise paste; `>` / `<` indent each line in the block; `I` / `A` enter Insert at the block's left/right column with the typed prefix replicated to every row on Esc. Every operator lands as a single undo unit -- counts on linewise ops (`2dd`, `2>>`), block-visual rectangle ops, and I/A replications all collapse to one `u`. |
| 2     | Terminal UI Bootstrap                 | ✅ done                  | crossterm + ratatui; modal cursor; mode line; gutter                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| 3     | Tree-Sitter                           | ✅ done (Rust/Python/JS/Markdown) | Highlights wired through a shared `LangRegistry` (process-wide `Arc`); injection callback resolves fenced ` ```rust ``` ` blocks in markdown to the rust config (and any registered language to its config) without per-document copies. Markdown is the dual-grammar split (block + inline). Grammar extension API used by builtins, not yet by plugins. New `Style` variants (`Heading1..6`, `Bold`, `Italic`, `Link`, `Url`, `MarkupRaw`, `Markup`) for precise theme targeting. |
| 4     | LSP                                   | 🚧 in progress (4.2)     | `lattice-lsp` crate: wire layer + per-server actor + document sync + diagnostics broadcast + supervisor + App-side wiring + edit-dispatch + open-on-`:e` (Phase 4.1 complete). Phase 4.2 in progress: typed feature wrappers in `actor.rs` + `features.rs` (4.2.a) + hover end-to-end via `K` (4.2.b) shipped. Cancellation tokens plumbed through every wrapper so motion / mode-change can drop stale responses. Multi-server first-non-empty merge today; concat-with-name-separator polish queued. Remaining 4.2: definition (4.2.c), references (4.2.d), document symbols (4.2.e), workspace symbols (4.2.f), LSP completion (4.2.g). Then 4.3 edits, 4.4 polish, 4.5 expansion. |
| 5     | GPU Rendering Foundation              | ⛔ not started           | TUI is the live renderer for v1; GPU is a separate paint surface                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| 6     | Document Renderer + UI Components     | ⛔ not started           | Popups, pickers, panels-as-buffers all live in §5.9                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 7     | Plugin Host                           | ⛔ not started           | wasmtime + Component Model + WIT scaffolding                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| 8     | Major/Minor Modes + Reference Plugins | ⛔ not started           | Major / minor modes are themselves plugins (§5.8.3)                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| 8b    | Bundled plugins                       | ⛔ not started           | Curated set of first-party WASM Component plugins shipping with the editor binary -- LSP server manager (lighthouse), plugin manager, fuzzy-finder, project grep, git client, snippet engine, editing helpers, diff viewer, outline sidebar, format-on-save, test runner, markdown preview. Each crate lives at `crates/lattice-plugin-<name>/`. See DESIGN.md §5.5.6 for the strategy + the seven WIT prerequisites Phase 7 must expose. Depends on Phase 7. |
| 9     | Rich Buffer Rendering                 | ⛔ not started           | Per-line shaped path, Fenwick height index                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| 10    | Polish + v1.0                         | ⛔ not started           | `*scratch:rust*` live-eval workflow (§10), accessibility, packaging, themes                                                                                                                                                                                                                                                                                                                                                                                                                                          |

Active focus: **Phase 4 (LSP) -- foundation 4 commits in.** The
§5.2.1 async-dispatcher refactor (`Pending<T>`) and the §5.6.8
render-snapshot model live in `lattice-runtime`; the actor +
arc-swap publish/load contract is in place. `lattice-lsp` ships
the wire layer, per-server actor with capability handshake,
document sync (utf-8/utf-16/utf-32 column conversion), and a
diagnostics broadcast bus -- everything needed for the editor
to attach servers and receive diagnostics.

LSP docs are now comprehensive across audiences: design-doc
readers (`DESIGN.md` §5.4), implementers / contributors
([`lsp-architecture.md`](lsp-architecture.md)), users
([`help/lsp.md`](help/lsp.md)), and per-feature trackers
([`lsp-features.md`](lsp-features.md) -- every LSP 3.17
capability with status).

Roadmap: 4.1.a wire (done) → 4.1.b actor + handshake (done) →
4.1.c document sync (done) → 4.1.d.i diagnostics routing (done)
→ 4.1.d.ii decoration layer → 4.1.d.iii renderer integration
(gutter glyphs + underlines) → 4.1.d.iv `:diagnostics` buffer
view → 4.1.e final doc polish → 4.2 navigation → 4.3 edits →
4.4 polish (semantic tokens, inlay hints, folding, document
highlight) → 4.5 expansion (call/type hierarchy, code lens,
inline completion).

---

## Vim grammar coverage (Phase 1 catalog)

This section enumerates every named primitive in vim's grammar against its
status here. Anchor: DESIGN.md §5.2 + the seven unifications in §5.10–§5.12.

### Modal states

| State              | Status           | Anchor    | Notes                                                                                                                                                                                                                                                                                                     |
|--------------------|------------------|-----------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| Normal             | ✅               | §5.2      | Plus block cursor in TUI                                                                                                                                                                                                                                                                                  |
| Insert             | ✅               | §5.2      | Plus bar cursor                                                                                                                                                                                                                                                                                           |
| Visual (Charwise)  | ✅               | §5.2, B.1 | Selection extends, operators on Range::Selection                                                                                                                                                                                                                                                          |
| Visual (Linewise)  | ✅               | §5.2      |                                                                                                                                                                                                                                                                                                           |
| Visual (Blockwise) | ✅ d/y/c/I/A/>/< + paste | §15:18    | Ctrl-V (or Ctrl-Q on terminals that hijack Ctrl-V) enters; render highlights the rectangle; `d` / `y` / `c` dispatch per-row in the dispatcher with merged Edits + one Blockwise yank. `>` / `<` indent each covered line. `I` / `A` enter Insert at the block's left / right column on the top row; on Esc the typed prefix is replicated to every other row at the same column (rows shorter than the column are skipped, vim's behavior). `YankKind::Blockwise` paste replays each row at the same column. |
| Operator-Pending   | ✅               | §5.2      | Resolved through translate_normal pending state                                                                                                                                                                                                                                                           |
| Command (`:`)      | ✅               | §5.9.10   | Rich minibuffer scope is partial; full spec is post-Phase-1                                                                                                                                                                                                                                               |
| Search (`/`, `?`)  | ✅               | §5.9.10   | Live preview wired (hlsearch on every keystroke); fancy-regex backend                                                                                                                                                                                                                                     |
| Replace (`R`)      | ✅               | §5.2      | Backspace-restore wired                                                                                                                                                                                                                                                                                   |

### Motions (Reflex-class)

| Motion                            | Key            | Status | Anchor                                                |
|-----------------------------------|----------------|--------|-------------------------------------------------------|
| char_left / char_right            | h, l           | ✅     | §5.2.2                                                |
| line_up / line_down               | k, j           | ✅     | §5.2.2                                                |
| line_start / line_end             | 0, $           | ✅     | §5.2.2                                                |
| first_non_blank                   | ^              | ✅     | §5.2.2                                                |
| word_forward                      | w              | ✅     | §5.2.2                                                |
| word_backward                     | b              | ✅     | §5.2.2                                                |
| word_end                          | e              | ✅     | §5.2.2                                                |
| WORD_forward / backward / end     | W, B, E        | ✅     | Whitespace-delimited variants                         |
| paragraph_forward / backward      | }, {           | ✅     | §5.2.2                                                |
| sentence_forward / backward       | ), (           | ✅     |                                                       |
| goto_first_line / goto_last_line  | gg, G          | ✅     | §5.2.2                                                |
| find_char_forward / backward      | f, F           | ✅     | §5.2.2                                                |
| till_char_forward / backward      | t, T           | ✅     | §5.2.2                                                |
| find_repeat / find_repeat_reverse | ;, ,           | ✅     |                                                       |
| viewport_top / middle / bottom    | H, M, L        | ✅     | App-level (needs viewport_height)                     |
| word_search_forward / backward    | *, #           | ✅     | §B.3 informally                                       |
| match_bracket                     | %              | ✅     | App-level                                             |
| jump_history_back / forward       | Ctrl-O, Ctrl-I | ✅     | §5.1.1 unified ring (filtered to AutoJump+PluginPush) |
| mark_history_back / forward       | g;, g,         | ✅     | §5.1.1 unified ring (filtered to NamedMark)           |
| page_down / page_up               | Ctrl-F, Ctrl-B | ✅     | App-level                                             |
| scroll_line_up / down             | Ctrl-Y, Ctrl-E | ✅     | App-level                                             |
| half_page_down / up               | Ctrl-D, Ctrl-U | ✅     | Hardcoded count 10                                    |
| mark jumps                        | 'a, \`a        | ✅     | §5.1.1                                                |

### Operators (Reflex-class for sync prelude)

| Operator     | Key      | Status | Anchor                                     |
|--------------|----------|--------|--------------------------------------------|
| delete       | d, dd, D | ✅     | §5.2.2                                     |
| change       | c, cc, C | ✅     | §5.2.2                                     |
| yank         | y, yy, Y | ✅     | §5.2.2                                     |
| indent_left  | <        | ✅     | §5.2.2                                     |
| indent_right | >        | ✅     | §5.2.2                                     |
| upper        | gU       | ✅     | §5.2.2                                     |
| lower        | gu       | ✅     | §5.2.2                                     |
| toggle_case  | g~       | ✅     | §5.2.2                                     |
| filter       | !        | ⛔     | Subprocess pipe; depends on `:!cmd` (§B.6) |
| format       | gq       | ⛔     | Depends on plugin / formatter              |
| join_lines   | J, gJ    | ✅     | App-level (not a grammar operator)         |

### Text objects

| Text object              | Key       | Status | Anchor                                                                                                                                                                                                  |
|--------------------------|-----------|--------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| inner_word / around_word | iw / aw   | ✅     | §5.2.2; alphanum + `_` run                                                                                                                                                                              |
| inner_WORD / around_WORD | iW / aW   | ✅     | Whitespace-delimited; punctuation kept (`foo.bar` is one WORD). Shares `text_object_inner_word_class` / `_around_word_class` with `iw` / `aw` -- only the byte classifier differs (`is_big_word_byte`). |
| inner_quote_dbl / around | i" / a"   | ✅     |                                                                                                                                                                                                         |
| inner_quote_sgl / around | i' / a'   | ✅     |                                                                                                                                                                                                         |
| inner_quote_btk / around | i\` / a\` | ✅     |                                                                                                                                                                                                         |
| inner_paren / around     | i( / a(   | ✅     |                                                                                                                                                                                                         |
| inner_bracket / around   | i[ / a[   | ✅     |                                                                                                                                                                                                         |
| inner_brace / around     | i{ / a{   | ✅     |                                                                                                                                                                                                         |
| inner_angle / around     | i< / a<   | ✅     | `<…>` pair; reuses `text_object_inner_brackets` / `_around_brackets`. Both `<` and `>` resolve to the same target.                                                                                      |
| inner_tag / around       | it / at   | ✅     | XML/HTML tags                                                                                                                                                                                           |
| inner_paragraph / around | ip / ap   | ✅     |                                                                                                                                                                                                         |
| inner_sentence / around  | is / as   | ✅     |                                                                                                                                                                                                         |

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

| Feature                                    | Status                | Anchor                                                                                                  |
|--------------------------------------------|-----------------------|---------------------------------------------------------------------------------------------------------|
| `/` `?` `n` `N` regex search with wrap     | ✅                    | §5.9.10; backed by `fancy-regex` (DFA + bounded NFA fallback for backrefs)                              |
| `*` / `#` word-search                      | ✅                    | Word is regex-escaped before compile so literals containing `.` `*` `(` etc. don't trigger metachars   |
| Search highlight in buffer                 | ✅                    | §5.6.2                                                                                                  |
| `:s/PAT/REPL/[g]` substitute               | ✅                    | Full regex; replacement template uses `$1`/`${name}`/`$0` (regex crate / fancy-regex syntax, not vim's `\1`) |
| Pattern backrefs (`/(\w+).*\1/`)           | ✅                    | fancy-regex NFA path; bounded by 1M-iteration recursion limit                                            |
| Replacement backrefs (`:s/(a)(b)/$2$1/`)   | ✅                    | `$1` etc. via fancy-regex's `replace_all` template                                                       |
| Search-as-you-type live preview (hlsearch) | ✅                    | every match highlighted; persists after submit; compile errors silenced during preview                  |
| Substitute-as-you-type live preview        | ✅                    | DESIGN.md §5.9.10; magenta strike-through overlay on matches as the user types `:s/pat/repl/...`; honours `/g` flag and `%s` scope |
| Search cooperative cancellation            | ✅                    | DESIGN.md §5.2.5; search loops poll a `CancellationToken` per chunk + per match; flipped token returns `CoreError::Cancelled` |
| Per-search deadline timer                  | ⛔                    | the cancellation seam is in place; the deadline-flipper (Reflex < 2 ms) is the remaining piece          |

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

**Kind-prefix form on `:` (§5.2.1 closure)** ✅. Every command --
motion, operator, text-object, ex-command, plugin contribution -- is
reachable from `:` by the `:<kind> <name>` form. Three kind words
(`motion`, `operator`, `text-object`) are reserved on the `:` line
and disambiguate the namespace; ex-commands keep their bare alias
surface unchanged.

```
:motion goto-first-line               # naked motion
:operator delete word-forward         # operator + bare target (motion)
:operator delete inner-word           # operator + bare target (text-object)
:operator delete motion:word-forward  # full canonical form (disambig)
:text-object inner-word               # errors helpfully
:write foo.txt                        # ex-command, vim alias surface
```

Operator targets resolve via implicit-namespace lookup: the bare tail
is tried as `motion:<tail>`, then `text-object:<tail>`, then as a full
canonical name. Plugin-registered names (e.g.
`motion:my-plugin:fancy-jump`) are reachable via
`:motion my-plugin:fancy-jump` -- the second-word colon is part of
the tail, not adjacent to the cmdline colon. See
`crates/lattice-ui-tui/src/excommand.rs::parse_kind_prefixed`.

**`:` surface invariant.** DESIGN.md §2.2 explicitly excludes a
function-call / palette / scripting syntax on `:`. The `:` line is
vim's ex-syntax DSL; code paths (plugins, `init.rs`, the Rust
functional API) construct `CommandInvocation` directly via the WIT
host. Two input surfaces, one dispatcher.

**Surface-form gating.** Each `ExCommandSpec` carries a
`surface_form: SurfaceForm` (`Keyword` or `Delimiter { hint }`).
Delimiter-form commands (`ex:substitute`, `ex:global`) are routed by
the parser's delimiter-detection pass; the keyword form
(`:ex:global`) returns `WrongSurfaceForm { name, hint }`, which
surfaces the intended syntax (`:g/pattern/body  (or :v/pattern/body
for inverted)`). The `gen:commands` completion generator filters
delimiter-form commands so the cmdline popup never offers a
candidate that would error when accepted. They remain reachable via
`:describe-command` / `:apropos` for introspection.

| Command                                           | Status                          | Anchor                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
|---------------------------------------------------|---------------------------------|----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| :w / :write [path]                                | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :q / :q!                                          | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :wq / :x / :wq! / :x!                             | ✅ registry                     | §5.2.1 (Effect::Many)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| :e / :edit [path] / :e!                           | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :d / :delete                                      | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :noh / :nohl / :nohlsearch                        | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :reg / :registers                                 | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :marks                                            | ✅ registry                     | §5.2.1                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :set option=value                                 | ✅                              | §5.12. Typed-options registry in `lattice-config` (renderer-agnostic): `OptionType` trait + `Option<T>` with `ArcSwap<T>` cell + typed `OptionHandle<T>`. Core options (number, relativenumber, wrap, ignorecase, tabstop, foldenable, foldmethod, scrolloff, completion.auto_insert_single) register from `lattice-config::register_core_options`; renderer options register from each renderer crate. App integration: `do_set` ⇒ `config.parse_and_set_command` ⇒ `apply_post_set` for cascades (relativenumber ⇒ number, foldmethod ⇒ recompute, ui.* ⇒ theme refresh).      |
| :s/.../.../[g]                                    | ✅ registry (regex)             | §5.2.1 / §B.2; `Args::List([pattern, replacement, flags])`, scope via Range::CurrentLine/Whole. fancy-regex compile + per-line `replace_all`; replacement template uses `$1`/`${name}`/`$0`.                                                                                                                                                                                                                                                                                                                                                              |
| :g/pattern/cmd                                    | ✅ registry                     | §B.2; `Args::List([pattern, false, ArgValue::Invocation(body)])`. Body parsed once at `:g` parse time (no per-match re-parse); body syntax errors surface before `:g` fires.                                                                                                                                                                                                                                                                                                                                                                                                     |
| :v/pattern/cmd                                    | ✅ registry                     | §B.2; same shape as `:g`, inverted flag set.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     |
| :describe-command                                 | ✅ buffer (popup)               | §5.11; renders `CommandSpec.doc` + each `args_schema` entry's name/kind/doc/default.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| :describe-buffer                                  | ✅ buffer (popup)               | §5.11; path / language / modal / cursor / dirty / line-count / registers / marks / position-history / macros / folds / view options.                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| :describe-key <chord>                             | ✅ buffer (popup)               | §5.11; renders every `KeymapEntry` for the chord through the unified `Introspectable` surface. Each binding shows `Bound at: [[file:keymap.rs:LINE]]` (built-in) -- the row's source captured by the `keymap_entry!` macro -- plus an Action: `[[command:...]]` cross-reference. The `chord` arg uses `ArgKind::Chord`: typing `:describe-key <space>` puts the cmdline into chord-capture mode (raw key events render as canonical tokens -- `Ctrl-c` -> `<C-c>`, `Up` -> `<Up>`); `:describe-key<CR>` with no arg arms a one-shot prompt and the very next chord auto-submits. |
| :keymap                                           | ✅ buffer (popup)               | §5.11; lists all default bindings grouped by mode, every chord linked via `[[key:...]]` for follow-up `:describe-key`.                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| :apropos <pattern>                                | ✅ buffer (popup)               | §5.11; case-insensitive substring over every `CommandSpec.name` + `doc`. Picker UI (§5.9.7) is post-1.0.                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| :describe-option                                  | ✅                              | §5.11 / §5.12. Reads from `lattice-config::ConfigRegistry`'s `ErasedOption` view: name, aliases, type label, current value, default value, enumerated values (where applicable), doc body. Markdown-rendered into a help buffer.                                                                                                                                                                                                                                                                                                                                                |
| :describe-event, :describe-mode                   | ⛔                              | §5.11; each lands when its registry does (event bus §5.10 / modes Phase 8).                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| Command-line history (Up/Down)                    | ✅                              | §B.3                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| :history-*                                        | ⛔                              | §B.3 (picker UI; Up/Down already works)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| :customize                                        | ⛔                              | §5.12                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| :autocmd / :add-hook                              | ⛔                              | §5.10                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |

---

## Introspection architecture (DESIGN.md §5.11)

Help is **buffer-backed from day one**, modeled after emacs's `*Help*`.
A `HelpBuffer` (in `lattice-ui-tui::help`) holds a real
`lattice_core::Buffer` (rope) plus the title, scroll offset, a real
**cursor** (`Position`), and an extracted `Vec<HelpLink>`. The popup
overlay treats the help buffer as a buffer: `j` `k` `h` `l` `0` `$`
`gg` `G` `Ctrl-D` `Ctrl-U` move the cursor and scroll auto-follows.
The terminal cursor is rendered at the screen translation of
`help.cursor` so the user sees their position. **`HelpDisplayMode` enumerates
Popup / Split / Tab / Window** for the per-user display preference;
the popup overlay is one strategy and stays available for transient
surfaces (hover, future doc lookups, error toasts).

Help also lives in the unified [`BufferRegistry`] as
`BufferData::Help(HelpBuffer)`. `App::open_help_in_pane(buffer)`
allocates a `BufferId`, inserts the registry entry, and swaps the
active pane to it -- the in-pane counterpart to `App::open_help`
(popup). The registry holds the durable record (`:ls` / `:bn` /
picker discovery); `App.help_buffer` mirrors the active in-pane
copy so the existing keymap + render paths stay single-path.
Pane-switch hooks (`snapshot_active_pane` / `load_active_pane`)
sync the two at boundaries, mirroring the Document buffer pattern
(`syntax`/`folds` snapshots). De-dup is by title -- re-running
`:lsp-log rust` surfaces the existing buffer rather than allocating
a duplicate. Persistent help views (LSP logs, `:diagnostics`,
`:apropos`, `:describe-*`) migrate to this path in Phase 3
alongside the picker (Phase 2).

### Provenance (§5.11.1)

Every registration / binding / set captures a `SourceLocation`
(`lattice-grammar::source`). `:describe-*` output always includes a
`[[file:...]]` link to where the thing came from -- vim's
`:verbose set` semantics, applied uniformly across commands, keys,
options, events, modes.

| Capture mechanism                           | Used for                                                                                    | Status                                        | Forgery resistance                                                                                        |
|---------------------------------------------|---------------------------------------------------------------------------------------------|-----------------------------------------------|-----------------------------------------------------------------------------------------------------------|
| `#[track_caller]` on `register_*`           | built-in command registrations in `builtins.rs` / `ex_commands.rs` -- zero call-site burden | ✅ implemented                                | compiler-captured, caller cannot supply or override                                                       |
| `keymap_entry!` declarative macro           | static keymap rows (per-row `file!()` + `line!()`)                                          | ✅ implemented                                | macro is the only construction path; `source` field is `pub(crate)`                                       |
| Trusted subsystem builds value              | config loader, plugin host bridge, runtime dispatcher                                       | ⛔ deferred (no trusted subsystems exist yet) | `pub(crate) insert_*` registry methods exist; sibling crates will use sealed-trait re-exports when needed |
| `SourceLocation::synthetic` (cfg-test only) | test fixtures                                                                               | ✅ implemented                                | invisible outside tests                                                                                   |

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

### Completion pipeline (§5.11.3)

`lattice-completion` is a standalone crate with its own test corpus
(81 tests) wired into the `:`-line via the App's
`completion_registry` + `completion_state`. Four pluggable stages,
vertico-shaped:

| Stage      | Trait                | Built-ins                                                             |
|------------|----------------------|-----------------------------------------------------------------------|
| Generation | `CandidateGenerator` | `gen:commands`, `gen:files` (host-state generators in lattice-ui-tui) |
| Matching   | `CandidateMatcher`   | `match:prefix` (default), `match:substring`, `match:fuzzy`            |
| Ranking    | `CandidateRanker`    | `rank:score` (default), `rank:alphabetical`                           |
| Annotation | `CandidateAnnotator` | `anno:kind-label`, `anno:doc-snippet`                                 |

`CompletionRegistry` registers each stage with `#[track_caller]`
provenance. `CompletionPipeline::run(ctx, query, cache)` walks the
four stages and returns a `Vec<RenderedCandidate>` for the renderer.
Slot detection (`current_slot`) parses the `:`-line into
`CommandLineSlot::CommandName`, `Arg { command_name, arg_index,
arg_spec, .. }`, `DelimiterBody { command_name, body }`,
`UnknownCommand`, `BeyondSchema`, or `Empty`.

**Caching** is opt-in per generator via `cache_key()` returning
`Option<CacheKey>`. `gen:commands` returns a fixed `"gen:commands:v1"`
key (commands don't change at runtime in v1, so cached effectively
forever); `gen:files` keys per-directory with a 1-second TTL.

**Composability** is structural: every stage is a trait, plugins
register impls against the same registry as built-ins, the
default matcher / ranker / annotators are configurable per-user
(`cmdline.matcher = "match:fuzzy"` post-§5.12). The pluggable
shape mirrors emacs's `vertico` / `orderless` / `marginalia` /
`consult` -- composability by design, not retrofit.

**Forgery resistance** mirrors the §5.11.1 invariant: no public
API takes a `SourceLocation` parameter. `register_*` are all
`#[track_caller]`; `pub(crate) insert_*` companions exist for
trusted subsystems (config loader, plugin host bridge) but
deferred until first cross-crate trusted subsystem lands.

The crate doesn't depend on `lattice-ui-tui`, so it's tested
independently. CI runs `cargo test -p lattice-completion` as its
own line item.

**App integration:**

| Capability                               | Status      | Notes                                                                                                                                                                                                                     |
|------------------------------------------|-------------|---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|
| `<Tab>` opens completion popup           | ✅          | Slot-aware: command-name slot uses `gen:commands`; arg slot uses `arg_spec.completion`.                                                                                                                                   |
| `<Tab>` advances candidates while open   | ✅          | wraps at end                                                                                                                                                                                                              |
| `<S-Tab>` moves backward                 | ✅          | wraps at start                                                                                                                                                                                                            |
| `<CR>` accepts selected                  | ✅          | replaces `[replace_start, end)` with the candidate's `text`                                                                                                                                                               |
| `<Esc>` two-stage dismiss                | ✅          | first Esc closes popup; second cancels cmdline                                                                                                                                                                            |
| Typing dismisses popup                   | ✅          | re-trigger with Tab for fresh candidate set                                                                                                                                                                               |
| `<C-h>` describe under cursor            | ✅          | hybrid: word-at-cursor describes self if registered; else parent command at `arg:<name>` anchor                                                                                                                           |
| `<C-u>` clear cmdline                    | ✅          | also dismisses popup                                                                                                                                                                                                      |
| `<C-w>` delete trailing word             | ✅          | strips whitespace then last token                                                                                                                                                                                         |
| Vertico-style popup render               | ✅          | bordered, anchored BELOW the `:` prompt (cmdline shifts up to make room), selected row highlighted, matched byte ranges painted, annotations right-aligned                                                                |
| Alias-preferred candidate text           | ✅          | `gen:commands` returns canonical names (`ex:describe-command`); host post-process rewrites to the user-facing alias (`describe-command`) and recomputes match ranges. Parser accepts both forms via two-stage resolution. |
| Single-candidate auto-insert             | ✅          | `:set completion.auto_insert_single` (default on). When `<Tab>` would open a popup with exactly one candidate, applies it directly instead. Fires only at popup-open boundary; narrowing an open popup to one candidate while typing does NOT auto-insert (vim-style; less surprising). Phase 4.2 (#199) should reuse `App::open_completion_popup` (or factor a shared helper) when wiring Insert-mode / LSP completion so this option stays universal without a second knob. |
| Help overlay dismissed when entering `:` | ✅          | Q16: user can only focus on one thing                                                                                                                                                                                     |
| `<C-b>` / `<C-e>` cursor movement        | ⛔ deferred | needs full cmdline-cursor refactor; v1 cursor stays at end                                                                                                                                                                |
| `<C-r>` register paste                   | ⛔ deferred | needs `Pending::AfterCommandLineRegister` substate                                                                                                                                                                        |
| Completion inside `:s/.../.../` body     | ⛔ deferred | DelimiterBody slot returned; renderer doesn't recurse into pattern/replacement/flags                                                                                                                                      |

**ArgSpec.completion**: `Option<&'static str>` field naming a registered
generator. v1 wires `gen:files` for `:write`/`:edit` paths and
`gen:commands` for `:describe-command`'s name arg. Adding `gen:options`
when typed options land is one schema edit per command.

The default matcher (`match:fuzzy`) is set at registry construction;
users / configs can swap to `match:prefix` or `match:substring` once
typed options land via `cmdline.matcher = "match:prefix"`.

Link markup -- forward-compatible reference syntax in help bodies:

| Markup               | Resolution                           |
|----------------------|--------------------------------------|
| `[[command:NAME]]`   | re-dispatch `:describe-command NAME` |
| `[[key:CHORD]]`      | re-dispatch `:describe-key CHORD`    |
| `[[file:PATH:LINE]]` | open PATH at LINE                    |
| `[[anything-else]]`  | Unresolved (preserved verbatim)      |

The popup renderer is dumb today: links render verbatim. The
follow-link motion + styled link ranges + `[[file:...]]` source
navigation arrive incrementally:

| Capability                               | Status | Notes                                                             |
|------------------------------------------|--------|-------------------------------------------------------------------|
| Buffer-backed help (rope content)        | ✅     | `HelpBuffer.content: Buffer`                                      |
| Link markup defined + parsed             | ✅     | `parse_help_links` returns `Vec<HelpLink>` with byte ranges       |
| Help formatters emit links               | ✅     | `:describe-key`, `:apropos`, `:keymap` reference cross-targets    |
| Display: Popup overlay                   | ✅     | transient surface (hover, doc lookups); reachable via `App::open_help` |
| Display: In-pane (registry-tracked)      | ✅     | `BufferData::Help` + `App::open_help_in_pane` + `activate_buffer` Help arm; call-site migration to in-pane (`:lsp-log`, `:describe-*`, `:diagnostics`) follows in Phase 3 |
| Display: Split / Tab / Window            | 🟡     | in-pane lands with active pane today; multi-pane (split into a sibling pane) follows the picker / Phase 3 |
| Vertico-style picker primitive (§5.9.7)  | ✅     | `lattice-ui-tui::picker::Picker` -- query line + substring filter + selection cursor + `PickerAction` accept tag. **Renderer-agnostic data model**: the only imports are stdlib + `lattice_completion`; host-coupled candidate builders live on the host side (the buffer-source builder is `app::raw_buffer_candidates`; the LSP-source builder is `LspInstanceRow::into_candidate` fed by host-snapshotted rows). `Picker::set_raw_candidates` is the single mutation entry point. Drops into a sibling crate `lattice-picker` with no source edits when a second renderer needs it. Sources: `Buffers`, `LspInstances { prefilter }`. First instantiations: `:b` buffer switcher; `:lsp-log` / `:lsp-server-log` / `:lsp-trace-log` LSP picker (Phase 3). Layout: vertico-style (query takes over cmdline row; candidates render in band immediately below, selected row closest to prompt, no border). Live preview-in-pane for `SwitchToBuffer` (selection-change activates the buffer in active pane without pushing position history; `<Esc>` restores `preview_origin`). Pipeline-driven matcher / ranker / annotators graduate the picker in a follow-up. |
| Cmdline tab-completion: vertico-styled   | ✅     | `:` line tab-completion popup converged with the picker's visual shape. No bordered box / title; candidates render flush in the row band below the cmdline, reusing `candidate_to_line`. The cmdline appends a faint `(n/m)` count suffix when the popup is open, mirroring the picker prompt's inline indicator. |
| LSP picker -- log / trace dispatch       | ✅     | `:lsp-log [server]`, `:lsp-trace-log [server]`, `:lsp-server-log` route through `App::open_lsp_picker` (Phase 3). Single-match short-circuit opens directly; multi-match opens the picker pre-filtered. Opened buffer goes through `open_help_in_pane`. `:lsp-trace` is now a pure toggle (no buffer-open side-effect). |
| LSP log buffers live-tail (Phase 4)      | ✅     | `Event::LspLogPushed` fires on every `LspLogger::log` append; `App::drain_lsp_log_events` (called per main-loop tick) refreshes any open `*lsp*` / `*lsp:<server>*` / `*lsp:<server>:trace*` help buffer from a fresh logger snapshot. Coalesces by scope so a burst of N records resolves to one rebuild per affected scope. Cursor + scroll preserved across the rebuild (clamped to new line bounds); popup hot-path mirror synced. |
| Styled link ranges in renderer           | ⛔     | renderer ignores `links` today                                    |
| Follow-link motion (e.g. `<CR>` on link) | ⛔     | needs tree-sitter help grammar + link motion                      |
| Help major mode + tree-sitter grammar    | ⛔     | post-Phase-3-extension; sections / code-blocks / link-targets     |
| `SourceLocation` on `CommandSpec`        | ⛔     | needs `register_*` API extension; powers `[[file:...]]` auto-emit |
| `:source-of <command>`                   | ⛔     | depends on `SourceLocation`                                       |
| `:describe-key`                          | ✅     | keymap registry §5.2.3 -- see below                               |
| `:keymap`                                | ✅     | full default keymap, grouped by mode                              |
| `:describe-option`                       | ⛔     | needs typed options registry §5.12                                |
| `:describe-event`                        | ⛔     | needs event bus §5.10                                             |
| `:describe-mode`                         | ⛔     | needs major/minor modes (Phase 8)                                 |

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

The async core lands in `lattice-runtime`. Every document is owned by
its own tokio task (the **document actor**); mutations route through a
bounded mpsc mailbox; commits publish an immutable `DocumentSnapshot`
to an `arc_swap::ArcSwap` cell that any reader (renderer, future LSP
client, future plugin) loads wait-free.

`lattice_grammar::execute` stays a pure sync function (no tokio
dependency, no async signature). The actor calls it *inside* its own
task via the `Dispatch` message; only the *boundary* between caller
and document is async.

App talks to the actor through `DocumentHandle` (cheap Clone -- an
`mpsc::Sender` + `Arc<PublishedSnapshot>`). The TUI input loop, which
is a blocking crossterm poll, bridges sync→async via
`lattice_runtime::block_on`. Every mutating method returns
`Pending<T>` (a typed wrapper around `oneshot::Receiver`); the App's
`*_blocking` helpers concentrate the bridge in one place.

**Publish-before-reply.** The actor's run loop publishes the new
snapshot *before* sending the `oneshot` reply for any mutation. This
guarantees that any caller observing the reply also observes the new
snapshot via `arc_swap::load` -- without this ordering, callers can
race past their own commit.

| Concern                                                    | Status               | Anchor                                  |
|------------------------------------------------------------|----------------------|-----------------------------------------|
| Document actor / bounded mpsc mailbox                      | ✅                   | §5.7 (`lattice-runtime::DocumentActor`) |
| `Pending<T>` returned by every mutating call               | ✅                   | §5.2.1 (`lattice-runtime::Pending`)     |
| Bounded backpressure (`RuntimeError::Busy`)                | ✅                   | §5.2.1 (mailbox cap = 64)               |
| `arc-swap` published `DocumentSnapshot`                    | ✅                   | §5.6.8 (`PublishedSnapshot`)            |
| Renderer reads via single snapshot load per frame          | ✅                   | §5.6.8 (`render::draw_*`)               |
| Publish-before-reply ordering                              | ✅                   | §5.6.8 (acquire/release contract)       |
| Sync `lattice_grammar::execute` (runs inside actor)        | ✅                   | §5.2.1 (purity preserved)               |
| Latency-class declarations (Reflex / Display / Background) | ✅ declarative        | §5.2.5 (`LatencyClass` field on `CommandSpec`; runtime enforcement deferred) |
| Cancellation token contract                                | ✅ user-Esc           | §5.2.5; `CancellationToken` (Arc<AtomicBool>) plumbed through `dispatch_with_cancel` → grammar dispatcher → operator/motion/text-object contexts → search loops. Deadline-timer flipper (Reflex < 2 ms, Display < 10 ms) is the remaining piece. |
| Event bus (observation baseline)                           | ✅                   | §5.10; `EventBus` in lattice-runtime: kind-indexed dispatch, `SubscriptionTarget::Channel` (mpsc) + `Invocation` (queued via `drain_pending_invocations`). |
| App-side event publish                                     | ✅                   | §5.10; App publishes `DocumentChanged` (apply_edit / batch / undo / redo), `SelectionsChanged` (set_selections), `ModalModeChanged` (only on actual axis movement), `BeforeSave` + `DocumentSaved` (sync wrapper around save / save_as), `BeforeQuit` (Action::Quit + `:q` after dirty-check), `OptionChanged` (every typed-options registry write — including `:set foo=bar`, `:set nofoo`, and direct `config.set` paths; carries canonical name + old + new formatted strings). |
| Config → event bus bridge                                  | ✅                   | §5.10 + §5.12; `lattice-config::ConfigRegistry` exposes `set_event_publisher(EventPublisher)`. App wires the bus at boot via a closure that calls `event_bus.publish(event)`. Subscribers see option changes through `Event::OptionChanged` instead of polling.                                                                                                                                                                              |
| App-side cascade via bus subscription                      | ✅                   | §5.10 + §5.12. App subscribes a `tokio::sync::mpsc::UnboundedReceiver<Event>` filtered to `EventKind::OptionChanged` at boot; `App::drain_option_changes` consumes it and runs the per-option cascade (`relativenumber⇒number`, `foldmethod⇒recompute_folds`, `ui.*⇒sync_theme_from_config`). Drained at the end of `do_set` (synchronous user-visible behaviour preserved) and at the top of every main_loop iteration (backstop for writes outside the keystroke path -- plugin tasks, customize buffer, init.rs). The chained cascade case (`relativenumber⇒number` itself fires another `OptionChanged`) is handled by the drain's `while let Ok` loop. No registry-mutex re-entrancy risk: publisher closure runs after the registry drops every lock. |
| Veto-class hooks (1ms p99)                                 | ⛔                   | §5.2.1 (needs Before-event return-path so handlers can mutate / abort; v1 publish is observation-only) |
| Events-over-invocation rule                                | ⛔                   | §5.2.5 (needs `:autocmd` and `add-hook` parser front-ends to desugar into `subscribe`) |
| Interactive arg-prompts (§B.1 phase 2)                     | ✅                   | Submitting bare `:cmd<CR>` with a Required first arg arms a prompt: prefills `:cmd `, surfaces the schema's prompt in the echo area, and waits for typed input (Chord-kind args additionally auto-submit on the next captured chord). Optional-default args take the parser's normal path. |
| Multi-pane selection transformation                        | n/a (single-pane v1) | §5.6.8                                  |

This is **Phase 4 / 7's prerequisite** — LSP clients and the WASM
plugin host can now share `DocumentHandle` with the App; both
register against the same actor, both observe the same snapshot
stream. The remaining ⛔ rows (cancellation, latency classes, hook
classification, event bus) layer on top of the actor without
restructuring it.

---

## Performance posture

| Concern                                     | Status | Anchor          |
|---------------------------------------------|--------|-----------------|
| Criterion bench harness                     | ✅     | §8.2            |
| Render hot-path is viewport-bounded         | ✅     | §8.2 (this commit) |
| Actor / runtime benches                     | ✅     | §5.6.8 / §8.2   |
| `LatencyClass` declaration on `CommandSpec` | ✅     | §5.2.5          |
| Test + clippy CI gate                       | ✅     | (.github/workflows/ci.yml) |
| Bench-compile CI gate                       | ✅     | (catches bench rot) |
| Bench baseline recording (push to main)     | ✅     | (artifact upload, no diff yet) |
| Bench regression detection (>10% threshold) | ⛔     | §8.2 -- needs stable runner |
| Per-class budget assertions in CI           | ⛔     | §5.2.5 -- needs cancellation/deadline machinery |
| Allocation discipline check in CI           | ⛔     | §A.6            |
| Long-running session benches                | ⛔     | §A.6            |
| Cross-platform acceptance suite             | ⛔     | §A.6            |

**Render hot path.** `compose_visible_lines` previously did
`buffer.as_string().split('\n').collect::<Vec<String>>()` once per
frame -- O(buffer size) bytes per paint, blowing the §8.2 <2ms frame
budget on any non-trivial buffer. Now uses ropey's O(log n) per-line
API via `Buffer::line(idx)` and materializes only the visible
window (`height` lines, typically 50). 100MB log files now pay the
same per-frame cost as 100-line files.

**Actor benches** (`crates/lattice-runtime/benches/actor.rs`)
characterize the load-bearing async primitives:

| Benchmark | DESIGN target (p99) | Observed (median) |
|---|---|---|
| `apply_edit` round-trip | <100µs | ~80µs (constant across 10/1k/50k lines) |
| `snapshot_load` (`load_full`) | <20ns | ~17ns (Arc bump path) |
| `snapshot_load_cached` (`Cache::load`, steady) | <500ps | ~305ps |
| `snapshot_post_publish_read` | -- | ~17ns |

The `apply_edit` round-trip meets §8.2 with 20% margin. The renderer
now reads through `arc_swap::Cache::load` per frame (~50× faster than
`load_full`) -- `App` holds a `SnapshotCache` rebuilt on each
document switch, the runtime calls `app.snapshot_cache.load_arc()`
once per frame in `runtime.rs`, and the resulting
`&DocumentSnapshot` is threaded through the active-pane render path
(`compose_visible_lines`, `cursor_screen_position`,
`closed_fold_display_span`, `buffer_line_to_visible_row`,
`draw_mode_line`). Inactive panes render different documents and
still go through `entry.handle.snapshot()` (`load_full`); a per-doc
cache map for inactive panes is queued behind a profiling motivator.
The 50+ `app.document.snapshot()` call sites in keystroke handlers
remain on `load_full` -- each runs ≤1× per keystroke, dominated by
other work, so the migration there is deferred.

**`LatencyClass` declaration** (DESIGN.md §5.2.5) is now a field on
every `CommandSpec`. `:describe-command` surfaces it under a
"Latency:" section. v1 is purely declarative; the cancellation /
deadline machinery that enforces it lands with the §5.10 event-bus.
Default classifications:

- **Reflex** (<2ms p99): every motion, operator, text-object; cheap
  ex-commands (`:quit`, `:noh`, `:set`, `:delete`, `:s`, `:g`,
  `:v`).
- **Display** (<10ms p99 sync prelude): file I/O (`:write`,
  `:write-quit`, `:edit`); help-buffer builders (`:reg`, `:marks`,
  `:describe-*`, `:apropos`, `:keymap`).
- **Background**: none yet -- the indexer / file-watcher / LSP
  debounce paths arrive in Phases 4-7.

**CI** (`.github/workflows/ci.yml`) gates every push/PR on:

- `cargo test --workspace --locked` + `cargo clippy --workspace
  --tests --locked -- -D warnings` across a cross-platform matrix
  (ubuntu-latest, macos-latest, windows-latest; `fail-fast: false`).
- `cargo fmt --all -- --check` -- rejects unformatted code.
- `cargo doc --no-deps --workspace` with `RUSTDOCFLAGS=-D warnings`
  -- catches broken intra-doc links before merge.
- `cargo bench --workspace --no-run` per platform -- bench-compile
  rot detection.
- `bench-baseline` on push-to-main runs the benches in `--quick`
  mode and uploads the criterion reports as artifacts. Groundwork
  for the regression gate when stable bench infrastructure
  (self-hosted runner or `bencher.dev`) lands.

Current bench coverage: motions (word_forward / backward / end /
first_non_blank / counted), operators (dw / dd / d_whole / yw / cw / diw /
di_paren), search (forward first / last / no-match-with-wrap / backward),
buffer (insert at origin / middle, delete one byte, position round-trip),
runtime actor (apply_edit round-trip / snapshot_load /
snapshot_post_publish_read at 10/1k/50k lines).

---

## In-progress

(none — pick the next item from "Up next" below.)

Update this section when picking up the in-flight item.

---

## Up next (priority order)

1. **Phase 4: LSP** — diagnostics, completion, hover, go-to-definition,
   references. The cancellation-token plumbing is in place
   (`dispatch_with_cancel` + cooperative search cancellation), so LSP
   request cancellation hooks into existing seams; the remaining work
   is the LSP client (tower-lsp or hand-rolled) + per-server shims.
2. **Computed folds** (per `docs/help/folding.md`) — **✅ done for
   all v1 providers except tree-sitter syntax queries.** Manual
   `zf` / `zo` / `zc` / `za` / `zR` / `zM` / `zd` / `zj` / `zk`,
   plus the new `zi` (`:set foldenable!`). Two computed providers:
   `compute_indent_folds` (universal) and `compute_markdown_folds`
   (ATX heading nesting, code-fence aware for both ``` and ~~~).
   `:set foldmethod=manual|indent|markdown|syntax` parses; `Syntax`
   is a v1 cascade (markdown for `.md`, indent otherwise) until the
   tree-sitter scope-query provider lands.

   Beyond storage, the user-facing pieces from `docs/help/folding.md`:
   identity-hash recompute (heading text + indent depth) preserves
   closed-state across edits in unrelated sections; closed folds
   render heading-preserved with a dim ` ┄ N lines folded` suffix
   (no `+--- N lines ---` line replacement); gutter glyphs ▾ open
   / ▸ closed; `dd` / `yy` / `cc` / `>>` on a closed fold expand
   to the full fold range as a single undo unit; jump-class motions
   (search, gg / G, H / M / L, marks, Ctrl-O / Ctrl-I, `%`) auto-
   open the destination fold; `:set foldenable` + `zi` short-circuit
   every fold-aware path while preserving closed-state.

   Tree-sitter-driven folds (function bodies, classes, blocks via
   `folds.scm`) remain queued. The `Fold` data type is shared, so
   a tree-sitter provider drops into the existing recompute /
   identity / render plumbing without a redesign.
3. **`:set option=value` + typed options** (§5.12) ✅ done — now
   in its own renderer-agnostic crate. The `lattice-config` crate
   owns the option machinery: an `OptionType` trait (with built-in
   impls for `bool`, `i64`, `String`); `Option<T>` whose value cell
   is an `arc_swap::ArcSwap<T>` for wait-free hot-path reads; an
   `ErasedOption` trait + `ConfigRegistry` backing both typed
   `OptionHandle<T>` access and by-name (`:set foo=bar`) access;
   the `:set` syntax parser; and an `OptionsGenerator` that the
   completion pipeline picks up via the `gen:options` source.
   `register_core_options(&registry) → CoreOptions` registers the
   nine renderer-agnostic options (number, relativenumber, wrap,
   ignorecase, tabstop, foldenable, foldmethod, scrolloff,
   completion.auto_insert_single); each renderer registers its
   own UI-specific options through the same `register` API and
   gets its own typed-handle struct back (`lattice-ui-tui` ships
   `register_tui_options → TuiOptions` for `ui.dim_inactive`,
   `ui.separator`, `ui.separator_color`,
   `ui.statusline_active_fg`, `ui.statusline_inactive_fg`).
   `FoldMethod` lives in `lattice-core::folding` (any renderer
   reads it the same way); the `OptionType for FoldMethod` impl
   lives in `lattice-config::domain` to keep core's dep direction
   inward. `:set name=value` echoes are byte-identical to the
   pre-migration wording, including all error messages
   (`E518: Unknown option`, `E474: not a boolean option`,
   `tabstop out of range [1, 32]: N`). Multi-option `:set`
   syntax (`:set ic hls scs`) still deferred.

   **App integration**: `App.config: Arc<ConfigRegistry>` plus
   typed handle structs (`core_options`, `tui_options`) replace
   the previous duplicated `App.foldmethod` /
   `App.show_line_numbers` / etc. fields. Read-side accessors
   (`app.foldmethod()`, `app.tabstop()`, ...) wrap the indirection
   so call sites read like a field access. `do_set` calls
   `config.parse_and_set_command(...)` and runs `apply_post_set`
   for cascade side effects: `relativenumber` ⇒ `number=true`,
   `foldmethod` ⇒ `recompute_folds()`, every `ui.*` ⇒
   `sync_theme_from_config()` to refresh the cached `Theme`
   `Style` projections.

   **§5.12 amendment landed in DESIGN.md (no plugin code yet).**
   Two-layer config codified: `~/.config/lattice/options.toml`
   for static data; `~/.config/lattice/init.rs` compiled to WASM
   Component, loaded by the §5.5 plugin host with a `boot`
   capability, for programmable config. Auto-build on first boot
   (cargo-component under `~/.cache/lattice/`); cache by source
   hash + lattice version + WIT revision. `lattice config build`
   exists as a diagnostic CLI. Project-local code-config deferred
   behind a future per-directory trust prompt; project-local
   `options.toml` supported. Implementation depends on Phase 7
   (plugin host); `lattice-config-api` crate added to the project
   layout as the WIT-bindings reexport user `init.rs` consumes.

3a. **§5.2.1 closure: kind-prefix form on `:`** ✅ done. The legacy
   parser-kind rejection is gone; every command (motion, operator,
   text-object, ex-command, plugin contribution) is reachable from
   `:` via `:<kind> <name>` syntax. Three reserved kind words on
   `:` (`motion`, `operator`, `text-object`); ex-commands keep
   their bare alias surface. Operator targets resolve via
   implicit-namespace lookup. DESIGN.md §2.2 codifies the
   no-function-call-syntax-on-`:` invariant; §5.2.1 specifies the
   kind-prefix grammar. See
   `crates/lattice-ui-tui/src/excommand.rs::parse_kind_prefixed`.
4. **Multi-buffer foundations** (§5.9) — the trigger for `HelpDisplayMode`
   beyond `Popup`. Until this lands, all introspection is overlay-rendered.
   - **B.1.a buffer abstraction + active-buffer routing** ✅ done.
     `BufferKind { Document, Help }` + `BufferId` newtype; `App::active_buffer`
     decides which cursor a motion / page / scroll / `<C-o>` / `<C-i>`
     action mutates. Help routes through the same `translate_normal` chord
     grammar as document buffers; only three buffer-local bindings differ
     (`Esc` / `q` dismiss, `<CR>` follows the link under the cursor). The
     unified position-history ring carries `(buffer, buffer_id)` so jump-list
     walks switch active_buffer cleanly when crossing buffer boundaries.
     `lattice_grammar::execute_motion_only` exposes a read-only motion
     dispatch path that resolves a `CommandInvocation` against a bare
     `Buffer` -- no `Document` / undo / selections required, suitable for
     help and (later) file-tree / outline / diagnostics views.
   - **B.1.b pane tree + splits** ✅ done. Recursive binary-split
     `PaneTree` with `<C-w>{s,v,c,q,h,j,k,l,w,W}` chord grammar. Each
     leaf carries per-pane viewport stash (cursor + scroll); the
     active pane's stash is hot-loaded into `App::cursor` /
     `App::scroll` so motion code stays unchanged. Active-pane
     switches snapshot back into the source pane's stash and load
     from the destination's. Geometry-aware navigation
     (`<C-w>{h,j,k,l}`) walks the spatial neighbour computed from
     `compute_rects`. Inactive-pane rendering is a placeholder
     until B.1.c brings meaningfully distinct buffer content.
   - **B.1.c multiple Document buffers** ✅ done. Replaced by the
     unified registry below; original implementation used a
     dedicated `documents: HashMap<BufferId, DocumentEntry>`.
   - **B.1.d buffer-as-content kinds: file-tree** ✅ done. New
     `BufferKind::FileTree` variant + `FileTreeBuffer` (rope-
     backed, same shape as `HelpBuffer`). `:Tree [path]` opens a
     tree buffer rooted at `path` (or the document's parent dir /
     cwd); same path de-dups (already-open trees are switched to,
     not duplicated). Multiple trees coexist (one per
     distinct root). `:TreeClose` removes from the registry.
     Standard motions route via the same active-buffer dispatch
     as Help. `<CR>` on a directory toggles expansion; on a file
     opens it via the standard `:e FILE` path. Outline + diagnostics
     panels queue behind their own integrations.
   - **Unified `BufferRegistry`** ✅ done. Documents and file trees
     live in a single keyspace under `App.buffers`
     (`HashMap<BufferId, BufferEntry>` with `BufferData::Document |
     FileTree` discriminant). `:bn` / `:bp` / `:ls` / `:bd` /
     `:b N` operate on the registry uniformly -- cycling between a
     document and a tree feels the same as cycling between two
     documents. Each entry carries `BufferFlags { listed, hidden }`;
     unlisted buffers are skipped by `:bn` / `:bp`. `:e folder`
     defers to `:Tree folder` (vim's `:Explore` semantics). Help
     stays overlay-rendered for now -- moving it into the registry
     is a follow-up that doesn't require structural change.
     `DocumentEntry` stashes per-buffer hot-path state (syntax tree,
     fold list, last-parsed version) when a buffer leaves active;
     a single `App::activate_buffer_state` lifecycle hook fires on
     every transition into a document buffer (both `:e <new>` and
     `:b N` paths), reparsing if needed and seeding folds against
     the active `foldmethod` so users no longer have to reach for
     `<C-l>` after switching files. New buffer-level state plugs
     into the same hook -- no per-option fixups across the
     activation paths.
   - **Pane visuals** ✅ done. Each pane gets a vim-style status
     line (active reverse-videoed via theme, inactive dim);
     `│` separator drawn between vertically split panes. Inactive
     Document panes keep their tree-sitter syntax highlights
     (refreshed lazily by `App::refresh_pane_highlights`); a
     `Theme::inactive_pane_overlay` modifier (default `DIM`)
     layers on top so focus stays unambiguous without losing
     color. Customizable via `:set ui.dim_inactive`,
     `ui.separator`, `ui.separator_color`,
     `ui.statusline_active_fg`, `ui.statusline_inactive_fg`.
5. **Hover popup + inline completion popup polish** — completion popup
   is wired (vertico-style); hover popup scaffolding ✅ done. New
   `HoverPopup` type with markdown body + buffer-position anchor;
   markdown highlights computed via the shared `LangRegistry`. The
   renderer floats the popup near the cursor (below if room, above
   otherwise). `:hover [text]` opens manually for now (Phase 4 LSP
   will source `text` from `textDocument/hover`); `:HoverClose`
   dismisses.
5b. **Help topic surface (`:help`)** ✅ done.
    `lattice-ui-tui::help_topics` defines a `HelpTopicRegistry`
    keyed by name; bodies are either `Static(&'static str)`
    (built-ins are sourced from `docs/help/*.md` via
    `include_str!` so the binary is self-contained) or
    `Dynamic(closure)` -- the seam for LSP / plugin / config-
    supplied topics. `:help` with no arg opens the registry's
    `index` topic (the README content); `:help <topic>` opens the
    named topic; `:h` is an alias. `<Tab>` enumerates topics via a
    new `gen:help-topics` completion source. `:describe-*`
    appends `See also: [topic](help:topic)` cross-links when a
    topic's `related_command_patterns` substring-matches the
    described command. New `HelpLinkTarget::Topic(name)` variant
    + `help:` URL scheme so topic links are first-class everywhere
    a help body can render.
6. **Help major mode + tree-sitter grammar** — defines sections,
   link-targets, code-blocks. Needs the help mode registered as a major
   mode, which depends on the modes registry (Phase 8) but the *grammar*
   can be drafted earlier.
7. **Veto-class hooks + actor event publish** (§5.10.2 / §5.2.1) —
   observation-only event bus is in place; pre-mutation hooks
   (`BeforeSave`, `BeforeQuit`) need the mutation/abort return path.
   Actor wiring to publish `DocumentChanged` / `SelectionsChanged` /
   `ModalModeChanged` events. Unblocks autocmds.
8. **Per-`LatencyClass` deadline timers** — Reflex commands observe a
   2 ms deadline, Display commands 10 ms, both via the cancellation
   token already plumbed.
9. **Bench regression gate** — needs a stable runner (self-hosted or
   `bencher.dev`); shared GitHub runners have ~20% bench variance that
   dwarfs a 10% regression signal.
10. **Render-hot-path alloc discipline** — dhat-based assertion that
    steady-state frames produce no allocations.

---

## §15 open questions still load-bearing

These are tracked in DESIGN.md §15. Items the implementation has resolved
are crossed out there. Items that influence active tasks:

- §15:18 Folds storage / interaction — feeds the **Computed folds** task above.
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

Workspace tests as of the last commit. Coverage by crate:

| Crate                            | Tests |
|----------------------------------|-------|
| lattice-protocol                 | 30    |
| lattice-core (incl. integration) | 86    |
| lattice-grammar                  | 183   |
| lattice-completion               | 117   |
| lattice-config                   | 57    |
| lattice-syntax                   | 13    |
| lattice-runtime                  | 35    |
| lattice-lsp                      | 32    |
| lattice-ui-tui                   | 906   |

Plus criterion benches for hot paths (search, buffer, motions, operators,
runtime actor) — see `docs/BENCHMARKS.md` for the latest numbers.

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
