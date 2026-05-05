# Insert-Mode Completion (design)

This document is the design spec for buffer-level Insert-mode
completion in lattice. It is the editor surface that turns the
existing [`lattice-completion`](../crates/lattice-completion/)
pipeline (today: cmdline / minibuffer only) into a first-class
input flow inside Insert mode, and it is the consumer that
finally lands LSP `textDocument/completion` as one source
among many — replacing the `:complete` picker bridge from
Phase 4.2.g with an inline-as-you-type popup.

It is also the canonical place to point at when the question
"how does completion work in lattice?" comes up. Implementer
references live in
[`lsp-architecture.md`](lsp-architecture.md) (LSP wire
detail) and [`crates/lattice-completion/`](../crates/lattice-completion/)
(pipeline traits + cmdline path); this document is the
behavioural spec they hang off.

Status: spec in place; implementation queued as Phase 4.2.g
follow-on.

---

## 1. Goals

1. **Multi-source.** LSP, snippets, buffer words, paths in
   strings, tree-sitter local symbols, plugin-supplied. A
   single popup renders the union; sources contribute
   independently and can be enabled / disabled per-language
   or per-buffer.
2. **Async-first.** No source blocks the input loop. LSP
   round-trips, filesystem walks, plugin generators all flow
   through a token-cancellable `tokio` task pipeline. The
   keystroke handler returns instantly; the popup updates as
   sources finish.
3. **Fuzzy matching with ranking.** Same matcher / ranker
   shape as the picker, with per-source priority and
   frequency-aware bias. The popup is "the right answer at
   the top," not a giant alphabetical wall.
4. **Multi-column display + side documentation popup.**
   `[kind glyph] [label]   [detail]   [src]` per row;
   selecting a row optionally opens a side popup with the
   full documentation.
5. **Vim-grammar compatible.** Existing `<C-x>`-prefixed
   chords + `<C-n>` / `<C-p>` semantics work; the popup
   doesn't fight Insert-mode bindings the user already has.
6. **Snippet engine.** First-class. Tab-stop placeholder
   navigation, choice placeholders, transformations, and
   server-supplied snippets all expand the same way.

Non-goals (deferred to follow-up):

- AI / Copilot-style multi-line ghost text.
- "Smart completion" type-aware filtering (JetBrains
  Ctrl+Shift+Space).
- Postfix templates (`.var`, `.if`).

---

## 2. Survey of modern editors

Quick tour of what works in the field. Choices below pull
from this matrix.

| Editor | Trigger | Sources | Matcher | Docs popup | Snippets | Notes |
|---|---|---|---|---|---|---|
| VS Code | trigger chars + alpha + `<C-Space>` | LSP + snippets + word + plugin | fuzzy (FZF) | side panel, lazy-resolve | textmate, full | "suggestion mode" toggle (insert vs replace), commit chars |
| Neovim (`blink.cmp` / `nvim-cmp`) | per-source debounce | LSP + snippets + buffer + path + tree-sitter + LSP-snippet + AI | per-source matcher | side popup, configurable | LuaSnip / vsnip | per-source priority + `min_keyword_length`, ghost text |
| Helix | trigger chars + alpha | LSP + snippets + word + path | fuzzy (FZF) | side popup | textmate | async pipeline; preselect honoured |
| JetBrains | trigger + `<C-Space>` (basic) / `<C-S-Space>` (smart) | LSP + LiveTemplates + word + DB-aware | substring + camelCase | popup right (`<C-Q>`) | LiveTemplates | statistics-based ranking, postfix templates, type-aware filter |
| Sublime | alpha auto | snippets + word | fuzzy | n/a | textmate | one of the original "fast feel" benchmarks |
| Emacs (`corfu` + `cape`) | trigger + alpha | per-major-mode generators | orderless (multi-substring) | side popup or echo area | yasnippet | great composability — every mode plugs its own generators |

Common patterns lattice should ship:

- **Trigger chars + alpha + manual.** All three; users get
  what they expect from any of the three "fingers."
- **Per-source priority and matchers.** Sources don't all
  need the same matcher (snippets benefit from prefix +
  abbreviation; LSP is fine with fuzzy).
- **Lazy-resolve documentation.** Don't pay
  `completionItem/resolve` until the user actually focuses
  the item.
- **Side popup for docs.** Right of the completion popup
  when there's room; below otherwise.
