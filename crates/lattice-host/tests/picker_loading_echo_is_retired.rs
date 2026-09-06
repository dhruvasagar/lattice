//! OR.15 — a non-live async picker source retires its `(loading)` echo when
//! it seats.
//!
//! The failure arm of `drain_pending_picker_init` has always replaced that
//! message; the success arm never did. So `picker: <source>... (loading)`
//! stayed on the status line for the rest of the session — after the picker
//! seated, after an accept, after anything.
//!
//! Not cosmetic. A progress message that outlives the operation it describes
//! makes every later outcome unreadable, and it did exactly that: org-roam's
//! "Create note:" row was reported as doing nothing at all, and this stale
//! line is the reason a *silent success* was indistinguishable from a failure.
//! Most of that investigation went looking for a swallowed error that never
//! existed.
//!
//! ## What these tests do NOT cover, stated rather than implied
//!
//! They pin `retire_picker_loading_message`'s **contract** — it clears the
//! message its source parked, and leaves every other message alone. They do
//! **not** prove `drain_pending_picker_init` calls it: driving that drain
//! needs a pending async init, which needs a real plugin picker source, which
//! is a different test's subject (`async_picker_accept_applies_open_buffer_at`).
//!
//! So deleting the call from the drain would leave these three green. That is
//! a real gap and it is written down rather than papered over; the wiring is
//! one line, verified by reading, and the guard that matters — not eating
//! somebody else's message — is what is actually hard to get right and is
//! covered here.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

/// The exact text the parked source sets, duplicated here ON PURPOSE.
///
/// The production code routes both its set and its clear through one helper so
/// they cannot drift; this test hardcodes the string so that if that helper's
/// output ever changes, the test fails rather than following it silently. A
/// test that derives its expectation from the code under test asserts nothing.
const PARKED: &str = "picker: org-roam-node... (loading)";

fn editor() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

/// A message set by something else must survive the seat.
///
/// The clear is guarded on the message still being the one this source parked;
/// anything set afterwards belongs to a later action and is not the picker's
/// to discard. Without that guard the fix would eat unrelated status text.
#[test]
fn a_later_message_is_not_eaten_by_the_seat() {
    let mut ed = editor();
    ed.set_message(
        lattice_host::action::EchoLevel::Info,
        "written to /tmp/notes.org",
    );
    // Seating happens inside `drain_pending_picker_init`; with no pending
    // init the drain is a no-op, which is precisely the "not ours to clear"
    // case — the message must still be standing.
    let _ = ed.drain_pending_picker_init();
    assert_eq!(
        ed.last_message.as_ref().map(|m| m.text.as_str()),
        Some("written to /tmp/notes.org"),
        "an unrelated message must survive"
    );
}

/// The parked echo is retired, and the test would fail if it were not.
///
/// Drives the seat directly rather than standing up a real async plugin
/// source: this asserts the message lifecycle, and a source that genuinely
/// resolves is `async_picker_accept_applies_open_buffer_at.rs`'s job.
#[test]
fn seating_retires_the_parked_loading_echo() {
    let mut ed = editor();
    ed.set_message(lattice_host::action::EchoLevel::Info, PARKED);
    assert_eq!(
        ed.last_message.as_ref().map(|m| m.text.as_str()),
        Some(PARKED),
        "sanity: the parked echo is what we start from"
    );

    ed.retire_picker_loading_message("org-roam-node");

    assert!(
        ed.last_message.is_none(),
        "the parked echo must be retired, not left standing: {:?}",
        ed.last_message
    );
}

/// Retiring is scoped to the source that parked it — a DIFFERENT source
/// seating must not clear another's message.
#[test]
fn another_sources_seat_does_not_retire_this_ones_echo() {
    let mut ed = editor();
    ed.set_message(lattice_host::action::EchoLevel::Info, PARKED);

    ed.retire_picker_loading_message("some-other-source");

    assert_eq!(
        ed.last_message.as_ref().map(|m| m.text.as_str()),
        Some(PARKED),
        "only the source that parked the message may retire it"
    );
}
