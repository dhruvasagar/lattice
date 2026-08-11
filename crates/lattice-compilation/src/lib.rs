//! `lattice-compilation` — native compilation mode (CM.1).
//!
//! Runs a build/test/lint command off-thread (pipe-captured) and
//! streams its stdout+stderr into a read-only synthetic
//! `*compilation*` `Document` buffer live. The emacs
//! `compilation-mode` / `M-x recompile` and vim `:make` workflow's
//! streaming half, on Lattice's substrate.
//!
//! This is a **native built-in** wired through the `SubsystemBoot`
//! install seam (not a plugin). CM.1 delivers the streaming buffer
//! plus `:compile` / `:recompile` / `:make`. The error list
//! (CM.2), parser registry (CM.3), and `*problems*` multibuffer
//! view (CM.4) land in later slices — see
//! `docs/dev/architecture/compilation-mode.md` and its slice plan.
//!
//! ## Shape
//!
//! - [`events`] — the `CompilationOutputPushed` typed event +
//!   `OutputChunk`.
//! - [`mode`] — `CompilationMode` (`ReadOnly + NoFile` major) + its
//!   streaming drain.
//! - [`service`] — `CompilationService` (process lifecycle,
//!   pipe-capture, kill-on-recompile) off the actor thread.
//! - [`ex_commands`] — `:compile` / `:recompile` / `:make`.
//! - [`install`] — the crate-owned `SubsystemBoot` entry point.

mod events;
mod ex_commands;
mod headerline;
mod mode;
mod parser;
mod parsers;
mod service;

pub use events::{CompilationOutputPushed, OutputChunk};
pub use ex_commands::register_compilation_ex_commands;
pub use headerline::{
    COMPILATION_HEADERLINE_PROVIDER_ID, CompilationHeaderline, CompilationHeadlineState,
};
pub use mode::{CompilationMode, apply_chunk};
pub use parser::{
    CompilationLocation, CompilationParser, ParserRegistry, match_severity, parse_location_line,
    scan_location_lines, scan_severities,
};
pub use service::{CompilationService, CompilationServiceHandle, DefaultCompilationService};

use std::path::PathBuf;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId};
use lattice_grammar::AppEffect;
use lattice_grammar::effect::Effect;
use lattice_mode::inbound::InboundBus;
use lattice_mode::{GutterSeverityLevel, ModeActivator, SubsystemBoot};
use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

/// CM.3c: the off-thread → host-state seam for the `*compilation*` buffer's
/// per-buffer severity gutter index. The compilation drain (in
/// `CompilationMode::on_activate`) sends `(buffer_id, full_index)` through
/// this bus whenever the index changes; the `install`-registered handler
/// maps each send to [`AppEffect::CompilationGutterSet`]. Registered as a
/// [`ServiceRegistry`](lattice_mode::ServiceRegistry) handle under this exact
/// alias (per the Arc/TypeId convention); the drain looks it up via
/// `ctx.service::<CompilationGutterBusHandle>()`. `send` bakes in the editor
/// wake, so marks reach the screen off-keystroke.
pub type CompilationGutterBusHandle = Arc<InboundBus<(BufferId, Vec<(u32, ErrorSeverity)>)>>;

/// CM.3c (2026-07-22): the off-thread → host-state seam for the
/// `*compilation*` buffer's location-line index (theme-based
/// highlighting). Twin of [`CompilationGutterBusHandle`]: the
/// compilation drain sends `(buffer_id, full_location_lines)` through
/// this bus whenever the index changes; the `install`-registered handler
/// maps each send to [`AppEffect::CompilationLocationLines`].
/// Registered as a ServiceRegistry handle under this exact alias; the
/// drain looks it up via `ctx.service::<CompilationLocationBusHandle>()`.
pub type CompilationLocationBusHandle = Arc<InboundBus<(BufferId, Vec<(u32, u32, u32)>)>>;

