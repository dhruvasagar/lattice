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

/// What an accepted row routes into: the switch-commands menu (PC.6).
///
/// Two hops, because `picker-accept-outcome` has no "open a transient" arm and
/// should not grow one — an accept resolves to a typed outcome, and opening a
/// menu is an effect. `invoke-command` bridges them, which is the route
/// `roam_insert`'s create row already takes.
pub const ACCEPT_COMMAND: &str = "project-switch-to";

/// PC.12: what the `… (choose a dir)` row invokes — the command that opens the
/// directory sub-picker.
pub const CHOOSE_DIR_COMMAND: &str = "project-choose-dir";

/// PC.12: what the create row invokes. Takes a PATH rather than a resolved
/// root, because a user who typed one (or a directory the sub-picker walked
/// into) has named a place, not necessarily a project — resolving it is the
/// command's job, through the host seam that can actually answer.
pub const REMEMBER_AND_SWITCH_COMMAND: &str = "project-remember-and-switch";

pub fn spec() -> PickerSourceSpec {
    PickerSourceSpec {
        id: PROJECTS_PICKER.to_string(),
        doc: "Switch to a remembered project".to_string(),
        args_schema: Vec::new(),
        args_hint: String::new(),
        // Not live: the remembered list cannot change while the picker is open.
        live: false,
        // PC.12: the create row is back, and the argument that ruled it out is
        // still correct — it just answered a different question.
        //
        // "No create row, because *creating a project* is not a thing this
        // owns; a project comes into existence by having a root marker, which
        // is git's or cargo's job." True. But this row does not create
        // anything: it REMEMBERS a project that already exists, which is
        // exactly what `:project-remember` does and what `project.el`'s
        // `… (choose a dir)` does. The label says `remember`, not `create`,
        // because that is what happens.
        //
        // It is the second half of the choose-a-dir flow: the sub-picker fills
        // this picker's query with the path it chose, and this row is what
        // makes that query actionable. Typing a path in by hand takes the same
        // route, which is a free consequence rather than a second mechanism.
        create_label: Some("\u{2026} (remember %s)".to_string()),
    }
}

/// The always-present row that opens the directory sub-picker.
///
/// `project.el`'s label verbatim, because that is the muscle memory being
/// imported.
///
/// **Not the create row.** A create row appears only once the query is
/// non-empty (`push_create_row` returns early on an empty query), and an empty
/// list with an empty query is precisely when this is needed — a fresh install
/// where the user has nothing remembered and no idea what to type. So it is an
/// ordinary candidate the source emits, and the two coexist: this one gets you
/// a path, the create row turns a path into a remembered project.
fn choose_a_dir_row() -> (RawCandidate, RoutingPayload) {
    (
        RawCandidate {
            insert_text: None,
            // Matched on the words a user would reach for. The path-shaped
            // queries the sub-picker leaves behind will not match it, which is
            // right: once there is a path in the query the create row is the
            // one that should answer.
            text: "choose a dir directory browse".to_string(),
            display: "\u{2026} (choose a dir)".to_string(),
            source: Some(PROJECTS_PICKER.to_string()),
            kind: CandidateKind::Plain,
            data: CandidateData::Plain,
            annotations: Vec::new(),
            display_spans: Vec::new(),
        },
        RoutingPayload::InvokeCommand(CommandRef {
            id: CHOOSE_DIR_COMMAND.to_string(),
            args: Args::None,
        }),
    )
}

