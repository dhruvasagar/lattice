# Org export — HTML, Markdown and LaTeX/PDF from an org file

> **Where the code is.** The export *feature* lives in
> [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), a **separate repository**,
> as one mode (`org-export-mode`). What lives in *this* tree is the one generic
> host change it forces (§6): a synchronous, grant-checked **`write-file`**
> host service. Viewing the PDF it produces inside the editor is a separate
> feature with its own fragment, [`pdf-mode.md`](pdf-mode.md); export uses it
> but does not need it.

Sequencing and status: [`slice-plans/org-export.md`](../operations/slice-plans/org-export.md).
User documentation: [`org.md`](../../user/org.md) and the plugin's own
`doc/export.md` (`:help org.export`).

This reverses a cut. [`org-mode.md`](org-mode.md) §10 lists "export backends"
as *out*, and the plugin's `:help org` says so. Babel's reversal
([`org-babel.md`](org-babel.md)) needed a host primitive; export needs almost
none. It was cut for scope rather than feasibility, and §6 is the whole of
what the host must add.

## 0. Ownership — one mode owns export

**Everything that is export lives in `lattice-org-plugin`, inside one minor
mode, `org-export-mode`.** The host's share is `write-file` (§6), which must be
justifiable without the word "org": a formatter that writes a sibling file, or
a code generator whose output a spawned compiler then reads, needs exactly the
same call.

What the mode owns, all declared by the plugin:

- **Its keymap layer.** `<C-c><C-e>` at `KeymapLayer::MinorMode(org-export-mode)`,
  never at `Builtin` and never in `org-mode`'s major layer. The prefix is
  export's alone and never declines.
- **Its activation.** An `ActivationPolicy` naming org's major, an entry in
  `default_modes`, and a `mode_gates` switch, `org.export.enabled` (default on),
  the same shape `org-babel-mode` uses.
- **Every action handler body and ex-command** (§2), the dispatcher's
  transient source, every option under `org.export.*`, its events-seam
  handlers, its store keys, its `*org-export*` log buffer and its
  `:help org.export` page.
- **Its code**, in `src/export/…`, not grown into `lib.rs`.

**The acid test**, as for babel: export lands with **zero `Editor::`
methods, zero host `Action` variants, no `BufferKind`, no `Lang` arm and no
export-specific code in lattice.**

## 1. The files have another reader, and so does the output

As with babel, the source is the user's org file and emacs opens it too. Export
reads the same keywords emacs's `ox` reads (`#+TITLE`, `#+OPTIONS`, `#+SELECT_TAGS`,
`:noexport:`, `EXPORT_FILE_NAME`, `#+BEGIN_EXPORT html`, `#+ATTR_HTML`, …) and
gives them the same meaning. A file set up for emacs export exports here
without edits; a keyword lattice does not implement is **named in the echo
area once per export**, never silently ignored.

The *output* is held to a weaker standard, on purpose: **structurally
compatible, not byte-identical.**

- **HTML** uses `ox-html`'s document shape and class names (`#content`,
  `.title`, `#table-of-contents`, `.outline-2`, `.outline-text-2`, `.src
  .src-python`, `.org-ul`, `#footnotes`), so a stylesheet written for emacs
  output styles ours. It ships **its own default stylesheet**. Class names are
  an interface; `org-html-style-default` is GPL'd code, and copying it would
  put GPL'd text in every exported file and in the plugin.
- **Markdown** is **GitHub-flavoured**. `ox-md` falls back to raw HTML for
  tables and footnotes; a GFM file reads better on the forges where Markdown
  is actually read, and GFM has both. This is the one deliberate divergence,
  and the guide says so.
- **LaTeX** uses `ox-latex`'s preamble shape (`\documentclass`, the default
  package list, `\title`/`\author`/`\date`, `\maketitle`, `\tableofcontents`,
  `\section`…), so a `#+LATEX_HEADER` written for emacs compiles here. It
  carries the four knobs emacs users actually tune (Dhruva, 2026-11-25):
  **user-defined classes** with their sectioning (`org.export.latex-classes`,
  emacs's `org-latex-classes`, beside the built-in `article` / `report` /
  `book`), `#+LATEX_HEADER_EXTRA`, **`#+LATEX_COMPILER`** (`pdflatex`,
  `xelatex`, `lualatex`: it picks the compiler-specific default packages,
  `inputenc`/`fontenc` or `fontspec`, and is `%latex` in the PDF process),
  and the **source-block backend** (`org.export.latex-src-block-backend`:
  `verbatim`, `listings` or `minted`). Two deliberate divergences, both so a
  first export compiles: the backend's package is added to the preamble
  (emacs leaves it to `org-latex-packages-alist`), and listings takes its
  options per listing rather than by `\lstset`, which carries one block's
  caption into the next, with no `language=` for a language listings does
  not know. Without a compiler choice, Unicode
  text and system fonts cannot build; without minted, source is plain; without
  classes, a `memoir` or house class cannot be used.

