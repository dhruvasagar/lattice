//! MG.18b: locating the hunk at a cursor, and turning it into a patch.
//!
//! Pure functions over diff text. No buffer, no store, no `Editor` —
//! `magit.md` §7.2's substrate-helper pattern, and what lets MG.22
//! relocate this into `magit-hunk-mode` rather than rewrite it.
//!
//! # Why lines arrive through an accessor
//!
//! [`hunk_at_with`] pulls lines one at a time by index rather than
//! taking a slice. Its production caller reads a buffer snapshot, and
//! a `*magit:diff*` buffer can hold tens of thousands of lines —
//! materialising all of them to stage one hunk would put an
//! O(document) copy on a keystroke path (paramount #1). Through the
//! accessor the read is O(distance to the file header + hunk body),
//! which is the work the answer actually costs. The slice form below
//! is the same parser, adapted for tests.
//!
//! # Why the buffer text is the source of truth
//!
//! `magit.md` §7.5: hunk boundaries are *already* derived from buffer
//! text. `]c` / `[c` walk `hunk_lines` in `magit_core_mode.rs` — a raw
//! scan for `@@` / `diff --git`, identical in every magit buffer,
//! consulting no cache. Staging reads the same text, so navigation and
//! staging cannot disagree about where a hunk begins. A parsed-hunk
//! cache (§7.2 records one as deliberately absent) would give `]c` one
//! boundary set and `s` another with nothing forcing agreement.
//!
//! # Why headers are copied, never reconstructed
//!
//! [`HunkPatch::header`] holds the file's header lines **verbatim** —
//! `diff --git`, `index`, mode changes, `---`, `+++`. Rebuilding them
//! from a parsed path would have to re-derive git's own quoting for
//! paths with spaces or non-ASCII bytes, and would silently drop
//! rename and mode metadata. Copying cannot get any of that wrong.

use std::fmt::Write as _;
use std::path::PathBuf;

use crate::highlight::{DiffLineClass, classify_diff_line};

/// One hunk, plus the file header needed to apply it on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkPatch {
    /// Verbatim file-header lines: `diff --git` through `+++`.
    pub header: Vec<String>,
    /// The `@@` line and its body, verbatim.
    pub hunk: Vec<String>,
    /// Buffer line of the `@@` header, for cursor restoration.
    pub header_line: usize,
    /// Buffer line one past the hunk's last body line.
    pub end_line: usize,
}

impl HunkPatch {
    /// `path:line` naming this hunk's position **in the file** — the
    /// `@@` header's new-side start, not a buffer row. Prompts and
    /// echoes use it, so `Discard hunk at src/main.rs:42?` points at
    /// something the user can find after the buffer is rebuilt.
    ///
    /// Falls back to the path alone, then to `"hunk"`, so a header
    /// this cannot parse still yields a sentence.
    pub fn display_location(&self) -> String {
        let path = self.display_path();
        let start = self
            .hunk
            .first()
            .and_then(|h| parse_hunk_starts(h.trim_end()));
        match (path, start) {
            (Some(p), Some(HunkStarts { new, .. })) => format!("{p}:{new}"),
            (Some(p), None) => p.to_string(),
            (None, _) => "hunk".to_string(),
        }
    }

    /// The file this hunk belongs to, for finding it again after the
    /// buffer is rebuilt (MG.18d).
    ///
    /// Prefers the `+++ b/` side and falls back to `--- a/`, so a
    /// deletion — whose `+++` is `/dev/null` — still names its file.
    /// [`Self::display_path`] deliberately does not: a prompt saying
    /// "discard hunk at /dev/null" would be worse than one that omits
    /// the path, while a cursor restore that skipped deletions would
    /// silently lose the user's place on exactly the rows that are
    /// hardest to find again.
    pub fn file_path(&self) -> Option<&str> {
        self.display_path().or_else(|| {
            let minus = self.header.iter().find(|l| l.starts_with("--- "))?;
            let rest = minus[4..].trim_end();
            (rest != "/dev/null").then(|| rest.strip_prefix("a/").unwrap_or(rest))
        })
    }

    /// The path from the `+++ b/…` line, for prompts and messages.
    /// `None` for a deletion (`+++ /dev/null`) or a malformed header.
    ///
    /// Display only — [`Self::to_patch`] never consults it, so a path
    /// this cannot parse still stages correctly.
    pub fn display_path(&self) -> Option<&str> {
        let plus = self.header.iter().find(|l| l.starts_with("+++ "))?;
        let rest = plus[4..].trim_end();
        if rest == "/dev/null" {
            return None;
        }
        Some(rest.strip_prefix("b/").unwrap_or(rest))
    }

    /// A standalone patch: the file header, then this hunk alone.
    ///
    /// Always ends in a newline — `git apply` rejects a patch whose
    /// final line is unterminated.
    pub fn to_patch(&self) -> String {
        let mut out = String::new();
        for line in self.header.iter().chain(self.hunk.iter()) {
            let _ = writeln!(out, "{line}");
        }
        out
    }
}

/// Which way a patch will be handed to `git apply`, and therefore
/// which side of it the target already matches.
///
/// MG.18e: this is the only thing that distinguishes staging a region
/// from unstaging one. The two are mirror images — one function with a
/// flag, not two that can drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyDirection {
    /// `s` — the target holds the hunk's **old** side.
    Forward,
    /// `u` / `x` — the target holds the hunk's **new** side.
    Reverse,
}

impl HunkPatch {
    /// MG.18e: rewrite this hunk to carry only the changes on the buffer
    /// rows in `selected`.
    ///
    /// Region staging is not "a smaller hunk" — the body has to be
    /// *rewritten*, because a patch must still describe a complete
    /// transformation of the region it covers:
    ///
    /// | Line | Selected | Unselected (`Forward`) | Unselected (`Reverse`) |
    /// |---|---|---|---|
    /// | `+added` | stays `+` | **dropped** | becomes context |
    /// | `-removed` | stays `-` | becomes context | **dropped** |
    /// | context | context | context | context |
    ///
    /// The asymmetry is not a convention, it is what the target
    /// contains. Applying forward, the target holds the old side: an
    /// unselected `+` is not there and must not appear at all, while an
    /// unselected `-` *is* there and survives — i.e. context. Reversed,
    /// the target holds the new side and the roles swap exactly.
    ///
    /// Both counts are recounted from the rewritten body; `git apply`
    /// validates them against it and rejects the patch outright if they
    /// disagree ("corrupt patch"). The two **start** lines are kept
    /// verbatim: whichever side the target matches is preserved
    /// line-for-line by the rules above, so its start is still correct,
    /// and the other side's start is not something git checks.
    ///
    /// `None` when the selection contains no `+`/`-` line at all — a
    /// body of pure context is a patch that does nothing, and telling
    /// the user "nothing to stage there" beats handing git a no-op.
    pub fn restrict_to_rows(
        &self,
        selected: std::ops::RangeInclusive<usize>,
        direction: ApplyDirection,
    ) -> Option<HunkPatch> {
        let mut body: Vec<String> = Vec::new();
        let mut selected_changes = 0usize;
        // `\ No newline at end of file` annotates the line above it, so
        // it rides along only if that line survived. Dropping the line
        // and keeping its marker would attach it to whatever came
        // before, silently changing THAT line's trailing newline.
        let mut kept_previous = false;

        for (k, line) in self.hunk.iter().enumerate().skip(1) {
            // The parser consumed the body contiguously, markers
            // included, so `hunk[k]` is buffer row `header_line + k`.
            let row = self.header_line + k;
            if line.starts_with('\\') {
                if kept_previous {
                    body.push(line.clone());
                }
                continue;
            }
            let marker = line.chars().next();
            let is_change = matches!(marker, Some('+') | Some('-'));
            if !is_change {
                body.push(line.clone());
                kept_previous = true;
                continue;
            }
            let is_add = marker == Some('+');
            if selected.contains(&row) {
                body.push(line.clone());
                kept_previous = true;
                selected_changes += 1;
                continue;
            }
            let dropped = match direction {
                ApplyDirection::Forward => is_add,
                ApplyDirection::Reverse => !is_add,
            };
            if dropped {
                kept_previous = false;
            } else {
                // Contextualise: same content, no marker. `+`/`-` are
                // one byte, so the slice is char-boundary safe.
                body.push(format!(" {}", &line[1..]));
                kept_previous = true;
            }
        }

        if selected_changes == 0 {
            return None;
        }

        let old = body
            .iter()
            .filter(|l| !l.starts_with('\\') && !l.starts_with('+'))
            .count();
        let new = body
            .iter()
            .filter(|l| !l.starts_with('\\') && !l.starts_with('-'))
            .count();
        let header = rewrite_hunk_header(self.hunk.first()?, old, new)?;

        let mut hunk = Vec::with_capacity(body.len() + 1);
        hunk.push(header);
        hunk.extend(body);
        Some(HunkPatch {
            header: self.header.clone(),
            hunk,
            header_line: self.header_line,
            end_line: self.end_line,
        })
    }
}

