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
| EX.1                      | Plugin API 0.7.0, the whole of it: `write-file`, transient `flag`, posted opens and `confirm` **(lattice)** | ✅ |
| EX.R1                     | lattice release carrying EX.1; publish lattice-wit 0.7.0 **(lattice)**      | 📝     |
| **Phase 2 — the mode and the parse** |                                                                  |        |
| EX.2                      | `org-export-mode`: mode, layer, gate, options, dispatcher, event pipeline **(plugin)** | ✅ |
| EX.3                      | Parser: elements, and the stepped pipeline **(plugin)**                     | ✅     |
| EX.4                      | Parser: objects **(plugin)**                                                | ✅     |
| EX.5a                     | Options and filtering **(plugin)**                                          | ✅     |
| EX.5b                     | `#+MACRO` expansion **(plugin)**                                            | ✅     |
| EX.5c                     | `#+INCLUDE` **(plugin)**                                                    | ✅     |
| EX.5d                     | Internal link resolution, `broken-links` **(plugin)**                       | ✅     |
| **Phase 3 — backends**    |                                                                             |        |
| EX.6a                     | HTML: transcoder, stylesheet, file sink **(plugin)**                        | ✅     |
| EX.6b                     | Buffer and open sinks, every backend **(plugin)**                           | ✅     |
| EX.7                      | Markdown (GFM) **(plugin)**                                                 | ✅     |
| EX.8a                     | LaTeX: transcoder, built-in classes, `.tex` file / buffer **(plugin)**      | ✅     |
| EX.8b                     | LaTeX knobs: classes, `HEADER_EXTRA`, compiler, src backend **(plugin)**    | ✅     |
| EX.8c                     | Every export default an `org.export.*` option (design §3.3) **(plugin)**     | ✅     |
| EX.9                      | PDF: `latex-pdf-process`, `*org-export*` log, `l p` / `l o` **(plugin)**    | ✅     |
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

### EX.1 — plugin API 0.7.0 (lattice) ✅

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
4. ✅ **EX.1d `post-effects` carries `confirm`.** A confirm was
   peer-applied, and a post drains host-side (`drain_tick_callbacks` keeps
   only renderer signals), so a posted one was dropped. Its body moved to
   `Editor::open_confirm`, applied in `handle_effect`; both peers' arms are
   now no-ops, which also retires the two copies the TUI and GPUI carried.
   Tests: `posted_confirm_raises_its_dialog.rs` (a tick callback yields it,
   the dialog is up with no key, `y` fires with the carried args; both fail
   with the host arm removed), the authorizer's `a_posted_confirm_is_kept`,
   and `post_effects_seam.rs` (a posted confirm naming a closed command is
   refused by the gate). The TUI's confirm-seeding tests now apply the effect
   rather than calling the deleted `do_confirm`.
5. ✅ **EX.1e** the package to `@0.7.0`, the crates to 0.7.0, the bundled
   plugins and the `init.rs` world re-pinned, the breaking note. Done with
   `cargo xtask bump-plugin-api 0.7.0` (39 package lines, 4 inline WIT
   references, the three crates, `Cargo.lock`), then by hand: the README
   pins, the authoring guide, the distribution doc, `version-mode.md`, the
   regenerated reference, and the fixture-guest and bundled-plugin
   lockfiles (version lines only; the bundled plugins depend by path).
   **Still a release blocker for EX.R1, as at 0.5.0 and 0.6.0:**
   `refresh_wit_package` never rewrites the package line of a user-owned
   `wit/init.wit`, so a scaffolded `init.rs` fails to build until the user
   edits it; the CHANGELOG states the step.

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

### EX.2 — the mode (plugin) ✅

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

**As built** (plugin `8760ca9`, on `build: plugin API 0.7.0` `4a92707`,
which moves the plugin's pins to 0.7.0 through the uncommitted
`.cargo/config.toml` patch until EX.R1 publishes):
- **`src/export/mod.rs`** owns the surface (mode, layer, options,
  commands, dispatcher); **`src/export/pipeline.rs`** the request and the
  events-seam half. `lib.rs` gains one call per registering seam and one
  arm per dispatch `match`, as babel's.
- **The dispatcher is a branch on org's one transient source**
  (`args: string("export")`), as the capture, TODO and agenda menus are;
  a guest has one `transient-source` id. Rows fire with `args: none`, so
  the host projects the flags into the commands' `subtree` / `body-only`
  slots by name. The switches the last export used are stored
  (`export/switches`) and seed the next open's flag defaults; the key is
  cleared on load, so a session starts with both off, as emacs's.
- **Departure: the renderers are placeholders**, the source verbatim in
  each format's shell (`<pre class="example">`, a fenced block,
  `verbatim`). EX.3–EX.8 replace `render`; the request, the crossing and
  the sink stay. Subtree and body-only already act on them.
- **Departure: options with their consumers.** Only `enabled` and
  `directories` are registered; `open-with` and `html-stylesheet` arrive
  with EX.6, and `latex-pdf-process` (with its `proc:spawn` grant) with
  EX.9, so no option ships that does nothing.
- **One step, not stepped.** A placeholder render is O(text) with no
  parse; the stepping of design §7 arrives with the parser (EX.3), where
  the fuel goes.
- **Tests.** `integration/tests/org_export.rs`, five against a real
  editor: the dispatcher only in org buffers; off removes layer and
  effect, the rest of org stays; a refused target echoes
  `org.export.directories` and writes nothing; an export lands on disk
  with no key after the row's (settled on the wake alone); a flag reaches
  the export and is still set at the next open (fails with the store
  write removed). Unit tests in both files.

### EX.3 — parser: elements, and the stepped pipeline (plugin) ✅

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

