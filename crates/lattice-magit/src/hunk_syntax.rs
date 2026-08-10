//! DS.1 — syntax spans for the code inside a unified diff.
//!
//! Owned by `magit-hunk-mode`, which activates on precisely the five
//! diff-bearing majors and already owns what is inside the diff.
//!
//! This produces only the SYNTAX layer. It knows nothing about diff
//! colouring; composing the two is DS.2's job, and the contract that
//! lets them compose without merging is
//! `docs/dev/architecture/span-layering.md`.
//!
//! ## Fragments, not files
//!
//! A hunk begins mid-file, so tree-sitter sees an unbalanced fragment
//! and produces an ERROR node at the top. That is accepted
//! deliberately: the failure direction is right. Tokens that resolve
//! (keywords, strings, comments, numbers) come out correct, and tokens
//! that need enclosing context come out UNCOLOURED — never coloured
//! wrong. The worst case degrades to exactly the appearance before
//! this existed. See the design fragment §6 for the blob-accurate
//! alternative and why it is deferred.

use std::path::Path;
use std::sync::Arc;

use lattice_syntax::{Lang, LangRegistry, StyledSpan, oneshot_highlight_lines};

use crate::highlight::DiffLineClass;

/// The `+` / `-` / space column every diff content line carries.
///
/// One ASCII byte and one column, so it serves as both a byte offset
/// into the line and a shift for the syntax spans — which are
/// themselves byte offsets. Spans are moved right by this much so they
/// address the DIFF's bytes rather than the reconstructed source's.
const MARKER: usize = 1;

/// A hunk's lines, split into the two sides tree-sitter can parse.
///
/// Context lines belong to BOTH sides — they are unchanged, so they
/// appear in the old file and the new one. That is what makes the
/// reconstruction parseable at all: without the context, an added line
/// would be a lone fragment with nothing around it.
#[derive(Default)]
struct HunkSides {
    /// `(diff line index, reconstructed-source line index)` for the
    /// new side — context + added.
    new_map: Vec<(usize, usize)>,
    new_text: String,
    /// Same for the old side — context + removed.
    old_map: Vec<(usize, usize)>,
    old_text: String,
}

impl HunkSides {
    fn push(&mut self, diff_line: usize, marker: u8, content: &str) {
        // The source line index is `map.len()`, not a re-count of the
        // text: one line is appended per entry, so they advance
        // together. Counting `text.lines()` on every push would make
        // reconstruction O(hunk²) for no gain.
        if marker == b' ' || marker == b'+' {
            self.new_map.push((diff_line, self.new_map.len()));
            self.new_text.push_str(content);
            self.new_text.push('\n');
        }
        if marker == b' ' || marker == b'-' {
            self.old_map.push((diff_line, self.old_map.len()));
            self.old_text.push_str(content);
            self.old_text.push('\n');
        }
    }

    fn is_empty(&self) -> bool {
        self.new_map.is_empty() && self.old_map.is_empty()
    }
}

