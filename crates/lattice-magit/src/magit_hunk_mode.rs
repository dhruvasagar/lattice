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

use lattice_core::FoldOverlayServiceHandle;
use lattice_mode::{
    ActivationPolicy, BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode,
    ModeContext, ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};

use crate::hunk_fold_source::MagitHunkFoldSource;

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
            // `]f` / `[f` moved here from `magit-core-mode`, where they
            // were bound on all ten majors and meant something in one.
            // In a branch / stash / remote / log list they jumped
            // between *rows* while claiming to move between files (a
            // job `j` and `]]` already do); in the diff-content views
            // they matched indented CONTEXT lines, so they walked
            // through arbitrary code; in the rebase todo, whose rows
            // sit at column 0, they matched nothing at all.
            //
            // This mode's five majors are exactly the file-bearing
            // ones. Which rows count as "a file" still differs between
            // them — entries in magit-status, `diff --git` headers in a
            // pure diff — and that is what `MagitView::file_lines`
            // answers. Same shape MG.24a gave `]c` / `[c`.
            keymap_entry! { mode: Normal, chord: "]f", doc: "Next file", cmd: "action:magit-next-file" },
            keymap_entry! { mode: Normal, chord: "[f", doc: "Previous file", cmd: "action:magit-prev-file" },
            // MG.19: vim-fugitive's key for exactly this, and it lands
            // in the `d`-prefixed family `diff-mode` already owns
            // (`do` / `dp` / `d2o`). `dv` is not an operator+motion —
            // `v` forces characterwise on a `d` that never completes —
            // so it is inert in a read-only magit buffer.
            keymap_entry! { mode: Normal, chord: "dv", doc: "Open the file at cursor side-by-side against its baseline", cmd: "action:magit-diff-side-by-side" },
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
    let read = |i: usize| u32::try_from(i).ok().and_then(|l| snap.buffer.line(l));
    let path = crate::hunk::path_at_cursor(read, ctx.cursor.line as usize)?;
    let target = view.diff_target(&path, ctx.cursor)?;

    // MG.50: land on the code under the cursor — the right line AND the
    // right offset within it, so `<CR>` on a hunk row puts the caret on
    // the same token it was on in the diff rather than at line start.
    //
    // `None` here is the ordinary case, not a failure — a file entry row
    // is not inside a hunk, and emacs opens those at the top too. The
    // target opens unpositioned.
    match crate::hunk::source_position_at(read, ctx.cursor.line as usize, ctx.cursor.byte) {
        Some(pos) => Some(at_position(target, pos)),
        None => Some(target),
    }
}

/// MG.50: re-express an "open this" effect as "open this AT `pos`".
///
/// Positioning has to be part of the SAME effect rather than a
/// following `CursorMove`: the opens are peer-applied (the TUI/GPUI
/// `do_edit` path) while a cursor effect runs host-side against
/// whatever buffer is active at that moment, so the two cannot be
/// ordered to land the caret on a buffer that does not exist yet. This
/// is the reason `Effect::OpenBufferAt` exists at all — see its doc,
/// which records the same bug for search `<CR>`.
///
/// Effects with nothing to position (an echo, a refusal) pass through.
fn at_position(
    effect: lattice_grammar::Effect,
    pos: crate::hunk::SourcePos,
) -> lattice_grammar::Effect {
    use lattice_grammar::Effect;
    let position = lattice_protocol::position::Position::new(pos.line, pos.byte);
    match effect {
        Effect::OpenBuffer { path, force } => Effect::OpenBufferAt {
            path,
            position,
            force,
        },
        Effect::OpenSyntheticBuffer { name, mode_id } => Effect::OpenSyntheticBufferAt {
            name,
            mode_id,
            position,
        },
        other => other,
    }
}

