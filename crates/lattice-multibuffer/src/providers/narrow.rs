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
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
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
    registry: CommandRegistryHandle,
    lang_registry: Option<Arc<LangRegistry>>,
) -> BufferId {
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    sources.insert(source_id, source_handle);
    let excerpt = Excerpt::new(source_id, start_line, end_line);

    let name = if label.is_empty() {
        format!("*narrow:L{}–{}*", start_line + 1, end_line + 1)
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
        "Narrow the editing surface to a region: `:narrow` (the cursor's \
         paragraph) or `:{start},{end}narrow`. The region opens as a focused, \
         editable view; edits propagate to the source file. `:widen` restores \
         the full buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: true,
            parse_args: Arc::new(|_s: &str, _bang: bool| Ok(Args::None)),
            apply: Arc::new(|ctx| {
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
            parse_args: Arc::new(|_s: &str, _bang: bool| Ok(Args::None)),
            apply: Arc::new(|_ctx| Ok(Effect::AppAction(AppEffect::NarrowWiden))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}

/// N.1.3 (2026-06-10): register the **`zn` narrow operator** into the
/// grammar's `CommandRegistry` and return its `OperatorId`.
///
/// The operator is *universal* — you narrow from any editable buffer,
/// like `d` / `y` / `c`. Per the mode-ownership split, this crate owns
/// the operator SPEC + `apply`; the host wires the `zn` chord to the
/// returned `OperatorId` at the universal operator-pending layer
/// (operator-pending composition needs the host-resolved `Builtins`,
/// which this crate can't reach). The narrow-VIEW surface
/// (`:widen` / `:w` / `q`) stays with `NarrowMinorMode`.
///
/// `apply` reads the resolved `OperatorContext.range` (the span the
/// following motion / text object produced), converts it to an
/// inclusive whole-line span, and emits
/// `Effect::AppAction(AppEffect::NarrowLines { .. })`; the host arm
/// narrows the active buffer to that span via `create_narrow_view`.
pub fn register_narrow_operator(registry: &mut CommandRegistry) -> lattice_grammar::OperatorId {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::{OperatorContext, OperatorSpec};

    registry.register_operator(
        "operator:narrow",
        "Narrow the editing surface to the {motion}/{text-object} that follows \
         `zn` — a focused, editable view of that region. `znn` narrows the \
         current line; `znip` a paragraph; `znaf` a function. `:widen` restores.",
        OperatorSpec {
            repeatable: false,
            blockwise_per_row: false,
            args_schema: vec![],
            apply: Arc::new(|ctx: &mut OperatorContext| {
                let (start_line, end_line) = range_to_narrow_lines(
                    ctx.range.start.line,
                    ctx.range.start.byte,
                    ctx.range.end.line,
                    ctx.range.end.byte,
                );
                Ok(Effect::AppAction(AppEffect::NarrowLines {
                    start_line,
                    end_line,
                }))
            }),
        },
    )
}

/// Map an operator's resolved range (start/end `line`+`byte`) to an
/// inclusive whole-line span for narrowing. A half-open end at column
/// 0 means the last covered line is the previous one — the common shape
/// for *forward* linewise / paragraph motions (`znip`, `znj`, `znG`),
/// which is what this heuristic is tuned for.
///
/// Known v1 edge (review-flagged): a *backward* motion (`znk`, `zn{`)
/// whose cursor — the higher endpoint after ordering — lands at column 0
/// drops the cursor line, because given only `(start, end)` the function
/// can't distinguish the cursor anchor from a half-open motion end.
/// Backward narrows are rare; if they matter, the operator should thread
/// the anchor through explicitly. `range_to_lines_reversed_is_ordered`
/// pins the current behaviour.
fn range_to_narrow_lines(
    start_line: u32,
    start_byte: u32,
    end_line: u32,
    end_byte: u32,
) -> (u32, u32) {
    let ((lo_line, _lo_byte), (hi_line, hi_byte)) = if start_line <= end_line {
        ((start_line, start_byte), (end_line, end_byte))
    } else {
        ((end_line, end_byte), (start_line, start_byte))
    };
    let mut end = hi_line;
    if hi_byte == 0 && end > lo_line {
        end -= 1;
    }
    (lo_line, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_to_lines_mid_line_end_is_inclusive() {
        // `znj`-like: cursor line + 1, end mid-line → covers both lines.
        assert_eq!(range_to_narrow_lines(0, 0, 3, 5), (0, 3));
    }

    #[test]
    fn range_to_lines_half_open_end_at_col0_drops_trailing_line() {
        // Linewise / paragraph motions end at column 0 of the line
        // AFTER the last content line → last covered line is the prev.
        assert_eq!(range_to_narrow_lines(0, 0, 3, 0), (0, 2));
    }

    #[test]
    fn range_to_lines_single_line() {
        assert_eq!(range_to_narrow_lines(2, 0, 2, 4), (2, 2));
    }

    #[test]
    fn range_to_lines_reversed_is_ordered() {
        // Backwards motion (e.g. `znk`): end before start.
        assert_eq!(range_to_narrow_lines(5, 0, 2, 0), (2, 4));
    }

    #[test]
    fn register_narrow_operator_registers_the_operator() {
        let mut registry = CommandRegistry::new();
        let _op = register_narrow_operator(&mut registry);
        assert!(registry.id_by_name("operator:narrow").is_some());
    }
}
