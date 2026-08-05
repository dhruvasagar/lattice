//! Fold ranges for magit-status inline expansions — the ENTRY level.
//!
//! An overlay [`FoldSource`] that reads live state on every
//! `compute_folds()` call rather than caching stale line numbers, so
//! it never desyncs from concurrent edits. It emits one fold per
//! expanded entry (file / stash / commit), spanning the entry's header
//! row through the end of its inserted patch — so folding "this entry"
//! hides the whole thing.
//!
//! **MG.45 moved the levels BELOW that out.** File and hunk folds now
//! come from [`crate::hunk_fold_source::MagitHunkFoldSource`], which
//! `magit-hunk-mode` registers on every buffer that renders a diff.
//! Two reasons, and the second is the one that mattered:
//!
//! 1. Only magit-status had a fold source, so the commit, diff,
//!    revision and stash-show buffers had no diff-aware folds at all.
//! 2. This source can only see ONE expansion's rows. A commit or
//!    stash entry expands to a MULTI-file patch (`git show`,
//!    `stash show -p`), which needs a file level between entry and
//!    hunk — and every file's hunks were landing as siblings directly
//!    under the entry instead.
//!
//! The two sources compose by range containment, the way the fold
//! engine expresses nesting everywhere else, so an expanded commit
//! folds as entry ▸ file ▸ hunk with neither source knowing about the
//! other.
//!
//! Registered by `MagitStatusMode::on_activate` via
//! `FoldOverlayServiceHandle`; `MagitStatusGuard::drop` removes it
//! (same Drop-based lifecycle `DiffModeGuard` and
//! `MultibufferModeGuard` use).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use lattice_core::{BufferId, Fold, FoldSource, ProviderId};

use crate::actions::{StatusBufferState, classify_line, entry_key};

/// Namespace for per-buffer magit-status fold-source ids, OR'd with
/// the buffer's id so simultaneous magit-status buffers (unusual,
/// but not disallowed) register distinct overlay ids.
pub const MAGIT_STATUS_FOLD_NAMESPACE: u64 = 0x6A61_0001_0000_0000;

pub struct MagitStatusFoldSource {
    id: ProviderId,
    state: Arc<Mutex<StatusBufferState>>,
}

impl MagitStatusFoldSource {
    pub fn new(state: Arc<Mutex<StatusBufferState>>, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(MAGIT_STATUS_FOLD_NAMESPACE | buffer_id.0 as u64),
            state,
        }
    }
}

/// MG.44: a fold's identity, which is what carries its **closed
/// state** across recomputes.
///
/// Keyed by the entry (`Staged:src/a.rs`), NOT by the line it happens
/// to sit on. `Fold::identity` exists precisely "so that adding or
/// removing lines elsewhere in the buffer doesn't reopen this fold" —
/// hashing the line number is the one thing guaranteed to defeat that,
/// because a refresh reorders sections and rewrites diffs above.
///
/// This mattered less when `=` deleted the diff outright: a collapsed
/// entry left `expanded` and had nothing to reopen. Now that `=`
/// folds, a fold that silently reopened on `gr` would look exactly
/// like the editor forgetting what the user just did.
///
/// `discriminator` separates the nested levels within one entry — the
/// entry fold itself from each of its hunks.
fn fold_identity(namespace: &str, key: &str, discriminator: usize) -> u64 {
    let mut h = DefaultHasher::new();
    namespace.hash(&mut h);
    key.hash(&mut h);
    discriminator.hash(&mut h);
    h.finish()
}

/// The stable key for a section fold.
///
/// The rendered header carries a count (`Unstaged changes (3)`) that
/// changes whenever the working tree does, so the KEY is the fixed
/// prefix, never the row's text. Identity is what carries closed-state
/// across a recompute — keying on the text would reopen a collapsed
/// section on exactly the `gr` that had something to report, which is
/// the bug `fold_identity` was written to prevent.
fn section_key(text: &str) -> Option<&'static str> {
    crate::sections::SECTION_HEADER_PREFIXES
        .iter()
        .copied()
        .find(|prefix| text.starts_with(prefix))
}

