# Verification checklist

A walk-through to manually verify the features and fixes shipped in
the recent session by running the editor. Open a real file (the
project's own README is fine) so the buffer has multiple lines and
some unicode (`§`).

```sh
cargo run --release -- README.md
```

Each section maps to one or more commits. If anything diverges from
the expected behaviour, capture the exact keystrokes you typed and
the output you saw — that's enough to reproduce.

---

## 1. Build & launch

- [ ] `cargo build --release` succeeds without warnings
- [ ] `cargo test --workspace` reports 1099 passing, 0 failing
- [ ] `cargo clippy --workspace --all-targets` clean
- [ ] `cargo fmt --all -- --check` clean
- [ ] `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace` clean
- [ ] `cargo run --release -- README.md` opens the file in the TUI

---

## 2. Cursor display-width fix
*(commit `3f10caa`, fixes the `/Perf` issue you found)*

Open a file with multibyte chars on a line. README.md line 11 and
the project's CLAUDE.md line 54 contain `§` (2 bytes / 1 cell).

- [ ] `/Perf<CR>` highlights `Perf` on a `§`-containing line
- [ ] After search submit, the cursor sits exactly on `P`, not on `e`
- [ ] `n` jumps to the next match; cursor lands on `P` of that match
- [ ] `N` jumps to the previous match; cursor lands on `P`
- [ ] On a CJK / emoji line (try `:%s/x/中/<CR>` then `gg/中`), the
      cursor still lands on the first column of the wide glyph

---

## 3. Cancellation tokens
*(commits `57ff0b7`, `a5dca1c`, `262c249`)*

This is mostly infrastructure — there's no Esc-to-cancel UX yet
because the v1 input loop is single-threaded. Verification is via
tests, but two regression checks are worth running:

- [ ] `cargo test -p lattice-protocol` shows 35 passing (5 cancel
      tests added)
- [ ] `cargo test -p lattice-grammar` shows 183+ passing (cancel +
      dispatcher tests)
- [ ] `/(\w+) \w+ \1<CR>` (a backref pattern that needs the
      fancy-regex NFA) still finds the duplicate-word match without
      hanging the editor

---

## 4. §5.10 event bus
*(commit `a637f9c`)*

Observation-only baseline — nothing publishes yet, so the bus is
not user-visible. Verification:

- [ ] `cargo test -p lattice-runtime` shows 30 passing (8 event-bus
      tests included)

---

## 5. Block-visual I / A / > / < + single-undo
*(commits `c8636e9`, `b50fa61`, `fa9267f`, `db29123`, `75013f5`)*

Open a small buffer with at least 3 short lines:

```sh
printf 'abcd\n1234\nWXYZ\n' > /tmp/box.txt
cargo run --release -- /tmp/box.txt
```

### `>` / `<` in any visual mode

- [ ] `Vjj>`  (linewise visual + indent right) — indents 3 lines
- [ ] `u` once restores the original (single undo)
- [ ] `Vjj<` (after re-indenting) — dedents 3 lines
- [ ] `2>>` indents the cursor's line + the line below; `u` once reverts
- [ ] `3>>` indents 3 lines; `u` once reverts

### Block-visual rectangle ops

- [ ] `Ctrl-V` (or `Ctrl-Q`) enters Blockwise visual; cursor block changes
- [ ] `lj` extends the block 1 col right + 1 row down — rectangle highlights
- [ ] `x` deletes the rectangle; cursor lands on the block's top-left col
      (NOT on column 0)
- [ ] `u` once fully reverts the rectangle delete
- [ ] `Ctrl-V` then `ll` then `y` — rectangle yanked; `p` pastes it as a
      rectangle on consecutive lines

### Block-visual `I` (insert at block's left column)

- [ ] `Ctrl-V` with cursor at column 1, `jj` to extend down — block selects
      column 1 across 3 rows
- [ ] `I` enters Insert with cursor at the block's top-left
- [ ] Type `XYZ` — `XYZ` appears live on the top row only
- [ ] `<Esc>` — `XYZ` is replicated on the other 2 rows at the same column
- [ ] `u` once fully reverts (whole I session is one undo)
- [ ] Re-do the I session but type `<Backspace>` mid-way — backspace
      reflects on the top row; on `<Esc>` the (corrected) prefix replicates

### Block-visual `A` (append at block's right column)

- [ ] `Ctrl-V` with cursor at column 1, extend down 2 rows + right 1 col
- [ ] `A` enters Insert one byte past the block's right column on the top row
- [ ] Type `@` then `<Esc>` — `@` lands on every row at the right edge
- [ ] `u` once fully reverts

### Edge case: short rows

- [ ] On a buffer like `abcd\n12\nWXYZ`, block at columns 3..3, `I` and type
      `Q<Esc>` — top and bottom row get `Q`; the short middle row is
      skipped (vim's behavior)

---

## 6. Counts on linewise operators
*(commit `b50fa61`, `fa9267f`)*

Open a buffer with several lines:

- [ ] `2dd` deletes 2 lines; `u` once restores both
- [ ] `3yy` yanks 3 lines; `p` pastes all 3 below
- [ ] `2>>` indents 2 lines; `u` once dedents both
- [ ] `2<<` (after a 2>>) dedents 2 lines; `u` once restores
- [ ] `2gUU` uppercases 2 lines; `u` once restores
- [ ] `2cc` clears 2 lines and enters Insert
- [ ] Visual selection `Vjj>` indents the 3 lines; `u` once dedents

---

## 7. `:g` body parsed at submit time
*(commit `81cc40b`)*

- [ ] `:g/foo/this-is-not-a-command<CR>` reports the unknown-command
      error IMMEDIATELY, before any matching line is processed
- [ ] `:g/foo/<CR>` (empty body) errors at submit with "empty body"
- [ ] `:g/^/d<CR>` deletes every line (body is `:d`, parsed once)

---

## 8. Substitute live preview
*(commit `4fec95d`)*

Open README.md (has many `Lattice` occurrences):

- [ ] `:s/Lattice` (no second `/`) — first match on the cursor's line
      lights up in magenta with strike-through
- [ ] `:s/Lattice/X` — same first match still highlighted; the typed
      replacement does not modify the buffer (preview only)
- [ ] `:s/Lattice/X/g` — every match on the cursor's line lights up
- [ ] `:%s/Lattice/X/g` — every match across the whole buffer lights up
- [ ] `<Esc>` cancels the cmdline; preview clears
- [ ] `<CR>` (with `:s/Lattice/X/g`) actually performs the substitute;
      the preview clears as the cmdline closes
- [ ] After backspacing past `s/`, the preview disappears

---

## 9. Generalized interactive arg-prompts
*(commit `4fa9731`)*

- [ ] `:describe-command<CR>` (no arg) does NOT error — instead the
      cmdline prefills `describe-command ` and the echo line shows
      `command:` (the schema's prompt). Cursor stays in Command mode
      so you can type the arg and submit.
- [ ] `:apropos<CR>` (no arg) prefills `apropos ` with a `pattern:` prompt
- [ ] `:describe-key<CR>` (no arg) prefills `describe-key ` and the
      next chord auto-submits the lookup (chord-kind arg behavior)
- [ ] `:write<CR>` (no arg, optional path) saves the current buffer
      normally — no prompt arms (path is Optional, not Required)
- [ ] `:e!<CR>` (no arg, alias + bang) reloads the current file —
      no prompt (path is Optional)
- [ ] After an arg-prompt is armed, `<Esc>` cancels back to Normal
      and clears the arming

---

## 10. CI / tooling
*(commit `231964b`)*

Inspect the workflow:

- [ ] `.github/workflows/ci.yml` shows a matrix with `ubuntu-latest`,
      `macos-latest`, `windows-latest` for both `test` and `bench-compile`
- [ ] A `fmt` job runs `cargo fmt --all -- --check`
- [ ] A `doc` job runs `cargo doc --no-deps --workspace` with
      `RUSTDOCFLAGS=-D warnings`
- [ ] On a fresh PR, the workflow should run all 5 jobs (test ×3,
      fmt, doc, bench-compile ×3); push to main additionally runs
      bench-baseline

---

## 11. Documentation
*(commits `58a17ad`, `5e4086c`, `91a6d8e`)*

- [ ] `README.md` exists and is non-empty
- [ ] The architecture diagram renders as a mermaid flowchart on GitHub
      (visit the repo home page; the diagram should show three layers
      in colored boxes)
- [ ] `LICENSE` (MIT) is at the repo root
- [ ] `docs/implementation.md` "Up next" list starts with Phase 4 (LSP)
      and no longer mentions the per-search timeout (shipped)
- [ ] `docs/verify.md` (this file) exists

---

## What to do if a check fails

1. Confirm you're running the latest `main` branch:
   `git log --oneline -1` should show a commit message that matches
   what you're testing.
2. Rebuild from a clean state: `cargo clean && cargo build --release`.
   Sometimes a stale `target/` shadows recent changes.
3. Capture the exact keystrokes + the visual output. Include the
   commit SHA and any error message printed in the echo area.
4. Open a GitHub issue with the above plus the output of
   `cargo --version` and the OS / terminal.
