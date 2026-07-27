//! MG.1: magit-global universal minor mode — entry-point chords.
//!
//! Activates on every buffer (Universal policy) so `C-x g`,
//! `C-c g`, and `C-c f` work from any buffer kind — document,
//! help, file tree, oil, terminal, etc.
//!
//! Fold audit fix (MG.8): also contributes the `action:magit-global-*`
//! handlers the root dispatch transient's items fire. These exist
//! precisely because `TransientItemKind::Action` dispatch resolves
//! through `ActionHandlerRegistry::lookup` only — never through the
//! ex-command path — so an item that should "open the log buffer
//! from wherever the user happens to be" can't just target the
//! `magit-log` ex-command's `CommandId`; nothing in
//! `ActionHandlerRegistry` answers for it. Every OTHER
//! `action:magit-*` handler in this crate is registered per-buffer
//! from `on_activate` (only live while its owning magit buffer is
//! the one that activated it) — these are global instead, via
//! [`Mode::action_handlers`], because this mode's
//! `ActivationPolicy::Universal` means they're needed everywhen,
//! matching what a global dispatch menu needs.
//!
//! **Bug fix history:** an earlier version registered these from
//! `on_activate` itself, gated by a `OnceLock` so the
//! process-lifetime handlers were only installed once despite
//! `Universal` re-running `on_activate` on every buffer. That was
//! fundamentally racy: `Mode::on_activate`'s returned future runs
//! through a "try-sync-then-spawn" cascade
//! (`lattice_mode::registry::ModeRegistry::spawn_cascade`) shared
//! with every OTHER mode admitted by the same activation batch — if
//! any mode ordered earlier in that batch has real async work, the
//! WHOLE batch (including this mode's own, otherwise-synchronous,
//! step) defers to a background task with no guarantee it completes
//! before the user's next keystroke. Symptom: `C-c g`/`C-c f` opened
//! the transient fine (its `CommandId`s resolve independently, from
//! `CommandRegistry` at `install()` time), but every item's key just
//! dismissed the menu and did nothing — `ActionHandlerRegistry::lookup`
//! returned `None` because the handler registration hadn't run yet,
//! or (with a separate now-fixed bug where the guard latched on the
//! FIRST ATTEMPT regardless of success) had permanently failed to
//! run at all. [`Mode::action_handlers`] sidesteps the whole hazard:
//! the host's `register_mode_action_handlers` walks every mode's
//! contributed list in a plain synchronous `for` loop at boot,
//! strictly after the command registry is frozen — no cascade, no
//! `Universal`-activation timing dependency, no per-buffer state
//! needed (these handlers close over nothing but read `ActionContext`
//! at call time), so no on-activate registration is needed at all.

use std::sync::{Arc, OnceLock};

use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, ActivationPolicy, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_vcs::Repository;

pub struct MagitGlobalMode;

impl MagitGlobalMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-global-mode")
    }
}

fn magit_global_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "<C-x>g",
                doc: "Open magit-status for the current repo",
               cmd: "magit-status"
            },
            keymap_entry! {
                mode: Normal, chord: "<C-c>g",
                doc: "Open magit dispatch transient (repo-level)",
                cmd: "magit-dispatch"
            },
            keymap_entry! {
                mode: Normal, chord: "<C-c>f",
                doc: "Open magit file-dispatch transient",
                cmd: "magit-file-dispatch"
            },
        ]
    })
}

impl Mode for MagitGlobalMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        // Universal: activate on every buffer kind so the entry
        // chords work from help, file tree, oil, terminal, etc.
        ActivationPolicy::Universal
    }

    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::new()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_global_keymap_entries())
    }

    /// See the module doc for why these are contributed here (a
    /// plain, synchronous, boot-time list) rather than registered
    /// from `on_activate`.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        global_action_handler_contributions()
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async { Ok(()) })
    }
}

