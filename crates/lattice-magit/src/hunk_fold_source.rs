//! MG.45: folding *inside* a unified diff — file, then hunk.
//!
//! Owned by [`crate::MagitHunkMode`], which activates on exactly the
//! five majors that render diff text (status, diff, commit, revision,
//! stash-show). Before this, only magit-status had a fold source at
//! all, so `<Tab>` in the commit or diff buffer had nothing
//! diff-aware to act on — and the source it did have emitted no FILE
//! level, so a multi-file expansion (`git show`, `stash show -p`) put
//! every file's hunks directly under the entry as siblings.
//!
//! **The division of ownership.** magit-status owns the *entry* fold
//! — that is about which rows it expanded, which only it knows. This
//! source owns everything *within* the diff text, which is derived
//! from the buffer and identical in every buffer that has one. The two
//! compose by range containment, the way the fold engine expresses
//! nesting everywhere else, so in magit-status a commit expands as
//! entry ▸ file ▸ hunk without either source knowing about the other.
//!
//! Reading the buffer rather than a side table is what makes one
//! source serve five majors: there is no per-major state to consult.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use lattice_core::{BufferId, Fold, FoldSource, ProviderId};
use lattice_mode::BufferStoreHandle;

/// Namespace for per-buffer hunk-fold ids, OR'd with the buffer id so
/// several diff buffers register distinct overlay sources.
pub const MAGIT_HUNK_FOLD_NAMESPACE: u64 = 0x6A61_0002_0000_0000;

pub struct MagitHunkFoldSource {
    id: ProviderId,
    store: Arc<BufferStoreHandle>,
    buffer_id: BufferId,
}

impl MagitHunkFoldSource {
    pub fn new(store: Arc<BufferStoreHandle>, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(MAGIT_HUNK_FOLD_NAMESPACE | buffer_id.0 as u64),
            store,
            buffer_id,
        }
    }
}

/// A fold's identity, which carries its **closed state** across
/// recomputes.
///
/// Keyed by what the fold is ABOUT — the file path, and the hunk's
/// ordinal within that file — never by the line it sits on. `gr`
/// rewrites the buffer and moves everything; a line-keyed identity
/// would reopen every fold on the refresh that had something to
/// report. Same reasoning as `fold_source::fold_identity`, which was
/// fixed for the same bug.
fn fold_identity(namespace: &str, file: &str, ordinal: usize) -> u64 {
    let mut h = DefaultHasher::new();
    namespace.hash(&mut h);
    file.hash(&mut h);
    ordinal.hash(&mut h);
    h.finish()
}

/// Exposed so `fold_source`'s test can assert the two providers'
/// identities cannot collide — they nest, and a collision would make
/// the outer fold inherit an inner one's closed state.
#[cfg(test)]
pub(crate) fn identity_for_test(namespace: &str, file: &str, ordinal: usize) -> u64 {
    fold_identity(namespace, file, ordinal)
}

/// Is this the first line of a file's section in a unified diff?
fn is_file_header(line: &str) -> bool {
    line.starts_with("diff --git ") || line.starts_with("diff --cc ")
}

/// The path a `diff --git a/x b/x` header names.
///
/// Takes the **b-side**, which is where the file ends up: a rename
/// reports different a/b paths, and the post-image is the one the rest
/// of the hunk machinery (and the user) means by "this file".
fn file_of(header: &str) -> String {
    let rest = header
        .strip_prefix("diff --git ")
        .or_else(|| header.strip_prefix("diff --cc "))
        .unwrap_or(header)
        .trim();
    match rest.split_once(" b/") {
        Some((_, b)) => b.to_string(),
        // `diff --cc <path>` names one path, and a header we cannot
        // split is still a stable key for identity purposes.
        None => rest.trim_start_matches("a/").to_string(),
    }
}