/// Per-line syntax spans for `diff`, indexed by the DIFF's own line
/// numbering and already offset past the change marker.
///
/// Lines that carry no code — `diff --git`, `@@`, `---` / `+++`, the
/// `\ No newline` note — get an empty vec, as do files whose extension
/// has no registered grammar.
///
/// One `Vec` per line of `diff`, so the result can be zipped straight
/// against the diff text.
pub(crate) fn syntax_spans_for_diff(
    diff: &str,
    registry: Arc<LangRegistry>,
) -> Vec<Vec<StyledSpan>> {
    let mut out: Vec<Vec<StyledSpan>> = vec![Vec::new(); diff.lines().count()];
    let mut lang = Lang::Plain;
    let mut hunk = HunkSides::default();

    for (i, line) in diff.lines().enumerate() {
        // `classify_diff_line` is the single prefix ladder for diff
        // text and stays that way — this walk refines exactly one of
        // its answers rather than repeating it. `Context` covers both
        // real context lines (leading space) and the inter-file
        // metadata git emits (`index abc..def`, `new file mode`), and
        // only the former is code.
        match crate::highlight::classify_diff_line(line) {
            DiffLineClass::FileCommand => {
                // A new file starts: the hunk before it is complete,
                // and the language may change.
                flush_hunk(&mut hunk, lang, &registry, &mut out);
                lang = line
                    .strip_prefix("diff --git ")
                    .map(lang_from_diff_header)
                    .unwrap_or(Lang::Plain);
            }
            // Hunks are parsed one at a time rather than per file: two
            // hunks from the same file are not contiguous source, and
            // concatenating them would invent adjacency the file does
            // not have.
            DiffLineClass::Hunk => flush_hunk(&mut hunk, lang, &registry, &mut out),
            // `---` / `+++` name the files. Not code, and not a change.
            DiffLineClass::FilePath => {}
            DiffLineClass::Added => hunk.push(i, b'+', &line[MARKER..]),
            DiffLineClass::Removed => hunk.push(i, b'-', &line[MARKER..]),
            DiffLineClass::Context => match line.as_bytes().first() {
                Some(b' ') => hunk.push(i, b' ', &line[MARKER..]),
                // `\ No newline at end of file` sits INSIDE a hunk —
                // between the removed side's last line and the added
                // side's — so it is skipped, not treated as a
                // boundary. Flushing here would split the hunk in two
                // and strip the second half of its context.
                Some(b'\\') => {}
                // `index abc..def`, `new file mode`, `similarity
                // index`, blank separators: metadata BETWEEN files, and
                // a real boundary.
                _ => flush_hunk(&mut hunk, lang, &registry, &mut out),
            },
        }
    }
    flush_hunk(&mut hunk, lang, &registry, &mut out);

    out
}

/// Parse both sides of one completed hunk and write their spans into
/// `out`, then reset `hunk` for the next one.
///
/// A no-op without a grammar, which is how an unsupported language
/// degrades: no spans, so the diff renders exactly as it did before.
fn flush_hunk(
    hunk: &mut HunkSides,
    lang: Lang,
    registry: &Arc<LangRegistry>,
    out: &mut [Vec<StyledSpan>],
) {
    let sides = std::mem::take(hunk);
    if sides.is_empty() || matches!(lang, Lang::Plain) {
        return;
    }
    for (text, map) in [
        (&sides.new_text, &sides.new_map),
        (&sides.old_text, &sides.old_map),
    ] {
        if map.is_empty() {
            continue;
        }
        let Some(spans) =
            oneshot_highlight_lines(lang, registry.clone(), text, 0, map.len() as u32)
        else {
            continue;
        };
        for (diff_line, src_line) in map {
            let (Some(row), Some(slot)) = (spans.get(*src_line), out.get_mut(*diff_line)) else {
                continue;
            };
            // A context line belongs to both sides. The new side is
            // written first and keeps the slot: it is the file's
            // current state, and the old side's parse would yield the
            // same tokens from a staler tree.
            if !slot.is_empty() {
                continue;
            }
            *slot = row
                .iter()
                .map(|s| StyledSpan {
                    start: s.start + MARKER,
                    end: s.end + MARKER,
                    style: s.style,
                })
                .collect();
        }
    }
}

/// Resolve the language from a `diff --git a/… b/…` header's tail.
///
/// The b-side (destination) is preferred: for a rename the two differ,
/// and the destination is what the file is now. Falls back to the
/// a-side for a deletion, where the b-side is `/dev/null`.
fn lang_from_diff_header(rest: &str) -> Lang {
    let mut a = None;
    let mut b = None;
    for tok in rest.split_whitespace() {
        if let Some(p) = tok.strip_prefix("a/") {
            a = Some(p);
        } else if let Some(p) = tok.strip_prefix("b/") {
            b = Some(p);
        }
    }
    let pick = b.filter(|p| *p != "/dev/null").or(a);
    match pick {
        Some(p) => Lang::detect_from_path(Some(Path::new(p))),
        None => Lang::Plain,
    }
}

