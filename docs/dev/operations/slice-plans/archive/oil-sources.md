# Oil listing sources — slice plan (LS)

> **ARCHIVED 2026-11-03.** LS.1–LS.5 complete, verified against source
> and tests before filing. The design fragment is
> `docs/dev/architecture/directory-listing-mode.md` §11 (the *what* and
> *why*); this file is the *when* and *in what order*.
>
> **Naming.** The five commits carry `OR.1`–`OR.4b` in their messages:
> the prefix was chosen before noticing that `OR` already names org-roam
> (`implementation.md`, 2026-08-29). Code and docs were renamed to `LS`
> in LS.5; the commit messages cannot be, so read `OR.n` there as `LS.n`.

Asked for 2026-11-03: "when we checkout a file at revision, I would like
`-` that opens oil to remain at that revision and show files and folders
at that revision, vim-fugitive does this beautifully". Option A of the
two presented: oil gains a pluggable listing source with the filesystem
as the default, and magit supplies a read-only git-tree source.

## Status

| Slice | Title | Status | Commit |
|---|---|---|---|
| LS.1 | `OilSource` seam; oil lists, re-lists and names through the buffer's source; no behaviour change | ✅ | `91db2526` |
| LS.2 | A read-only source refuses edits (`read-only-mode`) and `:w` | ✅ | `36598f26` |
| LS.3 | `lattice_vcs::Tree` — a directory at a revision, or in the index; bench | ✅ | `c7aabfc2` |
| LS.4a | `OilSourceProvider` registry; `-` asks it; `Effect::OpenOilSource` + `Effect::InTarget`; `-` at the top says so | ✅ | `3522ab50` |
| LS.4b | magit's `GitTreeSource` + `MagitOilOrigins`; end-to-end walk | ✅ | `b8c63b6c` |
| LS.5 | Docs: design §11, magit.md, crate overview, this plan, the benchmark row | ✅ | this commit |

## How the plan moved while it ran

- **The revision copy carries no new state.** The plan had magit set a
  listing-owned buffer-local on the file-at-revision buffer. Modes
  cannot write buffer-locals from an action handler, and the buffer's
  name already carries repository, revision and path — so the provider
  reads the name (LS.4a), the `BufferScopeSource` shape.
- **`-` stays oil's.** Binding `-` on magit's file-revision major was
  considered and dropped: a major's layer sits under `oil-global-mode`,
  a Universal minor that binds `-`, so it could never fire.
- **Synchronous, measured.** The plan said "off the UI thread via the
  inbound primitive". Oil's open path is synchronous on the editor actor
  (never the render thread), like the `read_dir` it generalises; gix
  in-process measured 0.32 ms a listing with the repository opened per
  call (LS.3's bench), so an asynchronous listing would only have made
  it open empty for a frame.
- **LS.4 split in two.** `<C-v>` on a revision's file needed a way to
  open a *non-path* buffer in a split, which did not exist
  (`OpenInTarget` takes a path). `Effect::InTarget` went into the oil
  half (LS.4a) so the magit half (LS.4b) had something to return.

## Verification

Targeted, per slice: `lattice-listing` lib; `lattice-host`
`oil_lists_from_its_source` (10), `read_only_mode_gates_edits`,
`oil_modeline_marks_pending_changes`, `focused_pane_follows_its_buffer`,
`buffer_scope_dir_drives_project_resolution`; `lattice-vcs` `integration
tree` (4); `lattice-magit` lib (797, including `oil_tree` 6);
`lattice-plugin-host` boundary (92); `lattice-ui-tui` `oil` (23) and
`magit_bindings` (37, five runs); `cargo check -p lattice-ui-gpui
--features window`. Whole-crate gates not run.
