//! Wire the dashboard subsystem into the editor at boot (DB.2, DB.5, DB.6).
//! Called from the host's Phase-B install list (`editor_boot.rs`) — the
//! *only* host touch-point the mode-ownership acid test allows.
//!
//! `install` registers five things, all owned by this crate:
//! - `dashboard-mode` (the major mode) into the mode registry;
//! - the `:dashboard` ex-command, whose `apply` returns the lifecycle
//!   `Effect::OpenDashboard` the host applies (buffer creation mutates
//!   `&mut Editor`, so the *applier* is the sanctioned host boundary — same
//!   shape as `:messages`/`:diff`);
//! - the built-in `DashboardRegistry` as a service, so the host's
//!   `do_open_dashboard` can compose the page;
//! - (DB.5) the mode-owned startup trigger: a one-shot subscription to
//!   `lattice_mode::Startup` that emits the *same* `Effect::OpenDashboard`
//!   when the editor booted with no file argument and `dashboard.enabled`;
//! - (DB.6) the mode-owned recompose trigger: a long-lived subscription to
//!   `Event::OptionChanged` that re-emits `Effect::OpenDashboard` — this
//!   time gated on the dashboard already being open — whenever
//!   `dashboard.sections`, `dashboard.source`, or `ui.nerd_fonts` changes.

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::{
    Args, CommandError, CommandRegistry, Effect, ExCommandSpec, GrammarResult, LatencyClass,
    SurfaceForm,
};
use lattice_mode::{BufferStoreHandle, Startup, SubsystemBoot};
use lattice_protocol::event::{Event, EventKind};
use lattice_runtime::{EventFilter, SubscriptionTarget};

use crate::mode::register_dashboard_modes;
use crate::options::DashboardEnabled;
use crate::sections::builtin_registry;

/// The `*dashboard*` synthetic buffer's registered name — the same literal
/// the host's `do_open_dashboard` / `BufferRegistry::by_name` use. Kept as
/// one constant here since both triggers below need it.
const DASHBOARD_BUFFER_NAME: &str = "*dashboard*";

/// DB.5: the mode-internal signal `install_startup_trigger`'s spawned task
/// sends through the generic `inbound` primitive once it decides the
/// dashboard should auto-open. Carries no payload — the handler always maps
/// it to `Effect::OpenDashboard`, mirroring `:dashboard`'s own applier.
struct DashboardStartupTrigger;

/// DB.6: the signal `install_recompose_triggers`'s spawned task sends when a
/// composition-affecting option changes AND the dashboard is already open.
/// Maps to the same `Effect::OpenDashboard` — its host applier
/// (`do_open_dashboard`) already recomposes-in-place for an existing buffer,
/// so no new host applier is needed, only a new (gated) trigger.
struct DashboardRecomposeTrigger;

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
    install_recompose_triggers(boot);
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

/// DB.6 (design.md §7): recompose the dashboard, in place, whenever one of
/// the options its composition depends on changes: `dashboard.sections`,
/// `dashboard.source` (§8's full-override escape hatch), or `ui.nerd_fonts`
/// (icon-palette choice — a `lattice-host`-owned option; matched by string
/// name here, the same way `dashboard.enabled`'s startup gate stays free of
/// naming its own crate's option *type* across the trigger boundary, so this
/// crate never needs a `lattice-host` dependency).
///
/// Gated on `boot.buffer_store().find_by_name(DASHBOARD_BUFFER_NAME)` —
/// unlike the startup trigger, this must NEVER create the buffer. Editing
/// `dashboard.sections` while the dashboard isn't the active buffer (or
/// isn't open at all) must stay silent; only an already-open dashboard
/// refreshes.
///
/// Pane resize is deliberately NOT one of the matched names: `content_left_pad`
/// (DB.4's gutter-based centring) is recomputed from the live viewport width
/// on every `rebuild_option_cache` call, which `Editor::set_pane_viewport`
/// already invokes on every resize (`lattice-host/src/editor_actor.rs`) — a
/// full recompose would just redundantly rebuild the same fragments for a
/// value the renderer already derives fresh, with no visible difference.
///
/// Subscribes via the legacy `Event::OptionChanged` bus (not the typed-event
/// path DB.5 uses for `Startup`) — `OptionChanged` is a pre-existing
/// `lattice_protocol::Event` variant, the same one `ConfigRegistry` has
/// always published on every `:set`. Long-lived (loops for the editor's
/// lifetime), unlike the startup trigger's single `.recv().await`.
fn install_recompose_triggers(boot: &mut impl SubsystemBoot) {
    let buffer_store: BufferStoreHandle = boot.buffer_store().clone();
    let recompose_bus = boot.inbound(|_: DashboardRecomposeTrigger| vec![Effect::OpenDashboard]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    boot.event_bus().subscribe(
        EventFilter::kind(EventKind::OptionChanged),
        SubscriptionTarget::Channel(tx),
    );
    boot.runtime_handle().spawn(async move {
        while let Some(event) = rx.recv().await {
            let Event::OptionChanged { name, .. } = event else {
                continue;
            };
            if !matches!(
                name.as_str(),
                "dashboard.sections" | "dashboard.source" | "ui.nerd_fonts"
            ) {
                continue;
            }
            if buffer_store.find_by_name(DASHBOARD_BUFFER_NAME).is_some() {
                let _ = recompose_bus.send(DashboardRecomposeTrigger);
            }
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
            parse_args: Arc::new(parse_no_args),
            apply: Arc::new(|_| Ok(Effect::OpenDashboard)),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
