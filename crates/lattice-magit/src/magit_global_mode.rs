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

use lattice_grammar::{EchoLevel, Effect};
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
    // PD.3 (2026-08-12): the Diff transient's `e` row — the editable
    // cross-file project diff. It cannot use `open!`: the view is a
    // multibuffer, not a synthetic Document, so it routes through the
    // generic provider-view seam instead of `OpenSyntheticBuffer`.
    //
    // The handler is a pure name + args hand-off; everything the view
    // does lives in `providers::project_diff`'s registered opener. The
    // ex-command `:magit-project-diff` returns the identical effect, so
    // the two front-ends cannot drift apart.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-project-diff",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            Some(Effect::AppAction(
                lattice_grammar::app_effect::AppEffect::OpenProviderView {
                    provider: crate::providers::project_diff::PROVIDER_NAME.to_string(),
                    args: ctx.args.clone(),
                },
            ))
        }),
    });
    open!(
        "action:magit-global-commit",
        "*magit:commit*",
        "magit-commit-mode"
    );
    // MG.43h: `d` / `l` carry the argument menu's toggles into the
    // view they open. The values are left under the buffer's name for
    // the mode to take on activation (`ViewArgsRequests`) — the buffer
    // does not exist yet, so there is nothing else to hold them.
    macro_rules! open_view_with_args {
        ($action_name:expr, $buffer_name:expr, $mode_id:expr, $flags:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let extra = crate::magit_core_mode::view_argv($flags, &ctx.args);
                    if !extra.is_empty()
                        && let Some(reqs) = ctx
                            .services
                            .get::<crate::magit_diff_mode::ViewArgsRequestsHandle>()
                    {
                        reqs.put($buffer_name.to_string(), extra);
                    }
                    Some(Effect::OpenSyntheticBuffer {
                        name: $buffer_name.to_string(),
                        mode_id: $mode_id.to_string(),
                    })
                }),
            });
        };
    }
    open_view_with_args!(
        "action:magit-global-log",
        "*magit:log*",
        "magit-log-mode",
        crate::magit_log_mode::LOG_ARGS
    );
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
    // MG.21d: `M` on the root dispatch, magit's own key for remote
    // management. It opens the remote BUFFER rather than a submenu —
    // see `magit_remote_mode`'s header for why the list needs a
    // surface that can show URLs.
    open!(
        "action:magit-global-remote",
        "*magit:remote*",
        "magit-remote-mode"
    );
    // MG.21i: `o` on the root dispatch, magit's own key.
    open!(
        "action:magit-global-submodule",
        "*magit:submodule*",
        "magit-submodule-mode"
    );
    // MG.35: `y` on the root dispatch, magit's own key.
    open!(
        "action:magit-global-refs",
        crate::magit_refs_mode::REFS_BUFFER,
        "magit-refs-mode"
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
    // MG.42-E1: magit's `w`. Same compose buffer, different intent —
    // the name selects it (see `CommitIntent::from_buffer_name`).
    open!(
        "action:magit-global-reword",
        "*magit:reword*",
        "magit-commit-mode"
    );
    open_view_with_args!(
        "action:magit-global-diff",
        "*magit:diff*",
        "magit-diff-mode",
        crate::magit_diff_mode::DIFF_ARGS
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
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    // MG.41g: no notification handle. The op publishes
                    // `BackgroundTaskFinished`; the notification layer
                    // subscribes. magit does not depend on it.
                    Some(spawn_remote_op($op, &ctx.args))
                }),
            });
        };
    }
    // MG.41c: one handler per destination. The op is the same; only
    // the target differs, which is why seven push rows cost one macro
    // rather than seven functions.
    macro_rules! remote_op_to {
        ($action_name:expr, $op:expr, $target:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    // A `Prompted` target's value rides in as the
                    // `dest` arg the transient filled in; the others
                    // ignore it.
                    let prompted =
                        ctx.args
                            .as_list()
                            .and_then(|l| l.last())
                            .and_then(|v| match v {
                                lattice_grammar::ArgValue::String(s) => Some(s.clone()),
                                _ => None,
                            });
                    Some(spawn_remote_op_to($op, &ctx.args, $target, prompted))
                }),
            });
        };
    }

    // Push — magit's seven destinations.
    remote_op_to!(
        "action:magit-global-push-configured",
        RemoteOp::PUSH,
        RemoteTarget::Configured
    );
    remote_op_to!(
        "action:magit-global-push-upstream",
        RemoteOp::PUSH,
        RemoteTarget::Upstream
    );
    remote_op_to!(
        "action:magit-global-push-elsewhere",
        RemoteOp::PUSH,
        RemoteTarget::Prompted
    );
    remote_op_to!(
        "action:magit-global-push-other-branch",
        RemoteOp::PUSH,
        RemoteTarget::Prompted
    );
    remote_op_to!(
        "action:magit-global-push-refspecs",
        RemoteOp::PUSH,
        RemoteTarget::Prompted
    );
    remote_op_to!(
        "action:magit-global-push-tag",
        RemoteOp::PUSH,
        RemoteTarget::Prompted
    );
    remote_op_to!(
        "action:magit-global-push-all-tags",
        RemoteOp::PUSH,
        RemoteTarget::AllTags
    );

    // Pull — magit's three.
    remote_op_to!(
        "action:magit-global-pull-configured",
        RemoteOp::PULL,
        RemoteTarget::Configured
    );
    remote_op_to!(
        "action:magit-global-pull-upstream",
        RemoteOp::PULL,
        RemoteTarget::Upstream
    );
    remote_op_to!(
        "action:magit-global-pull-elsewhere",
        RemoteOp::PULL,
        RemoteTarget::Prompted
    );

    // Fetch — magit's six (submodules is deferred; see the slice plan).
    remote_op_to!(
        "action:magit-global-fetch-configured",
        RemoteOp::FETCH,
        RemoteTarget::Configured
    );
    remote_op_to!(
        "action:magit-global-fetch-upstream",
        RemoteOp::FETCH,
        RemoteTarget::Upstream
    );
    remote_op_to!(
        "action:magit-global-fetch-elsewhere",
        RemoteOp::FETCH,
        RemoteTarget::Prompted
    );
    remote_op_to!(
        "action:magit-global-fetch-other-branch",
        RemoteOp::FETCH,
        RemoteTarget::Prompted
    );
    remote_op_to!(
        "action:magit-global-fetch-refspecs",
        RemoteOp::FETCH,
        RemoteTarget::Prompted
    );
    remote_op_to!(
        "action:magit-global-fetch-all-remotes",
        RemoteOp::FETCH,
        RemoteTarget::AllRemotes
    );

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
    // The merge sequence rows — the two things a stopped merge can do.
    remote_op!(
        "action:magit-global-merge-continue",
        RemoteOp::MERGE_CONTINUE
    );
    remote_op!("action:magit-global-merge-abort", RemoteOp::MERGE_ABORT);
    // MG.41e: the rebase sequencer rows.
    remote_op!(
        "action:magit-global-rebase-continue",
        RemoteOp::REBASE_CONTINUE
    );
    remote_op!("action:magit-global-rebase-skip", RemoteOp::REBASE_SKIP);
    remote_op!("action:magit-global-rebase-abort", RemoteOp::REBASE_ABORT);
    // MG.42-E4: the sequencer controls.
    remote_op!(
        "action:magit-global-cherry-pick-continue",
        RemoteOp::CHERRY_PICK_CONTINUE
    );
    remote_op!(
        "action:magit-global-cherry-pick-skip",
        RemoteOp::CHERRY_PICK_SKIP
    );
    remote_op!(
        "action:magit-global-cherry-pick-abort",
        RemoteOp::CHERRY_PICK_ABORT
    );
    remote_op!(
        "action:magit-global-revert-continue",
        RemoteOp::REVERT_CONTINUE
    );
    remote_op!("action:magit-global-revert-skip", RemoteOp::REVERT_SKIP);
    remote_op!("action:magit-global-revert-abort", RemoteOp::REVERT_ABORT);
    // MG.41d: magit's `x` / `i` stash variants — same spawner, different
    // argv, so they cost a line each rather than a handler each.
    remote_op!(
        "action:magit-global-stash-keep-index",
        RemoteOp::STASH_KEEP_INDEX
    );
    remote_op!("action:magit-global-stash-staged", RemoteOp::STASH_STAGED);
    // MG.42-E2: the snapshots. No input, so they fire directly.
    macro_rules! snapshot_op {
        ($action_name:expr, $label:expr, $extra:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(spawn_git_sequence($label, stash_snapshot_steps($extra)))
                }),
            });
        };
    }
    snapshot_op!("action:magit-global-stash-snapshot", "stash snapshot", &[]);
    snapshot_op!(
        "action:magit-global-stash-snapshot-index",
        "stash snapshot (index)",
        &["--staged"]
    );
    snapshot_op!(
        "action:magit-global-stash-snapshot-worktree",
        "stash snapshot (worktree)",
        &["--keep-index"]
    );
    // MG.23b: the two repo-wide index rows magit puts on `S` / `U`.
    // Both need no target — they act on the whole index — which is why
    // they land before the commit-acting rows (`A` / `_` / `O`), whose
    // root-dispatch entries still want a commit picker.
    // MG.21g: bisect. Every mark checks out a different commit, so
    // each of these refreshes EVERY live magit view rather than one —
    // an open log or diff is just as stale as the status buffer after
    // a `good`. See `buffer_state::refresh_all_views`.
    //
    // Start asks for its two ends. `HEAD` seeds the bad one because
    // "the bug is here now" is why you are starting a bisect at all;
    // the good end has no defensible default and is left empty.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-bisect-start",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(prompt_seeded(
                "Bisect — known BAD revision: ",
                "action:magit-global-bisect-start-good",
                "HEAD".to_string(),
            ))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-bisect-start-good",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let bad = ctx.prompt_value?.trim().to_string();
            if bad.is_empty() {
                return None;
            }
            Some(Effect::OpenPrompt {
                prompt: format!("Bisect {bad} back to — known GOOD revision: "),
                initial: String::new(),
                on_submit_action: "action:magit-global-bisect-start-finish".to_string(),
                buffer_name: Some(bisect_start_buffer_name(&bad)),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-bisect-start-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let good = ctx.prompt_value?.trim().to_string();
            let bad = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(lattice_core::BufferId(ctx.buffer_id.0 as u32))
                .and_then(|n| bad_from_bisect_start_buffer_name(&n))?;
            if good.is_empty() {
                return None;
            }
            spawn_bisect(ctx, "start", move |repo| {
                lattice_vcs::Bisect::start(repo, Some(&bad), Some(&good))
            });
            Some(Effect::Echo {
                level: EchoLevel::Info,
                text: "bisecting\u{2026}".to_string(),
            })
        }),
    });

    macro_rules! bisect_mark {
        ($action_name:expr, $verb:literal, $call:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    // `None` = the revision git checked out for you.
                    // Naming one would mean reading a cursor, and this
                    // fires from a menu that has none.
                    spawn_bisect(ctx, $verb, $call);
                    None
                }),
            });
        };
    }
    bisect_mark!("action:magit-global-bisect-good", "good", |repo| {
        lattice_vcs::Bisect::good(repo, None)
    });
    bisect_mark!("action:magit-global-bisect-bad", "bad", |repo| {
        lattice_vcs::Bisect::bad(repo, None)
    });
    bisect_mark!("action:magit-global-bisect-skip", "skip", |repo| {
        lattice_vcs::Bisect::skip(repo, None)
    });
    bisect_mark!("action:magit-global-bisect-reset", "reset", |repo| {
        lattice_vcs::Bisect::reset(repo)
    });

    // MG.23c1: prompt-backed rows. The first action opens the prompt;
    // the `-finish` half does the work with what was typed.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-tag",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(prompt_for("Tag name: ", "action:magit-global-tag-finish"))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-tag-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let name = ctx.prompt_value?.trim();
            // An empty prompt is a cancel, not a request to tag HEAD
            // with the empty string (which git would reject anyway,
            // loudly and confusingly).
            (!name.is_empty()).then(|| spawn_git(tag_argv(name), "tag"))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-gitignore",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(prompt_for(
                "Ignore pattern: ",
                "action:magit-global-gitignore-finish",
            ))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-gitignore-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let pattern = ctx.prompt_value?.trim();
            (!pattern.is_empty()).then(|| spawn_gitignore(pattern.to_string()))
        }),
    });
    // MG.23d: file operations. Each reads `active_target`, so they act
    // on the visited file from `C-c f` and on the named one from
    // `:magit-other-file-dispatch`.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-untrack",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let (_workdir, rel) = active_target(ctx)?;
            // No confirm: the file stays on disk and only leaves the
            // index, which `git add` puts back. §12.13's bar is work
            // git cannot hand back.
            Some(spawn_git(untrack_argv(&rel.to_string_lossy()), "untrack"))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-delete",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let (_workdir, rel) = active_target(ctx)?;
            // Carries the path (IX.1): the execute half acts on what
            // this prompt names, not on wherever the cursor ends up.
            Some(crate::confirm::ask_target(
                format!("Delete {}?", rel.display()),
                "action:magit-global-file-delete-execute",
                rel.to_string_lossy().into_owned(),
            ))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-delete-execute",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let path = match crate::confirm::carried_target(ctx) {
                Some(carried) => carried,
                None => active_target(ctx)?.1.to_string_lossy().into_owned(),
            };
            Some(spawn_git(delete_argv(&path), "delete"))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-rename",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let (_workdir, rel) = active_target(ctx)?;
            let from = rel.to_string_lossy().into_owned();
            Some(Effect::OpenPrompt {
                prompt: "Rename to: ".to_string(),
                // Seeded with the current path so a rename within the
                // same directory is an edit rather than a retype.
                initial: from.clone(),
                on_submit_action: "action:magit-global-file-rename-finish".to_string(),
                // The source rides in the buffer name: by submit time
                // the prompt buffer is the active one, so nothing else
                // still knows which file this was.
                buffer_name: Some(format!("*magit:rename:{from}*")),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-rename-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let to = ctx.prompt_value?.trim().to_string();
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let from = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| rename_source_from_prompt_buffer_name(&n))?;
            // Renaming a file to its own name is what submitting the
            // seeded value unchanged means — a cancel, not a git call
            // that would fail with "source and destination are the
            // same".
            (!to.is_empty() && to != from).then(|| spawn_git(rename_argv(&from, &to), "rename"))
        }),
    });

    // MG.23d2: `,c` — this file as it was at some revision, written
    // over the working-tree copy. Prompt for the revision, then confirm,
    // because the write is over uncommitted work and git keeps no copy
    // of what it replaced.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-checkout",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let (_workdir, rel) = active_target(ctx)?;
            let path = rel.to_string_lossy().into_owned();
            // MG.53.c/g: pick the revision — branch, tag or commit.
            // `magit-file-checkout` is
            // `<rev> <path>`, so the pick fills the `{}` — and that
            // ex-command still runs the same confirm, so reaching this
            // by picker does not skip the "discards local changes"
            // guard.
            Some(Effect::OpenPicker {
                source: crate::picker_sources::REVISION_PICK_SOURCE.to_string(),
                args: vec![format!("magit-file-checkout {{}} {path}")],
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-checkout-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let rev = ctx.prompt_value?.trim().to_string();
            if rev.is_empty() {
                return None;
            }
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let path = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| checkout_target_from_prompt_buffer_name(&n))?;
            // Carries both halves (IX.1): by execute time the prompt
            // buffer is gone and the confirm dialog is what is active,
            // so neither the revision nor the path is re-derivable.
            Some(crate::confirm::ask_with(
                format!("Checkout {path} from {rev}, discarding its uncommitted changes?"),
                "action:magit-global-file-checkout-execute",
                lattice_grammar::Args::List(vec![
                    lattice_grammar::ArgValue::String(rev),
                    lattice_grammar::ArgValue::String(path),
                ]),
            ))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-checkout-execute",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            // No re-derivation fallback, unlike the other execute
            // halves: there is no sensible guess for a revision, and
            // checking out from the wrong one is the exact damage the
            // confirm exists to prevent. Both slots or nothing.
            let rev = ctx.arg_str(0)?;
            let path = ctx.arg_str(1)?;
            Some(spawn_git(checkout_file_argv(rev, path), "checkout file"))
        }),
    });

    // MG.23c2: `I` init and `m` merge, on c1's prompt shape.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-init",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            // Seeded with the working directory: initialising *here* is
            // the overwhelmingly common intent, and creating a `.git`
            // in the wrong place is annoying enough to be worth showing
            // the path before it happens rather than after.
            let cwd = std::env::current_dir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| ".".to_string());
            Some(prompt_seeded(
                "Initialize repository in: ",
                "action:magit-global-init-finish",
                cwd,
            ))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-init-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let dir = ctx.prompt_value?.trim();
            (!dir.is_empty()).then(|| spawn_git(init_argv(dir), "init"))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-merge",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            // MG.52: a picker. A branch that does not exist is not a
            // merge target or a reset destination — it is a typo, and
            // git reports it long after the keystroke that caused it.
            Some(Effect::OpenPicker {
                source: crate::picker_sources::BRANCH_PICK_SOURCE.to_string(),
                args: vec!["magit-merge".to_string()],
            })
        }),
    });
    // MG.43e: a prompt whose answer names a BUFFER rather than an
    // argv — for rows that show something instead of changing it.
    macro_rules! prompted_op_open {
        ($entry:expr, $prompt:expr, $finish:expr, $name:expr, $mode_id:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $entry,
                handler: Arc::new(|_ctx: &ActionContext<'_>| Some(prompt_for($prompt, $finish))),
            });
            contributions.push(ActionHandlerContribution {
                action_name: $finish,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let value = ctx.prompt_value?.trim();
                    if value.is_empty() {
                        return None;
                    }
                    Some(Effect::OpenSyntheticBuffer {
                        name: ($name)(value),
                        mode_id: $mode_id.to_string(),
                    })
                }),
            });
        };
    }
    // MG.53.a: the picker peer of `prompted_op!`.
    //
    // Same two contributions, but the entry opens the branch picker
    // instead of a prompt and the finish half is gone — the ex-command
    // named here IS the finish half, because a picked candidate reaches
    // an operation only as an ex line. The `*_argv` builder moved with
    // it, so there is still exactly one place that knows each
    // operation's git arguments.
    // MG.53.d: the same shape against a source other than branches.
    macro_rules! picked_from {
        ($entry:expr, $source:expr, $ex_command:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $entry,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(Effect::OpenPicker {
                        source: $source.to_string(),
                        args: vec![$ex_command.to_string()],
                    })
                }),
            });
        };
    }
    macro_rules! picked_op {
        ($entry:expr, $ex_command:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $entry,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(Effect::OpenPicker {
                        source: crate::picker_sources::BRANCH_PICK_SOURCE.to_string(),
                        args: vec![$ex_command.to_string()],
                    })
                }),
            });
        };
    }
    picked_op!(
        "action:magit-global-merge-no-commit",
        "magit-merge-no-commit"
    );
    picked_op!("action:magit-global-merge-squash", "magit-merge-squash");
    // MG.43d: magit's branch `s` spin-off and `S` spin-out.
    //
    // Both create a branch from the current branch's unpushed commits
    // and rewind the current branch to its upstream. `checkout` is the
    // only difference: spin-off leaves you on the new branch, spin-out
    // leaves you where you were.
    macro_rules! spinoff_op {
        ($entry:expr, $finish:expr, $prompt:expr, $checkout:expr, $label:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $entry,
                handler: Arc::new(|_ctx: &ActionContext<'_>| Some(prompt_for($prompt, $finish))),
            });
            contributions.push(ActionHandlerContribution {
                action_name: $finish,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let branch = ctx.prompt_value?.trim().to_string();
                    if branch.is_empty() {
                        return None;
                    }
                    let shown = format!("{} {branch}", $label);
                    Some(spawn_computed($label, shown, move |wd| {
                        crate::cherry_move::branch_spinoff(wd, &branch, $checkout)
                    }))
                }),
            });
        };
    }
    spinoff_op!(
        "action:magit-global-branch-spinoff",
        "action:magit-global-branch-spinoff-finish",
        "Spin off branch: ",
        true,
        "branch spin-off"
    );
    spinoff_op!(
        "action:magit-global-branch-spinout",
        "action:magit-global-branch-spinout-finish",
        "Spin out branch: ",
        false,
        "branch spin-out"
    );

    // MG.43d: the cherry-move rows. Each resolves a commit first (the
    // cursor, or a picker), stashes it, then prompts for the branch —
    // the same carry `two_input_op!` uses, and consumed the same way.
    macro_rules! cherry_move_finish {
        ($finish:expr, $label:expr, $body:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $finish,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let branch = ctx.prompt_value?.trim().to_string();
                    let commit = take_first_input()?;
                    if branch.is_empty() {
                        return None;
                    }
                    let shown = format!("{} {commit} -> {branch}", $label);
                    Some(spawn_computed($label, shown, move |wd| {
                        let f: fn(&std::path::Path, &str, &str) -> Result<(), String> = $body;
                        f(wd, &commit, &branch)
                    }))
                }),
            });
        };
    }
    cherry_move_finish!(
        "action:magit-cherry-harvest-finish",
        "cherry harvest",
        |wd, commit, branch| {
            // Move it FROM `branch` onto the current one, and stay put.
            let current = crate::cherry_move::current_branch_of(wd)
                .ok_or_else(|| "not on a branch".to_string())?;
            crate::cherry_move::cherry_move(wd, commit, Some(branch), &current, None, true)
        }
    );
    cherry_move_finish!(
        "action:magit-cherry-donate-finish",
        "cherry donate",
        |wd, commit, branch| {
            // Move it from the current branch onto `branch`, and stay
            // on the current one.
            let current = crate::cherry_move::current_branch_of(wd)
                .ok_or_else(|| "not on a branch".to_string())?;
            crate::cherry_move::cherry_move(wd, commit, Some(&current), branch, None, false)
        }
    );
    cherry_move_finish!(
        "action:magit-cherry-spinout-finish",
        "cherry spin-out",
        |wd, commit, branch| {
            let current = crate::cherry_move::current_branch_of(wd)
                .ok_or_else(|| "not on a branch".to_string())?;
            // The new branch starts at the UPSTREAM, not here — see
            // `spin_start_point`. Starting at the current branch would
            // make the cherry-pick empty, because the commit is
            // already there.
            let start = crate::cherry_move::spin_start_point(wd, commit);
            crate::cherry_move::cherry_move(wd, commit, Some(&current), branch, Some(&start), false)
        }
    );
    cherry_move_finish!(
        "action:magit-cherry-spinoff-finish",
        "cherry spin-off",
        |wd, commit, branch| {
            let current = crate::cherry_move::current_branch_of(wd)
                .ok_or_else(|| "not on a branch".to_string())?;
            let start = crate::cherry_move::spin_start_point(wd, commit);
            crate::cherry_move::cherry_move(wd, commit, Some(&current), branch, Some(&start), true)
        }
    );

    // MG.43f: magit's fetch `m` — fetch submodules too.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-fetch-submodules",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(spawn_git(
                ["fetch", "--recurse-submodules"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                "fetch --recurse-submodules",
            ))
        }),
    });

    // MG.43e: merge `p` preview — a read-only diff of what merging
    // would bring in. Opens a buffer rather than running anything.
    prompted_op_open!(
        "action:magit-global-merge-preview",
        "Preview merge with branch: ",
        "action:magit-global-merge-preview-finish",
        |branch: &str| format!("*magit:diff:merge-preview:{branch}*"),
        "magit-diff-mode"
    );

    // MG.43e: merge `i` — merge THIS branch into another and delete
    // this one. The mirror of `a` absorb; the direction is the whole
    // difference, and it deletes a different branch.
    picked_op!("action:magit-global-merge-into", "magit-merge-into");
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-merge-into-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let target = ctx.prompt_value?.trim().to_string();
            if target.is_empty() {
                return None;
            }
            // Detached HEAD has no branch to merge or delete, so this
            // declines rather than acting on `HEAD`.
            let Some(current) = current_branch() else {
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: not on a branch".to_string(),
                });
            };
            if current == target {
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: cannot merge a branch into itself".to_string(),
                });
            }
            Some(spawn_git_sequence(
                "merge into",
                merge_into_steps(&current, &target),
            ))
        }),
    });

    // MG.43e: tag `p` prune.
    picked_from!(
        "action:magit-global-tag-prune",
        crate::picker_sources::REMOTE_PICK_SOURCE,
        "magit-tag-prune"
    );

    // MG.43b: rebase `e` elsewhere, and `f` autosquash.
    picked_op!(
        "action:magit-global-rebase-onto-elsewhere",
        "magit-rebase-onto"
    );
    picked_op!(
        "action:magit-global-rebase-autosquash",
        "magit-rebase-autosquash"
    );
    // MG.42-E2: absorb — merge then delete, as one operation.
    picked_op!("action:magit-global-merge-absorb", "magit-merge-absorb");
    // MG.42-E3: two-input operations. The first prompt's finish opens
    // the second; the second builds the argv from both.
    macro_rules! two_input_op {
        ($entry:expr, $p1:expr, $mid:expr, $p2:expr, $finish:expr, $argv:expr, $what:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $entry,
                handler: Arc::new(|_ctx: &ActionContext<'_>| Some(prompt_for($p1, $mid))),
            });
            contributions.push(ActionHandlerContribution {
                action_name: $mid,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let first = ctx.prompt_value?.trim();
                    // Cancelling or clearing the FIRST prompt must run
                    // nothing — not a half-applied operation with an
                    // empty argument.
                    if first.is_empty() {
                        return None;
                    }
                    stash_first_input(first.to_string());
                    Some(prompt_for($p2, $finish))
                }),
            });
            contributions.push(ActionHandlerContribution {
                action_name: $finish,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let second = ctx.prompt_value?.trim();
                    // Same at the second step, and the pending value is
                    // consumed either way so it cannot leak into a later
                    // chain.
                    let first = take_first_input()?;
                    if second.is_empty() {
                        return None;
                    }
                    Some(spawn_git($argv(&first, second), $what))
                }),
            });
        };
    }
    two_input_op!(
        "action:magit-global-reset-file",
        "Reset file from commit: ",
        "action:magit-global-reset-file-path",
        "File path: ",
        "action:magit-global-reset-file-finish",
        reset_file_argv,
        "reset a file"
    );
    // MG.43b: rebase `s` — a subset onto a new base. Two refs, and
    // the order is load-bearing (see `rebase_subset_argv`).
    two_input_op!(
        "action:magit-global-rebase-subset",
        "Rebase onto (new base): ",
        "action:magit-global-rebase-subset-upstream",
        "Commits after (upstream): ",
        "action:magit-global-rebase-subset-finish",
        rebase_subset_argv,
        "rebase --onto"
    );
    // MG.43e: tag `r` release — annotated, so two inputs.
    two_input_op!(
        "action:magit-global-tag-release",
        "Release tag name: ",
        "action:magit-global-tag-release-message",
        "Tag message: ",
        "action:magit-global-tag-release-finish",
        tag_release_argv,
        "tag -a"
    );
    two_input_op!(
        "action:magit-global-stash-branch",
        "New branch name: ",
        "action:magit-global-stash-branch-stash",
        "Stash (e.g. stash@{0}): ",
        "action:magit-global-stash-branch-finish",
        stash_branch_argv,
        "stash branch"
    );

    // MG.43g: magit's `C` configure rows. One prompt-then-write pair
    // per key, generated from the SAME table the menus render from, so
    // a row and its handler cannot drift apart.
    //
    // The prompt is seeded with the current value, so editing an
    // existing setting starts from what it is rather than blank.
    macro_rules! config_op {
        ($action_name:expr, $finish:expr, $config_key:expr, $label:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    // Seeded with the current value: a configure row
                    // edits an EXISTING setting, so starting blank
                    // would mean retyping it to change one character,
                    // and an accidental `<CR>` would clear it.
                    let current = crate::git_config::value_of($config_key).unwrap_or_default();
                    Some(prompt_seeded(
                        concat!($label, " (", $config_key, "): "),
                        $finish,
                        current,
                    ))
                }),
            });
            contributions.push(ActionHandlerContribution {
                action_name: $finish,
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    // An empty value UNSETS rather than declining: a
                    // configure row must be able to clear a setting,
                    // and blanking the prompt is how magit does it.
                    let value = ctx.prompt_value?.trim().to_string();
                    Some(crate::git_config::set($config_key, &value))
                }),
            });
        };
    }
    config_op!(
        "action:magit-config-pull-rebase",
        "action:magit-config-pull-rebase-finish",
        "pull.rebase",
        "Rebase on pull"
    );
    config_op!(
        "action:magit-config-push-default",
        "action:magit-config-push-default-finish",
        "remote.pushDefault",
        "Default push target"
    );
    config_op!(
        "action:magit-config-fetch-prune",
        "action:magit-config-fetch-prune-finish",
        "fetch.prune",
        "Prune on fetch"
    );
    config_op!(
        "action:magit-config-tag-sign",
        "action:magit-config-tag-sign-finish",
        "tag.gpgSign",
        "Sign tags"
    );
    config_op!(
        "action:magit-config-notes-ref",
        "action:magit-config-notes-ref-finish",
        "core.notesRef",
        "Notes ref"
    );

    // MG.43b: magit's rebase `p` / `u` — onto the configured push
    // target or the upstream. Both are plain revisions to git, so
    // neither needs the `RemoteTarget` resolution push/pull use.
    macro_rules! rebase_onto {
        ($action_name:expr, $target:expr, $what:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(spawn_git(rebase_onto_argv($target), $what))
                }),
            });
        };
    }
    rebase_onto!(
        "action:magit-global-rebase-onto-push",
        "@{push}",
        "rebase onto @{push}"
    );
    rebase_onto!(
        "action:magit-global-rebase-onto-upstream",
        "@{upstream}",
        "rebase onto @{upstream}"
    );

    // MG.43a: magit's commit `e` — add what is staged to the last
    // commit, keeping its message.
    //
    // The one commit row that takes NO argument: it always acts on
    // HEAD, so there is nothing to resolve and no prompt to answer.
    // `--no-edit` is what makes it "extend" rather than "amend" — the
    // message is deliberately left alone.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-commit-extend",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(spawn_git(
                ["commit", "--amend", "--no-edit"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
                "commit --amend --no-edit",
            ))
        }),
    });

    // MG.43a: magit's branch `x` — reset the current branch to another
    // ref.
    //
    // Destructive in the same way `reset --hard` is, so it asks, and
    // the ref the prompt named is CARRIED to the execute half rather
    // than re-derived (IX.1) — a background refresh while the dialog
    // is open must not change what gets reset.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-reset",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            // MG.52: a picker. A branch that does not exist is not a
            // merge target or a reset destination — it is a typo, and
            // git reports it long after the keystroke that caused it.
            Some(Effect::OpenPicker {
                source: crate::picker_sources::BRANCH_PICK_SOURCE.to_string(),
                args: vec!["magit-branch-reset".to_string()],
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-reset-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let target = ctx.prompt_value?.trim();
            if target.is_empty() {
                return None;
            }
            Some(crate::confirm::ask_target(
                format!("git reset --hard {target} — discard uncommitted changes?"),
                "action:magit-global-branch-reset-execute",
                target.to_string(),
            ))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-reset-execute",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let target = crate::confirm::carried_target(ctx)?;
            Some(spawn_git(
                vec!["reset".to_string(), "--hard".to_string(), target.clone()],
                "branch reset",
            ))
        }),
    });

    // MG.42-E1: merge `e` — prompt for the branch, then compose the
    // merge message in a buffer. Genuinely different from the `n`
    // don't-commit row: this completes the merge in one step with an
    // authored message, rather than leaving a staged merge behind.
    picked_op!("action:magit-global-merge-edit", "magit-merge-edit");
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-merge-edit-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let branch = ctx.prompt_value?.trim();
            if branch.is_empty() {
                return None;
            }
            Some(Effect::OpenSyntheticBuffer {
                name: crate::magit_commit_mode::CommitIntent::merge_edit_buffer_name(branch),
                mode_id: "magit-commit-mode".to_string(),
            })
        }),
    });

    picked_from!(
        "action:magit-global-tag-delete",
        crate::picker_sources::TAG_PICK_SOURCE,
        "magit-tag-delete"
    );
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-merge-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let branch = ctx.prompt_value?.trim();
            // `--no-edit` for the same reason `revert` passes it: git
            // would otherwise open `$EDITOR` for the merge message,
            // which inside lattice is a wait on a prompt that never
            // appears.
            (!branch.is_empty()).then(|| spawn_git(merge_argv(branch), "merge"))
        }),
    });
    remote_op!("action:magit-global-stage-all", RemoteOp::STAGE_ALL);
    remote_op!("action:magit-global-unstage-all", RemoteOp::UNSTAGE_ALL);

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
                    let (_workdir, rel) = active_target(ctx)?;
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
                    let (workdir, rel) = active_target(ctx)?;
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
            let (_workdir, rel) = active_target(ctx)?;
            // IX.1: carry the path the prompt names, so the execute half
            // acts on exactly what was confirmed. It needs no change to
            // read it — `active_target` already prefers the `file`
            // argument over the visited file, which is the same seam
            // `:magit-other-file-dispatch` uses.
            Some(crate::confirm::ask_with(
                format!("Discard changes to {}?", rel.display()),
                "action:magit-global-file-discard-execute",
                lattice_grammar::Args::List(vec![lattice_grammar::ArgValue::String(
                    rel.to_string_lossy().into_owned(),
                )]),
            ))
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
    // MG.26b: blame no longer opens a buffer — it activates a minor on
    // the buffer you are already reading, so the file keeps its own
    // major, its parser and therefore its highlighting.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-blame",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(Effect::ToggleMode {
                mode_name: crate::MagitBlameMode::mode_id().as_str().to_string(),
            })
        }),
    });

    // MG.29: the branch submenu's picker-backed rows.
    //
    // The branch buffer's own `<CR>` / `c` read a cursor, and a menu
    // opened from anywhere has none — so these ASK, which is the same
    // answer MG.23j gave `A` / `_` / `O` and the reason magit puts its
    // branch commands in an ungated group.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-checkout",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(Effect::OpenPicker {
                source: crate::picker_sources::BRANCH_CHECKOUT_SOURCE.to_string(),
                args: Vec::new(),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-create",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            // The same two-step wizard `c` runs in the branch buffer —
            // pick a base, then name the branch. Ungated: the buffer
            // version refuses outside a branch list, which is right for
            // a chord and wrong for a menu row.
            Some(Effect::OpenPicker {
                source: "magit-branch-pick-base".to_string(),
                args: Vec::new(),
            })
        }),
    });

    // MG.28: `v` — this file at a revision you name.
    //
    // The gap it fills: `magit-file-revision-mode` has existed since
    // MG.11, but the only ways in were `<CR>` on a file inside a
    // revision/diff view and `gj`/`gk` to walk from there. There was no
    // way to say "this file, at that revision" directly.
    //
    // Magit prompts for a revision AND a file. Here only the revision
    // is asked, because `C-c f` already means "the file I am visiting"
    // (MG.23a) — asking for something the menu already knows is the
    // deviation magit's own file-dispatch makes for the same reason.
    // `:magit-find-file <rev> [<path>]` is the explicit form.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-at-revision",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let (_workdir, rel) = active_target(ctx)?;
            let path = rel.to_string_lossy().into_owned();
            // MG.53.c/g: a picker over REVISIONS — branches, tags and
            // recent commits. `magit-find-file`
            // is `<rev> <path>`, so the pick goes in the `{}` rather
            // than on the end — see `picker_sources::picked_line`.
            //
            // The picker's first row is HEAD (it is `git log`), which
            // preserves what the prompt's `HEAD` default gave: "what did
            // this look like before my edits" is still one keystroke,
            // and now it shows the subject.
            Some(Effect::OpenPicker {
                source: crate::picker_sources::REVISION_PICK_SOURCE.to_string(),
                args: vec![format!("magit-find-file {{}} {path}")],
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-at-revision-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let rev = ctx.prompt_value?.trim().to_string();
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let path = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| path_from_prompt_buffer_name(&n, "*magit:show-at:"))?;
            if rev.is_empty() {
                return None;
            }
            Some(Effect::OpenSyntheticBuffer {
                name: crate::magit_file_revision_mode::blob_buffer_name(
                    &rev,
                    std::path::Path::new(&path),
                ),
                mode_id: crate::magit_file_revision_mode::MagitFileRevisionMode::mode_id()
                    .to_string(),
            })
        }),
    });

    // MG.28: `V` — from a blob buffer, back to the LIVE file.
    //
    // The gap: `gj` / `gk` walk a blob's history, and nothing walked
    // back out. From `*magit:file:<rev>:<path>*` the only way to the
    // working-tree copy was to type `:e <path>` yourself, which means
    // knowing the path you are already looking at.
    //
    // Lands on the SAME LINE, via the atomic open-and-position effect.
    // Line numbers drift between revisions, so this is "roughly where
    // you were" rather than a promise — but landing at the top of a
    // file you were reading the middle of is the worse answer, and the
    // alternative (a diff-based line map) is a different feature.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-visit-live",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let parsed = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| crate::magit_file_revision_mode::parse_buffer_name(&n));
            Some(match parsed {
                Some((_git_ref, path)) => {
                    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
                    let full = workdir.join(&path);
                    if full.exists() {
                        Effect::OpenBufferAt {
                            path: Some(full),
                            position: ctx.cursor,
                            force: false,
                        }
                    } else {
                        // The file existed at that revision and does
                        // not now. Saying so beats opening an empty
                        // buffer named after a deleted path.
                        Effect::Echo {
                            level: lattice_grammar::EchoLevel::Warn,
                            text: format!(
                                "magit: {} no longer exists in the working tree",
                                path.display()
                            ),
                        }
                    }
                }
                // Not a file-at-revision buffer.
                //
                // This used to be an error, and it was wrong. `V` means
                // "take me to the live file"; run from an ordinary file
                // buffer you are ALREADY there, so the request is
                // satisfied, not refused. Erroring told the user they had
                // done something wrong when they had asked for a state
                // they were already in.
                //
                // `Effect::None` rather than an echo: there is nothing to
                // report. An echo saying "you are already on the live
                // file" would be noise on a key whose whole job is to put
                // you there.
                //
                // Contrast reverse blame, which genuinely cannot run
                // outside a revision buffer — it needs a revision to
                // resolve against, so refusing IS the answer there. The
                // two looked alike and are not: one is missing an input,
                // the other has already reached its destination.
                None => Effect::None,
            })
        }),
    });

    // MG.23f2: reverse blame — "when did each of these lines go away",
    // the one blame variant magit has that we had no answer for.
    //
    // It does NOT go through `active_target`, and that is the whole
    // shape of it: reverse blame needs a *revision* as well as a path,
    // and its output is the file **as it was at that revision**. Run
    // from a working-tree file it would replace what you are looking at
    // with an older version of it, annotated with shas that mean the
    // opposite of the ones next door in a normal blame. So it is
    // reachable only from a buffer that is already showing a revision —
    // a blob buffer — which is magit's own rule ("Only blob buffers can
    // be blamed in reverse") reached from the same reasoning rather
    // than copied.
    //
    // `staged` is refused with it: the index is not a commit, so there
    // is no range to walk forward from. Same exclusion `gj`/`gk` make,
    // for the same reason.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-file-blame-reverse",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let parsed = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| crate::magit_file_revision_mode::parse_buffer_name(&n))
                .filter(|(git_ref, _)| git_ref != "staged");
            Some(match parsed {
                // MG.26b: the blob buffer this was opened from IS the
                // content reverse blame wants to annotate, so the mode
                // activates on it in place. The direction and revision
                // cannot ride on `ToggleMode` — it carries a mode name
                // and nothing else, which is right, since the grammar
                // crate must not learn what a blame direction is — so
                // they are left as a request keyed by the buffer's
                // name and consumed by `on_activate`.
                Some((git_ref, path)) => {
                    let name = crate::magit_file_revision_mode::blob_buffer_name(&git_ref, &path);
                    if let Some(requests) = ctx
                        .services
                        .get::<crate::magit_blame_mode::BlameRequestsHandle>()
                    {
                        requests.put(
                            name,
                            crate::magit_blame_mode::BlameDirection::Reverse,
                            git_ref.clone(),
                        );
                    }
                    Effect::ToggleMode {
                        mode_name: crate::MagitBlameMode::mode_id().as_str().to_string(),
                    }
                }
                // Naming what is missing and where to get it beats a
                // row that appears to do nothing: the answer is one
                // `<CR>` on a log entry away.
                None => Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: reverse blame needs a revision — open the file at one first \
                           (<CR> on a log entry, then gj/gk to walk)"
                        .to_string(),
                },
            })
        }),
    });

    // MG.38 / MG.39: every one of these needs arguments a menu cannot
    // guess — a subtree prefix, a mailbox path, a commit range — so each
    // row opens a prompt seeded with what IS knowable, and the finish
    // handler reads the operation back out of the prompt buffer's name.
    // Same wizard shape the clone rows and the branch-create flow use.
    macro_rules! prompted {
        ($open:expr, $prompt:expr, $initial:expr, $finish:expr, $buffer:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $open,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(Effect::OpenPrompt {
                        prompt: $prompt.to_string(),
                        initial: $initial.to_string(),
                        on_submit_action: $finish.to_string(),
                        buffer_name: Some($buffer.to_string()),
                    })
                }),
            });
        };
    }

    for op in [
        SubtreeOp::ADD,
        SubtreeOp::MERGE,
        SubtreeOp::PULL,
        SubtreeOp::PUSH,
        SubtreeOp::SPLIT,
    ] {
        contributions.push(ActionHandlerContribution {
            action_name: match op.sub {
                "add" => "action:magit-global-subtree-add",
                "merge" => "action:magit-global-subtree-merge",
                "pull" => "action:magit-global-subtree-pull",
                "push" => "action:magit-global-subtree-push",
                _ => "action:magit-global-subtree-split",
            },
            handler: Arc::new(move |_ctx: &ActionContext<'_>| {
                Some(Effect::OpenPrompt {
                    prompt: format!("{} {}: ", op.what, op.usage()),
                    initial: String::new(),
                    on_submit_action: "action:magit-global-subtree-finish".to_string(),
                    // The operation rides in the prompt buffer's name —
                    // one finish handler for all five, rather than five
                    // near-identical ones.
                    buffer_name: Some(format!("*magit:subtree:{}*", op.sub)),
                })
            }),
        });
    }
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-subtree-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let line = ctx.prompt_value?.trim().to_string();
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let sub = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| path_from_prompt_buffer_name(&n, "*magit:subtree:"))?;
            let op = match sub.as_str() {
                "add" => SubtreeOp::ADD,
                "merge" => SubtreeOp::MERGE,
                "pull" => SubtreeOp::PULL,
                "push" => SubtreeOp::PUSH,
                "split" => SubtreeOp::SPLIT,
                _ => return None,
            };
            if line.is_empty() {
                return None;
            }
            Some(spawn_subtree_op(op, &line))
        }),
    });

    prompted!(
        "action:magit-global-am-apply",
        "Apply patches (paths, add -3 for three-way): ",
        "",
        "action:magit-global-am-apply-finish",
        "*magit:am*"
    );
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-am-apply-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let line = ctx.prompt_value?.trim().to_string();
            if line.is_empty() {
                return None;
            }
            let Some(argv) = am_argv(&line, am_wants_three_way(&line)) else {
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: usage — :magit-am <patch>… [-3]".to_string(),
                });
            };
            Some(spawn_git(argv, "am"))
        }),
    });

    prompted!(
        "action:magit-global-format-patch",
        "Create patches for range: ",
        "@{upstream}..HEAD",
        "action:magit-global-format-patch-finish",
        "*magit:format-patch*"
    );
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-format-patch-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let range = ctx.prompt_value?.trim().to_string();
            // Written to the repository root rather than the editor's
            // process directory: a scatter of `.patch` files somewhere
            // unexpected is tedious to undo, and the repo root is the
            // one directory the user can predict from here.
            let root = crate::workdir::magit_workdir()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(argv) = format_patch_argv(&range, (!root.is_empty()).then_some(root.as_str()))
            else {
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: usage — :magit-format-patch <range>".to_string(),
                });
            };
            Some(spawn_git(argv, "format-patch"))
        }),
    });

    macro_rules! am_op {
        ($action_name:expr, $op:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(spawn_remote_op($op, &lattice_grammar::Args::None))
                }),
            });
        };
    }
    am_op!("action:magit-global-am-continue", RemoteOp::AM_CONTINUE);
    am_op!("action:magit-global-am-skip", RemoteOp::AM_SKIP);
    am_op!("action:magit-global-am-abort", RemoteOp::AM_ABORT);

    // MG.40: `Y` cherries. The upstream is asked for, seeded with
    // `@{upstream}` — the answer in the overwhelmingly common case, and
    // the one thing about this question that IS knowable from here.
    prompted!(
        "action:magit-global-cherries",
        "Cherries against upstream: ",
        "@{upstream}",
        "action:magit-global-cherries-finish",
        "*magit:cherries*"
    );
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-cherries-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let upstream = ctx.prompt_value?.trim().to_string();
            if upstream.is_empty() {
                return None;
            }
            Some(Effect::OpenSyntheticBuffer {
                name: crate::magit_cherry_mode::cherry_buffer_name(&upstream, "HEAD"),
                mode_id: crate::MagitCherryMode::mode_id().as_str().to_string(),
            })
        }),
    });

    // MG.37: the notes submenu's handlers.
    //
    // Edit and remove need a COMMIT, and this menu has no cursor on one
    // when opened outside a magit buffer — so they answer the same two
    // ways `A` / `_` / `O` (MG.23j) and `M` (MG.34) do: the commit under
    // the cursor when there is one, the commit picker when there is not.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-note-edit",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let at_cursor =
                crate::buffer_state::view_for(ctx).and_then(|v| v.commit_at_cursor(ctx.cursor));
            Some(match at_cursor {
                Some(commit) => Effect::OpenSyntheticBuffer {
                    name: crate::magit_notes_mode::note_buffer_name(&commit),
                    mode_id: crate::MagitNotesMode::mode_id().as_str().to_string(),
                },
                None => Effect::OpenPicker {
                    source: crate::picker_sources::COMMIT_PICK_SOURCE.to_string(),
                    args: vec!["magit-note-edit".to_string()],
                },
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-note-remove",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let at_cursor =
                crate::buffer_state::view_for(ctx).and_then(|v| v.commit_at_cursor(ctx.cursor));
            Some(match at_cursor {
                // No confirm: a note is not history, removing one loses
                // only the note, and it is one `T` away from being
                // retyped. `prune` DOES ask — it can drop many at once
                // and names none of them.
                Some(commit) => spawn_note_remove(commit),
                None => Effect::OpenPicker {
                    source: crate::picker_sources::COMMIT_PICK_SOURCE.to_string(),
                    args: vec!["magit-note-remove".to_string()],
                },
            })
        }),
    });

    // Prune asks, because it removes an unbounded number of notes and
    // names none of them — the same bar `x` discard and branch-delete
    // are held to (MG.12). The ask half performs no git call, so
    // answering `n` cannot mutate.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-note-prune",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(crate::confirm::ask(
                "Drop every note whose commit no longer exists?".to_string(),
                "action:magit-global-note-prune-execute",
            ))
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-note-prune-execute",
        handler: Arc::new(|_ctx: &ActionContext<'_>| Some(spawn_note_prune())),
    });

    macro_rules! notes_merge_op {
        ($action_name:expr, $op:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $action_name,
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(spawn_remote_op($op, &lattice_grammar::Args::None))
                }),
            });
        };
    }
    notes_merge_op!(
        "action:magit-global-note-merge-commit",
        RemoteOp::NOTES_MERGE_COMMIT
    );
    notes_merge_op!(
        "action:magit-global-note-merge-abort",
        RemoteOp::NOTES_MERGE_ABORT
    );

    picked_from!(
        "action:magit-global-note-merge",
        crate::picker_sources::REF_PICK_SOURCE,
        "magit-note-merge"
    );
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-note-merge-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let spec = ctx.prompt_value?.trim().to_string();
            if spec.is_empty() {
                return None;
            }
            Some(spawn_note_merge(&spec))
        }),
    });

    // MG.36: magit's `C` clone — a two-step wizard, the same shape the
    // branch-create wizard uses. URL first, then where to put it.
    //
    // Two prompts rather than one line with both, because the second
    // has a *derived default*: `git clone` picks the directory name off
    // the URL, and re-typing it is the step everyone skips in a
    // terminal. Asking separately is what makes offering that default
    // possible.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-clone",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(Effect::OpenPrompt {
                prompt: "Clone repository: ".to_string(),
                initial: String::new(),
                on_submit_action: "action:magit-global-clone-dest".to_string(),
                buffer_name: Some("*magit:clone*".to_string()),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-clone-dest",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let url = ctx.prompt_value?.trim().to_string();
            if url.is_empty() {
                return None;
            }
            // Absolute, so "where did it go" is answered on screen
            // before the clone runs rather than after it.
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let initial = match default_clone_dest(&url) {
                name if name.is_empty() => String::new(),
                name => cwd.join(name).to_string_lossy().into_owned(),
            };
            Some(Effect::OpenPrompt {
                prompt: "Clone into: ".to_string(),
                initial,
                on_submit_action: "action:magit-global-clone-finish".to_string(),
                // The URL rides in the prompt buffer's name — the same
                // way the branch-create wizard carries its base, and
                // magit's blame / rebase / revision modes carry theirs.
                buffer_name: Some(format!("*magit:clone-from:{url}*")),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-clone-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let dest = ctx.prompt_value?.trim().to_string();
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let url = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| path_from_prompt_buffer_name(&n, "*magit:clone-from:"))?;
            if dest.is_empty() {
                return None;
            }
            Some(spawn_clone(url, dest))
        }),
    });

    // MG.34: magit's `M` "Merged" — which merge brought a commit into
    // HEAD.
    //
    // One action answers from two places, the same dual shape MG.23j
    // gave `A` / `_` / `O`: the commit under the cursor when there is
    // one (this row is reachable from a magit log or revision buffer),
    // and the commit picker when there is not — which is the ordinary
    // case, since magit's own home for this row is *file*-dispatch and a
    // file buffer has no commit under the cursor at all.
    //
    // No chord. `M` is mid-screen and `gM` is go-to-middle-of-line, both
    // vim grammar we owe the user; magit binds this as a transient
    // suffix rather than a key for its own reasons, and following that
    // costs the grammar nothing.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-log-merged",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let at_cursor =
                crate::buffer_state::view_for(ctx).and_then(|v| v.commit_at_cursor(ctx.cursor));
            Some(match at_cursor {
                Some(commit) => Effect::OpenSyntheticBuffer {
                    name: crate::magit_revision_mode::merged_buffer_name(&commit),
                    mode_id: "magit-revision-mode".to_string(),
                },
                None => Effect::OpenPicker {
                    source: crate::picker_sources::COMMIT_PICK_SOURCE.to_string(),
                    args: vec!["magit-log-merged".to_string()],
                },
            })
        }),
    });

    // MG.34: magit's `e` "Edit line" — start a rebase that stops on the
    // commit that wrote the line at the cursor, so it can be amended
    // instead of fixed up in a follow-on commit.
    //
    // The cursor line is read here (it is the one thing only this
    // context knows) and the blame is left to the rebase mode, because
    // finding the commit is a `git` call and this handler runs on the
    // actor thread — same split `M` above makes.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-edit-line-commit",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let (_workdir, rel) = active_target(ctx)?;
            Some(Effect::OpenSyntheticBuffer {
                // `git blame -L` counts from 1; the cursor from 0.
                name: crate::magit_rebase_mode::edit_line_buffer_name(
                    ctx.cursor.line + 1,
                    &rel.to_string_lossy(),
                ),
                mode_id: crate::magit_rebase_mode::MagitRebaseMode::mode_id()
                    .as_str()
                    .to_string(),
            })
        }),
    });

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

    // ── MG.32: the rest of magit's branch transient ──────────────
    //
    // Keys follow magit with evil-collection-magit's remaps applied
    // (`(magit-branch "k" "x" magit-branch-delete)`), so a row lands in
    // the slot muscle memory already expects — MG.23's policy #1.
    //
    // `b` — checkout a branch **or revision**, magit's own wording.
    // MG.52: a PICKER, not a prompt.
    //
    // This asked for free text because it accepts anything `git
    // checkout` does — a tag, a remote ref, a raw SHA. But nobody types
    // a branch name they are not sure exists: a typo here is reported
    // by git long after the keystroke that caused it, and the branch
    // the user wanted was on a list the editor could have shown.
    //
    // The `-finish` handler below is kept and still reachable through
    // `:magit-checkout <rev>`, which is the scriptable path.
    //
    // **The REVISION picker, not the branch one.** MG.52 first pointed
    // this at `magit-branch` along with every other branch prompt, and
    // that quietly deleted the row: this is magit's `b`
    // (branch/revision) and the submenu also has `l` (local branch),
    // whose whole difference is that `b` reaches a tag, `origin/main`
    // or a SHA and `l` does not. Listing local branches in both made
    // them the same row and made checking out anything else from the
    // menu unreachable. Nothing errored, because
    // `git checkout <local branch>` is a fine command — which is why
    // `the_branch_revision_row_is_not_the_local_branch_row` asserts the
    // two sources differ rather than checking either row alone.
    //
    // `magit-revision` (MG.53.g) is refs + recent commits, which is
    // exactly "anything git can take" minus the typo.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-checkout-rev",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(Effect::OpenPicker {
                source: crate::picker_sources::REVISION_PICK_SOURCE.to_string(),
                args: vec!["magit-checkout".to_string()],
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-checkout-rev-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let rev = ctx.prompt_value?.trim().to_string();
            if rev.is_empty() {
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: revision is empty".to_string(),
                });
            }
            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                let Ok(repo) = Repository::discover(".") else {
                    tracing::error!(target: "lattice_magit", "branch checkout: repo discover failed");
                    return;
                };
                if let Err(e) = lattice_vcs::Branch::checkout(&repo, &rev) {
                    tracing::error!(target: "lattice_magit", "checkout {rev}: {e}");
                }
            }));
            Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: "magit: checking out…".to_string(),
            })
        }),
    });

    // `n` — new branch, NOT checked out. The picker + prompt are `c`'s;
    // only the `checkout` flag differs, so `Branch::create` already
    // covers it and no new vcs surface was needed.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-create-no-checkout",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(Effect::OpenPicker {
                source: crate::picker_sources::BRANCH_CREATE_NO_CHECKOUT_SOURCE.to_string(),
                args: Vec::new(),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-branch-create-no-checkout-finish",
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
                .and_then(|n| {
                    branch_from_prompt_buffer_name(&n, BRANCH_CREATE_NO_CHECKOUT_PROMPT_PREFIX)
                })?;
            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                let Ok(repo) = Repository::discover(".") else {
                    tracing::error!(target: "lattice_magit", "branch create: repo discover failed");
                    return;
                };
                if let Err(e) = lattice_vcs::Branch::create(&repo, &name, false, Some(&base)) {
                    tracing::error!(target: "lattice_magit", "branch create {name} from {base}: {e}");
                }
            }));
            Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: "magit: creating branch…".to_string(),
            })
        }),
    });

    // `m` — rename. Not destructive in MG.12's sense: nothing is
    // discarded, and `Branch::rename` uses `-m` (not `-M`), which
    // REFUSES to overwrite an existing name rather than clobbering it.
    // So it acts directly, like checkout and merge, rather than asking.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-rename",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(Effect::OpenPicker {
                source: crate::picker_sources::BRANCH_RENAME_SOURCE.to_string(),
                args: Vec::new(),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-branch-rename-finish",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            let new_name = ctx.prompt_value?.trim().to_string();
            if new_name.is_empty() {
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: "magit: branch name is empty".to_string(),
                });
            }
            let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let old = ctx
                .services
                .get::<BufferStoreHandle>()?
                .name_for(buffer_id)
                .and_then(|n| branch_from_prompt_buffer_name(&n, BRANCH_RENAME_PROMPT_PREFIX))?;
            if old == new_name {
                // The prompt is pre-filled with the current name, so
                // submitting unchanged is the likeliest accident. git
                // would error; saying nothing happened is kinder.
                return Some(Effect::Echo {
                    level: lattice_grammar::EchoLevel::Info,
                    text: format!("magit: {old} unchanged"),
                });
            }
            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                let Ok(repo) = Repository::discover(".") else {
                    tracing::error!(target: "lattice_magit", "branch rename: repo discover failed");
                    return;
                };
                if let Err(e) = lattice_vcs::Branch::rename(&repo, &old, &new_name) {
                    tracing::error!(target: "lattice_magit", "branch rename {old} -> {new_name}: {e}");
                }
            }));
            Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: "magit: renaming branch…".to_string(),
            })
        }),
    });

    // `x` — delete (magit's `k`). Opens the picker; the picker's accept
    // routes through `:magit-branch-delete <name>`, which raises the
    // MG.12 confirm. The git call lives only in the execute half below,
    // so answering `n` cannot reach it.
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-delete",
        handler: Arc::new(|_ctx: &ActionContext<'_>| {
            Some(Effect::OpenPicker {
                source: crate::picker_sources::BRANCH_DELETE_SOURCE.to_string(),
                args: Vec::new(),
            })
        }),
    });
    contributions.push(ActionHandlerContribution {
        action_name: "action:magit-global-branch-delete-execute",
        handler: Arc::new(|ctx: &ActionContext<'_>| {
            // Always carried: this pair is only ever raised by the
            // ex-command, which names its target. There is no cursor to
            // fall back to — the menu opens from anywhere.
            let name = crate::confirm::carried_target(ctx)?;
            tokio::task::spawn(tokio::task::spawn_blocking(move || {
                let Ok(repo) = Repository::discover(".") else {
                    tracing::error!(target: "lattice_magit", "branch delete: repo discover failed");
                    return;
                };
                if let Err(e) = lattice_vcs::Branch::delete(&repo, &name) {
                    tracing::error!(target: "lattice_magit", "branch delete {name}: {e}");
                }
            }));
            Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: "magit: deleting branch…".to_string(),
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
/// MG.23a: the file a `C-c f` item acts on — **the argument if one was
/// supplied, else the visited file**.
///
/// `C-c f` supplies none, so it keeps acting on the buffer you were in
/// when you opened it: no "which file?" prompt, which is the one
/// deliberate deviation from magit (magit prompts, defaulting to the
/// current file). `:magit-other-file-dispatch` supplies one, which is
/// how a stand-alone invocation names a file it is not visiting.
///
/// The argument is repo-relative and the repository is discovered from
/// the working directory — the same resolution every other repo-level
/// magit action uses. An empty argument counts as absent, because a
/// transient argument left at its default renders as an empty string.
///
/// This is also the seam a future universal-prefix would use: it would
/// set the same argument rather than needing a mechanism of its own.
fn active_target(ctx: &ActionContext<'_>) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    match ctx.arg_str(FILE_TARGET_SLOT) {
        Some(rel) => {
            let workdir = crate::workdir::magit_workdir()?;
            Some((workdir, std::path::PathBuf::from(rel)))
        }
        None => active_file(ctx),
    }
}

/// Slot of the optional `file` argument in every `C-c f` action's
/// `args_schema`. One constant so the schema and the reader cannot
/// disagree about which slot carries the target.
pub(crate) const FILE_TARGET_SLOT: usize = 0;

fn active_file(ctx: &ActionContext<'_>) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
    let path = ctx
        .services
        .get::<BufferStoreHandle>()?
        .path_for(buffer_id)?;
    // B3: `gix::discover` requires a directory, and `path` is the file
    // — `workdir_for_file` is the form that knows that.
    crate::workdir::workdir_for_file(&path)
}

/// Parse the base branch name stashed in a branch-create prompt
/// buffer's synthetic name (`*magit:branch-create-from:<base>*`).
/// `None` for any other buffer name — the empty-base case
/// (`*magit:branch-create-from:*`) is also rejected since an empty
/// base is never a valid ref.
fn base_branch_from_prompt_buffer_name(buffer_name: &str) -> Option<String> {
    branch_from_prompt_buffer_name(buffer_name, BRANCH_CREATE_PROMPT_PREFIX)
}

/// The prompt-buffer-name prefixes the branch flows carry their target
/// in. **Constants rather than literals**, because the writer
/// (`picker_sources`) and the reader (the finish handlers) live in
/// different files: a name format load-bearing in two places, spelled
/// twice, is exactly the writer/reader drift that left every magit-stash
/// chord dead until MG.15.
pub(crate) const BRANCH_CREATE_PROMPT_PREFIX: &str = "*magit:branch-create-from:";
pub(crate) const BRANCH_CREATE_NO_CHECKOUT_PROMPT_PREFIX: &str =
    "*magit:branch-create-nocheckout-from:";
pub(crate) const BRANCH_RENAME_PROMPT_PREFIX: &str = "*magit:branch-rename:";

/// MG.32: one parser for every `*magit:…:<branch>*` prompt-buffer name.
///
/// Generalised from `base_branch_from_prompt_buffer_name` rather than
/// copied per flow — three near-identical strip-prefix/strip-suffix
/// parsers is the shape where a fix lands in one and misses the others.
fn branch_from_prompt_buffer_name(buffer_name: &str, prefix: &str) -> Option<String> {
    let s = buffer_name.strip_prefix(prefix)?;
    let s = s.strip_suffix("*")?;
    (!s.is_empty()).then(|| s.to_string())
}

/// Test-only window onto [`branch_from_prompt_buffer_name`] so
/// `picker_sources`' round-trip guard exercises the REAL reader.
///
/// The point of that guard is that the writer and the reader live in
/// different modules; asserting against a re-implementation here would
/// prove only that the copy agrees with itself.
#[cfg(test)]
pub(crate) fn branch_from_prompt_buffer_name_for_test(
    buffer_name: &str,
    prefix: &str,
) -> Option<String> {
    branch_from_prompt_buffer_name(buffer_name, prefix)
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
    /// MG.23k: a value that must be **joined** to its argument rather
    /// than passed as a separate token.
    ///
    /// Not a stylistic variant — git rejects the separated form for
    /// some options and accepts it for others. `git log --author x`
    /// works; `git diff -U 3` and `git diff --unified 3` are both
    /// errors, because a long option's value needs `=` and `-U`'s
    /// needs gluing on. So the argument carries its own joiner
    /// (`"--unified="`) and the value is appended to it. Verified
    /// against real git rather than assumed — the separated form was
    /// tried first and rejected.
    ValueJoined {
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
/// MG.41c: where a remote operation sends or takes its refs.
///
/// Magit's push / pull / fetch menus are **one operation with several
/// destinations**, not several operations — which is why a single
/// unlabelled "push" row was the wrong shape. Modelling the
/// destination as data means push gains six rows and one handler
/// rather than six handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteTarget {
    /// Whatever git itself resolves with no destination argument:
    /// `branch.<name>.pushRemote`, then `remote.pushDefault`, then
    /// `branch.<name>.remote`. Magit's `p`.
    Configured,
    /// The branch's `@{upstream}`, resolved explicitly. Magit's `u`.
    ///
    /// Distinct from [`Self::Configured`] whenever `pushRemote` and
    /// the upstream differ — the triangular-workflow case the two rows
    /// exist to separate. Git has no single-token spelling for it, so
    /// this is resolved with a `rev-parse` at run time.
    Upstream,
    /// Every configured remote (`--all`). Magit's `a`, fetch only.
    AllRemotes,
    /// Every tag (`--tags`). Magit's `t`, push only.
    AllTags,
    /// A destination the user types — a remote, a branch, a refspec, a
    /// tag. Magit's `e` / `o` / `r` / `T`, which differ only in what
    /// they prompt for.
    Prompted,
}

impl RemoteTarget {
    /// Extra argv this target contributes, appended after the flags.
    ///
    /// `resolved` carries the value a [`Self::Prompted`] target asked
    /// for, or the `remote branch` pair a [`Self::Upstream`] lookup
    /// produced. Returning a `Vec` rather than an `Option<String>` is
    /// what lets `Upstream` expand to two tokens.
    pub fn argv(self, resolved: Option<&str>) -> Vec<String> {
        match self {
            Self::Configured => Vec::new(),
            Self::AllRemotes => vec!["--all".to_string()],
            Self::AllTags => vec!["--tags".to_string()],
            // Both carry a caller-resolved value; an empty one
            // contributes nothing rather than an empty argument git
            // would read as a real (empty) ref.
            Self::Upstream | Self::Prompted => resolved
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|v| v.split_whitespace().map(str::to_string).collect())
                .unwrap_or_default(),
        }
    }
}

/// MG.41c: resolve `@{upstream}` into the `remote branch` pair a push
/// destination needs.
///
/// `git push` has no `@{upstream}` spelling, so magit computes it and
/// so must this. Returns `None` when the branch has no upstream — the
/// caller reports that rather than pushing somewhere unintended, which
/// is the whole risk this row carries.
pub fn resolve_upstream(workdir: &std::path::Path) -> Option<String> {
    let full = run_remote_op(
        workdir,
        &[
            "rev-parse".to_string(),
            "--abbrev-ref".to_string(),
            "--symbolic-full-name".to_string(),
            "@{upstream}".to_string(),
        ],
    )
    .ok()?;
    // `origin/main` -> `origin main`. A remote name cannot contain `/`,
    // but a BRANCH can (`origin/feature/x`), so split once from the
    // left and keep the remainder whole.
    let (remote, branch) = full.trim().split_once('/')?;
    if remote.is_empty() || branch.is_empty() {
        return None;
    }
    Some(format!("{remote} {branch}"))
}

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
        // `--ff-only` stays the default: a pull that can create a merge
        // commit behind your back is the wrong default. `--rebase`
        // REPLACES it (see `argv`) — it solves the same problem a
        // different way, and git rejects the two together.
        args: &["pull", "--ff-only"],
        flags: &[
            RemoteFlag {
                name: "rebase",
                arg: "--rebase",
                key: "-r",
                doc: "Rebase local commits onto the fetched head instead of merging",
                kind: RemoteArgKind::Flag,
            },
            RemoteFlag {
                name: "autostash",
                arg: "--autostash",
                key: "-a",
                doc: "Stash local changes for the pull and reapply them after",
                kind: RemoteArgKind::Flag,
            },
        ],
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
            // MG.41c: magit offers bare `--force` on `-F`. Lattice
            // deliberately does NOT, and that predates this slice —
            // `force_push_uses_force_with_lease` pins its absence
            // because the difference is whether a colleague's commits
            // survive when the remote moved under you.
            // `--force-with-lease` refuses in exactly that case, so the
            // menu is strictly safer and loses nothing a user cannot
            // still do from a shell. Matching magit key-for-key does
            // not extend to re-adding a footgun someone removed on
            // purpose.
            RemoteFlag {
                name: "no-verify",
                arg: "--no-verify",
                key: "-h",
                doc: "Skip the pre-push hook",
                kind: RemoteArgKind::Flag,
            },
            RemoteFlag {
                name: "dry-run",
                arg: "--dry-run",
                key: "-n",
                doc: "Show what would be pushed without sending anything",
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
            // MG.41c: APPENDED, not inserted. The slice order IS the
            // `args_schema` order, so adding a flag mid-table shifts
            // every later slot and silently re-points existing `:`-line
            // positional args at the wrong toggle.
            RemoteFlag {
                name: "tags",
                arg: "--tags",
                key: "-t",
                doc: "Fetch all tags as well as the branches being fetched",
                kind: RemoteArgKind::Flag,
            },
        ],
    };
    /// MG.34: the way out of a rebase that stopped.
    ///
    /// `magit-edit-line-commit` marks a commit `edit`, which is only
    /// useful if the rebase can be resumed afterwards — and until this
    /// slice the ONLY sequencer control was `C-c C-k` inside a todo
    /// buffer, so a stopped rebase left the repository in a state the
    /// editor could not finish. Shipping the `edit` row without these
    /// would have been shipping a trap.
    ///
    /// `GIT_EDITOR` is forced to `true` by [`run_remote_op`] for the
    /// same reason `run_rebase` does it: `--continue` opens the commit
    /// message in `$EDITOR`, and an editor we cannot drive would hang
    /// the task forever.
    // MG.42-E4: the cherry-pick / revert sequencer controls. Both
    // sequences stop on conflict and need the same three ways out;
    // they are separate consts rather than one shared set because
    // `git cherry-pick --continue` and `git revert --continue` are
    // different commands and each errors during the other's sequence.
    pub const CHERRY_PICK_CONTINUE: Self = Self {
        what: "cherry-pick --continue",
        args: &["cherry-pick", "--continue"],
        flags: &[],
    };
    pub const CHERRY_PICK_SKIP: Self = Self {
        what: "cherry-pick --skip",
        args: &["cherry-pick", "--skip"],
        flags: &[],
    };
    pub const CHERRY_PICK_ABORT: Self = Self {
        what: "cherry-pick --abort",
        args: &["cherry-pick", "--abort"],
        flags: &[],
    };
    pub const REVERT_CONTINUE: Self = Self {
        what: "revert --continue",
        args: &["revert", "--continue"],
        flags: &[],
    };
    pub const REVERT_SKIP: Self = Self {
        what: "revert --skip",
        args: &["revert", "--skip"],
        flags: &[],
    };
    pub const REVERT_ABORT: Self = Self {
        what: "revert --abort",
        args: &["revert", "--abort"],
        flags: &[],
    };
    /// Conclude a merge stopped on a conflict. Equivalent to
    /// committing the prepared merge message once the index is clean;
    /// git refuses it while unmerged paths remain, which is the right
    /// answer and is reported rather than swallowed.
    pub const MERGE_CONTINUE: Self = Self {
        what: "merge --continue",
        args: &["merge", "--continue"],
        flags: &[],
    };
    /// Throw the merge away and restore the branch.
    pub const MERGE_ABORT: Self = Self {
        what: "merge --abort",
        args: &["merge", "--abort"],
        flags: &[],
    };
    pub const REBASE_CONTINUE: Self = Self {
        what: "rebase --continue",
        args: &["rebase", "--continue"],
        flags: &[],
    };
    pub const REBASE_SKIP: Self = Self {
        what: "rebase --skip",
        args: &["rebase", "--skip"],
        flags: &[],
    };
    /// The abort `C-c C-k` runs, reachable when there is no todo buffer
    /// open — which is the case once the rebase has actually started.
    pub const REBASE_ABORT: Self = Self {
        what: "rebase --abort",
        args: &["rebase", "--abort"],
        flags: &[],
    };

    /// MG.37: the notes operations with no argument. `spawn_remote_op`'s
    /// shape — one bounded argv, off-thread, notify — fits them for the
    /// same reason it fits the rebase sequencer.
    pub const NOTES_PRUNE: Self = Self {
        what: "notes prune",
        args: &["notes", "prune"],
        flags: &[RemoteFlag {
            name: "dry-run",
            arg: "--dry-run",
            key: "-n",
            doc: "Report what would be dropped without dropping it",
            kind: RemoteArgKind::Flag,
        }],
    };
    pub const NOTES_MERGE_COMMIT: Self = Self {
        what: "notes merge --commit",
        args: &["notes", "merge", "--commit"],
        flags: &[],
    };
    pub const NOTES_MERGE_ABORT: Self = Self {
        what: "notes merge --abort",
        args: &["notes", "merge", "--abort"],
        flags: &[],
    };

    /// MG.39: the way out of a `git am` that stopped.
    ///
    /// `am` stops on a patch that does not apply, exactly as `rebase`
    /// stops on `edit` — and for the same reason as MG.34's sequencer
    /// commands, shipping the apply without these would leave the
    /// repository in a state the editor cannot finish.
    pub const AM_CONTINUE: Self = Self {
        what: "am --continue",
        args: &["am", "--continue"],
        flags: &[],
    };
    pub const AM_SKIP: Self = Self {
        what: "am --skip",
        args: &["am", "--skip"],
        flags: &[],
    };
    pub const AM_ABORT: Self = Self {
        what: "am --abort",
        args: &["am", "--abort"],
        flags: &[],
    };

    /// MG.23b: magit's `S` — stage every tracked modification at once.
    ///
    /// `add --update`, matching `magit-stage-modified`: tracked changes
    /// only. Untracked files are deliberately NOT swept in — "stage
    /// everything" quietly adding a file you never told git about is
    /// how build artefacts and secrets get committed. Magit reaches
    /// that behaviour behind a prefix argument, which is exactly the
    /// deferred `<C-u>` work; until then `s` on the Untracked entry in
    /// magit-status is the explicit path.
    pub const STAGE_ALL: Self = Self {
        what: "stage all",
        args: &["add", "--update"],
        flags: &[],
    };
    /// MG.23b: magit's `U` — unstage everything.
    ///
    /// A bare `git reset`: the index goes back to HEAD and the working
    /// tree is untouched, so nothing is lost and every change is still
    /// there to re-stage. That is why it does not ask, per §12.13's
    /// no-confirm set for index-only work — the blast radius is wider
    /// than one file but it is still fully reversible.
    pub const UNSTAGE_ALL: Self = Self {
        what: "unstage all",
        args: &["reset", "--quiet"],
        flags: &[],
    };
    /// MG.41d: magit's `x` — stash everything but leave the index
    /// staged, so a partially-staged commit can be tried in isolation.
    pub const STASH_KEEP_INDEX: Self = Self {
        what: "stash --keep-index",
        args: &["stash", "push", "--keep-index"],
        flags: &[],
    };
    /// MG.41d: magit's `i` — stash only what is staged.
    pub const STASH_STAGED: Self = Self {
        what: "stash --staged",
        args: &["stash", "push", "--staged"],
        flags: &[],
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
        // MG.41c: `--rebase` REPLACES the default `--ff-only` rather
        // than joining it — git rejects the pair outright, so leaving
        // both in would make the `-r` row fail every time instead of
        // doing the obvious thing. Rebase serves the same purpose the
        // default was protecting (no surprise merge commit), so
        // dropping it here loses no safety.
        if self.rebase_selected(args) {
            argv.retain(|a| a != "--ff-only");
        }
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
                RemoteArgKind::ValueJoined { .. } => {
                    if let Some(lattice_grammar::ArgValue::String(v)) = slot
                        && !v.is_empty()
                    {
                        argv.push(format!("{}{v}", flag.arg));
                    }
                }
            }
        }
        argv
    }

    /// Is the `--rebase` toggle on? Looked up by NAME rather than by
    /// slot index so it stays correct if the flag table is reordered.
    fn rebase_selected(&self, args: &lattice_grammar::Args) -> bool {
        self.flags
            .iter()
            .position(|f| f.name == "rebase")
            .and_then(|i| args.as_list().and_then(|l| l.get(i)))
            .is_some_and(|v| matches!(v, lattice_grammar::ArgValue::Bool(true)))
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
                RemoteArgKind::ValueJoined { .. } => {
                    if !v.is_empty() {
                        out.push(' ');
                        out.push_str(flag.arg);
                        out.push_str(&v);
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
                    RemoteArgKind::Value { .. } | RemoteArgKind::ValueJoined { .. } => {
                        lattice_grammar::ArgKind::String
                    }
                };
                lattice_grammar::ArgSpec::optional(f.name, kind, f.doc)
            })
            .collect()
    }
}

