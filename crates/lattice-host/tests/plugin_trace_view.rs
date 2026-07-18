//! PO.4.1 — the `:plugin-trace` view wires end-to-end: the ex-command
//! self-registers at boot, and opening the `*plugin-trace*` buffer activates
//! `plugin-trace-mode` (the major mode that seeds from the tracer ring +
//! subscribes to `PluginTracePushed` in `on_activate`). The line format + the
//! seed/filter logic are unit-tested in `lattice-plugin-trace` (`format` +
//! `mode`); here we prove the host wiring — that `Effect::OpenSyntheticBuffer {
//! mode_id: "plugin-trace-mode" }` lands on the provider's registered mode with
//! zero host-specific code (the `plugins_manager_view` split).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_plugin_host::{PluginTracerHandle, TraceLevel};
use lattice_plugin_trace::PluginTraceMode;

#[test]
fn plugin_trace_ex_command_registered_at_boot() {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    assert!(
        editor.registry.load().id_by_name("plugin-trace").is_some(),
        "the :plugin-trace ex-command self-registers at boot"
    );
}

#[tokio::test]
async fn opening_the_trace_buffer_activates_plugin_trace_mode() {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));

    // The generic open the `:plugin-trace` ex-command emits
    // (`Effect::OpenSyntheticBuffer`) resolves to this host method — no
    // provider-specific host code.
    editor.open_synthetic_buffer("*plugin-trace*", "plugin-trace-mode");

    let id = editor
        .buffers
        .by_name("*plugin-trace*")
        .expect("*plugin-trace* buffer exists after :plugin-trace");
    let major = editor.active_modes.get(&id).and_then(|m| m.major());
    assert_eq!(
        major,
        Some(PluginTraceMode::mode_id()),
        "the trace buffer's major mode is the provider-registered plugin-trace-mode"
    );
}

#[tokio::test]
async fn set_plugin_trace_level_raises_the_tracer_default_live() {
    // PO.4.3: `:set plugin.trace-level=debug` → the config publishes
    // `OptionChanged` → the loader's observer parses it → `tracer.set_default_level`.
    // End-to-end proof the whole live chain is wired.
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let tracer = editor
        .services
        .get::<PluginTracerHandle>()
        .expect("PluginTracer registered at boot");
    // An unknown plugin id reads the global default gate.
    assert_eq!(
        tracer.plugin_level(u32::MAX),
        TraceLevel::Info,
        "the default gate is Info before any :set"
    );

    editor
        .config
        .parse_and_set_command("plugin.trace-level=debug")
        .expect("plugin.trace-level accepts `debug`");

    // The observer runs on the editor runtime (not this test runtime); poll.
    let mut raised = false;
    for _ in 0..200 {
        if tracer.plugin_level(u32::MAX) == TraceLevel::Debug {
            raised = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        raised,
        "the loader observer raised the tracer default to Debug live"
    );
}
