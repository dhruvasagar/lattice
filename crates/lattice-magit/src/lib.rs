//! Magit — git porcelain as a core plugin.
//!
//! Feature-buffer crate inverted out of `lattice-host`. Owns every
//! magit buffer view's mode, keymap, action handler, and synthetic-
//! buffer provisioning. Installs through the `SubsystemBoot` seam —
//! one line in `editor_boot.rs`, zero `Editor::do_magit_*` methods.
//!
//! See [`docs/dev/architecture/magit.md`] and
//! [`docs/dev/operations/slice-plans/magit.md`].

pub mod actions;
pub mod buffer_state;
mod confirm;
// MG.18d: `pub` because `MagitView::refresh_restoring` (a public
// trait) names `HunkRestore` in its signature.
pub mod cursor_restore;
pub mod fold_source;
pub mod headerline;
mod highlight;
// MG.18c: public so the bench can measure the parser directly — the
// accessor shape it pins (read the hunk, not the document) is the
// paramount-#1 claim staging rests on, and MG.22 relocates this module
// into `magit-hunk-mode` as the owner of diff content.
pub mod hunk;
pub mod magit_blame_mode;
pub mod magit_branch_mode;
pub mod magit_commit_mode;
pub mod magit_core_mode;
pub mod magit_diff_mode;
pub mod magit_file_revision_mode;
pub mod magit_global_mode;
pub mod magit_log_mode;
pub mod magit_rebase_mode;
pub mod magit_revision_mode;
pub mod magit_stash_mode;
pub mod magit_stash_show_mode;
pub mod magit_status_mode;
pub mod picker_sources;
pub mod refresh;
pub mod sections;
pub mod transients;

use std::sync::Arc;

use lattice_grammar::{
    ActionSpec, ArgSpec, Args, Effect, ExCommandSpec, GrammarResult, LatencyClass, SurfaceForm,
    registry::CommandRegistry,
};
use lattice_mode::SubsystemBoot;

use magit_blame_mode::MagitBlameMode;
use magit_branch_mode::MagitBranchMode;
use magit_commit_mode::MagitCommitMode;
use magit_core_mode::MagitCoreMode;
use magit_diff_mode::MagitDiffMode;
use magit_file_revision_mode::MagitFileRevisionMode;
use magit_global_mode::MagitGlobalMode;
use magit_log_mode::MagitLogMode;
use magit_rebase_mode::MagitRebaseMode;
use magit_revision_mode::MagitRevisionMode;
use magit_stash_mode::MagitStashMode;
use magit_status_mode::MagitStatusMode;

/// Register all magit modes, commands, and keymaps via the generic
/// `SubsystemBoot` seam. Called once from `editor_boot.rs` during
/// the Phase-B subsystem install pass.
pub fn install(boot: &mut impl SubsystemBoot) {
    // ── Modes ──────────────────────────────────────────────

    boot.modes_mut()
        .register(MagitGlobalMode)
        .expect("magit-global-mode registers without conflict");

    boot.modes_mut()
        .register(MagitCoreMode)
        .expect("magit-core-mode registers without conflict");

    boot.modes_mut()
        .register(MagitStatusMode)
        .expect("magit-status-mode registers without conflict");

    boot.modes_mut()
        .register(MagitCommitMode)
        .expect("magit-commit-mode registers without conflict");

    boot.modes_mut()
        .register(MagitDiffMode)
        .expect("magit-diff-mode registers without conflict");

    boot.modes_mut()
        .register(MagitLogMode)
        .expect("magit-log-mode registers without conflict");

    boot.modes_mut()
        .register(MagitBlameMode)
        .expect("magit-blame-mode registers without conflict");

    boot.modes_mut()
        .register(MagitStashMode)
        .expect("magit-stash-mode registers without conflict");

    boot.modes_mut()
        .register(MagitBranchMode)
        .expect("magit-branch-mode registers without conflict");

    boot.modes_mut()
        .register(MagitRebaseMode)
        .expect("magit-rebase-mode registers without conflict");

    boot.modes_mut()
        .register(MagitRevisionMode)
        .expect("magit-revision-mode registers without conflict");

    boot.modes_mut()
        .register(MagitFileRevisionMode)
        .expect("magit-file-revision-mode registers without conflict");

    // MG.15
    boot.modes_mut()
        .register(magit_stash_show_mode::MagitStashShowMode)
        .expect("magit-stash-show-mode registers without conflict");

    // ── Per-buffer mode state (MG.13) ──────────────────────
    //
    // Each per-buffer mode's action handlers are registered once at
    // boot via `Mode::action_handlers()` and resolve their state
    // through these services at call time, keyed by `BufferId`. That
    // removes the window in which a magit chord resolved but found no
    // handler because `on_activate` had not finished. Register and
    // look up through the `…StatesHandle` aliases — `ServiceRegistry`
    // keys on `TypeId` (`feedback_servicesregistry_arc_typeid`).
    // See `buffer_state`'s module docs.
    register_buffer_state_services(boot);

    // ── MG.18d: the cursor's way back after an async refresh ───
    cursor_restore::install_cursor_bus(boot);

    // ── Ex-commands ────────────────────────────────────────

    register_ex_commands(boot.commands_mut());

    // ── Action commands (keymap resolution targets) ──────

    register_action_commands(boot.commands_mut());

    // ── Transient menus (magit-dispatch / magit-file-dispatch) ──

    // Fold audit fix: resolve the root dispatch's action ids now,
    // while `boot.commands_mut()` still gives direct access to the
    // registry `register_action_commands` just populated above —
    // `TransientSourceRegistry`'s builders receive only a
    // `TransientContext` (see its doc comment for why
    // `Effect::OpenTransient` can only carry a name, not a
    // `TransientSpec`), so this is captured by value rather than
    // looked up again on every press.
    let dispatch_ids = resolve_dispatch_ids(boot.commands_mut());
    let file_dispatch_ids = resolve_file_dispatch_ids(boot.commands_mut());
    let other_file_dispatch_ids = resolve_file_dispatch_ids(boot.commands_mut());
    let transient_registry = lattice_picker::TransientSourceRegistry::new();
    // MG.23h: the root dispatch varies with where it was opened — see
    // `transients::dispatch_transient`. The file dispatch does not: its
    // rows all act on the visited file, which is the same question
    // wherever you press `C-c f`.
    transient_registry.register("magit-dispatch", move |ctx| {
        transients::dispatch_transient(&dispatch_ids, ctx)
    });
    transient_registry.register("magit-file-dispatch", move |_| {
        transients::file_dispatch_transient(&file_dispatch_ids)
    });
    // MG.23a: the same rows for a file you are not visiting. Registered
    // as a source + an ex-command and bound to NO chord — `C-c f` is the
    // common case; a user who wants magit's always-ask behaviour binds
    // this instead.
    transient_registry.register("magit-other-file-dispatch", move |_| {
        transients::other_file_dispatch_transient(&other_file_dispatch_ids)
    });
    boot.register_service::<lattice_picker::TransientSourceRegistryHandle>(Arc::new(
        transient_registry,
    ));
}

/// MG.17a: parse a remote operation's flags off the `:` line into the
/// positional `Args::List` its `args_schema` declares.
///
/// Accepts each flag's full git spelling (`--force-with-lease`) and
/// nothing else — no abbreviations. The transient shows the same
/// strings, so what you learn in one surface types correctly in the
/// other, and an unrecognised token is silently ignored rather than
/// failing the command: the flags are additive, so the worst case is
/// an operation that does slightly less than you asked, never
/// something you didn't ask for.
fn parse_remote_flags(op: magit_global_mode::RemoteOp, line: &str) -> Args {
    use magit_global_mode::RemoteArgKind;
    if op.flags.is_empty() {
        return Args::None;
    }
    let given: Vec<&str> = line.split_whitespace().collect();
    Args::List(
        op.flags
            .iter()
            .map(|f| match f.kind {
                RemoteArgKind::Flag => lattice_grammar::ArgValue::Bool(given.contains(&f.arg)),
                // MG.17b: `-m some message` — everything after the
                // marker to the end of the line, so a stash message
                // does not have to be quoted. That means a value
                // argument must come last on the line, which is stated
                // in the ex-command's doc; the transient has no such
                // constraint.
                RemoteArgKind::Value { .. } => lattice_grammar::ArgValue::String(
                    given
                        .iter()
                        .position(|t| *t == f.arg)
                        .map(|i| given[i + 1..].join(" "))
                        .unwrap_or_default(),
                ),
            })
            .collect(),
    )
}

/// MG.13: register one `BufferStates<S>` service per per-buffer mode.
///
/// Factored out of [`install`] so the test below can assert that every
/// migrated mode has its slot — a mode whose service is missing has
/// handlers that silently resolve `None` and no-op, which from the
/// user's side is indistinguishable from the dead-chord bug this slice
/// exists to remove.
fn register_buffer_state_services(boot: &mut impl SubsystemBoot) {
    boot.register_service::<magit_branch_mode::BranchStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<magit_stash_mode::StashStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<magit_revision_mode::RevisionStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<magit_blame_mode::BlameStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<magit_commit_mode::CommitStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<magit_rebase_mode::RebaseStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<magit_log_mode::LogStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<magit_diff_mode::DiffStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    boot.register_service::<actions::StatusStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    // MG.23g: stash-show gained per-buffer state when `a` / `-` needed
    // somewhere to read its workdir from.
    boot.register_service::<magit_stash_show_mode::StashShowStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    // Shared-action dispatch: `gr` is bound by `magit-core-mode` and
    // registered exactly once at boot; each view publishes its own
    // refresh body here. See `buffer_state::MagitView` for why a
    // per-mode registration of a shared action id is unsafe.
    boot.register_service::<buffer_state::MagitViewsHandle>(Arc::new(
        buffer_state::MagitViews::default(),
    ));
}

