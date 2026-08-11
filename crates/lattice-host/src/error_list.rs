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
pub use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity, ErrorSource};

/// EP.2: what a slice write should do with the navigation index.
///
/// Private because the choice is expressed at the call site by picking
/// [`ErrorList::set`] or [`ErrorList::refresh`] — naming the two
/// intentions rather than passing a bool nobody can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    /// New run: start at the top.
    Reset,
    /// Live refresh: keep the user where they were.
    Keep,
}

/// The error list: an ordered set of entries plus a cursor
/// (`index`) into them. `:cnext` / `]q` walk the index (wrapping
/// vim-style); `:cc N` jumps to the Nth (1-based). Empty by default.
///
/// EP.1 (2026-08-10): entries are held as **per-source slices**, not
/// one flat vec, and a producer's write replaces only its own slice.
/// The flat view every consumer reads ([`Self::entries`]) is the
/// concatenation in [`ErrorSource::PRESENTATION_ORDER`], each slice
/// keeping its producer's own ordering.
///
/// Two producers on one untagged list is a clobber: the language server
/// republishes on every edit-debounce, so its feed would wipe a compile
/// run's entries while the user walked them. Sorting the merged list
/// instead of concatenating was rejected — producer order is
/// information (rustc emits the root cause before the cascade).
///
/// See `docs/dev/architecture/error-list.md` §3.1.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorList {
    /// One run per source, kept in `PRESENTATION_ORDER`. A source with
    /// nothing to show simply has no run here.
    slices: Vec<(ErrorSource, Vec<ErrorEntry>)>,
    /// Flat concatenation of `slices`, rebuilt on every write. Cached
    /// rather than recomputed because `entries()` returns a borrowed
    /// slice and every navigation call reads it.
    flat: Vec<ErrorEntry>,
    index: usize,
}

impl ErrorList {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace `source`'s entries for a **new run**, leaving every
    /// other source's alone.
    ///
    /// This is the producer entry point (compilation / LSP call it via
    /// [`crate::editor::Editor::set_error_list`]). An empty `entries`
    /// clears just that source's run — which is what a fresh compile
    /// with no errors means, and it must not disturb the language
    /// server's diagnostics sitting alongside.
    ///
    /// Resets the index to 0, which is what a new run means: the
    /// user asked for fresh results and expects to start at the top.
    /// For a *refresh* of an existing feed use [`Self::refresh`] —
    /// resetting there would throw a user walking entry 7 back to entry
    /// 1 every time they typed.
    pub fn set(&mut self, source: ErrorSource, entries: Vec<ErrorEntry>) {
        self.splice(source, entries, Anchor::Reset);
    }

    /// EP.2 (2026-08-10): replace `source`'s entries the way a *live
    /// feed* does — keeping the user where they were.
    ///
    /// The distinction from [`Self::set`] is the **producer's to
    /// declare**, never inferred from the data: a language server
    /// republishing after a keystroke is a refresh, a compile run is a
    /// new run, and the two are indistinguishable by looking at the
    /// entries. Without this, the default-on diagnostic feed would make
    /// `:cnext` unusable — which is precisely the experience someone
    /// would set `lsp.diagnostics-to-error-list = false` to escape.
    ///
    /// Re-anchoring, in order: the same entry by `(path, message)`
    /// (tolerant of line drift, since typing above an error moves it);
    /// else the first entry of the same path at-or-after the old line;
    /// else index 0.
    pub fn refresh(&mut self, source: ErrorSource, entries: Vec<ErrorEntry>) {
        self.splice(source, entries, Anchor::Keep);
    }

    /// Shared body of [`Self::set`] and [`Self::refresh`].
    fn splice(&mut self, source: ErrorSource, entries: Vec<ErrorEntry>, anchor: Anchor) {
        // Capture the identity of the entry under the index BEFORE the
        // rebuild — afterwards the ordinal is meaningless.
        let previous = match anchor {
            Anchor::Keep => self.current().cloned(),
            Anchor::Reset => None,
        };

        match self.slices.iter_mut().find(|(s, _)| *s == source) {
            Some(run) => run.1 = entries,
            None => self.slices.push((source, entries)),
        }
        self.slices.retain(|(_, e)| !e.is_empty());
        self.slices.sort_by_key(|(s, _)| {
            ErrorSource::PRESENTATION_ORDER
                .iter()
                .position(|p| p == s)
                .unwrap_or(usize::MAX)
        });
        self.rebuild_flat();

        self.index = match previous {
            None => 0,
            Some(prev) => self.reanchor(&prev),
        };
    }

