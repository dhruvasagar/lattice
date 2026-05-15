//! Popup placement model -- re-exported from
//! `lattice-core::ui::popup`.
//!
//! Moved from `lattice-ui-tui` in Phase 5.2 first wave.

pub use lattice_core::ui::popup::PopupPlacement;
pub use lattice_help::HelpMetadata;

/// Renderer-agnostic snapshot of popup content + position + metadata.
#[derive(Debug, Clone)]
pub struct PopupSnapshot {
    pub title: String,
    pub content: lattice_core::Buffer,
    pub cursor: lattice_protocol::position::Position,
    pub scroll: u32,
    pub metadata: HelpMetadata,
    pub placement: PopupPlacement,
}
