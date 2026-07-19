# auto-pair — the trivial-first bundled plugin

> **Design fragment.** Contracts, data model, rationale, rejected alternatives,
> paramount-goal alignment. Slice sequencing is the compact §7 below (inline
> while small; splits to `../operations/slice-plans/plugin-auto-pair.md` if it
> grows). Sibling fragments: [`plugin-host.md`](plugin-host.md) (the seam spine),
> [`keymap-architecture.md`](keymap-architecture.md) (the layered keymap the
> insert-mode bindings land in).
>
> **Status: 📝 designed, not built.** The **first real bundled 8b plugin**,
> chosen to de-risk the packaging/load pipeline with **zero new host surface** —
> every seam it uses (grammar `register-action`, keymap `register-binding`) is
> already wired end-to-end. Sequenced BEFORE lighthouse
> ([`lighthouse.md`](lighthouse.md)), whose real cost is a four-seam host
> extension.

## 1. Why (and why *this* one first)

Auto-pairing — type `(`, get `()` with the cursor between — is a universal
editor expectation, and one of design.md §5.5.6's named "editing helpers"
bundled plugins. But its purpose *here* is strategic: it is the **pipeline
validator**. Shipping it proves the whole first-party bundled-plugin path end to
end — a plugin crate → a `wasm32-wasip2` component + `manifest.toml` → on-disk
discovery → `activate` → live seam contributions — on a *real*, user-visible
workload, with **no new host work** (unlike lighthouse, which forces four host
seams). Get this working and the packaging/distribution/load pipeline is proven;
then lighthouse can invest in the host-services extension against a known-good
pipeline.

It also validates two things a pure-grammar plugin wouldn't: the **insert-mode
keymap-binding** path and **insert-mode plugin-action dispatch** (a plugin action
that runs on a printable keystroke and returns an edit).

## 2. What it does (v1 — tight)

- Type an **opener** (`(` `[` `{` `"` `'` `` ` ``) → insert the matching closer
  and leave the cursor **between** the pair.
- Type a **closer** (`)` `]` `}` `"` `'` `` ` ``) when the character right after
  the cursor **is that closer** → step **over** it (move the cursor right)
  instead of inserting a second one.

That is the whole v1 contract. It matches the muscle-memory minimum every editor
provides; nothing surprising, nothing that inserts a character the user didn't
expect.

### The pair table

| Opener | Closer | Notes |
|---|---|---|
| `(` | `)` | |
| `[` | `]` | |
| `{` | `}` | |
| `"` | `"` | same char open/close |
| `'` | `'` | same char open/close |
| `` ` `` | `` ` `` | same char open/close |

