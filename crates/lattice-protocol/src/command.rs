//! Commands sent from clients (UI, plugins) to the core dispatcher.
//!
//! Phase 0 only needs the document-management and editing variants. The full
//! v0.4 catalog (modal mode, mode contributions, grammar registration, UI
//! contributions, picker / popup / notification, subscription) lands as
//! subsequent phases require it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::edit::Edit;
use crate::ids::DocumentId;
use crate::selection::SelectionSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    // ---- Document management ----
    OpenDocument {
        path: PathBuf,
    },
    CloseDocument {
        id: DocumentId,
    },
    SaveDocument {
        id: DocumentId,
    },
    SaveDocumentAs {
        id: DocumentId,
        path: PathBuf,
    },

    // ---- Editing ----
    ApplyEdit {
        id: DocumentId,
        edit: Edit,
    },
    ApplyEditBatch {
        id: DocumentId,
        edits: Vec<Edit>,
    },
    Undo {
        id: DocumentId,
    },
    Redo {
        id: DocumentId,
    },

    // ---- Selection ----
    SetSelections {
        id: DocumentId,
        selections: SelectionSet,
    },
}
