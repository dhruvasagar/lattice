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
pub mod blame;
pub mod buffer_io;
pub mod buffer_state;
mod confirm;
// MG.18d: `pub` because `MagitView::refresh_restoring` (a public
// trait) names `HunkRestore` in its signature.
pub mod cursor_restore;
pub mod fold_source;
pub mod headerline;
mod highlight;
pub mod options;
// MG.18c: public so the bench can measure the parser directly — the
// accessor shape it pins (read the hunk, not the document) is the
// paramount-#1 claim staging rests on, and MG.22 relocates this module
// into `magit-hunk-mode` as the owner of diff content.
pub mod hunk;
pub mod magit_blame_mode;
pub mod magit_branch_mode;
pub mod magit_cherry_mode;
pub mod magit_commit_mode;
pub mod magit_core_mode;
pub mod magit_diff_mode;
pub mod magit_file_revision_mode;
pub mod magit_global_mode;
pub mod magit_hunk_mode;
pub mod magit_log_mode;
pub mod magit_notes_mode;
pub mod magit_rebase_mode;
pub mod magit_refs_mode;
pub mod magit_remote_mode;
pub mod magit_revision_mode;
pub mod magit_stash_mode;
pub mod magit_stash_show_mode;
pub mod magit_status_mode;
pub mod magit_submodule_mode;
pub mod picker_sources;
pub mod refresh;
pub mod sections;
pub mod transients;
pub mod workdir;

use std::sync::Arc;

use lattice_grammar::{
    ActionSpec, ArgSpec, Args, Effect, ExCommandSpec, GrammarResult, LatencyClass, SurfaceForm,
    registry::CommandRegistry,
};
use lattice_mode::SubsystemBoot;

use magit_blame_mode::MagitBlameMode;
use magit_branch_mode::MagitBranchMode;
use magit_cherry_mode::MagitCherryMode;
use magit_commit_mode::MagitCommitMode;
use magit_core_mode::MagitCoreMode;
use magit_diff_mode::MagitDiffMode;
use magit_file_revision_mode::MagitFileRevisionMode;
use magit_global_mode::MagitGlobalMode;
use magit_log_mode::MagitLogMode;
use magit_notes_mode::MagitNotesMode;
use magit_rebase_mode::MagitRebaseMode;
use magit_refs_mode::MagitRefsMode;
use magit_remote_mode::MagitRemoteMode;
use magit_revision_mode::MagitRevisionMode;
use magit_stash_mode::MagitStashMode;
use magit_status_mode::MagitStatusMode;
use magit_submodule_mode::MagitSubmoduleMode;

/// Register all magit modes, commands, and keymaps via the generic
/// `SubsystemBoot` seam. Called once from `editor_boot.rs` during
/// the Phase-B subsystem install pass.
pub fn install(boot: &mut impl SubsystemBoot) {
    // MG.41g: capture the bus so spawned git tasks can report
    // completion. magit publishes `BackgroundTaskFinished`; the
    // notification layer subscribes. No dependency either way.
    magit_global_mode::set_event_bus(boot.event_bus().clone());

    // ── Modes ──────────────────────────────────────────────

    boot.modes_mut()
        .register(MagitGlobalMode)
        .expect("magit-global-mode registers without conflict");

    boot.modes_mut()
        .register(MagitCoreMode)
        .expect("magit-core-mode registers without conflict");

    // MG.24a: the second shared minor. `magit-core-mode` is every magit
    // buffer; this one is every magit buffer that renders a diff.
    boot.modes_mut()
        .register(magit_hunk_mode::MagitHunkMode)
        .expect("magit-hunk-mode registers without conflict");

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
        .register(MagitRemoteMode)
        .expect("magit-remote-mode registers without conflict");

    // MG.35
    boot.modes_mut()
        .register(MagitRefsMode)
        .expect("magit-refs-mode registers without conflict");

    // MG.37
    boot.modes_mut()
        .register(MagitNotesMode)
        .expect("magit-notes-mode registers without conflict");

    // MG.40
    boot.modes_mut()
        .register(MagitCherryMode)
        .expect("magit-cherry-mode registers without conflict");

    boot.modes_mut()
        .register(MagitSubmoduleMode)
        .expect("magit-submodule-mode registers without conflict");

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

    let blame_requests: magit_blame_mode::BlameRequestsHandle =
        Arc::new(magit_blame_mode::BlameRequests::default());
    boot.register_service::<magit_blame_mode::BlameRequestsHandle>(blame_requests.clone());
    // NOTIF.1d: the same handle the action handlers get as a service —
    // MG.41g: no notification handle is captured any more — the git
    // ops publish `BackgroundTaskFinished` and the notification layer
    // subscribes, so magit has no dependency on it at all.
    register_ex_commands(boot.commands_mut(), blame_requests);

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
    // MG.41a: ONE resolver for every transient. It scans the registry
    // for `action:magit-` names rather than reading a hand-kept struct,
    // so registering an action is the only step needed before a row can
    // reference it — the four-place enumeration this replaces
    // (`reg` / struct field / `id_by_name` line / builder) is down to
    // two, and neither of the removed two can drift silently.
    let dispatch_ids = transients::MagitActionIds::resolve(boot.commands_mut());
    let file_dispatch_ids = dispatch_ids.clone();
    let other_file_dispatch_ids = dispatch_ids.clone();
    let view_args_ids_src = dispatch_ids.clone();
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
    // MG.23k: `D`. The rows depend on which magit view you are in, so
    // this is the second context-varying source after the root
    // dispatch — the builder reads `ctx.major_mode`.
    let view_args_ids = view_args_ids_src;
    transient_registry.register("magit-view-arguments", move |ctx| {
        transients::view_arguments_transient(&view_args_ids, ctx)
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
                // MG.23k: the joined form is one token, `--unified=3`,
                // so it is found by prefix and the value is what
                // follows. No "must come last" constraint, because
                // there is nothing after it to swallow.
                RemoteArgKind::ValueJoined { .. } => lattice_grammar::ArgValue::String(
                    given
                        .iter()
                        .find_map(|t| t.strip_prefix(f.arg))
                        .unwrap_or_default()
                        .to_string(),
                ),
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
    // MG.21c
    boot.register_service::<magit_remote_mode::RemoteStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    // MG.35
    boot.register_service::<magit_refs_mode::RefsStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    // MG.37
    boot.register_service::<magit_notes_mode::NoteStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    // MG.40
    boot.register_service::<magit_cherry_mode::CherryStatesHandle>(Arc::new(
        buffer_state::BufferStates::default(),
    ));
    // MG.21i
    boot.register_service::<magit_submodule_mode::SubmoduleStatesHandle>(Arc::new(
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
    // MG.32: the branch submenu's `x`. Unlike the chord's execute half
    // above there is NO cursor to fall back on — the menu opens from
    // anywhere — so the carried slot is the only source of the target.
    (
        "action:magit-global-branch-delete-execute",
        "Delete the branch the menu named, after confirmation",
        &[("branch", "Branch the prompt named")],
    ),
    // MG.21i. The slot is what makes the answer act on the submodule
    // the QUESTION named rather than whatever is under the cursor when
    // it is answered — a refresh landing while the dialog is open would
    // otherwise re-point a working-tree deletion at a different one.
    (
        "action:magit-submodule-remove-execute",
        "Remove the submodule after confirmation",
        &[("path", "Submodule path the prompt named")],
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

/// MG.28: what `:magit-find-file` says when it is not given both
/// halves. Both are required — there is no defensible default file,
/// and a default revision would silently show you HEAD when you asked
/// for something else.
fn find_file_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-find-file <rev> <path> \
               (or `C-c f v` for the file you are visiting)"
            .to_string(),
    }
}

/// MG.39: what `:magit-am` says with no patch to apply.
fn am_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-am <patch>… [-3]".to_string(),
    }
}

/// MG.39: what `:magit-format-patch` says with no range. No default:
/// `format-patch` with none writes a patch per commit since the root,
/// which is never what anyone meant.
fn format_patch_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-format-patch <range>  (e.g. @{upstream}..HEAD)".to_string(),
    }
}

/// MG.40: what `:magit-cherries` says with no upstream. "Not upstream
/// yet" has no meaning without naming the upstream.
fn cherries_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-cherries <upstream> [<head>]".to_string(),
    }
}

/// MG.37: what the note ex-commands say with no commit. No default —
/// defaulting to HEAD would edit or remove a note on a commit the user
/// never named.
fn note_usage(cmd: &str) -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: format!("magit: usage — :{cmd} <commit>  (or `C-c g T` to pick one)"),
    }
}

/// MG.37: merge takes a ref and an optional strategy, not a commit.
fn note_merge_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-note-merge <notes-ref> \
               [manual|ours|theirs|union|cat_sort_uniq]"
            .to_string(),
    }
}

/// MG.36: what `:magit-clone` says with nothing usable. The
/// destination is optional and derived; the URL is not, and there is no
/// defensible guess for it.
fn clone_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-clone <url> [<destination>] \
               (or `C` in the dispatch)"
            .to_string(),
    }
}

/// MG.34: what `:magit-log-merged` says with no commit. No default —
/// "the merge that brought HEAD in" is not a question with an answer,
/// and guessing one would show a buffer the user did not ask for.
fn log_merged_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-log-merged <commit> \
               (or `C-c f M` to pick one)"
            .to_string(),
    }
}

/// MG.23f2: what `:magit-blame-reverse` says when it is not given both
/// halves. An error rather than a best guess — see the registration for
/// why there is no defensible default revision.
fn reverse_blame_usage() -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: "magit: usage — :magit-blame-reverse <rev> <path>".to_string(),
    }
}


