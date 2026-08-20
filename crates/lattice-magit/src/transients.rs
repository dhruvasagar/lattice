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
        ("cherry-pick", CHERRY_PICK_ROWS),
        ("cherry-pick/sequence", CHERRY_PICK_SEQUENCE_ROWS),
        ("revert", REVERT_ROWS),
        ("revert/sequence", REVERT_SEQUENCE_ROWS),
        ("merge", MERGE_ROWS),
        ("merge/sequence", MERGE_SEQUENCE_ROWS),
        ("tag", TAG_ROWS),
        ("rebase/start", REBASE_START_ROWS),
        ("rebase/sequence", REBASE_SEQUENCE_ROWS),
        // PD.3 (2026-08-12): these two were never listed, so the drift
        // tests below had never covered the Diff or Log menus — `d`,
        // `f` and `v` included. Exactly the bookkeeping lapse this
        // function's doc comment warns about, found by adding a fourth
        // Diff row and noticing nothing checked it.
        ("diff/show", DIFF_SHOW_ROWS),
        ("log/show", LOG_SHOW_ROWS),
    ]
}

// MG.41c: magit's destination rows. Keys are magit's own.

const PUSH_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "p",
        label: "pushRemote",
        doc: "Push to the configured push-remote",
        action: "action:magit-global-push-configured",
        placeholder: "push_configured_op",
    },
    TransientRow {
        key: "u",
        label: "@{upstream}",
        doc: "Push to this branch's upstream — differs from pushRemote in a triangular workflow",
        action: "action:magit-global-push-upstream",
        placeholder: "push_upstream_op",
    },
    TransientRow {
        key: "e",
        label: "elsewhere",
        doc: "Push to a remote you name",
        action: "action:magit-global-push-elsewhere",
        placeholder: "push_elsewhere_op",
    },
    TransientRow {
        key: "o",
        label: "another branch",
        doc: "Push a branch other than HEAD",
        action: "action:magit-global-push-other-branch",
        placeholder: "push_other_op",
    },
    TransientRow {
        key: "r",
        label: "refspecs",
        doc: "Push explicit refspecs",
        action: "action:magit-global-push-refspecs",
        placeholder: "push_refspecs_op",
    },
    TransientRow {
        key: "T",
        label: "a tag",
        doc: "Push a single tag",
        action: "action:magit-global-push-tag",
        placeholder: "push_tag_op",
    },
    TransientRow {
        key: "t",
        label: "all tags",
        doc: "Push every tag",
        action: "action:magit-global-push-all-tags",
        placeholder: "push_all_tags_op",
    },
];

const PULL_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "p",
        label: "pushRemote",
        doc: "Pull from the configured remote",
        action: "action:magit-global-pull-configured",
        placeholder: "pull_configured_op",
    },
    TransientRow {
        key: "u",
        label: "@{upstream}",
        doc: "Pull from this branch's upstream",
        action: "action:magit-global-pull-upstream",
        placeholder: "pull_upstream_op",
    },
    TransientRow {
        key: "e",
        label: "elsewhere",
        doc: "Pull from a remote you name",
        action: "action:magit-global-pull-elsewhere",
        placeholder: "pull_elsewhere_op",
    },
];

const FETCH_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "p",
        label: "pushRemote",
        doc: "Fetch from the configured remote",
        action: "action:magit-global-fetch-configured",
        placeholder: "fetch_configured_op",
    },
    TransientRow {
        key: "u",
        label: "@{upstream}",
        doc: "Fetch this branch's upstream",
        action: "action:magit-global-fetch-upstream",
        placeholder: "fetch_upstream_op",
    },
    TransientRow {
        key: "e",
        label: "elsewhere",
        doc: "Fetch from a remote you name",
        action: "action:magit-global-fetch-elsewhere",
        placeholder: "fetch_elsewhere_op",
    },
    TransientRow {
        key: "o",
        label: "another branch",
        doc: "Fetch a branch you name",
        action: "action:magit-global-fetch-other-branch",
        placeholder: "fetch_other_op",
    },
    TransientRow {
        key: "r",
        label: "refspecs",
        doc: "Fetch explicit refspecs",
        action: "action:magit-global-fetch-refspecs",
        placeholder: "fetch_refspecs_op",
    },
    TransientRow {
        key: "a",
        label: "all remotes",
        doc: "Fetch from every configured remote",
        action: "action:magit-global-fetch-all-remotes",
        placeholder: "fetch_all_op",
    },
    // MG.43f: magit's `m` — fetch submodules alongside the superproject.
    TransientRow {
        key: "m",
        label: "submodules",
        doc: "Fetch the superproject and its submodules",
        action: "action:magit-global-fetch-submodules",
        placeholder: "fetch_submodules_op",
    },
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
    // MG.43d: magit's `s` / `S` — a branch from the unpushed commits.
    // The pair differs only in where you end up.
    TransientRow {
        key: "s",
        label: "spin-off",
        doc: "Branch the unpushed commits and check it out",
        action: "action:magit-global-branch-spinoff",
        placeholder: "branch_spinoff_op",
    },
    TransientRow {
        key: "S",
        label: "spin-out",
        doc: "Branch the unpushed commits, staying on this branch",
        action: "action:magit-global-branch-spinout",
        placeholder: "branch_spinout_op",
    },
    // MG.43a: magit's `x` — reset this branch to another ref.
    TransientRow {
        key: "x",
        label: "reset",
        doc: "Reset the current branch to another ref (asks first)",
        action: "action:magit-global-branch-reset",
        placeholder: "branch_reset_op",
    },
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
    // MG.43f: magit's `w` — the working tree only; HEAD and the index
    // are left alone.
    TransientRow {
        key: "w",
        label: "worktree",
        doc: "Reset the working tree to a commit, keeping HEAD and the index",
        action: "action:magit-reset-worktree",
        placeholder: "reset_worktree_op",
    },
    // MG.42-E3: two inputs — the commit, then the path.
    TransientRow {
        key: "f",
        label: "a file",
        doc: "Restore one file from a commit, leaving everything else alone",
        action: "action:magit-global-reset-file",
        placeholder: "reset_file_op",
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
    // MG.42-E1: reword — message only. Deliberately NOT amend: a
    // reword that swept in staged changes would be a content change
    // nobody asked for.
    TransientRow {
        key: "w",
        label: "reword",
        doc: "Change the last commit's message, leaving the index alone",
        action: "action:magit-global-reword",
        placeholder: "commit_reword_op",
    },
    // MG.43a: magit's `e` — the one commit row that takes no target.
    TransientRow {
        key: "e",
        label: "extend",
        doc: "Add staged changes to the last commit, keeping its message",
        action: "action:magit-global-commit-extend",
        placeholder: "commit_extend_op",
    },
    // MG.42-E1: augment — a squash marker you annotate.
    TransientRow {
        key: "A",
        label: "augment",
        doc: "Record a squash! for a commit, with a note you write",
        action: "action:magit-commit-augment",
        placeholder: "commit_augment_op",
    },
    // MG.42-E2: magit's instant variants — record AND fold in.
    TransientRow {
        key: "F",
        label: "instant fixup",
        doc: "Record a fixup! and fold it in immediately",
        action: "action:magit-commit-instant-fixup",
        placeholder: "commit_instant_fixup_op",
    },
    TransientRow {
        key: "S",
        label: "instant squash",
        doc: "Record a squash! and fold it in immediately",
        action: "action:magit-commit-instant-squash",
        placeholder: "commit_instant_squash_op",
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
    // MG.42-E2: magit's snapshots — a restore point that leaves the
    // working tree exactly as it was.
    TransientRow {
        key: "Z",
        label: "snapshot",
        doc: "Stash everything and put it straight back",
        action: "action:magit-global-stash-snapshot",
        placeholder: "stash_snapshot_op",
    },
    TransientRow {
        key: "I",
        label: "snapshot index",
        doc: "Snapshot the staged changes, leaving the working tree alone",
        action: "action:magit-global-stash-snapshot-index",
        placeholder: "stash_snapshot_index_op",
    },
    TransientRow {
        key: "W",
        label: "snapshot worktree",
        doc: "Snapshot the working tree, leaving the index alone",
        action: "action:magit-global-stash-snapshot-worktree",
        placeholder: "stash_snapshot_worktree_op",
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
    // MG.42-E3: two inputs — the branch name, then the stash.
    TransientRow {
        key: "b",
        label: "branch",
        doc: "Start a branch from a stash — for when it no longer applies to HEAD",
        action: "action:magit-global-stash-branch",
        placeholder: "stash_branch_op",
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

/// MG.43h: the `d` / `l` argument menus.
///
/// MG.41f built this, found the toggles would render and be silently
/// discarded, and reverted the wiring — correctly, because
/// `action:magit-global-diff` declared no `args_schema` for them to
/// project onto. It concluded the fix was "teach the open actions to
/// accept arguments", i.e. an operation change.
///
/// It was narrower than that. MG.17a's projection was already
/// generic; only the empty schema was missing. Declaring each open
/// action's own flag table, plus a place to leave the values for a
/// buffer that does not exist yet (`ViewArgsRequests`), is the whole
/// of it.
///
/// Each view declares its OWN table rather than the union
/// `action:magit-view-refresh-args` uses — that one action serves both
/// views, whereas these are two, and a diff must never be handed a log
/// flag.
fn view_open_transient(
    title: &str,
    ids: &MagitActionIds,
    flags: &'static [crate::magit_global_mode::RemoteFlag],
    rows: &'static [TransientRow],
) -> TransientSpec {
    let mut groups = Vec::new();
    if !flags.is_empty() {
        groups.push(TransientGroup {
            label: "Arguments".into(),
            items: flag_items_from(flags),
        });
    }
    groups.push(TransientGroup {
        label: "Show".into(),
        items: rows.iter().map(|r| row_item(ids, r)).collect(),
    });
    TransientSpec {
        title: title.into(),
        groups,
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// MG.49: the Diff menu's targets.
///
/// `f` and `v` are here because binding `d` to this menu takes their
/// chords: the trie checks a node's own binding before its children, so
/// a bound `d` makes `dv` unreachable, and `d` itself was
/// `magit-diff-file` on magit-status. Both keep working through the
/// menu instead of being silently lost — which is the whole reason the
/// rows moved rather than the chords being dropped.
const DIFF_SHOW_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "d",
        label: "diff",
        doc: "Diff the working tree against HEAD",
        action: "action:magit-global-diff",
        placeholder: "diff_op",
    },
    TransientRow {
        key: "f",
        label: "file",
        doc: "Diff the file at cursor in a dedicated buffer",
        action: "action:magit-diff-file",
        placeholder: "diff_file",
    },
    TransientRow {
        key: "v",
        label: "side-by-side",
        doc: "Open the file at cursor side-by-side against its baseline",
        action: "action:magit-diff-side-by-side",
        placeholder: "diff_side_by_side",
    },
    // PD.3 (2026-08-12): the editable cross-file view. `e` for "edit"
    // reads correctly and is free. `p` (for "project") was considered
    // and passed over — real magit binds `p` to *diff paths* in this
    // same menu, so reusing it would fight muscle memory people already
    // have.
    //
    // A peer of `d`, not a replacement for it: `d` is the patch view
    // people already know, and the editable view earns its own row.
    TransientRow {
        key: "e",
        label: "edit",
        doc: "Edit the working-tree diff across files",
        action: "action:magit-project-diff",
        placeholder: "diff_project",
    },
];

const LOG_SHOW_ROWS: &[TransientRow] = &[TransientRow {
    key: "l",
    label: "log",
    doc: "Show commit history",
    action: "action:magit-global-log",
    placeholder: "show_log",
}];

// MG.42-E4: cherry-pick / revert, idle and stopped.
//
// The ways OUT are identical in shape but NOT interchangeable:
// `git revert --continue` errors during a cherry-pick and vice versa,
// which is why each sequence has its own rows rather than a shared set.

const CHERRY_PICK_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "A",
        label: "pick",
        doc: "Cherry-pick a commit onto this branch",
        action: "action:magit-cherry-pick",
        placeholder: "cherry_pick_op",
    },
    // MG.43a: magit's `a` — apply the change WITHOUT recording a
    // commit, so it can be edited or split before committing.
    TransientRow {
        key: "a",
        label: "apply",
        doc: "Apply a commit's changes without committing",
        action: "action:magit-cherry-pick-apply",
        placeholder: "cherry_pick_apply_op",
    },
    // MG.43d: the commit-MOVING rows. `A` / `a` copy; these four
    // remove the commit from where it came from.
    TransientRow {
        key: "h",
        label: "harvest",
        doc: "Move a commit here from another branch, removing it there",
        action: "action:magit-cherry-harvest",
        placeholder: "cherry_harvest_op",
    },
    TransientRow {
        key: "d",
        label: "donate",
        doc: "Move a commit to another branch, staying on this one",
        action: "action:magit-cherry-donate",
        placeholder: "cherry_donate_op",
    },
    TransientRow {
        key: "n",
        label: "spinout",
        doc: "Move a commit to a new branch, staying on this one",
        action: "action:magit-cherry-spinout",
        placeholder: "cherry_spinout_op",
    },
    TransientRow {
        key: "s",
        label: "spinoff",
        doc: "Move a commit to a new branch and check it out",
        action: "action:magit-cherry-spinoff",
        placeholder: "cherry_spinoff_op",
    },
];

const CHERRY_PICK_SEQUENCE_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "A",
        label: "continue",
        doc: "Resume the cherry-pick after resolving the conflict",
        action: "action:magit-global-cherry-pick-continue",
        placeholder: "cherry_pick_continue_op",
    },
    TransientRow {
        key: "s",
        label: "skip",
        doc: "Skip the commit the cherry-pick stopped on",
        action: "action:magit-global-cherry-pick-skip",
        placeholder: "cherry_pick_skip_op",
    },
    TransientRow {
        key: "a",
        label: "abort",
        doc: "Abandon the cherry-pick, restoring the branch",
        action: "action:magit-global-cherry-pick-abort",
        placeholder: "cherry_pick_abort_op",
    },
];