**As built** (2026-11-25):

- **Where.** `src/export/parse/` in the plugin: `mod.rs` (the IR, sections,
  headlines via `todo::parse` so "what is a headline" stays in one place,
  planning, property drawers, `Context::for_file` for a file's own
  `#+TODO:` / `#+SEQ_TODO:` / `#+TYP_TODO:`), `element.rs` (everything
  else), `dump.rs` (the readable tree). Paragraph text, titles, cells and
  item tags stay raw for EX.4.
- **Departure: the stepping landed here, with the parser,** not in EX.2,
  and it covers the render too: a run parses a chunk of ~64 KiB per call
  (cut at headlines, so the chunked parse is the whole parse — pinned by a
  test over the corpus and over the random inputs), then renders headlines
  per call under the same budget, walking the tree in preorder with an
  index-path cursor and escaping as it goes. The first cut rendered in one
  final call and a 1.5 MB file trapped on fuel; the integration test
  `a_file_too_large_for_one_call_is_exported_in_steps` fails with the
  stepping disabled (verified). **EX.6–EX.8's transcoders render through the
  same walk**, a headline's own parts per visit, opening and closing the
  outline as the cursor's depth moves; none may render the whole tree in
  one call.
- **Runs live in guest memory**, as the roam scan's do, not in the store:
  nothing in a run has to survive the instance, and serialising chunk trees
  between steps would spend fuel on the bound it exists to keep. A second
  export of the same target replaces the run in flight; others run beside
  it.
- **The placeholder body is the parse tree** (`dump`) in each shell, so the
  parse is visible end to end through a real export. It is also the IR's
  production reader until the transcoders exist; when EX.8 lands `dump`
  goes back to `#[cfg(test)]`, for the goldens.
- **Bounded.** End markers (`#+end_X`, `:END:`, `\end{X}`) are indexed once
  per section, so an unterminated opener is a lookup, not a rescan (60,000
  of them parse in milliseconds); nesting past 32 is read as text.
- **Known bound:** one section larger than the step budget is one step,
  since inside a section there is no exact cut. The request step also
  reads the whole file once (`#+TODO:`, `#+TITLE:`, the chunk plan).
- **Tests.** Four golden corpora (`testdata/*.org` → `.ir`,
  `UPDATE_GOLDEN=1` rewrites them for review) plus the chunk-equivalence,
  no-panic (3,000 generated inputs), depth and rescan tests in the parser;
  chunk planning, multi-step runs and the corpus render in the pipeline;
  six integration tests.

### EX.4 — parser: objects (plugin) ✅

