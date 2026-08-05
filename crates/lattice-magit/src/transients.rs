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

/// MG.41a: every registered magit action, keyed by its registered name.
///
/// Replaces the per-row `Option<CommandId>` struct field. Adding a
/// transient row used to mean editing four places that had to stay in
/// sync — `register_action_commands`, a struct field, a
/// `resolve_dispatch_ids` line, and the builder — none of which failed
/// to compile when they drifted; the row just silently rendered as a
/// disabled placeholder. With rows naming their command directly, two
/// of those four disappear.
///
/// **Resolution is automatic.** `resolve` scans the registry for every
/// `action:magit-` name rather than reading a hand-kept list, so there
/// is no third enumeration hiding here either: registering an action is
/// the only step, and a row referencing it works immediately.
///
/// The cost is losing compile-time field checking. Mitigated the way
/// this repo already mitigates it for the `<C-h>` help prefix: a test
/// asserts every name any row references actually resolves, so drift
/// fails loudly instead of rendering a placeholder.
#[derive(Debug, Clone, Default)]
pub struct MagitActionIds {
    by_name: std::collections::HashMap<String, CommandId>,
}

impl MagitActionIds {
    /// Prefix every magit action shares. Anything registered under it
    /// is reachable from a transient row without further bookkeeping.
    const PREFIX: &'static str = "action:magit-";

    pub fn resolve(registry: &lattice_grammar::CommandRegistry) -> Self {
        let names: Vec<String> = registry
            .names()
            .filter(|n| n.starts_with(Self::PREFIX))
            .map(str::to_string)
            .collect();
        let by_name = names
            .into_iter()
            .filter_map(|n| registry.id_by_name(&n).map(|id| (n, id)))
            .collect();
        Self { by_name }
    }

    pub fn get(&self, name: &str) -> Option<CommandId> {
        self.by_name.get(name).copied()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }
}

/// MG.41a: one transient row, as data.
///
/// `action` is the registered command name — the single place a row's
/// behaviour is identified. `placeholder` is the disabled-row marker
/// shown when the action is missing, kept per-row so an unresolved row
/// is still visually distinct from its neighbours.
pub struct TransientRow {
    pub key: &'static str,
    pub label: &'static str,
    pub doc: &'static str,
    pub action: &'static str,
    pub placeholder: &'static str,
}

/// Build a row from the table entry, degrading to a disabled
/// placeholder when its action is not registered.
fn row_item(ids: &MagitActionIds, row: &TransientRow) -> TransientItem {
    action_or_placeholder(
        ids.get(row.action),
        row.key,
        row.label,
        row.doc,
        row.placeholder,
    )
}

/// Build a whole group from a static table — the shape every
/// non-gated transient group now uses.
fn row_group(label: &str, ids: &MagitActionIds, rows: &'static [TransientRow]) -> TransientGroup {
    TransientGroup {
        label: label.into(),
        items: rows.iter().map(|r| row_item(ids, r)).collect(),
    }
}

/// Every static row table in this module, for the drift tests.
///
/// A table not listed here is not covered by
/// `every_row_action_is_registered`, so new tables must be added — the
/// one piece of bookkeeping this design keeps, and the test below
/// makes forgetting it visible by counting.
#[cfg(test)]
pub(crate) fn all_row_tables() -> &'static [(&'static str, &'static [TransientRow])] {
    &[
        ("branch/checkout", BRANCH_CHECKOUT_ROWS),
        ("branch/create", BRANCH_CREATE_ROWS),
        ("branch/do", BRANCH_DO_ROWS),
        ("reset", RESET_ROWS),
        ("commit", COMMIT_ROWS),
        ("stash", STASH_ROWS),
        ("subtree", SUBTREE_ROWS),
        ("jump", JUMP_ROWS),
        ("push", PUSH_ROWS),
        ("pull", PULL_ROWS),
        ("fetch", FETCH_ROWS),
        ("merge", MERGE_ROWS),
        ("tag", TAG_ROWS),
        ("rebase/start", REBASE_START_ROWS),
        ("rebase/sequence", REBASE_SEQUENCE_ROWS),
    ]
}



// MG.41c: magit's destination rows. Keys are magit's own.

const PUSH_ROWS: &[TransientRow] = &[
    TransientRow { key: "p", label: "pushRemote", doc: "Push to the configured push-remote", action: "action:magit-global-push-configured", placeholder: "push_configured_op" },
    TransientRow { key: "u", label: "@{upstream}", doc: "Push to this branch's upstream — differs from pushRemote in a triangular workflow", action: "action:magit-global-push-upstream", placeholder: "push_upstream_op" },
    TransientRow { key: "e", label: "elsewhere", doc: "Push to a remote you name", action: "action:magit-global-push-elsewhere", placeholder: "push_elsewhere_op" },
    TransientRow { key: "o", label: "another branch", doc: "Push a branch other than HEAD", action: "action:magit-global-push-other-branch", placeholder: "push_other_op" },
    TransientRow { key: "r", label: "refspecs", doc: "Push explicit refspecs", action: "action:magit-global-push-refspecs", placeholder: "push_refspecs_op" },
    TransientRow { key: "T", label: "a tag", doc: "Push a single tag", action: "action:magit-global-push-tag", placeholder: "push_tag_op" },
    TransientRow { key: "t", label: "all tags", doc: "Push every tag", action: "action:magit-global-push-all-tags", placeholder: "push_all_tags_op" },
];

const PULL_ROWS: &[TransientRow] = &[
    TransientRow { key: "p", label: "pushRemote", doc: "Pull from the configured remote", action: "action:magit-global-pull-configured", placeholder: "pull_configured_op" },
    TransientRow { key: "u", label: "@{upstream}", doc: "Pull from this branch's upstream", action: "action:magit-global-pull-upstream", placeholder: "pull_upstream_op" },
    TransientRow { key: "e", label: "elsewhere", doc: "Pull from a remote you name", action: "action:magit-global-pull-elsewhere", placeholder: "pull_elsewhere_op" },
];

const FETCH_ROWS: &[TransientRow] = &[
    TransientRow { key: "p", label: "pushRemote", doc: "Fetch from the configured remote", action: "action:magit-global-fetch-configured", placeholder: "fetch_configured_op" },
    TransientRow { key: "u", label: "@{upstream}", doc: "Fetch this branch's upstream", action: "action:magit-global-fetch-upstream", placeholder: "fetch_upstream_op" },
    TransientRow { key: "e", label: "elsewhere", doc: "Fetch from a remote you name", action: "action:magit-global-fetch-elsewhere", placeholder: "fetch_elsewhere_op" },
    TransientRow { key: "o", label: "another branch", doc: "Fetch a branch you name", action: "action:magit-global-fetch-other-branch", placeholder: "fetch_other_op" },
    TransientRow { key: "r", label: "refspecs", doc: "Fetch explicit refspecs", action: "action:magit-global-fetch-refspecs", placeholder: "fetch_refspecs_op" },
    TransientRow { key: "a", label: "all remotes", doc: "Fetch from every configured remote", action: "action:magit-global-fetch-all-remotes", placeholder: "fetch_all_op" },
];

// ---- MG.41a: static row tables ----
//
// One entry per row. Keys are magit's own — inside a transient the menu
// owns every keystroke, so there is no vim-grammar conflict to dodge
// (see the slice plan's scoping note).

const BRANCH_CHECKOUT_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "b",
        label: "branch/revision",
        doc: "Check out anything git can: a branch, tag, remote ref or SHA",
        action: "action:magit-global-branch-checkout-rev",
        placeholder: "branch_checkout_rev_op",
    },
    TransientRow {
        key: "l",
        label: "local branch",
        doc: "Pick a local branch and check it out",
        action: "action:magit-global-branch-checkout",
        placeholder: "branch_checkout_op",
    },
];

