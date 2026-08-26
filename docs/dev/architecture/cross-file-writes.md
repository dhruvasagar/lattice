# Cross-file writes

**Status:** built (XF.0–XF.6, 2026-08-26). The primitive ships and is
gated. Org's archive (OM.6b) landed the same day, and needed one thing
this document did not anticipate: a guest cannot name a file *beside* its
own without knowing which file it is in. `document.path()` (OM.6b.0) is
that half; see [`org-mode.md`](org-mode.md). OM.11 (refile + capture)
remains, in the plugin repo.

Where this document and the code disagree, the code and the slice plan are
what happened. Two things the build changed and the slice plan records in
full: the effect **applies inline** rather than through `ApplyEdit`'s
deferral (`next_actions` is walked unconditionally, so a deferred cut would
run whether or not the insert landed — the exact failure §5 forbids), and
`FileAnchor::resolve_line` answers in *lines* while turning a line into a
position stays host-side, because only the host has the buffer and the
append position is not `Position::new(line_count, 0)`.

Slice plan:
[`../operations/slice-plans/cross-file-writes.md`](../operations/slice-plans/cross-file-writes.md).
Extends [`plugin-host.md`](plugin-host.md) (the effect vocabulary, the
capability model). Unblocks [`org-mode.md`](org-mode.md) OM.6b and OM.11.

## 1. What is blocked

Three org commands — `org-archive-subtree`, `org-refile`, `org-capture` — do
the same thing: **take text from here and put it in a different file.** All
three are ⛔ today, and confirmed rather than assumed: *no effect in the WIT
surface writes to a file other than the buffer's own.*

- `edits` and `apply-edit` both target a buffer. `apply-edit-payload.target`
  is a `u32` `BufferId`, and a guest cannot learn the id of a file the editor
  has never opened.
- `open-buffer-at` changes what is focused. It does not compose with a
  follow-on edit in any defined order, and nothing says the next effect in the
  list applies to the buffer the previous one opened.
- `host-services` has `walk`, which reads. There is no write peer, and adding
  one there would be the wrong shape — see §7.

This is not plugin work. It is a missing host primitive, and it is one
primitive rather than three: archive, refile and capture differ only in where
the text comes from and which file it lands in.

## 2. The thesis

> A plugin can move text into a file it has not opened, through the editor's
> own document pipeline, under a capability it declared.

Three words in that sentence are load-bearing.

**"Through the pipeline"** and not to disk. If the target file is already open
in a buffer — quite likely, since org users keep their files open — a direct
write makes the buffer and the disk disagree with nobody told. Routing through
the document actor means the open buffer sees the edit, `u` undoes it, the LSP
is notified, and the syntax worker reparses. All of that already works for a
peer buffer (`Editor::apply_targeted_edit` handles a non-active target today);
what is missing is only *path → buffer*.

**"Has not opened"** is the whole difficulty. The guest has no handle on the
target, so it cannot compute a byte range in it. §4 is about that.

**"A capability it declared"** — `fs:write:<prefix>`, the vocabulary that
already exists, checked at a place that is new. §6.

## 3. The effect

```wit
/// Where in the target file the text lands. A *position*, not a range,
/// because the guest has never read this file — see §4.
variant file-anchor {
    /// After the last line. The common case: archive, refile and capture
    /// all append by default.
    end,
    /// Before the first line.
    start,
    /// Before this 0-based line. Out of range clamps to `end`.
    line(u32),
}

record write-to-file-payload {
    /// Absolute, or relative to the editor's working directory. Must lie
    /// within one of the plugin's `fs:write` prefixes (§6).
    path: string,
    anchor: file-anchor,
    /// The text to insert. A trailing newline is the guest's business; the
    /// host inserts exactly these bytes.
    text: string,
    /// When present, this range is removed from the buffer the action ran
    /// in — and ONLY after the insert has landed (§5).
    cut: option<range>,
}

/// Move text into another file. One effect, not two — see §5.
write-to-file(write-to-file-payload),
```

Native peer: `Effect::WriteToFile { path, anchor, text, cut }` in
`lattice-grammar`, so a native mode can use it too. Nothing about this is
org-specific and nothing about it is WASM-specific.