impl FoldSource for MagitStatusFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<Fold> {
        let Ok(g) = self.state.lock() else {
            return Vec::new();
        };
        let Some(handle) = g.store.handle_for(g.buffer_id) else {
            return Vec::new();
        };
        let snap = handle.snapshot();
        let total = snap.buffer.line_count();
        let mut folds = Vec::new();

        // MG.48: the SECTION level.
        //
        // magit-status's sections are the buffer's top-level structure,
        // and only magit-status knows what a section is — the same
        // reason this source owns the entry level.
        //
        // Declared here because MG.46 pinned `foldmethod=manual` on
        // every magit buffer that renders a diff, so no primary
        // provider fabricates code-level folds inside hunk text.
        // Section collapsing used to fall out of the `indent` primary
        // as a side effect of the rows being indented under their
        // header — something magit never declared, and which therefore
        // vanished the moment the primary was turned off. A structure
        // this mode depends on should not have been borrowed from a
        // generic provider's incidental behaviour.
        //
        // Computed regardless of `expanded`: a section is foldable
        // whether or not any entry inside it has been opened.
        let starts: Vec<(u32, &'static str)> = (0..total)
            .filter_map(|line| {
                let text = snap.buffer.line(line)?;
                section_key(text.trim()).map(|key| (line, key))
            })
            .collect();
        for (n, &(start, key)) in starts.iter().enumerate() {
            let limit = starts.get(n + 1).map(|&(l, _)| l).unwrap_or(total);
            // Trim trailing blanks so a collapsed section does not
            // swallow the blank row that delimits it from the next.
            let mut end = limit.saturating_sub(1);
            while end > start
                && snap
                    .buffer
                    .line(end)
                    .map(|l| l.trim().is_empty())
                    .unwrap_or(false)
            {
                end -= 1;
            }
            if end > start {
                folds.push(Fold {
                    start_line: start,
                    end_line: end,
                    closed: false,
                    identity: Some(fold_identity("magit:section", key, 0)),
                });
            }
        }

        if g.expanded.is_empty() {
            return folds;
        }
        let mut line = 0u32;
        while line < total {
            let Some(text) = snap.buffer.line(line) else {
                break;
            };
            if !text.starts_with("  ") {
                line += 1;
                continue;
            }
            // Every classified entry is exactly one line; if it's a
            // currently-expanded one, its patch occupies the next
            // `count` lines (an exact count from insertion time, not
            // a re-scanned guess) — fold that span, then jump past it
            // so the scan never has to disambiguate patch content
            // from real entries.
            if let Some(sl) = classify_line(&g, line) {
                let key = entry_key(&sl);
                if let Some(&count) = g.expanded.get(&key)
                    && count > 0
                {
                    let count = count as u32;
                    let body_end = (line + count).min(total.saturating_sub(1));
                    folds.push(Fold {
                        start_line: line,
                        end_line: body_end,
                        closed: false,
                        identity: Some(fold_identity("magit:entry", &key, 0)),
                    });
                    // MG.45: the hunk level moved to
                    // `MagitHunkFoldSource`, which magit-hunk-mode
                    // registers on every buffer that renders a
                    // diff. Emitting it here too would put two
                    // folds over the same rows from two providers
                    // — and it could only ever describe a
                    // SINGLE-file expansion, because a multi-file
                    // one (`git show`, `stash show -p`) needs a
                    // file level between entry and hunk that this
                    // source has no way to see.
                    line += 1 + count;
                    continue;
                }
            }
            line += 1;
        }
        folds
    }
}

#[cfg(test)]
mod section_key_tests {
    use super::section_key;

