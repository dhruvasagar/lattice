//! `diff-mode` minor mode + the host-side bridge that toggles
//! it on participating buffers as `DiffSession`s open and close.
//!
//! ## The invariant
//!
//! A buffer participates in any [`DiffSession`] ⟺ `diff-mode`
//! is active on that buffer. The toggle lives inside the
//! [`DiffSubsystem`] (`register_with_sources` /
//! `drop_session`) so every present and future `:diff*`
//! ex-command (`:diff <buf>`, `:diffthis`, `:diffsplit`,
//! future `:Gdiff` D.7, future `:diff-accept` / `:diff-reject`
//! D.6) and the doc-close auto-drop
//! (`note_buffer_closed → drop_session`) inherit the toggle
//! without each call site repeating the activation logic.
//! See `docs/dev/architecture/diff-system.md` §3.4.7.
//!
//! ## Why a bridge instead of a direct call
//!
//! The subsystem holds an `Arc<DiffSubsystem>` and may be
//! invoked from a tokio worker (the bus-subscription drainer
//! that translates `DocumentClosed` events into
//! `drop_session`). The mode-activation API
//! (`ModeRegistry::activate_minor` / `deactivate_minor`)
//! needs `&mut ActiveModes` per buffer plus references to
//! mode-guards, config, event bus, and services — all of which
//! live on `Editor` and aren't shareable across threads.
//!
//! The bridge resolves this by queueing mode changes from any
//! thread into a `Mutex<Vec<DiffModeChange>>` and exposing a
//! `drain_pending` call the editor invokes from the dispatch
//! tail (`publish_render_state`). The dispatch tail applies
//! each change using the existing `mode_registry` pattern.
//!
//! ## Why ref-counted per buffer
//!
//! v1's `:diff*` flows don't intentionally put the same
//! buffer in two sessions, but `lookup_session_for` indexes
//! only the primary side. A baseline-side buffer can therefore
//! be staged into a second session without the existing
//! rejection check catching it; D.6 three-way will plausibly
//! see a `MergeBase` participant also diffed against working
//! tree. Naive "session opens → mode on / session closes →
//! mode off" would flip diff-mode off prematurely when the
//! first of two sessions sharing a buffer closes.
//!
//! The bridge therefore keeps a per-buffer refcount keyed by
//! the participating session keys. Activation appends the
//! session key to the buffer's bucket; deactivation removes
//! it; the bridge enqueues a `Deactivate` change only when
//! the bucket empties.

use lattice_core::{BufferId, FoldOverlayServiceHandle, ProviderId};
use lattice_grammar::Effect;
use lattice_mode::registry::ModeRegistry;
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, DecorationCtx, ElementContent,
    ElementId, GutterDecoration, GutterDiffKind, Keymap, KeymapEntry, LifecycleFuture, Mode,
    ModeContext, ModeId, ModeKind, ModelineElement, ModelineRole, ModelineService, Scope, Zone,
    keymap_entry,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use crate::fold::HunkFoldSource;
use crate::subsystem::DiffSubsystemHandle;

// ──────────────────────────────────────────────────────────────
// DiffMode — the minor mode itself
// ──────────────────────────────────────────────────────────────

/// Render-time snapshot for `DiffMode::gutter_decorations`. Carries
/// the active buffer's diff sign map; injected by the renderer only for
/// the active-document buffer (sign_map is active-doc only).
pub struct DiffDecorationData {
    pub sign_map: std::sync::Arc<crate::overlay::DiffSignMap>,
}

// ──────────────────────────────────────────────────────────────
// Modeline element (ML.3): the `+N ~M` diff-stats segment
// ──────────────────────────────────────────────────────────────

/// Modeline element id for the diff add/change summary. Owned by the
/// diff subsystem (`feedback_mode_owns_its_surface`).
pub const DIFF_ELEMENT: &str = "diff";

/// Register the `diff` modeline descriptor (ML.3). `Scope::Global`: the
/// active document's session sign-map is a single shared value, so the
/// summary renders on the **active** pane's modeline (the renderer gates
/// Global elements to the active pane, modeline.md §7) — there is no
/// distinct per-side count to show. Content is computed host-side on the
/// actor (where the sign map already lives, `sync_diff_modeline_element`)
/// rather than counted on the render thread (paramount #1). Left zone,
/// after `core.path`.
pub fn register_diff_modeline_element(svc: &ModelineService) {
    svc.register(
        ModelineElement::new(ElementId::new(DIFF_ELEMENT), Zone::Left, 20).with_scope(Scope::Global),
    );
}

/// Format a diff sign-map into the `+N ~M` modeline content (ML.3). The
/// formatter the diff subsystem owns — moved here from the retired
/// `DiffMode::status_line_items`. Empty content (no adds/changes) ⇒
/// hidden this frame (the caller's `apply` clears the slot).
pub fn diff_content(sign_map: &crate::overlay::DiffSignMap) -> ElementContent {
    use crate::overlay::DiffSignKind;
    let mut added = 0u32;
    let mut changed = 0u32;
    for (_line, kind) in sign_map.entries() {
        match kind {
            DiffSignKind::Add => added += 1,
            DiffSignKind::Change | DiffSignKind::Conflict | DiffSignKind::Remove => {
                changed += 1;
            }
        }
    }
    if added == 0 && changed == 0 {
        return ElementContent::default();
    }
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("+{added}"));
    }
    if changed > 0 {
        parts.push(format!("~{changed}"));
    }
    ElementContent::text(parts.join(" "), ModelineRole::new(lattice_mode::modeline::ROLE_MODE_ITEM))
}

/// `diff-mode` minor. Empty marker in v1 (D.5.a): the bit other
/// layers consult. D.5.b/c land the `do`/`dp` chord bindings
/// against the `MinorMode(ModeId::new("diff-mode"))` keymap
/// layer (K.1.b shape).
pub struct DiffMode;

impl DiffMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("diff-mode")
    }
}

