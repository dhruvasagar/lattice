# magit-hunk-mode — the mode that owns diff *content*

**Status:** designed 2026-07-29, not implemented. Slice plan:
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

1. **Structural highlighting** of the diff — see "Parsing" below.
2. **`<CR>`** — one handler, one diff-path parser.
3. **Options** — `magit.hunk.*`, which today do not exist anywhere.
4. **Navigation within diff content**, eventually — `]c` / `[c` already
   come from `magit-core-mode`; hunk-scoped folds could move here.

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

### The wrinkle: a minor supplying a parser

Lattice attaches parsers through the **major** (`Lang` →
`major_mode_id_for_lang` → `DocumentSyntax` buffer-local). A magit
buffer's major is `magit-*`, not a language mode, so `magit-hunk-mode`
cannot get a parser the usual way.

Resolve before implementing. The likely answer is that the mode
registers `"diff"` in the `LangRegistry` and sets the buffer's
`DocumentSyntax` local itself on activation — the same slot
`activate_document` writes — but this needs checking against the
syntax worker's assumptions about who owns that local.

## Why the current output looks flat

Worth separating from the parser question, because it is independent
and cheaper to fix.

Magit's diff views map their spans to `Style::DiffAdd` / `DiffRemove`,
which resolve to `diff.add.text` / `diff.remove.text` — **foreground
only** (`spec().fg("green")`). Meanwhile `diff.add.line` /
`diff.remove.line` already exist and carry **background tints**
(`spec().bg("diff.add.bg")`), and the gutter/overlay diff path already
uses them.

So the "emacs magit looks richer" gap is largely that the line-level
background elements are never applied in these buffers. That is an
element-mapping change, not a parsing change, and it lands whether or
not tree-sitter-diff does.

Emacs magit also does **word-level** intra-line highlighting (the
changed span within a modified line gets a stronger tint). That needs a
word-diff, which neither the grammar nor the current pipeline
produces — see "Open".

## Options this mode should own

None of these exist today; `lattice-magit` registers no options at all.

| Option | Type | Purpose |
|---|---|---|
| `magit.hunk.context-lines` | int | `-U<n>` for the diffs magit generates |
| `magit.hunk.syntax-highlight` | bool | language-aware hunk content, once it exists |
| `magit.hunk.line-backgrounds` | bool | the `diff.*.line` tints above |

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
