//! PC.5 — the `projects` picker: one row per remembered project.
//!
//! Design: `docs/dev/architecture/project-commands.md` §5.
//!
//! ## The row
//!
//! Basename as the display, full path as the annotation. That is
//! `magit-repo-scoping.md` §3.1's rule applied here for its reason: names are
//! read far more often than they are parsed, and two checkouts can share a
//! basename (`~/work/api` and `~/oss/api`), so the basename is for humans and
//! the path is the identity. A picker showing only basenames could not tell
//! those two apart at all.
//!
//! The **path is part of the matched text**, not only the annotation.
//! Annotations are shown, never matched, so a source that put the path only
//! there would make `oss/api` unsearchable — the same trap org-roam hit with
//! aliases, where 12% of the corpus was findable only under a name the matcher
//! never saw.
//!
//! ## Order
//!
//! Whatever the store holds, untouched: the list is already most-recently-
//! visited first (`projects.rs`), and re-sorting here would throw away the one
//! property that makes switching back and forth between two projects free.

use crate::lattice::plugin_host::types::{
    Annotation, AnnotationCustom, Args, CandidateData, CandidateKind, CommandRef,
    PickerAcceptOutcome, PickerSourceSpec, RawCandidate, RoutingPayload,
};
use crate::projects;

/// The registered picker id.
pub const PROJECTS_PICKER: &str = "projects";

/// What an accepted row routes into **today**.
///
/// PC.6 replaces this with the `project-switch-commands` transient, which is
/// what `project.el`'s `C-x p p` actually shows. Until the menu exists, going
/// straight to find-file is the useful default rather than a dead accept —
/// and it is the verb the picker would open the menu ON anyway.
pub const ACCEPT_COMMAND: &str = "project-find-file";

pub fn spec() -> PickerSourceSpec {
    PickerSourceSpec {
        id: PROJECTS_PICKER.to_string(),
        doc: "Switch to a remembered project".to_string(),
        args_schema: Vec::new(),
        args_hint: String::new(),
        // Not live: the remembered list cannot change while the picker is open.
        live: false,
        // No create row. "Create a project" is not a thing this owns — a
        // project comes into existence by having a root marker, which is git's
        // or cargo's job. `:project-remember` is the deliberate seeding verb,
        // and offering a create row here would imply this can make one.
        create_label: None,
    }
}

/// One row per remembered project.
///
/// An empty list is an **`err`**, not an empty candidate set. The host echoes it
/// and leaves the picker shut, which is the `roam_find` rule: "nothing
/// remembered yet" and "the feature is broken" look identical in an empty
/// picker and have entirely different fixes.
pub fn init(list: Vec<String>) -> Result<Vec<(RawCandidate, RoutingPayload)>, String> {
    if list.is_empty() {
        return Err(
            "project: no projects remembered yet — open a file in one, or `:project-remember <dir>`"
                .to_string(),
        );
    }
    Ok(list.into_iter().map(row).collect())
}

fn row(root: String) -> (RawCandidate, RoutingPayload) {
    let name = projects::basename(&root).to_string();
    // Matched text carries BOTH, so `oss/api` narrows to the right checkout
    // while typing just `api` still finds them all.
    let text = format!("{name} {root}");
    (
        RawCandidate {
            insert_text: None,
            text,
            display: name,
            source: Some(PROJECTS_PICKER.to_string()),
            kind: CandidateKind::Plain,
            data: CandidateData::Plain,
            annotations: vec![Annotation::Custom(AnnotationCustom {
                text: root.clone(),
                // `.doc` is the descriptive column — what the path is here.
                // A real annotation-slot key rather than a syntax element
                // name, or it resolves to the fallback colour and the picker
                // looks unstyled (the mistake org-roam's annotations made).
                slot: "completion.annotation.doc".to_string(),
            })],
            display_spans: Vec::new(),
        },
        // The ROOT, resolved here where it is in hand. The accept forwards it
        // as a command argument rather than making the ex-command re-read the
        // store to learn a path the picker already had.
        RoutingPayload::InvokeCommand(CommandRef {
            id: ACCEPT_COMMAND.to_string(),
            args: Args::String(root),
        }),
    )
}

/// Resolve a chosen row — a forward, since the row already built the command.
pub fn accept(routing: RoutingPayload) -> Result<PickerAcceptOutcome, String> {
    match routing {
        RoutingPayload::InvokeCommand(cmd) => Ok(PickerAcceptOutcome::InvokeCommand(cmd)),
        _ => Err("project: the projects picker got a routing token it did not emit".to_string()),
    }
}