/// DX.3-C7 (2026-06-24): deregisters the buffer's [`HunkFoldSource`] when
/// `diff-mode` deactivates (the diff session closed, or the buffer's mode
/// set changed). `Drop` fires from the mode guard store when
/// `deactivate_minor` runs — the same Drop-based lifecycle multibuffer's
/// `MultibufferModeGuard` uses for its excerpt/file-boundary fold sources.
/// Empty when the fold service / diff subsystem weren't registered (some
/// test harnesses) — Drop is then a no-op.
pub struct DiffModeGuard {
    fold_registrations: Vec<(FoldOverlayServiceHandle, ProviderId)>,
}

impl Drop for DiffModeGuard {
    fn drop(&mut self) {
        for (svc, id) in self.fold_registrations.drain(..) {
            svc.remove_source(id);
        }
    }
}

impl Mode for DiffMode {
    type Guard = DiffModeGuard;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    /// MO.x (2026-06-24): contribute the `do`/`dp` chords through the
    /// standard `Mode::keymap()` seam (the multibuffer `]e`/`[e` pattern).
    /// The host's K.2.4 translation pass resolves each entry's `cmd` name
    /// against the `CommandRegistry` and pushes the layer under
    /// `MinorMode(diff-mode)`, gated to diff-active buffers by K.1.c —
    /// retiring the host's bespoke explicit `push_layer` (the emacs-keys
    /// host-push that DX.5/DX.7 left as this follow-up). The binding choice
    /// now lives wholly with the mode; the host pushes nothing diff-specific.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(diff_mode_keymap_entries())
    }
    /// CR.1 (2026-06-24): the `do`/`dp` chord handler bodies, now
    /// mode-owned (they were `Editor::do_diff_get`/`do_diff_put` +
    /// `Action::DiffGet`/`DiffPut` host-side). Bound globally
    /// (buffer-agnostic) at boot by the host's `register_mode_action_handlers`
    /// walk: `diff-mode` is active on many buffers at once and the
    /// `ActionHandlerRegistry` is keyed by `CommandId` alone, so a
    /// per-`on_activate` registration would let one buffer's deactivation
    /// evict the handler for the others — the snippet `action:snippet-expand`
    /// precedent. Each handler reads the active buffer + cursor from the
    /// `ActionContext` and the diff session from the `DiffSubsystemHandle`
    /// service, resolves the hunk via `DiffSubsystem::diff_*_effect`, and
    /// hands the host an `Effect::ApplyEdit` (CR.0): the mode owns the
    /// resolution, the host owns the apply. `do`/`dp` pass `None` as the
    /// target (auto-resolve the peer in 2-way; "target required" in 3-way);
    /// the `:diffget`/`:diffput` optional `<bufnr>` is a separate host call
    /// into the same `diff_*_effect` resolver.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        let get: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let sub = ctx.services.get::<DiffSubsystemHandle>()?;
            let active = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            sub.diff_get_effect(active, ctx.cursor.line, None)
        });
        let put: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let sub = ctx.services.get::<DiffSubsystemHandle>()?;
            let active = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            sub.diff_put_effect(active, ctx.cursor.line, None)
        });
        // CR.6 (2026-06-24): `]c`/`[c` hunk-nav, now mode-owned (was the
        // host's `do_next_hunk`/`do_prev_hunk`). The resolvers return a
        // generic `Effect::SelectionChange`; the host owns the cursor write.
        let next_hunk: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let sub = ctx.services.get::<DiffSubsystemHandle>()?;
            let active = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            sub.diff_next_hunk_effect(active, ctx.cursor.line)
        });
        let prev_hunk: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let sub = ctx.services.get::<DiffSubsystemHandle>()?;
            let active = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            sub.diff_prev_hunk_effect(active, ctx.cursor.line)
        });
        vec![
            ActionHandlerContribution { action_name: "action:diff-get", handler: get },
            ActionHandlerContribution { action_name: "action:diff-put", handler: put },
            ActionHandlerContribution { action_name: "action:hunk-next", handler: next_hunk },
            ActionHandlerContribution { action_name: "action:hunk-prev", handler: prev_hunk },
        ]
    }
    // ML.3: the `+N ~M` summary moved off `status_line_items` to a
    // registered modeline element (`register_diff_modeline_element` /
    // `diff_content`), pushed via the actor's `sync_diff_modeline_element`.
    fn gutter_decorations(&self, ctx: &DecorationCtx<'_>) -> Vec<GutterDecoration> {
        use crate::overlay::DiffSignKind;
        let Some(data) = ctx.service::<DiffDecorationData>() else {
            return Vec::new();
        };
        data.sign_map
            .entries()
            .iter()
            .map(|(line, kind)| {
                let gdk = match kind {
                    DiffSignKind::Add => GutterDiffKind::Add,
                    DiffSignKind::Remove => GutterDiffKind::Remove,
                    DiffSignKind::Change => GutterDiffKind::Change,
                    DiffSignKind::Conflict => GutterDiffKind::Conflict,
                };
                GutterDecoration::Diff { line: *line, kind: gdk }
            })
            .collect()
    }

    /// DX.3-C7 (2026-06-24): register the buffer's hunk-fold source.
    ///
    /// Mirrors `MultibufferMode::on_activate`: pull the
    /// `FoldOverlayService` + the data handle (here the
    /// `DiffSubsystemHandle`) from the service registry, look up the diff
    /// session for this buffer, build a [`HunkFoldSource`] over it, and
    /// register it. The returned [`DiffModeGuard`]'s `Drop` removes it
    /// when the mode deactivates. Missing service / no session just skips
    /// (returns an empty guard) — folds are inactive, never a panic.
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, DiffModeGuard> {
        Box::pin(async move {
            // lattice-host maps lattice_core::BufferId →
            // lattice_protocol::ids::BufferId via `new(id.0 as u64)`;
            // invert here to key into the diff subsystem.
            let core_buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);

            // Both handle types are `Arc<dyn Trait>` / `Arc<T>` aliases;
            // `ctx.service::<T>()` returns `Option<Arc<T>>`, so clone
            // through the outer Arc to obtain the inner handle.
            let fold_service = ctx
                .service::<FoldOverlayServiceHandle>()
                .map(|outer| (*outer).clone());
            let diff_subsystem = ctx
                .service::<DiffSubsystemHandle>()
                .map(|outer| (*outer).clone());

            let mut fold_registrations = Vec::new();

            match (fold_service, diff_subsystem) {
                (Some(svc), Some(sub)) => match sub.lookup(core_buffer_id) {
                    Some(session) => {
                        let source = Arc::new(HunkFoldSource::new(session, core_buffer_id));
                        let id = svc.add_source(source, core_buffer_id);
                        fold_registrations.push((svc, id));
                    }
                    None => {
                        tracing::debug!(
                            "DiffMode::on_activate: no diff session for buffer {:?}; \
                             hunk folds inactive",
                            core_buffer_id
                        );
                    }
                },
                _ => {
                    tracing::debug!(
                        "DiffMode::on_activate: fold service or diff subsystem not \
                         registered; hunk folds inactive (expected in some tests)"
                    );
                }
            }

            Ok(DiffModeGuard { fold_registrations })
        })
    }
}

