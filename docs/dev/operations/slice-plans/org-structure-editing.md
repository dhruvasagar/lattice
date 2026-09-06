# Org structure editing — headlines, lists and checkboxes — slice plan

> **Status: Active.** Opened 2026-09-06. Implements
> [`org-mode.md`](../../architecture/org-mode.md) §5.6.

> **For agentic workers:** REQUIRED SUB-SKILL — use
> `superpowers:subagent-driven-development` (recommended) or
> `superpowers:executing-plans` to implement this plan slice-by-slice. Steps
> use checkbox (`- [ ]`) syntax for tracking.

Design owns *what* and *why*; this file owns *when*, *in what order*, and
*how*.

**Plan location note.** The `writing-plans` default is
`docs/superpowers/plans/`; this repo's convention (CLAUDE.md, "Doc
organisation") is design fragment + slice plan, two files, and a third
parallel plan document is exactly the drift that convention exists to
prevent. The execution detail is folded in here.

**Goal.** Give org the list half of its structure editing — insert, indent,
move, cycle, convert — with headline and checkbox verbs unified under the
same context-dispatched gestures, working in Insert, Normal and Visual across
both renderers.

**Architecture.** A new `Lists` model in the plugin (tree-first, indent
fallback) that `Checkboxes` is rebuilt on. Gesture-named actions dispatch on
what is under the cursor, calling the bodies the dedicated chords already
call. Three host slices unblock chords and a context field that no plugin can
supply for itself.

**Tech stack.** Rust → `wasm32-wasip2` component (plugin); crossterm 0.28
(TUI terminal setup); WIT component model (the guest seam).

---

## Ownership invariant

**Every org verb, chord and handler body lands in `lattice-org-plugin`.** The
three host slices add no org logic — they are substrate a WASM guest cannot
supply for itself: a plugin cannot push terminal escape sequences, and cannot
add a field to its own inbound context.

The acid test from CLAUDE.md, run against this plan:

| Check | Result |
|---|---|
| New `Editor::` methods in `lattice-host` | **0** |
| New variants in the host's `Action` enum | **0** |
| Host code that knows what a headline, list item or bullet is | **0** |
| Host code that names `org` | **0** |
| New crates | **0** |

Each host slice states what it adds and why it is generic:

- **OS.0** — tests only, over a fixture plugin. No production code unless it
  finds a defect, in which case the defect is in generic Insert-mode dispatch.
- **OS.1** — terminal setup in `runtime.rs` plus one `ui.*` option. Makes
  `<S-CR>` / `<C-CR>` / `<M-S-CR>` deliverable **for every consumer**;
  `lattice-protocol` has always spelled them and GPUI has always sent them.
- **OS.2** — one field on `lattice-grammar`'s `ActionContext` and its WIT
  mirror, populated from the resolver that already exists. Generic: any
  plugin's Visual-mode action gets it.

**OS.11 touches `docs/user/org.md`** in this repo. That is a pre-existing
lattice-owned overview page that already describes org; extending one table
row is documentation, not ownership.

## Global constraints

- **`cargo test` in the plugin repo loads a prebuilt artefact.** Every plugin
  slice must run `cargo build --release --target wasm32-wasip2` **before**
  `cargo test`, or the suite tests the previous binary and passes against
  broken code (`cargo-test-does-not-rebuild-a-loaded-artefact`).
- **One slice, one commit**, message explaining why that slice exists.
- **Gates before committing, not after**: `scripts/precommit.sh <crate>…` in
  the lattice repo; `cargo fmt --all && cargo clippy --all-targets && cargo
  test` in the plugin repo. Zero new rustc warnings in touched code. Do not
  chase the workspace's known `unwrap_used` / `panic` residue.
- **Never `git add -A`.** Stage by explicit path — `docs/dev/notes/todo.org`
  is Dhruva's scratch file and must never be committed (`never-commit-todo-org`).
- **Test by pressing the chord**, not by dispatching the action by name. A
  name-dispatch test passes against a mode-scoping bug, a missing binding and
  a dead prefix alike.
- **Option naming is underscore-within-namespace**: `ui.keyboard_enhancement`,
  matching `ui.nerd_fonts`. Org's own options stay `org.*` and dashed, as they
  already are.
- **Effects an action returns must be applied in tests.** Dropping them is
  indistinguishable from a broken feature
  (`dropped-renderer-effects-look-like-dead-features`).

### The gate, spelled once

Every slice ends with this. Steps below say "run the gate" and name only the
files to stage, because the commands never vary.

