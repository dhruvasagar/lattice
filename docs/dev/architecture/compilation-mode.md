# Compilation Mode

Authoritative design for Lattice's native **compilation mode**: run
a build/test/lint command, stream its output into a live buffer, parse
error/warning locations into a walkable **error list**, and navigate
from the log — or from a grouped multibuffer "problems" view — to the
offending source. The emacs `compilation-mode` / `M-x recompile` and
vim `:make` + quickfix workflows, unified on Lattice's substrate.

This is a **native built-in** (a Rust crate wired through the
`SubsystemBoot` install seam), not a plugin. Error-format *parsers*
are the extensibility seam (built-in cargo/rustc + gnu-style today;
WASM-contributable in Phase 7).

Companion to `design.md` (§5.1 buffer model, §5.1.1 position history,
§5.9 everything-is-a-buffer, §5.10 events) and to `terminal-as-document.md`
(the sibling "run a process into a buffer" surface) and
`multibuffer-views.md` (the grouped problems view). Sequencing lives in
`../operations/slice-plans/compilation-mode.md`.

## 1. The three separable concerns

Compilation output is not one artefact. It is three, and the design
error to avoid is forcing one substrate to serve all three:

1. **The stream** — the raw build log scrolling in live: `Compiling
   foo v0.1.0`, progress, the multi-line `error[E0308]` snippet cargo
   prints, panics/backtraces, the final summary. The developer *watches
   this*. It is inherently a **stream of text**, and most of it is not
   an error location.
2. **The navigation** — parsed error/warning locations, walked with
   `:next-error`/`:previous-error`, jumped to source, decorated in the log.
3. **The grouped view** — optionally, the same locations as source
   excerpts grouped by file, editable in place ("fix all the sites").

The substrate assignment:

| Concern      | Substrate                                   | Why                                                                 |
|--------------|---------------------------------------------|---------------------------------------------------------------------|
| Stream       | streaming synthetic `Document` (`*compilation*`) | a multibuffer cannot stream; the log is text, watched live          |
| Navigation   | **error list** (new core substrate)         | a persistent list + index walked by generic `:next-error`; net-new  |
| Grouped view | multibuffer provider (`lattice-multibuffer`)| exactly what excerpt-composition is for; editable-in-place          |

A multibuffer serves concern #3 well and concern #1 *not at all* — it
is a snapshot composition, not a stream, and reducing the build to
source excerpts discards the log the developer came to see. So the
**primary artefact is the streaming buffer**; the multibuffer is a
*secondary* view fed by the same parser. This matches the universal
cross-editor convention (vim `:make`+quickfix, emacs compilation
buffer, VSCode output panel, Zed terminal): the raw log leads,
navigation overlays, the structured list is auxiliary.

## 2. The `*compilation*` buffer (concern #1)

`*compilation*` is a read-only synthetic `Document` with a synthetic
name — the codified `*messages*` / ai-log pattern, not a new
`BufferKind`. It carries `ReadOnly = true, NoFile = true`. `:ls` and
`:b *compilation*` reach it; `:bn`/`:bp` skip it (`listed = false`).

`compilation-mode` is a **major** mode (content-type identity, like
`MessagesMode` / `TerminalMode`), activated on the buffer *by id*
(`activate_major_by_id`) — no on-disk language-detection path. It owns:
the keymap, the streaming drain producer, the parser wiring, and the
action-handler bodies. There is no `Editor::do_compile_*` on the host.

**The mode provisions its own buffer.** `start_compilation` (called by
the `AppEffect::CompileRun` arm, which passes the `Editor` as
`&mut dyn ModeActivator`) calls `ModeActivator::ensure_named_document(
COMPILATION_BUFFER_NAME, compilation-mode, …)` — the `&mut`-backed
creation seam whose `Editor` impl mints the buffer *and* runs
`on_activate` (establishing the streaming drain), then runs the service.
Creation lives on `ModeActivator` (not the `&self` `BufferStore` handle,
which is find-only) because activating a mode needs `&mut Editor`. The
host's only role is generic: activate the returned buffer + repaint.
Idempotent on `:recompile` (reuses the buffer; the first drain stays
live). See `error-list.md` and the `ModeActivator` trait for the seam.

