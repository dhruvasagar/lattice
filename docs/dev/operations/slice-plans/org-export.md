# Slice plan — org export

Design: [`org-export.md`](../../architecture/org-export.md). Read it first; this
plan argues from it and does not repeat it.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

Two repositories, as for babel. **(lattice)** slices land in this tree and
ship in a lattice release before any **(plugin)** slice may use them.
**(plugin)** slices land in
[`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), all
of them inside `org-export-mode` (design §0). Viewing the PDF in the editor is
[`pdf-mode.md`](pdf-mode.md)'s plan; `l o` works without it, through
`org.export.open-with = "system"`.

| Slice                     | Description                                                                 | Status |
|---------------------------|-----------------------------------------------------------------------------|--------|
| **Phase 0 — settle**      |                                                                             |        |
| EX.0                      | Spike: fuel budget, re-export into a buffer, list grant, transient **(plugin)** | 📝 |
| **Phase 1 — host**        |                                                                             |        |
| EX.1                      | `host-services.write-file`, plugin API 0.7.0 **(lattice)**                  | 📝     |
| EX.R1                     | lattice release carrying EX.1; publish lattice-wit 0.7.0 **(lattice)**      | 📝     |
| **Phase 2 — the mode and the parse** |                                                                  |        |
| EX.2                      | `org-export-mode`: mode, layer, gate, options, dispatcher, event pipeline **(plugin)** | 📝 |
| EX.3                      | Parser: elements **(plugin)**                                               | 📝     |
| EX.4                      | Parser: objects **(plugin)**                                                | 📝     |
| EX.5                      | Options, filtering, macros, `#+INCLUDE`, link resolution **(plugin)**       | 📝     |
| **Phase 3 — backends**    |                                                                             |        |
| EX.6                      | HTML: transcoder, stylesheet, file / buffer / open sinks **(plugin)**       | 📝     |
| EX.7                      | Markdown (GFM) **(plugin)**                                                 | 📝     |
| EX.8                      | LaTeX: transcoder, `.tex` file / buffer **(plugin)**                        | 📝     |
| EX.9                      | PDF: `latex-pdf-process`, `*org-export*` log, `l p` / `l o` **(plugin)**    | 📝     |
| **Phase 4 — babel**       |                                                                             |        |
| EX.10                     | `:exports`, run on a copy, one consent, cancel **(plugin)**                 | 📝     |
| **Phase 5 — finish**      |                                                                             |        |
| EX.11                     | Bench, guide (`doc/export.md`), user docs, scope lines **(plugin + lattice)** | 📝   |
| EX.R2                     | org plugin release **(plugin)**                                             | 📝     |

## Ordering

EX.0 comes first because two of its answers can change slices after it: if a
large export does not fit one events-seam call, EX.2's pipeline is a stepped
one from the start; and the re-export answer decides whether EX.6 needs a host
change. EX.1 then goes in early so EX.R1 ships while the plugin work
proceeds: only EX.9 needs it. EX.3–EX.8 are pure plugin and can land against
lattice-wit 0.6.0; EX.9 is the first slice that needs 0.7.0. EX.10 comes after
the backends because it acts on the IR they already consume, and a backend that
is wrong is easier to see without babel in the way.

## Slices

### EX.0 — spike (plugin) 📝

Answers, recorded in this plan with the evidence, before EX.2:

1. **Fuel and epoch.** A throwaway parse-and-render on the events seam over
   the 2,300-line reference file and a generated 20,000-line one. Record the
   fuel used against the budget. If 20,000 lines do not fit with a 2× margin,
   EX.2 builds the stepped pipeline (design §7).
2. **Re-export into a buffer** (design §9). Try the candidates in the order
   given there. Pick one that needs no host change if one exists, and record
   why the others lost.
3. **`fs:write:{org.export.directories}` with an empty default** loads, grants
   nothing, and a value the user writes grants exactly its entries (OB.1's
   machinery, exercised for a new option).
4. **The transient** (`open-transient`) can carry the dispatcher's two
   toggles and twelve actions from a plugin, and a toggle's state reaches the
   action.

Throwaway code; nothing from it is committed except this plan's update.

### EX.1 — `write-file` (lattice) 📝

`host-services.write-file(path, text) -> result<_, string>` (design §6), on
both linkers, `spawn_blocking` host-side.

Tests: a write inside the grant lands, and `ok` means it is on disk; outside
the grant, refused; a symlink inside the grant pointing outside, refused (the
`delete-file` canonicalization, reused, not copied); a path open in a dirty
buffer, refused with a message naming the buffer; open and clean, written and
the buffer reloads; a directory, refused; a guest against an unwired linker
fails instantiation, as for `post-effects`. Plugin API reference regenerated.
CHANGELOG: Added.

### EX.2 — the mode (plugin) 📝

`org-export-mode`: its activation, `mode_gates` switch
`org.export.enabled`, the `C-c C-e` layer, the options (`enabled`,
`directories`, `open-with`, `latex-pdf-process`, `html-stylesheet`), the
manifest grants, the ex-commands, the dispatcher transient, and the pipeline
skeleton: emit `org.export-requested` → events-seam handler → a stub
transcoder → the file sink, through `can-write-file` first.

Tests: the chord opens the dispatcher only in org buffers; `org.export.enabled
= false` removes the layer and the commands' effect; a refused target is
reported before anything runs, naming `org.export.directories`; the chord's
grammar-seam call emits and returns, and the export lands with no further
keypress (tested the way it fails: no action dispatched after the chord).

### EX.3 — parser: elements (plugin) 📝

Sections and headlines (TODO keyword, priority, tags, COMMENT), paragraphs,
plain / ordered / description lists with checkboxes and continuation,
tables (alignment cookies, hlines), every `#+begin_X` block, drawers and
property drawers, keywords and affiliated keywords (`#+NAME`, `#+CAPTION`,
`#+ATTR_*`), footnote definitions, fixed-width `: `, comments, horizontal
rules, LaTeX environments, planning lines, clocks.

Tests: golden IR dumps over a corpus that covers each element, including
the edge cases org-element is known for (a list ended by two blank lines, a
block inside a list item, a drawer that never closes, a headline inside a
block). Property: parsing never panics on arbitrary text (a fuzz-style test
over generated input, run natively).

### EX.4 — parser: objects (plugin) 📝

Emphasis (`*` `/` `_` `+` `=` `~`, with org's pre/post character rules),
links (bracket, angle, plain; `links.rs`'s parser, extended rather than
duplicated), footnote references (numbered, named, inline), entities,
LaTeX fragments, sub- and superscripts, line breaks, timestamps, targets and
radio targets, statistics cookies, macros.

Tests: golden object trees, plus the classic traps: `a*b*c` (not emphasis),
`=code with *stars*=` (verbatim wins), a link with brackets in its
description, emphasis across a line break, unicode around markers.

### EX.5 — options and filtering (plugin) 📝

Design §3.3 in full: precedence, the v1 `#+OPTIONS` keys, keywords,
`SELECT_TAGS` / `EXCLUDE_TAGS`, `COMMENT`, subtree export with `EXPORT_*`
properties, `#+MACRO` expansion, `#+INCLUDE` through `read-file`, internal
link resolution in `ox`'s order, `broken-links`. An unimplemented keyword is
named in the echo, once per export.

### EX.6 — HTML (plugin) 📝

The transcoder (design §1's structure and class names), the default
stylesheet (written fresh, design §1), `#+HTML_HEAD`, `#+ATTR_HTML`, raw
`#+BEGIN_EXPORT html`, MathJax for LaTeX fragments (`tex:t`), the TOC,
numbering, footnotes section, image links as `<img>`. The three sinks:
file, buffer (EX.0's answer), open (`org.export.open-with`).

Tests: golden HTML for the corpus; the document is well-formed (parsed back
by an HTML parser in the test); every internal link's target id exists.

### EX.7 — Markdown (plugin) 📝

GFM: ATX headings (with `H` and below as lists, as `ox-md`), pipe tables,
footnotes, fenced code with language, task-list checkboxes, autolinks, raw
`#+BEGIN_EXPORT markdown`. Golden tests; tables with inline markup and
pipes in cells.

### EX.8 — LaTeX (plugin) 📝

The preamble (design §1), `LATEX_CLASS` (`article`, `report`, `book`),
`LATEX_CLASS_OPTIONS`, `LATEX_HEADER`, sectioning by level with `H` and
below as `itemize`, tables as `tabular` (or `longtable` via
`#+ATTR_LATEX`), `verbatim` / `listings` for source, escaping of the ten
specials, raw `#+BEGIN_EXPORT latex`. Golden tests, plus: every golden
`.tex` compiles with `latexmk` when one is installed (skipped, and saying so,
when not).

### EX.9 — PDF (plugin) 📝

Needs EX.R1. `l p`: write `foo.tex` with `write-file`, then run each
`org.export.latex-pdf-process` entry in order (`%f` / `%b` / `%o`), each
after the last exits zero, as jobs; output to `*org-export*`; echo the
result. `l o`: then open it (`open-with`). Unconfigured: the echo says how to
configure a compiler, and nothing runs.

Tests: the process grant follows only a user-written value (OB.2's pins,
re-run for the new option); a failing step stops the list and names the log;
a fake compiler (a script that writes a file named `%b.pdf`) proves the
`.tex` is complete on disk when it starts; cancel kills the running step.

### EX.10 — babel during export (plugin) 📝

Design §4: `:exports` × the four values, run through `org-babel-mode`'s
runner on a copy, one consent per export, `n` uses existing results,
`:eval no-export` / `never-export`, unconfigured languages named once,
babel off ⇒ existing results, `:org-export-cancel`.

Tests: the source buffer's text is byte-identical after an export that ran
blocks; a `:var` chain and a `:session` block export with fresh results; a
run cancelled mid-way writes nothing.

### EX.11 — finish (plugin + lattice) 📝

- A `cargo bench` in the plugin: parse, and each transcoder, over the
  reference file and the 20,000-line one; results recorded in
  [`benchmarks.md`](../benchmarks.md).
- The plugin's `doc/export.md` (`:help org.export`), linked from
  `doc/org.md`; `doc/org.md`'s "What this plugin is not" loses "export
  backends".
- lattice's `docs/user/org.md` gains export; `org-mode.md` §10's scope line
  moves export out of the cuts, as babel's did.
- CHANGELOG entries in both repositories.

### EX.R1, EX.R2 — releases 📝

Dhruva's: lattice with EX.1 and lattice-wit 0.7.0 published, then the org
plugin with export, pinned to `@0.7.0`.
