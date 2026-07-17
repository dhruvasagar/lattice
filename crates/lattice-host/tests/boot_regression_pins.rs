//! BC.2 — boot-sequence regression pins (the gate for the BC.3+ migrations).
//!
//! Boot composition restructures `editor_boot.rs` from a god-function into
//! Phase A (generic primitives) → Phase B (per-subsystem `install(boot)`),
//! migrating one subsystem per slice. Boot is load-bearing: a behaviour change
//! ripples everywhere. These pins capture each subsystem's CURRENT boot
//! contract — its modes are registered, its subsystem-owned ex-commands
//! resolve by name, its services are present under the right `TypeId`, and one
//! representative off-keystroke async path wakes the editor — so a migration
//! that silently drops any of them fails here, BEFORE it lands.
//!
//! Design fragment: `docs/dev/architecture/boot-composition.md`.
//! Slice plan: `docs/dev/operations/slice-plans/boot-composition.md` (BC.2).
//!
//! ## Scope notes
//!
//! - **Services** use the EXACT type the register site used (per the
//!   `ServiceRegistry` Arc/TypeId rule — `register::<T>` keys on
//!   `TypeId::of::<T>()`, so `get::<T>()` must pass the same `T`; e.g. the
//!   event bus is registered as `Arc<EventBus>`, looked up as `Arc<EventBus>`).
//! - **Commands** pin only *subsystem-wired* names (claude-code, multibuffer).
//!   LSP's `lsp-*` commands come from the generic
//!   `lattice_grammar::ex_commands::populate`, not LSP-subsystem boot wiring, so
//!   they are not a meaningful "LSP boot" pin.
//! - **Wake** pins the boot-wired *event → wake* forwarder seam (LSP refresh +
//!   multibuffer excerpts-ready, both published on `editor.event_bus`, both
//!   waking `editor.async_landed`). The claude-code inbound → wake is already
//!   covered by `lattice-claude-code`'s `send_wakes_the_actor` + the BC.1
//!   `inbound` tests; the terminal/diff wakes fire from `on_activate` /
//!   subsystem tasks, not boot wiring, so they are out of scope for a *boot*
//!   pin (they need a buffer activated first).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::ModeId;

/// Boot a real editor on a scratch document. `Editor::boot` is synchronous,
/// acquires the process-wide shared runtime, and (M-async.5) does NOT spawn
/// language servers or do blocking I/O — LSP attachment is lazy (on buffer
/// open), so this is cheap and side-effect-free.
fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("scratch\n"))
}

fn assert_mode(editor: &Editor, name: &str) {
    assert!(
        editor.mode_registry.load().is_registered(ModeId::new(name)),
        "mode `{name}` must be registered at boot"
    );
}

fn assert_command(editor: &Editor, name: &str) {
    assert!(
        editor.registry.load().lookup_by_name(name).is_some(),
        "ex-command `{name}` must resolve by name at boot"
    );
}

// ── Modes ──────────────────────────────────────────────────────────────────

#[test]
fn lsp_modes_registered_at_boot() {
    let editor = boot();
    for name in [
        "lsp-mode",
        "lsp-diagnostics-mode",
        "lsp-progress-mode",
        "lsp-folding-mode",
        "lsp-completion-mode",
        "lsp-log-mode",
        "lsp-trace-log-mode",
        "lsp-server-log-mode",
    ] {
        assert_mode(&editor, name);
    }
}

#[test]
fn multibuffer_mode_registered_at_boot() {
    assert_mode(&boot(), "multibuffer-mode");
}

#[test]
fn terminal_modes_registered_at_boot() {
    let editor = boot();
    for name in [
        "terminal-mode",
        "terminal-insert-mode",
        "terminal-normal-mode",
    ] {
        assert_mode(&editor, name);
    }
}

#[test]
fn diff_mode_registered_at_boot() {
    assert_mode(&boot(), "diff-mode");
}