### Process execution — off the UI/actor thread (paramount #1)

The compiler runs **pipe-captured** (not PTY): stdout+stderr captured
as text, so the output is rope-backed, parseable, and navigable. The
child process and its output reader run on `spawn_blocking` (a blocking
read loop), **never** `tokio::spawn` — the editor actor runs a
`current_thread` runtime, so a naive spawn would land the read loop on
the actor thread and violate "no UI-thread work". This mirrors the
terminal reader task.

The reader emits captured lines to the tick drain via a typed event
(`CompilationOutput`), boot subscribes an mpsc, and `run_tick_pending`
coalesces all lines available that tick into **one** `apply_edit_batch`
via `append_to_owned_buffer` — one actor round-trip, one undo unit,
one repaint. Lines are published individually (one line per event) so
output appears in real time; the per-tick cadence is the natural
debounce window.

Lifecycle: `:recompile` kills the prior child before relaunching; the
buffer is cleared and re-streamed. **`<C-c>` in `*compilation*`** kills
a still-running build on demand (`:compilation-kill` →
`CompilationService::kill()`). On Unix the child is launched in its own
process group (`pre_exec` + `setpgid(0,0)`) so `killpg(-pgid, SIGKILL)`
terminates the shell AND every pipeline grandchild (`seq | while …`
doesn't leave orphans keeping pipes open). On Windows `TerminateProcess`
handles the single child. The pipe readers EOF on the closed pipes; the
coordinator publishes a `Finished` summary with `"\nCompilation
terminated\n"`. On normal exit a summary line is appended
("Compilation finished" / "exited abnormally with code N"). Exit is an
`info!` one-shot (user-actionable); per-line streaming is never logged.

### The compilation headerline (sticky status bar)

`compilation-mode` renders a **view-header virtual row** above
`*compilation*` — a mode-owned sticky status bar, the direct twin of
the project-search headerline (§`multibuffer-views.md` status surface).
It shows, from a `CompilationHeadlineState { command, last_counts:
Option<(errors, warnings)>, running, killed }`:

- a **state icon** leading: `⟳` (grey) running, `✔` (green) success,
  `✗` (red) failure, `■` (red) killed;
- the **quoted command** (`"cargo build --release"`),
  emphasis-highlighted in warm yellow;
- a **status badge** trailing: `…` while running, `ok` on success,
  `3e 2w` error/warning counts on failure, `killed` on cancellation.

Rendered forms:

```
  ⟳ "cargo build --release" …            (running)
  ✔ "cargo build --release" ok           (finished clean)
  ✗ "cargo build --release" 3e 2w        (finished with errors)
  ■ "cargo build --release" killed       (explicitly killed / <C-c>)
```

The drain owns the state: a `Reset` chunk sets `command` + `running =
true` + clears `killed`; a `Finished` chunk clears `running`, writes
the error/warning counts, and detects `"Compilation terminated"` in
the summary to set `killed = true`. Production is entirely off the UI
thread; the renderer only reads the published row. Colours resolve
from the theme (`compilation.headerline.command` + sibling
`in_progress` / `success` / `failure` / `dim` elements), never
hardcoded RGB.

## 3. The error list (concern #2)

Compilation navigation needs a **persistent, cross-file list with an
index** — vim's quickfix list, surfaced as the "error list." This is a
**core substrate**, not compilation-specific, so its design lives in its
own fragment: **[`error-list.md`](error-list.md)**. Compilation is its
first *producer*.

The essentials as they bear on compilation: the list is core `Editor`
state (not mode-owned), navigated by generic `:next-error`/
`:previous-error`/`:error`/`:first-error`/`:last-error`/
`:next-error-file`/`:previous-error-file` (vim `:c*` aliases) + `]qq`/
`[qq`/`]qf`/`[qf`/`]Q`/`[Q` from *any* buffer (it survives closing
`*compilation*`), and compilation feeds it the tool-agnostic way (§5):
the parser's `Vec<ErrorEntry>` reaches `Editor::set_error_list` over the
native `InboundBus → AppEffect::SetErrorList` seam. See `error-list.md`
for the data model, the buffer-independence rationale (vim vs emacs),
the no-diagnostic-fallback rule, and the producer contract.

## 4. The problems view (concern #3)

`:problems` opens `*problems*`, a multibuffer provider that groups the
error-list entries as anchored source excerpts by file, each headed by
its message + severity, **editable in place** (edits propagate to the
source via the standard multibuffer pipeline). It is fed from the
error list, reusing `lattice-multibuffer` machinery — the search
provider is the template. This is the developer's original "multibuffer"
instinct, correctly placed as the *secondary* surface. Async progress
(scan/compose) surfaces in the view's headerline per the async-buffer
rule.

## 5. Parser registry (concern #2 producer, extensibility seam)

Compiler error formats differ per tool. A `CompilationParser` — a named
matcher over streamed lines producing `ErrorEntry`s — is the
extensibility seam, mirroring emacs `compilation-error-regexp-alist`.
The built-in set is **four** parsers, fed each line in registration
order (`ParserRegistry::with_builtins`), the results de-duplicated by
`(path, line, col)` so the first (richest) match for a location wins:

1. **`CargoRustcParser`** — multi-line rustc/cargo diagnostics: an
   `error[E0308]: …` / `warning: …` header primes severity + message,
   emitted when the following `--> path:line:col` location line
   arrives.
2. **`GnuStyleParser`** — single-line gcc/clang/eslint form
   (`path:line:col: severity: message`, and the short `path:line:
   message`).
3. **`TestPanicParser`** — Rust test / `panic!` output: `thread
   '<name>' panicked at path:line:col[: message]`, emitted as an
   `Error`.
4. **`GeneralParser`** — a **catch-all** that finds `file:line:col`
   (or `file:line`) **anywhere** in a line (unanchored,
   `Regex::find_iter`), gated on an `is_file_like` check (the path must
   contain `/` or `.`) so timestamps, version strings, and other
   `word:digits` noise are rejected. This is what makes compilation
   tool-agnostic: any log line, script output, printf-debug print, or
   bespoke linter that embeds a location becomes navigable, with no
   per-tool parser. Registered **last** and emits `Info` severity +
   empty message, so the format-specific parsers above win the
   `(path, line, col)` de-dup and supply richer severity/message when
   they match.

**Both stdout and stderr are parsed.** Compile diagnostics
(rustc/cargo) print on stderr, but **test-failure output / thread
panics print on stdout** — so parsing only one stream would miss test
failures. Two dedicated reader threads (one per pipe, so a large pipe
can't deadlock the other) each run their own `ParserRegistry` and merge
into a **shared `Arc<Mutex<Vec<ErrorEntry>>>` accumulator**; each reader
sends the full accumulated list through the `InboundBus` seam, so
whichever pipe delivers an entry, the visible list grows without one
stream clobbering the other. Parsing runs **in the readers'
`spawn_blocking`-spawned OS threads**, off the UI thread. A parser that
fails to match a line simply skips it (log at `debug!` on a
malformed-but-claimed match, never panic, never swallow silently).

### Plugin-contributed parsers register a *factory*, not a parser

A WASM plugin declaring the `error-parser` seam joins that same
`Vec<Box<dyn CompilationParser>>` and is indistinguishable from a native
parser downstream — the WIT world mirrors the native one-method trait
rather than inventing a second shape for the same job.

What the plugin registers, though, is a **`CompilationParserFactory`**:
something that mints a parser, not a parser itself. The reason is the
two readers above. A `CompilationParser` carries pending multi-line
state behind `&mut self`, and the two streams are independent — a header
line on stderr must not prime a diagnostic that a stdout line then
completes. A shared instance would fuse them. For a WASM-backed parser
it could not be shared regardless, since each owns a `wasmtime::Store`.
So each reader calls `create_all()` once at the top of its loop and owns
what it gets back for the run, exactly as it already owns its own
`ParserRegistry::with_builtins()`.

The contract, in full:

- **Registration is RCU** behind `Arc<ArcSwap<CompilationParserFactories>>`
  — the picker registry's wait-free-read / rare-write idiom. Reads happen
  once per **run**, not per line, so a plugin loaded mid-build joins the
  *next* build. That is the honest behaviour: a parser starting halfway
  through a stream has no pending state for the lines it missed.
- **Placement is before the catch-all, after the format-specific
  natives** (`register_before_catch_all`). After `GeneralParser`, its thin
  salvaged `Info` entry would win the first-entry-wins de-dup for every
  location the plugin also matched, silently discarding the plugin's
  severity and message — the plugin would look inert. Before the natives,
  it would displace a parser that understands the format better.
- **Instantiation is verified at load.** `PluginHost::error_parser_factory`
  spawns one instance and throws it away, so a component that cannot start
  fails the *load* rather than reporting success and then contributing
  nothing to every build forever.
- **A later failure costs only the plugin.** A factory that cannot mint an
  instance is logged and skipped; a guest that traps is poisoned for the
  session and yields nothing further. The build keeps streaming and the
  other parsers, native and plugin, carry on.
- **Teardown is by provenance, not by token.** The registry keys entries by
  the host-issued plugin id, so `PluginTeardown::unload` removes them the
  way it removes commands — there is no per-contribution `Vec` to record,
  and therefore none to forget.

Zero registered factories is the overwhelmingly common case and costs a
single `is_empty()` check per run; the per-line path is byte-identical to
the no-plugin one, which is why this carries no bench of its own — the
existing `compilation_parse` bench already measures the shape that runs.

Matched lines in `*compilation*` gain a severity gutter decoration and
a **location-line background tint** (theme element
`compilation.location`) so navigable `file:line:col` lines stand out
from surrounding prose; the tint colour resolves from the theme (no
hardcoded RGB) and is produced off-thread in the drain
(`scan_location_lines`), shipped to the renderer over a native
`InboundBus` twin of the severity-gutter seam. `<CR>` on a matched line
jumps to that source location (via `jump_to_file_line_col`) and syncs
the error-list index. `<CR>` reads the cursor line and parses a location
out of it directly (no precomputed buffer-line→entry map — stdout/stderr
interleave in the log, so a positional map is unreliable; per-line parse
is interleaving-proof, and reuses the same four location patterns via
`match_location_line`).

### Gutter decoration is delivered the *native* way (not the plugin seam)

Compilation is a **native built-in** mode, so its severity gutter marks
flow through the same path LSP and diff use — **not** the WASM plugin
gutter cache (`wasm_gutter_decorations` /
`GutterDecorationSourceRegistry` / `AsyncGutterDecorationSource`). The
severity index is built **off the UI thread in the drain**
(`scan_severities` matches an error/warning keyword per appended line,
tracking the running buffer line number), shipped to host `render_state`
over the native `InboundBus → AppEffect::CompilationGutterSet` seam (the
same off-thread→host-state transport as `SetErrorList`), and each renderer
injects a generic `lattice_mode::CompilationSeverityData` carrier into
`DecorationCtx`. `CompilationMode::gutter_decorations` reads that carrier
and returns `GutterDecoration::Severity` — the existing gutter column
shared with LSP diagnostics, so no new renderer glyph work. The carrier
type lives in `lattice-mode` (below both peers) so neither renderer takes
a `lattice-compilation` dependency. Read at paint is O(entries),
wait-free; production is entirely off-thread — paramount #1 intact.

## 6. Keymap surface (`compilation-mode`)

| Chord      | Action                                              |
|------------|-----------------------------------------------------|
| `gr`       | recompile (mirrors project-search's `gr` refresh)   |
| `<CR>`     | jump to location on the cursor line (syncs error-list index) |
| `<C-c>`    | kill the running compilation (→ `:compilation-kill`) |
| `]qq`/`[qq`| next/prev error entry (Builtin; global, any buffer) |

`<C-c>` is a **mode-dispatchable** chord: it is no longer a universal
Quit hatch, so `compilation-mode` claims it (the binding + the handler
both live with the mode, per `feedback_mode_owns_its_surface`).

Read-only, `NoFile`; `:q` never warns unsaved, `:w` is a no-op (mode
`options()`), consistent with `*messages*`/terminal.

## 7. Ex-command naming

- `:compile <cmd>` / `:recompile` — the primary (emacs-canonical for
  this exact feature; not an LSP-coupled subsystem name, so the
  dashed-namespaced rule does not apply — these *are* the domain-
  canonical names).
- `:make` — vim-canonical alias (runs the configured build command,
  populates the error list).
- `:compilation-kill` — kill the running compilation child
  (`CompilationService::kill()`), bound to `<C-c>` in `compilation-mode`
  (§6). Subsystem-coupled but not LSP; the dashed name is the single
  alias (no collapsed / generic-name forms).
- The **error-list** navigation commands (`:next-error` family +
  `:error-list` / `:problems`) and their vim `:c*` aliases live with the
  error-list substrate — see [`error-list.md`](error-list.md) §2.

## 8. Paramount-goal alignment

- **#1 Performance.** Process + capture + parse all on `spawn_blocking`,
  off the actor thread; the drain is O(lines-this-tick) coalesced into
  one batch; element fan-out stays O(viewport). No UI-thread I/O, parse,
  or shaping. Bench: append throughput + actor-latency-during-noisy-build.
- **#2 Extensibility.** Parsers are a registry, WASM-contributable in
  Phase 7. WIT surface deferred with the plugin host.
- **#3 Everything-is-a-buffer.** `*compilation*` and `*problems*` are
  Documents; zero `BufferKind` branch; the error list is generic core
  state like the jump ring.
- **#4 Asynchronicity.** Nothing blocks the UI; streaming is
  event-bus → tick-drain; the build runs as an isolated task with
  kill-on-recompile lifecycle.

## 8b. ANSI escapes in captured output (CM.5)

Captured stdout/stderr is a pipe, and a pipe is not a tty, so cargo,
rustc and most well-behaved tools turn colour off by themselves. That
makes escape handling look optional. It is not — it is a **correctness**
requirement, and the display half is the smaller half.

The moment anything forces colour on — `cargo build --color=always`,
`CLICOLOR_FORCE=1`, `ls --color=always`, or any tool that probes `TERM`
instead of isatty — raw `ESC[…m` bytes arrive in the stream. They land
in two places, and the second is the damaging one:

1. In the buffer text, where they render as garbage.
2. **In front of the parser regexes.** A colourised rustc line is
   `ESC[1mESC[31merror[E0308]ESC[0m: …`, which the `error` pattern does
   not match. The diagnostic silently never reaches the error list, and
   `:cnext` skips a real error with no indication anything went wrong.

So **stripping is unconditional and happens first**, in the pipe reader,
before either the parsers or the buffer see a line. Everything
downstream — `ParserRegistry::feed`, `scan_severities`,
`scan_location_lines`, the `<CR>` jump — operates on text that has never
contained an escape sequence, and needs no knowledge that escapes exist.

Having parsed the SGR parameters in order to remove them, turning them
into spans is nearly free, so the reader also emits per-line
`StyledSpan`s. Those travel **with** the text on `OutputChunk::Append`
rather than on a parallel channel, for the reason the highlight drain
gives about diff signs: one thing being spliced cannot desynchronise
from itself.

**What is modelled, and what is not.** Foreground colour from the
16-colour palette, plus bold. Not background (`StyledSpan` carries a
foreground `Style`; backgrounds are the separate `RefineSpan` axis with
different precedence), not 256-colour beyond the first 16 slots, not
truecolor, and not italic/underline/reverse — `Style` is one value per
span, not a set, and the colour is the information-bearing half. Every
unmodelled parameter is still *parsed*, so it cannot desynchronise the
parameter walk and silently change how a later parameter is read. This
is not a narrow subset in practice: anstyle, which cargo is built on,
emits nothing outside it.

**Bold is bright.** `SGR 1` with a normal colour resolves to that
colour's bright slot, which is how terminals have rendered
bold-plus-colour since the hardware did. It is also the only way to keep
both attributes of cargo's `bold red` `error:` prefix, given one `Style`
per span.

**Colours are theme elements, not literals.** The reader emits
`Style::Element(id)` over 17 registered elements (`compilation.ansi.red`
… `compilation.ansi.bright-white`, `compilation.ansi.bold`), so a theme
retunes captured colour by name like any other element — which matters
most on light themes, where a terminal's default red can be unreadable.
Defaults are `ColorRef::Literal(Color::Named(..))` rather than palette
references: the `ansi.*` palette family already exists, but all 21
builtin palettes define its entries as the same pass-through
`Color::Named`, so per-theme keys would be 21 identical copies of one
value. Promoting these to a core `ansi.*` **element** family is the
upgrade path if a second consumer (terminal, agent output) appears.

**`\r` restarts the line.** A bare carriage return mid-line discards
what preceded it, as a terminal does. Without this a cargo build streams
every intermediate `Building [===>   ] 41/1000` state concatenated into
one unreadable row.

**Span/text alignment is the subtle invariant.** The published span list
must stay exactly as long as the buffer's text, or a later coloured line
splices over the wrong row — a failure that is silent at the moment it
is introduced and only shows up further down the log. Two mechanisms
maintain it: a flush pads its spans to the line count it actually
appends (batches mix reader output with editor-generated summaries), and
a wholly uncoloured flush publishes nothing at all but banks its line
count as *debt*, paid as leading empty rows by the next flush that has
colour to show. The debt is what makes the common case — no colour
anywhere — cost zero publishes and zero renderer wakes without
sacrificing alignment.

**Where it runs.** In the pipe reader's own thread, off the UI/actor
thread, ahead of the parse it protects. Benched in three shapes
(`compilation_ansi`): uncoloured ~600 MiB/s, which is the path nearly
every real build takes.

## 9. Rejected alternatives

- **Multibuffer as the primary artefact.** Rejected: a multibuffer
  cannot stream a live process and discards the non-location log
  (progress, summary, backtraces) the developer came to watch. It is the
  right substrate for the *grouped* view only (§4).
- **PTY-backed capture (reuse terminal).** Rejected for the primary:
  a PTY yields a cell grid, not a rope-backed text Document, so
  `:cnext` navigation and line parsing become far harder. Pipe-capture
  matches emacs and keeps the log parseable. (ANSI-SGR is handled over
  the captured text instead — §8b.)
- **Error list owned by `compilation-mode`.** Rejected: its consumer
  is *generic* host dispatch (`:next-error`), so by the substrate-vs-mode
  rule it is core state; a mode-private list would block project search /
  other tools from ever feeding the same navigation.
- **Gutter marks via the WASM plugin decoration cache.** Rejected: that
  cache (`wasm_gutter_decorations` + `GutterDecorationSourceRegistry`)
  is the *plugin* seam. Compilation is a native built-in and must not
  route its decorations through the plugin path — it uses the native
  `render_state` → `DecorationCtx` → `Mode::gutter_decorations` path LSP
  and diff use (§5). (A boot-registered service read inside
  `gutter_decorations` was also rejected — impossible: `DecorationCtx`
  carries a *fresh per-frame* registry of render-state snapshots, not
  the boot registry.)