/// MG.23c1: a repo-level operation whose single argument the user
/// types.
///
/// The shape is the branch-create wizard's, generalised: the menu row
/// (or chord) returns [`Effect::OpenPrompt`], and the `-finish` action
/// named as its submit target does the work with `ctx.prompt_value`.
/// Two actions per operation, no transient state, and the ex-command
/// form skips the prompt entirely by taking the value as an argument —
/// which is what makes these scriptable rather than menu-only.
/// MG.42-E3: the first value of a two-input operation, waiting for the
/// second prompt to be answered.
///
/// A single slot is sufficient and cannot mis-pair. The second prompt
/// is only ever opened BY the first's finish handler, so a read is
/// always preceded by the matching write; and the second finish
/// `take()`s, so a value is consumed once. A cancelled second prompt
/// leaves a stale value, which the next chain's first step overwrites
/// before anything reads it.
static PENDING_FIRST_INPUT: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// MG.43d: the cherry-move rows carry their resolved commit across
/// the branch prompt through the same slot `two_input_op!` uses.
pub(crate) fn stash_pending_commit(value: String) {
    stash_first_input(value);
}

/// `prompt_for`, for the half of a cherry-move row that lives in
/// `magit_core_mode`.
pub(crate) fn prompt_for_pub(prompt: &str, finish_action: &str) -> Effect {
    prompt_for(prompt, finish_action)
}

