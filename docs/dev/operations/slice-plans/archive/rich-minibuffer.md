# Rich minibuffer — slice plan

> **Status: ✅ DONE (MB.1–MB.5 ✅ 2026-07-24).**
> Sequencing companion to the design
> fragment [`../../../../architecture/rich-minibuffer.md`](../../../../architecture/rich-minibuffer.md)
> (the *what + why*). This file owns *when + in what order + status*.
> User docs land per slice (`docs/user/modal-editing.md` / `ex-commands.md`
> command-line section).

Two-tier model: the `:` line is a readline-grade buffer (insert-only);
`<C-x><C-e>` expands the command into a full editing mini-buffer;
history is walkable + picker-browsable. Land each slice green; ship
doc + test + graceful-error together. The `:` line is latency-critical —
every slice re-verifies keystroke→glyph.

## Phase 1 — the readline `:` line (tier 1)

- **MB.1 — `command-line-mode` + buffer-backed readline `:` line.** ✅
  (2026-07-23) The `:` line is a focused synthetic `*command-line*`
  `Document`; typing routes through the universal Insert dispatcher onto
  it (real mid-line editing). `command_line: String` + the projection are
  **fully retired** — `Editor::command_line()` computes the text from the
  buffer (single source of truth), `set_command_line_text` owner-writes
  it, and all ~160 read/write sites were migrated. Acceptance tests in
  `app/cmdline.rs` (`mb1_*`) cover mid-line insert, cancel/restore, and
  history-seed.
  - `*command-line*` one-line synthetic `Document` (unlisted, `NoFile`),
    created via `ModeActivator::ensure_named_document` on `:`;
    `command-line-mode` major, **insert-only** (no Normal entry).
  - Keys route through the normal Insert dispatcher — the universal
    insert-mode readline chords (`<C-w>`/`<C-u>`/`<C-a>`/`<C-e>`/`<C-b>`/
    `<C-f>`, `<Left>`/`<Right>`, `<Del>`) edit it directly; **retire
    `translate_command` + the `command_line: String` field**.
  - Mode owns submit/cancel: `<CR>` submit (buffer text → existing
    `:`-parser → `CommandInvocation` → history push → close → restore
    prior buffer → dispatch); `<Esc>`/`<C-c>` cancel + restore.
  - History walk: `<C-p>`/`<C-n>` + `<Up>`/`<Down>` seed from
    `command_history`, preserving `command_history_pending`.
  - Render: echo-area row draws the tier-1 buffer's line + cursor —
    **TUI + GPUI same slice**. Keep the existing completion popup working.
  - **Test:** open→edit-mid-line (`<Left>`/`<C-w>`/`<C-a>`)→`<CR>` runs
    the right command; `<Esc>`/`<C-c>` cancels + restores prior
    buffer/cursor; typo fixed by `<C-b>`-walk submits correctly; history
    `<C-p>`/`<C-n>` walk restores pending text; latency probe unmoved.

## Phase 2 — expand the command line in place (tier 2)

- **MB.2 — `<C-x><C-e>` in-place expand + full modal.** ✅ (2026-07-23)
  Landed as four slices: **MB.2a** state machine (`expanded` flag,
  `command_line_expanded()`, `<C-x><C-e>` toggle, modal Command⇄Insert,
  `:`-no-op guard); **MB.2d** tier-2 editing semantics (`<CR>` = newline,
  `<Esc>` = to Normal, arrows = multi-line nav, all gated on
  `expanded`); **MB.2b/c** the render band in TUI + GPUI (grows in place,
  pushes panes/mode-lines up, half-frame default). **Deferred:** the
  `command-line.expand-height` typed option (band is half-frame for now)
  and a `--features gui` runtime check of the GPUI band geometry — see
  MB.2e below.