/// Rebuild an `@@` header with new counts, keeping both starts and any
/// trailing function-context suffix git emitted.
fn rewrite_hunk_header(original: &str, old: usize, new: usize) -> Option<String> {
    let trimmed = original.trim_end();
    let starts = parse_hunk_starts(trimmed)?;
    // Everything after the closing `@@` — git's function-context hint.
    // Carried through rather than recomputed: it is a display aid, and
    // deriving it would mean guessing at the language's idea of an
    // enclosing definition.
    let suffix = trimmed
        .strip_prefix("@@ ")
        .and_then(|rest| rest.split_once(" @@"))
        .map(|(_, after)| after)
        .unwrap_or("");
    Some(format!(
        "@@ -{},{} +{},{} @@{}",
        starts.old, old, starts.new, new, suffix
    ))
}

/// The `-old,count +new,count` line counts declared by an `@@` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HunkCounts {
    old: usize,
    new: usize,
}

/// Parse `@@ -12,7 +12,8 @@ trailing context` into its two counts.
///
/// A missing count means 1 (`@@ -12 +12 @@` is a one-line hunk) — an
/// omission git emits routinely and a naive `split(',')` drops.
fn parse_hunk_counts(header: &str) -> Option<HunkCounts> {
    let inner = header.strip_prefix("@@ ")?;
    let inner = inner.split(" @@").next()?;
    let mut parts = inner.split_whitespace();
    let old = parse_range_count(parts.next()?.strip_prefix('-')?)?;
    let new = parse_range_count(parts.next()?.strip_prefix('+')?)?;
    Some(HunkCounts { old, new })
}

/// `"12,7"` → 7; `"12"` → 1.
fn parse_range_count(range: &str) -> Option<usize> {
    match range.split_once(',') {
        Some((_, count)) => count.parse().ok(),
        None => Some(1),
    }
}

/// The `-old +new` **start lines** declared by an `@@` header — the
/// file positions, used only to name the hunk in prompts.
struct HunkStarts {
    old: usize,
    new: usize,
}

fn parse_hunk_starts(header: &str) -> Option<HunkStarts> {
    let inner = header.strip_prefix("@@ ")?;
    let inner = inner.split(" @@").next()?;
    let mut parts = inner.split_whitespace();
    let old = parse_range_start(parts.next()?.strip_prefix('-')?)?;
    let new = parse_range_start(parts.next()?.strip_prefix('+')?)?;
    Some(HunkStarts { old, new })
}

/// `"12,7"` → 12; `"12"` → 12.
fn parse_range_start(range: &str) -> Option<usize> {
    range
        .split_once(',')
        .map(|(start, _)| start)
        .unwrap_or(range)
        .parse()
        .ok()
}

/// Locate the hunk containing `cursor` in `lines`.
///
/// The slice form of [`hunk_at_with`], for tests: production reads a
/// buffer, which has no slice to hand.
#[cfg(test)]
pub(crate) fn hunk_at(lines: &[&str], cursor: usize) -> Option<HunkPatch> {
    hunk_at_with(|i| lines.get(i).map(|l| (*l).to_string()), cursor)
}

/// Locate the hunk containing `cursor`, reading lines through `read`.
///
/// `read(i)` returns line `i` without its trailing newline, or `None`
/// past the end. Lines are used **verbatim** — trailing whitespace is
/// part of the diff's content, and trimming it produces a patch whose
/// context no longer matches the file.
///
/// Returns `None` when the cursor is not inside a hunk body — on a
/// section header, a file entry, a commit message, a diff's own
/// `---`/`+++` header lines, or anywhere *after* the last body line of
/// the hunk above. Callers fall back to file-level staging, which is
/// what keeps every pre-MG.18 behaviour intact.
/// MG.22: the file a diff line belongs to — the one diff-path parser.
///
/// Three modes had a copy of this (magit-diff, magit-commit,
/// magit-revision), each scanning upward for `diff --git a/<path>`,
/// and magit-revision additionally checking the cursor line for a
/// `git show --stat` summary row (`" src/main.rs | 12 +++++-----"`).
///
/// **The order of those two checks is load-bearing, and the copy that
/// had both got it wrong.** magit-revision tried the stat row *first*,
/// and `parse_stat_line` splits on `" | "` — so `<CR>` on any diff
/// body line containing that sequence (` let x = a | b;`, a markdown
/// table, a doc comment) resolved to the text left of the pipe and
/// opened a buffer named after it.
///
/// Scanning for the `diff --git` header first removes the ambiguity
/// structurally rather than by tightening the stat pattern: a diff
/// body line **always** has a header above it, and a stat row never
/// does, because `git show --stat -p` prints the summary before the
/// first diff. So reaching the stat check at all means the cursor is
/// above every diff, which is exactly where stat rows live.
///
/// Reads through an accessor rather than a materialised buffer, for
/// the reason [`hunk_at_with`] does: a large `git show` is tens of
/// thousands of lines and resolving one path must not copy them.
pub fn path_at_cursor(read: impl Fn(usize) -> Option<String>, cursor: usize) -> Option<PathBuf> {
    for l in (0..=cursor).rev() {
        let text = read(l)?;
        if let Some(rest) = text.strip_prefix("diff --git a/") {
            // `a/<path> b/<path>` — take the first. They differ only
            // for renames, which file-level resolution does not
            // special-case.
            return rest.split(" b/").next().map(PathBuf::from);
        }
    }
    // Above every diff header: a `--stat` summary row, if this buffer
    // has one.
    parse_stat_line(&read(cursor)?)
}

