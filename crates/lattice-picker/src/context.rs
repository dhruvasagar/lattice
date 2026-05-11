//! Host-built snapshot the picker primitive hands to source
//! generators on every `:picker <source>` open.
//!
//! `PickerContext` is the *runtime-varying* "where am I"
//! state every picker source needs: the active buffer, the
//! workspace root, the recent-files MRU, marks, registers,
//! and the position-history ring. It does NOT carry feature-
//! specific facades (LSP supervisor handle, snippet registry,
//! grammar registry) -- those are captured by source
//! generators at construction time. See
//! `docs/dev/architecture/picker.md` for why.
//!
//! The struct is borrow-heavy by design: sync sources read
//! straight through the borrows, async sources clone what
//! they need into their future's captures before the
//! synchronous `init` call returns. Three fields are owned
//! vecs (`buffers`, `marks`, `registers`) -- the App rebuilds
//! these fresh on each picker-open because their backing
//! state lives in non-borrow-friendly types (HashMaps with
//! transient liveness). Allocation cost is trivial at our
//! sizes (<100 buffers, <26 marks, <40 registers).

use std::path::{Path, PathBuf};

use lattice_core::Buffer;
use lattice_protocol::Position;

/// Snapshot the host passes to `PickerSourceGenerator::init`
/// (and `accept`) on each picker invocation.
///
/// Sources access the borrowed fields directly during the
/// synchronous prelude; the moment `init` returns, the
/// borrow is released and any captured async work owns its
/// own clones.
pub struct PickerContext<'a> {
    pub active_buffer: ActiveBufferSnapshot<'a>,
    pub workspace_root: &'a Path,
    pub recent_files: &'a [PathBuf],
    pub position_history: &'a [PositionEntry],
    pub buffers: Vec<BufferEntry>,
    pub marks: Vec<(char, Position)>,
    /// Registers as `(name, preview)` pairs. The preview is the
    /// register's stored content truncated for display -- the
    /// real content lives App-side and the host pastes via the
    /// `PasteRegister` outcome.
    pub registers: Vec<(String, String)>,
}

/// Snapshot of the active document buffer at the moment the
/// picker opened. Carries enough state for line / mark /
/// outline / grep / LSP-position sources to do their work
/// without a second App round-trip.
pub struct ActiveBufferSnapshot<'a> {
    pub buffer_id: u32,
    pub path: Option<&'a Path>,
    /// Language id (`"rust"`, `"markdown"`, ...). Snippet and
    /// outline sources filter on this; absent for unknown /
    /// untyped buffers.
    pub language: Option<&'a str>,
    pub cursor: Position,
    /// Visual-mode selection extent at picker-open, if any.
    /// `:picker grep` defaults its pattern to the selected
    /// text when present.
    pub selection: Option<(Position, Position)>,
    /// Read-only borrow of the rope. Line / outline / grep
    /// sources walk this directly; long-running async work
    /// must extract what it needs before `init` returns.
    pub buffer: &'a Buffer,
}

/// One buffer in the registry, projected for picker rows.
/// `kind_label` is a display string -- the picker primitive
/// stays oblivious to the actual `BufferKind` enum so the
/// "no kind-specific logic" rule (CLAUDE.md memory) holds at
/// this seam too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferEntry {
    pub id: u32,
    pub kind_label: String,
    pub path: Option<PathBuf>,
    pub title: String,
    pub dirty: bool,
}

/// Picker-friendly view of one entry in the App's
/// position-history ring (§5.1.1 unified jump list + mark
/// ring). The App's richer `PositionEntry` carries
/// `BufferKind` + framework-internal fields; the picker only
/// needs `(buffer_id, line, col, source)` to render a row
/// and emit a jump outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionEntry {
    pub buffer_id: u32,
    pub line: u32,
    pub col: u32,
    pub source: PositionSource,
}

/// Why this entry was pushed onto the position history.
/// Mirror of `lattice_ui_tui::app::PositionSource` but kept
/// here so `lattice-picker` doesn't depend on the host
/// crate. The App translates between the two at
/// PickerContext-build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionSource {
    /// Big motions: `gg`, `G`, search, `*`, `#`, `%`, mark jump.
    AutoJump,
    /// Explicit user "remember here" push (reserved for
    /// `g<C-o>` style emacs-`set-mark` equivalents).
    ExplicitMark,
    /// LSP / fuzzy-finder / plugin-pushed jumps.
    PluginPush,
    /// Named mark (`mX`). Walks via `g;` / `g,`.
    NamedMark(char),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_entry_clone_and_eq() {
        let a = BufferEntry {
            id: 7,
            kind_label: "doc".into(),
            path: Some("/tmp/foo.rs".into()),
            title: "foo.rs".into(),
            dirty: false,
        };
        let b = a.clone();
        assert_eq!(a, b);
    }

    #[test]
    fn position_entry_carries_source_variant() {
        let e = PositionEntry {
            buffer_id: 3,
            line: 12,
            col: 4,
            source: PositionSource::NamedMark('a'),
        };
        assert_eq!(
            e.source,
            PositionSource::NamedMark('a'),
            "NamedMark name survives copy"
        );
        assert_ne!(e.source, PositionSource::AutoJump);
    }
}