## 2. What the user does

`C-c C-e` opens the **dispatcher**, emacs's `org-export-dispatch`. It is a
host transient (`open-transient`, magit's mechanism), not a bespoke buffer:

| key | does | ex-command |
|---|---|---|
| `h h` | HTML to `foo.html` | `:org-export-html` |
| `h H` | HTML into a buffer | `:org-export-html-buffer` |
| `h o` | HTML to file, then open it | `:org-export-html-open` |
| `m m` / `m M` / `m o` | the same for Markdown, `foo.md` | `:org-export-markdown`… |
| `l l` / `l L` | LaTeX to `foo.tex` / into a buffer | `:org-export-latex`… |
| `l p` | LaTeX, then compile to `foo.pdf` | `:org-export-pdf` |
| `l o` | PDF, then open it | `:org-export-pdf-open` |
| `C-s` | toggle: the subtree at point only | `--subtree` |
| `C-b` | toggle: body only (no preamble/`<head>`) | `--body-only` |
| | cancel a running export | `:org-export-cancel` |
| | show the PDF compile's output | `:org-export-log` |

`C-s` and `C-b` are transient **`flag` rows**, magit's switches: they flip in
place, and the fired command reads them through its `args-schema`
(`subtree`, `body-only`). A plugin menu could not carry a flag before 0.7.0
(§6). The letters are emacs's. The toggles stay set across exports in the session,
as emacs's do. Ex-commands are dashed and namespaced, per the naming rule.

**Opening** (`h o`, `l o`, `m o`) goes through `org.export.open-with`:
`"editor"` opens the file in lattice (a PDF in [`pdf-mode`](pdf-mode.md) in
the GUI), and `"system"` hands it to `open-external-uri`. The default is
`"editor"`. In the TUI, where a PDF page draws as alt text, the guide
recommends `"system"`.

**Exporting is asynchronous and visible.** The chord returns at once; the echo
area says `Exporting foo.org → foo.html…` and, when done,
`Exported foo.html (3 blocks run)` or the error. A PDF compile shows the
compiler's progress in `*org-export*` (an output buffer, `output-append`),
which is never opened unprompted; a failed compile names it in the echo.

## 3. The pipeline

```
C-c C-e h h ──► grammar seam: emit `org.export-requested` {buffer, backend, sink, toggles}
                  │  returns at once
                  ▼
events seam ──► buffer-text ─► parse ─► IR ─► filter ─► babel pass ─► transcode ─► sink
                                                         (async jobs)
```

**Nothing heavy runs on the keystroke.** The grammar seam only emits an event
the plugin itself subscribes to; parsing, rendering and writing happen on the
plugin's events actor, the pattern roam's scan already uses
(`EV_ROAM_SCAN_STEP`). The buffer is read with `buffer-text`, so export sees
the buffer as it is, unsaved edits included, as emacs does.

### 3.1 The parser — org-element in Rust, not tree-sitter

Export parses with a **pure-Rust, org-element-shaped parser** in the plugin
(`src/export/parse/`). It produces elements (sections, paragraphs, lists,
tables, blocks, drawers, keywords, footnote definitions, fixed-width areas,
LaTeX environments, horizontal rules) and objects (emphasis, verbatim,
code, links, footnote references, entities, LaTeX fragments, sub- and
superscripts, line breaks, timestamps, targets, statistics cookies, macros).

Tree-sitter is the wrong tool here, for three independent reasons:

1. **The grammar stops at the paragraph.** `tree-sitter-org`'s `paragraph`
   is a run of anonymous tokens: no emphasis, links, verbatim or footnote
   references. An inline parser is needed regardless.
2. **The grammar is not org's.** Its README: "not meant to implement emacs'
   orgmode parser exactly". Fine for highlighting, folding and motions,
   where a near-miss is cosmetic; export's output *is* the parse, and every
   divergence lands in a published file.
3. **It cannot parse text on the events actor.** The tree-sitter world parses
   *files* (`parse-file`), and the grammar seam's snapshot is the
   keystroke's. A text parser runs on any seam, and natively in tests.

The parser is babel's `block.rs` approach grown up: line-oriented,
no allocation per character, one pass for elements and a second, bounded per
paragraph, for objects.

### 3.2 One IR, three transcoders

The parse is a backend-neutral tree, the `ox` model: **one IR, and each
backend is a transcoder over it** (`src/export/{html,markdown,latex}.rs`).
Filtering (§3.3) and babel (§4) act on the IR once, before any transcoder, so
a fourth backend is a transcoder and nothing else. Transcoders are pure:
`fn(&Doc, &Options) -> String`, tested by golden files.

### 3.3 Options and filtering

Read from the file, in `ox`'s order of precedence (later wins): the plugin's
options → `#+OPTIONS:`, `#+TITLE:` and peers → for a subtree export, the
headline's `EXPORT_*` properties.

**Every default is a plugin option** (Dhruva, 2026-11-25: "latex compiler etc
must all be configurable (and other things for org-export)"). Each `ox`
variable the export reads has a typed `org.export.<name>` option. The option
is the lowest layer and the file's keywords override it, so a user sets their
defaults once instead of in every file:

| group | options (emacs's variable) |
|---|---|
| contents | one per `#+OPTIONS:` item: `with-toc`, `with-section-numbers`, `headline-levels`, `with-author`, `with-email`, `with-date`, `with-title`, `with-tags`, `with-todo-keywords`, `with-priority`, `with-timestamps`, `with-planning`, `with-clocks`, `with-properties`, `with-drawers`, `with-footnotes`, `with-statistics-cookies`, `with-latex`, `with-sub-superscripts`, `with-special-strings`, `with-emphasize`, `with-fixed-width`, `with-tables`, `with-entities`, `preserve-breaks`, `broken-links` (`org-export-with-*` and peers); and `author`, `email`, `select-tags`, `exclude-tags`, `language` |
| HTML | `html-stylesheet`, `html-head`, `html-head-extra` |
| LaTeX | `latex-compiler`, `latex-class`, `latex-class-options`, `latex-header`, `latex-classes`, `latex-src-block-backend`, `latex-image-width`, `latex-pdf-process` |

Option types follow `ox`'s: a flag is a boolean, `headline-levels` an integer,
a list option a TOML array, and the rest (a depth such as `with-toc`, a
choice such as `with-tags`) a string in the `#+OPTIONS:` spelling, `true`
and `false` standing for `t` and `nil`. One table in the plugin maps each
option to its `#+OPTIONS:` key and goes through the same parser, so the two
spell values alike. `#+HTML_HEAD` replaces `html-head`'s lines, as the
keyword overrides `org-html-head` in ox; `#+LATEX_HEADER` adds to
`latex-header`'s, which plays `org-latex-packages-alist`'s always-loaded
part. A value that does
not parse is refused when it is set, naming it, as with every typed option.
A string option with a closed set of values (`latex-compiler`,
`latex-src-block-backend`) is checked when an export reads it, and an export
it would change stops, naming the option and the values it takes; the option
types have no enum of strings to refuse it earlier.
The plugin reads them when the export is asked for, not per keystroke.

The v1 set, each with `ox`'s default: `toc`, `num`, `H`, `^`, `author`,
`date`, `title`, `email`, `tags`, `todo`, `pri`, `<` (timestamps), `p`
(planning), `d` (drawers), `f` (footnotes), `tex`, `broken-links`, `*`, `-`,
`:`, `|`, `e`, `\n`. Keywords: `TITLE`, `SUBTITLE`, `AUTHOR`, `EMAIL`,
`DATE`, `LANGUAGE`, `SELECT_TAGS`, `EXCLUDE_TAGS` (default `noexport`),
`HTML_HEAD`, `HTML_HEAD_EXTRA`, `LATEX_CLASS`, `LATEX_CLASS_OPTIONS`,
`LATEX_HEADER`, `LATEX_HEADER_EXTRA`, `LATEX_COMPILER`, `MACRO`, `CAPTION`,
`NAME`, `ATTR_HTML`, `ATTR_LATEX`, `INCLUDE`.

A `COMMENT` headline and a `:noexport:` subtree are dropped; `SELECT_TAGS`
keeps only tagged subtrees and their ancestors. Internal links resolve
against `CUSTOM_ID`, `#+NAME`, `<<targets>>` and headline text, in `ox`'s
order; a link that resolves to nothing is an error unless
`broken-links:t` / `mark`, as in emacs.

**`#+INCLUDE`** reads the file with `read-file`, so it can include only what
the plugin may read. An include it may not read is an error naming the path,
not a silent omission.

## 4. Babel during export — run like emacs, on a copy

Export evaluates source blocks as emacs does (`org-export-use-babel`): a
block's `:exports` decides what appears, and `results` / `both` run it.

| `:exports` | appears | runs |
|---|---|---|
| `code` (default) | the block | no |
| `results` | its result | yes |
| `both` | block, then result | yes |
| `none` | nothing | no |

**It runs on a copy.** Emacs evaluates in a copy of the buffer, so exporting
never rewrites the file's `#+RESULTS:`, and neither does this. The result of
a run lives in the IR for that export and nowhere else.

**It runs through babel, not beside it.** Each block goes through
`org-babel-mode`'s runner: the same languages table (only a configured
program runs), the same header-argument merge, `:var` chains, noweb and
sessions. Export adds no second way to run a program. With `org-babel-mode`
off, `:exports results` uses the result already in the file and says so once.

**Consent is asked once per export**, as `execute-buffer` asks once:
`Export runs 3 blocks (sh ×2, python)? (y/n)`, through `effect.confirm`, or
straight through with `org.babel.confirm = false`. `n` exports with the
results already in the file. Blocks marked `:eval no-export` /
`never-export` never run during export, as in emacs. A block in an
unconfigured language uses its existing result, and the echo names the
language once.

**State crosses seams through the store**, as babel's jobs do: an export run
is a store record (`export/<run-id>`) holding the IR, the pending job ids and
the sink. Each `job-finished` fills one result; the last one transcodes and
writes. `:org-export-cancel` cancels the run's jobs and drops the record.

## 5. Sinks

**File.** The default target is the source's name with the backend's
extension, beside it; a subtree export uses the headline's
`EXPORT_FILE_NAME` when there is one, as emacs does. The write is an
`effect.write-to-file` with the `whole` anchor (OB.9), posted from the events
seam (`post-effects`). It is gated on `fs:write`, so the plugin may export
only where it may write:

```toml
capabilities = [
	"fs:write:{org.directory|~/org}",
	"fs:write:{org.roam-directory|~/org-roam}",
	"fs:write:{org.babel.tangle-directories}",
	"fs:write:{org.export.directories}",
	"proc:spawn:{org.babel.languages:command}",
	"proc:spawn:{org.export.latex-pdf-process:command}",
	"state:write",
]
```

`org.export.directories` is a list, empty by default. A file under
`org.directory` already exports; a project's `README.org` needs its
directory listed. A refusal says exactly that, naming the option, before
anything runs: export checks the target with `can-write-file` *before*
asking to run blocks, as capture checks its target before a word is typed.

**Buffer.** `h H` / `m M` / `l L` open `*Org HTML Export*` (and peers) in
`html-mode`, `markdown-mode` or `text-mode`, with the output as seed text.
Re-exporting **replaces** the buffer's text in one undo step and keeps the
cursor's line. Seed text alone cannot do that (it is ignored for a buffer
that exists, by design), so the buffer carries a minor whose
`minor-activated` event hands the plugin its id: one per backend
(`org-export-html-output-mode`, `-markdown-`, `-latex-`), because the event
names the buffer and not the export, and the buffer cannot be read at that
moment to tell them apart (`buffer-text` waits for the editor to publish
that the plugin's mode is on). A re-export posts one whole-text `apply-edit` and then
`open-synthetic-buffer`; returning to the buffer restores the caret's line
(§9).

**PDF.** `l p` writes `foo.tex`, then runs the user's
`org.export.latex-pdf-process` in the file's directory, emacs's
`org-latex-pdf-process`:

```toml
[[org.export.latex-pdf-process]]
command = "latexmk"
args = ["-pdf", "-interaction=nonstopmode", "-output-directory=%o", "%f"]
```

A list, run in order, each after the previous succeeds; `%f` is the `.tex`
path, `%b` its base name, `%o` its directory, `%latex` the file's
`#+LATEX_COMPILER` (default `pdflatex`). `%latex` is safe to splice from the
file because it is one of three names, refused otherwise; the command around
it is still the user's. It is granted **only from a
value the user wrote** (OB.2's rule): the plugin's default is empty, so no
compiler runs until the user names one, and PDF export says how to name one.
A non-zero exit stops the list; the log is in `*org-export*`, and the echo
names it and `:org-export-log`, which shows it. The `.tex` is written with
`write-file`, whose `ok` means the bytes are on disk, so the compiler never
starts on a half-written file; the `write-to-file` effect would race it.
The log starts each export on a clean page, and its headerline says what
is running and how it ended.

## 6. The host changes: plugin API 0.7.0, one publish

Everything export and pdf-mode need from the plugin API ships in **one**
version, 0.7.0, audited against every slice before EX.1 starts (Dhruva,
2026-11-25: "all updates to WIT for pdf / export are bundled together in the
next version (0.7.0), I don't want multiple publishes for this"). Four
changes:

| change | why | ABI |
|---|---|---|
| `host-services.write-file` | the `.tex` on disk before the compiler starts (below) | additive |
| `flag` arm on `transient-item-kind` | the dispatcher's `C-s` / `C-b` toggles (§2) | **breaking**: a type plugins return |
| `post-effects` carries `open-buffer` and `open-external-uri`, for a path inside the plugin's own `fs` grants | `h o` / `m o` / `l o` open the output once the export lands on the events seam | behaviour |
| `post-effects` carries `confirm` | babel's one consent, asked after the parse, when the count is exact (§4) | behaviour |

**0.7.0 is therefore breaking**, as 0.4, 0.5 and 0.6 were: every plugin and
every `init.rs` moves its pin to `lattice-wit = "0.7"` and rebuilds. The
`flag` arm alone forces it; `write-file` would have been additive.

**The open-effects stay narrow.** A posted open lands seconds after the
command that caused it (a compile), so it cannot come from the command's
return; but a background plugin opening anything at all would be a new
surface. Limited to a path inside the plugin's `fs` grants, it opens only
what the plugin could already have written.

**A posted `confirm` can catch keys.** Any plugin may now raise a y/n prompt
from the background, and a prompt that appears while the user types takes
the next key. Export's lands milliseconds after the user's own chord. The
alternative, counting blocks on the chord as `execute-buffer` does, puts an
O(file) scan on the grammar seam's Reflex budget and miscounts: it cannot see
`:noexport:`, `SELECT_TAGS` or `#+INCLUDE`.

The pdf side needs nothing here: PF.5b is pdf-mode alone.

### `write-file`

The `.tex` must be **on disk before the compiler starts.** `write-to-file` is
an *effect*: it is applied later, on the editor actor, and the guest learns
nothing about when. Spawning `latexmk` right after posting it is a race the
compiler can win, reading a missing or stale file.

The addition is a host service, the peer of `read-file` and `delete-file`:

```wit
/// Write `text` to `path`, replacing it. Gated on `fs:write`, re-checked
/// host-side with the canonicalization `delete-file` uses, so a symlink
/// inside a grant that points outside it is refused. Refused when `path`
/// is open in a buffer with unsaved changes (the user's edits are not
/// overwritten behind their back); an open, clean buffer reloads through
/// autoread. `ok` once the bytes are written.
write-file: func(path: string, text: string) -> result<_, string>;
```

Synchronous, because ordering is the point: when it returns `ok`, the file
is there. Like `read-file` and `delete-file` it runs on the calling seam's
thread (host functions are synchronous imports on every linker), so from
the events seam, where export writes, the editor actor never waits on it;
an action on the grammar seam would pay the write on the keystroke, which
is why export moves the work off that seam first (§7). Additive on its own;
it ships in 0.7.0 with the rest of the table above.

**Why not the alternatives.** Waiting for a `watch` event on the `.tex`
before spawning: the same race, one hop later, plus a watcher per export.
Piping the `.tex` on the compiler's stdin: only tectonic reads a document
from stdin; `latexmk` and `pdflatex` need a file, and they are what users
have configured for emacs.

HTML and Markdown keep using the effect: nothing reads their output before
the host applies it, and an effect list is applied in order, so `h o`'s
open follows its write.

## 7. Performance and the UX contract

- **Off the keystroke.** The chord costs one `emit-event`; everything else
  is on the plugin's events actor (§3). Paramount goal #1.
- **Bounded per call.** No events-seam call may exceed its fuel and epoch
  budget, and a whole-file export does not fit one: EX.0 measured 145% of
  the event budget for 20,000 real lines (7.9% for 2,300). So the export
  **steps** through self-emitted events, as the roam scan does, each step
  bounded by the bytes it covers: the includes, the parse a chunk at a
  time, then three walks over the headlines (numbering the outline, the
  writer's index pass for its contents or what it must know before the
  body, then the body a section at a time). Anything over the whole file
  is a walk, never a loop in one call: numbering 4,000 headlines at once
  was enough to trap. Fuel is the bound; wall time is milliseconds.
- **A bench.** `cargo bench` in the plugin covers parse and each transcoder
  natively (the parser is pure Rust), so a regression shows in numbers, not
  in a user's report.
- **No pixel the user did not ask for.** Exporting edits nothing in the
  source buffer: babel runs on a copy, and a buffer export only touches
  its own buffer.

## 8. Not built, and why

- **Visible-only export** (`C-v`). Needs the fold state, which no seam
  hands a plugin. ⛔ until a fold-state read exists for another reason.
- **Async export** (`C-a`). Every export is asynchronous already, so there is
  nothing to toggle.
- **Syntax highlighting in exported source blocks.** Emacs uses `htmlize`
  against the buffer's faces. Here the HTML gets `src-<lang>` classes, which
  a highlight.js or Prism stylesheet picks up. Host-side highlighting into
  HTML spans is ⛔, a follow-up if wanted.
- **ODT, texinfo, beamer, iCalendar, publishing projects** (`org-publish`).
  Each is another transcoder or a project layer over the same IR, and none
  is in the first cut.
- **Export templates** (`C-c C-e #`). Insertion of option skeletons; ⛔.
- **The rest of ox-latex's knobs.** Default-package overrides
  (`org-latex-packages-alist`, `org-latex-default-packages-alist`) are
  `#+LATEX_HEADER` lines, and a class header's `[NO-DEFAULT-PACKAGES]`
  drops the defaults; the hyperref template is niche; ⛔ for both, each a
  typed option when someone asks. `#+BIND` binds elisp
  variables for one export, and lattice has no elisp: ❌, not deferred.

## 9. Settled in EX.0

- **Re-exporting into a buffer: the whole-text `apply-edit`** (§5), with the
  id learned from the minor's activation (a minor per backend, EX.6b). No host change. Output buffers
  lost: their major is fixed to `plugin-output-mode`, so no `html-mode`,
  and they cut lines at 4,096 characters. A `replace` flag on the payload
  would be a host change for what `apply-edit` already does. Keeping the
  caret's line needed one host *fix*, not a feature: returning to any
  buffer put the caret on line 1 (`a6d71dc5`).
- **The fuel budget:** stepped (§7).
- **The dispatcher's toggles: transient `flag` rows** (Dhruva, 2026-11-25),
  crossed in 0.7.0 (§6).
- **Consent: after the parse, through a posted `confirm`** (Dhruva,
  2026-11-25; §6).

## 10. Rejected alternatives

- **Tree-sitter as the parser, with an inline layer on top** (§3.1). It keeps
  one parser for highlighting and export, but it is the wrong one for export
  and cannot run on the events actor.
- **A new host `tree-sitter.parse-text`.** Generic and small, but it fixes
  only reason 3 of §3.1 and leaves the grammar's divergences in every
  exported file.
- **Shelling out to `pandoc`.** pandoc's org reader is good but not emacs's:
  no babel, no `#+INCLUDE` semantics, different option handling. It would
  also make every export depend on an installed program.
- **Running export on the grammar seam.** Simplest, and it puts a whole-file
  parse and render on the keystroke. Paramount goal #1.
- **Writing results back into the file during export.** Emacs does not, and
  a published file must not change the source as a side effect.
- **Copying `ox-html`'s stylesheet.** It is GPL'd (§1).

## 11. Paramount-goal alignment

- **#1 performance:** nothing but an event emit on the keystroke; parse and
  render are measured and bounded (§7).
- **#2 extensibility:** the whole feature is a user-installable plugin; the
  host gains one generic service any plugin can use.
- **#3 vim modal editing:** untouched; the dispatcher is a transient and the
  chords live on the mode's own layer.
- **#4 asynchronicity:** export, babel runs and the PDF compile are jobs and
  events; the editor never waits on them.
