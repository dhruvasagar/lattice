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
use lattice_protocol::position::Position;

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline::{self, MagitHeaderlineHandle};

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
    /// MG.14: the buffer's headerline. Re-set on every refresh from
    /// the same `run_log` output that produced the text.
    headerline: Option<MagitHeaderlineHandle>,
    /// MG.23k: extra git arguments the `D` menu set, replayed on every
    /// refresh so `gr` does not silently revert to the default log.
    extra_args: Vec<String>,
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
            let workdir = crate::workdir::magit_workdir().unwrap_or_default();

            // "*magit:log:<path>*" scopes the history to one file
            // (mirrors magit-blame/magit-diff's file-in-buffer-name
            // pattern); bare "*magit:log*" stays repo-wide.
            let path: Option<std::path::PathBuf> = store.name_for(buffer_id).and_then(|name| {
                let s = name.strip_prefix("*magit:log:")?;
                let s = s.strip_suffix('*')?;
                (!s.is_empty()).then(|| std::path::PathBuf::from(s))
            });

            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.14: install the headerline in the same synchronous
            // prefix as the state publish. It renders nothing until
            // the log lands below; installing here means a reopened
            // buffer never inherits the previous activation's row.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

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
                    headerline: hl.clone(),
                    extra_args: Vec::new(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(LogView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            // Populate log: blocking I/O on spawn_blocking, then apply
            // edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let path_for_task = path.clone();
            let text =
                tokio::task::spawn_blocking(move || run_log(&wd, path_for_task.as_deref(), &[]))
                    .await
                    .unwrap();
            headerline::publish(&hl, log_header_fields(&text, path.as_deref()));
            let spans = crate::highlight::log_styled_spans(&text);
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

fn refresh(s: Arc<Mutex<LogState>>) -> Option<Effect> {
    let (handle, wd, path, pending, buffer_id, hl, extra) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.path.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
            g.headerline.clone(),
            g.extra_args.clone(),
        )
    };
    tokio::task::spawn(async move {
        let for_task = path.clone();
        let text = tokio::task::spawn_blocking(move || run_log(&wd, for_task.as_deref(), &extra))
            .await
            .unwrap_or_default();
        headerline::publish(&hl, log_header_fields(&text, path.as_deref()));
        let spans = crate::highlight::log_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// MG.14: the header for this view — the ref logged, how many
/// commits are shown, and the path filter when file-scoped. The
/// count comes from the text just produced rather than a second
/// `git rev-list`, so the header costs no extra git round-trip.
/// `--graph` connector rows carry no SHA and are not counted.
fn log_header_fields(text: &str, path: Option<&std::path::Path>) -> Vec<crate::headerline::Field> {
    let commits = text.lines().filter(|l| extract_sha(l).is_some()).count();
    crate::headerline::log_fields("HEAD", commits, path)
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

/// MG.23k: the arguments `D` offers in a log buffer.
///
/// magit's own `L` transient set, minus what this view already does by
/// default (`--graph`, `--decorate`, `--oneline` are always on — a
/// toggle for something already enabled does nothing visible, which is
/// the inert-row failure in a different costume).
pub(crate) const LOG_ARGS: &[crate::magit_global_mode::RemoteFlag] = &[
    crate::magit_global_mode::RemoteFlag {
        name: "all",
        arg: "--all",
        key: "-a",
        doc: "Every ref, not just the current branch",
        kind: crate::magit_global_mode::RemoteArgKind::Flag,
    },
    crate::magit_global_mode::RemoteFlag {
        name: "count",
        arg: "-n",
        key: "-n",
        doc: "How many commits to show (default 50)",
        // Separated is correct here — `git log -n 200` is valid, and
        // this is exactly why the joined/separated distinction is per
        // argument rather than global.
        kind: crate::magit_global_mode::RemoteArgKind::Value { prompt: "Commits" },
    },
    crate::magit_global_mode::RemoteFlag {
        name: "author",
        arg: "--author",
        key: "-A",
        doc: "Only commits by an author matching this pattern",
        kind: crate::magit_global_mode::RemoteArgKind::Value { prompt: "Author" },
    },
];

fn run_log(workdir: &std::path::Path, path: Option<&std::path::Path>, extra: &[String]) -> String {
    let mut args = vec![
        "log".to_string(),
        "--oneline".to_string(),
        "--graph".to_string(),
        "--decorate".to_string(),
    ];
    // The default count is only applied when the `D` menu did not set
    // one. Appending both and letting git take the last would work,
    // but it puts two contradictory `-n`s in the argv — and the next
    // person to read it cannot tell which wins without knowing git.
    if !extra.iter().any(|a| a == "-n") {
        args.push("-50".to_string());
    }
    args.extend(extra.iter().cloned());
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

    /// MG.23k: `D`'s rows for a log buffer. Magit puts these on `L`;
    /// see `MagitView::argument_flags` for why one chord serves both.
    fn argument_flags(&self) -> &'static [crate::magit_global_mode::RemoteFlag] {
        LOG_ARGS
    }

    fn refresh_with_args(&self, extra: Vec<String>) -> Option<Effect> {
        if let Ok(mut g) = self.0.lock() {
            g.extra_args = extra;
        }
        refresh(self.0.clone())
    }

    /// MG.20: the commit on the row under the cursor. `--graph`
    /// connector rows carry no sha and correctly yield `None`, so
    /// pressing `V` on one does nothing rather than acting on a
    /// neighbouring commit.
    fn commit_at_cursor(&self, cursor: Position) -> Option<String> {
        let g = self.0.lock().ok()?;
        let handle = g.store.handle_for(g.buffer_id)?;
        let snap = handle.snapshot();
        let line = snap.buffer.line(cursor.line)?;
        extract_sha(&line).map(|s| s.to_string())
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        Some(self.0.lock().ok()?.workdir.clone())
    }
}
