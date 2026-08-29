//! The event/hook boundary conversions (plugin-host.md §5 `events`, PH7.8a).
//!
//! Mirrors `lattice_runtime::EventBus` + the `lattice_protocol::Event` enum —
//! the observation-only hook surface a plugin subscribes to. Two directions:
//!
//!   - **`Event`** crosses **host→guest** (delivered to the guest `on-event`
//!     export). It is fully owned (no borrows), so it round-trips both ways
//!     compiler-exhaustively — the honest "every variant covered" guarantee: a
//!     new `Event` arm cannot land without a mapping here (the `effect` mirror
//!     precedent). Paths cross as `string`; a non-UTF-8 path is a typed
//!     boundary error, never a lossy `to_string_lossy` (`path_to_wit`).
//!
//!   - **`EventFilter`** crosses **guest→host** (a plugin's `subscribe(filter)`
//!     at `activate`). It is a ONE-WAY projection (`project_event_filter`): the
//!     native `EventFilter` cannot round-trip — its `GlobSet` doesn't reverse to
//!     patterns and its `predicate` is an arbitrary Rust closure. The
//!     declarative subset (`kinds` / `path-globs` / `major-modes`) crosses; a
//!     plugin that needs custom logic filters inside its own `on-event` handler
//!     (the grammar typed-error-defer precedent). This is the reverse of the
//!     grammar `project_*` fns (there native→WIT; here WIT→native).

use crate::WitBoundary;
use crate::boundary::path_to_wit;
use crate::lattice::plugin_host::types::{
    Event as WitEvent, EventAppliedEdit as WitEventAppliedEdit, EventDocumentChanged,
    EventDocumentOpened, EventDocumentPath, EventFilter as WitEventFilter,
    EventKind as WitEventKind, EventModalModeChanged, EventModeLifecycle, EventOptionChanged,
    EventPlugin as WitEventPlugin, EventPluginLifecycle as WitEventPluginLifecycle,
    EventSelectionsChanged,
};
use lattice_keymap::ModeId;
use lattice_protocol::event::AppliedEdit as NativeEventAppliedEdit;
use lattice_protocol::ids::{BufferId, DocumentId};
use lattice_protocol::position::Range as NativeRange;
use lattice_protocol::selection::SelectionSet as NativeSelectionSet;
use lattice_protocol::{Event as NativeEvent, EventKind as NativeEventKind};
use lattice_runtime::{EventFilter as NativeEventFilter, compile_glob_set};

/// `Option<PathBuf>` → `option<string>`, non-UTF-8 a typed error (mirrors the
/// private `opt_path_to_wit` in `boundary_effect`; kept local to avoid widening
/// that module's surface for one caller).
fn opt_path_to_wit(path: &Option<std::path::PathBuf>) -> Result<Option<String>, String> {
    path.as_ref().map(|p| path_to_wit(p)).transpose()
}

impl WitBoundary for NativeEventAppliedEdit {
    type Wit = WitEventAppliedEdit;

    fn to_wit(&self) -> Result<WitEventAppliedEdit, String> {
        Ok(WitEventAppliedEdit {
            original_range: self.original_range.to_wit()?,
            inserted_range: self.inserted_range.to_wit()?,
            replaced_text: self.replaced_text.clone(),
            inserted_text: self.inserted_text.clone(),
        })
    }

    fn from_wit(wit: WitEventAppliedEdit) -> Result<Self, String> {
        Ok(NativeEventAppliedEdit {
            original_range: NativeRange::from_wit(wit.original_range)?,
            inserted_range: NativeRange::from_wit(wit.inserted_range)?,
            replaced_text: wit.replaced_text,
            inserted_text: wit.inserted_text,
        })
    }
}

impl WitBoundary for NativeEventKind {
    type Wit = WitEventKind;