fn stash_first_input(value: String) {
    if let Ok(mut slot) = PENDING_FIRST_INPUT.lock() {
        *slot = Some(value);
    }
}

fn take_first_input() -> Option<String> {
    PENDING_FIRST_INPUT.lock().ok().and_then(|mut s| s.take())
}

fn prompt_for(prompt: &str, finish_action: &str) -> Effect {
    prompt_seeded(prompt, finish_action, String::new())
}

/// [`prompt_for`] with the input pre-filled — for a value that has an
/// obvious default the user will usually accept and occasionally edit,
/// which is what magit does for `init`'s directory.
fn prompt_seeded(prompt: &str, finish_action: &str, initial: String) -> Effect {
    Effect::OpenPrompt {
        prompt: prompt.to_string(),
        initial,
        on_submit_action: finish_action.to_string(),
        buffer_name: None,
    }
}

/// Run a one-shot git command off the actor thread, echoing what was
/// asked for and logging what happened.
///
/// The dynamic-argv peer of [`spawn_remote_op`], whose `RemoteOp` argv
/// is static. Same discipline: the echo returns synchronously and the
/// real outcome lands in `*messages*` via `tracing`, because there is
/// no synchronous path back from a detached task.
/// MG.41g: the event bus, captured at install so a spawned task can
/// report completion without a handle threaded through every caller.
///
/// A `OnceLock` rather than a parameter deliberately. `spawn_git` has
/// no `ActionContext` to thread anything through, which is precisely
/// why the previous `Option<NotificationStoreHandle>` parameter was
/// forgotten in five of ten spawners. Set once, at install.
static EVENT_BUS: std::sync::OnceLock<std::sync::Arc<lattice_runtime::EventBus>> =
    std::sync::OnceLock::new();

