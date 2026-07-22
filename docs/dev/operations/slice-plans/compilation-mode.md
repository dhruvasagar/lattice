# Compilation mode — slice plan

> **Status: 🚧 IN PROGRESS (2026-07-21).** Sequencing companion to the
> design fragments
> [`../../architecture/compilation-mode.md`](../../architecture/compilation-mode.md)
> (the runner + `*compilation*`/`*problems*`) and
> [`../../architecture/error-list.md`](../../architecture/error-list.md)
> (the core error-list / quickfix substrate — CM.2/CM.7/CM.8). This file
> owns *when + in what order + status*. User docs:
> `docs/user/compilation.md`, `docs/user/error-list.md`.

Native built-in (`lattice-compilation` crate + `SubsystemBoot` install
seam). Option C: streaming `*compilation*` buffer primary, quickfix
navigation, multibuffer `*problems*` view secondary. Land each slice
green; ship doc + bench + test + graceful-error together.

## Phase 1 — the streaming compilation buffer (the 80% value)

- **CM.1 — crate + streaming run.** ✅ (2026-07-22)
  - `lattice-compilation` crate; `CompilationMode` (major, `ReadOnly +
    NoFile`); `*compilation*` synthetic Document via
    `ensure_named_synthetic_document(name, mode_id, SYNTHETIC_BUFFER_FLAGS)`.
  - `:compile <cmd>` / `:recompile` / `:make` ex-commands.
  - Off-thread process: `spawn_blocking` child + blocking stdout/stderr
    reader → `CompilationOutput` typed event (`register_event!`) → boot
    mpsc → drain in `run_tick_pending`, coalesced into one
    `append_to_owned_buffer` batch.
  - Lifecycle: kill-prior-on-recompile (SIGTERM→SIGKILL), clear+restream,
    exit summary line (`info!` one-shot).
  - Crate-owned `install(boot)`; one line in `editor_boot.rs` Phase-B.
  - **Test:** ✅ 11 tests — mode id/kind/options (ReadOnly+NoFile),
    `apply_chunk` semantics, `run` streams `echo hello` (Reset→Append→
    Finished over a real EventBus), graceful `:recompile` with no prior
    cmd. **Bench:** ✅ `compilation_append` — append cost stays flat
    (~8.8µs p50) across logs of 0/250/2000/10000 batches (no degradation
    as the log grows).

## Phase 2 — quickfix substrate + navigation

- **CM.2 — quickfix core substrate.** ✅ (2026-07-22)
  - `QuickfixEntry` / `QuickfixList { entries, index }` on `Editor`
    (shaped like `position_history`).
  - Generic ex-commands `:cnext`/`:cprev`/`:cc [N]`/`:cfirst`/`:clast`,
    each stepping via `jump_to_file_line_col` (records position history).
  - Builtin normal-mode `]q`/`[q`; no-op on empty list.
  - Repoint existing `:cnext`/`:cn`/`:cp` aliases: quickfix when
    non-empty, diagnostic fallback when empty.
  - **Test:** ✅ 8 unit (set/step-wrap/`jump_to` bounds/first/last/empty)
    + 3 integration (cross-file Next walk+wrap+position-history push;
    Prev-wrap/`:cc N`/`:cfirst`/`:clast`/out-of-range; empty-list
    diagnostic fallback). No bench: nav is O(1), reuses the benched
    `jump_to_file_line_col` path. No GPUI parity needed (reuses existing
    signal fan-out).
- **CM.3a — parser registry + quickfix population + `gr`.** ✅ (2026-07-22)
  - Done: `QuickfixEntry`/`QuickfixSeverity` moved to `lattice-protocol`;
    `CompilationParser` trait + `ParserRegistry` + cargo/rustc (multi-line)
    + gnu-style parsers (fancy-regex, compiled once); stderr parsed in the
    reader thread → accumulated → `InboundBus` → `AppEffect::QuickfixSet`
    → `set_quickfix_list`; `Reset` clears; `gr`→`:recompile`. 20+10 tests;
    `compilation_parse` bench ~2.3M lines/sec.
- **CM.3b — `<CR>`-jump.** ✅ (2026-07-22)
  - Done: `<CR>` on a location line reads the cursor line, `parse_location_line`s
    it, emits `AppEffect::CompileJumpToLocation` → host `jump_to_file_line_col`
    + `QuickfixList::set_index_to_matching`. No line→entry map needed (parses
    the line directly, interleaving-proof). No renderer edit. 23+12 tests.
