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
use lattice_grammar::{Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

use crate::buffer_state::{BufferStateGuard, BufferStates};
use crate::headerline;

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

pub struct RebaseState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    upstream: String,
    /// Resolved once at activation so the abort handler can decide
    /// *synchronously* whether a rebase is in progress without walking
    /// the filesystem to find the repo first (MG.12 — the confirm has
    /// to be part of the effect the chord returns, so the check cannot
    /// be deferred to `spawn_blocking` the way the abort itself is).
    gitdir: std::path::PathBuf,
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type RebaseStatesHandle = Arc<BufferStates<RebaseState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<RebaseState>>> {
    crate::buffer_state::state_for::<RebaseState>(ctx)
}

impl Mode for MagitRebaseMode {
    type Guard = BufferStateGuard<RebaseState>;

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

    /// MG.13: boot-registered — see `buffer_state`'s module docs.
    ///
    /// `upstream` is the field this mode cannot resolve before its
    /// `.await`. It is published empty, and `confirm` already refuses
    /// to run against an empty upstream — so a `C-c C-c` in that window
    /// correctly does nothing rather than rebasing onto an unresolved
    /// ref.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // confirm (C-c C-c)
            ActionHandlerContribution {
                action_name: "action:magit-rebase-confirm",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
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
                    // Bounded, single-shot git invocation, off the actor
                    // thread — same optimistic-close shape as
                    // magit-commit's confirm.
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
            },
            // abort (C-c C-k) — MG.12. No rebase has necessarily
            // started yet (that only happens on confirm), and the two
            // cases deserve different answers:
            //
            //   nothing in progress → `C-c C-k` just closes a todo
            //     buffer nobody ran. Asking there would be pure noise,
            //     so it closes the pane outright.
            //   rebase in progress  → `--abort` throws away everything
            //     the rebase has replayed so far, which is the same
            //     class of act as discard / branch-delete, so it asks.
            //
            // The in-progress check is a single `stat` against the
            // gitdir resolved at activation — cheap enough to run on
            // the actor thread in response to an explicit chord, and it
            // *has* to run here because the confirm is the effect this
            // handler returns.
            ActionHandlerContribution {
                action_name: "action:magit-rebase-abort",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let gitdir = { s.lock().ok()?.gitdir.clone() };
                    if rebase_in_progress(&gitdir) {
                        Some(abort_rebase_confirm())
                    } else {
                        Some(Effect::QuitEditor {
                            force: false,
                            scope: QuitScope::Pane,
                        })
                    }
                }),
            },
            // abort, after confirmation.
            ActionHandlerContribution {
                action_name: "action:magit-rebase-abort-execute",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let workdir = { s.lock().ok()?.workdir.clone() };
                    tokio::task::spawn(tokio::task::spawn_blocking(move || {
                        if let Ok(repo) = Repository::discover(&workdir)
                            && rebase_in_progress(repo.gitdir())
                        {
                            let _ = repo.run_git(["rebase", "--abort"]);
                        }
                    }));
                    Some(Effect::QuitEditor {
                        force: false,
                        scope: QuitScope::Pane,
                    })
                }),
            },
            // <CR> — show commit detail for the todo line at cursor,
            // matching magit-log/magit-blame's convention.
            ActionHandlerContribution {
                action_name: "action:magit-rebase-show-commit",
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

            let discovered = Repository::discover(".").ok();
            let workdir = discovered
                .as_ref()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            let gitdir = discovered
                .as_ref()
                .map(|r| r.gitdir().to_path_buf())
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

            // MG.14: the upstream is resolved below (it may come from
            // `@{upstream}` rather than the buffer name), so the header
            // fills in with the todo text.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            let rebase_running = rebase_in_progress(&gitdir);

            // MG.13: publish BEFORE the first `.await`. `upstream` is
            // not resolvable yet; it starts empty, and `confirm`
            // already refuses on an empty upstream.
            let Some(states) = ctx.service::<RebaseStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                RebaseState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    upstream: String::new(),
                    gitdir,
                },
            );
            let guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);

            let wd = workdir.clone();
            let (upstream, initial) = tokio::task::spawn_blocking(move || {
                build_rebase_buffer(&wd, explicit_upstream.as_deref())
            })
            .await
            .unwrap_or_else(|_| (String::new(), "Failed to prepare rebase.\n".to_string()));

            // Counted from the text just built, so no second
            // `rev-list`. Keyed on the leading verb rather than "has a
            // hex-looking token": the explanatory `#` footer is prose,
            // and an ordinary English word made only of `abcdef`
            // ("added", "faced") would otherwise count as a commit.
            let commits = initial.lines().filter(|l| is_todo_line(l)).count();
            headerline::publish(
                &hl,
                headerline::rebase_fields(&upstream, commits, rebase_running),
            );
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

            // Late-resolved field, now that the upstream is known.
            if let Ok(mut g) = state.lock() {
                g.upstream = upstream;
            }

            Ok(guard)
        })
    }
}

/// Is a rebase actually mid-flight? `git` records one as a
/// `rebase-merge` directory in the gitdir (`rebase-apply` for the
/// legacy `--apply` backend and for `git am`). Both are checked
/// because either means `--abort` has work to throw away.
fn rebase_in_progress(gitdir: &Path) -> bool {
    gitdir.join("rebase-merge").exists() || gitdir.join("rebase-apply").exists()
}

/// MG.12: the ask half of `C-c C-k`, reached only when a rebase is
/// genuinely in progress.
fn abort_rebase_confirm() -> Effect {
    crate::confirm::ask(
        "Abort this rebase?".to_string(),
        "action:magit-rebase-abort-execute",
    )
}

/// The verbs a rebase-todo line may lead with. Shared by the commit
/// counter below and mirrored by `highlight::rebase_styled_spans`,
/// which colours the same set.
const TODO_VERBS: [&str; 6] = ["pick", "reword", "edit", "squash", "fixup", "drop"];

/// MG.14: is this todo line a real commit row? `<verb> <sha> ...` —
/// not a `#` comment and not the trailing blank.
fn is_todo_line(line: &str) -> bool {
    TODO_VERBS
        .iter()
        .any(|v| line.strip_prefix(v).is_some_and(|r| r.starts_with(' ')))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.12: `C-c C-k` on a todo buffer that was never executed is
    /// just "close this buffer" — there is nothing to throw away, so
    /// it must not ask. This is why the confirm is gated rather than
    /// unconditional.
    #[test]
    fn a_gitdir_with_no_rebase_state_is_not_in_progress() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(!rebase_in_progress(dir.path()));
    }

    /// Both backends count: `rebase-merge` is the modern one,
    /// `rebase-apply` the legacy `--apply` / `git am` one. Missing
    /// either would abort real in-flight work without asking.
    #[test]
    fn either_rebase_state_directory_counts_as_in_progress() {
        for marker in ["rebase-merge", "rebase-apply"] {
            let dir = tempfile::tempdir().expect("temp dir");
            std::fs::create_dir(dir.path().join(marker)).expect("create marker dir");
            assert!(
                rebase_in_progress(dir.path()),
                "`{marker}` must count as a rebase in progress"
            );
        }
    }

    #[test]
    fn abort_confirm_points_at_the_execute_action() {
        match abort_rebase_confirm() {
            Effect::Confirm { prompt, yes_action } => {
                assert_eq!(prompt, "Abort this rebase?");
                assert_eq!(yes_action, "action:magit-rebase-abort-execute");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }
}