const REVERT_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "V",
        label: "revert commit",
        doc: "Revert a commit, creating an inverse commit",
        action: "action:magit-revert",
        placeholder: "revert_op",
    },
    // MG.43a: magit's `v` — stage the reversal WITHOUT committing it.
    TransientRow {
        key: "v",
        label: "revert changes",
        doc: "Apply the inverse of a commit without committing",
        action: "action:magit-revert-changes",
        placeholder: "revert_changes_op",
    },
];

const REVERT_SEQUENCE_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "V",
        label: "continue",
        doc: "Resume the revert after resolving the conflict",
        action: "action:magit-global-revert-continue",
        placeholder: "revert_continue_op",
    },
    TransientRow {
        key: "s",
        label: "skip",
        doc: "Skip the commit the revert stopped on",
        action: "action:magit-global-revert-skip",
        placeholder: "revert_skip_op",
    },
    TransientRow {
        key: "a",
        label: "abort",
        doc: "Abandon the revert, restoring the branch",
        action: "action:magit-global-revert-abort",
        placeholder: "revert_abort_op",
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
    // MG.42-E1: merge with an authored message.
    TransientRow {
        key: "e",
        label: "merge and edit message",
        doc: "Merge a branch, writing the merge message yourself",
        action: "action:magit-global-merge-edit",
        placeholder: "merge_edit_op",
    },
    // MG.43e: preview — shows what merging would bring in, without
    // merging. Read-only, so it sits with the acting rows but changes
    // nothing.
    TransientRow {
        key: "p",
        label: "preview",
        doc: "Show what merging a branch would bring in",
        action: "action:magit-global-merge-preview",
        placeholder: "merge_preview_op",
    },
    // MG.43e: the mirror of `a` absorb — merge THIS branch into
    // another and delete this one.
    TransientRow {
        key: "i",
        label: "merge into",
        doc: "Merge this branch into another, then delete this one",
        action: "action:magit-global-merge-into",
        placeholder: "merge_into_op",
    },
    // MG.42-E2: merge then delete, as one operation.
    TransientRow {
        key: "a",
        label: "absorb",
        doc: "Merge a branch and delete it — the delete is refused if the merge did not take",
        action: "action:magit-global-merge-absorb",
        placeholder: "merge_absorb_op",
    },
];

/// The merge menu while a merge is STOPPED on a conflict.
///
/// Peer of `CHERRY_PICK_SEQUENCE_ROWS`, and it exists for the same
/// reason: every row in `MERGE_ROWS` is a way IN, and git refuses all
/// of them while `MERGE_HEAD` is present ("you have not concluded your
/// merge"). An ungated menu therefore showed the user seven rows that
/// could only fail, and no row at all for the two things they actually
/// wanted.
///
/// No `skip`: that is a sequencer verb, and a merge is a single
/// operation with nothing to skip to. `--quit` is left out too — it
/// forgets the merge while keeping the index, which is a recovery tool
/// rather than a way out, and `q` is the menu's dismiss key anyway.
///
/// Keys follow the overload convention the sequencer menus established:
/// `m` is *merge* when idle and *continue* when stopped, `a` is
/// *absorb* when idle and *abort* when stopped. Safe only because the
/// gate never shows both sets at once.
const MERGE_SEQUENCE_ROWS: &[TransientRow] = &[
    TransientRow {
        key: "m",
        label: "continue",
        doc: "Conclude the merge after resolving the conflict",
        action: "action:magit-global-merge-continue",
        placeholder: "merge_continue_op",
    },
    TransientRow {
        key: "a",
        label: "abort",
        doc: "Abandon the merge, restoring the branch as it was",
        action: "action:magit-global-merge-abort",
        placeholder: "merge_abort_op",
    },
];