    /// MG.48: **a section's key must not carry its count.**
    ///
    /// The header renders as `Unstaged changes (3)`, and the count
    /// changes whenever the working tree does. Keying identity on the
    /// row's text would reopen a collapsed section on exactly the `gr`
    /// that had something to report — the same failure `fold_identity`
    /// exists to prevent, one level up.
    #[test]
    fn a_sections_key_ignores_its_changing_count() {
        assert_eq!(
            section_key("Unstaged changes (3)"),
            section_key("Unstaged changes (11)"),
            "the same section before and after a change must key alike",
        );
        assert_eq!(
            section_key("Unstaged changes (3)"),
            Some("Unstaged changes")
        );
    }

    /// Every rendered section is foldable, and nothing else is.
    #[test]
    fn every_section_header_keys_and_ordinary_rows_do_not() {
        for prefix in crate::sections::SECTION_HEADER_PREFIXES {
            assert_eq!(
                section_key(&format!("{prefix} (2)")),
                Some(prefix),
                "`{prefix}` is a section header and must fold",
            );
        }
        assert_eq!(section_key("  modified   src/a.rs"), None);
        assert_eq!(section_key("diff --git a/x b/x"), None);
        assert_eq!(section_key(""), None);
    }

    /// Distinct sections must not collide, or collapsing one would
    /// collapse another.
    #[test]
    fn distinct_sections_have_distinct_identities() {
        use super::fold_identity;
        let id = |k: &str| fold_identity("magit:section", k, 0);
        assert_ne!(id("Staged changes"), id("Unstaged changes"));
        assert_ne!(id("Recent commits"), id("Stashes"));
        // ...and a section must not collide with an entry that happens
        // to share its key, since the two nest.
        assert_ne!(
            fold_identity("magit:section", "Staged changes", 0),
            fold_identity("magit:entry", "Staged changes", 0),
        );
    }
}

#[cfg(test)]
mod fold_identity_tests {
    use super::fold_identity;

    /// MG.44: **a fold's closed state must survive `gr`.**
    ///
    /// `gr` re-runs every open entry's `git diff` and rebuilds the
    /// buffer, so an entry moves whenever anything above it changes —
    /// a file staged into another section, a diff that got shorter.
    /// Identity is what carries closed-state across that recompute, so
    /// keying it on the LINE meant a folded diff silently reopened on
    /// the refresh that had something to report.
    ///
    /// This matters now in a way it did not before: `=` used to delete
    /// the diff, so a collapsed entry left `expanded` and had nothing
    /// to reopen.
    #[test]
    fn an_entrys_identity_does_not_move_with_its_line() {
        let key = "Staged:src/a.rs";
        // Same entry, different position after a refresh.
        assert_eq!(
            fold_identity("magit:entry", key, 0),
            fold_identity("magit:entry", key, 0),
        );
    }

    /// Different entries never collide, or folding one would fold
    /// another.
    #[test]
    fn distinct_entries_have_distinct_identities() {
        assert_ne!(
            fold_identity("magit:entry", "Staged:src/a.rs", 0),
            fold_identity("magit:entry", "Staged:src/b.rs", 0),
        );
        // The same path staged and unstaged are two rows in the
        // buffer, and folding one must not fold the other.
        assert_ne!(
            fold_identity("magit:entry", "Staged:src/a.rs", 0),
            fold_identity("magit:entry", "Unstaged:src/a.rs", 0),
        );
    }

    /// MG.45: an entry fold must not collide with the file/hunk folds
    /// that now come from `MagitHunkFoldSource`.
    ///
    /// They are nested and come from two DIFFERENT providers, so a
    /// shared identity would make the outer one inherit an inner
    /// one's closed state across a recompute.
    #[test]
    fn an_entry_fold_does_not_share_an_identity_with_the_diff_folds() {
        let key = "Staged:src/a.rs";
        let entry = fold_identity("magit:entry", key, 0);
        assert_ne!(
            entry,
            crate::hunk_fold_source::identity_for_test("magit:diff-file", key, 0)
        );
        assert_ne!(
            entry,
            crate::hunk_fold_source::identity_for_test("magit:diff-hunk", key, 0)
        );
    }
}
