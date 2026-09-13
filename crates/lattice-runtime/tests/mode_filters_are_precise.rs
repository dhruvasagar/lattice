//! A mode-filtered subscription fires for THAT mode and nothing else.
//!
//! The property matters more for minors than majors: there are far more of
//! them, they activate in bursts as a buffer opens, and a subscriber that has
//! to compare names in its own handler gets woken for every one. For a plugin
//! that means a WASM call per activation per buffer, to do nothing.
//!
//! Before `minor_modes` existed there was no way to say it at all — the only
//! mode filter was `major_modes`, and `event_major_mode` answers `None` for
//! the minor lifecycle, so a `major_modes`-constrained subscription to
//! `MinorActivated` matched *nothing*. Both halves of that are pinned here:
//! the new filter selects, and the old one still refuses.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_keymap::ModeId;
use lattice_protocol::ids::BufferId;
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};

fn drain(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Event>) -> Vec<String> {
    let mut out = Vec::new();
    while let Ok(evt) = rx.try_recv() {
        out.push(match evt {
            Event::MinorActivated { minor, .. } | Event::MinorDeactivated { minor, .. } => minor,
            Event::MajorEntered { major, .. } | Event::MajorExiting { major, .. } => major,
            other => panic!("unexpected event {other:?}"),
        });
    }
    out
}

fn minor_activated(bus: &EventBus, minor: &str) {
    bus.publish(Event::MinorActivated {
        buffer: BufferId::new(1),
        minor: minor.to_string(),
    });
}

/// The headline: one named minor, and only it.
#[test]
fn a_minor_filter_fires_only_for_that_minor() {
    let bus = EventBus::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter {
            kinds: Some(vec![EventKind::MinorActivated]),
            path_glob: None,
            major_modes: None,
            minor_modes: Some(vec![ModeId::new("auto-pair-mode")]),
            predicate: None,
        },
        SubscriptionTarget::Channel(tx),
    );

    // The burst a real buffer-open produces.
    for m in [
        "auto-pair-mode",
        "read-only-mode",
        "magit-core-mode",
        "which-key-mode",
    ] {
        minor_activated(&bus, m);
    }

    assert_eq!(
        drain(&mut rx),
        vec!["auto-pair-mode".to_string()],
        "the subscription must see its own minor and no other — a handler that \
         has to filter by name is woken for every activation in every buffer"
    );
}

/// …and the major filter stays equally precise, which is the half that was
/// already true and must not regress while its peer is added.
#[test]
fn a_major_filter_fires_only_for_that_major() {
    let bus = EventBus::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter {
            kinds: Some(vec![EventKind::MajorEntered]),
            path_glob: None,
            major_modes: Some(vec![ModeId::new("org-mode")]),
            minor_modes: None,
            predicate: None,
        },
        SubscriptionTarget::Channel(tx),
    );

    for major in ["rust-mode", "org-mode", "markdown-mode"] {
        bus.publish(Event::MajorEntered {
            buffer: BufferId::new(1),
            major: major.to_string(),
        });
    }

    assert_eq!(drain(&mut rx), vec!["org-mode".to_string()]);
}

/// The two namespaces stay disjoint: a minor filter does not catch a major of
/// the same name, nor the reverse.
///
/// This is why they are separate fields rather than one merged `modes` list.
/// A merged filter would answer both questions at once, so a subscription
/// meaning "when this minor turns on" would also fire on a major that happened
/// to share its name — and mode ids are user-chosen strings, so that collision
/// is available to anyone.
#[test]
fn the_two_mode_namespaces_do_not_bleed_into_each_other() {
    let bus = EventBus::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter {
            kinds: Some(vec![EventKind::MinorActivated, EventKind::MajorEntered]),
            path_glob: None,
            major_modes: None,
            minor_modes: Some(vec![ModeId::new("shared-name-mode")]),
            predicate: None,
        },
        SubscriptionTarget::Channel(tx),
    );

    bus.publish(Event::MajorEntered {
        buffer: BufferId::new(1),
        major: "shared-name-mode".to_string(),
    });
    minor_activated(&bus, "shared-name-mode");

    assert_eq!(
        drain(&mut rx),
        vec!["shared-name-mode".to_string()],
        "exactly one — the MINOR activation. The major of the same name is a \
         different question and a `minor_modes` filter must not answer it"
    );
}

/// The old filter still refuses the minor lifecycle, which is what made the
/// new field necessary rather than a convenience.
#[test]
fn a_major_filter_still_matches_no_minor_event() {
    let bus = EventBus::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter {
            kinds: Some(vec![EventKind::MinorActivated]),
            path_glob: None,
            major_modes: Some(vec![ModeId::new("auto-pair-mode")]),
            minor_modes: None,
            predicate: None,
        },
        SubscriptionTarget::Channel(tx),
    );
    minor_activated(&bus, "auto-pair-mode");

    assert!(
        drain(&mut rx).is_empty(),
        "a `major_modes` constraint rejects every minor event — the trap the \
         `minor_modes` field exists to remove, pinned so the old behaviour \
         cannot be mistaken for the new one working"
    );
}

/// Constraining both matches nothing, because no event carries both names.
/// Stated as a test rather than left to inference: it is the one combination
/// whose emptiness could read as a bug.
#[test]
fn constraining_both_matches_nothing() {
    let bus = EventBus::new();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    bus.subscribe(
        EventFilter {
            kinds: Some(vec![EventKind::MinorActivated, EventKind::MajorEntered]),
            path_glob: None,
            major_modes: Some(vec![ModeId::new("org-mode")]),
            minor_modes: Some(vec![ModeId::new("auto-pair-mode")]),
            predicate: None,
        },
        SubscriptionTarget::Channel(tx),
    );
    minor_activated(&bus, "auto-pair-mode");
    bus.publish(Event::MajorEntered {
        buffer: BufferId::new(1),
        major: "org-mode".to_string(),
    });

    assert!(drain(&mut rx).is_empty());
}
