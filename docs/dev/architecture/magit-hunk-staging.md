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

### Retiring the `HunkSpec` sketch

The `(path, HunkSpec)` shape comes from
[`vcs-and-magit.md`](vcs-and-magit.md), which
[`magit.md`](magit.md) §3.2 already supersedes — it is the *early
sketch*, and magit.md records several of its decisions as reversed
(write-API no longer stubbed, native crate over WASM, no
`spawn-process` WIT gate). Its Write-API block locks:

```rust
pub fn stage_hunk(repo: &Repository, path: &Path, hunk: HunkSpec) -> Result<(), GitError>;
```

`HunkSpec` was never defined, and magit.md §7.3 records hunk staging
itself as target-design-not-built. The shape does not survive contact
with git, for two reasons:

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

The load-bearing choice — and [`magit.md`](magit.md) has already
answered the equivalent question twice, in ways that constrain it.

**Precedent 1 — hunk boundaries are already text-derived.** §7.5: `]c`
/ `[c` walk hunks via `hunk_lines` in `magit_core_mode.rs`, a raw
buffer scan for lines starting with `@@` or `diff --git`, **identical
in every magit buffer**, consulting no cache:

```rust
if t.starts_with("@@") || t.starts_with("diff --git") { lines.push(l); }
```

§7.5 is explicit that the target design's `DiffCache`-aware "only walk
expanded files' hunks" behaviour is *not built*, and that `]c`/`[c` are
plain text scans everywhere.

**Precedent 2 — the mode-private data deliberately excludes diffs.**
§7.2: `SectionIndex` "stores file paths and status labels — **not
diffs**", and the target design's "separate `DiffCache` keyed by
`(path, section)` with parsed `Hunk`/`ParsedHunk` data does not exist".

**Precedent 3 — the confirm contract mandates re-reading anyway.**
§12.13: the execute half of a destructive action "re-reads its target
at the cursor rather than carrying it through the prompt". `x` is
destructive, so hunk-discard *must* resolve its hunk from the cursor at
execute time no matter where the identity is stored.

### (A) Re-derive from buffer text at action time — **recommended**

Reuse the same scan `]c`/`[c` already use: find the enclosing `@@`,
take lines to the next `@@` / `diff --git`, walk up to the file header
for the path.

