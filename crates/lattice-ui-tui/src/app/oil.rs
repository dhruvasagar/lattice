//! Oil-buffer App surface -- thin delegates over the host's
//! oil methods. Phase 5.8.AD.1 migrated every body to
//! `lattice_host::dispatch::Editor::do_open_oil` etc., so this
//! file is just the renderer-coupled fan-out for the few sites
//! that need to hop through `handle_renderer_signal`.

use std::path::{Path, PathBuf};

use super::App;

impl App {
    /// Delegate to [`lattice_host::dispatch::Editor::set_oil_dir`].
    pub(super) fn set_oil_dir(&mut self, buffer_id: crate::buffers::BufferId, dir: PathBuf) {
        // Slice 3c.final.E.2: mutating call routed through
        // `mutate_editor` so the swap to actor-owned Editor is a
        // localized change in `App::mutate_editor`'s body.
        self.mutate_editor(move |e| e.set_oil_dir(buffer_id, dir));
    }

    /// Delegate to [`lattice_host::dispatch::Editor::oil_dir_for`].
    pub(super) fn oil_dir_for(&self, buffer_id: crate::buffers::BufferId) -> Option<PathBuf> {
        self.read_editor(move |e| e.oil_dir_for(buffer_id))
    }

    /// Delegate to [`lattice_host::dispatch::Editor::oil_with_dir`].
    pub(super) fn oil_with_dir(&self, dir: &Path) -> Option<crate::buffers::BufferId> {
        self.editor.oil_with_dir(dir)
    }

    /// `:Oil [dir]`. Phase 5.8.AD.1: body migrated to
    /// [`lattice_host::dispatch::Editor::do_open_oil`]. The
    /// wrapper fans host-returned signals through
    /// `handle_renderer_signal` so mode-activate cascades reach
    /// the renderer.
    pub(super) fn do_open_oil(&mut self, dir: Option<PathBuf>) {
        // Slice 3c.final.E.2: routed through `mutate_editor_with`
        // so the closure's signal return value comes back across
        // the actor boundary post-swap.
        let signals = self.mutate_editor_with(move |e| e.do_open_oil(dir));
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    pub(super) fn do_oil_follow(&mut self) {
        let signals = self.mutate_editor_with(move |e| e.do_oil_follow());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }

    pub(super) fn do_oil_navigate_up(&mut self) {
        let signals = self.mutate_editor_with(move |e| e.do_oil_navigate_up());
        for s in signals {
            self.handle_renderer_signal(s);
        }
    }
}
