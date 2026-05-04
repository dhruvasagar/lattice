//! Events published by the core to subscribed clients.
//!
//! Per DESIGN.md §5.10: every meaningful editor state transition
//! publishes a typed event. Vim's `autocmd` and emacs's hooks both
//! desugar to the same `EventBus::subscribe` call (filter +
//! sink). The `Event` enum is the catalog; `EventKind` is the
//! discriminator used by filter dispatch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::DocumentId;
use crate::position::Range;
use crate::selection::SelectionSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    DocumentOpened {
        id: DocumentId,
        path: Option<PathBuf>,
        version: u64,
    },
    DocumentClosed {
        id: DocumentId,
    },
    /// Fired before [`Self::DocumentSaved`]. Observation-only in
    /// v1; future revisions may carry a payload that handlers can
    /// mutate (formatters rewriting buffer content) or veto
    /// (return Err to abort the save).
    BeforeSave {
        id: DocumentId,
        path: PathBuf,
    },
    DocumentSaved {
        id: DocumentId,
        path: PathBuf,
    },
    DocumentChanged {
        id: DocumentId,
        version: u64,
        edits: Vec<AppliedEdit>,
    },
    SelectionsChanged {
        id: DocumentId,
        version: u64,
        selections: SelectionSet,
    },
    /// Fired when the modal state transitions
    /// (Normal -> Insert, Insert -> Normal, ...). Carries the
    /// previous and next state as opaque labels; the App owns the
    /// `ModalState` type so the protocol layer keeps it as String.
    ModalModeChanged {
        from: String,
        to: String,
    },
    /// Fired before the editor exits. Observation-only in v1; the
    /// veto path (a handler returning Err to abort quit) layers on
    /// once the bus grows the Before-event mutation semantics.
    BeforeQuit,
    /// Fired after a typed-options registry value changes
    /// (DESIGN.md §5.12). Carries the option's canonical name plus
    /// the formatted old / new value strings -- string-formatted
    /// (rather than `Box<dyn Any>`) because most subscribers just
    /// react to the change signal and don't need the typed value.
    /// Subscribers that need the typed value re-read through the
    /// registry (`config.with(handle, |v| ...)`).
    ///
    /// `old` is `None` for the very first publish after registration
    /// (when the option is initialised to its default and no prior
    /// value exists); subsequent edits always carry both sides.
    OptionChanged {
        name: String,
        old: Option<String>,
        new: String,
    },
    /// Fired when [`lattice_lsp::LspLogger::log`] appends a record
    /// to a log ring (subsystem-wide when `server_id` is `None`,
    /// per-server otherwise). Subscribers (the App) refresh any
    /// open `*lsp*` / `*lsp:<server>*` / `*lsp:<server>:trace*`
    /// help buffers from the logger snapshot so log views update
    /// live as records arrive.
    ///
    /// Carries primitive fields rather than a typed `LogRecord`
    /// because `lattice-protocol` sits below `lattice-lsp` in the
    /// crate graph; subscribers that need the typed record can
    /// re-snapshot through the logger.
    LspLogPushed {
        /// `None` for subsystem-wide records; `Some(id)` per-server.
        server_id: Option<String>,
        /// Severity tag (`"trace"`, `"debug"`, `"info"`, `"warn"`,
        /// `"error"`) -- string-formatted to keep the protocol crate
        /// independent of `lattice-lsp::LogLevel`.
        level: String,
        /// Source tag (`"client"`, `"stderr"`, `"log"`, `"show"`,
        /// `"trace"`).
        source: String,
        /// The record's message text.
        message: String,
    },
}

impl Event {
    /// Project the event to its [`EventKind`] discriminator. Used
    /// by [`crate::cancel`]-style filter dispatch in the runtime
    /// layer's event bus to avoid string-matching variant names.
    pub fn kind(&self) -> EventKind {
        match self {
            Event::DocumentOpened { .. } => EventKind::DocumentOpened,
            Event::DocumentClosed { .. } => EventKind::DocumentClosed,
            Event::BeforeSave { .. } => EventKind::BeforeSave,
            Event::DocumentSaved { .. } => EventKind::DocumentSaved,
            Event::DocumentChanged { .. } => EventKind::DocumentChanged,
            Event::SelectionsChanged { .. } => EventKind::SelectionsChanged,
            Event::ModalModeChanged { .. } => EventKind::ModalModeChanged,
            Event::BeforeQuit => EventKind::BeforeQuit,
            Event::OptionChanged { .. } => EventKind::OptionChanged,
            Event::LspLogPushed { .. } => EventKind::LspLogPushed,
        }
    }
}

/// Discriminator for [`Event`] variants. Stored in
/// `EventFilter::kinds` and used by the bus to bucket
/// subscriptions per kind so publish does no global iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    DocumentOpened,
    DocumentClosed,
    BeforeSave,
    DocumentSaved,
    DocumentChanged,
    SelectionsChanged,
    ModalModeChanged,
    BeforeQuit,
    OptionChanged,
    LspLogPushed,
}

/// An edit as actually applied to the buffer (the original `Edit` plus the
/// resulting range, useful for clients that want to know what changed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedEdit {
    pub original_range: Range,
    pub inserted_range: Range,
    pub replaced_text: String,
}
