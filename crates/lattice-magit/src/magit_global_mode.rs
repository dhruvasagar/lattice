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
    // MG.16: the body lives in [`spawn_remote_op`], NOT in this
    // macro. The transient item and the ex-command are two front-ends
    // over one implementation (the unified-dispatch rule) — a second
    // copy behind `:magit-push` would be a second place for the
    // credential handling, the echo text, and the outcome logging to
    // drift.
    macro_rules! remote_op {
        ($action_name:expr, $op:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|ctx: &ActionContext<'_>| Some(spawn_remote_op($op, &ctx.args))),
            });
        };
    }
    remote_op!("action:magit-global-pull", RemoteOp::PULL);
    remote_op!("action:magit-global-push", RemoteOp::PUSH);
    // Fetch is the non-merging half of pull — magit gives it its own
    // top-level key (`f`) precisely because "see what's upstream
    // without touching my tree" is a distinct, frequent intent.
    remote_op!("action:magit-global-fetch", RemoteOp::FETCH);
    // Stash-push is local, not remote, but `run_remote_op`'s
    // fail-fast + log-the-outcome shape fits any one-shot git
    // invocation whose result can't come back synchronously.
    remote_op!("action:magit-global-stash-create", RemoteOp::STASH);

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

/// MG.17b: what an argument contributes to the command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteArgKind {
    /// A toggle: contributes `arg` when on, nothing when off.
    Flag,
    /// A value: contributes `arg` followed by the typed text (or just
    /// the text when `arg` is empty, for a positional like a remote
    /// name). Contributes nothing when unset.
    Value {
        /// Label shown in the minibuffer while typing.
        prompt: &'static str,
    },
}

/// MG.17a: one argument on a [`RemoteOp`].
///
/// The single definition, read by four consumers that would otherwise
/// drift apart: the ex-command's `args_schema`, the transient menu's
/// item, the live preview string, and the argv builder. Adding
/// `--prune` to fetch means adding one row here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteFlag {
    /// Schema slot + transient-state key (`"force"`).
    pub name: &'static str,
    /// The git argument it contributes (`"--force"`), or `""` for a
    /// bare positional value.
    pub arg: &'static str,
    /// Key that selects it in the transient (`"-f"`).
    pub key: &'static str,
    /// One-line doc, shown in the menu and in `:describe-command`.
    pub doc: &'static str,
    /// MG.17b: toggle or value.
    pub kind: RemoteArgKind,
}

/// MG.16: one detached git operation, named once.
///
/// The transient item (`C-c g p`) and the ex-command (`:magit-pull`)
/// both resolve to one of these constants and call
/// [`spawn_remote_op`] — the operation is defined in exactly one
/// place, so the two surfaces cannot drift in argv, in echo text, or
/// in how the outcome is reported.
///
/// MG.17a: `flags` extends that to the optional arguments. The order
/// of the slice IS the `args_schema` order, which is what lets a
/// transient toggle and a `--force` on the `:` line resolve to the
/// same `Args::List` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteOp {
    /// Verb for the echo + logs, in `-ing` form ("pull" → "pulling…").
    pub what: &'static str,
    /// Base argv passed to `git`, before flags.
    pub args: &'static [&'static str],
    /// Toggleable flags, in `args_schema` order.
    pub flags: &'static [RemoteFlag],
}

