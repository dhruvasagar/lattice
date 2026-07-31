//! `*magit:file:<ref>:<path>*` — a file's content at a fixed
//! reference point, read-only.
//!
//! `<CR>` on a file line inside any magit buffer tied to a specific
//! commit or index state (magit-revision, the staged region of
//! magit-commit/magit-diff) resolves to this buffer instead of the
//! live working-tree file — the diff/commit you were looking at
//! describes a SPECIFIC version of the file, and the working-tree
//! copy may already have diverged from it (mid-rebase, after a
//! later edit, or simply because the commit is historical). Visiting
//! the CURRENT file for editing stays `<CR>` in buffers that are
//! themselves about current state (magit-status, magit-diff's
//! Unstaged scope) — see `magit.md` §6.3 for the full uniformity
//! rule across views.
//!
//! `<ref>` is either a real commit-ish (sha/tag/branch) or the
//! literal token `staged`, meaning "the index's blob for this path"
//! (`git show :<path>`) rather than a commit.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

use crate::headerline;

pub struct MagitFileRevisionMode;

impl MagitFileRevisionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-file-revision-mode")
    }
}

fn magit_file_revision_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            // MG.23f: blob navigation. Walking one file's history is
            // what this buffer is for — without these you can open a
            // revision but not step through them, so every step means
            // going back to the log.
            //
            // magit binds these to `n` / `p`; evil-collection-magit
            // remaps them to `gk` / `gj` and lattice follows it, for
            // the reason the remap exists: `n` is search-repeat, and a
            // read-only view of a file is exactly where you search.
            keymap_entry! { mode: Normal, chord: "gj", doc: "This file at the next revision", cmd: "action:magit-blob-next" },
            keymap_entry! { mode: Normal, chord: "gk", doc: "This file at the previous revision", cmd: "action:magit-blob-previous" },
        ]
    })
}

/// MG.23f: which way [`blob_step`] walks the file's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobStep {
    /// Older — the next entry in `git rev-list` order.
    Previous,
    /// Newer.
    Next,
}

/// The revision one step from `current` in `revisions`, which is
/// newest-first (`git rev-list` order).
///
/// `None` at either end rather than wrapping: a file's history has two
/// ends and silently jumping from the first commit to HEAD would read
/// as a glitch rather than as an edge. `None` too when `current` is not
/// in the list at all — which is what a `staged` pseudo-ref is, since
/// the index is not a commit and has no place in the walk.
///
/// Pure, so the walk is testable without a repository.
pub(crate) fn blob_step(revisions: &[String], current: &str, step: BlobStep) -> Option<String> {
    let at = revisions.iter().position(|r| r == current)?;
    let target = match step {
        BlobStep::Previous => at.checked_add(1)?,
        BlobStep::Next => at.checked_sub(1)?,
    };
    revisions.get(target).cloned()
}

/// Every commit that touched `path`, newest first.
///
/// `--follow` is deliberately absent: it would make the walk cross
/// renames, and the buffer name carries one path — a step that silently
/// changed which file you were reading would be worse than stopping.
fn file_revisions(workdir: &Path, path: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["log", "--format=%H", "--", &path.to_string_lossy()])
        .current_dir(workdir)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// MG.23f: `p` / `n` — the same file one revision older / newer.
fn blob_step_handlers() -> Vec<lattice_mode::ActionHandlerContribution> {
    fn step(ctx: &lattice_mode::ActionContext<'_>, step: BlobStep) -> Option<Effect> {
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let store = ctx.services.get::<lattice_mode::BufferStoreHandle>()?;
        let (git_ref, path) = store
            .name_for(buffer_id)
            .and_then(|name| parse_buffer_name(&name))?;
        let workdir = Repository::discover(".")
            .ok()
            .and_then(|r| r.workdir().map(|p| p.to_path_buf()))?;
        let revisions = file_revisions(&workdir, &path);
        match blob_step(&revisions, &git_ref, step) {
            Some(next) => Some(Effect::OpenSyntheticBuffer {
                name: format!("*magit:file:{next}:{}*", path.display()),
                mode_id: MagitFileRevisionMode::mode_id().to_string(),
            }),
            // Saying which end you are at beats a key that appears
            // broken — and `staged` lands here too, since the index is
            // not a commit and has no place in the walk.
            None => Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: match step {
                    BlobStep::Previous => "magit: no earlier revision of this file".to_string(),
                    BlobStep::Next => "magit: already at the newest revision".to_string(),
                },
            }),
        }
    }

    vec![
        lattice_mode::ActionHandlerContribution {
            action_name: "action:magit-blob-previous",
            handler: std::sync::Arc::new(|ctx| step(ctx, BlobStep::Previous)),
        },
        lattice_mode::ActionHandlerContribution {
            action_name: "action:magit-blob-next",
            handler: std::sync::Arc::new(|ctx| step(ctx, BlobStep::Next)),
        },
    ]
}