## 4. An anchor, not a range, and the asymmetry is load-bearing

`apply-edit` carries an `edit` with a byte range. This carries an anchor. The
two shapes differ, and that reads like a smell until you ask what the guest
knows in each case.

For its **own** buffer the guest holds `borrow<document>`: it can read lines,
count them, and compute a range that means something. For **another file** it
holds nothing — it has never seen the bytes, so a range it invented would be
a guess. `end` / `start` / `line(n)` are the three positions a guest can name
without reading, and they are exactly the three the blocked commands need.

The asymmetry is therefore the *answer* to "how do you address a position in a
file you cannot read", not an inconsistency to be tidied away.

It also bounds the blast radius. An insert-only primitive cannot silently
destroy content in a file the user was not looking at. A range-carrying one
could, on an off-by-one, and the user would find out later.

**Rejected: hand the guest a read handle for the target first.** A
`borrow<document>` minted for a path would let it compute a real range. That
is two new mechanisms (opening a document on a guest's behalf, and a handle
whose lifetime spans a call it did not initiate) to serve three commands that
only ever append — the same "most general answer, most machinery" trade
`org-mode.md` §6.3 already declined once for the agenda. If a plugin ever
genuinely needs to *replace* in another file, that is when to build it.

## 5. One effect, not two, because the failure mode is unrepresentable

Archive is "delete the subtree here, insert it there". As two effects that is
two ways to corrupt a document:

- insert succeeds, delete fails → the subtree exists twice,
- delete succeeds, insert fails → the subtree is **gone**.

The second is data loss from a keystroke, which is the failure this design
exists to avoid rather than to handle gracefully.

Nothing in the current contract helps, and the reason is stronger than
"there is no abort-on-failure": **an effect cannot report failure at all.**
`apply_effect_host` returns `()`, and `Effect::Many(parts)` walks its parts
unconditionally. So a host that *wanted* to stop after a failed insert has
nothing to stop on — giving two ordered effects the semantics they would need
means changing the signature of every effect in the vocabulary, to serve one
command family.

`cut` folds the pair into one effect the host applies as a unit: **insert
first, then cut, and only if the insert landed.** A failed insert leaves the
source buffer untouched — the user sees an error and still has their text.
The reverse ordering was considered and is worse: cutting first means a failed
insert has already destroyed the original.

This is not a transaction. The two edits land in different buffers and each is
separately undoable, so `u` in the source buffer reverses the cut and `u` in
the target reverses the insert. Making it one undo step would mean a
cross-buffer undo group, which the undo model does not have and should not
grow for this.

## 6. The capability gate, and where it lives

`fs:write:<prefix>`. No new capability vocabulary: a plugin that may write
under `~/org` may write under `~/org`, whether it does so through WASI or
through this effect.

**The check runs at the boundary, not at the applier**, and that is the
structural decision in this fragment.

An `Effect` is a guest *return value*. By the time it reaches
`Editor::handle_effect` the dispatcher has no idea which plugin produced it —
effects from a plugin and from a native mode are the same type, deliberately.
So the gate cannot live there without the effect carrying a plugin id, and an
id inside guest-returned data is guest-controlled input, which is exactly what
`provenance_ids_are_host_issued_unique_and_stamp_the_plugin_layer` exists to
forbid.

At the **boundary** the provenance is still known: the conversion runs against
a `Store<PluginState>` that carries the plugin's `CapabilityGrant`. So the
grammar seam's effect conversion checks the path against the grant and, on
denial, replaces the effect with an `Echo` naming the refusal. An effect that
reaches the dispatcher has already been authorised, and the dispatcher stays
generic.

