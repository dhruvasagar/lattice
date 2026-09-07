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
**All are deleted by this plan, not extended** — they are the per-feature
monkey-patching this design exists to remove.

| Landed | Fate |
|---|---|
| `Editor::reattach_plugin_syntax` (`ab512368`) | **delete** — re-resolution covers it |
| `Editor::last_plugin_langs` field (`ab512368`) | **delete** — the event replaces `Arc::ptr_eq` polling |
| the refold inside `install_document_syntax` (`f5c2099b`) | **delete** — folds follow activation |
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
| LA.2 | Re-resolve the major for fallback-major buffers | 📝 |
| LA.3 | Delete the three patches this replaces | 📝 |
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

## LA.2 — Re-resolve the major for fallback-major buffers **(host)** 📝

**Files**
- Modify: `crates/lattice-host/src/editor_boot.rs` — subscribe, bridge to the
  actor the way the `SyntaxReparsed` → `cells_wake` forwarder does
- Modify: `crates/lattice-host/src/dispatch.rs` — the re-resolution itself

**Interfaces**
- `Editor::reresolve_majors_after_catalog_change()`: for every open
  **document** buffer whose major is still the FALLBACK, run §7.4's ordered
  resolver and activate the winner. Activation emits `MajorEntered`, so minors,
  keymaps, syntax and folds all follow the normal path — this function itself
  must contain **no** syntax, fold or keymap logic. If it does, the design was
  not implemented.

- [ ] **Step 1: Write the failing test — an explicit major is NOT overridden**

Write this one FIRST. It is the only way the fix can do damage, and a
re-resolution that silently replaces a user's `:org-mode` is worse than the bug
being fixed.

- [ ] **Step 2: Write the failing test — a fallback-major buffer IS re-resolved**

Through `run_tick_pending` with no keypress, per
`async-results-must-reach-the-screen-without-a-keypress`.

- [ ] **Step 3: Implement**

- [ ] **Step 4: Confirm nothing per-keystroke**

The resolver must be reached only from the event, never from a dispatch tail.

- [ ] **Step 5: Gate and commit**

---

## LA.3 — Delete the patches this replaces **(host)** 📝

- [ ] **Step 1: Delete `reattach_plugin_syntax` and `last_plugin_langs`**
- [ ] **Step 2: Delete the refold from `install_document_syntax`**, keeping the
      helper and its active-buffer write
- [ ] **Step 3: Re-run
      `a_language_registered_after_boot_attaches_to_an_already_open_buffer`**

It must still pass, now via activation rather than the patch. If it needs
weakening to pass, LA.2 is incomplete — say so and stop.

- [ ] **Step 4: Gate and commit**

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
3. Confirm no `syntax_reattached_after_language_registration` (LA.3 deleted it)
   and exactly one `LanguagesRegistered` per plugin.