impl Mode for MagitFileRevisionMode {
    /// MG.14: the headerline registration — this mode's only
    /// per-activation resource. Dropping it removes the sticky row.
    type Guard = Option<crate::headerline::HeaderlineRegistration>;

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
    fn action_handlers(&self) -> Vec<lattice_mode::ActionHandlerContribution> {
        blob_step_handlers()
    }

    fn keymap(&self) -> Keymap {
        // No mode-specific chords — `q`/`gr`/nav come from magit-core
        // (this mode is in its `ActivationPolicy::Majors` list). `gr`
        // is a harmless no-op here (no refresh handler registered):
        // a fixed ref's blob content doesn't change.
        Keymap::from_entries(magit_file_revision_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<lattice_mode::BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            let parsed = store
                .name_for(buffer_id)
                .and_then(|name| parse_buffer_name(&name));

            // MG.14: `<path> @ <ref>`. Without it this buffer is
            // indistinguishable from the live file — which is exactly
            // the mistake the mode exists to prevent. Both fields come
            // out of the buffer name, so the header is complete before
            // `git show` runs.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            if let Some((git_ref, path)) = parsed.as_ref() {
                headerline::publish(&hl, headerline::file_revision_fields(git_ref, path));
            }

            let wd = workdir.clone();
            let text = tokio::task::spawn_blocking(move || match parsed {
                Some((git_ref, path)) => run_show_file(&wd, &git_ref, &path),
                None => "No file/ref given.\n".to_string(),
            })
            .await
            .unwrap_or_default();
            let snap = handle.snapshot();
            let last = snap.buffer.line_count().saturating_sub(1);
            let last_line = snap.buffer.line(last).unwrap_or_default();
            let end = Position::new(last, last_line.len() as u32);
            let _ = handle
                .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
                .await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.wake();
            }

            Ok(hl_registration)
        })
    }
}

/// `"*magit:file:<ref>:<path>*"` → `(ref, path)`. `ref` never
/// contains `:` (it's a sha/branch name or the literal `staged`
/// token), so the FIRST `:` in the stripped name is the ref/path
/// boundary — everything after it is the path, even if the path
/// itself contains further `:` characters (rare on POSIX, but not
/// disallowed).
pub(crate) fn parse_buffer_name(name: &str) -> Option<(String, PathBuf)> {
    let rest = name.strip_prefix("*magit:file:")?;
    let rest = rest.strip_suffix('*')?;
    let (git_ref, path) = rest.split_once(':')?;
    if git_ref.is_empty() || path.is_empty() {
        return None;
    }
    Some((git_ref.to_string(), PathBuf::from(path)))
}

