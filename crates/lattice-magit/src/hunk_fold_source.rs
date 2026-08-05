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
        let end = file_starts
            .get(n + 1)
            .map(|&next| next - 1)
            .unwrap_or(total.saturating_sub(1));
        if end > start {
            folds.push(Fold {
                start_line: start as u32,
                end_line: end as u32,
                closed: false,
                identity: Some(fold_identity("magit:diff-file", &file_of(&lines[start]), 0)),
            });
        }
        folds.extend(hunk_folds(lines, &file_of(&lines[start]), start, end));
    }

    // No file headers: fold the hunks that are there, against a single
    // unnamed file. Without this, magit-status's single-file inline
    // expansions would lose the hunk folds they have today.
    if file_starts.is_empty() {
        folds.extend(hunk_folds(lines, "", 0, total.saturating_sub(1)));
    }
    folds
}

/// One fold per `@@` hunk between `start` and `end` inclusive.
fn hunk_folds(lines: &[String], file: &str, start: usize, end: usize) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut ordinal = 0usize;
    let mut open: Option<usize> = None;
    if lines.is_empty() {
        return folds;
    }
    for l in start..=end.min(lines.len() - 1) {
        if !lines[l].starts_with("@@") {
            continue;
        }
        if let Some(h) = open {
            // The fold just closed belongs to the hunk that was open,
            // so the ordinal advances AFTER it is emitted. Bumping on
            // sight would leave ordinal 0 unused and land every hunk's
            // closed state on its neighbour after a refresh.
            if l > h + 1 {
                folds.push(Fold {
                    start_line: h as u32,
                    end_line: (l - 1) as u32,
                    closed: false,
                    identity: Some(fold_identity("magit:diff-hunk", file, ordinal)),
                });
            }
            ordinal += 1;
        }
        open = Some(l);
    }
    if let Some(h) = open
        && end > h
    {
        folds.push(Fold {
            start_line: h as u32,
            end_line: end as u32,
            closed: false,
            identity: Some(fold_identity("magit:diff-hunk", file, ordinal)),
        });
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
        let total = snap.buffer.line_count();
        let lines: Vec<String> = (0..total)
            .map(|i| snap.buffer.line(i as u32).unwrap_or_default())
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
@@ -1,2 +1,3 @@
 ctx
+added
@@ -10,2 +11,3 @@
 ctx
+more
diff --git a/two.txt b/two.txt
index 333..444 100644
--- a/two.txt
+++ b/two.txt
@@ -1,2 +1,3 @@
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
@@ -1,2 +1,3 @@
 ctx
+added
@@ -10,2 +11,3 @@
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

    /// An empty buffer, and a buffer with no diff at all, produce no
    /// folds rather than a zero-width one.
    #[test]
    fn text_with_no_diff_produces_no_folds() {
        assert!(diff_folds(&[]).is_empty());
        assert!(diff_folds(&lines("just some prose\nand more")).is_empty());
    }
}
