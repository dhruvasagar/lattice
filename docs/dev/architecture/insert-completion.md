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
5. **Vim-grammar compatible, command-first.** Bindings lean
   vim — `<C-x>`-prefixed family, `<C-n>` / `<C-p>` for
   next / prev, `<C-y>` to accept, `<C-e>` to dismiss. Every
   action is also a registered command in `CommandRegistry`,
   reachable from `:` and from the API. Bindings are the
   convenience layer over the canonical command surface
   (design.md §5.2.1).
6. **Snippet engine that ships with the snippet ecosystem.**
   TextMate-format JSON, drop-in compatible with VS Code's
   snippet repositories (and thus with the
   `friendly-snippets` corpus most editors already pull
   from). Snippets are one source in the unified popup, with
   a dedicated chord (`<C-x><C-s>`) for snippet-only
   expansion when the user wants to skip the popup.
7. **Manual trigger by default.** Auto-trigger is opt-in via
   configuration. The default user model is "I press a key
   to ask for completion," matching vim's `<C-x><C-o>` /
   `<C-n>` heritage and avoiding noisy popups during prose
   typing. The minute-one experience is predictable.
8. **Comprehensive help.** Every option, every source, every
   binding ships with a `:help completion-*` topic at the
   same time the feature lands. The help is part of the
   feature, not a follow-up.

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
| VS Code | trigger chars + alpha + `<C-Space>` | LSP + snippets + word + plugin | fuzzy (FZF) | side panel, lazy-resolve | TextMate, full | "suggestion mode" toggle (insert vs replace), commit chars |
| Neovim (`blink.cmp` / `nvim-cmp`) | per-source debounce | LSP + snippets + buffer + path + tree-sitter + LSP-snippet + AI | per-source matcher | side popup, configurable | LuaSnip / vsnip (TextMate) | per-source priority + `min_keyword_length`, ghost text |
| Helix | trigger chars + alpha | LSP + snippets + word + path | fuzzy (FZF) | side popup | TextMate | async pipeline; preselect honoured |
| JetBrains | trigger + `<C-Space>` (basic) / `<C-S-Space>` (smart) | LSP + LiveTemplates + word + DB-aware | substring + camelCase | popup right (`<C-Q>`) | LiveTemplates | statistics-based ranking, postfix templates, type-aware filter |
| Sublime | alpha auto | snippets + word | fuzzy | n/a | TextMate | one of the original "fast feel" benchmarks |
| Emacs (`corfu` + `cape`) | trigger + alpha | per-major-mode generators | orderless (multi-substring) | side popup or echo area | yasnippet | great composability — every mode plugs its own generators |

Common patterns lattice ships:

- **Manual trigger first; auto-trigger as opt-in.** vim's
  default heritage; users who want VS Code-style live
  popups flip a config knob.
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
	/// Auto-trigger: identifier-character threshold reached
	/// (default off; opt-in via `completion.auto_trigger`).
	IdentifierThreshold,
	/// Manual: user pressed `<C-x><C-o>` / `<C-Space>` /
	/// `<Tab>` (smart-tab) / called `cmd:completion-trigger`.
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
| `gen:snippet` | sync (bridged) | 150 | yes | Reads from `lattice-snippet` registry; matches on prefix + abbreviation. Snippets are one source in the unified popup. |
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

### 3.8 Completion-popup minor mode

While the popup is open, lattice activates a transient
**completion-popup minor mode**: a thin keymap layer above the
existing Insert-mode bindings. Bindings inside this layer
take effect *only* when `App.insert_completion.is_some()`.
Closing the popup deactivates the layer; the original Insert
bindings (and Normal-mode bindings post-`<Esc>`) restore
unchanged.

This is the architectural answer to "what happens when
`<C-d>` means two things?" (popup-docs-toggle vs Normal-mode
half-page-down vs Insert-mode shift-left-indent). The minor
mode owns those keys for the duration of the popup; the rest
of the editor never sees them.

The same pattern handles snippet placeholder navigation: an
**active-snippet minor mode** is layered above Insert mode
while a snippet expansion has live placeholders, and over-
rides `<Tab>` / `<S-Tab>` to step through placeholders. When
the user exits the snippet (`$0` reached, `<Esc>` pressed,
or cursor moved outside placeholder range), the minor mode
deactivates and `<Tab>` falls back to its Insert-mode
default.

Implementation sketch: lattice's input layer already routes
through `translate(ctx, event)` with `ctx` carrying mode +
pending state. We add a `MinorModeLayer` consult before the
mode dispatch:

```rust
fn translate(ctx: &TranslateContext<'_>, event: KeyEvent) -> Action {
	if ctx.completion_popup_open {
		if let Some(action) = COMPLETION_POPUP_LAYER.lookup(event) {
			return action;
		}
	}
	if ctx.snippet_active {
		if let Some(action) = SNIPPET_LAYER.lookup(event) {
			return action;
		}
	}
	// Fallback to mode-specific translation (Insert / Normal /
	// Visual / etc.).
	default_translate(ctx, event)
}
```

The layer tables live in `lattice-ui-tui::keymap` next to the
main keymap; users can override them via the standard
`:keymap completion-popup <chord> <ex-command>` /
`:keymap snippet <chord> <ex-command>` commands once the
keymap registry grows the per-layer surface (queued with the
keymap.rs cleanup in Phase 4.2.g.1).

---

## 4. Triggering

### 4.1 Default: manual trigger only

Out of the box, the popup opens **only** when the user asks
for it. Three paths trigger:

- `<C-x><C-o>` — vim's standard "omni-completion" chord. The
  vim-native muscle memory.
- `<C-Space>` — modern-editor muscle memory (VS Code,
  IntelliJ, Helix, etc.).
