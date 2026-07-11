//! M.2.b.2 (2026-06-01): `MultibufferMode` — the major mode bound
//! to `BufferKind::Multibuffer` via H.2's `Mode::target_buffer_kind`
//! declaration.
//!
//! Thin major: contributes `ReadOnly = true` + `NoFile = true`
//! (M.3 will make `ReadOnly` conditional once edit propagation
//! lands). Excerpt-jump motion keymap (`]e` / `[e` / `]E` / `[E`)
//! arrives in M.2.b.3. Provider-specific behaviour layers on as
//! minor modes (`ProjectSearchMultibufferMode` etc., M.6+).
//!
//! `register_multibuffer_modes(®istry, &events, mb_registry)`
//! is the single boot-wiring entry point the host calls. It
//! registers `MultibufferMode` AND wires the
//! `Event::DocumentClosed` subscriber that removes the closed
//! multibuffer's entry from the `MultibufferRegistry` (cleanup
//! contract per `multibuffer-views.md` §3.7).
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.7.

use std::sync::{Arc, OnceLock};

use lattice_config::{OptionOverrideSet, overrides};
use lattice_core::{BufferKind, FoldOverlayServiceHandle, ProviderId};
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    ModeRegistry, keymap_entry,
};
use lattice_protocol::{Event, EventKind};
use lattice_runtime::{EventBus, EventFilter, SubscriptionTarget};
use lattice_theme::{ElementOwner, ThemeRegistryHandle};

use crate::registry::MultibufferRegistryHandle;

/// M.7 / M.8: deregisters all fold overlay providers when the mode is
/// deactivated. Holds one entry per registered provider
/// (`ExcerptFoldProvider` + `FileBoundaryFoldProvider`). `Drop` fires
/// when the buffer's major mode is swapped out or the buffer closes.
pub struct MultibufferModeGuard {
    pub(crate) fold_registrations: Vec<(FoldOverlayServiceHandle, ProviderId)>,
}

impl Drop for MultibufferModeGuard {
    fn drop(&mut self) {
        for (svc, id) in self.fold_registrations.drain(..) {
            svc.remove_source(id);
        }
    }
}

/// K.2.5 (2026-06-02): static keymap catalog for `MultibufferMode`.
///
/// Four excerpt-jump motions registered by
/// [`crate::register_multibuffer_motions`] (`motions.rs:57-109`)
/// against the [`lattice_grammar::CommandRegistry`] under their
/// canonical names. The host translation pass
/// (`crates/lattice-host/src/keymap_mode_contributions.rs`)
/// resolves each row's `cmd` string at registration time and
/// builds a `KeymapBinding` carrying the entry's `doc` and
/// macro-captured `source`.
///
/// Replaces `crates/lattice-host/src/multibuffer_keymap.rs`'s
/// `multibuffer_mode_layer_bindings` which built the layer
/// trie by hand and was pushed explicitly via
/// `KeymapHandle::push_layer` at boot. The K.2.4 translation
/// pass handles that uniformly now; no per-mode host glue.
fn multibuffer_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "]e",
                doc: "Jump to next excerpt",
                cmd: "multibuffer.next-excerpt-start"
            },
            keymap_entry! {
                mode: Normal, chord: "[e",
                doc: "Jump to previous excerpt",
                cmd: "multibuffer.prev-excerpt-start"
            },
            keymap_entry! {
                mode: Normal, chord: "]E",
                doc: "Jump to next file boundary",
                cmd: "multibuffer.next-file-boundary"
            },
            keymap_entry! {
                mode: Normal, chord: "[E",
                doc: "Jump to previous file boundary",
                cmd: "multibuffer.prev-file-boundary"
            },
        ]
    })
}

/// Major mode for buffers of [`BufferKind::Multibuffer`]. Generic;
/// knows nothing about *why* excerpts exist. Provider-specific
/// behaviour (project-search, lsp-references, etc.) is layered as
/// minor modes registered by each provider's own
/// `register_<provider>` helper.
pub struct MultibufferMode;

impl MultibufferMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("multibuffer-mode")
    }
}

