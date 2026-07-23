---
summary: ":compile runs any CLI tool, streams its output into *compilation*, and parses file:line:col references into the error list; gr recompiles, <CR> jumps, :problems groups the results."
related: [error-list, ex:compile, ex:recompile, ex:make, ex:compilation-kill]
---

# Compilation mode

`:compile <command>` runs an external command, streams its output live
into a read-only `*compilation*` buffer, and parses every
`file:line:col` reference in that output into the
[error list](error-list.md). It is Lattice's equivalent of emacs
`M-x compile` and vim `:make` — a **tool-agnostic** command runner:
point it at a compiler, a linter, a test runner, `grep`, or any CLI
tool that prints locations, and you can jump straight to each one.

> **Status:** `:compile` / `:recompile` / `:make`, live streaming, the
> status headerline, `gr` recompile, `<CR>` jump-to-location, `<C-c>`
> kill, severity gutter marks, location-line highlighting, and the
> `*problems*` multibuffer view are shipped. Custom per-tool parsers
> (beyond the four built-in matchers) are a plugin-era extension.

---

## Quick reference

| Command / key | Meaning |
|---------------|---------|
| `:compile <cmd>` | Run `<cmd>`, stream output into `*compilation*`, populate the error list |
| `:recompile` | Re-run the last command |
| `:make [args]` | Vim-style alias for `:compile` (runs the build command) |
| `gr` | (in `*compilation*`) Re-run the last command |
| `<CR>` | (in `*compilation*`) Jump to the location on the cursor line |
| `<C-c>` | (in `*compilation*`) Kill the running command |
| `:compilation-kill` | Kill the running command (what `<C-c>` runs) |
| `:error-list` / `:cl` | Open the parsed locations in a fuzzy **picker** |
| `:problems` / `:problems-close` | Open / close the [`*problems*`](#the-problems-view) grouped view |
| `:next-error` / `:previous-error` / … | Walk the parsed locations — see [error list](error-list.md) |

---

## Running a command

```
:compile cargo build
```

The command runs **off the UI thread** — the editor stays fully
responsive while output streams in. `*compilation*` shows the raw log
(the command line, progress, warnings, the final summary), exactly as
the tool printed it. A run **replaces** the previous one: the buffer is
cleared and the error list reset.

`:recompile` (or `gr` inside the buffer) re-runs the last command;
starting a new run kills the previous process first. To stop a runaway
build without starting a new one, press **`<C-c>`** in `*compilation*`
(or run `:compilation-kill`) — the process is killed, the pipes close,
and a "terminated" line is appended.

A **status headerline** sits at the top of `*compilation*` — a sticky
bar showing the command being run, a spinner while it's running, and
a success/failure icon with error and warning counts when it finishes
(e.g. `◆ cargo build ✗ 3e 2w`, or `◆ cargo build ✔ ok`).

### Any tool, not just cargo

Compilation mode is **not tied to any specific tool.** The output is
run through four built-in matchers, so a location is picked up whether
it appears in a diagnostic, a log line, or a test panic:

```
path/to/file.rs:12:9: error: mismatched types      (path:line:col: message)
path/to/file.js:40: 'x' is not defined              (path:line: message)
error[E0308]: … / --> path/to/file.rs:12:9          (cargo/rustc multi-line)
[warn] found it at src/foo.rs:42:8 today            (file:line:col anywhere in the line)
```

The last one is a **catch-all**: any line that contains a
`file:line:col` (or `file:line`) reference — a log message, a script's
`printf` output, a bespoke linter — becomes navigable, as long as the
path looks like a real path (has a `/` or a `.`, so timestamps and
version strings aren't mistaken for locations). So all of these just
work, filling the error list you navigate with `:next-error`:

```
:compile npx eslint .
:compile npx tsc --noEmit
:compile pytest -q
:compile grep -Hn TODO -r src
:compile golangci-lint run
```

**Test failures are captured too.** A tool prints its diagnostics on
stderr, but test frameworks print failures on stdout — Lattice parses
both. So `:compile cargo test` fills the error list with each
`thread '…' panicked at file:line:col` location, and `:next-error`
walks straight to the failing assertions.

Lines that don't contain a location (progress bars, summaries,
backtraces) stream into the buffer untouched — only recognised
locations become error-list entries.

---

## Jumping to errors

Four ways, all views of the same parsed locations:

1. **`:next-error` / `:previous-error` / `]qq` / `[qq` from anywhere** —
   you don't need the `*compilation*` buffer focused or open. See
   [the error list](error-list.md) for the full command + chord set.
2. **`<CR>` in `*compilation*`** — with the cursor on a line that
   contains a location, jumps to that source file, and syncs the
   error-list position so `:next-error` continues from there.
3. **`:error-list` / `:cl`** — a fuzzy picker over the flat list.
4. **`:problems`** — the grouped `*problems*` view (below).

Error and warning lines carry a **severity mark** in the gutter (the
same column LSP diagnostics use), and every line that holds a
navigable `file:line:col` gets a subtle **background tint**, so the
jump targets stand out from surrounding prose as you scroll the log.

---

## The `*problems*` view

`:problems` opens `*problems*`, a [multibuffer](multibuffer.md) that
groups the error-list entries as editable source excerpts under each
file's header — fix an error right there and it propagates to the
source. `:problems-close` closes it. This is the "see everything at
once" counterpart to stepping through with `:next-error`.

---

## See also

- [The error list](error-list.md) — the navigation substrate and its
  full command set (`:next-error`, `:next-error-file`, `:error`,
  `]qq`/`[qq`, …).
- [Project search](project-search.md) — `:search` / `g/`, the
  interactive grep-into-a-multibuffer (a separate feature from
  `:compile` + the error list).