impl RemoteOp {
    pub const PULL: Self = Self {
        what: "pull",
        args: &["pull", "--ff-only"],
        flags: &[],
    };
    pub const PUSH: Self = Self {
        what: "push",
        args: &["push"],
        flags: &[
            RemoteFlag {
                name: "force-with-lease",
                // `--force-with-lease`, never bare `--force`: it refuses
                // when the remote moved since you last fetched, which is
                // exactly the case where a bare force silently destroys
                // someone else's commits. Magit defaults to the same.
                arg: "--force-with-lease",
                key: "-f",
                doc: "Force-push, but refuse if the remote moved since your last fetch",
                kind: RemoteArgKind::Flag,
            },
            RemoteFlag {
                name: "set-upstream",
                arg: "--set-upstream",
                key: "-u",
                doc: "Set the pushed branch's upstream to the remote branch",
                kind: RemoteArgKind::Flag,
            },
        ],
    };
    pub const FETCH: Self = Self {
        what: "fetch",
        args: &["fetch"],
        flags: &[
            RemoteFlag {
                name: "all",
                arg: "--all",
                key: "-a",
                doc: "Fetch from every remote, not just the default",
                kind: RemoteArgKind::Flag,
            },
            RemoteFlag {
                name: "prune",
                arg: "--prune",
                key: "-p",
                doc: "Delete local refs whose remote branch is gone",
                kind: RemoteArgKind::Flag,
            },
        ],
    };
    pub const STASH: Self = Self {
        what: "stash",
        args: &["stash", "push"],
        flags: &[
            RemoteFlag {
                name: "include-untracked",
                arg: "--include-untracked",
                key: "-u",
                doc: "Stash untracked files too, not just tracked changes",
                kind: RemoteArgKind::Flag,
            },
            // MG.17b: the first real `Argument` — a stash message. This is
            // the reason to have arguments at all: an unlabelled stash is
            // findable only by position, and positions renumber.
            RemoteFlag {
                name: "message",
                arg: "-m",
                key: "-m",
                doc: "Label the stash so you can recognise it later",
                kind: RemoteArgKind::Value {
                    prompt: "Stash message",
                },
            },
        ],
    };

    /// Resolve the full argv for this run: the base plus every flag the
    /// caller enabled. `args` is positional by `flags` order — see
    /// [`RemoteOp::flags`].
    pub fn argv(&self, args: &lattice_grammar::Args) -> Vec<String> {
        let mut argv: Vec<String> = self.args.iter().map(|s| (*s).to_string()).collect();
        for (i, flag) in self.flags.iter().enumerate() {
            let slot = args.as_list().and_then(|l| l.get(i));
            match flag.kind {
                RemoteArgKind::Flag => {
                    if matches!(slot, Some(lattice_grammar::ArgValue::Bool(true))) {
                        argv.push(flag.arg.to_string());
                    }
                }
                // An unset value contributes nothing at all — not an
                // empty string, which git would read as a real (empty)
                // argument.
                RemoteArgKind::Value { .. } => {
                    if let Some(lattice_grammar::ArgValue::String(v)) = slot
                        && !v.is_empty()
                    {
                        if !flag.arg.is_empty() {
                            argv.push(flag.arg.to_string());
                        }
                        argv.push(v.clone());
                    }
                }
            }
        }
        argv
    }

    /// The command line this run would execute, for the transient's
    /// live preview. Renders what `argv` will actually pass, so the
    /// preview cannot claim one thing and the run do another.
    pub fn preview(&self, value_of: &dyn Fn(&str) -> Option<String>) -> String {
        let mut out = String::from("git");
        for a in self.args {
            out.push(' ');
            out.push_str(a);
        }
        for flag in self.flags {
            let Some(v) = value_of(flag.name) else {
                continue;
            };
            match flag.kind {
                RemoteArgKind::Flag => {
                    if v == "true" {
                        out.push(' ');
                        out.push_str(flag.arg);
                    }
                }
                RemoteArgKind::Value { .. } => {
                    if !v.is_empty() {
                        if !flag.arg.is_empty() {
                            out.push(' ');
                            out.push_str(flag.arg);
                        }
                        // Quoted: a stash message has spaces, and an
                        // unquoted preview would read as several args.
                        out.push_str(&format!(" {v:?}"));
                    }
                }
            }
        }
        out
    }

    /// `args_schema` for this operation's ex-command — one optional
    /// bool per flag, in slice order.
    pub fn arg_specs(&self) -> Vec<lattice_grammar::ArgSpec> {
        self.flags
            .iter()
            .map(|f| {
                let kind = match f.kind {
                    RemoteArgKind::Flag => lattice_grammar::ArgKind::Bool,
                    RemoteArgKind::Value { .. } => lattice_grammar::ArgKind::String,
                };
                lattice_grammar::ArgSpec::optional(f.name, kind, f.doc)
            })
            .collect()
    }
}