/// `git show --stat`'s summary row: `" <path> | <N> <bar>"`.
///
/// Only ever reached from above the first `diff --git` header — see
/// [`path_at_cursor`] for why that ordering is what makes splitting on
/// `" | "` safe.
fn parse_stat_line(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim_start();
    let (path, _rest) = trimmed.split_once(" | ")?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

pub fn hunk_at_with(read: impl Fn(usize) -> Option<String>, cursor: usize) -> Option<HunkPatch> {
    let header_line = enclosing_hunk_header(&read, cursor)?;
    let header_text = read(header_line)?;
    let counts = parse_hunk_counts(header_text.trim_end())?;

    // Consume the body using the header's declared counts rather than
    // stopping at "a line that doesn't look like diff content".
    //
    // This is load-bearing in magit-status: the file entry that follows
    // an inline diff is `"  modified src/foo.rs"`, which begins with a
    // space and is therefore indistinguishable from a context line by
    // prefix alone. The counts delimit the hunk exactly, so the parser
    // never runs past its end into the surrounding buffer.
    let mut old_seen = 0usize;
    let mut new_seen = 0usize;
    let mut hunk = vec![header_text];
    let mut idx = header_line + 1;
    while old_seen < counts.old || new_seen < counts.new {
        let Some(line) = read(idx) else { break };
        // "\ No newline at end of file" annotates the line above and
        // counts toward neither side, but must ride along: dropping it
        // changes whether the applied result ends in a newline.
        if line.starts_with('\\') {
            hunk.push(line);
            idx += 1;
            continue;
        }
        match line.chars().next() {
            Some('+') => new_seen += 1,
            Some('-') => old_seen += 1,
            // A context line counts toward both sides. An empty line is
            // a context line whose single space git trimmed in transit;
            // treat it as context rather than aborting the hunk.
            Some(' ') | None => {
                old_seen += 1;
                new_seen += 1;
            }
            // Anything else means the diff was truncated (a buffer that
            // ends mid-hunk). Stop rather than absorb foreign lines.
            _ => break,
        }
        hunk.push(line);
        idx += 1;
    }

    // A hunk whose declared counts were not satisfied is truncated;
    // applying it would corrupt. Refuse instead.
    if old_seen < counts.old || new_seen < counts.new {
        return None;
    }

    // A `\ No newline at end of file` following the LAST body line is
    // reached after the counts are satisfied, so the loop above never
    // sees it. Dropping it would tell git to append a newline the file
    // does not have — a one-byte corruption of every no-final-newline
    // file staged this way.
    while let Some(line) = read(idx) {
        if !line.starts_with('\\') {
            break;
        }
        hunk.push(line);
        idx += 1;
    }

    // The cursor must be INSIDE the hunk it resolved, not merely below
    // one. In magit-status the line after an expanded diff is the next
    // file entry, and `s` there must stage that file — the pre-MG.18c
    // behaviour — rather than restage the hunk above it. Without this
    // the backward scan would happily claim every row down to the next
    // `@@`, whatever it contained.
    if cursor >= idx {
        return None;
    }

    let header = file_header_above(&read, header_line)?;
    Some(HunkPatch {
        header,
        hunk,
        header_line,
        end_line: idx,
    })
}

/// MG.18d: the 0-based index of the hunk whose header sits at
/// `header_row`, among the hunks of the file it belongs to.
///
/// The ordinal is what survives a rebuild: staging hunk *k* removes it,
/// so ordinal *k* then names the hunk that took its place. Counted
/// backwards from the hunk to its `diff --git` line, so it is the same
/// walk [`hunk_at_with`] already does and cannot disagree with it about
/// where the file starts.
pub fn hunk_ordinal_at(read: impl Fn(usize) -> Option<String>, header_row: usize) -> usize {
    let mut ordinal = 0usize;
    for row in (0..header_row).rev() {
        let Some(line) = read(row) else { break };
        match classify_diff_line(&line) {
            DiffLineClass::Hunk => ordinal += 1,
            DiffLineClass::FileCommand => break,
            _ => {}
        }
    }
    ordinal
}

/// The `@@` line at or above `cursor`, without crossing into a
/// different file's diff or out of the diff entirely.
fn enclosing_hunk_header(read: &impl Fn(usize) -> Option<String>, cursor: usize) -> Option<usize> {
    // Past the end of the buffer there is nothing to resolve.
    read(cursor)?;
    for idx in (0..=cursor).rev() {
        let line = read(idx)?;
        match classify_diff_line(&line) {
            DiffLineClass::Hunk => return Some(idx),
            // Reaching the file command or its path headers means the
            // cursor sat above the first `@@` — inside the header, not
            // inside a hunk.
            DiffLineClass::FileCommand => return None,
            _ => {}
        }
    }
    None
}

/// The verbatim file-header block above `header_line`: from the
/// `diff --git` line down to just before the first `@@`.
///
/// `None` when there is none above the cursor — a diff fragment with no
/// file header cannot be applied, so refusing here is what stops a
/// patch that would target the wrong file.
fn file_header_above(
    read: &impl Fn(usize) -> Option<String>,
    header_line: usize,
) -> Option<Vec<String>> {
    let start = (0..header_line).rev().find(|&i| {
        read(i)
            .map(|l| classify_diff_line(&l) == DiffLineClass::FileCommand)
            .unwrap_or(false)
    })?;
    let mut header = Vec::new();
    for i in start..header_line {
        let line = read(i)?;
        if classify_diff_line(&line) == DiffLineClass::Hunk {
            break;
        }
        header.push(line);
    }
    Some(header)
}

/// MG.22: the one diff-path parser, and the misfire it retires.
#[cfg(test)]
mod path_at_cursor_tests {
    use super::*;

    fn reader(lines: &'static [&'static str]) -> impl Fn(usize) -> Option<String> {
        move |i: usize| lines.get(i).map(|s| s.to_string())
    }

    /// `git show --stat -p`: header, stat summary, then the diff.
    const SHOW: &[&str] = &[
        "commit a1b2c3d4",
        "Author: Jane Doe <jane@example.com>",
        "",
        "    do the thing",
        "",
        " src/main.rs | 12 +++++-----",
        " 1 file changed",
        "",
        "diff --git a/src/main.rs b/src/main.rs",
        "index 111..222 100644",
        "--- a/src/main.rs",
        "+++ b/src/main.rs",
        "@@ -1,3 +1,3 @@",
        " fn main() {",
        "-    let x = a | b;",
        "+    let x = a & b;",
        " }",
    ];

    /// The bug this ordering removes: `parse_stat_line` splits on
    /// `\" | \"`, so a diff body line containing that sequence used to
    /// resolve to the text left of the pipe. magit-revision checked the
    /// stat row FIRST, so `<CR>` on this line opened a buffer named
    /// `"    let x = a"`.
    #[test]
    fn a_diff_line_containing_a_pipe_resolves_to_its_file_not_to_itself() {
        let got = path_at_cursor(reader(SHOW), 14);
        assert_eq!(
            got,
            Some(PathBuf::from("src/main.rs")),
            "a body line with ` | ` in it must resolve through the \
             `diff --git` header above it"
        );
    }

    /// And the stat row still works — it is reached precisely because
    /// there is no diff header above it.
    #[test]
    fn a_stat_summary_row_resolves_to_the_file_it_names() {
        assert_eq!(
            path_at_cursor(reader(SHOW), 5),
            Some(PathBuf::from("src/main.rs"))
        );
    }

    /// The common case, in a buffer with no stat section at all.
    #[test]
    fn a_plain_diff_resolves_from_the_header_above_the_cursor() {
        const DIFF: &[&str] = &[
            "diff --git a/src/lib.rs b/src/lib.rs",
            "--- a/src/lib.rs",
            "+++ b/src/lib.rs",
            "@@ -1 +1 @@",
            "-old",
            "+new",
        ];
        assert_eq!(
            path_at_cursor(reader(DIFF), 5),
            Some(PathBuf::from("src/lib.rs"))
        );
    }

    /// The second file's lines must resolve to the second file — an
    /// upward scan that stopped at the first header ever seen would
    /// name the wrong one.
    #[test]
    fn a_multi_file_diff_resolves_to_the_nearest_header_above() {
        const TWO: &[&str] = &[
            "diff --git a/a.txt b/a.txt",
            "@@ -1 +1 @@",
            "-a",
            "diff --git a/b.txt b/b.txt",
            "@@ -1 +1 @@",
            "-b",
        ];
        assert_eq!(path_at_cursor(reader(TWO), 2), Some(PathBuf::from("a.txt")));
        assert_eq!(path_at_cursor(reader(TWO), 5), Some(PathBuf::from("b.txt")));
    }

    /// Nothing above and nothing stat-shaped on the line: no answer,
    /// rather than a guess.
    #[test]
    fn prose_with_no_diff_above_it_resolves_to_nothing() {
        const HEADER_ONLY: &[&str] = &["commit a1b2c3d4", "Author: Jane", "", "    subject"];
        assert_eq!(path_at_cursor(reader(HEADER_ONLY), 3), None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_HUNKS: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1234567..89abcde 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,3 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
 }
@@ -20,2 +20,3 @@ fn other() {
     let x = 1;
+    let y = 2;
     drop(x);
";

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    #[test]
    fn a_cursor_inside_the_first_hunk_finds_exactly_that_hunk() {
        let l = lines(TWO_HUNKS);
        // Line 6 is `-    println!("old");`
        let h = hunk_at(&l, 6).expect("cursor is inside hunk 1");
        assert_eq!(h.header_line, 4);
        assert!(h.hunk[0].starts_with("@@ -1,3 +1,3 @@"));
        assert!(
            h.hunk.iter().any(|s| s.contains("println!(\"new\")")),
            "hunk 1's body is present: {:?}",
            h.hunk
        );
        assert!(
            !h.hunk.iter().any(|s| s.contains("let y = 2")),
            "hunk 2 must NOT bleed in: {:?}",
            h.hunk
        );
    }

    #[test]
    fn a_cursor_in_the_second_hunk_finds_the_second() {
        let l = lines(TWO_HUNKS);
        let h = hunk_at(&l, 11).expect("cursor is inside hunk 2");
        assert!(h.hunk[0].starts_with("@@ -20,2 +20,3 @@"));
        assert!(h.hunk.iter().any(|s| s.contains("let y = 2")));
        assert!(!h.hunk.iter().any(|s| s.contains("println!")));
    }

    #[test]
    fn the_hunk_header_line_itself_resolves_to_its_own_hunk() {
        let l = lines(TWO_HUNKS);
        let h = hunk_at(&l, 4).expect("cursor on the @@ line");
        assert_eq!(h.header_line, 4);
    }

    #[test]
    fn a_cursor_in_the_file_header_is_not_in_a_hunk() {
        let l = lines(TWO_HUNKS);
        // `--- a/src/main.rs` — above the first @@.
        assert!(
            hunk_at(&l, 2).is_none(),
            "header lines fall back to file-level staging"
        );
        assert!(hunk_at(&l, 0).is_none(), "the diff --git line likewise");
    }

    #[test]
    fn the_counts_stop_the_body_before_a_following_status_entry() {
        // THE magit-status hazard: the entry line after an inline diff
        // starts with a space, exactly like a context line. Only the
        // `@@` counts distinguish them.
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 keep
-old
+new
  modified src/other.rs
  modified src/third.rs
";
        let l = lines(text);
        let h = hunk_at(&l, 5).expect("inside the hunk");
        assert_eq!(
            h.hunk,
            vec!["@@ -1,2 +1,2 @@", " keep", "-old", "+new"],
            "the body stops at the declared counts, not at the next entry"
        );
        assert!(
            !h.to_patch().contains("modified src/other.rs"),
            "a status entry must never reach the patch:\n{}",
            h.to_patch()
        );
    }

    /// MG.18c — the fallback that keeps file-level staging alive.
    /// `s` on the entry line *below* an expanded diff must stage that
    /// file, not restage the hunk above it. The backward scan finds
    /// that hunk's `@@` either way; only the containment check
    /// distinguishes "inside it" from "somewhere after it".
    #[test]
    fn a_cursor_below_a_hunk_resolves_to_no_hunk() {
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 keep
-old
+new
  modified src/other.rs
";
        let l = lines(text);
        assert!(
            hunk_at(&l, 6).is_some(),
            "the last body line is still inside the hunk"
        );
        assert!(
            hunk_at(&l, 7).is_none(),
            "the following status entry is not in the hunk — `s` there stages the file"
        );
    }

    /// Trailing whitespace is content. `git apply` matches context
    /// byte-for-byte, so trimming a context line's trailing spaces
    /// produces a patch git refuses — every diff touching a
    /// whitespace-dirty region would fail to stage.
    #[test]
    fn trailing_whitespace_survives_into_the_patch() {
        // Written with escapes: a literal with trailing spaces is
        // invisible in review and the first formatter to touch this
        // file would silently delete the thing under test.
        let text = concat!(
            "diff --git a/a.txt b/a.txt\n",
            "--- a/a.txt\n",
            "+++ b/a.txt\n",
            "@@ -1,2 +1,2 @@\n",
            " keep   \n",
            "-old\t\n",
            "+new\n",
        );
        let l = lines(text);
        let h = hunk_at(&l, 5).expect("inside the hunk");
        assert_eq!(h.hunk[1], " keep   ", "context kept verbatim");
        assert_eq!(h.hunk[2], "-old\t", "removed line kept verbatim");
    }

    /// The marker after the FINAL body line is reached only once the
    /// counts are satisfied, so the body loop never sees it. Dropping
    /// it tells git to append a newline the file does not have.
    #[test]
    fn a_trailing_no_newline_marker_after_the_last_body_line_rides_along() {
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
+new
\\ No newline at end of file
";
        let l = lines(text);
        let h = hunk_at(&l, 4).expect("inside the hunk");
        assert_eq!(
            h.hunk.last().map(String::as_str),
            Some("\\ No newline at end of file"),
            "the trailing marker must reach the patch: {:?}",
            h.hunk
        );
        assert_eq!(h.end_line, 7, "and be counted as part of the hunk");
    }

    /// MG.18d: the ordinal is what survives a rebuild, so it must be
    /// counted within the file — not from the top of the buffer.
    #[test]
    fn a_hunks_ordinal_counts_within_its_own_file() {
        let l = lines(TWO_HUNKS);
        let read = |i: usize| l.get(i).map(|s| (*s).to_string());
        assert_eq!(hunk_ordinal_at(read, 4), 0, "the first `@@`");
        assert_eq!(hunk_ordinal_at(read, 9), 1, "the second");
    }

    #[test]
    fn the_ordinal_restarts_at_each_files_header() {
        let text = "\
diff --git a/a.txt b/a.txt
@@ -1,1 +1,1 @@
-a
diff --git a/b.txt b/b.txt
@@ -1,1 +1,1 @@
-b
@@ -9,1 +9,1 @@
-c
";
        let l = lines(text);
        let read = |i: usize| l.get(i).map(|s| (*s).to_string());
        assert_eq!(
            hunk_ordinal_at(read, 4),
            0,
            "b.txt's first hunk is ordinal 0, not 1 — the count stops at its own `diff --git`"
        );
        assert_eq!(hunk_ordinal_at(read, 6), 1);
    }

    /// A deletion's `+++` is `/dev/null`, so the display path is `None`
    /// — but the cursor restore still has to find the file again.
    #[test]
    fn file_path_falls_back_to_the_minus_side_for_a_deletion() {
        let text = "\
diff --git a/gone.txt b/gone.txt
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-was here
";
        let l = lines(text);
        let h = hunk_at(&l, 4).expect("inside the hunk");
        assert_eq!(h.display_path(), None, "prompts omit /dev/null");
        assert_eq!(
            h.file_path(),
            Some("gone.txt"),
            "but the restore must still name the file"
        );
    }

    #[test]
    fn display_location_names_the_file_line_not_the_buffer_row() {
        let l = lines(TWO_HUNKS);
        // Hunk 2 sits at buffer row 9 but describes file line 20.
        let h = hunk_at(&l, 11).unwrap();
        assert_eq!(h.display_location(), "src/main.rs:20");
    }

    #[test]
    fn display_location_falls_back_when_the_header_is_unparseable() {
        let text = "\
diff --git a/gone.txt b/gone.txt
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-was here
";
        let l = lines(text);
        let h = hunk_at(&l, 4).unwrap();
        assert_eq!(h.display_location(), "hunk", "a deletion has no b/ path");
    }

    #[test]
    fn a_hunk_header_without_counts_means_one_line() {
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -5 +5 @@
-old
+new
";
        let l = lines(text);
        let h = hunk_at(&l, 4).expect("inside the hunk");
        assert_eq!(h.hunk, vec!["@@ -5 +5 @@", "-old", "+new"]);
    }

    #[test]
    fn a_no_newline_marker_rides_along_with_the_body() {
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
";
        let l = lines(text);
        let h = hunk_at(&l, 4).expect("inside the hunk");
        assert!(
            h.hunk.iter().any(|s| s.starts_with('\\')),
            "dropping the marker would change the applied result's trailing newline: {:?}",
            h.hunk
        );
    }

    #[test]
    fn a_truncated_hunk_is_refused_rather_than_applied() {
        // A buffer that ends mid-hunk (the diff was cut off). Applying
        // the fragment would corrupt the file.
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,5 +1,5 @@
 one
-two
";
        let l = lines(text);
        assert!(
            hunk_at(&l, 5).is_none(),
            "an unsatisfied count means truncated — refuse"
        );
    }

    #[test]
    fn a_hunk_with_no_file_header_above_it_is_refused() {
        // Without a header there is no way to know which file this
        // targets; a patch built from it could hit the wrong one.
        let text = "\
@@ -1,2 +1,2 @@
 keep
-old
+new
";
        let l = lines(text);
        assert!(hunk_at(&l, 2).is_none());
    }

    #[test]
    fn the_patch_carries_the_header_verbatim_and_ends_in_a_newline() {
        let l = lines(TWO_HUNKS);
        let h = hunk_at(&l, 6).unwrap();
        let patch = h.to_patch();
        assert!(patch.starts_with("diff --git a/src/main.rs b/src/main.rs\n"));
        assert!(
            patch.contains("index 1234567..89abcde 100644"),
            "index/mode metadata is preserved, not reconstructed:\n{patch}"
        );
        assert!(
            patch.ends_with('\n'),
            "git apply rejects an unterminated patch"
        );
    }

    #[test]
    fn display_path_reads_the_plus_header() {
        let l = lines(TWO_HUNKS);
        let h = hunk_at(&l, 6).unwrap();
        assert_eq!(h.display_path(), Some("src/main.rs"));
    }

    #[test]
    fn display_path_is_none_for_a_deletion() {
        let text = "\
diff --git a/gone.txt b/gone.txt
--- a/gone.txt
+++ /dev/null
@@ -1 +0,0 @@
-was here
";
        let l = lines(text);
        let h = hunk_at(&l, 4).expect("inside the hunk");
        assert_eq!(h.display_path(), None);
        assert!(
            h.to_patch().contains("+++ /dev/null"),
            "the patch still carries the real header"
        );
    }

    #[test]
    fn every_body_line_of_a_multi_hunk_diff_resolves_to_its_own_hunk() {
        // Sweep: no cursor position inside a hunk may resolve to the
        // wrong one, and none may panic.
        let l = lines(TWO_HUNKS);
        let first: Vec<usize> = (5..=8).collect();
        let second: Vec<usize> = (10..=12).collect();
        for c in first {
            let h = hunk_at(&l, c).unwrap_or_else(|| panic!("line {c} is in hunk 1"));
            assert_eq!(h.header_line, 4, "line {c} belongs to hunk 1");
        }
        for c in second {
            let h = hunk_at(&l, c).unwrap_or_else(|| panic!("line {c} is in hunk 2"));
            assert_eq!(h.header_line, 9, "line {c} belongs to hunk 2");
        }
    }
}

/// MG.18e: the region rewrite, as a table.
///
/// Rows 5–8 of `REGION` are the body: ` keep`, `-old-a`, `-old-b`,
/// `+new-a`, `+new-b` — enough to select adds only, removes only, an
/// interleaved slice, the first change, and the last.
#[cfg(test)]
mod region {
    use super::*;

    const REGION: &str = "\
diff --git a/a.txt b/a.txt
index 111..222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,3 +1,3 @@
 keep
-old-a
-old-b
+new-a
+new-b
";
    /// Body rows, by name, so the tests read as intent not arithmetic.
    const KEEP: usize = 5;
    const OLD_A: usize = 6;
    const OLD_B: usize = 7;
    const NEW_A: usize = 8;
    const NEW_B: usize = 9;

    fn whole() -> HunkPatch {
        let lines: Vec<&str> = REGION.lines().collect();
        hunk_at(&lines, OLD_A).expect("the fixture parses")
    }

    fn body(p: &HunkPatch) -> Vec<&str> {
        p.hunk.iter().map(String::as_str).collect()
    }

    /// The boundary case that keeps the two paths honest: selecting
    /// every line must produce exactly what whole-hunk staging does.
    #[test]
    fn selecting_the_whole_body_reproduces_the_whole_hunk_patch() {
        let whole = whole();
        let restricted = whole
            .restrict_to_rows(KEEP..=NEW_B, ApplyDirection::Forward)
            .expect("changes are selected");
        assert_eq!(
            restricted.to_patch(),
            whole.to_patch(),
            "an all-selected region is not a special case, it IS the hunk"
        );
    }

    /// Selecting nothing changeable is not an empty patch — it is a
    /// refusal, so the caller can say "nothing to stage there".
    #[test]
    fn a_selection_with_no_change_in_it_is_refused() {
        assert!(
            whole()
                .restrict_to_rows(KEEP..=KEEP, ApplyDirection::Forward)
                .is_none(),
            "a context-only selection would be a patch that does nothing"
        );
    }

    /// Staging one added line: the other addition is DROPPED (it is not
    /// in the index yet, so it cannot appear at all), and both removals
    /// become context (they are still in the index).
    #[test]
    fn staging_one_addition_drops_the_other_and_contextualises_removals() {
        let p = whole()
            .restrict_to_rows(NEW_A..=NEW_A, ApplyDirection::Forward)
            .expect("one addition selected");
        assert_eq!(
            body(&p),
            vec!["@@ -1,3 +1,4 @@", " keep", " old-a", " old-b", "+new-a"],
            "old side unchanged (3 lines), new side gains exactly the one addition"
        );
    }

    /// Staging one removal: it stays `-`, the other removal becomes
    /// context, and BOTH additions vanish.
    #[test]
    fn staging_one_removal_keeps_it_and_drops_every_addition() {
        let p = whole()
            .restrict_to_rows(OLD_B..=OLD_B, ApplyDirection::Forward)
            .expect("one removal selected");
        assert_eq!(
            body(&p),
            vec!["@@ -1,3 +1,2 @@", " keep", " old-a", "-old-b"],
        );
    }

    /// The mirror image. Unstaging reverses the roles: an unselected
    /// addition is in the index and survives as context; an unselected
    /// removal is not, and goes.
    #[test]
    fn unstaging_mirrors_the_rules_exactly() {
        let p = whole()
            .restrict_to_rows(OLD_A..=OLD_A, ApplyDirection::Reverse)
            .expect("one removal selected");
        assert_eq!(
            body(&p),
            vec!["@@ -1,4 +1,3 @@", " keep", "-old-a", " new-a", " new-b"],
            "new side unchanged at 3 — it is what a reverse apply matches. The old \
             side is 4 because un-removing `old-a` puts it back ALONGSIDE the \
             additions that stay staged."
        );
    }

    /// An interleaved selection exercises both rules in one body.
    #[test]
    fn an_interleaved_selection_applies_both_rules() {
        let p = whole()
            .restrict_to_rows(OLD_B..=NEW_A, ApplyDirection::Forward)
            .expect("one removal and one addition selected");
        assert_eq!(
            body(&p),
            vec!["@@ -1,3 +1,3 @@", " keep", " old-a", "-old-b", "+new-a"],
        );
    }

    /// A selection reaching past the hunk in either direction clamps to
    /// the body — the cursor's hunk is the unit, and rows outside it
    /// belong to other entries.
    #[test]
    fn a_selection_wider_than_the_hunk_clamps_to_its_body() {
        let p = whole()
            .restrict_to_rows(0..=999, ApplyDirection::Forward)
            .expect("everything selected");
        assert_eq!(p.to_patch(), whole().to_patch());
    }

    /// The header's function-context suffix is a display hint git
    /// emitted; recomputing it would mean guessing at the language's
    /// idea of an enclosing definition, so it rides through.
    #[test]
    fn the_headers_function_context_suffix_survives_the_rewrite() {
        let text = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -10,1 +10,1 @@ fn main() {
-old
+new
";
        let lines: Vec<&str> = text.lines().collect();
        let p = hunk_at(&lines, 4)
            .expect("parses")
            .restrict_to_rows(4..=4, ApplyDirection::Forward)
            .expect("the removal is selected");
        assert_eq!(
            p.hunk[0], "@@ -10,1 +10,0 @@ fn main() {",
            "starts and suffix kept, counts recomputed"
        );
    }

    /// A `\ No newline` marker annotates the line above it. If that line
    /// is dropped the marker must go too, or it re-attaches to whatever
    /// came before and silently changes THAT line's trailing newline.
    #[test]
    fn a_marker_whose_line_was_dropped_is_dropped_with_it() {
        let text = "\
diff --git a/a.txt b/a.txt
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 keep
-old
+new
\\ No newline at end of file
";
        let lines: Vec<&str> = text.lines().collect();
        let whole = hunk_at(&lines, 5).expect("parses");
        // Select the removal only: the addition (row 6) is dropped, and
        // its marker (row 7) must not survive it.
        let p = whole
            .restrict_to_rows(5..=5, ApplyDirection::Forward)
            .expect("the removal is selected");
        assert!(
            !p.hunk.iter().any(|l| l.starts_with('\\')),
            "the marker belonged to the dropped line: {:?}",
            p.hunk
        );
        // Selecting the addition keeps both.
        let p = whole
            .restrict_to_rows(6..=6, ApplyDirection::Forward)
            .expect("the addition is selected");
        assert!(
            p.hunk.last().is_some_and(|l| l.starts_with('\\')),
            "kept with the line it annotates: {:?}",
            p.hunk
        );
    }
}

/// MG.18b round-trip: the parser's output must be a patch **git
/// accepts**. Unit tests above prove the parse is self-consistent;
/// only git can prove it is correct.
#[cfg(test)]
mod git_round_trip {
    use super::*;
    use std::process::Command;

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn git_ok(dir: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// A repo whose working tree differs from HEAD in two places far
    /// enough apart that git reports two hunks.
    fn two_hunk_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(p.join("a.txt"), &base).unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);
        let modified: String = (1..=20)
            .map(|i| match i {
                2 => "line 2 CHANGED\n".to_string(),
                19 => "line 19 CHANGED\n".to_string(),
                _ => format!("line {i}\n"),
            })
            .collect();
        std::fs::write(p.join("a.txt"), &modified).unwrap();
        dir
    }

    /// Byte index of the nth line starting with `@@ `.
    fn nth_hunk_line(text: &str, n: usize) -> usize {
        text.lines()
            .enumerate()
            .filter(|(_, l)| l.starts_with("@@ "))
            .map(|(i, _)| i)
            .nth(n)
            .expect("hunk header")
    }

    #[test]
    fn a_parsed_hunk_applies_to_the_index_without_taking_its_neighbour() {
        let dir = two_hunk_repo();
        let p = dir.path();
        let diff = git(p, &["diff", "--", "a.txt"]);
        let lines: Vec<&str> = diff.lines().collect();

        // Cursor one line into the FIRST hunk's body.
        let cursor = nth_hunk_line(&diff, 0) + 1;
        let h = hunk_at(&lines, cursor).expect("cursor is inside hunk 1");

        let repo = lattice_vcs::Repository::discover(p).expect("discover");
        lattice_vcs::Index::apply_patch(&repo, &h.to_patch(), true, false)
            .expect("the synthesized patch must be one git accepts");

        let staged = git(p, &["diff", "--cached", "--", "a.txt"]);
        assert!(
            staged.contains("line 2 CHANGED"),
            "the selected hunk reached the index:\n{staged}"
        );
        assert!(
            !staged.contains("line 19 CHANGED"),
            "the neighbouring hunk must stay unstaged:\n{staged}"
        );
    }

    #[test]
    fn the_second_hunk_applies_just_as_cleanly() {
        // Guards an off-by-one that would only bite the non-first hunk:
        // a header block collected from the wrong `diff --git`, or a
        // body that started one line late.
        let dir = two_hunk_repo();
        let p = dir.path();
        let diff = git(p, &["diff", "--", "a.txt"]);
        let lines: Vec<&str> = diff.lines().collect();

        let cursor = nth_hunk_line(&diff, 1) + 1;
        let h = hunk_at(&lines, cursor).expect("cursor is inside hunk 2");

        let repo = lattice_vcs::Repository::discover(p).expect("discover");
        lattice_vcs::Index::apply_patch(&repo, &h.to_patch(), true, false)
            .expect("hunk 2's patch applies");

        let staged = git(p, &["diff", "--cached", "--", "a.txt"]);
        assert!(staged.contains("line 19 CHANGED"), "{staged}");
        assert!(!staged.contains("line 2 CHANGED"), "{staged}");
    }

    #[test]
    fn a_parsed_hunk_reverses_out_of_the_index() {
        // `u` unstages by applying the staged hunk in reverse.
        let dir = two_hunk_repo();
        let p = dir.path();
        git_ok(p, &["add", "a.txt"]);
        let staged_diff = git(p, &["diff", "--cached", "--", "a.txt"]);
        let lines: Vec<&str> = staged_diff.lines().collect();

        let cursor = nth_hunk_line(&staged_diff, 0) + 1;
        let h = hunk_at(&lines, cursor).expect("cursor inside staged hunk 1");

        let repo = lattice_vcs::Repository::discover(p).expect("discover");
        lattice_vcs::Index::apply_patch(&repo, &h.to_patch(), true, true)
            .expect("reverse-apply unstages");

        let still = git(p, &["diff", "--cached", "--", "a.txt"]);
        assert!(!still.contains("line 2 CHANGED"), "{still}");
        assert!(still.contains("line 19 CHANGED"), "{still}");
    }

    /// MG.23g: `a` puts one hunk of a commit into the **working
    /// tree** and leaves the index alone, and `-` takes it back out.
    ///
    /// Against a real repository, because what is being asserted is
    /// git's behaviour under `(cached = false)`: a `cached` slip would
    /// stage a commit's hunk invisibly, which no argv-shaped test
    /// would notice.
    #[test]
    fn a_committed_hunk_applies_to_the_worktree_and_reverses_back_out() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(p.join("a.txt"), &base).unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);

        // A commit that changes two distant lines — two hunks, so
        // applying one must not drag the other along.
        let edited: String = (1..=20)
            .map(|i| match i {
                2 => "line 2 FROM COMMIT\n".to_string(),
                19 => "line 19 FROM COMMIT\n".to_string(),
                _ => format!("line {i}\n"),
            })
            .collect();
        std::fs::write(p.join("a.txt"), &edited).unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "the change"]);
        // ...then rewind the working tree and the index to before it,
        // which is the situation `a` exists for: the change is in
        // history but not here.
        git_ok(p, &["reset", "--hard", "HEAD~1"]);

        let show = git(p, &["show", "HEAD@{1}", "--", "a.txt"]);
        let lines: Vec<&str> = show.lines().collect();
        let cursor = nth_hunk_line(&show, 0) + 1;
        let h = hunk_at(&lines, cursor).expect("cursor inside the commit's first hunk");

        let repo = lattice_vcs::Repository::discover(p).expect("discover");
        lattice_vcs::Index::apply_patch(&repo, &h.to_patch(), false, false)
            .expect("`a` applies a committed hunk to the working tree");

        let on_disk = std::fs::read_to_string(p.join("a.txt")).unwrap();
        assert!(
            on_disk.contains("line 2 FROM COMMIT"),
            "the hunk must land in the file:\n{on_disk}"
        );
        assert!(
            !on_disk.contains("line 19 FROM COMMIT"),
            "and only that hunk — the commit's other change stays out:\n{on_disk}"
        );
        assert_eq!(
            git(p, &["diff", "--cached", "--", "a.txt"]).trim(),
            "",
            "`a` writes the working tree, never the index — a staged \
             hunk here would be a `cached` slip nobody would see"
        );

        // `-` is the exact inverse, which is what makes neither of
        // them need a confirm.
        lattice_vcs::Index::apply_patch(&repo, &h.to_patch(), false, true)
            .expect("`-` reverses it back out");
        assert_eq!(
            std::fs::read_to_string(p.join("a.txt")).unwrap(),
            base,
            "reversing must restore the file exactly"
        );
    }

    /// MG.18e: a repo whose edit produces ONE hunk containing two
    /// removals and two additions — the shape region staging exists
    /// for.
    fn one_hunk_two_changes_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        let base: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(p.join("a.txt"), &base).unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);
        let edited: String = (1..=10)
            .map(|i| match i {
                4 => "line 4 EDITED\n".to_string(),
                5 => "line 5 EDITED\n".to_string(),
                _ => format!("line {i}\n"),
            })
            .collect();
        std::fs::write(p.join("a.txt"), &edited).unwrap();
        dir
    }

    /// The row of the body line whose text contains `needle`.
    fn row_containing(text: &str, needle: &str) -> usize {
        text.lines()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{text}"))
    }

    /// The counts are the part `git apply` validates — a rewritten body
    /// with a stale header is rejected as "corrupt patch". Only git can
    /// prove the arithmetic.
    #[test]
    fn a_region_patch_stages_only_the_selected_line() {
        let dir = one_hunk_two_changes_repo();
        let p = dir.path();
        let diff = git(p, &["diff", "--", "a.txt"]);
        let lines: Vec<&str> = diff.lines().collect();

        let removal = row_containing(&diff, "-line 4");
        let whole = hunk_at(&lines, removal).expect("cursor inside the hunk");
        let region = whole
            .restrict_to_rows(removal..=removal, ApplyDirection::Forward)
            .expect("one removal selected");

        let repo = lattice_vcs::Repository::discover(p).expect("discover");
        lattice_vcs::Index::apply_patch(&repo, &region.to_patch(), true, false)
            .expect("git must accept the rewritten hunk");

        let staged = git(p, &["diff", "--cached", "--", "a.txt"]);
        assert!(
            staged.contains("-line 4") && !staged.contains("line 4 EDITED"),
            "only the removal of line 4 reached the index:\n{staged}"
        );
        // ` line 5` appears as CONTEXT in the index diff, which is the
        // point — it is unchanged there. What must be absent is either
        // changed form of it.
        assert!(
            !staged.contains("-line 5") && !staged.contains("line 5 EDITED"),
            "line 5's change stayed out of the index entirely:\n{staged}"
        );
        assert!(
            std::fs::read_to_string(p.join("a.txt"))
                .unwrap()
                .contains("line 4 EDITED"),
            "the worktree is untouched — staging is an index operation"
        );
    }

    /// The reverse direction, against real git: stage everything, then
    /// unstage one line of it. Proves the mirrored rules produce a
    /// patch git accepts REVERSED, which is a different validation path
    /// (it matches the new side, not the old).
    #[test]
    fn a_region_patch_unstages_only_the_selected_line() {
        let dir = one_hunk_two_changes_repo();
        let p = dir.path();
        git_ok(p, &["add", "a.txt"]);
        let staged_diff = git(p, &["diff", "--cached", "--", "a.txt"]);
        let lines: Vec<&str> = staged_diff.lines().collect();

        let addition = row_containing(&staged_diff, "+line 4 EDITED");
        let whole = hunk_at(&lines, addition).expect("cursor inside the staged hunk");
        let region = whole
            .restrict_to_rows(addition..=addition, ApplyDirection::Reverse)
            .expect("one addition selected");

        let repo = lattice_vcs::Repository::discover(p).expect("discover");
        lattice_vcs::Index::apply_patch(&repo, &region.to_patch(), true, true)
            .expect("git must accept the rewritten hunk reversed");

        let still = git(p, &["diff", "--cached", "--", "a.txt"]);
        assert!(
            !still.contains("line 4 EDITED"),
            "line 4's change left the index:\n{still}"
        );
        assert!(
            still.contains("line 5 EDITED"),
            "line 5's change is still staged:\n{still}"
        );
    }

    #[test]
    fn every_cursor_position_in_a_hunk_yields_the_same_patch() {
        // The user's cursor can be anywhere in the hunk when they press
        // `s`. All of those must produce byte-identical patches, or
        // staging would depend on where you happened to be standing.
        let dir = two_hunk_repo();
        let p = dir.path();
        let diff = git(p, &["diff", "--", "a.txt"]);
        let lines: Vec<&str> = diff.lines().collect();

        let start = nth_hunk_line(&diff, 0);
        let end = nth_hunk_line(&diff, 1);
        let expected = hunk_at(&lines, start).expect("at the header").to_patch();
        for cursor in start..end {
            let got = hunk_at(&lines, cursor)
                .unwrap_or_else(|| panic!("line {cursor} is inside hunk 1"))
                .to_patch();
            assert_eq!(
                got, expected,
                "cursor at line {cursor} produced a different patch"
            );
        }
    }
}

