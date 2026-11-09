//! Programmatic diff requests — the host-drained "open a diff and await the
//! user's verdict" capability that [`DiffSession::bind_completion`] was built
//! for.
//!
//! I4 (Claude Code IDE peer, `openDiff`): an off-thread producer (the IDE
//! peer's WebSocket task) sends a [`ProgrammaticDiffRequest`] over the
//! host-drained inbound bus
//! ([`lattice_mode::inbound::make_inbound_raw`](lattice_mode::inbound::make_inbound_raw)),
//! whose `send` wakes the editor; the host drains it on the actor thread, opens
//! a side-by-side diff (the baseline file vs the proposed text), and
//! `bind_completion`s the request's [`response`](ProgrammaticDiffRequest::response)
//! oneshot to the session. The producer awaits `response` directly — when the
//! user resolves the diff (`:diff-accept` / `:diff-reject`, or a close-tab
//! cancel that drops the session), the existing teardown
//! ([`DiffSession::take_completion`] in `tear_down_single_diff_session`) fires
//! the bound [`DiffOutcome`] back.
//!
//! The type lives here, NOT in the IDE-peer crate, on purpose: the host must
//! drain it and the open is irreducibly `&mut Editor` + lattice-diff types, so
//! keeping the request a *diff-subsystem* type means the host references no
//! IDE-peer internals (preserving the BC.3b invariant that the host carries
//! zero claude-code internals beyond one `install` line), and a second consumer
//! — an LSP `WorkspaceEdit` preview, a magit-style plugin — reuses the same bus.
//! See `StaticSource`'s doc comment, which already names these consumers.
//!
//! [`DiffSession::bind_completion`]: crate::subsystem::DiffSession::bind_completion
//! [`DiffSession::take_completion`]: crate::subsystem::DiffSession::take_completion

use std::path::PathBuf;

use lattice_mode::inbound::InboundBus;
use tokio::sync::oneshot;

use crate::subsystem::DiffOutcome;

/// Where a programmatic diff's left (baseline) side comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffBaseline {
    /// The on-disk content of `old_file_path` (openDiff: the agent proposes a
    /// change to a file).
    Disk,
    /// This text. A change to a *buffer* is reviewed against what the buffer
    /// holds, which may be unsaved or have no file at all; disk would be the
    /// wrong side. `old_file_path` then only names the language and the label.
    Text(String),
}

/// What an Accept does with the right (proposed) side.
#[derive(Debug)]
pub enum OnAccept {
    /// Save it to `new_file_path`: the openDiff contract, where the review is
    /// the save.
    Save,
    /// Hand its final text back, unsaved, and let the producer apply it (an
    /// edit to a buffer, which owns its own undo). The right side then carries
    /// no path, so a stray `:w` in it cannot write over the file. Sent before
    /// the [`DiffOutcome`], so a producer that sees `Accept` finds it waiting.
    ReturnText(oneshot::Sender<String>),
}

/// A request to open an interactive side-by-side diff and block (on the
/// producer side) until the user Keeps or Rejects it.
///
/// `baseline` fills the read-only left side: `old_file_path`'s on-disk content,
/// or text the producer supplies. `new_contents` is the proposed text (the
/// editable right side). `on_accept` decides whether an Accept saves it to
/// `new_file_path` or hands it back. `response` resolves with the user's
/// [`DiffOutcome`] when the diff is torn down.
#[derive(Debug)]
pub struct ProgrammaticDiffRequest {
    /// Baseline file path: read for the left side under [`DiffBaseline::Disk`];
    /// otherwise the language and label only.
    pub old_file_path: PathBuf,
    /// Where the left side's text comes from.
    pub baseline: DiffBaseline,
    /// What an Accept does with the right side.
    pub on_accept: OnAccept,
    /// The path the proposed content carries (the right side's buffer path); an
    /// Accept writes the right side here. Usually equals `old_file_path`.
    pub new_file_path: PathBuf,
    /// The proposed text — the editable right side.
    pub new_contents: String,
    /// Display label for the diff (the agent's tab name). Presentation only —
    /// the teardown is keyed on [`origin_session`](Self::origin_session), NOT
    /// this label (a diff may show as a tab today, a window/split tomorrow).
    pub tab_name: String,
    /// D-fix.6: the originating session — the IDE-peer connection id that
    /// produced this diff. The host stores it with the opened diff so a
    /// session-scoped close (a `close_tab` / `closeAllDiffTabs` from THAT
    /// connection) tears down only the diffs that connection opened, never
    /// another session's. `0` means "no originating session" (a non-IDE
    /// producer — an LSP `WorkspaceEdit` preview, a plugin); such diffs are
    /// matched by no connection's close.
    pub origin_session: u64,
    /// Resolved with the user's verdict when the diff is torn down. A dropped
    /// receiver (the producer gave up) makes the teardown's `send` a no-op; a
    /// dropped sender (the session was cancelled without an explicit outcome)
    /// surfaces to the producer's `await` as a recv error it maps to a reject.
    pub response: oneshot::Sender<DiffOutcome>,
}

/// The host-drained inbound bus carrying [`ProgrammaticDiffRequest`]s.
///
/// A clone is registered as a boot service so a producer subsystem (the IDE
/// peer) reads it via `boot.service::<ProgrammaticDiffBus>()` and `send`s
/// requests; the wake is baked into [`InboundBus::send`] (paramount #4 — the
/// editor learns of the request off-keystroke without a keypress). The host
/// holds the matching receiver on the `Editor` and drains it per tick.
pub type ProgrammaticDiffBus = InboundBus<ProgrammaticDiffRequest>;