Same-char pairs (`"`/`'`/`` ` ``) get the skip-over rule but **not** a naive
"always open" — v1 keeps it simple (open on type, skip on the matching next
char); the "don't open a second quote right after a word" refinement is v2.

## 3. Seam usage (the mechanism)

Two already-wired seams, no host changes:

1. **grammar `register-action`** — the plugin registers one action per behavior.
   An action's `apply-action(ctx)` reads the projected `action-context` (cursor +
   buffer snapshot) and returns an `Effect` (an edit):
   - `auto-pair-open-<name>` → an edit inserting `"<open><close>"` at the cursor
     and positioning the caret between them.
   - `auto-pair-close-<name>` → if `char-after-cursor == <close>`, an effect that
     moves the caret right (skip-over); else an edit inserting `<close>`.
2. **keymap `register-binding`** — bind the printable openers/closers in
   **Insert** `binding-mode` to those action command names:
   `register-binding(insert, "(", "auto-pair-open-paren")`, … The binding lands in
   the plugin's `KeymapLayer::User`-equivalent, above the default insert-mode
   self-insert, so the action fires instead of the raw character insert.

The keystroke path: insert-mode `(` → the keymap resolves it to the plugin's
action command → the grammar dispatcher runs `apply-action` (the **sync
trampoline** — this is why grammar is the one synchronous seam) → the edit
`Effect` crosses back and the host applies it. All within the Reflex sub-frame
budget; the action's guest logic is trivial (a couple of field reads + one edit),
far inside the fuel/epoch bound.

> **Build-time validation gate.** The one assumption to confirm early: an
> insert-mode `register-binding` to a plugin **action** actually intercepts a
> printable key before the default self-insert, and the action's edit `Effect`
> applies in insert mode. If insert-mode plugin-action dispatch turns out
> unwired, that is a small host slice (wiring the insert-layer keymap → grammar
> action path) — surfaced here so it is designed, not discovered mid-build.

## 4. Capabilities

**None.** Auto-pair is pure editing — it reads the buffer snapshot the action
context already projects and returns edits. Its `manifest.toml` requests **zero**
capabilities: no `fs`, no `net`, no `proc`. This is the point — the simplest
possible trust surface for the pipeline validator. (Contrast lighthouse:
`net:http` + `proc:spawn` + `fs:write`.)

## 5. Paramount-goal alignment

- **#2 Extensibility.** The end-to-end proof that a first-party feature ships *as
  a component* — the bundled-plugin pipeline (build → manifest → discovery →
  activate → contribute) exercised on a real workload, dogfooding the host with
  zero new surface.
- **#1 Performance.** The actions run on the sync grammar trampoline (the one
  on-keystroke seam) but are trivial guest logic under the Reflex budget; the
  boundary-trace `HotGate` keeps them free to observe. No async, no I/O.
- **UX (higher court).** Correct-by-construction v1: only ever inserts the closer
  the user's opener implies, only ever skips a closer that is already there. No
  surprise edits — the veto bar for an editing helper.

## 6. Rejected alternatives

- **Native auto-pair.** Rejected: it would not dogfood the bundled-plugin
  pipeline, which is the entire reason to build this first. (Built-in vim grammar
  stays native by design; auto-pair is an *extension*, the right place to prove
  the plugin path.)
- **A static keymap-only mapping** (`inoremap ( ()<Left>`-style). Rejected: it
  cannot do the **skip-over** rule (which needs to read the char after the cursor)
  and has no path to the v2 context refinements. The grammar-action indirection is
  what buys real logic.
- **The events/hooks seam** (react to an insert event). Rejected: too indirect
  and too late for a per-keystroke edit — the pair must be inserted *as* the key
  is handled, not observed after the fact; that is exactly what the sync grammar
  action provides.
- **A pure-grammar plugin (a text-object/ex-command) as the pipeline validator.**
  Rejected as the first pick: it validates less — no insert-mode binding, no
  insert-mode action dispatch — so auto-pair de-risks more of the pipeline for the
  same packaging cost.

## 7. Slices

Small enough to sequence inline; splits to a slice-plan file if v2 grows it.

- **AP.1 — crate scaffold + the pipeline** 📝: `plugins/auto-pair/` guest
  (`wasm32-wasip2`, `crate-type = ["cdylib"]`, `wit-bindgen`) + `manifest.toml`
  (id `auto-pair`, `provides = ["grammar", "keymap"]`, **no** capabilities);
  `register-grammar` registers the open/close actions, `register-keymap` binds the
  insert-mode chords. **Exit:** the crate builds to a component; a host round-trip
  test loads it via the loader and the actions + bindings register (provenance
  `SourceLayer::Plugin`).
- **AP.2 — the behavior + the insert-mode dispatch proof** 📝: the open-insert +
  close-skip action bodies; the build-time gate (§3) proven — an insert-mode `(`
  dispatches the plugin action and the pair lands, a `)` before a `)` skips. If
  insert-mode plugin-action dispatch needs host wiring, that host slice lands
  here. **Exit:** typing `(` yields `()` with the caret between; typing `)` at a
  `)` steps over; asserted end-to-end through the loaded plugin.
- **AP.3 — bundling** 📝: ship `auto-pair.wasm` compiled-in / in `core-plugins/`,
  loaded pre-granted at boot (it needs no grant). **Exit:** a fresh editor
  auto-pairs out of the box; `:plugins` lists it.

**Deferred to v2:** wrap-selection (type `(` with a Visual selection → surround
it), backspace-deletes-the-empty-pair, don't-open before a word char, string/
comment suppression, and per-language pair tables.
