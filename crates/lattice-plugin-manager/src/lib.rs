//! PL8.H.2 — the buffer-backed `:plugins` manager view.
//!
//! A pure PROVIDER crate (the `oil` / `lattice-dashboard` shape): it owns
//! `plugins-mode` (major, read-only) and the `:plugins` ex-command, and reads the
//! loader's `plugin_status()` (PL8.H.1) via the `PluginLoaderHandle` service at
//! activation. Everything wires through the generic [`SubsystemBoot`] seam, so
//! adding it to the editor is **one line** in the host's Phase-B install list and
//! **zero** host internals — no `Editor::` method, no host `Action` variant (the
//! mode-ownership acid test).
//!
//! Open flow, all generic primitives (no provider-specific host code):
//!   1. `:plugins` (registered here) returns
//!      [`Effect::OpenSyntheticBuffer`](lattice_grammar::Effect::OpenSyntheticBuffer)
//!      `{ name: "*plugins*", mode_id: "plugins-mode" }`.
//!   2. The host generically ensures that buffer under `plugins-mode` + activates
//!      it (`Editor::open_synthetic_buffer`).
//!   3. `PluginManagerMode::on_activate` projects the status table into the
//!      buffer and subscribes to `PluginCrashed` for live health.
//!
//! Interactivity (reload / unload / describe chords) is PL8.H.3.

use std::sync::Arc;

use lattice_grammar::{Args, Effect, ExCommandSpec, LatencyClass, SurfaceForm};
use lattice_mode::SubsystemBoot;

mod actions;
mod mode;
mod render;

pub use mode::PluginManagerMode;
pub use render::{
    PLUGINS_BUFFER_NAME, PLUGINS_MODE_ID, render_status, render_status_with_failures,
};

/// Install the plugin-manager view: register `plugins-mode` + the `:plugins`
/// ex-command. Seated in the host's Phase-B install list (before the registry
/// freeze), so it registers directly via `boot.modes_mut()` / `boot.commands_mut()`.
///
/// No dependency on the loader being installed first: the mode resolves
/// `PluginLoaderHandle` at *activation* (by which point boot is complete), and
/// the ex-command returns a static effect. A missing loader service at activation
/// degrades to an empty buffer, never a panic.
pub fn install(boot: &mut impl SubsystemBoot) {
    boot.modes_mut()
        .register(PluginManagerMode)
        .expect("plugins-mode registers without conflict");

    // PL8.H.3: the in-view `action:plugins-*` commands (dead-body) so the mode's
    // keymap `cmd:` names resolve; the mode's `action_handlers` do the work.
    actions::register_actions(boot.commands_mut());

    boot.commands_mut().register_ex_command(
        "plugins",
        "Open the plugin manager (`:plugins`) — a buffer listing every loaded \
         plugin with its health (quarantined after a crash), trust tier, and \
         capabilities granted/denied. Read-only; reload / unload act on plugins \
         via `:plugin-reload` / `:plugin-unload` (in-view chords land in a \
         follow-up).",
        plugins_spec(),
    );
}

/// The `:plugins` ex-command: takes no argument, returns the generic
/// open-synthetic-buffer effect. `Reflex` — it does no blocking work on the
/// dispatch path (the host runs the open; the mode projects content off-thread).
fn plugins_spec() -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
        apply: Arc::new(|_ctx| {
            Ok(Effect::OpenSyntheticBuffer {
                name: PLUGINS_BUFFER_NAME.to_string(),
                mode_id: PLUGINS_MODE_ID.to_string(),
            })
        }),
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    }
}