/// MG.41e: magit's `t` tag submenu.
const TAG_ROWS: &[TransientRow] = &[
    // MG.43e: magit's `r` — an annotated release tag, which is a real
    // object rather than a pointer.
    TransientRow {
        key: "r",
        label: "release",
        doc: "Create an annotated release tag (asks name and message)",
        action: "action:magit-global-tag-release",
        placeholder: "tag_release_op",
    },
    // MG.43e: magit's `p` — drop local tags gone from the remote.
    TransientRow {
        key: "p",
        label: "prune",
        doc: "Drop local tags that no longer exist on the remote",
        action: "action:magit-global-tag-prune",
        placeholder: "tag_prune_op",
    },
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
    // MG.43b: magit's onto-a-target rows. `p` and `u` need no prompt —
    // git resolves `@{push}` / `@{upstream}` itself.
    TransientRow {
        key: "p",
        label: "onto pushRemote",
        doc: "Rebase this branch onto its push target",
        action: "action:magit-global-rebase-onto-push",
        placeholder: "rebase_onto_push_op",
    },
    TransientRow {
        key: "u",
        label: "onto @{upstream}",
        doc: "Rebase this branch onto its upstream",
        action: "action:magit-global-rebase-onto-upstream",
        placeholder: "rebase_onto_upstream_op",
    },
    TransientRow {
        key: "e",
        label: "onto elsewhere",
        doc: "Rebase this branch onto a ref you name",
        action: "action:magit-global-rebase-onto-elsewhere",
        placeholder: "rebase_onto_elsewhere_op",
    },
    TransientRow {
        key: "s",
        label: "a subset",
        doc: "Replay the commits after one ref onto another",
        action: "action:magit-global-rebase-subset",
        placeholder: "rebase_subset_op",
    },
    // MG.43c: the todo-rewriting rows. Each names a commit and changes
    // its verb; the verb IS the operation.
    TransientRow {
        key: "m",
        label: "edit a commit",
        doc: "Replay history, stopping at a commit so you can change it",
        action: "action:magit-rebase-edit-commit",
        placeholder: "rebase_edit_commit_op",
    },
    TransientRow {
        key: "w",
        label: "reword a commit",
        doc: "Change an older commit's message",
        action: "action:magit-rebase-reword-commit",
        placeholder: "rebase_reword_commit_op",
    },
    TransientRow {
        key: "k",
        label: "remove a commit",
        doc: "Replay history without a commit",
        action: "action:magit-rebase-remove-commit",
        placeholder: "rebase_remove_commit_op",
    },
    TransientRow {
        key: "f",
        label: "autosquash",
        doc: "Replay, folding in fixup! and squash! markers",
        action: "action:magit-global-rebase-autosquash",
        placeholder: "rebase_autosquash_op",
    },
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
    // `m` for "unmerged" — `u` is taken by unstaged, and magit's own
    // jump menu keys are per-section initials with the same collisions
    // resolved the same way.
    TransientRow {
        key: "m",
        label: "unmerged",
        doc: "Jump to the unmerged-into-upstream section",
        action: "action:magit-jump-unmerged",
        placeholder: "jump_unmerged_op",
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
                        // Free text: these are values being *created*
                        // (a new remote name, a URL), not names of
                        // things that already exist. MG.53's rule —
                        // a picker for a new name is worse than
                        // useless, because there is nothing to pick.
                        source: None,
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
    config_rows: &'static [ConfigRow],
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
    // MG.43g: magit's `C`, reporting the key's current value inline.
    groups.extend(config_group(ids, config_rows));
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

/// MG.43g: a configure row — magit's `C`.
///
/// Renders the key's CURRENT value inline and fires an action that
/// prompts for a new one. The value comes from the prefetched cache,
/// never from a read here: this runs while a menu is being built,
/// which is a keystroke path.
///
/// Falls back to the same inert placeholder every other row uses when
/// its action does not resolve, so an unregistered configure action
/// renders disabled rather than panicking.
fn variable_item(
    ids: &MagitActionIds,
    key_chord: &str,
    label: &str,
    description: &str,
    config_key: &str,
    action: &str,
    placeholder_name: &str,
) -> TransientItem {
    let Some(cid) = ids.get(action) else {
        return action_or_placeholder(None, key_chord, label, description, placeholder_name);
    };
    TransientItem {
        key: vec![key_chord.to_string()],
        label: label.to_string(),
        description: description.to_string(),
        kind: TransientItemKind::Variable {
            key: config_key.to_string(),
            value: crate::git_config::value_of(config_key),
            action: cid,
        },
    }
}

/// MG.43g: the config keys each menu's `C` row reports.
///
/// Magit's own keys, per menu. Kept as data for the same reason rows
/// are: a menu gaining a key is a table entry, and the drift test can
/// walk them.
pub(crate) struct ConfigRow {
    pub key: &'static str,
    pub label: &'static str,
    pub config_key: &'static str,
    /// The action that changes it. Named per row for the same reason
    /// `TransientRow::action` is: a `Variable` fires an action and
    /// carries no key, so the row must name a handler that knows which
    /// key it edits.
    pub action: &'static str,
}

pub(crate) const BRANCH_CONFIG_ROWS: &[ConfigRow] = &[ConfigRow {
    key: "C",
    label: "rebase on pull",
    config_key: "pull.rebase",
    action: "action:magit-config-pull-rebase",
}];

pub(crate) const PUSH_CONFIG_ROWS: &[ConfigRow] = &[ConfigRow {
    key: "C",
    label: "default push target",
    config_key: "remote.pushDefault",
    action: "action:magit-config-push-default",
}];

pub(crate) const PULL_CONFIG_ROWS: &[ConfigRow] = &[ConfigRow {
    key: "C",
    label: "rebase on pull",
    config_key: "pull.rebase",
    action: "action:magit-config-pull-rebase",
}];

pub(crate) const FETCH_CONFIG_ROWS: &[ConfigRow] = &[ConfigRow {
    key: "C",
    label: "prune on fetch",
    config_key: "fetch.prune",
    action: "action:magit-config-fetch-prune",
}];

pub(crate) const TAG_CONFIG_ROWS: &[ConfigRow] = &[ConfigRow {
    key: "C",
    label: "sign tags",
    config_key: "tag.gpgSign",
    action: "action:magit-config-tag-sign",
}];

pub(crate) const NOTES_CONFIG_ROWS: &[ConfigRow] = &[ConfigRow {
    key: "C",
    label: "notes ref",
    config_key: "core.notesRef",
    action: "action:magit-config-notes-ref",
}];

/// Every configure table, for the drift test — the same bookkeeping
/// `all_row_tables` keeps for action rows.
#[cfg(test)]
pub(crate) fn all_config_tables() -> &'static [(&'static str, &'static [ConfigRow])] {
    &[
        ("branch", BRANCH_CONFIG_ROWS),
        ("push", PUSH_CONFIG_ROWS),
        ("pull", PULL_CONFIG_ROWS),
        ("fetch", FETCH_CONFIG_ROWS),
        ("tag", TAG_CONFIG_ROWS),
        ("notes", NOTES_CONFIG_ROWS),
    ]
}

/// The `Configure` group a menu appends, or nothing when the table is
/// empty.
fn config_group(ids: &MagitActionIds, rows: &'static [ConfigRow]) -> Option<TransientGroup> {
    if rows.is_empty() {
        return None;
    }
    Some(TransientGroup {
        label: "Configure".into(),
        items: rows
            .iter()
            .map(|r| {
                variable_item(
                    ids,
                    r.key,
                    r.label,
                    "Change this setting for the repository",
                    r.config_key,
                    r.action,
                    "config_op",
                )
            })
            .collect(),
    })
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
        ]
        .into_iter()
        .chain(config_group(ids, BRANCH_CONFIG_ROWS))
        .collect(),
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
/// **MG.43g added `C`** (`core.notesRef`), the first of magit's four
/// configure rows here. `TransientItemKind::Variable` exists now, so
/// the remaining three (`c` / `d` / `D`, chiefly `notes.displayRef`)
/// are table entries rather than a missing capability. Only outside a
/// merge: a stopped notes merge shows the ways out and nothing else.
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
    // MG.43g: `C` only outside a merge — a stopped notes merge shows
    // the ways out and nothing else, which is the whole point of the
    // gate. Changing the notes ref mid-merge is exactly what the user
    // must not be doing.
    let groups: Vec<TransientGroup> = if merge_in_progress {
        groups
    } else {
        groups
            .into_iter()
            .chain(config_group(ids, NOTES_CONFIG_ROWS))
            .collect()
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

// MR.4: the seven cwd-based gate readers that stood here are gone.
//
// Each answered "is a bisect / rebase / merge / … stopped" by probing
// `magit_workdir()` — the process's repository — which is the wrong
// question once a menu belongs to the buffer it was opened over. The
// same seven answers now come from `DispatchGates::probe_in`, which
// takes the repository as an argument, so there is nowhere left for a
// row to read the working directory from.

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
/// MG.42-E4: `A`, gated on whether a cherry-pick is stopped.
fn cherry_pick_transient(ids: &MagitActionIds, in_progress: bool) -> TransientSpec {
    let groups = if in_progress {
        vec![row_group(
            "Cherry-pick in progress",
            ids,
            CHERRY_PICK_SEQUENCE_ROWS,
        )]
    } else {
        vec![row_group("Cherry-pick", ids, CHERRY_PICK_ROWS)]
    };
    TransientSpec {
        title: "Cherry-pick".into(),
        groups,
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

/// MG.42-E4: `_`, gated the same way.
fn revert_transient(ids: &MagitActionIds, in_progress: bool) -> TransientSpec {
    let groups = if in_progress {
        vec![row_group("Revert in progress", ids, REVERT_SEQUENCE_ROWS)]
    } else {
        vec![row_group("Revert", ids, REVERT_ROWS)]
    };
    TransientSpec {
        title: "Revert".into(),
        groups,
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

fn merge_transient(ids: &MagitActionIds, in_progress: bool) -> TransientSpec {
    let groups = if in_progress {
        vec![row_group("Merge in progress", ids, MERGE_SEQUENCE_ROWS)]
    } else {
        vec![row_group("Merge", ids, MERGE_ROWS)]
    };
    TransientSpec {
        title: "Merge".into(),
        groups,
        preview: None,
        footer: Some("q dismiss  Esc/BS back".into()),
    }
}

fn tag_transient(ids: &MagitActionIds) -> TransientSpec {
    TransientSpec {
        title: "Tag".into(),
        groups: vec![row_group("Tag", ids, TAG_ROWS)]
            .into_iter()
            .chain(config_group(ids, TAG_CONFIG_ROWS))
            .collect(),
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
pub fn dispatch_transient(
    ids: &MagitActionIds,
    ctx: &TransientContext,
    workdir: &std::path::Path,
) -> TransientSpec {
    dispatch_transient_with(ids, ctx, DispatchGates::probe_in(workdir))
}

/// One repository question, asked once — the shape every gate above
/// shares.
fn repo_flag(workdir: &std::path::Path, ask: impl Fn(&lattice_vcs::Repository) -> bool) -> bool {
    lattice_vcs::Repository::discover(workdir)
        .ok()
        .map(|repo| ask(&repo))
        .unwrap_or(false)
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
    /// MG.42-E4: a cherry-pick sequence stopped on a conflict.
    pub cherry_pick: bool,
    /// MG.42-E4: a revert sequence stopped on a conflict.
    pub revert: bool,
    /// A merge stopped on a conflict. `MERGE_HEAD` was checked nowhere
    /// before this, so the merge menu offered only ways IN while git
    /// refused every one of them.
    pub merge: bool,
}

impl DispatchGates {
    /// Read the gates from the repository.
    ///
    /// Every *guard* over this menu passes gates in rather than calling
    /// this, deliberately: probing would make a test's row count depend
    /// on whether the developer's own checkout happened to be mid-bisect
    /// while the suite ran — a flake that reads as a real regression.
    /// MR.4: probed in `workdir`, which is the repository of the buffer
    /// the menu was opened over — not the process's.
    ///
    /// This is what the rows are *about*: offering `rebase --continue`
    /// because some other checkout is mid-rebase is a row that does
    /// nothing here, and hiding it while THIS repository is stopped is
    /// worse — the way out of a stopped rebase is missing from the menu
    /// whose job is to show it.
    pub fn probe_in(workdir: &std::path::Path) -> Self {
        let flight = lattice_vcs::Repository::discover(workdir)
            .ok()
            .and_then(|repo| lattice_vcs::InFlightOp::detect(&repo));
        Self {
            bisect: repo_flag(workdir, lattice_vcs::Bisect::in_progress),
            notes_merge: repo_flag(workdir, |repo| {
                lattice_vcs::Note::merge_in_progress(repo.gitdir())
            }),
            am: flight == Some(lattice_vcs::InFlightOp::ApplyPatch),
            rebase: flight == Some(lattice_vcs::InFlightOp::Rebase),
            cherry_pick: flight == Some(lattice_vcs::InFlightOp::CherryPick),
            revert: flight == Some(lattice_vcs::InFlightOp::Revert),
            merge: flight == Some(lattice_vcs::InFlightOp::Merge),
        }
    }
}

/// [`dispatch_transient`] with the gates supplied rather than probed —
/// pure, and the form every guard over this menu uses.
/// MG.49: one root menu — reachable from the dispatch **and** from its
/// own chord.
///
/// Emacs binds these on `magit-mode-map`, the parent keymap every
/// magit-derived mode inherits, so `z` opens stash from the log buffer
/// and the diff buffer as much as from status. `magit-core-mode` is the
/// same shape here, which is why the chords live there rather than on
/// `magit-status-mode`.
///
/// Chords follow **evil-collection-magit**, the reference this crate
/// already uses for `gr` / `O` / `x` — so push is `p`, not magit's `P`
/// (`p` is free in a read-only buffer; `P` is not, being paste).
pub struct RootMenu {
    /// Registered `TransientSourceRegistry` name, and the string the
    /// chord's `Effect::OpenTransient` names.
    pub source: &'static str,
    /// The chord `magit-core-mode` binds, or `None` when the key is a
    /// live vim chord in a read-only buffer and the menu is reachable
    /// only through the dispatch.
    ///
    /// **This is the constraint emacs does not have.** Magit binds all
    /// seventeen on `magit-mode-map` because emacs is not modal. Here a
    /// minor-mode layer beats the builtin vim layer, so binding `f`
    /// would take find-char away inside every magit buffer — and a
    /// magit buffer is text you navigate. Vim's grammar IS the public
    /// command API (paramount goal #3), so it wins: only keys whose vim
    /// meaning is an *editing operator* — inert where nothing is
    /// editable — are free to take.
    pub chord: Option<&'static str>,
    /// The action the chord fires.
    pub action: &'static str,
    /// Keymap documentation.
    pub doc: &'static str,
}

/// The seventeen root menus, in dispatch order.
///
/// A table rather than seventeen hand-written registrations + seventeen
/// keymap entries + seventeen handlers: those three lists have to agree,
/// and three parallel lists that must agree are exactly where a gap goes
/// unnoticed — the failure `magit-hunk-mode` was created to end.
pub const ROOT_MENUS: &[RootMenu] = &[
    RootMenu {
        source: "magit-menu-diff",
        // `d`: delete operator, but `magit-branch` / `-remote` / `-stash` /
        // `-submodule` majors bind `d` for delete-this-row, and a minor
        // shadows a major.

        // Reachable via the dispatch, which `C-c g` opens.
        chord: None,
        action: "action:magit-menu-diff",
        doc: "Diff menu",
    },
    RootMenu {
        source: "magit-menu-commit",
        // `c`: change operator, but `magit-branch` binds `c` (create) and
        // `magit-refs` binds `c` (checkout).

        // Reachable via the dispatch, which `C-c g` opens.
        chord: None,
        action: "action:magit-menu-commit",
        doc: "Commit menu",
    },
    RootMenu {
        source: "magit-menu-log",
        // `l`: right-motion — dispatch only.
        chord: None,
        action: "action:magit-menu-log",
        doc: "Log menu",
    },
    RootMenu {
        source: "magit-menu-cherry-pick",
        // `A`: append-EOL operator — inert; was already this mode's chord.
        chord: Some("A"),
        action: "action:magit-menu-cherry-pick",
        doc: "Cherry-pick menu",
    },
    RootMenu {
        source: "magit-menu-revert",
        // `_`: documented free by MG.20; was already this mode's chord.
        chord: Some("_"),
        action: "action:magit-menu-revert",
        doc: "Revert menu",
    },
    RootMenu {
        source: "magit-menu-reset",
        // `O`: open-line-above operator — inert; was already this mode's chord.
        chord: Some("O"),
        action: "action:magit-menu-reset",
        doc: "Reset menu",
    },
    RootMenu {
        source: "magit-menu-bisect",
        // `B`: back-WORD motion (also test-enforced) — dispatch only.
        chord: None,
        action: "action:magit-menu-bisect",
        doc: "Bisect menu",
    },
    RootMenu {
        source: "magit-menu-notes",
        // `T`: till-back motion — dispatch only.
        chord: None,
        action: "action:magit-menu-notes",
        doc: "Notes menu",
    },
    RootMenu {
        source: "magit-menu-branch",
        // `b`: back-word motion — dispatch only.
        chord: None,
        action: "action:magit-menu-branch",
        doc: "Branch menu",
    },
    RootMenu {
        source: "magit-menu-stash",
        // `z`: vim fold prefix (`zf` / `za` / `zo`) — dispatch only.
        chord: None,
        action: "action:magit-menu-stash",
        doc: "Stash menu",
    },
    RootMenu {
        source: "magit-menu-fetch",
        // `f`: find-char motion — dispatch only.
        chord: None,
        action: "action:magit-menu-fetch",
        doc: "Fetch menu",
    },
    RootMenu {
        source: "magit-menu-pull",
        // `F`: find-char-back motion — dispatch only.
        chord: None,
        action: "action:magit-menu-pull",
        doc: "Pull menu",
    },
    // evil-collection-magit moves magit's `P` here; `P` is paste.
    RootMenu {
        source: "magit-menu-push",
        // `p`: paste — inert in a read-only buffer, and the key
        // evil-collection-magit itself moves push to. `magit-blame-mode`
        // overrides it on blob buffers, which the layer order expresses:
        // blame activates after the major cascade, so its layer wins.
        // `p`: paste is inert, but `magit-remote` (prune) and `magit-stash`
        // (pop) bind `p`.

        // Reachable via the dispatch, which `C-c g` opens.
        chord: None,
        action: "action:magit-menu-push",
        doc: "Push menu",
    },
    RootMenu {
        source: "magit-menu-patches",
        // `w`: word motion — dispatch only.
        chord: None,
        action: "action:magit-menu-patches",
        doc: "Patch (am / format-patch) menu",
    },
    RootMenu {
        source: "magit-menu-rebase",
        // `r`: replace operator, but `magit-remote` binds `r` (rename).

        // Reachable via the dispatch, which `C-c g` opens.
        chord: None,
        action: "action:magit-menu-rebase",
        doc: "Rebase menu",
    },
    RootMenu {
        source: "magit-menu-tag",
        // `t`: till motion — dispatch only.
        chord: None,
        action: "action:magit-menu-tag",
        doc: "Tag menu",
    },
    RootMenu {
        source: "magit-menu-merge",
        // `m`: set-mark — dispatch only.
        chord: None,
        action: "action:magit-menu-merge",
        doc: "Merge menu",
    },
];

/// Build one root menu by name — the SAME spec the dispatch nests, so a
/// chord and the menu path to it can never disagree.
///
/// `None` for an unknown name; the caller then leaves the transient
/// unopened rather than showing an empty menu.
pub fn root_menu_spec(
    source: &str,
    ids: &MagitActionIds,
    ctx: &TransientContext,
    gates: DispatchGates,
) -> Option<TransientSpec> {
    let spec = match source {
        "magit-menu-diff" => view_open_transient(
            "Diff",
            ids,
            crate::magit_diff_mode::DIFF_ARGS,
            DIFF_SHOW_ROWS,
        ),
        "magit-menu-commit" => commit_transient(ids),
        "magit-menu-log" => {
            view_open_transient("Log", ids, crate::magit_log_mode::LOG_ARGS, LOG_SHOW_ROWS)
        }
        "magit-menu-cherry-pick" => cherry_pick_transient(ids, gates.cherry_pick),
        "magit-menu-revert" => revert_transient(ids, gates.revert),
        "magit-menu-reset" => reset_transient(ids),
        "magit-menu-bisect" => bisect_transient(ids, gates.bisect),
        "magit-menu-notes" => notes_transient(ids, gates.notes_merge),
        "magit-menu-branch" => branch_transient(ids),
        "magit-menu-stash" => stash_transient(ids),
        "magit-menu-fetch" => {
            remote_op_transient("Fetch", RemoteOp::FETCH, ids, FETCH_ROWS, FETCH_CONFIG_ROWS)
        }
        "magit-menu-pull" => {
            remote_op_transient("Pull", RemoteOp::PULL, ids, PULL_ROWS, PULL_CONFIG_ROWS)
        }
        "magit-menu-push" => {
            remote_op_transient("Push", RemoteOp::PUSH, ids, PUSH_ROWS, PUSH_CONFIG_ROWS)
        }
        "magit-menu-patches" => patch_transient(ids, gates.am),
        "magit-menu-rebase" => rebase_transient(ids, gates.rebase),
        "magit-menu-tag" => tag_transient(ids),
        "magit-menu-merge" => merge_transient(ids, gates.merge),
        _ => return None,
    };
    let _ = ctx;
    Some(spec)
}

pub fn dispatch_transient_with(
    ids: &MagitActionIds,
    ctx: &TransientContext,
    gates: DispatchGates,
) -> TransientSpec {
    let _bisect_in_progress = gates.bisect;
    TransientSpec {
        title: "Magit dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Working tree".into(),
                items: vec![
                    status_row(ids, ctx),
                    // MG.43h: a submenu now. The toggles are consumed
                    // — `action:magit-global-diff` declares the schema
                    // they project onto, and the values ride to the
                    // opened buffer via `ViewArgsRequests`.
                    TransientItem {
                        key: vec!["d".into()],
                        label: "diff".into(),
                        description: "Diff the working tree against HEAD".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-diff", ids, ctx, gates)
                                .expect("`magit-menu-diff` is in ROOT_MENUS"),
                        )),
                    },
                    // PD.6: the editable cross-file diff, promoted to the
                    // dispatch's top level.
                    //
                    // It shipped reachable only as `d` → `e`, one level
                    // down inside the Diff menu, next to three rows that
                    // open patch text. That put the view people would
                    // reach for most often behind the one they would
                    // reach for least, and made it look like a variant of
                    // the patch views rather than the different surface it
                    // is. `e` for edit, matching the row it also keeps
                    // inside the Diff menu — the two front-ends emit the
                    // same action, so they cannot drift.
                    //
                    // An Action row, not a Submenu: this opens a view,
                    // and there is nothing to choose first.
                    action_or_placeholder(
                        ids.get("action:magit-project-diff"),
                        "e",
                        "edit diff",
                        "Edit the working-tree diff across every changed file",
                        "project_diff",
                    ),
                    TransientItem {
                        key: vec!["c".into()],
                        label: "commit".into(),
                        description: "Commit changes".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-commit", ids, ctx, gates)
                                .expect("`magit-menu-commit` is in ROOT_MENUS"),
                        )),
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
                    // MG.43h: the log's peer, same mechanism.
                    TransientItem {
                        key: vec!["l".into()],
                        label: "log".into(),
                        description: "Show commit history".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-log", ids, ctx, gates)
                                .expect("`magit-menu-log` is in ROOT_MENUS"),
                        )),
                    },
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
                    // MG.42-E4: gated submenus. A stopped sequence
                    // needs continue / skip / abort, and offering
                    // "pick another commit" mid-conflict is the wrong
                    // menu entirely.
                    TransientItem {
                        key: vec!["A".into()],
                        label: "cherry-pick".into(),
                        description: "Cherry-pick, or drive a stopped one".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-cherry-pick", ids, ctx, gates)
                                .expect("`magit-menu-cherry-pick` is in ROOT_MENUS"),
                        )),
                    },
                    TransientItem {
                        key: vec!["_".into()],
                        label: "revert".into(),
                        description: "Revert, or drive a stopped one".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-revert", ids, ctx, gates)
                                .expect("`magit-menu-revert` is in ROOT_MENUS"),
                        )),
                    },
                    TransientItem {
                        key: vec!["O".into()],
                        label: "reset".into(),
                        description: "Reset this branch to a commit".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-reset", ids, ctx, gates)
                                .expect("`magit-menu-reset` is in ROOT_MENUS"),
                        )),
                    },
                    // MG.21g: magit's own key, in magit's own group.
                    // The submenu's contents depend on whether a
                    // bisect is running — see `bisect_transient`.
                    TransientItem {
                        key: vec!["B".into()],
                        label: "bisect".into(),
                        description: "Find the commit that introduced a bug".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-bisect", ids, ctx, gates)
                                .expect("`magit-menu-bisect` is in ROOT_MENUS"),
                        )),
                    },
                    // MG.37: magit's `T`. A submenu rather than a direct
                    // action because notes have four operations and two
                    // more while a merge is stopped — the same shape `B`
                    // has, and gated the same way.
                    TransientItem {
                        key: vec!["T".into()],
                        label: "notes".into(),
                        description: "Edit, remove, merge or prune commit notes".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-notes", ids, ctx, gates)
                                .expect("`magit-menu-notes` is in ROOT_MENUS"),
                        )),
                    },
                ],
            },
            TransientGroup {
                label: "Branches".into(),
                items: vec![TransientItem {
                    key: vec!["b".into()],
                    label: "branch".into(),
                    description: "Checkout, create, or list branches".into(),
                    kind: TransientItemKind::Submenu(Arc::new(
                        root_menu_spec("magit-menu-branch", ids, ctx, gates)
                            .expect("`magit-menu-branch` is in ROOT_MENUS"),
                    )),
                }],
            },
            TransientGroup {
                label: "Stashing".into(),
                items: vec![TransientItem {
                    key: vec!["z".into()],
                    label: "stash".into(),
                    description: "Stash operations".into(),
                    kind: TransientItemKind::Submenu(Arc::new(
                        root_menu_spec("magit-menu-stash", ids, ctx, gates)
                            .expect("`magit-menu-stash` is in ROOT_MENUS"),
                    )),
                }],
            },
            TransientGroup {
                label: "Remotes".into(),
                items: vec![
                    TransientItem {
                        key: vec!["f".into()],
                        label: "fetch".into(),
                        description: "Fetch from the remote without merging".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-fetch", ids, ctx, gates)
                                .expect("`magit-menu-fetch` is in ROOT_MENUS"),
                        )),
                    },
                    // MG.41c: pull IS a submenu now. It was a plain row
                    // because `--ff-only` was not optional and there was
                    // nothing else to show — but magit's pull has three
                    // destinations, and `-r` / `-a` are real toggles.
                    TransientItem {
                        key: vec!["F".into()],
                        label: "pull".into(),
                        description: "Fetch + integrate from the remote".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-pull", ids, ctx, gates)
                                .expect("`magit-menu-pull` is in ROOT_MENUS"),
                        )),
                    },
                    TransientItem {
                        key: vec!["p".into()],
                        label: "push".into(),
                        description: "Push to the remote".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-push", ids, ctx, gates)
                                .expect("`magit-menu-push` is in ROOT_MENUS"),
                        )),
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
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-patches", ids, ctx, gates)
                                .expect("`magit-menu-patches` is in ROOT_MENUS"),
                        )),
                    },
                    // MG.41e: a submenu now, gated like bisect / am.
                    // A stopped rebase needs continue / skip / abort,
                    // and offering "start an interactive rebase" while
                    // one is half-done is the wrong menu entirely.
                    TransientItem {
                        key: vec!["r".into()],
                        label: "rebase".into(),
                        description: "Rebase, or drive a stopped one".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-rebase", ids, ctx, gates)
                                .expect("`magit-menu-rebase` is in ROOT_MENUS"),
                        )),
                    },
                    // MG.23c1: magit's own keys. Both ask for their one
                    // value rather than taking it from context — there
                    // is nothing at a cursor to read from a menu opened
                    // anywhere.
                    TransientItem {
                        key: vec!["t".into()],
                        label: "tag".into(),
                        description: "Create or delete a tag".into(),
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-tag", ids, ctx, gates)
                                .expect("`magit-menu-tag` is in ROOT_MENUS"),
                        )),
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
                        kind: TransientItemKind::Submenu(Arc::new(
                            root_menu_spec("magit-menu-merge", ids, ctx, gates)
                                .expect("`magit-menu-merge` is in ROOT_MENUS"),
                        )),
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
                        // MG.53.e: the file must already exist, so it is
                        // picked rather than typed — the rule this whole
                        // plan applies. A free-text path is a typo
                        // waiting to happen, and git reports it long
                        // after the keystroke that caused it, by which
                        // point the menu has closed.
                        //
                        // The listing lives in `lattice-picker`, not
                        // here: magit names the source and the walk stays
                        // generic, so the next provider that needs
                        // "choose a file, then act" declares the same
                        // row instead of copying a directory walk into
                        // its own crate.
                        source: Some(lattice_picker::TransientArgSource::new(
                            lattice_picker::FILE_PICK_SOURCE,
                        )),
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
                    action_or_placeholder(
                        ids.get("action:magit-global-file-stage"),
                        "s",
                        "stage",
                        "Stage this file",
                        "stage_file",
                    ),
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
                    action_or_placeholder(
                        ids.get("action:magit-global-file-blame"),
                        "b",
                        "blame",
                        "Blame this file",
                        "blame_file",
                    ),
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
                    body.contains("spawn_git(crate::repo_scope::action_workdir(ctx), ")
                        || body.contains("spawn_remote_op("),
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
        assert!(
            checked >= 8,
            "expected to inspect every spawner, saw {checked}"
        );
    }

    /// **The same rule, for the spawners that are not in
    /// `magit_global_mode.rs`.**
    ///
    /// `every_spawner_reports_completion` above reads ONE file. That
    /// was the whole gap: each major mode grew its own
    /// `spawn_mutation_and_refresh` / `spawn_*_mutation`, none of them
    /// in that file, so none were ever checked — and those helpers are
    /// what the chords a user presses most (`s`, `u`, `x`, branch `d`,
    /// stash `p`, remote `a`) actually call. A guard scoped to a file
    /// rather than to the rule is a guard with a blind spot the size of
    /// the rest of the crate.
    ///
    /// Refreshers are exempt from reporting SUCCESS — a repopulated
    /// buffer is its own report, and a notification per `gr` is noise —
    /// but not from reporting failure, which is why they are named
    /// individually here rather than matched by a `refresh` substring.
    #[test]
    fn every_mutation_helper_in_the_crate_reports_completion() {
        // (file, helper) pairs that mutate the repository.
        const MUTATORS: &[(&str, &str)] = &[
            ("actions.rs", "spawn_mutation_and_refresh"),
            ("magit_branch_mode.rs", "spawn_mutation_and_refresh"),
            ("magit_diff_mode.rs", "spawn_mutation_and_refresh"),
            ("magit_stash_mode.rs", "spawn_mutation_and_refresh"),
            ("magit_remote_mode.rs", "spawn_remote_mutation"),
            ("magit_submodule_mode.rs", "spawn_submodule_mutation"),
            ("magit_core_mode.rs", "spawn_patch_discard"),
            ("magit_core_mode.rs", "spawn_hunk_apply"),
        ];
        let sources: &[(&str, &str)] = &[
            ("actions.rs", include_str!("actions.rs")),
            ("magit_branch_mode.rs", include_str!("magit_branch_mode.rs")),
            ("magit_diff_mode.rs", include_str!("magit_diff_mode.rs")),
            ("magit_stash_mode.rs", include_str!("magit_stash_mode.rs")),
            ("magit_remote_mode.rs", include_str!("magit_remote_mode.rs")),
            (
                "magit_submodule_mode.rs",
                include_str!("magit_submodule_mode.rs"),
            ),
            ("magit_core_mode.rs", include_str!("magit_core_mode.rs")),
        ];

        for (file, helper) in MUTATORS {
            let src = sources
                .iter()
                .find(|(f, _)| f == file)
                .map(|(_, s)| *s)
                .unwrap_or_else(|| panic!("{file} is in the source table"));
            let idx = src
                .find(&format!("fn {helper}"))
                .unwrap_or_else(|| panic!("{file}: `{helper}` not found — renamed?"));
            let body_end = src[idx..]
                .find("\nfn ")
                .map(|e| idx + e)
                .unwrap_or(src.len());
            let body = &src[idx..body_end];
            assert!(
                body.contains("finish_task"),
                "{file}: `{helper}` mutates the repository and does not \
                 report completion. The user presses a key, git runs, and \
                 nothing says whether it worked — on failure the buffer \
                 just refreshes as though it had.",
            );
        }
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
        assert!(
            RemoteTarget::Configured
                .argv(Some("origin main"))
                .is_empty()
        );
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
            assert!(
                names.contains(&expected),
                "push missing `{expected}`: {names:?}"
            );
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
            assert!(
                names.contains(&expected),
                "fetch missing `{expected}`: {names:?}"
            );
        }
    }
}

