//! MG.8: magit transient menu definitions.
//!
//! Defines the `TransientSpec` instances for the repo-level
//! dispatch (C-c g) and file-level dispatch (C-c f) menus.
//! Each is a grouped action menu rendered by the PICK.1
//! transient picker overlay.

use std::sync::Arc;

use lattice_picker::{
    TransientContext, TransientGroup, TransientItem, TransientItemKind, TransientSpec,
    TransientState, TransientValue,
};
use lattice_protocol::ids::CommandId;

use crate::magit_global_mode::RemoteOp;

/// MG.17a: the `Flag` items for a [`RemoteOp`], built from the op's own
/// flag table so the menu can't offer a toggle the argv builder ignores.
fn flag_items(op: RemoteOp) -> Vec<TransientItem> {
    flag_items_from(op.flags)
}

/// MG.23k: the same translation for any flag table, not only a
/// [`RemoteOp`]'s — the view-arguments menu has flags but no operation.
fn flag_items_from(flags: &'static [crate::magit_global_mode::RemoteFlag]) -> Vec<TransientItem> {
    use crate::magit_global_mode::RemoteArgKind;
    flags
        .iter()
        .map(|f| TransientItem {
            key: vec![f.key.to_string()],
            label: f.arg.to_string(),
            description: f.doc.to_string(),
            kind: match f.kind {
                RemoteArgKind::Flag => TransientItemKind::Flag {
                    name: f.name.to_string(),
                    default: false,
                },
                // MG.17b: a value argument opens a prompt and comes
                // back to this menu with the value filled in.
                RemoteArgKind::Value { prompt } | RemoteArgKind::ValueJoined { prompt } => {
                    TransientItemKind::Argument {
                        name: f.name.to_string(),
                        default: None,
                        prompt: prompt.to_string(),
                    }
                }
            },
        })
        .collect()
}

/// MG.17a: the live preview for a [`RemoteOp`] transient — the exact
/// git command the current toggles resolve to. Rendered by the same
/// `RemoteOp::preview` the argv builder is paired with, so the preview
/// cannot claim one command while the run executes another.
fn remote_preview(op: RemoteOp) -> Box<dyn Fn(&TransientState) -> String + Send + Sync> {
    Box::new(move |state: &TransientState| {
        op.preview(&|name| match state.get(name) {
            Some(TransientValue::Bool(b)) => Some(b.to_string()),
            Some(TransientValue::String(v)) => Some(v.clone()),
            None => None,
        })
    })
}

/// MG.17a: a sub-transient for one remote operation — its flags, then
/// the key that runs it.
///
/// Flags need a menu that stays open while you toggle them, which the
/// flat root dispatch cannot do: pressing `P` there fires immediately.
/// So `P` now opens this, and `P` again (or `<CR>`) runs it — one extra
/// keystroke, in exchange for the flags being reachable at all.
fn remote_op_transient(
    title: &str,
    op: RemoteOp,
    run_key: &str,
    run_id: Option<CommandId>,
    run_label: &str,
    run_doc: &str,
    placeholder: &str,
) -> TransientSpec {
    let mut groups = Vec::new();
    let flags = flag_items(op);
    if !flags.is_empty() {
        groups.push(TransientGroup {
            label: "Arguments".into(),
            items: flags,
        });
    }
    groups.push(TransientGroup {
        label: "Actions".into(),
        items: vec![action_or_placeholder(
            run_id,
            run_key,
            run_label,
            run_doc,
            placeholder,
        )],
    });
    TransientSpec {
        title: title.into(),
        groups,
        preview: Some(remote_preview(op)),
        footer: Some("q dismiss  BS back".into()),
    }
}