/// CM.3d (2026-07-22): the theme colours bus — the mode sends
/// resolved `(bg, fg)` once during activation; the handler maps to
/// [`AppEffect::CompilationThemeColors`].
pub type CompilationThemeColorsBusHandle = Arc<InboundBus<(u32, u32)>>;

/// CM.3c: map the parser-native [`ErrorSeverity`] onto the renderer-
/// facing [`GutterSeverityLevel`] (`Error→Error`, `Warning→Warning`,
/// `Info→Info`, `Note→Info`). The single conversion in the whole
/// compilation-severity pipeline — the parser, the inbound bus, and the
/// `AppEffect` payload all carry `ErrorSeverity`, and the host arm calls
/// this once when writing the render-state slot. `Note` collapses to `Info`
/// because `GutterSeverityLevel` has no `Note` rank; there is no reverse
/// conversion, so nothing is lost by round-trip.
pub fn gutter_level(severity: ErrorSeverity) -> GutterSeverityLevel {
    match severity {
        ErrorSeverity::Error => GutterSeverityLevel::Error,
        ErrorSeverity::Warning => GutterSeverityLevel::Warning,
        ErrorSeverity::Info => GutterSeverityLevel::Info,
        ErrorSeverity::Note => GutterSeverityLevel::Info,
    }
}

/// Synthetic name of the streaming compilation buffer. `:ls` and
/// `:b *compilation*` reach it; `:bn` / `:bp` skip it
/// (`listed = false`).
pub const COMPILATION_BUFFER_NAME: &str = "*compilation*";

/// Compilation-mode's synthetic-buffer flags: unlisted (skipped by
/// `:bn` / `:bp`), non-hidden, non-ephemeral — the canonical shape for
/// a mode-owned subsystem buffer.
const COMPILATION_BUFFER_FLAGS: BufferFlags = BufferFlags {
    listed: false,
    hidden: false,
    ephemeral: false,
};

/// Provision the `*compilation*` buffer and kick off a run — the
/// compilation mode's own responsibility, driven through the
/// `&mut`-backed [`ModeActivator::ensure_named_document`] creation seam.
///
/// Called from the `AppEffect::CompileRun` dispatch arm (which passes
/// the `Editor` as `&mut dyn ModeActivator`). Creates + activates the
/// `*compilation*` buffer on first use — activation runs
/// [`CompilationMode`]'s `on_activate`, establishing the streaming drain
/// **before** the service publishes its first `Reset` — then runs the
/// registered [`CompilationServiceHandle`]. Idempotent on `:recompile`
/// (reuses the buffer; the drain from the first activation stays live).
///
/// Returns the buffer id (so the host can activate the buffer + repaint),
/// or `None` when the compilation service is not registered.
pub fn start_compilation(
    activator: &mut dyn ModeActivator,
    cmdline: Option<String>,
    cwd: Option<PathBuf>,
) -> Option<BufferId> {
    let id = activator.ensure_named_document(
        COMPILATION_BUFFER_NAME,
        CompilationMode::mode_id(),
        COMPILATION_BUFFER_FLAGS,
    );
    // `services.get::<CompilationServiceHandle>()` returns
    // `Arc<Arc<dyn CompilationService>>` per the ServiceRegistry
    // Arc/TypeId convention — unwrap one layer before `run`.
    let svc = activator.services().get::<CompilationServiceHandle>()?;
    (*svc).clone().run(cmdline, cwd);
    Some(id)
}

