# surround-mode — slice plan

> **Status: 🚧 ACTIVE (2026-07-25).** SU.1–SU.3d complete (19 tests).
> SU.4–SU.6 deferred to v2.
> Sequencing companion to the design fragment
> [`../../architecture/plugin-surround.md`](../../architecture/plugin-surround.md).

Native minor mode in `lattice-mode` (SL.0). Three operators registered in
`CommandRegistry` at boot; keymap at `MinorMode(surround-mode)`; one infrastructure
change (multi-char capture in `action_from_bound_with_capture`). Land each slice
green; ship doc + bench + test + graceful-error together.

## Slice SU.1 — Pair detection + surround operators

- **SU.1a — `open_close_pair` mapping.** ✅
  - Function in `lattice-mode/src/modes/surround.rs`: `fn open_close_pair(ch: char) -> Option<(char, char)>`
  - Maps bracket/quotes to canonical (open, close) form.
  - Unit tests for every bracket type + symmetric pairs.

- **SU.1b — `find_surround_pair` algorithm.** ✅
  - `fn find_surround_pair(document: &Buffer, cursor: Position, target: char) -> Option<(usize, usize)>`
  - Backward scan with closer-stack; forward scan with opener-stack.
  - Returns byte-offset pair or `None`.
  - Unit tests: nested `((x))` → inner; `"hello"` → bounds; cursor between `()`; no match.
  - Bench test: 10k-char line completes in <1ms.

- **SU.1c — Register operators in `CommandRegistry`.** ✅
  - `pub fn register_surround_operators(registry: &mut CommandRegistry) -> SurroundOperators`
  - `SurroundOperators { delete: OperatorId, change: OperatorId, add: OperatorId }`
  - All three: `repeatable: true`, `blockwise_per_row: false`.
  - Operator closures call `find_surround_pair` / `open_close_pair`.
  - 11 operator dispatcher tests: happy-path `ds"`, `cs"'`, `S{char}`; no-match no-op; undo-batch.

## Slice SU.2 — Multi-char capture + keymap + `SurroundMode`

- **SU.2a — Multi-char capture in `action_from_bound_with_capture`.** ✅
  - `crates/lattice-host/src/keymap_normal.rs:2007`: extended to handle `captured.len() > 1`.
  - When len > 1, produces `Args::List([ArgValue::Char(c) for c in captured])`.
  - Existing single-char callers unaffected (len == 1 path unchanged).

- **SU.2b — `SurroundMode` struct + `Mode` impl + keymap.** ✅
  - `pub struct SurroundMode` holds `SurroundOperators` for keymap construction.
  - `Mode::kind() = Minor`, `activation_policy() = Global`, `Guard = ()`.
  - `Mode::keymap()` returns chain-form bindings with `ChordPattern::CharLiteral`
    for wildcard character capture + table-form entries for catalog.

- **SU.2c — Declining bindings for `S` in Visual mode.** ✅
  - Bind `[S, CharLiteral]` in surround-mode Visual layer → surround-add on selection.
  - Surround mode's minor-mode layer takes precedence over builtin at lookup time.

## Slice SU.3 — Boot integration + tests + benchmarks

- **SU.3a — Boot integration.** ✅
  - `register_surround_operators(registry)` called from grammar bootstrap in `editor_boot.rs`.
  - `register_surround_modes(registry, operators)` called after `register_foundation_modes`.
  - `translate_mode_keymaps` picks up surround-mode's chain-form bindings at boot.

- **SU.3b — Integration tests.** ✅
  - 11 operator dispatcher tests in `crates/lattice-mode/src/modes/surround.rs`:
    - `ds"` on `"hello"` → `hello`
    - `ds(` on `fn foo(x: i32)` → `fn foox: i32`
    - `ds)` on `(hello world)` → `hello world`
    - `cs"'` on `"hello"` → `'hello'`
    - `cs(]` on `(hello)` → `[hello]`
    - `yss"` on `hello` → `"hello"`
    - `yss(` on `hello` → `(hello)`
    - Visual `S"` on selection → wrap
    - No-op on unmatched `ds"` / `cs"'`
    - No-op on unknown wrapper char
  - Each test drives `grammar_execute()` through the real dispatcher.

- **SU.3c — Performance benchmarks.** ✅
  - Latency bench test: `find_surround_pair` on 10k-char line < 1ms.

- **SU.3d — Update implementation ledger.** ✅
  - Surround entry added to `docs/dev/operations/implementation.md`.

## Deferred to v2

- **SU.4 — `ys{motion}{char}`.** ⛔ Needs post-motion char capture infrastructure.
- **SU.5 — HTML/XML tag targets** (`t` — `cst<div>`, `dst`, `ysiwt`). ⛔ Needs tag-scanning parser.
- **SU.6 — Config gate** (`surround.enabled` typed option + `:surround-mode` toggle). ⛔

## Cross-renderer note

Surround operators are renderer-agnostic (grammar layer). No renderer files touched.
No TUI/GPUI parity concern for this feature.