#[test]
fn emacs_keys_mode_registered_at_boot() {
    assert_mode(&boot(), "emacs-keys-mode");
}

#[test]
fn claude_code_mode_registered_at_boot() {
    assert_mode(&boot(), "claude-code-mode");
}

// ── Subsystem-wired ex-commands ────────────────────────────────────────────

#[test]
fn subsystem_ex_commands_resolve_by_name() {
    let editor = boot();
    // claude-code (register_claude_code_ex_commands)
    assert_command(&editor, "claude-code-start");
    assert_command(&editor, "claude-code-stop");
    // multibuffer narrow provider (register_narrow_ex_commands)
    assert_command(&editor, "narrow");
    assert_command(&editor, "widen");
}

// ── Services (exact register-site type per the Arc/TypeId rule) ─────────────

#[test]
fn lsp_services_present_at_boot() {
    let editor = boot();
    assert!(
        editor
            .services
            .get::<lattice_lsp::LspSupervisorHandle>()
            .is_some(),
        "LspSupervisorHandle must be registered"
    );
    assert!(
        editor
            .services
            .get::<lattice_lsp::modes::DiagnosticsQueryHandle>()
            .is_some(),
        "DiagnosticsQueryHandle must be registered"
    );
    assert!(
        editor.services.get::<lattice_lsp::LspLogger>().is_some(),
        "LspLogger must be registered"
    );
}

#[test]
fn terminal_service_present_at_boot() {
    assert!(
        boot()
            .services
            .get::<lattice_terminal::TerminalStoreHandle>()
            .is_some(),
        "TerminalStoreHandle must be registered"
    );
}

#[test]
fn multibuffer_service_present_at_boot() {
    assert!(
        boot()
            .services
            .get::<lattice_multibuffer::MultibufferRegistryHandle>()
            .is_some(),
        "MultibufferRegistryHandle must be registered"
    );
}

#[test]
fn claude_code_service_present_at_boot() {
    assert!(
        boot()
            .services
            .get::<lattice_ai::mcp::ClaudeCodeServerHandle>()
            .is_some(),
        "ClaudeCodeServerHandle must be registered"
    );
}

#[test]
fn plugin_loader_service_present_at_boot() {
    // PL8.A: `lattice_plugin_loader::install` stands the wasmtime runtime up and
    // registers the loader handle. Pinned here so a later boot restructure (or a
    // plugin-host build regression) that drops the loader fails BEFORE it lands.
    // Registered as `PluginLoaderHandle` (= `Arc<PluginLoader>`), looked up with
    // the same `T` per the ServiceRegistry Arc/TypeId rule.
    assert!(
        boot()
            .services
            .get::<lattice_plugin_loader::PluginLoaderHandle>()
            .is_some(),
        "PluginLoaderHandle must be registered at boot (the editor instantiates the plugin host)"
    );
}

#[test]
fn plugin_tracer_service_present_at_boot() {
    // PO.1: `lattice_plugin_loader::install` stands the boundary tracer up and
    // registers it (publisher bound to the runtime bus). Pinned so a boot
    // restructure that drops it fails before it lands — the seams (PO.2/PO.3) and
    // the trace-buffer views (PO.4) both resolve it via this service.
    assert!(
        boot()
            .services
            .get::<lattice_plugin_host::PluginTracerHandle>()
            .is_some(),
        "PluginTracerHandle must be registered at boot (PO.1)"
    );
}

