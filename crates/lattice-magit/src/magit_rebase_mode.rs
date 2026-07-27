//! MG.9: magit-rebase major mode.
//!
//! Editable interactive rebase todo buffer. C-c C-c runs rebase,
//! C-c C-k aborts.
//!
//! Fold audit fix: this used to populate the buffer with a
//! hardcoded fake todo and, on `C-c C-c`, write it straight to
//! `.git/rebase-merge/git-rebase-todo` and run `git rebase
//! --continue` — against a rebase that had never actually been
//! started, which always failed silently. The real flow: build the
//! todo from `git log` against a real upstream, and on `C-c C-c`
//! actually START the interactive rebase, injecting the buffer's
//! (possibly user-edited) todo via the standard
//! `GIT_SEQUENCE_EDITOR` trick — `git rebase -i` invokes the
//! sequence editor as `<editor> <path-to-generated-todo>`, so
//! setting it to `cp <our-file>` replaces git's todo with ours in
//! one step. `GIT_EDITOR=true` avoids hanging on a `reword` step's
//! commit-message prompt (accepts the original message unchanged —
//! there's no message-editing UI wired up here yet, a known
//! limitation, not a silent failure: the commit simply keeps its
//! original message).

use std::path::Path;
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
use lattice_vcs::Repository;

use crate::magit_core_mode::ActionRegsGuard;

pub struct MagitRebaseMode;

impl MagitRebaseMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-rebase-mode")
    }
}

fn magit_rebase_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<C-c><C-c>", doc: "Execute rebase", cmd: "action:magit-rebase-confirm" },
            keymap_entry! { mode: Insert, chord: "<C-c><C-k>", doc: "Abort rebase", cmd: "action:magit-rebase-abort" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-c>", doc: "Execute rebase", cmd: "action:magit-rebase-confirm" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-k>", doc: "Abort rebase", cmd: "action:magit-rebase-abort" },
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show commit detail at cursor", cmd: "action:magit-rebase-show-commit" },
        ]
    })
}

struct RebaseState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    upstream: String,
}

impl Mode for MagitRebaseMode {
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
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_rebase_keymap_entries())
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

            // Extract the upstream from the buffer name:
            // "*magit:rebase:<upstream>*" → "<upstream>", mirroring
            // magit-blame's file-in-buffer-name pattern. No explicit
            // arg (bare "*magit:rebase*") falls back to resolving
            // `@{upstream}`.
            let explicit_upstream = store.name_for(buffer_id).and_then(|name| {
                let s = name.strip_prefix("*magit:rebase:")?;
                let s = s.strip_suffix("*")?;
                (!s.is_empty()).then(|| s.to_string())
            });

            let wd = workdir.clone();
            let (upstream, initial) = tokio::task::spawn_blocking(move || {
                build_rebase_buffer(&wd, explicit_upstream.as_deref())
            })
            .await
            .unwrap_or_else(|_| (String::new(), "Failed to prepare rebase.\n".to_string()));

            let spans = crate::highlight::rebase_styled_spans(&initial);
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

            let state = Arc::new(Mutex::new(RebaseState {
                buffer_id,
                store: store.clone(),
                workdir,
                upstream,
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

            // confirm (C-c C-c)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-rebase-confirm") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            let (todo, workdir, upstream) = {
                                let g = s.lock().ok()?;
                                if g.upstream.is_empty() {
                                    return None;
                                }
                                let handle = g.store.handle_for(g.buffer_id)?;
                                let snap = handle.snapshot();
                                let mut todo = String::new();
                                for l in 0..snap.buffer.line_count() as u32 {
                                    let text = snap.buffer.line(l).unwrap_or_default();
                                    if text.starts_with('#') || text.trim().is_empty() {
                                        continue;
                                    }
                                    todo.push_str(&text);
                                    todo.push('\n');
                                }
                                (todo, g.workdir.clone(), g.upstream.clone())
                            };
                            if todo.trim().is_empty() {
                                return None;
                            }
                            // Bounded, single-shot git invocation, off
                            // the actor thread — same optimistic-close
                            // shape as magit-commit's confirm.
                            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                                if let Err(e) = run_rebase(&workdir, &upstream, &todo) {
                                    tracing::error!(target: "lattice_magit", "rebase failed: {e}");
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

            // abort (C-c C-k)
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-rebase-abort") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |_ctx: &ActionContext<'_>| {
                            let workdir = { s.lock().ok()?.workdir.clone() };
                            // No rebase has necessarily started yet
                            // (that only happens on confirm) — only
                            // run `--abort` if `.git/rebase-merge` (or
                            // the legacy `-apply` dir) says one is
                            // actually in progress, so this can't fail
                            // on a rebase that was never begun.
                            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                                if let Ok(repo) = Repository::discover(&workdir) {
                                    let gitdir = repo.gitdir();
                                    if gitdir.join("rebase-merge").exists()
                                        || gitdir.join("rebase-apply").exists()
                                    {
                                        let _ = repo.run_git(["rebase", "--abort"]);
                                    }
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

            // <CR> — show commit detail for the todo line at cursor,
            // matching magit-log/magit-blame's convention (see the
            // magit-status `action:magit-visit` fix: SHA-`<CR>` opens
            // the dedicated commit buffer everywhere a per-row SHA is
            // shown).
            {
                let s = state.clone();
                if let Some(cid) = registry.id_by_name("action:magit-rebase-show-commit") {
                    regs.push(handlers.register(
                        cid,
                        Arc::new(move |ctx: &ActionContext<'_>| {
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
                    ));
                }
            }

            Ok(ActionRegsGuard(regs))
        })
    }
}

/// A rebase-todo line is `<verb> <sha> <subject>` (or a `#`-comment) —
/// the sha is the first hex-looking whitespace-delimited token,
/// mirroring `magit_log_mode::extract_sha`'s same "first hex token"
/// scan (duplicated rather than shared: each mode's line format
/// differs enough that a shared parser would need its own
/// verb/graph-char skip logic anyway).
fn extract_sha(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|tok| tok.len() >= 4 && tok.chars().all(|c| c.is_ascii_hexdigit()))
}

