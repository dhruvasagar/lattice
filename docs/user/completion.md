---
summary: "Insert-mode completion: triggers, sources, popup keymap, configuration, troubleshooting."
related: [complete, completion, snippet, ex:complete]
---

# Insert-mode completion

Completion in lattice surfaces inside Insert mode as a popup
near the cursor: type some characters, press the trigger
chord, pick a candidate, accept. Sources contributing to the
popup come from many places — LSP servers, snippet
registries, words in your buffers, paths under the cursor,
local symbols from the syntax tree. They all converge in one
list, ranked together.

> **Status:** Phase 4.2.g.1 + 4.2.g.2 + 4.2.g.3 + 4.2.g.4
> shipped. Manual trigger via `<C-x><C-o>` / `<C-Space>`;
> buffer-words, LSP, **and snippet** sources contribute to
> the unified popup; LSP items carry through with their
> `filterText` / `sortText` / `detail` /
> `additionalTextEdits` / `commitCharacters`; `isIncomplete`
> refresh re-fires LSP on each keystroke when the server
> requests it. **`<C-d>` toggles a side documentation popup**
> for the focused candidate; `completionItem/resolve` fires
> lazily for items that arrived without `documentation`;
> `<C-f>` / `<C-b>` page through long docs bodies. **Snippets
> ship with a TextMate JSON parser, render walker, active
> state machine** (`<Tab>` / `<S-Tab>` / `<Esc>` minor mode)
> + LSP `insertTextFormat == Snippet` routing through the
> same engine. Per-source priority + frequency ranking
> (4.2.g.5), tree-sitter / path sources (4.2.g.6) follow. The
> behavioural spec for the full surface is in
> [`docs/insert-completion.md`](../dev/architecture/insert-completion.md).

---

## Triggering

`<C-Space>` in Insert mode opens the popup. That is the
default: completion opens when you ask. `:describe-buffer` lists
the sources a buffer completes from, and why any is off.

`:set completion.auto_trigger` opens it as you type instead:
once the word before the cursor is `completion.min_chars` (2)
characters long, or on a character a source declares as its
trigger — `.` for a language server, say, which then asks only
the sources that declared it. Markdown and plain text stay
manual even with it on; turn it on for one language with
`[completion.per-language.<lang>] auto_trigger = true`.

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
| `<C-d>` | Toggle docs side popup | `:complete-docs` |
| `<C-f>` | Page docs popup forward | `:complete-docs-down` |
| `<C-b>` | Page docs popup backward | `:complete-docs-up` |

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
| `gen:lsp-completion` | `textDocument/completion` results from every LSP server attached to the buffer. Item adaptation: `filterText` (matcher target) / `sortText` (ranker tiebreaker) / `detail` (one-line signature) / `documentation` (markdown body for the docs popup) / `tags[Deprecated]` (strikethrough + ranker penalty in 4.2.g.5) / `commitCharacters` (auto-accept-then-insert on type, 4.2.g.7) / `additionalTextEdits` (auto-imports, applied alongside the main insert as one undo unit). `isIncomplete: true` re-fires the request on every keystroke that mutates the query. | 4.2.g.2 ✅ |
| `gen:snippet` | Per-language snippets from `lattice-snippet`. TextMate JSON format; drop-in compatible with VS Code / friendly-snippets. | 4.2.g.4 ✅ |
| `gen:path` | Filesystem entries when typing a path inside a string literal. Tree-sitter scope detection identifies the string context (rust / python / javascript today); walk caps at 200 entries; hidden files + `.git` / `node_modules` / `target` / `dist` skipped. | 4.2.g.6 ✅ |
| `gen:tree-sitter-symbol` | Definition-position identifiers from the buffer's syntax tree (function defs, struct / enum / trait / type names, `const` / `static`, `let` bindings, function parameters, modules). Per-language `symbols.scm` query in `lattice-syntax`; rust / python / javascript ship today. | 4.2.g.6 ✅ |
| `gen:plugin-*` | Reserved for plugin-supplied sources via the WASM Component plugin host (Phase 7). | post-4.x |

