# Late-language activation — a catalog change re-resolves the major — slice plan

> **Status: Active.** Opened 2026-09-07. Implements
> [`mode-architecture.md`](../../architecture/mode-architecture.md) §7.4,
> "Major mode, second trigger — `LanguagesRegistered`".

Design owns *what* and *why*; this file owns *when*, *in what order*, and
*how*.

**Goal.** A file opened before its plugin finished loading ends up in the state
it would have had if opened after — major mode, keymaps, syntax, folds, minors,
LSP — without reopening it and without each plugin implementing that for
itself.

**The bug it closes.** `lattice todo.org` boots with no org anything;
`:e todo.org` moments later works. Plugin discovery is spawned off the boot
thread, so the initial document is resolved against a catalog that does not yet
contain its language, and §7.4's resolver runs on open only.

## What this REPLACES

Three patches landed on 2026-09-07 while the cause was still being narrowed.
The two that *infer* a catalog change are deleted by this plan, not extended —
they are the per-feature monkey-patching this design exists to remove. The
third turned out not to be a patch at all but the missing invariant of a shared
helper, and LA.3 says why it stays.

| Landed | Fate |
|---|---|
| `Editor::reattach_plugin_syntax` (`ab512368`) | **deleted** (LA.3) — re-resolution covers it |
| `Editor::last_plugin_langs` field (`ab512368`) | **deleted** (LA.3) — the event replaces `Arc::ptr_eq` polling |
| the refold inside `install_document_syntax` (`f5c2099b`) | **KEPT** — see LA.3 step 2; the "folds follow activation" premise fails for a language that claims no major |
| `install_document_syntax` itself (`f5c2099b`) | **KEEP** — it de-duplicates four install sites and fixes a real trap (the active buffer reads `self.syntax`, not the per-buffer slot) |

`syntax_reparse_panicked` / `_worker_stopped` (`8c360ab0`) and the
`cells_matrix_invalidated` instrumentation (`abef9d06`) are unrelated to this
plan and stay.

## Global constraints

- **One slice, one commit**, message explaining why that slice exists.
- **Gates before committing**: `scripts/precommit.sh <crate>…`. Zero new rustc
  warnings in touched code.
- **Never `git add -A`.** Stage by explicit path (`never-commit-todo-org`).
- **Test through the trigger, not the helper.** Asserting that a re-resolution
  function works proves nothing about whether anything calls it — the failure
  mode this whole area keeps producing (`none-paths-are-not-coverage`).
