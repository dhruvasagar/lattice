//! Wire the dashboard subsystem into the editor at boot (DB.2). Called from
//! the host's Phase-B install list (`editor_boot.rs`) — the *only* host
//! touch-point the mode-ownership acid test allows.
//!
//! `install` registers three things, all owned by this crate:
//! - `dashboard-mode` (the major mode) into the mode registry;
//! - the `:dashboard` ex-command, whose `apply` returns the lifecycle
//!   `Effect::OpenDashboard` the host applies (buffer creation mutates
//!   `&mut Editor`, so the *applier* is the sanctioned host boundary — same
//!   shape as `:messages`/`:diff`);
//! - the built-in `DashboardRegistry` as a service, so the host's
//!   `do_open_dashboard` can compose the page.

use lattice_grammar::{
    Args, CommandError, CommandRegistry, Effect, ExCommandSpec, GrammarResult, LatencyClass,
    SurfaceForm,
};
use lattice_mode::SubsystemBoot;

use crate::mode::register_dashboard_modes;
use crate::sections::builtin_registry;

/// Reject any trailing characters after `:dashboard` (it takes no args).
fn parse_no_args(rest: &str, _bang: bool) -> GrammarResult<Args> {
    if rest.trim().is_empty() {
        Ok(Args::None)
    } else {
        Err(CommandError::BadArgs(
            "trailing characters after :dashboard".into(),
        ))
    }
}

/// Install the dashboard subsystem: mode + `:dashboard` command + the
/// built-in section registry service.
pub fn install(boot: &mut impl SubsystemBoot) {
    register_dashboard_modes(boot.modes_mut());
    register_dashboard_commands(boot.commands_mut());
    // The built-in section registry, read by `do_open_dashboard` at compose
    // time. Registered (and looked up) as the bare `DashboardRegistry` type
    // per the ServiceRegistry Arc/TypeId rule.
    boot.register_service(builtin_registry());
}

fn register_dashboard_commands(registry: &mut CommandRegistry) {
    registry.register_ex_command(
        "dashboard",
        "Open the *dashboard* launch page (`:dashboard`).",
        ExCommandSpec {
            latency_class: LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_no_args),
            apply: Box::new(|_| Ok(Effect::OpenDashboard)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