**In `lattice-org-plugin` (OS.3–OS.9, OS.10, and OS.11's plugin half):**

```bash
cd ~/src/dhruvasagar/lattice-org-plugin
cargo build --release --target wasm32-wasip2   # FIRST -- tests load this artefact
cargo fmt --all
cargo clippy --all-targets
cargo test
git add <the files this slice names>            # never -A
git commit
```

**In `lattice` (OS.0, OS.1, OS.2, OS.11's host half):**

```bash
cd ~/src/dhruvasagar/lattice
scripts/precommit.sh <the crates this slice names>
git add <the files this slice names>            # never -A
git commit
```

If `precommit.sh` refuses with "another cargo job", check with `pgrep` first —
it matches `.cargo/bin` in PATH env dumps and false-positives. If nothing is
really running, `PRECOMMIT_ALLOW_CONCURRENT=1`
(`precommit-concurrency-false-positive`).

## Test helpers — what exists, what you write

The tests below call helpers by name. **Reuse what is there; do not add a
parallel set.** In `lattice-org-plugin/tests/org_structure.rs`:

| Helper | Status |
|---|---|
| `boot_sealed_editor()` | **exists** (~line 40) — seals off the real `~/.config/lattice`; autoload is disabled inside it |
| `org_plugin_wasm() -> Option<Vec<u8>>` | **exists** (~line 46) — `None` when the component was not built, which is how every test in this file skips |
| `loader_over_editor(&Editor, &Path)` | **exists** (~line 55) |
| `press(&mut Editor, &str)` | **exists** (~line 219) — expands `<leader>`, dispatches each chord. Does **not** apply renderer effects |
| `press_chord(&mut Editor, &str)` | **exists** (~line 321) — `press` plus `apply_renderer_effects`. Use this for anything that edits |
| `chord(&str) -> KeyChord` | **exists** (~line 209) |
| `org_editor(text) -> Option<Editor>` | **write it** — boot + load org + open a buffer holding `text` with `org-mode` active. Every existing test does this inline; factor it out in OS.3 and reuse it. Returns `None` when `org_plugin_wasm()` does, so callers keep the `let Some(..) else { return }` skip |
| `org_editor_with_keywords(text, kw)` | **write it (OS.5)** — as above, plus `:set org.todo-keywords=…` before the buffer opens |
| `goto(&mut Editor, line, byte)` | **write it (OS.4)** — set the cursor without dispatching motions, so a test's setup cannot fail for a motion's reasons |
| `text(&Editor) -> String` | **write it (OS.4)** — the active buffer's whole text |
| `line_at(&Editor, n) -> String` | **write it (OS.4)** |
| `cursor(&Editor) -> (u32, u32)` | **write it (OS.4)** |
| `last_echo(&Editor) -> Option<String>` | **write it (OS.6)** — the most recent `Effect::Echo`. Needed by every refusal test: asserting "nothing changed" alone passes against a chord that is simply unbound |

In `list.rs`'s own test module (OS.3):

| Helper | Status |
|---|---|
| `lists_over(text) -> Lists<'_>` | **write it** — build a `Lists` over `text` with **`tree: None`**, exercising the indent fallback |
| `lists_over_parsed(text)` | **write it** — the same fixture with a real `TreeSnapshot`. Every structural test runs through **both**; the fallback is the half that silently rots, and a tree-only test would never notice |

In `crates/lattice-host/tests/plugin_insert_mode_chords.rs` (OS.0):

| Helper | Status |
|---|---|
| `boot_sealed_editor_with_fixture()` | **write it** — reuse the loader helper the existing fixture tests use; check `crates/lattice-plugin-host/tests/` for the pattern rather than inventing one |
| `fixture_action_fired(&Editor) -> bool` | **write it** — however the existing multiseam fixture records that a callback ran |
| `set_buffer_text` / `buffer_text` | **write it** — or reuse equivalents if the host test suite already has them |

---

## Status

| Slice | Title | Status |
|---|---|---|
| OS.0 | An Insert-mode plugin chord reaches a grammar action — pin it **(host)** | ✅ |
| OS.0b | An ALT-bearing chord can be bound at all **(host)** — *carved from OS.0's finding* | 📝 |
| OS.1 | The keyboard protocol, so Shift+Enter exists at all **(host)** | 📝 |
| OS.2 | A Visual-mode plugin action can see its region **(host)** | 📝 |
| OS.3 | `Lists` — the model, and `Checkboxes` rebuilt on it **(plugin)** | 📝 |
| OS.4 | `<M-CR>` — meta-return dispatches on what is at point **(plugin)** | 📝 |
| OS.5 | `<M-S-CR>` — the variant, and the headline insert family **(plugin)** | 📝 |
| OS.6 | The Meta-arrows: promote/demote *is* indent/outdent **(plugin)** | 📝 |
| OS.7 | `<M-Up>` / `<M-Down>` — move an item or a subtree **(plugin)** | 📝 |
| OS.8 | `<C-t>` / `<C-d>` in Insert, declining off a list **(plugin)** | 📝 |
| OS.9 | Bullet cycling, and line ↔ item ↔ headline **(plugin)** | 📝 |
| OS.10 | The Visual peers **(plugin)** | 📝 |
| OS.11 | `:help org` — the Lists section, and the site **(plugin + host)** | 📝 |

## Dependencies

**OS.3 is the gate for everything in the plugin.** Nothing below it can be
written against a list the plugin cannot see.

- **OS.0 blocked OS.4 and OS.8 and has now answered.** ✅ Declines in Insert
  DO fall through, so **OS.8 is unblocked**. ALT-bearing Insert chords do
  **not** dispatch, so **OS.4 and OS.5 are blocked on OS.0b** — `<M-CR>` and
  `<M-S-CR>` cannot fire until it lands. This is the slice working: it cost
  one test file to learn, instead of eight dead bindings.
- **OS.0b blocks OS.4 and OS.5.** Nothing else — OS.6/OS.7 bind in Normal
  (a different dispatch path), OS.8 uses CTRL, which was never stripped.
- **OS.2 blocks OS.10** and nothing else.
- **OS.1 blocks nothing.** `<M-S-CR>` is unreachable in the TUI without it,
  reachable in GPUI and through `<leader>o…` either way, so OS.5 lands green
  regardless. Sequenced early because it is small and because testing OS.5 by
  keypress in a terminal wants it.
- **OS.4 → OS.5.** The variant is an arm on the table OS.4 builds.
- **OS.6, OS.7, OS.8, OS.9 are independent of each other** — four disjoint
  verb groups over one model. OS.8 reuses OS.6's bodies, so it wants OS.6
  first, but nothing else orders them.
- **OS.11 last**, because it documents what actually landed.

The three host slices are independent of one another and may land in any
order, or in parallel with OS.3.

## File structure

**lattice (host)**

| File | Responsibility | Slice |
|---|---|---|
| `crates/lattice-host/tests/plugin_insert_mode_chords.rs` | *create* — pins Insert-mode plugin dispatch + decline fall-through | OS.0 |
| `crates/lattice-ui-tui/src/runtime.rs` | *modify* — push/pop keyboard enhancement flags around the session | OS.1 |
| `crates/lattice-host/src/ui/theme_options.rs` | *modify* — `ui.keyboard_enhancement` declaration | OS.1 |
| `crates/lattice-grammar/src/registry.rs` | *modify* — `selection` field on `ActionContext` | OS.2 |
| `crates/lattice-grammar/src/dispatcher.rs` | *modify* — populate it on the action path | OS.2 |
| `wit/types.wit` | *modify* — `selection` on the `action-context` record | OS.2 |
| `crates/lattice-plugin-host/src/boundary_grammar.rs` | *modify* — mirror it in `project_action_context` | OS.2 |
| `docs/user/org.md` | *modify* — one table row | OS.11 |

**lattice-org-plugin**

| File | Responsibility | Slice |
|---|---|---|
| `src/list.rs` | *create* — `Bullet`, `Item`, `Lists`, `renumber`. Pure logic, no effects | OS.3 |
| `src/checkbox.rs` | *modify* — `Checkboxes` rebuilt over `Lists`; tally/cookie logic unchanged | OS.3 |
| `src/lib.rs` | *modify* — action ids, `register_action` calls, keymap binds, handler bodies | OS.4–OS.10 |
| `tests/org_structure.rs` | *modify* — every chord test | OS.3–OS.10 |
| `doc/org.md` | *modify* — the Lists section | OS.11 |

**On `lib.rs`.** It is 8,776 lines, well past this repo's own 800-line
guideline, and this plan adds roughly another 700. The established pattern in
this plugin is deliberate and consistent — **pure logic in modules
(`headline.rs`, `checkbox.rs`, `todo.rs`), `Effect`-returning action bodies in
`lib.rs`** — and unilaterally inverting it mid-feature is the unrelated
refactoring this plan should not do. So: new logic goes in `list.rs`, new
action bodies follow the pattern into `lib.rs`, and **extracting action bodies
by feature is recorded here as worth doing and explicitly out of scope.** If
that extraction is wanted, it is its own plan, run against the whole file
rather than the part this feature happens to touch.

---

## OS.0 — An Insert-mode plugin chord reaches a grammar action **(host)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.2.

**A test slice, deliberately, and it may become a fix.**

`keymap_insert.rs:617`'s `action_from_bound` builds `Action::Invoke(inv)`,
which resolves through the unified dispatcher against the grammar registry,
where a plugin's `register_action` entries live. So it *should* work. But
`plugin-actions-need-a-dispatch-fallback` records this class biting twice
(`ActionHandlerRegistry` lookups missing plugin grammar actions; prompt
submits and transient rows silently doing nothing), and the symptom is a
chord that does nothing — indistinguishable from one that is unbound.

**Files**
- Create: `crates/lattice-host/tests/plugin_insert_mode_chords.rs`

**Interfaces**
- Consumes: the existing multiseam test fixture plugin
  (`wit/multiseam-fixture.wit` and its test component) — reuse it rather than
  minting a new fixture; check `crates/lattice-plugin-host/tests/` for the
  loader helper the existing fixture tests use.
- Produces: nothing consumed by later slices. This slice produces a *fact*:
  either both behaviours hold, or OS.4 and OS.8 are blocked pending a fix.

- [ ] **Step 1: Write the failing test for Insert-mode dispatch**

A fixture plugin declaring a mode with
`ModeKeymapBinding { binding_mode: Insert, chord: "<M-CR>", command: "<fixture-action>" }`.
Boot a sealed editor, open a buffer with that mode active, enter Insert, press
the chord, assert the guest action ran.

```rust
#[tokio::test(flavor = "multi_thread")]
async fn an_insert_mode_plugin_chord_reaches_its_guest_action() {
    let mut editor = boot_sealed_editor_with_fixture().await;
    press(&mut editor, "i");                 // Normal -> Insert
    press(&mut editor, "<M-CR>");
    assert!(
        fixture_action_fired(&editor),
        "an Insert-mode plugin binding must reach apply-action"
    );
}
```

- [ ] **Step 2: Write the failing test for decline fall-through in Insert**

Bind the fixture's action to `<C-t>` in Insert and have it return
`Effect::Declined`. `<C-t>` is a Builtin Insert binding (indent by
`shiftwidth`, `keymap_entry.rs:473`), so the assertion is that the *builtin*
ran.

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_declined_insert_chord_falls_through_to_the_builtin() {
    let mut editor = boot_sealed_editor_with_fixture().await;
    set_buffer_text(&mut editor, "hello\n");
    press(&mut editor, "i");
    press(&mut editor, "<C-t>");
    assert_eq!(
        buffer_text(&editor), "    hello\n",
        "Declined in Insert must reach the builtin shiftwidth indent"
    );
}
```

- [ ] **Step 3: Run both, record what actually happens**

```bash
cargo test -p lattice-host --test plugin_insert_mode_chords
```

Both passing is the expected outcome and the slice is done at Step 5. If
either fails, **stop and report before writing a fix** — a failure here
changes OS.4's and OS.8's shape, and the fix belongs in generic Insert-mode
dispatch, not in anything org-shaped.

- [ ] **Step 4: Run the gate**

```bash
scripts/precommit.sh lattice-host
```

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-host/tests/plugin_insert_mode_chords.rs
git commit
```

Message says what was pinned and why it was in doubt — that this class has
silently failed twice, and that eight bindings are about to depend on it.

## OS.0b — An ALT-bearing chord can be bound at all **(host)** 📝

**Carved from OS.0's finding, 2026-09-06.** Not in the original plan; OS.0
exists to surface exactly this class and did.

`normalize_for_insert_lookup` (`keymap_insert.rs:594-609`) strips ALT and
SUPER off every incoming chord **before any lookup**, so an ALT-bearing
binding registers correctly into the trie and can never fire. Its stated
rationale — *"no Insert binding (base or overlay) uses them"* — was true of
the BUILTINS and was falsified by the `modes` WIT seam, which lets a plugin
declare `binding-mode: insert`. A seam that accepts a registration and then
silently drops every keystroke is the failure
`plugin-gates-hand-guests-throwaway-contexts` names.

**Wider than Insert.** `dispatch_insert` also serves `ModalState::Command`,
`Search` and `Prompt` (`input.rs:378`, `:400`, `:411`, `:435`), and
`keymap_select.rs:285` reuses the same normalize. Five modal states, every
consumer, both renderers — this is not an org bug.

**Files**
- Modify: `crates/lattice-host/src/keymap_insert.rs` — `dispatch_insert`'s
  **three** lookup sites: the partial-chord branch (~line 481), the
  single-chord lookup (~line 492), and `resolve_native_action`'s
  fall-through re-resolve (~line 556)
- Modify: `crates/lattice-host/src/keymap_select.rs` — the peer at ~line 285
- Modify: `crates/lattice-host/tests/plugin_insert_mode_chords.rs` — **remove
  the `#[ignore]`**; that test passing is this slice's acceptance criterion
- Test: `keymap_insert.rs`'s own `#[cfg(test)]` module

**Interfaces**
- Produces: no signature changes. `normalize_for_insert_lookup` stays exactly
  as it is and keeps its callers; what changes is that it is now a *fallback*
  rather than a precondition.

- [ ] **Step 1: Un-ignore OS.0's test and watch it fail**

```bash
cargo test -p lattice-host --test plugin_insert_mode_chords -- --include-ignored
```

Expected: `an_insert_mode_plugin_chord_reaches_its_guest_action` FAILS with
`last_message` showing the echo `i` left behind rather than the fixture's.
This is the driver; do not weaken it.

- [ ] **Step 2: Write the regression tests for what must NOT change**

These are the point of choosing raw-then-fallback over deleting the strip.
Each asserts a behaviour that exists today:

```rust
#[test]
fn alt_enter_still_reaches_the_builtin_newline_when_nothing_binds_it() {
    // <M-CR> unbound anywhere -> falls back to normalized <CR> -> Builtin.
}

#[test]
fn alt_x_still_types_a_literal_x() {
    // Unbound either way; `literal_text_fallback` inserts "x".
}

#[test]
fn shift_tab_is_unaffected() {
    // SHIFT was never stripped; raw lookup finds it on the first try.
}

#[test]
fn the_ctrl_x_ctrl_o_two_chord_still_resolves() {
    // The partial-chord branch must get the same treatment as the
    // single-chord one, or a multi-key ALT chord dies at its prefix.
}
```

- [ ] **Step 3: Implement raw-then-fallback at every lookup site**

```rust
// Look the chord up AS IT ARRIVED first, so a layer that deliberately
// bound an ALT chord is reachable. Fall back to the normalized form
// only when the raw lookup found nothing AND normalizing would
// actually change the chord -- so a chord carrying neither ALT nor
// SUPER costs exactly one lookup, as it always did.
//
// `Partial` counts as a hit: an ALT-bearing PREFIX is a deliberate
// registration, and falling back mid-sequence would strand its
// continuation.
let looked = normalize_for_insert_lookup(*chord);
let raw = handle.lookup_with_context(BindingMode::Insert, &[*chord], active_minor_modes);
let result = match raw {
    LookupResult::Bound { .. } | LookupResult::Partial => raw,
    _ if looked != *chord => {
        handle.lookup_with_context(BindingMode::Insert, &[looked], active_minor_modes)
    }
    other => other,
};
```

Apply the same shape at all three sites in `keymap_insert.rs` and at
`keymap_select.rs:285`. Factor it into one helper rather than pasting it four
times — four copies of a lookup rule is how one of them drifts.

- [ ] **Step 4: Correct the module docstring**

The file's own header states the stripped-modifier rule as by-design. Update
it to say what is now true: builtins use neither ALT nor SUPER, so the
normalized form remains the fallback, but a mode or plugin layer may bind
them and the raw chord is tried first. Leaving a docstring that contradicts
the code is how the next reader re-introduces this.

- [ ] **Step 5: Run everything, including the five modal states**

```bash
cargo test -p lattice-host --test plugin_insert_mode_chords -- --include-ignored
cargo test -p lattice-host keymap_insert
cargo test -p lattice-host keymap_select
scripts/precommit.sh lattice-host
```

`dispatch_insert` serves Command, Search and Prompt as well as Insert, so a
regression there will surface as a command-line or prompt test failing.
Treat any such failure as this slice's, not as flake.

- [ ] **Step 6: Commit**

Stage `crates/lattice-host/src/keymap_insert.rs`,
`crates/lattice-host/src/keymap_select.rs`, and
`crates/lattice-host/tests/plugin_insert_mode_chords.rs`. The message records
that the stripped-modifier rule was a true statement about builtins that the
`modes` seam falsified, and that the symptom was a binding which registers
and then silently never fires.

## OS.1 — The keyboard protocol, so Shift+Enter exists at all **(host)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.1.

`runtime.rs:141-152` sets up raw mode, the alternate screen, bracketed paste
and mouse capture, and never pushes `KeyboardEnhancementFlags`. Without them a
terminal cannot express Shift+Enter or Ctrl+Enter — it sends a bare `\r`, so
`<S-CR>`, `<C-CR>` and `<M-S-CR>` are unreachable for **every** consumer.

**Files**
- Modify: `crates/lattice-ui-tui/src/runtime.rs` (setup ~line 141-152;
  teardown ~line 190-200)
- Modify: `crates/lattice-host/src/ui/theme_options.rs` (option declaration)
- Test: `crates/lattice-ui-tui/src/runtime.rs` (`#[cfg(test)]` module) and
  the existing chord round-trip tests in `crates/lattice-ui-tui/src/chord.rs`

**Interfaces**
- Produces: `UiKeyboardEnhancement: bool = true` (`ui.keyboard_enhancement`),
  read by `runtime.rs` at session setup. Nothing else consumes it.

- [ ] **Step 1: Declare the option**

In `theme_options.rs`, alongside `UiNerdFonts`, using the same macro form:

```rust
/// Whether to request the terminal's keyboard-enhancement protocol
/// (the "kitty protocol") when the terminal reports support for it.
///
/// `true` (default) -- push `DISAMBIGUATE_ESCAPE_CODES` when
/// `supports_keyboard_enhancement()` says yes. This is what makes
/// `<S-CR>`, `<C-CR>` and `<M-S-CR>` distinguishable from a bare
/// `<CR>`; without it every terminal sends the same `\r` for all four.
///
/// `false` -- never push it. Set this if a terminal answers the
/// support probe wrongly and keys start arriving mangled; recovering
/// from that should not need a rebuild.
#[name("ui.keyboard_enhancement")]
pub UiKeyboardEnhancement: bool = true;
```

- [ ] **Step 2: Write the failing round-trip test**

The regression risk is that disambiguation changes how Esc and the C0
controls arrive, so the guard is that existing chords still decode to the same
`KeyChord`. Extend `chord.rs`'s test module:

```rust
#[test]
fn disambiguated_events_decode_to_the_same_chords() {
    // Under DISAMBIGUATE_ESCAPE_CODES crossterm reports Kind::Press
    // explicitly and may carry state bits; the adapter must ignore both.
    for (code, mods) in [
        (KeyCode::Esc, KeyModifiers::NONE),
        (KeyCode::Char('c'), KeyModifiers::CONTROL),
        (KeyCode::Enter, KeyModifiers::NONE),
        (KeyCode::Tab, KeyModifiers::SHIFT),
    ] {
        let plain = KeyEvent::new(code, mods);
        let disambiguated = KeyEvent {
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
            ..plain
        };
        assert_eq!(from_event(&plain), from_event(&disambiguated));
    }
}

#[test]
fn shift_enter_is_a_distinct_chord_from_enter() {
    let enter = from_event(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let s_enter = from_event(&KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
    assert_ne!(enter, s_enter);
    assert_eq!(s_enter.unwrap().to_string(), "<S-CR>");
}
```

- [ ] **Step 3: Run to verify the round-trip test fails or passes**

```bash
cargo test -p lattice-ui-tui chord::
```

`shift_enter_is_a_distinct_chord_from_enter` should already PASS — the adapter
has always handled it; it is the *terminal* that could not send it. That test
is the guard, not the driver. If it fails, the adapter has a bug and this
slice fixes it first.

- [ ] **Step 4: Push the flags at setup**

In `runtime.rs`, after `EnableBracketedPaste`. Guarded by both the option and
the probe, and **recording what was pushed** so teardown pops exactly that:

```rust
// Without this, a terminal cannot express Shift+Enter or Ctrl+Enter --
// it sends the same `\r` as Enter alone, so `<S-CR>` / `<C-CR>` /
// `<M-S-CR>` are unreachable however they are bound. `lattice-protocol`
// has always spelled them and GPUI has always delivered them; this is
// the renderer asymmetry, not a new capability.
//
// DISAMBIGUATE_ESCAPE_CODES only. REPORT_ALL_KEYS_AS_ESCAPE_CODES would
// route ordinary text input through the escape path, which is not wanted.
// `let mut` -- the push can still be refused after the probe said yes,
// and teardown must pop exactly what was pushed.
//
// Read the option the way this crate already reads its neighbours:
// `get_typed::<T>()` answers `Option<&T>` (see `theme_options.rs`'s own
// tests), so an unregistered option degrades to the default rather than
// panicking.
let mut keyboard_enhanced = config
    .get_typed::<UiKeyboardEnhancement>()
    .map(|v| *v)
    .unwrap_or(true)
    && crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
if keyboard_enhanced {
    // Log + skip rather than propagate: a terminal that advertises
    // support and then refuses the push must not fail startup.
    if let Err(e) = execute!(
        stdout,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    ) {
        tracing::debug!("keyboard enhancement push refused: {e}");
        keyboard_enhanced = false;
    }
}
```

- [ ] **Step 5: Pop at teardown, on every exit path**

Before `DisableBracketedPaste`, and it must run on the panic path too — the
alternate-screen restore is already positioned for that reason and this
follows it there:

```rust
if keyboard_enhanced {
    let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
}
```

Read the existing restore path first and put the pop with the alt-screen
leave, whatever shape that path has (panic hook, guard type, or explicit
call). A terminal left in enhancement mode after a crash is one the user
cannot type out of, which is a worse failure than the one being fixed.

- [ ] **Step 6: Test the gate logic**

```rust
#[test]
fn the_option_off_means_no_push_even_when_supported() {
    assert!(!should_push_enhancement(/* option */ false, /* probe */ true));
}

#[test]
fn an_unsupporting_terminal_is_not_pushed_to() {
    assert!(!should_push_enhancement(true, false));
}

#[test]
fn a_supporting_terminal_with_the_option_on_is_pushed_to() {
    assert!(should_push_enhancement(true, true));
}
```

Extract the two-condition gate as `fn should_push_enhancement(option: bool,
supported: bool) -> bool` so it is testable without a tty.

- [ ] **Step 7: Run the gate**

```bash
scripts/precommit.sh lattice-ui-tui lattice-host
```

- [ ] **Step 8: Manual check, because a tty is not testable in CI**

In a supporting terminal (kitty, ghostty, wezterm, alacritty, foot, iTerm2
3.5+): start lattice, confirm normal editing is unaffected, quit, and confirm
the shell still behaves. Then `:set ui.keyboard_enhancement=off`, restart,
confirm the same. Record the terminal you verified in the commit message.

- [ ] **Step 9: Commit**

```bash
git add crates/lattice-ui-tui/src/runtime.rs \
        crates/lattice-ui-tui/src/chord.rs \
        crates/lattice-host/src/ui/theme_options.rs
git commit
```

**Renderer parity: none needed**, and say so in the message. This is TUI-side
terminal setup; GPUI has always delivered these chords, and closing that
asymmetry is the whole slice.

## OS.2 — A Visual-mode plugin action can see its region **(host)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.5.

`lattice-mode`'s `ActionContext` has carried `selection: Option<Range>` since
MG.18e (magit region staging); `lattice-grammar`'s — the one a *plugin* action
arrives through — never gained it, so the WIT mirror has nothing to copy and a
Visual-mode plugin action sees strictly less than the same action reached
natively. Verbatim the position OC.10 fixed for `ex-command-context`.

**Files**
- Modify: `crates/lattice-grammar/src/registry.rs` (`ActionContext`, ~line 569)
- Modify: `crates/lattice-grammar/src/dispatcher.rs` (action dispatch path;
  the resolver is at ~line 756)
- Modify: `wit/types.wit` (`action-context` record, ~line 1578)
- Modify: `crates/lattice-plugin-host/src/boundary_grammar.rs`
  (`project_action_context`, line 166)
- Test: `crates/lattice-plugin-host/src/boundary_grammar.rs` test module, and
  a dispatch test in `crates/lattice-grammar/src/dispatcher.rs`

**Interfaces**
- Produces, consumed by OS.10:
  - Native: `ActionContext.selection: Option<lattice_protocol::position::Range>`
  - WIT: `action-context.selection: option<range>`
  - Guest-side (org): `ctx.selection` is `Option<Range>` with `start` / `end`
    of type `Position { line: u32, byte: u32 }`.
  - Contract: `Some` only when the action fired from a Visual/Select chord;
    `None` in Normal and on every non-chord firing path (prompt submit,
    transient item, `Confirm` yes-action) — matching the documented contract
    of the `lattice-mode` field it mirrors.

- [ ] **Step 1: Write the failing projection test**

```rust
#[test]
fn a_visual_selection_is_mirrored_to_the_guest() {
    let mut ctx = native_action_context();
    ctx.selection = Some(Range::new(pos(2, 0), pos(4, 7)));
    let wit = project_action_context(&ctx).unwrap();
    let sel = wit.selection.expect("selection mirrored");
    assert_eq!((sel.start.line, sel.start.byte), (2, 0));
    assert_eq!((sel.end.line, sel.end.byte), (4, 7));
}

#[test]
fn no_selection_projects_as_none() {
    let ctx = native_action_context();          // selection: None
    assert!(project_action_context(&ctx).unwrap().selection.is_none());
}
```

- [ ] **Step 2: Run to verify it fails**

```bash
cargo test -p lattice-plugin-host boundary_grammar
```

Expected: FAIL to compile — `ActionContext` has no field `selection`, and
`WitActionContext` has no field `selection`.

- [ ] **Step 3: Add the native field**

`registry.rs`, on `ActionContext`, with a doc comment that names the contract
and the precedent:

```rust
/// OS.2: the active region — the Visual/Select selection extent,
/// normalised so `start <= end`. `None` in Normal mode and on every
/// non-chord firing path.
///
/// The peer of `lattice_mode::ActionContext::selection` (MG.18e), which
/// native mode handlers have had since magit needed to stage part of a
/// hunk. A plugin action reached the same way saw strictly less — the
/// position OC.10 fixed for `ex-command-context`.
pub selection: Option<lattice_protocol::position::Range>,
```

Fix every construction site the compiler names; `None` is correct for all of
them except the one in Step 4.

- [ ] **Step 4: Populate it on the action dispatch path**

In `dispatcher.rs`, where the action's `ActionContext` is built, resolve the
selection through the **existing** resolver rather than re-deriving it —
`resolve_grammar_range(Range::Selection, …)` already handles linewise
(complete lines), charwise (inclusive of the head, per vim) and blockwise.

```rust
// Only when Visual/Select is actually up: `resolve_grammar_range` answers
// for the primary selection whether or not one is active, and reporting a
// collapsed cursor as a "region" would make every Normal-mode action look
// like it had one.
let selection = if visual_is_active {
    resolve_grammar_range(Range::Selection, document, cursor, count).ok()
} else {
    None
};
```

**`visual_is_active` is a placeholder for whatever this function already has
in hand — read the surrounding code before writing it.** The dispatcher tests
the modal state in several places (`dispatcher.rs:331` matches
`Some(Range::Selection)`; `:359` computes `visual_linewise`), so the answer is
almost certainly already local. Use it rather than threading a new parameter,
and if it genuinely is not reachable, `document.selections().primary().visual`
being `Some(_)` is the same question asked of the document.

- [ ] **Step 5: Add the WIT field and the mirror**

`wit/types.wit`, on `action-context`:

```wit
/// OS.2: the active region when the action fired from a Visual-mode
/// chord; `none` in Normal and on every non-chord firing path. The
/// `ex-command-context` precedent (OC.10): a command reached one way
/// must not see less than the same command reached another.
selection: option<range>,
```

`boundary_grammar.rs:166`:

```rust
selection: ctx.selection.map(|r| r.to_wit()).transpose()?,
```

Match the conversion helper the neighbouring fields use — `cursor` uses
`ctx.cursor.to_wit()?`, so follow whatever `Range` exposes rather than
hand-rolling one.

- [ ] **Step 6: Write the dispatch-level test**

The projection test proves the wire; this proves the *contract*:

```rust
#[test]
fn a_normal_mode_action_sees_no_selection() { /* dispatch in Normal, assert None */ }

#[test]
fn a_linewise_visual_action_sees_whole_lines() {
    // anchor mid-line on 1, head mid-line on 3, linewise
    // => start (1,0), end (3, line_len(3))
}

#[test]
fn a_charwise_visual_action_sees_an_inclusive_head() {
    // head at (2,4) charwise => end byte 5, matching vim
}
```

- [ ] **Step 7: Run everything**

```bash
cargo test -p lattice-grammar
cargo test -p lattice-plugin-host
```

- [ ] **Step 8: Run the gate**

```bash
scripts/precommit.sh lattice-grammar lattice-plugin-host lattice-host
```

Run `lattice-ui-tui` and `lattice-ui-gpui` in **separate** invocations if they
are touched — combined load times out `settle_mode`
(`precommit-both-renderer-crates-flakes-magit`).

- [ ] **Step 9: Commit**

```bash
git add crates/lattice-grammar/src/registry.rs \
        crates/lattice-grammar/src/dispatcher.rs \
        wit/types.wit \
        crates/lattice-plugin-host/src/boundary_grammar.rs
git commit
```

**Explicitly not `apply-operator`.** Giving the operator seam a `document` is
the larger and better fix — it is what would make text-transforming plugin
operators possible at all — and §5.6.5 records it as a known gap. No verb in
this plan needs it. Say so in the message so the omission is a decision on the
record rather than an oversight.

## OS.3 — `Lists` — the model, and `Checkboxes` rebuilt on it **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.3.

The gate for the whole plugin half.

**Files**
- Create: `lattice-org-plugin/src/list.rs`
- Modify: `lattice-org-plugin/src/checkbox.rs` (rebuild `Checkboxes` over
  `Lists`; move `strip_bullet` and bullet-shape knowledge out)
- Modify: `lattice-org-plugin/src/lib.rs` (`mod list;`)
- Test: `list.rs`'s own `#[cfg(test)]` module (host-target unit tests, the
  `headline.rs` pattern)

**Interfaces**
- Consumes: `crate::tree` (`enclosing`, `children_of_kind`, `node_text`),
  `TreeSnapshot`, the grammar's `list` / `listitem` / `bullet` node kinds.
- Produces, consumed by OS.4–OS.10:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delim { Dot, Paren }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bullet {
    Dash,
    Plus,
    Star,
    Ordered { n: u32, delim: Delim },
}

impl Bullet {
    /// The literal text, e.g. `-`, `+`, `3.`, `3)`.
    pub fn render(self) -> String;

    /// The next shape in the cycle:
    ///     Dash -> Plus -> Ordered{Dot} -> Ordered{Paren} -> Dash
    /// `n` restarts at 1 on entry to an ordered form.
    ///
    /// `Star` is DELIBERATELY not in the cycle, though it is a legal
    /// bullet. It is legal only when indented -- at column 0 it is a
    /// headline -- so cycling a top-level list into it would turn every
    /// item into a heading. It is parsed because org files contain it;
    /// it is not somewhere the cycle will take you.
    pub fn cycled(self) -> Bullet;
}

/// Parse one line as a list item, given the indent already measured.
/// `None` when the line is not an item -- including `* Heading` at
/// column 0, which is a headline and not a `Star` bullet.
pub fn parse_bullet(line: &str, indent: usize) -> Option<Item>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub line: u32,
    pub indent: usize,
    pub bullet: Bullet,
    pub checkbox: Option<crate::checkbox::Check>,
    /// Byte offset where the item's own text begins, after the
    /// bullet and any checkbox.
    pub content_byte: u32,
}

