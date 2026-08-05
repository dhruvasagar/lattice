//! Fold audit fix (MG.6/MG.7): `*magit:commit:<sha>*` revision view.
//!
//! A read-only `git show` of one commit. Previously, magit-log's
//! `<CR>` wrote the same content to an uncleaned temp file in the
//! repo workdir and opened it via a plain `Effect::OpenBuffer` —
//! this is the real synthetic buffer the design always specified,
//! shared by magit-log's `<CR>` and magit-blame's `<CR>`.
//!
//! `<CR>` on a file line here (the `--stat` summary or a `diff --git`
//! header) opens that file's content AS OF THIS COMMIT
//! (`magit-file-revision-mode`), not the live working-tree file —
//! see `magit_file_revision_mode`'s doc comment for why.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    BufferStoreHandle, CapabilitySet, Keymap, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet,
};

use crate::buffer_state::{BufferStateGuard, BufferStates};
use crate::headerline;

pub struct MagitRevisionMode;

impl MagitRevisionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-revision-mode")
    }
}

pub struct RevisionState {
    sha: String,
    /// MG.23g: where `a` / `-` apply the hunk under the cursor. Read
    /// from the repository at activation, because a `git apply` needs a
    /// directory and this buffer has no file of its own.
    workdir: PathBuf,
}

/// MG.23g: this buffer's [`MagitView`], so `a` / `-` can act on a hunk
/// of the commit it shows.
///
/// The view exists for `diff_source` and `workdir`; the rest of the
/// trait declines. Publishing it is what turns `magit-core-mode`'s
/// generic hunk resolution loose in here — nothing about `a` / `-` is
/// specific to this mode, which is exactly why the handler is not.
struct RevisionView(Arc<Mutex<RevisionState>>);

impl crate::buffer_state::MagitView for RevisionView {
    /// This buffer's content is a unified diff, so "a file" is a
    /// `diff --git` header — not the generic indented-row scan, which
    /// here matches every indented CONTEXT line and would walk `]f`
    /// through arbitrary code.
    fn file_lines(
        &self,
        store: &lattice_mode::BufferStoreHandle,
        buffer: lattice_core::BufferId,
    ) -> Option<Vec<u32>> {
        Some(crate::magit_core_mode::diff_file_lines(store, buffer))
    }

    /// A fixed sha's `git show` cannot change, so `gr` has nothing to
    /// rebuild. `None` rather than a re-run: repainting identical text
    /// would move the cursor for no reason.
    ///
    /// This is also what `a` / `-` get after applying — correctly. The
    /// commit is unchanged by putting one of its hunks in the working
    /// tree; what changed is the tree, which this buffer does not show.
    fn refresh(&self) -> Option<Effect> {
        None
    }

    /// Everything here came out of a commit, so a hunk under the cursor
    /// is history — `a` applies it to the working tree, `-` reverses it
    /// back out, and `s` / `u` are refused with a sentence saying so.
    fn diff_source(
        &self,
        _cursor: lattice_protocol::position::Position,
    ) -> Option<crate::buffer_state::DiffSource> {
        Some(crate::buffer_state::DiffSource::Committed)
    }

    /// MG.22: this commit's version of the file.
    fn diff_target(
        &self,
        path: &std::path::Path,
        _cursor: lattice_protocol::position::Position,
    ) -> Option<Effect> {
        let sha = self.0.lock().ok()?.sha.clone();
        (!sha.is_empty()).then(|| Effect::OpenSyntheticBuffer {
            name: crate::magit_file_revision_mode::blob_buffer_name(&sha, path),
            mode_id: "magit-file-revision-mode".to_string(),
        })
    }

    /// MG.24c: this buffer IS one commit, so the answer does not depend
    /// on the cursor — every line of a `git show` belongs to the sha in
    /// the buffer's name.
    ///
    /// `magit-core-mode.md` has claimed since MG.20 that `A` / `_` /
    /// `O` work in "the revision view". They did not: this view was
    /// added by MG.23g for `a` / `-` and never overrode
    /// `commit_at_cursor`, so the trait default returned `None` and the
    /// chords were consumed dead keys. Reading a sha off the line under
    /// the cursor would have been the wrong fix — the `--stat` rows and
    /// the diff body carry no sha at all, so it would work on the
    /// header lines and nowhere else.
    fn commit_at_cursor(&self, _cursor: lattice_protocol::position::Position) -> Option<String> {
        let sha = self.0.lock().ok()?.sha.clone();
        (!sha.is_empty()).then_some(sha)
    }