/// The `action:magit-global-*` `CommandId`s [`dispatch_transient`]'s
/// items fire, resolved once at `install()` time (all the names it
/// needs are registered earlier in the same call, by
/// `register_action_commands`) and captured by the
/// `TransientSourceRegistry` builder closure — the registry's
/// builders take only a [`TransientContext`], so capture is how a
/// boot-time-resolved id reaches a spec built long after boot,
/// possibly many times (once per `C-c g` press).
#[derive(Debug, Clone, Copy, Default)]
pub struct DispatchActionIds {
    pub status: Option<CommandId>,
    pub commit: Option<CommandId>,
    pub amend: Option<CommandId>,
    pub log: Option<CommandId>,
    pub diff: Option<CommandId>,
    pub branch: Option<CommandId>,
    /// MG.21d: `M` — remote management, magit's own key.
    pub remote: Option<CommandId>,
    /// MG.23k: `D` — re-run this view with different git arguments.
    pub view_args: Option<CommandId>,
    /// MG.21i: `o` — the submodule list, magit's own key.
    pub submodule: Option<CommandId>,
    /// MG.21g: `B` — bisect. Start is shown only when none is running;
    /// the marks only when one is.
    pub bisect_start: Option<CommandId>,
    pub bisect_good: Option<CommandId>,
    pub bisect_bad: Option<CommandId>,
    pub bisect_skip: Option<CommandId>,
    pub bisect_reset: Option<CommandId>,
    pub stash: Option<CommandId>,
    pub stash_create: Option<CommandId>,
    pub rebase: Option<CommandId>,
    pub fetch: Option<CommandId>,
    pub pull: Option<CommandId>,
    pub push: Option<CommandId>,
    /// MG.23b: magit's `S` / `U` — repo-wide index operations.
    pub stage_all: Option<CommandId>,
    pub unstage_all: Option<CommandId>,
    /// MG.23c1: prompt-backed rows, on magit's own keys.
    pub tag: Option<CommandId>,
    pub gitignore: Option<CommandId>,
    /// MG.23c2.
    pub init: Option<CommandId>,
    pub merge: Option<CommandId>,
    /// MG.23h: the section-acting rows, shown only inside a magit
    /// buffer. `discard` is magit-status's own `x` action, reused
    /// rather than duplicated.
    pub apply_hunk: Option<CommandId>,
    pub reverse_hunk: Option<CommandId>,
    pub discard: Option<CommandId>,
    /// MG.23j: the commit operations, in magit's ungated group. The
    /// same actions the chords fire — they ask for a commit when there
    /// is none under the cursor.
    pub cherry_pick: Option<CommandId>,
    pub revert: Option<CommandId>,
    pub reset_soft: Option<CommandId>,
    pub reset_mixed: Option<CommandId>,
    pub reset_hard: Option<CommandId>,
    /// MG.23h: `magit-status-jump`'s rows, one per section we render.
    pub jump_staged: Option<CommandId>,
    pub jump_unstaged: Option<CommandId>,
    pub jump_untracked: Option<CommandId>,
    pub jump_stashes: Option<CommandId>,
    pub jump_commits: Option<CommandId>,
}

/// An item that fires `id` if resolved, or falls back to a `Flag`
/// placeholder if the action name wasn't found in the registry
/// (shouldn't happen in practice — `register_action_commands` always
/// runs first — but a missing id silently downgrading to "does
/// nothing when toggled" beats a panic or a dangling `CommandId`).
fn action_or_placeholder(
    id: Option<CommandId>,
    key: &str,
    label: &str,
    description: &str,
    placeholder_name: &str,
) -> TransientItem {
    let kind = match id {
        Some(cid) => TransientItemKind::Action(cid),
        None => TransientItemKind::Flag {
            name: placeholder_name.to_string(),
            default: false,
        },
    };
    TransientItem {
        key: vec![key.to_string()],
        label: label.to_string(),
        description: description.to_string(),
        kind,
    }
}

/// MG.23h: the "Applying changes" rows, gated per magit's own
/// `:if-derived magit-mode` — see the call site for the reasoning and
/// for why `s` / `u` are not among them.
fn applying_changes_items(ids: &DispatchActionIds, ctx: &TransientContext) -> Vec<TransientItem> {
    let mut items = Vec::new();
    if ctx.has_minor(crate::MagitCoreMode::mode_id().as_str()) {
        items.push(action_or_placeholder(
            ids.apply_hunk,
            "a",
            "apply",
            "Apply the hunk at cursor to the working tree",
            "apply_hunk",
        ));
        items.push(action_or_placeholder(
            ids.reverse_hunk,
            "-",
            "reverse",
            "Reverse the hunk at cursor out of the working tree",
            "reverse_hunk",
        ));
        items.push(action_or_placeholder(
            ids.discard,
            "x",
            "discard",
            "Discard the hunk or file at cursor (asks first)",
            "discard_at_cursor",
        ));
    }
    items.push(action_or_placeholder(
        ids.stage_all,
        "S",
        "stage all",
        "Stage every tracked modification (git add --update)",
        "stage_all_op",
    ));
    items.push(action_or_placeholder(
        ids.unstage_all,
        "U",
        "unstage all",
        "Unstage everything, keeping your working tree (git reset)",
        "unstage_all_op",
    ));
    items
}

