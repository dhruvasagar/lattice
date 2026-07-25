//! MG.8: magit transient menu definitions.
//!
//! Defines the `TransientSpec` instances for the repo-level
//! dispatch (C-c g) and file-level dispatch (C-c f) menus.
//! Each is a grouped action menu rendered by the PICK.1
//! transient picker overlay.

use std::sync::Arc;

use lattice_picker::{
    TransientGroup, TransientItem, TransientItemKind, TransientSpec, TransientState,
    TransientValue,
};

/// Build the repo-level dispatch transient (`C-c g`).
///
/// Each Submenu item opens a nested transient. Action items are
/// placeholders — real command bindings are wired in a follow-up
/// when mode-contributed action IDs are available.
pub fn dispatch_transient() -> TransientSpec {
    TransientSpec {
        title: "Magit dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Working tree".into(),
                items: vec![
                    TransientItem {
                        key: vec!["s".into()],
                        label: "stage".into(),
                        description: "Stage changes".into(),
                        kind: TransientItemKind::Flag {
                            name: "stage_all".into(),
                            default: false,
                        },
                    },
                    TransientItem {
                        key: vec!["c".into()],
                        label: "commit".into(),
                        description: "Commit changes".into(),
                        kind: TransientItemKind::Submenu(Arc::new(commit_transient())),
                    },
                ],
            },
            TransientGroup {
                label: "History".into(),
                items: vec![TransientItem {
                    key: vec!["l".into()],
                    label: "log".into(),
                    description: "Show commit history".into(),
                    kind: TransientItemKind::Flag {
                        name: "show_log".into(),
                        default: false,
                    },
                }],
            },
            TransientGroup {
                label: "Branches".into(),
                items: vec![TransientItem {
                    key: vec!["b".into()],
                    label: "branch".into(),
                    description: "Branch operations".into(),
                    kind: TransientItemKind::Flag {
                        name: "branch_op".into(),
                        default: false,
                    },
                }],
            },
        ],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// Build a commit sub-transient.
fn commit_transient() -> TransientSpec {
    TransientSpec {
        title: "Commit".into(),
        groups: vec![TransientGroup {
            label: "Actions".into(),
            items: vec![TransientItem {
                key: vec!["c".into()],
                label: "commit".into(),
                description: "Create a new commit".into(),
                kind: TransientItemKind::Flag {
                    name: "do_commit".into(),
                    default: false,
                },
            }],
        }],
        preview: None,
        footer: Some("q dismiss  BS back".into()),
    }
}

/// Build the file-level dispatch transient (`C-c f`).
pub fn file_dispatch_transient() -> TransientSpec {
    TransientSpec {
        title: "File dispatch".into(),
        groups: vec![
            TransientGroup {
                label: "Actions".into(),
                items: vec![
                    TransientItem {
                        key: vec!["s".into()],
                        label: "stage".into(),
                        description: "Stage this file".into(),
                        kind: TransientItemKind::Flag {
                            name: "stage_file".into(),
                            default: false,
                        },
                    },
                    TransientItem {
                        key: vec!["d".into()],
                        label: "diff".into(),
                        description: "Show diff for this file".into(),
                        kind: TransientItemKind::Flag {
                            name: "diff_file".into(),
                            default: false,
                        },
                    },
                ],
            },
        ],
        preview: None,
        footer: Some("q dismiss".into()),
    }
}
