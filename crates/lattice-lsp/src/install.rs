//! BC.8a (2026-06-24): the crate-owned `install(boot)` entry point.
//!
//! LSP is the last + largest boot-composition migration; it is sub-sliced
//! (BC.8a–e). **BC.8a is the behaviour-preserving foundation:** the LSP modes
//! and the four server-`*/refresh` off-keystroke wakes register through the
//! generic [`SubsystemBoot`] surface, collapsing those host sites into the
//! Phase-B install line. The inbound buses (apply-edit / show-document /
//! configuration / show-message-request) reshape onto `boot.inbound::<T>` in
//! the later sub-slices (BC.8b–e).
//!
//! ## What BC.8a collapses here
//!
//! - **Modes** — `register_lsp_log_modes` (the ~17 LSP modes: log/trace/
//!   diagnostics/hover/signature/format/rename/symbols/code-action/nav/
//!   progress/highlight/selection-range/folding/inlay/semantic-tokens) +
//!   `register_lsp_completion_mode`, which needs the `LspSupervisorHandle`.
//!   That handle is a **host-created** value (the supervisor + its four
//!   server-initiated buses live in `editor_boot::build_lsp_subsystem`, which
//!   produces `Editor` fields — the diff `DiffSubsystem`-bind residue class), so
//!   the host registers it as a **Phase-A service** and the install reads it
//!   back generically via `boot.service::<LspSupervisorHandle>()` — the trait
//!   names no host type, and mode-ownership is preserved (the *mode* owns its
//!   registration; the host owns the supervisor's lifecycle).
//! - **Off-keystroke wakes** — the four `workspace/*/refresh` notifications
//!   (`LspInlayHintRefresh` / `LspSemanticTokensRefresh` / `LspDiagnosticRefresh`
//!   / `LspCodeLensRefresh`) become `boot.wake_on_event::<E>()`. Behaviour is
//!   byte-identical to the host's hand-rolled L1c `wake_on` forwarders
//!   (subscribe-typed + spawn a notify task), now baked into the primitive so
//!   the wake can't be forgotten (paramount #4). The per-type **drain** channels
//!   (`pending_*_refresh_rx` → `drain_*_refresh` Editor methods) keep doing the
//!   cache-eviction work host-side — they are `&mut Editor` residue, untouched.
//!
//! ## What stays host-side (residue, NOT mode-ownership violations)
//!
//! - **`build_lsp_subsystem`** — produces `Editor`-field values (the supervisor
//!   handle, the `DiagnosticsLayer` the renderer reads per-frame, the four
//!   `pending_*_rx` inbound receivers). `install(boot)` returns nothing and
//!   can't seat Editor fields — the diff `DiffSubsystem`-bind precedent.
//! - **The host-created services** (`LspSupervisorHandle`, `LspLogger`,
//!   `DiagnosticsQueryHandle`) are registered host-side because the host owns
//!   the values (diff's `DiffSubsystemHandle` precedent). `install` only
//!   *reads* the supervisor handle for completion-mode registration.
//! - **`lsp_diagnostics.set_wake`** arms the `DiagnosticsLayer`'s render wake —
//!   an Editor-field. The inbound drains + the modeline (`ModelineElementUpdate`,
//!   a generic event) wake stay host-side.

use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::Effect;
use lattice_grammar::app_effect::AppEffect;
use lattice_mode::SubsystemBoot;
use lattice_protocol::error_list::{ErrorEntry, ErrorSource, ErrorWrite};

use crate::LspSupervisorHandle;
use crate::completion::register_lsp_completion_mode;
use crate::diagnostics_layer::DiagnosticsLayer;
use crate::error_list_feed::ErrorListFeed;
use crate::modes::register_lsp_log_modes;
use crate::{
    LspCodeLensRefresh, LspDiagnosticRefresh, LspInlayHintRefresh, LspSemanticTokensRefresh,
};

/// Wire the LSP subsystem's modes + off-keystroke refresh wakes into the editor
/// at boot (BC.8a). One Phase-B line in `editor_boot.rs`.
pub fn install(boot: &mut impl SubsystemBoot) {
    // ── Modes ───────────────────────────────────────────────────────────────
    register_lsp_log_modes(boot.modes_mut());
    // `lsp-completion-mode` captures the supervisor handle. It is a host-created
    // value registered as a Phase-A service (before this install line); read it
    // back generically so the trait names no host type. Clone one Arc layer off
    // (`boot.service::<T>()` yields `Arc<T>`; the registered `T` is the handle).
    let lsp: LspSupervisorHandle = (*boot
        .service::<LspSupervisorHandle>()
        .expect("LspSupervisorHandle registered as a Phase-A service before lattice_lsp::install"))
    .clone();
    register_lsp_completion_mode(boot.modes_mut(), lsp);

    // ── Off-keystroke wakes ─────────────────────────────────────────────────
    // The four `workspace/*/refresh` notifications repaint without a keypress.
    // The per-type drain channels (host-side `pending_*_refresh_rx`) still do
    // the cache-eviction in `run_tick_pending`; these wakes only fire
    // `async_landed`. Byte-identical to the retired L1c `wake_on` forwarders.
    boot.wake_on_event::<LspInlayHintRefresh>();
    boot.wake_on_event::<LspSemanticTokensRefresh>();
    boot.wake_on_event::<LspDiagnosticRefresh>();
    boot.wake_on_event::<LspCodeLensRefresh>();

    // ── EP.3: the error-list feed ───────────────────────────────────────────
    install_error_list_feed(boot);

    // ── LR.1: the references multibuffer ────────────────────────────────────
    install_references_provider(boot);
}

