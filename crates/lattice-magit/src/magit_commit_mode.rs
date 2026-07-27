//! MG.4: magit-commit major mode.
//!
//! Shows the staged diff (read-only top region) and an editable
//! message region below. C-c C-c commits, C-c C-k aborts.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{CommandRegistryHandle, Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandlerRegistryHandle, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::{Commit, Repository};

use crate::magit_core_mode::ActionRegsGuard;

pub struct MagitCommitMode;

impl MagitCommitMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-commit-mode")
    }
}

const MESSAGE_MARKER: &str = "--- Commit message (edit below) ---";

fn magit_commit_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<C-c><C-c>", doc: "Confirm commit", cmd: "action:magit-commit-confirm" },
            keymap_entry! { mode: Insert, chord: "<C-c><C-k>", doc: "Abort commit", cmd: "action:magit-commit-abort" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-c>", doc: "Confirm commit", cmd: "action:magit-commit-confirm" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-k>", doc: "Abort commit", cmd: "action:magit-commit-abort" },
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Visit staged file at cursor", cmd: "action:magit-commit-visit-file" },
        ]
    })
}

struct CommitState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    amend: bool,
    /// First line at/after `MESSAGE_MARKER` — bounds the staged-diff
    /// region so `<CR>`'s file-visit handler doesn't fire while the
    /// cursor is down in the (editable) message text.
    diff_end_line: u32,
}

impl Mode for MagitCommitMode {
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
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_commit_keymap_entries())
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

            // Detect amend: opened via `ca` → buffer name is "*magit:amend*"
            let amend = store
                .name_for(buffer_id)
                .map(|n| n.contains("amend"))
                .unwrap_or(false);

            // Populate the buffer: staged diff + message area. Amend
            // pre-populates the previous commit's message instead of a
            // blank region, matching what it's about to replace.
            let wd = workdir.clone();
            let (staged, prior_message) = tokio::task::spawn_blocking(move || {
                let staged = run_staged_diff(&wd);
                let prior = if amend {
                    run_prior_commit_message(&wd)
                } else {
                    String::new()
                };
                (staged, prior)
            })
            .await
            .unwrap_or_default();
            let initial = format!(
                "--- Staged diff (review before committing) ---\n\
                 {}\n\
                 {MESSAGE_MARKER}\n\
                 {}\n\
                 \n",
                if staged.is_empty() {
                    "(nothing staged)"
                } else {
                    &staged
                },
                prior_message.trim(),
            );
            // Diff content starts right after the header line (line 0)
            // and ends at the message marker — scoping the styler to
            // that range keeps the header's own "---" from being
            // misclassified as a diff file marker (see
            // `highlight::commit_buffer_styled_spans`'s doc comment).
            let diff_end_line = initial
                .lines()
                .position(|l| l.contains(MESSAGE_MARKER))
                .unwrap_or(0);
            let spans = crate::highlight::commit_buffer_styled_spans(&initial, 1, diff_end_line);
            let snap = handle.snapshot();
            let last = snap.buffer.line_count().saturating_sub(1);
            let last_line = snap.buffer.line(last).unwrap_or_default();
            let end = Position::new(last, last_line.len() as u32);
            let _ = handle
                .apply_edit_batch(vec![Edit::replace(
                    Range::new(Position::ZERO, end),
                    initial,
                )])
                .await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.store_and_wake(buffer_id, spans);
            }

            // Register action handlers
            let state = Arc::new(Mutex::new(CommitState {
                buffer_id,
                store: store.clone(),
                workdir,
                amend,
                diff_end_line: diff_end_line as u32,
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

            // ── confirm (C-c C-c) ──────────────────────
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-commit-confirm") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            let (message, workdir, amend) = {
                                let g = s.lock().ok()?;
                                let handle = g.store.handle_for(g.buffer_id)?;
                                let snap = handle.snapshot();
                                let mut message = String::new();
                                let mut in_message = false;
                                for l in 0..snap.buffer.line_count() as u32 {
                                    let text = snap.buffer.line(l).unwrap_or_default();
                                    if text.contains(MESSAGE_MARKER) {
                                        in_message = true;
                                        continue;
                                    }
                                    if in_message && !text.trim().is_empty() {
                                        message.push_str(&text);
                                        message.push('\n');
                                    }
                                }
                                (message, g.workdir.clone(), g.amend)
                            };
                            if message.trim().is_empty() {
                                // Fail loud instead of silently doing
                                // nothing — an empty subject used to
                                // just no-op the chord with no feedback.
                                return Some(Effect::Echo {
                                    level: lattice_grammar::EchoLevel::Error,
                                    text: "magit: commit message is empty".to_string(),
                                });
                            }
                            // Commit is a bounded, single-object git
                            // write (unlike `git status`/`git diff`,
                            // it never scans the working tree) — but
                            // it's still disk I/O, so it stays off the
                            // actor thread like every other mutation.
                            // The buffer closes optimistically; a
                            // failure surfaces via `tracing::error!`
                            // (no synchronous path back to the echo
                            // area from a detached task) rather than
                            // leaving the compose buffer open forever
                            // on a rare `gix` failure.
                            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                                let Ok(repo) = Repository::discover(&workdir) else {
                                    tracing::error!(target: "lattice_magit", "commit: repo discover failed");
                                    return;
                                };
                                let result = if amend {
                                    Commit::amend(&repo, message.trim())
                                } else {
                                    Commit::create(&repo, message.trim())
                                };
                                if let Err(e) = result {
                                    tracing::error!(target: "lattice_magit", "commit failed: {e}");
                                }
                            }));
                            Some(Effect::QuitEditor {
                                force: false,
                                scope: QuitScope::Pane,
                            })
                        }),
                    ));
                }
            }

            // ── abort (C-c C-k) ─────────────────────────
            {
                if let Some(cid) = registry.id_by_name("action:magit-commit-abort") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            Some(Effect::QuitEditor {
                                force: false,
                                scope: QuitScope::Pane,
                            })
                        }),
                    ));
                }
            }

            // <CR> — visit the file at cursor AS STAGED (the index
            // blob), not the live working-tree file: this buffer
            // shows the STAGED diff specifically, which may already
            // differ from a since-edited working copy. Same target
            // magit-diff-mode's Staged-scoped `<CR>` opens.
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-commit-visit-file") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
                            let g = s.lock().ok()?;
                            if ctx.cursor.line >= g.diff_end_line {
                                return None;
                            }
                            let handle = g.store.handle_for(g.buffer_id)?;
                            let path = file_at_cursor(&handle, ctx.cursor.line)?;
                            Some(Effect::OpenSyntheticBuffer {
                                name: format!("*magit:file:staged:{}*", path.display()),
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

/// Walk upward from `line` to the nearest `diff --git a/<path>
/// b/<path>` header — same shape `magit_diff_mode::file_at_cursor`
/// uses, duplicated here since this buffer's state type differs.
fn file_at_cursor(
    handle: &Arc<dyn lattice_runtime::Document>,
    line: u32,
) -> Option<std::path::PathBuf> {
    let snap = handle.snapshot();
    for l in (0..=line).rev() {
        let text = snap.buffer.line(l)?;
        if let Some(rest) = text.strip_prefix("diff --git a/") {
            let path = rest.split(" b/").next()?;
            return Some(std::path::PathBuf::from(path));
        }
    }
    None
}

fn run_staged_diff(workdir: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

/// `git log -1 --format=%B` — the current HEAD commit's full message,
/// used to pre-populate the amend buffer instead of leaving it blank.
fn run_prior_commit_message(workdir: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}
