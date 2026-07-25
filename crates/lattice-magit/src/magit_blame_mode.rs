//! MG.7: magit-blame major mode.
//!
//! Runs `git blame --line-porcelain <path>` on open, populates
//! buffer with annotated content. <CR> shows commit, p re-blames.

use std::sync::{Arc, OnceLock};

use lattice_config;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_vcs::Repository;

use crate::magit_status_mode::MagitStatusMode;

pub struct MagitBlameMode;

impl MagitBlameMode {
    pub fn mode_id() -> ModeId { ModeId::new("magit-blame-mode") }
}

fn magit_blame_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show commit for blamed line", cmd: "action:magit-blame-show-commit" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Re-blame at parent commit", cmd: "action:magit-blame-parent" },
        ]
    })
}

impl Mode for MagitBlameMode {
    type Guard = ();

    fn id(&self) -> ModeId { Self::mode_id() }
    fn kind(&self) -> ModeKind { ModeKind::Major }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> { None }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet { CapabilitySet::empty() }
    fn keymap(&self) -> Keymap { Keymap::from_entries(magit_blame_keymap_entries()) }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx
                .service::<lattice_mode::BufferStoreHandle>()
            else {
                return Ok(());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(());
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(());
            };

            // Get the blamed file path from the active buffer (before
            // switching to blame view) or from current file.
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // Extract file path from buffer name: "*magit:blame:<path>*" → "<path>"
            let file_path = store
                .name_for(buffer_id)
                .and_then(|name| {
                    let s = name.strip_prefix("*magit:blame:")?;
                    Some(s.strip_suffix("*")?.to_string())
                })
                .unwrap_or_else(|| ".".to_string());

            let h = handle.clone();
            let wd = workdir.clone();
            runtime.spawn_blocking(move || {
                let text = run_blame(&wd, &file_path);
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let snap = h.snapshot();
                    let last = snap.buffer.line_count().saturating_sub(1);
                    let last_line = snap.buffer.line(last).unwrap_or_default();
                    let end = Position::new(last, last_line.len() as u32);
                    let _ = h.apply_edit_batch(vec![
                        Edit::replace(Range::new(Position::ZERO, end), text),
                    ]).await;
                });
            });

            Ok(())
        })
    }
}

fn run_blame(workdir: &std::path::Path, path: &str) -> String {
    if path.is_empty() || path == "." {
        return "No file to blame — open :magit-blame <file> or run from a file buffer.\n"
            .to_string();
    }
    let output = std::process::Command::new("git")
        .args(["blame", "--line-porcelain", path])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| format!("Could not blame {}\n", path));

    // Format blame output: extract author + SHA per line
    let mut result = String::new();
    for line in output.lines() {
        if line.len() < 40 {
            continue;
        }
        let sha: String = line.chars().take(8).collect();
        // porcelain format — the interesting lines are author, committer-time, filename
        if line.starts_with("author ") {
            let author = line.strip_prefix("author ").unwrap_or("?");
            result.push_str(&format!("{} {:>12}  ", sha, author));
        } else if line.starts_with("\t") {
            result.push_str(line.strip_prefix("\t").unwrap_or(""));
            result.push('\n');
        }
    }

    if result.is_empty() { format!("No blame data for {}\n", path) } else { result }
}
