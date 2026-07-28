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
| HD.2 | Split `magit-buffers.md` into per-mode docs | 📝 |
| HD.3 | Docs for the ~22 user-facing modes with no coverage | 📝 |
| HD.4 | Docs for the family modes (19 language, 6 display) + internals | 📝 |

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

### HD.2 — magit per-mode docs 📝

`magit-buffers.md` covers 10 modes in one page. Split into
`magit-commit-mode.md`, `magit-revision-mode.md`,
`magit-file-revision-mode.md`, `magit-diff-mode.md`,
`magit-log-mode.md`, `magit-blame-mode.md`, `magit-stash-mode.md`,
`magit-stash-show-mode.md`, `magit-branch-mode.md`,
`magit-rebase-mode.md`. `magit.md` stays the subsystem umbrella;
`magit-transient.md` stays (the dispatch menus are not a mode).
`magit-core-mode` and `magit-global-mode` need docs too — their chord
surfaces are currently described inside `magit.md`.

### HD.3 — the undocumented user-facing modes 📝

`dashboard-mode`, `messages-mode`, `repl-mode`, `hover-mode`,
`problems-minor-mode`, `diff-conflict-mode`, `terminal-normal-mode`,
`terminal-insert-mode`, `snippet-mode`, `active-snippet-mode`,
`ai-conversation-mode`, `ai-permission-mode`, `ai-log-mode`,
`pi-mode`, `lsp-log-mode`, `lsp-server-log-mode`,
`lsp-trace-log-mode`, `text-mode`, `search-line-mode`,
`prompt-line-mode`, `command-line-expand-mode`, `plugins-mode`.

### HD.4 — family + internal modes 📝

19 language majors and 6 display minors get standalone docs;
`languages.md` and `display.md` become index pages pointing at them.
Internal plumbing modes (`preview-mode`, `completion-popup-mode`,
`path-completion-mode`, `buffer-words-mode`,
`tree-sitter-completion-mode`, `snippet-completion-mode`,
`lsp-completion-mode`) get short docs saying what they are and what
they exist for, so `:help <mode-id>` is never dead.