/// The `action:magit-global-*` handler contributions the root
/// dispatch transient's items (and the branch-create wizard's
/// prompt) fire — each just directly builds the same
/// `Effect::OpenSyntheticBuffer` its equivalent ex-command returns
/// (open-status/-commit/-log/-branch/-stash/-rebase), a real remote
/// git operation (pull/push), a real file-scoped stage/diff, or the
/// branch-create wizard's finish step. The host resolves
/// `action_name` -> `CommandId` and performs the actual
/// `ActionHandlerRegistry::register` call; this function only builds
/// the declarative list. See [`Mode::action_handlers`]'s doc comment
/// for why closing over no per-buffer state is what makes this safe.
fn global_action_handler_contributions() -> Vec<ActionHandlerContribution> {
    let mut contributions = Vec::new();

    macro_rules! open {
        ($action_name:expr, $buffer_name:expr, $mode_id:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(Effect::OpenSyntheticBuffer {
                        name: $buffer_name.to_string(),
                        mode_id: $mode_id.to_string(),
                    })
                }),
            });
        };
    }

    open!(
        "action:magit-global-status",
        "*magit:status*",
        "magit-status-mode"
    );
    open!(
        "action:magit-global-commit",
        "*magit:commit*",
        "magit-commit-mode"
    );
    open!("action:magit-global-log", "*magit:log*", "magit-log-mode");
    open!(
        "action:magit-global-branch",
        "*magit:branch*",
        "magit-branch-mode"
    );
    open!(
        "action:magit-global-stash",
        "*magit:stash*",
        "magit-stash-mode"
    );
    open!(
        "action:magit-global-rebase",
        "*magit:rebase*",
        "magit-rebase-mode"
    );
    open!(
        "action:magit-global-amend",
        "*magit:amend*",
        "magit-commit-mode"
    );
    open!(
        "action:magit-global-diff",
        "*magit:diff*",
        "magit-diff-mode"
    );

    // pull/push — real git operations, run off the actor thread.
    // `GIT_TERMINAL_PROMPT=0` makes a missing/expired credential fail
    // fast and cleanly (git errors out immediately) instead of
    // hanging the background task waiting for interactive input that
    // can never arrive. Optimistic `Effect::Echo` returns
    // synchronously; the real outcome lands via `tracing`, same as
    // every other detached background mutation in this crate — no
    // synchronous path exists back to the echo area from a task that
    // outlives the handler call, so success/failure is logged rather
    // than silently dropped (never both silent AND absent).
    macro_rules! remote_op {
        ($action_name:expr, $what:expr, $git_args:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    let workdir = Repository::discover(".")
                        .ok()
                        .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                        .unwrap_or_default();
                    tokio::task::spawn(async move {
                        let result = tokio::task::spawn_blocking(move || {
                            run_remote_op(&workdir, $git_args)
                        })
                        .await
                        .unwrap_or_else(|e| Err(e.to_string()));
                        match result {
                            Ok(out) => tracing::info!(
                                target: "lattice_magit",
                                "magit: {} succeeded: {out}", $what
                            ),
                            Err(err) => tracing::error!(
                                target: "lattice_magit",
                                "magit: {} failed: {err}", $what
                            ),
                        }
                    });
                    Some(Effect::Echo {
                        level: lattice_grammar::EchoLevel::Info,
                        text: concat!("magit: ", $what, "ing…").to_string(),
                    })
                }),
            });
        };
    }
    remote_op!(
        "action:magit-global-pull",
        "pull",
        &["pull", "--ff-only"][..]
    );
    remote_op!("action:magit-global-push", "push", &["push"][..]);
    // Fetch is the non-merging half of pull — magit gives it its own
    // top-level key (`f`) precisely because "see what's upstream
    // without touching my tree" is a distinct, frequent intent.
    remote_op!("action:magit-global-fetch", "fetch", &["fetch"][..]);
    // Stash-push is local, not remote, but `run_remote_op`'s
    // fail-fast + log-the-outcome shape fits any one-shot git
    // invocation whose result can't come back synchronously.
    remote_op!(
        "action:magit-global-stash-create",
        "stash",
        &["stash", "push"][..]
    );

    // file-dispatch (`C-c f`) — every item acts on the file in
    // whatever buffer was active when the transient was opened,
    // resolved through `active_file` below.

    /// Open a file-scoped magit buffer named `<prefix><rel-path>*`
    /// in `mode_id` — the shape `C-c f`'s diff/log/blame items share.
    macro_rules! file_open {
        ($action_name:expr, $prefix:expr, $mode_id:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let (_workdir, rel) = active_file(ctx)?;
                    Some(Effect::OpenSyntheticBuffer {
                        name: format!(concat!($prefix, "{}*"), rel.display()),
                        mode_id: $mode_id.to_string(),
                    })
                }),
            });
        };
    }

    /// Run a blocking git mutation against the active file, off the
    /// actor thread, echoing optimistically — the same detached
    /// shape `remote_op!` uses (no synchronous path back to the echo
    /// area from a task that outlives the handler call).
    macro_rules! file_mutate {
        ($action_name:expr, $past_tense:expr, $body:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let (workdir, rel) = active_file(ctx)?;
                    let shown = rel.clone();
                    tokio::task::spawn(async move {
                        let _ = tokio::task::spawn_blocking(move || {
                            if let Ok(repo) = Repository::discover(&workdir) {
                                #[allow(clippy::redundant_closure_call)]
                                ($body)(&repo, &rel);
                            }
                        })
                        .await;
                    });
                    Some(Effect::Echo {
                        level: lattice_grammar::EchoLevel::Info,
                        text: format!(concat!("magit: ", $past_tense, " {}"), shown.display()),
                    })
                }),
            });
        };
    }

    file_mutate!(
        "action:magit-global-file-stage",
        "staged",
        |repo: &Repository, rel: &std::path::Path| {
            let _ = lattice_vcs::Index::stage_path(repo, rel);
        }
    );
    file_mutate!(
        "action:magit-global-file-unstage",
        "unstaged",
        |repo: &Repository, rel: &std::path::Path| {
            let _ = lattice_vcs::Index::unstage_path(repo, rel);
        }
    );
    // Discard is destructive, so it asks first — same `Effect::Confirm`
    // → `<action>-execute` two-step magit-status's own `x` uses.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-discard",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let (_workdir, rel) = active_file(ctx)?;
            Some(Effect::Confirm {
                prompt: format!("Discard changes to {}?", rel.display()),
                yes_action: "action:magit-global-file-discard-execute".to_string(),
            })
        }),
    });
    file_mutate!(
        "action:magit-global-file-discard-execute",
        "discarded changes to",
        |repo: &Repository, rel: &std::path::Path| {
            let _ = repo.run_git(["checkout", "--", &rel.to_string_lossy()]);
        }
    );

    file_open!(
        "action:magit-global-file-diff",
        "*magit:diff:",
        "magit-diff-mode"
    );
    file_open!(
        "action:magit-global-file-log",
        "*magit:log:",
        "magit-log-mode"
    );
    file_open!(
        "action:magit-global-file-blame",
        "*magit:blame:",
        "magit-blame-mode"
    );

    // Branch-create wizard's second step — fired by the prompt
    // opened after `magit-branch-pick-base`'s accept. `ctx.buffer_id`
    // is the PROMPT buffer (see `Editor::do_prompt_line_submit`'s
    // doc comment); its synthetic name carries the picked base
    // branch, exactly like magit's blame/rebase/revision modes
    // encode their target in the buffer name.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-branch-create-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let name = ctx.prompt_value?.trim().to_string();
            if name.is_empty() {
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: branch name is empty".to_string(),
                });
            }
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let base = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| base_branch_from_prompt_buffer_name(&n))?;
            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                let Ok(repo) = Repository::discover(".") else {
                    tracing::error!(target: "lattice_magit", "branch create: repo discover failed");
                    return;
                };
                if let Err(e) = lattice_vcs::Branch::create(&repo, &name, true, Some(&base)) {
                    tracing::error!(target: "lattice_magit", "branch create {name} from {base}: {e}");
                }
            }));
            Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: "magit: creating branch…".to_string(),
            })
        }),
    });

    contributions
}