/// The `(old, new)` line counts a `@@ -a,b +c,d @@` header declares.
///
/// A count may be omitted (`@@ -1 +1 @@`), which means 1. `None` for a
/// combined header (`@@@`, from `diff --cc`), whose body lines carry
/// one prefix char per parent — [`hunk_extent`] falls back to a prefix
/// scan for those.
fn hunk_counts(header: &str) -> Option<(usize, usize)> {
    if header.starts_with("@@@") {
        return None;
    }
    let body = header.strip_prefix("@@")?;
    let ranges = body.split("@@").next()?;
    let mut old = None;
    let mut new = None;
    for tok in ranges.split_whitespace() {
        let (slot, digits) = match tok.split_at_checked(1)? {
            ("-", rest) => (&mut old, rest),
            ("+", rest) => (&mut new, rest),
            _ => continue,
        };
        let count = match digits.split_once(',') {
            Some((_, c)) => c.parse().ok()?,
            // No comma: a one-line range.
            None => 1usize,
        };
        *slot = Some(count);
    }
    Some((old?, new?))
}

/// Is `line` a line of a hunk's body?
///
/// `\` is git's "No newline at end of file" marker, which belongs to
/// the hunk but counts against neither side. An empty line is a
/// context line whose trailing space was stripped somewhere upstream.
fn is_body_line(line: &str) -> bool {
    line.is_empty() || line.starts_with([' ', '+', '-', '\\'])
}

/// The last line belonging to the hunk whose header is at `header`.
///
/// **This is the bound the whole file turns on.** A hunk declares its
/// own length, so its extent is derived from that rather than from
/// "wherever the next `@@` or `diff --git` happens to be". In a
/// pure-diff buffer the two agree. In magit-status they do not: the
/// diff is an *embedded fragment*, and the rows after it are entry
/// rows, section headers and commit rows. Bounding by the next marker
/// (or, for the final hunk, by the end of the buffer) is what made a
/// fold swallow the rest of the status buffer.
///
/// The declared counts are an upper bound, not a promise — the scan
/// also stops at the first line that is not hunk body, so a truncated
/// or hand-written patch still terminates at its real end. `  modified
/// a.rs` is why the counts have to lead: an entry row is
/// indistinguishable from a context line by prefix alone.
fn hunk_extent(lines: &[String], header: usize) -> usize {
    let total = lines.len();
    let (mut old_left, mut new_left) = match hunk_counts(&lines[header]) {
        Some(c) => c,
        // Combined diff: fall back to a prefix scan, which is still
        // bounded by the first non-body line.
        None => {
            let mut last = header;
            for (i, line) in lines.iter().enumerate().take(total).skip(header + 1) {
                if !is_body_line(line) {
                    break;
                }
                last = i;
            }
            return last;
        }
    };
    let mut last = header;
    for (i, line) in lines.iter().enumerate().take(total).skip(header + 1) {
        if old_left == 0 && new_left == 0 {
            break;
        }
        let consumed = match line.chars().next() {
            // A context line spends one from each side.
            None | Some(' ') => {
                if old_left == 0 || new_left == 0 {
                    break;
                }
                old_left -= 1;
                new_left -= 1;
                true
            }
            Some('-') => {
                if old_left == 0 {
                    break;
                }
                old_left -= 1;
                true
            }
            Some('+') => {
                if new_left == 0 {
                    break;
                }
                new_left -= 1;
                true
            }
            // Belongs to the hunk, counts against neither side.
            Some('\\') => true,
            _ => false,
        };
        if !consumed {
            break;
        }
        last = i;
    }
    last
}

/// Compute file ▸ hunk folds over `lines`.
///
/// Split from the `FoldSource` impl so it is testable without a live
/// buffer and a spawned document actor — the same split
/// `magit_diff_mode::source_for_scope` uses.
pub(crate) fn diff_folds(lines: &[String]) -> Vec<Fold> {
    let mut folds = Vec::new();
    let total = lines.len();
    if total == 0 {
        return folds;
    }

    // Where each file's section starts. A diff with no `diff --git`
    // header at all (a bare `git stash show -p` style patch, or the
    // fragment magit-status inlines for one file) still gets hunk
    // folds below — the file level is simply absent because the text
    // does not express one.
    let file_starts: Vec<usize> = (0..total).filter(|&i| is_file_header(&lines[i])).collect();

    for (n, &start) in file_starts.iter().enumerate() {
        // Scan no further than the next file's header; within that,
        // each hunk bounds itself.
        let limit = file_starts.get(n + 1).copied().unwrap_or(total);
        let file = file_of(&lines[start]);
        let hunks = hunk_folds(lines, &file, start, limit);
        // The file section ends where its last hunk ends. A section
        // with no hunks at all (a pure rename or mode change) has only
        // its metadata rows, which is not something to fold.
        if let Some(end) = hunks.iter().map(|h| h.end_line).max()
            && end > start as u32
        {
            folds.push(Fold {
                start_line: start as u32,
                end_line: end,
                closed: false,
                identity: Some(fold_identity("magit:diff-file", &file, 0)),
            });
        }
        folds.extend(hunks);
    }

    // No file headers: fold the hunks that are there, against a single
    // unnamed file. Without this, magit-status's single-file inline
    // expansions would lose the hunk folds they have today.
    if file_starts.is_empty() {
        folds.extend(hunk_folds(lines, "", 0, total));
    }
    folds
}

