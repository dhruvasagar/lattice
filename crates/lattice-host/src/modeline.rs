//! Built-in modeline element vocabulary + shared per-pane content
//! resolution (slice ML.1a-render).
//!
//! The modeline's **content strategy is common across renderers**: this
//! module computes each built-in element's [`ElementContent`] (text +
//! theme role) from `(pane, RenderState)`, host-side, so the TUI
//! (ML.1a) and GPUI (ML.2) peers paint *identical* content and differ
//! ONLY in layout / paint (and GPUI-only richness like tooltips and
//! clicks). See `docs/dev/architecture/modeline.md` §4 (host-side
//! computation), §8 (theme roles), §10 (cross-renderer parity).
//!
//! Two halves:
//! - **descriptors** — [`register_builtin_elements`] registers the
//!   `core.*` set into the shared [`ModelineService`] once at boot
//!   (host owns the built-ins; modes/plugins own theirs, §6).
//! - **content** — [`resolve_builtin_content`] (per-pane built-ins) and
//!   [`resolve_mode_items_content`] (the temporary legacy mode-items
//!   pull that feeds the Center zone until ML.3 migrates LSP/diff to
//!   registered elements).

use lattice_core::ui::pane::PaneState;
use lattice_mode::{ElementContent, ElementId, ModelineElement, ModelineRole, ModelineService, Zone};

use crate::render_state::RenderState;

// --- Built-in element ids ---------------------------------------------
// Single source of truth shared by boot registration AND the renderers'
// content resolution — no stringly-typed drift between the two sites.

/// Modal-state label (`[NORMAL]`). Left zone, first.
pub const CORE_MODE: &str = "core.mode";
/// Buffer path + dirty marker (or a pane provider's custom label).
pub const CORE_PATH: &str = "core.path";
/// Cursor line:column. Right zone.
pub const CORE_POSITION: &str = "core.position";
/// Detected language label. Right zone, far right.
pub const CORE_LANG: &str = "core.lang";

// --- Theme roles ------------------------------------------------------
// Assigned now so ML.1b is a pure theme-lookup change (the renderer
// resolves these against `ResolvedTheme`); until then both peers fold
// every role onto the single `pane_status_*` style.

pub const ROLE_MODE: &str = "modeline.mode";
pub const ROLE_PATH: &str = "modeline.path";
pub const ROLE_POSITION: &str = "modeline.position";
pub const ROLE_LANG: &str = "modeline.lang";
pub const ROLE_MODE_ITEM: &str = "modeline.mode_item";

/// Register the host's built-in modeline descriptors. Called once at
/// boot against the shared service (the same instance the renderers
/// snapshot and modes reach via `ctx.service::<ModelineServiceHandle>()`).
/// Priorities are uniform leftward→rightward in every zone; the
/// renderer right-aligns the Right *block* (see [`Zone`]).
pub fn register_builtin_elements(svc: &ModelineService) {
    svc.register(ModelineElement::new(ElementId::new(CORE_MODE), Zone::Left, 0));
    svc.register(ModelineElement::new(ElementId::new(CORE_PATH), Zone::Left, 10));
    svc.register(ModelineElement::new(ElementId::new(CORE_POSITION), Zone::Right, 10));
    svc.register(ModelineElement::new(ElementId::new(CORE_LANG), Zone::Right, 20));
}

/// Modal-state short label for the active document, read from the
/// published [`RenderState`] (no actor crossing). Shared by the
/// modeline resolver and the renderers' `modal_label` accessors so the
/// vocabulary has one definition.
pub fn modal_label(rs: &RenderState) -> &'static str {
    use lattice_grammar::ModalState;
    let ad = rs.active_document.load();
    // A Terminal buffer reflects its own sub-state; the underlying
    // modal stays Normal while terminal-insert is the discriminator.
    if matches!(ad.buffer_kind, lattice_core::BufferKind::Terminal) {
        return if ad.terminal_insert_active {
            "TERMINAL-INSERT"
        } else if ad.terminal_visual_active {
            "TERMINAL-VISUAL"
        } else {
            "TERMINAL"
        };
    }
    match ad.modal {
        ModalState::Normal => "NORMAL",
        ModalState::Insert => "INSERT",
        ModalState::Visual(_) => "VISUAL",
        ModalState::Select(_) => "SELECT",
        ModalState::OperatorPending => "O-PEND",
        ModalState::Command => "CMD",
        ModalState::Search(_) => "SEARCH",
        ModalState::Replace => "REPLACE",
    }
}

