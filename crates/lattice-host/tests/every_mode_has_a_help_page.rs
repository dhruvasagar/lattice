//! HD.7 — the coverage guard, in the direction that was missing.
//!
//! `every_user_doc_in_docs_user_is_registered_as_a_topic` (in
//! `lattice-help`) guards **doc → topic**: every file on disk resolves as
//! `:help <name>`. Nothing guarded **mode → doc**, so a registered mode with
//! no page was invisible to the whole suite — and that is the direction HD.1's
//! rule actually cares about. The rule is "a mode answers to its id on every
//! surface", and `:help <mode-id>` is one of those surfaces.
//!
//! ## Why this lives in `lattice-host` and not next to its twin
//!
//! The enumeration has to come from the **mode registry**, which only exists
//! once a real `Editor` has booted — that is the only place every mode-owning
//! crate has registered. Grepping `ModeId::new` out of the source instead
//! catches dozens of test fixtures (`stub-mode`, `test-minor-3`, `plugin-a`)
//! that are not modes a user can reach, which is exactly the noise that would
//! get the guard weakened until it stopped guarding.
//!
//! ## The allowlist
//!
//! Some registered modes deliberately have no page. Each needs a reason next
//! to it, because "add it to the list" is the cheap way to make this test
//! green and the reason is the only thing standing in the way of that.

#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// Mode ids that intentionally answer to no `:help` page.
///
/// Keep this SHORT and keep the reasons specific. A mode belongs here when a
/// user cannot meaningfully act on a page about it — not when writing the page
/// is inconvenient.
const NO_PAGE_EXPECTED: &[(&str, &str)] = &[];

fn help_topic_names() -> HashSet<String> {
    lattice_help::topics::builtin_topics()
        .names()
        .map(|s: &str| s.to_string())
        .collect()
}

#[test]
fn every_registered_mode_answers_to_help_by_its_id() {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let topics = help_topic_names();
    let allowed: HashSet<&str> = NO_PAGE_EXPECTED.iter().map(|(id, _)| *id).collect();

    let registry = editor.mode_registry.load();
    let mut missing: Vec<String> = registry
        .iter()
        .map(|(id, _)| id.to_string())
        .filter(|id| !topics.contains(id) && !allowed.contains(id.as_str()))
        .collect();
    missing.sort();

    assert!(
        missing.is_empty(),
        "these modes are registered but `:help <id>` fails for them.\n\
         A mode answers to its id on `:<id>` and `:describe-mode <id>` already; \
         help is the surface that drifts.\n\
         Either add `docs/user/<id>.md`, or add the id to `NO_PAGE_EXPECTED` \
         with a reason a user would accept.\n\
         Missing: {missing:#?}"
    );
}

/// The allowlist must not rot into a place where pages go to be forgotten: an
/// entry naming a mode that no longer exists is a reason nobody will ever
/// re-examine, and it makes the list look more justified than it is.
#[test]
fn the_allowlist_names_only_modes_that_exist() {
    let editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let registry = editor.mode_registry.load();
    let registered: HashSet<String> = registry.iter().map(|(id, _)| id.to_string()).collect();

    let stale: Vec<&str> = NO_PAGE_EXPECTED
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| !registered.contains(*id))
        .collect();

    assert!(
        stale.is_empty(),
        "NO_PAGE_EXPECTED names modes that are not registered: {stale:?}"
    );
}

/// ...and an entry that HAS grown a page should leave the list, or the list
/// stops describing what is actually uncovered.
#[test]
fn the_allowlist_does_not_list_modes_that_now_have_pages() {
    let topics = help_topic_names();
    let redundant: Vec<&str> = NO_PAGE_EXPECTED
        .iter()
        .map(|(id, _)| *id)
        .filter(|id| topics.contains(*id))
        .collect();

    assert!(
        redundant.is_empty(),
        "these have `docs/user/<id>.md` now and should come off NO_PAGE_EXPECTED: {redundant:?}"
    );
}