/// One fold per `@@` hunk between `start` and `limit` (exclusive).
fn hunk_folds(lines: &[String], file: &str, start: usize, limit: usize) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut ordinal = 0usize;
    let mut l = start;
    while l < limit.min(lines.len()) {
        if !lines[l].starts_with("@@") {
            l += 1;
            continue;
        }
        let end = hunk_extent(lines, l).min(limit.saturating_sub(1));
        if end > l {
            folds.push(Fold {
                start_line: l as u32,
                end_line: end as u32,
                closed: false,
                identity: Some(fold_identity("magit:diff-hunk", file, ordinal)),
            });
        }
        // The ordinal advances per hunk SEEN, not per fold emitted, so
        // a one-line hunk (which has nothing to fold) does not shift
        // its successors' identities and land their closed state on a
        // neighbour after a refresh.
        ordinal += 1;
        l = end.max(l) + 1;
    }
    folds
}

impl FoldSource for MagitHunkFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<Fold> {
        let Some(handle) = self.store.handle_for(self.buffer_id) else {
            return Vec::new();
        };
        let snap = handle.snapshot();
        let total = snap.buffer.content_line_count();
        let lines: Vec<String> = (0..total)
            .map(|i| snap.buffer.line(i).unwrap_or_default())
            .collect();
        diff_folds(&lines)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    const TWO_FILES: &str = "\
diff --git a/one.txt b/one.txt
index 111..222 100644
--- a/one.txt
+++ b/one.txt
@@ -1,1 +1,2 @@
 ctx
+added
@@ -10,1 +11,2 @@
 ctx
+more
diff --git a/two.txt b/two.txt
index 333..444 100644
--- a/two.txt
+++ b/two.txt
@@ -1,1 +1,2 @@
 ctx
+other";

    /// MG.45: **a multi-file diff gets a FILE level.**
    ///
    /// This is the nesting that was missing: `git show` and
    /// `stash show -p` are multi-file, and without a per-file fold
    /// every file's hunks sat as siblings under the entry, so folding
    /// "this file" was not expressible at all.
    #[test]
    fn each_file_in_a_diff_gets_its_own_fold() {
        let folds = diff_folds(&lines(TWO_FILES));
        let file_starts: Vec<u32> = folds
            .iter()
            .filter(|f| f.start_line == 0 || f.start_line == 10)
            .map(|f| f.start_line)
            .collect();
        assert!(file_starts.contains(&0), "one.txt starts at 0: {folds:?}");
        assert!(file_starts.contains(&10), "two.txt starts at 10: {folds:?}");
    }

    /// A file's fold must CONTAIN its hunks — containment is how the
    /// fold engine expresses nesting, so a file fold that stopped
    /// short would leave its last hunk a sibling rather than a child.
    #[test]
    fn a_files_fold_contains_its_own_hunks() {
        let folds = diff_folds(&lines(TWO_FILES));
        let file = folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("one.txt has a fold");
        let hunks: Vec<&Fold> = folds
            .iter()
            .filter(|f| f.start_line == 4 || f.start_line == 7)
            .collect();
        assert_eq!(hunks.len(), 2, "one.txt has two hunks: {folds:?}");
        for h in hunks {
            assert!(
                h.start_line >= file.start_line && h.end_line <= file.end_line,
                "hunk {h:?} must sit inside file {file:?}",
            );
        }
    }

    /// One file's fold must not swallow the next file's rows.
    #[test]
    fn a_files_fold_stops_before_the_next_file() {
        let folds = diff_folds(&lines(TWO_FILES));
        let first = folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("one.txt has a fold");
        assert_eq!(
            first.end_line, 9,
            "one.txt ends on the row before `diff --git a/two.txt`: {folds:?}",
        );
    }