pub(crate) fn set_event_bus(bus: std::sync::Arc<lattice_runtime::EventBus>) {
    let _ = EVENT_BUS.set(bus);
}

/// The bus `finish_task` publishes on, for modes that need to LISTEN
/// to it. `None` in a harness that never installed one.
pub(crate) fn event_bus() -> Option<std::sync::Arc<lattice_runtime::EventBus>> {
    EVENT_BUS.get().cloned()
}

/// Does this event mean a magit view is now stale?
///
/// Only magit's own work does. An LSP request or a compilation
/// finishing is also a `BackgroundTaskFinished`, and neither says
/// anything about the repository — refreshing on those would run
/// `git status` every time a build ended.
///
/// Deliberately keyed on the event's `source` rather than its `label`:
/// labels are human sentences that change with wording, and every
/// magit spawner already stamps the same source.
pub(crate) fn invalidates_a_magit_view(event: &lattice_protocol::event::Event) -> bool {
    matches!(
        event,
        lattice_protocol::event::Event::BackgroundTaskFinished { source, .. }
            if source == "magit"
    )
}

/// MG.41g: report a finished background operation — log **and**
/// publish, in one call.
///
/// The two are deliberately not separable: a spawner that logged but
/// did not publish is exactly the silent-completion bug this replaces,
/// and making them one call means the failure mode requires actively
/// skipping the helper rather than merely forgetting an argument.
///
/// magit never mentions notifications. The notification layer is one
/// subscriber on `BackgroundTaskFinished`; LSP, compilation, or a
/// plugin get the same treatment by publishing the same event.
pub(crate) fn finish_task(label: &str, result: Result<String, String>) {
    use lattice_protocol::event::{Event, TaskOutcome};
    let outcome = match &result {
        Ok(out) => {
            // `debug!`, not `info!`: the notification tees itself to
            // `*messages*`, and two lines saying the same thing is the
            // flooding the diagnostic-logging rule warns about.
            tracing::debug!(target: "lattice_magit", "magit: {label} succeeded: {out}");
            TaskOutcome::Succeeded {
                summary: first_line(out),
            }
        }
        Err(err) => {
            // `error!` keeps git's FULL stderr; the published message
            // is the truncated one-liner a notification can show.
            tracing::error!(target: "lattice_magit", "magit: {label} failed: {err}");
            TaskOutcome::Failed {
                message: first_line(err),
            }
        }
    };
    if let Some(bus) = EVENT_BUS.get() {
        bus.publish(Event::BackgroundTaskFinished {
            source: "magit".to_string(),
            label: label.to_string(),
            outcome,
        });
    }
}