/// LR.1 (2026-08-11): register the references view's mode + per-view
/// service, and wire its `DocumentClosed` cleanup.
///
/// The provider lives in this crate rather than `lattice-multibuffer`
/// per the provider-home reversal: `gr`'s binding, the handler bodies
/// and the view all belong to the LSP subsystem, so they sit together.
fn install_references_provider(boot: &mut impl SubsystemBoot) {
    use crate::providers::references::{
        LspReferencesService, LspReferencesServiceHandle, register_references_mode,
    };

    register_references_mode(boot.modes_mut());
    // The refresh action's command must resolve for the mode's
    // `refresh_action()` target and its handler registration.
    crate::providers::references::register_references_actions(boot.commands_mut());

    let service: LspReferencesServiceHandle = Arc::new(LspReferencesService::new());
    boot.register_service::<LspReferencesServiceHandle>(Arc::clone(&service));

    // Drop a closed view's stored origin. Same shape as
    // `register_multibuffer_modes`: only wire the subscriber when a
    // runtime is in scope, so `Editor` constructed outside one (the
    // host lib tests) skips it rather than panicking. A skipped
    // subscriber leaks at most one small entry per view in a
    // short-lived test process.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::debug!(
            "references: no tokio runtime in scope; skipping DocumentClosed cleanup \
             (expected in test paths)"
        );
        return;
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<lattice_protocol::Event>();
    boot.event_bus().subscribe(
        lattice_runtime::EventFilter::kind(lattice_protocol::EventKind::DocumentClosed),
        lattice_runtime::SubscriptionTarget::Channel(tx),
    );
    handle.spawn(async move {
        while let Some(event) = rx.recv().await {
            if let lattice_protocol::Event::DocumentClosed { id } = event {
                service.forget_by_document_id(id);
            }
        }
    });
}

/// EP.3 (2026-08-10): wire the language server up as a producer of the
/// core error list.
///
/// `boot.inbound` is mandatory here, not stylistic: its `send` bakes in
/// the `async_landed` wake, so a republish reaches `*problems*` and the
/// `:next-error` family without waiting for a keystroke. A bare
/// `tick_callback` would reproduce the "it only updates when I press
/// something" bug class (`boot-composition.md` §3).
///
/// Every write is [`ErrorWrite::Refresh`] — a live feed must re-anchor
/// the navigation index, or walking the list while typing snaps the
/// user back to entry 1 on each keystroke (EP.2).
///
/// No-ops when the `DiagnosticsLayer` is absent (test harnesses that
/// install a trimmed boot): the feed simply never starts, rather than
/// panicking a subsystem installer.
fn install_error_list_feed(boot: &mut impl SubsystemBoot) {
    let Some(layer) = boot.service::<DiagnosticsLayer>() else {
        tracing::debug!("lsp: DiagnosticsLayer service absent; error-list feed not started");
        return;
    };
    let layer: DiagnosticsLayer = (*layer).clone();

    let config = boot.service::<Arc<ConfigRegistry>>();

    let bus = boot.inbound::<Vec<ErrorEntry>, _>(|entries| {
        vec![Effect::AppAction(AppEffect::SetErrorList {
            source: ErrorSource::Lsp,
            write: ErrorWrite::Refresh,
            entries,
        })]
    });

    // Read the option every tick rather than capturing its value, so
    // `:set lsp.diagnostics-to-error-list` takes effect immediately
    // instead of at the next restart.
    let enabled = move || match &config {
        // `get_typed` returns None before the option registry is
        // initialised; treat that as the option's default (on) rather
        // than silently disabling a feature the user expects to have.
        Some(cfg) => cfg
            .get_typed::<lattice_config::core_options::LspDiagnosticsToErrorList>()
            .map(|v| *v)
            .unwrap_or(true),
        None => true,
    };

    let feed = ErrorListFeed::spawn(layer, enabled, move |entries| {
        // A send failure means the drain is gone — the editor is
        // shutting down. Nothing to recover, and nothing worth
        // surfacing to the user, but don't swallow it silently either.
        if bus.send(entries).is_err() {
            tracing::debug!("lsp: error-list drain closed; feed send dropped");
        }
    });
    boot.register_service::<crate::error_list_feed::ErrorListFeedHandle>(Arc::new(feed));
}