Emphasis (`*` `/` `_` `+` `=` `~`, with org's pre/post character rules),
links (bracket, angle, plain; `links.rs`'s parser, extended rather than
duplicated), footnote references (numbered, named, inline), entities,
LaTeX fragments, sub- and superscripts, line breaks, timestamps, targets and
radio targets, statistics cookies, macros.

Tests: golden object trees, plus the classic traps: `a*b*c` (not emphasis),
`=code with *stars*=` (verbatim wins), a link with brackets in its
description, emphasis across a line break, unicode around markers.

**As built** (2026-11-25):

- **Where.** `src/export/parse/objects.rs`. The element pass runs it on
  paragraphs, headline titles (which keep `raw_title` too, for EX.5's
  `[[*Title]]` resolution), table cells, descriptive items' tags and
  captions, inside the parse step, so its fuel is stepped with the rest.
  Verse bodies and keyword values (`#+TITLE:` and peers) stay text: EX.5
  and the transcoders parse them with the same function.
- **Links extended, not duplicated.** `links.rs` gained `bracket_link`
  (org's rules: `\]` escapes in the path, single brackets allowed in the
  description) and `close_before`; `link_at` now goes through
  `bracket_link`, which fixed a real bug: `[[file:a\]b.org]]` opened a file
  named with the backslash.
- **Departure: export snippets** (`@@html:…@@`) are parsed too; org has
  them, and EX.6–EX.8 need them for raw inline output. Inline source blocks
  and babel calls are not (EX.10).
- **Entities: a table of ~150 names** (Greek, arrows, relations and
  operators, typography) with their UTF-8, not org's full `org-entities`
  list. An unknown `\name` is a LaTeX fragment, as in org. The LaTeX
  backend passes most names through; the few that are not LaTeX commands
  (`\nbsp`, `\ndash`…) need a mapping in EX.8.
- **Sub- and superscripts** are always read as `^:t`, and record whether
  they were braced. The `#+OPTIONS:` line that governs them may come after
  them, so EX.5a applies `^:{}` / `^:nil` after the parse (as ox does) by
  putting the unwanted ones back as text.
- **Linear in the line, measured.** The first cut was quadratic: an opener
  that never closes scanned to the end of its line, and a 60 KB line of
  ` *a` took 1.36 s natively, a fuel trap in the guest. Emphasis closes are
  now found once per text and binary-searched, the one-newline window from
  a newline index, and every other closer stops at the next opener of its
  kind or at a character its grammar forbids. A test runs seventeen such
  60 KB lines (0.1 s together).
- **Tests.** Inline tables of the traps, links and the rest in
  `objects.rs`; a fifth golden corpus, `objects.org`, that puts objects in
  every place they live; the chunk-equivalence and render-walk tests run
  over it; no-panic over 5,000 generated strings; nesting bounds.

### EX.5 — options, filtering, macros, includes, links (plugin)

Design §3.3 in full: precedence, the v1 `#+OPTIONS` keys, keywords,
`SELECT_TAGS` / `EXCLUDE_TAGS`, `COMMENT`, subtree export with `EXPORT_*`
properties, `#+MACRO` expansion, `#+INCLUDE` through `read-file`, internal
link resolution in `ox`'s order, `broken-links`. An unimplemented keyword is
named in the echo, once per export.

Carved in four: each is a mechanism of its own with its own failure modes.
Macros and includes were one slice in the first carve and split when they
turned out to sit at opposite ends of the pipeline (macros in the render
view, includes before the parse). EX.5d needs the tree both produce.

#### EX.5a — options and filtering ✅

- **`export/options.rs`.** `Options::resolve(keywords, subtree, file_stem)`
  in ox's precedence: ox's defaults, then the file's keywords in file order
  (`#+OPTIONS:` lines, `#+TITLE:` lines concatenated, the accumulating
  `#+HTML_HEAD`, `#+LATEX_HEADER`, `#+FILETAGS`), then a subtree's
  `EXPORT_*` properties. The `#+OPTIONS:` reader takes ox's lisp
  spellings: `t`, `nil`, numbers, symbols (`not-in-toc`, `{}`, `mark`) and
  lists (`d:("NOTES")`, `d:(not "LOGBOOK")`). Every key §3.3 lists is
  read, with ox's default; `c:`, `prop:` and `stat:` too.
- **The unsupported are named.** An unknown `#+OPTIONS:` key (or a value a
  known key does not take), and an export keyword ox reads that this does
  not (`#+SETUPFILE`, `#+BIND`, `#+EXPORT_FILE_NAME`, `#+LATEX_COMPILER`,
  `#+LATEX_HEADER_EXTRA`…), are collected once each and the closing echo
  becomes a warning: `Exported notes.md; not supported, so left out:
  #+SETUPFILE, html-postamble:`. Keywords export never reads (`#+STARTUP`,
  `#+TODO`) are not named: nothing is lost.
- **The whole file is parsed, always.** A subtree export no longer parses
  only the subtree's lines: the file's keywords apply to a subtree too, and
  only the parse tells a keyword from a look-alike inside an example block.
  The subtree is the innermost headline holding the cursor's line, taken
  out of the assembled tree; its raw title is the default title. Keywords
  are gathered from each chunk's sections as the parse steps run, from
  anywhere in the file, as ox does.
- **`export/filter.rs` — a view, not a rewrite.** The render walk asks
  `visits(h)` for each headline and does not descend into one it skips
  (`COMMENT`, an exclude tag, own or `#+FILETAGS`, or outside the select-tag
  selection). The selection is one iterative pass over the headlines (not
  their sections) after the parse. `own(h)` gives the visited headline's
  parts pruned: planning (`p:`), properties (`prop:`), clocks (`c:`),
  drawers (`d:`, default all but `LOGBOOK`), fixed-width (`::`), tables
  (`|:`), footnotes (`f:`), LaTeX (`tex:nil`), comments and comment blocks
  always. Objects: timestamps per `<:`, cookies per `stat:`, and `^:`,
  `*:` and `e:` revert what they turn off to the text it was written as
  (the IR now records `braced` scripts and `{}`-terminated entities so the
  revert is exact). The pruning is per visited headline, so a step still
  touches only its own headlines.
- **`filter::plain`** gives an object list's text without markup, for the
  `<title>` and, later, PDF metadata.
- **No user-level defaults.** The defaults are ox's own; there is no
  `org.export.*` option for, say, `toc`. A file that wants `toc:nil` says
  so, as it would for emacs. Revisit if asked.
- **Placeholder output** now opens with the resolved options, then the
  filtered tree.
- **Tests.** Options: defaults, every spelling, precedence including
  `EXPORT_*`, the ignored list, file tags. Filter: `COMMENT` / `noexport`,
  select tags with ancestors, file tags, each element and object rule, the
  reverts. Pipeline: the render-walk test now compares against a plain
  recursive filtered dump of all five corpora (it fails when the walk
  descends into a skipped subtree; checked); the subtree test picks the
  innermost headline. Integration: a file with `noexport`, `COMMENT`, an
  `#+OPTIONS: ^:nil` after its text and two unsupported asks, end to end.

#### EX.5b — `#+MACRO` ✅

- **`export/macros.rs`.** Templates are the keyword built-ins (`title`,
  `author`, `date`, `email`, `input-file`) and `#+MACRO:` lines, which win;
  `Options::resolve` collects them in its one precedence pass, so a
  subtree's `EXPORT_AUTHOR` is what `{{{author}}}` gives. `$1`… substitute,
  a missing argument is empty. The function built-ins: `time(FORMAT)` (the
  request's local time, through the plugin's `format_time`), `date(FORMAT)`
  (a `#+DATE:` timestamp, formatted; otherwise as written), `property(NAME)`
  (the headline being rendered, no inheritance: ox's default),
  `keyword(NAME)` (every value, space-joined), `n(NAME, ACTION)` counters.
- **In the tree, not the text.** ox replaces macros in the text before it
  parses; here they expand in the render view, as the walk reaches them,
  and the expansion is parsed as objects and filtered like its
  surroundings. Same result wherever a macro stands where an object may,
  which is where ox's replacement looks too. It keeps `n` in document order
  without a pass of its own, and keeps the expansion stepped.
- **The filter is a `View`.** It owns the `Options`, the macro state and
  the current headline's properties; `own`, `elements` and `objects` take
  it `&mut`.
- **Failures.** An undefined macro fails the export, as in ox, naming it:
  `org: {{{nope}}} is not defined`. A macro that expands into itself is
  stopped at depth 16 with the same kind of error. `(eval …)` bodies,
  `modification-time` and `property` with a search are named in the closing
  echo and expand to nothing.
- **Tests.** Templates and arguments, each function built-in, counters,
  the failures (`macros.rs`); precedence of built-ins and `#+MACRO:`
  (`options.rs`); expansion that is markup, nested, filtered by `*:nil`,
  `property` reading the rendered headline, the depth stop (`filter.rs`);
  an undefined macro in the title and in the body writes nothing
  (`pipeline.rs`); the objects corpus gained the `#+MACRO:` its `kbd` call
  needs; the integration test now carries a macro.

#### EX.5c — `#+INCLUDE` ✅

- **`export/include.rs`, textual and before the parse**, as in ox: an
  included file's headlines nest into the outline, its keywords apply, and
  its includes include in turn, all of which the parse of the whole sees
  for free. The text is held as pieces (text runs and unread includes);
  each step reads one include through `read-file` and splices its pieces
  in, so no call reads two files; the text is joined once at the end and
  only then are the parse chunks planned. A file with no include takes no
  extra step and makes no copy.
- **Found by one pass that skips blocks**, where `#+INCLUDE:` is content.
- **What it reads**: a path relative to the including file (or absolute,
  or `~`), lexically normalised so a cycle is caught by name (`org: … includes
  itself`); nesting stops at 16. `:lines "a-b"` (1-based, either end open),
  `:minlevel`, and ox's default without one: an include under a level-N
  headline has its shallowest headline at N+1. `::*Title` and `::#custom-id`
  take that subtree; `:only-contents` drops the headline, its planning and
  property drawer. A block kind wraps: `src lang …`, `example`,
  `export html`, or any other name; `src` and `example` content is
  comma-escaped so the parse reads it back as written.
- **Failures name the path**: not granted or missing (`org: cannot include
  /p: <host's reason>`), an unreadable directive, a search with no match.
  The export stops; nothing is written.
- **The cursor follows.** A line map records what each top-level include
  became (an empty one still took its own line), so a subtree export's
  cursor line is moved past the includes above it before the subtree is
  taken.
- **`#+TODO:` stays the including file's**, read before anything is
  included, as emacs reads the buffer's settings.
- **Departures, accepted**: footnote labels from an included file are not
  renamed (ox renames them to avoid clashes); a search other than `*` /
  `#` is read as a headline title, where ox would also try targets and
  names.
- **Tests.** `include.rs`: splice and nesting below the headline,
  `:minlevel`, relative includes from an include, blocks and escaping,
  both searches and `:only-contents`, an include in a block is text, each
  failure's message, the line map with an empty include. `pipeline.rs`:
  an included file's macro and title apply, an include a step, a subtree
  export under two includes takes the right headline (fails with the map
  disabled; checked), a missing include writes nothing. Integration: a
  granted include is spliced, an ungranted one fails naming its path,
  through the real host `read-file`.

#### EX.5d — links and `broken-links` ✅

- **`export/targets.rs`, built during the parse steps** a section at a
  time: `CUSTOM_ID`s, `ID`s, headline titles, `#+NAME:`s, `<<targets>>`
  and `<<<radio targets>>>`, each with the headline it sits under. After
  the selection the index learns which headlines are exported
  (`filter::live`, one pass over the headlines): a target in a subtree the
  export leaves out, or in the preamble of a subtree export, is no target,
  as in ox, which resolves against the pruned tree.
- **ox's order**: `#id` → `CUSTOM_ID`; `*Title` → headline; `id:X` → an
  `ID` here, else the roam index (known elsewhere: external; unknown:
  broken; no index: let through, since there is no telling); a scheme or a
  file path → external; anything else → target, then name, then title.
  Titles and plain paths compare with whitespace collapsed and ASCII case
  folded. `[[(ref)]]` code references are named in the closing echo.
- **The IR carries the answer**: `Object::Link` gained `target:
  Option<String>`, the anchor, `None` from the parse and for an external
  link; the filter sets it. Anchors are a `CUSTOM_ID` or `org` and an
  FNV-1a hash of what they name, so a re-export keeps them.
- **`broken-links:`**: nil (default) stops the export with `org: [[x]]
  links to nothing in the export (#+OPTIONS: broken-links:mark exports it
  marked)`; `mark` writes `[BROKEN LINK: x]`; `t` drops the link,
  description and all, as ox does.
- **Radio targets link their text**: whole words, ASCII case aside, the
  earliest and then longest match; not inside a link or the radio target
  itself.
- **A `Host` trait** (`read-file`, id lookup) sits behind the run, so the
  pipeline's tests use an in-memory one; the guest's is `Guest`.
- **Placeholder output** names each headline's anchor.
- **Tests.** Every path kind, liveness, anchor stability (`targets.rs`);
  resolved links, all three `broken-links` settings, a link into a
  `noexport` subtree, radio links (`filter.rs`); both liveness tests fail
  with liveness disabled (checked). The objects corpus's `[[x]]` now points
  at its `<<anchor>>`. Integration: a broken link stops the export and
  writes nothing.

### EX.6 — HTML (plugin) ✅

The transcoder (design §1's structure and class names), the default
stylesheet (written fresh, design §1), `#+HTML_HEAD`, `#+ATTR_HTML`, raw
`#+BEGIN_EXPORT html`, MathJax for LaTeX fragments (`tex:t`), the TOC,
numbering, footnotes section, image links as `<img>`. The three sinks:
file, buffer (EX.0's answer), open (`org.export.open-with`).

Tests: golden HTML for the corpus; the document is well-formed (parsed back
by an HTML parser in the test); every internal link's target id exists.

Carved in two: the sinks are not HTML's alone (`h H` / `m M` / `l L` and
`h o` / `m o` / `l o` are one mechanism for three backends), so they land
once, after the transcoder, rather than three times.

#### EX.6a — the HTML transcoder ✅

- **`export/html.rs`, written as the render walks.** `Html::start` (in the
  prepare step) numbers the headlines and builds the table of contents in
  one pass over the headlines alone, then writes the head, the title and
  the preamble; `Html::headline` writes one visited headline's own parts
  per walk visit; `Html::finish` closes what is open and writes the
  footnotes and the postamble. Open containers are a stack, so a headline
  closes exactly the sections at its level or deeper. The render step's
  budget counts what the writer added, as the placeholder's did.
- **ox-html's shape**: `#content`, `.title` / `.subtitle`,
  `#table-of-contents`, `outline-container-<anchor>` / `.outline-N`,
  `.outline-text-N`, `section-number-N`, `.todo` / `.done` plus the
  keyword, `.tag`, `.timestamp-wrapper`, `.org-src-container` /
  `pre.src.src-<lang>`, `pre.example`, `.org-ul` / `.org-ol` / `.org-dl`,
  checkbox `on` / `off` / `trans`, `.figure` / `figure-number`,
  `table-number`, `listing-number`, `.verse`, `.org-center`, a special
  block's own name, `#footnotes` / `.footdef` / `fnr.N` / `fn.N`,
  `#postamble`. Levels are relative (a subtree export's top is `h2`), `h6`
  at most.
- **Options it reads**: `H:` (deeper headlines are items of a `<ol>` when
  numbered, `<ul>` when not, as in ox), `num:` (reaches below `H:`, as in
  ox) and `UNNUMBERED` (`t`, `notoc`, inherited), `toc:` (capped at `H:`),
  `todo:`, `pri:`, `tags:` (`not-in-toc`), `tex:` (`t` = MathJax, `$…$` →
  `\(…\)`, `$$…$$` → `\[…\]`, the script only when the page has math;
  `verbatim`; `nil`), `-:` (en/em dash, ellipsis, soft hyphen), `\n:`,
  `author:` / `date:` / `email:` / `title:`, `#+HTML_HEAD` /
  `_EXTRA`, `#+LANGUAGE`, `#+SUBTITLE`.
- **Elements**: a paragraph that is only an image link is a figure with
  its caption; `#+ATTR_HTML` becomes attributes on the image or table;
  tables have `<thead>` (the rows before the first rule), `<colgroup>`, a
  cookie's alignment or right-aligned when most cells are numbers (org's
  `org-table-number-fraction`); list items' first paragraph has no `<p>`,
  as in ox; `#+NAME` is the element's `id`; `export html` blocks, `#+HTML:`
  and `@@html:…@@` are raw. A `#+TOC:` is named in the closing echo.
- **Links**: internal ones take the anchor the filter resolved and, with
  no description, the headline's number or title; a file link to `.org`
  goes to its `.html`; an `id:` link goes to `<node's file>.html#ID-<id>`,
  relative when the file is beside or under this one, which is why the
  `Host` id lookup now answers with the node's file (`IdFile`). A
  headline with an `:ID:` carries `<a id="ID-…">` whether or not its
  properties are exported. A described-less image link is `<img>`.
- **Footnotes** are numbered by first reference, wherever they are defined;
  a definition referencing another numbers it in turn; a repeated
  reference gets `fnr.N.2`. The `Footnotes` heading is collected, not
  exported, as in ox.
- **XML-safe**: void elements self-close, the only entities are XML's own
  and numeric ones, so the output is well-formed XML as well as HTML.
- **`org.export.html-stylesheet`** (registered here, its reader's slice):
  `default` inlines `export/html.css`, written fresh (ox-html's is GPL'd),
  light and dark; `none`; anything else is linked.
- **Done keywords** come from the file's `#+TODO:` lines or the configured
  sequences: `Context` now keeps the sequence lines, so it knows which
  keywords are done.
- **Departures, accepted**: no syntax highlighting (design §8, classes
  only); a footnote referenced but never defined exports empty (ox stops);
  the table of contents leaves out TODO keywords; a timestamp range is one
  span, its `--` not typeset.
- **Tests** (`html.rs`, 19): goldens for the five corpus files, each parsed
  back by `quick-xml` (a new dev-dependency) as strict XML with every
  `href="#…"` landing on an `id`; numbering and contents with `UNNUMBERED`;
  `num:nil toc:nil`; headlines below `H:` as list items (failed before
  `num:` reached below `H:`); done/todo classes; footnote order; MathJax
  only with math; file / id / image / URL links; the three stylesheet
  settings; body only; the postamble on and off; escaping versus raw HTML;
  table head and numeric alignment; source classes; an `:ID:` anchor with
  properties off (fails with the anchor disabled; checked). Integration:
  the real export writes the transcoder's document with the stylesheet
  inline. The objects corpus's `@@html:<kbd>@@` snippet is now closed, so
  the golden is well-formed.

#### EX.6b — buffer and open sinks ✅

- **`export/sink.rs`** owns the end of a run: the pipeline's last step
  hands it the output and the done echo, and it returns what to post. The
  request carries a `Sink` (`File`, `Buffer`, `Open { system }`), decided
  by the command, so the pipeline stays pure and its tests cover every
  sink.
- **Commands**: `:org-export-html-buffer`, `-markdown-buffer`,
  `-latex-buffer`, `:org-export-html-open`, `-markdown-open`; dispatcher
  rows `h H`, `h o`, `m M`, `m o`, `l L`. `l o` is PDF's, EX.9. A buffer
  export skips the `can-write-file` check (nothing is written) but still
  needs the buffer's file, for the title, includes and relative links.
- **The buffer sink**: `*Org HTML Export*`, `*Org MD Export*`,
  `*Org LATEX Export*` (emacs's names) in `html-mode`, `markdown-mode` and
  `text-mode` (lattice has no LaTeX major). A re-export posts a whole-text
  `apply-edit` (`cursor: none`) and then `open-synthetic-buffer`, as EX.0
  found; a killed buffer's stale id is dropped by the host and the open
  seeds a new one. The ids are kept in the plugin's store, so a reload
  still finds the buffers.
- **Departure: one output minor per backend**, not one
  `org-export-output-mode` (design §5 / §9, now updated). The activation
  event names the buffer but not which export it is, and `buffer-text`
  cannot tell them apart at that moment: it reads a buffer only once the
  editor has published that the plugin's mode is on there, and the event
  arrives before that publish (found by the integration test, which
  returned `none` from the activation handler). A hash of the seeded text
  was the first cut and could not be checked for that reason. With
  `org-export-{html,markdown,latex}-output-mode`, the mode's name says
  which buffer it is, with no read and no race.
- **The open sink**: the write, then `open-buffer` (`force: false`) for
  `editor`, or `open-external-uri` for `system`, both posted, both inside
  the plugin's grant, as 0.7.0 allows. `org.export.open-with` is read on
  the command; another value is refused there, naming it.
- **Tests.** `sink.rs`: file / open / system effects in order, a buffer
  sink's first open (name, major, minor) and its in-place replacement
  (range to the true end, no cursor), the minor-to-backend mapping, the
  names. Integration: `h H` opens and focuses `*Org HTML Export*` and
  writes no file, and a second `h H` after an edit to the source replaces
  the same buffer's text (failed while the id was never learned;
  checked); `h o` writes the file and focuses it in lattice.

### EX.7 — Markdown (plugin) ✅

GFM: ATX headings (with `H` and below as lists, as `ox-md`), pipe tables,
footnotes, fenced code with language, task-list checkboxes, autolinks, raw
`#+BEGIN_EXPORT markdown`. Golden tests; tables with inline markup and
pipes in cells.

**As built.**

- **`export/common.rs` first.** What every backend computes the same way
  moved out of `html.rs`: the `Outline` (export levels, section numbers,
  the TOC entries, description-less link labels, `:ID:`s taken before the
  filter), `Footnotes` (numbered by first reference, definitions collected
  wherever they are, written in turn as one references another) and the
  image tests. `html.rs` uses it unchanged in output: every HTML golden is
  byte-identical. LaTeX will take the same pieces.
- **`export/markdown.rs`**, the HTML writer's interface (`start`,
  `headline`, `finish`, `Writer::Markdown`), written as the walk goes.
  Each element is a block, blocks a blank line apart; a list item's or a
  quote's contents are indented or prefixed as a whole.
- **ox-md's shape.** No title heading (as ox-md). ATX headings to
  `min(H, 6)`, prefixed by the section number when numbered; below that, a
  list item with ox-md's four-wide bullet (`1.` from the headline's own
  number, else `-`), nested by the item-headlines actually open above it
  rather than by level, since skipped levels would otherwise indent an item
  with nothing above it, which CommonMark reads as a code block. The TOC is
  a nested list, numbered entries ordered items, as ox-md's.
- **Anchors only where needed.** A heading writes `<a id>` when the TOC or
  some link goes to it, so a document people read as text is not strewn
  with them. A link can come before what it names, so `Targets::collect`
  now records every link path, and `Targets::referred` resolves them once
  before the walk. A `#+NAME`d element is anchored on the same rule;
  `<<targets>>` and radio targets always are.
- **GFM where ox-md uses HTML** (design §1): pipe tables (header from the
  rows before the first rule, an empty header when there is none so the
  first row stays data; extra header rows become body rows; `<l>`/`<c>`/`<r>`
  alignment), `[^n]` footnotes with four-space continuations, `[x]`/`[ ]`
  task items, `~~strike~~`.
- **Escaping.** `` \`*_[]~|$ `` everywhere, `<` and `&` as entities; at a
  line start, what would open a heading, quote, list or setext underline.
  `|` because a cell ends at one; `$` because GitHub reads it as math.
  Entities' UTF-8 is escaped too: `\vert`, newly in the entity table with
  `\vbar` (emacs has both), is how a `|` gets into a cell. Code spans and
  fences are longer than the longest backtick run they hold.
- **Lists in a row.** Two org lists that two blank lines separate are one
  Markdown list unless something stands between them; an empty
  `<!-- -->` does, CommonMark's own answer.
- **Links.** `[desc](url)`; a bare URL or `mailto:` as `<autolink>`; `.org`
  → `.md`; an `id:` link goes to the node's file (another file's Markdown
  has no `ID-` anchor to land on), and with no roam index, the description
  alone. A standalone image is `![caption](path)`, the caption as alt text,
  as ox-md.
- **Raw output**: `export` blocks, snippets and `#+KEYWORD:`s for `md`,
  `markdown` and `html` pass through (every Markdown renderer takes inline
  HTML). Math: `$…$` kept, `\(…\)` → `$…$`, `\[…\]` → `$$…$$`,
  environments as a `$$` block.
- **Departures**: a partial checkbox is the cookie `\[-\]` (GFM's boxes have
  two states; ox-md does the same); a bold descriptive term is not wrapped
  in bold again; the placeholder's Markdown and HTML shells are gone, the
  dump now LaTeX's alone.
- **Tests.** Goldens for the five corpus files (`testdata/*.md`), each
  checked by parsing it as GFM with `pulldown-cmark` (a new dev-dependency)
  so every `#fragment` link lands on an anchor it writes; targeted tests read
  the GFM events, not only the text: heading levels and the items below
  `H:`, nesting under skipped levels (no code block), anchors only where
  linked (fails with `referred` empty; checked), heading prefixes, tables
  (cell count, alignment, `\vert`, code), fences around backticks,
  footnote order and definitions, task markers, escaping (no quote or
  heading from text), links, raw passthrough, math, quote and verse. The
  pipeline's mechanics tests (walk order, steps, subtrees, includes,
  macros, title) moved to the LaTeX dump; **EX.8 removes the dump**, so it
  must give them a backend-neutral oracle first (stepped output equal to
  the same export in one step is the candidate). Integration: the
  `m m` tests assert real Markdown now.

### EX.8 — LaTeX (plugin) ✅

The preamble (design §1), `LATEX_CLASS` (`article`, `report`, `book`),
`LATEX_CLASS_OPTIONS`, `LATEX_HEADER`, sectioning by level with `H` and
below as `itemize`, tables as `tabular` (or `longtable` via
`#+ATTR_LATEX`), `verbatim` / `listings` for source, escaping of the ten
specials, raw `#+BEGIN_EXPORT latex`. Golden tests, plus: every golden
`.tex` compiles with `latexmk` when one is installed (skipped, and saying so,
when not).

**Decided 2026-11-25:** the four tuned knobs, design §1: user classes
(`org.export.latex-classes`), `#+LATEX_HEADER_EXTRA`, `#+LATEX_COMPILER`,
and the source-block backend (`verbatim` / `listings` / `minted`). The rest
of ox-latex's surface is design §8 (packages-alist and hyperref template
⛔, `#+BIND` ❌). Carved in two:

