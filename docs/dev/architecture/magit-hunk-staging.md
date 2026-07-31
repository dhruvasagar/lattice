# Hunk-level staging

**Status:** designed 2026-07-29, **complete 2026-07-30**. MG.18a
(patch-shaped write API), MG.18b (hunk parser), MG.18c (`s`/`u`/`x` on
the hunk at the cursor), MG.18d (the buffer and cursor survive the
refresh) and MG.18e (region staging) all landed. Slice plan:
[`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md)
§MG.18.

## Why

*(Written before the work; kept as the problem statement.)* Every
`s` / `u` / `x` in every magit view is file-level. `p` (`git add -p`) is
deliberately disabled — it needs terminal suspend — so **there is
currently no path to partial staging at all**. That is the largest
single divergence from Emacs magit, where staging a hunk (or a selected
region of one) is the ordinary way to build a commit.

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

**These two functions were deleted, not fixed** (MG.18a). Hunk staging
does not have the shape `(path, hunk_index)` — see "Mechanism" — so
keeping the names would have preserved a misleading API.

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
[`vcs-and-magit.md`](../archive/vcs-and-magit.md), which
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
merit. The sketch itself is left as written: it is archived history,
not a document to maintain forward.

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
a cursor that resolved to a file before still does. "Inside" is
containment, not proximity: the parser refuses a cursor *below* a
hunk's last body line. In magit-status the row after an expanded diff
is the next file entry, and `s` there must stage that file rather than
restage the hunk above it.

### Where the resolution lives

In `magit-core-mode`'s shared handler, not in each view. A hunk is a
property of diff *text*, identical in every magit buffer — the same
argument `magit.md` §7.5 makes for `]c` / `[c` living there. One
implementation serves magit-status's inline diffs, magit-diff's buffer,
and whatever binds `s` next; a future diff-showing view gains hunk
staging by binding the chord, with no new code.

What genuinely differs per view stays behind `MagitView`: which file a
non-hunk line names (the file-level fallback), and which tree the text
was diffed against (below). Per the substrate-vs-mode-helper rule this
is *not* a `Document` trait method — the consumer is magit's own
handler, not generic host machinery.

## The operation asks which tree the hunk came from

A hunk's patch is only meaningful against the tree it was diffed from,
so `MagitView::diff_source(cursor)` answers `Staged` / `Unstaged` /
`Committed` / `None`, and the operation is gated on it:

| Chord | Acts on | Applies |
|---|---|---|
| `s` | an `Unstaged` hunk | `git apply --cached` |
| `u` | a `Staged` hunk | `git apply --cached --reverse` |
| `x` | an `Unstaged` hunk | `git apply --reverse` (worktree) |
| `a` | a `Committed` hunk | `git apply` (worktree) |
| `-` | a `Committed` hunk | `git apply --reverse` (worktree) |

Every view answers from what it already knows — magit-status from the
section header above the cursor, magit-diff from the scope in its
buffer name — so nothing is stored and nothing can go stale.

**Why gate at all, when git would refuse a mismatched patch anyway.**
Two reasons, and the second is the load-bearing one:

1. `error: patch does not apply` in `*messages*` is indistinguishable,
   from the user's seat, from a missed keypress. "that hunk isn't
   staged" says which key to press instead.
2. `x` on a **staged** hunk would *not* be refused by git. The staged
   diff's `+` side is in the worktree too, so `git apply --reverse`
   succeeds and removes the change from the file while leaving it in
   the index — it vanishes from the buffer and is still committed by
   the next `cc`. Refusing (with "unstage it with `u` first") is the
   only safe answer until `apply_patch` grows `--index`, which is
   MG.18e's territory.

`None` means "not classifiable here" and refuses hunk-level staging
rather than guessing. `*magit:diff*` against HEAD lands there: it mixes
staged and unstaged changes into single hunks, so a hunk from it is a
patch against neither tree. **The refusal does not fall through to
file-level** — falling through would turn a keypress aimed at one hunk
into staging the whole file, which is the same class of silent
over-staging MG.18a deleted the stubs over. The message points at the
file header, where file-level staging is still one deliberate press
away.

Emacs magit reaches the same place by a different road: it computes a
diff type per buffer and refuses to stage from a `committed` or
`undefined` one. Ours is strictly more permissive (file-level staging
survives in the HEAD view).

### `Committed` — MG.23g

The third source is a patch **already in history**: a revision view's
`git show`, a stash detail's `git stash show -p`. Neither `s` nor `u`
can act on it, because the change is not sitting between two of this
checkout's trees — it is a description of something that already
happened. What it supports is `a` (put this one hunk into my working
tree) and `-` (take it back out): cherry-picking or reverting one hunk
of a commit rather than the whole thing, where `A` and `_` take all of
it.

Three properties fall out of the gate rather than being added to it:

- **Both write the working tree, never the index.** The question `a`
  answers is about the file you would edit, not about what is queued
  for the next commit. The result is an ordinary unstaged change that
  `s` then stages normally. A `cached` slip here would stage a
  commit's hunk invisibly, which is why the round-trip test asserts an
  empty `git diff --cached` rather than only asserting the file.
- **Neither confirms.** Each is the other's exact inverse — `a` adds a
  change `-` removes, `-` removes one still held by the commit it came
  from — so both are recoverable without consulting anything the user
  cannot see, which is §12.13's actual test. `git apply` also refuses
  outright on drifted context, so neither can damage an edit in
  progress.
- **No file-level fallback.** `s`/`u` fall through to the view's
  whole-file path when the cursor is not in a hunk. For `a`/`-` the
  file-scale meaning is cherry-pick and revert, so falling through
  would turn a missed cursor into a far larger action than the key
  promises. They echo instead — a bare `None` would be worse still,
  since a Normal-mode chord a mode binds is consumed unconditionally
  and would read as a dead key.

The views that answer `Committed` (`magit-revision-mode`,
`magit-stash-show-mode`) both return `None` from `MagitView::refresh`,
and correctly: applying a hunk to the working tree does not change the
commit being shown. The buffer that *did* change is the one the user is
not looking at.

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

The asymmetry is not a convention: it is what the target contains.
Applying forward, the target holds the old side, so an unselected `+`
is not there and must not appear at all, while an unselected `-` *is*
there and survives — i.e. context.

Getting the counts wrong produces a patch git rejects (loud, fine) or
one that applies at the wrong offset (silent, not fine). The rewrite is
a pure function over `(hunk, selected_rows, direction)` and is tested as
one, independent of git, with a table of cases: all-selected (≡ whole
hunk, byte-identical), none-selected (a refusal, not an empty patch),
adds-only, removes-only, interleaved, the header's function-context
suffix, and a `\ No newline` marker whose line was dropped. Two git
round-trips then prove both directions produce patches git *accepts* —
the counts are what git validates, and only git can settle the
arithmetic.

**The two start lines are kept verbatim.** Whichever side the target
matches is preserved line-for-line by the rules above, so its start is
still correct; the other side's start is not something git checks.

**Reverse direction:** unstaging a region reverses the roles — dropping
unselected `-` and contextualising unselected `+`. One function with a
direction flag, not two, so the two can't drift.

### How the handler sees the region

`ActionContext` gained `selection: Option<Range>` — the Visual/Select
extent, normalised. This is design §5.2's "Visual mode IS the active
region" reaching mode action handlers, the same way `Range::Selection`
is the default range argument for an ex-command; magit is the first
consumer, not the reason. It carries no visual *kind*: a diff line is
the unit of every consumer so far.

Three firing paths build an `ActionContext`, and only two carry a
region:

| Path | Region | Why |
|---|---|---|
| chord dispatch | yes | the Visual-mode press itself |
| transient item | **yes** | a `Confirm` becomes a transient, so `x`'s execute half fires here |
| prompt submit | no | the cursor described is the *prompt's* |

The transient row is load-bearing. `Effect::Confirm` opens a transient,
so a destructive action's execute half re-resolves through that path —
with `None` there, a region `x` would ask about 2 lines and then discard
the whole hunk. It is safe to carry because `open_transient` touches
neither the modal state nor the anchor, and the transient owns every
keystroke while open, so the region cannot have moved since the chord
fired — the same argument §12.13's cursor re-resolution already relies
on.

### Scope: one hunk at a time

The region is intersected with the hunk under the cursor. A selection
reaching past it acts on the part inside — magit's own region can span
hunks, which needs a multi-hunk patch builder, so the echo names the
count (`staged 3 lines of src/main.rs:42`) rather than implying it did
more. A selection holding no `+`/`-` line at all is refused with a
reason, *before* the staged/unstaged gate: the user picked those rows
deliberately, and "there is nothing there" is more useful than a lecture
about which side of the index they are on.

Acting on a region ends Visual mode, like any vim operator on a
selection — staying selected would invite a second `s` over rows whose
meaning just changed under the refresh.

## Refresh and the cursor

Every mutation triggers the existing refresh, which replaces the whole
buffer. Before MG.18d that also collapsed every expansion
(`expanded.clear()`), so after staging one hunk of a five-hunk file the
user was looking at a collapsed file entry, having lost their place —
tolerable at file granularity, actively bad at hunk granularity, where
the whole point is to stage several hunks in sequence. This is the
scenario the "eventual consistency is acceptable, losing the user's
place is not" clause of the UX contract exists for, and it was part of
MG.18's deliverable, not a follow-up.

### The rebuild carries the expansions

`build_and_format` takes the set of open entry keys and **inlines their
diffs into the text it builds**, so the expansion arrives with the
rebuild rather than being re-applied afterwards. One edit, one span
vector, no splice arithmetic against a buffer that is being replaced
underneath it, and no collapse-then-expand flicker. The line counts in
`expanded` are recomputed from the text that was actually written —
carrying them over would collapse the wrong rows, since staging a hunk
makes the diff shorter.

A `gr` gets the same treatment, which is the same wart one size up: a
manual refresh no longer throws away every diff you had open.

### The cursor is restored by identity, not by row

A rebuilt buffer invalidates rows — files move between sections, counts
change, the staged hunk is gone. So a mutation records *what* the user
was working on: the file, which side of the index, and the **ordinal**
of the hunk within that file. Staging hunk *k* removes it, so ordinal
*k* then names the next remaining hunk; magit's behaviour and the
restore rule are the same arithmetic. Clamping to the last hunk covers
staging the final one; an anchor that has vanished entirely (the file
left the section) sends no cursor at all, leaving the user where the
refresh put them rather than guessing.

Naming the *anchor* is per-view and stays behind `MagitView`:
magit-status looks for an entry row under the matching section header,
a diff buffer for a `diff --git` line. The shared staging path names
the work (`HunkSite`); each view names the landmark.

### Why the cursor cannot just be an `Effect::CursorMove`

Two things had to be true, and neither is automatic:

1. **It must not wait for a keypress.** `run_tick_pending` is reached
   from `App::apply`'s tail *and* from the editor actor's
   `async_landed` arm. A drain registered as a bare `tick_callback` has
   no wake of its own, so its results sit until the user presses
   something — "staging works, but the cursor only catches up when I
   touch a key". The position therefore travels on a
   `SubsystemBoot::inbound` bus, whose `send` fires `async_landed` from
   inside the sender (`boot-composition.md` §3).
2. **It must name its buffer.** By the time the position is resolved,
   two git calls have run; a `q` in that window means a bare
   `CursorMove` would land the caret in whatever the user moved to. It
   is an `Effect::CursorMoveIn { target, position }`, which the host
   applies only while `target` is focused.

Both are tested by their failure mode: reverting the bus to a
`tick_callback` fails `magits_cursor_bus_wakes_the_editor_without_a_keypress`
with exactly that message.

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
- [`vcs-and-magit.md`](../archive/vcs-and-magit.md) — the superseded early sketch,
  kept for history; prefer magit.md
