# Hunk-level staging

**Status:** designed 2026-07-29, not implemented. Slice plan:
[`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md)
§MG.18.

## Why

Every `s` / `u` / `x` in every magit view is file-level. `p`
(`git add -p`) is deliberately disabled — it needs terminal suspend —
so **there is currently no path to partial staging at all**. That is
the largest single divergence from Emacs magit, where staging a hunk
(or a selected region of one) is the ordinary way to build a commit.

### The stubs are worse than absent

`lattice-vcs`'s `Index::stage_hunk` / `unstage_hunk` look like the
missing capability. They are not:

```rust
pub fn stage_hunk(repo: &Repository, path: impl AsRef<Path>, _hunk_index: usize) -> Result<()> {
    Self::stage_path(repo, path)   // stages the WHOLE file
}
```

The parameter is discarded. The doc comment claims "the magit-status
action handler overrides with hunk-level precision" — no such override
exists, and never has. A caller who trusts the signature silently
stages everything, which in a staging UI is data loss in the sense that
matters: the commit contains changes the user deliberately excluded.

**These two functions are deleted, not fixed.** Hunk staging does not
have the shape `(path, hunk_index)` — see "Mechanism" — so keeping the
names would preserve a misleading API. Removing them is part of MG.18's
first slice, and is worth doing even if the rest slips.

## Mechanism: synthesize a patch, `git apply --cached`

There is no index API for "stage this hunk". Git's own `add -p` builds
a patch containing the selected hunks and pipes it to
`git apply --cached`. Magit does the same. So do we.

| Action | Command | Patch built from |
|---|---|---|
| `s` stage hunk | `git apply --cached` | the unstaged diff |
| `u` unstage hunk | `git apply --cached --reverse` | the staged diff |
| `x` discard hunk | `git apply --reverse` | the unstaged diff (worktree) |

A synthesized patch is a valid `diff --git` header, the `---` / `+++`
path pair, and exactly the selected `@@` hunks:

```
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -14,7 +14,8 @@ fn main() {
 	let x = 1;
-	println!("{}", x);
+	println!("{x}");
+	println!("done");
 }
```

`git apply` requires the context lines to match the target exactly, and
**that is the safety property we want**: if the worktree moved under a
stale buffer, the apply fails loudly instead of staging the wrong
lines. Failures surface as a user-visible error and a refresh, never a
silent no-op.

### Diverging from the "shape locked now" sketch

[`vcs-and-magit.md`](vcs-and-magit.md) §Write-API locks:

```rust
pub fn stage_hunk(repo: &Repository, path: &Path, hunk: HunkSpec) -> Result<(), GitError>;
```

`HunkSpec` was never defined. The shape does not survive contact with
git, for two reasons:

1. **There is no index operation that stages "hunk N of path P".** Any
   implementation would synthesize a patch internally — and to be safe
   under `git apply`, the spec would have to carry the hunk's full body
   including context. At that point `HunkSpec` *is* the patch, and the
   `path` parameter is a second source of truth that can disagree with
   it.
2. **Region staging cannot be expressed as a `HunkSpec` at all.** It is
   a *rewritten* hunk (unselected `+` dropped, unselected `-`
   contextualised, counts recomputed), not a reference to an existing
   one.

So the write API becomes patch-shaped, and the two named functions go
away rather than being filled in:

```rust
impl Index {
    /// Apply `patch` to the index (`--cached`) or the worktree,
    /// forward or reversed. The unit of partial staging.
    pub fn apply_patch(repo: &Repository, patch: &str, cached: bool, reverse: bool) -> Result<()>;
}
```

Per heuristic #1 a locked sketch is a starting point, not authority —
it is re-evaluated when the slice executes, and this one loses on
merit. `vcs-and-magit.md`'s Write-API block is updated with MG.18a.

### `lattice-vcs` needs stdin

`Repository::run_git` is `Command::new("git").args(..).output()` — no
stdin. Piping a patch needs a sibling:

```rust
pub fn run_git_stdin<I, S>(&self, args: I, input: &[u8]) -> Result<Vec<u8>>
```

Writing the patch to a temp file and passing a path would avoid it, but
leaves a file behind on every crash and races a concurrent magit in the
same repo. Stdin is what git's own tooling uses.

## Hunk identity: derived from the buffer, not stored

The load-bearing choice. Three options, evaluated against the goals.

### (A) Re-derive from buffer text at action time

Walk up from the cursor to the nearest `@@` header, take lines to the
next `@@` (or the end of the diff region), and walk further up to the
file header for the path.

> **UX (higher court):** neutral — no visible difference; correctness
> rests on the buffer text matching what git produced, which it does
> because the buffer *is* git's output verbatim.
> **Paramount goals:** protects #3 (the vim grammar keeps working —
> `s` resolves against wherever the cursor happens to be, including
> after the user scrolled, folded or searched); protects #1 (nothing
> is computed until the chord fires, so expansion stays cheap).
> Sacrifices nothing measurable.
> **Heuristic #1 (long-term fit):** genuinely better, not merely
> smaller. One parser serves **all three** surfaces — magit-status's
> inline expansion, magit-diff's whole buffer, and the commit buffer's
> staged region — because all three hold the same unified-diff text.
> The alternatives need per-surface plumbing.
> **Heuristic #2 (paramount, not other editors):** anchored on
> everything-is-a-buffer — the buffer's text is the model, so reading
> it back is not a hack, it is the design. (Emacs magit stores
> structure in overlays; that is a fact about elisp, not a reason.)
> **Heuristic #3 (third option):** (B) and (C) below.
> **Standing-rule check (mode ownership):** the parser and the chord
> bodies both live in `lattice-magit`, and both move together into
> `magit-hunk-mode` at MG.22. No host-side `Editor::do_stage_hunk`.

**Cost, stated plainly:** a writer/reader pair — `diff_styled_spans`
writes the buffer, the hunk parser reads it back. That is exactly the
drift that left every magit-stash chord dead until MG.15. Mitigated by
routing both through MG.21a's single `classify_diff_line` ladder and by
round-trip tests (parse(render(hunks)) == hunks), not by hoping.

### (B) Store parsed hunks in mode state

Keep `Vec<Hunk>` with line ranges alongside `StatusBufferState::expanded`.

Structural and drift-free, but: `expanded` is cleared on every refresh
and the inline diff is re-fetched, so the map must be rebuilt in
lockstep with an async insert that already had a race worth commenting
(`expanded.insert` deliberately happens *after* the edit lands). It
also solves the problem only for magit-status — magit-diff and the
commit buffer have no `expanded` map, so they would each need their
own. Three mechanisms where (A) has one.

### (C) Have git re-derive it

Re-run `git diff` at action time and match the cursor's hunk by index.

Correct by construction, no parser at all — but it is an fs + subprocess
round-trip per keystroke, and the hunk index is only stable if nothing
changed between render and action, which is precisely when it matters.
Rejected on paramount #1 and on correctness.

**Recommendation: (A)**, because it protects paramount-#3 — one hunk
parser makes `s`/`u`/`x` behave identically in every buffer whose
content is a diff, which is what "the grammar is the API" means here —
and because heuristic #1 favours the design with one mechanism over the
one with three.

## Resolution order: hunk, then file

`s` / `u` / `x` resolve **hunk-at-cursor first, file-at-cursor second**:

| Cursor on | Acts on |
|---|---|
| a `+`/`-`/context line inside an expanded diff | that hunk |
| a `@@` header | that hunk |
| a file entry line | the whole file (unchanged) |
| a section header | nothing |

The file-level path is untouched, so no existing behaviour regresses —
a cursor that resolved to a file before still does.

## Region staging: the hard part

Magit's most-used partial-stage path is visual-mode: select some lines
inside a hunk, press `s`, stage only those. This is not "a smaller
hunk" — the patch must be *rewritten*:

- Selected `+` lines stay `+`.
- **Unselected `+` lines are dropped entirely** (they are not in the
  index-side content).
- Selected `-` lines stay `-`.
- **Unselected `-` lines become context** (` `) — they still exist in
  the index-side file.
- The `@@ -old_start,old_count +new_start,new_count @@` counts are
  recomputed from the rewritten body.

Getting the counts wrong produces a patch git rejects (loud, fine) or
one that applies at the wrong offset (silent, not fine). The rewrite is
a pure function over `(hunk, selected_line_set)` and is tested as one,
independent of git, with a table of cases:
all-selected (≡ whole hunk), none-selected (≡ no-op, not an empty
patch), adds-only, removes-only, interleaved, first line, last line.

**Reverse direction:** unstaging a region reverses the roles — dropping
unselected `-` and contextualising unselected `+`. One function with a
direction flag, not two, so the two can't drift.

## Refresh and the cursor

Every mutation triggers the existing refresh, which replaces the whole
buffer and collapses expansions (`expanded.clear()`). After staging one
hunk of a five-hunk file the user is looking at a collapsed file entry,
having lost their place — tolerable at file granularity, actively bad
at hunk granularity, where the whole point is to stage several hunks in
sequence.

**Required:** re-expand the entry the action fired in and restore the
cursor to the next remaining hunk (magit's behaviour). This is the
scenario the "eventual consistency is acceptable, losing the user's
place is not" clause of the UX contract exists for, and it is part of
MG.18's deliverable — not a follow-up.

## Ownership and sequencing

The hunk parser, the patch synthesizer and the `s`/`u`/`x` bodies all
belong to whichever mode owns diff *content*. Today that is nothing;
after MG.22 it is `magit-hunk-mode`.

Landing MG.18 first means this code lives in `lattice-magit` free
functions and moves into the mode at MG.22. That is the right order
anyway — MG.22's own blocker (a minor mode supplying a tree-sitter
parser) is unresolved, and hunk staging must not wait on it. The move
is a relocation, not a rewrite, provided MG.18 keeps the parser and the
patch builder as free functions with no `Editor::` methods added, per
the mode-ownership acid test.

## Cross-references

- [`magit.md`](magit.md) — the subsystem this serves
- [`magit-hunk-mode.md`](magit-hunk-mode.md) — the eventual owner (MG.22)
- [`mode-architecture.md`](mode-architecture.md) — mode ownership rules
