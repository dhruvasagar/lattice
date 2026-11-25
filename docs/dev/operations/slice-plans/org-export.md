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
| EX.0                      | Spike: fuel budget, re-export into a buffer, list grant, transient **(plugin)** | ✅ |
| **Phase 1 — host**        |                                                                             |        |
| EX.1                      | Plugin API 0.7.0, the whole of it: `write-file`, transient `flag`, posted opens and `confirm` **(lattice)** | 🚧 |
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
proceeds. It is the WHOLE of 0.7.0 (one publish), and several slices lean on
it: EX.2's dispatcher (the `flag` rows), EX.6 / EX.7's `o` sinks (posted
opens), EX.9 (`write-file`) and EX.10 (posted `confirm`). The parser, EX.3–EX.5,
is pure plugin and can land against lattice-wit 0.6.0 while EX.R1 is out;
the rest is built against a path dependency on 0.7.0 and released after it. EX.10 comes after
the backends because it acts on the IR they already consume, and a backend that
is wrong is easier to see without babel in the way.

## Slices

### EX.0 — spike (plugin) ✅

**Answers** (2026-11-25; the throwaway code is gone, as planned):

1. **Fuel: a large export does not fit one call. EX.2 builds the stepped
   pipeline.** A throwaway line-oriented parse plus object pass plus HTML
   render, built as the component is (`opt-level = "s"`), run under the
   host's fuel setting against `PluginBudget::event()` (100M fuel, 5 s):

   | input | fuel | of the budget | wall |
   |---|---|---|---|
   | 2,300 lines of a real org file | 7.9M | 7.9% | 0.4 ms |
   | 20,000 lines of real org files | 145M | 145% | 9 ms |
   | 20,000 lines of dense synthetic markup | 680M | 680% | 77 ms |

   The bar was 20,000 lines inside half the budget; real text is 3× over it.
   Fuel, not the epoch, is the bound: wall time is far below 5 s. A step
   should take about 5,000 real lines (≈35M fuel), less where markup is
   dense, so a step is bounded by bytes of objects, not by lines.
   The host does not record fuel per call (`trace.rs`'s `fuel_delta` is
   always 0); the harness read `Store::get_fuel` directly.
2. **Re-export: one whole-text `apply-edit`, no host change.** The export
   buffer opens with `activate-minor: org-export-output-mode`; the
   `minor-activated` event hands the plugin the buffer id, which it keeps in
   the store (the agenda log's pattern, `lib.rs`'s `AGENDA_LOG_MODE_ID`).
   Re-export reads the end position with `buffer-text`, then posts
   `apply-edit` (whole range, `cursor: none`) followed by
   `open-synthetic-buffer`. Run against the host, this is one `u` and the
   caret returns to its line, but only after `a6d71dc5`: until then every
   return to a buffer landed on line 1 (`:b` included), a host bug this
   spike found and fixed. A killed buffer leaves a stale id, the
   `apply-edit` is dropped, and the `open-synthetic-buffer` seeds a fresh
   one, so posting both is always right. Never pass `cursor: some`: it moves
   the focused buffer's caret whatever `target` names.
   *Lost:* output buffers (major fixed to `plugin-output-mode`, so no
   `html-mode`, and lines cut at 4,096 characters, a minified stylesheet's
   length); a `replace` flag on the payload (a host change, and not needed).
3. **`fs:write:{org.export.directories}`, empty by default:** answered by
   what exists. Grants resolve by option name, and the host already pins
   both halves for any list option (`capability.rs`:
   `an_empty_list_grants_nothing_and_does_not_fall_back`,
   `a_list_option_grants_each_entry`); `org.babel.tangle-directories` is the
   same shape in org. EX.2 adds the option to `org_manifest.rs`'s
   option-bound pin.
4. **The transient cannot carry the toggles as they stand.** The WIT's
   `transient-item-kind` has `action`, `argument` and `dismiss`; `flag` is
   deferred in `plugin-transients.md` §6 "because nothing needs a boolean
   toggle yet". Export is the first that does. **Open: a decision for
   Dhruva** (crossing `flag` in plugin API 0.7.0 beside `write-file`, or
   toggles as rows that reopen the menu).