- **Commit characters.** Server-advertised; typing them
  auto-accepts the current item.
- **`additionalTextEdits` on accept.** Auto-import without
  user intervention.

Choices we deliberately **don't** copy:

- Type-aware "smart" basic completion (JetBrains). The
  language server can sort items; we trust its `sortText`.
  Adding our own static analysis is post-1.0.
- Statistics-based ranking that persists across sessions.
  Frequency tracking is in-session only for v1; persisting
  to disk needs a privacy story.
- "Hippie expand" cycle through every word in the universe
  (Emacs's `dabbrev-expand`). Buffer-words covers the
  legitimate use case; cycling globally creates noisy
  suggestions.

---

## 3. Architecture

### 3.1 The data flow

```
+-----------------+        +-----------------+
|  user keystroke | -----> | InsertCompletionState |
+-----------------+        +-----------------+
                                  |
                                  | trigger eval
                                  v
                          +---------------+
                          | source set    |
                          | (per-buffer)  |
                          +---------------+
                            |  |  |  ...
                  async async sync sync
                    |       |    |    |
                    v       v    v    v
               oneshot  oneshot live live
               LSP req  resolve buf  snip
                    \      |    /    /
                     \     |   /    /
                      v    v  v    v
                    +-----------------+
                    | aggregator       |  <-- de-dups,
                    +-----------------+      runs matcher,
                            |                 ranker, annotators
                            v                 every 16ms
                    +-----------------+
                    | popup renderer  |
                    +-----------------+
                            |
                          (focus)
                            |
                            v
                    +-----------------+
                    | doc popup       |  (lazy-resolve fires
                    +-----------------+   on focus stable)
```

### 3.2 New types

```rust
// lattice-completion (new module: `insert.rs`).

/// Live state for an in-flight Insert-mode completion. Held
/// on `App` while the popup is up; dropped on dismiss.
pub struct InsertCompletionState {
	/// The trigger that opened this completion (auto-trigger
	/// char, `<C-Space>`, etc.). Stays constant for the
	/// lifetime of the popup.
	pub trigger: CompletionTrigger,
	/// Anchor: where the replacement region starts. `cursor.byte`
	/// at popup-open. The region `anchor..cursor` is the
	/// "current word" the popup filters against.
	pub anchor: Position,
	/// Live filter text. Re-derived from
	/// `buffer[anchor..cursor]` on every keystroke; passed to
	/// the matcher.
	pub query: String,
	/// All raw candidates seen so far. Sources push into this
	/// asynchronously; aggregator re-runs matcher / ranker
	/// when new entries arrive (debounced to ~16ms / one frame).
	pub raw: Vec<RawCandidate>,
	/// Matched + scored + ranked + annotated. Re-derived from
	/// `raw` whenever `query` or `raw` changes.
	pub rendered: Vec<RenderedCandidate>,
	/// Selected index into `rendered`. Sticky across re-rank
	/// when the same candidate is still in the list.
	pub selected: usize,
	/// "Pinned" — user's first-non-default selection. After
	/// pinning, refilter doesn't reset to index 0.
	pub user_picked: bool,
	/// Per-source cancellation tokens.
	pub source_tokens: HashMap<SourceId, CancellationToken>,
	/// Documentation popup open + which candidate it's
	/// currently rendering for.
	pub doc_popup: Option<DocPopupState>,
	/// Whether the LSP source said `isIncomplete: true` -- if
	/// so, every keystroke re-fires LSP.
	pub lsp_incomplete: bool,
}

/// Why the popup is open.
pub enum CompletionTrigger {
	/// User typed a server-advertised character. `char` is
	/// the trigger; `request_kind` rides on the LSP request.
	TriggerChar(char),
	/// User typed an identifier character past the threshold
	/// (default: 2 chars).
	IdentifierThreshold,
	/// `<C-Space>` / `<C-x><C-o>` -- explicit user request.
	Manual,
	/// Server returned `isIncomplete: true` and the user kept
	/// typing.
	IncompleteRefresh,
}

/// Side popup showing detail + documentation for the focused
/// candidate. Lazy: doesn't open until requested.
pub struct DocPopupState {
	pub for_index: usize,
	/// Resolved markdown body. None means "we asked but the
	/// item has no docs"; `Option<String>::None` is distinct
	/// from "haven't asked yet" -- the latter doesn't open
	/// the popup at all.
	pub body: Option<String>,
	/// In-flight resolve token, if any.
	pub resolve_token: Option<CancellationToken>,
}
```

### 3.3 Source shape

Sources implement an async sibling of the existing
`CandidateGenerator`:

```rust
/// Insert-mode source. Mirror of `CandidateGenerator` but
/// async-first: sources may take a turn or many to produce
/// candidates. The aggregator pushes results onto the state's
/// `raw` vec as they arrive.
#[async_trait]
pub trait AsyncCandidateGenerator: Send + Sync {
	fn id(&self) -> SourceId;

	/// Default priority bucket. Higher buckets sort above
	/// lower; per-source priority can be overridden per-buffer
	/// via `:set completion.priority.<source>`.
	fn default_priority(&self) -> u32 { 100 }

	/// Whether this source is auto-triggered (typing alpha
	/// chars opens the popup with this source) or
	/// manual-only (`<C-Space>` / `<C-x><C-s>` etc.).
	fn auto_trigger(&self) -> bool { true }

	/// Server-advertised trigger characters that should fire
	/// the source. Empty = "fire on identifier-threshold or
	/// manual."
	fn trigger_chars(&self, ctx: &InsertContext<'_>) -> Vec<char> {
		Vec::new()
	}

	/// Produce candidates for the given context. The
	/// `tx` is the aggregator's mailbox -- sources push as
	/// many `RawCandidate`s as they like; the channel can be
	/// re-used across multiple async produce passes (e.g.
	/// LSP `isIncomplete` re-fetches).
	async fn produce(
		&self,
		ctx: InsertContext<'_>,
		tx: mpsc::UnboundedSender<RawCandidate>,
		token: CancellationToken,
	);
}
```

The synchronous `CandidateGenerator` stays where it is and
keeps powering cmdline completion; the new
`AsyncCandidateGenerator` is the Insert-mode peer. A
`SyncBridgeGenerator` wraps a sync generator into the async
trait so buffer-words / snippets / file paths don't pay
async overhead.

### 3.4 Sources at v1

| Source | Trait | Priority | Auto-trigger | Notes |
|---|---|---|---|---|
| `gen:lsp-completion` | async | 200 | yes | Per-server fan-out; merges + dedups (label, kind). Cancel on each keystroke; `isIncomplete` refresh. |
| `gen:snippet` | sync (bridged) | 150 | yes | Reads from `lattice-snippet` registry; matches on prefix + abbreviation. |
| `gen:buffer-words` | sync (bridged) | 100 | yes | Walks visible buffers' rope text; words >= 3 chars; deduped. |
| `gen:path` | sync (bridged) | 90 | trigger-char `'/'` in string contexts | Filesystem walk capped at 200 entries. |
| `gen:tree-sitter-symbol` | sync (bridged) | 80 | yes | Local symbols from the buffer's syntax tree (functions, vars in scope). |
| `gen:plugin-*` | async | as configured | as configured | Reserved for the WASM plugin host (Phase 7). |

### 3.5 Matcher

Default Insert-mode matcher: a fuzzy matcher that scores by

- Exact match (label == query, case-insensitive): top.
- Prefix match: high.
- camelCase / snake_case word-boundary match: medium-high.
- Substring match: medium.
- Subsequence (FZF-style "abc" matches `aXbYc`): lower.

The fuzzy matcher returns the byte ranges that "matched"; the
renderer paints those with the match face.

`completionItem.filterText` overrides the label for matching
when present (LSP convention: keep `label` pretty, match
against `filterText`).

`completionItem.sortText` is the server's stable sort key —
respected as a tiebreaker after our score, so two items with
equal fuzzy score keep the server's order.

### 3.6 Ranker

```
final_score = base_score
            + per_source_priority
            + frequency_bonus
            + preselect_bonus
            - deprecated_penalty
```

- `base_score`: from the matcher.
- `per_source_priority`: from `AsyncCandidateGenerator::default_priority`,
  overridable per-buffer.
- `frequency_bonus`: 0–50, based on how many times the user
  has accepted this exact `(label, kind)` pair in the
  current session. In-memory only for v1; persisted later.
- `preselect_bonus`: +200 when the LSP item carries
  `preselect: true`.
- `deprecated_penalty`: -100 when the LSP item carries the
  `Deprecated` tag. (Still shown, just sunk to the bottom.)

### 3.7 Aggregator

Single per-buffer task that owns the `mpsc::UnboundedReceiver`
the sources push into. On each receive (or on a 16ms tick if
no receive), it:

1. Appends new entries to `state.raw`.
2. Re-runs matcher + ranker against the live `query`.
3. Caps the result list at 200 (cheap for the renderer; longer
   lists provide diminishing returns).
4. Notifies the renderer (next frame picks up the new
   `state.rendered`).

Coalescing the run on a tick (rather than per-receive) keeps
the renderer from thrashing during a flurry of source pushes
(e.g. tree-sitter walk emitting 100 symbols at once).

---

## 4. Triggering

### 4.1 Auto-trigger rules

In Insert mode, after every text insertion:

1. **Trigger character.** If the inserted char is in any
   active source's `trigger_chars`, open the popup with
   `CompletionTrigger::TriggerChar(c)`.
2. **Identifier threshold.** Otherwise, if the inserted char
   is a word character AND the prefix
   `buffer[anchor..cursor]` length is `>= completion.min_chars`
   (default 2), open with `CompletionTrigger::IdentifierThreshold`.
3. **Otherwise.** No popup.

The popup, once open, stays open across non-trigger chars and
re-filters live until:

- The user types a non-word character that isn't a commit
  char → close.
- The user moves the cursor outside `[anchor, cursor]` →
  close.
- `<Esc>` → close popup AND exit Insert.
- `<C-e>` → close popup, stay in Insert.

### 4.2 Manual trigger

`<C-Space>` (default) or `<C-x><C-o>`:

- Always opens the popup, even if the cursor is on whitespace
  or zero typed chars.
- Sets `CompletionTrigger::Manual` -- sources that
  `auto_trigger() == false` participate too.
- The user can override the binding via `:keymap`.

### 4.3 isIncomplete refresh

When the LSP source returns `isIncomplete: true`, every
subsequent keystroke that mutates `query` re-fires the LSP
request. Without `isIncomplete`, the matcher filters
client-side over the last fetched set.

---

## 5. Display

### 5.1 Popup geometry

Anchored below the cursor at the start of `[anchor, cursor]`.
Falls back to above when there's no room below.

Width: capped at 60 cells (the popup itself), plus the
documentation popup when open (another 60 cells, anchored
right of the completion popup, or below if narrow screen).

Height: capped at 12 rows. Selected row sticks at the top
band; alternatives fan downward (closest to the cursor at
top, matching the picker's vertico convention).

### 5.2 Multi-column row layout

```
┌───────────────────────────────────────────────────────┐
│ ƒ  foo_bar                  fn(x: i32) -> Result   lsp│
│ ƒ  foo_baz                  fn(s: &str)             lsp│
│ v  foo_count                u64                     buf│
│ ✂  for_each                 [snippet]               snip│
│ T  Foo                      struct                  lsp│
└───────────────────────────────────────────────────────┘
```

- Column 1 (3 cells): kind glyph (ƒ / v / T / 🅢 / 🅔 / ✂ / · / etc.).
- Column 2 (≤ 30 cells): label, with match-face highlighting on
  the byte ranges the matcher consumed.
- Column 3 (≤ 22 cells): detail (one-liner: signature, type, etc.).
- Column 4 (3-4 cells): source tag (`lsp` / `buf` / `snip` /
  `path` / `ts` / `<plugin>`).

Truncation: each column gets a fixed budget; overflow
ellipsises with `…`. Width tracking:

- If the available popup width < 60, drop column 4 first,
  then shrink column 3 to its minimum (10 cells).
- If still too narrow, fall back to single-column (label
  only).

Deprecated items render with a strike-through face on the
label.

### 5.3 Documentation popup

Side popup showing the focused item's full documentation:

```
┌─ candidates ────────────────┐ ┌─ docs ───────────────────────┐
│ ƒ  foo_bar    fn(x:i32)…  lsp│ │ pub fn foo_bar(x: i32) -> Result│
│ ƒ  foo_baz    fn(s:&str)  lsp│ │                              │
│ v  foo_count  u64         buf│ │ Computes the bar of x.       │
│ ✂  for_each   [snippet]   snip│ │                              │
│ T  Foo        struct      lsp│ │ # Errors                     │
│                              │ │ Returns Err if x < 0.        │
└──────────────────────────────┘ └──────────────────────────────┘
```

Triggered by **`<C-d>`** (default) — toggles the doc popup
on / off. Once open, the body re-fetches (lazy resolve via
`completionItem/resolve` for LSP items that arrived
without `documentation`) every time the focused candidate
changes.

`<C-f>` / `<C-b>` scroll the doc popup contents when it has
focus; otherwise they continue to scroll the buffer behind.

Auto-show option: `:set completion.docs_auto=true` opens
the doc popup whenever the candidate selection changes.
Default false to keep the popup compact for fast typing
flows.

### 5.4 Ghost text (deferred)

A future polish item: render the would-be insertion as
ghost text (dim, no underline) past the cursor for the
top-ranked or selected candidate. Useful for AI-source
multi-line proposals. Out of scope for v1; the popup is the
primary surface.

---

## 6. Keystroke model

Default Insert-mode keymap with the popup open:

| Chord | Action |
|---|---|
| `<C-n>` / `<Down>` | Select next |
| `<C-p>` / `<Up>` | Select previous |
| `<C-y>` | Accept selected |
| `<Tab>` | Accept; if accepted item is a snippet, jump to first placeholder |
| `<CR>` | Accept; if no selection, insert newline (vim default) |
| `<C-e>` | Cancel popup, keep typing |
| `<C-Space>` / `<C-x><C-o>` | Re-trigger / cycle next |
| `<C-x><C-p>` | Cycle previous |
| `<C-d>` | Toggle doc popup |
| `<C-f>` / `<C-b>` | Scroll doc popup (when open) |
| `<commit-char>` | Auto-accept current item, then insert the char |

Every binding above is registered through the existing
`keymap` registry so users can rebind via the standard
`:keymap insert <chord> ex:<command>` interface.

Vim purists who want pure `<C-x><C-o>` etc. without
auto-trigger get it via `:set completion.auto=false`.

`<Esc>` closes the popup AND exits Insert (vim semantics).
The popup-close-only escape hatch is `<C-e>`.

### 6.1 Commit characters

LSP items may carry `commitCharacters: Vec<String>` (each
string is a single char). Typing one auto-accepts the
selected item then inserts the typed char. Example:
typing `(` after `foo_bar` accepts `foo_bar` then inserts
`(`, taking you straight to the function-call site.

When no LSP item supplies `commitCharacters`, no chars
auto-commit; the user accepts explicitly. Configurable
default extras via `:set completion.extra_commit_chars=";.()"`
for users who want vim-style behaviour.

### 6.2 Snippet placeholder navigation

When the accepted item is a snippet (kind `Snippet` or
`insertTextFormat == Snippet`), the snippet body is parsed
and inserted with placeholders. After insertion, Insert
mode enters a sub-state where:

| Chord | Action |
|---|---|
| `<Tab>` | Jump to next placeholder |
| `<S-Tab>` | Jump to previous placeholder |
| `<Esc>` | Exit snippet mode (placeholders become plain text) |
| any motion / edit | Updates the active placeholder; mirrored into all references |

Placeholders ($1, $2, …, $0) navigate in numeric order;
`$0` is the final cursor position, exited automatically.

Choice placeholders (`${1|a,b,c|}`) open a mini-picker
inline.

Transformation placeholders (`${1/pat/repl/}`) re-evaluate
the regex on each character of the active placeholder.

---

## 7. LSP source specifics

### 7.1 Request shape

`textDocument/completion` with:

- `position` at the cursor.
- `context.triggerKind`:
  - `Invoked` for manual / identifier-threshold triggers.
  - `TriggerCharacter` for trigger chars (with the char in
    `triggerCharacter`).
  - `TriggerForIncompleteCompletions` for isIncomplete
    refreshes.

### 7.2 Item handling

Per item:

1. **Filter text.** Use `filterText` for matcher input;
   fall back to `label` if absent.
2. **Replace range.** From `textEdit.range` when present;
   else heuristic word-boundary scan from the cursor.
3. **Insert text.** From `textEdit.newText` or `insertText`
   or `label`, in that order.
4. **Insert format.** `Snippet` items go through the
   snippet engine; `PlainText` items splice as-is.
5. **Insert mode.** `AdjustIndentation` re-indents the
   inserted region to the current line's indent;
   `AsIs` doesn't.
6. **Sort text.** `sortText` rides the candidate as a
   tiebreaker for the ranker.
7. **Tags.** `[Deprecated]` → strikethrough +
   ranker penalty.
8. **Preselect.** Bumps the item to the initial selection.
9. **Commit characters.** Ride the candidate; popup
   accept-on-typed-commit-char path consumes them.

### 7.3 Lazy resolve

When the user focuses an item that arrived without
`documentation`, fire `completionItem/resolve` to fill in
the missing fields. Cancellable via the doc-popup's token.

### 7.4 Additional text edits

Items with `additionalTextEdits` (commonly auto-imports)
apply those edits **as part of the same undo unit** as the
main insert. Order:

1. Apply `additionalTextEdits` first (ascending start
   position).
2. Apply the main insert.

This way `<C-z>` undoes the import alongside the
insertion.

### 7.5 Cancellation discipline

- Each new keystroke cancels the prior in-flight LSP
  request unless it carries `isIncomplete: false` (server-
  cached set; client-side filter sufficient).
- Exiting Insert mode cancels all in-flight requests.
- Picking a candidate cancels the pending resolve of any
  other candidate.

---

## 8. Snippet engine

A new `lattice-snippet` crate with:

- **Parser.** TextMate / LSP snippet syntax (`$0`, `$1`,
  `${2:placeholder}`, `${3|a,b,c|}`, `${4/pat/repl/}`,
  `\$`, `\\`, variables `$TM_FILENAME` etc.).
- **Body type.** `Snippet { tokens: Vec<SnippetToken> }`.
- **Renderer.** Walks tokens producing the inserted text +
  the per-placeholder ranges to track post-insert.
- **Variables.** Built-ins:
  - `$TM_SELECTED_TEXT` — last visual selection.
  - `$TM_CURRENT_LINE` — current line text.
  - `$TM_CURRENT_WORD` — word under cursor.
  - `$TM_FILENAME`, `$TM_FILEPATH`, `$TM_DIRECTORY`.
  - `$CLIPBOARD` — current `"+` register.
  - `$WORKSPACE_NAME`, `$WORKSPACE_FOLDER`.
  - `$CURRENT_YEAR`, `$CURRENT_MONTH`, `$CURRENT_DATE`,
    `$CURRENT_HOUR`, `$CURRENT_MINUTE`, etc.
  - `$RANDOM`, `$UUID`.
  - `$LINE_COMMENT`, `$BLOCK_COMMENT_START`,
    `$BLOCK_COMMENT_END` — from the major mode's
    `comment_string` table (Phase 8).

- **Custom snippet registry** at
  `~/.config/lattice/snippets/<lang>.json` (textmate
  format) + per-project `.lattice/snippets/<lang>.json`.

- **LSP-supplied snippets** flow through the same engine;
  the LSP source flags items `insertTextFormat == Snippet`
  and the accept path routes them through the engine
  rather than `apply_lsp_completion_item`'s plain insert.

---

## 9. Configuration surface

```toml
[completion]
auto                 = true   # auto-trigger on identifier threshold
min_chars            = 2      # for identifier-threshold trigger
debounce_ms          = 50     # per-keystroke debounce
docs_auto            = false  # auto-show docs popup on selection
extra_commit_chars   = ""     # editor-side commit chars (server's union with this)
auto_insert_single   = false  # auto-accept when only one match remains
fuzzy_threshold      = 5      # min match score to render

[completion.source.lsp]
enabled    = true
priority   = 200

[completion.source.snippet]
enabled    = true
priority   = 150

[completion.source.buffer-words]
enabled    = true
priority   = 100
min_word_length = 3

[completion.source.path]
enabled    = true
priority   = 90

[completion.source.tree-sitter]
enabled    = true
priority   = 80

[completion.per-language.markdown]
sources    = ["snippet", "buffer-words", "path"]   # no LSP / ts for prose

[completion.per-language.rust]
auto_insert_single = true   # rust users like `single-match-auto-accept`
```

Per-buffer override path: `:setlocal completion.auto=false`
(once `setlocal` lands; today only the global form works).

The plugin host eventually exposes
`completion.source.<plugin-id>` keys for plugin-supplied
sources.

---

## 10. Implementation plan

### Phase 4.2.g.1 — Insert-mode shell + sync sources

Goal: `<C-Space>` opens a popup with buffer-words completion;
`<Tab>` / `<C-y>` accept; `<C-n>` / `<C-p>` navigate;
`<Esc>` / `<C-e>` dismiss. No LSP, no async.

Deliverables:

- `lattice-completion::insert` module:
  `InsertCompletionState`, `CompletionTrigger`,
  `AsyncCandidateGenerator` trait,
  `SyncBridgeGenerator`.
- New buffer-words sync generator
  `gen:buffer-words` -- walks active buffer's rope text
  (visible-region first, then off-screen) for words with
  the configured min length.
- New fuzzy matcher
  `match:fuzzy-insert`.
- `App::insert_completion: Option<InsertCompletionState>`.
- Insert-mode key bindings: `<C-Space>` opens; `<C-n>` /
  `<C-p>` / `<Tab>` / `<CR>` / `<C-y>` / `<C-e>` /
  `<Esc>` route through the popup.
- Renderer: completion popup widget anchored at cursor;
  multi-column rendering with kind glyph + label + source
  tag (no detail / docs popup yet).
- Tests: trigger / dismiss / accept / refilter on
  keystroke; snapshot tests for renderer columns.

Done when: `<C-Space>` shows buffer words and `<C-y>`
inserts the chosen one.

### Phase 4.2.g.2 — LSP source

Goal: `gen:lsp-completion` async generator pushes items as
they arrive; the popup reflects them merged with
buffer-words. Replaces the `:complete` picker bridge.

Deliverables:

- `lattice-ui-tui::completion_lsp` module: implements
  `AsyncCandidateGenerator` over `ServerHandle::completion`.
  Cancellation via the existing token plumbing; per-server
  fan-out + dedup by `(label, kind)`.
- `isIncomplete` refresh path -- the aggregator re-fires
  the LSP source on every keystroke when the last response
  was incomplete.
- Trigger-character detection: walk attached servers'
  `completionProvider.triggerCharacters`, union with
  `completion.extra_commit_chars`.
- Item adaptation: `filterText` / `sortText` /
  `preselect` / `tags[Deprecated]` / commit characters;
  `textEdit.range` overrides anchor.
- Auto-trigger on identifier threshold (`min_chars`);
  manual trigger as before.
- Phase out the `:complete` ex command (the inline popup
  replaces it).

Done when: typing `foo.` in a `.rs` buffer pops up
rust-analyzer's completions inline.

### Phase 4.2.g.3 — Documentation side popup

Goal: `<C-d>` toggles a side popup showing the focused
candidate's full documentation; `completionItem/resolve`
fires lazily.

Deliverables:

- `DocPopupState` + render path right of (or below) the
  completion popup.
- LSP `completionItem/resolve` integration: fires when the
  focused candidate's `documentation` is missing; result
  feeds `DocPopupState.body`.
- Existing markdown-render path (the hover popup's renderer)
  reused for the doc popup body.
- Auto-show option: `:set completion.docs_auto=true`.
- Tests: focus toggles popup; resolve fills body once;
  cancellation drops the popup on dismiss.

Done when: `<C-d>` on a function candidate shows its
signature + doc comment; `<C-f>` scrolls.

### Phase 4.2.g.4 — Snippet engine

Goal: snippet items expand with placeholder navigation;
LSP items with `insertTextFormat == Snippet` route through
the engine.

Deliverables:

- New `lattice-snippet` crate:
  parser + body type + renderer + variable resolver.
- Insert-mode sub-state for placeholder navigation:
  `<Tab>` next, `<S-Tab>` prev, `<Esc>` exit. Cursor jumps
  apply the same `Position`-tracking the buffer-words path
  uses; mirrored placeholders re-render on edit.
- Snippet registry: load `~/.config/lattice/snippets/<lang>.json`
  at startup; `:reload-snippets` re-reads.
- LSP item routing: items flagged `Snippet` go through the
  engine; plain-text items use the existing splice path.
- `gen:snippet` source fed by the registry.
- Choice placeholders inline-picker (mini-picker over the
  choices); transformation placeholders re-evaluate per-
  edit.
- Tests: textmate parse round-trip; placeholder navigation;
  variable substitution; LSP-snippet end-to-end (mock
  server).

Done when: typing `for<C-Space>` and accepting expands
`for x in y { z }` with placeholder hops on `<Tab>`.

### Phase 4.2.g.5 — Frequency ranking + per-source priority

Goal: items the user has accepted recently bubble to the
top; per-source priority is configurable.

Deliverables:

- In-memory `(label, kind) -> count` map on the App,
  bumped per accept.
- Ranker reads the map; bonus capped at +50.
- Per-source priority override via `:set
  completion.source.<id>.priority=<n>`.
- Per-language source filter:
  `[completion.per-language.<lang>]
  sources = ["snippet", "buffer-words", "lsp"]`.
- Tests: accepted item ranks above peers next time.

### Phase 4.2.g.6 — Tree-sitter + path sources

Goal: ship the local-symbol and path sources.

Deliverables:

- `gen:tree-sitter-symbol`: query the buffer's syntax tree
  for in-scope identifiers (function defs, let-bindings,
  closure params).
- `gen:path`: triggered by `'/'` inside a string literal
  (detected via tree-sitter scope query); filesystem walk
  capped at 200 entries; respects `.gitignore`.

### Phase 4.2.g.7 — Polish

- Ghost text for the top-ranked item (optional, off by
  default).
- Auto-insert single match (`completion.auto_insert_single`).
- `additionalTextEdits` apply path coalesces with the main
  insert into one undo unit (refactor existing
  `apply_lsp_completion_item`).
- Postfix-completion seam (deferred to plugin host).
- Persistent frequency tracking (post-1.0; needs privacy
  story).

---

## 11. Performance commitments

| Path | Budget | Notes |
|---|---|---|
| Trigger evaluation per keystroke | < 100 µs | sync; no allocation past the 1-char compare |
| Sync source produce | < 1 ms | bounded by source-supplied cap (200 entries) |
| Async source dispatch (LSP request kicked off) | < 2 ms | cancel + new request build |
| Aggregator tick (re-rank after a push) | < 5 ms for 200 items | matcher + ranker + annotate |
| Popup render | < 4 ms | per-frame budget |
| Snippet expansion | < 2 ms | parser is regex-light + linear walk |
| `completionItem/resolve` round-trip | bounded by LSP latency | non-blocking; drain + re-render |

These line up with the editor's overall keystroke-to-glyph
budget (8 ms at 120 Hz / 16 ms at 60 Hz, CLAUDE.md goal #1).

---

## 12. Open questions

1. **Snippet placeholder mirroring with concurrent edits.**
   What if the user types in one mirror and a buffer event
   from another source (LSP didChange) lands at the same
   place? v1: ignore (mirrors are fully client-side); the
   server's didChange covers our insert anyway.

2. **isIncomplete + manual trigger.** When the LSP source
   is `isIncomplete`, manual `<C-Space>` should re-fire?
   Probably yes — manual is "show me the latest." Verify
   against rust-analyzer's behaviour before final.

3. **Multi-server LSP completion ordering.** Two servers
   on a `.cpp` file (clangd + a linter bridge) both return
   items. Order: union, dedup by `(label, kind)`, sort by
   ranker. Per-server priority breaks ties. Verify the
   architecture doc's "score-merging" wording matches this.

4. **Plugin generator security.** WASM plugins must
   honour `token.is_cancelled()`; a misbehaving plugin
   that ignores cancellation can starve the aggregator.
   Mitigation: aggregator drops slow-source pushes after
   a deadline (e.g. 500 ms past the keystroke that
   triggered them).

5. **Documentation popup width when wrapping is on.**
   The doc body is markdown; long lines wrap. Cap at 60
   cells and let wrap handle overflow. Detail (column 3
   in the main popup) doesn't wrap — it's a one-liner.

6. **Auto-trigger off in comments / strings?** Some users
   want LSP completion suppressed inside string literals.
   Tree-sitter scope query on the cursor position + a
   `completion.suppress_in = ["string", "comment"]`
   option. Default empty (suppress nothing).

---

## 13. Cross-references

- DESIGN.md §5.11.3 — completion pipeline (cmdline today;
  Insert-mode peer formalised by this doc).
- DESIGN.md §5.9.10 — rich minibuffer (the picker UX this
  doc's popup descends from).
- [`lsp-architecture.md`](lsp-architecture.md) §10 — LSP
  request fan-out, cancellation tokens.
- [`crates/lattice-completion/`](../crates/lattice-completion/)
  — current pipeline traits; `insert.rs` is the new module
  this doc adds.
- `lattice-snippet` (new crate, 4.2.g.4) — engine + parser.
- `help/lsp.md` — user-facing LSP completion blurb (will
  pivot from `:complete` picker to inline popup once
  4.2.g.2 lands).