See `:help completion-sources` for per-source detail.

## Snippets (Phase 4.2.g.4)

Snippets are templates with **placeholders** you cycle
through. The engine implements TextMate / LSP snippet syntax,
so VS Code's `friendly-snippets` pack drops in unchanged.

### Where snippets come from

A small **built-in** set ships with the editor and is available
from the first keystroke — no install step. It covers common
Rust scaffolding (`fn`, `for`, `match`, `impl`, `struct`,
`test`, …) plus a few language-agnostic ones (`todo`, `fixme`,
`date`, `uuid`).

To add your own, put `<language>.json` packs (`ruby.json`,
`shellscript.json`; `_global.json` for every language) or VS Code
`*.code-snippets` files in **`~/.config/lattice/snippets/`**, and run
**`:reload-snippets`**. A friendly-snippets clone goes in
`~/.config/lattice/snippet-packs/` (or any folder named in
`snippet.dirs`); a project's own in `.lattice/snippets/`. Later wins on a
shared prefix: packages, then yours, then the project's. The echo reports the
split, e.g. `reloaded 24 snippets (22 built-in + 2 user)`.

### Where snippets activate

Snippets are gated by a minor mode (`snippet-mode`) whose
activation is controlled by two options:

| Option | Values | Default | Meaning |
|---|---|---|---|
| `snippet.activation` | `global` · `supported-languages` · `off` | `global` | Which buffers get snippets. |
| `snippet.languages` | comma-separated language ids | *(empty)* | Allowlist consulted only when `activation = supported-languages`. |

With the default `global`, every document buffer has snippets
enabled — but the source still self-filters by language, so a
Rust buffer only ever sees Rust (+ `_global`) snippets. `global`
means "each buffer sees *its own* language's snippets", not "all
snippets everywhere".

To restrict snippets to specific languages, set
`snippet.activation = supported-languages` and list the
languages:

```toml
[snippet]
activation = "supported-languages"
languages  = "rust,python"   # note: a string, not a TOML list
```

Each id `L` maps to the major mode `L-mode`, so only languages
with a registered major mode match. `snippet.activation = off`
disables auto-activation entirely.

Both keys are live-settable (`:set snippet.activation=off`,
`:set snippet.languages=rust,python`); `:set snippet.activation=`
then `<Tab>` completes the three values. A change takes effect
for buffers opened *afterward* — buffers already open keep their
current snippet state until reopened.

### Two ways to expand

- **Through the popup.** `gen:snippet` contributes candidates
  to the unified popup. Select one and `<C-y>` / `<Tab>` /
  `<CR>` expands the body and starts placeholder navigation.
- **Direct chord.** `<C-x><C-s>` looks up the word at the
  cursor in the per-language registry and expands the first
  match without surfacing the popup. Quiet no-op when no
  snippet matches.

LSP-supplied items whose `insertTextFormat` is `Snippet` route
through the same engine — the server's templated `insertText`
becomes a real expansion with placeholders, not a literal
splice.

### Active-snippet minor mode

While a snippet is in flight, an **active-snippet minor mode**
binds three keys until the snippet ends:

| Chord | Action |
|---|---|
| `<Tab>` | Next placeholder; reaching the final stop ends the snippet |
| `<S-Tab>` | Previous placeholder |
| `<Esc>` | End the snippet and return to Normal |

When you land on a placeholder that has a **default**
(`${1:value}`), its text is **selected** (the buffer enters
[Select mode](help:modal-editing)) — so the first character you
type **replaces the whole default** and drops you into Insert,
the conventional "type to replace the placeholder" behaviour. To
keep the default, press `<Tab>`. An **empty** tabstop (`$1`) places a
bare Insert cursor — there is nothing to overtype. The minor mode
yields to the completion popup when both are open: `<Tab>`
accepts a candidate first, then snippet navigation resumes.

