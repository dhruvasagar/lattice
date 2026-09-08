//! OC.11a — every config-load diagnostic survives the moment it happened.
//!
//! `load_persistent_config` collected N `LoadMessage`s, showed ONE of them in a
//! summary echo (`"config: 3 issues (first: …)"`), and dropped the vector. A
//! status line is overwritten by the next keystroke, and at boot it is gone
//! before the user has done anything — which is precisely when a config error
//! matters and when it is least likely to be read. Two of three refused options
//! left no trace at all.
//!
//! This is the general defect behind OC.11's specific one: org's capture could
//! not tell a REFUSED `capture-templates` from an unset one, and the user could
//! not find out either, because the message that said so had scrolled away.
//! Making the diagnostics durable does not give the guest a new seam — it gives
//! the person a way to answer "why is this not doing what I configured".
//!
//! Design: `cross-file-writes.md` is unrelated; the option system is
//! `docs/dev/architecture/configuration.md`. Slice plan:
//! `slice-plans/org-capture.md` (OC.11a).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_runtime::{EventBus, MessagesLayer, MessagesRing};
use tracing_subscriber::layer::SubscriberExt;

/// A workspace whose project config has `count` distinct problems in it.
///
/// Written as a real file and loaded through the real loader rather than by
/// synthesising `LoadMessage`s: the thing under test is that the host does not
/// drop what the loader produced, and a hand-built vector would not prove the
/// loader still produces it.
fn workspace_with_bad_config(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("lattice-oc11a-{tag}-"))
        .tempdir()
        .unwrap();
    let cfg = dir.path().join(".lattice");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(
        cfg.join("config.toml"),
        // Three separate failures, on purpose — one is not enough to catch the
        // bug, which was "only the first survives".
        //
        // Deliberately NOT three of a kind: `tabstop` is a real option given a
        // value of the wrong type (a validation failure), the other two are
        // names that do not exist (lookup failures). They travel different
        // paths through the loader, and a test that only proved one path
        // survived would not have shown the other did.
        //
        // Option names are TOP-LEVEL, mirroring the option name verbatim —
        // there is no `[editor]` table. Writing them under one is what made
        // the first draft of this fixture report `editor.tabstop` as an
        // unknown option and quietly test nothing about validation.
        concat!(
            "tabstop = \"not-a-number\"\n",
            "definitely-not-an-option = 1\n",
            "another-nonexistent-option = true\n",
        ),
    )
    .unwrap();
    dir
}

/// Drive `load_persistent_config` under a per-test subscriber and return every
/// line that reached the `*messages*` ring.
///
/// `with_default` rather than the global installer, which can only run once per
/// process — the pattern `messages_subscriber`'s own tests use.
fn messages_from_loading(root: &std::path::Path) -> Vec<String> {
    let ring = Arc::new(Mutex::new(MessagesRing::with_capacity(64)));
    let bus = Arc::new(EventBus::new());
    let layer = MessagesLayer::new(ring.clone(), bus);
    let subscriber = tracing_subscriber::registry().with(layer);

    tracing::subscriber::with_default(subscriber, || {
        let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
        let _ = editor.load_persistent_config(Some(root));
    });

    let ring = ring.lock().unwrap();
    ring.records().iter().map(|r| r.text.clone()).collect()
}

/// **Every** issue reaches `*messages*`, not just the one the echo showed.
#[test]
fn every_config_issue_reaches_the_messages_ring() {
    let dir = workspace_with_bad_config("all");
    let lines = messages_from_loading(dir.path());

    let config_lines: Vec<&String> = lines.iter().filter(|l| l.contains("config ")).collect();
    assert!(
        config_lines.len() >= 3,
        "all three problems are recorded, not just the first: {lines:#?}"
    );

    for wanted in [
        "tabstop",
        "definitely-not-an-option",
        "another-nonexistent-option",
    ] {
        assert!(
            config_lines.iter().any(|l| l.contains(wanted)),
            "`{wanted}` left a durable record: {config_lines:#?}"
        );
    }
}

/// Each line names the FILE it came from.
///
/// A user with both a user and a project config gets issues from two places,
/// and "which file" is half of what makes the message actionable — without it
/// the user knows an option was refused and not where to go and fix it.
#[test]
fn a_diagnostic_names_the_file_it_came_from() {
    let dir = workspace_with_bad_config("source");
    let lines = messages_from_loading(dir.path());

    let named = lines
        .iter()
        .find(|l| l.contains("definitely-not-an-option"))
        .expect("the unknown option was reported");
    assert!(
        named.contains("config.toml"),
        "the diagnostic says which file to edit: {named}"
    );
}

/// A clean config is silent. The durable record must not become noise every
/// editor start — a log line per boot for a config with nothing wrong with it
/// is how `*messages*` stops being read.
#[test]
fn a_clean_config_records_nothing() {
    let dir = tempfile::Builder::new()
        .prefix("lattice-oc11a-clean-")
        .tempdir()
        .unwrap();
    let cfg = dir.path().join(".lattice");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.toml"), "tabstop = 4\nnumber = true\n").unwrap();

    let lines = messages_from_loading(dir.path());
    assert!(
        !lines.iter().any(|l| l.contains("config ")),
        "nothing to report, nothing reported: {lines:#?}"
    );
}