/// Resolve the upstream (explicit arg, or `@{upstream}`) and build the
/// todo-buffer text. Returns `(upstream, buffer_text)`; `upstream` is
/// empty when resolution failed — `buffer_text` explains why, and the
/// confirm handler refuses to run against an empty upstream.
fn build_rebase_buffer(workdir: &Path, explicit_upstream: Option<&str>) -> (String, String) {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return (String::new(), "Not a git repository.\n".to_string()),
    };
    let upstream = match explicit_upstream {
        Some(u) => u.to_string(),
        None => match repo.run_git_str(["rev-parse", "--abbrev-ref", "@{upstream}"]) {
            Ok(s) => s.trim().to_string(),
            Err(_) => {
                return (
                    String::new(),
                    "No upstream configured for this branch.\n\
                     Use `:magit-rebase <ref>` to rebase onto a specific ref.\n"
                        .to_string(),
                );
            }
        },
    };
    let log = repo
        .run_git_str([
            "log",
            "--reverse",
            "--format=pick %h %s",
            &format!("{upstream}..HEAD"),
        ])
        .unwrap_or_default();
    if log.trim().is_empty() {
        return (
            String::new(),
            format!("Nothing to rebase — already up to date with {upstream}.\n"),
        );
    }
    let text = format!(
        "{log}\n\
         # Rebase onto {upstream} — edit the list above, then C-c C-c to run,\n\
         # or C-c C-k to abort.\n\
         # Commands: pick, reword, edit, squash, fixup, drop\n\
         # (reword keeps the original message — no message-edit UI yet)\n"
    );
    (upstream, text)
}

fn run_rebase(workdir: &Path, upstream: &str, todo: &str) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!(
        "lattice-rebase-todo-{}-{}",
        std::process::id(),
        upstream.replace(['/', ' '], "_")
    ));
    std::fs::write(&tmp, todo).map_err(|e| e.to_string())?;
    let editor_cmd = format!("cp {}", tmp.display());
    let result = std::process::Command::new("git")
        .args(["rebase", "-i", upstream])
        .env("GIT_SEQUENCE_EDITOR", &editor_cmd)
        .env("GIT_EDITOR", "true")
        .current_dir(workdir)
        .output();
    let _ = std::fs::remove_file(&tmp);
    match result {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}