- **No boot re-sequencing.** Discovery stays off the boot thread (paramount #4).

## Status

| Slice | Title | Status |
|---|---|---|
| LA.1 | `LanguagesRegistered` — the event, published once per load | ✅ |
| LA.2 | Re-resolve the major for fallback-major buffers | ✅ |
| LA.3 | Delete the patches this replaces | ✅ |
| LA.4 | End-to-end: an org file on argv gets org's keymaps | 📝 |

## Dependencies

LA.1 → LA.2 → LA.3. LA.4 is the acceptance test and lands last, because it is
the only one that fails today for the *reported* reason (keymaps) rather than a
proxy for it.

---

## LA.1 — `LanguagesRegistered` — the event **(loader)** ✅

**Files**
- Created: `crates/lattice-plugin-loader/src/events.rs` — the event type, in
  the crate that *produces* it (the `lattice-plugin-host::PluginTracePushed`
  precedent; `lattice-host` depends on the loader, so a host subscriber sees
  it)
- Modified: `crates/lattice-plugin-loader/src/lib.rs` — publish at the
  end of `load`, beside `Event::PluginLoaded`
- Created: `crates/lattice-plugin-loader/tests/languages_registered_event.rs`

**Interfaces**
- Produces: `LanguagesRegistered { plugin: PluginId }`, published **once per
  plugin load**, after its languages *and* majors have registered — not once
  per language, or the resolver runs against a half-installed catalog.

- [x] **Step 1: Find where the loader already publishes typed events**

`install.rs` has `boot.event_bus()` and publishes `PluginTracePushed`. Follow
that shape exactly rather than inventing a second path.

> **Correction to an earlier claim.** During diagnosis this plan's author
> asserted "`lattice-plugin-loader` has no event bus", and used
> `Arc::ptr_eq` polling on the language registry instead. That was wrong —
> `install.rs` holds `boot.event_bus()`. The polling exists only because of
> that mistake and LA.3 removes it.

- [x] **Step 2: Publish after the drain completes, once**

Landed at the *end* of `PluginLoader::load` — after `apply_default_mode_gate`,
not merely after the seam loop, so a subscriber resolving against the catalog
sees it settled: registered by the drain **and** enabled/disabled by the gate.

Gated on the manifest declaring `language` or `modes`. Declared-seam gated
rather than registered-count gated: the wasted re-resolution when every
language was rejected costs one scan that finds nothing, where deriving the
gate from what actually registered would need every drain to report a count
upward — and a drain that forgot to would fail silently, which is the failure
mode this whole area keeps producing.

- [x] **Step 3: Test that a load publishes exactly one**

`language-guest` declares FOUR languages of which three are rejected, so a
per-language publish reads as 1-vs-4 rather than a coincidence. A second test
pins the negative: a help-only plugin publishes nothing, which is what keeps
LA.2's O(major-modes × open buffers) off every auto-pair-shaped plugin.

- [x] **Step 4: Gate and commit**

---

## LA.2 — Re-resolve the major for fallback-major buffers **(host)** ✅

**Files**
- Modified: `crates/lattice-host/src/editor_boot.rs` — subscribe, bridge to the
  actor the way the `SyntaxReparsed` → `cells_wake` forwarder does
- Modified: `crates/lattice-host/src/editor.rs` — the drain channel
- Modified: `crates/lattice-host/src/dispatch.rs` — the re-resolution itself
- Modified: `docs/dev/architecture/mode-architecture.md` §7.4 — the
  design correction below

**Interfaces**
- `Editor::reresolve_majors_after_catalog_change()`: for every open
  **document** buffer whose major is still the FALLBACK, run §7.4's ordered
  resolver and activate the winner. Activation emits `MajorEntered`, so minors,
  keymaps, syntax and folds all follow the normal path — this function itself
  must contain **no** syntax, fold or keymap logic. If it does, the design was
  not implemented.

> **Design correction, agreed with the user before implementing.** The
> interface above is *not* sufficient, and the sufficiency claim was the
> design's, not the plan's. Syntax follows from activation only when the late
> plugin ships a **major bound to its language** — org does; a plugin shipping
> only a grammar does not, and that shape is one `modes.rs` already pins
> (`a_plugin_language_with_no_claimed_major_still_falls_back_to_text_mode`).
> Such a buffer re-resolves to the same fallback, nothing activates, and
> nothing attaches. `reattach_plugin_syntax` covers it today because it keys on
> the *language*; LA.3 deletes that, so LA.2 has to carry it or the slice ships
> a silent regression.
>
> So the function re-derives **both** facts the catalog decides, in the order
> the open path derives them: the buffer's LANGUAGE via `detect_from_path`
> (which reads the live plugin registry), then its MAJOR via the ordered
> resolver. Language first is load-bearing — `activate_mode_by_id` recomputes
> folds and *then* rebuilds syntax, so major-first folds against the
> grammarless tree and stamps the fold version, which is the
> highlighted-but-unfolded bug reported the last time this was fixed one layer
> at a time.
>
> The "no syntax logic" rule still holds where it was aimed: the **major**
> branch contains none. What LA.2 adds is step 1 of the open path, not a
> per-feature patch. `mode-architecture.md` §7.4 carries the correction.

- [x] **Step 1: Write the failing test — an explicit major is NOT overridden**

`a_catalog_change_does_not_override_an_explicitly_chosen_major`.
`:markdown-mode` on a `.rs` buffer is the sharpest form: the user's choice
disagrees with the path in BOTH halves, so a re-resolution that ignored intent
would put the buffer back on `rust-mode` *and* swap its grammar back to Rust.
The FALLBACK gate is what makes both survive.

- [x] **Step 2: Write the failing test — a fallback-major buffer IS re-resolved**

`a_catalog_change_re_resolves_a_fallback_major_buffer`, through
`run_tick_pending` with no keypress. Verified to FAIL without the drain call
(`Some(text-mode)` vs `Some(tlangreresolve-mode)`), which is the assertion the
user actually reached for — the major is what carries the keymaps.

Plus `a_catalog_change_attaches_a_language_that_claims_no_major`, the
language-only shape the correction above exists for. It passes today via
`reattach_plugin_syntax` too; LA.3's deletion is what makes it cover the new
path, which is exactly why LA.3 must not weaken it.

- [x] **Step 3: Implement**

- [x] **Step 4: Confirm nothing per-keystroke**

The resolver's only caller is `drain_catalog_changes`, gated on the
`LanguagesRegistered` channel. An idle tick costs one `try_recv` on an empty
channel. The drain coalesces — booting a config with six language plugins
publishes six events and re-resolves once, against the same final catalog six
passes would have seen.

Two bus subscriptions, the ML.3 modeline-element shape: one channel the Editor
drains, one whose only job is to fire `async_landed`. Without the second the
re-resolution would sit until the next keypress — the failure
`boot-composition.md` §3 exists to design out, and the one whose symptom reads
as a rendering bug.

- [x] **Step 5: Gate and commit**

---

## LA.3 — Delete the patches this replaces **(host)** ✅

- [x] **Step 1: Delete `reattach_plugin_syntax` and `last_plugin_langs`**

Both gone, and with them the `syntax_reattached_after_language_registration`
trace. The `Arc::ptr_eq` poll existed only because the loader was wrongly
believed to have no event bus; LA.1 published the fact instead of inferring it.

Collateral fixed while there: `install_inmemory_syntax`'s doc comment (the I4
`openDiff` block) had been orphaned onto `reattach_plugin_syntax` when that
function was inserted above it. It is back on its own function.

