# magit-hunk-mode — the mode that owns diff *content*

**Status:** designed 2026-07-29; **implemented except the parser**. The
mode exists and owns the hunk chords, `]c`/`[c`, `]f`/`[f` and `<CR>`
(MG.24a + MG.22's seam, 2026-08-01), and its options landed in MG.22b
(2026-08-02). `tree-sitter-diff` parsing is **deferred past v1**
(2026-08-03) — see "Parsing" below, which records what the 2026-08-03
scoping pass established and where this document was wrong. Slice plan:
[`../operations/slice-plans/magit.md`](../operations/slice-plans/magit.md)
§MG.22.

## Why

Five magit majors display buffers whose content is a unified diff, and
each reimplements the same three behaviours:

| Duplicated | Sites | Where |
|---|---|---|
| `diff_styled_spans` calls | 8 | status (inline `=`), commit, diff ×3, revision, stash-show |
| `file_at_cursor` parsers (walk up to `diff --git a/`) | 3 | commit, diff, revision |
| `<CR>` visit-file handlers | 3 + status's context-aware one | commit, diff, revision |

Nothing owns "this buffer's content is a unified diff" — so styling,
parsing and `<CR>` are copied per view, and the *options* that ought to
govern diff display (context lines, whether to syntax-highlight) have
nowhere to live at all. That is the mode-ownership rule inverted: the
behaviour exists five times and its owner exists zero times.

## Shape

A **minor mode** in `lattice-magit`, layered under whichever magit
major owns the buffer. Minor rather than major because the same content
appears beneath five different majors, each of which keeps its own
chords, refresh body and lifecycle. Same relationship
[`help-mode`](../../user/help-mode.md) has with `markdown-mode`: the
major says what the buffer *is*, the minor says what its content *is*.

Activated by the majors that show diff content:
`magit-diff-mode`, `magit-revision-mode`, `magit-stash-show-mode`,
`magit-commit-mode` (its staged region), and `magit-status-mode` (its
inline `=` expansions).

## What it owns

1. **The chords that act on a hunk** — `s` / `u` / `x` in Normal and
   Visual, plus `a` / `-`. See "Acting on the hunk" below.
2. **Navigation within diff content** — `]c` / `[c`, and (2026-08-02)
   `]f` / `[f`. The file pair moved here from `magit-core-mode`, which
   bound it on all ten majors and where it meant something in one: in
   the list views it stepped between rows while calling itself "next
   file" (a job `j` and `]]` already do), in the diff views it matched
   indented *context* lines and walked through arbitrary code, and in
   the rebase todo — whose rows sit at column 0 — it matched nothing.
   This mode's five majors are exactly the file-bearing ones. Which
   rows count as a file still differs between them, and
   `MagitView::file_lines` answers that: entries in magit-status,
   `diff --git` headers in a pure diff. Hunk-scoped folds could join
   later.
3. **Structural highlighting** of the diff — see "Parsing" below.
4. **`<CR>`** — one handler, one diff-path parser.
5. **Options** — `magit.hunk.*`, which today do not exist anywhere.

### Acting on the hunk (amended 2026-08-01)

**The original list omitted `s` / `u` / `x`, and that was an
oversight rather than a decision.** This fragment and MG.18's hunk
staging were designed the same week and neither folded in the other:
MG.18 centralised the *machinery* (`resolve_hunk`, `HunkOp`, the
`DiffSource` gate all live in `magit_core_mode`) while leaving the
**chords** declared per-major, and this fragment listed only what it
had noticed being duplicated — the parsers and `<CR>`.

The omission had a cost. A live report found `x` missing from
`magit-diff-mode`, and the audit behind it found the set had drifted
everywhere:

| Major | shows diff | `s` | `u` | `x` |
|---|---|---|---|---|
| magit-status | inline via `=` | ✓ | ✓ | ✓ |
| magit-diff | yes | ✓ | ✓ | **✗** |
| magit-commit | staged region | ✗ | ✗ | ✗ |
| magit-revision | `git show` | ✗ | ✗ | ✗ |
| magit-stash-show | `stash show -p` | ✗ | ✗ | ✗ |

Eight declarations covering three actions, eleven of fifteen cells
empty, and nobody noticed the missing `x` because there was no single
place it should have been. That is the failure mode a copied set has:
**a gap in it does not announce itself.**

So the chords belong here, by this fragment's own principle — *the
major says what the buffer is, the minor says what its content is.*
`s` / `u` / `x` / `a` / `-` act on the hunk under the cursor, which is
diff content by definition.

**`]c` / `[c` and `a` / `-` move here too**, and item 2's "eventually"
becomes now. They sit on `magit-core-mode`, which activates on **all
eleven** magit majors — so they are consumed dead keys in magit-branch,
magit-log, magit-stash, magit-rebase, magit-blame and
magit-file-revision, none of which have hunks. `magit-core-mode` should
mean "every magit buffer" (`gr`, `q`, `]]`, `[[`, `TAB`, and the commit
operations), not "every magit buffer, and these four work in five of
them".

**What does not move: the machinery.** `resolve_hunk`, `HunkOp`, the
`DiffSource` gate and the region rewrite stay where MG.18 put them.
This mode contributes the chords and the handler bodies that call
them — the seam is already correct, only the bindings were in the
wrong place.

**Sequencing.** This half needs no `tree-sitter-diff` and no answer to
the parser wrinkle below; it is chords, an `ActivationPolicy` naming
the five majors, and deletions from the majors that had them. It can
land first and alone.

> **Landed 2026-08-01.** `<CR>` now belongs to this mode, resolving
> through `MagitView::diff_target`. See "the seam, as built" below.

**`<CR>` was NOT in the first half**, and the reason is worth naming
because it looks like it should be. In magit-status `<CR>` is
`magit-visit`, which dispatches on what is under the cursor — a file
entry, a stash, a commit row — not only on diff content. A minor's
binding wins over a major's, so moving `<CR>` here before the
`diff_target` seam exists would replace status's context-aware visit
with a diff-only one. It moves with the seam below, not with the
chords.

### `<CR>`: one chord, per-view target

The chord and the parsing belong to the mode; *which version of the
file to open* belongs to the view, because it genuinely differs:

| View | `<CR>` opens |
|---|---|
| `magit-diff-mode`, staged scope | the index blob |
| `magit-diff-mode`, unstaged/HEAD scope | the working-tree file |
| `magit-revision-mode` | the file at that sha |
| `magit-commit-mode` | the index blob |
| `magit-status-mode` | depends on the section the diff was expanded from |

Resolved through the existing `MagitView` seam — a third use of the
pattern MG.13 introduced for `gr`/`s`/`u` and MG.20 extended with
`commit_at_cursor`, rather than a fourth mechanism:

```rust
trait MagitView {
    // …
    fn diff_target(&self, path: &Path) -> Option<Effect> { None }
}
```

The mode parses the path out of the diff and asks the view what it
means. A view that declines gets no `<CR>`, which is the correct
default for a buffer with no meaningful target.

Rejected: parsing the scope back out of the buffer name
(`*magit:diff:staged:<path>*`). It needs no new plumbing, but it makes
a name format load-bearing in a second place — precisely the
writer/reader drift that left every magit-stash chord dead until MG.15.

### The seam, as built

Two trait methods, because the question splits cleanly:

- **`MagitView::diff_target(path)`** — *which version* of the file to
  open. Genuinely per-view: the index blob for a staged diff, the live
  file for an unstaged one, the file at a sha for a revision, the
  stash's copy for a stash detail.
- **`MagitView::visit_at_cursor(cursor)`** — what `<CR>` does when the
  cursor is **not** in diff content. Only magit-status implements it,
  and it is why the chord could not simply move: there `<CR>` resolves
  file entries, stashes and commit rows, and a minor's binding wins
  over a major's.

**The handler asks the view first, and that order is a correctness
requirement.** The obvious order — resolve the diff path, then ask
which version — is wrong in magit-status, because an expanded inline
diff renders *below* the entry it belongs to:

```text
  modified a.txt
    diff --git a/a.txt b/a.txt     ← a.txt's expansion
    @@ …
  modified b.txt                   ← cursor here
```

`path_at_cursor` scans upward, so on `modified b.txt` it finds
*a.txt's* header and `<CR>` opens the wrong file — silently, and only
when some earlier entry happens to be expanded. Asking the view first
means only rows it does not recognise (i.e. diff content) reach path
resolution.

**One parser replaced three, and the merge fixed a bug.**
`hunk::path_at_cursor` is now the only diff-path parser.
magit-revision's copy checked `git show --stat` rows *before* scanning
for the `diff --git` header, and `parse_stat_line` splits on `" | "` —
so `<CR>` on any diff body line containing that sequence (`a | b`, a
markdown table) resolved to the text left of the pipe and opened a
buffer named after it. Scanning for the header first removes the
ambiguity structurally: a diff body line always has one above it, and
a stat row never does.

## Parsing: `tree-sitter-diff`, not the hand-rolled styler

Use [`tree-sitter-diff`](https://github.com/tree-sitter-grammars/tree-sitter-diff)
(tree-sitter-grammars org, ~343k downloads) and delete
`highlight::diff_styled_spans`. Its highlight query covers everything
the hand-rolled version does and the cases it silently misses:

| Construct | hand-rolled | tree-sitter-diff |
|---|---|---|
| `+`/`-` lines, `@@` hunks, `+++`/`---` | ✅ | ✅ `addition` `deletion` `location` `new_file` `old_file` |
| renames | ✗ | ✅ `similarity` `dissimilarity` `score` |
| binary files | ✗ | ✅ `binary_change` |
| mode changes, `index` lines | ✗ | ✅ `mode` `index` |
| `diff --git` arguments | crude prefix match | ✅ `command` `argument` `filename` |

**It does not highlight hunk *content* in the file's own language.**
The grammar describes diff structure; the language of the code inside a
hunk is determined by a filename several lines above, which tree-sitter
injections cannot express. Language-aware hunk content is a separate
problem — see "Open" below.

### Deferred past v1 (2026-08-03), and what the scoping pass settled

MG.21a already gave these diffs their background tints, so what the
parser adds is narrow: renames, `index` lines, mode changes and binary
markers style as context rather than as metadata. Not worth a new
workspace grammar dependency and a `Lang` variant before v1. Four
findings, three of which correct this document:

**The wrinkle below dissolves — it was the wrong question.** Lattice
attaches parsers through the **major** (`Lang` →
`major_mode_id_for_lang` → `DocumentSyntax` buffer-local), and a magit
buffer's major is `magit-*`. This document guessed the mode should
write `DocumentSyntax` itself. It must not: magit's synthetic buffers
publish through `PendingSyntheticHighlights`, never through a live
`SyntaxHandle`, and `lattice_syntax::oneshot_highlight_lines`
(`oneshot.rs`) is already the exact primitive — parse once, return
`Vec<Vec<StyledSpan>>`, hand it to the channel the eight span sites
already use. `DocumentSyntax` is never touched, and the syntax worker's
ownership of that local is never in question. MG.26c set the precedent
for the blob buffer. Nothing incremental is lost: these buffers are
read-only and replaced wholesale on refresh.

**"Delete `diff_styled_spans`" is too strong.** Its
`classify_diff_line` / `DiffLineClass` have a second consumer —
`hunk.rs:482-528`, MG.18's hunk-boundary and ordinal machinery. Only
the *styler* goes; the ladder stays as the structural classifier. Two
diff readers then coexist, which is tolerable because they answer
different questions and the ladder is stateless and line-local, but it
is not the clean deletion described above.

**The grammar's own highlight query is unusable, and the mapping is
load-bearing beyond colour.** `tree-sitter-diff 0.1.0` (crates.io, ABI
14, compatible with tree-sitter 0.26) covers every construct the table
above claims, but `queries/highlights.scm` opens with *"These scopes
are arbitrary and line up with good colors for the `tree-sitter
highlight` command"* — `addition` → `@string`, `deletion` →
`@keyword`. We ship our own `queries/diff/highlights.scm`, as markdown
already does. Critically `(addition)` **must** map to `Style::DiffAdd`
and `(deletion)` to `Style::DiffRemove`: MG.21a's
`Editor::diff_signs_from_spans` finds the row tint by looking for those
two styles on the row, so any other mapping silently removes the
background tint from every magit diff — the very thing MG.21a added.

**Scope, when it is picked up.** Syntax install is
`Lang::detect_from_path`-driven, not major-mode-driven
(`dispatch.rs:5766`), so adding `.diff` / `.patch` detection lights up
ordinary patch files with no major-mode work;
`major_mode_id_for_lang(Diff) → None` is the honest arm. A `diff-mode`
major was considered and rejected — it would own no keymap, lifecycle
or options, an abstraction ahead of its requirement. And the parse must
move *inside* each site's `spawn_blocking`: all eight style after the
await today, which is fine for a prefix ladder and not for a parse.

## Why the current output looked flat — fixed by MG.21a

Independent of the parser question, and landed first.

Magit's diff views map their spans to `Style::DiffAdd` / `DiffRemove`,
which resolve to `diff.add.text` / `diff.remove.text` — **foreground
only** (`spec().fg("green")`). `diff.add.line` / `diff.remove.line`
carry the **background tints** (`spec().bg("diff.add.bg")`).

An earlier revision of this document called that gap "an
element-mapping change". **That was wrong**, and the correction is the
interesting part: the `diff.*.line` elements are never applied through
a `StyledSpan` at all. Both renderers apply them as a **full-row
background**, keyed on a `DiffSignMap` looked up by buffer id
(`diff_tint_bg` in the TUI, the `theme_ids.diff_*_line` reads in
GPUI's `window.rs`). Sign maps were built solely from live
`DiffSession`s — and a buffer whose *content* is a diff has no session,
because there is no baseline to diff it against. So those elements were
unreachable from magit no matter how the spans were mapped.

MG.21a closes it by **deriving** the sign map from the spans the mode
already publishes: in `drain_pending_synthetic_highlights`, a row
styled `DiffAdd` / `DiffRemove` yields an `Add` / `Remove` sign, and
`diff_sign_maps_by_buffer` merges those into the same map both
renderers already read. Consequences worth recording:

- **No renderer change in either peer**, so the TUI/GPUI parity rule is
  satisfied by construction rather than by a matched pair of edits.
- **No producer change at any of the 8 span sites** — including
  magit-status's inline `=` expansion, which splices spans mid-buffer.
  Deriving from the post-splice spans means the tint cannot drift out
  of alignment with the text; a parallel signs channel would have had
  to repeat the splice arithmetic and could.
- **Conditioned on the style, not on the mode or `BufferKind`.** Any
  synthetic buffer that marks a row as a diff addition gets the tint.
- Gutter `+`/`-` glyphs do *not* appear, because those come from
  `mode.gutter_decorations` on modes active in the buffer, and
  `diff-mode` is not active in a magit buffer. The sign map drives the
  tint; the active mode drives the glyphs. Desirable here — the text
  already begins with `+`/`-`.

Emacs magit also does **word-level** intra-line highlighting (the
changed span within a modified line gets a stronger tint). That needs a
word-diff, which neither the grammar nor the current pipeline
produces — see "Open".

## Options this mode should own

MG.22b registered the first options `lattice-magit` has ever had; before it, the crate had none.

| Option | Type | Purpose | State |
|---|---|---|---|
| `magit.hunk.context-lines` | int | `-U<n>` for the diffs magit generates | ✅ MG.22b, default 3 |
| `magit.hunk.syntax-highlight` | bool | language-aware hunk content, once it exists | ⛔ **not registered** — the feature does not exist, and an option that changes nothing is a menu row that does nothing with a quieter failure mode (`:set` reports success). Lands with the feature, which is now itself deferred past v1. |
| ~~`magit.hunk.line-backgrounds`~~ | bool | opt *out* of the `diff.*.line` tints | ✅ MG.22b, but as **`ui.diff.line-backgrounds`** in `lattice-diff`. MG.21a found the mechanism generic — `Editor::diff_signs_from_spans` derives the tint from any mode's spans — so naming it for magit would understate what it turns off. |

## Open

- **Language-aware hunk content.** Highlight the code inside a hunk in
  its own language. Needs per-file sub-highlighting driven by the diff
  header's path, not injections.
- **Word-level intra-line diff**, as emacs magit does. Needs a word-diff
  the pipeline does not currently produce.
- **Inline (gutter/overlay) diffs are out of scope.** This mode is
  about buffers whose *content* is a diff. The overlay path in
  `lattice-diff` — signs, deletion blocks on ordinary file buffers —
  remains unowned by any mode; noted separately in the help-docs slice
  plan.

## Cross-references

- [`magit.md`](magit.md) — the subsystem this serves
- [`diff-system.md`](diff-system.md) — the diff subsystem and its overlay path
- [`mode-architecture.md`](mode-architecture.md) — Mode trait, minors, `MagitView`