/// Resolve the root dispatch transient's action ids. Factored out of
/// [`install`] so the regression tests below exercise the SAME
/// resolution `install` performs — a test that re-listed the field
/// assignments by hand would silently stop covering any field added
/// afterwards, which is exactly the class of bug (an item silently
/// downgrading to an inert `Flag`) these tests exist to catch.
fn resolve_dispatch_ids(registry: &CommandRegistry) -> transients::DispatchActionIds {
    transients::DispatchActionIds {
        status: registry.id_by_name("action:magit-global-status"),
        commit: registry.id_by_name("action:magit-global-commit"),
        amend: registry.id_by_name("action:magit-global-amend"),
        log: registry.id_by_name("action:magit-global-log"),
        diff: registry.id_by_name("action:magit-global-diff"),
        branch: registry.id_by_name("action:magit-global-branch"),
        stash: registry.id_by_name("action:magit-global-stash"),
        stash_create: registry.id_by_name("action:magit-global-stash-create"),
        rebase: registry.id_by_name("action:magit-global-rebase"),
        stage_all: registry.id_by_name("action:magit-global-stage-all"),
        tag: registry.id_by_name("action:magit-global-tag"),
        gitignore: registry.id_by_name("action:magit-global-gitignore"),
        init: registry.id_by_name("action:magit-global-init"),
        merge: registry.id_by_name("action:magit-global-merge"),
        unstage_all: registry.id_by_name("action:magit-global-unstage-all"),
        fetch: registry.id_by_name("action:magit-global-fetch"),
        pull: registry.id_by_name("action:magit-global-pull"),
        push: registry.id_by_name("action:magit-global-push"),
        // MG.23h: the section-acting rows reuse the chords' own
        // actions rather than declaring menu-only twins — a second
        // action for "discard the thing at cursor" is a second place
        // for its confirm contract to drift.
        apply_hunk: registry.id_by_name("action:magit-apply-hunk"),
        reverse_hunk: registry.id_by_name("action:magit-reverse-hunk"),
        discard: registry.id_by_name("action:magit-discard"),
        cherry_pick: registry.id_by_name("action:magit-cherry-pick"),
        revert: registry.id_by_name("action:magit-revert"),
        reset_soft: registry.id_by_name("action:magit-reset-soft"),
        reset_mixed: registry.id_by_name("action:magit-reset-mixed"),
        reset_hard: registry.id_by_name("action:magit-reset-hard"),
        jump_staged: registry.id_by_name("action:magit-jump-staged"),
        jump_unstaged: registry.id_by_name("action:magit-jump-unstaged"),
        jump_untracked: registry.id_by_name("action:magit-jump-untracked"),
        jump_stashes: registry.id_by_name("action:magit-jump-stashes"),
        jump_commits: registry.id_by_name("action:magit-jump-commits"),
    }
}

/// IX.2: the execute half of each destructive pair, and the slots its
/// confirmation carries.
///
/// Every slot is optional: a confirm raised by a path that carries
/// nothing leaves them unset and the handler re-derives, which is the
/// pre-IX.1 behaviour. Names matter — the host projects the dialog's
/// state onto this schema **by name**, so a rename here without one at
/// the `ask` site silently reverts that action to re-deriving.
///
/// `magit-rebase-abort-execute` is absent deliberately: it aborts *the*
/// in-progress rebase, of which there is exactly one, so it has no
/// target to carry and nothing to get wrong.
const CONFIRM_TARGET_ACTIONS: &[(&str, &str, &[(&str, &str)])] = &[
    (
        "action:magit-discard-execute",
        "Execute the discard after confirmation",
        &[
            ("path", "Repo-relative path, for a file-level discard"),
            ("patch", "The synthesized patch, for a hunk or region"),
            ("workdir", "Repository the patch applies in"),
        ],
    ),
    (
        "action:magit-global-file-delete-execute",
        "Delete the file after confirmation",
        &[("file", "Repo-relative path the prompt named")],
    ),
    (
        "action:magit-global-file-checkout-execute",
        "Check the file out from the named revision after confirmation",
        &[
            ("rev", "Revision the prompt named"),
            ("file", "Repo-relative path the prompt named"),
        ],
    ),
    (
        "action:magit-branch-delete-execute",
        "Delete the branch after confirmation",
        &[("branch", "Branch the prompt named")],
    ),
    (
        "action:magit-stash-drop-execute",
        "Drop the stash after confirmation",
        &[("stash", "Stash index the prompt named")],
    ),
    (
        "action:magit-reset-hard-execute",
        "Reset --hard after confirmation",
        &[("commit", "Commit the prompt named")],
    ),
];

/// MG.23a: the actions that take an optional `file` target — every
/// `C-c f` row. Listed once so the schema pass, the
/// `magit-other-file-dispatch` rows and the tests cannot drift apart.
///
/// `…-discard-execute` is deliberately NOT here: see
/// [`transients::other_file_dispatch_transient`] for why an
/// explicit target cannot survive a confirm today.
const FILE_TARGET_ACTIONS: &[(&str, &str)] = &[
    (
        "action:magit-global-file-stage",
        "Stage the file in the current buffer",
    ),
    (
        "action:magit-global-file-unstage",
        "Unstage the file in the current buffer",
    ),
    (
        "action:magit-global-file-discard",
        "Discard changes to the file in the current buffer",
    ),
    (
        "action:magit-global-file-diff",
        "Show diff for the file in the current buffer",
    ),
    (
        "action:magit-global-file-log",
        "Show commit history for the file in the current buffer",
    ),
    (
        "action:magit-global-file-blame",
        "Blame the file in the current buffer",
    ),
    // The execute half needs the slot too, not just the ask half that
    // fills it: the host projects the confirm dialog's state onto THIS
    // action's schema, so without a `file` slot the carried path lands
    // nowhere and the handler silently falls back to the visited file.
    // IX.1 migrated the ask half and missed this; the destructive-pair
    // guard is what found it.
    (
        "action:magit-global-file-discard-execute",
        "Execute the file discard after confirmation",
    ),
    // MG.23d
    (
        "action:magit-global-file-untrack",
        "Stop tracking the file, keeping it on disk",
    ),
    (
        "action:magit-global-file-delete",
        "Delete the file (asks first)",
    ),
    (
        "action:magit-global-file-rename",
        "Rename the file (asks for the new name)",
    ),
    // MG.23d2
    (
        "action:magit-global-file-checkout",
        "Check the file out from a revision (asks for it, then confirms)",
    ),
];

/// MG.23f2: what `:magit-blame-reverse` says when it is not given both
/// halves. An error rather than a best guess — see the registration for
/// why there is no defensible default revision.
fn reverse_blame_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-blame-reverse <rev> <path>".to_string(),
    }
}

/// Resolve the file dispatch transient's action ids — same
/// shared-with-tests rationale as [`resolve_dispatch_ids`].
fn resolve_file_dispatch_ids(registry: &CommandRegistry) -> transients::FileDispatchActionIds {
    transients::FileDispatchActionIds {
        stage: registry.id_by_name("action:magit-global-file-stage"),
        unstage: registry.id_by_name("action:magit-global-file-unstage"),
        discard: registry.id_by_name("action:magit-global-file-discard"),
        diff: registry.id_by_name("action:magit-global-file-diff"),
        log: registry.id_by_name("action:magit-global-file-log"),
        blame: registry.id_by_name("action:magit-global-file-blame"),
        blame_reverse: registry.id_by_name("action:magit-global-file-blame-reverse"),
        untrack: registry.id_by_name("action:magit-global-file-untrack"),
        delete: registry.id_by_name("action:magit-global-file-delete"),
        rename: registry.id_by_name("action:magit-global-file-rename"),
        checkout: registry.id_by_name("action:magit-global-file-checkout"),
    }
}