// ──────────────────────────────────────────────────────────────
// DiffConflictMode (DX.8) — smerge-style conflict-resolution surface
// ──────────────────────────────────────────────────────────────

/// `diff-conflict-mode` (smerge-style) minor — DX.8 shell (2026-06-24).
///
/// Separates conflict *resolution* from the 2-way `diff-mode` surface
/// (design `diff-extraction.md` §4): it should activate **only** on
/// buffers whose diff session carries conflict regions
/// ([`crate::overlay::DiffSignKind::Conflict`]), and will contribute the
/// conflict-resolution chords (keep-ours / keep-theirs / keep-both /
/// next-conflict) + a conflict gutter.
///
/// v1 is a deliberate marker **shell**: the activation predicate is
/// [`sign_map_has_conflicts`]; the resolution chords and the
/// bridge-driven activation wiring are a tracked follow-up. Conflict
/// *resolution* actions don't exist yet, so DX.8 establishes the
/// separately-activatable surface **without inventing behaviour** — the
/// decomposition (conflict resolution ≠ 2-way diffing) is the win, not a
/// premature chord set. `Guard = ()`: it allocates no per-buffer
/// resources until the resolution chords land.
pub struct DiffConflictMode;

impl DiffConflictMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("diff-conflict-mode")
    }
}

impl Mode for DiffConflictMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    /// CR.3 (2026-06-24): the conflict-resolution chords, contributed
    /// through the standard `Mode::keymap()` seam (the `diff-mode`
    /// `do`/`dp` pattern). Vim-fugitive 3-way family — `d2o`/`d3o`
    /// (keep-ours / keep-theirs), `d2p`/`d3p` (put-ours / put-theirs),
    /// `dB` (keep-both). The host's K.2.4 translate pass resolves each
    /// `cmd` name and pushes the layer under
    /// `MinorMode(diff-conflict-mode)`, gated by K.1.c to
    /// conflict-mode-active buffers.
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(diff_conflict_mode_keymap_entries())
    }
    /// CR.3: the conflict-chord handler bodies, mode-owned. Bound globally
    /// at boot by `register_mode_action_handlers` (same rationale as
    /// [`DiffMode::action_handlers`]). Each reads the active buffer +
    /// cursor from the `ActionContext` and the `DiffSubsystemHandle`
    /// service and calls the matching `DiffSubsystem::diff_*_effect`
    /// resolver, which resolves "theirs" from the session and returns
    /// `Effect::ApplyEdit` — or, for the degenerate keep-ours / put-ours
    /// self-targets, an informative `Echo` (Dhruva 2026-06-24).
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        macro_rules! conflict_handler {
            ($method:ident) => {{
                let h: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
                    let sub = ctx.services.get::<DiffSubsystemHandle>()?;
                    let active = lattice_core::BufferId(ctx.buffer_id.0 as u32);
                    sub.$method(active, ctx.cursor.line)
                });
                h
            }};
        }
        vec![
            ActionHandlerContribution {
                action_name: "action:diff-keep-ours",
                handler: conflict_handler!(diff_keep_ours_effect),
            },
            ActionHandlerContribution {
                action_name: "action:diff-keep-theirs",
                handler: conflict_handler!(diff_keep_theirs_effect),
            },
            ActionHandlerContribution {
                action_name: "action:diff-put-ours",
                handler: conflict_handler!(diff_put_ours_effect),
            },
            ActionHandlerContribution {
                action_name: "action:diff-put-theirs",
                handler: conflict_handler!(diff_put_theirs_effect),
            },
            ActionHandlerContribution {
                action_name: "action:diff-keep-both",
                handler: conflict_handler!(diff_keep_both_effect),
            },
        ]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// DX.8 (2026-06-24): the `diff-conflict-mode` activation predicate.
///
/// A diff session warrants conflict resolution iff its published sign map
/// carries at least one [`crate::overlay::DiffSignKind::Conflict`] region.
/// A pure function over the sign map — trivially testable and safe to
/// consult anywhere — so the bridge can gate `diff-conflict-mode`
/// activation on it (follow-up) the same way `DiffModeBridge` gates
/// `diff-mode` on session participation.
pub fn sign_map_has_conflicts(sign_map: &crate::overlay::DiffSignMap) -> bool {
    use crate::overlay::DiffSignKind;
    sign_map
        .entries()
        .iter()
        .any(|(_line, kind)| matches!(kind, DiffSignKind::Conflict))
}

/// Register the diff modes against `registry`. Called from
/// [`crate::install::install`] (the Phase-B install list) alongside the
/// other feature-crate `register_*_modes` helpers. Registers both
/// `diff-mode` (the 2-way base surface) and the `diff-conflict-mode`
/// shell (DX.8).
pub fn register_diff_modes(registry: &mut ModeRegistry) {
    registry
        .register(DiffMode)
        .expect("diff-mode must register without conflict");
    registry
        .register(DiffConflictMode)
        .expect("diff-conflict-mode must register without conflict");
}