    fn to_wit(&self) -> Result<WitEventKind, String> {
        Ok(match self {
            NativeEventKind::DocumentOpened => WitEventKind::DocumentOpened,
            NativeEventKind::DocumentClosed => WitEventKind::DocumentClosed,
            NativeEventKind::BeforeSave => WitEventKind::BeforeSave,
            NativeEventKind::DocumentSaved => WitEventKind::DocumentSaved,
            NativeEventKind::DocumentChanged => WitEventKind::DocumentChanged,
            NativeEventKind::SelectionsChanged => WitEventKind::SelectionsChanged,
            NativeEventKind::ModalModeChanged => WitEventKind::ModalModeChanged,
            NativeEventKind::BeforeQuit => WitEventKind::BeforeQuit,
            NativeEventKind::OptionChanged => WitEventKind::OptionChanged,
            NativeEventKind::MajorEntered => WitEventKind::MajorEntered,
            NativeEventKind::MajorExiting => WitEventKind::MajorExiting,
            NativeEventKind::MinorActivated => WitEventKind::MinorActivated,
            NativeEventKind::MinorDeactivated => WitEventKind::MinorDeactivated,
            NativeEventKind::Plugin => WitEventKind::Plugin,
            // PH7.12: the host-internal crash/quarantine signal. Not projected to
            // guests in v1 -- the WIT `event-kind` has no `plugin-crashed`
            // variant, so a plugin cannot declare it in a filter and this arm is
            // unreachable in practice. Kept as a typed error (never a silent
            // drop) to honour the boundary's non-lossy discipline; a Phase-8
            // monitoring plugin that needs it adds the WIT variant then.
            NativeEventKind::PluginCrashed => {
                return Err(
                    "event-kind `plugin-crashed` is host-internal, not deliverable to plugins"
                        .to_string(),
                );
            }
            // CI.1: plugin-lifecycle signals ARE deliverable — an init.rs
            // subscribes to run deferred config.
            NativeEventKind::PluginLoaded => WitEventKind::PluginLoaded,
            NativeEventKind::PluginUnloaded => WitEventKind::PluginUnloaded,
            // CI.4: host-internal enable/disable bridge — the Editor handles it,
            // never a guest (no WIT variant), like `plugin-crashed`.
            NativeEventKind::ModeEnablementRequested => {
                return Err(
                    "event-kind `mode-enablement-requested` is host-internal, not deliverable to plugins"
                        .to_string(),
                );
            }
            // MG.41g: host-internal FOR NOW. The design intends plugins
            // to both publish and receive this (a plugin's async work
            // should get completion reporting for free), which needs a
            // WIT variant + payload mirror. Additive when it lands;
            // erroring here keeps the boundary honest until then rather
            // than silently dropping the event.
            NativeEventKind::BackgroundTaskFinished => {
                return Err(
                    "event-kind `background-task-finished` is not yet mirrored in WIT".to_string(),
                );
            }
            // OR.2: a plugin's own directory watch fired.
            NativeEventKind::FilesChanged => WitEventKind::FilesChanged,
        })
    }

    fn from_wit(wit: WitEventKind) -> Result<Self, String> {
        Ok(match wit {
            WitEventKind::DocumentOpened => NativeEventKind::DocumentOpened,
            WitEventKind::DocumentClosed => NativeEventKind::DocumentClosed,
            WitEventKind::BeforeSave => NativeEventKind::BeforeSave,
            WitEventKind::DocumentSaved => NativeEventKind::DocumentSaved,
            WitEventKind::DocumentChanged => NativeEventKind::DocumentChanged,
            WitEventKind::SelectionsChanged => NativeEventKind::SelectionsChanged,
            WitEventKind::ModalModeChanged => NativeEventKind::ModalModeChanged,
            WitEventKind::BeforeQuit => NativeEventKind::BeforeQuit,
            WitEventKind::OptionChanged => NativeEventKind::OptionChanged,
            WitEventKind::MajorEntered => NativeEventKind::MajorEntered,
            WitEventKind::MajorExiting => NativeEventKind::MajorExiting,
            WitEventKind::MinorActivated => NativeEventKind::MinorActivated,
            WitEventKind::MinorDeactivated => NativeEventKind::MinorDeactivated,
            WitEventKind::Plugin => NativeEventKind::Plugin,
            WitEventKind::PluginLoaded => NativeEventKind::PluginLoaded,
            WitEventKind::PluginUnloaded => NativeEventKind::PluginUnloaded,
            // OR.2: what a guest passes to `subscribe` to hear its own watch.
            WitEventKind::FilesChanged => NativeEventKind::FilesChanged,
        })
    }
}

