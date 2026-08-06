# Help docs — mode-aligned naming and full coverage

**Status:** in progress — HD.1–HD.5 ✅, HD.6 📝, HP.1/HP.2 ✅. Design
fragment: none — this is a naming + coverage convention, recorded here
and enforced by tests in `crates/lattice-help/src/topics.rs`. The HP
slices at the end cover how a page *renders* rather than which pages
exist.

## Why

A mode already answers to its id on two surfaces: `:<mode-id>` (the
auto-generated toggle) and `:describe-mode <mode-id>`. Help was the
only surface that used a different name, so one subject took three
spellings:

```
:compilation-mode                  toggle
:describe-mode compilation-mode    introspection
:help compilation                  ← prose docs
```

Two docs already used the mode-id form (`emacs-keys-mode.md`,
`lsp-mode.md`), so the convention existed and was applied unevenly.

## The rule

**A doc that is one mode's user surface is named after that mode.** Its
topic name, its `H1`, and the mode id are the same string.

- Topic name is the file stem (`lattice-help/build.rs`), so the rename
  IS a `git mv`.
- A doc covering a *subsystem* keeps the subsystem's name — `lsp.md`,
  `magit.md`, `plugins.md`, `completion.md`, `help.md`. These are not
  mode docs; naming them after one mode inside them would mislabel the
  doc and leave that mode undocumented.
- A doc covering *two peer modes* is split (`filetree-oil.md` →
  `file-tree-mode.md` + `oil-mode.md`).
- Every mode lattice ships gets a doc — see S3/S4.

## Audit (2026-07-28)

83 production modes; 14 had a prose doc.

| Group | Count | Coverage before |
|---|---|---|
| Language majors (`rust-mode`, `yaml-mode`, …) | 19 | `languages.md`, collectively |
| Display minors (`wrap-mode`, `line-numbers-mode`, …) | 6 | `display.md`, collectively |
| Magit | 13 | `magit-status` + `magit-buffers.md` covering 10 |
| LSP / completion / snippet families | 13 | `lsp.md`, `completion.md`, collectively |
| Internal plumbing (`preview-mode`, `prompt-line-mode`, …) | ~10 | `:describe-mode` only |
| User-facing, no doc at all | ~22 | none |

**Second finding, unrelated to naming:** 210 of the 269 cross-doc links
in `docs/user/` were dead inside `:help`. Links written
`](compilation.md)` fall through `classify_link_url` to
`HelpLinkTarget::Unresolved`, so `<CR>` echoed ``no handler for
`compilation.md` ``. They render fine on GitHub, which is why it went
unnoticed. Only the 59 `](help:topic)` links worked.

## Slices

| Slice | Description | Status |
|---|---|---|
| HD.1 | Mode-aligned renames, link rewrite, guard tests, describe-mode ↔ help cross-link | ✅ |
| HD.2 | Split `magit-buffers.md` into per-mode docs | ✅ |
| HD.3 | Docs for the user-facing modes with no coverage | ✅ |
| HD.4 | Docs for the family modes (19 language, 6 display) + internals | ✅ |
| HD.5 | Compress the embedded docs (unblocks HD.4) | ✅ |
| HD.6 | Runtime doc directory + plugin-contributed topics | 📝 |

### HD.1 — naming + links ✅ (2026-07-28)

- **Mode-id renames** (the mode was the odd one out, not the doc):
  `narrow-minor-mode` → `narrow-mode`,
  `project-search-multibuffer-mode` → `project-search-mode`. Most
  minors don't carry `minor` in the id (`surround-mode`,
  `auto-pair-mode`, `hover-mode`), so these two were the outliers. Rust
  types renamed to match (`NarrowMode`, `ProjectSearchMode`) — a struct
  whose `mode_id()` disagrees with its own name is a trap for the next
  reader.
- **Doc renames** (11): compilation, surround, tutor, claude-code,
  opencode, multibuffer, magit-status, diff, command-line, terminal,
  project-search → `*-mode`. `H1`s retitled to match.
- **Split:** `filetree-oil.md` → `file-tree-mode.md` + `oil-mode.md`,
  each cross-linking the other as the read-only / writable peer.
- **Links:** 176 sibling `.md` links rewritten to `help:` form. Links
  to `../dev/**` are deliberately left as plain markdown — those are
  developer docs, not help topics, so a `help:` link would point at
  nothing; they stay resolvable on disk and on GitHub.
- **Anchors:** `help:topic#anchor` now works.
  `do_open_help_topic` splits the fragment and seeds the initial
  scroll. Without this the rewrite would have turned 26
  working-on-GitHub anchored links into topic-lookup misses.