/// Wire the compilation subsystem's ex-commands, mode, service, and
/// off-keystroke wake into the editor at boot. One Phase-B line in
/// `editor_boot.rs`.
pub fn install(boot: &mut impl SubsystemBoot) {
    register_compilation_ex_commands(boot.commands_mut());
    boot.modes_mut().register(CompilationMode).ok();

    // CM.3a: the sanctioned off-thread → host-state seam for parsed
    // error entries (LSP-diagnostics shape). The stderr reader
    // accumulates parsed `ErrorEntry`s and sends the FULL list; this
    // handler maps each send to `AppEffect::SetErrorList`, whose host arm
    // calls `Editor::set_error_list` (replace-semantics — the growing
    // list stays visible; an empty vec on a new run clears it). The wake
    // is baked into `InboundBus::send`, so the list reaches the screen
    // off-keystroke without a keypress.
    let qf_bus = boot.inbound::<Vec<ErrorEntry>, _>(|entries| {
        vec![Effect::AppAction(AppEffect::SetErrorList {
            // EP.1: this producer owns the `Compilation` slice; an LSP
            // republish alongside it replaces only its own.
            source: lattice_protocol::error_list::ErrorSource::Compilation,
            entries,
        })]
    });

    // CM.3c: the twin off-thread → host-state seam for the `*compilation*`
    // buffer's per-buffer severity gutter index. The drain (in
    // `CompilationMode::on_activate`) sends `(buffer_id, full_index)`; this
    // handler maps each send to `AppEffect::CompilationGutterSet`, whose host
    // arm writes the render-state slot the renderer injects into
    // `gutter_decorations`. Registered as a service so the drain — which lives
    // in the mode's `on_activate`, not in the service — can reach it via
    // `ctx.service`.
    let gutter_bus =
        boot.inbound::<(BufferId, Vec<(u32, ErrorSeverity)>), _>(|(buffer, entries)| {
            vec![Effect::AppAction(AppEffect::CompilationGutterSet {
                buffer: buffer.0,
                entries,
            })]
        });
    boot.register_service::<CompilationGutterBusHandle>(Arc::new(gutter_bus));

    // CM.3c (2026-07-22): the location-line index bus — twin of the
    // gutter-severity bus above. The drain sends `(buffer_id, lines)`
    // through this whenever the index changes; the handler maps each
    // send to `AppEffect::CompilationLocationLines`, which the host
    // stores in the render-state slot the renderers read.
    let location_bus = boot.inbound::<(BufferId, Vec<(u32, u32, u32)>), _>(|(buffer, lines)| {
        vec![Effect::AppAction(AppEffect::CompilationLocationLines {
            buffer: buffer.0,
            lines,
        })]
    });
    boot.register_service::<CompilationLocationBusHandle>(Arc::new(location_bus));

    // CM.3d (2026-07-22): theme colours bus — the mode sends resolved
    // `compilation.location` bg/fg once during activation so the
    // renderers read from the theme rather than hardcoding RGB.
    let theme_colors_bus = boot.inbound::<(u32, u32), _>(|(bg, fg)| {
        vec![Effect::AppAction(AppEffect::CompilationThemeColors {
            bg,
            fg,
        })]
    });
    boot.register_service::<CompilationThemeColorsBusHandle>(Arc::new(theme_colors_bus));

    let svc: CompilationServiceHandle = Arc::new(DefaultCompilationService::new(
        boot.event_bus().clone(),
        boot.runtime_handle().clone(),
        qf_bus,
    ));
    boot.register_service::<CompilationServiceHandle>(svc);

    // Streamed output arrives off-keystroke; wake the editor so the
    // `*compilation*` buffer repaints without a keypress.
    boot.wake_on_event::<CompilationOutputPushed>();
}

#[cfg(test)]
mod gutter_level_tests {
    use super::gutter_level;
    use lattice_mode::GutterSeverityLevel;
    use lattice_protocol::error_list::ErrorSeverity;

    #[test]
    fn maps_each_severity_to_a_gutter_level() {
        // Error/Warning/Info map 1:1; Note collapses to Info (GutterSeverityLevel
        // has no Note rank). Single conversion in the whole pipeline.
        assert_eq!(
            gutter_level(ErrorSeverity::Error),
            GutterSeverityLevel::Error
        );
        assert_eq!(
            gutter_level(ErrorSeverity::Warning),
            GutterSeverityLevel::Warning
        );
        assert_eq!(gutter_level(ErrorSeverity::Info), GutterSeverityLevel::Info);
        assert_eq!(gutter_level(ErrorSeverity::Note), GutterSeverityLevel::Info);
    }
}