**Found for the PDF loop** (Dhruva, 2026-11-25: "one option that exports and
opens the PDF within lattice and be able to see the PDF update if
re-exported"). `l o` with `open-with = "editor"` already opens it in
pdf-mode, and PF.5 reloads it in place. A reload that catches pdfTeX
mid-write replaces the pages with an error line; that is pdf-mode's
[PF.5b](pdf-mode.md), and EX.9 depends on it.

**The questions as planned:**

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

### EX.1 — plugin API 0.7.0 (lattice) 🚧

**Every WIT and plugin-API change export and pdf-mode need, in one version**
(design §6's table; Dhruva, 2026-11-25). A later slice that finds it needs
another is a miss in this audit, so the audit was run against EX.2–EX.10 and
PF.5b before EX.1 starts. It lands as one commit per part (EX.1a–EX.1e),
each green on its own, the version bump and re-pins last.

1. ✅ **EX.1a `host-services.write-file(path, text) -> result<_, string>`.**
   Host functions are synchronous imports wired once for every linker, so
   it runs on the calling seam's thread as `read-file` does; the plan's
   `spawn_blocking` had no async import to sit behind. The open-buffer
   check needed one store addition, `BufferStore::find_by_path` (defaulted;
   the registry answers it with `document_with_path`).
   *Dropped test:* "a guest against an unwired linker fails instantiation".
   `write-file` is in `host-services`, which every linker wires whole, so
   there is no linker without it to test against.
2. ✅ **EX.1b `flag` on `transient-item-kind`**: `flag(transient-flag { name, default })`,
   mirroring `TransientItemKind::Flag`. A guest sets `default` per open, from
   its store, which is how the toggles stay set across exports.
   The conversion is one arm in `spec_from_wit`; open, toggle and the
   schema projection are the host's native flag machinery, unchanged.
   Tests: `transient_source.rs`'s `a_flag_row_crosses_with_its_name_and_default`
   (a real guest) and `lattice-host`'s `plugin_transient_flag_round_trips.rs`
   (default reaches the command, a key flips it, projected by name against a
   schema in the reverse order; both fail with the open-time seeding removed).
3. ✅ **EX.1c `post-effects` carries `open-buffer` and `open-external-uri`** when the
   path is inside the plugin's `fs` grants (read or write); outside, the
   effect is replaced by a warning naming the grant, as `write-to-file` is.
   A URI that is not a file path is refused when posted.
   As built: a posted `open-buffer` becomes the host-applied
   `open-buffer-at-column` with no column, because a post drains host-side
   (`run_tick_pending` → `handle_effect`) where the peer-applied open is
   dropped; `show_document_open_effect.rs` already pins that arm. A posted
   open needs an absolute path (the host would resolve a relative one
   against `:cd`, not where the grant was compared) and is never `force`d.
   `open-external-uri` takes an absolute path or a `file://` URI; one with a
   percent-escape is refused rather than decoded, since the check would read
   `%2e%2e` where the OS handler opens `..`. Tests: `effect_authorizer.rs`
   unit tests (inside, outside, each refused shape) and
   `post_effects_seam.rs` through the events guest.
4. 📝 **EX.1d `post-effects` carries `confirm`.**
5. 📝 **EX.1e** the package to `@0.7.0`, the crates to 0.7.0, the bundled
   plugins and the `init.rs` world re-pinned, the breaking note.

Tests: a write inside the grant lands, and `ok` means it is on disk; outside
the grant, refused; a symlink inside the grant pointing outside, refused (the
`delete-file` canonicalization, reused, not copied); a path open in a dirty
buffer, refused with a message naming the buffer; open and clean, written
(the reload is autoread's, pinned by its own suite); a directory, refused
(EX.1a: `host_services.rs` unit tests and
`tests/write_file_on_the_sync_linker.rs` through a real guest). A guest menu's `flag` row
round-trips: its default reaches the state, a toggle flips it, and the fired
command's `args-schema` receives it by name. A posted `open-buffer` inside
the grant opens the file without a keypress and one outside is refused with
the grant named; the same for `open-external-uri`, plus a non-path URI
refused. A posted `confirm` raises the prompt without a keypress and `y`
fires its action. Plugin API reference regenerated, the bundled plugins and
the `init.rs` world moved to `@0.7.0`. CHANGELOG: Added, and the breaking
note for the `flag` arm, as 0.6.0's for `file-anchor`.

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

Needs PF.5b. Tests: the process grant follows only a user-written value
(OB.2's pins, re-run for the new option); `l o` with the PDF already open
reloads that buffer at its page instead of opening a second one; a failing
step stops the list and names the log;
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