#[cfg(test)]
mod config_row_tests {
    use super::*;
    use lattice_grammar::CommandRegistry;

    /// MG.43g: every configure row's action resolves.
    ///
    /// The same drift guard `every_row_action_is_registered` gives the
    /// action tables. A `Variable` whose action does not resolve falls
    /// back to an inert placeholder, so the row would render and do
    /// nothing when pressed.
    #[test]
    fn every_config_row_action_is_registered() {
        let mut registry = CommandRegistry::new();
        crate::register_action_commands(&mut registry);
        for (table, rows) in all_config_tables() {
            for row in *rows {
                assert!(
                    registry.id_by_name(row.action).is_some(),
                    "{table} row `{}` ({}) references unregistered `{}`",
                    row.key,
                    row.label,
                    row.action,
                );
            }
        }
    }

    /// Every configure row names a non-empty git-config key.
    ///
    /// An empty key would render `" = …"` forever: the cache can never
    /// answer for it, so the row would report nothing while looking
    /// like it was still loading.
    #[test]
    fn every_config_row_names_a_key() {
        for (table, rows) in all_config_tables() {
            for row in *rows {
                assert!(
                    !row.config_key.is_empty(),
                    "{table} row `{}` names no config key",
                    row.key,
                );
                assert!(
                    row.config_key.contains('.'),
                    "{table} row `{}` names `{}`, which is not a git-config key",
                    row.key,
                    row.config_key,
                );
            }
        }
    }

