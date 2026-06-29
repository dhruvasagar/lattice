# Multibuffer Providers — Slice Catalogue

> **Status: Active.** Extracted 2026-06-10 from the archived M-series slice
> plan. The completed M.0–M.10 + M.V slices remain in the archive for
> historical reference; this file owns the pending provider work.

Each entry below is a provider submodule in
`crates/lattice-multibuffer/src/providers/<name>.rs`, gated behind a cargo
feature. The repeated shape (locked 2026-06-01 in architecture §3.7 "Provider
model"): per-provider service trait + impl + handle, per-provider minor mode,
public trigger function, typed events, `register_<provider>` boot helper. M.6
lands `lattice-multibuffer::providers::search` as the worked example; every
entry below follows the same template.

**Narrow mode (N.1)** is handled separately — it's a first-class primitive, not
a `MultibufferProvider`, and has its own design fragment and slice plan:
- Design: [`docs/dev/architecture/narrow-mode.md`](../../architecture/narrow-mode.md)
- Slice plan: [`narrow-mode.md`](narrow-mode.md)

---

## Provider model — locked conventions

**All in-tree providers ship within `lattice-multibuffer`** (not as standalone
crates). Each provider adds its own cargo feature pulling its data-source
dependencies:
- `search` → `ignore`/`fancy-regex` (**shipped M.6**)
- `lsp-references` → `lattice-lsp`
- `diff` → `lattice-diff` (after the diff subsystem extraction)
- `diagnostics` → `lattice-diagnostics` (after that extraction)
- `ai-edits` → AI integration crate when it lands

Default-features ships all providers; opt-out builds disable per feature.

**Dep-direction constraint:** `lattice-host` already depends on
`lattice-multibuffer`, so `lattice-multibuffer` MUST NOT depend on
`lattice-host`. Any provider whose data source currently lives in
`lattice-host` (diff, diagnostics, host-side LSP fan-in) requires extracting
the data source to its own crate first.

**Universal prerequisites** (assume for every entry unless noted):
M.2.b.2 (`ModeActivator` + `MultibufferRegistry` + `create_multibuffer_view`),
M.3 (edit propagation — for editable providers), M.4 (live source updates +
headerline status API), M.7 / M.8 (excerpt + file folds — recommended for any
N≥10-excerpt view). All ✅ as of 2026-06-08.

---

## Priority labels

- **A** — daily-driver workflows; high impact, high frequency.
- **B** — power features / plugin foundations; specialised but unlock
  workflows lattice will be defined by (magit-style VCS, LSP-driven refactors,
  AI multi-file edits).
- **C** — speculative / experimental; useful once the v1 core is stable.

---

## Priority A — daily-driver workflows

### A.1 `ProjectDiffProvider` 📝

Composes every changed file in the working tree (or a comparison range) into
one multibuffer. One excerpt per hunk; file-boundary folds (M.8) group hunks
per file so a 50-file diff collapses to a 50-row outline at `:set foldlevel=0`.

- Extra deps: **D.7** (VCS subsystem, deferred) for the hunk source-of-truth;
  falls back to a `git diff`-shell-out implementation in the interim.
- User surface: `:project-diff` (working tree vs HEAD), `:project-diff staged`,
  `:project-diff <rev1>..<rev2>`, `:project-diff branch <name>`.
- Edit propagation: hunk edits write back to the working-tree file via the
  standard apply_edit path; staged-diff edits rewrite the index entry
  (post-D.7).
- Tests: 10-file diff → expected excerpt count; edit a hunk excerpt →
  working-tree file updates and the hunk recomputes; closing a source file
  auto-removes its excerpts; foldlevel=0 shows one row per file.
- Compose: A.1 is the canonical AI-multi-file-diff base — A.2 reuses A.1's
  rendering with a different edit-commit path.

### A.2 `AIProposedEditsProvider` 📝

Renders an LSP-shape `WorkspaceEdit` from an AI plugin as a multibuffer with
per-excerpt accept/reject. Solves the "AI proposed changes to 14 files —
review each, accept most, reject some" workflow.

- Extra deps: AI plugin API (post-v1 — uses the WIT plugin Resource shape
  `Arc<dyn Document>`).
- User surface: AI plugin calls `host.show_workspace_edit(edits,
  on_commit_callback)` → multibuffer opens; user navigates excerpts via
  `]e` / `[e`; per-excerpt `:accept-excerpt` / `:reject-excerpt`;
  bulk `:accept-all` / `:reject-all`; `:commit` invokes the callback.
- Tests: synthesize a 5-file `WorkspaceEdit`; accept 3, reject 2; verify only
  accepted edits land in source files; callback receives committed-edits subset.
- Compose: shares file-boundary-fold rendering with A.1; per-excerpt
  accept/reject UI generalises into A.8.

### A.3 `LspReferencesProvider` 📝

`gr` (find references) / `:lsp-references` opens every reference site as a
multibuffer. Each excerpt shows the call-site with configurable surrounding
context (default ±2 lines). Edit-in-place propagates to source files for
cross-callsite refactors.

- Extra deps: LSP subsystem (already in lattice).
- User surface: `gr` keymap, `:lsp-references [<symbol>]` ex-command.
- Edit propagation: standard via M.3; per-source LSP version match enforced —
  if a source-file version changed mid-edit, surface a warning before applying.
- Tests: mock LSP returns 8 reference locations across 3 files → 8 excerpts;
  edit one → source file updates; version-mismatch path prompts user.

### A.4 `DiagnosticsProvider` 📝

All LSP diagnostics across the workspace as one multibuffer. Header per excerpt
encodes severity (Error/Warning/Info/Hint), file:line, and the message; the
excerpt body shows the offending line + context.

- Extra deps: LSP subsystem.
- User surface: `:diagnostics` (all severities), `:diagnostics warning`
  (filter), `:diagnostics file <path>` (single-file scope); `]d` / `[d`
  motions for next/prev diagnostic inside the multibuffer.
- Edit propagation: fixing the offending line updates the source; LSP re-runs
  on the next debounce tick; diagnostic excerpts auto-remove when cleared.
- Tests: 12 diagnostics across 4 files → 12 excerpts; filter by severity;
  edit clears matching diagnostic after next LSP republish.

### A.6 `QuickfixProvider` 📝

The canonical refactor surface for compiler errors / test failures / lint
warnings. Replaces vim's traditionally read-only quickfix list with an editable
multibuffer. The compile → multibuffer → fix-in-place → recompile loop
collapses into one surface.

- Extra deps: none beyond M-series.
- User surface: `:copen` opens the active quickfix list as a multibuffer;
  `:make` populates it from compiler output; `:cgetexpr <cmd>` from arbitrary
  tool output; `:cnext` / `:cprev` navigate between excerpts; `:lopen` for
  the location-list variant.
- Edit propagation: standard via M.3; source file updated; quickfix entry
  auto-recomputes on next populate tick.
- Tests: 6 compiler-error entries across 3 files → 6 excerpts; edit excerpt
  → source file changes; `:cnext` jumps to next excerpt's cursor position.
- Compose: A.6 is the bridge between vim's quickfix grammar and the multibuffer
  abstraction — A.3, A.4, A.10, A.11, A.12 can optionally route through
  quickfix for uniform navigation.

### A.15 `OutlineBufferProvider` 📝

Symbol outline of the current file (or N files, or a directory) as a
multibuffer. Each symbol is an excerpt with its signature line and (optionally)
its body. Folds via M.7 collapse symbols to one-line summaries, giving a
navigable cross-file outline.

- Extra deps: tree-sitter outline OR LSP `textDocument/documentSymbol`.
- User surface: `:outline` (active file), `:outline-workspace`,
  `:outline-dir <path>`; `]s` / `[s` motions for next/prev symbol within the
  outline; standard `z*` folding.
- Edit propagation: editing a symbol's body propagates to the source file.
- Tests: file with 10 functions → 10 excerpts; toggle fold → outline collapses;
  cross-file outline of 5-file module composes correctly.

---

## Priority B — power features + plugin foundations

### A.7 `GitHunksProvider` 📝

magit-style cross-file VCS workflow. All staged hunks (or working-tree hunks,
or both) across the workspace as one editable multibuffer. Per-excerpt stage /
unstage / discard actions.

- Extra deps: **D.7** (VCS subsystem) + the magit-style plugin layer.
- User surface: `:Gstaged`, `:Gunstaged`, `:Gdiff <rev>`. Per-excerpt: `s`
  stages from working tree, `u` unstages, `x` discards. `:Gcommit` opens
  the commit-message editor with staged multibuffer's content shown for
  reference.
- Edit propagation: working-tree hunks via apply_edit; staged hunks rewrite
  the index via lattice-vcs.
- Tests: stage 4 hunks across 2 files → `:Gstaged` shows 4 excerpts; unstage
  one → excerpt disappears, reappears in `:Gunstaged`.

### A.8 `LspRefactorPreviewProvider` 📝

`<F2>` / `:lsp-rename Foo` returns a `WorkspaceEdit`; this provider renders it
as a multibuffer with per-excerpt accept/reject before commit. Gives a review
step before a rename is applied.

- Extra deps: LSP subsystem.
- User surface: rename keymap + `:lsp-rename <new-name>`. Per-excerpt
  `:accept` / `:reject`; `:accept-all`; `:cancel`.
- Edit propagation: only at commit time. Excerpts are read-only previews.
- Tests: 15 call-sites of `foo` renaming to `bar`; reject 2 → 13 commits
  land; cancel discards everything.
- Compose: shares per-excerpt accept/reject UI with A.2.

### A.9 `LspWorkspaceSymbolsProvider` 📝

`:WorkspaceSymbols Foo` opens every definition + key call-site of the symbol
as a multibuffer. The canonical cross-codebase signature-refactor surface.

- Extra deps: LSP subsystem (`workspace/symbol` request).
- User surface: `:WorkspaceSymbols <query>` ex-command; workspace-symbol
  picker (`gO`) optionally opens result in a multibuffer.
- Edit propagation: standard.
- Tests: workspace with 3 definitions of `Foo` + 8 callers → 11 excerpts.

---

## Priority C — specialised / experimental

### A.10 `LspCallHierarchyProvider` 📝

Incoming / outgoing calls of a function as a multibuffer.

- Extra deps: LSP `callHierarchy/*` requests.
- User surface: `:call-hierarchy incoming` / `outgoing`; navigate with
  `]C` / `[C`.

### A.11 `LspTypeHierarchyProvider` 📝

Supertypes / subtypes / trait impls as a multibuffer.

- Extra deps: LSP `typeHierarchy/*` requests.
- User surface: `:type-hierarchy super` / `sub` / `impls`.

### A.12 `TodoProvider` 📝

Workspace-wide TODO/FIXME/XXX/HACK scan as an editable multibuffer.

- Extra deps: none.
- User surface: `:todos`, `:fixmes`, `:scan <pattern>`; option
  `multibuffer.todo_patterns` for user-configured tags.
- Tests: 8 TODOs across 4 files → 8 excerpts; edit TODO out of existence →
  excerpt auto-removes on next scan.

### A.13 `MergeConflictTriageProvider` 📝

Every conflict marker in the working tree as one multibuffer.

- Extra deps: none — D.6 conflict-marker recognition already shipped.
- User surface: `:conflicts`; `:resolve-all-ours` / `:resolve-all-theirs`.

### A.14 `PRReviewProvider` 📝

Read-only multibuffer of a pull request's diff with inline comment threads as
excerpt decorations.

- Extra deps: magit-style plugin API (post-v1) + GitHub/GitLab API integration.
- User surface: `:gh-pr <number>`; navigate hunks with `]h` / `[h`; comment
  with `:pr-comment`.

### A.16 `CompilationOutputProvider` 📝

`cargo build` / `cargo test` output, parsed and rendered as a live-updating
multibuffer. One excerpt per compiler error / test failure.

- Extra deps: process-launcher subsystem.
- Compose: largely redundant with A.6 (quickfix); ship only if cargo-specific
  decorations (test status icons, timing) add enough value.

### A.17 `NotebookProvider` 📝

Markdown cells + code cells from N source files composed into a notebook view.

- Extra deps: rich-buffer rendering primitives (post-v1).
- User surface: `:notebook <glob>`.

### A.18 `ReplTranscriptProvider` 📝

REPL session transcript as a multibuffer. Each input/output pair an excerpt.

- Extra deps: REPL subsystem (post-v1).

### A.19 `HelpCrossReferenceProvider` 📝

`:help <topic>` opens a multibuffer with relevant `:help` sections side-by-side.

- Extra deps: `:help` subsystem (already in lattice).

### A.20 `ProjectConfigProvider` 📝

`.gitignore` + `.editorconfig` + `Cargo.toml` (or config-set per project type)
composed for cross-file consistency edits.

- Extra deps: none.
- User surface: `:project-config`.

### A.21 `LogTailProvider` 📝

`tail -F` over N log files composed and live-updating, with
timestamp-decorated headers.

- Extra deps: file-watcher subsystem (already in lattice).
- User surface: `:tail <file> [<file> ...]`.

---

## Sequencing notes

- **A.5 NarrowProvider** is now **N.1** — see the dedicated slice plan.
- **A.1 / A.6 / A.4 / A.3** are the four most-touched daily-driver workflows;
  ordering between them is preference-driven. Recommend: **A.6 first** (the
  most general — unlocks A.3 / A.4 / A.10 / A.11 / A.12 to optionally route
  through quickfix grammar).
- **A.2 / A.7 / A.8** unlock magit and AI-driven workflows; they have plugin /
  subsystem dependencies that push them past v1.0.
- **C-tier** entries are deliberately under-specified — pick up when a user
  need surfaces or when they enable a follow-on subsystem.
- Per-PR scope discipline: one A.x slice = one `MultibufferProvider` impl +
  one ex-command + tests. Sibling affordances that get reused across slices
  land in their own slice the first time two consumers need them.