/// MO.x (2026-06-24): the `diff-mode` keymap entries, contributed through
/// `DiffMode::keymap()`. Each entry carries a canonical `action:diff-*`
/// **name**; the host's K.2.4 translate pass (`translate_mode_keymaps`)
/// resolves it against the `CommandRegistry` and pushes the layer under
/// `MinorMode(diff-mode)` — the multibuffer `multibuffer_keymap_entries`
/// pattern. This replaces the former `diff_mode_layer_bindings` host-push
/// builder (DX.5): the binding choice now lives wholly with the mode and the
/// host pushes nothing diff-specific. An unregistered name is dropped by the
/// translate pass with a `warn!` — never a boot panic (graceful degradation).
///
/// - `do` → `action:diff-get` (D.5.b): rewrite the current side's hunk to
///   match the baseline.
/// - `dp` → `action:diff-put` (D.5.c): push the current side's hunk into the
///   peer buffer.
///
/// K.1.c's per-keystroke filter gates the chords so they only fire on
/// buffers where `diff-mode` is in `ActiveModes.minors()`; buffers without
/// diff-mode active fall through to the normal `d`-operator resolution.
fn diff_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "do",
                doc: "diff-get: rewrite the current side's hunk to match the baseline",
                cmd: "action:diff-get"
            },
            keymap_entry! {
                mode: Normal, chord: "dp",
                doc: "diff-put: push the current side's hunk into the peer buffer",
                cmd: "action:diff-put"
            },
            keymap_entry! {
                mode: Normal, chord: "]c",
                doc: "diff: jump the cursor to the next hunk start (wraps)",
                cmd: "action:hunk-next"
            },
            keymap_entry! {
                mode: Normal, chord: "[c",
                doc: "diff: jump the cursor to the previous hunk start (wraps)",
                cmd: "action:hunk-prev"
            },
        ]
    })
}

/// CR.3 (2026-06-24): the `diff-conflict-mode` keymap entries — the
/// vim-fugitive 3-way conflict-resolution family, contributed through
/// `DiffConflictMode::keymap()` and pushed under
/// `MinorMode(diff-conflict-mode)` by the host's K.2.4 translate pass.
/// K.1.c gates them to buffers where `diff-conflict-mode` is active (i.e.
/// the session carries a `Conflict` region), so on a clean / 2-way buffer
/// `d2o` etc. fall through to the normal `d`-operator resolution.
///
/// - `d2o` → keep-ours, `d3o` → keep-theirs (diffget from local/remote);
/// - `d2p` → put-ours, `d3p` → put-theirs (diffput to local/remote);
/// - `dB` → keep-both (ours⌢theirs).
///
/// In the marker-free 3-way model the cursor sits on `local` (= ours), so
/// `d2o`/`d2p` are degenerate self-targets that emit an informative echo
/// (see the `diff_*_effect` resolvers).
fn diff_conflict_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "d2o",
                doc: "conflict keep-ours: keep the local side for the conflict under the cursor",
                cmd: "action:diff-keep-ours"
            },
            keymap_entry! {
                mode: Normal, chord: "d3o",
                doc: "conflict keep-theirs: take the remote side into local",
                cmd: "action:diff-keep-theirs"
            },
            keymap_entry! {
                mode: Normal, chord: "d2p",
                doc: "conflict put-ours: push local into the ours side",
                cmd: "action:diff-put-ours"
            },
            keymap_entry! {
                mode: Normal, chord: "d3p",
                doc: "conflict put-theirs: push local into the remote side",
                cmd: "action:diff-put-theirs"
            },
            keymap_entry! {
                mode: Normal, chord: "dB",
                doc: "conflict keep-both: splice ours then theirs into local",
                cmd: "action:diff-keep-both"
            },
        ]
    })
}

// ──────────────────────────────────────────────────────────────
// DiffModeBridge — refcount + cross-thread queue
// ──────────────────────────────────────────────────────────────

/// Direction of a pending [`DiffModeChange`]. `Activate` adds
/// `diff-mode` to the buffer's `ActiveModes.minors()`;
/// `Deactivate` removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffModeAction {
    Activate,
    Deactivate,
}

/// CR.3 (2026-06-24): which diff minor mode a [`DiffModeChange`] toggles.
/// `Base` = `diff-mode` (every session participant); `Conflict` =
/// `diff-conflict-mode` (only sessions whose sign map carries a
/// `Conflict` region). The host's `apply_pending_diff_mode_changes` maps
/// the kind to the concrete `ModeId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffModeKind {
    Base,
    Conflict,
}

/// One queued toggle. Buffered until the dispatch tail drains
/// the bridge and applies them through the mode registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffModeChange {
    pub buffer: BufferId,
    pub action: DiffModeAction,
    /// CR.3: which mode (`diff-mode` vs `diff-conflict-mode`) to toggle.
    pub kind: DiffModeKind,
}

#[derive(Debug, Default)]
struct BridgeState {
    /// session_key → the participants that session activated
    /// for. Stored so `note_session_closed(session_key)` can
    /// look up which buffers to decrement without re-reading
    /// the descriptor (which has been removed by the time
    /// `drop_session` calls the bridge).
    sessions: HashMap<BufferId, Vec<BufferId>>,
    /// participant buffer → set of session keys currently
    /// holding it active. Refcount via vec length.
    refcounts: HashMap<BufferId, Vec<BufferId>>,
    /// CR.3: sessions currently flagged conflict-bearing → the
    /// participants their conflict state activated `diff-conflict-mode`
    /// for. Mirrors `sessions` but for the conflict overlay mode, so
    /// `note_session_closed` can decrement them without re-reading the
    /// (already-removed) sign map.
    conflict_sessions: HashMap<BufferId, Vec<BufferId>>,
    /// CR.3: per-buffer `diff-conflict-mode` refcount, keyed by the
    /// session keys currently holding it conflict-active. A buffer may
    /// hold `diff-mode` (always, while in a session) without holding
    /// `diff-conflict-mode` (only while a conflict region exists).
    conflict_refcounts: HashMap<BufferId, Vec<BufferId>>,
    /// Pending toggle queue drained from the dispatch tail.
    pending: Vec<DiffModeChange>,
}