/// Register all magit ex-commands in the command registry.
/// MG.26b: `blame_requests` is threaded in rather than looked up,
/// because an ex-command's `apply` receives `lattice_grammar`'s
/// `ActionContext`, which carries no service registry — by design, the
/// grammar crate knows nothing about magit's services. Capturing the
/// same `Arc` the mode's handlers get as a service means both surfaces
/// write to one map instead of two.
fn register_ex_commands(
    registry: &mut CommandRegistry,
    blame_requests: magit_blame_mode::BlameRequestsHandle,
) {
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
    // MG.21c
    mk(
        "magit-remote",
        "Open the Magit remote list buffer.",
        "*magit:remote*",
        "magit-remote-mode",
    );
    // MG.21i
    mk(
        "magit-submodule",
        "Open the Magit submodule list buffer.",
        "*magit:submodule*",
        "magit-submodule-mode",
    );
    // MG.35: magit's `y` show-refs. Named for what it lists rather than
    // for magit's key, per the dashed-namespaced ex-command rule.
    mk(
        "magit-refs",
        "Open the Magit refs buffer — every branch, remote-tracking branch and tag.",
        magit_refs_mode::REFS_BUFFER,
        "magit-refs-mode",
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
                apply: {
                    Arc::new(move |ctx| {
                        Ok(magit_global_mode::spawn_remote_op(
                            op,
                            &ctx.args
                        ))
                    })
                },
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
    // MG.34: sequencer controls. Not remote operations, but the same
    // shape — one bounded `git` argv, run off the actor thread, result
    // reported by notification — so they reuse the mechanism rather
    // than growing a parallel one. `:magit-stash` set that precedent.
    //
    // These exist because `C-c f e` marks a commit `edit`: a rebase that
    // stops needs a way forward, and before this slice the only
    // sequencer control was `C-c C-k` in a todo buffer, which is gone by
    // the time the rebase is actually running.
    mk_op(
        "magit-rebase-continue",
        "Resume a rebase that stopped (after amending, or resolving conflicts).",
        magit_global_mode::RemoteOp::REBASE_CONTINUE,
    );
    mk_op(
        "magit-rebase-skip",
        "Skip the commit a stopped rebase is sitting on.",
        magit_global_mode::RemoteOp::REBASE_SKIP,
    );
    mk_op(
        "magit-rebase-abort",
        "Abandon a rebase in progress, restoring the branch to where it started.",
        magit_global_mode::RemoteOp::REBASE_ABORT,
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
        // MG.41e: merge / tag variants. Same prompt-then-finish shape
        // as their siblings above; only the argv differs.
        mk_prompted(
            "magit-merge-no-commit",
            "Merge a branch but stop before committing. With arg: the branch; without, asks.",
            "branch",
            "Merge branch (no commit)",
            "action:magit-global-merge-no-commit-finish",
            |branch| {
                magit_global_mode::spawn_git(
                    magit_global_mode::merge_no_commit_argv(&branch),
                    "merge --no-commit",
                )
            },
        );
        mk_prompted(
            "magit-merge-squash",
            "Squash a branch's changes into the index. With arg: the branch; without, asks.",
            "branch",
            "Squash branch",
            "action:magit-global-merge-squash-finish",
            |branch| {
                magit_global_mode::spawn_git(
                    magit_global_mode::merge_squash_argv(&branch),
                    "merge --squash",
                )
            },
        );
        mk_prompted(
            "magit-tag-delete",
            "Delete a local tag. With arg: the tag; without, asks.",
            "name",
            "Delete tag",
            "action:magit-global-tag-delete-finish",
            |name| {
                magit_global_mode::spawn_git(
                    magit_global_mode::tag_delete_argv(&name),
                    "tag -d",
                )
            },
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
                // MG.26b: blame annotates the buffer you are reading
                // rather than opening one of its own. With no argument
                // that is a plain toggle; with a path, open the file
                // first and toggle on it — the same composition `dv`
                // uses, and the reason the argument stays useful.
                apply: Arc::new(|ctx| {
                    let toggle = Effect::ToggleMode {
                        mode_name: "magit-blame-mode".to_string(),
                    };
                    Ok(match ctx.args {
                        Args::String(ref path) if !path.trim().is_empty() => Effect::Many(vec![
                            Effect::OpenBuffer {
                                path: Some(std::path::PathBuf::from(path.trim())),
                                force: false,
                            },
                            toggle,
                        ]),
                        _ => toggle,
                    })
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
            // MG.41d: magit's remaining reset modes + the autosquash
            // pair. Each is data — same handler, different argv.
            magit_global_mode::CommitOp::RESET_KEEP,
            magit_global_mode::CommitOp::RESET_INDEX,
            magit_global_mode::CommitOp::COMMIT_FIXUP,
            magit_global_mode::CommitOp::COMMIT_SQUASH,
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
                                format!("git {} {commit} — discard uncommitted changes?", op.what),
                                yes,
                                commit,
                            ),
                            None => {
                                let workdir = workdir::magit_workdir().unwrap_or_default();
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
        // MG.29: what the branch-checkout picker invokes. Also the
        // scriptable form — `:magit-checkout <branch>`.
        registry.register_ex_command(
            "magit-checkout",
            "Check out a branch: `<branch>`.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref name) = ctx.args else {
                        return Ok(Effect::Echo {
                            level: lattice_grammar::EchoLevel::Error,
                            text: "magit: usage — :magit-checkout <branch>".to_string(),
                        });
                    };
                    let name = name.trim().to_string();
                    if name.is_empty() {
                        return Ok(Effect::Echo {
                            level: lattice_grammar::EchoLevel::Error,
                            text: "magit: usage — :magit-checkout <branch>".to_string(),
                        });
                    }
                    Ok(magit_global_mode::spawn_git(
                        vec!["checkout".to_string(), name],
                        "checkout",
                    ))
                }),
                args_schema: vec![ArgSpec::required(
                    "branch",
                    lattice_grammar::ArgKind::String,
                    "the branch to check out",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );

        // MG.28: the explicit form of `C-c f v` — a file you are not
        // visiting. `<rev>` alone shows the file you ARE visiting,
        // which is what the chord does.
        registry.register_ex_command(
            "magit-find-file",
            "Open a file as it was at a revision: `<rev> <path>`.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref spec) = ctx.args else {
                        return Ok(find_file_usage());
                    };
                    match spec.split_once(char::is_whitespace) {
                        Some((rev, path)) if !rev.is_empty() && !path.trim().is_empty() => {
                            Ok(Effect::OpenSyntheticBuffer {
                                name: magit_file_revision_mode::blob_buffer_name(
                                    rev,
                                    std::path::Path::new(path.trim()),
                                ),
                                mode_id: "magit-file-revision-mode".to_string(),
                            })
                        }
                        _ => Ok(find_file_usage()),
                    }
                }),
                args_schema: vec![ArgSpec::required(
                    "spec",
                    lattice_grammar::ArgKind::String,
                    "<rev> <path> — the revision, and the file to show at it",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
        // MG.38 / MG.39 / MG.40: the scriptable halves. Each takes the
        // same line the menu's prompt takes, so a user who learned one
        // surface can use the other without re-learning the argument
        // order.
        {
            let mut mk_subtree = |op: magit_global_mode::SubtreeOp, doc: &'static str| {
                registry.register_ex_command(
                    op.ex_command,
                    doc,
                    ExCommandSpec {
                        latency_class: LatencyClass::Reflex,
                        accepts_bang: false,
                        accepts_range: false,
                        parse_args: Arc::new(|line: &str, _bang: bool| {
                            Ok(Args::String(line.trim().to_string()))
                        }),
                        apply: Arc::new(move |ctx| {
                            let line = match ctx.args {
                                Args::String(ref l) => l.clone(),
                                _ => String::new(),
                            };
                            Ok(magit_global_mode::spawn_subtree_op(
                                op,
                                &line
                            ))
                        }),
                        args_schema: vec![ArgSpec::required(
                            "spec",
                            lattice_grammar::ArgKind::String,
                            op.usage(),
                        )],
                        surface_form: SurfaceForm::Keyword,
                    },
                );
            };
            mk_subtree(
                magit_global_mode::SubtreeOp::ADD,
                "Add a repository as a subtree: `<prefix> <repository> <ref> [--squash]`.",
            );
            mk_subtree(
                magit_global_mode::SubtreeOp::MERGE,
                "Merge a ref into an existing subtree: `<prefix> <ref>`.",
            );
            mk_subtree(
                magit_global_mode::SubtreeOp::PULL,
                "Fetch and merge a subtree's upstream: `<prefix> <repository> <ref> [--squash]`.",
            );
            mk_subtree(
                magit_global_mode::SubtreeOp::PUSH,
                "Push a subtree's history to its own repository: `<prefix> <repository> <ref>`.",
            );
            mk_subtree(
                magit_global_mode::SubtreeOp::SPLIT,
                "Extract a subtree's history into its own branch: `<prefix>`.",
            );
        }

        registry.register_ex_command(
            "magit-am",
            "Apply a mailbox of patches: `<patch>… [-3]`.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref line) = ctx.args else {
                        return Ok(am_usage());
                    };
                    match magit_global_mode::am_argv(
                        line,
                        magit_global_mode::am_wants_three_way(line),
                    ) {
                        Some(argv) => Ok(magit_global_mode::spawn_git(argv, "am")),
                        None => Ok(am_usage()),
                    }
                }),
                args_schema: vec![ArgSpec::required(
                    "patches",
                    lattice_grammar::ArgKind::String,
                    "<patch>… [-3]",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
        registry.register_ex_command(
            "magit-format-patch",
            "Write a commit range out as .patch files in the repository root.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref range) = ctx.args else {
                        return Ok(format_patch_usage());
                    };
                    let root = workdir::magit_workdir()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    match magit_global_mode::format_patch_argv(
                        range,
                        (!root.is_empty()).then_some(root.as_str()),
                    ) {
                        Some(argv) => Ok(magit_global_mode::spawn_git(argv, "format-patch")),
                        None => Ok(format_patch_usage()),
                    }
                }),
                args_schema: vec![ArgSpec::required(
                    "range",
                    lattice_grammar::ArgKind::String,
                    "the commit range to turn into patches",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
        registry.register_ex_command(
            "magit-cherries",
            "Show which commits are not upstream yet: `<upstream> [<head>]`.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref spec) = ctx.args else {
                        return Ok(cherries_usage());
                    };
                    let spec = spec.trim();
                    if spec.is_empty() {
                        return Ok(cherries_usage());
                    }
                    let (upstream, head) = match spec.split_once(char::is_whitespace) {
                        Some((u, h)) if !h.trim().is_empty() => (u, h.trim()),
                        _ => (spec, "HEAD"),
                    };
                    Ok(Effect::OpenSyntheticBuffer {
                        name: magit_cherry_mode::cherry_buffer_name(upstream, head),
                        mode_id: "magit-cherry-mode".to_string(),
                    })
                }),
                args_schema: vec![ArgSpec::required(
                    "spec",
                    lattice_grammar::ArgKind::String,
                    "<upstream> [<head>] — what to compare against, and what to compare",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );

        // MG.37: the scriptable halves of the notes submenu, and what
        // the commit picker routes to when the menu was opened without
        // a commit under the cursor.
        registry.register_ex_command(
            "magit-note-edit",
            "Edit the note on a commit — opens an editable buffer.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref commit) = ctx.args else {
                        return Ok(note_usage("magit-note-edit"));
                    };
                    let commit = commit.trim();
                    if commit.is_empty() {
                        return Ok(note_usage("magit-note-edit"));
                    }
                    Ok(Effect::OpenSyntheticBuffer {
                        name: magit_notes_mode::note_buffer_name(commit),
                        mode_id: "magit-notes-mode".to_string(),
                    })
                }),
                args_schema: vec![ArgSpec::required(
                    "commit",
                    lattice_grammar::ArgKind::String,
                    "the commit whose note to edit",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
        registry.register_ex_command(
            "magit-note-remove",
            "Remove the note from a commit.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref commit) = ctx.args else {
                        return Ok(note_usage("magit-note-remove"));
                    };
                    let commit = commit.trim();
                    if commit.is_empty() {
                        return Ok(note_usage("magit-note-remove"));
                    }
                    Ok(magit_global_mode::spawn_note_remove(commit.to_string()))
                }),
                args_schema: vec![ArgSpec::required(
                    "commit",
                    lattice_grammar::ArgKind::String,
                    "the commit whose note to remove",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
        registry.register_ex_command(
            "magit-note-merge",
            "Merge a notes ref into this one: `<ref> [manual|ours|theirs|union|cat_sort_uniq]`.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: {
                    Arc::new(move |ctx| {
                        let Args::String(ref spec) = ctx.args else {
                            return Ok(note_merge_usage());
                        };
                        if spec.trim().is_empty() {
                            return Ok(note_merge_usage());
                        }
                        Ok(magit_global_mode::spawn_note_merge(
                            spec
                        ))
                    })
                },
                args_schema: vec![ArgSpec::required(
                    "spec",
                    lattice_grammar::ArgKind::String,
                    "<notes-ref> [strategy]",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );

        // MG.36: the scriptable half of `C`. With both arguments it
        // clones directly; with one it derives the destination the way
        // `git clone` itself would, so `:magit-clone <url>` behaves like
        // the terminal command people already know.
        registry.register_ex_command(
            "magit-clone",
            "Clone a repository: `<url> [<destination>]`.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: {
                    Arc::new(move |ctx| {
                        let Args::String(ref spec) = ctx.args else {
                            return Ok(clone_usage());
                        };
                        let spec = spec.trim();
                        if spec.is_empty() {
                            return Ok(clone_usage());
                        }
                        let (url, dest) = match spec.split_once(char::is_whitespace) {
                            Some((url, dest)) if !dest.trim().is_empty() => {
                                (url, dest.trim().to_string())
                            }
                            _ => (spec, magit_global_mode::default_clone_dest(spec)),
                        };
                        if dest.is_empty() {
                            // Nothing usable in the URL to name a
                            // directory after, and git would refuse for
                            // the same reason — say so here instead.
                            return Ok(clone_usage());
                        }
                        Ok(magit_global_mode::spawn_clone(
                            url.to_string(),
                            dest
                        ))
                    })
                },
                args_schema: vec![ArgSpec::required(
                    "spec",
                    lattice_grammar::ArgKind::String,
                    "<url> [<destination>] — what to clone, and where",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );

        // MG.34: magit's `M` "Merged" (magit-file-dispatch, level 7).
        //
        // The commit you name is the *question*, not the answer: the
        // buffer shows the merge that brought it into HEAD, which is a
        // different commit and costs a `git log` walk to find. That walk
        // happens in `magit-revision-mode`'s activation, which is why
        // this hands over a `*magit:merged:*` name rather than a
        // resolved sha — see that module for the reasoning.
        registry.register_ex_command(
            "magit-log-merged",
            "Show the merge commit that brought <commit> into HEAD.",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    Ok(Args::String(line.trim().to_string()))
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref commit) = ctx.args else {
                        return Ok(log_merged_usage());
                    };
                    let commit = commit.trim();
                    if commit.is_empty() {
                        return Ok(log_merged_usage());
                    }
                    Ok(Effect::OpenSyntheticBuffer {
                        name: magit_revision_mode::merged_buffer_name(commit),
                        mode_id: "magit-revision-mode".to_string(),
                    })
                }),
                args_schema: vec![ArgSpec::required(
                    "commit",
                    lattice_grammar::ArgKind::String,
                    "the commit whose merge to find",
                )],
                surface_form: SurfaceForm::Keyword,
            },
        );
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
                // MG.26b: reverse blame annotates the blob buffer —
                // the file at that revision, which is the content
                // reverse blame is about. The direction and revision
                // are left as a request keyed by that buffer's name,
                // because `ToggleMode` carries a mode name and nothing
                // else.
                apply: {
                    let requests = blame_requests.clone();
                    Arc::new(move |ctx| {
                        let Args::String(ref spec) = ctx.args else {
                            return Ok(reverse_blame_usage());
                        };
                        match spec.split_once(char::is_whitespace) {
                            Some((rev, path)) if !rev.is_empty() && !path.trim().is_empty() => {
                                let name = magit_file_revision_mode::blob_buffer_name(
                                    rev,
                                    std::path::Path::new(path.trim()),
                                );
                                requests.put(
                                    name.clone(),
                                    magit_blame_mode::BlameDirection::Reverse,
                                    rev.to_string(),
                                );
                                Ok(Effect::Many(vec![
                                    Effect::OpenSyntheticBuffer {
                                        name,
                                        mode_id: "magit-file-revision-mode".to_string(),
                                    },
                                    Effect::ToggleMode {
                                        mode_name: "magit-blame-mode".to_string(),
                                    },
                                ]))
                            }
                            _ => Ok(reverse_blame_usage()),
                        }
                    })
                },
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
    {
        // MG.32: `:magit-branch-delete <name>` — the ask half of the
        // branch submenu's `x`, and the scriptable form besides.
        //
        // **This is an ex-command rather than an action because a
        // picker's accept can only reach an operation through
        // `InvokeCommand`**, which dispatches ex-commands — the same
        // constraint that shaped MG.23j's commit picker. It does no git
        // call at all: it raises the MG.12 confirm carrying the name,
        // and only `action:magit-global-branch-delete-execute` deletes,
        // so answering `n` cannot mutate anything.
        registry.register_ex_command(
            "magit-branch-delete",
            "Delete a branch by name — asks first (force delete).",
            ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: Arc::new(|line: &str, _bang: bool| {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        Err(lattice_grammar::error::CommandError::BadArgs(
                            "magit-branch-delete: branch name required".to_string(),
                        ))
                    } else {
                        Ok(Args::String(trimmed.to_string()))
                    }
                }),
                apply: Arc::new(|ctx| {
                    let Args::String(ref name) = ctx.args else {
                        return Ok(Effect::Echo {
                            level: lattice_grammar::EchoLevel::Error,
                            text: "magit-branch-delete: branch name required".to_string(),
                        });
                    };
                    Ok(confirm::ask_target(
                        format!("Delete branch {name}?"),
                        "action:magit-global-branch-delete-execute",
                        name.clone(),
                    ))
                }),
                args_schema: vec![ArgSpec::required(
                    "name",
                    lattice_grammar::ArgKind::String,
                    "branch to delete",
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
/// MG.41a: test-only door onto [`register_action_commands`], so the
/// row-table drift tests can build the same registry `install` does
/// without standing up a whole boot.
#[cfg(test)]
pub(crate) fn register_action_commands_for_test(registry: &mut CommandRegistry) {
    register_action_commands(registry);
}

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
    // MG.22: magit-hunk-mode's `<CR>` — one action for the five buffers
    // that render a diff, replacing magit-diff / magit-commit /
    // magit-revision's own visit actions, each of which carried its own
    // copy of the diff-path parser.
    reg(
        "action:magit-visit-diff-target",
        "Visit the file at cursor, in the version this view describes",
    );
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

    // magit-log-mode
    reg(
        "action:magit-log-show-commit",
        "Show the commit detail at cursor",
    );

    // magit-blame-mode
    reg(
        "action:magit-blame-show-commit",
        "Show the commit for the blamed line",
    );
    reg("action:magit-blame-parent", "Re-blame at the parent commit");
    reg(
        "action:magit-blame-quit",
        "Stop blaming — the buffer becomes editable again",
    );
    // MG.23f2. Deliberately NOT in `FILE_TARGET_ACTIONS`: it needs a
    // revision as well as a path, and takes both from the blob buffer
    // it is invoked in — a `file` argument alone could not say which
    // revision to walk forward from. See its handler for why that
    // restricts it to blob buffers.
    reg(
        "action:magit-global-file-blame-reverse",
        "For each line of this revision of the file, the last commit it existed in",
    );

    // MG.34: magit-file-dispatch's `M` and `e`.
    //
    // Neither is in `FILE_TARGET_ACTIONS`. `M` takes a *commit*, not a
    // path — it is only nominally file-scoped, and magit files it under
    // file-dispatch because that is where you are when you wonder how a
    // commit got here. `e` takes a path AND a cursor line, and a `file`
    // argument alone cannot carry the line, so a target-file form would
    // silently blame line 1 of whatever was named.
    reg(
        "action:magit-global-log-merged",
        "Show the merge commit that brought a commit into HEAD",
    );
    reg(
        "action:magit-global-edit-line-commit",
        "Start a rebase that stops on the commit that wrote the line at the cursor",
    );

    // MG.35: magit-refs-mode
    reg(
        "action:magit-refs-show",
        "Show the commit the ref at cursor points at",
    );
    reg(
        "action:magit-refs-checkout",
        "Check out the ref at cursor (refuses on a tag or remote-tracking branch)",
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
        "action:magit-reset-keep",
        "Reset --keep to the commit at cursor (refuses rather than discarding your work)",
    );
    reg(
        "action:magit-reset-index",
        "Reset the index to the commit at cursor, leaving HEAD and the working tree alone",
    );
    reg(
        "action:magit-commit-fixup",
        "Record a fixup! commit for the commit at cursor (folded by rebase --autosquash)",
    );
    reg(
        "action:magit-commit-squash",
        "Record a squash! commit for the commit at cursor (folded by rebase --autosquash)",
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

    // MG.21c: magit-remote-mode. The `-url` / `-finish` halves are
    // fired by a prompt submit, never by a chord, but they are real
    // registered actions all the same — `do_prompt_line_submit`
    // resolves `on_submit_action` through the command registry and
    // reports "unknown action" if it is missing.
    reg(
        "action:magit-global-remote",
        "Open the Magit remote list buffer",
    );
    // MG.29: the branch submenu's picker-backed rows.
    reg(
        "action:magit-global-branch-checkout",
        "Pick a branch and check it out",
    );
    reg(
        "action:magit-global-branch-create",
        "Pick a base, then name a new branch",
    );
    // MG.21g: bisect. The `-start-good` / `-start-finish` halves are
    // fired by a prompt submit rather than a chord, but must still be
    // registered — `do_prompt_line_submit` resolves `on_submit_action`
    // through the command registry.
    reg(
        "action:magit-global-bisect-start",
        "Start a bisect (asks for a bad then a good revision)",
    );
    reg(
        "action:magit-global-bisect-start-good",
        "Ask for the good revision after the bad one",
    );
    reg(
        "action:magit-global-bisect-start-finish",
        "Start the bisect once both ends are known",
    );
    reg(
        "action:magit-global-bisect-good",
        "Mark the revision git checked out as good",
    );
    reg(
        "action:magit-global-bisect-bad",
        "Mark the revision git checked out as bad",
    );
    reg(
        "action:magit-global-bisect-skip",
        "Skip a revision that cannot be tested",
    );
    reg(
        "action:magit-global-bisect-reset",
        "End the bisect and return to where it started",
    );
    // MG.21i: magit-submodule-mode.
    reg("action:magit-submodule-add", "Add a submodule");
    reg(
        "action:magit-submodule-add-path",
        "Ask where to put the submodule after its URL",
    );
    reg(
        "action:magit-submodule-add-finish",
        "Add the submodule once its URL and path are known",
    );
    reg(
        "action:magit-submodule-update",
        "Initialise and check out the submodule at cursor",
    );
    reg(
        "action:magit-submodule-sync",
        "Re-copy the configured URL into the submodule at cursor",
    );
    reg(
        "action:magit-submodule-remove",
        "Remove the submodule at cursor (asks first)",
    );
    reg(
        "action:magit-global-submodule",
        "Open the Magit submodule list buffer",
    );
    // MG.35
    reg(
        "action:magit-global-refs",
        "Open the Magit refs buffer — every branch, remote-tracking branch and tag",
    );
    // MG.37: magit-notes-mode's own chords.
    reg(
        "action:magit-cherry-show",
        "Show the commit at cursor in the cherry list",
    );
    reg("action:magit-note-confirm", "Save this note");
    reg("action:magit-note-abort", "Close the note buffer without saving");

    // MG.38 / MG.39 / MG.40.
    for (name, doc) in [
        ("action:magit-global-subtree-add", "Add a repository as a subtree"),
        ("action:magit-global-subtree-merge", "Merge a ref into a subtree"),
        ("action:magit-global-subtree-pull", "Fetch and merge a subtree's upstream"),
        ("action:magit-global-subtree-push", "Push a subtree's history to its repository"),
        ("action:magit-global-subtree-split", "Extract a subtree's history"),
        ("action:magit-global-subtree-finish", "Run the subtree operation once its arguments are known"),
        ("action:magit-global-am-apply", "Apply a mailbox of patches"),
        ("action:magit-global-am-apply-finish", "Run the patch apply once the files are known"),
        ("action:magit-global-am-continue", "Resume a stopped patch apply"),
        ("action:magit-global-am-skip", "Skip the patch that would not apply"),
        ("action:magit-global-am-abort", "Abandon a stopped patch apply"),
        ("action:magit-global-format-patch", "Write a commit range out as .patch files"),
        ("action:magit-global-format-patch-finish", "Run format-patch once the range is known"),
        ("action:magit-global-cherries", "Show which commits are not upstream yet"),
        ("action:magit-global-cherries-finish", "Open the cherry list once the upstream is known"),
    ] {
        reg(name, doc);
    }

    // MG.37: the notes submenu.
    reg(
        "action:magit-global-note-edit",
        "Edit the note on a commit — opens an editable buffer",
    );
    reg(
        "action:magit-global-note-remove",
        "Remove the note from a commit",
    );
    reg(
        "action:magit-global-note-prune",
        "Drop notes whose commit no longer exists (asks first)",
    );
    reg(
        "action:magit-global-note-prune-execute",
        "Execute the notes prune after confirmation",
    );
    reg(
        "action:magit-global-note-merge",
        "Merge another notes ref into this one",
    );
    reg(
        "action:magit-global-note-merge-finish",
        "Run the notes merge once the ref is known",
    );
    reg(
        "action:magit-global-note-merge-commit",
        "Finish a notes merge that stopped on a conflict",
    );
    reg(
        "action:magit-global-note-merge-abort",
        "Abandon a notes merge that stopped on a conflict",
    );

    // MG.36: the clone wizard's three steps.
    reg("action:magit-global-clone", "Clone a repository — asks for the URL");
    reg(
        "action:magit-global-clone-dest",
        "Second step of the clone wizard — asks where to put it",
    );
    reg(
        "action:magit-global-clone-finish",
        "Run the clone once the URL and destination are known",
    );

    reg(
        "action:magit-view-arguments",
        "Re-run this view with different git arguments",
    );
    // MG.19: `dv`. The session it opens is `lattice-diff`'s, so
    // `do` / `dp` / `]c` / `[c` and the scroll binding are that
    // subsystem's — nothing is reimplemented here.
    reg(
        "action:magit-diff-side-by-side",
        "Open the file at cursor side-by-side against its baseline",
    );
    reg("action:magit-remote-add", "Add a remote");
    reg(
        "action:magit-remote-add-url",
        "Ask for the new remote's URL after its name",
    );
    reg(
        "action:magit-remote-add-finish",
        "Add the remote once its name and URL are known",
    );
    reg("action:magit-remote-rename", "Rename the remote at cursor");
    reg(
        "action:magit-remote-rename-finish",
        "Rename the remote to the typed name",
    );
    reg("action:magit-remote-remove", "Remove the remote at cursor");
    reg(
        "action:magit-remote-set-url",
        "Set the URL of the remote at cursor",
    );
    reg(
        "action:magit-remote-set-url-finish",
        "Point the remote at the typed URL",
    );
    reg(
        "action:magit-remote-prune",
        "Delete local refs whose branch is gone from the remote at cursor",
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
    // MG.41e: the rebase submenu's sequence rows. The ex-commands
    // already existed (`:magit-rebase-continue` etc.); these are the
    // action names the menu rows fire.
    reg(
        "action:magit-global-rebase-continue",
        "Resume a rebase that stopped, after amending or resolving conflicts",
    );
    reg(
        "action:magit-global-rebase-skip",
        "Skip the commit a stopped rebase is sitting on",
    );
    reg(
        "action:magit-global-cherry-pick-continue",
        "Resume a cherry-pick that stopped on a conflict",
    );
    reg(
        "action:magit-global-cherry-pick-skip",
        "Skip the commit a stopped cherry-pick is sitting on",
    );
    reg(
        "action:magit-global-cherry-pick-abort",
        "Abandon a cherry-pick in progress, restoring the branch",
    );
    reg(
        "action:magit-global-revert-continue",
        "Resume a revert that stopped on a conflict",
    );
    reg(
        "action:magit-global-revert-skip",
        "Skip the commit a stopped revert is sitting on",
    );
    reg(
        "action:magit-global-revert-abort",
        "Abandon a revert in progress, restoring the branch",
    );
    reg(
        "action:magit-global-rebase-abort",
        "Abandon a rebase in progress, restoring the branch to where it started",
    );
    reg(
        "action:magit-global-fetch",
        "Fetch from the remote without merging",
    );
    reg(
        "action:magit-global-pull",
        "Fetch + fast-forward merge from the remote",
    );
    reg("action:magit-global-push", "Push to the remote");
    reg(
        "action:magit-global-stash-keep-index",
        "Stash everything but leave the index staged",
    );
    reg("action:magit-global-stash-staged", "Stash only the staged changes");

    // MG.41c: magit's destination rows. The op is the same each time —
    // only where it sends or takes refs differs — so these share one
    // handler shape (`spawn_remote_op_to`) rather than one function
    // each.
    reg(
        "action:magit-global-push-configured",
        "Push to the configured push-remote (git resolves pushRemote / pushDefault)",
    );
    reg(
        "action:magit-global-push-upstream",
        "Push to this branch's @{upstream} — differs from the push-remote in a triangular workflow",
    );
    reg("action:magit-global-push-elsewhere", "Push to a remote you name");
    reg("action:magit-global-push-other-branch", "Push a branch other than HEAD");
    reg("action:magit-global-push-refspecs", "Push explicit refspecs");
    reg("action:magit-global-push-tag", "Push a single tag");
    reg("action:magit-global-push-all-tags", "Push every tag");

    reg(
        "action:magit-global-pull-configured",
        "Pull from the configured remote",
    );
    reg("action:magit-global-pull-upstream", "Pull from this branch's @{upstream}");
    reg("action:magit-global-pull-elsewhere", "Pull from a remote you name");

    reg(
        "action:magit-global-fetch-configured",
        "Fetch from the configured remote",
    );
    reg("action:magit-global-fetch-upstream", "Fetch this branch's @{upstream}");
    reg("action:magit-global-fetch-elsewhere", "Fetch from a remote you name");
    reg("action:magit-global-fetch-other-branch", "Fetch a branch you name");
    reg("action:magit-global-fetch-refspecs", "Fetch explicit refspecs");
    reg("action:magit-global-fetch-all-remotes", "Fetch from every configured remote");

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
    // MG.41e: the merge / tag submenu rows.
    reg(
        "action:magit-global-merge-no-commit",
        "Merge a branch but stop before committing (asks which)",
    );
    reg(
        "action:magit-global-merge-squash",
        "Squash a branch's changes into the index without a merge commit (asks which)",
    );
    reg("action:magit-global-tag-delete", "Delete a local tag (asks which)");
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
    // MG.28: `v` on the file dispatch — the direct way into
    // `magit-file-revision-mode`, which until now was reachable only by
    // `<CR>` inside a revision view and `gj`/`gk` from there.
    reg(
        "action:magit-global-file-at-revision",
        "Open the current file as it was at a revision you name",
    );
    reg(
        "action:magit-global-file-at-revision-finish",
        "Open the file once the revision is known",
    );
    reg(
        "action:magit-global-file-visit-live",
        "From a file-at-revision, open the working-tree copy at the same line",
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

    // MG.32: the rest of magit's branch transient. Each row is an
    // ask-half (opens a picker or a prompt) plus, where the flow needs
    // one, a finish-half fired by the prompt it opened.
    reg(
        "action:magit-global-branch-checkout-rev",
        "Check out a branch or revision you name",
    );
    reg(
        "action:magit-global-branch-checkout-rev-finish",
        "Check out the typed branch or revision",
    );
    reg(
        "action:magit-global-branch-create-no-checkout",
        "Create a branch without checking it out",
    );
    reg(
        "action:magit-branch-create-no-checkout-finish",
        "Create the new branch (from the picked base) without checking it out",
    );
    reg("action:magit-global-branch-rename", "Rename a branch");
    reg(
        "action:magit-branch-rename-finish",
        "Rename the picked branch to the typed name",
    );
    reg(
        "action:magit-global-branch-delete",
        "Delete a branch — asks first",
    );
    drop(reg);

    // MG.23k: `D` — re-run the current view with different git
    // arguments. ONE action for both flag tables: the schema is their
    // union (the names are disjoint), and the argv is built from the
    // VIEW's own table, so a diff buffer can never be handed a log
    // flag. `project_transient_state` matches by name, so a slot the
    // open menu did not offer simply stays unset.
    registry.register_action(
        "action:magit-view-refresh-args",
        "Re-run the current magit view with the chosen git arguments",
        ActionSpec {
            apply: none.clone().unwrap(),
            args_schema: magit_diff_mode::DIFF_ARGS
                .iter()
                .chain(magit_log_mode::LOG_ARGS.iter())
                .map(|f| {
                    let kind = match f.kind {
                        magit_global_mode::RemoteArgKind::Flag => lattice_grammar::ArgKind::Bool,
                        _ => lattice_grammar::ArgKind::String,
                    };
                    lattice_grammar::ArgSpec::optional(f.name, kind, f.doc)
                })
                .collect(),
        },
    );

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
        collect!(MagitRemoteMode, "magit-remote-mode");
        collect!(MagitRefsMode, "magit-refs-mode");
        collect!(MagitNotesMode, "magit-notes-mode");
        collect!(MagitCherryMode, "magit-cherry-mode");
        collect!(MagitSubmoduleMode, "magit-submodule-mode");
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
        let ids = transients::MagitActionIds::resolve(&registry);

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

    /// MG.21d: `M` opens remote management, in every context.
    ///
    /// It reads nothing from the cursor — the buffer it opens lists the
    /// remotes itself — so unlike the section-acting rows above it must
    /// NOT be gated on being inside a magit buffer. And it must be a
    /// real `Action`: an `M` that fell back to a `Flag` would look
    /// present and do nothing, which is the failure the no-inert-rows
    /// policy exists to stop.
    #[test]
    fn remote_management_is_offered_everywhere_and_is_not_an_inert_row() {
        use lattice_picker::TransientItemKind;

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        for ctx in [in_magit_status(), in_magit_log(), outside_magit()] {
            let item = transients::dispatch_transient(&ids, &ctx)
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.key.iter().any(|k| k == "M"))
                .cloned()
                .expect("the dispatch offers `M` in every context");
            assert!(
                matches!(item.kind, TransientItemKind::Action(_)),
                "`M` resolved to {:?}, not a real action",
                item.label
            );
        }
    }

    /// MG.23k: the union schema and the tables it is built from must
    /// stay in lockstep.
    ///
    /// The action receives a POSITIONAL list, so a slot that shifts
    /// means a toggle lands in a neighbour's slot and the wrong git
    /// flag runs — silently, with a diff that looks merely surprising.
    #[test]
    fn the_view_argument_schema_matches_the_tables_it_is_built_from() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let spec = registry
            .lookup_by_name("action:magit-view-refresh-args")
            .expect("registered");
        let declared: Vec<&str> = spec.args_schema.iter().map(|a| a.name.as_ref()).collect();
        let expected: Vec<&str> = magit_core_mode::VIEW_ARG_TABLES
            .iter()
            .flat_map(|t| t.iter())
            .map(|f| f.name)
            .collect();
        assert_eq!(declared, expected);
    }

    /// Flag names must be unique across the tables, or `view_argv`'s
    /// position lookup resolves the wrong slot.
    #[test]
    fn no_two_view_arguments_share_a_name() {
        let mut seen = std::collections::HashSet::new();
        for f in magit_core_mode::VIEW_ARG_TABLES
            .iter()
            .flat_map(|t| t.iter())
        {
            assert!(
                seen.insert(f.name),
                "`{}` appears in more than one view-argument table — \
                 `view_argv` resolves slots by name",
                f.name
            );
        }
    }

    /// A view only ever gets its OWN arguments, even though the action
    /// carries the union of both tables.
    #[test]
    fn a_view_never_receives_the_other_views_arguments() {
        use lattice_grammar::{ArgValue, Args};
        // Every slot set: diff's three, then log's three.
        let all_set = Args::List(vec![
            ArgValue::Bool(true),              // ignore-space
            ArgValue::Bool(true),              // stat
            ArgValue::String("3".into()),      // unified
            ArgValue::Bool(true),              // all
            ArgValue::String("200".into()),    // count
            ArgValue::String("dhruva".into()), // author
        ]);

        let diff = magit_core_mode::view_argv(magit_diff_mode::DIFF_ARGS, &all_set);
        assert_eq!(diff, vec!["-w", "--stat", "--unified=3"]);
        assert!(
            !diff.iter().any(|a| a.contains("author") || a == "--all"),
            "a diff must not receive log arguments: {diff:?}"
        );

        let log = magit_core_mode::view_argv(magit_log_mode::LOG_ARGS, &all_set);
        assert_eq!(log, vec!["--all", "-n", "200", "--author", "dhruva"]);
        assert!(
            !log.iter().any(|a| a == "-w" || a.starts_with("--unified")),
            "a log must not receive diff arguments: {log:?}"
        );
    }

    /// The joined form is not cosmetic: `git diff -U 3` and
    /// `--unified 3` are both errors, so the value has to arrive glued
    /// to its argument as a single token.
    #[test]
    fn the_context_argument_is_one_joined_token() {
        use lattice_grammar::{ArgValue, Args};
        let args = Args::List(vec![
            ArgValue::Bool(false),
            ArgValue::Bool(false),
            ArgValue::String("5".into()),
        ]);
        assert_eq!(
            magit_core_mode::view_argv(magit_diff_mode::DIFF_ARGS, &args),
            vec!["--unified=5"]
        );
    }

    /// An unset value contributes nothing — not an empty string, which
    /// git reads as a real (empty) argument and rejects.
    #[test]
    fn unset_view_arguments_contribute_nothing() {
        use lattice_grammar::{ArgValue, Args};
        let none = Args::List(vec![
            ArgValue::Bool(false),
            ArgValue::Bool(false),
            ArgValue::String(String::new()),
        ]);
        assert!(magit_core_mode::view_argv(magit_diff_mode::DIFF_ARGS, &none).is_empty());
        assert!(
            magit_core_mode::view_argv(magit_diff_mode::DIFF_ARGS, &Args::None).is_empty(),
            "no state at all is the same as nothing set"
        );
    }

    /// `D`'s menu shows the arguments of the view it was opened in,
    /// and says so plainly where there are none — the chord is on
    /// `magit-core-mode`, so it fires in every magit buffer.
    #[test]
    fn the_argument_menu_follows_the_view_it_was_opened_in() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        let keys = |ctx: &lattice_picker::TransientContext| -> Vec<String> {
            transients::view_arguments_transient(&ids, ctx)
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .flat_map(|i| i.key.clone())
                .collect()
        };

        let in_diff = lattice_picker::TransientContext {
            major_mode: Some("magit-diff-mode".into()),
            minor_modes: vec!["magit-core-mode".into()],
        };
        let diff_keys = keys(&in_diff);
        for k in ["-w", "-s", "-U", "g"] {
            assert!(diff_keys.contains(&k.to_string()), "diff: {diff_keys:?}");
        }
        assert!(
            !diff_keys.contains(&"-A".to_string()),
            "diff: {diff_keys:?}"
        );

        let in_log = lattice_picker::TransientContext {
            major_mode: Some("magit-log-mode".into()),
            minor_modes: vec!["magit-core-mode".into()],
        };
        let log_keys = keys(&in_log);
        for k in ["-a", "-n", "-A", "g"] {
            assert!(log_keys.contains(&k.to_string()), "log: {log_keys:?}");
        }
        assert!(!log_keys.contains(&"-w".to_string()), "log: {log_keys:?}");

        // A view with no arguments: a menu that says so, not an empty
        // one and not a missing key.
        let elsewhere = in_magit_status();
        assert!(keys(&elsewhere).is_empty());
        assert!(
            transients::view_arguments_transient(&ids, &elsewhere).groups[0]
                .label
                .contains("no arguments"),
            "a view without arguments must say so"
        );
    }

    /// MG.26b: two minors that can be active on the SAME buffer must
    /// not bind the same chord.
    ///
    /// `magit-blame-mode` annotates blob buffers, where
    /// `magit-core-mode` is also active. Both binding `q` — which the
    /// first draft did, since `q` is magit's own key for stopping a
    /// blame — resolves by registration order, which is not a contract
    /// anything should depend on. The chord guard would not have caught
    /// it: both chords reach a registered action and a handler.
    #[test]
    fn the_blame_minor_shares_no_chord_with_magit_core() {
        use lattice_mode::Mode;
        let core: Vec<&str> = MagitCoreMode
            .keymap()
            .entries
            .iter()
            .map(|e| e.chord)
            .collect();
        for entry in MagitBlameMode.keymap().entries {
            assert!(
                !core.contains(&entry.chord),
                "`{}` is bound by BOTH magit-blame-mode and magit-core-mode, and \
                 both are active on a blob buffer — which one wins is registration \
                 order, not a contract",
                entry.chord
            );
        }
    }

    /// `magit-core-mode` activates by major, so naming a *minor* in
    /// its allowlist is an entry that can never match — dead config
    /// that reads as intent.
    #[test]
    fn magit_core_activates_only_on_real_majors() {
        use lattice_mode::{ActivationPolicy, Mode};
        let ActivationPolicy::Majors(majors) = MagitCoreMode.activation_policy() else {
            panic!("magit-core-mode activates by major");
        };
        assert!(
            !majors.contains(&MagitBlameMode::mode_id()),
            "magit-blame-mode is a minor — it can never be an active MAJOR, so \
             this entry never matches"
        );
        assert_eq!(MagitBlameMode.kind(), lattice_mode::ModeKind::Minor);
    }

    /// MG.28: `v` on the file dispatch, and `:magit-find-file`, are the
    /// direct ways into `magit-file-revision-mode`.
    ///
    /// The mode has existed since MG.11 with no direct entry point —
    /// reachable only by `<CR>` inside a revision view and `gj`/`gk`
    /// from there — so "show me this file at that revision" had no
    /// answer. Both must resolve, or the row is inert and the command
    /// is missing.
    #[test]
    fn a_file_at_a_revision_is_reachable_directly() {
        use lattice_picker::TransientItemKind;

        let mut actions = CommandRegistry::new();
        register_action_commands(&mut actions);
        let ids = transients::MagitActionIds::resolve(&actions);
        let row = transients::file_dispatch_transient(&ids)
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .find(|i| i.key.iter().any(|k| k == "v"))
            .cloned()
            .expect("`C-c f v` must exist");
        assert!(
            matches!(row.kind, TransientItemKind::Action(_)),
            "`v` resolved to {:?}, not a real action",
            row.label
        );

        let mut ex = CommandRegistry::new();
        register_ex_commands(&mut ex, Default::default());
        let id = ex
            .id_by_name("magit-find-file")
            .expect("`:magit-find-file` must exist");
        assert!(
            ex.ex_command_spec(id).is_some(),
            "`:magit-find-file` must be an EX command"
        );
    }

    /// MG.28: `V` is the way back out. `gj` / `gk` walk a blob's
    /// history and nothing walked back to the working-tree copy — you
    /// had to type `:e <path>` for a path you were already looking at.
    ///
    /// `v` and `V` must be distinct rows: one goes in, the other comes
    /// out, and a single key doing both by context would be the
    /// mislabelled-chord problem `]f` had.
    #[test]
    fn the_way_into_a_revision_and_the_way_back_are_separate_rows() {
        use lattice_picker::TransientItemKind;

        let mut actions = CommandRegistry::new();
        register_action_commands(&mut actions);
        let ids = transients::MagitActionIds::resolve(&actions);
        let spec = transients::file_dispatch_transient(&ids);

        let row = |key: &str| {
            spec.groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.key.iter().any(|k| k == key))
                .cloned()
                .unwrap_or_else(|| panic!("`C-c f {key}` must exist"))
        };
        for key in ["v", "V"] {
            assert!(
                matches!(row(key).kind, TransientItemKind::Action(_)),
                "`{key}` must be a real action"
            );
        }
        assert_ne!(
            transients::MagitActionIds::resolve(&actions).get("action:magit-global-file-at-revision"),
            transients::MagitActionIds::resolve(&actions).get("action:magit-global-file-visit-live"),
            "in and out are different actions, not one key guessing"
        );
    }

    /// MG.29: `b` is a submenu, and the list it used to open directly
    /// is still reachable inside it.
    ///
    /// The regression this guards is a real one to make: moving `b` to
    /// a submenu and forgetting to carry the list row would silently
    /// remove the only way to the branch buffer from the menu.
    #[test]
    fn the_branch_submenu_keeps_the_list_it_replaced() {
        use lattice_picker::{TransientItemKind, TransientSpec};

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        let item = transients::dispatch_transient(&ids, &outside_magit())
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .find(|i| i.key.iter().any(|k| k == "b"))
            .cloned()
            .expect("`b` must exist on the dispatch");
        let TransientItemKind::Submenu(spec) = &item.kind else {
            panic!("`b` must open a submenu, got {:?}", item.label);
        };
        let spec: &TransientSpec = spec;
        let rows: Vec<(String, bool)> = spec
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .flat_map(|i| {
                let real = matches!(i.kind, TransientItemKind::Action(_));
                i.key.iter().map(move |k| (k.clone(), real))
            })
            .collect();

        // MG.32: the full set magit's own branch transient shows, minus
        // the still-deferred `s` / `S` / `C`.
        //
        // MG.41a moved delete from `x` to magit's own `k`. Inside a
        // transient the menu owns every keystroke, so there is no vim
        // grammar to dodge and no reason to diverge — and the old `x`
        // put DELETE where a magit user reaches for reset. `x` is left
        // free for reset, which MG.41d adds.
        for key in ["b", "l", "c", "n", "m", "k", "L"] {
            let (_, real) = rows
                .iter()
                .find(|(k, _)| k == key)
                .unwrap_or_else(|| panic!("`b {key}` must exist: {rows:?}"));
            assert!(real, "`b {key}` must be a real action");
        }
        assert!(
            !rows.iter().any(|(k, _)| k == "x"),
            "`x` must stay free for reset (magit's own key); delete is `k`: {rows:?}",
        );
        assert_eq!(
            transients::MagitActionIds::resolve(&registry).get("action:magit-global-branch"),
            registry.id_by_name("action:magit-global-branch"),
            "`b L` fires the SAME action `b` used to — the list did not \
             disappear when MG.32 moved it off `l`"
        );
    }

    /// MG.32: the two keys MG.29 got wrong, pinned so they cannot drift
    /// back.
    ///
    /// Both were found by inventorying magit's own `magit-branch`
    /// transient (with `evil-collection-magit-popup-changes` applied) —
    /// the step MG.29 skipped:
    ///
    /// - **`l` is checkout-local-branch in magit**, so the list buffer
    ///   (a lattice concept magit has no row for) had squatted on an
    ///   occupied key. The list moved to `L`.
    /// - **`b` is branch/*revision* in magit** — it takes a tag, a
    ///   remote ref or a raw SHA. MG.29's `b` offered a list of local
    ///   branches, which cannot express any of those; that row *was*
    ///   magit's `l`, and is now bound as such.
    ///
    /// A test on the mapping rather than on mere presence, because both
    /// bugs were "the row exists, under the wrong letter" — the
    /// presence check above passed throughout.
    #[test]
    fn the_branch_submenu_keys_mean_what_magit_means_by_them() {
        use lattice_picker::{TransientItemKind, TransientSpec};

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        let item = transients::dispatch_transient(&ids, &outside_magit())
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .find(|i| i.key.iter().any(|k| k == "b"))
            .cloned()
            .expect("`b` must exist on the dispatch");
        let TransientItemKind::Submenu(spec) = &item.kind else {
            panic!("`b` must open a submenu");
        };
        let spec: &TransientSpec = spec;

        let action_for = |key: &str| -> Option<lattice_grammar::CommandId> {
            spec.groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.key.iter().any(|k| k == key))
                .and_then(|i| match i.kind {
                    TransientItemKind::Action(id) => Some(id),
                    _ => None,
                })
        };

        // Both sides of every comparison below are `Option`, so
        // `None == None` would pass vacuously — an unregistered action
        // and an absent row would agree with each other. Pin that these
        // resolve before comparing them.
        for key in ["b", "l", "L"] {
            assert!(
                action_for(key).is_some(),
                "`b {key}` must resolve to a real action, or the assertions \
                 below compare None to None and prove nothing"
            );
        }

        assert_eq!(
            action_for("l"),
            registry.id_by_name("action:magit-global-branch-checkout"),
            "`l` is magit's checkout-LOCAL-branch, and that is exactly the \
             picker MG.29 had built — it only sat on the wrong key"
        );
        assert_eq!(
            action_for("b"),
            registry.id_by_name("action:magit-global-branch-checkout-rev"),
            "`b` is magit's branch/REVISION: it must reach the prompt that \
             accepts a tag / remote ref / SHA, not the local-branch list"
        );
        assert_eq!(
            action_for("L"),
            registry.id_by_name("action:magit-global-branch"),
            "the list buffer has no magit counterpart, so it takes `L` — \
             capital-as-variant, and a key magit's transient leaves free"
        );
        assert_ne!(
            action_for("b"),
            action_for("l"),
            "branch/revision and local-branch are different operations; one \
             of them pointing at the other is the MG.29 bug returning"
        );
    }

    /// MG.32: the keys magit uses for the four deferred rows stay FREE.
    ///
    /// The no-inert-rows policy says a row appears only once its
    /// operation exists — but MG.23's policy #1 also says a row landing
    /// later must land where muscle memory expects. Both hold only if
    /// nothing else claims `s` / `S` / `C` / `X` in the meantime, which
    /// is the kind of thing a later slice does without noticing.
    #[test]
    fn the_deferred_branch_rows_keep_their_magit_keys_free() {
        use lattice_picker::{TransientItemKind, TransientSpec};

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        let item = transients::dispatch_transient(&ids, &outside_magit())
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .find(|i| i.key.iter().any(|k| k == "b"))
            .cloned()
            .expect("`b` must exist on the dispatch");
        let TransientItemKind::Submenu(spec) = &item.kind else {
            panic!("`b` must open a submenu");
        };
        let spec: &TransientSpec = spec;
        let taken: Vec<String> = spec
            .groups
            .iter()
            .flat_map(|g| &g.items)
            .flat_map(|i| i.key.iter().cloned())
            .collect();

        // s spin-off, S spin-out, C configure, X reset (magit's `x`,
        // moved by evil-collection-magit).
        for key in ["s", "S", "C", "X"] {
            assert!(
                !taken.iter().any(|k| k == key),
                "`{key}` is magit's key for a deferred branch row — leaving it \
                 free is what lets that row land in the slot muscle memory \
                 expects. Taken: {taken:?}"
            );
        }
    }

    /// Every submenu tells the user `Esc` goes back, now that it does.
    /// A footer still saying only `BS` would be documenting behaviour
    /// the editor no longer has.
    #[test]
    fn submenu_footers_offer_esc_as_back() {
        use lattice_picker::{TransientItemKind, TransientSpec};

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);
        let root = transients::dispatch_transient(&ids, &outside_magit());

        let mut checked = 0;
        for item in root.groups.iter().flat_map(|g| &g.items) {
            if let TransientItemKind::Submenu(spec) = &item.kind {
                let spec: &TransientSpec = spec;
                let footer = spec.footer.clone().unwrap_or_default();
                assert!(
                    footer.contains("Esc"),
                    "submenu {:?} does not offer Esc as back: {footer:?}",
                    spec.title
                );
                checked += 1;
            }
        }
        assert!(checked >= 4, "expected several submenus, checked {checked}");
    }

    /// MG.21i: `o` opens the submodule list, in every context.
    ///
    /// Same claim as `M`'s, for the same reason — the buffer lists the
    /// submodules itself, so there is nothing to read from a cursor
    /// and nothing to gate on.
    #[test]
    fn submodule_management_is_offered_everywhere_and_is_not_an_inert_row() {
        use lattice_picker::TransientItemKind;

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        for ctx in [in_magit_status(), in_magit_log(), outside_magit()] {
            let item = transients::dispatch_transient(&ids, &ctx)
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.key.iter().any(|k| k == "o"))
                .cloned()
                .expect("the dispatch offers `o` in every context");
            assert!(
                matches!(item.kind, TransientItemKind::Action(_)),
                "`o` resolved to {:?}, not a real action",
                item.label
            );
        }
    }

    /// Both list buffers are reachable two ways, and the two must name
    /// the same mode — a drift is silent, since the buffer would open
    /// with no mode and every chord on it would be inert.
    #[test]
    fn the_submodule_buffer_is_reachable_by_ex_command_and_by_action() {
        let mut registry = CommandRegistry::new();
        register_ex_commands(&mut registry, Default::default());
        let id = registry
            .id_by_name("magit-submodule")
            .expect("`:magit-submodule` must exist");
        assert!(
            registry.ex_command_spec(id).is_some(),
            "`:magit-submodule` must be an EX command, not an action of the same name"
        );

        let mut actions = CommandRegistry::new();
        register_action_commands(&mut actions);
        assert!(
            actions
                .id_by_name("action:magit-global-submodule")
                .is_some(),
            "`o` fires `action:magit-global-submodule` — it must be registered"
        );
        assert_eq!(
            MagitSubmoduleMode::mode_id().as_str(),
            "magit-submodule-mode",
            "the mode id both open paths hardcode"
        );
    }

    /// MG.21g: the bisect menu shows the operations that can actually
    /// run, and only those.
    ///
    /// Outside a bisect, `good` / `bad` / `skip` / `reset` are not
    /// merely useless — git errors on them, so they would be rows that
    /// look actionable and produce a log line. `start` during a bisect
    /// is the same in reverse. Both directions are asserted: a gate
    /// that never opens and a gate that never closes both pass a
    /// one-sided test.
    #[test]
    fn the_bisect_menu_offers_start_or_the_marks_but_never_both() {
        use lattice_picker::{TransientItemKind, TransientSpec};

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        let bisect_keys = |in_progress: bool| -> Vec<String> {
            let root = transients::dispatch_transient_with(
                &ids,
                &outside_magit(),
                transients::DispatchGates {
                    bisect: in_progress,
                    notes_merge: false,
                    am: false,
                    rebase: false,
                    cherry_pick: false,
                    revert: false,
                },
            );
            let item = root
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.key.iter().any(|k| k == "B"))
                .expect("the dispatch offers `B`");
            let TransientItemKind::Submenu(spec) = &item.kind else {
                panic!("`B` must open a submenu, got {:?}", item.label);
            };
            let spec: &TransientSpec = spec;
            spec.groups
                .iter()
                .flat_map(|g| &g.items)
                .flat_map(|i| i.key.clone())
                .collect()
        };

        let idle = bisect_keys(false);
        assert_eq!(idle, vec!["B"], "idle offers only start: {idle:?}");

        let running = bisect_keys(true);
        for k in ["g", "b", "k", "r"] {
            assert!(
                running.contains(&k.to_string()),
                "`{k}` must be offered during a bisect: {running:?}"
            );
        }
        assert!(
            !running.contains(&"B".to_string()),
            "start must NOT be offered during a bisect: {running:?}"
        );
    }

    /// Every bisect row must resolve to a real action in both states —
    /// an inert `Flag` here would be a row that looks like it marks a
    /// revision and does nothing.
    #[test]
    fn every_bisect_row_resolves_to_a_real_action() {
        use lattice_picker::{TransientItemKind, TransientSpec};

        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

        for in_progress in [false, true] {
            let root = transients::dispatch_transient_with(
                &ids,
                &outside_magit(),
                transients::DispatchGates {
                    bisect: in_progress,
                    notes_merge: false,
                    am: false,
                    rebase: false,
                    cherry_pick: false,
                    revert: false,
                },
            );
            let item = root
                .groups
                .iter()
                .flat_map(|g| &g.items)
                .find(|i| i.key.iter().any(|k| k == "B"))
                .expect("`B` row");
            let TransientItemKind::Submenu(spec) = &item.kind else {
                panic!("`B` must open a submenu");
            };
            let spec: &TransientSpec = spec;
            for row in spec.groups.iter().flat_map(|g| &g.items) {
                assert!(
                    matches!(row.kind, TransientItemKind::Action(_)),
                    "bisect row {:?} is inert (in_progress={in_progress})",
                    row.label
                );
            }
        }
    }

    /// The chord half of the same claim, from the other direction: `M`
    /// (remote) and `B` (bisect) are *transient* keys only. Binding
    /// either as a chord inside a magit buffer would shadow a vim
    /// motion — middle-of-screen and back-WORD — which is the same
    /// reasoning that keeps `V` free
    /// (`feedback_magit_keys_follow_evil_magit`). Magit binds both in
    /// its own buffers; it can, because it is not modal.
    #[test]
    fn no_magit_mode_binds_m_or_b_as_a_chord() {
        use lattice_mode::Mode;
        macro_rules! check {
            ($($mode:expr => $label:literal),* $(,)?) => {
                $(for entry in $mode.keymap().entries {
                    for taken in ["M", "B"] {
                        assert!(
                            entry.chord != taken,
                            "`{}` binds `{taken}`, shadowing the vim motion — \
                             put it on the dispatch transient instead", $label
                        );
                    }
                })*
            };
        }
        check!(
            MagitCoreMode => "magit-core-mode",
            MagitStatusMode => "magit-status-mode",
            MagitBranchMode => "magit-branch-mode",
            MagitRemoteMode => "magit-remote-mode",
            MagitRefsMode => "magit-refs-mode",
            MagitNotesMode => "magit-notes-mode",
            MagitCherryMode => "magit-cherry-mode",
            MagitSubmoduleMode => "magit-submodule-mode",
            MagitStashMode => "magit-stash-mode",
            MagitLogMode => "magit-log-mode",
            MagitDiffMode => "magit-diff-mode",
            MagitBlameMode => "magit-blame-mode",
            MagitRebaseMode => "magit-rebase-mode",
            MagitRevisionMode => "magit-revision-mode",
            MagitFileRevisionMode => "magit-file-revision-mode",
            magit_stash_show_mode::MagitStashShowMode => "magit-stash-show-mode",
            magit_hunk_mode::MagitHunkMode => "magit-hunk-mode",
        );
    }

    /// The `s` row swaps meaning in magit-status and only there — the
    /// `:if-mode` half, which is a different predicate from the one
    /// above and would be indistinguishable from it if only the status
    /// buffer were tested.
    #[test]
    fn the_status_row_becomes_a_section_jump_only_in_the_status_buffer() {
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);

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
        let ids = transients::MagitActionIds::resolve(&registry);
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
        let ids = transients::MagitActionIds::resolve(&registry);
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

    /// MG.24c — every view the docs say answers "what commit is under
    /// the cursor" actually overrides the method that answers it.
    ///
    /// `magit-core-mode.md` names four views for `A` / `_` / `O`. Two
    /// of them — the revision view and the rebase todo — never
    /// implemented `commit_at_cursor`, so the trait default returned
    /// `None` and the chords were consumed dead keys for two of the
    /// four documented cases. Nothing failed loudly; the doc simply
    /// described behaviour no code provided.
    ///
    /// Asserted structurally rather than by driving the chords: what
    /// went wrong was a *missing override*, and an override that exists
    /// but returns `None` for a given buffer is a different (and
    /// legitimate) thing. This catches the class that actually bit.
    #[test]
    fn every_commit_showing_view_overrides_commit_at_cursor() {
        use crate::buffer_state::MagitView;
        use lattice_protocol::position::Position;

        // A view whose `commit_at_cursor` is the trait DEFAULT answers
        // `None` for every cursor. That is what the revision and rebase
        // views did before this slice.
        struct DefaultOnly;
        impl MagitView for DefaultOnly {
            fn refresh(&self) -> Option<Effect> {
                None
            }
        }
        assert!(
            DefaultOnly.commit_at_cursor(Position::new(0, 0)).is_none(),
            "the trait default must answer None — this test's premise"
        );

        // The guard: the source files for the views the docs name must
        // each carry an override. A structural check, because
        // constructing these views needs a live buffer store and a
        // published state, which is a fixture per mode rather than a
        // fact about the code.
        for (module, file) in [
            ("magit-status", include_str!("actions.rs")),
            ("magit-log", include_str!("magit_log_mode.rs")),
            ("magit-revision", include_str!("magit_revision_mode.rs")),
            ("magit-rebase", include_str!("magit_rebase_mode.rs")),
        ] {
            assert!(
                file.contains("fn commit_at_cursor"),
                "`{module}` is named in magit-core-mode.md as a view where \
                 `A` / `_` / `O` act on the commit at the cursor, so its \
                 MagitView must override `commit_at_cursor` — without it \
                 the trait default answers None and the chords are dead"
            );
        }
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
        register_ex_commands(&mut registry, Default::default());
        for op in [
            magit_global_mode::CommitOp::CHERRY_PICK,
            magit_global_mode::CommitOp::REVERT,
            magit_global_mode::CommitOp::RESET_SOFT,
            magit_global_mode::CommitOp::RESET_MIXED,
            magit_global_mode::CommitOp::RESET_HARD,
            // MG.41d: magit's remaining reset modes + the autosquash
            // pair. Each is data — same handler, different argv.
            magit_global_mode::CommitOp::RESET_KEEP,
            magit_global_mode::CommitOp::RESET_INDEX,
            magit_global_mode::CommitOp::COMMIT_FIXUP,
            magit_global_mode::CommitOp::COMMIT_SQUASH,
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

    /// MG.21c: `:magit-remote` exists and is an ex-command.
    ///
    /// Two independent registrations open `*magit:remote*` with
    /// `magit-remote-mode` — this one and `M`'s
    /// `action:magit-global-remote`. Both are asserted here, because a
    /// drift between them is silent: the buffer would open with no
    /// mode, so every chord on it would be inert while the buffer
    /// itself looked fine.
    #[test]
    fn the_remote_buffer_is_reachable_by_ex_command_and_by_action() {
        let mut registry = CommandRegistry::new();
        register_ex_commands(&mut registry, Default::default());
        let id = registry
            .id_by_name("magit-remote")
            .expect("`:magit-remote` must exist");
        assert!(
            registry.ex_command_spec(id).is_some(),
            "`:magit-remote` must be an EX command, not an action of the same name"
        );

        let mut actions = CommandRegistry::new();
        register_action_commands(&mut actions);
        assert!(
            actions.id_by_name("action:magit-global-remote").is_some(),
            "`M` fires `action:magit-global-remote` — it must be registered"
        );

        // The mode both paths name has to be the one that is actually
        // installed, or the buffer opens without its keymap.
        assert_eq!(
            MagitRemoteMode::mode_id().as_str(),
            "magit-remote-mode",
            "the mode id both open paths hardcode"
        );
    }

    /// The repo-level rows fire the SAME actions the chords fire, so a
    /// row cannot drift onto a second handler with its own idea of the
    /// confirm contract.
    #[test]
    fn the_commit_rows_reuse_the_chords_actions() {
        // MG.41a: this property is now STRUCTURAL. Rows name their
        // command directly, so a row cannot drift onto a twin handler —
        // there is no second place to keep in sync. What is still worth
        // asserting is that the tables reference the *chord* actions
        // (`action:magit-reset-soft`) and not invented `-global-`
        // variants; getting exactly that wrong is what
        // `every_row_action_is_registered` caught while this slice was
        // being written.
        let mut registry = CommandRegistry::new();
        register_action_commands(&mut registry);
        let ids = transients::MagitActionIds::resolve(&registry);
        for action in [
            "action:magit-cherry-pick",
            "action:magit-revert",
            "action:magit-reset-soft",
            "action:magit-reset-mixed",
            "action:magit-reset-hard",
        ] {
            assert_eq!(
                ids.get(action),
                registry.id_by_name(action),
                "the `{action}` row must fire that action, not a twin"
            );
            assert!(
                ids.get(action).is_some(),
                "`{action}` must stay registered — a row references it"
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

    /// MG.26b — reverse blame annotates the blob buffer it was run in,
    /// rather than opening a buffer of its own. Both halves still come
    /// out of that buffer's name; they now go into the request map,
    /// because `ToggleMode` carries only a mode name.
    #[test]
    fn reverse_blame_toggles_the_minor_on_the_blob_buffer_it_runs_in() {
        match fire_reverse_blame_in("*magit:file:a1b2c3d:src/main.rs*") {
            Some(Effect::ToggleMode { mode_name }) => {
                assert_eq!(mode_name, "magit-blame-mode");
            }
            other => panic!("expected the blame minor to be toggled, got {other:?}"),
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
            // Its chords are all `ex:` (`<C-x>g`, `<C-c>g`, `<C-c>f`),
            // so the `action:` filter below skips every one — included
            // anyway so the count check covers all fourteen registered
            // modes rather than thirteen plus an exception.
            MagitGlobalMode => "magit-global-mode",
            MagitCoreMode => "magit-core-mode",
            MagitGlobalMode => "magit-global-mode",
            MagitStatusMode => "magit-status-mode",
            MagitCommitMode => "magit-commit-mode",
            MagitDiffMode => "magit-diff-mode",
            MagitLogMode => "magit-log-mode",
            MagitBlameMode => "magit-blame-mode",
            MagitStashMode => "magit-stash-mode",
            MagitBranchMode => "magit-branch-mode",
            MagitRemoteMode => "magit-remote-mode",
            MagitRefsMode => "magit-refs-mode",
            MagitNotesMode => "magit-notes-mode",
            MagitCherryMode => "magit-cherry-mode",
            MagitSubmoduleMode => "magit-submodule-mode",
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

        // MG.22: `s`/`u`/`x` moved to `magit-hunk-mode`, so checking
        // the majors here would pass vacuously — they bind none of
        // them now. The pairing claim moved with the chords.
        for (label, keymap) in [("magit-hunk-mode", magit_hunk_mode::MagitHunkMode.keymap())] {
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
            MagitRemoteMode,
            MagitRefsMode,
            MagitNotesMode,
            MagitCherryMode,
            MagitSubmoduleMode,
            MagitRebaseMode,
            MagitRevisionMode,
            MagitFileRevisionMode,
            magit_stash_show_mode::MagitStashShowMode,
            magit_hunk_mode::MagitHunkMode,
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
            MagitRemoteMode => "magit-remote-mode",
            MagitRefsMode => "magit-refs-mode",
            MagitNotesMode => "magit-notes-mode",
            MagitCherryMode => "magit-cherry-mode",
            MagitSubmoduleMode => "magit-submodule-mode",
            MagitRebaseMode => "magit-rebase-mode",
            MagitRevisionMode => "magit-revision-mode",
            MagitFileRevisionMode => "magit-file-revision-mode",
            magit_stash_show_mode::MagitStashShowMode => "magit-stash-show-mode",
            magit_hunk_mode::MagitHunkMode => "magit-hunk-mode",
        );

        // MG.22: the two lists above are HAND-KEPT, and a mode missing
        // from them is not covered — which is not hypothetical. This
        // slice added `magit-hunk-mode`, bound `<CR>` on it, and forgot
        // to register the action; the guard said nothing, because the
        // mode was in neither list. `<CR>` would have been inert in all
        // five diff buffers.
        //
        // Cross-checked against `install`'s own registrations so the
        // omission cannot recur silently: every `.register(` there must
        // appear here.
        let installed = include_str!("lib.rs")
            .lines()
            .filter_map(|l| l.trim().strip_prefix(".register("))
            .filter_map(|l| l.strip_suffix(")"))
            .filter(|m| m.contains("Magit"))
            .count();
        assert_eq!(
            installed, 19,
            "`install` registers {installed} magit modes but this guard \
             checks 19 — a mode registered at boot and absent from the \
             lists above has its chords unverified"
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
            // One slot per flag, in table order. MG.41c appended
            // `--no-verify` / `--dry-run`, so the list grew — built
            // from the table rather than hard-coded so the next
            // addition does not fail this test for the wrong reason.
            Args::List(
                RemoteOp::PUSH
                    .flags
                    .iter()
                    .map(|f| ArgValue::Bool(f.name == "set-upstream"))
                    .collect()
            ),
            "the typo drops out; the flag that parsed still applies"
        );
    }

    /// An operation with no flags keeps `Args::None`, so its handler
    /// sees exactly what it saw before MG.17a.
    ///
    /// MG.41c: this used `PULL`, which now carries `-r` / `-a`. The
    /// property is about flagless ops, not about pull, so it moves to
    /// one that still is — asserted rather than assumed, so the test
    /// cannot quietly stop testing anything if that op gains flags too.
    #[test]
    fn a_flagless_operation_parses_to_no_args() {
        use lattice_grammar::Args;
        use magit_global_mode::RemoteOp;
        assert!(
            RemoteOp::REBASE_CONTINUE.flags.is_empty(),
            "this test needs a genuinely flagless op",
        );
        assert_eq!(parse_remote_flags(RemoteOp::REBASE_CONTINUE, "--force"), Args::None);
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
        register_ex_commands(&mut registry, Default::default());
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
        register_ex_commands(&mut registry, Default::default());
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
            ("magit-remote-mode", MagitRemoteMode.action_handlers()),
            ("magit-refs-mode", MagitRefsMode.action_handlers()),
            ("magit-notes-mode", MagitNotesMode.action_handlers()),
            ("magit-cherry-mode", MagitCherryMode.action_handlers()),
            ("magit-submodule-mode", MagitSubmoduleMode.action_handlers()),
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
        let ids = transients::MagitActionIds::resolve(&registry);
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
        let ids = transients::MagitActionIds::resolve(&registry);
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
            &transients::dispatch_transient(&transients::MagitActionIds::resolve(&registry), &in_magit_status()),
            "dispatch",
        );
        check(
            &transients::file_dispatch_transient(&transients::MagitActionIds::resolve(&registry)),
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
        // 15 file-dispatch items: stage/unstage/discard,
        // diff/log/blame, MG.23f2's reverse blame, MG.23d's
        // untrack/rename/delete, MG.23d2's checkout, MG.28's
        // at-revision/visit-live, and MG.34's merged/edit-line.
        let file = inert_items(
            &transients::file_dispatch_transient(&Default::default()),
            "",
        );
        assert_eq!(
            file.len(),
            15,
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
        // MG.21g: the gate is passed in, not probed. Probing would make
        // this count depend on whether the developer's own checkout is
        // mid-bisect while the suite runs — a flake, and one that would
        // have looked like a real regression.
        let root = inert_items(
            &transients::dispatch_transient_with(
                &Default::default(),
                &outside_magit(),
                transients::DispatchGates::default(),
            ),
            "",
        );
        assert_eq!(
            root.len(),
            // MG.41c: push/pull/fetch each replaced ONE run row with
            // destination rows — 7, 3 and 6 — so 46 + 6 + 2 + 5 = 59.
            // MG.41d: +2 reset modes, +2 commit autosquash rows.
            72,
            "expected every root-dispatch leaf (incl. both submenus') to \
             report inert, got: {root:?}"
        );

        // The in-progress branch of the bisect menu is a different set
        // of rows, and an unresolved id there would be just as inert.
        let bisecting = inert_items(
            &transients::dispatch_transient_with(
                &Default::default(),
                &outside_magit(),
                transients::DispatchGates {
                    bisect: true,
                    notes_merge: false,
                    am: false,
                    rebase: false,
                    cherry_pick: false,
                    revert: false,
                },
            ),
            "",
        );
        assert_eq!(
            bisecting.len(),
            // MG.41c: +13 destination rows; MG.41d: +4 more.
            75,
            "the in-progress bisect menu trades `start` for good/bad/skip/reset: {bisecting:?}"
        );
    }
}
