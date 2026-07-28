//! MG.6: magit-log major mode.
//!
//! Runs `git log --oneline --graph --decorate -50` on open,
//! populates buffer content. <CR> shows commit detail.
//!
//! `*magit:log:<path>*` scopes the log to one file's history
//! (`git log -- <path>`) — the target of `C-c f l` in the file
//! dispatch transient, mirroring `magit-blame`/`magit-diff`'s
//! path-in-buffer-name pattern. Bare `*magit:log*` stays repo-wide.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};

pub struct MagitLogMode;

impl MagitLogMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-log-mode")
    }
}

fn magit_log_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show commit detail at cursor", cmd: "action:magit-log-show-commit" },
        ]
    })
}

pub struct LogState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    /// `Some` when this buffer is scoped to one file's history
    /// (opened as `*magit:log:<path>*`); `None` for the repo-wide
    /// `*magit:log*`.
    path: Option<std::path::PathBuf>,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type LogStatesHandle = Arc<BufferStates<LogState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<LogState>>> {
    crate::buffer_state::state_for::<LogState>(ctx)
}

impl Mode for MagitLogMode {
    type Guard = BufferStateGuard<LogState>;

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
        Keymap::from_entries(magit_log_keymap_entries())
    }

    /// MG.13: boot-registered — see `buffer_state`'s module docs. `gr`
    /// is NOT here: it is a shared action owned by `magit-core-mode`
    /// and reached through this mode's `MagitView` below.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // <CR> — show commit detail for the SHA at cursor.
            ActionHandlerContribution {
                action_name: "action:magit-log-show-commit",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let g = s.lock().ok()?;
                    let handle = g.store.handle_for(g.buffer_id)?;
                    let snap = handle.snapshot();
                    let line = snap.buffer.line(ctx.cursor.line)?;
                    let sha = extract_sha(&line)?;
                    Some(Effect::OpenSyntheticBuffer {
                        name: format!("*magit:commit:{sha}*"),
                        mode_id: "magit-revision-mode".to_string(),
                    })
                }),
            },
        ]
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
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // "*magit:log:<path>*" scopes the history to one file
            // (mirrors magit-blame/magit-diff's file-in-buffer-name
            // pattern); bare "*magit:log*" stays repo-wide.
            let path: Option<std::path::PathBuf> = store.name_for(buffer_id).and_then(|name| {
                let s = name.strip_prefix("*magit:log:")?;
                let s = s.strip_suffix('*')?;
                (!s.is_empty()).then(|| std::path::PathBuf::from(s))
            });

            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.13: publish BEFORE the first `.await` — see the note
            // in `magit_branch_mode::on_activate`.
            let Some(states) = ctx.service::<LogStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                LogState {
                    buffer_id,
                    path: path.clone(),
                    store: store.clone(),
                    workdir: workdir.clone(),
                    pending_highlights: pending_highlights.clone(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(LogView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            // Populate log: blocking I/O on spawn_blocking, then apply
            // edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let path_for_task = path.clone();
            let text = tokio::task::spawn_blocking(move || run_log(&wd, path_for_task.as_deref()))
                .await
                .unwrap();
            let spans = crate::highlight::log_styled_spans(&text);
            apply_full_replace(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

async fn apply_full_replace(handle: &Arc<dyn lattice_runtime::Document>, text: String) {
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await;
}

fn refresh(s: Arc<Mutex<LogState>>) -> Option<Effect> {
    let (handle, wd, path, pending, buffer_id) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.path.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };
    tokio::task::spawn(async move {
        let text = tokio::task::spawn_blocking(move || run_log(&wd, path.as_deref()))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::log_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// `git log --oneline --graph --decorate` renders each commit as
/// `[graph chars] <sha> <subject>` — the sha is the first
/// hex-looking whitespace-delimited token on the line, wherever the
/// graph drawing characters end. Returns `None` for graph-only lines
/// (merge/branch connectors with no commit).
fn extract_sha(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|tok| tok.len() >= 4 && tok.chars().all(|c| c.is_ascii_hexdigit()))
}

fn run_log(workdir: &std::path::Path, path: Option<&std::path::Path>) -> String {
    let mut args = vec![
        "log".to_string(),
        "--oneline".to_string(),
        "--graph".to_string(),
        "--decorate".to_string(),
        "-50".to_string(),
    ];
    if let Some(p) = path {
        args.push("--".to_string());
        args.push(p.to_string_lossy().into_owned());
    }
    let text = std::process::Command::new("git")
        .args(&args)
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "Not a git repository.\n".to_string());
    if text.trim().is_empty() {
        // A path-scoped log with no commits is a real, common
        // outcome (an untracked or brand-new file) — say so rather
        // than leaving a blank buffer that reads like a failure.
        match path {
            Some(p) => format!("No commits touching {}.\n", p.display()),
            None => "No commits yet.\n".to_string(),
        }
    } else {
        text
    }
}

/// `gr` for this view — `magit-core-mode` owns the chord and the one
/// boot-registered handler; see [`MagitView`].
struct LogView(Arc<Mutex<LogState>>);

impl MagitView for LogView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }
}