/// MG.50: the SOURCE line the cursor is looking at, inside a diff.
///
/// `<CR>` in emacs magit opens the file *at the line the code under the
/// cursor lives on*, not at the top. The diff already carries the
/// answer: a hunk's `@@` header names where its body starts in the new
/// file, so the target is that start plus however many new-side rows
/// precede the cursor within the hunk.
///
/// **Which rows count.** Only those that exist on the NEW side — context
/// (` `) and additions (`+`). A deletion (`-`) is not in the file being
/// opened, so it advances nothing; a cursor sitting on one resolves to
/// the position where the deleted text *was*, which is where a reader
/// looking at it wants to land. `\ No newline at end of file` belongs to
/// neither side.
///
/// Returns a 0-based buffer row, ready for
/// [`lattice_protocol::position::Position`]. `None` when the cursor is
/// not inside a hunk (a file entry, a section header, a `diff --git`
/// line) — the caller then opens at the top, which is what emacs does
/// for a file entry too.
pub fn source_line_at(read: impl Fn(usize) -> Option<String>, cursor: usize) -> Option<u32> {
    let header_line = enclosing_hunk_header(&read, cursor)?;
    let header_text = read(header_line)?;
    let header = header_text.trim_end();
    let start = parse_hunk_starts(header)?.new;
    let counts = parse_hunk_counts(header)?;

    // On the `@@` row itself: the hunk's first new-side line.
    if cursor == header_line {
        return u32::try_from(start.saturating_sub(1)).ok();
    }

    // Walk the body under the header's DECLARED counts rather than
    // until something stops looking like diff content — the same reason
    // `hunk_at_with` does, and the same trap the fold source hit: in
    // magit-status the row after a hunk is `"  modified src/foo.rs"`,
    // which begins with a space and is indistinguishable from a context
    // line by prefix alone. The counts end the hunk exactly.
    let (mut old_left, mut new_left) = (counts.old, counts.new);
    let mut advanced = 0usize;
    let mut row = header_line + 1;
    while old_left > 0 || new_left > 0 {
        // Tested INSIDE the loop, so it can only match while the hunk
        // still has body left. Testing it in the loop condition would
        // also match the row one PAST the last body line — which in
        // magit-status is the next file's entry row, and would hand back
        // a line number inside a file the cursor is not in.
        if row == cursor {
            return u32::try_from(start.saturating_sub(1) + advanced).ok();
        }
        let text = read(row)?;
        match text.chars().next() {
            // Context: present on both sides.
            None | Some(' ') => {
                old_left = old_left.checked_sub(1)?;
                new_left = new_left.checked_sub(1)?;
                advanced += 1;
            }
            // An addition exists only in the file being opened.
            Some('+') => {
                new_left = new_left.checked_sub(1)?;
                advanced += 1;
            }
            // A deletion is not in that file, so it advances nothing —
            // a cursor on one resolves to the position it occupied.
            Some('-') => old_left = old_left.checked_sub(1)?,
            // `\ No newline at end of file` belongs to neither side.
            Some('\\') => {}
            _ => return None,
        }
        row += 1;
    }
    // Counts exhausted without reaching the cursor: it sits past this
    // hunk's last body line, so there is no line in this file to name.
    None
}