### Snippet syntax (TextMate / LSP)

```text
$1                       plain tabstop
${1:default}             placeholder with default text
${1|one,two,three|}      choice (<C-n>/<C-p> cycle the options)
$0                       exit position (last)
$1 ... $1 ...            mirrors -- edits ripple
${TM_FILENAME}           variable (see below)
${1/pat/sub/flags}       transform on the captured text
\$ \\                    escape
```

VS Code's snippet variables all resolve — `$TM_FILENAME`,
`$TM_FILENAME_BASE`, `$RELATIVE_FILEPATH`, `$WORKSPACE_NAME`,
`$LINE_COMMENT`, `$BLOCK_COMMENT_START`, `$CLIPBOARD`, `$CURRENT_*` (local
time), `$UUID`, `$TM_SELECTED_TEXT` (the text you wrap, below), … An
unknown name becomes a placeholder showing it.

### Wrapping text in a snippet

`gS` is an operator: `gSiw`, `gSip`, `gSaf`, `gSS` (the line), or `gS` in
Visual. It opens the snippet picker, and the snippet you pick replaces
the text, which lands where the snippet says `$TM_SELECTED_TEXT`. Wrapped
lines keep their indentation and nesting. `<Esc>` in the picker changes
nothing; `u` undoes a wrap.

### Loading snippet packs

`:reload-snippets` re-reads every directory configured in
`App::snippet_dirs` and rebuilds the registry. Files are
keyed by stem: `rust.json` → language `"rust"`,
`_global.json` → the all-language `*` slot. Format is
friendly-snippets-compatible TextMate JSON:

```json
{
    "for loop": {
        "prefix": "for",
        "body": [
            "for ${1:i} in ${2:iter} {",
            "\t$0",
            "}"
        ],
        "description": "for-in loop"
    }
}
```

`prefix` and `body` accept either a string or an array of
strings (joined with `\n`). Top-level keys are snippet names.

### Ex-commands

| Command | What it does |
|---|---|
| `:reload-snippets` | Re-read every configured snippet directory and rebuild the registry. |

Direct expansion has no ex-command form — use the `<C-x><C-s>`
chord (contributed by `snippet-mode`).

## Tree-sitter symbols (Phase 4.2.g.6)

The `gen:tree-sitter-symbol` source pulls definition-position
identifiers from the buffer's syntax tree -- the names you'd
typically want to complete to within the file.

### What gets captured

Per-language `symbols.scm` queries in
`crates/lattice-syntax/queries/<lang>/symbols.scm` define what
counts as a definition. Today's coverage:

| Language | Captures |
|---|---|
| Rust | `fn` / `fn`-signature names, `struct` / `enum` / `union` / `trait` / `type` names, `const` / `static` names, `let` bindings (flat patterns), function parameters, `mod` names. |
| Python | `def` / `class` names, parameter identifiers (positional, default, typed, lambda), assignment targets, `for ... in` binding, `with ... as` binding. |
| JavaScript | `function` / `class` / generator declaration names, method names, `var` / `let` / `const` declarators, formal parameters, single-identifier arrow-function params. |

Markdown / markdown-inline ship no symbols query (prose has
no symbol semantic in this sense); the source emits nothing
for those buffers.

### Versus buffer-words

Both sources see the same definition-position names
(buffer-words walks every alphanumeric run; tree-sitter
walks the parse tree). Spec §3.4 puts `gen:buffer-words`
at priority 100 and `gen:tree-sitter-symbol` at 80 so the
buffer-words copy ranks first when both contribute the same
name -- matching the popup ordering most users expect.
Tree-sitter's value over the long run is its **structural
signal** (it knows `outer` is a function, not just a word);
future popup polish (4.2.g.7) will surface that as kind
glyphs / source labels.

### When LSP is attached