pub struct Lists<'a> { /* tree + line fn + line_count, the Checkboxes shape */ }

impl<'a> Lists<'a> {
    pub fn new(
        tree: Option<&'a TreeSnapshot>,
        line: &'a dyn Fn(u32) -> Option<String>,
        line_count: u32,
    ) -> Self;

    /// The item whose BULLET is on line `n`. `None` on a continuation
    /// line, so acting from mid-item is a deliberate decision a caller
    /// makes with `enclosing_item`, not an accident.
    pub fn item_at(&self, n: u32) -> Option<Item>;

    /// The item line `n` belongs to, walking back over continuation
    /// lines. The "which item am I in" answer.
    pub fn enclosing_item(&self, n: u32) -> Option<Item>;

    /// Last line of the item at `start`, INCLUDING its continuation
    /// lines and its nested children. The unit a move or indent carries.
    pub fn item_end(&self, start: u32) -> u32;

    /// Item lines at the same indent, in order, within one list.
    pub fn siblings(&self, n: u32) -> Vec<u32>;

    /// Item lines one level deeper, directly under the item at `n`.
    pub fn children(&self, n: u32) -> Vec<u32>;

    /// First and last line of the whole list containing `n`.
    pub fn list_span(&self, n: u32) -> Option<(u32, u32)>;
}