/// The spans for a unified diff — the ONE entry point every magit view
/// that shows a diff calls.
///
/// `None` registry ⇒ the flat classifier, exactly as before syntax
/// layering existed. That is the designed degradation, not an error
/// path: a harness without the service, or a build without grammars,
/// renders the diff the way it always did.
///
/// One function rather than a `match` repeated at each call site, so
/// the five views cannot drift — and so DS.5's option gate has a
/// single place to live.
pub(crate) fn diff_spans(diff: &str, registry: Option<&Arc<LangRegistry>>) -> Vec<Vec<StyledSpan>> {
    match registry {
        Some(r) => layered_diff_spans(diff, r.clone()),
        None => crate::highlight::diff_styled_spans(diff),
    }
}

/// DS.5 — the grammar registry to highlight with, or `None`.
///
/// `None` when there is no grammar service (a stripped harness) or
/// when `magit.hunk.syntax-highlight` is off. Both collapse to the
/// same answer on purpose: the flat classifier is the degradation
/// path, so the option turns the feature off by taking the same route
/// a missing grammar already takes.
///
/// Resolved at USE time rather than stored, so `:set` lands on the
/// next refresh — the contract `magit.hunk.context-lines` set.
pub(crate) fn syntax_registry(
    registry: Option<Arc<LangRegistry>>,
    config: Option<&Arc<lattice_config::ConfigRegistry>>,
) -> Option<Arc<LangRegistry>> {
    let enabled = config
        .and_then(|c| c.get_typed::<crate::options::MagitHunkSyntaxHighlight>())
        .map(|v| *v)
        // No config registry ⇒ the option's own default, not `false`:
        // a harness without config should behave like a default install.
        .unwrap_or(true);
    registry.filter(|_| enabled)
}

/// DS.4 — spans for a buffer whose diff occupies only part of it.
///
/// The commit buffer is a message region, a marker line, then the
/// staged diff. Its own marker starts with `---`, which the diff
/// classifier would read as a file header — so the diff region is
/// SLICED OUT and styled alone, rather than styling the whole buffer
/// and hoping the marker survives. The window is the invariant, and
/// this keeps it literal.
///
/// `diff_start_line` / `diff_end_line` are inclusive-exclusive line
/// indices into `text`. Rows outside get no spans.
pub(crate) fn windowed_diff_spans(
    text: &str,
    diff_start_line: usize,
    diff_end_line: usize,
    registry: Option<&Arc<LangRegistry>>,
) -> Vec<Vec<StyledSpan>> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Vec<StyledSpan>> = vec![Vec::new(); lines.len()];
    let end = diff_end_line.min(lines.len());
    if diff_start_line >= end {
        return out;
    }
    let region = lines[diff_start_line..end].join("\n");
    for (i, row) in diff_spans(&region, registry).into_iter().enumerate() {
        if let Some(slot) = out.get_mut(diff_start_line + i) {
            *slot = row;
        }
    }
    out
}

