//! The generic boot-completion signal (design.md §9.1) (DB.5).
//!
//! Published once per boot, after `Editor::boot` returns, by each renderer's
//! post-boot seam (TUI `App::new`, GPUI `GpuiApp::new`) — carrying the file
//! (if any) passed on the command line. Declared here, alongside the
//! existing [`crate::ModeEvent`] typed-event precedent, rather than as a
//! per-renderer ad hoc signal, so any subsystem's `install(&mut boot)` can
//! subscribe via the generic typed-event bus without a `lattice-host`
//! dependency. `lattice-dashboard`'s startup-trigger subscription is the
//! first consumer (`lattice_dashboard::install`).

use std::path::PathBuf;

/// Fired once per boot after `Editor::boot` returns. `opened_file` is the
/// path (if any) the renderer's post-boot seam captured from the boot
/// `Document` *before* it moved into `Editor::boot` (which consumes it).
#[derive(Debug, Clone)]
///
/// # Examples
///
/// ```
/// use lattice_mode::Startup;
/// use lattice_runtime::EventBus;
///
/// let bus = EventBus::new();
/// let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
/// bus.subscribe_typed::<Startup>(tx); // e.g. from a subsystem's `install`
/// bus.publish_typed(Startup { opened_file: None }); // the renderer, after boot
/// assert!(rx.try_recv().unwrap().opened_file.is_none()); // no file: show a dashboard
/// ```
pub struct Startup {
    /// The file named on the command line, or `None` when the editor
    /// started without one.
    pub opened_file: Option<PathBuf>,
}

// DB.5: register as a typed event so `EventBus::publish_typed` /
// `subscribe_typed` can carry it -- same mechanism `ModeEvent` uses
// (`event.rs`).
lattice_protocol::register_event!(
    Startup,
    "editor.startup",
    "Published once per boot, after Editor::boot returns, carrying the file \
     (if any) opened on the command line.",
    "lattice-mode",
);