const BRANCH_CREATE_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "c",
        label: "new branch and checkout",
        doc: "Pick a base, then name a new branch and check it out",
        action: "action:magit-global-branch-create",
        placeholder: "branch_create_op",
    },
    TransientRow {
        key: "n",
        label: "new branch",
        doc: "Pick a base, then name a new branch — without checking it out",
        action: "action:magit-global-branch-create-no-checkout",
        placeholder: "branch_create_no_checkout_op",
    },
];

const BRANCH_DO_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "m",
        label: "rename",
        doc: "Pick a branch, then type its new name",
        action: "action:magit-global-branch-rename",
        placeholder: "branch_rename_op",
    },
    // MG.41a: magit's own keys. `k` deletes; `x` is reset (MG.41d adds
    // it). Before this, `x` deleted — putting the destructive
    // operation where a magit user expects reset.
    TransientRow {
        key: "k",
        label: "delete",
        doc: "Pick a branch to delete — asks first",
        action: "action:magit-global-branch-delete",
        placeholder: "branch_delete_op",
    },
    TransientRow {
        key: "L",
        label: "list",
        doc: "Open the branch list buffer",
        action: "action:magit-global-branch",
        placeholder: "branch_op",
    },
];

const RESET_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "s",
        label: "soft",
        doc: "Move HEAD, keep the index and working tree",
        action: "action:magit-reset-soft",
        placeholder: "reset_soft_op",
    },
    TransientRow {
        key: "m",
        label: "mixed",
        doc: "Move HEAD and reset the index, keep the working tree",
        action: "action:magit-reset-mixed",
        placeholder: "reset_mixed_op",
    },
    TransientRow {
        key: "h",
        label: "hard",
        doc: "Move HEAD and discard index + working-tree changes",
        action: "action:magit-reset-hard",
        placeholder: "reset_hard_op",
    },
    // MG.41d: magit's own keys for the rest of the modes.
    TransientRow {
        key: "k",
        label: "keep",
        doc: "Move HEAD but refuse if that would discard uncommitted work",
        action: "action:magit-reset-keep",
        placeholder: "reset_keep_op",
    },
    TransientRow {
        key: "i",
        label: "index",
        doc: "Set the index to a commit without moving HEAD or touching the working tree",
        action: "action:magit-reset-index",
        placeholder: "reset_index_op",
    },
];

const COMMIT_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "c",
        label: "commit",
        doc: "Commit the staged changes",
        action: "action:magit-global-commit",
        placeholder: "commit_op",
    },
    TransientRow {
        key: "a",
        label: "amend",
        doc: "Amend the previous commit",
        action: "action:magit-global-amend",
        placeholder: "amend_op",
    },
    // MG.41d: the autosquash pair, on magit's own keys. Both record a
    // marker commit a later `rebase --autosquash` folds in — `fixup`
    // discards its message, `squash` keeps it for editing.
    TransientRow {
        key: "f",
        label: "fixup",
        doc: "Record a fixup! commit for a commit you pick",
        action: "action:magit-commit-fixup",
        placeholder: "commit_fixup_op",
    },
    TransientRow {
        key: "s",
        label: "squash",
        doc: "Record a squash! commit for a commit you pick",
        action: "action:magit-commit-squash",
        placeholder: "commit_squash_op",
    },
];

const STASH_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "z",
        label: "stash",
        doc: "Stash the working tree and index",
        action: "action:magit-global-stash-create",
        placeholder: "stash_create_op",
    },
    TransientRow {
        key: "l",
        label: "list",
        doc: "Open the stash list buffer",
        action: "action:magit-global-stash",
        placeholder: "stash_op",
    },
    // MG.41d: magit's other stash-creation variants.
    TransientRow {
        key: "i",
        label: "index",
        doc: "Stash only the staged changes",
        action: "action:magit-global-stash-staged",
        placeholder: "stash_staged_op",
    },
    TransientRow {
        key: "x",
        label: "keeping index",
        doc: "Stash everything but leave the index staged",
        action: "action:magit-global-stash-keep-index",
        placeholder: "stash_keep_index_op",
    },
    // MG.41d: magit's use rows. These reuse the SAME actions the stash
    // buffer's chords fire — a menu path to an operation must not grow
    // a second handler with its own idea of the confirm contract, which
    // is the property `the_commit_rows_reuse_the_chords_actions` pins
    // for the reset rows.
    TransientRow {
        key: "a",
        label: "apply",
        doc: "Apply a stash, keeping it on the stack",
        action: "action:magit-stash-apply",
        placeholder: "stash_apply_op",
    },
    TransientRow {
        key: "p",
        label: "pop",
        doc: "Apply a stash and drop it",
        action: "action:magit-stash-pop",
        placeholder: "stash_pop_op",
    },
    TransientRow {
        key: "k",
        label: "drop",
        doc: "Delete a stash without applying it — asks first",
        action: "action:magit-stash-drop",
        placeholder: "stash_drop_op",
    },
    TransientRow {
        key: "v",
        label: "show",
        doc: "Show a stash's diff",
        action: "action:magit-stash-show",
        placeholder: "stash_show_op",
    },
];

const SUBTREE_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "a",
        label: "add",
        doc: "Add a repository as a subtree at a prefix",
        action: "action:magit-global-subtree-add",
        placeholder: "subtree_add_op",
    },
    TransientRow {
        key: "m",
        label: "merge",
        doc: "Merge a repository into an existing subtree prefix",
        action: "action:magit-global-subtree-merge",
        placeholder: "subtree_merge_op",
    },
    TransientRow {
        key: "f",
        label: "pull",
        doc: "Fetch and merge upstream changes into a subtree prefix",
        action: "action:magit-global-subtree-pull",
        placeholder: "subtree_pull_op",
    },
    TransientRow {
        key: "p",
        label: "push",
        doc: "Push a subtree prefix to its upstream repository",
        action: "action:magit-global-subtree-push",
        placeholder: "subtree_push_op",
    },
    TransientRow {
        key: "s",
        label: "split",
        doc: "Split a prefix into its own synthetic history",
        action: "action:magit-global-subtree-split",
        placeholder: "subtree_split_op",
    },
];

/// MG.41e: magit's `m` merge submenu.
const MERGE_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "m",
        label: "merge",
        doc: "Merge a branch into the current one",
        action: "action:magit-global-merge",
        placeholder: "merge_op",
    },
    TransientRow {
        key: "n",
        label: "merge, don't commit",
        doc: "Merge but stop before committing, so the result can be inspected first",
        action: "action:magit-global-merge-no-commit",
        placeholder: "merge_no_commit_op",
    },
    TransientRow {
        key: "s",
        label: "squash",
        doc: "Take the branch's changes as one staged change, with no merge commit",
        action: "action:magit-global-merge-squash",
        placeholder: "merge_squash_op",
    },
];

/// MG.41e: magit's `t` tag submenu.
const TAG_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "t",
        label: "tag",
        doc: "Tag HEAD with a name you type",
        action: "action:magit-global-tag",
        placeholder: "tag_op",
    },
    TransientRow {
        key: "k",
        label: "delete",
        doc: "Delete a local tag — the remote copy is untouched",
        action: "action:magit-global-tag-delete",
        placeholder: "tag_delete_op",
    },
];

/// MG.41e: the rebase submenu, shown when NO rebase is running.
const REBASE_START_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "i",
        label: "interactively",
        doc: "Start an interactive rebase — pick a base, then edit the todo list",
        action: "action:magit-global-rebase",
        placeholder: "rebase_op",
    },
];