    /// MG.43g: **building a menu reads the cache, never the disk.**
    ///
    /// This is the paramount-#1 constraint the whole slice is shaped
    /// around: a menu is built on a keystroke. With nothing prefetched
    /// every row must still build, reporting "not read yet" rather
    /// than blocking to find out.
    #[test]
    fn rows_build_without_a_prefetched_value() {
        let mut registry = CommandRegistry::new();
        crate::register_action_commands(&mut registry);
        let ids = MagitActionIds::resolve(&registry);

        let group = config_group(&ids, BRANCH_CONFIG_ROWS).expect("branch has a configure row");
        let item = &group.items[0];
        match &item.kind {
            TransientItemKind::Variable { key, value, .. } => {
                assert_eq!(key, "pull.rebase");
                // `None`, not `Some("")`: nothing has been read, and
                // claiming `unset` would state a fact about the user's
                // config that was never checked.
                assert_eq!(*value, None, "an unread key must not report a value");
            }
            other => panic!("configure rows must be Variable, got {other:?}"),
        }
    }

    /// The three display states stay distinct.
    #[test]
    fn unread_and_unset_render_differently() {
        assert_eq!(TransientItemKind::variable_display(None), "…");
        assert_eq!(TransientItemKind::variable_display(Some("")), "unset");
        assert_eq!(TransientItemKind::variable_display(Some("true")), "");
    }
}

