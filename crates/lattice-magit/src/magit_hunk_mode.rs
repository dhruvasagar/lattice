//! MG.24a: `magit-hunk-mode` — the minor that owns diff *content*.
//!
//! Design fragment:
//! `docs/dev/architecture/magit-hunk-mode.md`.
//!
//! Five majors render unified diff, and each declared its own chords
//! for acting on it. The set had drifted: magit-status had `s`/`u`/`x`,
//! magit-diff had `s`/`u` and no `x`, and magit-commit,
//! magit-revision and magit-stash-show had none at all — eight
//! declarations covering three actions, eleven of fifteen cells empty.
//! Nobody noticed the missing `x` because there was no single place it
//! should have been, which is the failure mode a copied set has: **a
//! gap in it does not announce itself.**
//!
//! So the chords live here, on the mode that says what the buffer's
//! *content* is, while the major keeps saying what the buffer *is*.
//!
//! **The machinery did not move.** `resolve_hunk`, `HunkOp`, the
//! `DiffSource` gate and MG.18e's region rewrite stay in
//! `magit_core_mode` where MG.18 put them; this mode contributes the
//! bindings and the handlers that call them. Only the bindings were in
//! the wrong place.
//!
//! **`<CR>` moved here once the seam existed.** The chord and the
//! diff-path parsing belong to the mode; *which version of the file to
//! open* belongs to the view, because it genuinely differs — the index
//! blob for a staged diff, the live file for an unstaged one, the file
//! at a sha for a revision, the stash's copy for a stash. That is
//! `MagitView::diff_target`.
//!
//! Magit-status's `<CR>` is context-aware over rows that are not diffs
//! at all (a file entry, a stash, a commit), and a minor's binding
//! wins over a major's — so that behaviour is reached through
//! `MagitView::visit_at_cursor` rather than being replaced by a
//! diff-only handler.

use std::sync::OnceLock;

use lattice_mode::{
    ActivationPolicy, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};

use crate::magit_commit_mode::MagitCommitMode;
use crate::magit_diff_mode::MagitDiffMode;
use crate::magit_revision_mode::MagitRevisionMode;
use crate::magit_stash_show_mode::MagitStashShowMode;
use crate::magit_status_mode::MagitStatusMode;

pub struct MagitHunkMode;

impl MagitHunkMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-hunk-mode")
    }
}

fn magit_hunk_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            // Normal and Visual for each: MG.18e's region staging acts
            // on the lines a selection covers, and it is reached by the
            // same key. A Normal-only binding would leave the region
            // path bound in some majors and not others — which is the
            // drift this mode exists to end.
            keymap_entry! { mode: Normal, chord: "s", doc: "Stage hunk or file at cursor", cmd: "action:magit-stage" },
            keymap_entry! { mode: Visual, chord: "s", doc: "Stage the selected lines", cmd: "action:magit-stage" },
            keymap_entry! { mode: Normal, chord: "u", doc: "Unstage hunk or file at cursor", cmd: "action:magit-unstage" },
            keymap_entry! { mode: Visual, chord: "u", doc: "Unstage the selected lines", cmd: "action:magit-unstage" },
            keymap_entry! { mode: Normal, chord: "x", doc: "Discard hunk or file at cursor", cmd: "action:magit-discard" },
            keymap_entry! { mode: Visual, chord: "x", doc: "Discard the selected lines", cmd: "action:magit-discard" },
            // MG.23g's committed-hunk pair. They were on
            // `magit-core-mode`, which activates on all eleven magit
            // majors — so they were consumed dead keys in the six with
            // no diff content in them.
            keymap_entry! { mode: Normal, chord: "a", doc: "Apply the hunk at cursor to the working tree", cmd: "action:magit-apply-hunk" },
            keymap_entry! { mode: Normal, chord: "-", doc: "Reverse the hunk at cursor out of the working tree", cmd: "action:magit-reverse-hunk" },
            // Hunk navigation, for the same reason: `]c` in a branch
            // list resolved an empty header set and returned `None`,
            // and a Normal-mode chord a mode binds is consumed
            // unconditionally.
            keymap_entry! { mode: Normal, chord: "]c", doc: "Next hunk", cmd: "action:magit-next-hunk" },
            keymap_entry! { mode: Normal, chord: "[c", doc: "Previous hunk", cmd: "action:magit-prev-hunk" },
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Visit the file at cursor", cmd: "action:magit-visit-diff-target" },
        ]
    })
}