    /// Where the index should land after a refresh, given the entry it
    /// pointed at before. See [`Self::refresh`] for the ordering.
    fn reanchor(&self, prev: &ErrorEntry) -> usize {
        // 1. The same entry, wherever it moved to. Line is excluded
        //    from the match on purpose: editing above an error shifts
        //    its line without making it a different error.
        if let Some(i) = self
            .flat
            .iter()
            .position(|e| e.path == prev.path && e.message == prev.message)
        {
            return i;
        }
        // 2. It is gone — land on the next surviving entry in the same
        //    file, so the user keeps working where they were.
        if let Some(i) = self
            .flat
            .iter()
            .position(|e| e.path == prev.path && e.line >= prev.line)
        {
            return i;
        }
        // 3. The file itself is clean now. Start over rather than
        //    pointing somewhere arbitrary.
        0
    }

    /// Rebuild the cached flat view from `slices`.
    fn rebuild_flat(&mut self) {
        self.flat = self
            .slices
            .iter()
            .flat_map(|(_, entries)| entries.iter().cloned())
            .collect();
    }

    /// The entries contributed by one source, or an empty slice when it
    /// has none.
    pub fn entries_from(&self, source: ErrorSource) -> &[ErrorEntry] {
        self.slices
            .iter()
            .find(|(s, _)| *s == source)
            .map(|(_, e)| e.as_slice())
            .unwrap_or(&[])
    }

    /// Which sources currently contribute entries, in presentation
    /// order.
    pub fn sources(&self) -> Vec<ErrorSource> {
        self.slices.iter().map(|(s, _)| *s).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.flat.is_empty()
    }

    /// CM.4 (2026-07-22): read-only slice of the entries. The
    /// `:copen` producer clones these to build the `*problems*`
    /// multibuffer view.
    pub fn entries(&self) -> &[ErrorEntry] {
        &self.flat
    }

    pub fn len(&self) -> usize {
        self.flat.len()
    }

    /// The 0-based index the list currently points at. Meaningless
    /// (returns 0) when the list is empty.
    pub fn index(&self) -> usize {
        self.index
    }

    /// The entry the index currently points at, or `None` when the
    /// list is empty.
    pub fn current(&self) -> Option<&ErrorEntry> {
        self.flat.get(self.index)
    }

    /// Move the index by `delta`, wrapping vim-style (`:cnext` past
    /// the last entry wraps to the first; `:cprev` past the first
    /// wraps to the last), and return the entry now under the index.
    /// `None` only when the list is empty.
    pub fn step(&mut self, delta: i64) -> Option<&ErrorEntry> {
        let len = self.flat.len();
        if len == 0 {
            return None;
        }
        // `rem_euclid` keeps the result in `[0, len)` for any sign.
        let next = (self.index as i64 + delta).rem_euclid(len as i64);
        self.index = next as usize;
        self.flat.get(self.index)
    }