/// The buffer-label segment for `pane`: a pane provider's custom label
/// when one is supplied (`provider_label` — the file-tree / oil / help
/// M.4 mechanism, resolved renderer-side and passed in so the assembly
/// stays common), else the document path + dirty marker (or the
/// registry name slot for non-document panes). Empty only for the
/// genuinely-nameless case, which the caller treats as hidden.
pub fn pane_path_segment(
    pane: &PaneState,
    rs: &RenderState,
    provider_label: Option<&str>,
) -> String {
    if let Some(label) = provider_label {
        return label.to_string();
    }
    let reg = &rs.buffers.registry;
    // Non-document panes (Terminal, …) without a provider: name slot.
    if !reg.contains_document(pane.buffer_id) {
        return reg
            .name_of(pane.buffer_id)
            .unwrap_or_else(|| "[no buffer]".to_string());
    }
    let label = reg
        .document_path(pane.buffer_id)
        .map(|p| p.display().to_string())
        .or_else(|| reg.name_of(pane.buffer_id))
        .unwrap_or_else(|| "[no name]".to_string());
    // Suppress the dirty marker for synthetics (streamed buffers the
    // user can never save).
    let synthetic = reg.name_of(pane.buffer_id).is_some();
    let dirty = if !synthetic && reg.document_dirty(pane.buffer_id) {
        " [+]"
    } else {
        ""
    };
    format!("{label}{dirty}")
}

/// Detected-language label for `pane`'s buffer (empty when no path).
fn lang_label(pane: &PaneState, rs: &RenderState) -> &'static str {
    rs.buffers
        .registry
        .document_path(pane.buffer_id)
        .map(|p| lattice_syntax::Lang::detect_from_path(Some(p.as_path())).label())
        .unwrap_or("")
}

/// Resolve a built-in (`core.*`) element's content for `pane`. Pure
/// reads off the published [`RenderState`] — O(1), no allocation
/// proportional to document size (paramount #1).
///
/// `is_active` gates the elements that read *active-document* global
/// state: `core.mode` (the modal label is a single active-doc value —
/// showing it on an inactive pane would be wrong, not just noisy) and
/// `core.lang` (parity with the legacy footer, which omitted it on
/// inactive panes). `provider_label` overrides `core.path` for
/// provider-backed panes (file-tree / oil / help).
///
/// An empty [`ElementContent`] means "hidden this frame" — the caller
/// skips it (the same contract [`lattice_mode::ModelineSnapshot::zone`]
/// uses).
pub fn resolve_builtin_content(
    id: &str,
    pane: &PaneState,
    is_active: bool,
    rs: &RenderState,
    provider_label: Option<&str>,
) -> ElementContent {
    match id {
        CORE_MODE => {
            if is_active {
                ElementContent::text(
                    format!("[{}]", modal_label(rs)),
                    ModelineRole::new(ROLE_MODE),
                )
            } else {
                ElementContent::default()
            }
        }
        CORE_PATH => {
            let text = pane_path_segment(pane, rs, provider_label);
            if text.is_empty() {
                ElementContent::default()
            } else {
                ElementContent::text(text, ModelineRole::new(ROLE_PATH))
            }
        }
        CORE_POSITION => ElementContent::text(
            format!("{}:{}", pane.cursor.line + 1, pane.cursor.byte),
            ModelineRole::new(ROLE_POSITION),
        ),
        CORE_LANG => {
            let lang = if is_active { lang_label(pane, rs) } else { "" };
            if lang.is_empty() {
                ElementContent::default()
            } else {
                ElementContent::text(lang, ModelineRole::new(ROLE_LANG))
            }
        }
        _ => ElementContent::default(),
    }
}

