//! Popup placement model -- re-exported from
//! `lattice-core::ui::popup`.
//!
//! Moved from `lattice-ui-tui` in Phase 5.2 first wave.

pub use lattice_core::ui::popup::{PopupFocus, PopupPlacement};
// PU-A.2: `PopupSnapshot` + `HelpMetadata` are help's `<C-o>` back-stack
// history; they live in `lattice-help`. Re-exported here so existing
// `crate::popup::PopupSnapshot` references stay valid.
pub use lattice_help::{HelpMetadata, PopupSnapshot};
