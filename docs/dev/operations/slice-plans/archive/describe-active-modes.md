# `:describe-active-modes` — buffer mode stack + buffer-scoped bindings

**Status:** ✅ landed (2026-08-04, DAM.1–DAM.6). Design fragment:
[keymap-architecture.md §12.5–§12.6](../../../../architecture/keymap-architecture.md).

## Why

`<C-h>m` has been bound since K.3.2 (2026-06-02) and has never done
what three separate places claim it does. It routes to
`:describe-mode`, whose arg is `ArgDefault::Required`, so a no-arg
invocation arms the interactive `mode:` prompt instead of showing the
buffer's active modes. The claim appears in:

- `crates/lattice-host/src/keymap_help.rs:16` (module doc table)
- `crates/lattice-host/src/keymap_help.rs:256` (`HelpPrefixEntry.doc`)
- `docs/dev/architecture/keymap-architecture.md` §12.1 (corrected)

This arc makes the behaviour real, and keeps the accidental
prompt-for-any-mode path on `<C-h>M`.

## Slices

### DAM.1 — `:describe-active-modes` command + effect ✅

Additive only; `ex:describe-mode` is not touched.

- `wit/types.wit`: add `describe-active-modes()` to the effect
  variant (additive — do **not** widen `describe-mode(string)`).
- `crates/lattice-grammar/src/effect.rs`: `Effect::DescribeActiveModes`.
- `crates/lattice-plugin-host/src/boundary_effect.rs`: both mapping
  directions + a round-trip case in the existing boundary test.
- `crates/lattice-grammar/src/ex_commands.rs`: register
  `ex:describe-active-modes`, `parse_no_args`,
  `LatencyClass::Display`, empty `args_schema`.

- `crates/lattice-host/src/excommand.rs`: an `ALIAS_TABLE` entry.
  Ex-commands register as `ex:<name>`; the bare `:` name is an
  explicit alias, **not** derived — omitting it makes the command
  silently unreachable from the `:` line.

**Tests.** Command resolves by name; no-arg parse; empty
`args_schema` (a Required arg is exactly what makes
`:describe-mode` prompt); `:describe-modes` must NOT exist;
`:describe-mode` keeps its required arg.

**Unplanned work this slice turned up — the completion prefix
rule.** The naming decision was necessary but *insufficient*. `:`
line candidate matching is **fuzzy subsequence**, so
`describe-mode` also matches `describe-active-modes`. Two
candidates skipped `open_completion_popup`'s
`candidates.len() == 1` branch and broke `:describe-mode<Tab>`
anyway — the same bug, from a direction the name choice could not
prevent. Every `describe-*` name that mentions modes hits this, so
no rename avoids it.

Fixed with a rule, not a workaround: on the command-name slot
(`replace_start == 0`) a literal prefix beats a fuzzy subsequence;
arg and file slots keep full fuzzy matching. It requires a
*prefix*, not merely a unique fuzzy hit — `dscrbmode` still opens
the popup rather than rewriting the line. Pinned by four tests in
`crates/lattice-host/tests/command_line_completion.rs`.

### DAM.2 — content builder ✅

`Editor::build_describe_active_modes_content()` in
`crates/lattice-host/src/dispatch.rs`, alongside
`build_describe_mode_content`.

- Resolve buffer via `active_buffer_id()` (**not**
  `document_buffer_id`).
- `active_modes.get(&id)` → `major()` + `minors()`.
- Per mode: `mode_registry.load().get(mode_id)` →
  `DynMode::keymap()` for chords, `help_topics.lookup(id)` for the
  summary, `◆`/`◇` for kind.
- Walk **both** `Keymap::entries` (catalog form, always has `doc`)
  **and** `Keymap::bindings` (chain form, `doc: Option<String>`).
  Walking only `entries` silently under-reports every chain-form
  mode.
- Rows with `doc: None` render the resolved command name.
- Rows emit `[mode-id](help:mode-id)`.

**Also in this slice:** fix `build_describe_mode_content` to read
`active_buffer_id()` instead of `document_buffer_id` for its
"active on current buffer" line — wrong today for every
non-document buffer.