LSP completions are usually richer (full signatures,
documentation, additional edits) so they outrank both at
priority 200. The tree-sitter source is most useful in
unsaved scratch files, languages without an LSP server, and
when the LSP is still warming up.

## Path completion (Phase 4.2.g.6)

The `gen:path` source surfaces filesystem entries when the
cursor sits inside a string literal. Triggering completion
inside `"src/|"` walks the `src/` directory and lists its
entries; typing more of the filename narrows the popup; on
accept the popup-supplied filename replaces just the current
path segment (everything after the last `/`).

### Scope detection

A tree-sitter scope check classifies the cursor: any
ancestor whose node kind matches a string-literal shape
(`string`, `string_literal`, `raw_string_literal`,
`byte_string_literal`, `template_string`, `string_fragment`)
opens path-completion mode. Outside string scope the popup
behaves normally; the path source contributes nothing.

### Path resolution

- Absolute paths (`"/etc/hosts"`) walk the filesystem from
  `/`.
- Relative paths walk from the active document's directory
  when the buffer has a path on disk; unsaved buffers fall
  back to `std::env::current_dir()`.

### What's filtered

The walk caps at 200 entries per directory and skips:

- Dotfiles (any name starting with `.`).
- `.git`, `node_modules`, `target`, `dist`.

`.gitignore` integration queues for a follow-up.

### Path-completion popup behaviour

When the popup opens in path-completion mode:

- Only `gen:path` candidates contribute. The buffer-words /
  snippet / tree-sitter sources skip emit so the popup shows
  filesystem entries cleanly. The LSP request short-circuits
  for the same reason.
- Directories carry a trailing `/` in the displayed text; on
  accept, the cursor lands just past the slash, ready for the
  next path segment.
- The popup closes on whitespace / non-path characters as
  usual; accept a directory and re-trigger to descend.

## Configuration

Every option is typed and queryable via
`:describe-option completion.<name>`. The default values are
chosen for "vim-like minute-one experience" — manual
trigger, conservative thresholds, no popup noise during
prose typing.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `completion.auto_trigger` | bool | `false` | Open the popup automatically as you type. |
| `completion.min_chars` | int | `2` | With `auto_trigger`, how many characters of a word open the popup. |
| `completion.extra_commit_chars` | string | `""` | Editor-side commit characters unioned with each LSP server's per-item `commitCharacters`. Typing one of these while the popup is open accepts the focused candidate then inserts the character (e.g. `:set completion.extra_commit_chars=.,;` to accept on `.`, `,`, or `;` globally). (4.2.g.7) |
| `completion.ghost_text` | bool | `false` | Render the top-ranked candidate's suffix as a dimmed inline overlay after the cursor while the popup is open. Off by default; flip on for vscode-style preview. Only fires when the cursor is at end-of-line and the top candidate is a case-insensitive prefix of the typed query. Suppressed in path-completion mode (filenames are already shown in full). (4.2.g.7) |
| `completion.auto_insert_single` | bool | `true` | Auto-accept when only one candidate matches. |
| `completion.source.<name>.priority` | int | source-default | Ranker priority of `lsp`, `snippet`, `buffer-words`, `path` or `tree-sitter`. A plugin's source ranks by the priority it declares. |

See `:help completion-options` for the full list with
explanations and examples (`:help` topic auto-rendered from
`:describe-option completion.*` entries).

## Per-language overrides

Lattice ships sensible per-language defaults out of the box;
the TOML form below lets you override any of them. Values
parse from `~/.config/lattice/lattice.toml` (user) and
`<workspace>/.lattice/config.toml` (project, takes precedence)
at startup.

### Built-in defaults

| Language | `sources` | `auto_trigger` | `auto_insert_single` |
|---|---|---|---|
| `markdown` | `snippet`, `buffer-words` (no LSP for prose) | `false` | inherits global |
| `text` | `snippet`, `buffer-words` | `false` | inherits |
| `rust` | inherits (all enabled sources) | inherits | `true` |