- **`:describe-mode` → `:help` cross-link:** the describe view now
  emits `See also: [<mode>](help:<mode>)` when a topic named after the
  mode exists. The two halves answer different questions —
  introspection (live keymap, options, capabilities) versus prose
  (what it's for, worked examples) — and neither replaces the other,
  but arriving at one without knowing the other exists was the common
  failure. Aligning the names is what makes the link a registry hit.
- **Index:** `README.md` rows now show the name you type
  (`` [`compilation-mode`](help:compilation-mode) ``) instead of a
  filename. `surround-mode` and `terminal-mode` were missing from the
  index entirely — added.
- **Tests** (`crates/lattice-help/src/topics.rs`): every `help:` link
  resolves to a registered topic; no sibling `.md` links remain; the
  index lists every topic. All three scan with code spans and fenced
  blocks stripped, so a doc may still *show* link syntax as an example
  (`buffers.md` documents the index format with a literal
  `` `[name](help:name)` ``) without tripping them.

**No aliases.** `:help diff` is now an error rather than resolving to
`diff-mode`. Chosen deliberately: one name per topic, nothing to keep
in sync. Every in-repo reference was updated in the same pass; notes
outside the repo will break.

### HD.2 — magit per-mode docs ✅ (2026-07-28)

`magit-buffers.md` covered 10 modes in one page. Retired; each mode now
has its own topic, so `:help magit-log-mode` reaches the log buffer's
page directly instead of an anchor part-way down a 400-line document.

12 new docs: `magit-commit-mode`, `magit-revision-mode`,
`magit-file-revision-mode`, `magit-diff-mode`, `magit-log-mode`,
`magit-blame-mode`, `magit-stash-mode`, `magit-stash-show-mode`,
`magit-branch-mode`, `magit-rebase-mode`, plus `magit-core-mode` and
`magit-global-mode` — the two shared modes, whose chord surfaces were
previously buried inside `magit.md`.

`magit.md` stays the subsystem umbrella and gains the cross-cutting
Headerline table (one row per view, each linking its mode's page); its
duplicated magit-core chord table is now a pointer to
`magit-core-mode`. `magit-transient.md` stays as-is — the dispatch
menus are not a mode.

**Stale claims corrected against source while splitting** (the split
forced a read of every mode's real keymap, which is how these
surfaced):

- Branch `d` was documented as deleting "without confirmation, no
  confirmation dialog, no destructive-action warning". MG.12 gave it
  an `Effect::Confirm` two-step; the doc had never caught up.
- `magit.log.count` / `.graph` / `.decorate` and
  `magit.blame.author-width` / `.date-format` were documented as the
  configuration lever for those views. `lattice-magit` registers **no
  options at all** — `:set` on any of them fails with `unknown
  option`. `magit.md` already said so in its Options section, so the
  page contradicted itself; the per-mode pages now state the values
  are hardcoded and point at that section.

**Test added:** `every_anchored_help_link_names_a_heading_that_exists`.
`do_open_help_topic` treats a missing anchor as "open the topic
unscrolled" rather than erroring — correct behaviour (the page is
still the right answer), but it means a renamed heading silently
degrades every link pointing at it with nothing reporting it at
runtime. Verified non-vacuous by injecting a broken anchor.

### HD.3 — the undocumented user-facing modes ✅ (2026-07-28)

23 new docs. Mode coverage goes from 14/83 to **50/83**; everything
left is HD.4's scope (19 language majors, 6 display minors, 8
internals).

| Area | Docs |
|---|---|
| Launch / logs | `dashboard-mode`, `messages-mode`, `plugins-mode` |
| Terminal | `terminal-normal-mode`, `terminal-insert-mode`, `repl-mode` |
| Minibuffer | `search-line-mode`, `prompt-line-mode`, `command-line-expand-mode` |
| Snippets | `snippet-mode`, `snippet-completion-mode`, `active-snippet-mode` |
| LSP logs | `lsp-log-mode`, `lsp-server-log-mode`, `lsp-trace-log-mode`, `hover-mode` |
| AI | `ai-conversation-mode`, `ai-permission-mode`, `ai-log-mode`, `pi-mode` |
| Other | `text-mode`, `problems-minor-mode`, `diff-conflict-mode` |

`snippet-completion-mode` was pulled forward from HD.4: the two other
snippet docs link to it, and a forward reference would have been a
dangling link.

**Every page written against source**, not against the old prose —
each mode's real keymap, options, and module docs were read first.
Two things that surfaced and are documented as they actually are:

- `diff-conflict-mode` is a **marker shell**. It activates on the
  right buffers, but the resolution chords (keep-ours / keep-theirs /
  keep-both / next-conflict) and the conflict gutter do not exist. The
  page says so at the top rather than describing an smerge surface
  that isn't there.
- `problems-minor-mode` has no chords of its own — `q`-to-close is a
  tracked follow-up — so the page points at `multibuffer-mode` for
  what you can actually do in the buffer.

**The guard tests earned their keep immediately:** the link test
caught two forward references (`markdown-mode`, `snippet-completion-mode`)
in the first drafts, and the index test enumerated all 23 missing
index rows rather than leaving them to be spotted by eye.

### HD.4 — family + internal modes ✅ (2026-07-29)

33 docs. **Mode coverage is now 83/83.**

- **19 language majors** — generated from a fact table (grammar crate,
  extensions, which queries exist) rather than hand-written 19 times,
  because they genuinely differ only in those fields. Per-language
  notes where reality diverges: `.h` maps to `c-mode` not `cpp-mode`
  and why, `sql-mode`'s permissive multi-dialect grammar accepting more
  than any single engine, `tsx-mode` sharing TypeScript's query files.
  `markdown-mode` is hand-written — it is the only bundled language
  whose highlight query lattice writes itself, it backs every help
  buffer as well as `.md` files, and it is missing symbols + text
  objects that the other eighteen have.
- **6 display minors** — each documents its option, `:set` surface, and
  default. `whitespace-show-mode` and `current-line-highlight-mode` say
  plainly that the option cascades but **the renderer pipeline has not
  landed**, so toggling changes state without changing the screen.
- **8 internals** — `help-mode`, the completion gate/popup pair, the
  four completion sources, `preview-mode`.

`languages.md` and `display.md` became index pages: the language table
links each row to its mode page, and display gained a mode/option
table, while both keep the cross-cutting material that has no single
mode (`tabstop`, `scrolloff`, the coverage roadmap).

**Every page states its options and keybindings explicitly, including
when the answer is "none".** A language major contributing no options
is worth saying: it tells you there is no per-language settings
mechanism, which is a real gap rather than an omission from the page.

## Open design item — a mode for inline diff hunks

Raised 2026-07-29. There is no mode owning the **inline diff** surface.
`diff-mode` is the two-way session minor (`do` / `dp` / `]c` / `[c`);
inline hunks are rendered by `lattice-diff`'s overlay with no mode at
all, their colours are `diff.*` theme **builtins**, magit styles its
own inline expansions through `highlight::diff_styled_spans`, and the
single registered option (`git.auto-head-diff`) sits in the host's
`git.*` group.

A dedicated mode would own: syntax highlighting *within* hunk content
(today ± lines get one flat colour, not real highlighting), context
lines, sign glyphs, auto-expand behaviour, and the `diff.*` elements —
per the mode-ownership rule. Overlaps MG.18 (hunk staging) and MG.19
(side-by-side), so worth sequencing with them.

### HD.5 — compressed embed ✅ (2026-07-29)

HD.3 left 14 KB of headroom under a 512 KB raw-markdown budget, with 33
HD.4 pages queued. The budget doc forbids raising the number, so the
embedding model changed instead.

Docs are now deflate-compressed at build time and inflated on first
open into a `OnceLock` cache. **495 KB raw → 197 KB embedded (2.5×)**;
the budget is now 384 KB *compressed*, which allows roughly 960 KB of
raw markdown — about double the current set.

- The budget test measures **embedded** bytes now. Raw size stopped
  being the cost the moment compression landed; continuing to measure
  it would fire the alarm on volume the binary never pays for.
- Two new guards: `embedded_bodies_are_actually_compressed` (a
  regression embedding bodies raw would otherwise slip under a
  compressed-bytes budget while the binary grew) and
  `every_embedded_topic_decompresses_to_its_original_length`.
- `miniz_oxide` was already in the tree transitively (via flate2 and
  zstd), so the direct edge adds no new distinct dependency.
- Bench `crates/lattice-help/benches/topics.rs`: boot 17.8 µs and
  decompresses nothing; first open of the largest topic 66.6 µs;
  cached open 562 ns.

**The budget doc predicted ~5× and got 2.5×** — each doc deflates
independently and at ~7 KB the window barely warms up. Corrected there
rather than left as folklore. A shared dictionary would compress
better but would mean inflating the whole corpus to read one topic,
which costs the laziness.

### HD.6 — runtime doc directory 📝