/// The mode-contributed status items for `pane`, as one space-joined
/// span (migrated host-side from the TUI `collect_status_line_items`).
/// This is the **temporary** Center-zone feed: LSP progress / readiness
/// and diff-sign counts still arrive through the `Mode::status_line_items`
/// trait here, and move to registered elements pushed over the event
/// bus in ML.3 (at which point this function and the trait retire).
///
/// Empty content (no active modes, or no items) ⇒ hidden this frame.
pub fn resolve_mode_items_content(pane: &PaneState, rs: &RenderState) -> ElementContent {
    use lattice_mode::{ServiceRegistry, StatusLineCtx};

    let mut services = ServiceRegistry::new();
    // LSP progress / readiness: process-wide; the mode picks what to show.
    services.register(lattice_lsp::modes::LspProgressStatusData {
        progress: rs.lsp.progress.clone(),
        server_status: rs.lsp.server_status.clone(),
    });
    // Diff signs: active-buffer only (DiffRenderState is per active pane).
    let ad = rs.active_document.load();
    if pane.buffer_id == ad.document_buffer_id {
        services.register(crate::diff::mode::DiffStatusData {
            sign_map: rs.diff.sign_map.clone(),
        });
    }

    let ctx = StatusLineCtx::new(pane.buffer_id, &services);

    let modes_rs = rs.modes.clone();
    let Some(active) = modes_rs.map.get(&pane.buffer_id) else {
        return ElementContent::default();
    };

    let registry = &modes_rs.mode_registry;
    let mut all_ids: Vec<lattice_mode::ModeId> = Vec::new();
    if let Some(major) = active.major() {
        all_ids.push(major);
    }
    all_ids.extend_from_slice(active.minors());

    let mut items: Vec<lattice_mode::StatusLineItem> = Vec::new();
    for id in all_ids {
        if let Some(mode) = registry.get(id) {
            items.extend(mode.status_line_items(&ctx));
        }
    }
    items.sort_by_key(|i| i.priority);
    if items.is_empty() {
        return ElementContent::default();
    }
    let text = items
        .iter()
        .map(|i| i.text.as_str())
        .collect::<Vec<_>>()
        .join("  ");
    ElementContent::text(text, ModelineRole::new(ROLE_MODE_ITEM))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Boot registers the four built-ins with the spec'd zones +
    /// priorities (modeline.md §3 / slice plan ML.1a-render).
    #[test]
    fn boot_registers_builtin_descriptors() {
        let document = lattice_core::Document::empty();
        let editor = crate::editor::Editor::boot(document);
        let snap = editor.modeline.snapshot();
        let reg = &snap.registry;
        assert_eq!(reg.len(), 4, "exactly the four core built-ins");

        let mode = reg.get(&ElementId::new(CORE_MODE)).unwrap();
        assert_eq!((mode.zone, mode.priority), (Zone::Left, 0));
        let path = reg.get(&ElementId::new(CORE_PATH)).unwrap();
        assert_eq!((path.zone, path.priority), (Zone::Left, 10));
        let pos = reg.get(&ElementId::new(CORE_POSITION)).unwrap();
        assert_eq!((pos.zone, pos.priority), (Zone::Right, 10));
        let lang = reg.get(&ElementId::new(CORE_LANG)).unwrap();
        assert_eq!((lang.zone, lang.priority), (Zone::Right, 20));
    }

    /// `core.mode` shows the modal label only on the active pane (it is
    /// a single active-document read); `core.position` always shows.
    #[test]
    fn builtin_mode_is_active_only_position_always() {
        let document = lattice_core::Document::empty();
        let mut editor = crate::editor::Editor::boot(document);
        let rs = editor.build_render_state();
        let pane = editor.pane_tree.active().clone();

        let active_mode = resolve_builtin_content(CORE_MODE, &pane, true, &rs, None);
        assert_eq!(active_mode.plain(), "[NORMAL]");
        let inactive_mode = resolve_builtin_content(CORE_MODE, &pane, false, &rs, None);
        assert!(inactive_mode.is_empty(), "mode hidden on inactive panes");

        // Position renders regardless of active/inactive; row 1, col 0.
        let pos = resolve_builtin_content(CORE_POSITION, &pane, false, &rs, None);
        assert_eq!(pos.plain(), "1:0");
    }

    /// A provider label overrides `core.path`; without one, the empty
    /// scratch document falls back to the registry name slot.
    #[test]
    fn builtin_path_honours_provider_label() {
        let document = lattice_core::Document::empty();
        let mut editor = crate::editor::Editor::boot(document);
        let rs = editor.build_render_state();
        let pane = editor.pane_tree.active().clone();

        let overridden =
            resolve_builtin_content(CORE_PATH, &pane, true, &rs, Some("[tree] /root"));
        assert_eq!(overridden.plain(), "[tree] /root");

        // No provider: a fresh scratch buffer has no path; the segment
        // is non-empty (a name slot or "[no name]"), never a panic.
        let fallback = resolve_builtin_content(CORE_PATH, &pane, true, &rs, None);
        assert!(!fallback.plain().is_empty());
    }
}