impl WitBoundary for NativeEvent {
    type Wit = WitEvent;

    /// Compiler-exhaustive: a new `Event` arm forces a mapping here. Ids cross
    /// as `u64` (`.raw()`); paths via `path_to_wit` (non-UTF-8 → typed error).
    fn to_wit(&self) -> Result<WitEvent, String> {
        Ok(match self {
            NativeEvent::DocumentOpened {
                id,
                path,
                version,
                text,
            } => WitEvent::DocumentOpened(EventDocumentOpened {
                id: id.raw(),
                path: opt_path_to_wit(path)?,
                version: *version,
                text: text.clone(),
            }),
            NativeEvent::DocumentClosed { id } => WitEvent::DocumentClosed(id.raw()),
            NativeEvent::BeforeSave { id, path } => WitEvent::BeforeSave(EventDocumentPath {
                id: id.raw(),
                path: path_to_wit(path)?,
            }),
            NativeEvent::DocumentSaved { id, path } => WitEvent::DocumentSaved(EventDocumentPath {
                id: id.raw(),
                path: path_to_wit(path)?,
            }),
            NativeEvent::DocumentChanged {
                id,
                path,
                version,
                edits,
            } => WitEvent::DocumentChanged(EventDocumentChanged {
                id: id.raw(),
                path: opt_path_to_wit(path)?,
                version: *version,
                edits: edits
                    .iter()
                    .map(WitBoundary::to_wit)
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            NativeEvent::SelectionsChanged {
                id,
                version,
                selections,
            } => WitEvent::SelectionsChanged(EventSelectionsChanged {
                id: id.raw(),
                version: *version,
                selections: selections.to_wit()?,
            }),
            NativeEvent::ModalModeChanged { from, to } => {
                WitEvent::ModalModeChanged(EventModalModeChanged {
                    from_state: from.clone(),
                    to_state: to.clone(),
                })
            }
            NativeEvent::BeforeQuit => WitEvent::BeforeQuit,
            NativeEvent::OptionChanged { name, old, new } => {
                WitEvent::OptionChanged(EventOptionChanged {
                    name: name.clone(),
                    old: old.clone(),
                    new_value: new.clone(),
                })
            }
            NativeEvent::MajorEntered { buffer, major } => {
                WitEvent::MajorEntered(mode_lifecycle(buffer, major))
            }
            NativeEvent::MajorExiting { buffer, major } => {
                WitEvent::MajorExiting(mode_lifecycle(buffer, major))
            }
            NativeEvent::MinorActivated { buffer, minor } => {
                WitEvent::MinorActivated(mode_lifecycle(buffer, minor))
            }
            NativeEvent::MinorDeactivated { buffer, minor } => {
                WitEvent::MinorDeactivated(mode_lifecycle(buffer, minor))
            }
            // Opaque bytes cross verbatim — the host is a thin router (PH7.8b).
            NativeEvent::Plugin { name, payload } => WitEvent::Plugin(WitEventPlugin {
                name: name.clone(),
                payload: payload.clone(),
            }),
            // PH7.12: host-internal crash/quarantine signal — never routed to a
            // guest (no WIT variant to subscribe to), so this is unreachable in
            // practice; a typed error keeps the boundary non-lossy.
            NativeEvent::PluginCrashed { .. } => {
                return Err(
                    "event `plugin-crashed` is host-internal, not deliverable to plugins"
                        .to_string(),
                );
            }
            // CI.1: plugin-lifecycle signals ARE delivered to guests.
            NativeEvent::PluginLoaded { name, id } => {
                WitEvent::PluginLoaded(WitEventPluginLifecycle {
                    name: name.clone(),
                    id: *id,
                })
            }
            NativeEvent::PluginUnloaded { name, id } => {
                WitEvent::PluginUnloaded(WitEventPluginLifecycle {
                    name: name.clone(),
                    id: *id,
                })
            }
            // CI.4: host-internal — never routed to a guest.
            NativeEvent::ModeEnablementRequested { .. } => {
                return Err(
                    "event `mode-enablement-requested` is host-internal, not deliverable to plugins"
                        .to_string(),
                );
            }
            // MG.41g: see the `EventKind` arm above.
            NativeEvent::BackgroundTaskFinished { .. } => {
                return Err(
                    "event `background-task-finished` is not yet mirrored in WIT".to_string(),
                );
            }
            // OR.2. The plugin id does NOT cross: the delivery actor has
            // already established that this batch belongs to the guest it is
            // about to hand it to, so sending the id would be telling a plugin
            // its own name. A non-UTF-8 path is SKIPPED rather than failing the
            // batch — `walk`'s rule, for `walk`'s reason: one oddly-named file
            // must not cost an index every other file in the same burst.
            NativeEvent::FilesChanged { paths, .. } => WitEvent::FilesChanged(
                paths
                    .iter()
                    .filter_map(|p| p.to_str().map(str::to_string))
                    .collect(),
            ),
        })
    }

    fn from_wit(wit: WitEvent) -> Result<Self, String> {
        Ok(match wit {
            WitEvent::DocumentOpened(p) => NativeEvent::DocumentOpened {
                id: DocumentId::new(p.id),
                path: p.path.map(std::path::PathBuf::from),
                version: p.version,
                text: p.text,
            },
            WitEvent::DocumentClosed(id) => NativeEvent::DocumentClosed {
                id: DocumentId::new(id),
            },
            WitEvent::BeforeSave(p) => NativeEvent::BeforeSave {
                id: DocumentId::new(p.id),
                path: std::path::PathBuf::from(p.path),
            },
            WitEvent::DocumentSaved(p) => NativeEvent::DocumentSaved {
                id: DocumentId::new(p.id),
                path: std::path::PathBuf::from(p.path),
            },
            WitEvent::DocumentChanged(p) => NativeEvent::DocumentChanged {
                id: DocumentId::new(p.id),
                path: p.path.map(std::path::PathBuf::from),
                version: p.version,
                edits: p
                    .edits
                    .into_iter()
                    .map(NativeEventAppliedEdit::from_wit)
                    .collect::<Result<Vec<_>, _>>()?,
            },
            WitEvent::SelectionsChanged(p) => NativeEvent::SelectionsChanged {
                id: DocumentId::new(p.id),
                version: p.version,
                selections: NativeSelectionSet::from_wit(p.selections)?,
            },
            WitEvent::ModalModeChanged(p) => NativeEvent::ModalModeChanged {
                from: p.from_state,
                to: p.to_state,
            },
            WitEvent::BeforeQuit => NativeEvent::BeforeQuit,
            WitEvent::OptionChanged(p) => NativeEvent::OptionChanged {
                name: p.name,
                old: p.old,
                new: p.new_value,
            },
            WitEvent::MajorEntered(p) => NativeEvent::MajorEntered {
                buffer: BufferId::new(p.buffer),
                major: p.mode,
            },
            WitEvent::MajorExiting(p) => NativeEvent::MajorExiting {
                buffer: BufferId::new(p.buffer),
                major: p.mode,
            },
            WitEvent::MinorActivated(p) => NativeEvent::MinorActivated {
                buffer: BufferId::new(p.buffer),
                minor: p.mode,
            },
            WitEvent::MinorDeactivated(p) => NativeEvent::MinorDeactivated {
                buffer: BufferId::new(p.buffer),
                minor: p.mode,
            },
            WitEvent::Plugin(p) => NativeEvent::Plugin {
                name: p.name,
                payload: p.payload,
            },
            WitEvent::PluginLoaded(p) => NativeEvent::PluginLoaded {
                name: p.name,
                id: p.id,
            },
            WitEvent::PluginUnloaded(p) => NativeEvent::PluginUnloaded {
                name: p.name,
                id: p.id,
            },
            // OR.2. A guest never publishes one of these — the host originates
            // every watch batch — so `plugin` has no source but `0`. That is
            // the honest value rather than a fabricated one: an unaddressed
            // batch matches no actor's id and is therefore delivered to nobody,
            // which is the correct outcome for an event a guest invented.
            WitEvent::FilesChanged(paths) => NativeEvent::FilesChanged {
                plugin: 0,
                paths: paths.into_iter().map(std::path::PathBuf::from).collect(),
            },
        })
    }
}

/// The shared `major/minor` lifecycle payload — buffer id + the mode's
/// canonical name (the four lifecycle arms share one WIT record).
fn mode_lifecycle(buffer: &BufferId, mode: &str) -> EventModeLifecycle {
    EventModeLifecycle {
        buffer: buffer.raw(),
        mode: mode.to_string(),
    }
}

/// Project a guest-supplied [`WitEventFilter`] into the native
/// [`EventFilter`](NativeEventFilter) (guest→host, one-way). The declarative
/// fields cross; the native `predicate` is always `None` (a Rust closure can't
/// cross — the guest filters in its `on-event` handler instead). `kinds = none`
/// stays the wildcard; `path-globs` compile via the same `compile_glob_set` the
/// native subscribers use, so a plugin filter matches identically to a native
/// one.
pub fn project_event_filter(wit: WitEventFilter) -> Result<NativeEventFilter, String> {
    let kinds = wit
        .kinds
        .map(|ks| {
            ks.into_iter()
                .map(NativeEventKind::from_wit)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    Ok(NativeEventFilter {
        kinds,
        path_glob: wit.path_globs.map(compile_glob_set),
        major_modes: wit
            .major_modes
            .map(|ms| ms.iter().map(|m| ModeId::new(m)).collect()),
        predicate: None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_protocol::position::Position;
    use lattice_protocol::selection::{Selection, SelectionSet};

    fn pos(line: u32, byte: u32) -> Position {
        Position { line, byte }
    }

    fn rng(l0: u32, b0: u32, l1: u32, b1: u32) -> NativeRange {
        NativeRange {
            start: pos(l0, b0),
            end: pos(l1, b1),
        }
    }

    /// Round-trip an event through WIT and assert equality of its debug form —
    /// `Event` isn't `PartialEq`, so we compare the structural debug rendering,
    /// which is faithful for these owned payloads.
    fn round_trip(ev: NativeEvent) {
        let wit = ev.to_wit().unwrap();
        let back = NativeEvent::from_wit(wit).unwrap();
        assert_eq!(format!("{ev:?}"), format!("{back:?}"));
    }

    #[test]
    fn document_lifecycle_arms_round_trip() {
        round_trip(NativeEvent::DocumentOpened {
            id: DocumentId::new(7),
            path: Some(std::path::PathBuf::from("src/lib.rs")),
            version: 3,
            text: "fn main() {}\n".into(),
        });
        round_trip(NativeEvent::DocumentOpened {
            id: DocumentId::new(8),
            path: None, // scratch buffer
            version: 0,
            text: String::new(),
        });
        round_trip(NativeEvent::DocumentClosed {
            id: DocumentId::new(7),
        });
        round_trip(NativeEvent::BeforeSave {
            id: DocumentId::new(1),
            path: std::path::PathBuf::from("/tmp/a.rs"),
        });
        round_trip(NativeEvent::DocumentSaved {
            id: DocumentId::new(1),
            path: std::path::PathBuf::from("/tmp/a.rs"),
        });
    }

    #[test]
    fn document_changed_carries_edits_without_delta() {
        round_trip(NativeEvent::DocumentChanged {
            id: DocumentId::new(2),
            path: Some(std::path::PathBuf::from("x.rs")),
            version: 4,
            edits: vec![NativeEventAppliedEdit {
                original_range: rng(0, 0, 0, 3),
                inserted_range: rng(0, 0, 0, 5),
                replaced_text: "abc".into(),
                inserted_text: "hello".into(),
            }],
        });
    }

    #[test]
    fn selections_changed_round_trips_the_selection_set() {
        let set = SelectionSet::from_parts(
            vec![Selection {
                anchor: pos(0, 0),
                head: pos(0, 4),
                visual: None,
            }],
            0,
        );
        round_trip(NativeEvent::SelectionsChanged {
            id: DocumentId::new(3),
            version: 9,
            selections: set,
        });
    }

    #[test]
    fn scalar_arms_round_trip() {
        round_trip(NativeEvent::ModalModeChanged {
            from: "Normal".into(),
            to: "Insert".into(),
        });
        round_trip(NativeEvent::BeforeQuit);
        round_trip(NativeEvent::OptionChanged {
            name: "wrap".into(),
            old: Some("off".into()),
            new: "on".into(),
        });
        round_trip(NativeEvent::OptionChanged {
            name: "wrap".into(),
            old: None, // first publish after registration
            new: "off".into(),
        });
    }

    #[test]
    fn mode_lifecycle_arms_round_trip() {
        for ev in [
            NativeEvent::MajorEntered {
                buffer: BufferId::new(5),
                major: "rust-mode".into(),
            },
            NativeEvent::MajorExiting {
                buffer: BufferId::new(5),
                major: "rust-mode".into(),
            },
            NativeEvent::MinorActivated {
                buffer: BufferId::new(5),
                minor: "diff-mode".into(),
            },
            NativeEvent::MinorDeactivated {
                buffer: BufferId::new(5),
                minor: "diff-mode".into(),
            },
        ] {
            round_trip(ev);
        }
    }

    #[test]
    fn event_kind_round_trips_every_arm() {
        for kind in [
            NativeEventKind::DocumentOpened,
            NativeEventKind::DocumentClosed,
            NativeEventKind::BeforeSave,
            NativeEventKind::DocumentSaved,
            NativeEventKind::DocumentChanged,
            NativeEventKind::SelectionsChanged,
            NativeEventKind::ModalModeChanged,
            NativeEventKind::BeforeQuit,
            NativeEventKind::OptionChanged,
            NativeEventKind::MajorEntered,
            NativeEventKind::MajorExiting,
            NativeEventKind::MinorActivated,
            NativeEventKind::MinorDeactivated,
            NativeEventKind::Plugin,
        ] {
            assert_eq!(
                NativeEventKind::from_wit(kind.to_wit().unwrap()).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn plugin_event_round_trips_opaque_payload() {
        // The host is a thin router: the name + arbitrary bytes cross verbatim,
        // including a non-UTF-8 payload (MessagePack is binary, not text).
        round_trip(NativeEvent::Plugin {
            name: "git-gutter.hunks-changed".into(),
            payload: vec![0x00, 0x91, 0xff, 0x7f, 0xde],
        });
        round_trip(NativeEvent::Plugin {
            name: "empty".into(),
            payload: Vec::new(),
        });
    }

    #[test]
    fn non_utf8_path_is_a_typed_error() {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let bad = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(b"/tmp/\xff.rs"));
            let ev = NativeEvent::DocumentSaved {
                id: DocumentId::new(1),
                path: bad,
            };
            assert!(
                ev.to_wit().is_err(),
                "non-UTF-8 path must not cross lossily"
            );
        }
    }

    #[test]
    fn project_event_filter_crosses_declarative_fields() {
        let wit = WitEventFilter {
            kinds: Some(vec![WitEventKind::DocumentSaved, WitEventKind::BeforeQuit]),
            path_globs: Some(vec!["**/*.rs".into()]),
            major_modes: Some(vec!["rust-mode".into()]),
        };
        let native = project_event_filter(wit).unwrap();
        let kinds = native.kinds.expect("kinds crossed");
        assert_eq!(kinds.len(), 2);
        assert!(kinds.contains(&NativeEventKind::DocumentSaved));
        assert!(native.path_glob.is_some(), "path glob compiled");
        let modes = native.major_modes.expect("major modes crossed");
        assert_eq!(modes[0].as_str(), "rust-mode");
        // The predicate never crosses — a plugin filters in `on-event`.
        assert!(native.predicate.is_none());
    }

    #[test]
    fn project_event_filter_wildcard_is_unconstrained() {
        let wit = WitEventFilter {
            kinds: None,
            path_globs: None,
            major_modes: None,
        };
        let native = project_event_filter(wit).unwrap();
        assert!(native.kinds.is_none(), "none kinds stays the wildcard");
        assert!(native.path_glob.is_none());
        assert!(native.major_modes.is_none());
    }
}