- `<Tab>` smart-tab — when the cursor is right after a word
  character, `<Tab>` triggers completion. When the cursor
  is on whitespace / start-of-line, `<Tab>` inserts a tab
  literal (vim's default Insert-mode `<Tab>` behaviour).
  Smart-tab keeps the literal-tab use case intact while
  giving "type some letters then `<Tab>`" the obvious
  meaning.

All three resolve to the same command:
`cmd:completion-trigger` with `manual = true`. Users can
rebind freely; the chord is the convenience layer.

### 4.2 Auto-trigger (opt-in)

When `completion.auto_trigger = true`:

1. **Trigger character.** If the inserted char is in any
   active source's `trigger_chars`, open the popup with
   `CompletionTrigger::TriggerChar(c)`.
2. **Identifier threshold.** Otherwise, if the inserted char
   is a word character AND the prefix
   `buffer[anchor..cursor]` length is `>= completion.min_chars`
   (default 2), open with
   `CompletionTrigger::IdentifierThreshold`.
3. **Otherwise.** No popup.

Auto-trigger can be enabled per-language via
`[completion.per-language.<lang>] auto_trigger = true` for
users who want noise-free prose typing but live completion
in code buffers.

### 4.3 Once-open behaviour

The popup, once open, stays open across non-trigger chars and
re-filters live until:

- The user types a non-word character that isn't a commit
  char → close.
- The user moves the cursor outside `[anchor, cursor]` →
  close.
- `<Esc>` → close popup AND exit Insert (vim semantics).
- `<C-e>` → close popup, stay in Insert (vim semantics).

### 4.4 isIncomplete refresh

When the LSP source returns `isIncomplete: true`, every
subsequent keystroke that mutates `query` re-fires the LSP
request. Without `isIncomplete`, the matcher filters
client-side over the last fetched set.

Manual `<C-x><C-o>` / `<C-Space>` always re-fires the LSP
source regardless of `isIncomplete` — manual trigger means
"give me the freshest set."

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

Triggered by **`<C-d>`** — toggles the doc popup on / off.
The chord is bound *only* in the completion-popup minor mode
(§3.8), so Normal-mode `<C-d>` (half-page-down) and Insert-
mode `<C-d>` (shift-left-indent) keep their original
meanings as soon as the popup closes.

`<C-f>` / `<C-b>` scroll the doc popup contents when it's
open and focused; otherwise they fall through to their
Insert-mode defaults.

Auto-show option: `completion.docs_auto = true` opens the
doc popup whenever the candidate selection changes. Default
false to keep the popup compact for fast typing flows.

### 5.4 Ghost text (deferred)

A future polish item: render the would-be insertion as
ghost text (dim, no underline) past the cursor for the
top-ranked or selected candidate. Useful for AI-source
multi-line proposals. Out of scope for v1; the popup is the
primary surface.

---

## 6. Keystroke model

### 6.1 Default keymap (completion-popup minor mode)

Active *only* while the completion popup is open. All bindings
resolve to ex-commands; users override via
`:keymap completion-popup <chord> ex:<command>`.

| Chord | Ex-command | Behaviour |
|---|---|---|
| `<C-n>` / `<Down>` | `ex:completion-next` | Select next candidate |
| `<C-p>` / `<Up>` | `ex:completion-prev` | Select previous candidate |
| `<C-y>` | `ex:completion-accept` | Accept current selection (vim convention) |
| `<Tab>` | `ex:completion-accept` | Accept (also; smart-tab open path becomes accept once popup is up) |
| `<CR>` | `ex:completion-accept` | Accept; if no selection committed yet, falls through to newline |
| `<C-e>` | `ex:completion-cancel` | Close popup, stay in Insert (vim convention) |
| `<Esc>` | `ex:completion-cancel-and-exit-insert` | Close popup AND exit Insert (vim convention) |
| `<C-x><C-o>` / `<C-Space>` | `ex:completion-trigger` | Re-trigger / refresh (`isIncomplete` re-fires) |
| `<C-x><C-p>` | `ex:completion-prev` | Cycle previous (vim's omni-cycle-back convention) |
| `<C-d>` | `ex:completion-toggle-docs` | Toggle docs side popup |
| `<C-f>` (when docs open) | `ex:completion-scroll-docs-down` | Page docs down |
| `<C-b>` (when docs open) | `ex:completion-scroll-docs-up` | Page docs up |
| any commit-char (LSP-supplied) | `ex:completion-accept-then-insert` | Auto-accept current item, then insert the typed char |

### 6.2 Default keymap (Insert mode, popup closed)

| Chord | Ex-command | Behaviour |
|---|---|---|
| `<C-x><C-o>` | `ex:completion-trigger` | Manual trigger |
| `<C-Space>` | `ex:completion-trigger` | Manual trigger (alternate) |
| `<Tab>` | smart-tab | Trigger if cursor is right after a word char; insert tab otherwise |
| `<C-x><C-s>` | `ex:snippet-expand` | Snippet-only expansion (skips the popup; expands a snippet matching the word at cursor, if any) |

### 6.3 Default keymap (active-snippet minor mode)

Active when a snippet expansion has live placeholders.

| Chord | Ex-command | Behaviour |
|---|---|---|
| `<Tab>` | `ex:snippet-next-placeholder` | Jump to next `$N` |
| `<S-Tab>` | `ex:snippet-prev-placeholder` | Jump to previous `$N` |
| `<C-l>` | `ex:snippet-next-placeholder` | Alternate (consistent with completion `<C-x><C-o>` family) |
| `<C-h>` | `ex:snippet-prev-placeholder` | Alternate |
| `<Esc>` | `ex:snippet-leave` | Exit snippet mode (placeholders become plain text); also exits Insert by vim convention |

When a snippet is active AND the completion popup is open
(e.g. user typed inside a placeholder), the completion-popup
layer wins for `<Tab>` / `<C-n>` etc. -- the popup is the
foreground UI. `<Esc>` cascades: closes popup → exits
snippet → exits Insert.

### 6.4 Picker / completion consistency

Lattice's vertico picker uses `<C-n>` / `<C-p>` for
navigation already. The completion popup follows the same
convention: **`<C-n>` / `<C-p>` are the cross-feature next /
prev keys**, in pickers and the completion popup. `<Tab>` is
also bound for completion specifically (no conflict — pickers
don't bind `<Tab>` for navigation).

This way users learn one navigation pattern and it works
across every list-shaped surface.

### 6.5 Command surface (every binding has a command)

Every action above is registered as an ex-command in the
unified `CommandRegistry` (design.md §5.2.1). Users can
invoke any of them from `:`, from a macro, from a plugin,
or from `init.rs`:

| Ex-command | Surface form | Notes |
|---|---|---|
| `ex:completion-trigger` | `:complete` (renamed from the picker bridge once 4.2.g.2 ships) | `bang!` form forces refresh past `isIncomplete` cache |
| `ex:completion-next` | `:complete-next` | |
| `ex:completion-prev` | `:complete-prev` | |
| `ex:completion-accept` | `:complete-accept` | |
| `ex:completion-cancel` | `:complete-cancel` | |
| `ex:completion-cancel-and-exit-insert` | `:complete-cancel!` | Bang variant; same semantics as `<Esc>` shortcut |
| `ex:completion-toggle-docs` | `:complete-docs` | |
| `ex:completion-scroll-docs-down` | `:complete-docs-down` | |
| `ex:completion-scroll-docs-up` | `:complete-docs-up` | |
| `ex:completion-accept-then-insert` | n/a (commit-char internal; commands surface accept then `feedkey` separately) | |
| `ex:snippet-expand` | `:snippet-expand` | Optional `<word>` arg; without, expands the snippet at cursor |
| `ex:snippet-next-placeholder` | `:snippet-next` | |
| `ex:snippet-prev-placeholder` | `:snippet-prev` | |
| `ex:snippet-leave` | `:snippet-leave` | |

Plugin-supplied sources register their own commands (e.g.
`:complete-source-toggle <id>`) via the standard registry
APIs.

### 6.6 Keymap query surface

`:describe-key <chord>` already resolves to the bound
command; the same path works inside the minor modes when
the user passes the layer name:

```
:describe-key completion-popup <C-d>
```

→ "Toggle docs side popup (`ex:completion-toggle-docs`)."

Help cross-link from `:help completion-keymap` lists every
binding with its command + the layer it belongs to.

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

### 8.1 Format: TextMate JSON (drop-in compatible)

A new `lattice-snippet` crate parses **TextMate-format JSON**,
the format VS Code uses. This is the dominant format in the
ecosystem — every modern editor reads it, every snippet
repository ships in it. Critically, lattice can drop in the
`friendly-snippets` corpus (a 30+-language collection of
hand-curated snippets pulled from VS Code marketplace
packages, used by neovim's LuaSnip, vim-vsnip, and others)
with **no conversion**.

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
	},
	"impl Display": {
		"prefix": ["impl_display", "displ"],
		"body": [
			"impl ::std::fmt::Display for ${1:Ty} {",
			"\tfn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {",
			"\t\twrite!(f, \"${2:fmt}\"$0)",
			"\t}",
			"}"
		]
	}
}
```

Top-level keys are snippet names (free-form). Each value
carries:

- `prefix`: string or array of strings — what the user types
  to trigger.
- `body`: string or array of lines (joined with `\n`).
- `description`: optional one-liner shown in the popup's
  detail column.
- `scope`: optional comma-separated list of source scopes
  (`source.rust,source.markdown.injection.rust`) the
  snippet applies in. v1 maps `source.<name>` to the
  buffer's tree-sitter language; richer scope expressions
  land later.

### 8.2 Body syntax (TextMate / LSP-compatible)

The body uses the same syntax LSP servers emit and VS Code
parses:

| Construct | Meaning |
|---|---|
| `$1`, `$2`, ..., `$0` | Tab-stops. `$0` is the final cursor position, exited automatically. |
| `${1:placeholder}` | Tab-stop with default text. |
| `${1\|opt1,opt2,opt3\|}` | Choice placeholder — opens a mini-picker over the options. |
| `${1/pattern/replacement/flags}` | Transformation — regex against the placeholder's current text. |
| `\$`, `\\`, `\}` | Escapes. |
| `$VAR_NAME`, `${VAR_NAME}` | Variable substitution. |
| `${VAR_NAME:default}` | Variable with fallback if undefined. |
| `${VAR_NAME/pat/repl/flags}` | Variable with regex transform. |

Built-in variables (TextMate / VS Code parity):

| Variable | Value |
|---|---|
| `TM_SELECTED_TEXT` | Last visual selection |
| `TM_CURRENT_LINE` | Current line text |
| `TM_CURRENT_WORD` | Word under cursor |
| `TM_LINE_INDEX` | 0-based current line |
| `TM_LINE_NUMBER` | 1-based current line |
| `TM_FILENAME` | Current filename |
| `TM_FILENAME_BASE` | Filename without extension |
| `TM_DIRECTORY` | Containing directory (absolute) |
| `TM_FILEPATH` | Full path |
| `WORKSPACE_NAME`, `WORKSPACE_FOLDER` | Workspace root info |
| `CLIPBOARD` | Current `"+` register |
| `CURRENT_YEAR`, `CURRENT_MONTH`, `CURRENT_DATE`, `CURRENT_DAY_NAME`, `CURRENT_HOUR`, `CURRENT_MINUTE`, `CURRENT_SECOND`, etc. | Timestamps |
| `RANDOM`, `RANDOM_HEX` | Random ints |
| `UUID` | Random UUID |
| `LINE_COMMENT`, `BLOCK_COMMENT_START`, `BLOCK_COMMENT_END` | Per-major-mode comment strings (Phase 8) |

### 8.3 Loading paths

Loaded at startup; `:reload-snippets` re-reads on demand.

Lookup order (later overrides earlier):

1. **Bundled.** Shipped with the lattice binary (a small
   curated set in `bundled-snippets/`).
2. **friendly-snippets-style packages.** Any directory in
   `~/.config/lattice/snippet-packs/` — typically
   `~/.config/lattice/snippet-packs/friendly-snippets/`
   cloned from upstream.
3. **User snippets.** `~/.config/lattice/snippets/<lang>.json`.
4. **Project snippets.** `.lattice/snippets/<lang>.json` in
   the workspace root.

Per-language registry: each language indexes by prefix; the
`gen:snippet` source filters by the active buffer's
language. Multi-prefix snippets register under each prefix.

### 8.4 Crate structure

```
lattice-snippet/
├── src/
│   ├── lib.rs           — public re-exports
│   ├── parse.rs         — TextMate body parser
│   ├── token.rs         — `Snippet`, `SnippetToken`, `Placeholder`
│   ├── render.rs        — token walk → inserted text + placeholder ranges
│   ├── variables.rs     — built-in variable resolver
│   ├── registry.rs      — per-language snippet store + loader
│   └── load.rs          — JSON ingestion (TextMate format)
├── tests/
│   ├── parse.rs         — body-syntax round-trip
│   ├── friendly_snippets.rs   — load a slice of friendly-snippets, assert parses
│   └── render.rs        — placeholder navigation, transforms
└── bundled-snippets/    — shipped corpus
```

### 8.5 Active-snippet state

When a snippet expands:

```rust
pub struct ActiveSnippet {
	/// Buffer position the snippet was inserted at.
	pub origin: Position,
	/// Per-tabstop ranges in the buffer; mirrored entries
	/// share an index so editing one updates all.
	pub tabstops: Vec<TabstopGroup>,
	/// Currently focused tabstop index (1-based; `$0` is the
	/// final position).
	pub current: usize,
	/// `$N -> ranges` map for fast jump.
	pub by_index: HashMap<u32, Vec<Range>>,
}
```

Edits inside a tabstop range shift the matching ranges in
its mirror group; ranges in adjacent tabstops shift too as
the buffer text grows. Buffer events from outside the
snippet (LSP didChange, format-on-save) flow through
unchanged — the snippet ranges adjust via the same
position-update path the rest of the editor uses.

Exit triggers:

- `$0` reached.
- `<Esc>` (also exits Insert, vim convention).
- Cursor moved outside any tabstop range.
- New snippet expanded (the prior snippet exits before the
  new one begins).

### 8.6 Snippets in the unified popup

The user's primary path is the popup: trigger completion,
type a few characters, see snippets alongside LSP / buffer-
words / etc. results, accept any of them. The
`gen:snippet` source produces candidates whose
`CandidateData` carries the snippet body; on accept, the
host routes the candidate through the snippet engine
instead of the plain insert path.

The dedicated `<C-x><C-s>` chord (`:snippet-expand`) is the
escape hatch for "I know the snippet's prefix; expand it
without seeing the popup." It looks up the prefix at cursor
in the registry, expands directly, and skips the popup
entirely.

---

## 9. Configuration surface

Every option is a typed `lattice-config` registration with
help text the user can read via
`:help completion-options` (an alias of
`:describe-option completion.*`). Defaults shown.

```toml
[completion]
# Manual trigger (default). When false, popup opens only
# on explicit `<C-x><C-o>` / `<C-Space>` / smart-tab.
auto_trigger         = false

# Minimum prefix length before identifier-threshold auto-
# trigger fires. Only consulted when auto_trigger = true.
min_chars            = 2

# Per-keystroke debounce for refilter / re-rank passes.
# Lower = more responsive; higher = less work on rapid
# typing.
debounce_ms          = 50

# Auto-show docs popup on selection change. Off by default
# to keep the popup compact.
docs_auto            = false

# Editor-side commit characters (union with each LSP
# server's advertised set). Empty = LSP-only.
extra_commit_chars   = ""

# Auto-accept when the popup has only one candidate.
# Off by default; rust users typically flip this on.
auto_insert_single   = false

# Minimum match score for a candidate to render. Filters out
# very weak fuzzy matches that would otherwise waste popup
# rows.
fuzzy_threshold      = 5

# Tree-sitter scope query targets where completion should be
# suppressed. Default empty -- completion fires everywhere.
# Example: ["string", "comment"] suppresses popups inside
# string literals + comments. Per-language overridable.
suppress_in          = []

# Per-source enable / priority. `enabled = false` removes
# the source from the popup entirely.

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
# Visible buffers + recently-active ones.
scope            = "visible-and-recent"

[completion.source.path]
enabled    = true
priority   = 90
# Filesystem walk depth from the typed prefix.
max_depth  = 3
# Skip entries matching these glob patterns.
ignore     = ["node_modules", ".git", "target"]

[completion.source.tree-sitter]
enabled    = true
priority   = 80

# Per-language overrides. Any [completion] key can be set
# per-language.
[completion.per-language.markdown]
sources       = ["snippet", "buffer-words", "path"]   # no LSP / ts for prose
auto_trigger  = false                                  # explicit only

[completion.per-language.rust]
auto_insert_single = true
auto_trigger       = true

[completion.per-language.text]
sources       = ["snippet", "buffer-words"]
suppress_in   = []
```

Per-buffer override path: `:setlocal completion.auto_trigger=true`
(once `setlocal` lands; today only the global form works).

Plugin host eventually exposes
`completion.source.<plugin-id>` keys for plugin-supplied
sources.

---

## 10. Help system

Every option, source, binding, and command lands with a
`:help completion-*` topic. The help pages are part of the
feature, not an afterthought. Topic structure:

| Topic | Body |
|---|---|
| `completion` | Overview: what completion is, how to invoke it (manual default), how the popup looks, key bindings cheat-sheet, links to sub-topics. |
| `completion-keymap` | Every binding in every layer (Insert default, completion-popup minor mode, active-snippet minor mode), with chord + ex-command + behaviour. |
| `completion-trigger` | Manual vs. auto-trigger, smart-tab, identifier threshold, trigger characters. |
| `completion-popup` | Multi-column layout, kind glyphs legend, source tags, deprecated styling. |
| `completion-docs-popup` | Side documentation popup, `<C-d>` toggle, lazy resolve, scroll keys. |
| `completion-sources` | The six built-in sources + plugin source slot. Per-source priority, enable/disable, source-specific options. |
| `completion-sources-lsp` | LSP source: filterText / sortText / preselect / commit chars / additional edits. |
| `completion-sources-snippet` | Snippet source: TextMate format, prefix lookup, integration with the popup. |
| `completion-sources-buffer-words` | Word-completion from visible buffers. |
| `completion-sources-path` | Path source: trigger inside string contexts. |
| `completion-sources-tree-sitter` | Local symbols from the syntax tree. |
| `completion-snippets` | Snippet authoring: TextMate body syntax, placeholders, choices, transformations, variables, scope. |
| `completion-snippet-loading` | Where snippets load from, in priority order; bundled / friendly-snippets / user / project. |
| `completion-snippet-keymap` | Snippet expansion + placeholder navigation bindings. |
| `completion-options` | Every config option with type, default, valid values, examples. Alias of `:describe-option completion.*`. |
| `completion-commands` | Every ex-command + Rust API entry-point. |
| `completion-troubleshooting` | "Why doesn't completion fire?" / "popup is empty" / "LSP source not contributing" diagnosis flowchart. |
| `completion-glossary` | "Anchor", "tab-stop", "trigger char", "isIncomplete" defined. |

`:help completion` is the entry point; it renders an index
linking to every sub-topic. Sub-topics cross-link each
other and `:help lsp`, `:help keymap`, `:help options`.

The help authoring is gated to ship **with** each phase —
4.2.g.1 lands `completion`, `completion-keymap`,
`completion-popup`, `completion-options`,
`completion-troubleshooting` before the phase closes.
Subsequent phases extend the index as they introduce new
surfaces (4.2.g.2 adds `completion-sources-lsp`; 4.2.g.4
adds the snippet topics; etc.).

The wider help directive — every feature ships with help —
extends beyond completion. Documented separately as a
project convention.

---

## 11. Implementation plan

### Phase 4.2.g.1 — Insert-mode shell + sync sources + help skeleton

Goal: `<C-x><C-o>` / `<C-Space>` opens a popup with buffer-
words completion; `<Tab>` / `<C-y>` / `<CR>` accept; `<C-n>` /
`<C-p>` navigate; `<Esc>` / `<C-e>` dismiss. No LSP, no
async. Help index landed.

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
- Insert-mode key bindings: `<C-x><C-o>` / `<C-Space>` /
  smart-tab open the popup; routes through the new
  ex-commands `ex:completion-trigger` /
  `ex:completion-next` / `ex:completion-prev` /
  `ex:completion-accept` / `ex:completion-cancel` /
  `ex:completion-cancel-and-exit-insert`.
- `MinorModeLayer` plumbing: `translate(ctx, event)`
  consults the completion-popup layer when the popup is
  open.
- Renderer: completion popup widget anchored at cursor;
  multi-column rendering with kind glyph + label + source
  tag (no detail / docs popup yet).
- Tests: trigger / dismiss / accept / refilter on
  keystroke; smart-tab branching; minor-mode layer
  isolation (Normal-mode `<C-d>` unchanged when popup
  closed); snapshot tests for renderer columns.
- **Help**: `:help completion` (overview), `:help
  completion-keymap`, `:help completion-popup`,
  `:help completion-options`, `:help
  completion-troubleshooting`, `:help completion-commands`.

Done when: `<C-x><C-o>` shows buffer words; `<C-y>` inserts;
`:help completion` walks the user through it without
referencing source code.

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
- Trigger-character detection (when `auto_trigger = true`):
  walk attached servers'
  `completionProvider.triggerCharacters`, union with
  `completion.extra_commit_chars`.
- Item adaptation: `filterText` / `sortText` /
  `preselect` / `tags[Deprecated]` / commit characters;
  `textEdit.range` overrides anchor.
- Phase out the `:complete` ex command (the inline popup
  replaces it; the alias keeps working but echoes a
  one-liner pointing at `<C-x><C-o>`).
- **Help**: `:help completion-sources`, `:help
  completion-sources-lsp`, `:help completion-trigger`
  (extended with auto-trigger + trigger-character
  semantics).

Done when: typing `foo<C-Space>` in a `.rs` buffer pops up
rust-analyzer's completions inline.

### Phase 4.2.g.3 — Documentation side popup

Goal: `<C-d>` (within the completion-popup minor mode)
toggles a side popup showing the focused candidate's full
documentation; `completionItem/resolve` fires lazily.

Deliverables:

- `DocPopupState` + render path right of (or below) the
  completion popup.
- LSP `completionItem/resolve` integration: fires when the
  focused candidate's `documentation` is missing; result
  feeds `DocPopupState.body`.
- Existing markdown-render path (the hover popup's renderer)
  reused for the doc popup body.
- Auto-show option: `:set completion.docs_auto=true`.
- `<C-f>` / `<C-b>` scroll the doc popup when open.
- Tests: focus toggles popup; resolve fills body once;
  cancellation drops the popup on dismiss; `<C-d>` outside
  the minor mode does NOT toggle docs (Normal-mode
  half-page-down still wins).
- **Help**: `:help completion-docs-popup`.

Done when: `<C-d>` on a function candidate shows its
signature + doc comment; `<C-f>` scrolls; `<Esc>` closes
both popups.

### Phase 4.2.g.4 — Snippet engine + friendly-snippets compat

Goal: snippet items expand with placeholder navigation; LSP
items with `insertTextFormat == Snippet` route through the
engine; `friendly-snippets` repo loads as-is.

Deliverables:

- New `lattice-snippet` crate:
  - `parse.rs` — TextMate body parser (full syntax:
    placeholders, choices, transforms, variables,
    escapes).
  - `load.rs` — JSON ingestion (TextMate format VS Code
    uses).
  - `registry.rs` — per-language store with lookup by
    prefix.
  - `render.rs` — token walk producing inserted text +
    placeholder range tracking.
  - `variables.rs` — built-in variable resolver.
- Active-snippet minor mode in the input layer: `<Tab>` /
  `<S-Tab>` (and `<C-l>` / `<C-h>` aliases) navigate
  placeholders; `<Esc>` exits.
- Snippet registry loads bundled + friendly-snippets-style
  packs + user + project snippets in priority order.
- `gen:snippet` source feeds into the unified popup.
- `<C-x><C-s>` (`:snippet-expand`) for direct expansion.
- Choice placeholders open an inline mini-picker;
  transformation placeholders re-evaluate per-edit.
- LSP item routing: `insertTextFormat == Snippet` items go
  through the engine.
- **Help**: `:help completion-sources-snippet`, `:help
  completion-snippets`, `:help completion-snippet-loading`,
  `:help completion-snippet-keymap`.
- Tests: TextMate parse round-trip; placeholder
  navigation; variable substitution; LSP-snippet end-to-
  end (mock server); friendly-snippets corpus parses
  without panic.

Done when: typing `for<C-x><C-s>` expands `for x in y { z }`
with placeholder hops on `<Tab>`; cloning friendly-snippets
into `~/.config/lattice/snippet-packs/` makes its
language packs immediately available.

### Phase 4.2.g.5 — Frequency ranking + per-source priority + per-language config

Goal: items the user has accepted recently bubble to the
top; per-source priority is configurable; per-language
filters work.

Deliverables:

- In-memory `(label, kind) -> count` map on the App,
  bumped per accept.
- Ranker reads the map; bonus capped at +50.
- Per-source priority override via `:set
  completion.source.<id>.priority=<n>`.
- Per-language source filter:
  `[completion.per-language.<lang>] sources = [...]`.
- Per-language `auto_trigger`, `auto_insert_single`,
  `suppress_in` overrides.
- Tests: accepted item ranks above peers next time; per-
  language sources list filters the popup.
- **Help**: extend `:help completion-sources` and `:help
  completion-options` with the per-language patterns.

### Phase 4.2.g.6 — Tree-sitter + path sources

Goal: ship the local-symbol and path sources.

Deliverables:

- `gen:tree-sitter-symbol`: query the buffer's syntax tree
  for in-scope identifiers (function defs, let-bindings,
  closure params).
- `gen:path`: triggered by `'/'` inside a string literal
  (detected via tree-sitter scope query); filesystem walk
  capped at 200 entries; respects `.gitignore` plus
  configured `ignore` globs.
- **Help**: `:help completion-sources-tree-sitter`, `:help
  completion-sources-path`.

### Phase 4.2.g.7 — Polish

- Ghost text for the top-ranked item (optional, off by
  default).
- `additionalTextEdits` apply path coalesces with the main
  insert into one undo unit.
- Postfix-completion seam (deferred to plugin host).
- Persistent frequency tracking (post-1.0; needs privacy
  story).
- **Help**: `:help completion-glossary`, top-up of every
  prior topic.

---

## 12. Mode-driven source contributions

Sections 3 -- 11 describe the v1 wiring: sources are hardcoded
in the host. `populate_insert_completion_sync` calls
`BufferWordsSource` / `lattice-snippet` / `Syntax::collect_symbols`
directly; `do_lsp_insert_completion_request` fans out to LSP
servers from the TUI crate. `lsp-completion-mode` is a marker
minor whose only job is to gate the LSP fan-out at one site
(`App::lsp_completion_mode_enabled_for`).

This section describes the post-v1 evolution: **every source is
contributed by a minor mode**, and the engine asks active modes
what they contribute. The shape mirrors corfu + cape (emacs) and
the existing lattice contract for `options()` / `keymap()` /
`subscriptions()` / `decorations()` -- a new declarative
contribution on the `Mode` trait.

### 12.1 Why this shape

Four reasons, in priority order:

1. **Plugin parity.** A WASM plugin that wants to add a
   completion source registers a minor mode contributing the
   source. Same surface every other mode contribution uses; no
   special "completion plugin" path.
2. **Feature locality.** `lsp-completion-mode` lives in
   `lattice-lsp`; the LSP source struct should too. v1 has it
   in `lattice-ui-tui::app::lsp` because the trait surface
   didn't support async sources cleanly. Mode contributions
   close that gap.
3. **Uniform discoverability.** `:describe-mode
   lsp-completion-mode` renders *what source it contributes,
   with what id, with what priority, with what trigger chars*
   -- same rendering path as options / keymap.
   `:describe-buffer`'s "Active modes" section (already
   shipped) naturally surfaces "what sources are active here."
4. **Toggle semantics already work.** `:lsp-completion-mode`
   exists as a toggle command (M.5.1 auto-generated from the
   mode name). The user-facing surface doesn't change.

### 12.2 The trait surface

A new declarative contribution on `Mode`:

```rust
pub trait Mode: Send + Sync + 'static {
	// ... existing: id, kind, options, keymap,
	// subscriptions, decorations, implies, conflicts,
	// required_capabilities, on_activate, on_deactivate ...

	/// Insert-mode completion sources this mode contributes
	/// while active on a buffer. Empty = no contribution.
	/// Default no-op; LSP / snippet / buffer-words / etc.
	/// minors override.
	fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
		Vec::new()
	}
}
```

`CompletionSourceContribution` carries the source as one of two
shapes (sync produce, async spawn) plus the metadata the
aggregator needs upfront (priority, trigger chars, auto-fire
policy) without paying a dynamic-dispatch cost per keystroke:

```rust
pub struct CompletionSourceContribution {
	pub id: SourceId,
	pub default_priority: u32,
	pub auto_trigger: bool,
	pub trigger_chars: Vec<char>,
	pub kind: CompletionSourceKind,
}

pub enum CompletionSourceKind {
	/// Cheap, blocking. Aggregator calls `produce(ctx)`
	/// directly on the popup-open / refilter path.
	/// `Arc` so cloning the contribution stays O(1).
	Sync(Arc<dyn SyncCompletionSource>),

	/// Tokio-driven; the aggregator spawns the task on
	/// popup-open and on every `isIncomplete` re-fire. The
	/// task pushes into a host-owned mailbox that drains
	/// per-frame into `state.raw`.
	Async(Arc<dyn AsyncCompletionSource>),
}

pub trait SyncCompletionSource: Send + Sync + std::fmt::Debug {
	fn produce(&self, ctx: &InsertContext<'_>) -> Vec<RawCandidate>;
}

pub trait AsyncCompletionSource: Send + Sync + std::fmt::Debug {
	/// Spawn the per-popup-instance task. The task pushes
	/// into `tx` until `token` is cancelled or it has no
	/// more to send. `ctx_snapshot` is owned data the task
	/// captures (URI + cursor + query + trigger), since the
	/// borrow can't cross the spawn boundary.
	fn spawn(
		&self,
		ctx_snapshot: InsertContextSnapshot,
		tx: tokio::sync::mpsc::UnboundedSender<RawCandidate>,
		token: lattice_protocol::CancellationToken,
	);
}
```

The existing `InsertSource` trait (§3.3) is the v1 ancestor of
`SyncCompletionSource`. The async side is genuinely new -- v1
had no async-source trait, just bespoke host code. Naming the
two as a family clarifies the contract: a source is either
"give me data now, synchronously" or "spawn me; I'll push."

### 12.3 The `completion-mode` engine minor

`completion-mode` is the mode that *is* the completion engine's
active surface. Activation contract:

- **Activated** by the host when `App.insert_completion`
  transitions from `None` to `Some(_)` (i.e. the popup opens).
  No user toggle: it's a transient minor reflecting popup
  state.
- **Deactivated** on dismiss.
- **Contributes:** the popup-layer keymap (§6.1) -- the
  `<C-n>` / `<C-p>` / `<C-y>` / `<C-d>` / `<C-f>` / `<C-b>` /
  `<C-e>` / `<CR>` / `<Tab>` / `<Esc>` chords the
  `COMPLETION_POPUP_LAYER` already routes.
- **Does NOT contribute a source itself.** The engine isn't a
  source; it consumes sources from *other* active modes.

This formalises §3.8's "completion-popup minor mode" sketch as
a real registered mode. The architectural payoff is that
`:describe-key <C-n>` resolves through the mode registry and
attributes the binding to `completion-mode` -- same shape every
other transient layer (active-snippet-mode, hover-mode) uses.

### 12.4 Active source resolution + caching

The aggregator must not pay a per-keystroke "walk every active
mode" cost. Resolution is cached:

```rust
// On `App.active_modes[buffer]` change (activate / deactivate),
// recompute and cache:
buffer_locals[buffer].insert(ActiveCompletionSources(sources));

pub struct ActiveCompletionSources(pub Vec<CompletionSourceContribution>);
impl BufferLocal for ActiveCompletionSources {
	const OWNER_MODE: &'static str = "completion-mode";
	// ...
}
```

The aggregator reads through the buffer-local on the popup-
open path -- O(1) lookup, no registry walk per keystroke. The
recompute happens on mode-transition (rare) and walks the
buffer's `active_modes` set, calling `mode.completion_sources()`
on each. Per-language overrides (§3.4) filter the resolved
list at popup-open time (cheap; runs once per popup, not per
keystroke).

### 12.5 Source migration table

| Source today (§3.4) | Becomes | Mode | Crate |
|---|---|---|---|
| `gen:lsp-completion` | `AsyncCompletionSource` | `lsp-completion-mode` (already exists) | `lattice-lsp` |
| `gen:snippet` | `SyncCompletionSource` | new `snippet-completion-mode` | `lattice-snippet` |
| `gen:buffer-words` | `SyncCompletionSource` | new `buffer-words-mode` | `lattice-completion` |
| `gen:tree-sitter-symbol` | `SyncCompletionSource` | new `tree-sitter-completion-mode` | `lattice-syntax` |
| `gen:path` | `SyncCompletionSource` | new `path-completion-mode` | `lattice-completion` |
| `gen:plugin-*` | either shape | plugin's minor mode | plugin crate |

Each new minor follows the existing "a mode lives with the
crate that owns its feature" rule. `register_*_modes` per
crate wires registration at boot, exactly like
`register_lsp_log_modes` / `register_file_tree_modes` etc.

### 12.6 Auto-activation policy

Modes that contribute sources auto-activate by default on
relevant buffers:

| Mode | Auto-activates when |
|---|---|
| `buffer-words-mode` | every buffer with `text-mode` or any language major |
| `snippet-completion-mode` | every buffer where `snippet_registry.has_for(language)` returns true |
| `tree-sitter-completion-mode` | every buffer with an attached `Syntax` handle |
| `path-completion-mode` | every buffer (suppressed at non-string scopes by the source itself) |
| `lsp-completion-mode` | per M.6.1 cascade -- when `lsp-mode` activates and the attached server advertises `completionProvider` |

Users disable via `:<mode>-mode` toggle (vim grammar already
covers this). TOML-level disable lives in the existing
`completion.per-language.<lang>.sources` allowlist (§3.4) --
the per-language override still wins, since it's the
*selection* layer applied on top of the activated set.

### 12.7 LSP source relocation

The LSP source moves from bespoke host code into a real
`AsyncCompletionSource` implementation in `lattice-lsp`:

```rust
// crates/lattice-lsp/src/completion.rs (new module).

pub struct LspCompletionSource {
	pub lsp: LspSupervisorHandle,
}

impl AsyncCompletionSource for LspCompletionSource {
	fn spawn(
		&self,
		ctx: InsertContextSnapshot,
		tx: mpsc::UnboundedSender<RawCandidate>,
		token: CancellationToken,
	) {
		let lsp = self.lsp.clone();
		// Same fan-out + dedup + isIncomplete handling
		// currently in `do_lsp_insert_completion_request`,
		// but spawned from this method instead of the host.
		tokio::spawn(async move {
			// ... existing per-server iteration ...
		});
	}
}
```

`LspCompletionMode::completion_sources()` returns one
`CompletionSourceContribution { id: LSP_COMPLETION_SOURCE_ID,
default_priority: 200, kind: Async(Arc::new(LspCompletionSource {
lsp })) }`. The supervisor handle is captured at mode
construction (App boot wires it).

Once this lands, `lattice-ui-tui::app::lsp` no longer needs the
~200-line `do_lsp_insert_completion_request` path; the
aggregator drives the spawn through `Async::spawn`. The TUI
keeps only the per-frame drain that merges `state.raw`.

### 12.8 Slice plan

Each slice ships docs + tests + (perf-relevant) benches +
graceful error handling per CLAUDE.md. None of these can be
batched -- they're staged so a regression at any step is
attributable.

| Slice | What lands | Crates touched |
|---|---|---|
| **CSM.1** | `CompletionSourceContribution` + `SyncCompletionSource` + `AsyncCompletionSource` types in `lattice-completion::source`. Default `Mode::completion_sources()` -> `Vec::new()`. No behaviour change -- everything still uses the v1 hardcoded path. | `lattice-completion`, `lattice-mode` |
| **CSM.2** | `completion-mode` minor registered in `lattice-completion::modes` (new module). Host activates / deactivates on popup open / close. `COMPLETION_POPUP_LAYER` lookup gated on the mode being active (replaces the imperative `ctx.completion_popup_open` flag). | `lattice-completion`, `lattice-ui-tui` |
| **CSM.3** | `ActiveCompletionSources` buffer-local + the recompute-on-mode-transition wiring. Empty in practice (no mode contributes yet); proves the cache shape works. Reader plumbed into the aggregator, fallback to today's hardcoded calls when the local is empty. | `lattice-ui-tui` |
| **CSM.4** | `buffer-words-mode` (new minor in `lattice-completion`) contributes a `SyncCompletionSource`. Migrate the hardcoded buffer-words call in `populate_insert_completion_sync` to read through the active-sources cache. Tests pin behaviour parity. | `lattice-completion`, `lattice-ui-tui` |
| **CSM.5** | `snippet-completion-mode` in `lattice-snippet`. Same migration shape as CSM.4. The snippet metadata sidecar (`insert_completion_snippet_meta`) stays host-side -- the source emits candidates with `CandidateData::Extension` payloads; the host's accept path resolves them as before. | `lattice-snippet`, `lattice-ui-tui` |
| **CSM.6** | `tree-sitter-completion-mode` in `lattice-syntax`. Migrate `collect_symbols` walk into the source's `produce()`. | `lattice-syntax`, `lattice-ui-tui` |
| **CSM.7** | `path-completion-mode` in `lattice-completion`. Migrate the path-context branch (`completion_in_path_context` stays as the host's scope-detect flag; the mode auto-suppresses outside string scopes by reading the flag through the context snapshot). | `lattice-completion`, `lattice-ui-tui` |
| **CSM.8** | `LspCompletionSource` in `lattice-lsp::completion`. `LspCompletionMode::completion_sources()` returns it. Remove the bespoke `do_lsp_insert_completion_request` body -- the aggregator's async-spawn path covers it. `App::lsp_completion_mode_enabled_for` becomes a deprecated alias of the generic "is mode active" lookup. | `lattice-lsp`, `lattice-ui-tui` |
| **CSM.9** | Plugin reservation: the WIT surface (Phase 7 prereq) defines `mode.completion-sources -> list<completion-source>` so a WASM plugin can contribute. No actual plugin -- this slice just locks the WIT shape so the host-side runtime can target it. | `wit/`, `lattice-mode` |

Slice ordering: foundation (CSM.1) → engine mode (CSM.2) → cache
(CSM.3) → migrate sources cheapest-first (CSM.4 → CSM.7) → LSP
last (CSM.8) → plugin reservation (CSM.9). Each later slice
inherits the parity tests of every earlier slice.

### 12.9 Performance posture

The cached `ActiveCompletionSources` keeps the hot path
identical to today's:

- **Per-keystroke refilter:** reads cached source list (O(1)
  buffer-local lookup), calls each sync source's `produce()`
  (same cost as today's hardcoded calls), runs the existing
  matcher / ranker pipeline. No registry walk, no dynamic-
  dispatch beyond what the trait object already costs.
- **Async source spawn:** runs at popup-open + on
  `isIncomplete` re-fire only. Same trigger frequency as
  today; same cost.
- **Mode-transition recompute:** rare (mode flips on
  major-mode change, LSP attach/detach, manual `:` toggle).
  Walks `active_modes.len()` × `mode.completion_sources()`
  calls -- each a `Vec::new()` for non-contributing modes,
  bounded clone of contribution metadata for contributing
  ones. Worst case microseconds; happens off the keystroke
  path.

Benchmark coverage (lands with CSM.4 -- the first migrated
source): `bench_completion_sync_cached` measures the
cached-active-sources path against today's hardcoded path,
asserts the regression budget is under 5% on a 200-candidate
popup refilter.

### 12.10 Open questions

1. **Source contribution mutability post-activation.** A mode
   today contributes a static `OptionOverrideSet`. Should
   `completion_sources()` be allowed to vary by buffer (e.g.
   `lsp-completion-mode` returns trigger chars sourced from
   the *currently attached server's* `completionProvider`)?
   Default position: yes, but the mode reads buffer-locals
   for the per-buffer payload, not arguments to the method.
   The method itself stays `&self -> Vec<...>`; the dynamic
   bits come from the source closure capturing `Weak<...>`
   handles. Verify when CSM.8 lands.

2. **Cross-source dedup.** Today the host dedups LSP items by
   `(label, kind)` inside the LSP fan-out, and the aggregator
   reruns matcher / ranker over the merged set. Should
   cross-source dedup (e.g. a tree-sitter symbol that
   coincides with a buffer word) be a generic aggregator
   concern? Default position: no -- per-source priority
   already handles the "which wins on tie" question. Visual
   merge of identical-text rows is a 4.2.g.7 polish item.

3. **Manual toggle of `completion-mode`.** Should the user be
   able to `:completion-mode off` to disable the popup
   entirely? The mode auto-activates on popup-open, so
   user-level disable is conceptually "no completion source
   is active." Cleaner: a `completion.enabled` typed option
   that gates `do_completion_trigger` outright, leaving the
   mode purely transient. Resolve when CSM.2 lands.

---

## 13. Performance commitments

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

## 14. Open questions

1. **Snippet placeholder mirroring with concurrent edits.**
   What if the user types in one mirror and a buffer event
   from another source (LSP didChange) lands at the same
   place? v1: ignore (mirrors are fully client-side); the
   server's didChange covers our insert anyway.

2. **isIncomplete + manual trigger.** Manual `<C-x><C-o>` /
   `<C-Space>` always re-fires the LSP source — manual
   trigger means "give me the freshest set."
   (Resolved here; verify against rust-analyzer when 4.2.g.2
   lands.)

3. **Multi-server LSP completion ordering.** Two servers on
   a `.cpp` file (clangd + a linter bridge) both return
   items. Order: union, dedup by `(label, kind)`, sort by
   ranker. Per-server priority breaks ties. Verify the
   architecture doc's "score-merging" wording matches this.

4. **Plugin generator security.** WASM plugins must honour
   `token.is_cancelled()`; a misbehaving plugin that
   ignores cancellation can starve the aggregator.
   Mitigation: aggregator drops slow-source pushes after a
   deadline (e.g. 500 ms past the keystroke that triggered
   them).

5. **Documentation popup width when wrapping is on.** The
   doc body is markdown; long lines wrap. Cap at 60 cells
   and let wrap handle overflow. Detail (column 3 in the
   main popup) doesn't wrap — it's a one-liner.

---

## 15. Cross-references

- design.md §5.11.3 — completion pipeline (cmdline today;
  Insert-mode peer formalised by this doc).
- design.md §5.9.10 — rich minibuffer (the picker UX this
  doc's popup descends from).
- design.md §5.2.1 — unified command/grammar dispatch (why
  every binding here is also a registered command).
- [`lsp-architecture.md`](lsp-architecture.md) §10 — LSP
  request fan-out, cancellation tokens.
- [`crates/lattice-completion/`](../crates/lattice-completion/)
  — current pipeline traits; `insert.rs` is the new module
  this doc adds.
- `lattice-snippet` (new crate, 4.2.g.4) — engine + parser.
- `../../user/lsp.md` — user-facing LSP completion blurb (will
  pivot from `:complete` picker to inline popup once
  4.2.g.2 lands).
- `friendly-snippets` upstream:
  https://github.com/rafamadriz/friendly-snippets — the
  default snippet corpus we target compatibility with.
- [`mode-architecture.md`](mode-architecture.md) — `Mode`
  trait surface; §12 of this doc adds
  `Mode::completion_sources()` as a new declarative
  contribution.