The check itself is `host_services::grant_permits_walk`'s twin, and should
share its shape: canonicalize both sides so a `..` cannot escape, and fall
back to the raw path when canonicalization fails — which still requires a
literal prefix match, so it can only ever deny more, never widen. A target
that does not exist yet (capture's first run) will not canonicalize, and that
must not be a denial; canonicalize its **parent**.

`info!`, not `debug!`, on a denial: it is one-shot and user-actionable ("org
tried to write outside its granted paths"), which is the level rule's own
example.

**This shape generalises.** `AppEffect::OpenProviderView`'s WIT surface is
withheld today pending "the capability model for which providers a plugin may
trigger" (`boundary_app_effect.rs`). The answer to *where* that check goes is
the same as this one; only the policy differs. Landing this first is what
makes that a policy question rather than an architecture question.

## 7. What happens to the target buffer

**It is opened, edited, and left modified.** Not saved.

If the file is already open, that buffer is reused —
`Editor::find_document_by_path` already answers this, and getting it wrong
would silently clobber the user's unsaved work. If it is not open, the host
opens it as an ordinary listed document buffer *in the background*: it appears
in `:ls`, `:w` saves it, `:bd` closes it, and the active pane does not move.

That last part matters. A plugin's write must not steal focus — the user
pressed `<leader>o$` to archive a subtree, not to navigate somewhere.

**Not saved**, and this is convention-following rather than laziness: emacs's
`org-refile` and `org-archive-subtree` both leave the target buffer modified,
with saving behind a separate option. The user reviews and writes. A plugin
that silently writes files is a different and much larger authority than one
that edits buffers, and it should be an explicit later decision if it is ever
wanted.

**Rejected: a `save: bool` in the payload.** It costs nothing to add and it
quietly moves the "did a plugin touch my disk" line. Leaving it out means the
answer is uniformly no, which is an easier thing for a user to know.

**Rejected: `host-services.write-file`.** A host-services import would be a
direct disk write and would bypass everything §2 argues for — the open buffer
would not see it, undo would not cover it, the LSP would not hear about it.
It is also the wrong direction: `host-services` is what the guest *asks* the
host for mid-call; an edit is something the guest *returns*, so it belongs in
the effect vocabulary with every other mutation.

## 8. Failure behaviour

Every path degrades to "the user's text is still where it was".

- **Path outside the grant** → the effect is replaced with an `Echo` at the
  boundary and never reaches the dispatcher. Logged at `info!`.
- **Target unreadable / not UTF-8 / a directory** → echo, no edit, `cut` does
  not run.
- **Insert fails** → `cut` does not run. The source buffer is untouched.
- **Target file does not exist** → created, if its parent directory exists and
  is within the grant. Capture's first run is exactly this. A missing *parent*
  is an echo rather than a `mkdir -p`: creating directories is a larger
  authority than creating a file, and a typo'd path should not silently build
  a tree.
- **`cut` range out of bounds** → the insert has already landed and the cut is
  skipped with a `warn!`. Duplicated text is recoverable by hand; lost text is
  not, so this asymmetry is deliberate.

## 9. Performance

Off the keystroke path in every sense that matters, but not free: opening a
file reads it, and the read is synchronous with the effect.

The bound is the same one `:e` lives with, and the file is one the user
named through a command they invoked. It does not run per frame, per
keystroke, or per tick. The one thing to hold: the read must not land on the
editor actor's `current_thread` runtime as a blocking call inside a hot
dispatch — see the slice plan for where it goes.

No new per-keystroke cost, so no new bench gate. The existing grammar
round-trip ratchet still covers the guest call that returns the effect.

## 10. Scope

**In:** the `write-to-file` effect, its native peer, the boundary capability
gate, background open-or-reuse, insert-then-cut ordering, and the failure
behaviour above.

**Out, as cuts rather than omissions:** saving the target (§7), creating
parent directories (§8), replacing a range in another file (§4), and
cross-buffer undo grouping (§5).

**Not this fragment's problem:** which *providers* a plugin may trigger
(`OpenProviderView`). This lands the enforcement shape that question will
reuse; the policy is separate.

## 11. Paramount-goal alignment

**#1 Performance.** Nothing on the keystroke path changes. The file read is
bounded, user-initiated, and off the actor thread.

**#2 Extensibility.** This is the goal the fragment serves. Three org
commands unblock, and so does every future plugin that needs to write beside
itself — a note-taker filing into a journal, a codegen helper writing a
sibling module, a test scaffolder. None of them need a host change, and the
host still learns nothing about org.

**#3 Vim modal editing.** Untouched: the effect is returned by an action that
was reached through the ordinary grammar. `u` works in both buffers.

**#4 Asynchronicity.** The read is off the actor thread; the edit goes through
the document actor like every other edit.