/// One row per remembered project, then the `… (choose a dir)` row.
///
/// **An empty list no longer refuses to open**, and that reverses a decision
/// this function used to carry. It returned an `err` —
/// `project: no projects remembered yet — open a file in one, or
/// :project-remember <dir>` — on the `roam_find` rule, that "nothing
/// remembered yet" and "the feature is broken" look identical in an empty
/// picker and have entirely different fixes.
///
/// The rule is right and it does not apply here any more. It is about a picker
/// with nothing to OFFER; this one always has the choose-a-dir row, and a
/// fresh install is exactly when that row is the whole point. Refusing to open
/// put the escape hatch behind the wall it exists to get through — the user
/// was told to run a command instead of being handed the thing that runs it.
///
/// An empty list is therefore self-describing: no projects, and one row
/// offering to find one.
pub fn init(list: Vec<String>) -> Result<Vec<(RawCandidate, RoutingPayload)>, String> {
    let mut rows: Vec<(RawCandidate, RoutingPayload)> = list.into_iter().map(row).collect();
    rows.push(choose_a_dir_row());
    Ok(rows)
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

/// Resolve a chosen row.
///
/// A forward for the project rows and the choose-a-dir row, since both already
/// built their command. The create row is the one that has to be turned into
/// something: it carries only the QUERY the user typed (or the path the
/// sub-picker filled in), so it routes to the remember-and-switch command,
/// which is where a path becomes a project.
pub fn accept(routing: RoutingPayload) -> Result<PickerAcceptOutcome, String> {
    match routing {
        RoutingPayload::InvokeCommand(cmd) => Ok(PickerAcceptOutcome::InvokeCommand(cmd)),
        RoutingPayload::Create(query) => Ok(PickerAcceptOutcome::InvokeCommand(CommandRef {
            id: REMEMBER_AND_SWITCH_COMMAND.to_string(),
            // Verbatim, trimming included nowhere here: the command resolves
            // it through the host's project seam, which is the only thing that
            // can say what this path's project actually is.
            args: Args::String(query),
        })),
        _ => Err("project: the projects picker got a routing token it did not emit".to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn displays(rows: &[(RawCandidate, RoutingPayload)]) -> Vec<String> {
        rows.iter().map(|(c, _)| c.display.clone()).collect()
    }

    /// **The fresh-install case, and the one the old `err` broke.** With
    /// nothing remembered the picker still opens, and what it offers is the
    /// way to remember something. Refusing to open told the user to run a
    /// command instead of handing them the thing that runs it.
    #[test]
    fn an_empty_list_still_opens_and_offers_the_way_out() {
        let rows = init(Vec::new()).expect("an empty list is not an error any more");
        assert_eq!(displays(&rows), vec!["\u{2026} (choose a dir)".to_string()]);
    }

    /// Pinned last, after every project. `project.el` puts it there, and a row
    /// that could sort above a real project would put "go browsing" in front
    /// of "the thing you already told me about".
    #[test]
    fn the_choose_row_comes_after_every_project() {
        let rows = init(vec!["/src/a".to_string(), "/src/b".to_string()]).unwrap();
        assert_eq!(
            displays(&rows),
            vec![
                "a".to_string(),
                "b".to_string(),
                "\u{2026} (choose a dir)".to_string()
            ]
        );
    }

    /// It routes to the command that opens the sub-picker. A row whose command
    /// is not the registered one is a row that silently does nothing, which is
    /// indistinguishable from a broken key.
    #[test]
    fn the_choose_row_invokes_the_choose_dir_command() {
        let rows = init(Vec::new()).unwrap();
        let (_, routing) = rows.last().unwrap();
        match routing {
            RoutingPayload::InvokeCommand(cmd) => {
                assert_eq!(cmd.id, CHOOSE_DIR_COMMAND);
                assert!(matches!(cmd.args, Args::None), "it takes no argument");
            }
            other => panic!("expected InvokeCommand, got {other:?}"),
        }
    }

    /// The create row's query — a path the sub-picker filled in, or one the
    /// user typed — becomes the remember-and-switch command's argument. This
    /// is the seam that turns "a path is in the query" into "that project is
    /// remembered and open".
    #[test]
    fn the_create_row_routes_the_query_to_remember_and_switch() {
        let outcome = accept(RoutingPayload::Create("/src/dhruvasagar/comp".to_string()))
            .expect("the create row is a routing this source emits");
        match outcome {
            PickerAcceptOutcome::InvokeCommand(cmd) => {
                assert_eq!(cmd.id, REMEMBER_AND_SWITCH_COMMAND);
                match cmd.args {
                    Args::String(s) => assert_eq!(s, "/src/dhruvasagar/comp"),
                    other => panic!("the path must ride as the first argument: {other:?}"),
                }
            }
            other => panic!("expected InvokeCommand, got {other:?}"),
        }
    }

    /// The label says REMEMBER, not create. The distinction is the whole
    /// reason this source may carry a create row at all: a project comes into
    /// existence by having a root marker, which is git's job, not this
    /// plugin's. What the row does is record one that already exists.
    #[test]
    fn the_create_label_promises_to_remember_rather_than_to_create() {
        let label = spec().create_label.expect("PC.12 declares one");
        assert!(label.contains("remember"), "the label reads: {label}");
        assert!(
            !label.to_ascii_lowercase().contains("create"),
            "a `create` label would promise something this plugin cannot do: {label}"
        );
        assert!(label.contains("%s"), "the query is substituted in: {label}");
    }

    /// A project row still routes to the switch-commands hop, unchanged. The
    /// two new routings must not have displaced the one that was already
    /// there.
    #[test]
    fn a_project_row_still_routes_to_the_switch_menu() {
        let rows = init(vec!["/src/a".to_string()]).unwrap();
        match &rows[0].1 {
            RoutingPayload::InvokeCommand(cmd) => {
                assert_eq!(cmd.id, ACCEPT_COMMAND);
                match &cmd.args {
                    Args::String(s) => assert_eq!(s, "/src/a"),
                    other => panic!("the root rides along: {other:?}"),
                }
            }
            other => panic!("expected InvokeCommand, got {other:?}"),
        }
    }

    /// The choose row must not be findable by a PATH-shaped query. Once the
    /// sub-picker has filled the query with a path, the create row is the one
    /// that should answer — two rows both claiming the same query is a picker
    /// where `<CR>` does one of two different things depending on ranking.
    #[test]
    fn the_choose_rows_matched_text_is_words_not_a_path() {
        let rows = init(Vec::new()).unwrap();
        let text = &rows.last().unwrap().0.text;
        assert!(!text.contains('/'), "it must not look like a path: {text}");
        assert!(text.contains("dir"), "but it is findable by word: {text}");
    }
}