- **CM.3c — gutter severity decoration.** ✅ (2026-07-22)
  - **Native path (NOT the plugin seam):** severity index built off-thread in
    the drain (`scan_severities` per appended line, tracking the running buffer
    line number) → `CompilationGutterBusHandle` `InboundBus` →
    `AppEffect::CompilationGutterSet { buffer: u32, entries: Vec<(u32,
    QuickfixSeverity)> }` → host arm converts `QuickfixSeverity → GutterSeverity
    Level` (once, via `gutter_level`) and writes the `render_state
    .compilation_severity` per-buffer slot; both renderers inject a
    `lattice_mode::CompilationSeverityData` carrier into `DecorationCtx`;
    `CompilationMode::gutter_decorations` reads it and emits
    `GutterDecoration::Severity` (the existing LSP-shared gutter column — zero
    renderer glyph edit). Mirrors LSP/diff exactly; does NOT touch
    `wasm_gutter_decorations` / `GutterDecorationSourceRegistry` (the plugin
    decoration seam). `AppEffect` carries `QuickfixSeverity` (not
    `GutterSeverityLevel`) to keep `lattice-grammar` free of a `lattice-mode`
    dep and avoid a lossy `Hint↔Note` round-trip; the sole conversion is the
    host arm. `buffer` rides as the raw `u32` (`BufferId` is not `Serialize`).
  - **Test:** ✅ `match_severity` (cargo/gnu headers; `-->`/gnu-short/prose →
    None), `scan_severities` (absolute line numbers + base offset + empty),
    `gutter_level` mapping (Note→Info), `CompilationMode::gutter_decorations`
    (carrier→decorations + graceful-empty), `count_newlines`. TUI+GPUI parity
    compile-verified (`--features window`) + zero-`lattice-compilation`-dep
    grep audit. WIT boundary: `CompilationGutterSet` is a typed-`Err`
    deferred arm (native built-in), mirroring `QuickfixSet`.
  - Coverage boundary: the full drain→bus→AppEffect→host→render_state→renderer
    path is covered per-seam (scan extraction, `gutter_level` mapping,
    `gutter_decorations` consumption) rather than one integration test — the
    same seam-by-seam strategy as CM.2/CM.3a. Link styling on matched lines
    (design §5 "and a link") is a separate visual concern, out of this slice.

## Phase 3 — the problems view (secondary; the multibuffer instinct)

- **CM.4 — `*problems*` multibuffer view.** ✅ (2026-07-22)
  - Done: `:copen`/`:cclose` open/close a `ProblemsMinorMode` multibuffer
    grouping quickfix entries as anchored source excerpts (search provider
    template), editable-in-place, headerline count. 125 tests;
    `multibuffer_is_a_regular_buffer.rs` unchanged (no kind-branch).

## Phase 4 — error-list completeness (design: `error-list.md`)

- **CM.7 — chord scheme + file-nav + no-fallback + tool-agnostic.** ✅ (2026-07-22)
  - Chord scheme (Builtin, any buffer): `[Q`/`]Q` (first/last),
    `[qq`/`]qq` (prev/next), `[qf`/`]qf` (prev/next file) — `]q`/`[q`
    are prefixes. Replaced CM.2's direct `]q`/`[q`.
  - `:cnextfile`/`:cprevfile` (+ `:cnf`/`:cpf`), `:cr`/`:crewind`→cfirst;
    new `QuickfixTarget::{NextFile, PrevFile}` + `QuickfixList::step_file`
    (first entry of the target file group, wraps).
  - **Removed the empty-list diagnostic fallback** — error-list commands
    touch only the list (echo `no error list`, vim `E42`). Diagnostics
    stay on the dedicated `[d`/`]d` + `:diagnostics` (user decision:
    keep diagnostics as-is; no `:d*` parallel vocabulary).
  - **Tool-agnostic validated:** `crates/lattice-compilation/tests/tool_agnostic.rs`
    — grep/eslint/gcc/generic (NON-cargo) populate the list; hardened the
    gnu parser (grep no-space `path:line:text` via a file-like-path form;
    fixed a pre-existing `hh:mm:` timestamp false-positive). Traversal
    proven with no `*compilation*` buffer open. 14 unit + 8 integration
    + 37 compilation tests.
- **CM.8 — quickfix picker.** ✅ (2026-07-22)
  - `:clist`/`:cl` → `Effect::ListQuickfix` → `Editor::do_list_quickfix`
    builds a fuzzy picker from `quickfix.entries()`, mirroring
    `:diagnostics`/`do_list_diagnostics` (shared `PickerSource::LspLocations`
    + `JumpToLspLocation`). The third view of the list (step / pick /
    group). Host-only effect (WIT→`None`). Tests in `quickfix_navigation.rs`.
  - **Docs:** `docs/user/error-list.md`, `docs/user/compilation.md`,
    `docs/dev/architecture/error-list.md` all ✅ updated (readable
    `next-error` names + vim `:c*` aliases, `q`-chords, picker,
    no-fallback, three views). Full quickfix→error-list type rename.

> **Deferred (separate future commit, user-requested):** a parallel
> **diagnostics vocabulary** — `:dfirst`/`:dprev`/`:dprevfile`/`:dnext`/
> `:dnextfile`/`:dlast` + `[D`/`[dd`/`[df`/`]dd`/`]df`/`]D` — was
> discussed and **declined for now**: diagnostics keep their existing
> current-file `[d`/`]d` + `:diagnostics` picker. Revisit only if a
> project-wide diagnostic list is wanted (vim's location-list model would
> be the substrate-faithful home, reusing `QuickfixList`).

## Phase 5 — polish / extensibility (deferred)

- **CM.5 — ANSI-SGR → decorations** over captured text. ⛔ deferred.
- **CM.6 — WASM-contributable parsers** (Phase 7 plugin host / WIT). ⛔ deferred.

## Cross-renderer note

CM.3c's severity gutter decoration touches the renderer — TUI and GPUI
in the same patch (per the cross-renderer rule). End-of-slice grep audit
for missed GPUI sites.