impl Mode for MultibufferMode {
    type Guard = MultibufferModeGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// H.2 (2026-05-31) + M.2.b.2 (2026-06-01): buffers whose
    /// `BufferKind` is `Multibuffer` dispatch to this major via
    /// `ModeRegistry::find_major_for_kind`.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Multibuffer)
    }

    fn options(&self) -> OptionOverrideSet {
        // M.3 (2026-06-01): `ReadOnly` dropped from the major's
        // contribution now that edit propagation lands. Providers
        // that want a read-only view (e.g. read-only LSP-references
        // view) layer a minor mode that contributes `ReadOnly = true`.
        // `NoFile` stays because multibuffers aren't on-disk files;
        // `:w` is a no-op until a provider attaches save semantics
        // (M.6 SearchProvider's "save all sources" wrapper, etc.).
        overrides! {
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// K.2.5 (2026-06-02): excerpt-jump chord bindings.
    /// `]e` / `[e` (next / previous excerpt) and `]E` / `[E`
    /// (next / previous file boundary). Resolved at host
    /// translation time via `CommandRegistry` against the
    /// canonical motion names registered by
    /// `register_multibuffer_motions`.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(multibuffer_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, MultibufferModeGuard> {
        Box::pin(async move {
            // lattice-host converts lattice_core::BufferId → lattice_protocol::ids::BufferId
            // via `new(id.0 as u64)`; invert here so we can key into MultibufferRegistry.
            let core_buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);

            // T.7 (2026-06-18): the mode OWNS its excerpt-header theme
            // elements + their defaults — register them here so the
            // mode is the single source of the element vocabulary
            // ([[feedback_mode_owns_its_surface]]). Idempotent by name,
            // so re-activation is safe; `create_multibuffer_view` also
            // registers (before building the provider) to capture the
            // ids — both paths hit the same interned ids. Missing
            // service (test harness) just skips: the provider then
            // renders with no baked bg/fg.
            if let Some(theme) = ctx
                .service::<ThemeRegistryHandle>()
                .map(|outer| (*outer).clone())
            {
                let owner = ElementOwner::Mode(Self::mode_id().as_str().to_string().into());
                let _ = crate::register_multibuffer_theme_elements(theme.as_ref(), owner);
            }

            // Both handle types are `Arc<dyn Trait>` aliases.
            // `ctx.service::<T>()` returns `Option<Arc<T>>` which is
            // `Option<Arc<Arc<dyn Trait>>>` — clone through the outer
            // Arc to obtain the inner handle.
            let fold_service = ctx
                .service::<FoldOverlayServiceHandle>()
                .map(|outer| (*outer).clone());
            let mb_registry = ctx
                .service::<MultibufferRegistryHandle>()
                .map(|outer| (*outer).clone());

            let mut fold_registrations = Vec::new();

            match (fold_service, mb_registry) {
                (Some(svc), Some(reg)) => {
                    match reg.handle(core_buffer_id) {
                        Some(mb_handle) => {
                            // M.7: one fold per excerpt.
                            let excerpt_provider = Arc::new(crate::ExcerptFoldProvider::new(
                                (*mb_handle).clone(),
                                core_buffer_id,
                            ));
                            let excerpt_id = svc.add_source(excerpt_provider, core_buffer_id);
                            fold_registrations.push((svc.clone(), excerpt_id));

                            // M.8: one fold per source file (union of that file's excerpts).
                            let file_provider = Arc::new(crate::FileBoundaryFoldProvider::new(
                                (*mb_handle).clone(),
                                core_buffer_id,
                            ));
                            let file_id = svc.add_source(file_provider, core_buffer_id);
                            fold_registrations.push((svc, file_id));
                        }
                        None => {
                            tracing::debug!(
                                "MultibufferMode::on_activate: no handle for buffer {:?}; \
                                 excerpt + file-boundary folds inactive",
                                core_buffer_id
                            );
                        }
                    }
                }
                _ => {
                    tracing::debug!(
                        "MultibufferMode::on_activate: fold service or multibuffer \
                         registry not registered; excerpt folds inactive (expected in tests)"
                    );
                }
            }

            Ok(MultibufferModeGuard { fold_registrations })
        })
    }
}

