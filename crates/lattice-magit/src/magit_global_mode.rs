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
    // MG.23b: the two repo-wide index rows magit puts on `S` / `U`.
    // Both need no target — they act on the whole index — which is why
    // they land before the commit-acting rows (`A` / `_` / `O`), whose
    // root-dispatch entries still want a commit picker.
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
            Some(Effect::OpenPrompt {
                prompt: format!("Checkout {path} from revision: "),
                // `HEAD` is the overwhelmingly common intent — "put
                // back what I committed" — and it is also the one
                // revision you can name without looking anything up.
                initial: "HEAD".to_string(),
                on_submit_action: "action:magit-global-file-checkout-finish".to_string(),
                // Same carrier as rename: by submit time the prompt
                // buffer is the active one, so nothing else still knows
                // which file this was.
                buffer_name: Some(format!("*magit:checkout:{path}*")),
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
            Some(prompt_for(
                "Merge branch: ",
                "action:magit-global-merge-finish",
            ))
        }),
    });
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
    file_open!(
        "action:magit-global-file-blame",
        "*magit:blame:",
        "magit-blame-mode"
    );

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
                Some((git_ref, path)) => Effect::OpenSyntheticBuffer {
                    name: crate::magit_blame_mode::reverse_buffer_name(
                        &git_ref,
                        &path.to_string_lossy(),
                    ),
                    mode_id: crate::MagitBlameMode::mode_id().to_string(),
                },
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

/// MG.23c1: a repo-level operation whose single argument the user
/// types.
///
/// The shape is the branch-create wizard's, generalised: the menu row
/// (or chord) returns [`Effect::OpenPrompt`], and the `-finish` action
/// named as its submit target does the work with `ctx.prompt_value`.
/// Two actions per operation, no transient state, and the ex-command
/// form skips the prompt entirely by taking the value as an argument —
/// which is what makes these scriptable rather than menu-only.
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
pub fn spawn_git(argv: Vec<String>, what: &str) -> Effect {
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    let shown = format!("git {}", argv.join(" "));
    let logged = shown.clone();
    let what = what.to_string();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || run_remote_op(&workdir, &argv))
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match result {
            Ok(out) => {
                tracing::info!(target: "lattice_magit", "magit: {logged} succeeded: {out}")
            }
            Err(err) => tracing::error!(target: "lattice_magit", "magit: {what} failed: {err}"),
        }
    });
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {shown}"),
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
        match result {
            Ok(out) => tracing::info!(target: "lattice_magit", "magit: {out}"),
            Err(err) => {
                tracing::error!(target: "lattice_magit", "magit: could not update .gitignore: {err}")
            }
        }
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
    (
        "action:magit-global-merge",
        "action:magit-global-merge-finish",
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
pub fn spawn_remote_op(op: RemoteOp, args: &lattice_grammar::Args) -> Effect {
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
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
    /// MG.23j: this op's ex-command name, without the `:`.
    ///
    /// Load-bearing in two places beyond documentation. It is the
    /// scriptable surface (`:magit-cherry-pick <sha>`), and it is what
    /// the commit picker fires: a picked candidate resolves to the ex
    /// line `"<ex_command> <sha>"`, which is the only route from a
    /// picker to an operation that carries a value —
    /// `RoutingPayload::InvokeCommand` declares an `args` field but the
    /// host's arm destructures it away (`InvokeCommand { id, .. }`) and
    /// runs `id` as an ex line, so the value has to be *in* the line.
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
        ex_command: "magit-cherry-pick",
        args: &["cherry-pick"],
        confirm_action: None,
    };
    pub const RESET_SOFT: Self = Self {
        what: "reset --soft",
        ex_command: "magit-reset-soft",
        args: &["reset", "--soft"],
        confirm_action: None,
    };
    pub const RESET_MIXED: Self = Self {
        what: "reset --mixed",
        ex_command: "magit-reset-mixed",
        args: &["reset", "--mixed"],
        confirm_action: None,
    };
    pub const RESET_HARD: Self = Self {
        what: "reset --hard",
        ex_command: "magit-reset-hard",
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
