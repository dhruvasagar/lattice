//! Wire the dashboard subsystem into the editor at boot (DB.2, DB.5). Called
//! from the host's Phase-B install list (`editor_boot.rs`) — the *only* host
//! touch-point the mode-ownership acid test allows.
//!
//! `install` registers four things, all owned by this crate:
//! - `dashboard-mode` (the major mode) into the mode registry;
//! - the `:dashboard` ex-command, whose `apply` returns the lifecycle
//!   `Effect::OpenDashboard` the host applies (buffer creation mutates
//!   `&mut Editor`, so the *applier* is the sanctioned host boundary — same
//!   shape as `:messages`/`:diff`);
//! - the built-in `DashboardRegistry` as a service, so the host's
//!   `do_open_dashboard` can compose the page;
//! - (DB.5) the mode-owned startup trigger: a one-shot subscription to
//!   `lattice_mode::Startup` that emits the *same* `Effect::OpenDashboard`
//!   when the editor booted with no file argument and `dashboard.enabled`.

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::{
    Args, CommandError, CommandRegistry, Effect, ExCommandSpec, GrammarResult, LatencyClass,
    SurfaceForm,
};
use lattice_mode::{Startup, SubsystemBoot};

use crate::mode::register_dashboard_modes;
use crate::options::DashboardEnabled;
use crate::sections::builtin_registry;

/// DB.5: the mode-internal signal `install_startup_trigger`'s spawned task
/// sends through the generic `inbound` primitive once it decides the
/// dashboard should auto-open. Carries no payload — the handler always maps
/// it to `Effect::OpenDashboard`, mirroring `:dashboard`'s own applier.
struct DashboardStartupTrigger;

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
    install_startup_trigger(boot);
}

/// DB.5 (design.md §9.1): subscribe to `Startup` and, if the editor booted
/// with no file argument and `dashboard.enabled`, emit the same
/// `Effect::OpenDashboard` the `:dashboard` command emits — one applier, two
/// triggers. The activation-decision (no file + enabled) lives here, not in
/// the host.
///
/// The subscription is registered SYNCHRONOUSLY (`event_bus().subscribe_typed`
/// runs before `runtime.spawn`, not inside the spawned future) — `Startup`
/// publishes almost immediately after this `install` call returns (the
/// renderer's post-boot seam runs right after `Editor::boot`), so a
/// subscription registered lazily on the task's first poll could lose the
/// race and never see it. Mirrors `lattice_lsp::modeline::spawn_modeline_forwarder`'s
/// shape (`bus.subscribe_typed(tx)` before `runtime.spawn`).
///
/// Reads `dashboard.enabled` via `boot.service::<Arc<ConfigRegistry>>()` —
/// available because the host now registers it as a Phase-A service (the
/// DB.5 `ConfigRegistry` hoist in `editor_boot.rs`), before this Phase-B
/// `install` runs.
fn install_startup_trigger(boot: &mut impl SubsystemBoot) {
    let config = boot.service::<Arc<ConfigRegistry>>();
    let trigger_bus = boot.inbound(|_: DashboardStartupTrigger| vec![Effect::OpenDashboard]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Startup>();
    boot.event_bus().subscribe_typed(tx);
    boot.runtime_handle().spawn(async move {
        let Some(Startup { opened_file }) = rx.recv().await else {
            // Bus dropped before boot published `Startup` — unreachable in
            // practice (the bus outlives the editor), but a closed channel
            // ends the task cleanly rather than panicking.
            return;
        };
        let enabled = config
            .and_then(|c| c.get_typed::<DashboardEnabled>())
            .map(|v| *v)
            .unwrap_or(true);
        if opened_file.is_none() && enabled {
            let _ = trigger_bus.send(DashboardStartupTrigger);
        }
    });
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
