//! N.1.1 (2026-06-10): **narrow mode** — a focused, editable
//! one-excerpt multibuffer view of a region of a source buffer.
//!
//! Design: `docs/dev/architecture/narrow-mode.md`. Slice plan:
//! `docs/dev/operations/slice-plans/narrow-mode.md` (N.1.1).
//!
//! Narrow is a *one-excerpt* multibuffer + a marker minor mode.
//! There is no `BufferKind::Narrow` — the view is a regular
//! `BufferKind::Multibuffer` and reuses every M-series primitive
//! (M.3 edit propagation, M.4 live source updates, K.4.7 per-excerpt
//! syntax). Edits in the narrow view propagate to the source buffer;
//! `:widen` closes the view, leaving the source (with its edits)
//! open.
//!
//! Unlike `providers::search`, narrow is **not** feature-gated — it's
//! a first-class built-in primitive — and it carries no async scan,
//! no service, and no per-view state beyond the excerpt itself.
//!
//! Entry is the `:narrow [{range}]` ex-command (and, from N.1.3, the
//! `zn` operator). The ex-command emits `AppEffect::NarrowTrigger`;
//! the host arm resolves the range, fetches the active buffer's
//! handle, and calls [`create_narrow_view`].

use std::collections::HashMap;
use std::sync::Arc;

use lattice_config::OptionOverrideSet;
use lattice_core::{BufferFlags, BufferId};
use lattice_grammar::CommandRegistry;
use lattice_mode::{
    CapabilitySet, Keymap, LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};
use lattice_runtime::Document;
use lattice_syntax::LangRegistry;

use crate::registry::MultibufferRegistryHandle;
use crate::view::create_multibuffer_view;
use crate::{Excerpt, HeaderlineStatus};

// ─────────────────────────────────────────────────────────────────
// NarrowMinorMode — identity marker for narrow views
// ─────────────────────────────────────────────────────────────────

/// `narrow-minor-mode` — the provider-minor activated on a narrow
/// view. In N.1.1 it is a pure identity marker: a multibuffer with
/// this minor active IS a narrow view (distinguished from search /
/// diff multibuffers), which the host's `:widen` guard and N.1.5's
/// stacking logic read.
///
/// N.1.1.b adds the in-view surface (`q` → widen chord, `:w`
/// source-save override) here; for now `on_activate` is a no-op so
/// the marker is cheap.
pub struct NarrowMinorMode;

impl NarrowMinorMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("narrow-minor-mode")
    }
}

/// RAII guard for `NarrowMinorMode`. Unit in N.1.1 (no
/// subscriptions / action handlers yet); becomes a `Vec` of
/// `ActionHandlerRegistration` when the `q` / `:w` surface lands.
pub struct NarrowMinorModeGuard;

impl Mode for NarrowMinorMode {
    type Guard = NarrowMinorModeGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // Narrow views are EDITABLE — edits propagate to the source
        // via M.3. No ReadOnly override.
        OptionOverrideSet::new()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        // N.1.1: no contributed chords yet (the `q` → widen binding
        // lands in N.1.1.b once `action:narrow-widen` is registered).
        Keymap::default()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(NarrowMinorModeGuard) })
    }
}

// ─────────────────────────────────────────────────────────────────
// create_narrow_view — the shared sink
// ─────────────────────────────────────────────────────────────────

/// Allocate a one-excerpt multibuffer view over
/// `[start_line, end_line]` (inclusive, 0-based) of `source_id` and
/// activate [`NarrowMinorMode`] on it. Returns the new view's
/// `BufferId`.
///
/// `source_handle` is the *existing* source buffer's document handle
/// (an `Arc` clone) — the narrow view shares it, so edits in the
/// narrow view and the original pane stay live-synced (M.4). `label`
/// is shown in the headerline (`"[narrow] <label> L<start+1>–<end+1>"`);
/// pass the file name or a symbol name when known, else `""`.
///
/// Every entry surface (`:narrow`, Visual, the `zn` operator) funnels
/// through here.
pub fn create_narrow_view(
    activator: &mut dyn ModeActivator,
    source_id: BufferId,
    source_handle: Arc<dyn Document>,
    start_line: u32,
    end_line: u32,
    label: &str,
    registry: Arc<CommandRegistry>,
    lang_registry: Option<Arc<LangRegistry>>,
) -> BufferId {
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(source_id, source_handle);
    let excerpt = Excerpt::new(source_id, start_line, end_line);

    let name = if label.is_empty() {
        format!("*narrow:L{}-{}*", start_line + 1, end_line + 1)
    } else {
        format!("*narrow:{label}*")
    };

    let view_id = create_multibuffer_view(
        activator,
        sources,
        vec![excerpt],
        Some(name),
        BufferFlags::default(),
        registry,
        lang_registry,
    );

    // Set the sticky headerline. Narrow is instantaneous — no
    // InProgress phase, straight to Complete.
    if let Some(mb_reg) = activator.services().get::<MultibufferRegistryHandle>() {
        if let Some(view) = mb_reg.handle(view_id) {
            let summary = if label.is_empty() {
                format!("[narrow] L{}–{}", start_line + 1, end_line + 1)
            } else {
                format!("[narrow] {label} L{}–{}", start_line + 1, end_line + 1)
            };
            view.set_headerline(HeaderlineStatus::Complete { summary });
        }
    }

    activator.activate_minor_by_id(view_id, NarrowMinorMode::mode_id());
    view_id
}

// ─────────────────────────────────────────────────────────────────
// Boot integration
// ─────────────────────────────────────────────────────────────────

/// Boot helper — register the narrow provider-minor mode. Called
/// from `lattice-host::editor_boot` alongside the other multibuffer
/// mode registrations.
pub fn register_narrow_mode(mode_registry: &mut ModeRegistry) {
    mode_registry
        .register(NarrowMinorMode)
        .expect("narrow-minor-mode registers without conflict at boot");
}

/// Boot helper — register the `:narrow` + `:widen` ex-commands.
///
/// `:narrow` accepts an optional `{start},{end}` line range
/// (`accepts_range`). The host resolves the range against the active
/// document (the apply context has no document), so the apply just
/// forwards the raw [`lattice_grammar::range::Range`] through
/// `AppEffect::NarrowTrigger`. `:widen` emits `AppEffect::NarrowWiden`,
/// which the host guards to narrow views before closing.
pub fn register_narrow_ex_commands(registry: &mut CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::args::Args;
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};

    registry.register_ex_command(
        "narrow",
        "Narrow the editing surface to a region: `:narrow` (current line) or \
         `:{start},{end}narrow`. The region opens as a focused, editable view; \
         edits propagate to the source file. `:widen` restores the full buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: true,
            parse_args: Box::new(|_s: &str, _bang: bool| Ok(Args::None)),
            apply: Box::new(|ctx| {
                Ok(Effect::AppAction(AppEffect::NarrowTrigger {
                    range: ctx.range.clone(),
                }))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    registry.register_ex_command(
        "widen",
        "Close the active narrow view, restoring the full source buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Box::new(|_s: &str, _bang: bool| Ok(Args::None)),
            apply: Box::new(|_ctx| Ok(Effect::AppAction(AppEffect::NarrowWiden))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