/// Git output is often many lines; a notification shows one.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_string()
}

/// MG.42-E2: one step of a composite operation.
pub struct GitStep {
    /// Names this step in a failure report — "fixup commit", not the
    /// whole argv, which is too long for a notification.
    pub name: &'static str,
    pub argv: Vec<String>,
}

/// MG.42-E2: run several git invocations in order as ONE operation.
///
/// Two properties, both load-bearing:
///
/// 1. **Aborts on the first failure.** Several magit operations are
///    compositions where a half-done result is worse than none — a
///    snapshot whose `stash apply` never ran is silently just a stash,
///    and the user's working tree is empty when they expected it
///    restored. Continuing past a failed step manufactures exactly
///    that.
/// 2. **Reports once.** One logical operation produces one
///    `BackgroundTaskFinished`, naming the step that failed rather
///    than the whole sequence. Per-step reporting would turn a
///    two-command operation into two notifications, which is how a
///    notification surface becomes noise.
pub fn spawn_git_sequence(label: &'static str, steps: Vec<GitStep>) -> Effect {
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    let shown = steps
        .first()
        .map(|s| format!("git {}", s.argv.join(" ")))
        .unwrap_or_else(|| label.to_string());
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let mut last = String::new();
            for step in steps {
                match run_remote_op(&workdir, &step.argv) {
                    Ok(out) => last = out,
                    // The step name, not the argv: a notification is one
                    // line and "fixup commit failed" localises the
                    // problem better than a truncated command.
                    Err(e) => return Err(format!("{}: {e}", step.name)),
                }
            }
            Ok(last)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        finish_task(label, result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {shown}"),
    }
}

/// MG.43c: run a one-commit interactive rebase off the actor thread.
///
/// `message` is `Some` only for `reword`, where it is what git's
/// reword step writes — see `run_rebase_with_message`.
pub fn spawn_rebase_verb(label: &'static str, verb: &'static str, commit: &str) -> Effect {
    spawn_rebase_verb_with(label, verb, commit, None)
}

pub fn spawn_rebase_verb_with(
    label: &'static str,
    verb: &'static str,
    commit: &str,
    message: Option<String>,
) -> Effect {
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    let commit = commit.to_string();
    let shown = format!("git rebase ({verb} {commit})");
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            crate::magit_rebase_mode::rebase_one_commit(&workdir, &commit, verb, message.as_deref())
                .map(|()| String::new())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        finish_task(label, result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {shown}"),
    }
}

/// MG.43d: run a multi-step operation whose LATER steps depend on
/// state only discoverable part-way through.
///
/// `spawn_git_sequence` takes its steps up front, which cannot express
/// "create the branch only if it is missing" or "compare-and-swap the
/// ref only if the commit was at the tip". Computing those on the
/// actor thread would be git I/O on a keystroke, so the whole
/// operation runs inside one `spawn_blocking` and reports once,
/// exactly like a sequence does.
pub fn spawn_computed<F>(label: &'static str, shown: String, f: F) -> Effect
where
    F: FnOnce(&std::path::Path) -> Result<(), String> + Send + 'static,
{
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || f(&workdir).map(|()| String::new()))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        finish_task(label, result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {shown}"),
    }
}

pub fn spawn_git(argv: Vec<String>, what: &str) -> Effect {
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    let shown = format!("git {}", argv.join(" "));
    let logged = shown.clone();
    let what = what.to_string();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_remote_op(&workdir, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        // MG.41g: was log-only, so every `spawn_git` caller finished
        // invisibly.
        let _ = logged;
        finish_task(&what, result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {shown}"),
    }
}

// ── MG.39: `w` am / `W` format-patch ────────────────────────────────

/// `git format-patch <range>` — magit's `W` "Create patches".
///
/// Pure. `-o` defaults to the repository root rather than being left to
/// git's cwd: the editor's process directory is not necessarily where
/// the user thinks they are, and a scatter of `.patch` files in an
/// unexpected directory is tedious to undo.
pub(crate) fn format_patch_argv(range: &str, output_dir: Option<&str>) -> Option<Vec<String>> {
    let range = range.trim();
    if range.is_empty() {
        return None;
    }
    let mut argv = vec!["format-patch".to_string()];
    if let Some(dir) = output_dir {
        argv.push("-o".to_string());
        argv.push(dir.to_string());
    }
    argv.push(range.to_string());
    Some(argv)
}

/// `git am <mbox>…` — magit's `w` "Apply patches".
///
/// `--3way` is NOT default. It is magit's `-3` flag, and it changes what
/// happens on a conflict: git falls back to a three-way merge and leaves
/// conflict markers rather than refusing. That is a better outcome only
/// when you expected the patch not to apply cleanly, so it is opt-in —
/// the same judgement `--force-with-lease`-not-`--force` makes on push.
pub(crate) fn am_argv(files: &str, three_way: bool) -> Option<Vec<String>> {
    let files: Vec<&str> = files
        .split_whitespace()
        .filter(|f| !f.starts_with("--"))
        .collect();
    if files.is_empty() {
        return None;
    }
    let mut argv = vec!["am".to_string()];
    if three_way {
        argv.push("--3way".to_string());
    }
    argv.extend(files.iter().map(|f| f.to_string()));
    Some(argv)
}

/// Does the line ask for a three-way apply? Accepts magit's short flag
/// and git's long one, anywhere in the line.
pub(crate) fn am_wants_three_way(line: &str) -> bool {
    line.split_whitespace().any(|w| w == "--3way" || w == "-3")
}

// ── MG.38: `git subtree` ────────────────────────────────────────────

/// One `git subtree` operation.
///
/// Peer of [`CommitOp`] / [`RemoteOp`]: the argv template plus what the
/// operation needs from the user, so the menu row, the ex-command and
/// the prompt all read one definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtreeOp {
    /// Verb for the echo and the notification.
    pub what: &'static str,
    /// The `git subtree` subcommand.
    pub sub: &'static str,
    /// Ex-command name, without the `:`.
    pub ex_command: &'static str,
    /// Does it take a `<repository>` as well as a `<ref>`? `merge` and
    /// `split` do not; `add` / `pull` / `push` do.
    pub takes_repo: bool,
    /// Does it take a `<ref>` at all? `split` does not.
    pub takes_ref: bool,
}

impl SubtreeOp {
    pub const ADD: Self = Self {
        what: "subtree add",
        sub: "add",
        ex_command: "magit-subtree-add",
        takes_repo: true,
        takes_ref: true,
    };
    pub const MERGE: Self = Self {
        what: "subtree merge",
        sub: "merge",
        ex_command: "magit-subtree-merge",
        takes_repo: false,
        takes_ref: true,
    };
    pub const PULL: Self = Self {
        what: "subtree pull",
        sub: "pull",
        ex_command: "magit-subtree-pull",
        takes_repo: true,
        takes_ref: true,
    };
    pub const PUSH: Self = Self {
        what: "subtree push",
        sub: "push",
        ex_command: "magit-subtree-push",
        takes_repo: true,
        takes_ref: true,
    };
    pub const SPLIT: Self = Self {
        what: "subtree split",
        sub: "split",
        ex_command: "magit-subtree-split",
        takes_repo: false,
        takes_ref: false,
    };

    /// What the prompt asks for, in the order the operation reads them.
    pub fn usage(&self) -> &'static str {
        match (self.takes_repo, self.takes_ref) {
            (true, true) => "<prefix> <repository> <ref>",
            (false, true) => "<prefix> <ref>",
            _ => "<prefix>",
        }
    }
}

/// Build the argv for `op` from one whitespace-separated line.
///
/// Pure, so the flag shape is pinned without running a subtree — and
/// `--prefix=` in particular, which is the one argument every subtree
/// operation requires and the one git errors on last, after doing work.
///
/// `--squash` is accepted as a trailing word on `add` and `pull`, which
/// is where magit offers it. Returns `None` when the line does not carry
/// what the operation needs, so the caller can print `op.usage()`
/// instead of letting git fail with its own less specific message.
pub(crate) fn subtree_argv(op: SubtreeOp, line: &str) -> Option<Vec<String>> {
    let mut words: Vec<&str> = line.split_whitespace().collect();
    let squash = matches!(words.last(), Some(&"--squash") | Some(&"squash"))
        && matches!(op.sub, "add" | "pull");
    if squash {
        words.pop();
    }
    let wanted = 1 + usize::from(op.takes_repo) + usize::from(op.takes_ref);
    if words.len() != wanted {
        return None;
    }
    let mut argv = vec![
        "subtree".to_string(),
        op.sub.to_string(),
        format!("--prefix={}", words[0]),
    ];
    if squash {
        argv.push("--squash".to_string());
    }
    argv.extend(words[1..].iter().map(|w| w.to_string()));
    Some(argv)
}