#[cfg(test)]
mod source_line_tests {
    use super::source_line_at;

    const DIFF: &[&str] = &[
        "diff --git a/src/main.rs b/src/main.rs", // 0
        "index 111..222 100644",                  // 1
        "--- a/src/main.rs",                      // 2
        "+++ b/src/main.rs",                      // 3
        "@@ -10,3 +20,3 @@ fn main() {",          // 4  -> new starts at 20
        " context one",                           // 5  -> line 20
        "-deleted",                               // 6  -> not in the new file
        "+added",                                 // 7  -> line 21
        " context two",                           // 8  -> line 22
    ];

    fn read(i: usize) -> Option<String> {
        DIFF.get(i).map(|s| s.to_string())
    }

    /// The first body row is the header's own new-side start.
    #[test]
    fn the_first_body_row_is_the_hunks_start() {
        // `@@ +20` is 1-based; row 5 is buffer line 19.
        assert_eq!(source_line_at(read, 5), Some(19));
    }

    /// A deletion advances nothing — it is not in the file being opened.
    #[test]
    fn a_deletion_does_not_advance_the_source_line() {
        // Row 6 is the `-` itself. The deleted text is not in the file
        // being opened, so it resolves to the position it occupied —
        // just after `context one`, which is new line 21 (0-based 20).
        // That is where a reader looking at the deletion wants to land.
        assert_eq!(source_line_at(read, 6), Some(20));
        // Row 7 (`+added`) follows one context row and one deletion, so
        // only the context advanced: line 21 (0-based 20).
        assert_eq!(source_line_at(read, 7), Some(20));
    }