> **UX (higher court):** neutral-to-positive — navigation and staging
> agree on where a hunk starts *by construction*, so `]c` then `s`
> always stages the hunk you just jumped to.
> **Paramount goals:** protects #3 (one hunk concept across every magit
> buffer — the grammar behaves the same wherever the cursor is);
> protects #1 (nothing computed until the chord fires).
> **Heuristic #1 (long-term fit, on merit):** the genuinely-better
> design, and notably *not* the smaller-diff argument — it is the same
> mechanism already in production for hunk navigation. One parser then
> serves all three diff surfaces (status inline, magit-diff, the commit
> buffer's staged region) because all three hold the same text.
> **Heuristic #2 (paramount, not other editors):** anchored on
> everything-is-a-buffer — the buffer text *is* the model here, which
> is why §7.5's walkers read it directly. Emacs magit keeps structure
> in overlays; that is a fact about elisp, not an argument.
> **Heuristic #3 (third option):** (B) and (C) below.
> **Standing-rule check (mode ownership):** parser and chord bodies
> both live in `lattice-magit` as helper functions — the
> substrate-helper side of the rule, exactly as §7.2 classifies
> `classify_line` — and move together into `magit-hunk-mode` at MG.22.
> No `Editor::do_stage_hunk`.

**Cost, stated plainly:** a writer/reader pair — `diff_styled_spans`
writes the buffer, the hunk parser reads it back. That is the drift
that left every magit-stash chord dead until MG.15. Mitigated by
routing the reader through MG.21a's single `classify_diff_line` ladder
and by round-trip tests, not by hoping.

### (B) Store parsed hunks in mode state

A `DiffCache` of `Vec<Hunk>` alongside `StatusBufferState::expanded`.

This is precisely the structure §7.2 records as *not existing*, and
adopting it would put staging on a different hunk model than the
navigation that shares the buffer — `]c` would find one boundary set
and `s` another, and nothing would force them to agree. It also only
covers magit-status: magit-diff and the commit buffer have no
`expanded` map, so each needs its own. Three mechanisms where (A) has
one, plus a new disagreement hazard.

Additionally `expanded` is cleared on every refresh and its insert
deliberately happens *after* the async edit lands (§6.4) — a cache
keyed to it inherits that race.

### (C) Have git re-derive it

Re-run `git diff` at action time and match the cursor's hunk by index.

Correct by construction, no parser — but an fs + subprocess round-trip
per keystroke, and the hunk index is only stable if nothing changed
between render and action, which is exactly when it matters. Rejected
on paramount #1 and on correctness.

**Recommendation: (A)**, because it protects paramount-#3 — one hunk
concept across every buffer whose content is a diff, shared with the
navigation chords that already define it that way — and because
heuristic #1 favours reusing the mechanism already proven in `]c`/`[c`
over introducing a second, disagreeing one.

## Fulfilling §7.4 (stale hunk boundaries)

[`magit.md`](magit.md) §7.4 reserves "stale hunk boundary detection" as
a safeguard to build when hunk staging arrives. **`git apply`'s exact
context match is that safeguard**, and a stronger one than a hand-rolled
check: it validates every context line against the real target, so a
patch built from a buffer that has drifted is refused outright rather
than applied at a plausible-looking offset. No separate staleness
heuristic is needed; the failure path is "report and refresh".

## Why this is not blocked by the `p` problem

§7.3 explains that `git add -p` is unsupported because it is genuinely
interactive over stdin, which the TUI's raw-mode input loop already
owns — `Command::output()` would hang the actor against a child waiting
on stdin neither routes to the other.

`git apply` has none of that: it is non-interactive, we write the
complete patch and close the pipe, and it exits. The `p` blocker does
not transfer, which is why hunk staging can land without solving
terminal suspend.

## `x` follows the ask/execute contract

Hunk-level discard is destructive — `git apply --reverse` on the
worktree throws away work git cannot return. It therefore takes the
§12.13 shape, not a new one:

- `action:magit-discard` reads the hunk at the cursor, performs **no
  git call**, and returns `Effect::Confirm { prompt, yes_action }`.
- `action:magit-discard-execute` re-resolves the hunk at the cursor and
  applies the reversed patch.
- The prompt names its target — `Discard hunk at src/main.rs:42?` —
  since §12.13 requires a question answerable without dismissing it.

The existing `x` row in `DESTRUCTIVE_ACTIONS` already covers this
action pair; extending it to hunks changes what the execute half
resolves, not the contract.

`s` and `u` continue **not** to ask: §12.13 lists stage/unstage among
the deliberate no-confirm set (index-only, reversible), matching Emacs
magit's `magit-no-confirm` default.

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

[`magit.md`](magit.md) is the authoritative subsystem design; the
sections this fragment builds on directly:

- **§7.2** section index as mode-private helper data (no diffs cached)
- **§7.3** staging is file-level only today, and why `p` is blocked
- **§7.4** the stale-hunk-boundary safeguard this fulfils
- **§7.5** `]c`/`[c` hunk boundaries as a raw buffer scan — the
  precedent option (A) reuses
- **§6.4** `StatusBufferState::expanded` and its post-edit insert
- **§12.13** the destructive ask/execute contract `x` follows

Also:

- [`magit-hunk-mode.md`](magit-hunk-mode.md) — the eventual owner (MG.22)
- [`mode-architecture.md`](mode-architecture.md) — mode ownership rules
- [`vcs-and-magit.md`](vcs-and-magit.md) — the superseded early sketch,
  kept for history; prefer magit.md
