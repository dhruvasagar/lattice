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
    fn diff_target(&self, path: &std::path::Path) -> Option<Effect> {
        let sha = self.0.lock().ok()?.sha.clone();
        (!sha.is_empty()).then(|| Effect::OpenSyntheticBuffer {
            name: format!("*magit:file:{sha}:{}*", path.display()),
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

            // Extract the sha from the buffer name:
            // "*magit:commit:<sha>*" → "<sha>".
            let sha = store
                .name_for(buffer_id)
                .and_then(|name| {
                    let s = name.strip_prefix("*magit:commit:")?;
                    Some(s.strip_suffix("*")?.to_string())
                })
                .unwrap_or_default();

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
            let sha_for_task = sha.clone();
            let (text, meta) = tokio::task::spawn_blocking(move || {
                (
                    run_show(&wd, &sha_for_task, context),
                    run_show_meta(&wd, &sha_for_task),
                )
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
fn run_show_meta(workdir: &std::path::Path, sha: &str) -> headerline::RevisionMeta {
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

// MG.22: this mode's `parse_stat_line` / `file_at_cursor` tests moved
// with the functions, to `hunk::path_at_cursor_tests`. They gained a
// case in the move — the one that matters here, and the one this
// module's copy could never have caught, because it tested the stat
// parser in isolation rather than in the order the caller used it: a
// diff body line containing ` | ` used to resolve to the text left of
// the pipe, because the stat check ran first.