- [x] **Step 2: ~~Delete~~ KEEP the refold in `install_document_syntax`**

**The plan was wrong here and the code proves it.** "Folds follow activation"
holds when a major activates — and a language that claims no major activates
nothing, so this refold is the only thing that folds such a buffer. That is not
theory: with the refold removed, the fixture below computes `folds = []` after
the grammar attaches, and `[start_line: 0, end_line: 2]` with it.

The refold is not the per-feature monkey-patching this plan exists to remove.
It is the invariant of the helper it lives in: a new tree means new folds, in
the one place every syntax install goes through.

- [x] **Step 3: Re-run
      `a_language_registered_after_boot_attaches_to_an_already_open_buffer`**

Passes, now through the `LanguagesRegistered` event rather than the poll — the
trigger a real plugin load fires. Routing a test through the real trigger is
not weakening it; the assertions got **stronger** in two ways:

- the fold assertion moved from `last_folded_text_version.is_some()` — which
  boot's own activation already satisfies, so it passed either way — to actual
  fold ranges;
- the fixture's braces are at **column zero**. `foldmethod=syntax` falls back to
  indentation when there is no tree, so an indented body makes the grammarless
  pass produce exactly the fold the grammar would, and the assertion tests
  nothing. Two earlier fixtures (`fn main() {…}` and a block comment) both hit
  that coincidence and passed with the refold deleted.

The test also now asserts the buffer does **not** acquire a major nobody
claimed, and LA.2's duplicate of it was removed rather than left alongside.

- [x] **Step 4: Gate and commit**

---

## LA.4 — End-to-end: argv + org **(host)** 📝

- [ ] **Step 1: The acceptance test**

Boot with a `.org` document, load the org fixture plugin, tick once, and assert
**an org chord fires** — not that syntax is attached. Every previous fix here
passed a proxy assertion and left the reported symptom (`<M-Down>` doing
nothing) in place; the keymap is the thing the user actually reached for.

- [ ] **Step 2: Gate and commit**

---

## Verification, by hand

The bug was reported from a real terminal and the fixes were verified from
logs; this one should be closed the same way. With `--log-level debug`:

1. `lattice todo.org`
2. Without touching anything, confirm org syntax, folds, and `<M-Down>`.
3. Confirm no `syntax_reattached_after_language_registration` (LA.3 deleted it),
   one `major_reresolved_after_catalog_change` for the org buffer, and exactly
   one `LanguagesRegistered` per plugin.
