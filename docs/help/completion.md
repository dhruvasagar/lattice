# Insert-mode completion

Completion in lattice surfaces inside Insert mode as a popup
near the cursor: type some characters, press the trigger
chord, pick a candidate, accept. Sources contributing to the
popup come from many places — LSP servers, snippet
registries, words in your buffers, paths under the cursor,
local symbols from the syntax tree. They all converge in one
list, ranked together.

> **Status:** Phase 4.2.g.1 ships the foundation — manual
> trigger via `<C-x><C-o>` / `<C-Space>`; buffer-words source
> only. LSP source (4.2.g.2), docs popup (4.2.g.3), snippet
> engine (4.2.g.4), per-source priority + frequency ranking
> (4.2.g.5), tree-sitter / path sources (4.2.g.6) follow in
> their own commits. The behavioural spec for the full
> surface is in [`docs/insert-completion.md`](../insert-completion.md).

---

## Triggering

Three paths open the popup:

- `<C-x><C-o>` — vim's omni-completion chord. Vim-native
  muscle memory.
- `<C-Space>` — modern-editor muscle memory (VS Code, IntelliJ,
  Helix).
- Smart-tab — `<Tab>` triggers completion when the cursor is
  right after a word character; `<Tab>` inserts a literal
  tab when the cursor is on whitespace or at line start.
  (Phase 4.2.g.5.)

All three resolve to the same `cmd:completion-trigger`
ex-command. Manual trigger is the default; auto-trigger as
you type is opt-in via `:set completion.auto_trigger=true`
(coming with 4.2.g.5).

## Navigating the popup

Once the popup is open, lattice activates a transient
**completion-popup minor mode** — a thin keymap layer above
the usual Insert / Normal bindings. It owns the keys it
overrides for the popup's lifetime; closing the popup
deactivates the layer and the original bindings restore.

| Chord | Action | Ex-command |
|---|---|---|
| `<C-n>` / `<Down>` | Next candidate | `:complete-next` |
| `<C-p>` / `<Up>` | Previous candidate | `:complete-prev` |
| `<C-y>` | Accept selected (vim) | `:complete-accept` |
| `<Tab>` | Accept selected | `:complete-accept` |
| `<CR>` | Accept selected | `:complete-accept` |
| `<C-e>` | Cancel popup, stay in Insert (vim) | `:complete-cancel` |
| `<Esc>` | Cancel popup AND exit to Normal (vim) | `:complete-cancel!` |
| `<C-Space>` | Re-trigger / refresh | `:complete-trigger` |
| `<C-d>` | Toggle docs side popup (4.2.g.3) | `:complete-docs` |

Outside the popup, `<C-d>` keeps its Insert-mode
shift-left-indent meaning and `<C-d>` in Normal stays
half-page-down. The minor mode confines the override to the
popup's lifetime by design.

## Closing the popup

The popup closes automatically when:

- You press `<C-e>` (cancel, keep Insert) or `<Esc>` (cancel
  + exit Insert).
- You accept a candidate (`<C-y>` / `<Tab>` / `<CR>`).
- You move the cursor outside the popup's anchor range
  (e.g., `<Left>` / `<BS>` past the start, or `<Right>` past
  any newly-typed text into a different word).
- You type a non-word character (space, punctuation, etc.).
- The current set of candidates filters down to nothing as
  you keep typing.

## Sources

The list below is what 4.2.g ships. Each source is
configurable independently — enable / disable, override the
priority, set source-specific knobs (e.g.
`completion.source.buffer-words.min_word_length`).

| Source id | What it contributes | Ships in |
|---|---|---|
| `gen:buffer-words` | Words from the active buffer (and visible buffers in 4.2.g.6) — alphanumeric + underscore runs, deduped, default min length 3. | 4.2.g.1 ✅ |
| `gen:lsp-completion` | `textDocument/completion` results from every LSP server attached to the buffer. | 4.2.g.2 |
| `gen:snippet` | Per-language snippets from `lattice-snippet`. TextMate JSON format; drop-in compatible with VS Code / friendly-snippets. | 4.2.g.4 |
| `gen:path` | Filesystem entries when typing a path inside a string literal. | 4.2.g.6 |
| `gen:tree-sitter-symbol` | Local identifiers from the buffer's syntax tree (function defs, let-bindings, closure params). | 4.2.g.6 |
| `gen:plugin-*` | Reserved for plugin-supplied sources via the WASM Component plugin host (Phase 7). | post-4.x |

See `:help completion-sources` for per-source detail.

## Configuration

Every option is typed and queryable via
`:describe-option completion.<name>`. The default values are
chosen for "vim-like minute-one experience" — manual
trigger, conservative thresholds, no popup noise during
prose typing.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `completion.auto_trigger` | bool | `false` | Open the popup automatically as you type. (4.2.g.5) |
| `completion.min_chars` | int | `2` | Identifier-threshold characters before auto-trigger fires. |
| `completion.debounce_ms` | int | `50` | Per-keystroke debounce for refilter passes. |
| `completion.docs_auto` | bool | `false` | Auto-show docs popup on selection change. (4.2.g.3) |
| `completion.extra_commit_chars` | string | `""` | Editor-side commit characters. (4.2.g.2) |
| `completion.auto_insert_single` | bool | `false` | Auto-accept when only one candidate matches. |
| `completion.fuzzy_threshold` | int | `5` | Minimum match score to render. |
| `completion.suppress_in` | string list | `[]` | Tree-sitter scopes where completion is suppressed (e.g. `["string", "comment"]`). |
| `completion.source.<id>.enabled` | bool | `true` | Toggle a source on/off. |
| `completion.source.<id>.priority` | int | source-default | Per-source ranker priority. |

See `:help completion-options` for the full list with
explanations and examples (`:help` topic auto-rendered from
`:describe-option completion.*` entries).

## Per-language overrides

```toml
[completion.per-language.markdown]
sources = ["snippet", "buffer-words", "path"]
auto_trigger = false

[completion.per-language.rust]
auto_trigger = true
auto_insert_single = true
```

Per-buffer override via `:setlocal completion.auto_trigger=true`
once `setlocal` lands; the global form works today.

## Troubleshooting

**The popup doesn't open.** Check:

1. You're in Insert mode (`i` / `a` / `o` / `O` / `s`).
2. You typed at least one identifier character before
   `<C-Space>` / `<C-x><C-o>`.
3. `:set completion.auto_trigger?` is `false` — manual is
   the default.
4. Buffer has alphanumeric content for `gen:buffer-words` to
   draw from.

**The popup opens then immediately closes.** The popup
auto-dismisses when no candidates fuzzy-match the prefix.
Echo line says "no completions". Type a different prefix.

**LSP completions don't appear.** 4.2.g.2 hasn't shipped
yet. The bridge `:complete` command opens an LSP picker
instead — until 4.2.g.2 lands.

## Cross-references

- [`completion-keymap`](#) — every default binding in every
  layer.
- [`completion-sources`](#) — per-source detail.
- [`completion-options`](#) — every option with type +
  default + example.
- [`completion-popup`](#) — multi-column layout details.
- [`completion-snippets`](#) — TextMate snippet syntax (4.2.g.4).
- [`lsp`](lsp.md) — LSP integration overall.
- [`docs/insert-completion.md`](../insert-completion.md) —
  full behavioural spec.
