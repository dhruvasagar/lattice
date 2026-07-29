//! MG.18b: locating the hunk at a cursor, and turning it into a patch.
//!
//! Pure functions over diff text. No buffer, no store, no `Editor` —
//! `magit.md` §7.2's substrate-helper pattern, and what lets MG.22
//! relocate this into `magit-hunk-mode` rather than rewrite it.
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

// MG.18b lands the parser; MG.18c wires it to `s` / `u` / `x`. Until
// then every item here is exercised only by tests. Remove this with
// MG.18c — if it survives that slice, something did not get wired.
#![allow(dead_code)]

use std::fmt::Write as _;

use crate::highlight::{DiffLineClass, classify_diff_line};

/// One hunk, plus the file header needed to apply it on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HunkPatch {
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

/// Locate the hunk containing `cursor` in `lines`.
///
/// Returns `None` when the cursor is not inside a hunk body — on a
/// section header, a file entry, a commit message, or a diff's own
/// `---`/`+++` header lines. Callers fall back to file-level staging,
/// which is what keeps every pre-MG.18 behaviour intact.
pub(crate) fn hunk_at(lines: &[&str], cursor: usize) -> Option<HunkPatch> {
    let header_line = enclosing_hunk_header(lines, cursor)?;
    let counts = parse_hunk_counts(lines[header_line].trim_end())?;

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
    let mut hunk = vec![lines[header_line].trim_end().to_string()];
    let mut idx = header_line + 1;
    while idx < lines.len() && (old_seen < counts.old || new_seen < counts.new) {
        let line = lines[idx];
        // "\ No newline at end of file" annotates the line above and
        // counts toward neither side, but must ride along: dropping it
        // changes whether the applied result ends in a newline.
        if line.starts_with('\\') {
            hunk.push(line.trim_end().to_string());
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
        hunk.push(line.trim_end().to_string());
        idx += 1;
    }

    // A hunk whose declared counts were not satisfied is truncated;
    // applying it would corrupt. Refuse instead.
    if old_seen < counts.old || new_seen < counts.new {
        return None;
    }

    let header = file_header_above(lines, header_line)?;
    Some(HunkPatch {
        header,
        hunk,
        header_line,
        end_line: idx,
    })
}

/// The `@@` line at or above `cursor`, without crossing into a
/// different file's diff or out of the diff entirely.
fn enclosing_hunk_header(lines: &[&str], cursor: usize) -> Option<usize> {
    if cursor >= lines.len() {
        return None;
    }
    for idx in (0..=cursor).rev() {
        let line = lines[idx];
        match classify_diff_line(line) {
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
fn file_header_above(lines: &[&str], header_line: usize) -> Option<Vec<String>> {
    let start = (0..header_line)
        .rev()
        .find(|&i| classify_diff_line(lines[i]) == DiffLineClass::FileCommand)?;
    let mut header = Vec::new();
    for line in lines.iter().take(header_line).skip(start) {
        if classify_diff_line(line) == DiffLineClass::Hunk {
            break;
        }
        header.push(line.trim_end().to_string());
    }
    Some(header)
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
        assert!(patch.ends_with('\n'), "git apply rejects an unterminated patch");
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
            assert_eq!(got, expected, "cursor at line {cursor} produced a different patch");
        }
    }
}