#[test]
fn plugin_loader_captures_every_drain_service() {
    // PL8.B: the loader's `install` captures its drain-required services via
    // `boot.service::<T>()`, which returns `None` for any service registered
    // AFTER the install call. `drain_grammar` needs the `CommandRegistryHandle`
    // and `drain_mode` needs the `KeymapHandle`, both registered late in boot —
    // so `install` must be seated after them. A boot-ordering regression that
    // moves `install` earlier silently degrades those seams to a `NotWired` skip
    // (the unit-test drains bypass `install` by wiring `LoaderServices` by hand,
    // so ONLY this real-boot pin catches it). Assert every drain service landed.
    let editor = boot();
    let loader = editor
        .services
        .get::<lattice_plugin_loader::PluginLoaderHandle>()
        .expect("loader handle present");
    let wired = loader.wired_seams();
    assert!(
        wired.all(),
        "the plugin loader must capture every drain service at boot; missing: {wired:?}"
    );
}

#[test]
fn plugin_lifecycle_ex_commands_registered_at_boot() {
    // PL8.C.2 (option A): the loader self-registers `:plugin-load` /
    // `:plugin-unload` / `:plugin-reload` into the runtime-mutable command
    // registry at install — plain names resolving directly via `id_by_name`
    // (zero host code, no `expand_alias` entry). Pinned end-to-end on a real boot
    // so a regression in `register_ex_commands` (or a command-registry handle
    // that isn't the editor's shared one) fails before landing.
    let editor = boot();
    let commands = editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .expect("command registry service present");
    let snapshot = commands.load();
    for name in ["plugin-load", "plugin-unload", "plugin-reload", "reload-config"] {
        assert!(
            snapshot.id_by_name(name).is_some(),
            "`:{name}` must be registered in the editor's command registry at boot"
        );
    }
}

#[test]
fn generic_host_services_present_at_boot() {
    // These generic primitives are exactly what BC.3's Phase A will own and
    // hand to subsystems via `BootContext`; pin them so the Phase split does
    // not drop any.
    let editor = boot();
    assert!(
        editor
            .services
            .get::<Arc<lattice_runtime::EventBus>>()
            .is_some(),
        "EventBus (registered as Arc<EventBus>) must be present"
    );
    assert!(
        editor
            .services
            .get::<lattice_mode::BufferStoreHandle>()
            .is_some(),
        "BufferStoreHandle must be registered"
    );
    assert!(
        editor
            .services
            .get::<lattice_mode::TickCallbackRegistryHandle>()
            .is_some(),
        "TickCallbackRegistryHandle must be registered"
    );
    assert!(
        editor
            .services
            .get::<lattice_mode::ActionHandlerRegistryHandle>()
            .is_some(),
        "ActionHandlerRegistryHandle must be registered"
    );
    assert!(
        editor
            .services
            .get::<lattice_grammar::CommandRegistryHandle>()
            .is_some(),
        "CommandRegistryHandle must be registered"
    );
}

// ── Off-keystroke wake (boot-wired event → wake forwarders) ─────────────────

/// Publishing a subscribed typed event on the editor's bus must wake
/// `async_landed` with NO keystroke — proving the boot-wired forwarder is live.
/// `editor.async_landed` is the same `Notify` the actor's event-driven loop
/// awaits to run `run_tick_pending`.
async fn assert_event_wakes<E>(editor: &Editor, event: E)
where
    E: lattice_protocol::event_registry::Event,
{
    editor.event_bus.publish_typed(event);
    let woke =
        tokio::time::timeout(Duration::from_millis(500), editor.async_landed.notified()).await;
    assert!(
        woke.is_ok(),
        "a published, boot-subscribed event must wake async_landed off-keystroke"
    );
}

#[tokio::test]
async fn lsp_refresh_event_wakes_async_landed_off_keystroke() {
    let editor = boot();
    assert_event_wakes(
        &editor,
        lattice_lsp::LspInlayHintRefresh {
            server_id: Arc::from("test-server"),
        },
    )
    .await;
}

#[tokio::test]
async fn multibuffer_excerpts_ready_wakes_async_landed_off_keystroke() {
    let editor = boot();
    assert_event_wakes(
        &editor,
        lattice_multibuffer::providers::search::MultibufferExcerptsReady {
            view: lattice_core::BufferId(1),
        },
    )
    .await;
}
