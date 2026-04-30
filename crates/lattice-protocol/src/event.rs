//! Events published by the core to subscribed clients.
//!
//! Phase 0 only needs document lifecycle and edit-application events. Modal
//! mode, mode lifecycle, LSP, plugin, and UI events arrive in their own
//! phases.

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
}

/// An edit as actually applied to the buffer (the original `Edit` plus the
/// resulting range, useful for clients that want to know what changed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppliedEdit {
    pub original_range: Range,
    pub inserted_range: Range,
    pub replaced_text: String,
}