    /// Context after an addition keeps counting.
    #[test]
    fn context_after_an_addition_keeps_counting() {
        assert_eq!(source_line_at(read, 8), Some(21));
    }

    /// On the `@@` row, the hunk's start.
    #[test]
    fn the_header_row_resolves_to_the_hunk_start() {
        assert_eq!(source_line_at(read, 4), Some(19));
    }

    /// Outside a hunk there is no line to name — the caller opens at the
    /// top, which is what emacs does for a file entry.
    #[test]
    fn a_row_outside_any_hunk_has_no_source_line() {
        assert_eq!(source_line_at(read, 0), None);
        assert_eq!(source_line_at(read, 3), None);
    }

    /// The magit-status shape: a hunk with an ENTRY ROW under it.
    ///
    /// `"  modified src/other.rs"` starts with a space, so by prefix
    /// alone it is a context line. Walking until something stops looking
    /// like diff content would count it and hand back a line number
    /// inside a file the cursor is not in — the same trap that let a
    /// fold swallow the rest of the status buffer. The declared counts
    /// end the hunk exactly.
    #[test]
    fn an_entry_row_below_the_hunk_is_not_counted_as_context() {
        const STATUS: &[&str] = &[
            "diff --git a/a.rs b/a.rs", // 0
            "--- a/a.rs",               // 1
            "+++ b/a.rs",               // 2
            "@@ -1,1 +5,2 @@",          // 3
            " ctx",                     // 4  -> line 5
            "+added",                   // 5  -> line 6
            "  modified src/other.rs",  // 6  <- an entry row, not context
            "  modified src/third.rs",  // 7
        ];
        let r = |i: usize| STATUS.get(i).map(|s| s.to_string());
        assert_eq!(source_line_at(r, 4), Some(4), "` ctx` is new line 5");
        assert_eq!(source_line_at(r, 5), Some(5), "`+added` is new line 6");
        // Past the hunk's declared end: not inside it, so no line.
        assert_eq!(
            source_line_at(r, 6),
            None,
            "an entry row below the hunk must not resolve to a line \
             inside the hunk's file",
        );
        assert_eq!(source_line_at(r, 7), None);
    }

    /// A header with no comma (`@@ -1 +7 @@`) is a one-line range and
    /// still parses.
    #[test]
    fn a_single_line_range_parses() {
        const ONE: &[&str] = &["@@ -1 +7 @@", " ctx"];
        let r = |i: usize| ONE.get(i).map(|s| s.to_string());
        assert_eq!(source_line_at(r, 1), Some(6));
    }
}