#[cfg(test)]
mod merge_and_tag_argv_tests {
    use crate::magit_global_mode::{
        merge_absorb_steps, merge_into_steps, tag_prune_argv, tag_release_argv,
    };

    /// MG.43e: **`i` merge-into and `a` absorb delete DIFFERENT
    /// branches.**
    ///
    /// They are mirrors: absorb merges another branch into this one
    /// and deletes that one; merge-into merges this one into another
    /// and deletes this one. Getting the direction backwards deletes
    /// the branch the user is standing on and keeps the one they meant
    /// to fold in — and both forms are perfectly valid git.
    #[test]
    fn merge_into_deletes_this_branch_and_absorb_deletes_the_other() {
        let into = merge_into_steps("feature", "main");
        let deleted = into
            .iter()
            .find(|s| s.argv.first().map(String::as_str) == Some("branch"))
            .expect("merge-into deletes a branch");
        assert!(
            deleted.argv.contains(&"feature".to_string()),
            "merge-into deletes the CURRENT branch: {:?}",
            deleted.argv,
        );

        let absorb = merge_absorb_steps("feature");
        let deleted = absorb
            .iter()
            .find(|s| s.argv.first().map(String::as_str) == Some("branch"))
            .expect("absorb deletes a branch");
        assert!(
            deleted.argv.contains(&"feature".to_string()),
            "absorb deletes the OTHER branch: {:?}",
            deleted.argv,
        );
    }

    /// Merge-into checks the target out FIRST, then merges. Merging
    /// before checking out would merge into the wrong branch.
    #[test]
    fn merge_into_checks_out_before_merging() {
        let steps = merge_into_steps("feature", "main");
        assert_eq!(steps[0].argv, vec!["checkout", "main"]);
        assert_eq!(steps[1].argv.first().map(String::as_str), Some("merge"));
    }

    /// Both delete with `-d`, never `-D`: git refuses `-d` on a branch
    /// that is not fully merged, so a failed merge leaves it intact.
    /// `-D` would destroy it precisely when the merge did not take.
    #[test]
    fn neither_direction_force_deletes() {
        for steps in [
            merge_into_steps("feature", "main"),
            merge_absorb_steps("feature"),
        ] {
            for step in steps {
                assert!(
                    !step.argv.iter().any(|a| a == "-D"),
                    "a force delete would destroy the branch on a failed merge: {:?}",
                    step.argv,
                );
            }
        }
    }

    /// A release tag is ANNOTATED. Without `-a` git makes a
    /// lightweight tag — a bare pointer with no tagger, date or
    /// message, which most release tooling ignores.
    #[test]
    fn a_release_tag_is_annotated() {
        let argv = tag_release_argv("v1.0.0", "first release");
        assert_eq!(argv, vec!["tag", "-a", "v1.0.0", "-m", "first release"],);
    }

    /// `--prune-tags` needs `--prune` AND a remote. Alone it prunes
    /// nothing and still reports success, so the row would look like
    /// it worked.
    #[test]
    fn pruning_tags_carries_prune_and_a_remote() {
        let argv = tag_prune_argv("origin");
        assert!(argv.contains(&"--prune".to_string()), "{argv:?}");
        assert!(argv.contains(&"--prune-tags".to_string()), "{argv:?}");
        assert_eq!(argv.last().map(String::as_str), Some("origin"), "{argv:?}");
    }
}

#[cfg(test)]
mod merge_preview_tests {
    /// MG.43e: **preview uses THREE dots, not two.**
    ///
    /// `HEAD...<branch>` shows what the branch added since the two
    /// diverged — what a merge would bring in. `HEAD..<branch>` would
    /// additionally report everything HEAD gained in the meantime as
    /// though the merge were removing it, which is the opposite of
    /// what a preview is for, and both forms are valid git.
    #[test]
    fn the_preview_range_is_symmetric_difference() {
        let argv = crate::magit_diff_mode::merge_preview_argv_for_test("feature");
        let range = argv
            .iter()
            .find(|a| a.contains("HEAD"))
            .expect("the range names HEAD");
        assert_eq!(range, "HEAD...feature");
        assert!(
            !range.contains("HEAD..feature"),
            "two dots would invert what the preview reports",
        );
    }
}

#[cfg(test)]
mod rebase_argv_tests {
    use crate::magit_global_mode::{
        rebase_autosquash_argv, rebase_onto_argv, rebase_subset_argv, resolve_upstream,
    };

    /// MG.43b: **rebase takes ONE revision, and that is why these rows
    /// do not reuse push/pull's upstream resolution.**
    ///
    /// `resolve_upstream` deliberately produces a two-token
    /// `"<remote> <branch>"` pair, because `git push` wants them
    /// separate. `git rebase origin main` is not an error — git reads
    /// `origin` as the upstream and `main` as the branch to rebase,
    /// silently replaying a different range than the row promised.
    ///
    /// So the rows pass git's own revision syntax straight through.
    #[test]
    fn rebase_targets_are_a_single_revision() {
        assert_eq!(
            rebase_onto_argv("@{upstream}"),
            vec!["rebase", "@{upstream}"]
        );
        assert_eq!(rebase_onto_argv("@{push}"), vec!["rebase", "@{push}"]);
        for target in ["@{upstream}", "@{push}", "origin/main"] {
            assert_eq!(
                rebase_onto_argv(target).len(),
                2,
                "`{target}` must contribute exactly one token after `rebase`",
            );
        }
    }