**Tests.** Magit-refs buffer lists `magit-refs-mode` +
`magit-core-mode` with their chords; buffer with no major renders
`(none)` and still lists minors; unresolvable mode id is skipped,
not panicked on; a chain-form (`bind_chord`) binding appears with
its command name where the doc is `None`; `active_buffer_id`
regression test asserting `:describe-mode <name>` reports
"active: yes" for a mode live on a *non-document* buffer.

### DAM.3 — host dispatch + renderer parity ✅

- `crates/lattice-host/src/dispatch.rs`: `Effect::DescribeActiveModes`
  arm → `DisplayBufferRequest` with
  `BufferDisplayCategory::HelpDescribe`, mirroring the
  `Effect::DescribeMode` arm.
- Add the variant to the three `Effect::DescribeMode { .. }`
  classifier match arms in `dispatch.rs` (lines ~33376, ~33518 and
  the display-class list).
- **GPUI in the same patch** (cross-renderer rule):
  `crates/lattice-ui-gpui/src/lib.rs:1202` classifier arm.
- `crates/lattice-ui-tui/src/app/dispatch.rs`: arms at ~994, ~1417,
  ~1559.

**End-of-slice audit:**
`grep -rn "DescribeActiveModes" crates/lattice-ui-gpui/ --include="*.rs"`
— empty grep means GPUI was missed.

### DAM.4 — rebind `<C-h>m`, add `<C-h>M` ✅

`crates/lattice-host/src/keymap_help.rs`: `C_H_M` →
`ex:describe-active-modes`; new `C_H_CAP_M` → `ex:describe-mode`.
Fix the module doc table (currently wrong) and both `doc` strings.

**Tests.** `<C-h>m` resolves to the new command; `<C-h>M` resolves
to `ex:describe-mode`; the existing mode-scope negatives
(Insert/Visual/OperatorPending/Command/Search/Replace) extended to
cover `<C-h>M`; `help_prefix_chord_table_resolves_all_commands`
covers the new row automatically.

### DAM.5 — user docs ✅

- `docs/user/help.md` — the `<C-h>` map table.
- `docs/user/modes.md` — how to see what is live on a buffer.
- `docs/user/magit.md` — worth a pointer; magit is the case where
  major-only help under-reports most visibly.

Docs under `docs/user/**` auto-register as help topics via
`build.rs`, so no registration step.

### DAM.6 — `<C-h>K` → `:describe-bindings` ✅

Depends on DAM.2 (reuses the active-mode walk).

- New `ex:describe-bindings` + `Effect::DescribeActiveBindings`
  (additive WIT variant, same shape as DAM.1).
- Builder unions builtin `entries()` filtered to the current
  `BindingMode` with each active mode's `keymap()`.
- `:keymap` unchanged — stays the exhaustive static reference.
- `<C-h>K` repoints; `<C-h>b` stays `:describe-buffer`.

**Tests.** A chord bound only by an inactive mode does not appear;
a chord from an active minor does; `:keymap` still renders the
full catalog including modes the buffer is not in (regression);
`<C-h>K` resolves to `ex:describe-bindings` and *not* `ex:keymap`.

**Decisions made during implementation** (recorded in §12.6):

- No `ModalState → BindingMode` helper exists in the tree, so the
  builder maps locally. `Command` / `Search` / `Prompt` fold onto
  `Normal` — those states are how the user *typed* the command and
  they are back in Normal by the time the view renders.
- Chord cells are `key_link`ed except rows containing a
  `CharLiteral` (`{char}`) slot, which is not parseable chord
  notation and would produce an unresolvable `:describe-key`
  target.

## Not doing

- **No benchmark.** Display-class, on-demand, O(active modes) ≈ 5,
  no I/O or parsing. Deliberate omission, not an oversight — see
  §12.5.
- **No trie reverse-lookup** (`KeymapLayer` → mode) for DAM.2;
  `DynMode::keymap()` already owns the list. DAM.6 reads the
  builtin catalog directly rather than introducing one.
- **No `timeoutlen` / bare-`<C-h>` work.** Orthogonal; §12.3 stands.