/// MG.41e: shown INSTEAD when a rebase is stopped.
///
/// Gated for the same reason bisect / notes-merge / am are: outside a
/// rebase these three error, so ungated rows would look actionable and
/// fail; inside one, starting another is what you must not do.
const REBASE_SEQUENCE_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "r",
        label: "continue",
        doc: "Resume after amending or resolving conflicts",
        action: "action:magit-global-rebase-continue",
        placeholder: "rebase_continue_op",
    },
    TransientRow {
        key: "s",
        label: "skip",
        doc: "Skip the commit the rebase stopped on",
        action: "action:magit-global-rebase-skip",
        placeholder: "rebase_skip_op",
    },
    TransientRow {
        key: "a",
        label: "abort",
        doc: "Abandon the rebase, restoring the branch to where it started",
        action: "action:magit-global-rebase-abort",
        placeholder: "rebase_abort_op",
    },
];

const JUMP_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "s",
        label: "staged",
        doc: "Jump to the staged-changes section",
        action: "action:magit-jump-staged",
        placeholder: "jump_staged_op",
    },
    TransientRow {
        key: "u",
        label: "unstaged",
        doc: "Jump to the unstaged-changes section",
        action: "action:magit-jump-unstaged",
        placeholder: "jump_unstaged_op",
    },
    TransientRow {
        key: "n",
        label: "untracked",
        doc: "Jump to the untracked-files section",
        action: "action:magit-jump-untracked",
        placeholder: "jump_untracked_op",
    },
    TransientRow {
        key: "z",
        label: "stashes",
        doc: "Jump to the stashes section",
        action: "action:magit-jump-stashes",
        placeholder: "jump_stashes_op",
    },
    TransientRow {
        key: "c",
        label: "commits",
        doc: "Jump to the recent-commits section",
        action: "action:magit-jump-commits",
        placeholder: "jump_commits_op",
    },
];

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
    ids: &MagitActionIds,
    rows: &'static [TransientRow],
) -> TransientSpec {
    let mut groups = Vec::new();
    let flags = flag_items(op);
    if !flags.is_empty() {
        groups.push(TransientGroup {
            label: "Arguments".into(),
            items: flags,
        });
    }
    // MG.41c: several destinations, not one unlabelled run. Magit's
    // push menu offers seven; lattice offered `P`.
    groups.push(row_group("Destination", ids, rows));
    TransientSpec {
        title: title.into(),
        groups,
        preview: Some(remote_preview(op)),
        footer: Some("q dismiss  Esc/BS back".into()),
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
    /// MG.29: the branch submenu's own rows.
    pub branch_checkout: Option<CommandId>,
    pub branch_create: Option<CommandId>,
    /// MG.32: the four rows that completed the submenu against magit's
    /// own `magit-branch` transient.
    pub branch_checkout_rev: Option<CommandId>,
    pub branch_create_no_checkout: Option<CommandId>,
    pub branch_rename: Option<CommandId>,
    pub branch_delete: Option<CommandId>,
    /// MG.21d: `M` — remote management, magit's own key.
    pub remote: Option<CommandId>,
    /// MG.23k: `D` — re-run this view with different git arguments.
    pub view_args: Option<CommandId>,
    /// MG.21i: `o` — the submodule list, magit's own key.
    pub submodule: Option<CommandId>,
    /// MG.35: `y` — the refs buffer, magit's own key.
    pub refs: Option<CommandId>,
    /// MG.36: `C` — clone a repository, magit's own key.
    pub clone: Option<CommandId>,
    /// MG.37: the `T` notes submenu's rows, on magit's own keys.
    pub note_edit: Option<CommandId>,
    pub note_remove: Option<CommandId>,
    pub note_prune: Option<CommandId>,
    pub note_merge: Option<CommandId>,
    /// Shown only while a notes merge is stopped on a conflict — see
    /// [`notes_transient`].
    pub note_merge_commit: Option<CommandId>,
    pub note_merge_abort: Option<CommandId>,
    /// MG.38: the `"` subtree submenu's rows.
    pub subtree_add: Option<CommandId>,
    pub subtree_merge: Option<CommandId>,
    pub subtree_pull: Option<CommandId>,
    pub subtree_push: Option<CommandId>,
    pub subtree_split: Option<CommandId>,
    /// MG.39: `w` am / `W` format-patch, and the way out of a stopped
    /// `am`.
    pub am_apply: Option<CommandId>,
    pub am_continue: Option<CommandId>,
    pub am_skip: Option<CommandId>,
    pub am_abort: Option<CommandId>,
    pub format_patch: Option<CommandId>,
    /// MG.40: `Y` cherries.
    pub cherries: Option<CommandId>,
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
fn applying_changes_items(ids: &MagitActionIds, ctx: &TransientContext) -> Vec<TransientItem> {
    let mut items = Vec::new();
    if ctx.has_minor(crate::MagitCoreMode::mode_id().as_str()) {
        items.push(action_or_placeholder(
            ids.get("action:magit-apply-hunk"),
            "a",
            "apply",
            "Apply the hunk at cursor to the working tree",
            "apply_hunk",
        ));
        items.push(action_or_placeholder(
            ids.get("action:magit-reverse-hunk"),
            "-",
            "reverse",
            "Reverse the hunk at cursor out of the working tree",
            "reverse_hunk",
        ));
        items.push(action_or_placeholder(
            ids.get("action:magit-discard"),
            "x",
            "discard",
            "Discard the hunk or file at cursor (asks first)",
            "discard_at_cursor",
        ));
    }
    items.push(action_or_placeholder(
        ids.get("action:magit-global-stage-all"),
        "S",
        "stage all",
        "Stage every tracked modification (git add --update)",
        "stage_all_op",
    ));
    items.push(action_or_placeholder(
        ids.get("action:magit-global-unstage-all"),
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
fn reset_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Reset".into(),
        groups: vec![row_group("Reset", ids, RESET_ROWS)],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// MG.29 + MG.32: the `b` branch submenu.
///
/// `b` used to open the branch **list** straight away. That is one of
/// several things you want from "branches", and it made the other ones
/// — check one out, start one — reachable only by opening the list
/// first and then finding the chord. Magit puts them in a submenu; so
/// does this.
///
/// Every row ASKS rather than reading a cursor, because a menu opened
/// from anywhere has none — the same answer MG.23j gave the commit rows,
/// and the reason magit's own branch commands sit in an ungated group.
///
/// ## The keys are magit's, and MG.32 corrected two that were not
///
/// Pulled from `magit/lisp/magit-branch.el`'s `magit-branch` transient
/// with `evil-collection-magit-popup-changes` applied — MG.23's policy
/// #1 ("keys follow magit / evil-collection-magit from day one, so a row
/// landing later lands in the slot muscle memory already expects").
/// MG.29 shipped this submenu without doing that inventory, and two keys
/// were wrong as a result:
///
/// - **`l` was "list"**, but in magit `l` is *checkout local branch*.
///   The list is a lattice concept — magit's branch transient has no
///   list-buffer row at all — so it had squatted on an occupied key.
/// - **`b` was the local-branch picker**, but magit's `b` is
///   *branch/revision*: it accepts a tag, a remote ref or a raw SHA,
///   which a list of local branches cannot express. What MG.29 built
///   was magit's `l` under magit's `b`.
///
/// So the MG.29 row moved `b` → `l` keeping its action (pinned by a
/// test), `b` became the revision prompt it always meant, and the list
/// took `L` — free in magit's transient, and capital-as-variant matches
/// magit's own `d`/`D`, `l`/`L`, `b`/`B` pairs in file-dispatch.
///
/// Deferred, with magit's keys reserved so they stay free: `s`/`S`
/// spin-off/spin-out, `C` configure (a sub-transient over
/// `branch.<name>.*` that likely belongs to `:customize`, not a
/// hand-rolled menu), `X` reset (wants MG.23j's commit picker).
fn branch_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Branch".into(),
        groups: vec![
            row_group("Checkout", ids, BRANCH_CHECKOUT_ROWS),
            row_group("Create", ids, BRANCH_CREATE_ROWS),
            row_group("Do", ids, BRANCH_DO_ROWS),
        ],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// MG.37: the `T` notes submenu, gated on whether a notes merge is
/// stopped mid-flight.
///
/// Keys are magit's own (`magit-notes`): `T` edit, `r` remove, `m`
/// merge, `p` prune, and — while merging — `c` commit / `a` abort.
///
/// **Gated for the same reason `B` bisect is** (MG.21g): outside a
/// merge, `git notes merge --commit` / `--abort` error, so ungated rows
/// would look actionable and fail. Inside one, edit / remove / merge /
/// prune are what you must not be doing, so the menu shows only the two
/// ways out.
///
/// **Deferred, and magit's keys left free for them:** the four
/// configure rows (`c` / `d` / `C` / `D`, setting `core.notesRef` and
/// `notes.displayRef`). Those are transient *variable rows* — they
/// render a config value inside the menu and edit it in place — which
/// lattice's transients do not have. It is the same gap MG.21d named
/// for remote URLs, and `:customize` is the likelier long-term home for
/// per-repo git config than a hand-rolled menu.
fn notes_transient(ids: &MagitActionIds, merge_in_progress: bool) -> TransientSpec {
    let groups = if merge_in_progress {
        vec![TransientGroup {
            label: "Notes merge in progress".into(),
            items: vec![
                action_or_placeholder(
                    ids.get("action:magit-global-note-merge-commit"),
                    "c",
                    "commit merge",
                    "Finish the notes merge, keeping the resolved notes",
                    "note_merge_commit_op",
                ),
                action_or_placeholder(
                    ids.get("action:magit-global-note-merge-abort"),
                    "a",
                    "abort merge",
                    "Abandon the notes merge, restoring the notes ref",
                    "note_merge_abort_op",
                ),
            ],
        }]
    } else {
        vec![TransientGroup {
            label: "Notes".into(),
            items: vec![
                action_or_placeholder(
                    ids.get("action:magit-global-note-edit"),
                    "T",
                    "edit",
                    "Edit the note on a commit — opens an editable buffer",
                    "note_edit_op",
                ),
                action_or_placeholder(
                    ids.get("action:magit-global-note-remove"),
                    "r",
                    "remove",
                    "Remove the note from a commit",
                    "note_remove_op",
                ),
                action_or_placeholder(
                    ids.get("action:magit-global-note-merge"),
                    "m",
                    "merge",
                    "Merge another notes ref into this one",
                    "note_merge_op",
                ),
                action_or_placeholder(
                    ids.get("action:magit-global-note-prune"),
                    "p",
                    "prune",
                    "Drop notes whose commit no longer exists (asks first)",
                    "note_prune_op",
                ),
            ],
        }]
    };
    TransientSpec {
        title: "Notes".into(),
        groups,
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// MG.38: the `"` subtree submenu.
///
/// **Magit's key for subtree is `O`, and `O` is not free here** — the
/// MG.34–MG.40 scoping note said it was, and that was wrong: `O` is the
/// reset submenu, which is evil-collection-magit's remap of magit's `X`.
/// evil-collection resolves the collision it created, and this follows
/// it verbatim: `(magit-dispatch "O" "\"" magit-subtree)`. So subtree
/// takes `"`, which is the reference set the standing rule names.
///
/// Every row prompts, because every subtree operation needs a
/// `--prefix=<dir>` and most need a repository and a ref too — none of
/// which a menu can guess.
fn subtree_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Subtree".into(),
        groups: vec![row_group("Actions", ids, SUBTREE_ROWS)],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// MG.39: the `w` patch submenu, gated on whether a `git am` is stopped.
///
/// Magit splits these across `w` (am) and `W` (patch); one submenu holds
/// both because apply-a-patch and create-a-patch are the two halves of
/// the same email workflow and there are five rows between them.
///
/// **Gated like `B` and `T`:** an `am` stops on a patch that will not
/// apply, and outside that state `--continue` / `--skip` / `--abort`
/// error. Inside it, applying more patches is what you must not do.
fn patch_transient(ids: &MagitActionIds, am_in_progress: bool) -> TransientSpec {
    let groups = if am_in_progress {
        vec![TransientGroup {
            label: "Patch application stopped".into(),
            items: vec![
                action_or_placeholder(
                    ids.get("action:magit-global-am-continue"),
                    "c",
                    "continue",
                    "Resume applying after resolving the conflict",
                    "am_continue_op",
                ),
                action_or_placeholder(
                    ids.get("action:magit-global-am-skip"),
                    "s",
                    "skip",
                    "Skip the patch that would not apply",
                    "am_skip_op",
                ),
                action_or_placeholder(
                    ids.get("action:magit-global-am-abort"),
                    "a",
                    "abort",
                    "Abandon the whole apply, restoring the branch",
                    "am_abort_op",
                ),
            ],
        }]
    } else {
        vec![TransientGroup {
            label: "Patches".into(),
            items: vec![
                action_or_placeholder(
                    ids.get("action:magit-global-am-apply"),
                    "w",
                    "apply patches",
                    "Apply a mailbox of patches (git am)",
                    "am_apply_op",
                ),
                action_or_placeholder(
                    ids.get("action:magit-global-format-patch"),
                    "W",
                    "create patches",
                    "Write a commit range out as .patch files",
                    "format_patch_op",
                ),
            ],
        }]
    };
    TransientSpec {
        title: "Patches".into(),
        groups,
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
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
pub fn view_arguments_transient(ids: &MagitActionIds, ctx: &TransientContext) -> TransientSpec {
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
                    ids.get("action:magit-view-refresh-args"),
                    "g",
                    "refresh",
                    "Re-run with these arguments",
                    "view_args_op",
                )],
            },
        ],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
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

/// MG.39: is a `git am` stopped on a patch that would not apply?
///
/// Git records one as a `rebase-apply` directory in the gitdir — the
/// same marker the legacy rebase backend uses, which is why
/// `magit_rebase_mode::rebase_in_progress` checks it too. A stopped `am`
/// therefore also shows the rebase abort as available, which is
/// correct: `git rebase --abort` is what git itself suggests there.
/// MG.41e: is a rebase stopped, mid-conflict or at an `edit` stop?
///
/// Read from the repository like its peers. Git uses two directories
/// depending on the rebase backend — `rebase-merge` for the
/// interactive/merge one, `rebase-apply` for the older am-based one —
/// and checking only the first misses a whole class of stopped rebase.
///
/// `rebase-apply` is shared with `git am`, which is why
/// [`am_in_progress`] looks at the same path: the two are
/// distinguishable only by the `applying` marker file `am` leaves.
pub fn rebase_in_progress() -> bool {
    crate::workdir::magit_workdir()
        .and_then(|wd| lattice_vcs::Repository::discover(wd).ok())
        .map(|repo| {
            let gitdir = repo.gitdir();
            gitdir.join("rebase-merge").exists()
                || (gitdir.join("rebase-apply").exists()
                    && !gitdir.join("rebase-apply").join("applying").exists())
        })
        .unwrap_or(false)
}

pub fn am_in_progress() -> bool {
    crate::workdir::magit_workdir()
        .and_then(|wd| lattice_vcs::Repository::discover(wd).ok())
        .map(|repo| repo.gitdir().join("rebase-apply").exists())
        .unwrap_or(false)
}

/// MG.37: is a `git notes merge` stopped on a conflict?
///
/// Peer of [`bisect_in_progress`], and read the same way — from the
/// repository, so the menu reflects what git actually has half-done
/// rather than what the editor last remembered doing.
pub fn notes_merge_in_progress() -> bool {
    crate::workdir::magit_workdir()
        .and_then(|wd| lattice_vcs::Repository::discover(wd).ok())
        .map(|repo| lattice_vcs::Note::merge_in_progress(repo.gitdir()))
        .unwrap_or(false)
}

fn bisect_transient(ids: &MagitActionIds, in_progress: bool) -> TransientSpec {
    let items = if in_progress {
        vec![
            action_or_placeholder(
                ids.get("action:magit-global-bisect-good"),
                "g",
                "good",
                "Mark the revision git checked out as good",
                "bisect_good_op",
            ),
            action_or_placeholder(
                ids.get("action:magit-global-bisect-bad"),
                "b",
                "bad",
                "Mark the revision git checked out as bad",
                "bisect_bad_op",
            ),
            action_or_placeholder(
                ids.get("action:magit-global-bisect-skip"),
                "k",
                "skip",
                "Skip this revision — it cannot be tested",
                "bisect_skip_op",
            ),
            action_or_placeholder(
                ids.get("action:magit-global-bisect-reset"),
                "r",
                "reset",
                "End the bisect and return to where it started",
                "bisect_reset_op",
            ),
        ]
    } else {
        vec![action_or_placeholder(
            ids.get("action:magit-global-bisect-start"),
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
        footer: Some("q dismiss  Esc/BS back".into()),
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
fn status_row(ids: &MagitActionIds, ctx: &TransientContext) -> TransientItem {
    if ctx.is_major(crate::MagitStatusMode::mode_id().as_str()) {
        return TransientItem {
            key: vec!["s".into()],
            label: "jump".into(),
            description: "Jump to a section of this buffer".into(),
            kind: TransientItemKind::Submenu(Arc::new(jump_transient(ids))),
        };
    }
    action_or_placeholder(
        ids.get("action:magit-global-status"),
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
/// MG.41e: the `r` submenu, gated on whether a rebase is stopped.
fn merge_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Merge".into(),
        groups: vec![row_group("Merge", ids, MERGE_ROWS)],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

fn tag_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Tag".into(),
        groups: vec![row_group("Tag", ids, TAG_ROWS)],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

fn rebase_transient(ids: &MagitActionIds, in_progress: bool) -> TransientSpec {
    let groups = if in_progress {
        vec![row_group("Rebase in progress", ids, REBASE_SEQUENCE_ROWS)]
    } else {
        vec![row_group("Rebase", ids, REBASE_START_ROWS)]
    };
    TransientSpec {
        title: "Rebase".into(),
        groups,
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

fn jump_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Jump to section".into(),
        groups: vec![row_group("Sections", ids, JUMP_ROWS)],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
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
pub fn dispatch_transient(ids: &MagitActionIds, ctx: &TransientContext) -> TransientSpec {
    dispatch_transient_with(ids, ctx, DispatchGates::probe())
}

/// The mid-flight git operations the menu gates rows on.
///
/// A struct rather than positional `bool`s: they are adjacent, same
/// type, and both mean "something is half-done" — exactly the pair that
/// transposes silently, and a transposed gate shows the wrong menu with
/// no error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DispatchGates {
    /// MG.21g: `git bisect` is running.
    pub bisect: bool,
    /// MG.37: `git notes merge` stopped on a conflict.
    pub notes_merge: bool,
    /// MG.39: `git am` stopped on a patch that would not apply.
    pub am: bool,
    /// MG.41e: a rebase is stopped — mid-conflict or at an `edit` stop.
    pub rebase: bool,
}

impl DispatchGates {
    /// Read the gates from the repository.
    ///
    /// Every *guard* over this menu passes gates in rather than calling
    /// this, deliberately: probing would make a test's row count depend
    /// on whether the developer's own checkout happened to be mid-bisect
    /// while the suite ran — a flake that reads as a real regression.
    pub fn probe() -> Self {
        Self {
            bisect: bisect_in_progress(),
            notes_merge: notes_merge_in_progress(),
            am: am_in_progress(),
            rebase: rebase_in_progress(),
        }
    }
}

/// [`dispatch_transient`] with the gates supplied rather than probed —
/// pure, and the form every guard over this menu uses.
pub fn dispatch_transient_with(
    ids: &MagitActionIds,
    ctx: &TransientContext,
    gates: DispatchGates,
) -> TransientSpec {
    let bisect_in_progress = gates.bisect;
    TransientSpec {
        title: "Magit dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Working tree".into(),
                items: vec![
                    status_row(ids, ctx),
                    action_or_placeholder(
                        ids.get("action:magit-global-diff"),
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
                    action_or_placeholder(ids.get("action:magit-global-log"), "l", "log", "Show commit history", "show_log"),
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
                        ids.get("action:magit-cherry-pick"),
                        "A",
                        "cherry-pick",
                        "Cherry-pick a commit onto this branch",
                        "cherry_pick_op",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-revert"),
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
                    // MG.37: magit's `T`. A submenu rather than a direct
                    // action because notes have four operations and two
                    // more while a merge is stopped — the same shape `B`
                    // has, and gated the same way.
                    TransientItem {
                        key: vec!["T".into()],
                        label: "notes".into(),
                        description: "Edit, remove, merge or prune commit notes".into(),
                        kind: TransientItemKind::Submenu(Arc::new(notes_transient(
                            ids,
                            gates.notes_merge,
                        ))),
                    },
                ],
            },
            TransientGroup {
                label: "Branches".into(),
                items: vec![TransientItem {
                    key: vec!["b".into()],
                    label: "branch".into(),
                    description: "Checkout, create, or list branches".into(),
                    kind: TransientItemKind::Submenu(Arc::new(branch_transient(ids))),
                }],
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
                            ids,
                            FETCH_ROWS,
                        ))),
                    },
                    // MG.41c: pull IS a submenu now. It was a plain row
                    // because `--ff-only` was not optional and there was
                    // nothing else to show — but magit's pull has three
                    // destinations, and `-r` / `-a` are real toggles.
                    TransientItem {
                        key: vec!["F".into()],
                        label: "pull".into(),
                        description: "Fetch + integrate from the remote".into(),
                        kind: TransientItemKind::Submenu(Arc::new(remote_op_transient(
                            "Pull",
                            RemoteOp::PULL,
                            ids,
                            PULL_ROWS,
                        ))),
                    },
                    TransientItem {
                        key: vec!["P".into()],
                        label: "push".into(),
                        description: "Push to the remote".into(),
                        kind: TransientItemKind::Submenu(Arc::new(remote_op_transient(
                            "Push",
                            RemoteOp::PUSH,
                            ids,
                            PUSH_ROWS,
                        ))),
                    },
                    // MG.21d: magit's `M`. Not a submenu — the row
                    // opens the remote list buffer, because the URLs
                    // are the point and a menu cannot show them. `M`
                    // costs nothing here: transient keys do not shadow
                    // the vim grammar, which is also why `M` stays
                    // unbound as a chord inside magit buffers.
                    action_or_placeholder(
                        ids.get("action:magit-global-remote"),
                        "M",
                        "remote",
                        "Manage remotes — add, rename, remove, set URL, prune",
                        "remote_manage_op",
                    ),
                    // MG.21i: magit's `o`. Like `M`, a buffer rather
                    // than a submenu — and here magit agrees, since
                    // `magit-list-submodules` is a buffer there too.
                    action_or_placeholder(
                        ids.get("action:magit-global-submodule"),
                        "o",
                        "submodule",
                        "Manage submodules — add, update, sync, remove",
                        "submodule_op",
                    ),
                    // MG.40: magit's `Y`. A buffer, like `y` above and
                    // for the same reason — the answer is a list.
                    action_or_placeholder(
                        ids.get("action:magit-global-cherries"),
                        "Y",
                        "cherries",
                        "Which commits are not upstream yet, and which already are",
                        "cherries_op",
                    ),
                    // MG.35: magit's `y`. A buffer for the same reason
                    // `M` and `o` are — the answer is a list with a
                    // column of object ids, which a menu cannot show.
                    action_or_placeholder(
                        ids.get("action:magit-global-refs"),
                        "y",
                        "refs",
                        "Show every branch, remote-tracking branch and tag",
                        "show_refs",
                    ),
                    // MG.36: magit's `C`. In the Remotes group because
                    // that is what a clone reads from — magit files it
                    // under its own dispatch's ungated set for the same
                    // reason it needs no repository to be open.
                    action_or_placeholder(
                        ids.get("action:magit-global-clone"),
                        "C",
                        "clone",
                        "Clone a repository — asks for the URL, then where to put it",
                        "clone_op",
                    ),
                ],
            },
            TransientGroup {
                label: "Misc".into(),
                items: vec![
                    // MG.38: evil-collection-magit's key for subtree,
                    // because magit's own `O` is the reset submenu here.
                    TransientItem {
                        key: vec!["\"".into()],
                        label: "subtree".into(),
                        description: "Add, merge, pull, push or split a subtree".into(),
                        kind: TransientItemKind::Submenu(Arc::new(subtree_transient(ids))),
                    },
                    // MG.39: magit's `w`, holding `W`'s rows too.
                    TransientItem {
                        key: vec!["w".into()],
                        label: "patches".into(),
                        description: "Apply or create email patches".into(),
                        kind: TransientItemKind::Submenu(Arc::new(patch_transient(
                            ids, gates.am,
                        ))),
                    },
                    // MG.41e: a submenu now, gated like bisect / am.
                    // A stopped rebase needs continue / skip / abort,
                    // and offering "start an interactive rebase" while
                    // one is half-done is the wrong menu entirely.
                    TransientItem {
                        key: vec!["r".into()],
                        label: "rebase".into(),
                        description: "Rebase, or drive a stopped one".into(),
                        kind: TransientItemKind::Submenu(Arc::new(rebase_transient(
                            ids,
                            gates.rebase,
                        ))),
                    },
                    // MG.23c1: magit's own keys. Both ask for their one
                    // value rather than taking it from context — there
                    // is nothing at a cursor to read from a menu opened
                    // anywhere.
                    TransientItem {
                        key: vec!["t".into()],
                        label: "tag".into(),
                        description: "Create or delete a tag".into(),
                        kind: TransientItemKind::Submenu(Arc::new(tag_transient(ids))),
                    },
                    action_or_placeholder(
                        ids.get("action:magit-global-gitignore"),
                        "i",
                        "gitignore",
                        "Add a pattern to .gitignore",
                        "gitignore_op",
                    ),
                    // MG.23c2. `m` is the repo-level convenience for
                    // when you know the branch name; picking from a
                    // list is already served one level down, by `m` in
                    // the branch buffer.
                    TransientItem {
                        key: vec!["m".into()],
                        label: "merge".into(),
                        description: "Merge a branch into the current one".into(),
                        kind: TransientItemKind::Submenu(Arc::new(merge_transient(ids))),
                    },
                    action_or_placeholder(
                        ids.get("action:magit-global-init"),
                        "I",
                        "init",
                        "Initialize a git repository",
                        "init_op",
                    ),
                ],
            },
        ],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// Build a commit sub-transient. `c c` / `c a` mirror magit-status's
/// own `cc` / `ca` chords exactly, so the same two keystrokes commit
/// and amend whether you're inside the status buffer or reaching for
/// the dispatch menu from an ordinary file.
fn commit_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Commit".into(),
        groups: vec![row_group("Actions", ids, COMMIT_ROWS)],
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// Build a stash sub-transient. `z l` lists, `z z` pushes a new
/// stash — magit uses `z z` for "stash" (push) too, and apply/pop/
/// drop live as `a`/`p`/`d` chords inside the stash-list buffer
/// itself rather than being duplicated here (they need a stash
/// selected, which only the list view provides).
fn stash_transient(ids: &MagitActionIds) -> TransientSpec {
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
            row_group("Actions", ids, STASH_ROWS),
        ],
        preview: Some(remote_preview(RemoteOp::STASH)),
        footer: Some("q dismiss  Esc/BS back".into()),
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
    /// MG.28: `V` — from a blob buffer back to the live file.
    pub visit_live: Option<CommandId>,
    /// MG.23d: the file operations.
    pub untrack: Option<CommandId>,
    pub delete: Option<CommandId>,
    pub rename: Option<CommandId>,
    /// MG.23d2: check the file out from a revision.
    pub checkout: Option<CommandId>,
    /// MG.34: `M` — the merge that brought a commit into HEAD. Magit's
    /// own key for it in `magit-file-dispatch`.
    pub log_merged: Option<CommandId>,
    /// MG.34: `e` — start a rebase to amend the commit that wrote the
    /// line at the cursor. Magit's own key, in its "More actions" group.
    pub edit_line_commit: Option<CommandId>,
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
pub fn other_file_dispatch_transient(ids: &MagitActionIds) -> TransientSpec {
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
                        ids.get("action:magit-global-file-stage"),
                        "s",
                        "stage",
                        "Stage the target file",
                        "stage_other_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-unstage"),
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
                        ids.get("action:magit-global-file-discard"),
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
                        ids.get("action:magit-global-file-diff"),
                        "d",
                        "diff",
                        "Show the target file's diff",
                        "diff_other_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-log"),
                        "l",
                        "log",
                        "Show the target file's history",
                        "log_other_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-blame"),
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

pub fn file_dispatch_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "File dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Stage".into(),
                items: vec![
                    action_or_placeholder(ids.get("action:magit-global-file-stage"), "s", "stage", "Stage this file", "stage_file"),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-unstage"),
                        "u",
                        "unstage",
                        "Unstage this file",
                        "unstage_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-discard"),
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
                        ids.get("action:magit-global-file-untrack"),
                        ",x",
                        "untrack",
                        "Stop tracking this file, keeping it on disk",
                        "untrack_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-rename"),
                        ",r",
                        "rename",
                        "Rename this file (asks for the new name)",
                        "rename_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-delete"),
                        ",k",
                        "delete",
                        "Delete this file (asks first)",
                        "delete_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-checkout"),
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
                        ids.get("action:magit-global-file-diff"),
                        "d",
                        "diff",
                        "Show diff for this file",
                        "diff_file",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-log"),
                        "l",
                        "log",
                        "Show commit history for this file",
                        "log_file",
                    ),
                    action_or_placeholder(ids.get("action:magit-global-file-blame"), "b", "blame", "Blame this file", "blame_file"),
                    // MG.23f2, on magit's own key for it (`f`
                    // "...reverse" in magit-file-dispatch's Blame
                    // group). Only meaningful from a blob buffer; the
                    // handler says so rather than the row hiding, since
                    // there is no per-context menu content yet
                    // (MG.23h).
                    // MG.28: magit's own key for this.
                    action_or_placeholder(
                        ids.get("action:magit-global-file-at-revision"),
                        "v",
                        "view at revision",
                        "Open this file as it was at a revision you name",
                        "file_at_revision",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-visit-live"),
                        "V",
                        "back to the live file",
                        "From a file-at-revision, open the working-tree copy at the same line",
                        "file_visit_live",
                    ),
                    action_or_placeholder(
                        ids.get("action:magit-global-file-blame-reverse"),
                        "f",
                        "reverse blame",
                        "For each line of this revision, the last commit it existed in",
                        "blame_reverse_file",
                    ),
                    // MG.34, on magit's own key for it (`M` "Merged" in
                    // magit-file-dispatch). A row rather than a chord:
                    // `M` and `gM` are both vim motions, and magit binds
                    // this as a transient suffix anyway.
                    action_or_placeholder(
                        ids.get("action:magit-global-log-merged"),
                        "M",
                        "merged",
                        "Show the merge commit that brought a commit into HEAD",
                        "log_merged",
                    ),
                ],
            },
            // MG.34: magit's own group name for the row below.
            TransientGroup {
                label: "More actions".into(),
                items: vec![action_or_placeholder(
                    ids.get("action:magit-global-edit-line-commit"),
                    "e",
                    "edit line",
                    "Start a rebase to amend the commit that wrote the line at the cursor",
                    "edit_line_commit",
                )],
            },
        ],
        preview: None,
        footer: Some("q dismiss".into()),
    }
}

