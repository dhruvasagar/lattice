# surround-mode — slice plan

> **Status: 🚧 ACTIVE (2026-07-24).** All slices are 📝 planned.
> Sequencing companion to the design fragment
> [`../../architecture/plugin-surround.md`](../../architecture/plugin-surround.md).

Native minor mode in `lattice-mode` (SL.0). Three operators registered in
`CommandRegistry` at boot; keymap at `MinorMode(surround-mode)`; one infrastructure
change (multi-char capture in `action_from_bound_with_capture`). Land each slice
green; ship doc + bench + test + graceful-error together.

## Slice SU.1 — Pair detection + surround operators

- **SU.1a — `open_close_pair` mapping.** 📝
  - Function in `lattice-mode/src/modes/surround.rs`: `fn open_close_pair(ch: char) -> Option<(char, char)>`
  - Maps bracket/quotes to canonical (open, close) form.
  - Unit tests for every bracket type + symmetric pairs.

- **SU.1b — `find_surround_pair` algorithm.** 📝
  - `fn find_surround_pair(document: &Document, cursor: Position, target: char) -> Option<(usize, usize)>`
  - Backward scan with closer-stack; forward scan with opener-stack.
  - Returns byte-offset pair or `None`.
  - Unit tests: nested `((x))` → inner; `"hello"` → bounds; cursor between `()`; no match.

- **SU.1c — Register operators in `CommandRegistry`.** 📝
  - `pub fn register_surround_operators(registry: &mut CommandRegistry) -> SurroundOperators`
  - `SurroundOperators { delete: OperatorId, change: OperatorId, add: OperatorId }`
  - All three: `repeatable: true`, `blockwise_per_row: false`.
  - Operator closures call `find_surround_pair` / `open_close_pair`.
  - Unit tests per operator: happy-path `ds"`, `cs"'`, `S{char}`; no-match no-op; undo-batch.

## Slice SU.2 — Multi-char capture + keymap + `SurroundMode`

- **SU.2a — Multi-char capture in `action_from_bound_with_capture`.** 📝
  - `crates/lattice-host/src/keymap_normal.rs:2007`: extend to handle `captured.len() > 1`.
  - When len > 1, produce `Args::List([ArgValue::Char(c) for c in captured])`.
  - Existing single-char callers unaffected (len == 1 path unchanged).
  - Test: `[f, CharLiteral]` still captures single char; new test verifies multi-char `Args::List`.
  - Affected file: 1 function, ~10 lines changed.

- **SU.2b — `SurroundMode` struct + `Mode` impl + keymap.** 📝
  - `pub struct SurroundMode` in `lattice-mode/src/modes/surround.rs`.
  - `Mode::kind() = Minor`, `activation_policy() = Global`, `Guard = ()`.
  - `Mode::keymap()` returns bindings per design §3.
  - Keymap uses `keymap_entry!` macro (table form), resolved against `CommandRegistry` at
    translate time.
  - Test: mode registers without conflict; keymap entries resolve; activation on document buffer.

- **SU.2c — Declining bindings for `S` in Visual mode.** 📝
  - Bind `[S]` → `Partial` (arms `[S, CharLiteral]`) in surround-mode layer.
  - Overrides builtin `S` = `operator:change` in Visual mode.
  - When surround-mode is active and user types `S` then char, surround-add fires.
  - When surround-mode is NOT active (buffer with no surround-mode), builtin `S` fires.
  - Test: visual `S"` wraps selection; deactivated mode → builtin `S` works.

## Slice SU.3 — Boot integration + tests + benchmarks

- **SU.3a — Boot integration.** 📝
  - `register_surround_modes(registry: &mut ModeRegistry)` in
    `lattice-mode/src/modes/surround.rs`.
  - Called from `register_foundation_modes` in `modes/mod.rs`.
  - `register_surround_operators(registry)` called from grammar bootstrap in
    `crates/lattice-host/src/editor_boot.rs`.
  - `operator_prefix` mapping extended for surround operators.
  - Compile check: `cargo build -p lattice-cli` succeeds (mode + operators wired).

- **SU.3b — Integration tests.** 📝
  - `crates/lattice-host/tests/surround.rs`:
    - `ds"` on `"hello"` → `hello`
    - `cs"'` on `"hello"` → `'hello'`
    - `cs)]` on `(hello)` → `[hello]`
    - `yss"` on `hello` → `"hello"`
    - Visual `S(` on selected `hello` → `(hello)`
    - No-op on unmatched `ds(` when cursor not inside `()`
    - Dot-repeat: `.` after `ds"` repeats the delete
    - Register: `"ads"` yanks the deleted surround into register `a`
  - Each test drives `dispatch_blocking` through the real Editor.

- **SU.3c — Performance benchmarks.** 📝
  - `crates/lattice-grammar/benches/surround.rs`:
    - `find_surround_pair` on a 2000-line file (worst-case O(line))
    - Operator latency: p50 for `ds"` on a 2300-line Rust file
    - Pair-mapping: `open_close_pair` overhead (should be O(1), trivial)
  - Update `docs/dev/operations/benchmarks.md`.

- **SU.3d — Update implementation ledger.** 📝
  - Add "surround" entry to `docs/dev/operations/implementation.md`.
  - Mark SU.1–SU.3 status.

## Deferred to v2

- **SU.4 — `ys{motion}{char}`.** ⛔ Needs post-motion char capture infrastructure.
- **SU.5 — HTML/XML tag targets** (`t` — `cst<div>`, `dst`, `ysiwt`). ⛔ Needs tag-scanning parser.
- **SU.6 — Config gate** (`surround.enabled` typed option + `:surround-mode` toggle). ⛔

## Cross-renderer note

Surround operators are renderer-agnostic (grammar layer). No renderer files touched.
No TUI/GPUI parity concern for this feature.
