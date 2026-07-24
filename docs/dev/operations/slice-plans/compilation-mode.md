# Compilation mode — slice plan

> **Status: 🚧 ACTIVE (2026-07-23).** All core slices are ✅ (CM.1–CM.8
> plus the CM.3d headerline / kill / location-highlighting and CM.3e
> catch-all + test-panic + stdout follow-ups below). The plan stays
> **active** (not archived) because CM.5 (ANSI) and CM.6 (WASM parsers)
> are genuinely ⛔ deferred — per the archiving rule, a plan with any
> open slice is not archived. Sequencing companion to the
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
    NoFile`); `*compilation*` synthetic Document created through the
    **mode-owned creation seam** `ModeActivator::ensure_named_document`
    (see below) — `start_compilation` provisions + activates it, then
    runs the service.
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
  - **CM.1-fix (2026-07-22): mode-owned buffer-creation seam.** First
    `:compile` crashed — creation was routed through the `&self`
    `BufferStore::ensure_named_document`, a find-only stub that panicked
    on a miss (activating a mode needs `&mut Editor`). Fixed by adding
    the real create seam **`ModeActivator::ensure_named_document`**
    (`&mut`-backed; `Editor` impl → `ensure_named_synthetic_document`)
    and **removing** the lying `BufferStore::ensure_named_document` stub
    (trait method + handle + registry impl + 6 mock stubs). Design docs
    updated (`design.md` §5.10.5, `multibuffer-views.md` §3.7,
    `kind-agnostic-buffers.md` H.1, `error-list.md`/`compilation-mode.md`).
    Regression: `crates/lattice-host/tests/compile_run.rs` (2) drives the
    real `apply_app_effect(CompileRun)` path. This is the reliable API
    all extension-crate mode-owned buffers use (imperative front-end;
    the declarative peer is `Effect::OpenSyntheticBuffer` for pure
    ex-commands, e.g. AI-log).

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
- **CM.3d — headerline + location-line highlighting + `<C-c>` kill.** ✅ (2026-07-22; refreshed 2026-07-24)
  - **Headerline:** `CompilationHeaderline` (`headerline.rs`) — a
    compilation-mode-owned sticky **view-header virtual row** above
    `*compilation*`, the twin of the project-search headerline. Icon-led
    format: `⟳ "cargo build" …` (running), `✔ "cargo build" ok` (clean),
    `✗ "cargo build" 3e 2w` (errors), `■ "cargo build" killed` (killed).
    Killed state is detected from `"Compilation terminated"` in the Finished
    summary. Five theme elements resolve colours: `compilation.headerline.command`
    (warm yellow), `in_progress` (grey), `success` (green), `failure` (red),
    `dim` (muted, for warning counts and status text). The drain owns the
    state (Reset→`running=true, killed=false`+command; Finished→counts+killed
    detection); registered via the `VirtualRowRegistrar` service in `on_activate`.
  - **Location-line highlighting:** matched `file:line:col` lines get a
    background tint (new theme element `compilation.location`) so navigable
    lines stand out. The location-line index is produced off-thread in the
    drain (`scan_location_lines`), shipped over a native `InboundBus`
    (`CompilationLocationBusHandle`) twin of the severity-gutter seam; colours
    resolve from the theme (no hardcoded RGB).
  - **`<C-c>` kill:** `:compilation-kill` ex-command → `AppEffect::CompilationKill`
    → `CompilationService::kill()`. On Unix the child runs in its own process
    group (`pre_exec`+`setpgid(0,0)`) so `killpg(-pgid, SIGKILL)` terminates
    the shell and all pipeline grandchildren — no orphaned `seq | while`
    processes keeping pipes open. On Windows `TerminateProcess` handles the
    single child. Readers EOF on closed pipes; coordinator publishes Finished
    with `"\nCompilation terminated\n"`. Output streams line-by-line (no
    batching — each line is a single event). `<C-c>` is a mode-dispatchable
    chord bound in `compilation-mode`'s keymap.
  - Keymap is now `gr` (recompile) / `<CR>` (jump + sync index) / `<C-c>` (kill).
- **CM.3e — catch-all + test-panic parsers + stdout parsing.** ✅ (2026-07-23)
  - `GeneralParser` (`parsers/general.rs`) — catch-all that finds
    `file:line:col` (or `file:line`) **anywhere** in a line (unanchored,
    `Regex::find_iter`), gated on `is_file_like` (path contains `/` or `.`)
    to reject timestamps / version-strings / `word:digits` noise. Emits `Info`
    + empty message. Makes any tool's embedded location navigable.
  - `TestPanicParser` (`parsers/panicked.rs`) — matches Rust test / `panic!`
    output (`thread '<name>' panicked at path:line:col[: message]`) as `Error`.
  - `ParserRegistry::with_builtins` now registers **four** parsers
    (CargoRustc, GnuStyle, TestPanic, General — the catch-all last),
    de-duped by `(path, line, col)` so the richest match wins;
    `match_location_line` / `match_severity` chain all four for
    `<CR>`-jump + gutter.
  - **stdout AND stderr both parsed** (`service.rs`): compile diagnostics go to
    stderr, but test-failure output / thread panics go to stdout. Two dedicated
    reader threads share an `Arc<Mutex<Vec<ErrorEntry>>>` accumulator; each
    sends the full list through the `InboundBus`, so neither stream clobbers the
    other. So `:compile cargo test` populates the error list with panicking
    `file:line`.
  - **Test:** general-parser + panic-parser unit tests (anywhere-in-line,
    timestamp/version rejection, absolute paths, non-file rejection);
    `service.rs::stderr_diagnostics_populate_the_error_list`; the four-parser
    dedup path in `parser.rs`.
- **CM.3f — generic multibuffer `<CR>` jump-to-source.** ✅ (2026-07-23)
  - `<CR>` → `action:multibuffer-jump-to-source` moved OUT of the search
    provider (`providers/search.rs` shrank ~115 lines) into the shared
    `MultibufferMode` (`lattice-multibuffer/src/mode.rs`): `on_activate`
    registers the handler on the per-buffer `ActionHandlerRegistry`, so search,
    `*problems*`, and narrow all get `<CR>`-jump + the excerpt-jump motions
    (`]e`/`[e`/`]E`/`[E`) for free. Documented in
    [`../../architecture/multibuffer-views.md`](../../architecture/multibuffer-views.md)
    §3.6. (This slice belongs to the multibuffer substrate but is recorded
    here because it completes compilation's `*problems*` navigation.)

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