/// MG.19: `dv` — the file at cursor, side by side against its baseline.
///
/// **This composes what already exists rather than building a second
/// diff.** `lattice-diff` owns two-pane sessions: scroll binding,
/// filler rows, `]c` / `[c`, and `do` / `dp` are all consequences of a
/// registered `PaneGroup`, not of anything magit does. So the whole
/// slice is two effects in order:
///
/// 1. open the baseline — the file as it exists at the version this
///    diff was taken against — in the CURRENT pane;
/// 2. `Effect::Diffsplit` the working-tree file into a new vsplit,
///    which registers the session between the two.
///
/// The baseline goes first because `Diffsplit` diffs the new pane
/// against whatever pane is active. Getting that order backwards would
/// put the editable side on the left and silently invert what `do` and
/// `dp` mean.
///
/// The baseline is `*magit:file:<ref>:<path>*`
/// ([`crate::magit_file_revision_mode`]) — a synthetic buffer, which
/// works here only because synthetic magit buffers really are
/// `BufferKind::Document`. `do_diffsplit` refuses a non-Document
/// active pane, so "everything is a buffer" is load-bearing rather
/// than decorative in this path.
fn diff_side_by_side(ctx: &lattice_mode::ActionContext<'_>) -> Option<lattice_grammar::Effect> {
    use lattice_grammar::Effect;

    let view = crate::buffer_state::view_for(ctx)?;
    let store = ctx.services.get::<lattice_mode::BufferStoreHandle>()?;
    let handle = store.handle_for(lattice_core::BufferId(ctx.buffer_id.0 as u32))?;
    let snap = handle.snapshot();
    let path = crate::hunk::path_at_cursor(
        |i| u32::try_from(i).ok().and_then(|l| snap.buffer.line(l)),
        ctx.cursor.line as usize,
    )?;

    // Which version is "the other side" depends on what this buffer's
    // diff was taken against — the same question `s` / `u` / `x` ask,
    // answered by the same seam.
    //
    // Note this succeeds in a case where `s` / `u` / `x` deliberately
    // refuse: `diff_source` yields `None` for the unscoped
    // `*magit:diff*`, because a diff against HEAD mixes staged and
    // unstaged changes and there is no single tree to apply a hunk to.
    // *Showing* two versions has no such ambiguity — the question is
    // "which version", not "which tree do I write to" — so `None`
    // resolves to HEAD rather than declining.
    let git_ref = baseline_ref(view.diff_source(ctx.cursor), || {
        view.commit_at_cursor(ctx.cursor)
    })?;

    let workdir = view.workdir()?;
    let absolute = workdir.join(&path);
    // A file the commit deleted, or one not yet written, has no
    // working-tree side to put in the right-hand pane. Saying so beats
    // opening an empty split that reads as a broken diff.
    if !absolute.exists() {
        return Some(Effect::Echo {
            level: lattice_grammar::EchoLevel::Warn,
            text: format!(
                "{} has no working-tree copy to diff against",
                path.display()
            ),
        });
    }

    Some(side_by_side_effects(
        &crate::repo_scope::label_of_buffer(&store, lattice_core::BufferId(ctx.buffer_id.0 as u32)),
        &git_ref,
        &path,
        absolute,
    ))
}

/// Which version is the left-hand side.
///
/// Pure, and separate from the handler, because this is the part with
/// a decision in it — the handler around it is buffer plumbing.
pub(crate) fn baseline_ref(
    source: Option<crate::buffer_state::DiffSource>,
    commit_at_cursor: impl FnOnce() -> Option<String>,
) -> Option<String> {
    use crate::buffer_state::DiffSource;
    match source {
        // Both index-relative: `--cached` is HEAD↔index and a plain
        // diff is index↔worktree, so the index blob is the meaningful
        // other side in each.
        Some(DiffSource::Staged) | Some(DiffSource::Unstaged) => Some("staged".to_string()),
        // A commit's or a stash's patch describes a specific version,
        // so that is the baseline — not the index, which has nothing
        // to do with it.
        Some(DiffSource::Committed) => commit_at_cursor(),
        // `None` is where this deliberately differs from `s` / `u` /
        // `x`, which refuse here: the unscoped `*magit:diff*` is
        // against HEAD and mixes staged with unstaged, so there is no
        // single tree to apply a hunk TO. Showing two versions has no
        // such ambiguity — the question is "which version", not "which
        // tree do I write to" — so HEAD is the answer, not a refusal.
        None => Some("HEAD".to_string()),
    }
}

