//! CM.2 (2026-07-22): the **error list** — a persistent,
//! cross-file list of navigable locations with an index, walked by
//! *generic* host dispatch (`:cnext` / `:cprev` / `:cc` and the
//! Builtin `]q` / `[q` chords).
//!
//! Placement mirrors `position_history` on [`crate::editor::Editor`]:
//! this is **core/host state**, NOT owned by any mode. By the
//! substrate-vs-mode-helper rule (uniform-host consumer ⟹ core),
//! its consumer is generic navigation dispatch, so it lives on the
//! host like the jump ring. Compilation (CM.3), diagnostics, and
//! search are all *producers* that populate it via
//! [`crate::editor::Editor::set_error_list`].
//!
//! See `docs/dev/architecture/compilation-mode.md` §3.

// CM.3a (2026-07-22): the entry + severity value types moved down to
// `lattice-protocol` so the below-host compilation parser and the
// `AppEffect::SetErrorList` payload share ONE type. Re-exported here so
// existing callers (`lattice_host::error_list::ErrorEntry`, the CM.2
// tests) are unchanged. The *list* stays host-local (below).
pub use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

/// The error list: an ordered set of entries plus a cursor
/// (`index`) into them. `:cnext` / `]q` walk the index (wrapping
/// vim-style); `:cc N` jumps to the Nth (1-based). Empty by default.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorList {
    entries: Vec<ErrorEntry>,
    index: usize,
}

