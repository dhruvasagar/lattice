//! PO.4 — the buffer-backed plugin boundary-trace views.
//!
//! A pure PROVIDER crate (the `lattice-plugin-manager` shape): it owns
//! `plugin-trace-mode` (major, read-only) + the `:plugin-trace` ex-command, and
//! reads the host's `PluginTracer` (PO.1/PO.3) via the `PluginTracerHandle`
//! service at activation. Everything wires through the generic [`SubsystemBoot`]
//! seam, so adding it to the editor is **one line** in the host's Phase-B install
//! list and **zero** host internals — no `Editor::` method, no host `Action`
//! variant (the mode-ownership acid test).
//!
//! Open flow, all generic primitives (no provider-specific host code):
//!   1. `:plugin-trace` (registered here) returns
//!      [`Effect::OpenSyntheticBuffer`](lattice_grammar::Effect::OpenSyntheticBuffer)
//!      `{ name: "*plugin-trace*", mode_id: "plugin-trace-mode" }`.
//!   2. The host generically ensures that buffer under `plugin-trace-mode` +
//!      activates it (`Editor::open_synthetic_buffer`).
//!   3. `PluginTraceMode::on_activate` seeds from the tracer ring + subscribes to
//!      `PluginTracePushed` for the live tail (off-thread).
//!
//! The per-plugin `*plugin-trace:<name>*` view + the `:plugins` manager `t`
//! drill-in land in PO.4.2; the live `plugin.trace-level` option in PO.4.3.
//!
//! Design: `docs/dev/architecture/plugin-observability.md` §6.

use std::sync::Arc;

use lattice_grammar::{Args, Effect, ExCommandSpec, LatencyClass, SurfaceForm};
use lattice_mode::SubsystemBoot;

mod format;
mod mode;

pub use format::{
    SHARED_BUFFER_NAME, TRACE_MODE_ID, format_trace_line, parse_per_plugin_name,
    per_plugin_buffer_name,
};
pub use mode::PluginTraceMode;

/// Install the plugin-trace views: register `plugin-trace-mode` + the
/// `:plugin-trace` ex-command. Seated in the host's Phase-B install list (before
/// the registry freeze), so it registers directly via `boot.modes_mut()` /
/// `boot.commands_mut()`.
///
/// No dependency on the loader/tracer being installed first: the mode resolves
/// `PluginTracerHandle` at *activation* (by which point boot is complete), and
/// the ex-command returns a static effect. A missing tracer service at activation
/// degrades to an empty buffer, never a panic.
pub fn install(boot: &mut impl SubsystemBoot) {
    boot.modes_mut()
        .register(PluginTraceMode)
        .expect("plugin-trace-mode registers without conflict");

    boot.commands_mut().register_ex_command(
        "plugin-trace",
        "Open the shared plugin boundary-trace buffer (`*plugin-trace*`) — every \
         loaded plugin's host↔guest calls, interleaved and tagged by plugin id, \
         with timing / traps / capability denials. Read-only, live-tailing. Per \
         plugin, drill in with `t` on a `:plugins` row. Raise verbosity with \
         `:set plugin.trace-level=debug` (off by default — no per-call noise).",
        plugin_trace_spec(),
    );
}

/// The `:plugin-trace` ex-command: takes no argument, returns the generic
/// open-synthetic-buffer effect for the shared view. `Reflex` — it does no
/// blocking work on the dispatch path (the host runs the open; the mode seeds +
/// tails off-thread).
fn plugin_trace_spec() -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: Arc::new(|_line: &str, _bang: bool| Ok(Args::None)),
        apply: Arc::new(|_ctx| {
            Ok(Effect::OpenSyntheticBuffer {
                name: SHARED_BUFFER_NAME.to_string(),
                mode_id: TRACE_MODE_ID.to_string(),
            })
        }),
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    }
}