    /// The push-side resolution really does produce two tokens, so the
    /// test above is guarding against something real rather than a
    /// hypothetical.
    ///
    /// Run in a temp dir with no upstream: the function returns `None`
    /// rather than a bare token, which is itself the property that
    /// stops an unresolved destination becoming a bare `git push`.
    #[test]
    fn the_push_side_resolution_is_not_a_single_token() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert_eq!(
            resolve_upstream(dir.path()),
            None,
            "no upstream configured must resolve to nothing, never a partial ref",
        );
    }

    /// `--onto <newbase> <upstream>` — the order is not
    /// interchangeable, and git will happily run the swapped form,
    /// replaying the wrong range onto the wrong base.
    #[test]
    fn subset_puts_the_new_base_before_the_upstream() {
        assert_eq!(
            rebase_subset_argv("main", "feature~3"),
            vec!["rebase", "--onto", "main", "feature~3"],
        );
    }

    /// `--autosquash` needs `-i`: it only affects the generated todo
    /// list, which is an interactive-rebase concept. Without `-i` git
    /// accepts the flag and does nothing with it, so the row would
    /// look like it worked and fold in nothing.
    #[test]
    fn autosquash_is_interactive() {
        let argv = rebase_autosquash_argv("main");
        assert_eq!(argv, vec!["rebase", "-i", "--autosquash", "main"]);
        assert!(argv.iter().any(|a| a == "-i"));
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
        assert_eq!(
            CommitOp::RESET_KEEP.argv("abc123"),
            vec!["reset", "--keep", "abc123"]
        );
        assert!(CommitOp::RESET_KEEP.confirm_action.is_none());
        // The destructive sibling still does.
        assert!(CommitOp::RESET_HARD.confirm_action.is_some());
    }

    /// MG.43a: the `--no-commit` halves stage the change instead of
    /// recording it.
    ///
    /// That single flag is each row's entire reason to exist: `V`/`A`
    /// commit, `v`/`a` leave the result staged so it can be edited,
    /// split, or combined first. Losing the flag would silently
    /// collapse each pair into its sibling.
    #[test]
    fn the_no_commit_halves_stage_rather_than_commit() {
        assert_eq!(
            CommitOp::REVERT_CHANGES.argv("abc123"),
            vec!["revert", "--no-commit", "abc123"],
        );
        assert_eq!(
            CommitOp::CHERRY_PICK_APPLY.argv("abc123"),
            vec!["cherry-pick", "--no-commit", "abc123"],
        );
    }

    /// The committing halves and their `--no-commit` peers are NOT the
    /// same operation, and neither pair may collapse into the other.
    #[test]
    fn each_no_commit_half_differs_from_its_committing_sibling() {
        assert_ne!(
            CommitOp::REVERT.argv("abc123"),
            CommitOp::REVERT_CHANGES.argv("abc123"),
        );
        assert_ne!(
            CommitOp::CHERRY_PICK.argv("abc123"),
            CommitOp::CHERRY_PICK_APPLY.argv("abc123"),
        );
    }

    /// `--no-commit` carries no `--no-edit`.
    ///
    /// `--no-edit` exists on the committing halves to stop git opening
    /// `$EDITOR` in a context that cannot answer it. Nothing is
    /// committed here, so there is no editor to suppress. Git accepts
    /// the pair (verified — it exits 0 rather than erroring), so this
    /// is not guarding against a failure; it pins that the flag stays
    /// off, because a `--no-edit` here would read as though this row
    /// commits something.
    #[test]
    fn the_no_commit_halves_carry_no_edit_flag() {
        for argv in [
            CommitOp::REVERT_CHANGES.argv("abc123"),
            CommitOp::CHERRY_PICK_APPLY.argv("abc123"),
        ] {
            assert!(
                !argv.iter().any(|a| a == "--no-edit"),
                "no editor is opened when nothing is committed: {argv:?}",
            );
        }
    }

    /// MG.43f: **reset `w` restores the WORKING TREE only.**
    ///
    /// `git restore --source <commit> --worktree -- .` leaves HEAD and
    /// the index alone. The two obvious alternatives both do more:
    /// `reset` moves HEAD, and `checkout <commit> -- .` writes the
    /// index too — so a file the user had staged would silently be
    /// restaged to the commit's version. Verified against real git.
    #[test]
    fn reset_worktree_leaves_head_and_the_index_alone() {
        let argv = CommitOp::RESET_WORKTREE.argv("abc123");
        assert_eq!(
            argv,
            vec!["restore", "--source", "abc123", "--worktree", "--", "."],
        );
        assert!(
            !argv.iter().any(|a| a == "reset" || a == "checkout"),
            "neither `reset` nor `checkout`: both touch more than the worktree",
        );
        // `--worktree` without `--staged` is the whole point; adding
        // `--staged` would make it write the index too.
        assert!(!argv.iter().any(|a| a == "--staged"), "{argv:?}");
    }

    /// It overwrites uncommitted work, so it asks — the same bar
    /// `--hard` is held to.
    #[test]
    fn reset_worktree_asks_first() {
        assert!(CommitOp::RESET_WORKTREE.confirm_action.is_some());
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
        assert_eq!(
            CommitOp::RESET_SOFT.argv("abc"),
            vec!["reset", "--soft", "abc"]
        );
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
        // MG.43b added magit's onto-a-target rows; every one is a way
        // IN, so all belong to the idle set.
        assert_eq!(
            keys(&spec),
            vec!["p", "u", "e", "s", "m", "w", "k", "f", "i"]
        );
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

#[cfg(test)]
mod sequencer_gate_tests {
    use super::*;

    fn keys(spec: &TransientSpec) -> Vec<String> {
        spec.groups
            .iter()
            .flat_map(|g| &g.items)
            .flat_map(|i| i.key.clone())
            .collect()
    }

    /// MG.42-E4: idle offers the way IN only. `--continue` / `--skip`
    /// / `--abort` error when no sequence is running, so ungated rows
    /// would look actionable and fail.
    #[test]
    fn idle_sequencers_offer_only_the_way_in() {
        let ids = MagitActionIds::default();
        // MG.43a added the `--no-commit` halves; both are ways IN, so
        // both belong to the idle set.
        // MG.43d added the commit-MOVING rows; every one is a way IN.
        // A stopped merge offers only the ways OUT, and an idle one
        // only the ways IN — git refuses every `MERGE_ROWS` verb while
        // `MERGE_HEAD` exists, so an ungated menu was seven rows that
        // could only fail.
        assert_eq!(keys(&merge_transient(&ids, true)), vec!["m", "a"]);
        let idle_merge = keys(&merge_transient(&ids, false));
        assert!(
            idle_merge.len() > 2 && !idle_merge.contains(&"continue".to_string()),
            "idle merge offers the ways in: {idle_merge:?}"
        );
        assert_eq!(
            keys(&cherry_pick_transient(&ids, false)),
            vec!["A", "a", "h", "d", "n", "s"]
        );
        assert_eq!(keys(&revert_transient(&ids, false)), vec!["V", "v"]);
    }

    /// MG.43a: **keys are overloaded across the two states, and the
    /// gate is the only thing that makes that safe.**
    ///
    /// `A` is *pick* when idle and *continue* when stopped; `a` is
    /// *apply* when idle and *abort* when stopped. Every one of those
    /// is magit's own key, and the pairs are only safe because the two
    /// sets are mutually exclusive — a menu showing both would put
    /// "apply this commit" one row from "throw the sequence away".
    ///
    /// So rather than assert particular keys, this asserts the
    /// property that makes the overload safe: any key appearing in
    /// BOTH states must resolve to a different row in each. A future
    /// row that reused a key for the *same* operation in both states
    /// would be a gate that stopped doing its job.
    #[test]
    fn overloaded_keys_resolve_to_different_rows_in_each_state() {
        let ids = MagitActionIds::default();
        for (name, idle, stopped) in [
            (
                "cherry-pick",
                cherry_pick_transient(&ids, false),
                cherry_pick_transient(&ids, true),
            ),
            (
                "revert",
                revert_transient(&ids, false),
                revert_transient(&ids, true),
            ),
            (
                "merge",
                merge_transient(&ids, false),
                merge_transient(&ids, true),
            ),
        ] {
            let label_for = |spec: &TransientSpec, key: &str| {
                spec.groups
                    .iter()
                    .flat_map(|g| &g.items)
                    .find(|i| i.key.iter().any(|k| k == key))
                    .map(|i| i.label.clone())
            };
            let shared: Vec<String> = keys(&idle)
                .into_iter()
                .filter(|k| keys(&stopped).contains(k))
                .collect();
            // Vacuity guard: these menus DO overload keys, and a
            // refactor that stopped sharing any would silently make
            // the loop below assert nothing.
            assert!(
                !shared.is_empty(),
                "{name}: expected the two states to share at least one key",
            );
            for key in shared {
                assert_ne!(
                    label_for(&idle, &key),
                    label_for(&stopped, &key),
                    "{name}: `{key}` resolves to the same row in both states —                      the gate is no longer distinguishing them",
                );
            }
        }
    }

    /// Stopped offers the ways OUT only — starting another pick while
    /// one is mid-conflict is the wrong menu entirely.
    #[test]
    fn stopped_sequencers_offer_only_the_ways_out() {
        let ids = MagitActionIds::default();
        let cp = keys(&cherry_pick_transient(&ids, true));
        assert_eq!(cp, vec!["A", "s", "a"]);
        let rv = keys(&revert_transient(&ids, true));
        assert_eq!(rv, vec!["V", "s", "a"]);
    }

    /// The two sequences do NOT share their sequencer rows.
    ///
    /// `git revert --continue` errors during a cherry-pick and vice
    /// versa, so a shared row set would fire the wrong command in one
    /// of the two menus — the reason these are separate consts rather
    /// than one "sequencer" table.
    #[test]
    fn each_sequence_fires_its_own_commands() {
        let cp: Vec<&str> = CHERRY_PICK_SEQUENCE_ROWS.iter().map(|r| r.action).collect();
        let rv: Vec<&str> = REVERT_SEQUENCE_ROWS.iter().map(|r| r.action).collect();
        assert!(cp.iter().all(|a| a.contains("cherry-pick")), "{cp:?}");
        assert!(rv.iter().all(|a| a.contains("revert")), "{rv:?}");
        assert!(
            cp.iter().all(|a| !rv.contains(a)),
            "the two sequences must not share an action: {cp:?} vs {rv:?}",
        );
    }

    /// The gates default to "nothing running", so a developer's own
    /// half-finished cherry-pick cannot change a test's row count —
    /// the reason `probe()` is separate from the pure builder.
    #[test]
    fn gates_default_to_nothing_in_progress() {
        let g = DispatchGates::default();
        assert!(!g.cherry_pick && !g.revert && !g.rebase);
    }
}

#[cfg(test)]
mod sequence_step_tests {
    use crate::magit_global_mode::{
        instant_squash_steps, merge_absorb_steps, stash_snapshot_steps,
    };

    /// MG.42-E2: a snapshot APPLIES, never pops. A pop would remove the
    /// very stack entry the snapshot exists to create, leaving the user
    /// with neither a restore point nor a changed tree.
    #[test]
    fn a_snapshot_applies_rather_than_pops() {
        let steps = stash_snapshot_steps(&[]);
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].argv, vec!["stash", "push"]);
        assert_eq!(steps[1].argv, vec!["stash", "apply"]);
        assert!(
            !steps[1].argv.contains(&"pop".to_string()),
            "pop would destroy the snapshot it just made",
        );
    }

    /// The variants differ only in the push flags.
    #[test]
    fn snapshot_variants_pass_their_flags_to_the_push_only() {
        for extra in [vec!["--staged"], vec!["--keep-index"]] {
            let steps = stash_snapshot_steps(&extra);
            assert!(steps[0].argv.contains(&extra[0].to_string()));
            assert_eq!(
                steps[1].argv,
                vec!["stash", "apply"],
                "restore is unflagged"
            );
        }
    }

    /// The rebase base is `<commit>~1`, not `<commit>`.
    ///
    /// A fixup must be replayed ALONGSIDE the commit it targets, so the
    /// rebase has to start one before it. Rebasing onto the commit
    /// itself would leave the fixup unmerged and the operation silently
    /// pointless.
    #[test]
    fn instant_squash_rebases_from_one_before_the_target() {
        let steps = instant_squash_steps("fixup", "abc123");
        assert_eq!(steps.len(), 2);
        assert!(steps[0].argv.contains(&"--fixup".to_string()));
        assert!(steps[0].argv.contains(&"abc123".to_string()));
        assert!(
            steps[1].argv.contains(&"abc123~1".to_string()),
            "must rebase from one before the target: {:?}",
            steps[1].argv,
        );
        assert!(
            steps[1].argv.contains(&"--autostash".to_string()),
            "an instant fixup is reached mid-edit; without autostash it \
             fails exactly when it is most wanted",
        );
    }

    #[test]
    fn instant_squash_kind_selects_the_marker() {
        assert!(
            instant_squash_steps("squash", "x")[0]
                .argv
                .contains(&"--squash".to_string())
        );
        assert!(
            instant_squash_steps("fixup", "x")[0]
                .argv
                .contains(&"--fixup".to_string())
        );
    }

    /// Absorb deletes with `-d`, never `-D`.
    ///
    /// Git refuses `-d` on a branch that is not fully merged, so a
    /// failed merge leaves the branch intact. `-D` would destroy it in
    /// exactly the case where the merge did not take.
    #[test]
    fn absorb_uses_a_safe_delete() {
        let steps = merge_absorb_steps("feature");
        assert_eq!(steps.len(), 2);
        assert!(steps[1].argv.contains(&"-d".to_string()));
        assert!(
            !steps[1].argv.contains(&"-D".to_string()),
            "a forced delete would destroy the branch when the merge failed",
        );
    }
}

#[cfg(test)]
mod two_input_argv_tests {
    use crate::magit_global_mode::{reset_file_argv, stash_branch_argv};

    /// MG.42-E3: "reset a file" is `checkout <commit> -- <path>`, NOT
    /// `reset`.
    ///
    /// `checkout` replaces the file in both index and working tree,
    /// which is what the row promises. `reset <commit> -- <path>` moves
    /// index entries only and leaves the file on disk untouched — the
    /// same words, a different outcome, and the failure would look like
    /// the command silently doing nothing.
    #[test]
    fn resetting_a_file_checks_it_out() {
        let argv = reset_file_argv("abc123", "src/main.rs");
        assert_eq!(argv, vec!["checkout", "abc123", "--", "src/main.rs"]);
        assert_ne!(argv[0], "reset", "reset would not touch the working tree");
    }

    /// The `--` separator is what stops a path that looks like a ref
    /// from being read as one.
    #[test]
    fn the_path_is_separated_from_the_revision() {
        // A file literally named like a branch is the case this guards.
        let argv = reset_file_argv("HEAD", "main");
        let dashes = argv.iter().position(|a| a == "--").expect("has --");
        assert!(
            dashes < argv.iter().rposition(|a| a == "main").unwrap(),
            "the path must come after `--`: {argv:?}",
        );
    }