/// DS.2 — the layered spans for a unified diff: diff colouring on top,
/// syntax underneath.
///
/// Composition is CONCATENATION in precedence order, not a merge.
/// `cells_worker::style_at_byte` resolves a byte to the FIRST span
/// covering it, so pushing the diff layer first makes it win, and the
/// syntax layer shows through everywhere the diff layer does not
/// reach. Nothing is split and no combined value is computed — which
/// is what lets a third overlay be added later at zero cost. See
/// `docs/dev/architecture/span-layering.md`.
///
/// The diff layer claims:
///
/// * a **whole line** for headers (`diff --git`, `@@`, `---`/`+++`) —
///   there is no code on them to show through;
/// * the **single marker column** for content lines.
///
/// Narrowing that second claim from the whole line to one byte is the
/// entire change. It is also what keeps
/// `Editor::diff_signs_from_spans` working untouched: it looks for the
/// PRESENCE of a `DiffAdd` / `DiffRemove` span on the row, not its
/// extent, so the row's background tint still resolves.
pub(crate) fn layered_diff_spans(diff: &str, registry: Arc<LangRegistry>) -> Vec<Vec<StyledSpan>> {
    let syntax = syntax_spans_for_diff(diff, registry);
    diff.lines()
        .enumerate()
        .map(|(i, line)| {
            let class = crate::highlight::classify_diff_line(line);
            let mut out = Vec::new();
            if let Some(style) = class.style() {
                // Content lines cede everything past the marker to the
                // syntax layer; headers keep the whole line.
                let end = if class.is_content() {
                    MARKER.min(line.len())
                } else {
                    line.len()
                };
                if end > 0 {
                    out.push(StyledSpan {
                        start: 0,
                        end,
                        style,
                    });
                }
            }
            if let Some(row) = syntax.get(i) {
                out.extend(row.iter().copied());
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> Arc<LangRegistry> {
        LangRegistry::standard().expect("standard registry")
    }

    const RUST_DIFF: &str = "\
diff --git a/src/a.rs b/src/a.rs
index 1234567..89abcde 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@
 fn main() {
-    let old = 1;
+    let new = 2;
+    println!(\"hi\");
 }
";

    fn spans(diff: &str) -> Vec<Vec<StyledSpan>> {
        syntax_spans_for_diff(diff, registry())
    }

    #[test]
    fn result_is_one_entry_per_diff_line() {
        let out = spans(RUST_DIFF);
        assert_eq!(out.len(), RUST_DIFF.lines().count());
    }

    #[test]
    fn header_lines_carry_no_syntax() {
        let out = spans(RUST_DIFF);
        for (i, line) in RUST_DIFF.lines().enumerate() {
            if line.starts_with("diff --git")
                || line.starts_with("@@")
                || line.starts_with("---")
                || line.starts_with("+++")
                || line.starts_with("index ")
            {
                assert!(
                    out[i].is_empty(),
                    "line {i} ({line:?}) is not code and must carry no syntax"
                );
            }
        }
    }

    /// The whole point: code lines get spans, on both sides of the
    /// change. Added, removed and context all carry syntax — a removed
    /// line is still code and still worth reading.
    #[test]
    fn added_removed_and_context_lines_all_get_syntax() {
        let out = spans(RUST_DIFF);
        let idx = |needle: &str| {
            RUST_DIFF
                .lines()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} not in fixture"))
        };
        for needle in ["fn main", "let old", "let new", "println!"] {
            assert!(
                !out[idx(needle)].is_empty(),
                "{needle:?} is code and must carry syntax spans"
            );
        }
    }

    /// Spans are in DIFF coordinates, not source coordinates — shifted
    /// right past the `+` / `-` / space column. Without the shift every
    /// token would paint one byte left of its glyph.
    #[test]
    fn spans_are_offset_past_the_change_marker() {
        let out = spans(RUST_DIFF);
        let line_idx = RUST_DIFF
            .lines()
            .position(|l| l.starts_with(" fn main"))
            .expect("context line");
        let first = out[line_idx].first().expect("a span on `fn main() {`");
        assert!(
            first.start >= MARKER,
            "a span must not start inside the marker column: {first:?}"
        );
        // `fn` is the first token of " fn main() {" — byte 1 in diff
        // coordinates, byte 0 in source coordinates.
        assert_eq!(
            first.start, 1,
            "the `fn` keyword starts right of the marker"
        );
    }

    #[test]
    fn a_file_with_no_registered_grammar_yields_no_spans() {
        let diff = "\
diff --git a/notes.xyzzy b/notes.xyzzy
@@ -1,1 +1,1 @@
-old
+new
";
        assert!(
            spans(diff).iter().all(|r| r.is_empty()),
            "an unknown extension must degrade to no syntax, not fail"
        );
    }

    /// The b-side names what the file IS now, which is what should be
    /// parsed. For a rename the two sides differ and only the
    /// destination is right.
    #[test]
    fn language_comes_from_the_destination_path() {
        assert_eq!(
            lang_from_diff_header("a/old.txt b/new.rs"),
            Lang::detect_from_path(Some(Path::new("new.rs")))
        );
        // A deletion has no usable b-side, so the a-side stands in.
        assert_eq!(
            lang_from_diff_header("a/gone.rs b//dev/null"),
            Lang::detect_from_path(Some(Path::new("gone.rs")))
        );
    }

    /// Two files in one diff each parse as their own language, and the
    /// second file's hunk must not be parsed with the first's grammar.
    #[test]
    fn each_file_in_a_multi_file_diff_uses_its_own_language() {
        let diff = "\
diff --git a/a.rs b/a.rs
@@ -1,1 +1,1 @@
-fn a() {}
+fn b() {}
diff --git a/b.xyzzy b/b.xyzzy
@@ -1,1 +1,1 @@
-fn a() {}
+fn b() {}
";
        let out = spans(diff);
        let rust_line = diff
            .lines()
            .position(|l| l == "+fn b() {}")
            .expect("rust add");
        let unknown_line = diff
            .lines()
            .enumerate()
            .filter(|(_, l)| *l == "+fn b() {}")
            .map(|(i, _)| i)
            .last()
            .expect("unknown add");
        assert!(!out[rust_line].is_empty(), "the .rs hunk is highlighted");
        assert!(
            out[unknown_line].is_empty(),
            "the .xyzzy hunk has no grammar and must stay unhighlighted \
             even though its text is identical Rust"
        );
    }

    /// Separate hunks are not contiguous source. Parsing them together
    /// would invent adjacency the file does not have — the last line of
    /// one hunk is not followed by the first line of the next.
    #[test]
    fn hunks_are_parsed_independently() {
        let diff = "\
diff --git a/a.rs b/a.rs
@@ -1,1 +1,1 @@
+fn first() {}
@@ -50,1 +50,1 @@
+fn second() {}
";
        let out = spans(diff);
        for needle in ["+fn first() {}", "+fn second() {}"] {
            let i = diff.lines().position(|l| l == needle).expect(needle);
            assert!(!out[i].is_empty(), "{needle} must be highlighted");
        }
    }

    /// A fragment that cannot parse cleanly still yields spans for the
    /// tokens it did resolve — the failure direction the design chose.
    #[test]
    fn an_unbalanced_fragment_still_highlights_what_it_can() {
        let diff = "\
diff --git a/a.rs b/a.rs
@@ -10,2 +10,2 @@
-    let x = \"unterminated
+    let y = 3;
         }
";
        let out = spans(diff);
        let i = diff
            .lines()
            .position(|l| l == "+    let y = 3;")
            .expect("add line");
        assert!(
            !out[i].is_empty(),
            "a resolvable token in a broken fragment must still be styled"
        );
    }

    // ── DS.2: the layered composition ──────────────────────

    fn layered(diff: &str) -> Vec<Vec<StyledSpan>> {
        layered_diff_spans(diff, registry())
    }

    /// The precedence rule, asserted the way the renderer resolves it:
    /// `style_at_byte` takes the FIRST span covering a byte.
    fn style_at(spans: &[StyledSpan], byte: usize) -> Option<lattice_cells::style::Style> {
        spans
            .iter()
            .find(|s| byte >= s.start && byte < s.end)
            .map(|s| s.style)
    }

    /// The marker column belongs to the diff layer; the code past it
    /// belongs to syntax. This is the whole feature in one assertion.
    #[test]
    fn the_marker_is_diff_coloured_and_the_code_is_not() {
        let out = layered(RUST_DIFF);
        let i = RUST_DIFF
            .lines()
            .position(|l| l.starts_with("+    let new"))
            .expect("add line");
        assert_eq!(
            style_at(&out[i], 0),
            Some(lattice_cells::style::Style::DiffAdd),
            "the `+` column stays diff-coloured"
        );
        let code = style_at(&out[i], 5);
        assert!(
            code.is_some() && code != Some(lattice_cells::style::Style::DiffAdd),
            "code past the marker resolves to a syntax style, got {code:?}"
        );
    }

    /// `Editor::diff_signs_from_spans` finds a row's sign by the
    /// PRESENCE of a `DiffAdd` / `DiffRemove` span, whatever its
    /// extent. Narrowing the span to one byte must not cost the row its
    /// background tint — this is why the host needs no change.
    #[test]
    fn the_row_tint_still_resolves_from_a_one_byte_marker() {
        let out = layered(RUST_DIFF);
        for (needle, want) in [
            ("+    let new", lattice_cells::style::Style::DiffAdd),
            ("-    let old", lattice_cells::style::Style::DiffRemove),
        ] {
            let i = RUST_DIFF
                .lines()
                .position(|l| l.starts_with(needle))
                .unwrap_or_else(|| panic!("{needle} in fixture"));
            assert!(
                out[i].iter().any(|s| s.style == want),
                "{needle}: the row must still carry a {want:?} span for the sign map"
            );
        }
    }

    /// Header lines have no code, so the diff layer keeps all of them.
    #[test]
    fn headers_are_claimed_whole() {
        let out = layered(RUST_DIFF);
        for (i, line) in RUST_DIFF.lines().enumerate() {
            if line.starts_with("@@") || line.starts_with("diff --git") {
                let covers_all = out[i]
                    .first()
                    .is_some_and(|s| s.start == 0 && s.end == line.len());
                assert!(covers_all, "header {line:?} must be claimed whole");
            }
        }
    }

    /// A context line carries no diff style at all, so syntax owns it
    /// outright — including the marker column, which is a plain space.
    #[test]
    fn context_lines_are_all_syntax() {
        let out = layered(RUST_DIFF);
        let i = RUST_DIFF
            .lines()
            .position(|l| l.starts_with(" fn main"))
            .expect("context line");
        assert!(
            out[i]
                .iter()
                .all(|s| s.style != lattice_cells::style::Style::DiffAdd
                    && s.style != lattice_cells::style::Style::DiffRemove),
            "a context line is not a change and carries no diff span"
        );
        assert!(!out[i].is_empty(), "but it is still code, and still styled");
    }

    /// Composition must not disturb the row count — the caller zips
    /// these against the diff's lines.
    #[test]
    fn layering_preserves_one_row_per_line() {
        assert_eq!(layered(RUST_DIFF).len(), RUST_DIFF.lines().count());
    }

    /// With no grammar for the file, the result must be exactly what
    /// the unlayered classifier produced — the feature degrades to the
    /// previous appearance rather than to something new.
    #[test]
    fn without_a_grammar_the_diff_layer_still_stands_alone() {
        let diff = "\
diff --git a/notes.xyzzy b/notes.xyzzy
@@ -1,1 +1,1 @@
-old
+new
";
        let out = layered(diff);
        let i = diff.lines().position(|l| l == "+new").expect("add");
        assert_eq!(
            style_at(&out[i], 0),
            Some(lattice_cells::style::Style::DiffAdd)
        );
        assert!(
            style_at(&out[i], 1).is_none(),
            "no grammar ⇒ nothing under the diff layer"
        );
    }

    /// `\ No newline at end of file` sits INSIDE a hunk, between the
    /// two sides. Treating it as a boundary would split the hunk and
    /// strip the added side of its context — the lines would still be
    /// highlighted, but from a smaller and less parseable fragment.
    #[test]
    fn the_no_newline_marker_does_not_split_a_hunk() {
        let diff = "\
diff --git a/a.rs b/a.rs
@@ -1,2 +1,2 @@
 fn main() {
-    let old = 1;
\\ No newline at end of file
+    let new = 2;
 }
";
        let out = spans(diff);
        let i = diff
            .lines()
            .position(|l| l.starts_with("+    let new"))
            .expect("add line");
        assert!(
            !out[i].is_empty(),
            "the added line is still inside the hunk and must be highlighted"
        );
        let marker = diff
            .lines()
            .position(|l| l.starts_with('\\'))
            .expect("no-newline marker");
        assert!(
            out[marker].is_empty(),
            "the marker itself is not code and carries no syntax"
        );
    }

    /// Migrated from `highlight::commit_buffer_styled_spans`, which
    /// this replaced. The invariant is the commit buffer's own
    /// `--- Staged diff ---` marker: it starts with `---` and would be
    /// read as a diff FILE HEADER if the whole buffer were styled at
    /// once. Slicing the region out is what keeps that impossible
    /// rather than merely unlikely.
    #[test]
    fn the_commit_buffers_own_marker_is_never_styled_as_a_diff_header() {
        let text = "--- Staged diff (review before committing) ---\n\
                    +added\n\
                    --- Commit message (edit below) ---\n\
                    my message\n";
        // Line 1 is the only diff content: [1, 2).
        let spans = windowed_diff_spans(text, 1, 2, None);
        assert!(
            spans[0].is_empty(),
            "the header must not be coloured as a diff file marker"
        );
        assert_eq!(spans[1][0].style, lattice_cells::style::Style::DiffAdd);
        assert!(
            spans[2].is_empty(),
            "the message marker must stay unstyled despite starting with ---"
        );
        assert!(spans[3].is_empty(), "the message itself is not diff text");
    }

    /// The window also holds with a grammar in play — syntax must not
    /// leak outside the diff region.
    #[test]
    fn the_window_holds_with_syntax_enabled() {
        let text = "--- Staged diff ---\n\
                    diff --git a/a.rs b/a.rs\n\
                    @@ -1,1 +1,1 @@\n\
                    +fn main() {}\n\
                    --- Commit message ---\n\
                    my message\n";
        let spans = windowed_diff_spans(text, 1, 4, Some(&registry()));
        assert!(spans[0].is_empty(), "header outside the window");
        let added = 3;
        assert!(
            spans[added].len() > 1,
            "the added line carries the diff marker plus syntax"
        );
        assert!(spans[4].is_empty(), "message marker outside the window");
        assert!(spans[5].is_empty(), "message outside the window");
    }

    // ── DS.5: the option gate ──────────────────────────────

    /// Off ⇒ no registry ⇒ the flat classifier. The option turns the
    /// feature off by the same route a missing grammar takes, so there
    /// is one degradation path rather than two.
    #[test]
    fn the_option_off_falls_back_to_the_flat_classifier() {
        let config = Arc::new(lattice_config::ConfigRegistry::new());
        // `options! { … }` is a compile-time declaration; boot is what
        // makes it a runtime fact in a registry.
        config.init_from_linkme();
        config
            .set_typed::<crate::options::MagitHunkSyntaxHighlight>(false)
            .expect("the option is declared, so it can be set");
        assert!(
            syntax_registry(Some(registry()), Some(&config)).is_none(),
            "syntax-highlight=off must yield no registry"
        );
    }

    #[test]
    fn the_option_defaults_on_and_survives_a_missing_config() {
        assert!(
            syntax_registry(Some(registry()), None).is_some(),
            "a harness without config must behave like a default install"
        );
        let config = Arc::new(lattice_config::ConfigRegistry::new());
        assert!(
            syntax_registry(Some(registry()), Some(&config)).is_some(),
            "an unregistered option falls back to its default, which is on"
        );
        let booted = Arc::new(lattice_config::ConfigRegistry::new());
        booted.init_from_linkme();
        assert!(
            syntax_registry(Some(registry()), Some(&booted)).is_some(),
            "and the declared default is on"
        );
    }

    /// No grammar service ⇒ `None` regardless of the option.
    #[test]
    fn no_registry_stays_none_however_the_option_is_set() {
        assert!(syntax_registry(None, None).is_none());
    }

    #[test]
    fn an_empty_diff_is_handled() {
        assert!(spans("").is_empty());
    }
}