/// Rewrite ordered-list numbering across `span`, per indent level,
/// each level restarting at 1. Answers only the lines that CHANGE,
/// so a caller can fold them into one edit. Unordered items are
/// untouched.
pub fn renumber(
    lists: &Lists<'_>,
    span: (u32, u32),
) -> Vec<(u32, String)>;
```

- [ ] **Step 1: Write the failing bullet-shape tests**

```rust
#[test]
fn parses_every_bullet_shape_org_accepts() {
    assert_eq!(parse_bullet("- milk", 0).unwrap().bullet, Bullet::Dash);
    assert_eq!(parse_bullet("+ milk", 0).unwrap().bullet, Bullet::Plus);
    assert_eq!(
        parse_bullet("1. milk", 0).unwrap().bullet,
        Bullet::Ordered { n: 1, delim: Delim::Dot }
    );
    assert_eq!(
        parse_bullet("12) milk", 0).unwrap().bullet,
        Bullet::Ordered { n: 12, delim: Delim::Paren }
    );
}

/// The existing rule, carried over from `checkbox.rs` verbatim: `*` is
/// a bullet only when INDENTED. At column 0 it is a headline, and
/// treating it as a bullet is how a list verb would silently eat an
/// outline.
#[test]
fn a_star_at_column_zero_is_a_headline_not_a_bullet() {
    assert!(parse_bullet("* Heading", 0).is_none());
    assert_eq!(parse_bullet("  * nested", 2).unwrap().bullet, Bullet::Star);
}

