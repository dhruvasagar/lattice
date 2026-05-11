// `linkme`'s distributed-slice expansion uses `#[link_section]`
// declarations which the workspace's `unsafe_code = "deny"`
// lint flags. Same shape `lattice-lsp::events` and
// `lattice-config::core_options` use.
#![allow(unsafe_code)]

//! Picker-owned editor-bus events (§5.10).
//!
//! `PickerAccepted` is the canonical signal a candidate was
//! chosen. The MRU index subscribes to record on accept;
//! plugins (Phase 7+) can subscribe for telemetry / heatmap
//! views / behavioral learning. The publish path stays on
//! the orchestration thread so the bus delivers the event to
//! every subscriber synchronously -- the MRU subscriber
//! finishes its record-and-persist before the App applies
//! the accept outcome.
//!
//! Why typed events here instead of direct calls: the App's
//! `do_picker_accept` used to call `picker_mru.record`
//! inline. Decoupling lets plugin sources (Phase 7) react
//! without lattice-ui-tui growing per-plugin hooks, mirrors
//! the §5.10 hooks / autocmds unification, and makes the
//! "what fires on accept?" surface introspectable via
//! `:describe-events`.

use std::path::PathBuf;
use std::time::SystemTime;

/// Fired when the user accepts a candidate from a picker
/// (either through `Action::PickerAccept` or the single-
/// match LSP-picker short-circuit). The MRU index
/// subscribes to record + persist; future plugin
/// subscribers can read the same signal.
///
/// `source_id` is the picker registry id (`"files"`,
/// `"commands"`, ...). `identity` is the MRU key derived
/// from the routing payload via
/// `lattice_picker::routing_identity`; `None` means the
/// routing payload has no stable identity (line/col drifts,
/// per-request index, etc.) and the candidate doesn't
/// participate in MRU. Subscribers that care about
/// identity-bearing accepts skip the `None` arm.
///
/// `ts` is the wall-clock instant the accept fired; the MRU
/// subscriber stamps it on the `MruEntry`, while telemetry
/// subscribers can attribute by time window.
#[derive(Debug, Clone)]
pub struct PickerAccepted {
    pub source_id: String,
    pub identity: Option<String>,
    pub routing_payload_path: Option<PathBuf>,
    pub ts: SystemTime,
}

lattice_protocol::register_event!(
    PickerAccepted,
    "picker.accepted",
    "Fired when the user accepts a candidate from a picker. \
     The MRU index subscribes to record; plugins can subscribe \
     for telemetry / behavioral learning.",
    "lattice-picker",
);

/// Fired when a picker opens (`:picker <source>`). Carries
/// the source id so subscribers can react to specific
/// surfaces (e.g. a plugin that wants to inject extra
/// candidates into `:picker grep` when it sees the open).
///
/// Less load-bearing than `PickerAccepted` -- shipped for
/// symmetry + the introspection surface (`:describe-events`
/// shows what an editor can observe). No first-party
/// subscribers today.
#[derive(Debug, Clone)]
pub struct PickerOpened {
    pub source_id: String,
    pub ts: SystemTime,
}

lattice_protocol::register_event!(
    PickerOpened,
    "picker.opened",
    "Fired when a picker opens for a registered source.",
    "lattice-picker",
);

/// Fired when a picker dismisses without an accept (Esc,
/// empty filter, source-side abort). Counterpart to
/// `PickerAccepted` for subscribers that track open-without-
/// accept sessions.
#[derive(Debug, Clone)]
pub struct PickerDismissed {
    pub source_id: String,
    pub ts: SystemTime,
}

lattice_protocol::register_event!(
    PickerDismissed,
    "picker.dismissed",
    "Fired when a picker dismisses without an accept.",
    "lattice-picker",
);