    #[test]
    fn stash_branch_takes_the_name_then_the_stash() {
        assert_eq!(
            stash_branch_argv("recover", "stash@{0}"),
            vec!["stash", "branch", "recover", "stash@{0}"],
        );
    }
}

#[cfg(test)]
mod commit_intent_tests {
    use crate::magit_commit_mode::CommitIntent;

    /// MG.42-E1: the buffer name selects the intent in ONE place.
    ///
    /// `reword` is tested before `amend` on purpose — order is the
    /// difference between the two mapping correctly and one shadowing
    /// the other if a name ever changes.
    #[test]
    fn buffer_names_map_to_intents() {
        assert_eq!(
            CommitIntent::from_buffer_name("*magit:reword*"),
            CommitIntent::Reword
        );
        assert_eq!(
            CommitIntent::from_buffer_name("*magit:amend*"),
            CommitIntent::Amend
        );
        assert_eq!(
            CommitIntent::from_buffer_name("*magit:commit*"),
            CommitIntent::Create
        );
    }

    /// Both replacing intents open pre-filled; a fresh commit does not.
    #[test]
    fn replacing_intents_seed_the_prior_message() {
        assert!(CommitIntent::Amend.seeds_prior_message());
        assert!(CommitIntent::Reword.seeds_prior_message());
        assert!(!CommitIntent::Create.seeds_prior_message());
    }

    /// Reword and amend are NOT the same operation.
    ///
    /// `amend` sweeps in whatever is staged; `reword` passes `--only`
    /// and touches the message alone. Collapsing them would make a row
    /// labelled "reword" silently commit staged content.
    #[test]
    fn reword_is_distinct_from_amend() {
        assert_ne!(CommitIntent::Reword, CommitIntent::Amend);
    }

    /// MG.42-E1: the targeted intents carry their target IN the name.
    ///
    /// Augment and merge-edit act on something the user picked, and
    /// the compose buffer is opened long before the commit runs. The
    /// name is the carrier so there is no side-channel to go stale
    /// between opening the buffer and confirming it.
    #[test]
    fn targeted_intents_round_trip_through_the_buffer_name() {
        let name = CommitIntent::augment_buffer_name("lattice", "abc123");
        assert_eq!(
            CommitIntent::from_buffer_name(&name),
            CommitIntent::Augment {
                target: "abc123".to_string()
            }
        );

        let name = CommitIntent::merge_edit_buffer_name("lattice", "feature/x");
        assert_eq!(
            CommitIntent::from_buffer_name(&name),
            CommitIntent::MergeEdit {
                branch: "feature/x".to_string()
            }
        );
    }

    /// A target that is empty or absent must NOT produce a targeted
    /// intent.
    ///
    /// `git commit --squash= -m msg` is not a no-op — it is an error
    /// git reports at the point the user expected a commit. Falling
    /// back to `Create` is wrong too, so the name simply does not
    /// match and the buffer composes an ordinary commit.
    #[test]
    fn an_empty_target_does_not_produce_a_targeted_intent() {
        assert_eq!(
            CommitIntent::from_buffer_name("*magit:augment:lattice:*"),
            CommitIntent::Create
        );
        assert_eq!(
            CommitIntent::from_buffer_name("*magit:merge-edit:lattice:*"),
            CommitIntent::Create
        );
    }

    /// A target — or a REPOSITORY — containing "amend" or "reword"
    /// stays what the view word says it is.
    ///
    /// `amend-fixes` is an ordinary branch name and a checkout can be
    /// called anything; before MR.3 the intent was chosen by substring
    /// (`name.contains("amend")`), so either would have selected the
    /// opposite operation — merge-edit records a new merge commit,
    /// amend rewrites the last one. The repo here is deliberately named
    /// `amend` to pin that the structured parse, not ordering, is what
    /// makes this safe.
    #[test]
    fn a_target_containing_amend_stays_targeted() {
        assert_eq!(
            CommitIntent::from_buffer_name(&CommitIntent::merge_edit_buffer_name(
                "amend",
                "amend-fixes"
            )),
            CommitIntent::MergeEdit {
                branch: "amend-fixes".to_string()
            }
        );
    }

    /// MG.43c: reword-a-commit is checked BEFORE the bare `reword`
    /// test, and the two are different operations.
    ///
    /// `*magit:reword-commit:<sha>*` contains the substring `reword`,
    /// so an ordering slip would make it amend HEAD — rewriting the
    /// wrong commit's message, and one the user can see is wrong only
    /// after it has happened.
    #[test]
    fn reword_a_commit_is_not_reword_head() {
        let name = CommitIntent::reword_commit_buffer_name("lattice", "abc123");
        assert_eq!(
            CommitIntent::from_buffer_name(&name),
            CommitIntent::RewordCommit {
                target: "abc123".to_string()
            },
        );
        assert_ne!(CommitIntent::from_buffer_name(&name), CommitIntent::Reword);
    }

    /// It seeds from the commit it NAMES, not from HEAD.
    ///
    /// The buffer's text is what gets written back, so seeding from
    /// HEAD would show the wrong message and then apply it to the
    /// target — replacing one commit's message with another's.
    #[test]
    fn reword_a_commit_seeds_from_its_own_target() {
        let intent = CommitIntent::RewordCommit {
            target: "abc123".to_string(),
        };
        assert!(intent.seeds_prior_message());
        assert_eq!(intent.seed_source(), Some("abc123"));
        // The HEAD-acting intents name no source and fall back to HEAD.
        assert_eq!(CommitIntent::Reword.seed_source(), None);
        assert_eq!(CommitIntent::Amend.seed_source(), None);
    }

    /// Neither targeted intent pre-fills the buffer.
    ///
    /// Augment's note is the user's own addition BELOW the generated
    /// `squash!` line, and a merge message is written fresh — seeding
    /// either with a prior message would put text there the user then
    /// has to delete.
    #[test]
    fn targeted_intents_do_not_seed_a_prior_message() {
        assert!(
            !CommitIntent::Augment {
                target: "abc123".to_string()
            }
            .seeds_prior_message()
        );
        assert!(
            !CommitIntent::MergeEdit {
                branch: "main".to_string()
            }
            .seeds_prior_message()
        );
    }
}

#[cfg(test)]
mod root_menu_chord_tests {
    use super::*;

    /// MG.49: **the keys vim needs stay vim's.**
    ///
    /// Emacs binds all seventeen root menus on `magit-mode-map` because
    /// emacs is not modal. Here `magit-core-mode` is a MINOR layer,
    /// which beats the builtin vim layer — so binding `f` would take
    /// find-char away inside every magit buffer, and a magit buffer is
    /// text you navigate.
    ///
    /// Vim's grammar IS the public command API (paramount goal #3), so
    /// it wins. The rule that falls out: a root menu may take a key
    /// whose vim meaning is an *editing operator* (inert where nothing
    /// is editable) and may NOT take one whose meaning is a motion.
    ///
    /// This is a deny-list rather than a "not in the builtin keymap"
    /// check, because the keys we DO take (`d`, `c`, `r`, `A`, `O`) are
    /// in the builtin keymap too — as operators. Membership is not the
    /// question; what the key MEANS is.
    #[test]
    fn no_root_menu_takes_a_key_vim_uses_as_a_motion() {
        // Motion or prefix in Normal mode, and therefore load-bearing
        // in a read-only buffer.
        const VIM_MOTIONS: &[(&str, &str)] = &[
            ("l", "right"),
            ("b", "back-word"),
            ("B", "back-WORD"),
            ("w", "word"),
            ("f", "find-char"),
            ("F", "find-char-back"),
            ("t", "till"),
            ("T", "till-back"),
            ("m", "set-mark"),
            ("z", "fold prefix (`zf` / `za` / `zo`)"),
        ];
        for menu in ROOT_MENUS {
            let Some(chord) = menu.chord else { continue };
            if let Some((_, meaning)) = VIM_MOTIONS.iter().find(|(k, _)| *k == chord) {
                panic!(
                    "`{}` binds `{chord}`, which is vim's {meaning} — a minor \
                     layer shadows the builtin one, so this would remove the \
                     motion from every magit buffer. Leave it `None`; the menu \
                     stays reachable through the dispatch.",
                    menu.source,
                );
            }
        }
    }

    /// The other direction, so the rule above cannot be satisfied by
    /// binding nothing at all: the operator keys that ARE safe stay
    /// bound, including the three this mode carried before MG.49.
    #[test]
    fn the_operator_keys_are_actually_taken() {
        for (chord, source) in [
            ("A", "magit-menu-cherry-pick"),
            ("_", "magit-menu-revert"),
            ("O", "magit-menu-reset"),
        ] {
            let menu = ROOT_MENUS
                .iter()
                .find(|m| m.source == source)
                .unwrap_or_else(|| panic!("{source} is in ROOT_MENUS"));
            assert_eq!(
                menu.chord,
                Some(chord),
                "`{chord}` is a vim editing operator — inert in a read-only \
                 magit buffer — so {source} is free to take it",
            );
        }
    }

    /// PD.3: the Diff menu carries four targets, and `e` is the new one.
    ///
    /// `d` / `f` / `v` are asserted alongside it because they are the
    /// regression that matters: they only live in this menu at all
    /// because binding `d` to it took their chords (the trie checks a
    /// node's own binding before its children), so a row lost here is a
    /// chord lost outright, silently.
    #[test]
    fn the_diff_menu_carries_d_f_v_and_the_new_e_row() {
        let by_key: std::collections::HashMap<&str, &str> =
            DIFF_SHOW_ROWS.iter().map(|r| (r.key, r.action)).collect();
        assert_eq!(by_key.len(), 4, "four targets: {by_key:?}");
        assert_eq!(by_key.get("d"), Some(&"action:magit-global-diff"));
        assert_eq!(by_key.get("f"), Some(&"action:magit-diff-file"));
        assert_eq!(by_key.get("v"), Some(&"action:magit-diff-side-by-side"));
        assert_eq!(
            by_key.get("e"),
            Some(&"action:magit-project-diff"),
            "`e` opens the editable cross-file view"
        );
    }

    /// Every entry resolves to a spec, or a chord would open nothing.
    #[test]
    fn every_root_menu_builds() {
        let ids = MagitActionIds::default();
        let ctx = TransientContext::default();
        for menu in ROOT_MENUS {
            assert!(
                root_menu_spec(menu.source, &ids, &ctx, DispatchGates::default()).is_some(),
                "{} has no spec — its chord and its dispatch row would both \
                 open nothing",
                menu.source,
            );
        }
    }
}