    /// Jump to the `n`th entry (1-based, vim `:cc N`). `n == None`
    /// (bare `:cc`) keeps the current index and returns the current
    /// entry. An out-of-range `n` (0, or past the end) leaves the
    /// index unchanged and returns `None`.
    pub fn jump_to(&mut self, n: Option<usize>) -> Option<&ErrorEntry> {
        match n {
            None => self.current(),
            Some(n) => {
                if n == 0 || n > self.flat.len() {
                    return None;
                }
                self.index = n - 1;
                self.flat.get(self.index)
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
            .flat
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
        if self.flat.is_empty() {
            return None;
        }
        // Group-start indices: index 0, plus every index whose path
        // differs from the previous entry's (contiguous same-path run =
        // one file group).
        let mut starts: Vec<usize> = vec![0];
        for i in 1..self.flat.len() {
            if self.flat[i].path != self.flat[i - 1].path {
                starts.push(i);
            }
        }
        // The group the index currently sits in = the last start <= index.
        let cur_group = starts.iter().rposition(|&s| s <= self.index).unwrap_or(0);
        let ngroups = starts.len() as i64;
        let next_group = (cur_group as i64 + delta).rem_euclid(ngroups) as usize;
        self.index = starts[next_group];
        self.flat.get(self.index)
    }

    /// Jump to the first entry (`:cfirst`). `None` when empty.
    pub fn first(&mut self) -> Option<&ErrorEntry> {
        if self.flat.is_empty() {
            return None;
        }
        self.index = 0;
        self.flat.first()
    }

    /// Jump to the last entry (`:clast`). `None` when empty.
    pub fn last(&mut self) -> Option<&ErrorEntry> {
        if self.flat.is_empty() {
            return None;
        }
        self.index = self.flat.len() - 1;
        self.flat.last()
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
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a", 1), entry("b", 2), entry("c", 3)],
        );
        // Walk forward, then re-set: index must reset.
        qf.step(2);
        assert_eq!(qf.index(), 2);
        qf.set(ErrorSource::Compilation, vec![entry("x", 9), entry("y", 8)]);
        assert_eq!(qf.index(), 0);
        assert_eq!(qf.current(), Some(&entry("x", 9)));
    }

    #[test]
    fn step_wraps_forward_past_end_to_first() {
        let mut qf = ErrorList::new();
        qf.set(ErrorSource::Compilation, vec![entry("a", 1), entry("b", 2)]);
        assert_eq!(qf.current(), Some(&entry("a", 1)));
        assert_eq!(qf.step(1), Some(&entry("b", 2)));
        // Past the end wraps to the first.
        assert_eq!(qf.step(1), Some(&entry("a", 1)));
        assert_eq!(qf.index(), 0);
    }

    #[test]
    fn step_wraps_backward_past_start_to_last() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a", 1), entry("b", 2), entry("c", 3)],
        );
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
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a", 1), entry("b", 2), entry("c", 3)],
        );
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
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a", 1), entry("b", 2), entry("c", 3)],
        );
        assert_eq!(qf.last(), Some(&entry("c", 3)));
        assert_eq!(qf.index(), 2);
        assert_eq!(qf.first(), Some(&entry("a", 1)));
        assert_eq!(qf.index(), 0);
    }

    #[test]
    fn set_index_to_matching_finds_by_path_and_line() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a.rs", 1), entry("b.rs", 5), entry("b.rs", 9)],
        );
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
        qf.set(
            ErrorSource::Compilation,
            vec![
                entry("a.rs", 1),
                entry("a.rs", 4),
                entry("b.rs", 2),
                entry("b.rs", 7),
            ],
        );
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
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a.rs", 1), entry("a.rs", 4), entry("b.rs", 2)],
        );
        // Sit on a.rs's SECOND entry, then :cnextfile → b.rs first.
        qf.step(1);
        assert_eq!(qf.index(), 1);
        assert_eq!(qf.step_file(1), Some(&entry("b.rs", 2)));
        assert_eq!(qf.index(), 2);
    }

    #[test]
    fn step_file_single_file_is_stable() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Compilation,
            vec![entry("only.rs", 1), entry("only.rs", 9)],
        );
        // One file group → next/prev file both resolve to its start.
        assert_eq!(qf.step_file(1), Some(&entry("only.rs", 1)));
        assert_eq!(qf.index(), 0);
        assert_eq!(qf.step_file(-1), Some(&entry("only.rs", 1)));
        assert_eq!(qf.index(), 0);
    }

    // ── EP.1: tagged sources, scoped replace ──────────────────────
    //
    // The reason the slice exists: two producers on one untagged list
    // clobber each other. The language server republishes on every
    // edit-debounce, so its feed would wipe a compile run's entries
    // while the user was walking them.

    #[test]
    fn a_write_replaces_only_its_own_source() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Compilation,
            vec![entry("c.rs", 1), entry("c.rs", 2)],
        );
        qf.set(ErrorSource::Lsp, vec![entry("l.rs", 9)]);
        assert_eq!(qf.len(), 3);

        // A second compile run replaces ONLY the compile slice.
        qf.set(ErrorSource::Compilation, vec![entry("c.rs", 7)]);
        assert_eq!(qf.entries_from(ErrorSource::Compilation).len(), 1);
        assert_eq!(
            qf.entries_from(ErrorSource::Lsp),
            &[entry("l.rs", 9)],
            "the LSP slice must survive a compile run — this is the clobber"
        );
        assert_eq!(qf.len(), 2);
    }

    #[test]
    fn slices_concatenate_in_presentation_order() {
        let mut qf = ErrorList::new();
        // Insert LSP first to prove order comes from PRESENTATION_ORDER,
        // not from insertion.
        qf.set(ErrorSource::Lsp, vec![entry("l.rs", 9)]);
        qf.set(ErrorSource::Compilation, vec![entry("c.rs", 1)]);
        assert_eq!(
            qf.entries(),
            &[entry("c.rs", 1), entry("l.rs", 9)],
            "compilation precedes lsp regardless of write order"
        );
        assert_eq!(
            qf.sources(),
            vec![ErrorSource::Compilation, ErrorSource::Lsp]
        );
    }

    /// Producer order within a slice is preserved — rustc emits the
    /// root cause ahead of the errors it cascades into, and sorting the
    /// merged list would destroy that.
    #[test]
    fn producer_order_within_a_slice_is_untouched() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Compilation,
            vec![entry("z.rs", 90), entry("a.rs", 1), entry("m.rs", 50)],
        );
        assert_eq!(
            qf.entries(),
            &[entry("z.rs", 90), entry("a.rs", 1), entry("m.rs", 50)],
            "entries must NOT be sorted by path or line"
        );
    }

    #[test]
    fn an_empty_write_clears_only_that_source() {
        let mut qf = ErrorList::new();
        qf.set(ErrorSource::Compilation, vec![entry("c.rs", 1)]);
        qf.set(ErrorSource::Lsp, vec![entry("l.rs", 9)]);

        // A clean build sends an empty vec.
        qf.set(ErrorSource::Compilation, vec![]);
        assert!(qf.entries_from(ErrorSource::Compilation).is_empty());
        assert_eq!(qf.entries(), &[entry("l.rs", 9)]);
        assert_eq!(qf.sources(), vec![ErrorSource::Lsp]);
        assert!(!qf.is_empty(), "the LSP slice still has entries");
    }

    /// `step_file`'s "maximal run of consecutive entries sharing a
    /// path" operates on the concatenation, so it still lands on
    /// first-of-file across a two-slice list.
    #[test]
    fn step_file_works_across_two_slices() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Compilation,
            vec![entry("c.rs", 1), entry("c.rs", 4)],
        );
        qf.set(ErrorSource::Lsp, vec![entry("l.rs", 2), entry("l.rs", 7)]);

        // Start on c.rs's first. Next file → l.rs's FIRST entry.
        assert_eq!(qf.step_file(1), Some(&entry("l.rs", 2)));
        assert_eq!(qf.index(), 2);
        // Wraps back to c.rs's first.
        assert_eq!(qf.step_file(1), Some(&entry("c.rs", 1)));
        assert_eq!(qf.index(), 0);
    }

    /// A file flagged by BOTH producers stays ONE file group when its
    /// entries land adjacent across the slice boundary — `step_file`
    /// groups by *maximal run of consecutive entries sharing a path*,
    /// and the concatenation puts them side by side. So the common case
    /// does NOT double-visit.
    #[test]
    fn the_same_file_from_two_sources_stays_one_group_when_adjacent() {
        let mut qf = ErrorList::new();
        qf.set(ErrorSource::Compilation, vec![entry("same.rs", 1)]);
        qf.set(ErrorSource::Lsp, vec![entry("same.rs", 5)]);
        assert_eq!(qf.len(), 2);
        // One group → `:cnextfile` wraps to its own start.
        assert_eq!(qf.step_file(1), Some(&entry("same.rs", 1)));
        assert_eq!(qf.index(), 0);
    }

    /// The double-visit the concatenation *can* produce, and its actual
    /// precondition: the shared path is NON-contiguous in the flat view,
    /// so it forms two separate groups. This is the real cost of
    /// concatenating rather than merge-sorting — narrower than "any file
    /// flagged by both producers", which is what the design first said.
    #[test]
    fn a_non_contiguous_path_forms_two_groups() {
        let mut qf = ErrorList::new();
        // `same.rs` is split by `other.rs` inside the compile slice.
        qf.set(
            ErrorSource::Compilation,
            vec![entry("same.rs", 1), entry("other.rs", 2)],
        );
        qf.set(ErrorSource::Lsp, vec![entry("same.rs", 5)]);

        // Groups: [same.rs] [other.rs] [same.rs] — three, not two.
        assert_eq!(qf.step_file(1), Some(&entry("other.rs", 2)));
        assert_eq!(qf.step_file(1), Some(&entry("same.rs", 5)));
        assert_eq!(qf.index(), 2);
        // And wraps back to the first group.
        assert_eq!(qf.step_file(1), Some(&entry("same.rs", 1)));
    }

    // ── EP.2: index re-anchoring across a refresh ─────────────────
    //
    // A live diagnostic feed republishes on every edit-debounce. If a
    // refresh reset the index, walking the list while typing would snap
    // the user back to entry 1 on every keystroke — the experience
    // `lsp.diagnostics-to-error-list = false` exists to escape.

    #[test]
    fn refresh_keeps_the_user_on_the_same_entry_when_one_is_inserted_above() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Lsp,
            vec![entry("a.rs", 10), entry("a.rs", 20), entry("a.rs", 30)],
        );
        qf.step(2);
        assert_eq!(qf.current(), Some(&entry("a.rs", 30)));

        // The server republishes with an extra entry ABOVE — every
        // ordinal shifts by one.
        qf.refresh(
            ErrorSource::Lsp,
            vec![
                entry("a.rs", 5),
                entry("a.rs", 10),
                entry("a.rs", 20),
                entry("a.rs", 30),
            ],
        );
        assert_eq!(
            qf.current(),
            Some(&entry("a.rs", 30)),
            "must follow the ENTRY, not the ordinal"
        );
        assert_eq!(qf.index(), 3);
    }

    /// Editing above an error shifts its line without making it a
    /// different error, so the identity match ignores `line`.
    #[test]
    fn refresh_tolerates_line_drift() {
        let mut qf = ErrorList::new();
        qf.set(ErrorSource::Lsp, vec![entry("a.rs", 10), entry("a.rs", 20)]);
        qf.step(1);
        assert_eq!(qf.current(), Some(&entry("a.rs", 20)));

        // Same two errors, both pushed down three lines.
        qf.refresh(ErrorSource::Lsp, vec![entry("a.rs", 13), entry("a.rs", 23)]);
        assert_eq!(qf.index(), 1, "still on the second error, now at line 23");
        assert_eq!(qf.current().map(|e| e.line), Some(23));
    }

    /// When the entry is fixed, land on the next surviving one in the
    /// same file rather than jumping to the top.
    #[test]
    fn refresh_falls_forward_within_the_file_when_the_entry_is_gone() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Lsp,
            vec![entry("a.rs", 10), entry("a.rs", 20), entry("a.rs", 30)],
        );
        qf.step(1);
        assert_eq!(qf.current(), Some(&entry("a.rs", 20)));

        // The user fixed the line-20 error.
        qf.refresh(ErrorSource::Lsp, vec![entry("a.rs", 10), entry("a.rs", 30)]);
        assert_eq!(
            qf.current(),
            Some(&entry("a.rs", 30)),
            "next surviving entry in the same file, not entry 1"
        );
    }

    #[test]
    fn refresh_resets_when_the_whole_file_is_clean() {
        let mut qf = ErrorList::new();
        qf.set(ErrorSource::Lsp, vec![entry("a.rs", 10), entry("b.rs", 1)]);
        qf.step(1);
        assert_eq!(qf.current(), Some(&entry("b.rs", 1)));

        qf.refresh(ErrorSource::Lsp, vec![entry("a.rs", 10)]);
        assert_eq!(qf.index(), 0, "nothing to anchor to — start over");
    }

    /// The producer declares the intent; a new run still resets even
    /// when an anchor was available.
    #[test]
    fn a_new_run_still_resets_the_index() {
        let mut qf = ErrorList::new();
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a.rs", 10), entry("a.rs", 20)],
        );
        qf.step(1);
        assert_eq!(qf.index(), 1);

        // Identical entries, but via `set` — a fresh compile.
        qf.set(
            ErrorSource::Compilation,
            vec![entry("a.rs", 10), entry("a.rs", 20)],
        );
        assert_eq!(
            qf.index(),
            0,
            "a new run starts at the top even though the entry survived"
        );
    }

    /// Re-anchoring reads the FLAT view, so an LSP refresh must not
    /// move the index off a compile entry the user is sitting on.
    #[test]
    fn refresh_of_one_source_keeps_the_index_on_another_sources_entry() {
        let mut qf = ErrorList::new();
        qf.set(ErrorSource::Compilation, vec![entry("c.rs", 1)]);
        qf.set(ErrorSource::Lsp, vec![entry("l.rs", 9)]);
        // Sit on the compile entry (index 0 after the LSP set).
        assert_eq!(qf.current(), Some(&entry("c.rs", 1)));

        qf.refresh(ErrorSource::Lsp, vec![entry("l.rs", 9), entry("l.rs", 12)]);
        assert_eq!(
            qf.current(),
            Some(&entry("c.rs", 1)),
            "an LSP republish must not drag the cursor off a compile entry"
        );
    }

    #[test]
    fn step_file_on_empty_is_none() {
        let mut qf = ErrorList::new();
        assert_eq!(qf.step_file(1), None);
        assert_eq!(qf.step_file(-1), None);
    }
}