/// The two effects, in the order that matters.
///
/// `Diffsplit` diffs its new pane against whatever pane is ACTIVE, so
/// the baseline must be opened first. Reversed, the editable
/// working-tree copy would end up on the left and `do` / `dp` would
/// silently mean the opposite of what the user intends.
pub(crate) fn side_by_side_effects(
    repo: &str,
    git_ref: &str,
    path: &std::path::Path,
    absolute: std::path::PathBuf,
) -> lattice_grammar::Effect {
    use lattice_grammar::Effect;
    Effect::Many(vec![
        Effect::OpenSyntheticBuffer {
            name: crate::magit_file_revision_mode::blob_buffer_name(repo, git_ref, path),
            mode_id: crate::magit_file_revision_mode::MagitFileRevisionMode::mode_id().to_string(),
        },
        Effect::Diffsplit {
            path: absolute,
            remote: None,
        },
    ])
}

/// MG.45: deregisters this buffer's diff-fold source.
///
/// Drop-based, the same lifecycle `MagitStatusGuard` and
/// `DiffModeGuard` use — a source left registered on a buffer whose
/// mode has gone would keep computing folds over text it no longer
/// describes.
#[derive(Default)]
pub struct MagitHunkGuard {
    fold_registration: Option<(FoldOverlayServiceHandle, lattice_core::ProviderId)>,
}

impl Drop for MagitHunkGuard {
    fn drop(&mut self) {
        if let Some((svc, id)) = self.fold_registration.take() {
            svc.remove_source(id);
        }
    }
}

impl Mode for MagitHunkMode {
    /// MG.45: carries the diff-fold registration. Every ACTION this
    /// mode binds is still registered once at boot by the mode that
    /// owns its body (`magit-core-mode` for the shared hunk machinery,
    /// `magit-status-mode`'s `actions.rs` for discard's ask/execute
    /// pair) — this mode contributes bindings, not handlers.
    type Guard = MagitHunkGuard;

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

