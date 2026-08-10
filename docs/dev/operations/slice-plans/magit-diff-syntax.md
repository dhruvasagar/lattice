# Slice plan — syntax highlighting inside magit diffs

Design: [`span-layering`](../../architecture/span-layering.md) §6.

Sequencing note: the slices are ordered so that **no slice ships a
visible regression on its own**. Narrowing the marker span (the
enabling change) would, by itself, leave diff text uncoloured until
syntax arrives — so the narrowing and the syntax land together in
DS.3, after both halves exist and are tested in isolation.

| Slice | What | Status |
|---|---|---|
| DS.1 | Hunk reconstruction + syntax spans, pure | ✅ |
| DS.2 | The layered composition, pure | ✅ |
| DS.3 | Wire magit-status | ✅ |
| DS.4 | Wire diff / revision / stash-show / commit | 📝 |
| DS.5 | Option gate + user docs | 📝 |
| DS.6 | Blob-accurate parse for `magit-diff-mode` | ⛔ |

---

## DS.1 — hunk reconstruction + syntax spans ✅

Pure functions, no wiring, no behaviour change.

- Split a unified diff into per-file regions; resolve each file's
  `Lang` from the `diff --git a/… b/…` header via the existing
  path→language lookup.
- Per hunk, reconstruct both sides: **new** = context + added lines,
  **old** = context + removed lines, markers stripped.
- Parse each reconstructed side once (`Syntax::for_language` →
  `parse` → `highlight_lines`) and map the resulting per-line spans
  back onto the diff's own line numbering.

Tests: reconstruction (a hunk with adds, removes, and both);
line-number mapping for a multi-hunk, multi-file diff; a file whose
extension has no registered grammar yields no spans rather than
failing; a fragment that will not parse cleanly still yields spans for
the tokens it did resolve.

**Done when** the mapping is exercised without a renderer or a repo.

Landed. A design-review pass over DS.1–DS.3 before commit found three
things worth recording, because each is a rule this repo already
holds:

- **A second prefix ladder.** `syntax_spans_for_diff` re-implemented
  the classification `classify_diff_line` already owns — a function
  whose doc calls it "the single prefix ladder". It now builds on that
  one and refines exactly its `Context` answer, which conflates real
  context lines with inter-file metadata.
- **A stale comment**, describing a closure signature the code no
  longer had. The same defect class as the two shipped-feature-denied
  docs fixed earlier in this branch, caught this time before commit.
- **A regression the tests did not catch.** Routing
  `\ No newline at end of file` through the metadata branch made it a
  hunk boundary — but it sits INSIDE a hunk, between the two sides, so
  it split the hunk and stripped the added side of its context. Found
  by re-reading rather than by a failure, and now pinned by
  `the_no_newline_marker_does_not_split_a_hunk`.

## DS.2 — the layered composition ✅

The concatenation seam, in `magit-hunk-mode`.

- `diff_spans_with_syntax(diff) -> Vec<Vec<StyledSpan>>` emitting, per
  line, in precedence order: header span (whole line) **or** marker
  span (one byte), then the syntax spans offset by the marker width.
- Still pure — DS.3 changes what callers use.

Tests: precedence (a byte covered by both layers resolves to the diff
layer, per `style_at_byte`'s first-match rule); the marker column
keeps `DiffAdd`/`DiffRemove` so `diff_signs_from_spans` still derives
the row tint; syntax offsets land right of the marker; header lines
carry no syntax; a line with multi-byte content keeps correct byte
offsets.

## DS.3 — wire magit-status ✅

Switch `actions.rs`'s inline-expand span call to the new seam. First
slice with a visible change; the row tint and marker colour must be
unchanged from before, with code text now syntax-coloured.

## DS.4 — wire the remaining views 📝

`magit-diff-mode` (3 call sites), `magit-revision-mode`, and the
commit buffer's staged-diff region. Split from DS.3 so a problem in
the shared seam surfaces on one surface before it reaches five.

The commit buffer needs care: `commit_buffer_styled_spans` deliberately
restricts diff colouring to `[diff_start_line, diff_end_line)` so the
buffer's own `--- Staged diff ---` header is not misread as a diff
`---` marker. That windowing has to survive.

## DS.5 — option gate + user docs 📝

`magit.diff.syntax` (default on) so the parse can be turned off, and a
paragraph in the magit user page. Deferred behind the wiring
deliberately: whether an option is warranted depends on what DS.3/DS.4
cost in practice, and inventing one first would be guessing.

## DS.6 — blob-accurate parse ⛔

Deferred, not abandoned. For `magit-diff-mode`, parse the file at the
revision and slice the hunk's lines out of a full-file parse, so
context-dependent highlighting is right. Justified there by attention
(one file, deliberately opened) and not justified in status, where
most of the content is off-screen. Revisit if fragment parsing proves
visibly wrong on real code rather than merely incomplete.