/// The echo points at where the rest of them are.
///
/// The count alone told the user two more existed and gave them no way to read
/// either. A durable record nobody knows about is not much better than none.
#[test]
fn the_summary_echo_names_where_the_rest_are() {
    let dir = workspace_with_bad_config("echo");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let _ = editor.load_persistent_config(Some(dir.path()));

    let msg = editor
        .last_message
        .as_ref()
        .map(|m| m.text.clone())
        .unwrap_or_default();
    assert!(msg.contains("issues"), "the echo still summarises: {msg}");
    assert!(
        msg.contains(":messages"),
        "and says where to read all of them: {msg}"
    );
}

// ── OC.11c: a failed assignment is answerable, not just loggable ────────────
//
// OC.11a made diagnostics readable by the USER. This half makes them readable
// by the plugin that declared the option, which is a different problem: a
// failed assignment is a no-op (vim's rule, kept), so the option keeps its
// registered default and reading it back cannot distinguish "the user
// configured this and it did not parse" from "the user never configured this".
// org-capture filed notes through a legacy fallback for exactly that reason.

/// The registry answers WHY an option is at its default, when the reason is a
/// failed assignment.
#[test]
fn a_failed_set_is_recorded_against_the_option() {
    let dir = workspace_with_bad_config("recorded");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let _ = editor.load_persistent_config(Some(dir.path()));

    let found = editor
        .config
        .failed_assignment("tabstop")
        .expect("`tabstop = \"not-a-number\"` failed and is recorded");
    assert!(
        found.message.contains("tabstop"),
        "the message names the option: {}",
        found.message
    );
    assert!(
        found.source.is_some(),
        "and the file it came from, which is where the user goes to fix it"
    );
}

/// An option that loaded FINE has nothing recorded.
///
/// The half that keeps this from being "every option looks broken": `None` is
/// the answer for a clean load, and a consumer treats it as "trust the value".
#[test]
fn an_option_that_loaded_cleanly_has_no_record() {
    let dir = workspace_with_bad_config("clean-option");
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let _ = editor.load_persistent_config(Some(dir.path()));

    assert!(
        editor.config.failed_assignment("number").is_none(),
        "an option the bad config never mentions is not implicated by it"
    );
}

/// **A successful `:set` clears the record.**
///
/// The record describes the LAST assignment. Once a later one succeeds it
/// describes something untrue, and a user who fixes their value at runtime
/// must stop being told it is broken. Cleared inside `parse_and_set_command`
/// — the chokepoint every `:set` goes through — rather than by each caller,
/// which is the shape that gets forgotten.
#[test]
fn a_later_successful_set_clears_the_record() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));

    assert!(
        editor
            .config
            .parse_and_set_command("tabstop=999999")
            .is_err()
    );
    assert!(
        editor.config.failed_assignment("tabstop").is_some(),
        "the failure is recorded"
    );

    editor
        .config
        .parse_and_set_command("tabstop=4")
        .expect("a good value sets");
    assert!(
        editor.config.failed_assignment("tabstop").is_none(),
        "and a value that works retires it"
    );
}

/// A runtime `:set` failure carries NO source — it did not come from a file.
///
/// "Go fix your config" and "what you just typed did not take" send the user
/// to different places, so the two must stay distinguishable.
#[test]
fn a_runtime_set_failure_has_no_source_file() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    assert!(
        editor
            .config
            .parse_and_set_command("tabstop=999999")
            .is_err()
    );

    let found = editor.config.failed_assignment("tabstop").unwrap();
    assert!(found.source.is_none(), "a `:set` has no file behind it");
}

/// An UNKNOWN option records nothing.
///
/// There is no option for the diagnostic to be about, and keying one under a
/// typo would let `:set tabstpo=4` shadow the real `tabstop` a plugin then
/// asks about — a misattribution worse than the silence it replaces.
#[test]
fn an_unknown_option_records_nothing() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    assert!(editor.config.parse_and_set_command("tabstpo=4").is_err());

    assert!(editor.config.failed_assignment("tabstpo").is_none());
    assert!(
        editor.config.failed_assignment("tabstop").is_none(),
        "and the real option it was a typo OF is untouched"
    );
}

/// A failure is recorded under the option's CANONICAL name, not the alias the
/// user typed.
///
/// A plugin asking about its own config uses the name it declared. An
/// alias-keyed record is invisible to every reader.
#[test]
fn a_failure_records_under_the_canonical_name() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    // `ts` is tabstop's vim alias.
    assert!(editor.config.parse_and_set_command("ts=999999").is_err());

    assert!(
        editor.config.failed_assignment("tabstop").is_some(),
        "recorded under the declared name"
    );
    assert!(
        editor.config.failed_assignment("ts").is_some(),
        "and reachable through the alias too, since lookups canonicalise"
    );
}

/// A RELOAD rebuilds the record wholesale.
///
/// A load is a fresh reading of the whole configuration, so a per-message
/// update would leave a diagnostic behind forever for an option whose failing
/// line the user DELETED — there is no message to update it with.
#[test]
fn a_reload_forgets_what_the_user_fixed() {
    let dir = tempfile::Builder::new()
        .prefix("lattice-oc11c-reload-")
        .tempdir()
        .unwrap();
    let cfg = dir.path().join(".lattice");
    std::fs::create_dir_all(&cfg).unwrap();
    std::fs::write(cfg.join("config.toml"), "tabstop = \"not-a-number\"\n").unwrap();

    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let _ = editor.load_persistent_config(Some(dir.path()));
    assert!(editor.config.failed_assignment("tabstop").is_some());

    // The user fixes it and reloads.
    std::fs::write(cfg.join("config.toml"), "tabstop = 4\n").unwrap();
    let _ = editor.load_persistent_config(Some(dir.path()));
    assert!(
        editor.config.failed_assignment("tabstop").is_none(),
        "the complaint goes when the line that caused it does"
    );
}