    /// MG.46: **the diff text folds by hunk, never by code structure.**
    ///
    /// This mode owns what is inside the diff, so it owns which folds
    /// may exist there. `foldmethod=manual` leaves `ManualPrimary` —
    /// which produces nothing — as the primary, so the only folds are
    /// this mode's own file ▸ hunk overlays plus magit-status's entry
    /// overlay.
    ///
    /// Without it, a user whose global `foldmethod` is `indent` or
    /// `syntax` gets the primary provider run over the diff *as if it
    /// were source*. It is not: a hunk is a fragment with `+`/`-`/` `
    /// prefixes on every row, so the folds it derives are structurally
    /// meaningless — and the last one, opened by an indent that the
    /// fragment never closes, runs to the end of the buffer and
    /// swallows the rest of the magit-status document.
    ///
    /// Scoped to the mode rather than the buffer kind: the override
    /// reverts when the mode deactivates, and it reaches exactly the
    /// five diff-rendering majors this mode activates on.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::FoldMethodOption = lattice_core::FoldMethod::Manual,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_hunk_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<lattice_mode::ActionHandlerContribution> {
        vec![
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-visit-diff-target",
                handler: std::sync::Arc::new(visit_diff_target),
            },
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-diff-side-by-side",
                handler: std::sync::Arc::new(diff_side_by_side),
            },
        ]
    }

    /// MG.45: register the file ▸ hunk fold source.
    ///
    /// Registered HERE rather than per major because this mode already
    /// activates on exactly the buffers that render a diff — which is
    /// what makes one source serve five majors instead of five
    /// near-copies. magit-status keeps its own source for the ENTRY
    /// level; the two compose by range containment.
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(MagitHunkGuard::default());
            };
            let fold_registration = ctx
                .service::<FoldOverlayServiceHandle>()
                .map(|outer| (*outer).clone())
                .map(|svc| {
                    let source =
                        std::sync::Arc::new(MagitHunkFoldSource::new(store.clone(), buffer_id));
                    let id = svc.add_source(source, buffer_id);
                    (svc, id)
                });
            Ok(MagitHunkGuard { fold_registration })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.46: **diff text never folds by code structure.**
    ///
    /// The mode that owns what is inside a hunk owns which folds may
    /// exist there. Pinned as an option override rather than left to
    /// the user's global `foldmethod`: with `indent` or `syntax` set
    /// globally, the primary provider runs over the diff as if it were
    /// source, and the last fold — opened by an indent the fragment
    /// never closes — swallows the rest of the magit-status buffer.
    #[test]
    fn diff_buffers_fold_only_by_hunk_not_by_code_structure() {
        let opts = MagitHunkMode.options();
        let ov = opts
            .iter()
            .find(|o| {
                o.option_type_id == std::any::TypeId::of::<lattice_config::FoldMethodOption>()
            })
            .expect("magit-hunk-mode must pin `foldmethod`");
        assert_eq!(
            ov.downcast_value::<lattice_core::FoldMethod>().copied(),
            Some(lattice_core::FoldMethod::Manual),
            "`foldmethod` must be `manual` so only this mode's own \
             file/hunk overlays produce folds",
        );
    }

    /// The override must reach every major that renders a diff — the
    /// same five this mode binds its hunk chords on. A major that
    /// rendered diff text without it would fold that text as code.
    #[test]
    fn the_fold_override_covers_every_diff_rendering_major() {
        let ActivationPolicy::Majors(majors) = MagitHunkMode.activation_policy() else {
            panic!("magit-hunk-mode scopes itself to specific majors");
        };
        for expected in [
            MagitStatusMode::mode_id(),
            MagitDiffMode::mode_id(),
            MagitCommitMode::mode_id(),
            MagitRevisionMode::mode_id(),
            MagitStashShowMode::mode_id(),
        ] {
            assert!(
                majors.contains(&expected),
                "{expected:?} renders diff text, so it must inherit the \
                 `foldmethod=manual` override",
            );
        }
    }

    /// MG.19: the baseline is the version the diff was taken against,
    /// per source.
    #[test]
    fn the_baseline_is_the_version_this_diff_describes() {
        use crate::buffer_state::DiffSource;
        let no_commit = || None;
        assert_eq!(
            baseline_ref(Some(DiffSource::Staged), no_commit).as_deref(),
            Some("staged")
        );
        assert_eq!(
            baseline_ref(Some(DiffSource::Unstaged), no_commit).as_deref(),
            Some("staged"),
            "index↔worktree: the index is still the other side"
        );
        assert_eq!(
            baseline_ref(Some(DiffSource::Committed), || Some("a1b2c3d".into())).as_deref(),
            Some("a1b2c3d"),
            "a commit's patch describes that commit, not the index"
        );
    }

    /// Where `s` / `u` / `x` refuse, `dv` answers — and that asymmetry
    /// is deliberate rather than an oversight.
    #[test]
    fn an_unclassifiable_diff_still_has_a_baseline() {
        assert_eq!(
            baseline_ref(None, || None).as_deref(),
            Some("HEAD"),
            "the unscoped `*magit:diff*` is against HEAD; showing two \
             versions needs no tree to write to"
        );
    }

    /// A committed diff with no commit under the cursor (a `--graph`
    /// connector, a stat header) declines rather than guessing.
    #[test]
    fn a_committed_diff_with_no_commit_at_cursor_declines() {
        use crate::buffer_state::DiffSource;
        assert!(baseline_ref(Some(DiffSource::Committed), || None).is_none());
    }

    /// The order is the correctness requirement: `Diffsplit` diffs
    /// against the ACTIVE pane, so the baseline has to be opened
    /// first. Reversed, `do` and `dp` would mean the opposite.
    #[test]
    fn the_baseline_pane_is_opened_before_the_split() {
        use lattice_grammar::Effect;
        let effect = side_by_side_effects(
            "lattice",
            "staged",
            std::path::Path::new("src/main.rs"),
            std::path::PathBuf::from("/repo/src/main.rs"),
        );
        let Effect::Many(effects) = effect else {
            panic!("expected a two-effect sequence, got {effect:?}");
        };
        assert_eq!(effects.len(), 2);
        match &effects[0] {
            Effect::OpenSyntheticBuffer { name, mode_id } => {
                assert_eq!(name, "*magit:file:lattice:staged:src/main.rs*");
                assert_eq!(mode_id, "magit-file-revision-mode");
            }
            other => panic!("the baseline must be opened first, got {other:?}"),
        }
        match &effects[1] {
            Effect::Diffsplit { path, remote } => {
                assert_eq!(path, std::path::Path::new("/repo/src/main.rs"));
                assert!(remote.is_none(), "two-way, not a three-way merge");
            }
            other => panic!("the split must come second, got {other:?}"),
        }
    }

    /// `dv` is bound, and in the `d`-prefixed family `diff-mode`
    /// already owns — so it cannot collide with `do` / `dp`, which the
    /// same buffer gets once the session is live.
    ///
    /// MG.49b: this survived the root-menu work. The first cut bound `d`
    /// on `magit-core-mode`, which would have made `dv` unreachable (the
    /// trie checks a node's own binding before its children); that cut
    /// was reverted in favour of one `h` for the dispatch, so `d` is a
    /// free prefix again.
    #[test]
    fn dv_is_bound_and_does_not_shadow_the_diff_mode_chords() {
        let chords: Vec<&str> = magit_hunk_keymap_entries()
            .iter()
            .map(|e| e.chord)
            .collect();
        assert!(chords.contains(&"dv"), "{chords:?}");
        for owned_by_diff_mode in ["do", "dp"] {
            assert!(
                !chords.contains(&owned_by_diff_mode),
                "`{owned_by_diff_mode}` belongs to diff-mode; magit must not \
                 rebind it: {chords:?}"
            );
        }
    }

    /// `]f` / `[f` live here, not on `magit-core-mode`.
    ///
    /// On core they were bound across all ten majors and meant
    /// something in one. In the list views they jumped between rows
    /// while claiming to move between files — a job `j` and `]]`
    /// already do. In the diff views they matched indented CONTEXT
    /// lines, so they walked through arbitrary code. In the rebase
    /// todo, whose rows sit at column 0, they matched nothing.
    #[test]
    fn file_navigation_is_bound_here_and_not_on_magit_core() {
        use lattice_mode::Mode;
        let hunk: Vec<&str> = magit_hunk_keymap_entries()
            .iter()
            .map(|e| e.chord)
            .collect();
        for c in ["]f", "[f"] {
            assert!(hunk.contains(&c), "`{c}` must be bound here: {hunk:?}");
        }
        let core: Vec<&str> = crate::MagitCoreMode
            .keymap()
            .entries
            .iter()
            .map(|e| e.chord)
            .collect();
        for c in ["]f", "[f"] {
            assert!(
                !core.contains(&c),
                "`{c}` must NOT still be on magit-core-mode, where it is bound \
                 on majors that have no files: {core:?}"
            );
        }
    }

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

