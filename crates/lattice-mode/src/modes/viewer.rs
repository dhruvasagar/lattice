//! `viewer-mode` — what every buffer that PRESENTS a file shares (PF.4).
//!
//! `image-mode` and `pdf-mode` show a file rather than edit it, and a viewer
//! has keys of its own that are not about the file type: `q` closes it.
//! Those keys live here, on one minor both majors imply, rather than
//! declared again on each major — the shared-behaviour rule
//! (`prefer-minor-modes-over-duplication`), whose failure mode is a copied
//! key set that quietly drifts. `pdf-mode`'s page keys stay on `pdf-mode`:
//! a picture has no pages.
//!
//! Design: `docs/dev/architecture/pdf-mode.md` §2.

use std::sync::{Arc, OnceLock};

use lattice_grammar::Effect;
use lattice_grammar::registry::ActionSpec;

use crate::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    keymap_entry,
};

/// The action `q` runs in a viewer.
pub const VIEWER_CLOSE_ACTION: &str = "action:viewer-close";

/// Minor mode carrying the keys every file viewer shares. Activated through
/// [`Mode::implies`] by the majors that present a file.
pub struct ViewerMode;

impl ViewerMode {
    /// The canonical id, `"viewer-mode"`.
    pub fn mode_id() -> ModeId {
        ModeId::new("viewer-mode")
    }
}

impl Mode for ViewerMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// `q` closes the view, as in zathura, feh and magit. In a buffer that
    /// cannot be edited, vim's `q` (start recording a macro) records nothing
    /// worth replaying.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(viewer_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn viewer_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry! {
            mode: Normal, chord: "q",
            doc: "viewer: close the file's buffer, as :bd",
            cmd: "action:viewer-close"
        }]
    })
}

/// Register the actions [`ViewerMode`]'s keys run. The body is here, with the
/// mode; the host only applies the effect.
pub fn register_viewer_mode_actions(registry: &mut lattice_grammar::CommandRegistry) {
    // `KillBuffer`: the buffer is closed, as `:bd` closes it, and the pane
    // shows what it showed before. Not magit's `BuryBuffer`, which returns
    // only from an in-pane view: a PDF opened with `:e` is an ordinary file
    // buffer, and burying it is a no-op. Never dirty, so the `:bd!` that
    // `KillBuffer` amounts to loses nothing.
    registry.register_action(
        VIEWER_CLOSE_ACTION,
        "viewer: close the file's buffer, as `:bd` does (mode-owned).",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::KillBuffer)),
            args_schema: vec![],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_closes_the_view() {
        let km = ViewerMode.keymap();
        assert_eq!(km.entries.len(), 1);
        assert_eq!(km.entries[0].command, Some(VIEWER_CLOSE_ACTION));
    }
}