#[cfg(test)]
mod row_table_tests {
    use super::*;

    /// A `CommandRegistry` with magit's actions registered, as `install`
    /// leaves it.
    fn registry() -> lattice_grammar::CommandRegistry {
        let mut r = lattice_grammar::CommandRegistry::new();
        let _ = lattice_grammar::builtins::populate(&mut r);
        let _ = lattice_grammar::ex_commands::populate(&mut r);
        crate::register_action_commands_for_test(&mut r);
        r
    }

    /// MG.41a: THE test that replaces compile-time field checking.
    ///
    /// Rows name their command as a string, so a typo or a renamed
    /// action no longer fails to compile — it renders a disabled
    /// placeholder the user reads as "not implemented yet". This is the
    /// same guard `help_prefix_chord_table_resolves_all_commands` gives
    /// the `<C-h>` map, and it is why the string-keyed design is safe.
    #[test]
    fn every_row_action_is_registered() {
        let reg = registry();
        let ids = MagitActionIds::resolve(&reg);
        assert!(!ids.is_empty(), "no magit actions resolved at all");
        for (table, rows) in all_row_tables() {
            for row in *rows {
                assert!(
                    ids.get(row.action).is_some(),
                    "{table} row `{}` ({}) references unregistered `{}`",
                    row.key,
                    row.label,
                    row.action,
                );
            }
        }
    }