    fn workdir(&self) -> Option<PathBuf> {
        self.0.lock().ok().map(|g| g.workdir.clone())
    }
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type RevisionStatesHandle = Arc<BufferStates<RevisionState>>;

/// MG.34: the two buffer-name forms this mode answers to.
///
/// The second exists because magit's `M` "Merged" asks a question whose
/// answer is *a different commit from the one you named*, and finding it
/// costs a `git log` walk. The handler that fires the chord is
/// synchronous and must not run `git` on the actor thread (MG.31), so it
/// cannot resolve the merge and put the answer in the buffer name.
/// Encoding the *question* in the name instead lets this mode resolve it
/// inside the `spawn_blocking` it already runs for `git show` — no new
/// async seam, and one buffer open rather than two.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RevisionTarget {
    /// `*magit:commit:<sha>*` — show this commit.
    Commit(String),
    /// `*magit:merged:<sha>*` — show the merge that brought `<sha>` into
    /// HEAD. The sha in the name is the **source**; the commit shown is
    /// derived from it.
    Merged(String),
}

/// MG.34: the buffer name that asks "which merge brought `sha` in?".
pub(crate) fn merged_buffer_name(sha: &str) -> String {
    format!("*magit:merged:{sha}*")
}

/// Which question a buffer name asks. `None` for a name this mode does
/// not own — the caller shows the same "no commit sha given" text it
/// showed before MG.34, rather than guessing.
fn parse_target(name: &str) -> Option<RevisionTarget> {
    let body = name.strip_suffix('*')?;
    if let Some(sha) = body.strip_prefix("*magit:commit:") {
        return (!sha.is_empty()).then(|| RevisionTarget::Commit(sha.to_string()));
    }
    let sha = body.strip_prefix("*magit:merged:")?;
    (!sha.is_empty()).then(|| RevisionTarget::Merged(sha.to_string()))
}

/// MG.34: what a `*magit:merged:*` buffer says when nothing merged the
/// commit in.
///
/// Not an error, and worded so it does not read as one: a commit made
/// straight onto the branch you are on has no merge, which is the
/// ordinary case for most of a repository's history. Showing an empty
/// buffer would leave the reader unable to tell that from a failure.
fn not_merged_text(sha: &str) -> String {
    format!(
        "{sha} was not merged into HEAD.\n\
         \n\
         No merge commit lies on the ancestry path from it to HEAD, so it\n\
         reached this branch by a direct commit or a fast-forward rather\n\
         than by a merge. There is nothing to show.\n"
    )
}

impl Mode for MagitRevisionMode {
    type Guard = BufferStateGuard<RevisionState>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// MG.22: no chords of its own any more. `q` / `gr` / navigation
    /// come from `magit-core-mode`, and `<CR>` / `s` / `u` / `a` / `-`
    /// from `magit-hunk-mode` — this buffer is entirely diff content,
    /// so everything that acts on it belongs to the mode that owns
    /// diff content. What stays here is the `MagitView` telling those
    /// modes *which commit* they are looking at.
    fn keymap(&self) -> Keymap {
        Keymap::default()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let orphan = || BufferStateGuard::new(Arc::new(BufferStates::default()), buffer_id);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(orphan());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(orphan());
            };
            let workdir = crate::workdir::magit_workdir().unwrap_or_default();

            // MG.34: which question the buffer name asks — a commit
            // directly, or the merge that brought one in.
            let target = store.name_for(buffer_id).as_deref().and_then(parse_target);
            // The sha the state starts with. For `Merged` it is not
            // known yet (that is the whole question), so the state
            // starts empty and is filled in below once the walk has
            // run — the same late-resolve `magit-rebase-mode` does for
            // its upstream.
            let sha = match &target {
                Some(RevisionTarget::Commit(sha)) => sha.clone(),
                _ => String::new(),
            };

            // MG.14: the commit's identity (author, date, subject) is
            // not in the buffer name, so the header is filled in below
            // from the same `spawn_blocking` that runs `git show`.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: publish BEFORE the first `.await` — see the note
            // in `magit_branch_mode::on_activate`.
            let Some(states) = ctx.service::<RevisionStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                RevisionState {
                    sha: sha.clone(),
                    workdir: workdir.clone(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            // MG.23g: publish the view, or `a` / `-` have nothing to
            // ask about this buffer and refuse in it.
            if let Some(views) = ctx.service::<crate::buffer_state::MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(RevisionView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            let wd = workdir.clone();
            let context = crate::actions::context_lines(
                &ctx.service::<std::sync::Arc<lattice_config::ConfigRegistry>>()
                    .map(|outer| (*outer).clone()),
            );
            // MG.34: the merge walk runs here, inside the
            // `spawn_blocking` that was already fetching `git show` —
            // so the answer and the patch land in one paint. Splitting
            // them would show the buffer, then relabel it, which the
            // keystroke UX contract forbids.
            let (resolved, text, meta) = tokio::task::spawn_blocking(move || {
                let shown = match target {
                    Some(RevisionTarget::Commit(sha)) => Some(sha),
                    Some(RevisionTarget::Merged(source)) => {
                        match crate::magit_core_mode::resolve_merge_commit(&wd, &source) {
                            Some(merge) => Some(merge),
                            // Ordinary answer, not a failure — say so
                            // and stop, rather than `git show ""`.
                            None => {
                                return (
                                    String::new(),
                                    not_merged_text(&source),
                                    headerline::RevisionMeta::default(),
                                );
                            }
                        }
                    }
                    None => None,
                };
                let shown = shown.unwrap_or_default();
                let text = run_show(&wd, &shown, context);
                let meta = commit_meta(&wd, &shown);
                (shown, text, meta)
            })
            .await
            .unwrap_or_default();
            headerline::publish(&hl, headerline::revision_fields(&meta));
            // `git show --stat -p` is header lines (commit/author/date/
            // message/stat-summary) followed by a unified diff — none
            // of the header lines start with `+`/`-`/`@@`/`diff --git`/
            // `---`/`+++`, so the plain whole-buffer diff styler is
            // safe to apply directly (same reuse `magit-diff-mode`
            // makes for its own `git diff` output).
            let spans = crate::highlight::diff_styled_spans(&text);
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.store_and_wake(buffer_id, spans);
            }

            // MG.34: late-resolved, now the walk has run. Without this
            // the merge view's `A` / `_` / `O` / `<CR>` would act on
            // the *source* commit the name carries rather than on the
            // merge the buffer is showing — the same commit under two
            // names, which is the failure mode this whole slice exists
            // to avoid. Empty (unmerged) leaves them declining, which
            // is right: there is no commit on screen to act on.
            if let Ok(mut g) = state.lock() {
                g.sha = resolved;
            }

            Ok(guard)
        })
    }
}