#[test]
fn an_item_carries_its_checkbox_when_it_has_one() {
    assert_eq!(parse_bullet("- [ ] milk", 0).unwrap().checkbox, Some(Check::Off));
    assert_eq!(parse_bullet("- milk", 0).unwrap().checkbox, None);
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cd ~/src/dhruvasagar/lattice-org-plugin && cargo test --lib list::
```

Expected: FAIL — `src/list.rs` does not exist.

- [ ] **Step 3: Implement `Bullet`, `Item` and the line parser**

Move `strip_bullet` out of `checkbox.rs` and generalise it: it currently
answers "what follows the bullet" and must now also answer *which* bullet, and
parse an ordered marker. Keep its `*`-at-column-0 rule exactly.

- [ ] **Step 4: Write the failing structure tests**

**Each of these runs twice** — once through `lists_over` (no tree, indent
fallback) and once through `lists_over_parsed` (a real `TreeSnapshot`) — and
both must give the same answer. The fallback is what runs on an unparsed
buffer and is the half that silently rots, so a tree-only test would never
notice it diverging. Write them as one `#[test]` per behaviour taking a
constructor, or a small macro; do not write sixteen near-identical tests.

```rust
const NESTED: &str = "\
- one
  continued
  - one-a
  - one-b
- two
";

#[test]
fn item_end_spans_continuation_lines_and_children() {
    let lists = lists_over(NESTED);          // no tree: indent fallback
    assert_eq!(lists.item_end(0), 3, "`- one` runs through `- one-b`");
    assert_eq!(lists.item_end(2), 2, "`- one-a` is one line");
}

#[test]
fn siblings_skip_nested_children() {
    let lists = lists_over(NESTED);
    assert_eq!(lists.siblings(0), vec![0, 4]);
    assert_eq!(lists.children(0), vec![2, 3]);
}

#[test]
fn enclosing_item_walks_back_over_a_continuation_line() {
    let lists = lists_over(NESTED);
    assert!(lists.item_at(1).is_none(), "a continuation line is not an item");
    assert_eq!(lists.enclosing_item(1).unwrap().line, 0);
}

#[test]
fn list_span_bounds_the_whole_list() {
    let lists = lists_over(NESTED);
    assert_eq!(lists.list_span(2), Some((0, 4)));
}
```

- [ ] **Step 5: Write the failing renumber tests**

```rust
#[test]
fn renumber_fixes_gaps_and_leaves_unordered_alone() {
    let lists = lists_over("1. a\n5. b\n9. c\n");
    assert_eq!(
        renumber(&lists, (0, 2)),
        vec![(1, "2. b".to_string()), (2, "3. c".to_string())],
        "only the lines that change"
    );
    let plain = lists_over("- a\n- b\n");
    assert!(renumber(&plain, (0, 1)).is_empty());
}

#[test]
fn each_indent_level_restarts_at_one() {
    let lists = lists_over("1. a\n   1. a-a\n   7. a-b\n2. b\n");
    assert_eq!(renumber(&lists, (0, 3)), vec![(2, "   2. a-b".to_string())]);
}

#[test]
fn renumber_preserves_the_delimiter_each_item_uses() {
    let lists = lists_over("1) a\n4) b\n");
    assert_eq!(renumber(&lists, (0, 1)), vec![(1, "2) b".to_string())]);
}
```

- [ ] **Step 6: Implement `Lists`, tree-first with indent fallback**

Mirror `Checkboxes`'s shape exactly — same constructor signature, same
tree-then-fallback branch structure. The tree half reads `listitem` nodes and
their `bullet` field; the fallback half reads indentation. Both must answer
identically for the fixtures above, which is what the two-way tests pin.

- [ ] **Step 7: Rebuild `Checkboxes` over `Lists`**

`Checkboxes::item_at` becomes `Lists::item_at` filtered to items carrying a
box. `ancestors`, `child_item_lines`, the tally and the cookie logic do **not**
move — they sit above the model and are unchanged.

- [ ] **Step 8: Run every existing test unchanged**

```bash
cargo build --release --target wasm32-wasip2
cargo test
```

**Every existing checkbox test must pass without being edited.** If one needs
changing to accommodate the rewrite, that is a behaviour change — stop, and
report which test and what changed. Absorbing it silently is how a cookie
regression ships.

- [ ] **Step 9: Commit**

```bash
git add src/list.rs src/checkbox.rs src/lib.rs
git commit
```

Message: the model, and why `Checkboxes` was rebuilt rather than left beside
it — two walkers agree the day they are written and diverge on the first
grammar bump, surfacing as a cookie that quietly stops updating.

## OS.4 — `<M-CR>` — meta-return dispatches on what is at point **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4.
Needs OS.0, OS.3.

**Files**
- Modify: `lattice-org-plugin/src/lib.rs` — extend `meta_return` into an arm
  table; add `<M-CR>` binds in Normal and Insert to `org-mode`'s keymap
- Test: `lattice-org-plugin/tests/org_structure.rs`

**Interfaces**
- Consumes: `list::Lists`, `list::Item`, `list::Bullet`, `list::renumber`
  (OS.3); the existing `headline::Headlines` and the existing `meta_return`
  body; `replace_lines(ctx, from, to, to_len, text, cursor)` (`lib.rs:4029`).
- Produces: the arm table OS.5 adds its shift arm to. Keep the dispatch and
  the arms as separate functions so OS.5 extends rather than rewrites:

```rust
/// What the cursor is on. OS.5 adds no variants; it adds a second
/// verb over the same three.
enum AtPoint { CheckboxItem(list::Item), ListItem(list::Item), Headline(u32, usize) }

fn at_point(doc: &Document, tree: Option<&TreeSnapshot>, line: u32) -> Option<AtPoint>;
```

- [ ] **Step 1: Write the failing arm tests**

Pressed, in both binding modes, with effects applied:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn meta_return_on_a_plain_item_inserts_a_plain_item() {
    let Some(mut editor) = org_editor("- milk\n- eggs\n").await else { return };
    goto(&mut editor, 0, 0);
    press_chord(&mut editor, "<M-CR>").await;
    assert_eq!(text(&editor), "- milk\n- \n- eggs\n");
    assert_eq!(cursor(&editor), (1, 2), "caret sits after the bullet, ready to type");
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_return_on_a_checkbox_item_inserts_a_checkbox_item() {
    let Some(mut editor) = org_editor("- [X] bread\n").await else { return };
    goto(&mut editor, 0, 0);
    press_chord(&mut editor, "<M-CR>").await;
    assert_eq!(text(&editor), "- [X] bread\n- [ ] \n", "a NEW box starts empty");
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_return_on_a_headline_still_inserts_after_the_subtree() {
    let Some(mut editor) = org_editor("* One\n** Child\n* Two\n").await else { return };
    goto(&mut editor, 0, 0);
    press_chord(&mut editor, "<M-CR>").await;
    assert_eq!(text(&editor), "* One\n** Child\n* \n* Two\n",
        "respect-content: the new sibling must not adopt Child");
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_return_works_in_insert_mode_too() {
    let Some(mut editor) = org_editor("- milk\n").await else { return };
    goto(&mut editor, 0, 6);
    press(&mut editor, "A");                  // Insert, at end of line
    press_chord(&mut editor, "<M-CR>").await;
    assert_eq!(text(&editor), "- milk\n- \n");
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_return_renumbers_an_ordered_list_in_the_same_edit() {
    let Some(mut editor) = org_editor("1. a\n2. b\n").await else { return };
    goto(&mut editor, 0, 0);
    press_chord(&mut editor, "<M-CR>").await;
    assert_eq!(text(&editor), "1. a\n2. \n3. b\n");
    press(&mut editor, "u");
    assert_eq!(text(&editor), "1. a\n2. b\n", "one undo restores insert AND numbering");
}

#[tokio::test(flavor = "multi_thread")]
async fn meta_return_in_the_preamble_does_nothing() {
    let Some(mut editor) = org_editor("just prose\n").await else { return };
    press_chord(&mut editor, "<M-CR>").await;
    assert_eq!(text(&editor), "just prose\n");
}
```

`org_editor(text) -> Option<Editor>` follows the existing file's skip-when-not-built
pattern (returns `None` when `org_plugin_wasm()` is `None`). Reuse the
helpers already in `org_structure.rs` rather than adding parallel ones.

- [ ] **Step 2: Run to verify they fail**

```bash
cargo build --release --target wasm32-wasip2
cargo test --test org_structure meta_return
```

Expected: the list arms FAIL (a headline is inserted, or nothing is); the
headline and preamble tests PASS (existing behaviour).

- [ ] **Step 3: Implement `at_point` and the arms**

The checkbox arm before the plain-item arm (a checkbox item is also a list
item, so order decides), then the headline arm. **The headline arm calls the
existing `meta_return` body** — do not copy its respect-content logic.

- [ ] **Step 4: Bind the chord in both modes**

`lib.rs`'s `register_modes`, on `org-mode`. The existing `bind` closure
hardcodes `BindingMode::Normal`; add an `ibind` peer for Insert rather than
changing `bind`'s signature at every existing call site:

```rust
let ibind = |chord: &str, command: &str| ModeKeymapBinding {
    binding_mode: BindingMode::Insert,
    chord: chord.to_string(),
    command: command.to_string(),
};
// ...
bind("<M-CR>", "org-meta-return"),
ibind("<M-CR>", "org-meta-return"),
// `<leader><CR>` stays: it is the portable spelling, and a terminal
// that cannot send `<M-CR>` must still reach the verb.
```

- [ ] **Step 5: Run the tests**

```bash
cargo build --release --target wasm32-wasip2
cargo test --test org_structure meta_return
```

- [ ] **Step 6: Run the gate and commit**

```bash
cargo fmt --all && cargo clippy --all-targets && cargo test
git add src/lib.rs tests/org_structure.rs
git commit
```

## OS.5 — `<M-S-CR>` — the variant, and the headline insert family **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4.
Needs OS.4. Reads better after OS.1, does not need it.

**Files**
- Modify: `lattice-org-plugin/src/lib.rs`
- Test: `lattice-org-plugin/tests/org_structure.rs`

**Interfaces**
- Consumes: `at_point` and the arms from OS.4; `todo_keywords()` (existing).
- Produces: ActionIds `org-insert-todo-heading` (the `<M-S-CR>` dispatcher)
  and `org-insert-subheading`.

- [ ] **Step 1: Write the failing variant tests**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn shift_meta_return_upgrades_a_plain_item_to_a_checkbox_item() {
    let Some(mut editor) = org_editor("- milk\n").await else { return };
    press_chord(&mut editor, "<M-S-CR>").await;
    assert_eq!(text(&editor), "- milk\n- [ ] \n");
}

#[tokio::test(flavor = "multi_thread")]
async fn shift_meta_return_downgrades_a_checkbox_item_to_a_plain_one() {
    let Some(mut editor) = org_editor("- [X] bread\n").await else { return };
    press_chord(&mut editor, "<M-S-CR>").await;
    assert_eq!(text(&editor), "- [X] bread\n- \n");
}

/// The keyword comes from the CONFIGURED sequence, not a hardcoded
/// "TODO" -- `org.todo-keywords` is a list option and a user whose
/// first state is `NEXT` must get `NEXT`.
#[tokio::test(flavor = "multi_thread")]
async fn shift_meta_return_on_a_headline_uses_the_first_configured_keyword() {
    let Some(mut editor) = org_editor_with_keywords("* One\n", "NEXT TODO | DONE").await
        else { return };
    press_chord(&mut editor, "<M-S-CR>").await;
    assert_eq!(text(&editor), "* One\n* NEXT \n");
}

#[tokio::test(flavor = "multi_thread")]
async fn insert_subheading_nests_without_adopting_existing_children() {
    let Some(mut editor) = org_editor("* One\n** Child\n* Two\n").await else { return };
    goto(&mut editor, 0, 0);
    press_chord(&mut editor, "<leader>oi").await;
    assert_eq!(text(&editor), "* One\n** Child\n** \n* Two\n");
}
```

- [ ] **Step 2: Run to verify they fail**

```bash
cargo build --release --target wasm32-wasip2
cargo test --test org_structure shift_meta_return
cargo test --test org_structure subheading
```

- [ ] **Step 3: Implement the shift arms and subheading**

- [ ] **Step 4: Bind them**

```rust
bind("<M-S-CR>", "org-insert-todo-heading"),
ibind("<M-S-CR>", "org-insert-todo-heading"),
// `<leader>oi` -- the letter OA.27 left deliberately free, noting `i`
// as "the natural prefix for inserting things". Subheading has no
// modifier gesture because emacs gives it none either.
bind("<leader>oi", "org-insert-subheading"),
```

- [ ] **Step 5: Run, gate, commit**

```bash
cargo build --release --target wasm32-wasip2 && cargo test
cargo fmt --all && cargo clippy --all-targets
git add src/lib.rs tests/org_structure.rs && git commit
```

## OS.6 — The Meta-arrows: promote/demote *is* indent/outdent **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4, §5.6.6.
Needs OS.3.

**Files**
- Modify: `lattice-org-plugin/src/lib.rs`
- Modify: `lattice-org-plugin/src/list.rs` (add `indent_item` / `outdent_item`
  as pure functions, testable on the host target)
- Test: both files

**Interfaces**
- Consumes: `Lists`, `renumber` (OS.3); the existing `promote`/`demote`
  bodies behind `<leader>oh` / `ol` / `oH` / `oL`.
- Produces, consumed by OS.8 and OS.10: ActionIds `org-meta-left`,
  `org-meta-right`, `org-shift-meta-left`, `org-shift-meta-right`; and

```rust
/// Re-indent the item at `start` by `delta` levels, carrying its
/// continuation lines and (when `with_children`) its nested items.
/// `None` when the shift is refused. Answers only changed lines.
pub fn shift_item(
    lists: &Lists<'_>,
    start: u32,
    delta: isize,
    with_children: bool,
) -> Option<Vec<(u32, String)>>;
```

- [ ] **Step 1: Write the failing unit tests in `list.rs`**

```rust
#[test]
fn indenting_an_item_carries_its_children() {
    let lists = lists_over("- a\n- b\n  - b-a\n");
    let out = shift_item(&lists, 1, 1, true).unwrap();
    assert_eq!(out, vec![
        (1, "  - b".to_string()),
        (2, "    - b-a".to_string()),
    ]);
}

/// §5.6.6: an outdent at column zero is REFUSED, not silently turned
/// into a headline. `<leader>o*` is how an item becomes a headline and
/// it is a different gesture on purpose.
#[test]
fn outdenting_a_top_level_item_is_refused() {
    let lists = lists_over("- a\n");
    assert!(shift_item(&lists, 0, -1, true).is_none());
}
```

- [ ] **Step 2: Write the failing dispatch tests**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn meta_right_demotes_a_headline_and_indents_an_item() {
    let Some(mut editor) = org_editor("* One\n- milk\n").await else { return };
    goto(&mut editor, 0, 0);
    press_chord(&mut editor, "<M-Right>").await;
    assert_eq!(line_at(&editor, 0), "** One");
    goto(&mut editor, 1, 0);
    press_chord(&mut editor, "<M-Right>").await;
    assert_eq!(line_at(&editor, 1), "  - milk");
}

/// The existing refusal, reached through the new gesture.
#[tokio::test(flavor = "multi_thread")]
async fn meta_left_refuses_to_promote_a_level_one_subtree() {
    let Some(mut editor) = org_editor("* One\n** Child\n").await else { return };
    press_chord(&mut editor, "<M-S-Left>").await;
    assert_eq!(text(&editor), "* One\n** Child\n", "refused whole");
    assert!(last_echo(&editor).is_some(), "and it says so");
}

#[tokio::test(flavor = "multi_thread")]
async fn indenting_into_an_ordered_sublist_renumbers_it() {
    let Some(mut editor) = org_editor("1. a\n2. b\n1. c\n").await else { return };
    goto(&mut editor, 2, 0);
    press_chord(&mut editor, "<M-Right>").await;
    assert_eq!(text(&editor), "1. a\n2. b\n   1. c\n");
}
```

- [ ] **Step 3: Run to verify they fail; implement; run again**

```bash
cargo test --lib list::
cargo build --release --target wasm32-wasip2 && cargo test --test org_structure meta_
```

The headline arms **call the existing bodies** — `<leader>oh` / `ol` / `oH` /
`oL` stay bound and unchanged, and there must be exactly one promote
implementation.

- [ ] **Step 4: Bind, Normal only**

```rust
bind("<M-Left>", "org-meta-left"),
bind("<M-Right>", "org-meta-right"),
bind("<M-S-Left>", "org-shift-meta-left"),
bind("<M-S-Right>", "org-shift-meta-right"),
```

No `ibind` here — §5.6.2's split puts restructuring verbs in Normal, and OS.8
gives Insert its own spelling.

- [ ] **Step 5: Run the gate and commit**

Stage `src/list.rs`, `src/lib.rs`, `tests/org_structure.rs`. The message says
why one gesture carries two verbs: the arms call the bodies `<leader>oh` /
`ol` / `oH` / `oL` already call, so there is exactly one promote
implementation and nothing to drift.

## OS.7 — `<M-Up>` / `<M-Down>` — move an item or a subtree **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.4, §5.6.6.
Needs OS.3.

**Files**
- Modify: `lattice-org-plugin/src/list.rs` (`move_item`), `src/lib.rs`
- Test: both

**Interfaces**
- Produces: ActionIds `org-meta-up`, `org-meta-down`; and

```rust
/// Swap the item at `start` with its previous (`delta < 0`) or next
/// (`delta > 0`) SIBLING, carrying continuation lines and children.
/// `None` at either end of the sibling chain.
///
/// Answers the rewritten BLOCK, not changed lines like `shift_item`
/// and `renumber` do, and the difference is not an inconsistency: a
/// move rewrites one contiguous span whose lines all shift position,
/// so "which lines changed" is every line in it. The caller writes
/// the block over `(first_sibling_start ..= last_sibling_end)`.
pub fn move_item(lists: &Lists<'_>, start: u32, delta: isize) -> Option<String>;
```

- [ ] **Step 1: Write the failing tests**

```rust
#[test]
fn moving_an_item_carries_its_children_and_skips_theirs() {
    let lists = lists_over("- a\n  - a-a\n- b\n");
    // `- a` moves DOWN past `- b`; `- a-a` travels with it, and `- b`
    // is a sibling rather than something to descend into.
    assert_eq!(move_item(&lists, 0, 1).unwrap(), "- b\n- a\n  - a-a\n");
}

/// §5.6.6: a move stops at its parent rather than splicing the item
/// into a neighbouring list.
#[test]
fn moving_the_last_sibling_down_is_refused() {
    let lists = lists_over("- a\n- b\n");
    assert!(move_item(&lists, 1, 1).is_none());
}
```

```rust
#[tokio::test(flavor = "multi_thread")]
async fn meta_down_moves_a_subtree_and_an_item_alike() {
    let Some(mut editor) = org_editor("* One\n* Two\n").await else { return };
    press_chord(&mut editor, "<M-Down>").await;
    assert_eq!(text(&editor), "* Two\n* One\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn moving_an_ordered_item_renumbers_both_positions() {
    let Some(mut editor) = org_editor("1. a\n2. b\n").await else { return };
    press_chord(&mut editor, "<M-Down>").await;
    assert_eq!(text(&editor), "1. b\n2. a\n", "numbers stay positional");
}
```

- [ ] **Step 2: Run to verify they fail, implement, run again**

```bash
cargo test --lib list::move_item
cargo build --release --target wasm32-wasip2
cargo test --test org_structure meta_down
```

Expected first run: FAIL — `move_item` is not defined.

- [ ] **Step 3: Bind, Normal only**

```rust
bind("<M-Up>", "org-meta-up"),
bind("<M-Down>", "org-meta-down"),
// Emacs' subtree-explicit peers, same ActionIds -- on a headline both
// spellings mean the subtree, which is what org's own move does.
bind("<M-S-Up>", "org-meta-up"),
bind("<M-S-Down>", "org-meta-down"),
// `<leader>oK` / `oJ` stay bound and unchanged.
```

- [ ] **Step 4: Run the gate and commit**

Stage `src/list.rs`, `src/lib.rs`, `tests/org_structure.rs`. The message
records the refusal that matters: a move stops at its parent rather than
splicing an item into a neighbouring list.

## OS.8 — `<C-t>` / `<C-d>` in Insert, declining off a list **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.2.
Needs OS.0, OS.6.

Pure binding work over OS.6's bodies plus one new arm: the decline.

**Files**
- Modify: `lattice-org-plugin/src/lib.rs`
- Test: `lattice-org-plugin/tests/org_structure.rs`

- [ ] **Step 1: Write the failing tests, including the decline**

The decline test is the important one, and it must go through a keypress —
observing that the action returned `Declined` proves nothing about what the
user gets:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_t_indents_the_list_item_being_typed() {
    let Some(mut editor) = org_editor("- a\n- b\n").await else { return };
    goto(&mut editor, 1, 3);
    press(&mut editor, "A");
    press_chord(&mut editor, "<C-t>").await;
    assert_eq!(line_at(&editor, 1), "  - b");
}

/// `<C-t>` is a SHARED chord -- vim's Insert-mode shiftwidth indent
/// lives underneath it. Off a list item org must decline and let the
/// builtin run, the `<C-a>` / `<C-x>` argument in Insert mode.
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_t_on_prose_still_runs_vims_indent() {
    let Some(mut editor) = org_editor("* One\nsome prose\n").await else { return };
    goto(&mut editor, 1, 0);
    press(&mut editor, "A");
    press_chord(&mut editor, "<C-t>").await;
    assert_eq!(line_at(&editor, 1), "    some prose",
        "the builtin ran; org consumed nothing");
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement — a thin arm over OS.6's bodies**

On a list item, call the same body `<M-Right>` / `<M-Left>` call. Off one,
return `vec![Effect::Declined]`. No new indent logic.

- [ ] **Step 4: Bind, Insert only**

```rust
ibind("<C-t>", "org-meta-right"),
ibind("<C-d>", "org-meta-left"),
```

If OS.0 found that Insert-mode declines do **not** fall through, this slice is
blocked — say so and stop rather than shipping a chord that eats vim's indent.

- [ ] **Step 5: Run the gate and commit**

Stage `src/lib.rs`, `tests/org_structure.rs`. The message says why these two
decline where every other org chord consumes: they are shared chords with
vim's own indent underneath, the `<C-a>` / `<C-x>` argument moved into Insert.

## OS.9 — Bullet cycling, and line ↔ item ↔ headline **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.
Needs OS.3.

**Files**
- Modify: `lattice-org-plugin/src/list.rs` (`cycle_bullets`, `toggle_item`),
  `src/lib.rs`
- Test: both

**Interfaces**
- Produces: ActionIds `org-cycle-list-bullet`, `org-toggle-item`; and the
  extension of the existing `org-toggle-heading` to list items.

- [ ] **Step 1: Write the failing tests**

```rust
/// Cycling acts on the WHOLE list, not one item -- a list with mixed
/// bullets is not something org produces, and cycling one item would
/// create one. This is also emacs' no-region `C-c -`.
#[tokio::test(flavor = "multi_thread")]
async fn cycling_rewrites_every_bullet_in_the_list() {
    let Some(mut editor) = org_editor("- a\n- b\n").await else { return };
    press_chord(&mut editor, "<leader>o-").await;
    assert_eq!(text(&editor), "+ a\n+ b\n");
    press_chord(&mut editor, "<leader>o-").await;
    assert_eq!(text(&editor), "1. a\n2. b\n", "ordered entry numbers from 1");
    press_chord(&mut editor, "<leader>o-").await;
    assert_eq!(text(&editor), "1) a\n2) b\n");
    press_chord(&mut editor, "<leader>o-").await;
    assert_eq!(text(&editor), "- a\n- b\n", "and wraps");
}

#[tokio::test(flavor = "multi_thread")]
async fn toggle_item_round_trips_a_prose_line_preserving_indent() {
    let Some(mut editor) = org_editor("* One\n  some prose\n").await else { return };
    goto(&mut editor, 1, 0);
    press_chord(&mut editor, "<leader>o_").await;
    assert_eq!(line_at(&editor, 1), "  - some prose");
    press_chord(&mut editor, "<leader>o_").await;
    assert_eq!(line_at(&editor, 1), "  some prose");
}

/// Un-itemising a checkbox item drops its box, so the parent's cookie
/// is wrong until it is rewritten -- in the SAME edit, the rule the
/// toggle already follows.
#[tokio::test(flavor = "multi_thread")]
async fn un_itemising_a_checkbox_updates_the_cookie_in_one_edit() {
    let Some(mut editor) = org_editor("* Shop [1/2]\n- [X] a\n- [ ] b\n").await
        else { return };
    goto(&mut editor, 1, 0);
    press_chord(&mut editor, "<leader>o_").await;
    assert_eq!(line_at(&editor, 0), "* Shop [0/1]");
    press(&mut editor, "u");
    assert_eq!(line_at(&editor, 0), "* Shop [1/2]", "one undo restores both");
}

/// §5.6.6 / the existing rule: the new headline takes the ENCLOSING
/// level, not level 1.
#[tokio::test(flavor = "multi_thread")]
async fn toggle_heading_on_an_item_uses_the_enclosing_level() {
    let Some(mut editor) = org_editor("* One\n** Two\n- milk\n").await else { return };
    goto(&mut editor, 2, 0);
    press_chord(&mut editor, "<leader>o*").await;
    assert_eq!(line_at(&editor, 2), "** milk");
}
```

- [ ] **Step 2: Run to verify they fail, implement, run again**

```bash
cargo test --lib list::
cargo build --release --target wasm32-wasip2
cargo test --test org_structure cycling
cargo test --test org_structure toggle_item
```

- [ ] **Step 3: Bind**

```rust
bind("<leader>o-", "org-cycle-list-bullet"),
// `<C-c>-` is safe because `<C-c>` is only ever a PREFIX here.
// `<C-c><C-c>` must remain the sole terminal binding beneath it --
// a terminal node kills every longer chord grown under it.
bind("<C-c>-", "org-cycle-list-bullet"),
bind("<leader>o_", "org-toggle-item"),
// `<leader>o*` / `<C-c>*` already exist; only the handler is extended.
```

- [ ] **Step 4: Run the gate and commit**

Stage `src/list.rs`, `src/lib.rs`, `tests/org_structure.rs`. The message says
why cycling acts on the whole list rather than one item — a list with mixed
bullets is not something org produces, and cycling one item would create one.

## OS.10 — The Visual peers **(plugin)** 📝

Design: [`org-mode.md`](../../architecture/org-mode.md) §5.6.5.
Needs OS.2, and the verb slices whose ActionIds it binds (OS.6, OS.7, OS.9).

**Files**
- Modify: `lattice-org-plugin/src/lib.rs` — a `vbind` peer and a region
  wrapper each verb routes through
- Test: `lattice-org-plugin/tests/org_structure.rs`

**Interfaces**
- Consumes: `ctx.selection: Option<Range>` (OS.2).
- Contract, pinned here because it was the one ambiguity the design
  self-review caught: **a mixed-level region shifts every item by one and
  preserves relative structure** — it does not flatten to a common level. And
  the edit is **all-or-nothing**: if any item would hit a refusal, the whole
  invocation refuses and names the item that stopped it.

- [ ] **Step 1: Write the failing tests**

```rust
#[tokio::test(flavor = "multi_thread")]
async fn a_visual_region_indents_every_item_it_touches() {
    let Some(mut editor) = org_editor("- a\n- b\n- c\n").await else { return };
    goto(&mut editor, 0, 0);
    press(&mut editor, "Vj");                       // linewise, lines 0-1
    press_chord(&mut editor, "<M-Right>").await;
    assert_eq!(text(&editor), "  - a\n  - b\n- c\n");
}

/// Relative structure survives: a child must stay a child.
#[tokio::test(flavor = "multi_thread")]
async fn a_mixed_level_region_shifts_by_one_and_keeps_its_shape() {
    let Some(mut editor) = org_editor("- a\n  - a-a\n- b\n").await else { return };
    press(&mut editor, "VG");
    press_chord(&mut editor, "<M-Right>").await;
    assert_eq!(text(&editor), "  - a\n    - a-a\n  - b\n");
}

/// All-or-nothing: applying to the two thirds that could move is the
/// class of surprise §5.6.6 exists to prevent.
#[tokio::test(flavor = "multi_thread")]
async fn a_region_containing_one_refusal_refuses_whole() {
    let Some(mut editor) = org_editor("- a\n  - a-a\n").await else { return };
    press(&mut editor, "VG");
    press_chord(&mut editor, "<M-Left>").await;
    assert_eq!(text(&editor), "- a\n  - a-a\n", "nothing moved");
    assert!(last_echo(&editor).is_some(), "and it names what stopped it");
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_space_toggles_every_box_in_the_region() {
    let Some(mut editor) = org_editor("* S [0/2]\n- [ ] a\n- [ ] b\n").await
        else { return };
    goto(&mut editor, 1, 0);
    press(&mut editor, "Vj");
    press_chord(&mut editor, "<C-Space>").await;
    assert_eq!(text(&editor), "* S [2/2]\n- [X] a\n- [X] b\n");
    press(&mut editor, "u");
    assert_eq!(line_at(&editor, 0), "* S [0/2]", "one edit, one undo");
}

/// A Normal-mode firing has `selection: None` and must fall back to
/// the point-scoped behaviour rather than erroring.
#[tokio::test(flavor = "multi_thread")]
async fn no_selection_falls_back_to_the_item_at_point() {
    let Some(mut editor) = org_editor("- a\n- b\n").await else { return };
    press_chord(&mut editor, "<M-Right>").await;
    assert_eq!(text(&editor), "  - a\n- b\n");
}
```

- [ ] **Step 2: Run to verify they fail**

- [ ] **Step 3: Implement the region wrapper**

One function every region-capable verb routes through: read
`ctx.selection`, collect the item lines it touches, apply the verb to each,
fold every rewritten line into **one** `Effect::ApplyEdit`, and refuse whole
if any item refuses. `None` selection collapses to the single item at the
cursor — which is the point-scoped path, not a special case.

- [ ] **Step 4: Bind in Visual**

```rust
let vbind = |chord: &str, command: &str| ModeKeymapBinding {
    binding_mode: BindingMode::Visual,
    chord: chord.to_string(),
    command: command.to_string(),
};
// ...
vbind("<M-Left>", "org-meta-left"),
vbind("<M-Right>", "org-meta-right"),
vbind("<M-S-Left>", "org-shift-meta-left"),
vbind("<M-S-Right>", "org-shift-meta-right"),
vbind("<M-Up>", "org-meta-up"),
vbind("<M-Down>", "org-meta-down"),
vbind("<leader>o-", "org-cycle-list-bullet"),
vbind("<leader>o_", "org-toggle-item"),
vbind("<leader>o*", "org-toggle-heading"),
vbind("<C-Space>", "org-toggle-checkbox"),
```

- [ ] **Step 5: Run the gate and commit**

Stage `src/lib.rs`, `tests/org_structure.rs`. The message records the contract
this slice pins: a mixed-level region shifts by one and keeps its shape, and
the edit is all-or-nothing so a partially-applied region is not a state the
user can be left in.

## OS.11 — `:help org` — the Lists section, and the site **(plugin + host)** 📝

Three artefacts, one slice, because they describe one surface.

**Files**
- Modify: `lattice-org-plugin/doc/org.md` — a new **Lists** section; the
  **Structure** table extended; the **Checkboxes** section extended
- Modify: `lattice/docs/user/org.md` — the "What you get" **Outline** row
- Modify: `lattice/docs/dev/operations/slice-plans/org-structure-editing.md` —
  status icons to ✅
- Modify: `lattice/docs/dev/architecture/org-mode.md` — only if the design
  changed in flight

- [ ] **Step 1: Write the Lists section in `doc/org.md`**

Match the existing sections' shape: a two-column chord table, a short `org`
fenced example, then the rules that are not obvious. Cover the four bullet
shapes, the Insert/Normal/Visual split, the ordered-list renumbering rule,
and the four refusals from §5.6.6 — the existing Structure section already
documents its three refusals in exactly this way and is the model.

- [ ] **Step 2: Extend the Structure and Checkboxes tables**

- [ ] **Step 3: Extend `docs/user/org.md`**

The **Outline** row currently reads "headline folding, promotion and
demotion, structure motions, tree-sitter highlighting". Add list and checkbox
structure editing to it.

- [ ] **Step 4: Sync the site**

`docs/user/org.md` is already in `site/data/nav.toml` (section `config`, doc
`org`), so **no nav change** — but the sync must run and search must pick the
new text up (`docs-land-on-the-zola-site-too`):

```bash
site/scripts/sync-docs.sh
```

- [ ] **Step 5: Correct this plan's status table**

Set each slice's icon from what actually landed. If a slice shipped
differently from its section here, fix the section — do not bend the docs to
match a plan that was overtaken.

- [ ] **Step 6: Commit, in each repo**

```bash
# lattice-org-plugin
git add doc/org.md && git commit

# lattice -- explicit paths; never `-A`
git add docs/user/org.md \
        docs/dev/operations/slice-plans/org-structure-editing.md \
        site/content
git commit
```

---

## Closing check

Before calling this plan done, re-run the ownership invariant:

```bash
# In lattice. All four must return nothing.
git diff main --stat -- crates/ | grep -i org
git diff main -- crates/ | grep -E '^\+.*fn (do_|ensure_)org'
git diff main -- crates/lattice-host/src/action.rs
git diff main -- crates/ | grep -E '^\+.*headline|listitem|bullet'
```

Anything they surface is org logic that leaked into the host, and it belongs
in the plugin instead.
