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

use lattice_mode::SubsystemBoot;

use crate::LspSupervisorHandle;
use crate::completion::register_lsp_completion_mode;
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
}