/// Register all magit ex-commands in the command registry.
fn register_ex_commands(registry: &mut CommandRegistry) {
    let mut mk = |name: &'static str,
                  doc: &'static str,
                  buffer_name: &'static str,
                  mode_id: &'static str| {
        let mode_id = mode_id.to_string();
        registry.register_ex_command(
            name,
            doc,
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
                apply: Arc::new(move |_ctx| {
                    Ok(Effect::OpenSyntheticBuffer {
                        name: buffer_name.to_string(),
                        mode_id: mode_id.clone(),
                    })
                }),
                args_schema: Vec::new(),
                surface_form: SurfaceForm::Keyword,
            },
        );
    };

    mk(
        "magit-status",
        "Open the Magit status buffer for the current git repository.",
        "*magit:status*",
        "magit-status-mode",
    );
    mk(
        "magit-commit",
        "Open the Magit commit buffer with staged diff preview.",
        "*magit:commit*",
        "magit-commit-mode",
    );
    mk(
        "magit-diff",
        "Open a dedicated side-by-side diff view against HEAD.",
        "*magit:diff*",
        "magit-diff-mode",
    );
    mk(
        "magit-log",
        "Open the Magit commit history log.",
        "*magit:log*",
        "magit-log-mode",
    );
    mk(
        "magit-stash-list",
        "Open the Magit stash list buffer.",
        "*magit:stash*",
        "magit-stash-mode",
    );
    mk(
        "magit-branch",
        "Open the Magit branch list buffer.",
        "*magit:branch*",
        "magit-branch-mode",
    );
    // Drop the mk closure to release mutable borrow
    drop(mk);
    // Fold audit fix: `magit-dispatch` / `magit-file-dispatch` open
    // their OWN named transients (registered into
    // `TransientSourceRegistry` by `install`, below) instead of
    // aliasing `magit-status` — each returns `Effect::OpenTransient`
    // rather than `Effect::OpenSyntheticBuffer`.
    let mut mk_transient = |name: &'static str, doc: &'static str, source: &'static str| {
        registry.register_ex_command(
            name,
            doc,
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
                apply: Arc::new(move |_ctx| {
                    Ok(Effect::OpenTransient {
                        source: source.to_string(),
                    })
                }),
                args_schema: Vec::new(),
                surface_form: SurfaceForm::Keyword,
            },
        );
    };
    mk_transient(
        "magit-dispatch",
        "Open the Magit repo-level dispatch transient.",
        "magit-dispatch",
    );
    mk_transient(
        "magit-file-dispatch",
        "Open the Magit file-level dispatch transient.",
        "magit-file-dispatch",
    );
    mk_transient(
        "magit-other-file-dispatch",
        "Open the Magit file-level dispatch transient for a file you name, \
         rather than the one you are visiting.",
        "magit-other-file-dispatch",
    );
    drop(mk_transient);

    // MG.16: the remote/stash operations were reachable from `C-c g`
    // and nowhere else. Ex-commands are the scriptable surface and the
    // `:` discovery path, so a transient-only operation is invisible
    // to both — you cannot bind it, cannot script it, and cannot find
    // it by typing `:magit-<Tab>`.
    //
    // These are front-ends, not reimplementations: each resolves the
    // same `RemoteOp` constant its transient item fires and calls the
    // same `spawn_remote_op` body (the unified-dispatch rule). Names
    // are dashed + namespaced per the standing ex-command rule; no new
    // 1-2 letter shorts.
    let mut mk_op = |name: &'static str, doc: &'static str, op: magit_global_mode::RemoteOp| {
        registry.register_ex_command(
            name,
            doc,
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                // MG.17a: `--force-with-lease`, `--prune`, … parsed off
                // the `:` line into the SAME positional `Args::List` the
                // transient's flag toggles produce. `RemoteOp::flags` is
                // the one definition both read, so a flag added there
                // appears on both surfaces at once.
                parse_args: Arc::new(move |line: &str, _bang: bool| {
                    Ok(parse_remote_flags(op, line))
                }),
                apply: Arc::new(move |ctx| Ok(magit_global_mode::spawn_remote_op(op, &ctx.args))),
                args_schema: op.arg_specs(),
                surface_form: SurfaceForm::Keyword,
            },
        );
    };
    mk_op(
        "magit-fetch",
        "Fetch from the default remote without merging.",
        magit_global_mode::RemoteOp::FETCH,
    );
    mk_op(
        "magit-pull",
        "Pull from the upstream branch (fast-forward only).",
        magit_global_mode::RemoteOp::PULL,
    );
    mk_op(
        "magit-push",
        "Push the current branch to its upstream.",
        magit_global_mode::RemoteOp::PUSH,
    );
    // `:magit-stash` creates a stash; `:magit-stash-list` opens the
    // list buffer. The pair mirrors Emacs magit's own `z z` / `z l`,
    // where the bare stash key is the create.
    mk_op(
        "magit-stash",
        "Stash the working tree's changes.",
        magit_global_mode::RemoteOp::STASH,
    );
    drop(mk_op);
    // MG.23c1: the scriptable half of the prompt-backed operations.
    // With an argument they act directly; without one they open the
    // same prompt the menu row does, so `:magit-tag` and `C-c g t` are
    // the same operation reached two ways rather than two operations.
    {
        let mut mk_prompted = |name: &'static str,
                               doc: &'static str,
                               arg: &'static str,
                               arg_doc: &'static str,
                               prompt_action: &'static str,
                               run: fn(String) -> Effect| {
            registry.register_ex_command(
                name,
                doc,
                ExCommandSpec {
                    latency_class: LatencyClass::Reflex,
                    accepts_bang: false,
                    accepts_range: false,
                    parse_args: Arc::new(|line: &str, _bang: bool| {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            Ok(Args::None)
                        } else {
                            Ok(Args::String(trimmed.to_string()))
                        }
                    }),
                    apply: Arc::new(move |ctx| {
                        Ok(match ctx.args {
                            Args::String(ref v) if !v.trim().is_empty() => {
                                run(v.trim().to_string())
                            }
                            // No argument: ask, through the same action
                            // the menu row fires, so there is one prompt
                            // and one finish handler for both surfaces.
                            _ => Effect::OpenPrompt {
                                prompt: format!("{arg_doc}: "),
                                initial: String::new(),
                                on_submit_action: prompt_action.to_string(),
                                buffer_name: None,
                            },
                        })
                    }),
                    args_schema: vec![ArgSpec::optional(
                        arg,
                        lattice_grammar::ArgKind::String,
                        arg_doc,
                    )],
                    surface_form: SurfaceForm::Keyword,
                },
            );
        };
        mk_prompted(
            "magit-tag",
            "Tag HEAD. With arg: the tag name; without, asks for it.",
            "name",
            "Tag name",
            "action:magit-global-tag-finish",
            |name| magit_global_mode::spawn_git(magit_global_mode::tag_argv(&name), "tag"),
        );
        mk_prompted(
            "magit-merge",
            "Merge a branch into the current one. With arg: the branch; without, asks.",
            "branch",
            "Merge branch",
            "action:magit-global-merge-finish",
            |branch| magit_global_mode::spawn_git(magit_global_mode::merge_argv(&branch), "merge"),
        );
        mk_prompted(
            "magit-init",
            "Initialize a git repository. With arg: the directory; without, asks.",
            "directory",
            "Initialize repository in",
            "action:magit-global-init-finish",
            |dir| magit_global_mode::spawn_git(magit_global_mode::init_argv(&dir), "init"),
        );
        mk_prompted(
            "magit-gitignore",
            "Add a pattern to .gitignore. With arg: the pattern; without, asks for it.",
            "pattern",
            "Ignore pattern",
            "action:magit-global-gitignore-finish",
            magit_global_mode::spawn_gitignore,
        );
    }

    {
        registry.register_ex_command(
            "magit-blame",
            "Open git blame annotations for a file. With arg: specifies the file path.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        Ok(Args::None)
                    } else {
                        Ok(Args::String(trimmed.to_string()))
                    }
                }),
                apply: Arc::new(|ctx| {
                    let name = if let Args::String(ref path) = ctx.args {
                        format!("*magit:blame:{}*", path)
                    } else {
                        "*magit:blame*".to_string()
                    };
                    let mode_id = "magit-blame-mode".to_string();
                    Ok(Effect::OpenSyntheticBuffer { name, mode_id })
                }),
                args_schema: vec![ArgSpec::optional(
                    "file",
                    lattice_grammar::ArgKind::String,
                    "file path to blame",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
    }
    {
        // MG.23j: the scriptable surface for MG.20's three operations,
        // which shipped as chords and nothing else.
        //
        // Two ways in, the shape MG.23c1 established: `:magit-revert
        // <sha>` acts immediately; bare `:magit-revert` opens the
        // commit picker, which then fires *this same command* with the
        // picked sha appended. That round trip is why the picker takes
        // an ex-command name rather than an action name — see
        // `picker_sources::CommitPickSource`.
        //
        // `reset --hard` returns its confirm here exactly as the chord
        // does: `spawn_commit_op` is not reached until the `-execute`
        // half runs, so answering `n` performs no git call (§12.13).
        for op in [
            magit_global_mode::CommitOp::CHERRY_PICK,
            magit_global_mode::CommitOp::REVERT,
            magit_global_mode::CommitOp::RESET_SOFT,
            magit_global_mode::CommitOp::RESET_MIXED,
            magit_global_mode::CommitOp::RESET_HARD,
        ] {
            registry.register_ex_command(
                op.ex_command,
                // Leaked once at boot, from a `&'static` table — the
                // registry wants `&'static str` docs and these are
                // per-op. Five allocations for the process lifetime.
                Box::leak(
                    format!(
                        "git {} the named commit. With no argument: pick one.",
                        op.what
                    )
                    .into_boxed_str(),
                ),
                ExCommandSpec {
                    latency_class: LatencyClass::Reflex,
                    accepts_bang: false,
                    accepts_range: false,
                    parse_args: Arc::new(|line: &str, _bang: bool| {
                        Ok(Args::String(line.trim().to_string()))
                    }),
                    apply: Arc::new(move |ctx| {
                        let commit = match ctx.args {
                            Args::String(ref s) if !s.trim().is_empty() => s.trim().to_string(),
                            _ => {
                                return Ok(Effect::OpenPicker {
                                    source: picker_sources::COMMIT_PICK_SOURCE.to_string(),
                                    args: vec![op.ex_command.to_string()],
                                });
                            }
                        };
                        Ok(match op.confirm_action {
                            Some(yes) => confirm::ask_target(
                                format!(
                                    "git {} {commit} — discard uncommitted changes?",
                                    op.what
                                ),
                                yes,
                                commit,
                            ),
                            None => {
                                let workdir = lattice_vcs::Repository::discover(".")
                                    .ok()
                                    .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                                    .unwrap_or_default();
                                magit_global_mode::spawn_commit_op(op, workdir, &commit)
                            }
                        })
                    }),
                    args_schema: vec![ArgSpec::optional(
                        "commit",
                        lattice_grammar::ArgKind::String,
                        "commit to act on; omit to pick one",
                    )],
                    surface_form: SurfaceForm::Keyword,
                },
            );
        }
    }
    {
        // MG.23f2: the scriptable half of reverse blame. `C-c f`'s `f`
        // takes both arguments from the blob buffer it is pressed in;
        // this one is told them, which is also the only way to reverse
        // blame a file you are not currently reading at a revision.
        //
        // Both arguments are required — a default revision is exactly
        // what reverse blame cannot have. `HEAD` would make the range
        // `HEAD..HEAD`, i.e. empty, and report every line as still
        // present: a plausible-looking answer that says nothing.
        registry.register_ex_command(
            "magit-blame-reverse",
            "Reverse-blame a file: for each line as of <rev>, the last commit it existed in. \
             Takes `<rev> <path>`.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref spec) = ctx.args else {
                        return Ok(reverse_blame_usage());
                    };
                    match spec.split_once(char::is_whitespace) {
                        Some((rev, path)) if !rev.is_empty() && !path.trim().is_empty() => {
                            Ok(Effect::OpenSyntheticBuffer {
                                name: magit_blame_mode::reverse_buffer_name(rev, path.trim()),
                                mode_id: "magit-blame-mode".to_string(),
                            })
                        }
                        _ => Ok(reverse_blame_usage()),
                    }
                }),
                args_schema: vec![ArgSpec::required(
                    "spec",
                    lattice_grammar::ArgKind::String,
                    "<rev> <path> — the revision to walk forward from, and the file",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
    }
    {
        // Fold audit fix: the upstream to rebase onto is encoded into
        // the buffer name (`*magit:rebase:<upstream>*`), mirroring
        // `magit-blame`'s file-in-buffer-name pattern — `on_activate`
        // extracts it the same way. No arg falls back to
        // `*magit:rebase*`, and the mode resolves `@{upstream}` itself.
        registry.register_ex_command(
            "magit-rebase",
            "Start an interactive rebase. With arg: the upstream ref to rebase onto \
             (default: the branch's configured upstream).",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        Ok(Args::None)
                    } else {
                        Ok(Args::String(trimmed.to_string()))
                    }
                }),
                apply: Arc::new(|ctx| {
                    let name = if let Args::String(ref upstream) = ctx.args {
                        format!("*magit:rebase:{}*", upstream)
                    } else {
                        "*magit:rebase*".to_string()
                    };
                    let mode_id = "magit-rebase-mode".to_string();
                    Ok(Effect::OpenSyntheticBuffer { name, mode_id })
                }),
                args_schema: vec![ArgSpec::optional(
                    "upstream",
                    lattice_grammar::ArgKind::String,
                    "ref to rebase onto",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
    }
    {
        // Fold audit fix: magit-branch's `c` (create) chord was an
        // explicit stub ("needs minibuffer prompt"). This codebase
        // has no generic single-line-prompt-with-callback mechanism
        // yet (`:lsp-rename <new>` takes its arg the same way — typed
        // on the `:` line, not an interactive prompt buffer); `c`
        // points the user here instead of pretending to prompt.
        registry.register_ex_command(
            "magit-branch-create",
            "Create a new branch from HEAD and check it out.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        Err(lattice_grammar::error::CommandError::BadArgs(
                            "magit-branch-create: branch name required".to_string(),
                        ))
                    } else {
                        Ok(Args::String(trimmed.to_string()))
                    }
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref name) = ctx.args else {
                        return Ok(Effect::Echo {
                            level: lattice_grammar::EchoLevel::Error,
                            text: "magit-branch-create: branch name required".to_string(),
                        });
                    };
                    let name = name.clone();
                    tokio::task::spawn(tokio::task::spawn_blocking(move || {
                        let Ok(repo) = lattice_vcs::Repository::discover(".") else {
                            tracing::error!(target: "lattice_magit", "branch create: repo discover failed");
                            return;
                        };
                        if let Err(e) = lattice_vcs::Branch::create(&repo, &name, true, None) {
                            tracing::error!(target: "lattice_magit", "branch create {name}: {e}");
                        }
                    }));
                    Ok(Effect::Echo {
                        level: lattice_grammar::EchoLevel::Info,
                        text: "magit: creating branch…".to_string(),
                    })
                }),
                args_schema: vec![ArgSpec::required(
                    "name",
                    lattice_grammar::ArgKind::String,
                    "new branch name",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
    }
}