/// MG.14 header data: the commit's short sha, author, relative date
/// and subject. A separate `git show -s --format=…` rather than
/// scraping `run_show`'s header, because that output is locale- and
/// config-dependent (`log.date`, `i18n.logOutputEncoding`) while
/// `--format` is not. `-s` suppresses the diff, so this is a
/// metadata-only read next to the patch `run_show` already fetches.
pub(crate) fn commit_meta(workdir: &std::path::Path, sha: &str) -> headerline::RevisionMeta {
    if sha.is_empty() {
        return headerline::RevisionMeta::default();
    }
    let raw = std::process::Command::new("git")
        .args(["show", "-s", "--format=%h%x00%an%x00%ar%x00%s", sha])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default();
    headerline::parse_revision_meta(&raw)
}

fn run_show(workdir: &std::path::Path, sha: &str, context: i64) -> String {
    if sha.is_empty() {
        return "No commit sha given.\n".to_string();
    }
    std::process::Command::new("git")
        .args(["show", "--stat", "-p", &format!("--unified={context}"), sha])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| format!("Could not show commit {sha}\n"))
}

// ── MG.34: the `*magit:merged:*` name form ──────────────────────────
#[cfg(test)]
mod merged_target {
    use super::*;

    /// The two forms, and that they stay apart. Both live under
    /// `*magit:` and both carry a bare sha, so a parser that checked one
    /// prefix loosely would show the *source* commit where the merge was
    /// asked for — the same commit under two names, which is exactly the
    /// confusion this form exists to avoid.
    #[test]
    fn the_two_name_forms_stay_distinct() {
        assert_eq!(
            parse_target("*magit:commit:abc123*"),
            Some(RevisionTarget::Commit("abc123".into()))
        );
        assert_eq!(
            parse_target("*magit:merged:abc123*"),
            Some(RevisionTarget::Merged("abc123".into()))
        );
    }

    /// A name this mode does not own, and the two empty-sha forms. An
    /// empty sha would reach `git show ""`, whose failure text names no
    /// commit and reads like a bug in the editor.
    #[test]
    fn names_without_a_sha_are_not_targets() {
        assert_eq!(parse_target("*magit:commit:*"), None);
        assert_eq!(parse_target("*magit:merged:*"), None);
        assert_eq!(parse_target("*magit:log:main*"), None);
        assert_eq!(parse_target("a.txt"), None);
    }

    #[test]
    fn the_builder_and_the_parser_agree() {
        assert_eq!(
            parse_target(&merged_buffer_name("deadbeef")),
            Some(RevisionTarget::Merged("deadbeef".into()))
        );
    }

    /// `None` from the walk is the ordinary answer for a commit made
    /// straight onto the branch, so the buffer has to say that rather
    /// than being empty — an empty buffer is indistinguishable from a
    /// failure. The sha is named so the reader knows which commit was
    /// asked about.
    #[test]
    fn the_unmerged_message_names_the_commit_and_does_not_read_as_an_error() {
        let text = not_merged_text("abc123");
        assert!(text.contains("abc123"), "must name the commit: {text}");
        assert!(
            !text.to_lowercase().contains("error")
                && !text.to_lowercase().contains("failed")
                && !text.to_lowercase().contains("could not"),
            "a commit that was never merged is not a failure: {text}"
        );
    }
}

// MG.22: this mode's `parse_stat_line` / `file_at_cursor` tests moved
// with the functions, to `hunk::path_at_cursor_tests`. They gained a
// case in the move — the one that matters here, and the one this
// module's copy could never have caught, because it tested the stat
// parser in isolation rather than in the order the caller used it: a
// diff body line containing ` | ` used to resolve to the text left of
// the pipe, because the stat check ran first.