Other languages inherit every global option.

### TOML form

```toml
[completion.per-language.markdown]
sources = ["snippet", "buffer-words"]
auto_trigger = false

[completion.per-language.rust]
auto_trigger = true
auto_insert_single = true

[completion.per-language.python]
sources = ["lsp", "snippet"]    # LSP-led; skip noisy buffer words
```

### Recognised keys

- **`sources`** (`string[]`) — restrict the popup to these
  source ids. Short labels: `lsp`, `snippet`, `buffer-words`,
  `path`, `tree-sitter`. Plugin sources work by full id
  (`plugin:my-source`). Unset = every enabled source
  contributes. Sources outside this list short-circuit at
  emit time, so an LSP-disabled language never even fires
  the `textDocument/completion` request.
- **`auto_trigger`** (`bool`) — whether typing opens the popup
  automatically in this language. Inherits the global
  `completion.auto_trigger` when unset.
- **`auto_insert_single`** (`bool`) — whether the popup
  auto-accepts when only one candidate matches. Inherits
  the global `completion.auto_insert_single` when unset.
- **`suppress_in`** (`string[]`) — tree-sitter scopes where
  the popup should not fire (e.g. `["string", "comment"]`).
  Parsed, not yet applied.
  Plumbed today; scope-detect enforcement awaits the
  tree-sitter scope-query slice.

Per-key TOML overrides merge onto the built-in defaults — a
markdown override that only sets `auto_trigger = true`
preserves the default `sources` list (snippet +
buffer-words). Override a key with `[]` for an empty list to
explicitly clear the default.

Per-buffer overrides land with `:setlocal` (post-1.0); today
the per-language form is the supported runtime control.

## Kind icon reference

Each completion candidate carries a **kind icon** in the
annotation column (right side of the popup row) so you can
identify the type of the suggestion at a glance:

| Icon | Meaning |
|------|---------|
| `:`  | ex-command — e.g. `:write`, `:q` |
| `→`  | motion — e.g. `w`, `j`, `gg` |
| `~`  | operator — e.g. `d`, `c`, `y` |
| `…`  | text-object — e.g. `iw`, `ap`, `it` |
| `·`  | action — generic command action |
| `=`  | option — a `:set`-able option |
| `f`  | file or file path |
| `d`  | directory |
| `/`  | search pattern |
| `b`  | buffer — a buffer entry |
| `"`  | register — e.g. `"a`, `"0` |
| `'`  | mark — e.g. `'a`, `'.` |
| `@`  | chord — a key chord binding |
| `+`  | plugin — a plugin-provided command or API |

The same icon appears in help views (`:describe-command`,
`:describe-key`, `:apropos`) as the kind indicator next to
the identifier — for example, `ex:write :` or `motion:line-down →`.

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

**LSP completions don't appear.** Check:

1. `:lsp-status` shows the server is running for the
   buffer's language.
2. The server advertises `completionProvider` (most do).
3. The cursor is on a position the server recognises as
   completion-eligible (typically inside an identifier).
4. The LSP request didn't time out — server logs at
   `:lsp-log <server>` show response activity.

The legacy `:complete` ex-command still works as an
alternative — it opens a vertico picker over the LSP items
(useful when you want a fixed-size scrollable list).
`<C-x><C-o>` / `<C-Space>` is the recommended path for
typing-flow completion.

## Cross-references

- [`completion-keymap`](#) — every default binding in every
  layer.
- [`completion-sources`](#) — per-source detail.
- [`completion-options`](#) — every option with type +
  default + example.
- [`completion-popup`](#) — multi-column layout details.
- [`completion-snippets`](#) — TextMate snippet syntax (4.2.g.4).
- [`lsp`](help:lsp) — LSP integration overall.
- [`docs/insert-completion.md`](../dev/architecture/insert-completion.md) —
  full behavioural spec.
