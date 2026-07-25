//! Magit — git porcelain as a core plugin.
//!
//! Feature-buffer crate inverted out of `lattice-host`. Owns every
//! magit buffer view's mode, keymap, action handler, and synthetic-
//! buffer provisioning. Installs through the `SubsystemBoot` seam —
//! one line in `editor_boot.rs`, zero `Editor::do_magit_*` methods.
//!
//! See [`docs/dev/architecture/magit.md`] and
//! [`docs/dev/operations/slice-plans/magit.md`].

pub mod magit_core_mode;
pub mod magit_status_mode;

use std::sync::Arc;

use lattice_grammar::{
    Args, ExCommandSpec, LatencyClass, SurfaceForm,
    registry::CommandRegistry,
};
use lattice_grammar::Effect;
use lattice_mode::SubsystemBoot;

use magit_core_mode::MagitCoreMode;
use magit_status_mode::MagitStatusMode;

/// Register all magit modes, commands, and keymaps via the generic
/// `SubsystemBoot` seam. Called once from `editor_boot.rs` during
/// the Phase-B subsystem install pass.
pub fn install(boot: &mut impl SubsystemBoot) {
    // ── Modes ──────────────────────────────────────────────

    boot.modes_mut()
        .register(MagitCoreMode)
        .expect("magit-core-mode registers without conflict");

    boot.modes_mut()
        .register(MagitStatusMode)
        .expect("magit-status-mode registers without conflict");

    // ── Ex-commands ────────────────────────────────────────

    register_ex_commands(boot.commands_mut());
}

/// Register all magit ex-commands in the command registry.
fn register_ex_commands(registry: &mut CommandRegistry) {
    // :magit-status — open the status buffer
    registry.register_ex_command(
        "magit-status",
        "Open the Magit status buffer for the current git repository.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
            apply: Arc::new(|_ctx| {
                Ok(Effect::OpenSyntheticBuffer {
                    name: "*magit:status*".to_string(),
                    mode_id: "magit-status-mode".to_string(),
                })
            }),
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
}