/// Run a subtree operation off the actor thread, reporting completion.
///
/// A notification rather than `spawn_git`'s fire-time echo: every
/// subtree operation rewrites history or touches a remote, so "did it
/// work" is the question, and an echo written before it starts cannot
/// answer it.
pub fn spawn_subtree_op(op: SubtreeOp, line: &str) -> Effect {
    let Some(argv) = subtree_argv(op, line) else {
        return Effect::Echo {
            level: lattice_grammar::EchoLevel::Error,
            text: format!("magit: usage — :{} {}", op.ex_command, op.usage()),
        };
    };
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    let what = op.what;
    let shown = argv.join(" ");
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_remote_op(&workdir, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        // MG.41g: publish rather than hold a notification handle.
        let _ = shown;
        finish_task(what, result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {what}…"),
    }
}

// ── MG.37: the notes operations that are not a buffer ───────────────

/// `git notes merge <ref> [--strategy=<s>]`, parsed from one line.
///
/// Pure, so the flag shape is pinned without running a merge — the same
/// reason `clone_argv` / `tag_argv` / `log_merged_argv` are.
///
/// The strategy is optional and trailing (`<ref> ours`). An unrecognised
/// second word is an ERROR rather than a ref with a typo'd strategy
/// silently merged manually — `NoteMergeStrategy::parse` refuses, and
/// this returns `None` so the caller can say so.
pub(crate) fn note_merge_argv(spec: &str) -> Option<Vec<String>> {
    let spec = spec.trim();
    let (git_ref, strategy) = match spec.split_once(char::is_whitespace) {
        Some((r, s)) => (r, lattice_vcs::NoteMergeStrategy::parse(s)?),
        None => (spec, lattice_vcs::NoteMergeStrategy::Manual),
    };
    if git_ref.is_empty() {
        return None;
    }
    Some(vec![
        "notes".to_string(),
        "merge".to_string(),
        format!("--strategy={}", strategy.as_str()),
        git_ref.to_string(),
    ])
}

/// Remove one commit's note, off the actor thread.
pub(crate) fn spawn_note_remove(commit: String) -> Effect {
    spawn_git(
        vec![
            "notes".to_string(),
            "remove".to_string(),
            "--ignore-missing".to_string(),
            commit.clone(),
        ],
        "notes remove",
    )
}

/// Prune unreachable notes, reporting the outcome.
///
/// A notification rather than `spawn_git`'s echo: prune's whole result
/// is *what it removed*, and an echo written at fire time cannot carry
/// it.
pub(crate) fn spawn_note_prune() -> Effect {
    spawn_remote_op(RemoteOp::NOTES_PRUNE, &lattice_grammar::Args::None)
}

/// Merge a notes ref into the current one.
pub(crate) fn spawn_note_merge(spec: &str) -> Effect {
    let Some(argv) = note_merge_argv(spec) else {
        return Effect::Echo {
            level: lattice_grammar::EchoLevel::Error,
            text: "magit: usage — <notes-ref> [manual|ours|theirs|union|cat_sort_uniq]".to_string(),
        };
    };
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_remote_op(&workdir, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        // MG.41g: publish rather than post.
        finish_task("notes merge", result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: merging notes from {spec}…"),
    }
}

// ── MG.36: `C` clone ────────────────────────────────────────────────

/// The argv for cloning `url` into `dest`.
///
/// Pure, so the flags are pinned without running a clone — the same
/// shape `tag_argv` / `merge_argv` / `log_merged_argv` have.
///
/// `--` before the operands: a URL or a destination beginning with `-`
/// would otherwise be read as an option. That is not hypothetical for
/// the destination, which the user types.
pub(crate) fn clone_argv(url: &str, dest: &str) -> Vec<String> {
    vec![
        "clone".to_string(),
        "--".to_string(),
        url.to_string(),
        dest.to_string(),
    ]
}

/// The directory name `git clone <url>` would pick on its own.
///
/// Both URL shapes have to work, because both are what people paste:
/// `https://host/owner/repo.git` splits on `/`, and
/// `git@host:owner/repo.git` puts the interesting part after a `:`.
/// Trailing slashes are common in copied URLs and `.git` is usual on
/// the SSH form; neither belongs in a directory name.
///
/// Empty when nothing usable is left — the caller then asks rather than
/// pre-filling a prompt with a name it invented.
pub(crate) fn default_clone_dest(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let last = trimmed.rsplit(['/', ':']).next().unwrap_or_default().trim();
    last.strip_suffix(".git").unwrap_or(last).to_string()
}

/// Clone `url` into `dest`, off the actor thread.
///
/// Not [`spawn_git`], which runs in `magit_workdir()` — that would put
/// the clone *inside* the repository you are already in, which is
/// almost never what anyone means, and is silent when it happens.
/// `dest` is resolved against the process's own directory instead, and
/// the prompt pre-fills it absolute so the answer to "where did it go"
/// is on screen before the clone starts.
///
/// **The notification says what the clone does NOT do.** Magit shows the
/// new repository's status buffer afterwards; that is not reachable here
/// (`magit_workdir` is process-wide, and there is no `:cd`), so the
/// magit buffers keep pointing at the repository the editor was launched
/// in. Saying so once, at the moment it matters, is the difference
/// between a documented limit and a user concluding the clone failed.
pub fn spawn_clone(url: String, dest: String) -> Effect {
    let argv = clone_argv(&url, &dest);
    let _shown = dest.clone();
    tokio::task::spawn(async move {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let result = tokio::task::spawn_blocking(move || run_remote_op(&cwd, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        // MG.41g: publish rather than post.
        finish_task("clone", result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: cloning into {dest}…"),
    }
}

/// MG.23c1: append `pattern` to the repository's `.gitignore`.
///
/// Not a git command — git has no "add to gitignore" subcommand, so
/// this is a file append, done on `spawn_blocking` like every other
/// blocking call in this crate.
///
/// **Skips a pattern already present**, comparing whole trimmed lines.
/// Ignoring the same path twice is harmless to git but grows the file
/// and makes it harder to read, and the user pressing `i` twice on the
/// same build artefact is an ordinary mistake rather than an intent to
/// duplicate.
pub fn spawn_gitignore(pattern: String) -> Effect {
    let shown = pattern.clone();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let repo = Repository::discover(".").map_err(|e| e.to_string())?;
            let workdir = repo.workdir().ok_or("bare repository")?.to_path_buf();
            let path = workdir.join(".gitignore");
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            let Some(out) = gitignore_append(&existing, &pattern) else {
                return Ok(format!("{pattern} was already ignored"));
            };
            std::fs::write(&path, out).map_err(|e| e.to_string())?;
            Ok(format!("ignoring {pattern}"))
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        // MG.41g: was log-only.
        finish_task("update .gitignore", result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: ignoring {shown}"),
    }
}

/// MG.23d: the argv for each file operation.
///
/// Pure and shared, for the reason the MG.23c builders are — the flags
/// are what matters and a test must not have to run git in the
/// repository it lives in.
///
/// `untrack` is `rm --cached`: the file **stays on disk** and only
/// leaves the index, which is why it does not ask. `delete` is a plain
/// `rm` with no `-f`, so git itself refuses to remove a file with
/// uncommitted changes — the confirm is the second line of defence, not
/// the only one.
pub(crate) fn untrack_argv(path: &str) -> Vec<String> {
    vec![
        "rm".into(),
        "--cached".into(),
        "--".into(),
        path.to_string(),
    ]
}

pub(crate) fn delete_argv(path: &str) -> Vec<String> {
    vec!["rm".into(), "--".into(), path.to_string()]
}

pub(crate) fn rename_argv(from: &str, to: &str) -> Vec<String> {
    vec!["mv".into(), "--".into(), from.to_string(), to.to_string()]
}

/// MG.23d2: `git checkout <rev> -- <path>` — the file as it was at
/// `rev`, written over the working-tree copy.
///
/// `--` is load-bearing rather than decorative: without it a path that
/// happens to match a ref name is ambiguous, and git resolves the
/// ambiguity by checking out the *branch*.
pub(crate) fn checkout_file_argv(rev: &str, path: &str) -> Vec<String> {
    vec![
        "checkout".into(),
        rev.to_string(),
        "--".into(),
        path.to_string(),
    ]
}

/// The path a rename prompt is carrying, from its buffer name.
///
/// The prompt buffer is the active one by the time the user submits, so
/// `active_target` would resolve *it* rather than the file being
/// renamed. The name is the carrier — the same trick the branch-create
/// wizard uses for its base branch.
pub(crate) fn rename_source_from_prompt_buffer_name(buffer_name: &str) -> Option<String> {
    path_from_prompt_buffer_name(buffer_name, "*magit:rename:")
}

/// MG.23d2: the same carrier, for the checkout prompt.
pub(crate) fn checkout_target_from_prompt_buffer_name(buffer_name: &str) -> Option<String> {
    path_from_prompt_buffer_name(buffer_name, "*magit:checkout:")
}

/// MG.21g: the bad end of a bisect, carried from the first prompt to
/// the second. Same carrier as rename/checkout — by submit time the
/// prompt buffer is the active one, so nothing else still knows it.
pub(crate) const BISECT_START_PREFIX: &str = "*magit:bisect-start:";

pub(crate) fn bisect_start_buffer_name(bad: &str) -> String {
    format!("{BISECT_START_PREFIX}{bad}*")
}

pub(crate) fn bad_from_bisect_start_buffer_name(buffer_name: &str) -> Option<String> {
    path_from_prompt_buffer_name(buffer_name, BISECT_START_PREFIX)
}

/// Run a bisect operation off the actor thread, then refresh every
/// live magit view.
///
/// A bisect mark moves HEAD, so nothing that reads HEAD is still
/// accurate — the status buffer, an open log, an open diff. There is no
/// synchronous path back from the detached task (§4.6), so the log is
/// the report and the refresh is what the user sees.
fn spawn_bisect(
    ctx: &ActionContext<'_>,
    what: &'static str,
    op: impl FnOnce(&lattice_vcs::Repository) -> lattice_vcs::Result<()> + Send + 'static,
) {
    let Some(views) = ctx.services.get::<crate::buffer_state::MagitViewsHandle>() else {
        return;
    };
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    tokio::task::spawn(async move {
        let wd = workdir.clone();
        let outcome =
            tokio::task::spawn_blocking(move || match lattice_vcs::Repository::discover(&wd) {
                Ok(repo) => op(&repo),
                Err(e) => Err(lattice_vcs::VcsError::Bisect(format!(
                    "no repository at {}: {e}",
                    wd.display()
                ))),
            })
            .await;
        match outcome {
            // MG.41g: bisect marks check out a different commit — a
            // completion the user very much wants to see.
            Ok(Ok(())) => finish_task(&format!("bisect {what}"), Ok(String::new())),
            Ok(Err(e)) => finish_task(&format!("bisect {what}"), Err(e.to_string())),
            Err(e) => finish_task(&format!("bisect {what}"), Err(format!("panicked: {e}"))),
        }
        for view in views.all() {
            let _ = view.refresh();
        }
    });
}

fn path_from_prompt_buffer_name(buffer_name: &str, prefix: &str) -> Option<String> {
    let s = buffer_name.strip_prefix(prefix)?;
    let s = s.strip_suffix('*')?;
    (!s.is_empty()).then(|| s.to_string())
}

/// MG.23c: the argv each prompt-backed operation runs.
///
/// Pure, and separate from [`spawn_git`], for two reasons. The flags
/// are the part worth testing — `--no-edit` on merge is what stops git
/// opening an `$EDITOR` that never appears — and a test that reached
/// them through the spawning path would run **real git against the
/// repository the tests live in**. It would also be the only copy: the
/// action handler and the ex-command both build their argv here rather
/// than each inline.
pub(crate) fn tag_argv(name: &str) -> Vec<String> {
    vec!["tag".into(), name.to_string()]
}

pub(crate) fn init_argv(dir: &str) -> Vec<String> {
    vec!["init".into(), dir.to_string()]
}

/// `--no-edit` for the same reason `revert` passes it: git would open
/// `$EDITOR` for the merge message, and inside lattice that is a wait
/// on a prompt that never appears — a hang `Command::output()` cannot
/// recover from.
pub(crate) fn merge_argv(branch: &str) -> Vec<String> {
    vec!["merge".into(), "--no-edit".into(), branch.to_string()]
}

/// MG.41e: magit's `n` — merge but stop before committing, so the
/// result can be inspected or amended first.
///
/// No `--no-edit`: nothing is committed, so git never opens an editor
/// and the hang that flag exists to avoid cannot happen here.
pub(crate) fn merge_no_commit_argv(branch: &str) -> Vec<String> {
    vec!["merge".into(), "--no-commit".into(), branch.to_string()]
}

/// MG.41e: magit's `s` — take the branch's changes as ONE staged
/// change with no merge commit and no second parent.
pub(crate) fn merge_squash_argv(branch: &str) -> Vec<String> {
    vec!["merge".into(), "--squash".into(), branch.to_string()]
}

/// MG.42-E3: magit's reset `f` — restore ONE path from a commit.
///
/// `checkout <commit> -- <path>`, not `reset`: this replaces the file's
/// content in both index and working tree, which is what "reset a file
/// to that commit" means. A `reset` would move index entries only and
/// leave the file on disk untouched — the same words, a different
/// outcome.
pub(crate) fn reset_file_argv(commit: &str, path: &str) -> Vec<String> {
    vec![
        "checkout".into(),
        commit.to_string(),
        "--".into(),
        path.to_string(),
    ]
}

/// MG.43e: magit's tag `r` — an ANNOTATED release tag.
///
/// `-a` (with `-m`) is what separates this from the plain `t` row: a
/// release tag carries a tagger, a date and a message, and is a real
/// object rather than a pointer. Dropping `-a` would silently produce
/// a lightweight tag that most release tooling ignores.
pub(crate) fn tag_release_argv(name: &str, message: &str) -> Vec<String> {
    vec![
        "tag".into(),
        "-a".into(),
        name.to_string(),
        "-m".into(),
        message.to_string(),
    ]
}

/// MG.43e: magit's tag `p` — drop local tags that are gone from the
/// remote.
///
/// `--prune-tags` implies nothing on its own: it needs `--prune` AND a
/// remote, or git prunes nothing and reports success. Both are
/// therefore explicit here rather than left to config.
pub(crate) fn tag_prune_argv(remote: &str) -> Vec<String> {
    vec![
        "fetch".into(),
        "--prune".into(),
        "--prune-tags".into(),
        remote.to_string(),
    ]
}

/// MG.43e: magit's merge `i` — merge the CURRENT branch into another,
/// then delete the current one.
///
/// The mirror of `a` absorb, which merges another branch into this one
/// and deletes that one. The direction is the whole difference, and
/// getting it backwards deletes the wrong branch — so the steps are
/// spelled out rather than shared with absorb's builder.
///
/// Deletes with `-d`, never `-D`, for absorb's reason: git refuses
/// `-d` on a branch that is not fully merged, so a failed merge leaves
/// the branch intact.
pub(crate) fn merge_into_steps(current: &str, target: &str) -> Vec<GitStep> {
    vec![
        GitStep {
            name: "checkout the target branch",
            argv: vec!["checkout".into(), target.to_string()],
        },
        GitStep {
            name: "merge",
            argv: merge_argv(current),
        },
        GitStep {
            name: "delete the merged branch",
            argv: vec!["branch".into(), "-d".into(), current.to_string()],
        },
    ]
}

/// The branch `HEAD` is on, for operations that act on "this branch".
pub(crate) fn current_branch() -> Option<String> {
    let workdir = crate::workdir::magit_workdir()?;
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(&workdir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let branch = String::from_utf8(out.stdout).ok()?.trim().to_string();
    // Detached HEAD reports `HEAD`, which is not a branch anyone can
    // merge or delete — the caller must decline rather than act on it.
    (!branch.is_empty() && branch != "HEAD").then_some(branch)
}

/// MG.43b: magit's rebase `p` / `u` / `e` — rebase onto a ref.
///
/// **The target is a single revision, not a `remote branch` pair.**
/// `RemoteTarget::Upstream` expands `@{upstream}` to two tokens
/// because `git push` wants `<remote> <branch>`; `git rebase` wants
/// one revision, and would read a second token as the *upstream*
/// argument — silently rebasing a different range. Git resolves
/// `@{upstream}` and `@{push}` natively as revisions, so these rows
/// pass them through untouched rather than reusing that resolution.
pub(crate) fn rebase_onto_argv(target: &str) -> Vec<String> {
    vec!["rebase".into(), target.to_string()]
}

/// MG.43b: magit's rebase `s` — rebase a SUBSET of commits elsewhere.
///
/// `--onto <newbase> <upstream>` replays the commits after `upstream`
/// onto `newbase`. Both arguments are required and the order is not
/// interchangeable: swapping them replays the wrong range onto the
/// wrong base, and git will happily do it.
pub(crate) fn rebase_subset_argv(newbase: &str, upstream: &str) -> Vec<String> {
    vec![
        "rebase".into(),
        "--onto".into(),
        newbase.to_string(),
        upstream.to_string(),
    ]
}

/// MG.43b: magit's rebase `f` — replay, folding in `fixup!` /
/// `squash!` markers.
///
/// `-i` is required even though nothing is edited interactively:
/// `--autosquash` only applies to the generated todo list, which is an
/// interactive-rebase concept. `run_remote_op`'s
/// `GIT_SEQUENCE_EDITOR=true` accepts that list unchanged, which IS
/// autosquash — git has already ordered the lines.
pub(crate) fn rebase_autosquash_argv(upstream: &str) -> Vec<String> {
    vec![
        "rebase".into(),
        "-i".into(),
        "--autosquash".into(),
        upstream.to_string(),
    ]
}

/// MG.42-E3: magit's stash `b` — start a branch from a stash.
///
/// `git stash branch` checks out a new branch at the commit the stash
/// was made from, applies the stash, and drops it on success. Useful
/// exactly when a stash no longer applies to the current HEAD.
pub(crate) fn stash_branch_argv(branch: &str, stash: &str) -> Vec<String> {
    vec![
        "stash".into(),
        "branch".into(),
        branch.to_string(),
        stash.to_string(),
    ]
}

/// MG.42-E2: magit's `Z` / `I` / `W` snapshots — stash, then put it
/// straight back.
///
/// The point is a restore point that costs nothing: the stack gets an
/// entry, the working tree is untouched. `apply`, never `pop` — a pop
/// would remove the very entry the snapshot exists to create.
pub(crate) fn stash_snapshot_steps(extra: &[&str]) -> Vec<GitStep> {
    let mut push: Vec<String> = vec!["stash".into(), "push".into()];
    push.extend(extra.iter().map(|s| (*s).to_string()));
    vec![
        GitStep {
            name: "stash",
            argv: push,
        },
        GitStep {
            name: "restore working tree",
            argv: vec!["stash".into(), "apply".into()],
        },
    ]
}

/// MG.42-E2: magit's `F` / `S` — record a `fixup!` / `squash!` commit
/// and immediately fold it in.
///
/// The rebase base is `<commit>~1`: the fixup has to be replayed
/// alongside the commit it targets, so the rebase must start one
/// before it. `--autostash` because the user is very often
/// mid-edit — without it an instant fixup fails on a dirty tree,
/// which is precisely when people reach for it.
pub(crate) fn instant_squash_steps(kind: &'static str, commit: &str) -> Vec<GitStep> {
    vec![
        GitStep {
            name: "record the marker commit",
            argv: vec![
                "commit".into(),
                "--no-edit".into(),
                format!("--{kind}"),
                commit.to_string(),
            ],
        },
        GitStep {
            name: "autosquash",
            argv: vec![
                "rebase".into(),
                "-i".into(),
                "--autosquash".into(),
                "--autostash".into(),
                format!("{commit}~1"),
            ],
        },
    ]
}

/// MG.42-E2: magit's `a` absorb — merge a branch, then delete it.
///
/// `-d`, never `-D`: git refuses to delete a branch that is not fully
/// merged, so if the merge did not actually take, the branch survives.
/// A forced delete here would destroy the branch precisely in the case
/// where the merge failed.
pub(crate) fn merge_absorb_steps(branch: &str) -> Vec<GitStep> {
    vec![
        GitStep {
            name: "merge",
            argv: merge_argv(branch),
        },
        GitStep {
            name: "delete the merged branch",
            argv: vec!["branch".into(), "-d".into(), branch.to_string()],
        },
    ]
}

/// MG.41e: magit's `k` — delete a tag.
///
/// Local only. Deleting the remote copy is `push --delete`, a
/// different and far more consequential operation that magit also
/// keeps separate.
pub(crate) fn tag_delete_argv(name: &str) -> Vec<String> {
    vec!["tag".into(), "-d".into(), name.to_string()]
}

/// MG.23c: every prompt-backed operation, as (row, finish) pairs.
///
/// **Hand-kept, and honest about it.** A row added here is checked
/// against production — its prompt must target the finish action named,
/// and that action must exist — but a row added to production and *not*
/// here is simply unchecked. Deriving it would mean invoking every
/// contributed handler to see which return `OpenPrompt`, and some of
/// those handlers spawn git; a test that called them would run real
/// commands against the repository it lives in.
///
/// The pairing check below is the compensation: it is what catches the
/// failure that is otherwise silent from the code's side, where a
/// prompt accepts input and does nothing with it.
#[cfg(test)]
pub(crate) const PROMPTED_OPS: &[(&str, &str)] = &[
    ("action:magit-global-tag", "action:magit-global-tag-finish"),
    (
        "action:magit-global-gitignore",
        "action:magit-global-gitignore-finish",
    ),
    (
        "action:magit-global-init",
        "action:magit-global-init-finish",
    ),
];

/// MG.52: rows that open a BRANCH PICKER, paired with the ex-command the
/// picked branch is handed to.
///
/// The peer of [`PROMPTED_OPS`], and the reason it exists: these used to
/// be prompts and moving them left `each_prompt_row_targets_a_finish_action_that_exists`
/// with nothing to say about them. A row that silently stopped opening
/// its picker — or opened one naming a command nobody registered — would
/// otherwise be invisible to the suite.
#[cfg(test)]
pub(crate) const PICKED_BRANCH_OPS: &[(&str, &str)] = &[
    ("action:magit-global-merge", "magit-merge"),
    ("action:magit-global-branch-reset", "magit-branch-reset"),
    // NOT `branch-checkout-rev` — that row is magit's `b`
    // (branch/revision) and takes the REVISION picker, which is the
    // only thing that keeps it distinct from `l`. See
    // `the_branch_revision_row_is_not_the_local_branch_row`.
    // MG.53.a
    (
        "action:magit-global-merge-no-commit",
        "magit-merge-no-commit",
    ),
    ("action:magit-global-merge-squash", "magit-merge-squash"),
    (
        "action:magit-global-rebase-onto-elsewhere",
        "magit-rebase-onto",
    ),
    (
        "action:magit-global-rebase-autosquash",
        "magit-rebase-autosquash",
    ),
    // MG.53.b — the three whose ex-command is not one git call.
    ("action:magit-global-merge-absorb", "magit-merge-absorb"),
    ("action:magit-global-merge-edit", "magit-merge-edit"),
    ("action:magit-global-merge-into", "magit-merge-into"),
];

/// MG.53.d: rows backed by a picker that is NOT the branch one, as
/// `(action, picker source, ex-command)`.
///
/// Kept apart from [`PICKED_BRANCH_OPS`] because the source is the
/// thing worth asserting here — `t p` prunes tags but takes a REMOTE,
/// and pointing it at the tag picker would list the wrong nouns while
/// still running a real command. Only checking "opens some picker"
/// would miss that.
#[cfg(test)]
pub(crate) const PICKED_OTHER_OPS: &[(&str, &str, &str)] = &[
    (
        "action:magit-global-tag-delete",
        crate::picker_sources::TAG_PICK_SOURCE,
        "magit-tag-delete",
    ),
    (
        "action:magit-global-tag-prune",
        crate::picker_sources::REMOTE_PICK_SOURCE,
        "magit-tag-prune",
    ),
    // MG.53.e
    (
        "action:magit-global-note-merge",
        crate::picker_sources::REF_PICK_SOURCE,
        "magit-note-merge",
    ),
];

/// The `.gitignore` content after adding `pattern`, or `None` when it
/// is already ignored.
///
/// Split from [`spawn_gitignore`] so the rule is testable without a
/// repository or a runtime — the same split `classify_line` /
/// `classify_line_text` uses, and the reason the test exercises this
/// rather than a copy of it.
pub(crate) fn gitignore_append(existing: &str, pattern: &str) -> Option<String> {
    if existing.lines().any(|l| l.trim() == pattern.trim()) {
        return None;
    }
    let mut out = existing.to_string();
    // A file not ending in a newline would otherwise glue the new
    // pattern onto the last one, silently ignoring neither.
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(pattern.trim());
    out.push('\n');
    Some(out)
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
/// MG.41c: run a remote op against an explicit [`RemoteTarget`].
///
/// One handler for every destination row. `Upstream` resolves
/// `@{upstream}` on the blocking pool before running — it needs a git
/// call, and doing it here rather than at menu-build time keeps the
/// menu free of I/O.
///
/// A destination that cannot be resolved **aborts rather than falling
/// back** to a bare push. Falling back would silently send refs
/// somewhere the user did not choose, which is the one failure this
/// row must not have.
pub fn spawn_remote_op_to(
    op: RemoteOp,
    args: &lattice_grammar::Args,
    target: RemoteTarget,
    prompted: Option<String>,
) -> Effect {
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    let base = op.argv(args);
    let what = op.what;
    let shown = base.join(" ");
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let resolved = match target {
                RemoteTarget::Upstream => match resolve_upstream(&workdir) {
                    Some(v) => Some(v),
                    None => {
                        return Err(
                            "no upstream configured for this branch — set one,                              or pick a destination explicitly"
                                .to_string(),
                        );
                    }
                },
                RemoteTarget::Prompted => match prompted.as_deref() {
                    Some(v) if !v.trim().is_empty() => Some(v.to_string()),
                    _ => return Err("no destination given".to_string()),
                },
                _ => None,
            };
            let mut argv = base;
            argv.extend(target.argv(resolved.as_deref()));
            run_remote_op(&workdir, &argv)
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        finish_task(what, result);
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {shown}"),
    }
}

pub fn spawn_remote_op(op: RemoteOp, args: &lattice_grammar::Args) -> Effect {
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    let argv = op.argv(args);
    let shown = argv.join(" ");
    let logged = shown.clone();
    let what = op.what;
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_remote_op(&workdir, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        // NOTIF.1d / MG.41g: the completion the echo could never
        // carry — the echo is written at *fire* time, so before this
        // the operation succeeded invisibly and failed only into
        // `*messages*`.
        //
        // Publishes rather than posting: magit no longer knows the
        // notification subsystem exists, and `finish_task` keeps the
        // `debug!`-on-success / `error!`-on-failure split (the failure
        // log carries git's FULL stderr; the published message is the
        // one line a notification can show).
        let _ = logged;
        finish_task(what, result);
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
    /// MG.41d: tokens appended AFTER the commit (see [`Self::argv`]).
    /// Empty for every op that takes none.
    pub trailing: &'static [&'static str],
    /// MG.23j: this op's ex-command name, without the `:`.
    ///
    /// Load-bearing in two places beyond documentation. It is the
    /// scriptable surface (`:magit-cherry-pick <sha>`), and it is what
    /// the commit picker fires: a picked candidate resolves to the ex
    /// line `"<ex_command> <sha>"`.
    ///
    /// **That shape was once the ONLY route** — the host's
    /// `InvokeCommand` arm destructured `args` away, so a value not
    /// baked into the line was lost. Fixed 2026-08-03: the arm now
    /// renders `args` onto the line, and both forms work. This one is
    /// kept because it is also the exact text a user would type, which
    /// keeps the picker a front-end to the command rather than a second
    /// way in.
    pub ex_command: &'static str,
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
        trailing: &[],
        ex_command: "magit-revert",
        // `--no-edit` keeps the generated message: lattice has no
        // commit-message UI wired into this path, so opening $EDITOR
        // inside the editor would hang the operation on a prompt the
        // user cannot answer.
        args: &["revert", "--no-edit"],
        confirm_action: None,
    };
    pub const CHERRY_PICK: Self = Self {
        what: "cherry-pick",
        trailing: &[],
        ex_command: "magit-cherry-pick",
        args: &["cherry-pick"],
        confirm_action: None,
    };
    /// MG.43f: magit's reset `w` — reset the WORKING TREE to a
    /// commit, leaving HEAD and the index alone.
    ///
    /// `git restore --source <commit> --worktree` is exactly this, and
    /// is why the row needs no plumbing: `reset` moves HEAD, and
    /// `checkout <commit> -- .` writes the index too. Verified against
    /// real git — a file staged before the restore is still staged
    /// after it.
    ///
    /// The commit sits between `--source` and the rest, which is what
    /// `trailing` is for.
    pub const RESET_WORKTREE: Self = Self {
        what: "restore worktree",
        trailing: &["--worktree", "--", "."],
        ex_command: "magit-reset-worktree",
        args: &["restore", "--source"],
        // Overwrites uncommitted working-tree changes, the same bar
        // `--hard` is held to.
        confirm_action: Some("action:magit-reset-worktree-execute"),
    };

    /// MG.43a: magit's revert `v` — apply the inverse to the working
    /// tree and index WITHOUT committing.
    ///
    /// `--no-commit` is the whole row: `V` records a revert commit,
    /// `v` leaves the reversal staged so it can be edited, split, or
    /// combined before committing. No `--no-edit` here because nothing
    /// is committed, so git never opens an editor to hang on.
    pub const REVERT_CHANGES: Self = Self {
        what: "revert --no-commit",
        trailing: &[],
        ex_command: "magit-revert-changes",
        args: &["revert", "--no-commit"],
        confirm_action: None,
    };
    /// MG.43a: magit's cherry-pick `a` — apply the commit's changes
    /// without recording a commit. Peer of [`Self::REVERT_CHANGES`].
    pub const CHERRY_PICK_APPLY: Self = Self {
        what: "cherry-pick --no-commit",
        trailing: &[],
        ex_command: "magit-cherry-pick-apply",
        args: &["cherry-pick", "--no-commit"],
        confirm_action: None,
    };
    pub const RESET_SOFT: Self = Self {
        what: "reset --soft",
        trailing: &[],
        ex_command: "magit-reset-soft",
        args: &["reset", "--soft"],
        confirm_action: None,
    };
    pub const RESET_MIXED: Self = Self {
        what: "reset --mixed",
        trailing: &[],
        ex_command: "magit-reset-mixed",
        args: &["reset", "--mixed"],
        confirm_action: None,
    };
    pub const RESET_HARD: Self = Self {
        what: "reset --hard",
        trailing: &[],
        ex_command: "magit-reset-hard",
        args: &["reset", "--hard"],
        // The only one that destroys uncommitted work.
        confirm_action: Some("action:magit-reset-hard-execute"),
    };

    /// MG.41d: magit's `k` — move HEAD but refuse if that would
    /// discard uncommitted work, unlike `--hard` which discards it
    /// silently. No confirm precisely because git itself declines
    /// rather than destroying anything.
    pub const RESET_KEEP: Self = Self {
        what: "reset --keep",
        trailing: &[],
        ex_command: "magit-reset-keep",
        args: &["reset", "--keep"],
        confirm_action: None,
    };
    /// MG.41d: magit's `i` — set the index to `commit` WITHOUT moving
    /// HEAD. The trailing `--` is what makes it index-only; the same
    /// command without it moves HEAD too.
    pub const RESET_INDEX: Self = Self {
        what: "reset index",
        trailing: &["--"],
        ex_command: "magit-reset-index",
        args: &["reset"],
        confirm_action: None,
    };
    /// MG.41d: magit's `f` fixup — record a `fixup!` commit that a
    /// later `rebase --autosquash` folds into `commit`.
    pub const COMMIT_FIXUP: Self = Self {
        what: "commit --fixup",
        trailing: &[],
        ex_command: "magit-commit-fixup",
        args: &["commit", "--no-edit", "--fixup"],
        confirm_action: None,
    };
    /// MG.41d: magit's `s` squash — like fixup, but the message is
    /// kept for editing when the autosquash runs.
    pub const COMMIT_SQUASH: Self = Self {
        what: "commit --squash",
        trailing: &[],
        ex_command: "magit-commit-squash",
        args: &["commit", "--no-edit", "--squash"],
        confirm_action: None,
    };

    /// Full argv for `commit`.
    pub fn argv(&self, commit: &str) -> Vec<String> {
        let mut argv: Vec<String> = self.args.iter().map(|s| (*s).to_string()).collect();
        argv.push(commit.to_string());
        // MG.41d: tokens that must follow the commit. `git reset
        // <commit> --` resets the index WITHOUT moving HEAD; the same
        // words before the commit mean something else entirely, so the
        // position is load-bearing rather than cosmetic.
        argv.extend(self.trailing.iter().map(|s| (*s).to_string()));
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
        // MG.41g: was log-only.
        finish_task(&logged, result);
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
        // MG.34: `rebase --continue` opens `$EDITOR` on the commit
        // message. There is no editor here that git can drive, so
        // without this the child blocks forever holding a blocking-pool
        // thread and the operation never reports either way. `true`
        // accepts the message unchanged — the same limitation, and the
        // same reason, as `run_rebase`'s `GIT_EDITOR`.
        .env("GIT_EDITOR", "true")
        // MG.42-E2: the todo-list editor, distinct from `GIT_EDITOR`.
        // `rebase -i --autosquash` opens the generated todo list; `true`
        // accepts it unchanged, which is exactly what autosquash means
        // — git has already ordered the fixup!/squash! lines. Without
        // this an instant-fixup hangs the same way `--continue` would
        // without `GIT_EDITOR`.
        .env("GIT_SEQUENCE_EDITOR", "true")
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

    /// MG.23d — untrack keeps the file, delete does not force.
    ///
    /// The distinction is the whole reason untrack does not ask and
    /// delete does. `--cached` leaves the file on disk; a plain `rm`
    /// with no `-f` makes git itself refuse to remove a file with
    /// uncommitted changes, so the confirm is the second line of
    /// defence rather than the only one.
    #[test]
    fn untrack_keeps_the_file_and_delete_never_forces() {
        let untrack = untrack_argv("src/main.rs");
        assert!(
            untrack.contains(&"--cached".to_string()),
            "untrack must leave the file on disk — without `--cached` it \
             would delete it, and it does not ask: {untrack:?}"
        );
        let delete = delete_argv("src/main.rs");
        assert!(
            !delete.iter().any(|a| a == "-f" || a == "--force"),
            "delete must let git refuse a modified file rather than \
             overriding that refusal: {delete:?}"
        );
        // `--` before the path, or a file named like a flag is read as
        // one.
        for argv in [untrack, delete, rename_argv("a", "b")] {
            assert!(
                argv.contains(&"--".to_string()),
                "paths must be separated from flags: {argv:?}"
            );
        }
    }

    /// MG.23d — the rename prompt carries its source in the buffer
    /// name, because by submit time the prompt buffer is the active one
    /// and nothing else still knows which file this was.
    #[test]
    fn a_rename_prompt_carries_its_source_in_its_buffer_name() {
        assert_eq!(
            rename_source_from_prompt_buffer_name("*magit:rename:src/main.rs*").as_deref(),
            Some("src/main.rs")
        );
        // Another buffer's name must not be read as a rename source —
        // the finish handler would otherwise rename something arbitrary.
        assert!(rename_source_from_prompt_buffer_name("*magit:status*").is_none());
        assert!(
            rename_source_from_prompt_buffer_name("*magit:rename:*").is_none(),
            "an empty source is not a path"
        );
    }

    /// MG.23d2 — the checkout argv, where `--` is not cosmetic.
    ///
    /// `git checkout <rev> <path>` without it is ambiguous when the path
    /// matches a ref name, and git resolves the ambiguity by checking
    /// out the *branch* — a wrong action, not an error.
    #[test]
    fn checkout_file_names_the_revision_then_separates_the_path() {
        let argv = checkout_file_argv("HEAD~2", "main");
        assert_eq!(argv, vec!["checkout", "HEAD~2", "--", "main"]);
        let sep = argv.iter().position(|a| a == "--").expect("a separator");
        assert!(
            sep < argv.len() - 1 && argv.iter().position(|a| a == "HEAD~2").unwrap() < sep,
            "the revision goes before the separator and the path after: {argv:?}"
        );
    }

    /// MG.23d2 — the checkout prompt carries its path the same way the
    /// rename prompt does, and the two carriers must not read each
    /// other's buffers: a checkout finish that accepted a rename prompt's
    /// name would overwrite a file the user was only renaming.
    #[test]
    fn the_checkout_prompt_carries_its_path_and_only_its_own() {
        assert_eq!(
            checkout_target_from_prompt_buffer_name("*magit:checkout:src/main.rs*").as_deref(),
            Some("src/main.rs")
        );
        assert!(checkout_target_from_prompt_buffer_name("*magit:rename:src/main.rs*").is_none());
        assert!(rename_source_from_prompt_buffer_name("*magit:checkout:src/main.rs*").is_none());
        assert!(
            checkout_target_from_prompt_buffer_name("*magit:checkout:*").is_none(),
            "an empty target is not a path"
        );
    }

    /// MG.23d2 — the execute half acts on both carried slots or on
    /// nothing.
    ///
    /// The other execute halves fall back to re-deriving their target,
    /// which is safe because the fallback is "the file you are looking
    /// at". There is no such guess for a revision, and checking out from
    /// the wrong one is exactly the damage the confirm exists to
    /// prevent — so a missing slot must produce no git call at all.
    #[test]
    fn checkout_execute_declines_when_a_slot_is_missing() {
        use lattice_mode::Mode as _;

        let handlers = MagitGlobalMode.action_handlers();
        let handler = handlers
            .iter()
            .find(|c| c.action_name == "action:magit-global-file-checkout-execute")
            .expect("contributed")
            .handler
            .clone();
        let services = lattice_mode::ServiceRegistry::new();
        let events = lattice_runtime::EventBus::new();

        for args in [
            lattice_grammar::Args::None,
            // A revision and no path — the shape a confirm raised by a
            // path that carries less than it should would produce.
            lattice_grammar::Args::List(vec![lattice_grammar::ArgValue::String(
                "HEAD".to_string(),
            )]),
        ] {
            let ctx = ActionContext {
                buffer_id: lattice_protocol::ids::BufferId::new(1),
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                services: &services,
                events: &events,
                prompt_value: None,
                args,
            };
            assert!(
                handler(&ctx).is_none(),
                "a half-carried confirm must run no git command"
            );
        }
    }

    /// MG.23c2 — merge passes `--no-edit`, and nothing else opens an
    /// editor either.
    ///
    /// Asserted on the argv rather than by running it: git would
    /// otherwise open `$EDITOR` for the merge message and hang on a
    /// prompt that never appears — and a test that spawned it would run
    /// real git against the repository the tests live in.
    #[test]
    fn no_prompted_operation_can_open_an_editor() {
        assert_eq!(
            merge_argv("feature/x"),
            vec!["merge", "--no-edit", "feature/x"],
            "merge must never be able to open an editor"
        );
        // The other two have no editor-spawning form, but pin their
        // argv so a later flag cannot introduce one unnoticed.
        assert_eq!(tag_argv("v1.2.0"), vec!["tag", "v1.2.0"]);
        assert_eq!(init_argv("/tmp/x"), vec!["init", "/tmp/x"]);
        for argv in [merge_argv("b"), tag_argv("t"), init_argv("d")] {
            assert!(
                !argv.iter().any(|a| a == "--edit" || a == "-e"),
                "no prompted operation may request an editor: {argv:?}"
            );
        }
    }

    // ── MG.38: subtree ──────────────────────────────────────────────

    /// `--prefix=` is the argument every subtree operation requires, and
    /// the one git checks LAST — after it has already done work. Pinned
    /// so a refactor cannot drop it silently.
    #[test]
    fn every_subtree_op_carries_its_prefix() {
        for (op, line) in [
            (SubtreeOp::ADD, "vendor/lib https://host/lib.git main"),
            (SubtreeOp::MERGE, "vendor/lib main"),
            (SubtreeOp::PULL, "vendor/lib https://host/lib.git main"),
            (SubtreeOp::PUSH, "vendor/lib https://host/lib.git main"),
            (SubtreeOp::SPLIT, "vendor/lib"),
        ] {
            let argv = subtree_argv(op, line).expect("well-formed");
            assert_eq!(argv[0], "subtree");
            assert_eq!(argv[1], op.sub);
            assert_eq!(
                argv[2], "--prefix=vendor/lib",
                "{} lost its prefix: {argv:?}",
                op.sub
            );
        }
    }

    /// The wrong number of words is refused rather than passed to git.
    ///
    /// `subtree add <prefix> <repo> <ref>` with the ref missing is
    /// `subtree add <prefix> <repo>` — which git accepts as a DIFFERENT
    /// form, so a silent pass-through would do something the user did
    /// not ask for instead of erroring.
    #[test]
    fn the_wrong_argument_count_is_refused() {
        assert_eq!(
            subtree_argv(SubtreeOp::ADD, "vendor/lib https://host/lib.git"),
            None
        );
        assert_eq!(subtree_argv(SubtreeOp::ADD, "vendor/lib"), None);
        assert_eq!(subtree_argv(SubtreeOp::SPLIT, "vendor/lib extra"), None);
        assert_eq!(subtree_argv(SubtreeOp::MERGE, ""), None);
    }

    /// `--squash` is offered where magit offers it and nowhere else:
    /// `git subtree push --squash` is not a thing, and accepting it
    /// would build an argv git rejects.
    #[test]
    fn squash_is_accepted_only_where_it_means_something() {
        let added = subtree_argv(
            SubtreeOp::ADD,
            "vendor/lib https://host/lib.git main --squash",
        )
        .expect("valid");
        assert!(added.contains(&"--squash".to_string()), "{added:?}");
        // On push the trailing word is not a flag, so the count check
        // rejects the line rather than silently dropping the word.
        assert_eq!(
            subtree_argv(
                SubtreeOp::PUSH,
                "vendor/lib https://host/lib.git main --squash"
            ),
            None,
            "push has no --squash; the line must not be silently truncated"
        );
    }

    // ── MG.39: am / format-patch ────────────────────────────────────

    /// `--3way` is opt-in. It changes what a failed apply DOES — falling
    /// back to a three-way merge with conflict markers rather than
    /// refusing — so it must not be on by default, the same judgement
    /// `--force-with-lease`-not-`--force` makes on push.
    #[test]
    fn three_way_apply_is_opt_in_and_reachable_by_both_spellings() {
        assert_eq!(
            am_argv("0001.patch", false),
            Some(vec!["am".to_string(), "0001.patch".to_string()]),
            "no flag unless asked"
        );
        assert!(am_wants_three_way("0001.patch -3"));
        assert!(am_wants_three_way("--3way 0001.patch"));
        assert!(!am_wants_three_way("0001.patch"));
        let three = am_argv("0001.patch", true).expect("valid");
        assert_eq!(three[1], "--3way");
    }

    /// The flag words must not be mistaken for patch files, or `git am`
    /// would be handed `-3` as a path and fail on a file that does not
    /// exist.
    #[test]
    fn flag_words_are_not_treated_as_patch_files() {
        assert_eq!(
            am_argv("--3way a.patch b.patch", true),
            Some(vec![
                "am".to_string(),
                "--3way".to_string(),
                "a.patch".to_string(),
                "b.patch".to_string(),
            ])
        );
        assert_eq!(am_argv("--3way", true), None, "flags alone are not patches");
        assert_eq!(am_argv("", false), None);
    }

    /// The output directory is explicit. `format-patch` with none writes
    /// into the process's current directory, which is not necessarily
    /// where the user thinks they are — and a scatter of `.patch` files
    /// somewhere unexpected is tedious to undo.
    #[test]
    fn format_patch_names_its_output_directory() {
        let argv = format_patch_argv("@{upstream}..HEAD", Some("/repo")).expect("valid");
        assert_eq!(
            argv,
            vec!["format-patch", "-o", "/repo", "@{upstream}..HEAD"]
        );
        assert_eq!(
            format_patch_argv("HEAD~3..HEAD", None).expect("valid"),
            vec!["format-patch", "HEAD~3..HEAD"]
        );
        assert_eq!(
            format_patch_argv("  ", Some("/repo")),
            None,
            "a range is required"
        );
    }

    // ── MG.37: notes ────────────────────────────────────────────────

    /// A bare ref merges with git's own default strategy, and the
    /// strategy is a trailing word when given.
    #[test]
    fn note_merge_argv_defaults_to_manual_and_accepts_a_strategy() {
        assert_eq!(
            note_merge_argv("refs/notes/other"),
            Some(vec![
                "notes".to_string(),
                "merge".to_string(),
                "--strategy=manual".to_string(),
                "refs/notes/other".to_string(),
            ]),
            "no strategy given ⇒ git's default, stated explicitly"
        );
        assert_eq!(
            note_merge_argv("refs/notes/other theirs")
                .expect("valid")
                .get(2)
                .map(String::as_str),
            Some("--strategy=theirs")
        );
    }

    /// A misspelled strategy is REFUSED, not silently merged manually.
    ///
    /// This is the case worth pinning: `ours` and `theirs` resolve a
    /// conflict in opposite directions, and falling back to `manual` on
    /// a typo would stop the merge rather than resolve it — leaving the
    /// user in a state they did not ask for with no sign why.
    #[test]
    fn a_misspelled_strategy_is_refused_rather_than_defaulted() {
        for bad in ["ref our", "ref Theirs", "ref cat-sort-uniq", "ref x y"] {
            assert_eq!(
                note_merge_argv(bad),
                None,
                "{bad:?} must be refused, not merged with the default"
            );
        }
        assert_eq!(note_merge_argv(""), None, "no ref at all");
        assert_eq!(note_merge_argv("   "), None);
    }

    // ── MG.36: `C` clone ────────────────────────────────────────────

    /// Both URL shapes people actually paste. The SSH form is the one a
    /// naive `rsplit('/')` gets wrong for a single-segment path, and
    /// `.git` is usual on it — a destination directory literally named
    /// `repo.git` is not what anyone means.
    #[test]
    fn the_default_destination_is_the_name_git_would_have_picked() {
        for (url, expected) in [
            ("https://github.com/owner/repo.git", "repo"),
            ("https://github.com/owner/repo", "repo"),
            ("https://github.com/owner/repo/", "repo"),
            ("git@github.com:owner/repo.git", "repo"),
            ("git@github.com:repo.git", "repo"),
            ("ssh://git@host:22/owner/repo.git", "repo"),
            ("/srv/git/bare-repo.git", "bare-repo"),
            ("  https://host/owner/repo.git  ", "repo"),
        ] {
            assert_eq!(
                default_clone_dest(url),
                expected,
                "the directory `git clone {url}` would create"
            );
        }
    }

    /// Nothing usable left means the caller must ask rather than invent
    /// a name — an empty string is the signal, and both the ex-command
    /// and the prompt check for it.
    #[test]
    fn a_url_with_no_usable_last_segment_yields_nothing() {
        for url in ["", "   ", "/", "https://", ".git"] {
            assert_eq!(
                default_clone_dest(url),
                "",
                "{url:?} names no directory, so none may be guessed"
            );
        }
    }

    /// `--` before the operands. A destination beginning with `-` is
    /// something a user can type, and without the separator git would
    /// read it as an option — silently, and with a different effect.
    #[test]
    fn clone_argv_separates_operands_from_options() {
        assert_eq!(
            clone_argv("https://host/o/r.git", "/tmp/r"),
            vec!["clone", "--", "https://host/o/r.git", "/tmp/r"]
        );
        let hostile = clone_argv("https://host/o/r.git", "--upload-pack=evil");
        assert_eq!(
            hostile.iter().position(|a| a == "--"),
            Some(1),
            "the separator must precede both operands: {hostile:?}"
        );
    }

    /// MG.23c1 — the `.gitignore` append, against a real file.
    ///
    /// Not a git subcommand (git has none for this), so the file
    /// handling is ours to get right and worth testing directly.
    #[test]
    fn appending_to_gitignore_is_idempotent_and_newline_safe() {
        use super::gitignore_append as append;

        // A file with no trailing newline would otherwise glue the new
        // pattern onto the last one, ignoring neither.
        assert_eq!(
            append("target", "*.log").as_deref(),
            Some("target\n*.log\n"),
            "a missing trailing newline must not fuse two patterns"
        );
        assert_eq!(append("", "*.log").as_deref(), Some("*.log\n"));
        assert_eq!(
            append("target\n", "*.log").as_deref(),
            Some("target\n*.log\n")
        );
        // Pressing `i` twice on the same artefact is an ordinary
        // mistake, not a request for two identical lines.
        assert!(
            append("target\n*.log\n", "*.log").is_none(),
            "an already-ignored pattern is skipped"
        );
        assert!(
            append("target\n", " target ").is_none(),
            "compared as trimmed whole lines"
        );
    }

    /// MG.23c1 — an empty prompt is a cancel, not an empty-named tag.
    ///
    /// Submitting nothing is how you back out of a prompt, and `git tag
    /// ""` would fail with a message about refs rather than about what
    /// the user did.
    #[test]
    fn an_empty_prompt_cancels_rather_than_running_the_operation() {
        use lattice_mode::Mode as _;

        let handlers = MagitGlobalMode.action_handlers();
        // Alternating blank shapes, so neither "empty" nor "whitespace"
        // is the only one ever exercised.
        for (i, (_, action)) in PROMPTED_OPS.iter().enumerate() {
            let blank = if i % 2 == 0 { "   " } else { "" };
            let handler = handlers
                .iter()
                .find(|c| c.action_name == *action)
                .unwrap_or_else(|| panic!("`{action}` is contributed"))
                .handler
                .clone();
            let services = lattice_mode::ServiceRegistry::new();
            let events = lattice_runtime::EventBus::new();
            let ctx = ActionContext {
                buffer_id: lattice_protocol::ids::BufferId::new(1),
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                services: &services,
                events: &events,
                prompt_value: Some(blank),
                args: lattice_grammar::Args::None,
            };
            assert!(
                handler(&ctx).is_none(),
                "`{action}` must decline a blank submission rather than run \
                 git with an empty argument"
            );
        }
    }

    /// The prompt row and its finish half must name each other, or
    /// pressing the key opens a prompt whose submit goes nowhere.
    #[test]
    fn each_prompt_row_targets_a_finish_action_that_exists() {
        use lattice_mode::Mode as _;

        let handlers = MagitGlobalMode.action_handlers();
        let names: Vec<&str> = handlers.iter().map(|c| c.action_name).collect();
        let services = lattice_mode::ServiceRegistry::new();
        let events = lattice_runtime::EventBus::new();

        for (action, expected_finish) in PROMPTED_OPS {
            let handler = handlers
                .iter()
                .find(|c| c.action_name == *action)
                .unwrap_or_else(|| panic!("`{action}` is contributed"))
                .handler
                .clone();
            let ctx = ActionContext {
                buffer_id: lattice_protocol::ids::BufferId::new(1),
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                services: &services,
                events: &events,
                prompt_value: None,
                args: lattice_grammar::Args::None,
            };
            match handler(&ctx) {
                Some(Effect::OpenPrompt {
                    on_submit_action, ..
                }) => {
                    assert_eq!(
                        &on_submit_action.as_str(),
                        expected_finish,
                        "`{action}` must submit to its own finish half"
                    );
                    assert!(
                        names.contains(&on_submit_action.as_str()),
                        "`{action}` opens a prompt submitting to \
                         `{on_submit_action}`, which no mode contributes — the \
                         prompt would accept input and do nothing with it"
                    );
                }
                other => panic!("`{action}` should open a prompt, got {other:?}"),
            }
        }
    }

    /// MG.52: **a branch row opens the branch picker, naming a real
    /// ex-command.**
    ///
    /// Two ways these rot, and the suite could see neither before: the
    /// row quietly goes back to a prompt (which is the thing being
    /// removed — a branch that does not exist is a typo, and git reports
    /// it long after the keystroke), or it opens a picker naming a
    /// command nobody registered, in which case picking does nothing.
    #[test]
    fn each_branch_row_opens_the_picker_with_a_registered_command() {
        use lattice_mode::Mode as _;

        let handlers = MagitGlobalMode.action_handlers();
        let services = lattice_mode::ServiceRegistry::new();
        let events = lattice_runtime::EventBus::new();

        // The ex-commands the picker can hand a branch to.
        let mut registry = lattice_grammar::CommandRegistry::new();
        crate::register_action_commands_for_test(&mut registry);
        crate::register_ex_commands_for_test(&mut registry);

        for (action, ex_command) in PICKED_BRANCH_OPS {
            let handler = handlers
                .iter()
                .find(|c| c.action_name == *action)
                .unwrap_or_else(|| panic!("`{action}` is contributed"))
                .handler
                .clone();
            let ctx = ActionContext {
                buffer_id: lattice_protocol::ids::BufferId::new(1),
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                services: &services,
                events: &events,
                prompt_value: None,
                args: lattice_grammar::Args::None,
            };
            match handler(&ctx) {
                Some(Effect::OpenPicker { source, args }) => {
                    assert_eq!(
                        source,
                        crate::picker_sources::BRANCH_PICK_SOURCE,
                        "`{action}` must open the branch picker"
                    );
                    assert_eq!(
                        args.first().map(String::as_str),
                        Some(*ex_command),
                        "`{action}` must hand the picked branch to `{ex_command}`"
                    );
                    assert!(
                        registry.id_by_name(ex_command).is_some(),
                        "`{action}` picks a branch and runs `{ex_command}`, which is \
                         not a registered ex-command — picking would do nothing"
                    );
                }
                other => panic!("`{action}` should open the branch picker, got {other:?}"),
            }
        }
    }

    /// The branch submenu's `b` and `l` must not list the same thing.
    ///
    /// Magit has both because they answer different questions, and the
    /// difference is the *listing*, not the operation — both end in
    /// `git checkout`:
    ///
    /// - `l` **local branch** — your local branches, nothing else.
    /// - `b` **branch/revision** — anything `git checkout` accepts: a
    ///   branch, a tag, `origin/main`, a raw SHA.
    ///
    /// MG.52 converted every free-text branch prompt to a picker and
    /// swept `b` up with the rest, pointing it at the local-branch
    /// picker. Nothing failed — `git checkout <local branch>` is a
    /// perfectly good command — so the loss was silent: `b` and `l`
    /// became the same row, and checking out `origin/main` or a tag
    /// from the menu stopped being reachable at all. The regression is
    /// only visible as *two menu rows that do the same thing*, which no
    /// assertion about either row alone can see. Hence a test about the
    /// pair.
    ///
    /// MG.53.g built the source that resolves it: `magit-revision`
    /// lists refs **and** recent commits, so `b` covers everything it
    /// used to accept as free text without accepting a typo.
    #[test]
    fn the_branch_revision_row_is_not_the_local_branch_row() {
        use lattice_mode::Mode as _;

        let handlers = MagitGlobalMode.action_handlers();
        let services = lattice_mode::ServiceRegistry::new();
        let events = lattice_runtime::EventBus::new();
        let source_of = |action: &str| -> String {
            let handler = handlers
                .iter()
                .find(|c| c.action_name == action)
                .unwrap_or_else(|| panic!("`{action}` is contributed"))
                .handler
                .clone();
            let ctx = ActionContext {
                buffer_id: lattice_protocol::ids::BufferId::new(1),
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                services: &services,
                events: &events,
                prompt_value: None,
                args: lattice_grammar::Args::None,
            };
            match handler(&ctx) {
                Some(Effect::OpenPicker { source, .. }) => source,
                other => panic!("`{action}` should open a picker, got {other:?}"),
            }
        };

        let rev = source_of("action:magit-global-branch-checkout-rev");
        let local = source_of("action:magit-global-branch-checkout");
        assert_eq!(
            rev,
            crate::picker_sources::REVISION_PICK_SOURCE,
            "`b` is magit's branch/revision row — it must offer tags, \
             remote-tracking refs and commits, not just local branches, \
             or it is `l` with a different key"
        );
        assert_eq!(
            local,
            crate::picker_sources::BRANCH_CHECKOUT_SOURCE,
            "`l` is the local-branch row and stays local"
        );
        assert_ne!(
            rev, local,
            "`b` and `l` listing the same set makes one of the two rows dead"
        );
    }

    /// MG.53.d: **a non-branch picker row names the right SOURCE.**
    ///
    /// `t p` ("Prune tags gone from remote") takes a remote, not a tag —
    /// its label reads like a tag operation and its argv builder says
    /// otherwise. Pointed at the tag picker it would list tags, hand one
    /// to `fetch --prune-tags`, and fail against a remote that does not
    /// exist. Asserting only "opens a picker" would not catch it.
    #[test]
    fn each_non_branch_row_opens_the_right_picker() {
        use lattice_mode::Mode as _;

        let handlers = MagitGlobalMode.action_handlers();
        let services = lattice_mode::ServiceRegistry::new();
        let events = lattice_runtime::EventBus::new();
        let mut registry = lattice_grammar::CommandRegistry::new();
        crate::register_action_commands_for_test(&mut registry);
        crate::register_ex_commands_for_test(&mut registry);

        for (action, expected_source, ex_command) in PICKED_OTHER_OPS {
            let handler = handlers
                .iter()
                .find(|c| c.action_name == *action)
                .unwrap_or_else(|| panic!("`{action}` is contributed"))
                .handler
                .clone();
            let ctx = ActionContext {
                buffer_id: lattice_protocol::ids::BufferId::new(1),
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                services: &services,
                events: &events,
                prompt_value: None,
                args: lattice_grammar::Args::None,
            };
            match handler(&ctx) {
                Some(Effect::OpenPicker { source, args }) => {
                    assert_eq!(
                        source, *expected_source,
                        "`{action}` must list the nouns it operates on"
                    );
                    assert_eq!(args.first().map(String::as_str), Some(*ex_command));
                    assert!(
                        registry.id_by_name(ex_command).is_some(),
                        "`{action}` runs `{ex_command}`, which is not registered"
                    );
                }
                other => panic!("`{action}` should open a picker, got {other:?}"),
            }
        }
    }

    /// MG.53.c: **the two file/revision rows open the commit picker,
    /// with the path in the right slot.**
    ///
    /// Not covered by `PICKED_OTHER_OPS`, because these take their
    /// target from `ctx.args` rather than being context-free — which is
    /// exactly why they needed their own guard rather than being left
    /// out. A gap here was invisible: `C-c f v` reverting to a prompt
    /// would look like nothing had changed.
    ///
    /// The placeholder is the part worth asserting. `magit-find-file`
    /// is `<rev> <path>`, so a command built without `{}` would append
    /// the revision and open a file named after a sha — a plausible
    /// command that does the wrong thing.
    #[test]
    fn the_file_revision_rows_open_the_commit_picker_with_a_placeholder() {
        use lattice_mode::Mode as _;

        let handlers = MagitGlobalMode.action_handlers();
        let services = lattice_mode::ServiceRegistry::new();
        let events = lattice_runtime::EventBus::new();

        for (action, ex_command) in [
            ("action:magit-global-file-at-revision", "magit-find-file"),
            ("action:magit-global-file-checkout", "magit-file-checkout"),
        ] {
            let handler = handlers
                .iter()
                .find(|c| c.action_name == action)
                .unwrap_or_else(|| panic!("`{action}` is contributed"))
                .handler
                .clone();
            // The target comes from the args slot, the same way the
            // file dispatch supplies it.
            let args = lattice_grammar::Args::List(vec![lattice_grammar::ArgValue::String(
                "src/main.rs".to_string(),
            )]);
            let ctx = ActionContext {
                buffer_id: lattice_protocol::ids::BufferId::new(1),
                cursor: lattice_protocol::position::Position::new(0, 0),
                selection: None,
                services: &services,
                events: &events,
                prompt_value: None,
                args,
            };
            match handler(&ctx) {
                Some(Effect::OpenPicker { source, args }) => {
                    assert_eq!(
                        source,
                        crate::picker_sources::REVISION_PICK_SOURCE,
                        "`{action}` picks a REVISION — a branch or tag as much \
                         as a commit. The commit-only picker cannot reach a \
                         file that lives on another branch, because it is not \
                         in this branch's history at all."
                    );
                    let line = args.first().map(String::as_str).unwrap_or("");
                    assert!(
                        line.starts_with(ex_command),
                        "`{action}` must run `{ex_command}`, got {line:?}"
                    );
                    assert!(
                        line.contains("{}"),
                        "`{action}` builds `{line}` — without a `{{}}` the \
                         revision is appended after the path, which opens a \
                         file named after a sha"
                    );
                    assert!(
                        line.contains("src/main.rs"),
                        "`{action}` must carry the target path: {line:?}"
                    );
                }
                other => panic!("`{action}` should open the commit picker, got {other:?}"),
            }
        }
    }

    /// MG.23b — `S` stages tracked modifications ONLY.
    ///
    /// `add --update` and not `add --all`: "stage everything" quietly
    /// adding a file git was never told about is how build artefacts and
    /// secrets get committed. Magit reaches the include-untracked
    /// behaviour behind a prefix argument; until that exists the
    /// explicit path is `s` on the Untracked entry.
    #[test]
    fn stage_all_stages_tracked_modifications_not_untracked_files() {
        let argv = RemoteOp::STAGE_ALL.argv(&lattice_grammar::Args::None);
        assert_eq!(argv, vec!["add", "--update"]);
        assert!(
            !argv.iter().any(|a| a == "--all" || a == "-A"),
            "an untracked sweep must be opt-in, never the default: {argv:?}"
        );
    }

    /// `U` is a bare `git reset`: index back to HEAD, working tree
    /// untouched. That is what makes it safe to fire without asking —
    /// nothing is lost and every change is still there to re-stage.
    #[test]
    fn unstage_all_resets_the_index_and_leaves_the_working_tree() {
        let argv = RemoteOp::UNSTAGE_ALL.argv(&lattice_grammar::Args::None);
        assert_eq!(argv, vec!["reset", "--quiet"]);
        assert!(
            !argv.iter().any(|a| a == "--hard" || a == "--merge"),
            "a reset that touches the working tree would need MG.12's \
             confirm, and would not belong on an unprompted key: {argv:?}"
        );
    }

    /// Neither takes a target, which is precisely why they could land
    /// ahead of `A` / `_` / `O` — those act on the commit at the cursor,
    /// and the root dispatch has no cursor context.
    #[test]
    fn the_repo_wide_index_ops_take_no_arguments() {
        assert!(RemoteOp::STAGE_ALL.arg_specs().is_empty());
        assert!(RemoteOp::UNSTAGE_ALL.arg_specs().is_empty());
    }

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
                    RemoteArgKind::Value { .. } | RemoteArgKind::ValueJoined { .. } => {
                        lattice_grammar::ArgKind::String
                    }
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

#[cfg(test)]
mod reactive_refresh {
    use super::invalidates_a_magit_view;
    use lattice_protocol::event::{Event, TaskOutcome};

    fn finished(source: &str) -> Event {
        Event::BackgroundTaskFinished {
            source: source.to_string(),
            label: "push".to_string(),
            outcome: TaskOutcome::Succeeded {
                summary: "done".to_string(),
            },
        }
    }

    /// The whole point: a magit mutation reported by ANY surface — the
    /// branch view, a transient row, an ex-command, another pane —
    /// invalidates this buffer, so it refreshes without the user
    /// pressing `gr`.
    #[test]
    fn a_magit_task_invalidates_the_view() {
        assert!(invalidates_a_magit_view(&finished("magit")));
    }

    /// And nothing else does. An LSP request or a compilation
    /// finishing publishes the same event KIND and says nothing about
    /// the repository; refreshing on those would run `git status`
    /// every time a build ended.
    #[test]
    fn another_subsystems_task_does_not() {
        for source in ["lsp", "compilation", "plugin", ""] {
            assert!(
                !invalidates_a_magit_view(&finished(source)),
                "{source:?} must not trigger a magit refresh"
            );
        }
    }
}