/// MG.23j: the three resets, on the chords' own `s` / `m` / `h`
/// suffixes so `C-c g O h` and the `Oh` chord read the same.
///
/// A submenu rather than three top-level rows because `O` is one
/// concept with three strengths, and because the destructive one wants
/// to sit next to the two that are not — seeing `--soft` and `--mixed`
/// beside it is what makes "keeps your changes" legible at the moment
/// of choosing.
fn reset_transient(ids: &DispatchActionIds) -> TransientSpec {
    TransientSpec {
        title: "Reset".into(),
        groups: vec![TransientGroup {
            label: "Reset to a commit".into(),
            items: vec![
                action_or_placeholder(
                    ids.reset_soft,
                    "s",
                    "soft",
                    "Keep the index and the working tree",
                    "reset_soft_op",
                ),
                action_or_placeholder(
                    ids.reset_mixed,
                    "m",
                    "mixed",
                    "Keep the working tree, reset the index",
                    "reset_mixed_op",
                ),
                action_or_placeholder(
                    ids.reset_hard,
                    "h",
                    "hard",
                    "Discard everything uncommitted (asks first)",
                    "reset_hard_op",
                ),
            ],
        }],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// MG.21g: the `B` bisect submenu, gated on whether a bisect is
/// running.
///
/// **Why the menu is gated rather than showing everything.** `good` /
/// `bad` / `skip` / `reset` outside a bisect are not merely useless —
/// git errors on them, so they would be rows that look actionable and
/// produce a log line. `start` *during* a bisect is the same in
/// reverse. Magit gates this menu for exactly these reasons, and the
/// no-inert-rows policy says the same thing from our side.
///
/// **The gate is a `stat`, not a git call.** This spec is built when
/// `C-c g` is pressed — on the actor thread — so answering "is a
/// bisect running" by spawning `git` would be process-spawn latency on
/// a keystroke path (paramount goal #1). `Bisect::in_progress` reads
/// `.git/BISECT_LOG`, which is the file git itself creates and removes.
/// Discovering the repository is the same `magit_workdir` lookup every
/// magit chord already does.
/// MG.23k: `D` — the arguments the view you are in can be re-run with.
///
/// One menu, whose *content* is chosen by the major mode, because one
/// chord serves what magit splits across `D` (diff) and `L` (log) —
/// `L` is the bottom-of-screen motion here and stays off chords. See
/// `MagitView::argument_flags`.
///
/// A buffer with no arguments gets a menu that says so rather than an
/// empty one: the chord is bound on `magit-core-mode`, so it fires in
/// every magit buffer, and silence would read as a broken key.
pub fn view_arguments_transient(ids: &DispatchActionIds, ctx: &TransientContext) -> TransientSpec {
    let (title, flags) = if ctx.is_major(crate::MagitDiffMode::mode_id().as_str()) {
        ("Diff arguments", crate::magit_diff_mode::DIFF_ARGS)
    } else if ctx.is_major(crate::MagitLogMode::mode_id().as_str()) {
        ("Log arguments", crate::magit_log_mode::LOG_ARGS)
    } else {
        ("Arguments", &[] as &[crate::magit_global_mode::RemoteFlag])
    };

    if flags.is_empty() {
        return TransientSpec {
            title: title.into(),
            groups: vec![TransientGroup {
                label: "This buffer takes no arguments".into(),
                items: Vec::new(),
            }],
            preview: None,
            footer: Some("q dismiss".into()),
        };
    }

    TransientSpec {
        title: title.into(),
        groups: vec![
            TransientGroup {
                label: "Arguments".into(),
                items: flag_items_from(flags),
            },
            TransientGroup {
                label: "Actions".into(),
                items: vec![action_or_placeholder(
                    ids.view_args,
                    "g",
                    "refresh",
                    "Re-run with these arguments",
                    "view_args_op",
                )],
            },
        ],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// Whether a bisect is running in the current repository.
///
/// The one impure part of building this menu, isolated here so
/// [`bisect_transient`] and [`dispatch_transient_with`] stay pure — and
/// so the guards over them cannot depend on whether the *developer's*
/// checkout happens to be mid-bisect while the suite runs, which is
/// exactly the flake a stat inside the builder would have introduced.
pub fn bisect_in_progress() -> bool {
    crate::workdir::magit_workdir()
        .and_then(|wd| lattice_vcs::Repository::discover(wd).ok())
        .map(|repo| lattice_vcs::Bisect::in_progress(&repo))
        .unwrap_or(false)
}

fn bisect_transient(ids: &DispatchActionIds, in_progress: bool) -> TransientSpec {
    let items = if in_progress {
        vec![
            action_or_placeholder(
                ids.bisect_good,
                "g",
                "good",
                "Mark the revision git checked out as good",
                "bisect_good_op",
            ),
            action_or_placeholder(
                ids.bisect_bad,
                "b",
                "bad",
                "Mark the revision git checked out as bad",
                "bisect_bad_op",
            ),
            action_or_placeholder(
                ids.bisect_skip,
                "k",
                "skip",
                "Skip this revision — it cannot be tested",
                "bisect_skip_op",
            ),
            action_or_placeholder(
                ids.bisect_reset,
                "r",
                "reset",
                "End the bisect and return to where it started",
                "bisect_reset_op",
            ),
        ]
    } else {
        vec![action_or_placeholder(
            ids.bisect_start,
            "B",
            "start",
            "Start a bisect — asks for a bad then a good revision",
            "bisect_start_op",
        )]
    };

    TransientSpec {
        title: if in_progress {
            "Bisect (in progress)".into()
        } else {
            "Bisect".into()
        },
        groups: vec![TransientGroup {
            label: "Actions".into(),
            items,
        }],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// MG.23h: the `s` row, which means two different things.
///
/// In magit-status, "open the status buffer" is a no-op on the buffer
/// you are already looking at — so the row becomes the section jump,
/// which is the useful thing to want from a menu there. Everywhere
/// else it opens the buffer.
///
/// This is magit's own shape, on magit's own predicate: its dispatch
/// carries two `j` rows, `magit-status-jump :if-mode magit-status-mode`
/// and `magit-status-quick :if-not-mode magit-status-mode`. Ours keeps
/// the key on `s` (magit leaves `s` empty at this level, so there is
/// nothing to collide with) and swaps the meaning the same way.
fn status_row(ids: &DispatchActionIds, ctx: &TransientContext) -> TransientItem {
    if ctx.is_major(crate::MagitStatusMode::mode_id().as_str()) {
        return TransientItem {
            key: vec!["s".into()],
            label: "jump".into(),
            description: "Jump to a section of this buffer".into(),
            kind: TransientItemKind::Submenu(Arc::new(jump_transient(ids))),
        };
    }
    action_or_placeholder(
        ids.status,
        "s",
        "status",
        "Open the status buffer",
        "status_op",
    )
}

/// MG.23h: magit's `magit-status-jump`, over the sections we render.
///
/// Keys are magit's where the sections coincide (`s` staged, `u`
/// unstaged, `n` untracked, `z` stashes). Recent commits has no magit
/// counterpart — its status buffer reaches unpushed/unpulled instead —
/// so `c` is ours, free at this level and mnemonic.
fn jump_transient(ids: &DispatchActionIds) -> TransientSpec {
    TransientSpec {
        title: "Jump to section".into(),
        groups: vec![TransientGroup {
            label: "Jump to".into(),
            items: vec![
                action_or_placeholder(
                    ids.jump_staged,
                    "s",
                    "staged",
                    "Staged changes",
                    "jump_staged",
                ),
                action_or_placeholder(
                    ids.jump_unstaged,
                    "u",
                    "unstaged",
                    "Unstaged changes",
                    "jump_unstaged",
                ),
                action_or_placeholder(
                    ids.jump_untracked,
                    "n",
                    "untracked",
                    "Untracked files",
                    "jump_untracked",
                ),
                action_or_placeholder(ids.jump_stashes, "z", "stashes", "Stashes", "jump_stashes"),
                action_or_placeholder(
                    ids.jump_commits,
                    "c",
                    "commits",
                    "Recent commits",
                    "jump_commits",
                ),
            ],
        }],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// Build the repo-level dispatch transient (`C-c g`).
///
/// Key assignments follow Emacs magit's own `magit-dispatch` where a
/// corresponding lattice capability exists (`s` status, `c` commit,
/// `d` diff, `l` log, `b` branch, `z` stash, `r` rebase, `f` fetch,
/// `F` pull, `P` push) — muscle memory carries across editors for a
/// menu this central, and every one of these keys means the same
/// thing in magit. Magit entries with no lattice implementation
/// behind them (bisect, submodule, patch) are deliberately ABSENT
/// rather than present-and-inert: a menu row that does nothing when
/// pressed is worse than a row that isn't there.
///
/// MG.23h: `ctx` is where the menu was opened from. Two things vary on
/// it, both mirroring a predicate magit puts on its own dispatch — the
/// `s` row's meaning ([`status_row`]) and the section-acting rows
/// ([`applying_changes_items`]).
pub fn dispatch_transient(ids: &DispatchActionIds, ctx: &TransientContext) -> TransientSpec {
    dispatch_transient_with(ids, ctx, bisect_in_progress())
}

/// [`dispatch_transient`] with the bisect gate supplied rather than
/// probed — pure, and the form every guard over this menu uses.
pub fn dispatch_transient_with(
    ids: &DispatchActionIds,
    ctx: &TransientContext,
    bisect_in_progress: bool,
) -> TransientSpec {
    TransientSpec {
        title: "Magit dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Working tree".into(),
                items: vec![
                    status_row(ids, ctx),
                    action_or_placeholder(
                        ids.diff,
                        "d",
                        "diff",
                        "Diff the working tree against HEAD",
                        "diff_op",
                    ),
                    TransientItem {
                        key: vec!["c".into()],
                        label: "commit".into(),
                        description: "Commit changes".into(),
                        kind: TransientItemKind::Submenu(Arc::new(commit_transient(ids))),
                    },
                ],
            },
            // MG.23b/MG.23h: magit's own "Applying changes" group.
            //
            // The two repo-wide rows are unconditional — `add --update`
            // and `reset` need no target and work from anywhere, so
            // gating them (as magit does) would be strictly less useful.
            // The section-acting rows ARE gated, on the same test
            // magit's `:if-derived magit-mode` makes: they resolve the
            // hunk under the cursor, and outside a magit buffer there is
            // no diff text to find one in.
            //
            // Magit's `s` / `u` rows are deliberately absent. They would
            // collide with the `s` row above, and unlike `a` / `-` / `x`
            // their chords are the first thing anyone reaches for — a
            // menu path to them earns nothing and costs the status key.
            TransientGroup {
                label: "Applying changes".into(),
                items: applying_changes_items(ids, ctx),
            },
            TransientGroup {
                label: "History".into(),
                items: vec![
                    action_or_placeholder(ids.log, "l", "log", "Show commit history", "show_log"),
                    // MG.23j: magit's own keys, in magit's own ungated
                    // group. They need a commit and this menu has no
                    // cursor on one — so the action they fire asks,
                    // which is exactly what magit's `A` / `V` / `X`
                    // transients do and why magit does NOT gate them.
                    //
                    // The same actions the chords fire: in a magit
                    // buffer they take the commit under the cursor, and
                    // everywhere else they open the commit picker. One
                    // action, both surfaces.
                    action_or_placeholder(
                        ids.cherry_pick,
                        "A",
                        "cherry-pick",
                        "Cherry-pick a commit onto this branch",
                        "cherry_pick_op",
                    ),
                    action_or_placeholder(
                        ids.revert,
                        "_",
                        "revert",
                        "Revert a commit",
                        "revert_op",
                    ),
                    TransientItem {
                        key: vec!["O".into()],
                        label: "reset".into(),
                        description: "Reset this branch to a commit".into(),
                        kind: TransientItemKind::Submenu(Arc::new(reset_transient(ids))),
                    },
                    // MG.21g: magit's own key, in magit's own group.
                    // The submenu's contents depend on whether a
                    // bisect is running — see `bisect_transient`.
                    TransientItem {
                        key: vec!["B".into()],
                        label: "bisect".into(),
                        description: "Find the commit that introduced a bug".into(),
                        kind: TransientItemKind::Submenu(Arc::new(bisect_transient(
                            ids,
                            bisect_in_progress,
                        ))),
                    },
                ],
            },
            TransientGroup {
                label: "Branches".into(),
                items: vec![action_or_placeholder(
                    ids.branch,
                    "b",
                    "branch",
                    "Open the branch list",
                    "branch_op",
                )],
            },
            TransientGroup {
                label: "Stashing".into(),
                items: vec![TransientItem {
                    key: vec!["z".into()],
                    label: "stash".into(),
                    description: "Stash operations".into(),
                    kind: TransientItemKind::Submenu(Arc::new(stash_transient(ids))),
                }],
            },
            TransientGroup {
                label: "Remotes".into(),
                items: vec![
                    TransientItem {
                        key: vec!["f".into()],
                        label: "fetch".into(),
                        description: "Fetch from the remote without merging".into(),
                        kind: TransientItemKind::Submenu(Arc::new(remote_op_transient(
                            "Fetch",
                            RemoteOp::FETCH,
                            "f",
                            ids.fetch,
                            "fetch",
                            "Run the fetch",
                            "fetch_op",
                        ))),
                    },
                    // Pull has no flags today (`--ff-only` is not
                    // optional — a magit pull that could create a merge
                    // commit behind your back is the wrong default), so
                    // it stays a direct action rather than gaining a
                    // submenu with nothing in it.
                    action_or_placeholder(
                        ids.pull,
                        "F",
                        "pull",
                        "Fetch + fast-forward merge from the remote",
                        "pull_op",
                    ),
                    TransientItem {
                        key: vec!["P".into()],
                        label: "push".into(),
                        description: "Push to the remote".into(),
                        kind: TransientItemKind::Submenu(Arc::new(remote_op_transient(
                            "Push",
                            RemoteOp::PUSH,
                            "P",
                            ids.push,
                            "push",
                            "Run the push",
                            "push_op",
                        ))),
                    },
                    // MG.21d: magit's `M`. Not a submenu — the row
                    // opens the remote list buffer, because the URLs
                    // are the point and a menu cannot show them. `M`
                    // costs nothing here: transient keys do not shadow
                    // the vim grammar, which is also why `M` stays
                    // unbound as a chord inside magit buffers.
                    action_or_placeholder(
                        ids.remote,
                        "M",
                        "remote",
                        "Manage remotes — add, rename, remove, set URL, prune",
                        "remote_manage_op",
                    ),
                    // MG.21i: magit's `o`. Like `M`, a buffer rather
                    // than a submenu — and here magit agrees, since
                    // `magit-list-submodules` is a buffer there too.
                    action_or_placeholder(
                        ids.submodule,
                        "o",
                        "submodule",
                        "Manage submodules — add, update, sync, remove",
                        "submodule_op",
                    ),
                ],
            },
            TransientGroup {
                label: "Misc".into(),
                items: vec![
                    action_or_placeholder(
                        ids.rebase,
                        "r",
                        "rebase",
                        "Start an interactive rebase",
                        "rebase_op",
                    ),
                    // MG.23c1: magit's own keys. Both ask for their one
                    // value rather than taking it from context — there
                    // is nothing at a cursor to read from a menu opened
                    // anywhere.
                    action_or_placeholder(
                        ids.tag,
                        "t",
                        "tag",
                        "Tag HEAD with a name you type",
                        "tag_op",
                    ),
                    action_or_placeholder(
                        ids.gitignore,
                        "i",
                        "gitignore",
                        "Add a pattern to .gitignore",
                        "gitignore_op",
                    ),
                    // MG.23c2. `m` is the repo-level convenience for
                    // when you know the branch name; picking from a
                    // list is already served one level down, by `m` in
                    // the branch buffer.
                    action_or_placeholder(
                        ids.merge,
                        "m",
                        "merge",
                        "Merge a branch you name into the current one",
                        "merge_op",
                    ),
                    action_or_placeholder(
                        ids.init,
                        "I",
                        "init",
                        "Initialize a git repository",
                        "init_op",
                    ),
                ],
            },
        ],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// Build a commit sub-transient. `c c` / `c a` mirror magit-status's
/// own `cc` / `ca` chords exactly, so the same two keystrokes commit
/// and amend whether you're inside the status buffer or reaching for
/// the dispatch menu from an ordinary file.
fn commit_transient(ids: &DispatchActionIds) -> TransientSpec {
    TransientSpec {
        title: "Commit".into(),
        groups: vec![TransientGroup {
            label: "Actions".into(),
            items: vec![
                action_or_placeholder(
                    ids.commit,
                    "c",
                    "commit",
                    "Open the commit buffer",
                    "do_commit",
                ),
                action_or_placeholder(
                    ids.amend,
                    "a",
                    "amend",
                    "Amend the previous commit",
                    "do_amend",
                ),
            ],
        }],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// Build a stash sub-transient. `z l` lists, `z z` pushes a new
/// stash — magit uses `z z` for "stash" (push) too, and apply/pop/
/// drop live as `a`/`p`/`d` chords inside the stash-list buffer
/// itself rather than being duplicated here (they need a stash
/// selected, which only the list view provides).
fn stash_transient(ids: &DispatchActionIds) -> TransientSpec {
    TransientSpec {
        title: "Stash".into(),
        groups: vec![
            // MG.17a: `-u` rides here rather than in a further submenu —
            // this menu already exists and already stays open, so the
            // flag costs no extra keystroke.
            TransientGroup {
                label: "Arguments".into(),
                items: flag_items(RemoteOp::STASH),
            },
            TransientGroup {
                label: "Actions".into(),
                items: vec![
                    action_or_placeholder(
                        ids.stash_create,
                        "z",
                        "stash",
                        "Stash the working tree (git stash push)",
                        "stash_push",
                    ),
                    action_or_placeholder(
                        ids.stash,
                        "l",
                        "list",
                        "Open the stash list",
                        "stash_op",
                    ),
                ],
            },
        ],
        // The preview describes the stash-push the `z` key runs; `l`
        // just opens the list and ignores the flag.
        preview: Some(remote_preview(RemoteOp::STASH)),
        footer: Some("q dismiss  BS back".into()),
    }
}

/// The `action:magit-global-file-*` `CommandId`s
/// [`file_dispatch_transient`]'s items fire — resolved once at
/// `install()` time, same shape as [`DispatchActionIds`].
#[derive(Debug, Clone, Copy, Default)]
pub struct FileDispatchActionIds {
    pub stage: Option<CommandId>,
    pub unstage: Option<CommandId>,
    pub discard: Option<CommandId>,
    pub diff: Option<CommandId>,
    pub log: Option<CommandId>,
    pub blame: Option<CommandId>,
    /// MG.23f2: reverse blame. Only in `C-c f`, not in the other-file
    /// menu — that menu names a target by *path*, and reverse blame
    /// needs a revision the path cannot carry.
    pub blame_reverse: Option<CommandId>,
    /// MG.28: `v` — this file at a revision you name.
    pub at_revision: Option<CommandId>,
    /// MG.23d: the file operations.
    pub untrack: Option<CommandId>,
    pub delete: Option<CommandId>,
    pub rename: Option<CommandId>,
    /// MG.23d2: check the file out from a revision.
    pub checkout: Option<CommandId>,
}

/// Build the file-level dispatch transient (`C-c f`).
///
/// Items resolve to real actions that operate on the buffer that was
/// active when the transient was opened (`ActionContext::buffer_id`
/// at fire time) — see
/// `magit_global_mode::global_action_handler_contributions` for the
/// file-path resolution. Unlike the root dispatch, there is
/// no per-file `SectionIndex`-cursor resolution when opened from
/// inside a magit-status buffer; the file is always "whichever real
/// buffer was active", which covers the common case (editing a file,
/// pressing `C-c f` to stage/diff it) but not "invoke from within
/// magit-status, act on the entry at cursor".
/// Key assignments follow Emacs magit's own `magit-file-dispatch`
/// (`s` stage, `u` unstage, `x` discard, `d` diff, `l` log, `b`
/// blame) — the same reasoning as [`dispatch_transient`]'s. Magit
/// entries with no lattice implementation (stage-all/unstage-all,
/// edit-blob, trace-definition, commit-fixup) are absent rather than
/// inert.
/// MG.23a: `:magit-other-file-dispatch` — the file menu for a file you
/// are **not** visiting.
///
/// A stand-alone command, tied to no buffer: invoke it from anywhere,
/// set the target with `=f`, then act. Deliberately bound to no chord —
/// `C-c f` is the common case (act on what you are looking at) and this
/// is the occasional one; bind it yourself if you prefer magit's
/// always-ask behaviour.
///
/// Same rows and the same actions as [`file_dispatch_transient`]; the
/// only difference is the `file` argument, which those actions read in
/// preference to the visited file. With the argument unset every row
/// falls back to the visited file, so an unset menu is a superset of
/// `C-c f` rather than something that acts wrongly — and the preview
/// line always names the target that will be used.
///
/// **Discard works here as of IX.7.** It is destructive, so it goes
/// through §12.13's ask/execute pair — and `Effect::Confirm` opens a
/// transient of its own, which replaces this menu and its state. That
/// used to lose the target: the execute half found no `file` argument
/// and fell back to the visited file, asking about one file and
/// deleting another's changes. IX.1 made the confirm carry its target
/// and IX.2 made the execute half read it, so the dialog replacing this
/// menu no longer matters.
pub fn other_file_dispatch_transient(ids: &FileDispatchActionIds) -> TransientSpec {
    TransientSpec {
        title: "File dispatch (other file)".into(),
        groups: vec![
            TransientGroup {
                label: "Target".into(),
                items: vec![TransientItem {
                    key: vec!["=f".into()],
                    label: "file".into(),
                    description: "Repo-relative path to act on".into(),
                    kind: TransientItemKind::Argument {
                        name: "file".to_string(),
                        prompt: "File (repo-relative): ".to_string(),
                        default: None,
                    },
                }],
            },
            TransientGroup {
                label: "Stage".into(),
                items: vec![
                    action_or_placeholder(
                        ids.stage,
                        "s",
                        "stage",
                        "Stage the target file",
                        "stage_other_file",
                    ),
                    action_or_placeholder(
                        ids.unstage,
                        "u",
                        "unstage",
                        "Unstage the target file",
                        "unstage_other_file",
                    ),
                    // IX.7: destructive, and safe here now. Its confirm
                    // carries the target (IX.1/IX.2), so the dialog
                    // replacing this menu no longer loses it — before
                    // that, the execute half would have fallen back to
                    // the visited file and acted on something the prompt
                    // never named.
                    action_or_placeholder(
                        ids.discard,
                        "x",
                        "discard",
                        "Discard the target file's changes (asks first)",
                        "discard_other_file",
                    ),
                ],
            },
            TransientGroup {
                label: "Inspect".into(),
                items: vec![
                    action_or_placeholder(
                        ids.diff,
                        "d",
                        "diff",
                        "Show the target file's diff",
                        "diff_other_file",
                    ),
                    action_or_placeholder(
                        ids.log,
                        "l",
                        "log",
                        "Show the target file's history",
                        "log_other_file",
                    ),
                    action_or_placeholder(
                        ids.blame,
                        "b",
                        "blame",
                        "Blame the target file",
                        "blame_other_file",
                    ),
                ],
            },
        ],
        // The target is the whole point of this menu, so it is always on
        // screen — including when unset, where saying so beats leaving
        // the user to guess which file a row will hit.
        preview: Some(Box::new(
            |state: &lattice_picker::TransientState| match state.get("file") {
                Some(lattice_picker::TransientValue::String(p)) if !p.is_empty() => {
                    format!("target: {p}")
                }
                _ => "target: (none set — rows act on the visited file)".to_string(),
            },
        )),
        footer: Some("=f set target  q dismiss".into()),
    }
}

pub fn file_dispatch_transient(ids: &FileDispatchActionIds) -> TransientSpec {
    TransientSpec {
        title: "File dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Stage".into(),
                items: vec![
                    action_or_placeholder(ids.stage, "s", "stage", "Stage this file", "stage_file"),
                    action_or_placeholder(
                        ids.unstage,
                        "u",
                        "unstage",
                        "Unstage this file",
                        "unstage_file",
                    ),
                    action_or_placeholder(
                        ids.discard,
                        "x",
                        "discard",
                        "Discard this file's working-tree changes (asks first)",
                        "discard_file",
                    ),
                ],
            },
            // MG.23d. Magit puts these behind a `,` prefix in its own
            // file-dispatch, which is a deliberate signal rather than a
            // key shortage: they change what the file IS, not just what
            // is staged of it. Keeping the prefix keeps that signal —
            // and keeps `r` free for the blame-removal row magit also
            // has at this level.
            TransientGroup {
                label: "File".into(),
                items: vec![
                    action_or_placeholder(
                        ids.untrack,
                        ",x",
                        "untrack",
                        "Stop tracking this file, keeping it on disk",
                        "untrack_file",
                    ),
                    action_or_placeholder(
                        ids.rename,
                        ",r",
                        "rename",
                        "Rename this file (asks for the new name)",
                        "rename_file",
                    ),
                    action_or_placeholder(
                        ids.delete,
                        ",k",
                        "delete",
                        "Delete this file (asks first)",
                        "delete_file",
                    ),
                    action_or_placeholder(
                        ids.checkout,
                        ",c",
                        "checkout",
                        "Replace this file with its content at a revision (asks, then confirms)",
                        "checkout_file",
                    ),
                ],
            },
            TransientGroup {
                label: "Inspect".into(),
                items: vec![
                    action_or_placeholder(
                        ids.diff,
                        "d",
                        "diff",
                        "Show diff for this file",
                        "diff_file",
                    ),
                    action_or_placeholder(
                        ids.log,
                        "l",
                        "log",
                        "Show commit history for this file",
                        "log_file",
                    ),
                    action_or_placeholder(ids.blame, "b", "blame", "Blame this file", "blame_file"),
                    // MG.23f2, on magit's own key for it (`f`
                    // "...reverse" in magit-file-dispatch's Blame
                    // group). Only meaningful from a blob buffer; the
                    // handler says so rather than the row hiding, since
                    // there is no per-context menu content yet
                    // (MG.23h).
                    // MG.28: magit's own key for this.
                    action_or_placeholder(
                        ids.at_revision,
                        "v",
                        "view at revision",
                        "Open this file as it was at a revision you name",
                        "file_at_revision",
                    ),
                    action_or_placeholder(
                        ids.blame_reverse,
                        "f",
                        "reverse blame",
                        "For each line of this revision, the last commit it existed in",
                        "blame_reverse_file",
                    ),
                ],
            },
        ],
        preview: None,
        footer: Some("q dismiss".into()),
    }
}
