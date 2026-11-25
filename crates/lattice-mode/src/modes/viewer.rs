//! `viewer-mode` — what every buffer that PRESENTS a file shares (PF.4).
//!
//! `image-mode` and `pdf-mode` show a file rather than edit it, and a viewer
//! has keys of its own that are not about the file type: `q` closes it, `R`
//! reads it again and `gx` hands it to the system's own viewer.
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

/// The action `R` runs in a viewer.
pub const VIEWER_RELOAD_ACTION: &str = "action:viewer-reload";

/// The action `gx` runs in a viewer.
pub const VIEWER_OPEN_EXTERNALLY_ACTION: &str = "action:viewer-open-externally";

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
    /// worth replaying. `R` reloads it, as zathura's does; vim's `R` (Replace
    /// mode) has nothing to replace here. `gx` opens the file in the system
    /// viewer, as vim's `gx` opens the path under the cursor: here the file
    /// is what is under the cursor. It is also the TUI's way to see a page.
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
        vec![
            keymap_entry! {
                mode: Normal, chord: "q",
                doc: "viewer: close the file's buffer, as :bd",
                cmd: "action:viewer-close"
            },
            keymap_entry! {
                mode: Normal, chord: "R",
                doc: "viewer: read the file again, as :e!",
                cmd: "action:viewer-reload"
            },
            keymap_entry! {
                mode: Normal, chord: "gx",
                doc: "viewer: open the file in the system's viewer",
                cmd: "action:viewer-open-externally"
            },
        ]
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
    // `:e!`: the same reload autoread does on its own when the file changes,
    // for a watcher that missed it (a network mount) or a user who wants to
    // be sure. Forced, though a viewer is never dirty.
    registry.register_action(
        VIEWER_RELOAD_ACTION,
        "viewer: read the file again, as `:e!` does (mode-owned).",
        ActionSpec {
            apply: Arc::new(|_| {
                Ok(Effect::OpenBuffer {
                    path: None,
                    force: true,
                })
            }),
            args_schema: vec![],
        },
    );
    registry.register_action(
        VIEWER_OPEN_EXTERNALLY_ACTION,
        "viewer: open the file in the system's viewer (mode-owned).",
        ActionSpec {
            apply: Arc::new(|ctx| Ok(open_externally(ctx.path.as_deref().map(|p| p.as_path())))),
            args_schema: vec![],
        },
    );
}

/// `gx`'s effect for a viewer backed by `path`: the OS handler (`xdg-open`,
/// `open`, `explorer`), which takes a plain path as readily as a URI. A
/// viewer always has a file, but an action can fire anywhere its key is
/// bound, so the case without one says so instead of doing nothing.
fn open_externally(path: Option<&std::path::Path>) -> Effect {
    match path {
        Some(path) => Effect::OpenExternalUri {
            uri: path.to_string_lossy().into_owned(),
        },
        None => Effect::Echo {
            level: lattice_grammar::EchoLevel::Warn,
            text: "gx: this buffer has no file to open".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn q_closes_the_view_and_r_reloads_it() {
        let km = ViewerMode.keymap();
        let bound: Vec<_> = km.entries.iter().map(|e| e.command).collect();
        assert_eq!(
            bound,
            [
                Some(VIEWER_CLOSE_ACTION),
                Some(VIEWER_RELOAD_ACTION),
                Some(VIEWER_OPEN_EXTERNALLY_ACTION)
            ]
        );
    }

    #[test]
    fn gx_hands_the_file_to_the_system_viewer() {
        let path = std::path::Path::new("/papers/a b.pdf");
        assert!(matches!(
            open_externally(Some(path)),
            Effect::OpenExternalUri { uri } if uri == "/papers/a b.pdf"
        ));
        assert!(matches!(open_externally(None), Effect::Echo { .. }));
    }
}
