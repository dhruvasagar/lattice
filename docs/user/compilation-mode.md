---
summary: ":compile runs any CLI tool, streams its output into *compilation*, and parses file:line:col references into the error list; gr recompiles, <CR> jumps, :problems groups the results."
related: [error-list, ex:compile, ex:recompile, ex:make, ex:compilation-kill]
---

# compilation-mode

`:compile <command>` runs an external command, streams its output live
into a read-only `*compilation*` buffer, and parses every
`file:line:col` reference in that output into the
[error list](help:error-list). It is Lattice's equivalent of emacs
`M-x compile` and vim `:make` — a **tool-agnostic** command runner:
point it at a compiler, a linter, a test runner, `grep`, or any CLI
tool that prints locations, and you can jump straight to each one.

> **Status:** `:compile` / `:recompile` / `:make`, live line-by-line streaming,
> the status headerline, `gr` recompile, `<CR>` jump-to-location, `<C-c>`
> kill (process-group, kills entire pipeline), severity gutter marks,
> location-line highlighting, and the `*problems*` multibuffer view are
> shipped. Custom per-tool parsers (beyond the four built-in matchers)
> are a plugin-era extension.

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
| `:next-error` / `:previous-error` / … | Walk the parsed locations — see [error list](help:error-list) |

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
bar showing a state icon, the quoted command, and a status badge:

| State | Headerline |
|---|---|
| Running | `⟳ "cargo build --release" …` |
| Finished clean | `✔ "cargo build --release" ok` |
| Finished with errors | `✗ "cargo build --release" 3e 2w` |
| Killed (`<C-c>`) | `■ "cargo build --release" killed` |

The spinner turns grey, the checkmark green, and the cross/square red.
The command is double-quoted and highlighted in warm yellow. Error
counts show per-severity (errors first, then warnings).

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

### Coloured output

Most tools turn their colours off when their output is captured rather
than shown in a terminal, so a typical build arrives plain. When a tool
insists on colour anyway — `cargo build --color=always`, `CLICOLOR_FORCE=1`,
`ls --color=always` — Lattice reads it and paints the buffer to match,
rather than showing you the raw escape codes.

Progress lines that redraw themselves in place (`Building [===>  ] 41/1000`)
show their final state, not every frame concatenated together.

The colours are themeable like everything else: they resolve through
seventeen theme elements named `compilation.ansi.red`,
`compilation.ansi.bright-blue`, `compilation.ansi.bold` and so on. If a
tool's idea of "red" is hard to read against your background, retune
that element rather than turning colour off:

```
:set ui.compilation.ansi.red=#d20f39
```

Bold text that also carries a colour is shown in that colour's bright
variant, which is what terminals do — it's why `cargo`'s bold-red
`error:` reads as bright red here.

---

## Jumping to errors

Four ways, all views of the same parsed locations:

1. **`:next-error` / `:previous-error` / `]qq` / `[qq` from anywhere** —
   you don't need the `*compilation*` buffer focused or open. See
   [the error list](help:error-list) for the full command + chord set.
2. **`<CR>` in `*compilation*`** — with the cursor on a line that
   contains a location, jumps to that source file, and syncs the
   error-list position so `:next-error` continues from there.
3. **`:error-list` / `:cl`** — a fuzzy picker over the flat list.
4. **`:problems`** — the grouped `*problems*` view (below).

Error and warning lines carry a **severity mark** in the gutter (the
same column LSP diagnostics use), and every line that holds a
navigable `file:line:col` gets a subtle **background tint**, so the
jump targets stand out from surrounding prose as you scroll the log.

### Teaching Lattice a new diagnostic format

Out of the box, four built-in parsers read the output: rustc/cargo's
multi-line diagnostics, the gcc/clang/eslint `path:line:col: severity:
message` form, Rust test panics, and a catch-all that finds a
`file:line:col` anywhere in a line. That last one is why most tools
work with no configuration at all — if your linter prints a location,
it is already navigable.

For a tool whose format the catch-all cannot read — or one whose
severity and message you want carried through properly — a **plugin can
contribute its own parser**. A plugin declaring `error-parser` in its
`provides` is fed every captured line and returns the diagnostics it
recognised; its entries land in the error list beside the built-in ones
and behave identically for `:next-error`, `<CR>`, the gutter marks, and
`:problems`.

Plugin parsers are consulted *after* the format-specific built-ins and
*before* the catch-all, so they never displace a parser that understands
the format better, and they are never drowned out by the catch-all's
thinner guess. If such a plugin misbehaves, it costs only itself: a
parser that fails to start, or that crashes mid-build, is dropped with a
warning and your build keeps streaming.

---

## The `*problems*` view

`:problems` opens `*problems*`, a [multibuffer](help:multibuffer-mode) that
groups the error-list entries as editable source excerpts under each
file's header — fix an error right there and it propagates to the
source. `:problems-close` closes it. This is the "see everything at
once" counterpart to stepping through with `:next-error`.

---

## See also

- [The error list](help:error-list) — the navigation substrate and its
  full command set (`:next-error`, `:next-error-file`, `:error`,
  `]qq`/`[qq`, …).
- [Project search](help:project-search-mode) — `:search` / `g/`, the
  interactive grep-into-a-multibuffer (a separate feature from
  `:compile` + the error list).