/// Register every `action:magit-*` command so that mode keymap
/// entries resolve against the registry. Each action is a dead
/// marker returning `Effect::None` — the real handler is registered
/// per-buffer via `ActionHandlerRegistry` in `on_activate`.
fn register_action_commands(registry: &mut CommandRegistry) {
    let none = Some(Arc::new(
        |_: &lattice_grammar::ActionContext| -> GrammarResult<Effect> { Ok(Effect::None) },
    )
        as Arc<
            dyn Fn(&lattice_grammar::ActionContext) -> GrammarResult<Effect> + Send + Sync,
        >);

    let mut reg = |name: &str, doc: &str| {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: none.clone().unwrap(),
                args_schema: Vec::new(),
            },
        );
    };

    // magit-status-mode
    reg("action:magit-stage", "Stage the hunk or file at cursor");
    reg("action:magit-unstage", "Unstage the hunk or file at cursor");
    reg("action:magit-discard", "Discard the hunk or file at cursor");
    reg(
        "action:magit-discard-execute",
        "Execute the discard after confirmation",
    );
    reg("action:magit-commit", "Open the commit buffer");
    reg("action:magit-commit-amend", "Amend the previous commit");
    reg("action:magit-toggle-diff", "Toggle inline diff at cursor");
    reg(
        "action:magit-diff-file",
        "Open file diff in a dedicated buffer",
    );
    reg(
        "action:magit-stage-patch",
        "Stage hunk interactively (git add -p)",
    );
    reg("action:magit-visit", "Context-aware open/visit at cursor");
    // MG.23g: owned by magit-core-mode, so they work in every view that
    // shows a committed patch (revision, stash detail).
    reg(
        "action:magit-apply-hunk",
        "Apply the hunk at cursor to the working tree",
    );
    reg(
        "action:magit-reverse-hunk",
        "Reverse the hunk at cursor out of the working tree",
    );
    // MG.23h: magit's `magit-status-jump`, one action per section we
    // render. Owned by magit-status-mode — it is the only view with
    // sections to jump between.
    reg(
        "action:magit-jump-staged",
        "Jump to the Staged changes section",
    );
    reg(
        "action:magit-jump-unstaged",
        "Jump to the Unstaged changes section",
    );
    reg(
        "action:magit-jump-untracked",
        "Jump to the Untracked files section",
    );
    reg("action:magit-jump-stashes", "Jump to the Stashes section");
    reg(
        "action:magit-jump-commits",
        "Jump to the Recent commits section",
    );

    // magit-diff-mode
    reg(
        "action:magit-diff-visit-file",
        "Visit the file at cursor (working tree, or the index blob for a Staged-scoped diff)",
    );

    // magit-file-revision-mode
    reg(
        "action:magit-blob-previous",
        "Visit this file at the previous revision",
    );
    reg(
        "action:magit-blob-next",
        "Visit this file at the next revision",
    );

    // magit-core-mode
    reg("action:magit-refresh", "Refresh the current magit buffer");
    reg("action:magit-close", "Close the magit buffer (bury)");
    reg(
        "action:magit-next-section",
        "Jump to the next top-level section",
    );
    reg(
        "action:magit-prev-section",
        "Jump to the previous top-level section",
    );
    reg(
        "action:magit-next-file",
        "Jump to the next file/entry in the current section",
    );
    reg(
        "action:magit-prev-file",
        "Jump to the previous file/entry in the current section",
    );
    reg("action:magit-next-hunk", "Jump to the next hunk");
    reg("action:magit-prev-hunk", "Jump to the previous hunk");
    reg(
        "action:magit-toggle-fold",
        "Toggle section/hunk fold at cursor",
    );
    reg("action:magit-cycle-sections", "Cycle section visibility");

    // magit-commit-mode
    reg(
        "action:magit-commit-confirm",
        "Create the commit with the entered message",
    );
    reg("action:magit-commit-abort", "Abort the commit");
    reg(
        "action:magit-commit-visit-file",
        "Visit the staged file at cursor (index blob, not the working tree)",
    );

    // magit-log-mode
    reg(
        "action:magit-log-show-commit",
        "Show the commit detail at cursor",
    );

    // magit-revision-mode
    reg(
        "action:magit-revision-visit-file",
        "Visit the file at cursor as of this commit",
    );

    // magit-blame-mode
    reg(
        "action:magit-blame-show-commit",
        "Show the commit for the blamed line",
    );
    reg("action:magit-blame-parent", "Re-blame at the parent commit");
    // MG.23f2. Deliberately NOT in `FILE_TARGET_ACTIONS`: it needs a
    // revision as well as a path, and takes both from the blob buffer
    // it is invoked in — a `file` argument alone could not say which
    // revision to walk forward from. See its handler for why that
    // restricts it to blob buffers.
    reg(
        "action:magit-global-file-blame-reverse",
        "For each line of this revision of the file, the last commit it existed in",
    );

    // magit-stash-mode
    reg("action:magit-stash-apply", "Apply the stash at cursor");
    reg("action:magit-stash-pop", "Pop the stash at cursor");
    reg("action:magit-stash-drop", "Drop the stash at cursor");
    // MG.12: the git call lives here, behind `magit-stash-drop`'s
    // `Effect::Confirm`. See `confirm::DESTRUCTIVE_ACTIONS`.
    reg(
        "action:magit-stash-drop-execute",
        "Execute the stash drop after confirmation",
    );
    reg("action:magit-stash-create", "Create a new stash");
    // MG.15
    reg(
        "action:magit-stash-show",
        "Show the patch of the stash at cursor",
    );

    // MG.20: operations on the commit under the cursor. Owned by
    // magit-core-mode, so they work in every view that shows a commit
    // (log, status's Recent commits, revision, rebase todo).
    reg(
        "action:magit-cherry-pick",
        "Cherry-pick the commit at cursor onto the current branch",
    );
    reg(
        "action:magit-revert",
        "Revert the commit at cursor (creates an inverse commit)",
    );
    reg(
        "action:magit-reset-soft",
        "Reset --soft to the commit at cursor (keeps index + working tree)",
    );
    reg(
        "action:magit-reset-mixed",
        "Reset --mixed to the commit at cursor (keeps working tree)",
    );
    reg(
        "action:magit-reset-hard",
        "Reset --hard to the commit at cursor (DISCARDS working tree; asks first)",
    );
    reg(
        "action:magit-reset-hard-execute",
        "Execute the hard reset after confirmation",
    );

    // magit-branch-mode
    reg(
        "action:magit-branch-checkout",
        "Check out the branch at cursor",
    );
    reg("action:magit-branch-create", "Create a new branch");
    reg("action:magit-branch-delete", "Delete the branch at cursor");
    // MG.12: `Branch::delete` is a force delete (`-D`), so it drops
    // unmerged commits — the git call lives here, behind
    // `magit-branch-delete`'s `Effect::Confirm`.
    reg(
        "action:magit-branch-delete-execute",
        "Execute the branch delete after confirmation",
    );
    reg(
        "action:magit-branch-merge",
        "Merge the branch at cursor into current",
    );

    // magit-rebase-mode
    reg("action:magit-rebase-confirm", "Execute the rebase");
    reg("action:magit-rebase-abort", "Abort the rebase");
    // MG.12: only fired when a rebase is actually in progress — see
    // `magit_rebase_mode`'s abort handler, which closes the pane
    // outright when there is nothing to throw away.
    reg(
        "action:magit-rebase-abort-execute",
        "Execute the rebase abort after confirmation",
    );
    reg(
        "action:magit-rebase-show-commit",
        "Show the commit detail at cursor",
    );

    // magit-global-mode (Universal — always active, unlike every
    // action above which only has a live handler while its owning
    // buffer is open). Backs the `magit-dispatch` root transient's
    // items so pressing a key inside it works from ANY buffer, not
    // just from within the matching magit buffer kind.
    reg("action:magit-global-status", "Open the status buffer");
    reg("action:magit-global-commit", "Open the commit buffer");
    reg("action:magit-global-amend", "Amend the previous commit");
    reg("action:magit-global-log", "Open the log buffer");
    reg("action:magit-global-diff", "Open the diff buffer");
    reg("action:magit-global-branch", "Open the branch list");
    reg("action:magit-global-stash", "Open the stash list");
    reg(
        "action:magit-global-stash-create",
        "Stash the working tree (git stash push)",
    );
    reg("action:magit-global-rebase", "Start an interactive rebase");
    reg(
        "action:magit-global-fetch",
        "Fetch from the remote without merging",
    );
    reg(
        "action:magit-global-pull",
        "Fetch + fast-forward merge from the remote",
    );
    reg("action:magit-global-push", "Push to the remote");

    // MG.23a: the six file-dispatch actions declare an optional
    // `file` argument. `C-c f` leaves it unset and they act on the
    // visited file; `:magit-other-file-dispatch` sets it, which is how
    // a stand-alone invocation names a file it is not visiting. The
    // name must match the transient `Argument`'s name — the host maps
    // transient state onto the schema BY NAME
    // (`project_transient_state`), so a typo here degrades silently to
    // "always the current file".
    // MG.23c1: prompt-backed repo operations.
    reg("action:magit-global-tag", "Tag HEAD (asks for the name)");
    reg(
        "action:magit-global-tag-finish",
        "Create the tag with the typed name",
    );
    reg(
        "action:magit-global-gitignore",
        "Add a pattern to .gitignore (asks for it)",
    );
    reg(
        "action:magit-global-gitignore-finish",
        "Append the typed pattern to .gitignore",
    );

    // MG.23d: file operations.
    reg(
        "action:magit-global-file-untrack",
        "Stop tracking the file, keeping it on disk",
    );
    reg(
        "action:magit-global-file-delete",
        "Delete the file (asks first)",
    );
    reg(
        "action:magit-global-file-delete-execute",
        "Delete the file after confirmation",
    );
    reg(
        "action:magit-global-file-rename",
        "Rename the file (asks for the new name)",
    );
    // MG.23d2
    reg(
        "action:magit-global-file-checkout",
        "Check the file out from a revision (asks for it, then confirms)",
    );
    reg(
        "action:magit-global-file-checkout-finish",
        "Confirm checking the file out from the typed revision",
    );
    reg(
        "action:magit-global-file-rename-finish",
        "Rename the file to the typed name",
    );

    // MG.23c2
    reg(
        "action:magit-global-init",
        "Initialize a git repository (asks for the directory)",
    );
    reg(
        "action:magit-global-init-finish",
        "Run git init in the typed directory",
    );
    reg("action:magit-global-merge", "Merge a branch (asks which)");
    reg(
        "action:magit-global-merge-finish",
        "Merge the typed branch into the current one",
    );

    // MG.23b: repo-wide index operations (magit's `S` / `U`).
    reg(
        "action:magit-global-stage-all",
        "Stage every tracked modification",
    );
    reg("action:magit-global-unstage-all", "Unstage everything");

    // File-dispatch (`C-c f`) — file-level operations scoped to the
    // buffer active when the transient was opened.
    reg(
        "action:magit-global-file-stage",
        "Stage the file in the current buffer",
    );
    reg(
        "action:magit-global-file-unstage",
        "Unstage the file in the current buffer",
    );
    reg(
        "action:magit-global-file-discard",
        "Discard changes to the file in the current buffer",
    );
    reg(
        "action:magit-global-file-discard-execute",
        "Execute the file discard after confirmation",
    );
    reg(
        "action:magit-global-file-diff",
        "Show diff for the file in the current buffer",
    );
    reg(
        "action:magit-global-file-log",
        "Show commit history for the file in the current buffer",
    );
    reg(
        "action:magit-global-file-blame",
        "Blame the file in the current buffer",
    );

    // Branch-create wizard (`c` in magit-branch-mode): fired by the
    // prompt opened after picking a base branch via
    // `magit-branch-pick-base`. Global like the others above — the
    // prompt buffer isn't a magit-status/-branch buffer, so this
    // can't be a per-buffer handler.
    reg(
        "action:magit-branch-create-finish",
        "Create the new branch (from the picked base) with the typed name",
    );
    drop(reg);

    // MG.23a: the six file-dispatch actions gain an optional `file`
    // argument, re-registered here (rather than at their `reg` above)
    // because `reg` holds `registry` for its own lifetime and two
    // closures cannot both borrow it.
    //
    // `C-c f` leaves the argument unset and the action falls back to the
    // visited file — the one deliberate deviation from magit, which
    // prompts. `:magit-other-file-dispatch` sets it, which is how a
    // stand-alone invocation names a file it is not visiting.
    //
    // The name must match the transient `Argument`'s name: the host maps
    // transient state onto the schema BY NAME
    // (`project_transient_state`), so a mismatch degrades silently to
    // "always the current file" rather than failing.
    // IX.2: the execute half of every destructive pair declares the
    // slots its ask half carries, so the confirm dialog's state
    // projects onto them by name.
    for (name, doc, slots) in CONFIRM_TARGET_ACTIONS {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: none.clone().unwrap(),
                args_schema: slots
                    .iter()
                    .map(|(slot, slot_doc)| {
                        ArgSpec::optional(*slot, lattice_grammar::ArgKind::String, *slot_doc)
                    })
                    .collect(),
            },
        );
    }

    for (name, doc) in FILE_TARGET_ACTIONS {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: none.clone().unwrap(),
                args_schema: vec![ArgSpec::optional(
                    "file",
                    lattice_grammar::ArgKind::String,
                    "Repo-relative path to act on; the visited file when unset",
                )],
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect every leaf item in `spec` — RECURSING through
    /// submenus — that did NOT resolve to a real `Action`.
    /// `action_or_placeholder` silently downgrades an unresolved id
    /// to an inert `Flag` (see its doc comment), which from the
    /// user's side looks EXACTLY like "pressing the key does
    /// nothing": no error, no effect, transient stays open.
    /// Recursion matters because the root dispatch's `c` (commit)
    /// and `z` (stash) items are submenus whose own leaves would
    /// otherwise go unchecked.
    ///
    /// Returns findings rather than panicking so the vacuity test
    /// below can assert the walker actually FINDS inert items on a
    /// deliberately-unresolved spec (`TransientSpec` holds a
    /// `Box<dyn Fn>` preview, so it isn't `UnwindSafe` and can't be
    /// probed via `catch_unwind`).
    ///
    /// **MG.17a:** `Flag` stopped being a reliable inert-marker when
    /// real flags landed (`--force-with-lease`, `--prune`, …). A Flag
    /// whose name appears in some `RemoteOp::flags` table is a genuine
    /// toggle; anything else is still the placeholder fallback. That
    /// keeps the guard precise without hand-listing exceptions — adding
    /// a flag to a `RemoteOp` makes it legitimate automatically, and a
    /// placeholder can never match because placeholders are named after
    /// the action they failed to resolve.
    fn declared_flag_names() -> std::collections::HashSet<&'static str> {
        use magit_global_mode::RemoteOp;
        [
            RemoteOp::PULL,
            RemoteOp::PUSH,
            RemoteOp::FETCH,
            RemoteOp::STASH,
        ]
        .iter()
        .flat_map(|op| op.flags.iter().map(|f| f.name))
        .collect()
    }

    fn inert_items(spec: &lattice_picker::TransientSpec, path: &str) -> Vec<String> {
        let mut found = Vec::new();
        for group in &spec.groups {
            for item in &group.items {
                let where_ = format!("{path}{} / {}", group.label, item.label);
                match &item.kind {
                    lattice_picker::TransientItemKind::Action(_) => {}
                    lattice_picker::TransientItemKind::Submenu(sub) => {
                        found.extend(inert_items(sub, &format!("{where_} > ")));
                    }
                    lattice_picker::TransientItemKind::Flag { name, .. } => {
                        if !declared_flag_names().contains(name.as_str()) {
                            found.push(format!(
                                "'{where_}' fell back to an inert Flag placeholder \
                                 named '{name}' — its action id failed to resolve"
                            ));
                        }
                    }
                    // MG.17b: `Argument` became a real item kind. Same
                    // rule as `Flag` — legitimate when the name is
                    // declared in a `RemoteOp` table, suspect otherwise.
                    lattice_picker::TransientItemKind::Argument { name, .. } => {
                        if !declared_flag_names().contains(name.as_str()) {
                            found.push(format!(
                                "'{where_}' is an Argument named '{name}' that no \
                                 RemoteOp declares — nothing will consume its value"
                            ));
                        }
                    }
                    other => found.push(format!("unexpected item kind for '{where_}': {other:?}")),
                }
            }
        }
        found
    }

    /// MG.23h: `C-c g` pressed in an ordinary file buffer.
    fn outside_magit() -> lattice_picker::TransientContext {
        lattice_picker::TransientContext::default()
    }

    /// `C-c g` pressed in the status buffer — both predicates true.
    fn in_magit_status() -> lattice_picker::TransientContext {
        lattice_picker::TransientContext {
            major_mode: Some(MagitStatusMode::mode_id().as_str().to_string()),
            minor_modes: vec![MagitCoreMode::mode_id().as_str().to_string()],
        }
    }

    /// `C-c g` pressed in a magit buffer that is NOT the status buffer
    /// — the family predicate true, the exact-major one false.
    fn in_magit_log() -> lattice_picker::TransientContext {
        lattice_picker::TransientContext {
            major_mode: Some(MagitLogMode::mode_id().as_str().to_string()),
            minor_modes: vec![MagitCoreMode::mode_id().as_str().to_string()],
        }
    }

    /// Every key at the top level of `spec`, in order.
    fn top_level_keys(spec: &lattice_picker::TransientSpec) -> Vec<String> {
        spec.groups
            .iter()
            .flat_map(|g| &g.items)
            .flat_map(|i| i.key.clone())
            .collect()
    }

    fn assert_no_inert_items(spec: &lattice_picker::TransientSpec) {
        let found = inert_items(spec, "");
        assert!(
            found.is_empty(),
            "inert transient items:\n  {}",
            found.join("\n  ")
        );
    }

    /// MG.13 guard for the shared-action collision class.
    ///
    /// `Mode::action_handlers()` contributions are registered at boot
    /// into a map keyed by `CommandId` — `register` *inserts*, so two
    /// modes contributing the same `action_name` means the second
    /// silently replaces the first and one of them is dead. Worse,
    /// dropping either registration unregisters *by action id*, taking
    /// the survivor with it.
    ///
    /// `action:magit-refresh` (`gr`) is the live example: five modes
    /// bound it. It is now registered once by `magit-core-mode` and
    /// dispatched per-buffer through `buffer_state::MagitView`. This
    /// test fails if any future mode re-adds a duplicate contribution
    /// rather than publishing a view.
    #[test]
    fn no_two_modes_contribute_the_same_boot_action_handler() {
        use lattice_mode::Mode;
        let mut seen: Vec<(&'static str, &'static str)> = Vec::new();
        let mut collisions: Vec<String> = Vec::new();

        macro_rules! collect {
            ($mode:expr, $label:literal) => {
                for c in $mode.action_handlers() {
                    if let Some((prior, _)) = seen.iter().find(|(n, _)| *n == c.action_name) {
                        let _ = prior;
                        let owner = seen
                            .iter()
                            .find(|(n, _)| *n == c.action_name)
                            .map(|(_, o)| *o)
                            .unwrap_or("?");
                        collisions.push(format!(
                            "`{}` contributed by both `{}` and `{}` — the second \
                             replaces the first at boot and dropping either kills \
                             both; publish a `MagitView` instead",
                            c.action_name, owner, $label
                        ));
                    } else {
                        seen.push((c.action_name, $label));
                    }
                }
            };
        }

        collect!(MagitGlobalMode, "magit-global-mode");
        collect!(MagitCoreMode, "magit-core-mode");
        collect!(MagitStatusMode, "magit-status-mode");
        collect!(MagitCommitMode, "magit-commit-mode");
        collect!(MagitDiffMode, "magit-diff-mode");
        collect!(MagitLogMode, "magit-log-mode");
        collect!(MagitBlameMode, "magit-blame-mode");
        collect!(MagitStashMode, "magit-stash-mode");
        collect!(MagitBranchMode, "magit-branch-mode");
        collect!(MagitRebaseMode, "magit-rebase-mode");
        collect!(MagitRevisionMode, "magit-revision-mode");
        collect!(MagitFileRevisionMode, "magit-file-revision-mode");

        assert!(
            collisions.is_empty(),
            "duplicate boot action handlers:\n  {}",
            collisions.join("\n  ")
        );
    }

    /// IX.2 — every destructive pair's execute half declares the slots
    /// its ask half carries.
    ///
    /// The projection is **by name**, so an ask that carries `"branch"`
    /// against an execute declaring `"ref"` does not fail — the value
    /// lands nowhere and the handler silently falls back to re-deriving
    /// from the cursor, which is precisely the bug IX.1 removed. This
    /// pins the two halves to one table.
    #[test]
    fn every_destructive_execute_declares_the_slots_its_confirm_carries() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        for (name, _, slots) in CONFIRM_TARGET_ACTIONS {
            let spec = registry
                .lookup_by_name(name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let declared: Vec<&str> = spec.args_schema.iter().map(|a| a.name.as_ref()).collect();
            let expected: Vec<&str> = slots.iter().map(|(s, _)| *s).collect();
            assert_eq!(
                declared, expected,
                "`{name}`'s schema must match the slots its confirm carries, \
                 in order — the host projects positionally into these names"
            );
        }
    }

    /// Every execute half in the destructive table is one, and every
    /// destructive pair that *can* carry a target does.
    ///
    /// `magit-rebase-abort-execute` is the deliberate exception: there
    /// is exactly one in-progress rebase, so it has no target to name.
    #[test]
    fn every_destructive_pair_carries_a_target_except_the_one_with_none() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        for (_, execute) in confirm::DESTRUCTIVE_ACTIONS {
            if *execute == "action:magit-rebase-abort-execute" {
                continue;
            }
            // Checked against the registry rather than one table: an
            // execute half may declare its slots via
            // `CONFIRM_TARGET_ACTIONS` or, for the `C-c f` family, via
            // `FILE_TARGET_ACTIONS`. What matters is that it declares
            // somewhere to *receive* a target — an empty schema means
            // the carried value has nowhere to land and the handler
            // silently re-derives.
            let spec = registry
                .lookup_by_name(execute)
                .unwrap_or_else(|| panic!("`{execute}` is registered"));
            assert!(
                !spec.args_schema.is_empty(),
                "`{execute}` is destructive but declares no argument slot — a \
                 carried target would have nowhere to land, so it would \
                 re-derive at answer time, and a refresh landing while the \
                 dialog is open makes that a different target"
            );
        }
    }

    // ── MG.23h: the menu varies with where it was opened ──

    /// The section-acting rows appear in any magit buffer and nowhere
    /// else — the `:if-derived magit-mode` half.
    ///
    /// They resolve the hunk under the cursor, so outside a magit
    /// buffer there is no diff text for them to find one in and the row
    /// would be a key that explains why it did nothing. Both directions
    /// are asserted: a gate that never opens and a gate that never
    /// closes both pass a one-sided test.
    #[test]
    fn the_section_acting_rows_appear_only_inside_a_magit_buffer() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = resolve_dispatch_ids(&registry);

        for ctx in [in_magit_status(), in_magit_log()] {
            let keys = top_level_keys(&transients::dispatch_transient(&ids, &ctx));
            for k in ["a", "-", "x"] {
                assert!(
                    keys.contains(&k.to_string()),
                    "`{k}` must be offered in a magit buffer: {keys:?}"
                );
            }
        }

        let keys = top_level_keys(&transients::dispatch_transient(&ids, &outside_magit()));
        for k in ["a", "-", "x"] {
            assert!(
                !keys.contains(&k.to_string()),
                "`{k}` acts on the hunk at cursor — it must not appear \
                 outside a magit buffer: {keys:?}"
            );
        }
        // ...while the repo-wide pair is there in every context, which
        // is where we are deliberately more permissive than magit.
        for ctx in [in_magit_status(), in_magit_log(), outside_magit()] {
            let keys = top_level_keys(&transients::dispatch_transient(&ids, &ctx));
            assert!(keys.contains(&"S".to_string()) && keys.contains(&"U".to_string()));
        }
    }

    /// The `s` row swaps meaning in magit-status and only there — the
    /// `:if-mode` half, which is a different predicate from the one
    /// above and would be indistinguishable from it if only the status
    /// buffer were tested.
    #[test]
    fn the_status_row_becomes_a_section_jump_only_in_the_status_buffer() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = resolve_dispatch_ids(&registry);

        let row = |ctx: &lattice_picker::TransientContext| {
            transients::dispatch_transient(&ids, ctx)
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.key.iter().any(|k| k == "s"))
                .map(|i| {
                    (
                        i.label.clone(),
                        matches!(i.kind, lattice_picker::TransientItemKind::Submenu(_)),
                    )
                })
                .expect("`s` is always offered")
        };

        assert_eq!(
            row(&in_magit_status()),
            ("jump".to_string(), true),
            "in the status buffer, `s` must be the section-jump submenu \
             — opening the buffer you are already in is a no-op"
        );
        for ctx in [in_magit_log(), outside_magit()] {
            let (label, is_submenu) = row(&ctx);
            assert_eq!(label, "status");
            assert!(
                !is_submenu,
                "outside the status buffer, `s` opens it — a magit-log \
                 buffer has no sections to jump between"
            );
        }
    }

    /// Whatever the context, no two rows at one level share a key.
    ///
    /// This is the guard the gating actually needs: the added rows land
    /// in an existing menu, and `-`/`x`/`a` colliding with something
    /// already there would make one of them unreachable with no error.
    #[test]
    fn no_context_produces_a_duplicate_key_in_the_dispatch() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = resolve_dispatch_ids(&registry);
        for ctx in [in_magit_status(), in_magit_log(), outside_magit()] {
            let keys = top_level_keys(&transients::dispatch_transient(&ids, &ctx));
            let mut seen = std::collections::HashSet::new();
            for k in &keys {
                assert!(seen.insert(k.clone()), "duplicate key `{k}` in {keys:?}");
            }
        }
    }

    /// Every jump row resolves, and every section we render has one.
    ///
    /// The prefixes the handlers scan for are the same constants that
    /// render the headers, so this pins that the submenu covers all of
    /// them rather than whichever the author remembered.
    #[test]
    fn the_jump_submenu_covers_every_section_we_render() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = resolve_dispatch_ids(&registry);
        let spec = transients::dispatch_transient(&ids, &in_magit_status());
        let jump = spec
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .find_map(|i| match &i.kind {
                lattice_picker::TransientItemKind::Submenu(sub) if i.label == "jump" => {
                    Some(std::sync::Arc::clone(sub))
                }
                _ => None,
            })
            .expect("the jump submenu");
        assert_eq!(
            jump.selectable_count(),
            sections::SECTION_HEADER_PREFIXES.len(),
            "one row per rendered section, no more and no fewer"
        );
        assert_no_inert_items(&jump);
    }

    /// MG.23j — every commit op is reachable by its ex-command name,
    /// which is what the picker fires.
    ///
    /// The picker builds the ex line `"<ex_command> <sha>"` and hands
    /// it to the host, which runs it as typed. A name that no command
    /// answers produces a picker that lists commits, accepts one, and
    /// does nothing — with no error, because an unknown ex-command
    /// inside an accept path is not the same as one typed on the `:`
    /// line.
    #[test]
    fn every_commit_ops_ex_command_is_registered() {
        let mut registry = CommandRegistry::new();
        register_ex_commands(&mut registry);
        for op in [
            magit_global_mode::CommitOp::CHERRY_PICK,
            magit_global_mode::CommitOp::REVERT,
            magit_global_mode::CommitOp::RESET_SOFT,
            magit_global_mode::CommitOp::RESET_MIXED,
            magit_global_mode::CommitOp::RESET_HARD,
        ] {
            let id = registry.id_by_name(op.ex_command).unwrap_or_else(|| {
                panic!(
                    "`:{}` must exist — the commit picker fires it by name",
                    op.ex_command
                )
            });
            assert!(
                registry.ex_command_spec(id).is_some(),
                "`:{}` must be an EX command, not an action of the same name",
                op.ex_command
            );
        }
    }

    /// The repo-level rows fire the SAME actions the chords fire, so a
    /// row cannot drift onto a second handler with its own idea of the
    /// confirm contract.
    #[test]
    fn the_commit_rows_reuse_the_chords_actions() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = resolve_dispatch_ids(&registry);
        for (row, action) in [
            (ids.cherry_pick, "action:magit-cherry-pick"),
            (ids.revert, "action:magit-revert"),
            (ids.reset_soft, "action:magit-reset-soft"),
            (ids.reset_mixed, "action:magit-reset-mixed"),
            (ids.reset_hard, "action:magit-reset-hard"),
        ] {
            assert_eq!(
                row,
                registry.id_by_name(action),
                "the `{action}` row must fire that action, not a twin"
            );
        }
    }

    /// MG.23a — every `C-c f` action declares the optional `file`
    /// target, and declares it under the name the transient uses.
    ///
    /// The host maps transient state onto an action's schema **by name**
    /// (`project_transient_state`), so a mismatch here does not fail —
    /// it silently degrades `:magit-other-file-dispatch` to "always the
    /// visited file", which looks like the feature working on the wrong
    /// file rather than like a bug.
    #[test]
    fn every_file_dispatch_action_takes_the_file_target_under_that_name() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        for (name, _) in FILE_TARGET_ACTIONS {
            let spec = registry
                .lookup_by_name(name)
                .unwrap_or_else(|| panic!("`{name}` is registered"));
            let schema = &spec.args_schema;
            assert_eq!(
                schema.len(),
                1,
                "`{name}` should declare exactly the file target, got {schema:?}"
            );
            assert_eq!(
                schema[0].name.as_ref(),
                "file",
                "`{name}`'s target arg must be named `file` — the transient \
                 Argument is matched by name, and a mismatch silently means \
                 'always the visited file'"
            );
        }
    }

    /// The target argument the menu offers must be the one the actions
    /// read. Both halves are checked against the literal `"file"` above
    /// and here, so neither can be renamed alone.
    #[test]
    fn the_other_file_menu_offers_the_file_argument_the_actions_read() {
        let spec = transients::other_file_dispatch_transient(&Default::default());
        let named_file = spec.groups.iter().flat_map(|g| &g.items).any(|item| {
            matches!(
                &item.kind,
                lattice_picker::TransientItemKind::Argument { name, .. } if name == "file"
            )
        });
        assert!(
            named_file,
            "the menu must expose an `Argument` named `file`, or no row can \
             ever act on anything but the visited file"
        );
    }

    /// IX.7 — the other-file menu's destructive row carries its target.
    ///
    /// Replaces MG.23a's "no destructive row here" guard, which existed
    /// because `Effect::Confirm` opened a transient of its own and lost
    /// the target with it: the execute half fell back to the visited
    /// file and acted on something the prompt never named. IX.1/IX.2
    /// made the confirm carry its target, so the row is safe — and this
    /// test is what keeps it safe, by asserting the carrying rather than
    /// the absence.
    ///
    /// Exercises the whole chain: an argument on the context reaches the
    /// ask half, which puts it in the `Confirm` the host will seed the
    /// dialog from.
    #[test]
    fn the_other_file_menus_discard_carries_the_file_it_names() {
        use lattice_mode::Mode;

        let handler = MagitGlobalMode
            .action_handlers()
            .into_iter()
            .find(|c| c.action_name == "action:magit-global-file-discard")
            .expect("the ask half is contributed")
            .handler;

        let services = lattice_mode::ServiceRegistry::new();
        let events = lattice_runtime::EventBus::new();
        let ctx = lattice_mode::ActionContext {
            buffer_id: lattice_protocol::ids::BufferId::new(1),
            cursor: lattice_protocol::position::Position::new(0, 0),
            selection: None,
            services: &services,
            events: &events,
            prompt_value: None,
            // What `:magit-other-file-dispatch`'s `=f` row supplies.
            args: lattice_grammar::Args::List(vec![lattice_grammar::ArgValue::String(
                "Cargo.toml".to_string(),
            )]),
        };

        match handler(&ctx) {
            Some(lattice_grammar::Effect::Confirm { prompt, args, .. }) => {
                assert!(
                    prompt.contains("Cargo.toml"),
                    "the prompt names the target it will act on: {prompt}"
                );
                let carried = args.as_list().expect("the target is carried");
                assert!(
                    matches!(
                        &carried[0],
                        lattice_grammar::ArgValue::String(p) if p == "Cargo.toml"
                    ),
                    "and the execute half receives that same target, rather \
                     than re-deriving the visited file: {carried:?}"
                );
            }
            other => panic!("expected a Confirm carrying its target, got {other:?}"),
        }
    }

    /// MG.23f2 — a `BufferStore` that knows one thing: what a buffer is
    /// called. That is the only method reverse blame reads, and stubbing
    /// the rest keeps the test about the resolution rather than about
    /// standing up a registry.
    struct NamedBuffer(&'static str);

    impl lattice_mode::BufferStore for NamedBuffer {
        fn find_by_name(&self, _name: &str) -> Option<lattice_core::BufferId> {
            None
        }
        fn handle_for(
            &self,
            _id: lattice_core::BufferId,
        ) -> Option<std::sync::Arc<dyn lattice_runtime::Document>> {
            None
        }
        fn name_for(&self, _id: lattice_core::BufferId) -> Option<String> {
            Some(self.0.to_string())
        }
        fn insert_document_buffer(
            &self,
            _id: lattice_core::BufferId,
            _kind: lattice_core::BufferKind,
            _handle: std::sync::Arc<dyn lattice_runtime::Document>,
            _flags: lattice_core::BufferFlags,
            _name: Option<String>,
        ) {
        }
    }

    /// Fire `action:magit-global-file-blame-reverse` as if `C-c f`'s
    /// `f` were pressed in a buffer called `buffer_name`.
    fn fire_reverse_blame_in(buffer_name: &'static str) -> Option<Effect> {
        use lattice_mode::Mode;

        let handler = MagitGlobalMode
            .action_handlers()
            .into_iter()
            .find(|c| c.action_name == "action:magit-global-file-blame-reverse")
            .expect("reverse blame is contributed")
            .handler;

        let mut services = lattice_mode::ServiceRegistry::new();
        // Registered as `BufferStoreHandle`, NOT `Arc<BufferStoreHandle>`
        // — `register` keys on `TypeId::of::<T>()` and the handler looks
        // up `get::<BufferStoreHandle>()`, so the wrapped form would be
        // filed under a type nobody asks for and every case would come
        // back as the refusal (`feedback_servicesregistry_arc_typeid`).
        services.register(lattice_mode::BufferStoreHandle::new(std::sync::Arc::new(
            NamedBuffer(buffer_name),
        )));
        let events = lattice_runtime::EventBus::new();
        handler(&lattice_mode::ActionContext {
            buffer_id: lattice_protocol::ids::BufferId::new(1),
            cursor: lattice_protocol::position::Position::new(0, 0),
            selection: None,
            services: &services,
            events: &events,
            prompt_value: None,
            args: lattice_grammar::Args::None,
        })
    }

    /// MG.23f2 — both halves come out of the blob buffer's name.
    #[test]
    fn reverse_blame_takes_its_revision_from_the_blob_buffer_it_runs_in() {
        match fire_reverse_blame_in("*magit:file:a1b2c3d:src/main.rs*") {
            Some(Effect::OpenSyntheticBuffer { name, mode_id }) => {
                assert_eq!(name, "*magit:blame-reverse:a1b2c3d:src/main.rs*");
                assert_eq!(mode_id, "magit-blame-mode");
            }
            other => panic!("expected the reverse-blame buffer, got {other:?}"),
        }
    }

    /// Refusals are echoed, never silent. A handler returning `None`
    /// here would leave the menu row looking like a key that does
    /// nothing — the exact failure the no-inert-rows policy exists to
    /// prevent, arrived at from the other direction.
    ///
    /// `staged` is in the list deliberately: the index is not a commit,
    /// so there is no range to walk forward from — the same exclusion
    /// `gj`/`gk` make.
    #[test]
    fn reverse_blame_says_why_when_there_is_no_revision_to_walk_from() {
        for name in [
            "*magit:file:staged:src/main.rs*",
            "*magit:status*",
            "src/main.rs",
        ] {
            match fire_reverse_blame_in(name) {
                Some(Effect::Echo { level, text }) => {
                    assert_eq!(level, lattice_grammar::EchoLevel::Error);
                    assert!(
                        text.contains("revision"),
                        "the message must name what is missing: {text}"
                    );
                }
                other => panic!("expected an explained refusal in {name}, got {other:?}"),
            }
        }
    }

    /// No magit chord may shadow a Visual-mode entry key.
    ///
    /// Region staging needs a selection, so `v` / `V` / `C-v` have to
    /// keep meaning what vim says they mean in every magit buffer. A
    /// mode action that binds one of them takes it *unconditionally* —
    /// the chord is consumed even when the action has no target, because
    /// a handler returning `None` counts as handled. That is how revert
    /// on `V` made region staging unreachable before it moved to `_`
    /// (evil-collection-magit's key).
    ///
    /// The failure is silent from the code's side: the binding looks
    /// fine, the action works on the rows it applies to, and only the
    /// selection gesture quietly stops existing.
    #[test]
    fn no_magit_mode_binds_a_visual_entry_key() {
        use lattice_mode::Mode;

        const VISUAL_ENTRY: &[&str] = &["v", "V", "<C-v>"];
        let mut stolen: Vec<String> = Vec::new();
        macro_rules! check {
            ($($mode:expr => $label:literal),* $(,)?) => {
                $(for entry in $mode.keymap().entries {
                    if VISUAL_ENTRY.contains(&entry.chord)
                        && entry.modes.contains(&lattice_keymap::BindingMode::Normal)
                    {
                        stolen.push(format!(
                            "{}: binds `{}`, which is how you ENTER Visual mode — \
                             region staging becomes unreachable in this buffer",
                            $label, entry.chord
                        ));
                    }
                })*
            };
        }
        check!(
            MagitCoreMode => "magit-core-mode",
            MagitGlobalMode => "magit-global-mode",
            MagitStatusMode => "magit-status-mode",
            MagitCommitMode => "magit-commit-mode",
            MagitDiffMode => "magit-diff-mode",
            MagitLogMode => "magit-log-mode",
            MagitBlameMode => "magit-blame-mode",
            MagitStashMode => "magit-stash-mode",
            MagitBranchMode => "magit-branch-mode",
            MagitRebaseMode => "magit-rebase-mode",
            MagitRevisionMode => "magit-revision-mode",
            MagitFileRevisionMode => "magit-file-revision-mode",
            magit_stash_show_mode::MagitStashShowMode => "magit-stash-show-mode",
        );
        assert!(stolen.is_empty(), "{}", stolen.join("\n"));
    }

    /// MG.18e — a view that stages in Normal mode must also stage in
    /// Visual mode.
    ///
    /// The two halves are independent keymap rows, so dropping the
    /// Visual one is a silent regression: `s` over a selection would
    /// fall through to vim's substitute, hit the read-only gate, and
    /// report "buffer is read-only" — which reads as a bug in staging
    /// rather than a missing binding. Pinning the pairing means the next
    /// view that gains `s` cannot ship half of it.
    #[test]
    fn every_view_that_stages_in_normal_mode_also_stages_over_a_selection() {
        use lattice_mode::Mode;

        for (label, keymap) in [
            ("magit-status-mode", MagitStatusMode.keymap()),
            ("magit-diff-mode", MagitDiffMode.keymap()),
        ] {
            let bound = |mode: lattice_keymap::BindingMode, chord: &str| -> Option<&'static str> {
                keymap
                    .entries
                    .iter()
                    .find(|e| e.modes.contains(&mode) && e.chord == chord)
                    .and_then(|e| e.command)
            };
            for chord in ["s", "u", "x"] {
                let Some(normal) = bound(lattice_keymap::BindingMode::Normal, chord) else {
                    continue; // this view does not offer the chord at all
                };
                assert_eq!(
                    bound(lattice_keymap::BindingMode::Visual, chord),
                    Some(normal),
                    "{label}: `{chord}` acts in Normal mode but not over a \
                     selection — region staging is unreachable there"
                );
            }
        }
    }

    /// MG.15 — every chord every magit mode binds must reach a real
    /// handler. Three links, each of which has broken in production:
    ///
    /// 1. the keymap's `cmd:` names an action registered in the
    ///    command registry (an unregistered name resolves to nothing —
    ///    the key is silently inert, the MG.8 failure);
    /// 2. some mode contributes a boot handler for that action (a
    ///    registered command with no handler is equally inert, the
    ///    MG.13 failure);
    /// 3. and the mode binding it is the mode owning it, or reaches it
    ///    through `magit-core-mode` (the shared-action collision).
    ///
    /// Every prior slice bolted a bespoke test onto one of these after
    /// a bug shipped through it. This walks all three for every chord
    /// at once, so the next chord added is covered by construction.
    #[test]
    fn every_chord_every_mode_binds_reaches_a_registered_action_and_a_handler() {
        use lattice_mode::Mode;

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);

        // The union of every boot-registered handler, from every mode.
        let mut handled: Vec<&'static str> = Vec::new();
        macro_rules! handlers {
            ($($mode:expr),* $(,)?) => {
                $(for c in $mode.action_handlers() { handled.push(c.action_name); })*
            };
        }
        handlers!(
            MagitGlobalMode,
            MagitCoreMode,
            MagitStatusMode,
            MagitCommitMode,
            MagitDiffMode,
            MagitLogMode,
            MagitBlameMode,
            MagitStashMode,
            MagitBranchMode,
            MagitRebaseMode,
            MagitRevisionMode,
            MagitFileRevisionMode,
            magit_stash_show_mode::MagitStashShowMode,
        );

        let mut dead: Vec<String> = Vec::new();
        macro_rules! check {
            ($($mode:expr => $label:literal),* $(,)?) => {
                $(for entry in $mode.keymap().entries {
                    // `None` = a synthetic action with no registered
                    // command (`PushDigit` and peers); magit binds none
                    // today, but skipping keeps this honest if it does.
                    let Some(cmd) = entry.command else { continue };
                    // Only `action:` chords are this test's business;
                    // `ex:` chords route through the ex-command table.
                    if !cmd.starts_with("action:") {
                        continue;
                    }
                    if registry.lookup_by_name(cmd).is_none() {
                        dead.push(format!(
                            "{}: chord `{}` → `{cmd}`, which is NOT a registered \
                             action command — the key is silently inert",
                            $label, entry.chord
                        ));
                    } else if !handled.contains(&cmd) {
                        dead.push(format!(
                            "{}: chord `{}` → `{cmd}`, registered but NO mode \
                             contributes a handler — the key does nothing",
                            $label, entry.chord
                        ));
                    }
                })*
            };
        }
        check!(
            MagitCoreMode => "magit-core-mode",
            MagitStatusMode => "magit-status-mode",
            MagitCommitMode => "magit-commit-mode",
            MagitDiffMode => "magit-diff-mode",
            MagitLogMode => "magit-log-mode",
            MagitBlameMode => "magit-blame-mode",
            MagitStashMode => "magit-stash-mode",
            MagitBranchMode => "magit-branch-mode",
            MagitRebaseMode => "magit-rebase-mode",
            MagitRevisionMode => "magit-revision-mode",
            MagitFileRevisionMode => "magit-file-revision-mode",
            magit_stash_show_mode::MagitStashShowMode => "magit-stash-show-mode",
        );

        assert!(
            dead.is_empty(),
            "chords that cannot reach a handler:\n  {}",
            dead.join("\n  ")
        );
    }

    /// MG.17a — the two front-ends resolve a flag to the SAME `Args`.
    ///
    /// This is the claim the whole slice rests on: `:magit-push
    /// --force-with-lease` and toggling `-f` in the transient must
    /// reach `spawn_remote_op` with identical arguments, so there is
    /// one body and one behaviour rather than two that agree today and
    /// drift tomorrow.
    ///
    /// The transient half is simulated the way the host builds it —
    /// project the toggled state onto `arg_specs()` in order — because
    /// `Editor::transient_args_for` lives in `lattice-host` and can't
    /// be reached from here.
    #[test]
    fn the_cmdline_and_the_transient_resolve_a_flag_to_the_same_args() {
        use lattice_grammar::{ArgValue, Args};
        use magit_global_mode::RemoteOp;

        let op = RemoteOp::PUSH;

        // Front-end 1: the `:` line.
        let from_cmdline = parse_remote_flags(op, "--force-with-lease");

        // Front-end 2: the transient, `-f` toggled on.
        let toggled: std::collections::HashMap<&str, bool> =
            [("force-with-lease", true)].into_iter().collect();
        let from_transient = Args::List(
            op.arg_specs()
                .iter()
                .map(|spec| {
                    ArgValue::Bool(toggled.get(spec.name.as_ref()).copied().unwrap_or(false))
                })
                .collect(),
        );

        assert_eq!(from_cmdline, from_transient);
        assert_eq!(
            op.argv(&from_cmdline),
            vec!["push", "--force-with-lease"],
            "and both must produce the force-with-lease push"
        );
    }

    /// An unknown token on the `:` line is ignored rather than failing
    /// the command. The flags are additive, so the worst outcome is an
    /// operation that does slightly less than asked — never one that
    /// does something unasked.
    #[test]
    fn an_unrecognised_flag_on_the_cmdline_is_ignored_not_fatal() {
        use lattice_grammar::{ArgValue, Args};
        use magit_global_mode::RemoteOp;
        assert_eq!(
            parse_remote_flags(RemoteOp::PUSH, "--frce-with-lease --set-upstream"),
            Args::List(vec![ArgValue::Bool(false), ArgValue::Bool(true)]),
            "the typo drops out; the flag that parsed still applies"
        );
    }

    /// An operation with no flags keeps `Args::None`, so its handler
    /// sees exactly what it saw before MG.17a.
    #[test]
    fn a_flagless_operation_parses_to_no_args() {
        use lattice_grammar::Args;
        use magit_global_mode::RemoteOp;
        assert_eq!(parse_remote_flags(RemoteOp::PULL, "--force"), Args::None);
    }

    /// MG.16 — the remote/stash operations exist on both surfaces.
    ///
    /// They were transient-only: reachable from `C-c g` and nowhere
    /// else, so they could not be scripted, could not be rebound, and
    /// did not appear under `:magit-<Tab>`. Each ex-command must be
    /// registered AND resolve to the same `RemoteOp` its transient item
    /// fires — two front-ends, one body.
    #[test]
    fn every_remote_operation_has_both_a_transient_item_and_an_ex_command() {
        use lattice_mode::Mode;

        let mut registry = CommandRegistry::new();
        register_ex_commands(&mut registry);
        register_action_commands(&mut registry);
        let handlers = MagitGlobalMode.action_handlers();

        for (ex_name, action_name) in [
            ("magit-fetch", "action:magit-global-fetch"),
            ("magit-pull", "action:magit-global-pull"),
            ("magit-push", "action:magit-global-push"),
            ("magit-stash", "action:magit-global-stash-create"),
        ] {
            assert!(
                registry.lookup_by_name(ex_name).is_some(),
                "`:{ex_name}` must exist — an operation reachable only from a \
                 transient is invisible to `:` and unscriptable"
            );
            assert!(
                handlers.iter().any(|c| c.action_name == action_name),
                "`{action_name}` must still have its transient handler — the \
                 ex-command is a second front-end, not a replacement"
            );
        }
    }

    /// The four `RemoteOp` constants are the single definition of what
    /// each operation runs. If a fifth operation is added without a
    /// distinct argv this catches the copy-paste.
    #[test]
    fn each_remote_op_names_a_distinct_git_invocation() {
        use magit_global_mode::RemoteOp;
        let ops = [
            RemoteOp::FETCH,
            RemoteOp::PULL,
            RemoteOp::PUSH,
            RemoteOp::STASH,
        ];
        for (i, a) in ops.iter().enumerate() {
            assert!(!a.args.is_empty(), "`{}` has no argv", a.what);
            assert_eq!(
                a.args[0],
                match a.what {
                    "stash" => "stash",
                    other => other,
                },
                "argv must lead with the operation it claims to be"
            );
            for b in &ops[i + 1..] {
                assert_ne!(
                    a.args, b.args,
                    "`{}` and `{}` run the same git",
                    a.what, b.what
                );
            }
        }
    }

    /// `:magit-stash` (create) and `:magit-stash-list` (open the list)
    /// are distinct commands where one name is a strict prefix of the
    /// other. Both must resolve to themselves — a lookup that fell
    /// through to prefix matching would make `:magit-stash` open the
    /// list instead of stashing, which is a silent wrong action rather
    /// than an error.
    #[test]
    fn magit_stash_and_magit_stash_list_are_distinct_commands() {
        let mut registry = CommandRegistry::new();
        register_ex_commands(&mut registry);
        let create = registry
            .lookup_by_name("magit-stash")
            .expect("`:magit-stash` registered");
        let list = registry
            .lookup_by_name("magit-stash-list")
            .expect("`:magit-stash-list` registered");
        assert_eq!(create.name, "magit-stash");
        assert_eq!(list.name, "magit-stash-list");
        assert_ne!(create.id, list.id, "a prefix collision would alias the two");
    }

    /// Every shared action has exactly one owner, and it is
    /// `magit-core-mode`. Pins the arrangement the test above protects.
    ///
    /// `gr` is bound by core itself; `s` / `u` are bound by
    /// `magit-status-mode` and `magit-diff-mode` — the *binding* stays
    /// with whichever mode offers the chord, but the *handler* must
    /// exist once, so it lives on core and dispatches through
    /// `MagitView`.
    #[test]
    fn shared_actions_are_owned_solely_by_magit_core_mode() {
        use lattice_mode::Mode;
        const SHARED: &[&str] = &[
            "action:magit-refresh",
            "action:magit-stage",
            "action:magit-unstage",
        ];
        for name in SHARED {
            assert!(
                MagitCoreMode
                    .action_handlers()
                    .iter()
                    .any(|c| c.action_name == *name),
                "`{name}` is reachable from more than one magit view, so \
                 magit-core-mode must own its single handler"
            );
        }
        for (label, contributions) in [
            ("magit-branch-mode", MagitBranchMode.action_handlers()),
            ("magit-stash-mode", MagitStashMode.action_handlers()),
            ("magit-diff-mode", MagitDiffMode.action_handlers()),
            ("magit-log-mode", MagitLogMode.action_handlers()),
            ("magit-status-mode", MagitStatusMode.action_handlers()),
        ] {
            for c in contributions {
                assert!(
                    !SHARED.contains(&c.action_name),
                    "`{label}` contributes shared action `{}` — it must reach \
                     it through its MagitView instead",
                    c.action_name
                );
            }
        }
    }

    /// Regression test for a live-reported bug: `C-c g` then `l`
    /// (log) / `b` (branch) did nothing. Mirrors `install()`'s exact
    /// `register_action_commands` → resolve-`DispatchActionIds`
    /// sequence — every root dispatch item, at every submenu depth,
    /// must resolve to a real `Action`, not `Flag`.
    #[test]
    fn every_root_dispatch_item_resolves_to_a_real_action_not_a_flag_fallback() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        // Resolved through the SAME function `install` uses, so a
        // field added later is covered automatically rather than
        // needing this test to be remembered and updated.
        let ids = resolve_dispatch_ids(&registry);
        // MG.23h: BOTH shapes the menu can take. The gated rows only
        // exist in the magit-buffer one, so checking a single context
        // would leave whichever rows the other adds unverified.
        for ctx in [&outside_magit(), &in_magit_status()] {
            assert_no_inert_items(&transients::dispatch_transient(&ids, ctx));
        }
    }

    #[test]
    fn file_dispatch_items_resolve_to_real_actions() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = resolve_file_dispatch_ids(&registry);
        let spec = transients::file_dispatch_transient(&ids);
        assert_no_inert_items(&spec);
    }

    /// Guards against a subtler variant of the same bug: two items
    /// in one menu level sharing a key means the second is
    /// unreachable — pressing the key always fires the first. Not
    /// caught by the inert-Flag check (both resolve fine); it only
    /// shows up as "this menu entry does nothing".
    #[test]
    fn no_duplicate_keys_within_any_transient_menu_level() {
        fn check(spec: &lattice_picker::TransientSpec, path: &str) {
            let mut seen: Vec<(String, String)> = Vec::new();
            for group in &spec.groups {
                for item in &group.items {
                    for key in &item.key {
                        if let Some((_, prior)) = seen.iter().find(|(k, _)| k == key) {
                            panic!(
                                "key '{key}' in menu '{path}' is bound twice: \
                                 '{prior}' and '{}' — the second is unreachable",
                                item.label
                            );
                        }
                        seen.push((key.clone(), item.label.clone()));
                    }
                    if let lattice_picker::TransientItemKind::Submenu(sub) = &item.kind {
                        check(sub, &format!("{path}{} > ", item.label));
                    }
                }
            }
        }
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        check(
            &transients::dispatch_transient(&resolve_dispatch_ids(&registry), &in_magit_status()),
            "dispatch",
        );
        check(
            &transients::file_dispatch_transient(&resolve_file_dispatch_ids(&registry)),
            "file-dispatch",
        );
    }

    /// The inverse guard: with NO ids resolved, EVERY leaf must be
    /// reported inert — proving the walker actually visits leaves
    /// and detects the failure it claims to. A walker that silently
    /// visited nothing (wrong field, empty groups, no recursion)
    /// would pass the two tests above vacuously.
    #[test]
    fn unresolved_ids_do_produce_inert_items_so_the_guard_is_not_vacuous() {
        // 11 file-dispatch items: stage/unstage/discard,
        // diff/log/blame, MG.23f2's reverse blame, MG.23d's
        // untrack/rename/delete and MG.23d2's checkout.
        let file = inert_items(
            &transients::file_dispatch_transient(&Default::default()),
            "",
        );
        assert_eq!(
            file.len(),
            11,
            "expected every file-dispatch leaf to report inert, got: {file:?}"
        );
        // Root dispatch: 18 ACTION leaves — status, diff, log,
        // branch, pull, rebase directly, MG.23b's stage-all /
        // unstage-all, MG.23c1's tag / gitignore, MG.23c2's merge /
        // init, plus the commit
        // submenu's 2 (c/a), the stash submenu's 2 (z/l), and one each
        // inside the fetch and push submenus MG.17a introduced to hold
        // their flags. Recursion is
        // what makes the submenu leaves visible. The flag items
        // themselves are NOT counted — they are real toggles, not
        // placeholders; see `declared_flag_names`.
        //
        // This count is deliberately hardcoded: a row added without a
        // resolvable action id would otherwise slip in as a
        // permanently-inert placeholder, which is the "menu row that
        // does nothing" the no-inert-rows policy forbids. Bump it only
        // together with a real action.
        let root = inert_items(
            &transients::dispatch_transient(&Default::default(), &outside_magit()),
            "",
        );
        assert_eq!(
            root.len(),
            23,
            "expected every root-dispatch leaf (incl. both submenus') to \
             report inert, got: {root:?}"
        );
    }
}