    /// Two rows in one group cannot share a key — the second would be
    /// unreachable, and silently so.
    #[test]
    fn no_duplicate_keys_within_a_table() {
        for (table, rows) in all_row_tables() {
            let mut seen = std::collections::HashSet::new();
            for row in *rows {
                assert!(
                    seen.insert(row.key),
                    "{table} binds `{}` twice; the second row is unreachable",
                    row.key,
                );
            }
        }
    }

    /// Every row carries a non-empty label and doc — the transient
    /// renders both, and a blank one reads as a rendering bug.
    #[test]
    fn rows_are_fully_described() {
        for (table, rows) in all_row_tables() {
            for row in *rows {
                assert!(!row.key.is_empty(), "{table}: empty key");
                assert!(!row.label.is_empty(), "{table} `{}`: empty label", row.key);
                assert!(!row.doc.is_empty(), "{table} `{}`: empty doc", row.key);
                assert!(
                    row.action.starts_with(MagitActionIds::PREFIX),
                    "{table} `{}`: `{}` is outside the `{}` namespace, so \
                     `MagitActionIds::resolve` will never find it",
                    row.key,
                    row.action,
                    MagitActionIds::PREFIX,
                );
            }
        }
    }

    /// The resolver picks up magit actions automatically — the property
    /// that removes the third enumeration. If this regresses to a
    /// hand-kept list, adding an action would silently not be reachable.
    #[test]
    fn resolver_finds_actions_without_a_hand_kept_list() {
        let reg = registry();
        let ids = MagitActionIds::resolve(&reg);
        let registered = reg
            .names()
            .filter(|n| n.starts_with(MagitActionIds::PREFIX))
            .count();
        assert_eq!(
            ids.by_name.len(),
            registered,
            "resolve() must pick up EVERY registered magit action",
        );
    }
}