impl ErrorList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the entire list, resetting the index to 0. This is
    /// the producer entry point (compilation / diagnostics / search
    /// call it via [`crate::editor::Editor::set_error_list`]).
    pub fn set(&mut self, entries: Vec<ErrorEntry>) {
        self.entries = entries;
        self.index = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// CM.4 (2026-07-22): read-only slice of the entries. The
    /// `:copen` producer clones these to build the `*problems*`
    /// multibuffer view.
    pub fn entries(&self) -> &[ErrorEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The 0-based index the list currently points at. Meaningless
    /// (returns 0) when the list is empty.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The entry the index currently points at, or `None` when the
    /// list is empty.
    pub fn current(&self) -> Option<&ErrorEntry> {
        self.entries.get(self.index)
    }

    /// Move the index by `delta`, wrapping vim-style (`:cnext` past
    /// the last entry wraps to the first; `:cprev` past the first
    /// wraps to the last), and return the entry now under the index.
    /// `None` only when the list is empty.
    pub fn step(&mut self, delta: i64) -> Option<&ErrorEntry> {
        let len = self.entries.len();
        if len == 0 {
            return None;
        }
        // `rem_euclid` keeps the result in `[0, len)` for any sign.
        let next = (self.index as i64 + delta).rem_euclid(len as i64);
        self.index = next as usize;
        self.entries.get(self.index)
    }

    /// Jump to the `n`th entry (1-based, vim `:cc N`). `n == None`
    /// (bare `:cc`) keeps the current index and returns the current
    /// entry. An out-of-range `n` (0, or past the end) leaves the
    /// index unchanged and returns `None`.
    pub fn jump_to(&mut self, n: Option<usize>) -> Option<&ErrorEntry> {
        match n {
            None => self.current(),
            Some(n) => {
                if n == 0 || n > self.entries.len() {
                    return None;
                }
                self.index = n - 1;
                self.entries.get(self.index)
            }
        }
    }

    /// CM.3b: point the index at the first entry whose `(path, line)`
    /// matches, returning `true` when one was found (index moved) and
    /// `false` otherwise (index unchanged). Used by the `<CR>`-jump in
    /// `*compilation*` to sync the error cursor to the entry the
    /// user jumped to. Match is on `path` + 0-based `line` only —
    /// column is ignored so a jump to a line with several column-
    /// distinct diagnostics selects the first at that line.
    pub fn set_index_to_matching(&mut self, path: &std::path::Path, line: u32) -> bool {
        if let Some(pos) = self
            .entries
            .iter()
            .position(|e| e.line == line && e.path == path)
        {
            self.index = pos;
            true
        } else {
            false
        }
    }

    /// CM.7: move to the first entry of the next (`delta > 0`) or
    /// previous (`delta < 0`) **file**, wrapping vim-style, and return
    /// the entry now under the index. `:cnextfile` (`delta = 1`) /
    /// `:cprevfile` (`delta = -1`); a count moves that many files.
    ///
    /// A "file" is a maximal run of consecutive entries sharing a
    /// `path` (compiler / tool output groups a file's locations
    /// together; the parser preserves that order). Both directions
    /// land on the **first** entry of the target file — symmetric and
    /// intuitive (vim's `:cpfile` technically lands on the *last* entry
    /// of the previous file; we deliberately land on the first so a
    /// following `:cnext` walks that file top-to-bottom). `None` only
    /// when the list is empty.
    pub fn step_file(&mut self, delta: i64) -> Option<&ErrorEntry> {
        if self.entries.is_empty() {
            return None;
        }
        // Group-start indices: index 0, plus every index whose path
        // differs from the previous entry's (contiguous same-path run =
        // one file group).
        let mut starts: Vec<usize> = vec![0];
        for i in 1..self.entries.len() {
            if self.entries[i].path != self.entries[i - 1].path {
                starts.push(i);
            }
        }
        // The group the index currently sits in = the last start <= index.
        let cur_group = starts.iter().rposition(|&s| s <= self.index).unwrap_or(0);
        let ngroups = starts.len() as i64;
        let next_group = (cur_group as i64 + delta).rem_euclid(ngroups) as usize;
        self.index = starts[next_group];
        self.entries.get(self.index)
    }

    /// Jump to the first entry (`:cfirst`). `None` when empty.
    pub fn first(&mut self) -> Option<&ErrorEntry> {
        if self.entries.is_empty() {
            return None;
        }
        self.index = 0;
        self.entries.first()
    }

    /// Jump to the last entry (`:clast`). `None` when empty.
    pub fn last(&mut self) -> Option<&ErrorEntry> {
        if self.entries.is_empty() {
            return None;
        }
        self.index = self.entries.len() - 1;
        self.entries.last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(path: &str, line: u32) -> ErrorEntry {
        ErrorEntry {
            path: PathBuf::from(path),
            line,
            col: 0,
            severity: ErrorSeverity::Error,
            message: format!("msg {line}"),
        }
    }

    #[test]
    fn empty_list_has_no_current() {
        let qf = ErrorList::new();
        assert!(qf.is_empty());
        assert_eq!(qf.len(), 0);
        assert_eq!(qf.current(), None);
    }

    #[test]
    fn set_resets_index_to_zero() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("a", 1), entry("b", 2), entry("c", 3)]);
        // Walk forward, then re-set: index must reset.
        qf.step(2);
        assert_eq!(qf.index(), 2);
        qf.set(vec![entry("x", 9), entry("y", 8)]);
        assert_eq!(qf.index(), 0);
        assert_eq!(qf.current(), Some(&entry("x", 9)));
    }

    #[test]
    fn step_wraps_forward_past_end_to_first() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("a", 1), entry("b", 2)]);
        assert_eq!(qf.current(), Some(&entry("a", 1)));
        assert_eq!(qf.step(1), Some(&entry("b", 2)));
        // Past the end wraps to the first.
        assert_eq!(qf.step(1), Some(&entry("a", 1)));
        assert_eq!(qf.index(), 0);
    }

    #[test]
    fn step_wraps_backward_past_start_to_last() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("a", 1), entry("b", 2), entry("c", 3)]);
        // At index 0, stepping back wraps to the last.
        assert_eq!(qf.step(-1), Some(&entry("c", 3)));
        assert_eq!(qf.index(), 2);
        assert_eq!(qf.step(-1), Some(&entry("b", 2)));
    }

    #[test]
    fn step_on_empty_is_none() {
        let mut qf = ErrorList::new();
        assert_eq!(qf.step(1), None);
        assert_eq!(qf.step(-1), None);
    }

    #[test]
    fn jump_to_is_one_based_and_bounds_checked() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("a", 1), entry("b", 2), entry("c", 3)]);
        // 1-based: :cc 2 -> index 1.
        assert_eq!(qf.jump_to(Some(2)), Some(&entry("b", 2)));
        assert_eq!(qf.index(), 1);
        // Out of range leaves index unchanged, returns None.
        assert_eq!(qf.jump_to(Some(0)), None);
        assert_eq!(qf.jump_to(Some(4)), None);
        assert_eq!(qf.index(), 1);
        // Bare :cc keeps the current index.
        assert_eq!(qf.jump_to(None), Some(&entry("b", 2)));
        assert_eq!(qf.index(), 1);
    }

    #[test]
    fn first_and_last() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("a", 1), entry("b", 2), entry("c", 3)]);
        assert_eq!(qf.last(), Some(&entry("c", 3)));
        assert_eq!(qf.index(), 2);
        assert_eq!(qf.first(), Some(&entry("a", 1)));
        assert_eq!(qf.index(), 0);
    }

    #[test]
    fn set_index_to_matching_finds_by_path_and_line() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("a.rs", 1), entry("b.rs", 5), entry("b.rs", 9)]);
        // Match on (path, line) moves the index and returns true.
        assert!(qf.set_index_to_matching(&PathBuf::from("b.rs"), 9));
        assert_eq!(qf.index(), 2);
        assert_eq!(qf.current(), Some(&entry("b.rs", 9)));
        // First match wins when several entries share (path, line)
        // is not the case here, but a different match re-points.
        assert!(qf.set_index_to_matching(&PathBuf::from("a.rs"), 1));
        assert_eq!(qf.index(), 0);
        // No match leaves the index unchanged and returns false.
        assert!(!qf.set_index_to_matching(&PathBuf::from("b.rs"), 42));
        assert_eq!(qf.index(), 0);
        assert!(!qf.set_index_to_matching(&PathBuf::from("zzz.rs"), 1));
        assert_eq!(qf.index(), 0);
    }

    #[test]
    fn set_index_to_matching_on_empty_is_false() {
        let mut qf = ErrorList::new();
        assert!(!qf.set_index_to_matching(&PathBuf::from("a.rs"), 0));
    }

    #[test]
    fn first_last_on_empty_is_none() {
        let mut qf = ErrorList::new();
        assert_eq!(qf.first(), None);
        assert_eq!(qf.last(), None);
    }

    #[test]
    fn step_file_lands_on_first_entry_of_each_file_and_wraps() {
        let mut qf = ErrorList::new();
        // Two files: a.rs (2 entries) then b.rs (2 entries).
        qf.set(vec![
            entry("a.rs", 1),
            entry("a.rs", 4),
            entry("b.rs", 2),
            entry("b.rs", 7),
        ]);
        // Start at index 0 (a.rs, first). Next file → b.rs first entry.
        assert_eq!(qf.step_file(1), Some(&entry("b.rs", 2)));
        assert_eq!(qf.index(), 2);
        // Next file again wraps back to a.rs's first entry.
        assert_eq!(qf.step_file(1), Some(&entry("a.rs", 1)));
        assert_eq!(qf.index(), 0);
        // Prev file wraps to b.rs's FIRST entry (not last — symmetric).
        assert_eq!(qf.step_file(-1), Some(&entry("b.rs", 2)));
        assert_eq!(qf.index(), 2);
    }

    #[test]
    fn step_file_from_mid_file_goes_to_next_file_start() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("a.rs", 1), entry("a.rs", 4), entry("b.rs", 2)]);
        // Sit on a.rs's SECOND entry, then :cnextfile → b.rs first.
        qf.step(1);
        assert_eq!(qf.index(), 1);
        assert_eq!(qf.step_file(1), Some(&entry("b.rs", 2)));
        assert_eq!(qf.index(), 2);
    }

    #[test]
    fn step_file_single_file_is_stable() {
        let mut qf = ErrorList::new();
        qf.set(vec![entry("only.rs", 1), entry("only.rs", 9)]);
        // One file group → next/prev file both resolve to its start.
        assert_eq!(qf.step_file(1), Some(&entry("only.rs", 1)));
        assert_eq!(qf.index(), 0);
        assert_eq!(qf.step_file(-1), Some(&entry("only.rs", 1)));
        assert_eq!(qf.index(), 0);
    }

    #[test]
    fn step_file_on_empty_is_none() {
        let mut qf = ErrorList::new();
        assert_eq!(qf.step_file(1), None);
        assert_eq!(qf.step_file(-1), None);
    }
}