- **MB.2e — `command-line.expand-height` option.** ✅ (2026-07-24)
  Typed `command-line.expand-height` value type (`ExpandHeight`:
  `half` default / `full` / `Fixed(rows)`) in `lattice-config`,
  registered as `CommandLineExpandHeight` (`#[name("command-line.expand-height")]`,
  Editor group). `ExpandHeight::rows(frame_height)` resolves the policy
  against the live frame (host publishes the policy in
  `ModelineRenderState.cmdline_expand_height`; both renderers apply it —
  the draw path never reads config). TUI reserves the band rows; GPUI
  floors the band with a `min_h` so a short command still opens the
  configured height and the panes flex-shrink up. Tests: `ExpandHeight`
  parse/rows unit tests (`lattice-config`); `mb2e_*` accessor+rows test
  (`app/cmdline.rs`). GPUI wired + type-checked (`-p lattice-ui-gpui`
  default **and** `--features window`); the interactive
  `cargo run --features gui` visual confirmation is a manual step (a
  windowed GUI can't run headless in CI).
  - Only in `command-line-mode`: `<C-x><C-e>` toggles the `*command-line*`
    buffer's **expanded** state — the **same surface grows in place**,
    upward, **pushing the mode-line (and content) above it** into a
    full-width bottom mini-buffer band, and **enables full modal editing**
    (Normal/Insert/Visual, registers, undo, multi-line). NOT a separate
    split/pane/buffer — same buffer, taller, no copy-back.
  - **`:` no-op guard:** in the expanded band's Normal mode, the `:`
    enter-command-line chord is a **no-op** (already in the command line).
    (Automatic in tier 1 / insert-only.)
  - **`command-line.expand-height` option** (§5.12): how tall (`half`
    default / fixed rows / `full`). Render pushes the mode-line up (both
    peers).
  - Collapse (`<C-x><C-e>` again / mode chord) shrinks back to the one-row
    readline `:` line with the edited text → user `<CR>`s to execute
    (**no auto-execute**). `<C-c>` cancels (discards expanded edits).
  - **Test:** expand→multi-word edit with motions/registers→collapse lands
    edited text in the one-row line, then `<CR>` runs it; `:` in the
    expanded Normal mode is a no-op; cancel keeps the original;
    `expand-height` changes the band size; mode-line pushed up; TUI+GPUI.

## Phase 3 — history picker

- **MB.3 — `q:` / `:history` fuzzy history picker.** ✅ (2026-07-24)
  - `q:` (Normal-mode Builtin entry chord, sibling to `:`/`/`) + the
    `:history` ex-command open a fuzzy picker over `command_history`
    via the trait-driven `HistorySource` generator (id `history`,
    registered in `first_party_generators`; `:picker history` works
    for free). **Accept loads into the editable `:` line, does NOT
    execute** — new `PickerAcceptOutcome::LoadCommandLine` /
    `RoutingPayload::LoadCommandLine` → host `open_command_line(&text)`.
    Host-internal (rejected at the plugin WIT boundary with a typed
    error; no plugin surface). `q:`-from-the-expanded-band seeds the
    filter with the in-progress `:` text (`command_line_active()`
    gate; tier-1 is insert-only so `q:` can't fire there). Trie
    exact-match precedence keeps `qa`…`qz` macro recording intact.
  - **Test:** `HistorySource` unit tests (`lattice-picker` /
    `lattice-ui-tui::picker_sources`: newest-first, empty-history
    error, accept-translates); `mb3_*` acceptance in `app/cmdline.rs`
    (`q:` loads an editable command that `<CR>` then runs; `:history`
    opens the same picker; empty-history graceful; band-seeded filter).
  - **No new bench:** `q:` opens a picker (off the keystroke→glyph
    path); the picker infra is already benched, and MB.3 adds no work
    to the `:`/`/` latency-critical lines.

## Phase 4 — richness

- **MB.4 — highlighting + live decorations.** ✅ (2026-07-24)
  - `command_line_decorations(line, registry)` in `lattice-host::excommand`
    tokenizes the `:` line into typed spans (command word / range prefix /
    bang / `s///` pattern·replacement·flags), maps each to a
    `lattice_cells::style::Style`, validates via the same ex-parser (live
    **error indicator** — unknown command, surfaced once the word is
    "committed" to avoid mid-typing flicker), and builds a **parameter
    hint** from the resolved command's `ArgSpec`. `:s///` **substitution
    preview** already lands live (`refresh_substitute_preview`).
  - Mode-owned: `Editor::refresh_command_line_decorations()` (peer of
    `refresh_substitute_preview`) recomputes on every command-line edit
    on the **actor thread** (one-line tokenize, never the render thread)
    and publishes `ModelineRenderState.cmdline_decorations`; both peers
    read the published data and only map `Style`→colour + slice the line.
  - **Test:** `mb4_*` model tests (`excommand.rs`: keyword/error/prefix/
    substitute tokenization, committed-vs-prefix error gate); `mb4_*`
    wiring tests (`app/cmdline.rs`: typing populates decorations, live
    unknown-command error, submit/cancel clear). TUI + GPUI (`--features
    window`) render wired in lockstep; no paint-time parse.

## Phase 5 — unification

- **MB.5 — unify the `/` search line + other prompts.** ✅
  (2026-07-24)
  - `/` `?` migrate onto the substrate as `search-line-mode` (readline +
    search history + its own `<C-x><C-e>` expand); delete the parallel
    search `String`. Substrate ready for `git-commit-line` / `repl-input`.
  - **Test:** `/` edits mid-pattern + walks search history; incremental-
    search preview intact.

## Cross-cutting notes

- **Latency guard.** `:`/`/` are on the keystroke→glyph path; MB.1/MB.5
  must show the ratchet distribution unmoved (the edit is the benched
  one-line buffer path).
- **Cross-renderer.** Every render touch (MB.1 echo-area buffer row, MB.2
  split, MB.4 decorations) lands TUI **and** GPUI in the same slice.
- **Mode-ownership.** `command-line-mode` / `command-line-edit-mode` /
  `search-line-mode` own their chords + handler bodies + decoration
  providers + the `command-line` grammar; zero minibuffer-specific
  `Editor::do_*`. Acid test: `command_line: String` + `translate_command`
  are deleted, replaced by a focused buffer.