/// MG.22: the one `<CR>` handler.
///
/// **The view is asked first, and that order is a correctness
/// requirement rather than a preference.**
///
/// The obvious order — resolve the diff path, then ask the view which
/// version — is wrong in magit-status, where an expanded inline diff
/// is rendered *below* the file entry it belongs to:
///
/// ```text
///   modified a.txt
///     diff --git a/a.txt b/a.txt     ← a.txt's expansion
///     @@ …
///   modified b.txt                   ← cursor here
/// ```
///
/// `path_at_cursor` scans **upward** for the nearest `diff --git`, so
/// on `modified b.txt` it would find *a.txt's* header and `<CR>` would
/// open the wrong file — silently, and only in the case where some
/// earlier entry happens to be expanded.
///
/// Asking the view first removes that: magit-status classifies the row
/// (file entry, stash, commit) and answers, and only rows it does not
/// recognise — which is exactly the diff content — fall through to
/// path resolution. Views whose buffer is entirely diff decline the
/// first question and take the second.
fn visit_diff_target(ctx: &lattice_mode::ActionContext<'_>) -> Option<lattice_grammar::Effect> {
    let view = crate::buffer_state::view_for(ctx)?;
    if let Some(effect) = view.visit_at_cursor(ctx.cursor) {
        return Some(effect);
    }
    let store = ctx.services.get::<lattice_mode::BufferStoreHandle>()?;
    let handle = store.handle_for(lattice_core::BufferId(ctx.buffer_id.0 as u32))?;
    let snap = handle.snapshot();
    let path = crate::hunk::path_at_cursor(
        |i| u32::try_from(i).ok().and_then(|l| snap.buffer.line(l)),
        ctx.cursor.line as usize,
    )?;
    view.diff_target(&path)
}

impl Mode for MagitHunkMode {
    /// Nothing per activation. Every action this mode binds is
    /// registered once at boot by the mode that owns its body
    /// (`magit-core-mode` for the shared hunk machinery,
    /// `magit-status-mode`'s `actions.rs` for discard's ask/execute
    /// pair) — this mode contributes bindings, not handlers, so there
    /// is nothing to unwind.
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// The five majors that render unified diff — and only those.
    ///
    /// Deliberately NOT the list `magit-core-mode` carries. A branch
    /// list, a log, a stash list, a rebase todo, a blame and a blob
    /// have no hunks, so binding `s`/`u`/`x`/`]c` there would consume
    /// the keys to do nothing. That is the state `]c` and `a`/`-` were
    /// already in before this mode existed.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Majors(vec![
            MagitStatusMode::mode_id(),
            MagitDiffMode::mode_id(),
            MagitCommitMode::mode_id(),
            MagitRevisionMode::mode_id(),
            MagitStashShowMode::mode_id(),
        ])
    }

    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::new()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_hunk_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<lattice_mode::ActionHandlerContribution> {
        vec![lattice_mode::ActionHandlerContribution {
            action_name: "action:magit-visit-diff-target",
            handler: std::sync::Arc::new(visit_diff_target),
        }]
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The five majors that show diff content, and no others.
    ///
    /// Both halves matter. Missing one leaves that buffer without the
    /// staging chords — the state magit-commit, magit-revision and
    /// magit-stash-show were in. Adding one that shows no diff puts the
    /// keys back where they are consumed to do nothing, which is what
    /// `]c` and `a`/`-` did on `magit-core-mode`.
    #[test]
    fn activates_on_exactly_the_diff_showing_majors() {
        let ActivationPolicy::Majors(majors) = MagitHunkMode.activation_policy() else {
            panic!("magit-hunk-mode activates by major");
        };
        let ids: Vec<String> = majors.iter().map(|m| m.as_str().to_string()).collect();
        assert_eq!(
            ids,
            [
                "magit-status-mode",
                "magit-diff-mode",
                "magit-commit-mode",
                "magit-revision-mode",
                "magit-stash-show-mode",
            ]
        );
        for absent in [
            "magit-log-mode",
            "magit-branch-mode",
            "magit-stash-mode",
            "magit-rebase-mode",
            "magit-blame-mode",
            "magit-file-revision-mode",
        ] {
            assert!(
                !ids.iter().any(|i| i == absent),
                "`{absent}` renders no diff — binding hunk chords there \
                 consumes them to do nothing"
            );
        }
    }

    /// Every chord that acts on a hunk, in both the modes that can
    /// reach it. A Normal-only binding would leave MG.18e's region
    /// staging unreachable by its own documented gesture.
    #[test]
    fn the_staging_chords_are_bound_in_normal_and_visual() {
        let entries = magit_hunk_keymap_entries();
        for chord in ["s", "u", "x"] {
            for mode in ["Normal", "Visual"] {
                assert!(
                    entries
                        .iter()
                        .any(|e| e.chord == chord && format!("{:?}", e.modes).contains(mode)),
                    "`{chord}` must be bound in {mode}"
                );
            }
        }
    }

    /// `<CR>` is here now that `diff_target` exists — and the handler
    /// must ask the **view first**.
    ///
    /// A minor's binding wins over a major's, so without the fallback
    /// magit-status's context-aware visit (file entry / stash /
    /// commit rows) is silently replaced by a diff-only handler. Worse,
    /// resolving the diff path first would scan upward past a
    /// *previous* entry's expanded inline diff and open the wrong
    /// file — see this module's header for the layout that makes that
    /// happen.
    #[test]
    fn cr_is_bound_and_asks_the_view_before_the_diff_text() {
        assert!(
            magit_hunk_keymap_entries()
                .iter()
                .any(|e| e.chord == "<CR>"),
            "`<CR>` belongs to the mode that owns diff content"
        );
        let src = include_str!("magit_hunk_mode.rs");
        let view_first = src.find("view.visit_at_cursor(ctx.cursor)");
        let path_after = src.find("path_at_cursor(");
        assert!(
            matches!((view_first, path_after), (Some(v), Some(p)) if v < p),
            "the handler must consult `visit_at_cursor` BEFORE resolving \
             a diff path — the other order opens the wrong file in \
             magit-status whenever an earlier entry is expanded"
        );
    }
}
