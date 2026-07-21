+++
title = "auto-pair — the first bundled plugin (auto + manual pairing)"
+++


> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing is §12 (inline while small; splits to
> a slice-plan file if it grows). Sibling fragments:
> [`plugin-host.md`](plugin-host) (the seam spine),
> [`keymap-architecture.md`](keymap-architecture) (the layered keymap the
> insert-mode bindings + the *declining-binding* fall-through land in),
> [`lighthouse.md`](lighthouse) (the next 8b plugin, sequenced after this).
>
> **Status: ✅ built — ships as a core plugin.** The **first bundled 8b plugin**;
> both the `auto` and `manual` styles work end-to-end (incl. the TUI), and it
> ships prebuilt + enabled-by-default via the config gate (see
> [`plugin-manager.md`](plugin-manager) §7, [`config-and-init.md`](config-and-init)
> §6). It is *not* host-free (an earlier draft assumed so) — porting the manual
> style forced three small, **general** host prerequisites (§5). Those were the real
> cost and the real value: reusable capabilities many future plugins need,
> validated here on a concrete UX. Slice status/sequencing lives in the slice plan
> (`operations/slice-plans/archive/plugin-auto-pair.md`).

## 1. Why

Auto-pairing is a universal editor expectation and one of design.md §5.5.6's named
"editing helpers" bundled plugins. Its purpose *here* is to be the **first real
first-party plugin** — proving the bundled-plugin pipeline (crate →
`wasm32-wasip2` component + `manifest.toml` → discovery → `activate` → live seam
contributions) on a genuine, user-visible workload — while building out the host
surface (§5) that the plugin ecosystem needs regardless.

It ships **two styles**, gated by config:

- **`auto`** (default) — the familiar behavior: type an opener, get the pair.
- **`manual`** — type openers freely; a single key closes the nearest unmatched
  opener by a backward stack scan. This is a port of the author's
  [`vim-pairify`](https://github.com/dhruvasagar/vim-pairify) model (see §3), and
  is the *recommended* style: it eliminates every "surprise insertion" failure
  mode that makes auto-pairing perennially fiddly (§10).

## 2. The two styles + config gating

A typed option `auto-pair.style` = `auto` | `manual`, default `auto`, registered
via the **config** seam. The plugin reads it and registers the matching insert-mode
keymap; on an `OptionChanged` for it (via the **events** seam), it re-registers —
so the style flips live. Two more options: `auto-pair.close-key` (default
`<C-j>`, the manual close key) and the pairs table (§4), modifiable at runtime.

| Style | Openers `( [ { < " ' \`` | Closers | Close key |
|---|---|---|---|
| `auto` | insert the matching closer, caret between | typed closer before that closer → step over | — |
| `manual` | plain self-insert (unbound) | plain self-insert | one key → close nearest unmatched (§3), else fall through (§6) |

This makes the plugin exercise **four** seams (grammar / keymap / config / events)
— richer pipeline validation than an auto-only plan.

## 3. The manual-close algorithm (`find_pair`, ported)

Scan the scope text (§7) **backward** from the cursor, maintaining a stack of
*closers* seen. Faithful to `pairify#find_pair`:

```
for char, scanning backward from the cursor:
  if char is a CLOSER (in right-map):
    if char == '>' and the char before it is ' '   → skip (comparison operator)
    elif stack nonempty and symmetric(char) and stack.top == char → pop  # balanced quote pair
    else → push char
  elif char is an OPENER (in left-map):
    if char == '<' and the char after it is ' '     → skip (comparison operator)
    elif stack nonempty and complement(char, stack.top) → pop            # matched bracket pair
    elif stack empty → return left-map[char]         # nearest UNMATCHED open → its closer
  # (continue)
end → return stack.bottom or ''                       # stray closer, or nothing
```

- **Symmetric pairs** (`"` `'` `` ` ``, where open == close): the `symmetric(char)`
  branch pairs them by adjacency on the stack — the port of `s:is_symmetric` /
  `s:is_complement`. No open-vs-close ambiguity: consecutive same chars balance.
- **`<` / `>`**: the space-adjacency heuristic skips comparison operators (`a < b`,
  `a -> b`), so angle brackets only pair as generics/tags.
- **Return value** is the **closer to insert** at the cursor. `''` → nothing
  unmatched → **fall through** (§6).
- **Early exit**: it returns at the *first* unmatched opener going backward, so the
  typical scan is short (a few chars / the enclosing structure), independent of
  file size.

## 4. The pairs table

Two maps (`left`: opener→closer, `right`: closer→opener), the `vim-pairify`
default, modifiable at runtime (per-language additions, user overrides):

```
left:  ( → )   [ → ]   { → }   < → >   ' → '   " → "   ` → `
right: ) → (   ] → [   } → {   > → <   ' → '   " → "   ` → `
```

The table is the plugin's data; the host never interprets it. v1 ships the static
default; per-language tables + a user-facing add/remove API are v2.

## 5. Seam usage + the host prerequisites (LOAD-BEARING)

The plugin uses four **wired** seams — grammar `register-action`, keymap
`register-binding`, config `register-option`, events `subscribe` (OptionChanged).
But the *actions* need to read buffer text, which the grammar seam cannot do today
— so auto-pair forces three small, general host additions:

### 5.1 Grammar-context `document` handle + cursor  (prerequisite AP.0.1) — **done**

`action-context` carried only `args`/`register`/`count` — no cursor, no buffer.
The pairing actions need the cursor byte offset + surrounding text. AP.0.1
**extends the grammar `apply-action` callback with a `borrow<document>` handle +
a `cursor` on `action-context`** — the pattern the **picker** seam was designed
for (`init(ctx)` takes a `document`; the guest calls `document.get-text-range`,
bulk rope never crosses). The read machinery (`DocumentResource`, PH7.3c) already
existed; AP.0.1 is the **first place a host-owned resource crosses into a guest
export**, resolving the modeling subtlety the picker/plugin worlds deferred.
General: every grammar plugin that reasons about surrounding text needs it.

**As built** (motion/text-object contexts unchanged — the action path is all
`auto` needs; a text-reading motion reuses the same handle when one needs it):

- **WIT** — `action-context` gains `cursor: position`; `apply-action(callback,
  ctx, doc: borrow<document>)`; the `grammar-plugin` world `import buffer` +
  `grammar-callbacks` `use buffer.{document}`.
- **Native `ActionContext`** — gains `cursor: Position` + an **owned `Buffer`**
  (not `Arc<DocumentSnapshot>`): `Buffer` is a `lattice-core` type (an O(1)
  `ropey::Rope` clone), so `lattice-grammar` needs no `lattice-runtime`
  dependency; the trampoline mints the snapshot host-side. Owned, so no context
  lifetime and no blast radius beyond the two `ActionContext` construction sites.
- **Lifecycle** — the handle is minted **per dispatch**, never at registration:
  the dispatcher captures the current buffer + cursor, the trampoline pushes a
  `DocumentResource` into the store's `ResourceTable`, lends a
  `Resource::new_borrow` for the one synchronous `apply-action` call, then
  `table.delete`s it. The guest never holds a document across calls, so "the
  active buffer changed / differs" is a non-issue by construction. Synchronous
  throughout (the PH7.7 fork) and Reflex-budget-bounded.

> **bindgen gotcha (record for AP.0.3 + the picker's `init(doc)`):** the `with:`
> key that substitutes a resource's host rep type uses a **`.` before the
> resource name** — `"lattice:plugin-host/buffer.document": DocumentResource` —
> **not** a `/` (`…/buffer/document`). The slash form fails with the misleading
> `"interfaces … not referenced in the target world"`. `add_to_linker` also needs
> the empty interface-level `buffer::Host` impl alongside `HostDocument`, and the
> resource must be `borrow`-passed with `Resource::new_borrow(owned.rep())` +
> host-side `table.delete` (the host owns it throughout).

### 5.2 Declining / fall-through bindings  (prerequisite AP.0.2)  — **as built**

The manual close key must **fall through** when `find_pair` returns `''` — so
`<C-j>` still does whatever else is bound (completion nav, newline, a user remap;
requirement #4). A plugin binding normally *shadows* lower layers. Add a dispatch
capability: **an action may return "declined," and the dispatcher resumes at the
next keymap layer** (the built-in / user binding for that chord). General +
reusable (any conditional plugin binding wants it), and — unlike a per-keystroke
guest *predicate* — the guest runs only when the key is actually pressed, never on
the hot path.

**As built:** a grammar action returns `Effect::Declined`, which sets
`DispatchOutcome.declined`; the dispatcher then re-resolves the same chord with the
declining action's mode layers excluded. This works in BOTH renderers — the host
`dispatch_chord` path (GPUI + tests) *and* the TUI live keystroke path
(`runtime.rs`, which carries the flag out of `App::apply` and re-translates the
`KeyChord`). So `manual`-style close-key + `<BS>` fall-through land end-to-end in
the TUI, not just under programmatic dispatch.

### 5.3 tree-sitter query seam  (prerequisite AP.0.3) — **v1**

Lexically scoping the backward scan to the current function/class/block (§7) — and,
far more broadly, letting *any* plugin **query the syntax tree, resolve the node at
a position, walk it, and find the enclosing scope** — is a **foundational**
extensibility capability, not an auto-pair detail. lattice parses every language
with tree-sitter; a plugin seam over it directly serves **paramount goal #3**
(extensible modal editing, including "future tree-sitter-driven variants" of
motions / text objects) and unlocks a whole class of structural plugins (motions,
text objects, folds, refactors, rainbow-delimiters, …). **Promoted to v1
(2026-07-19, Dhruva):** a truly customizable, tree-sitter-driven editor needs this
seam early, not as a late refinement. It gets its **own design fragment**
([`plugin-treesitter-seam.md`](plugin-treesitter-seam), in design); auto-pair
is its first consumer (the enclosing-scope query bounds the scan). Complex, but
core — see that fragment for the query/node/cursor API, the capability model, and
the tree-mutation-under-read hazard.

### 5.4 Column-precise edit effect  (prerequisite AP.2) — **done**

An open action must place the caret *between* the inserted `()`, and a
close-insert after the `)` — column-precise positions the edit vocabulary didn't
express. Two general additions (not auto-pair-specific — every editing plugin
benefits): `action-context` gains **`buffer-id`** (the `target` an action names
in an `apply-edit`; mirrors `motion-context`), and **`apply-edit-payload.cursor`**
became `option<position>` (was `option<u32>`, a *row*) so `Effect::ApplyEdit` can
park the caret at any line+byte. `ApplyEdit` stays the single general precise-edit
primitive for native and WASM (the `feedback_effect_vocabulary_is_host_boundary`
principle) — no auto-pair-shaped effects added. Native row-start callers
(diff / ai) pass `Position::new(row, 0)`, behaviour-preserving. Close-**skip** (step
over an existing `)`) is a pure caret move with no text change, so it uses
`selection-change` (collapsed at cursor+1), not a no-op edit — no spurious
`DocumentChanged`.

## 6. Fall-through (the manual no-op)

Manual close key pressed, `find_pair` → `''` (nothing unmatched above): the action
returns **declined** (§5.2). The dispatcher then runs the next binding for the
close key — completion navigation if a menu is up, a newline, or the user's own
remap. The plugin never hardcodes `<C-j>`'s default, so it composes with the rest
of the keymap exactly as the `vim-pairify` `<expr>` fall-through did.

## 7. Scope of the backward scan — lexical, via tree-sitter (the perf spine)

The lesson from `vim-pairify`: on **very large buffers, reading buffer text to
scan costs real time + memory**; even lazy, allocation-free access scales poorly.
The better model — and the **v1 model** here — is to **query the current lexical
scope** (the enclosing function / class / block, via the tree-sitter seam §5.3)
and slice + scan **only that scope's byte range**. This is simultaneously:

- **fast** — bounded to the enclosing scope, so buffer size is irrelevant; the
  `document.slice` reads only the scope's bytes (lazy, no whole-buffer
  allocation), and `find_pair`'s early-exit trims it further; and
- **correct** — a stray unbalanced pair in another function never leaks into the
  result.

This is *why* the tree-sitter seam is a v1 prerequisite, not a v2 nicety: it is
the mechanism that keeps the scan bounded regardless of file size.

**Fallback (no parse tree):** for an unparsed language / scratch buffer where
tree-sitter has no enclosing scope, degrade to a cursor-backward `document.slice`
with `find_pair`'s early-exit and a generous line cap — never a whole-buffer
materialization.

## 8. Capabilities

**`tree-sitter` (editor capability), and nothing else** — no `fs` / `net` /
`proc`. The `auto` style is capability-free (it reads only via the host-owned
`document` handle), but the `manual` style's `enclosing` scope query rides the
tree-sitter seam, which TS.1 §5 gates on the `tree-sitter` editor capability — so
`plugin.toml` declares `editor_capabilities = ["tree-sitter"]`. Bundled ⇒
pre-granted; a `manual`-style user whose grant is withheld degrades to the
line-capped fallback scan (§7), never an error. Still the simplest trust surface
of the bundled set (contrast lighthouse's `net`+`proc`+`fs:write`).

> **Style gating is read at dispatch, not registered per-style (as built,
> AP.3).** §2 sketched "read `auto-pair.style`, register the matching keymap,
> re-register on `OptionChanged`." As built, the mode registers ONE static keymap
> (all pair keys + the close key + `<BS>`) and each action reads the live option
> via `config::get-option` and DECLINES when it shouldn't act (pair keys in
> `manual`; the close key in `auto`). This avoids dynamic keymap re-registration
> and the events-seam subscription entirely; the per-keystroke cost is one
> ConfigRegistry lookup, dwarfed by the WASM round-trip the bound key already
> pays. It required a general enabler — **a grammar guest can now read a config
> option**: `instantiate_grammar_plugin` wires the shared editor `ConfigRegistry`
> into the grammar store (it was `None` before, only the async config seam had
> it). Broadly useful (a comment plugin reading `commentstring`, etc.), not
> auto-pair-specific.

## 9. Paramount-goal alignment

- **#2 Extensibility.** The first first-party feature shipped *as a component*,
  and the vehicle that builds three reusable host capabilities (grammar
  doc-handle; declining bindings; later, scope query) validated on real UX.
- **#1 Performance.** Actions run on the sync grammar trampoline (the one
  on-keystroke seam) but are trivial guest logic under the Reflex budget; the
  backward scan early-exits and is bounded; the boundary-trace `HotGate` keeps it
  free to observe. No async, no I/O. The `document.slice` reads only the bytes the
  scan touches.
- **UX (higher court).** `auto` is the familiar default; `manual` is *surprise-free
  by construction* (§10). Neither ever inserts a character the user didn't intend.

## 10. UX rationale

- **Default `auto`.** Muscle memory across editors is auto; it must be the default
  so nothing surprises a newcomer. `manual` is opt-in via `auto-pair.style`.
- **`manual` is the flagship.** It removes *every* auto-pairing failure mode:
  no spurious closers, no fighting an auto-inserted char, no orphaned closer after
  deleting an opener, no wrong-context insertions. You type exactly your
  keystrokes; the one close key derives the correct closer from the stack and
  closes inside-out on repeat. Correctness is *one* algorithm (§3), not N context
  heuristics.
- **`auto` must still be credible.** A *basic* auto (open-insert + close-skip only)
  feels broken the moment you backspace an opener and get a stray closer. So auto
  v1 includes **backspace-deletes-the-empty-pair**; word-boundary / string-comment
  suppression is v2.
- **The close key (`<C-j>`) falls through** so it stays useful when there's nothing
  to close — the property that makes a single, always-available key ergonomic.

## 11. Rejected alternatives

- **Native auto-pair.** Rejected: would not dogfood the plugin pipeline (the point
  of doing it first) and would not force the reusable host surface (§5) the
  ecosystem needs anyway. Built-in vim grammar stays native; auto-pair is an
  *extension*.
- **Static keymap-only mapping** (`inoremap ( ()<Left>`). Rejected: cannot do
  skip-over, backspace-delete, or the manual backward scan (all need to read
  buffer text). The grammar-action + doc-handle indirection is what buys real
  logic.
- **Per-keystroke guest predicate for fall-through.** Rejected in favor of
  declining bindings (§5.2): a predicate runs a guest call on *every* press of the
  key; declining runs the guest only when the action fires and cleanly resumes the
  layer stack.
- **Events/hooks seam for the pairing.** Rejected: too indirect + too late for a
  per-keystroke edit; the pair must be produced *as* the key is handled (the sync
  grammar action), not observed after.

## 12. Slices

Sequencing, exit criteria, and status live in the slice plan
([`../operations/slice-plans/archive/plugin-auto-pair.md`](../operations/slice-plans/archive/plugin-auto-pair)),
built in two waves:

- **Wave 1 (pipeline proof + `auto` style, shippable):** AP.0.1 grammar-context
  `document` handle (§5.1) → AP.1 crate scaffold + registration → AP.2 `auto` style
  (open-insert + close-skip + backspace-delete-empty-pair).
- **Wave 2 (foundation + `manual` style):** AP.0.2 declining/fall-through bindings
  (§5.2) + AP.0.3 the tree-sitter query seam (§5.3, its own fragment +
  [slice plan](../operations/slice-plans/archive/plugin-treesitter-seam)) → AP.3
  `manual` style (`find_pair`, scope-bounded) → AP.4 bundling.

**Deferred to v2:** wrap-selection (opener with a Visual selection surrounds it),
word-boundary / string-comment suppression (auto), and per-language pair tables +
a user add/remove API. (Lexical-scope scanning via tree-sitter is **v1** — AP.0.3.)