- **EX.8a — the writer** ✅. First, the pipeline's mechanics tests get a
  backend-neutral oracle (the stepped export equals the same export in one
  step), so the dump can go. Then `latex.rs` on `common.rs`: the preamble,
  the three built-in classes and their sectioning, `LATEX_CLASS_OPTIONS`,
  `LATEX_HEADER`, levels below `H` as lists, tables, verbatim source,
  escaping, raw export, footnotes, labels / `\ref` for internal links,
  `\includegraphics` figures, math kept.
- **EX.8b — the knobs** ✅. `org.export.latex-classes`,
  `#+LATEX_HEADER_EXTRA`, `#+LATEX_COMPILER` (compiler-specific default
  packages; `%latex` lands with EX.9's process), the src-block backend
  (`listings` / `minted`), `#+ATTR_LATEX` (`:environment longtable`,
  `:width`). The `latexmk` compile check over the goldens.
  Every LaTeX knob is also an `org.export.latex-*` option (the compiler,
  class, class options, header, classes, src backend, image width), the
  file's keyword overriding it.
- **EX.8c — export defaults as options** ✅ (Dhruva, 2026-11-25: "latex
  compiler etc must all be configurable (and other things for
  org-export)"). The design §3.3 table: the contents options
  (`org.export.with-toc`, `-section-numbers`, `headline-levels`, `author`, …)
  and HTML's (`html-head`, `html-head-extra`). They are the lowest layer of
  `Options::resolve`, under `#+OPTIONS:` and the keywords. Tests: each option changes the
  export, a keyword overrides it, and a value that does not parse is refused naming the
  option.

**EX.8c as built** (plugin `696d487`).

- **`options::ITEMS`**: one row per `#+OPTIONS:` item (name, key, type,
  ox's default, doc), registered and read from the same table. A value goes
  through `Options::option`, the `#+OPTIONS:` parser, so the option and the
  keyword spell values alike. `every_items_default_is_the_default` is the
  drift guard against `Options::default()`.
- **`with-latex`**, not the design's earlier `with-tex`: emacs's variable is
  `org-export-with-latex`. The design table now lists every item, including
  the seven it had left out (`with-timestamps`, `-clocks`, `-properties`,
  `-statistics-cookies`, `-fixed-width`, `-tables`, `-entities`).
- **A bad value stops the export**, naming the option and the values it
  takes. That is checked when the export runs, not at `:set`, for the same
  WIT reason as EX.8b's strings.

**EX.8b as built** (plugin `e70ff9c`).

- **`Configured`.** The plugin's `org.export.*` options are read once per
  request into a base `Options` (and HTML's stylesheet), the lowest layer
  `Options::resolve` starts from. EX.8c adds its options to the same read.
- **Classes** are `LatexClass { name, header, sections }`: the three
  built-ins, then `org.export.latex-classes` entries, a later one of a name
  replacing an earlier. A header is spliced as `org-splice-latex-header`
  (`[DEFAULT-PACKAGES]` / `[PACKAGES]` / `[EXTRA]` and their `[NO-…]`);
  `#+LATEX_CLASS_OPTIONS` replaces the `\documentclass[…]` group.
- **Closed-set strings** (`latex-compiler`, `latex-src-block-backend`) are
  string options checked when the export runs, failing it naming the option.
  WIT's `option-type` has no string enum, so they cannot be refused at
  `:set`; a structured `enum-of` root would be, and is the follow-up if
  that matters.
- **The compile check** (`the_exports_compile`) runs `latexmk -pdf` over
  every golden plus a listings variant. `wrapfig` / `capt-of` are stubbed
  when absent (ox loads them, this writer never uses them); a file needing
  another absent package is skipped, naming it. The dev box lacks `ulem`
  and `listings`, so there it skips; it was run green once with throwaway
  stubs for those two on `TEXINPUTS`. The plugin's CI installs no TeX, so it skips
  there too. A TeX-equipped CI job is the way to make it bite, ⛔ until
  wanted.

**EX.8a as built.**

- **The oracle first.** A `Run` steps by a `budget` (`STEP_BYTES`, or
  any size in tests), and `steps_change_nothing` exports each corpus file
  in steps of one byte, and a 200 KB file in steps of the real budget,
  against the same export in one step, for every backend, and requires the
  same bytes. The mechanics tests (subtrees,
  includes and the cursor, macros, title, body only) assert real Markdown
  or LaTeX. With that, the placeholder went: `Writer::Dump`, the verbatim
  shell and the anchor lines. `parse::dump` is now a test fixture
  (`#[cfg(test)]`), what the parser's and filter's goldens compare.
- **`export/latex.rs`**, the same writer interface as HTML and Markdown,
  on `common.rs`. ox-latex's preamble: the `pdflatex` comment, the class
  with `[11pt]` unless `LATEX_CLASS_OPTIONS` says otherwise, its default
  packages in its order, `LATEX_HEADER` lines, `\author` (with
  `\thanks{email}` under `email:t`), `\date` (`\today` when unset),
  `\title` with the subtitle under `\medskip`, `\hypersetup` (`pdflang`
  from `#+LANGUAGE`), `secnumdepth` for `num:N`, `\maketitle`,
  `\tableofcontents` with `tocdepth` for `toc:N`.
- **Classes.** `article`, `report` and `book`, each with ox's sectioning
  list; an unknown `#+LATEX_CLASS` fails the export naming it. Headlines
  are `\section[short]{full}` (the short title when the full one carries a
  keyword, priority or tags), starred and `\addcontentsline`d when
  unnumbered but listed, each with a `\label`. Below `H:`, or past the
  class's last command, they are `itemize` / `enumerate` items opened by
  the first sibling and closed after the last.
- **Escaping.** The ten specials, `[` braced where LaTeX would read an
  optional argument (line start, after `\\` or `\item`), `...` as
  `\ldots{}`, `--` kept from ligaturing under `-:nil`. Entities: math
  commands in `\(…\)`, text commands, or escaped UTF-8.
- **Elements.** `verbatim` for source (in a `figure` when captioned),
  example, fixed-width and table.el; `tabular` with alignment from the
  cookies or a numeric column, `\hline` per rule, a `table` float with the
  caption above when captioned; images `\includegraphics[width=.9\linewidth]`
  in a `figure` or centred; lists with `\setcounter{enumN}` for `[@n]`,
  checkbox symbols, `\item[{term}]`; verse with `\\`, `\hspace*` and
  stanza gaps; quote, center and special blocks as environments; raw
  `latex` / `tex` keywords, blocks and snippets passed through.
- **Footnotes** at the first reference (`\footnote{…}`), later ones
  `\footnotemark[n]`, so every definition is collected before the body.
- **Links.** `\ref` to a numbered headline or a target with no
  description, `\hyperref[anchor]{…}` with one or to an unnumbered
  headline (its title), `\url` / `\href` outward with `%`, `#` and `\`
  escaped, a cross-file `id:` to the node's file.
- **No step does work in proportion to the file.** The wasm integration
  test of a 4000-headline file trapped once the dump was gone: numbering
  the outline and writing the contents ran in the one call after the parse,
  and at that size they passed the event handler's fuel. So a render is now
  three walks, each a budget a step: the outline (`Outline::begin` /
  `step`, over headline paths, which now live in `common.rs` with the
  render walk), the writer's index pass (`index`: HTML's and Markdown's
  contents, LaTeX's footnote definitions) and the body. Writers are made
  with `new(outline, settings)` once the outline is whole and write their
  head with `begin`. The largest step for that file went from 8 ms to under
  1 ms natively. The whole-tree passes left in the prepare step
  (assembling the tree, the selection, which targets are live, which a
  link refers to) cost about a sixth of what was there; a preamble with no
  headline at all is still one parse chunk and one call, as before.
- **Tests.** Goldens for the five corpus files (`testdata/*.tex`), each
  checked for TeX's grouping (every brace and environment closed, in
  order, `verbatim` read as text) and that every `\ref` / `\hyperref`
  lands on a `\label`. Targeted tests: the preamble, classes and the
  unknown one, starred and listed headlines, item headlines, the headline
  text and its short title, escaping and the braced `[`, links, footnotes
  defined after their reference, tables, lists, blocks with raw LaTeX and
  entities, captioned and bare images. Integration: `l l` writes the
  document. No TeX is installed on the build machine, so nothing compiled
  the goldens; that check is EX.8b's.

### EX.9 — PDF (plugin) ✅

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

**As built** (plugin `bf39bcd`, manifest test `5d3be4b`).

- **`Step::Compile`.** A PDF run renders as `l l` and ends in the compile
  rather than a write; the events seam hands it to `export::pdf`, so `Run`
  has no side effects and `pdf`'s lib tests drive it through a `Runner`
  fake (write, spawn, cancel, log).
- **`write-file`, then spawn.** The pin is the fake compiler: `sh -c` that
  makes `%b.pdf` only when `%f` already ends in `\end{document}`.
- **Placeholders fill arguments, never the program.** The program is the
  granted value, so it is run as written.
- **The log** is `*org-export*`, reset per export, with the headerline
  carrying running / succeeded / failed. `:org-export-log` shows it, which
  the design's "the echo names it" needed: an output buffer is not in
  front of the user until a command opens it.
- **Unconfigured** writes the `.tex` and echoes an example process; nothing
  runs.
- **Cancel** is an event to the events seam (`org/export-cancel`), where the
  runs and the jobs live; it drops runs in flight and cancels compiles,
  whose `job-finished` then says so. A second export of the same file
  cancels the first's compile.
- **Job events are shared with babel**: export subscribes beside it, and
  each ignores the other's ids.
- **`l o` reloads in place.** Opening the PDF a second time reuses its
  buffer (`opening_the_pdf_again_reuses_its_buffer`); pdf-mode's PF.5
  reload shows the new pages.
- **Found:** `org_manifest.rs` had been asserting babel's grants alone since
  EX.0 added export's write grant and gate; fixed with EX.9's spawn grant.

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