/// Boot wiring entry point. Called once from `lattice-host`'s
/// `editor_boot::boot` after the `ServiceRegistry` is populated
/// with [`MultibufferRegistryHandle`] and the `EventBus` exists.
///
/// 1. Registers [`MultibufferMode`] against `registry` (so
///    `ModeRegistry::find_major_for_kind(BufferKind::Multibuffer)`
///    returns its id post-H.2).
/// 2. Subscribes a `DocumentClosed` cleanup task that removes a
///    closed multibuffer's entry from `multibuffer_registry`.
///    Uses the existing `SubscriptionTarget::Channel` shape +
///    `tokio::spawn` for the drain loop (same pattern as the
///    LSP / mode-lifecycle drains).
pub fn register_multibuffer_modes(
    registry: &mut ModeRegistry,
    events: &Arc<EventBus>,
    multibuffer_registry: MultibufferRegistryHandle,
) {
    registry
        .register(MultibufferMode)
        .expect("multibuffer-mode registers without conflict at boot");

    // Cleanup subscriber: only wire when a tokio runtime is in
    // scope. Production boot runs inside the App's runtime so
    // this fires; tests that construct `Editor` outside a runtime
    // (`lattice-host` lib tests) gracefully skip the subscriber
    // wiring — the registry simply leaks entries for the
    // (short-lived) test process, which is observably fine
    // because no test asserts cleanup behaviour.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::debug!(
            "register_multibuffer_modes: no tokio runtime in scope; \
             skipping DocumentClosed cleanup subscriber wiring \
             (expected in test paths)"
        );
        return;
    };

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    events.subscribe(
        EventFilter::kind(EventKind::DocumentClosed),
        SubscriptionTarget::Channel(tx),
    );

    let reg = multibuffer_registry;
    handle.spawn(async move {
        while let Some(event) = rx.recv().await {
            if let Event::DocumentClosed { id } = event {
                reg.remove_by_document_id(id);
            }
        }
    });
}

/// K.2.5 (2026-06-02): register `:multibuffer-expand [n]` and
/// `:multibuffer-contract [n]` ex-commands.
///
/// Relocated from `crates/lattice-host/src/multibuffer_keymap.rs::register_multibuffer_ex_commands`
/// as part of the K.2.5 migration that moves multibuffer keymap +
/// ex-command registration into its owning crate. Boot path in
/// `editor_boot.rs` now calls this directly instead of the host
/// glue. Behaviour preserved verbatim:
///
/// Both commands take an optional non-negative integer (default 5
/// — Zed precedent). `apply` produces
/// `Effect::AppAction(AppEffect::MultibufferExpand { delta })`
/// where `delta` is positive for expand, negative for contract.
/// The host's apply_effect arm calls the substrate helper
/// `multibuffer_expand_excerpt_at` (M.10.4, 2026-06-03), which
/// looks up the active view via `MultibufferRegistry` and calls
/// `expand_excerpt_at` at the active cursor's row.
///
/// No-op when invoked on a non-multibuffer active buffer (no
/// registry entry for the buffer id).
pub fn register_multibuffer_ex_commands(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::args::{ArgSpec, Args};
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::error::CommandError;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};

    fn parse_optional_count(s: &str, _bang: bool) -> Result<Args, CommandError> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Ok(Args::None);
        }
        match trimmed.parse::<u32>() {
            // Stash as the decimal string so the apply closure
            // can re-parse without re-validating; production code
            // typically passes 1-2 digit counts.
            Ok(_) => Ok(Args::String(trimmed.to_string())),
            Err(_) => Err(CommandError::BadArgs(format!(
                "expected non-negative integer, got `{trimmed}`"
            ))),
        }
    }

    fn count_from_args(args: &Args) -> i32 {
        match args {
            Args::String(s) => s.parse::<i32>().unwrap_or(5),
            _ => 5,
        }
    }

    registry.register_ex_command(
        "multibuffer-expand",
        "Expand context around the excerpt under the cursor by N rows (default 5).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_count),
            apply: Box::new(|ctx| {
                let delta = count_from_args(&ctx.args);
                Ok(Effect::AppAction(AppEffect::MultibufferExpand { delta }))
            }),
            args_schema: Vec::<ArgSpec>::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );

    registry.register_ex_command(
        "multibuffer-contract",
        "Contract the excerpt under the cursor by N rows (default 5).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(parse_optional_count),
            apply: Box::new(|ctx| {
                let delta = -count_from_args(&ctx.args);
                Ok(Effect::AppAction(AppEffect::MultibufferExpand { delta }))
            }),
            args_schema: Vec::<ArgSpec>::new(),
            surface_form: SurfaceForm::Keyword,
        },
    );
}
