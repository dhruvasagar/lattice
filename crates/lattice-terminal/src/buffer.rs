use std::{path::PathBuf, sync::Arc};

use arc_swap::ArcSwap;
use lattice_core::BufferId;

use crate::{PtyHandle, TerminalSnapshot};

#[derive(Debug)]
pub struct TerminalBuffer {
    pub id: BufferId,
    pub pty: Arc<PtyHandle>,
    pub cwd: Option<PathBuf>,
    pub label: String,
    pub snapshot: Arc<ArcSwap<TerminalSnapshot>>,
    pub created_at: std::time::SystemTime,
}

pub struct ScrollbackView {
    // TODO: add lifetime if exposing references
    pub total_rows: u32,
    /// 0 = bottom (live); N = N rows up
    pub viewport_row: u32,
}

impl TerminalBuffer {
    pub fn scrollback_view(&self) -> ScrollbackView {
        // T1 stub: scrollback not yet implemented
        ScrollbackView {
            total_rows: 0,
            viewport_row: 0,
        }
    }
}