/// Host-side bridge between [`DiffSubsystem`] session lifecycle
/// and the per-buffer `ActiveModes` set. See module docs for
/// the why.
#[derive(Debug, Default)]
pub struct DiffModeBridge {
    state: Mutex<BridgeState>,
}

impl DiffModeBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `session_key` opened with `participants` as
    /// the user-visible diff sides. For each participant whose
    /// refcount transitions 0 → 1, queue an `Activate` change.
    /// Idempotent on re-open of the same session_key
    /// (subsystem `register_with_sources` is itself idempotent
    /// on session identity).
    pub fn note_session_opened(&self, session_key: BufferId, participants: &[BufferId]) {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        // Re-open of a known session_key: scrub the previous
        // participants first so the refcount stays correct
        // (descriptor may have changed sides — D.4.d.3 plans
        // for `:diff-mode <inline|split|three-way>`).
        if let Some(old) = state.sessions.remove(&session_key) {
            for b in old {
                Self::dec_refcount(&mut state, session_key, b);
            }
        }
        let owned: Vec<BufferId> = participants.to_vec();
        state.sessions.insert(session_key, owned.clone());
        for b in owned {
            Self::inc_refcount(&mut state, session_key, b);
        }
    }

    /// Record that `session_key` closed. For each participant
    /// whose refcount drops to 0, queue a `Deactivate` change.
    /// No-op on unknown session keys (close-before-open or
    /// double-close from auto-drop racing the user's
    /// `:diffoff`).
    pub fn note_session_closed(&self, session_key: BufferId) {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        if let Some(participants) = state.sessions.remove(&session_key) {
            for b in participants {
                Self::dec_refcount(&mut state, session_key, b);
            }
        }
        // CR.3: also clear any `diff-conflict-mode` activation this
        // session held, so the conflict overlay deactivates on close
        // (e.g. `:diffoff` / doc-close auto-drop) even if the recompute
        // never published a no-conflict sign map first.
        if let Some(participants) = state.conflict_sessions.remove(&session_key) {
            for b in participants {
                Self::dec_conflict(&mut state, session_key, b);
            }
        }
    }

    /// D.8.d (2026-05-31): record that an existing session
    /// gained a new participant `buf`. Increments the refcount
    /// + appends `buf` to the session's participant list so
    /// subsequent [`Self::note_session_closed`] decrements
    /// every participant including this one.
    ///
    /// Idempotent: re-adding a buffer that's already a
    /// participant of `session_key` is a no-op (the refcount
    /// bucket dedupes by session-key, mirroring
    /// [`Self::note_session_opened`]).
    ///
    /// No-op on unknown `session_key` — the caller has either
    /// already torn the session down or the bridge was never
    /// notified of the open. Defensive against races between
    /// `:diffthis` (which goes through the membership API) and
    /// `drop_session` (which goes through `note_session_closed`).
    pub fn note_session_extended(&self, session_key: BufferId, buf: BufferId) {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        let Some(participants) = state.sessions.get_mut(&session_key) else {
            return;
        };
        if participants.contains(&buf) {
            return;
        }
        participants.push(buf);
        Self::inc_refcount(&mut state, session_key, buf);
    }

    /// D.8.d (2026-05-31): record that a participant `buf` was
    /// removed from `session_key`. Decrements the refcount; if
    /// `buf` no longer participates in any session, queues a
    /// `Deactivate` change so the dispatch tail flips
    /// `ActiveModes.diff-mode` off on that buffer. Removes `buf`
    /// from the session's participant list so a subsequent
    /// `note_session_closed` doesn't double-decrement it.
    ///
    /// No-op on unknown `session_key` or unknown participant.
    pub fn note_session_shrunk(&self, session_key: BufferId, buf: BufferId) {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        let Some(participants) = state.sessions.get_mut(&session_key) else {
            return;
        };
        if let Some(pos) = participants.iter().position(|b| *b == buf) {
            participants.swap_remove(pos);
            Self::dec_refcount(&mut state, session_key, buf);
        }
    }

    /// Drain every pending change. Callers (the dispatch tail)
    /// apply them via the mode registry, which mutates the
    /// per-buffer `ActiveModes`.
    pub fn drain_pending(&self) -> Vec<DiffModeChange> {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        std::mem::take(&mut state.pending)
    }

    /// Test-only / introspection: current refcount for
    /// `buffer`. Returns 0 if no session activated diff-mode on
    /// it.
    pub fn refcount(&self, buffer: BufferId) -> usize {
        self.state
            .lock()
            .expect("DiffModeBridge mutex poisoned")
            .refcounts
            .get(&buffer)
            .map(Vec::len)
            .unwrap_or(0)
    }

    fn inc_refcount(state: &mut BridgeState, session_key: BufferId, buffer: BufferId) {
        let bucket = state.refcounts.entry(buffer).or_default();
        // Re-add of the same session_key is a no-op for the
        // refcount (idempotent re-open guard).
        if bucket.contains(&session_key) {
            return;
        }
        let was_empty = bucket.is_empty();
        bucket.push(session_key);
        if was_empty {
            state.pending.push(DiffModeChange {
                buffer,
                action: DiffModeAction::Activate,
                kind: DiffModeKind::Base,
            });
        }
    }

    fn dec_refcount(state: &mut BridgeState, session_key: BufferId, buffer: BufferId) {
        let Some(bucket) = state.refcounts.get_mut(&buffer) else {
            return;
        };
        if let Some(pos) = bucket.iter().position(|k| *k == session_key) {
            bucket.swap_remove(pos);
        }
        if bucket.is_empty() {
            state.refcounts.remove(&buffer);
            state.pending.push(DiffModeChange {
                buffer,
                action: DiffModeAction::Deactivate,
                kind: DiffModeKind::Base,
            });
        }
    }

    /// CR.3 (2026-06-24): reconcile `diff-conflict-mode` activation for
    /// `session_key` against its latest sign map. `has_conflicts` comes
    /// from [`sign_map_has_conflicts`]; the host calls this each cycle off
    /// the active session's published sign map. Idempotent on an unchanged
    /// state (no transition → nothing queued), so per-cycle calls are
    /// cheap. On the `false → true` transition it queues a `Conflict`
    /// Activate for each participant (refcounted, so a buffer shared by
    /// two conflict sessions stays active until the last clears); on
    /// `true → false` it queues a Deactivate for each whose conflict
    /// refcount hits zero.
    pub fn note_conflict_state(
        &self,
        session_key: BufferId,
        participants: &[BufferId],
        has_conflicts: bool,
    ) {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        let currently = state.conflict_sessions.contains_key(&session_key);
        if has_conflicts && !currently {
            let owned: Vec<BufferId> = participants.to_vec();
            state.conflict_sessions.insert(session_key, owned.clone());
            for b in owned {
                Self::inc_conflict(&mut state, session_key, b);
            }
        } else if !has_conflicts && currently {
            if let Some(parts) = state.conflict_sessions.remove(&session_key) {
                for b in parts {
                    Self::dec_conflict(&mut state, session_key, b);
                }
            }
        }
    }

    fn inc_conflict(state: &mut BridgeState, session_key: BufferId, buffer: BufferId) {
        let bucket = state.conflict_refcounts.entry(buffer).or_default();
        if bucket.contains(&session_key) {
            return;
        }
        let was_empty = bucket.is_empty();
        bucket.push(session_key);
        if was_empty {
            state.pending.push(DiffModeChange {
                buffer,
                action: DiffModeAction::Activate,
                kind: DiffModeKind::Conflict,
            });
        }
    }

    fn dec_conflict(state: &mut BridgeState, session_key: BufferId, buffer: BufferId) {
        let Some(bucket) = state.conflict_refcounts.get_mut(&buffer) else {
            return;
        };
        if let Some(pos) = bucket.iter().position(|k| *k == session_key) {
            bucket.swap_remove(pos);
        }
        if bucket.is_empty() {
            state.conflict_refcounts.remove(&buffer);
            state.pending.push(DiffModeChange {
                buffer,
                action: DiffModeAction::Deactivate,
                kind: DiffModeKind::Conflict,
            });
        }
    }
}