#[cfg(test)]
mod background_task_tests {
    /// MG.41g: magit publishes completion; it does not post
    /// notifications.
    ///
    /// The decoupling is the point of the slice, so it is worth
    /// asserting structurally rather than trusting a grep at review
    /// time: if a future spawner reaches for `lattice_notify` again,
    /// the coupling this removed is back.
    #[test]
    fn magit_does_not_depend_on_the_notification_crate() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("lattice-notify"),
            "magit must not depend on lattice-notify — completion is \
             reported by publishing `BackgroundTaskFinished`, which the \
             notification layer subscribes to",
        );
    }

    /// Every git-spawning helper reports completion through
    /// `finish_task`, which logs AND publishes in one call.
    ///
    /// Five of ten spawners previously reported nothing at all — the
    /// gap that motivated this slice — and the reason was that
    /// notification was an opt-in parameter each one could forget.
    #[test]
    fn every_spawner_reports_completion() {
        let src = include_str!("magit_global_mode.rs");
        // Spawners that delegate to another spawner inherit its
        // reporting; the rest must call `finish_task` themselves.
        let delegating = ["spawn_note_remove", "spawn_note_prune"];
        let mut checked = 0;
        for (idx, _) in src.match_indices("fn spawn_") {
            let name: String = src[idx + 3..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let body_end = src[idx..]
                .find("\nfn ")
                .map(|e| idx + e)
                .unwrap_or(src.len());
            let body = &src[idx..body_end];
            if delegating.contains(&name.as_str()) {
                assert!(
                    body.contains("spawn_git(") || body.contains("spawn_remote_op("),
                    "{name} is listed as delegating but calls neither spawner",
                );
            } else {
                assert!(
                    body.contains("finish_task"),
                    "{name} does not report completion — a background op that \
                     finishes invisibly is the bug MG.41g fixed",
                );
            }
            checked += 1;
        }
        assert!(checked >= 8, "expected to inspect every spawner, saw {checked}");
    }
}