#[cfg(test)]
mod at_position_tests {
    use super::at_position;
    use crate::hunk::SourcePos;
    use lattice_grammar::Effect;

    const POS: SourcePos = SourcePos { line: 41, byte: 12 };

    /// MG.50: an open becomes an open-AT, carrying BOTH axes.
    ///
    /// Both effect shapes matter: a working-tree file (`OpenBuffer`) and
    /// a blob (`OpenSyntheticBuffer`) are the two things `diff_target`
    /// returns, and magit-status produces one of each depending on which
    /// section the cursor sits under.
    ///
    /// The byte assertions are the point of this revision: the effect
    /// used to be built with a hardcoded `Position::new(line, 0)`, so
    /// every visit landed at the start of the line however far along the
    /// row the cursor had been.
    #[test]
    fn both_open_shapes_carry_the_line_and_the_offset() {
        match at_position(
            Effect::OpenBuffer {
                path: Some("/repo/src/main.rs".into()),
                force: false,
            },
            POS,
        ) {
            Effect::OpenBufferAt { position, .. } => {
                assert_eq!(position.line, POS.line);
                assert_eq!(position.byte, POS.byte);
            }
            other => panic!("a working-tree open must position: {other:?}"),
        }
        match at_position(
            Effect::OpenSyntheticBuffer {
                name: "*magit:file:staged:src/main.rs*".into(),
                mode_id: "magit-file-revision-mode".into(),
            },
            POS,
        ) {
            Effect::OpenSyntheticBufferAt {
                position, mode_id, ..
            } => {
                assert_eq!(position.line, POS.line);
                assert_eq!(position.byte, POS.byte);
                assert_eq!(mode_id, "magit-file-revision-mode");
            }
            other => panic!("a blob open must position: {other:?}"),
        }
    }

    /// An effect with nothing to position passes through untouched —
    /// a refusal must not be silently turned into an open.
    #[test]
    fn an_effect_with_nothing_to_position_is_unchanged() {
        let echo = Effect::Echo {
            level: lattice_grammar::EchoLevel::Warn,
            text: "no working-tree copy".into(),
        };
        assert!(matches!(at_position(echo, POS), Effect::Echo { .. }));
    }
}