    /// MG.45: **identity is keyed by file and ordinal, never by line.**
    ///
    /// `gr` rewrites the buffer and moves everything. A line-keyed
    /// identity reopens every fold on exactly the refresh that had
    /// something to report — the bug already fixed once in
    /// `fold_source.rs`, and it would have been reintroduced here.
    #[test]
    fn identity_survives_the_diff_moving() {
        let shifted = format!("preamble\n{TWO_FILES}");
        let a = diff_folds(&lines(TWO_FILES));
        let b = diff_folds(&lines(&shifted));
        let ids = |f: &[Fold]| -> Vec<Option<u64>> { f.iter().map(|x| x.identity).collect() };
        assert_eq!(
            ids(&a),
            ids(&b),
            "the same diff one row lower must keep every identity",
        );
        // ...and the ranges DID move, so the test is not vacuous.
        assert_ne!(a[0].start_line, b[0].start_line);
    }

    /// Two files' folds must not collide, or folding one would fold
    /// the other.
    #[test]
    fn different_files_have_different_identities() {
        let folds = diff_folds(&lines(TWO_FILES));
        let one = folds.iter().find(|f| f.start_line == 0).unwrap().identity;
        let two = folds.iter().find(|f| f.start_line == 10).unwrap().identity;
        assert_ne!(one, two);
    }

    /// A single-file fragment with no `diff --git` header still folds
    /// its hunks.
    ///
    /// magit-status inlines exactly this shape, and losing hunk folds
    /// there would be a regression on the one buffer that already
    /// worked.
    #[test]
    fn a_headerless_fragment_still_gets_hunk_folds() {
        let text = "\
@@ -1,1 +1,2 @@
 ctx
+added
@@ -10,1 +11,2 @@
 ctx
+more";
        let folds = diff_folds(&lines(text));
        assert_eq!(folds.len(), 2, "two hunks, no file level: {folds:?}");
        assert_eq!(folds[0].start_line, 0);
        assert_eq!(folds[1].start_line, 3);
    }

    /// A rename reports different a/ and b/ paths; the b-side is where
    /// the file ends up and is what the rest of magit means by it.
    #[test]
    fn a_rename_is_keyed_by_where_the_file_ends_up() {
        assert_eq!(file_of("diff --git a/old.txt b/new.txt"), "new.txt");
        assert_eq!(file_of("diff --git a/same.txt b/same.txt"), "same.txt");
    }

    /// MG.45: **the composition magit-status produces — entry ▸ file
    /// ▸ hunk.**
    ///
    /// This is the nesting the user reported as wrong. A commit entry
    /// expands to a multi-file patch, so the entry fold (from
    /// `MagitStatusFoldSource`) must CONTAIN a file fold per file,
    /// each containing its own hunks. The two sources never see each
    /// other, so this asserts they compose by containment.
    #[test]
    fn a_status_entry_fold_contains_the_file_and_hunk_folds() {
        // A commit row at line 0, its patch inlined below it.
        let mut buffer = vec!["  abc1234 some commit".to_string()];
        buffer.extend(lines(TWO_FILES));
        let entry_start = 0u32;
        let entry_end = (buffer.len() - 1) as u32;

        let folds = diff_folds(&buffer);
        assert!(
            folds.len() >= 4,
            "two files and three hunks must all fold: {folds:?}",
        );
        for f in &folds {
            assert!(
                f.start_line > entry_start && f.end_line <= entry_end,
                "{f:?} must sit strictly inside the entry fold \
                 [{entry_start}, {entry_end}]",
            );
        }

        // And the file folds each contain their own hunks — the level
        // that was missing entirely before MG.45.
        let files: Vec<&Fold> = folds
            .iter()
            .filter(|f| buffer[f.start_line as usize].starts_with("diff --git"))
            .collect();
        assert_eq!(files.len(), 2, "one fold per file: {folds:?}");
        let hunks: Vec<&Fold> = folds
            .iter()
            .filter(|f| buffer[f.start_line as usize].starts_with("@@"))
            .collect();
        assert_eq!(hunks.len(), 3, "three hunks across the two files");
        for h in hunks {
            assert!(
                files
                    .iter()
                    .any(|f| h.start_line >= f.start_line && h.end_line <= f.end_line),
                "hunk {h:?} must nest inside one of {files:?}",
            );
        }
    }