/// `git show <ref>:<path>` — or, for the `staged` pseudo-ref, `git
/// show :<path>` (git's own syntax for "stage 0 of the index", i.e.
/// the staged blob).
fn run_show_file(workdir: &Path, git_ref: &str, path: &Path) -> String {
    let spec = if git_ref == "staged" {
        format!(":{}", path.display())
    } else {
        format!("{git_ref}:{}", path.display())
    };
    std::process::Command::new("git")
        .args(["show", &spec])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| format!("Could not show {spec}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_buffer_name_splits_ref_and_path() {
        let (r, p) = parse_buffer_name("*magit:file:a1b2c3d:src/main.rs*").unwrap();
        assert_eq!(r, "a1b2c3d");
        assert_eq!(p, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn parse_buffer_name_handles_staged_pseudo_ref() {
        let (r, p) = parse_buffer_name("*magit:file:staged:src/main.rs*").unwrap();
        assert_eq!(r, "staged");
        assert_eq!(p, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn parse_buffer_name_rejects_malformed_names() {
        assert!(parse_buffer_name("*magit:file:*").is_none());
        assert!(parse_buffer_name("*magit:file:onlyref*").is_none());
        assert!(parse_buffer_name("not a magit buffer").is_none());
    }

    /// The list is newest-first, so "previous" walks *forward* through
    /// it — the one inversion in this module and the one worth pinning.
    #[test]
    fn blob_step_walks_newest_first_order_in_both_directions() {
        let revs = vec!["new".to_string(), "mid".to_string(), "old".to_string()];
        assert_eq!(
            blob_step(&revs, "mid", BlobStep::Previous).as_deref(),
            Some("old")
        );
        assert_eq!(
            blob_step(&revs, "mid", BlobStep::Next).as_deref(),
            Some("new")
        );
    }

    #[test]
    fn blob_step_stops_at_both_ends_rather_than_wrapping() {
        let revs = vec!["new".to_string(), "old".to_string()];
        assert!(blob_step(&revs, "old", BlobStep::Previous).is_none());
        assert!(blob_step(&revs, "new", BlobStep::Next).is_none());
    }

    /// `staged` is not a commit, so it is not in the walk. Landing on
    /// `None` is what turns `p` there into the echo rather than a jump
    /// to whatever happened to sit at index 0.
    #[test]
    fn blob_step_refuses_a_ref_that_is_not_in_the_history() {
        let revs = vec!["new".to_string(), "old".to_string()];
        assert!(blob_step(&revs, "staged", BlobStep::Previous).is_none());
        assert!(blob_step(&revs, "staged", BlobStep::Next).is_none());
    }
}

/// MG.23f: the walk against a real repository — `file_revisions` must
/// agree with git about both *which* commits touched the file and what
/// order they come in, or `p`/`n` step somewhere plausible and wrong.
#[cfg(test)]
mod blob_navigation_round_trip {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn rev(dir: &Path, spec: &str) -> String {
        let out = Command::new("git")
            .args(["rev-parse", spec])
            .current_dir(dir)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Three commits, but only two touch `a.txt` — so a walk that
    /// listed every commit instead of the file's own would step into a
    /// revision where `a.txt` never changed.
    fn three_commit_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "a: one"]);
        std::fs::write(p.join("b.txt"), "unrelated\n").unwrap();
        git_ok(p, &["add", "b.txt"]);
        git_ok(p, &["commit", "-m", "b: unrelated"]);
        std::fs::write(p.join("a.txt"), "two\n").unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "a: two"]);
        dir
    }

    #[test]
    fn file_revisions_lists_only_the_commits_that_touched_the_file() {
        let dir = three_commit_repo();
        let revs = file_revisions(dir.path(), Path::new("a.txt"));
        assert_eq!(
            revs.len(),
            2,
            "the unrelated middle commit must not be in a.txt's history: {revs:?}"
        );
        assert_eq!(revs[0], rev(dir.path(), "HEAD"), "newest first");
    }

    #[test]
    fn stepping_back_from_head_lands_on_the_files_earlier_revision() {
        let dir = three_commit_repo();
        let p = dir.path();
        let revs = file_revisions(p, Path::new("a.txt"));
        let head = rev(p, "HEAD");

        let earlier = blob_step(&revs, &head, BlobStep::Previous).expect("an earlier revision");
        assert_eq!(
            run_show_file(p, &earlier, Path::new("a.txt")).trim(),
            "one",
            "`p` must land on the content as it was, not on HEAD's"
        );

        // ...and back again, so the pair is genuinely an inverse.
        assert_eq!(
            blob_step(&revs, &earlier, BlobStep::Next).as_deref(),
            Some(head.as_str())
        );
        // The earlier revision is the file's first, so there is no
        // further step back.
        assert!(blob_step(&revs, &earlier, BlobStep::Previous).is_none());
    }
}
