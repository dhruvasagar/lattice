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
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet,
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
    ENTRIES.get_or_init(Vec::new)
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
fn parse_buffer_name(name: &str) -> Option<(String, PathBuf)> {
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
}