    /// The magit-status shape: a diff is an *embedded fragment*, not
    /// the whole buffer. Rows follow it that are not diff text at all.
    ///
    /// `  modified   b.rs` is an entry row; `Recent commits` is a
    /// section header. A file fold that ran to the next `diff --git`
    /// (or, for the last file, to the end of the buffer) swallowed
    /// every one of them — folding one file hid the rest of the status
    /// buffer. A hunk's extent is declared by its own `@@` header, and
    /// that is what must bound it.
    const STATUS_BUFFER: &str = "\
Unstaged changes (2)
  modified   a.rs
diff --git a/a.rs b/a.rs
index 111..222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,1 +1,2 @@
 ctx
+added
  modified   b.rs
diff --git a/b.rs b/b.rs
index 333..444 100644
--- a/b.rs
+++ b/b.rs
@@ -1,1 +1,2 @@
 ctx
+other
Recent commits
  abc1234 some commit
  def5678 another commit";

    /// MG.46: **a file fold must stop at the end of its own diff**, not
    /// run on to the next `diff --git` header.
    ///
    /// a.rs's diff ends on `+added` (line 8). Line 9 is b.rs's *entry
    /// row* — a status row that belongs to no diff at all.
    #[test]
    fn a_file_fold_stops_at_the_end_of_its_diff_not_the_next_header() {
        let buf = lines(STATUS_BUFFER);
        let folds = diff_folds(&buf);
        let a = folds
            .iter()
            .find(|f| f.start_line == 2)
            .expect("a.rs has a file fold");
        assert_eq!(
            a.end_line, 8,
            "a.rs ends on `+added`, not on b.rs's entry row: {folds:?}",
        );
    }

    /// MG.46: **the last file's fold must not run to the end of the
    /// buffer.**
    ///
    /// This is the reported symptom: in magit-status the rows after the
    /// final diff are section headers and commit entries, and folding
    /// the last file hid all of them.
    #[test]
    fn the_last_file_fold_stops_at_the_end_of_its_diff() {
        let buf = lines(STATUS_BUFFER);
        let folds = diff_folds(&buf);
        let b = folds
            .iter()
            .find(|f| f.start_line == 10)
            .expect("b.rs has a file fold");
        assert_eq!(
            b.end_line, 16,
            "b.rs ends on `+other`, not on the last commit row: {folds:?}",
        );
        assert!(
            buf[b.end_line as usize].starts_with('+'),
            "the last row of a file fold is diff text: {:?}",
            buf[b.end_line as usize],
        );
    }

    /// The same bound applies to hunks: a hunk ends where its `@@`
    /// header says it does, so it never reaches rows that follow the
    /// diff.
    #[test]
    fn a_hunk_fold_stops_at_the_end_of_its_own_body() {
        let buf = lines(STATUS_BUFFER);
        let folds = diff_folds(&buf);
        for f in folds
            .iter()
            .filter(|f| buf[f.start_line as usize].starts_with("@@"))
        {
            let last = &buf[f.end_line as usize];
            assert!(
                last.starts_with([' ', '+', '-', '\\']),
                "hunk {f:?} ends on non-diff row {last:?}",
            );
        }
    }

    /// No fold from this source may cover a row that is not part of a
    /// diff — the invariant the two tests above are instances of.
    #[test]
    fn no_fold_covers_a_non_diff_row() {
        let buf = lines(STATUS_BUFFER);
        // Section headers and entry rows — everything that is not part
        // of either inlined diff.
        let non_diff: Vec<u32> = vec![0, 1, 9, 17, 18, 19];
        for f in diff_folds(&buf) {
            for &row in &non_diff {
                assert!(
                    !(f.start_line <= row && row <= f.end_line),
                    "fold {f:?} covers non-diff row {row} ({:?})",
                    buf[row as usize],
                );
            }
        }
    }

    /// An empty buffer, and a buffer with no diff at all, produce no
    /// folds rather than a zero-width one.
    #[test]
    fn text_with_no_diff_produces_no_folds() {
        assert!(diff_folds(&[]).is_empty());
        assert!(diff_folds(&lines("just some prose\nand more")).is_empty());
    }
}