/// Run `op` off the actor thread and return the optimistic echo.
///
/// `GIT_TERMINAL_PROMPT=0` (in [`run_remote_op`]) makes a missing or
/// expired credential fail fast and cleanly instead of hanging the
/// background task on interactive input that can never arrive. The
/// echo returns synchronously; the real outcome lands via `tracing`,
/// same as every other detached background mutation in this crate —
/// no synchronous path exists back to the echo area from a task that
/// outlives the call, so success and failure are logged rather than
/// silently dropped (never both silent AND absent).
pub fn spawn_remote_op(op: RemoteOp, args: &lattice_grammar::Args) -> Effect {
    let workdir = Repository::discover(".")
        .ok()
        .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    let argv = op.argv(args);
    let shown = argv.join(" ");
    let logged = shown.clone();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_remote_op(&workdir, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match result {
            Ok(out) => tracing::info!(
                target: "lattice_magit",
                "magit: git {logged} succeeded: {out}"
            ),
            Err(err) => tracing::error!(
                target: "lattice_magit",
                "magit: git {logged} failed: {err}"
            ),
        }
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        // Naming the flags in the echo matters: a force-push and an
        // ordinary push are the same word otherwise, and the echo is
        // the only synchronous feedback this path produces.
        text: format!("magit: {}ing… (git {shown})", op.what),
    }
}

/// MG.20: an operation that acts on ONE commit.
///
/// Reset, revert and cherry-pick share a shape the [`RemoteOp`]s above
/// do not: they need a target — the commit under the cursor — so they
/// cannot be fired from a context-free global handler. The target is
/// resolved through [`crate::buffer_state::MagitView::commit_at_cursor`],
/// which every view answers for its own row format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitOp {
    /// Verb for the echo + logs.
    pub what: &'static str,
    /// argv template; the resolved commit is appended.
    pub args: &'static [&'static str],
    /// When set, the operation asks before running and this is the
    /// `-execute` half it fires on "yes". Reset --hard is the only one
    /// today: it discards working-tree changes irrecoverably, which is
    /// the same bar `x` / branch-delete / stash-drop are held to.
    pub confirm_action: Option<&'static str>,
}

impl CommitOp {
    pub const REVERT: Self = Self {
        what: "revert",
        // `--no-edit` keeps the generated message: lattice has no
        // commit-message UI wired into this path, so opening $EDITOR
        // inside the editor would hang the operation on a prompt the
        // user cannot answer.
        args: &["revert", "--no-edit"],
        confirm_action: None,
    };
    pub const CHERRY_PICK: Self = Self {
        what: "cherry-pick",
        args: &["cherry-pick"],
        confirm_action: None,
    };
    pub const RESET_SOFT: Self = Self {
        what: "reset --soft",
        args: &["reset", "--soft"],
        confirm_action: None,
    };
    pub const RESET_MIXED: Self = Self {
        what: "reset --mixed",
        args: &["reset", "--mixed"],
        confirm_action: None,
    };
    pub const RESET_HARD: Self = Self {
        what: "reset --hard",
        args: &["reset", "--hard"],
        // The only one that destroys uncommitted work.
        confirm_action: Some("action:magit-reset-hard-execute"),
    };

    /// Full argv for `commit`.
    pub fn argv(&self, commit: &str) -> Vec<String> {
        let mut argv: Vec<String> = self.args.iter().map(|s| (*s).to_string()).collect();
        argv.push(commit.to_string());
        argv
    }
}

/// Run a [`CommitOp`] against `commit`, off the actor thread.
///
/// Same detached shape as [`spawn_remote_op`]: optimistic echo now,
/// real outcome through `tracing`, because nothing can carry a result
/// back to an echo area from a task that outlives the handler call.
pub fn spawn_commit_op(op: CommitOp, workdir: std::path::PathBuf, commit: &str) -> Effect {
    let argv = op.argv(commit);
    let shown = argv.join(" ");
    let logged = shown.clone();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_remote_op(&workdir, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match result {
            Ok(out) => tracing::info!(
                target: "lattice_magit", "magit: git {logged} succeeded: {out}"
            ),
            Err(err) => tracing::error!(
                target: "lattice_magit", "magit: git {logged} failed: {err}"
            ),
        }
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        // Name the commit: these operations are indistinguishable from
        // each other in the echo area otherwise, and two of them
        // rewrite history.
        text: format!("magit: git {shown}"),
    }
}