// ──────────────────────────────────────────────────────────────
// Bridge tests — pure refcount semantics. End-to-end tests that
// drive the dispatch tail + assert ActiveModes live next to the
// `do_diff_open` / `register_two_pane_diff` call sites.
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(n: u32) -> BufferId {
        BufferId(n)
    }

    /// DX.8: the `diff-conflict-mode` activation predicate fires iff the
    /// session's sign map carries a `Conflict` region — not for a clean,
    /// add-only, or change/remove-only map. This is the gate a future
    /// bridge consults to toggle the mode.
    #[test]
    fn conflict_predicate_detects_conflict_regions() {
        use crate::overlay::{DiffSignKind, DiffSignMap};

        let with_conflict = DiffSignMap::from_entries(vec![
            (1, DiffSignKind::Add),
            (3, DiffSignKind::Conflict),
        ]);
        assert!(sign_map_has_conflicts(&with_conflict));

        // Add / Change / Remove are 2-way diff signs, NOT conflicts.
        let no_conflict = DiffSignMap::from_entries(vec![
            (1, DiffSignKind::Add),
            (2, DiffSignKind::Change),
            (5, DiffSignKind::Remove),
        ]);
        assert!(!sign_map_has_conflicts(&no_conflict));

        // A clean buffer has no conflict.
        assert!(!sign_map_has_conflicts(&DiffSignMap::default()));
    }

    /// DX.8: `register_diff_modes` registers BOTH `diff-mode` and the new
    /// `diff-conflict-mode` shell (the mode decomposition, design §4)
    /// without a registration conflict — and both carry the `-mode` suffix
    /// the registry enforces.
    #[test]
    fn register_diff_modes_registers_base_and_conflict_modes() {
        let mut registry = ModeRegistry::new();
        register_diff_modes(&mut registry);
        assert!(registry.is_registered(DiffMode::mode_id()), "diff-mode registered");
        assert!(
            registry.is_registered(DiffConflictMode::mode_id()),
            "diff-conflict-mode registered"
        );
        assert!(DiffConflictMode::mode_id().as_str().ends_with("-mode"));
    }

    /// MO.x (C10): the `diff-mode` keymap entries map `do`/`dp` to the
    /// canonical `action:diff-*` names, in order (catches a name swap or a
    /// dropped chord). The end-to-end resolution on the
    /// `MinorMode(diff-mode)` layer via the host's K.2.4 translate pass is
    /// pinned by `diff_get_put_chords_bound_on_diff_mode_layer` (DX.1) — a
    /// stronger guard than a builder unit test, so the DX.5 trie-builder
    /// tests retire with `diff_mode_layer_bindings`.
    #[test]
    fn keymap_entries_map_do_and_dp_to_diff_actions() {
        let pairs: Vec<(&str, Option<&str>)> = diff_mode_keymap_entries()
            .iter()
            .map(|e| (e.chord, e.command))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("do", Some("action:diff-get")),
                ("dp", Some("action:diff-put")),
                // CR.6: hunk-nav is mode-owned now (was host do_next/prev_hunk).
                ("]c", Some("action:hunk-next")),
                ("[c", Some("action:hunk-prev")),
            ],
        );
    }

    /// CR.1: `DiffMode::action_handlers()` contributes exactly the
    /// `do`/`dp` handler bodies, keyed to the `action:diff-get` /
    /// `action:diff-put` command names — the SAME names the keymap binds,
    /// so the host's `register_mode_action_handlers` boot walk resolves
    /// each to the chord's `CommandId`. Catches a name drift or a
    /// dropped/duplicated handler.
    #[test]
    fn action_handlers_contribute_diff_get_and_put() {
        let names: Vec<&str> = DiffMode
            .action_handlers()
            .iter()
            .map(|c| c.action_name)
            .collect();
        assert_eq!(
            names,
            vec![
                "action:diff-get",
                "action:diff-put",
                "action:hunk-next",
                "action:hunk-prev",
            ]
        );
    }

    /// CR.3: `DiffConflictMode` keymap maps the 5 vim-fugitive chords to
    /// the conflict action names, in order (catches a name swap / dropped
    /// chord). End-to-end layer resolution is pinned host-side by the
    /// DX.1-style `diff_conflict_chords_bound_on_conflict_mode_layer`.
    #[test]
    fn conflict_keymap_entries_map_chords_to_actions() {
        let pairs: Vec<(&str, Option<&str>)> = diff_conflict_mode_keymap_entries()
            .iter()
            .map(|e| (e.chord, e.command))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("d2o", Some("action:diff-keep-ours")),
                ("d3o", Some("action:diff-keep-theirs")),
                ("d2p", Some("action:diff-put-ours")),
                ("d3p", Some("action:diff-put-theirs")),
                ("dB", Some("action:diff-keep-both")),
            ],
        );
    }

    /// CR.3: `DiffConflictMode` contributes the 5 conflict handler bodies,
    /// keyed to the matching action names.
    #[test]
    fn conflict_action_handlers_contribute_five() {
        let names: Vec<&str> = DiffConflictMode
            .action_handlers()
            .iter()
            .map(|c| c.action_name)
            .collect();
        assert_eq!(
            names,
            vec![
                "action:diff-keep-ours",
                "action:diff-keep-theirs",
                "action:diff-put-ours",
                "action:diff-put-theirs",
                "action:diff-keep-both",
            ],
        );
    }

    /// CR.3: `note_conflict_state` queues a Conflict Activate per
    /// participant on the false→true transition, a Deactivate on
    /// true→false, and nothing on repeated same-state calls (so the
    /// host's per-cycle reconcile is cheap + idempotent).
    #[test]
    fn conflict_state_activates_and_deactivates_per_transition() {
        let bridge = DiffModeBridge::new();
        bridge.note_conflict_state(bid(2), &[bid(1), bid(2)], true);
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| c.kind == DiffModeKind::Conflict
                    && c.action == DiffModeAction::Activate)
        );
        // Idempotent: still conflict-bearing → nothing queued.
        bridge.note_conflict_state(bid(2), &[bid(1), bid(2)], true);
        assert!(bridge.drain_pending().is_empty());
        // Cleared → one Deactivate per participant.
        bridge.note_conflict_state(bid(2), &[bid(1), bid(2)], false);
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| c.kind == DiffModeKind::Conflict
                    && c.action == DiffModeAction::Deactivate)
        );
    }

    /// CR.3: closing a session also clears its conflict-mode activation,
    /// even if a no-conflict sign map was never published first
    /// (`:diffoff` / doc-close auto-drop).
    #[test]
    fn closing_session_clears_conflict_activation() {
        let bridge = DiffModeBridge::new();
        bridge.note_conflict_state(bid(2), &[bid(1), bid(2)], true);
        let _ = bridge.drain_pending();
        bridge.note_session_closed(bid(2));
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| c.kind == DiffModeKind::Conflict
                    && c.action == DiffModeAction::Deactivate)
        );
    }

    /// ML.3b: the formatter counts adds vs changes (Change/Conflict/Remove
    /// fold into `~`), and yields empty (hidden) content for a clean map.
    #[test]
    fn diff_content_counts_adds_and_changes() {
        use crate::overlay::{DiffSignKind, DiffSignMap};
        let map = DiffSignMap::from_entries(vec![
            (1, DiffSignKind::Add),
            (2, DiffSignKind::Add),
            (3, DiffSignKind::Change),
            (4, DiffSignKind::Conflict),
            (5, DiffSignKind::Remove),
        ]);
        // 2 adds, 3 changed (Change + Conflict + Remove).
        assert_eq!(diff_content(&map).plain(), "+2 ~3");

        // Clean map → hidden.
        assert!(diff_content(&DiffSignMap::default()).is_empty());

        // Adds only → no `~` segment.
        let adds = DiffSignMap::from_entries(vec![(1, DiffSignKind::Add)]);
        assert_eq!(diff_content(&adds).plain(), "+1");
    }

    /// ML.3b: the descriptor is Global (active-pane only), Left zone,
    /// after `core.path`.
    #[test]
    fn register_diff_element_is_global_left() {
        let svc = ModelineService::new();
        register_diff_modeline_element(&svc);
        let snap = svc.snapshot();
        let el = snap.registry.get(&ElementId::new(DIFF_ELEMENT)).expect("diff descriptor");
        assert_eq!(el.zone, Zone::Left);
        assert_eq!(el.priority, 20);
        assert_eq!(el.scope, Scope::Global);
    }

    /// DX.1 (BC.6 gate): `DiffMode::gutter_decorations` projects the active
    /// buffer's `DiffSignMap` into one `GutterDecoration::Diff` per signed line,
    /// mapping each `DiffSignKind` to its `GutterDiffKind`. This is the
    /// sign-gutter contract the extraction must preserve; it lives in `mode.rs`
    /// (mode-owned) so it moves with the mode into `lattice-diff` at DX.6.
    #[test]
    fn gutter_decorations_emit_diff_signs_from_sign_map() {
        use crate::overlay::{DiffSignKind, DiffSignMap};
        use lattice_mode::ServiceRegistry;

        let sign_map = std::sync::Arc::new(DiffSignMap::from_entries(vec![
            (0, DiffSignKind::Add),
            (2, DiffSignKind::Change),
            (5, DiffSignKind::Remove),
            (7, DiffSignKind::Conflict),
        ]));
        let mut services = ServiceRegistry::new();
        services.register(DiffDecorationData { sign_map });

        let ctx = DecorationCtx::new(bid(1), &services);
        let decos = DiffMode.gutter_decorations(&ctx);

        let expected = [
            (0u32, GutterDiffKind::Add),
            (2, GutterDiffKind::Change),
            (5, GutterDiffKind::Remove),
            (7, GutterDiffKind::Conflict),
        ];
        assert_eq!(decos.len(), expected.len(), "one decoration per signed line");
        for (deco, (eline, ekind)) in decos.iter().zip(expected) {
            match deco {
                GutterDecoration::Diff { line, kind } => {
                    assert_eq!(*line, eline);
                    assert_eq!(*kind, ekind);
                }
                other => panic!("expected a Diff gutter decoration, got {other:?}"),
            }
        }
    }

    /// DX.1: without a `DiffDecorationData` service (the renderer injects it
    /// only for the active diff buffer), the gutter contributes nothing — a
    /// non-diff buffer has no diff signs.
    #[test]
    fn gutter_decorations_empty_without_decoration_service() {
        use lattice_mode::ServiceRegistry;
        let services = ServiceRegistry::new();
        let ctx = DecorationCtx::new(bid(1), &services);
        assert!(DiffMode.gutter_decorations(&ctx).is_empty());
    }

    #[test]
    fn open_session_queues_activates_for_each_participant() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .any(|c| c.buffer == bid(1) && c.action == DiffModeAction::Activate)
        );
        assert!(
            changes
                .iter()
                .any(|c| c.buffer == bid(2) && c.action == DiffModeAction::Activate)
        );
    }

    #[test]
    fn close_session_queues_deactivates() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        let _ = bridge.drain_pending();
        bridge.note_session_closed(bid(2));
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| c.action == DiffModeAction::Deactivate)
        );
        assert!(changes.iter().any(|c| c.buffer == bid(1)));
        assert!(changes.iter().any(|c| c.buffer == bid(2)));
    }

    #[test]
    fn close_unknown_session_is_noop() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_closed(bid(99));
        assert!(bridge.drain_pending().is_empty());
    }

    #[test]
    fn empty_participants_queues_nothing() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[]);
        assert!(bridge.drain_pending().is_empty());
        bridge.note_session_closed(bid(2));
        assert!(bridge.drain_pending().is_empty());
    }

    #[test]
    fn two_independent_sessions_do_not_interfere() {
        // Session A: file1 ↔ file2. Session B: file3 ↔ file4.
        // Each open queues two activates; each close queues
        // two deactivates. The buffers never refcount-overlap,
        // so the changes are independent.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        bridge.note_session_opened(bid(4), &[bid(3), bid(4)]);
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 4);

        bridge.note_session_closed(bid(2));
        let after_close_a = bridge.drain_pending();
        assert_eq!(after_close_a.len(), 2);
        assert!(
            after_close_a
                .iter()
                .all(|c| c.action == DiffModeAction::Deactivate)
        );
        assert!(after_close_a.iter().any(|c| c.buffer == bid(1)));
        assert!(after_close_a.iter().any(|c| c.buffer == bid(2)));

        // Session B is untouched.
        assert_eq!(bridge.refcount(bid(3)), 1);
        assert_eq!(bridge.refcount(bid(4)), 1);
        assert_eq!(bridge.refcount(bid(1)), 0);
        assert_eq!(bridge.refcount(bid(2)), 0);
    }

    #[test]
    fn shared_buffer_in_two_sessions_holds_mode_until_last_closes() {
        // Pathological but legal: file1 participates in both
        // session A (file1 ↔ file2) and session B (file1 ↔
        // file3). Closing A must NOT deactivate diff-mode on
        // file1 because B still needs it.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        bridge.note_session_opened(bid(3), &[bid(1), bid(3)]);
        let _ = bridge.drain_pending();

        assert_eq!(bridge.refcount(bid(1)), 2);
        assert_eq!(bridge.refcount(bid(2)), 1);
        assert_eq!(bridge.refcount(bid(3)), 1);

        // Close session A: file2 deactivates; file1 must NOT
        // (still held by session B).
        bridge.note_session_closed(bid(2));
        let changes = bridge.drain_pending();
        assert_eq!(
            changes.len(),
            1,
            "only file2 should deactivate, got {changes:?}"
        );
        assert_eq!(changes[0].buffer, bid(2));
        assert_eq!(changes[0].action, DiffModeAction::Deactivate);
        assert_eq!(bridge.refcount(bid(1)), 1, "file1 still held by B");

        // Close session B: file1 + file3 deactivate.
        bridge.note_session_closed(bid(3));
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|c| c.action == DiffModeAction::Deactivate)
        );
        assert!(changes.iter().any(|c| c.buffer == bid(1)));
        assert!(changes.iter().any(|c| c.buffer == bid(3)));
    }

    #[test]
    fn reopen_same_session_key_is_idempotent_on_refcount() {
        // `register_with_sources` is idempotent on session
        // identity; the bridge mirrors that — re-opening the
        // same session_key with the same participants must not
        // double-count.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        assert_eq!(bridge.refcount(bid(1)), 1);
        assert_eq!(bridge.refcount(bid(2)), 1);
    }

    #[test]
    fn reopen_with_different_participants_releases_old() {
        // A future `:diff-mode split` may swap a session's
        // participants. The bridge must scrub old refcounts
        // and account the new set.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        let _ = bridge.drain_pending();

        // Swap baseline from file1 to file9.
        bridge.note_session_opened(bid(2), &[bid(9), bid(2)]);
        assert_eq!(bridge.refcount(bid(1)), 0, "old baseline released");
        assert_eq!(bridge.refcount(bid(9)), 1, "new baseline held");
        assert_eq!(bridge.refcount(bid(2)), 1, "primary still held");

        let changes = bridge.drain_pending();
        // Expect: file1 → Deactivate; file9 → Activate. file2's
        // refcount went 1 → 0 → 1 in one call, so its change
        // sequence is Deactivate + Activate.
        assert!(
            changes
                .iter()
                .any(|c| c.buffer == bid(1) && c.action == DiffModeAction::Deactivate)
        );
        assert!(
            changes
                .iter()
                .any(|c| c.buffer == bid(9) && c.action == DiffModeAction::Activate)
        );
    }

    #[test]
    fn drain_pending_is_idempotent() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1)]);
        assert_eq!(bridge.drain_pending().len(), 1);
        assert!(bridge.drain_pending().is_empty(), "second drain empty");
    }
}