/// Resolve the active buffer's file to `(repo-workdir,
/// repo-relative-path)` — every `C-c f` item's first step. `None`
/// when the active buffer has no backing file (a synthetic magit
/// buffer, `*messages*`, a scratch buffer) or isn't inside a git
/// repository; the item then silently does nothing, which is the
/// right outcome for "stage the file" with no file to stage.
fn active_file(ctx: &ActionContext<'_>) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
    let path = ctx
        .services
        .get::<BufferStoreHandle>()?
        .path_for(buffer_id)?;
    // `gix::discover` requires a directory — `path` is the file
    // itself, so start from its parent.
    let repo = Repository::discover(path.parent()?).ok()?;
    let workdir = repo.workdir()?.to_path_buf();
    let rel = path.strip_prefix(&workdir).unwrap_or(&path).to_path_buf();
    Some((workdir, rel))
}

/// Parse the base branch name stashed in a branch-create prompt
/// buffer's synthetic name (`*magit:branch-create-from:<base>*`).
/// `None` for any other buffer name — the empty-base case
/// (`*magit:branch-create-from:*`) is also rejected since an empty
/// base is never a valid ref.
fn base_branch_from_prompt_buffer_name(buffer_name: &str) -> Option<String> {
    let s = buffer_name.strip_prefix("*magit:branch-create-from:")?;
    let s = s.strip_suffix("*")?;
    (!s.is_empty()).then(|| s.to_string())
}

/// Run a git subcommand for a remote operation (pull/push), with
/// `GIT_TERMINAL_PROMPT=0` so a missing credential fails fast instead
/// of hanging. `git push`'s human-readable progress goes to stderr
/// even on success, so both streams are checked.
fn run_remote_op(workdir: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("git")
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .current_dir(workdir)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let out = String::from_utf8_lossy(&o.stdout);
            let err = String::from_utf8_lossy(&o.stderr);
            let combined = format!("{out}{err}");
            Ok(combined.trim().to_string())
        }
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_branch_from_prompt_buffer_name_extracts_the_stashed_base() {
        assert_eq!(
            base_branch_from_prompt_buffer_name("*magit:branch-create-from:feature/foo*"),
            Some("feature/foo".to_string())
        );
    }

    #[test]
    fn base_branch_from_prompt_buffer_name_rejects_an_empty_base() {
        assert_eq!(
            base_branch_from_prompt_buffer_name("*magit:branch-create-from:*"),
            None
        );
    }

    #[test]
    fn base_branch_from_prompt_buffer_name_rejects_unrelated_names() {
        assert_eq!(base_branch_from_prompt_buffer_name("*magit:status*"), None);
        assert_eq!(base_branch_from_prompt_buffer_name("*prompt*"), None);
    }
}