/// Run a git subcommand for a remote operation (pull/push), with
/// `GIT_TERMINAL_PROMPT=0` so a missing credential fails fast instead
/// of hanging. `git push`'s human-readable progress goes to stderr
/// even on success, so both streams are checked.
fn run_remote_op(workdir: &std::path::Path, args: &[String]) -> Result<String, String> {
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

    /// MG.20 — a commit operation appends its target.
    #[test]
    fn a_commit_op_appends_the_commit_to_its_argv() {
        assert_eq!(
            CommitOp::CHERRY_PICK.argv("a1b2c3d"),
            vec!["cherry-pick", "a1b2c3d"]
        );
        assert_eq!(
            CommitOp::RESET_HARD.argv("a1b2c3d"),
            vec!["reset", "--hard", "a1b2c3d"]
        );
    }

    /// Only `--hard` asks. `--soft` and `--mixed` keep your changes —
    /// a prompt on those would be noise that trains you to dismiss the
    /// one that matters.
    #[test]
    fn only_the_destructive_reset_asks_first() {
        assert!(CommitOp::RESET_HARD.confirm_action.is_some());
        assert!(CommitOp::RESET_SOFT.confirm_action.is_none());
        assert!(CommitOp::RESET_MIXED.confirm_action.is_none());
        assert!(CommitOp::REVERT.confirm_action.is_none());
        assert!(CommitOp::CHERRY_PICK.confirm_action.is_none());
    }

    /// Revert passes `--no-edit`. Without it git opens `$EDITOR` for
    /// the message, which inside lattice means the operation hangs on
    /// a prompt the user has no way to answer.
    #[test]
    fn revert_does_not_open_an_editor() {
        assert!(
            CommitOp::REVERT.args.contains(&"--no-edit"),
            "revert must not block on $EDITOR"
        );
    }

    /// Every destructive commit op's confirm target must be a real
    /// registered `-execute` action — `confirm::ask` debug-asserts the
    /// pairing, so a typo here fails loudly for the author instead of
    /// quietly for the user.
    #[test]
    fn the_hard_reset_confirm_targets_its_execute_half() {
        assert_eq!(
            CommitOp::RESET_HARD.confirm_action,
            Some("action:magit-reset-hard-execute")
        );
    }

    /// MG.17a — the flag table drives argv, and only when enabled.
    #[test]
    fn argv_appends_exactly_the_enabled_flags_in_schema_order() {
        use lattice_grammar::{ArgValue, Args};
        let op = RemoteOp::PUSH;
        assert_eq!(op.argv(&Args::None), vec!["push"], "no args = bare push");
        assert_eq!(
            op.argv(&Args::List(vec![
                ArgValue::Bool(false),
                ArgValue::Bool(false)
            ])),
            vec!["push"]
        );
        assert_eq!(
            op.argv(&Args::List(vec![
                ArgValue::Bool(true),
                ArgValue::Bool(false)
            ])),
            vec!["push", "--force-with-lease"]
        );
        assert_eq!(
            op.argv(&Args::List(vec![
                ArgValue::Bool(true),
                ArgValue::Bool(true)
            ])),
            vec!["push", "--force-with-lease", "--set-upstream"]
        );
    }

    /// A bare chord press (no transient, no args) must behave exactly
    /// as it did before flags existed — this is the regression that
    /// would break every existing `C-c g F` muscle memory.
    #[test]
    fn an_argless_invocation_runs_the_unflagged_command() {
        use lattice_grammar::Args;
        for op in [
            RemoteOp::PULL,
            RemoteOp::PUSH,
            RemoteOp::FETCH,
            RemoteOp::STASH,
        ] {
            assert_eq!(
                op.argv(&Args::None),
                op.args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "`{}` with no args must be the bare command",
                op.what
            );
        }
    }

    /// The preview and the run must agree. A preview that renders a
    /// different command than the one that executes is worse than no
    /// preview — it actively misleads before a push.
    #[test]
    fn the_preview_string_matches_the_argv_that_will_run() {
        use lattice_grammar::{ArgValue, Args};
        let op = RemoteOp::FETCH;
        for (all, prune) in [(false, false), (true, false), (false, true), (true, true)] {
            let args = Args::List(vec![ArgValue::Bool(all), ArgValue::Bool(prune)]);
            let preview = op.preview(&|name| match name {
                "all" => Some(all.to_string()),
                "prune" => Some(prune.to_string()),
                _ => None,
            });
            let from_argv = format!("git {}", op.argv(&args).join(" "));
            assert_eq!(preview, from_argv, "preview must render what runs");
        }
    }

    /// Push force-pushes with `--force-with-lease`, never bare
    /// `--force`. Pinned deliberately: the difference is whether a
    /// colleague's commits survive when the remote moved under you.
    #[test]
    fn force_push_uses_force_with_lease() {
        let force = RemoteOp::PUSH.flags[0];
        assert_eq!(force.arg, "--force-with-lease");
        assert!(
            !RemoteOp::PUSH.flags.iter().any(|f| f.arg == "--force"),
            "bare --force must not be offered"
        );
    }

    /// MG.17b — a value argument contributes `-m <text>`, and only
    /// when actually set. An unset value must contribute NOTHING, not
    /// an empty string: `git stash push -m ""` labels the stash with
    /// an empty message, which is worse than no label.
    #[test]
    fn a_value_argument_contributes_only_when_set() {
        use lattice_grammar::{ArgValue, Args};
        let op = RemoteOp::STASH;
        // slots: [include-untracked: Bool, message: String]
        assert_eq!(
            op.argv(&Args::List(vec![
                ArgValue::Bool(false),
                ArgValue::String(String::new())
            ])),
            vec!["stash", "push"],
            "an empty message must not reach git at all"
        );
        assert_eq!(
            op.argv(&Args::List(vec![
                ArgValue::Bool(false),
                ArgValue::String("wip: parser".into())
            ])),
            vec!["stash", "push", "-m", "wip: parser"],
            "the message is one argv entry, spaces and all"
        );
        assert_eq!(
            op.argv(&Args::List(vec![
                ArgValue::Bool(true),
                ArgValue::String("wip".into())
            ])),
            vec!["stash", "push", "--include-untracked", "-m", "wip"],
            "a flag and a value compose in schema order"
        );
    }

    /// A message with spaces must survive as ONE argument. Passing it
    /// unsplit is the whole reason argv is a `Vec<String>` rather than
    /// a formatted string.
    #[test]
    fn a_multi_word_message_stays_a_single_argv_entry() {
        use lattice_grammar::{ArgValue, Args};
        let argv = RemoteOp::STASH.argv(&Args::List(vec![
            ArgValue::Bool(false),
            ArgValue::String("refactor the parser and fix tests".into()),
        ]));
        assert_eq!(argv.last().unwrap(), "refactor the parser and fix tests");
        assert_eq!(argv.len(), 4, "push + -m + the message, not one word each");
    }

    /// The preview quotes a value so a multi-word message doesn't read
    /// as several arguments in the menu.
    #[test]
    fn the_preview_quotes_a_value_argument() {
        let preview = RemoteOp::STASH.preview(&|name| match name {
            "message" => Some("wip: two words".to_string()),
            _ => None,
        });
        assert_eq!(preview, r#"git stash push -m "wip: two words""#);
    }

    /// `arg_specs` is the ex-command's schema and must line up 1:1 with
    /// the flag table the transient and argv builder read — the whole
    /// point of one definition.
    #[test]
    fn arg_specs_mirror_the_flag_table_positionally() {
        for op in [
            RemoteOp::PULL,
            RemoteOp::PUSH,
            RemoteOp::FETCH,
            RemoteOp::STASH,
        ] {
            let specs = op.arg_specs();
            assert_eq!(specs.len(), op.flags.len(), "`{}` schema length", op.what);
            for (spec, flag) in specs.iter().zip(op.flags) {
                assert_eq!(spec.name.as_ref(), flag.name);
                let expected = match flag.kind {
                    RemoteArgKind::Flag => lattice_grammar::ArgKind::Bool,
                    RemoteArgKind::Value { .. } => lattice_grammar::ArgKind::String,
                };
                assert_eq!(spec.kind, expected, "`{}` slot `{}`", op.what, flag.name);
            }
        }
    }

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
