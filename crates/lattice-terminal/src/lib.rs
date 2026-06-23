//! lattice-terminal: PTY-backed terminal-buffer substrate.
//!
//! Issue #40 / Terminal-mode T1 (2026-05-22). See
//! `docs/dev/architecture/terminal-mode.md` for the design
//! and `docs/dev/operations/slice-plans/terminal-mode.md` for the
//! slice breakdown.
//!
//! # Surface (T1)
//!
//! - [`PtyHandle`]: writer + resize + kill for the master pty
//!   side. Cheap to clone (Arc-backed).
//! - [`TerminalSnapshot`]: renderer-facing immutable view of
//!   the cell grid + cursor + title. Read via
//!   `ArcSwap::load()` on the hot paint path.
//! - [`spawn`]: spawns a child process under a fresh PTY,
//!   returns the writer handle + the published snapshot cell.
//!   The reader task is spawned automatically and runs until
//!   the child exits or the handle is dropped.
//!
//! Input encoding (`<C-c>` → `\x03` etc.) is wired in T2.
//! Scrollback navigation + Visual yank land in T3.

pub mod buffer;
pub mod cell;
pub mod handle;
// BC.4: the crate-owned `install(boot)` entry point — one Phase-B line in
// `editor_boot`.
pub mod install;
pub mod modes;
pub mod reader;
pub mod snapshot;
pub mod spawner;
pub mod synthetic;

pub use buffer::{TerminalBuffer, TerminalVisualState, VisualKind};
pub use cell::{Cell, CellAttrs, CursorShape, NamedColor, TerminalColor};
pub use handle::{PtyHandle, PtyHandleError};
pub use install::install;
pub use modes::{
    register_terminal_modes, TerminalInsertMode, TerminalMode, TerminalNormalMode,
    TerminalNormalModeGuard,
};
pub use reader::{GridSearchHit, SearchDir, SharedTerm, TerminalScrollKind};
pub use snapshot::TerminalSnapshot;
pub use spawner::{spawn, SpawnConfig, SpawnError};
pub use synthetic::{SyntheticDoc, TerminalStore, TerminalStoreHandle};
