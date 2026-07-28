# Help docs — mode-aligned naming and full coverage

**Status:** in progress. Design fragment: none — this is a naming +
coverage convention, recorded here and enforced by tests in
`crates/lattice-help/src/topics.rs`.

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
| HD.4 | Docs for the family modes (19 language, 6 display) + internals | 📝 |
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

### HD.4 — family + internal modes 📝

Unblocked by HD.5. 33 pages: 19 language majors, 6 display minors, 8
internals (`help-mode`, `completion-mode`, `completion-popup-mode`,
`buffer-words-mode`, `path-completion-mode`,
`tree-sitter-completion-mode`, `lsp-completion-mode`, `preview-mode`).

The 19 language pages differ only in grammar and a few settings, so
they should share a template rather than 19 hand-written variants;
`languages.md` and `display.md` become index pages pointing at their
family members.

#### Scope once unblocked

19 language majors and 6 display minors get standalone docs;
`languages.md` and `display.md` become index pages pointing at them.
Internal plumbing modes (`preview-mode`, `completion-popup-mode`,
`path-completion-mode`, `buffer-words-mode`,
`tree-sitter-completion-mode`, `snippet-completion-mode`,
`lsp-completion-mode`) get short docs saying what they are and what
they exist for, so `:help <mode-id>` is never dead.

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