The deferred half of the 2026-07-29 distribution decision. Compression
bought room; it did not create the seam that matters:
`Editor::help_topics` is a plain `Arc<HelpTopicRegistry>` built once at
boot, **so a plugin cannot ship a `:help` page at all**. Given
plugin-first extensibility is paramount goal #2, that closes eventually
regardless of size.

Shape, resolution order, the survey of how Vim / Helix / Kakoune / Zed
handle it, and why the embedded set stays as a floor (`cargo install`
and scp'd binaries have no runtime dir, and unlike Helix a missing docs
dir fails quietly) are all recorded in
[`../embedded-docs-budget.md`](../embedded-docs-budget.md).

Scoped docs-only but named for growth, so `runtime/themes/` and
`runtime/queries/` can move later without a second migration.

---

## HP — how a help page *renders*

HD.1–HD.6 are about which pages exist and what they are called. HP is
about what a page looks like once opened, and both slices started from
the same finding: **a help buffer was showing markdown source.**

### HP.1 — tables line up ✅ (2026-08-06)

Nothing between a `.md` file and a help buffer touched tables, so
`|---|---|` rendered literally and cells had unrelated widths. Where a
doc *had* been hand-padded it was padded by `char` count — a different
number from the columns a terminal advances — so `✓`, `─`, `↑` and CJK
made a table that looked aligned in the source render ragged on screen.

`unicode_width` measures what the terminal will do. The guard uses a
fixture where char-count and display-width **disagree** and asserts
that too: a fixture where they happen to match cannot detect the bug it
exists for.

**Ordering is the load-bearing part.** Alignment runs BEFORE
`extract_links_and_clean`, never after — link ranges are byte offsets
into the cleaned text, so padding applied afterwards slides every link
on a padded row and `<CR>` opens the wrong page, silently, with the
link still highlighting and still looking live.

That guard took two attempts, and the first one is the lesson: padding
lands *after* a cell's text, so a link only moves if a cell **before**
it grows. The first fixture put the link on the widest row, where
nothing shifts, and **passed against the deliberately mis-ordered
build**. The rewritten fixture fails with `"    "` where the label
should be. Mutation-testing the guard is what caught it — the test was
written, looked reasonable, and proved nothing.

**Placement:** `lattice-mode/src/modes/table/layout.rs`, not
`lattice-help`, because the next consumer is an org-table-style
`table-mode` in that directory. Both need the same parse-measure-pad
core and differ only in *when* they run it — help once at content-build,
table-mode on every edit. A mode reaching into the help crate for it,
or growing a second copy, is the duplication this placement avoids.
`lattice-help -> lattice-mode` is cycle-free.

### HP.2 — inline literals say what they are ✅ (2026-08-06)

The markdown **block** grammar has no `code_span` node — that lives in
the inline grammar, which is not wired up — so `` `gr` ``,
`` `:magit-status` `` and `` `action:magit-refresh` `` had **no style at
all** and rendered as prose with visible backticks. That is most of
what "help pages aren't formatted nicely" was.

Four `Style` variants rather than one, on the `Magit*` precedent:
reusing `Keyword` or `Link` would name an unrelated source-code concept
and tie help's palette to the code palette. A reader scanning a help
page is hunting one thing — *which key do I press* — and a single
literal colour cannot separate that from *which command do I type*.

**The chord test is the keymap, not a shape heuristic.**
`parse_chord_sequence` is useless alone as a discriminator: it accepts
`.gitignore` as ten char-chords. A literal is a key when the live
keymap **binds** it, which also gets mode-contributed chords right.
Angle-bracket notation short-circuits the lookup, because a page
routinely documents chords for a mode that is not active while you read
about it.

**Split across two crates on purpose.** `lattice-help` finds the spans
(pure text); `lattice-host` classifies them (needs the live keymap).
Same division as `link_highlights`, and it avoided touching ~123
`from_lines` call sites.

**Renderer parity:** none needed, verified rather than assumed. No new
`Effect` / `DiffSignKind` / `host_theme.*` surface; zero exhaustive
`Style` matches in either renderer; both resolve through the shared
`resolve_syntax_style`, so TUI and GPUI pick the colours up together.

**Known residual.** A *bare alphabetic* mode-contributed chord
(magit's `gr`) is recognised only while that mode's keymap layer is
pushed — `:help magit-core-mode` styles it from inside a magit buffer
and not from an ordinary file. Angle-bracket chords are unaffected.
Closing it means classifying against the full contribution registry
rather than the pushed-layer set; documented in `help.md` rather than
left for a user to discover.