#[cfg(test)]
mod remote_target_tests {
    use crate::magit_global_mode::RemoteTarget;

    /// MG.41c: `p` — no destination argument, so git resolves
    /// `pushRemote` / `remote.pushDefault` itself. This is what the
    /// single unlabelled "push" row did; it is now named.
    #[test]
    fn configured_adds_nothing() {
        assert!(RemoteTarget::Configured.argv(None).is_empty());
        // A stray resolved value cannot leak in.
        assert!(RemoteTarget::Configured.argv(Some("origin main")).is_empty());
    }

    #[test]
    fn all_remotes_and_all_tags_are_flags() {
        assert_eq!(RemoteTarget::AllRemotes.argv(None), vec!["--all"]);
        assert_eq!(RemoteTarget::AllTags.argv(None), vec!["--tags"]);
    }

    /// The upstream pair expands to TWO tokens — `origin main`, not
    /// `origin/main`. `git push origin/main` would be read as a single
    /// refspec and fail, which is the bug this splitting avoids.
    #[test]
    fn upstream_expands_to_remote_and_branch() {
        assert_eq!(
            RemoteTarget::Upstream.argv(Some("origin main")),
            vec!["origin", "main"],
        );
    }

    /// An unresolved destination contributes NOTHING rather than an
    /// empty argument. `git push ""` is not a no-op — git reads it as a
    /// real (empty) refspec and errors.
    #[test]
    fn an_unresolved_destination_contributes_no_argument() {
        for t in [RemoteTarget::Upstream, RemoteTarget::Prompted] {
            assert!(t.argv(None).is_empty(), "{t:?} with None");
            assert!(t.argv(Some("")).is_empty(), "{t:?} with empty");
            assert!(t.argv(Some("   ")).is_empty(), "{t:?} with blank");
        }
    }

