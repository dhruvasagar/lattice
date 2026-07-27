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
use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{CommandRegistryHandle, Effect};
use lattice_mode::{
    ActionContext, ActionHandlerRegistryHandle, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

use crate::magit_core_mode::ActionRegsGuard;

pub struct MagitRevisionMode;

impl MagitRevisionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-revision-mode")
    }
}

fn magit_revision_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Visit file at this commit", cmd: "action:magit-revision-visit-file" },
        ]
    })
}

struct RevisionState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    sha: String,
}

impl Mode for MagitRevisionMode {
    type Guard = ActionRegsGuard;

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
        // `q`/`gr`/nav come from magit-core (this mode is in its
        // `ActivationPolicy::Majors` list). `gr` is a harmless no-op
        // here (no refresh handler registered); a commit's content
        // doesn't change under a fixed sha.
        Keymap::from_entries(magit_revision_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(ActionRegsGuard::default());
            };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Extract the sha from the buffer name:
            // "*magit:commit:<sha>*" → "<sha>".
            let sha = store
                .name_for(buffer_id)
                .and_then(|name| {
                    let s = name.strip_prefix("*magit:commit:")?;
                    Some(s.strip_suffix("*")?.to_string())
                })
                .unwrap_or_default();

            let wd = workdir.clone();
            let sha_for_task = sha.clone();
            let text = tokio::task::spawn_blocking(move || run_show(&wd, &sha_for_task))
                .await
                .unwrap_or_default();
            // `git show --stat -p` is header lines (commit/author/date/
            // message/stat-summary) followed by a unified diff — none
            // of the header lines start with `+`/`-`/`@@`/`diff --git`/
            // `---`/`+++`, so the plain whole-buffer diff styler is
            // safe to apply directly (same reuse `magit-diff-mode`
            // makes for its own `git diff` output).
            let spans = crate::highlight::diff_styled_spans(&text);
            let snap = handle.snapshot();
            let last = snap.buffer.line_count().saturating_sub(1);
            let last_line = snap.buffer.line(last).unwrap_or_default();
            let end = Position::new(last, last_line.len() as u32);
            let _ = handle
                .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
                .await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.store_and_wake(buffer_id, spans);
            }

            let state = Arc::new(Mutex::new(RevisionState {
                buffer_id,
                store: store.clone(),
                sha,
            }));

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            // <CR> — visit the file at cursor as of this commit.
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-revision-visit-file") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let g = s.lock().ok()?;
                            if g.sha.is_empty() {
                                return None;
                            }
                            let handle = g.store.handle_for(g.buffer_id)?;
                            let path = file_at_cursor(&handle, ctx.cursor.line)?;
                            Some(Effect::OpenSyntheticBuffer {
                                name: format!("*magit:file:{}:{}*", g.sha, path.display()),
                                mode_id: "magit-file-revision-mode".to_string(),
                            })
                        }),
                    ));
                }
            }

            Ok(ActionRegsGuard(regs))
        })
    }
}

/// Resolve the file at `line`: either `line` IS a `--stat` summary
/// row (`" path/to/x.rs | 12 +++++-----"`), or we walk upward to the
/// nearest `diff --git a/<path> b/<path>` header — same two shapes
/// `git show --stat -p` ever produces a path in.
fn file_at_cursor(handle: &Arc<dyn lattice_runtime::Document>, line: u32) -> Option<PathBuf> {
    let snap = handle.snapshot();
    if let Some(text) = snap.buffer.line(line) {
        if let Some(path) = parse_stat_line(&text) {
            return Some(path);
        }
    }
    for l in (0..=line).rev() {
        let text = snap.buffer.line(l)?;
        if let Some(rest) = text.strip_prefix("diff --git a/") {
            let path = rest.split(" b/").next()?;
            return Some(PathBuf::from(path));
        }
    }
    None
}

/// `git show --stat`'s summary line: `" <path> | <N> <bar-chart>"`.
/// Rejected if there's no ` | ` separator (commit/author/date/subject
/// lines never contain one).
fn parse_stat_line(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim_start();
    let (path, _rest) = trimmed.split_once(" | ")?;
    (!path.is_empty()).then(|| PathBuf::from(path.trim()))
}

fn run_show(workdir: &std::path::Path, sha: &str) -> String {
    if sha.is_empty() {
        return "No commit sha given.\n".to_string();
    }
    std::process::Command::new("git")
        .args(["show", "--stat", "-p", sha])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| format!("Could not show commit {sha}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_stat_line_extracts_path() {
        assert_eq!(
            parse_stat_line(" src/main.rs | 12 +++++-----"),
            Some(PathBuf::from("src/main.rs"))
        );
    }

    #[test]
    fn parse_stat_line_rejects_non_stat_lines() {
        assert_eq!(parse_stat_line("commit a1b2c3d"), None);
        assert_eq!(parse_stat_line("    fix the thing"), None);
        assert_eq!(parse_stat_line(""), None);
    }
}
