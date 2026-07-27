//! MG.8: magit transient menu definitions.
//!
//! Defines the `TransientSpec` instances for the repo-level
//! dispatch (C-c g) and file-level dispatch (C-c f) menus.
//! Each is a grouped action menu rendered by the PICK.1
//! transient picker overlay.

use std::sync::Arc;

use lattice_picker::{TransientGroup, TransientItem, TransientItemKind, TransientSpec};
use lattice_protocol::ids::CommandId;

/// The `action:magit-global-*` `CommandId`s [`dispatch_transient`]'s
/// items fire, resolved once at `install()` time (all the names it
/// needs are registered earlier in the same call, by
/// `register_action_commands`) and captured by the
/// `TransientSourceRegistry` builder closure — the registry's
/// `Fn() -> TransientSpec` builders take no arguments, so this is
/// how a boot-time-resolved id reaches a spec built long after boot,
/// possibly many times (once per `C-c g` press).
#[derive(Debug, Clone, Copy, Default)]
pub struct DispatchActionIds {
    pub status: Option<CommandId>,
    pub commit: Option<CommandId>,
    pub amend: Option<CommandId>,
    pub log: Option<CommandId>,
    pub diff: Option<CommandId>,
    pub branch: Option<CommandId>,
    pub stash: Option<CommandId>,
    pub stash_create: Option<CommandId>,
    pub rebase: Option<CommandId>,
    pub fetch: Option<CommandId>,
    pub pull: Option<CommandId>,
    pub push: Option<CommandId>,
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

/// Build the repo-level dispatch transient (`C-c g`).
///
/// Key assignments follow Emacs magit's own `magit-dispatch` where a
/// corresponding lattice capability exists (`s` status, `c` commit,
/// `d` diff, `l` log, `b` branch, `z` stash, `r` rebase, `f` fetch,
/// `F` pull, `P` push) — muscle memory carries across editors for a
/// menu this central, and every one of these keys means the same
/// thing in magit. Magit entries with no lattice implementation
/// behind them (bisect, merge, tag, revert, reset, cherry-pick,
/// submodule, patch) are deliberately ABSENT rather than present-
/// and-inert: a menu row that does nothing when pressed is worse
/// than a row that isn't there.
pub fn dispatch_transient(ids: &DispatchActionIds) -> TransientSpec {
    TransientSpec {
        title: "Magit dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Working tree".into(),
                items: vec![
                    action_or_placeholder(
                        ids.status,
                        "s",
                        "status",
                        "Open the status buffer",
                        "stage_all",
                    ),
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
            TransientGroup {
                label: "History".into(),
                items: vec![action_or_placeholder(
                    ids.log,
                    "l",
                    "log",
                    "Show commit history",
                    "show_log",
                )],
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
                    action_or_placeholder(
                        ids.fetch,
                        "f",
                        "fetch",
                        "Fetch from the remote without merging",
                        "fetch_op",
                    ),
                    action_or_placeholder(
                        ids.pull,
                        "F",
                        "pull",
                        "Fetch + fast-forward merge from the remote",
                        "pull_op",
                    ),
                    action_or_placeholder(ids.push, "P", "push", "Push to the remote", "push_op"),
                ],
            },
            TransientGroup {
                label: "Misc".into(),
                items: vec![action_or_placeholder(
                    ids.rebase,
                    "r",
                    "rebase",
                    "Start an interactive rebase",
                    "rebase_op",
                )],
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
        groups: vec![TransientGroup {
            label: "Actions".into(),
            items: vec![
                action_or_placeholder(
                    ids.stash_create,
                    "z",
                    "stash",
                    "Stash the working tree (git stash push)",
                    "stash_push",
                ),
                action_or_placeholder(ids.stash, "l", "list", "Open the stash list", "stash_op"),
            ],
        }],
        preview: None,
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
                ],
            },
        ],
        preview: None,
        footer: Some("q dismiss".into()),
    }
}