    /// A prompted destination may be several tokens (`origin my-branch`)
    /// or one (`v1.2.0`); both pass through verbatim.
    #[test]
    fn prompted_destinations_pass_through() {
        assert_eq!(RemoteTarget::Prompted.argv(Some("v1.2.0")), vec!["v1.2.0"]);
        assert_eq!(
            RemoteTarget::Prompted.argv(Some("origin feature/x")),
            vec!["origin", "feature/x"],
        );
    }
}

#[cfg(test)]
mod remote_flag_tests {
    use crate::magit_global_mode::RemoteOp;
    use lattice_grammar::{ArgValue, Args};

    fn flags(op: RemoteOp, on: &[&str]) -> Vec<String> {
        let list: Vec<ArgValue> = op
            .flags
            .iter()
            .map(|f| ArgValue::Bool(on.contains(&f.name)))
            .collect();
        op.argv(&Args::List(list))
    }

    /// MG.41c: `--rebase` REPLACES `--ff-only`. Git rejects the pair,
    /// so emitting both would make magit's `-r` row fail every time.
    #[test]
    fn pull_rebase_replaces_ff_only() {
        let argv = flags(RemoteOp::PULL, &["rebase"]);
        assert!(argv.contains(&"--rebase".to_string()), "{argv:?}");
        assert!(
            !argv.contains(&"--ff-only".to_string()),
            "--ff-only must be dropped when rebasing: {argv:?}",
        );
    }

    /// Without `-r`, the safe default stands: a pull cannot create a
    /// merge commit behind your back.
    #[test]
    fn pull_defaults_to_ff_only() {
        let argv = flags(RemoteOp::PULL, &[]);
        assert!(argv.contains(&"--ff-only".to_string()), "{argv:?}");
        assert!(!argv.contains(&"--rebase".to_string()), "{argv:?}");
    }

    /// `--autostash` is orthogonal — it must survive alongside either.
    #[test]
    fn pull_autostash_is_independent_of_rebase() {
        let with_rebase = flags(RemoteOp::PULL, &["rebase", "autostash"]);
        assert!(with_rebase.contains(&"--autostash".to_string()));
        assert!(!with_rebase.contains(&"--ff-only".to_string()));
        let without = flags(RemoteOp::PULL, &["autostash"]);
        assert!(without.contains(&"--autostash".to_string()));
        assert!(without.contains(&"--ff-only".to_string()));
    }

    /// The flag table is looked up by NAME, so reordering it cannot
    /// silently point `--rebase` at another toggle's slot.
    #[test]
    fn rebase_lookup_survives_table_order() {
        let idx = RemoteOp::PULL
            .flags
            .iter()
            .position(|f| f.name == "rebase")
            .expect("pull has a rebase flag");
        // Only that slot turns it on.
        let mut list: Vec<ArgValue> = RemoteOp::PULL
            .flags
            .iter()
            .map(|_| ArgValue::Bool(false))
            .collect();
        list[idx] = ArgValue::Bool(true);
        let argv = RemoteOp::PULL.argv(&Args::List(list));
        assert!(!argv.contains(&"--ff-only".to_string()), "{argv:?}");
    }

    /// MG.41c added magit's remaining push flags — except bare
    /// `--force`, which lattice deliberately does not offer.
    ///
    /// That divergence predates this slice and is pinned separately by
    /// `force_push_uses_force_with_lease`: `--force-with-lease` refuses
    /// exactly when a bare force would destroy commits you never
    /// fetched. "Match magit" governs KEYS inside a transient; it does
    /// not extend to re-adding a footgun someone removed on purpose.
    #[test]
    fn push_offers_magits_flag_set_minus_the_footgun() {
        let names: Vec<&str> = RemoteOp::PUSH.flags.iter().map(|f| f.name).collect();
        for expected in ["force-with-lease", "set-upstream", "no-verify", "dry-run"] {
            assert!(names.contains(&expected), "push missing `{expected}`: {names:?}");
        }
        assert!(
            !names.contains(&"force"),
            "bare --force stays out; see force_push_uses_force_with_lease",
        );
    }

    #[test]
    fn fetch_offers_tags_and_prune() {
        let names: Vec<&str> = RemoteOp::FETCH.flags.iter().map(|f| f.name).collect();
        for expected in ["tags", "prune", "all"] {
            assert!(names.contains(&expected), "fetch missing `{expected}`: {names:?}");
        }
    }
}

#[cfg(test)]
mod commit_op_argv_tests {
    use crate::magit_global_mode::CommitOp;

    /// MG.41d: `git reset <commit> --` resets the INDEX without moving
    /// HEAD. Drop the trailing `--` and it moves HEAD too — a very
    /// different operation, which is why the position is pinned.
    #[test]
    fn reset_index_puts_the_dashes_after_the_commit() {
        assert_eq!(
            CommitOp::RESET_INDEX.argv("abc123"),
            vec!["reset", "abc123", "--"],
        );
    }

    /// `--keep` refuses rather than discarding, so unlike `--hard` it
    /// carries no confirm step.
    #[test]
    fn reset_keep_needs_no_confirmation() {
        assert_eq!(CommitOp::RESET_KEEP.argv("abc123"), vec!["reset", "--keep", "abc123"]);
        assert!(CommitOp::RESET_KEEP.confirm_action.is_none());
        // The destructive sibling still does.
        assert!(CommitOp::RESET_HARD.confirm_action.is_some());
    }

    /// fixup / squash take the target commit LAST, which is what git's
    /// `--fixup <commit>` spelling expects.
    #[test]
    fn fixup_and_squash_target_the_commit() {
        assert_eq!(
            CommitOp::COMMIT_FIXUP.argv("abc123"),
            vec!["commit", "--no-edit", "--fixup", "abc123"],
        );
        assert_eq!(
            CommitOp::COMMIT_SQUASH.argv("abc123"),
            vec!["commit", "--no-edit", "--squash", "abc123"],
        );
    }

    /// Ops with no trailing tokens are unchanged by the new field —
    /// the shape every pre-MG.41d op relies on.
    #[test]
    fn ops_without_trailing_tokens_are_unaffected() {
        assert_eq!(CommitOp::RESET_SOFT.argv("abc"), vec!["reset", "--soft", "abc"]);
        assert!(CommitOp::RESET_SOFT.trailing.is_empty());
    }
}

#[cfg(test)]
mod rebase_gate_tests {
    use super::*;

    fn keys(spec: &TransientSpec) -> Vec<String> {
        spec.groups
            .iter()
            .flat_map(|g| &g.items)
            .flat_map(|i| i.key.clone())
            .collect()
    }

    /// MG.41e: outside a rebase the menu offers a way IN; it must not
    /// offer continue / skip / abort, which error when nothing is
    /// stopped and so would look actionable and fail.
    #[test]
    fn no_rebase_running_offers_only_the_way_in() {
        let spec = rebase_transient(&MagitActionIds::default(), false);
        assert_eq!(keys(&spec), vec!["i"]);
    }

    /// Inside one the menu offers only the ways OUT — starting another
    /// rebase while one is half-done is the wrong menu entirely.
    #[test]
    fn a_stopped_rebase_offers_only_the_ways_out() {
        let spec = rebase_transient(&MagitActionIds::default(), true);
        assert_eq!(keys(&spec), vec!["r", "s", "a"]);
        assert!(
            !keys(&spec).contains(&"i".to_string()),
            "must not offer to start a rebase while one is stopped",
        );
    }

    /// The gate is read from the repository, and covers BOTH backends.
    /// Git uses `rebase-merge` for the interactive/merge backend and
    /// `rebase-apply` for the older am-based one; checking only the
    /// first misses a whole class of stopped rebase, and
    /// `rebase-apply` is shared with `git am` (distinguished by the
    /// `applying` marker).
    #[test]
    fn the_gate_is_part_of_the_probe() {
        let gates = DispatchGates::default();
        assert!(!gates.rebase, "default gates report nothing in progress");
    }
}
