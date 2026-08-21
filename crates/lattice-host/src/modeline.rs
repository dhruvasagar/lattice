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
//! - **content** — [`resolve_builtin_content`] computes per-pane
//!   built-in (`core.*`) content host-side. Mode/plugin elements (`lsp`,
//!   `diff`, …) are *pushed* over the event bus and drained into the
//!   content store (ML.3); the renderer reads them from the snapshot, so
//!   they never round-trip through this module.

use std::collections::HashSet;

use lattice_config::{
    ConfigRegistry, ModelineCenter, ModelineLeft, ModelinePadding, ModelineRight,
    ModelineSeparator, ModelineZone,
};
use lattice_core::ui::pane::PaneState;
use lattice_protocol::CommandId;

use lattice_mode::{
    ElementContent, ElementId, ModelineElement, ModelineRegistry, ModelineRole, ModelineService,
    Zone,
};

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
// DX.4 (BC.6): `ROLE_MODE_ITEM` moved DOWN to `lattice-mode`'s modeline
// module (it is the role *modes* tag contributed content with, so
// `lattice-diff` can reach it without the host). Re-exported here so
// renderer style maps (`ml::ROLE_MODE_ITEM`) + `crate::modeline` call
// sites are unchanged; diff's import flips to `lattice_mode::modeline`
// at DX.6.
pub use lattice_mode::modeline::ROLE_MODE_ITEM;

/// Register the host's built-in modeline descriptors. Called once at
/// boot against the shared service (the same instance the renderers
/// snapshot and modes reach via `ctx.service::<ModelineServiceHandle>()`).
/// Priorities are uniform leftward→rightward in every zone; the
/// renderer right-aligns the Right *block* (see [`Zone`]).
pub fn register_builtin_elements(svc: &ModelineService) {
    svc.register(ModelineElement::new(
        ElementId::new(CORE_MODE),
        Zone::Left,
        0,
    ));
    svc.register(ModelineElement::new(
        ElementId::new(CORE_PATH),
        Zone::Left,
        10,
    ));
    svc.register(ModelineElement::new(
        ElementId::new(CORE_POSITION),
        Zone::Right,
        10,
    ));
    svc.register(ModelineElement::new(
        ElementId::new(CORE_LANG),
        Zone::Right,
        20,
    ));
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
        ModalState::Prompt => "PROMPT",
    }
}

/// Lean 3-letter modal label for the **modeline** (ML.5d). The full
/// name ([`modal_label`]) is echoed in the echo area on mode *change*;
/// the persistent modeline tag stays compact + modern (Helix-style
/// `NOR`/`INS`/`VIS`), disambiguated by colour through the
/// `modeline.mode` theme role. Same source-of-truth read as
/// [`modal_label`] (published `RenderState`, no actor crossing).
pub fn modal_label_short(rs: &RenderState) -> &'static str {
    use lattice_grammar::ModalState;
    let ad = rs.active_document.load();
    if matches!(ad.buffer_kind, lattice_core::BufferKind::Terminal) {
        return if ad.terminal_insert_active {
            "TIN"
        } else if ad.terminal_visual_active {
            "TVI"
        } else {
            "TRM"
        };
    }
    match ad.modal {
        ModalState::Normal => "NOR",
        ModalState::Insert => "INS",
        ModalState::Visual(_) => "VIS",
        ModalState::Select(_) => "SEL",
        ModalState::OperatorPending => "OPN",
        ModalState::Command => "CMD",
        ModalState::Search(_) => "SEA",
        ModalState::Replace => "REP",
        ModalState::Prompt => "PMT",
    }
}

/// Shorten a path to be relative to a base directory if the
/// `path.relative` option is enabled and a base directory is set.
fn maybe_shorten_path(path: &std::path::Path, rs: &RenderState) -> String {
    let relative = rs
        .options
        .config
        .get_typed::<lattice_config::core_options::PathRelative>()
        .map(|v| *v)
        .unwrap_or(false);
    if relative
        && let Some(ref cwd) = rs.current_dir
        && let Ok(rel) = path.strip_prefix(cwd)
    {
        let s = rel.display().to_string();
        return if s.is_empty() { ".".to_string() } else { s };
    }
    path.display().to_string()
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
        .map(|p| maybe_shorten_path(p.as_path(), rs))
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
                // Lean 3-letter tag, no brackets (ML.5d); colour comes
                // from the `modeline.mode` role. The full mode name is
                // echoed in the echo area on mode change instead.
                ElementContent::text(modal_label_short(rs), ModelineRole::new(ROLE_MODE))
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
        CORE_POSITION => {
            // The active (focused) pane's live cursor lives in
            // `rs.active_document` (published from `self.cursor` on every
            // motion); the pane-tree stash `pane.cursor` is only written
            // back when the pane goes inactive, so reading it here would
            // freeze line/col on the focused pane. Inactive panes keep
            // their stashed cursor.
            let cursor = if is_active {
                rs.active_document.load().cursor
            } else {
                pane.cursor
            };
            ElementContent::text(
                format!("{}:{}", cursor.line + 1, cursor.byte),
                ModelineRole::new(ROLE_POSITION),
            )
        }
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

// --- Config-driven zone layout (ML.5) -------------------------------
// The renderers iterate THIS instead of `registry.zone_ordered(zone)`
// so the `ui.modeline.{left,center,right}` options drive membership +
// order, while built-in content stays computed host-side
// (`resolve_builtin_content`) and pushed content stays read from the
// snapshot. Shared by both peers so only paint differs (design §11).

/// A pane modeline's resolved per-zone layout (ML.5): the ordered
/// element descriptors for each zone after applying the `ui.modeline.*`
/// config, plus the configured inter-element separator. Descriptors are
/// borrowed from the snapshot registry (`'a`).
pub struct ModelineLayout<'a> {
    pub left: Vec<&'a ModelineElement>,
    pub center: Vec<&'a ModelineElement>,
    pub right: Vec<&'a ModelineElement>,
    /// The effective inter-element separator the renderer inserts within
    /// a zone — already padded: a non-blank `ui.modeline.separator` is
    /// surrounded by a space each side (`|` → ` | `), a blank one is a
    /// single space. So the renderer inserts it verbatim.
    pub separator: String,
    /// `ui.modeline.padding` — columns of blank margin at the start
    /// (before Left) and end (after Right) of the row.
    pub padding: usize,
}

/// Resolve `cfg` for one `zone` against `registry`. `Auto` → descriptor
/// placement (`zone_ordered`) minus ids `claimed` by an explicit zone;
/// explicit `Ids` → exactly those registered ids in order (unknown ids
/// skipped + logged, never panic). Free fn (not a closure) so the
/// `'a` borrow from `registry` is expressed cleanly.
fn resolve_zone_descriptors<'a>(
    registry: &'a ModelineRegistry,
    zone: Zone,
    cfg: &ModelineZone,
    claimed: &HashSet<String>,
) -> Vec<&'a ModelineElement> {
    match cfg.ids() {
        Some(ids) => ids
            .iter()
            .filter_map(|id| {
                let el = registry.get(&ElementId::new(id.as_ref()));
                if el.is_none() {
                    tracing::debug!(
                        target: "modeline",
                        id = id.as_ref(),
                        "ui.modeline.* references an unregistered element id; skipping"
                    );
                }
                el
            })
            .collect(),
        None => registry
            .zone_ordered(zone)
            .into_iter()
            .filter(|el| !claimed.contains(el.id.as_str()))
            .collect(),
    }
}

/// Read a zone option's current value (cloned out of the wait-free
/// snapshot), defaulting to `Auto` when the registry hasn't been seeded
/// (tests / early boot).
fn zone_option<D>(config: &ConfigRegistry) -> ModelineZone
where
    D: lattice_config::OptionDecl<Value = ModelineZone>,
{
    config
        .get_typed::<D>()
        .map(|a| (*a).clone())
        .unwrap_or_default()
}

/// Resolve the full per-zone modeline layout for a pane from the
/// descriptor registry + the `ui.modeline.{left,center,right,separator}`
/// typed options (ML.5; design §11).
///
/// Per zone:
/// - **`Auto`** (the default) → descriptor-driven: `zone_ordered(zone)`,
///   minus any element id claimed by an *explicit* (non-`Auto`) other
///   zone, so a moved element never double-renders. With no config at
///   all every zone is `Auto` ⇒ exactly the pre-ML.5 descriptor layout.
/// - **explicit `Ids`** → exactly those registered ids, in listed order;
///   unknown / unregistered ids are skipped + logged (`debug!`, never
///   panic). An empty list is an explicitly-blank zone.
///
/// The renderer still resolves each descriptor's *content* itself
/// (built-ins host-side via [`resolve_builtin_content`], pushed via the
/// snapshot) and applies the `separator`; this fn owns only the
/// membership + order decision so both peers share it.
pub fn resolve_layout<'a>(
    registry: &'a ModelineRegistry,
    config: &ConfigRegistry,
) -> ModelineLayout<'a> {
    let left_cfg = zone_option::<ModelineLeft>(config);
    let center_cfg = zone_option::<ModelineCenter>(config);
    let right_cfg = zone_option::<ModelineRight>(config);

    // Effective separator: a non-blank glyph is auto-padded with a
    // space each side (so `|` renders as ` | ` — the user gives the
    // glyph, the renderer owns the spacing, sidestepping the `:set` /
    // TOML whitespace-trim). A blank separator is a single space.
    let raw_sep = config
        .get_typed::<ModelineSeparator>()
        .map(|a| (*a).clone())
        .unwrap_or_else(|| " ".to_string());
    let trimmed_sep = raw_sep.trim();
    let separator = if trimmed_sep.is_empty() {
        " ".to_string()
    } else {
        format!(" {trimmed_sep} ")
    };

    // Start/end row margin (`ui.modeline.padding`, default 1; validated
    // 0..=16). Clamp defensively in case the registry isn't seeded.
    let padding = config
        .get_typed::<ModelinePadding>()
        .map(|a| (*a).max(0) as usize)
        .unwrap_or(1);

    // Ids placed by any explicit (non-Auto) zone → removed from the Auto
    // fallback of the other zones, so moving an element into an explicit
    // zone doesn't leave a duplicate in its descriptor zone.
    let mut claimed: HashSet<String> = HashSet::new();
    for cfg in [&left_cfg, &center_cfg, &right_cfg] {
        if let Some(ids) = cfg.ids() {
            claimed.extend(ids.iter().map(|id| id.as_ref().to_string()));
        }
    }

    ModelineLayout {
        left: resolve_zone_descriptors(registry, Zone::Left, &left_cfg, &claimed),
        center: resolve_zone_descriptors(registry, Zone::Center, &center_cfg, &claimed),
        right: resolve_zone_descriptors(registry, Zone::Right, &right_cfg, &claimed),
        separator,
        padding,
    }
}

/// ML.4: one clickable region of a painted modeline row.
///
/// Coordinates are **absolute terminal cells**, not pane- or
/// zone-relative, because that is the space a mouse event arrives in
/// and translating once at record time beats translating at every
/// hit test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelineHitZone {
    /// Terminal row the modeline occupies.
    pub row: u16,
    /// First column of the region.
    pub col_start: u16,
    /// One past the last column.
    pub col_end: u16,
    /// The command a click dispatches. The handler body lives in the
    /// mode or plugin that registered the element — the host routes
    /// and nothing more (modeline.md §6, §9).
    pub action: CommandId,
}

/// ML.4: every clickable region on screen, rebuilt each frame.
///
/// This is the TUI's half of the interaction contract. The GPUI peer
/// needs no equivalent — its elements are real `div`s with their own
/// `on_mouse_down` listeners, so the window system does the hit test.
/// The two peers therefore reach the same behaviour by different
/// mechanisms, which is what modeline.md §9 specifies rather than an
/// accident of implementation: a terminal has no element tree to
/// attach a listener to.
///
/// Later zones win a tie. Nothing in the layout produces overlapping
/// regions today, but "last write wins" is the rule a painter follows,
/// so a hit test that disagreed with what is on screen would be the
/// bug.
#[derive(Debug, Clone, Default)]
pub struct ModelineHitMap {
    zones: Vec<ModelineHitZone>,
}

impl ModelineHitMap {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every recorded region. Called at the top of each frame —
    /// a stale map would dispatch clicks against a layout that is no
    /// longer painted.
    pub fn clear(&mut self) {
        self.zones.clear();
    }

    pub fn push(&mut self, zone: ModelineHitZone) {
        // An empty or inverted region can never be hit; keeping it
        // would only make the map longer to walk.
        if zone.col_end > zone.col_start {
            self.zones.push(zone);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.zones.is_empty()
    }

    pub fn len(&self) -> usize {
        self.zones.len()
    }

    /// The command at `(col, row)`, if any.
    pub fn hit(&self, col: u16, row: u16) -> Option<CommandId> {
        self.zones
            .iter()
            .rev()
            .find(|z| z.row == row && col >= z.col_start && col < z.col_end)
            .map(|z| z.action)
    }
}

#[cfg(test)]
mod hit_tests {
    use super::*;

    fn zone(row: u16, start: u16, end: u16, action: u64) -> ModelineHitZone {
        ModelineHitZone {
            row,
            col_start: start,
            col_end: end,
            action: CommandId(action),
        }
    }

    #[test]
    fn a_click_inside_a_region_finds_its_command() {
        let mut map = ModelineHitMap::new();
        map.push(zone(23, 4, 10, 7));
        assert_eq!(map.hit(4, 23), Some(CommandId(7)), "left edge is inside");
        assert_eq!(map.hit(9, 23), Some(CommandId(7)), "last column is inside");
    }

    #[test]
    fn the_end_column_is_exclusive() {
        let mut map = ModelineHitMap::new();
        map.push(zone(23, 4, 10, 7));
        assert_eq!(map.hit(10, 23), None, "col_end belongs to the next region");
        assert_eq!(map.hit(3, 23), None);
    }

    #[test]
    fn a_click_on_another_row_misses() {
        let mut map = ModelineHitMap::new();
        map.push(zone(23, 4, 10, 7));
        assert_eq!(map.hit(5, 22), None);
        assert_eq!(map.hit(5, 24), None);
    }

    #[test]
    fn adjacent_regions_do_not_bleed() {
        let mut map = ModelineHitMap::new();
        map.push(zone(1, 0, 5, 100));
        map.push(zone(1, 5, 9, 200));
        assert_eq!(map.hit(4, 1), Some(CommandId(100)));
        assert_eq!(map.hit(5, 1), Some(CommandId(200)));
    }

    #[test]
    fn separate_panes_keep_separate_rows_and_columns() {
        // A vertical split: two modelines on the same row, different
        // column spans. Clicking one must not reach the other.
        let mut map = ModelineHitMap::new();
        map.push(zone(23, 2, 8, 1));
        map.push(zone(23, 42, 48, 2));
        assert_eq!(map.hit(5, 23), Some(CommandId(1)));
        assert_eq!(map.hit(45, 23), Some(CommandId(2)));
        assert_eq!(map.hit(20, 23), None, "the gap between panes is dead");
    }

    #[test]
    fn empty_and_inverted_regions_are_not_recorded() {
        let mut map = ModelineHitMap::new();
        map.push(zone(1, 5, 5, 1));
        map.push(zone(1, 9, 4, 2));
        assert!(map.is_empty());
        assert_eq!(map.hit(5, 1), None);
    }

    #[test]
    fn clear_drops_the_previous_frames_regions() {
        let mut map = ModelineHitMap::new();
        map.push(zone(1, 0, 5, 1));
        map.clear();
        assert_eq!(
            map.hit(2, 1),
            None,
            "a stale map would dispatch against a layout no longer painted"
        );
    }

    #[test]
    fn a_later_region_wins_an_overlap() {
        let mut map = ModelineHitMap::new();
        map.push(zone(1, 0, 10, 1));
        map.push(zone(1, 4, 6, 2));
        assert_eq!(
            map.hit(5, 1),
            Some(CommandId(2)),
            "last painted wins, matching what is on screen"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ML.5 resolve_layout -----------------------------------------

    /// A registry holding just the four `core.*` built-ins (diff / lsp
    /// are owner-registered at boot; not needed for layout tests).
    fn builtin_registry() -> std::sync::Arc<ModelineRegistry> {
        let svc = ModelineService::new();
        register_builtin_elements(&svc);
        svc.snapshot().registry
    }

    /// A config registry with every workspace option (incl. the real
    /// `ui.modeline.*`) seeded at their defaults (all zones `Auto`).
    fn modeline_config() -> ConfigRegistry {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        r
    }

    fn zone_ids(els: &[&ModelineElement]) -> Vec<String> {
        els.iter().map(|e| e.id.as_str().to_string()).collect()
    }

    /// ML.4 parity contract. Both renderer peers reach a click target
    /// the same way — `el.interaction.on_click` on the descriptors
    /// `resolve_layout` hands back — and differ only in what they do
    /// with it (TUI records painted column ranges; GPUI hangs an
    /// `on_mouse_down` on the element's own `div`). Pinning it here,
    /// upstream of both, is what makes "both peers honour on_click" a
    /// property of the shared resolver rather than two implementations
    /// that happen to agree today.
    #[test]
    fn resolve_layout_preserves_a_declared_click_target() {
        let svc = ModelineService::new();
        register_builtin_elements(&svc);
        svc.register(
            ModelineElement::new(ElementId::new("clicky"), Zone::Right, 5).with_interaction(
                lattice_mode::Interaction {
                    on_click: Some(CommandId(42)),
                    hover: None,
                },
            ),
        );
        let registry = svc.snapshot().registry;
        let cfg = modeline_config();
        let layout = resolve_layout(&registry, &cfg);

        let el = layout
            .right
            .iter()
            .find(|e| e.id.as_str() == "clicky")
            .expect("the registered element is placed in its zone");
        assert_eq!(
            el.interaction.as_ref().and_then(|i| i.on_click),
            Some(CommandId(42)),
            "the descriptor both peers read must carry the click target"
        );
    }

    /// The overwhelmingly common case: an element that declared no
    /// interaction stays inert, so neither peer records a region or
    /// attaches a listener for it.
    #[test]
    fn builtins_declare_no_click_target() {
        let registry = builtin_registry();
        let cfg = modeline_config();
        let layout = resolve_layout(&registry, &cfg);
        for el in layout.left.iter().chain(layout.right.iter()) {
            assert!(
                el.interaction.is_none(),
                "{} must not be clickable by default",
                el.id.as_str()
            );
        }
    }

    /// No config (all zones `Auto`) ⇒ the pre-ML.5 descriptor layout,
    /// and the default separator is a single space.
    #[test]
    fn resolve_layout_auto_matches_descriptor_order() {
        let reg = builtin_registry();
        let cfg = modeline_config();
        let layout = resolve_layout(&reg, &cfg);
        assert_eq!(zone_ids(&layout.left), ["core.mode", "core.path"]);
        assert_eq!(zone_ids(&layout.right), ["core.position", "core.lang"]);
        assert!(layout.center.is_empty());
        assert_eq!(layout.separator, " ");
    }

    /// An explicit zone list reorders membership (by listed order, not
    /// descriptor priority) — proves a live `:set` takes effect on the
    /// next resolve.
    #[test]
    fn resolve_layout_explicit_list_reorders() {
        let reg = builtin_registry();
        let cfg = modeline_config();
        cfg.parse_and_set_command("ui.modeline.right=core.lang,core.position")
            .unwrap();
        let layout = resolve_layout(&reg, &cfg);
        // Listed order, not the descriptor priority order.
        assert_eq!(zone_ids(&layout.right), ["core.lang", "core.position"]);
    }

    /// Unknown / unregistered ids in an explicit list are skipped (and
    /// logged) — never panic.
    #[test]
    fn resolve_layout_skips_unknown_ids() {
        let reg = builtin_registry();
        let cfg = modeline_config();
        cfg.parse_and_set_command("ui.modeline.left=core.mode,does.not.exist,core.path")
            .unwrap();
        let layout = resolve_layout(&reg, &cfg);
        assert_eq!(zone_ids(&layout.left), ["core.mode", "core.path"]);
    }

    /// Moving an element into an explicit zone removes it from the
    /// Auto fallback of its descriptor zone (no double-render).
    #[test]
    fn resolve_layout_claimed_id_drops_from_auto_zone() {
        let reg = builtin_registry();
        let cfg = modeline_config();
        // Move core.position into an explicit Left; leave Right Auto.
        cfg.parse_and_set_command("ui.modeline.left=core.mode,core.path,core.position")
            .unwrap();
        let layout = resolve_layout(&reg, &cfg);
        assert_eq!(
            zone_ids(&layout.left),
            ["core.mode", "core.path", "core.position"]
        );
        // Right is Auto, but core.position is claimed by Left → only lang.
        assert_eq!(zone_ids(&layout.right), ["core.lang"]);
    }

    /// An explicitly-empty list (`[]` / `:set ui.modeline.right=`)
    /// blanks the zone, distinct from `Auto`.
    #[test]
    fn resolve_layout_empty_list_blanks_zone() {
        let reg = builtin_registry();
        let cfg = modeline_config();
        cfg.parse_and_set_command("ui.modeline.right=").unwrap();
        let layout = resolve_layout(&reg, &cfg);
        assert!(layout.right.is_empty(), "explicit empty Right zone");
        // Left untouched (still Auto / descriptor-driven).
        assert_eq!(zone_ids(&layout.left), ["core.mode", "core.path"]);
    }

    /// A non-blank `ui.modeline.separator` is **auto-padded** with a
    /// space on each side — the user gives the glyph, the renderer owns
    /// the spacing (sidesteps the `:set` / TOML whitespace-trim, which
    /// is why `|` and ` | ` previously rendered identically).
    #[test]
    fn resolve_layout_auto_pads_glyph_separator() {
        let reg = builtin_registry();
        let cfg = modeline_config();
        cfg.parse_and_set_command("ui.modeline.separator=|")
            .unwrap();
        assert_eq!(resolve_layout(&reg, &cfg).separator, " | ");
        // ` | ` trims to `|` then re-pads to the same — no double space.
        cfg.parse_and_set_command("ui.modeline.separator= | ")
            .unwrap();
        assert_eq!(resolve_layout(&reg, &cfg).separator, " | ");
        // A blank separator stays a single space (the default look).
        cfg.parse_and_set_command("ui.modeline.separator= ")
            .unwrap();
        assert_eq!(resolve_layout(&reg, &cfg).separator, " ");
    }

    /// `ui.modeline.padding` defaults to 1 and flows into the layout.
    #[test]
    fn resolve_layout_reads_padding_default_and_set() {
        let reg = builtin_registry();
        let cfg = modeline_config();
        assert_eq!(resolve_layout(&reg, &cfg).padding, 1, "default padding");
        cfg.parse_and_set_command("ui.modeline.padding=3").unwrap();
        assert_eq!(resolve_layout(&reg, &cfg).padding, 3);
        cfg.parse_and_set_command("ui.modeline.padding=0").unwrap();
        assert_eq!(resolve_layout(&reg, &cfg).padding, 0, "flush to edges");
    }

    /// Boot registers the four built-ins with the spec'd zones +
    /// priorities (modeline.md §3 / slice plan ML.1a-render).
    #[test]
    fn boot_registers_builtin_descriptors() {
        let document = lattice_core::Document::empty();
        let editor = crate::editor::Editor::boot(document);
        let snap = editor.modeline.snapshot();
        let reg = &snap.registry;
        // Four `core.*` built-ins + the diff subsystem's `diff` element
        // (ML.3b) + lattice-lsp's `lsp` element (ML.3c), all registered at
        // boot by their owners.
        assert_eq!(reg.len(), 6, "four core built-ins + diff + lsp");

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
        let pane = *editor.pane_tree.active();

        // ML.5d: lean 3-letter tag, no brackets.
        let active_mode = resolve_builtin_content(CORE_MODE, &pane, true, &rs, None);
        assert_eq!(active_mode.plain(), "NOR");
        let inactive_mode = resolve_builtin_content(CORE_MODE, &pane, false, &rs, None);
        assert!(inactive_mode.is_empty(), "mode hidden on inactive panes");

        // Position renders regardless of active/inactive; row 1, col 0.
        let pos = resolve_builtin_content(CORE_POSITION, &pane, false, &rs, None);
        assert_eq!(pos.plain(), "1:0");
    }

    /// Regression: `core.position` on the ACTIVE pane tracks the live
    /// cursor (`self.cursor` → `rs.active_document.cursor`), not the
    /// pane-tree stash (`pane.cursor`), which is only written back when
    /// the pane goes inactive. Before the fix the modeline froze at the
    /// stashed position while the caret moved.
    #[test]
    fn builtin_position_tracks_live_cursor_on_active_pane() {
        let document = lattice_core::Document::empty();
        let mut editor = crate::editor::Editor::boot(document);
        // Move the live cursor without switching panes, so the pane-tree
        // stash stays at origin (motion doesn't sync it back).
        editor.set_cursor(lattice_protocol::position::Position::new(1, 4));
        let rs = editor.build_render_state();
        let pane = *editor.pane_tree.active();

        // The pane-tree stash is still at origin.
        assert_eq!(pane.cursor, lattice_protocol::position::Position::ZERO);

        // Active pane reads the live cursor: line 2 (1-based), byte 4.
        let active = resolve_builtin_content(CORE_POSITION, &pane, true, &rs, None);
        assert_eq!(active.plain(), "2:4");

        // Inactive panes keep their own stashed cursor.
        let inactive = resolve_builtin_content(CORE_POSITION, &pane, false, &rs, None);
        assert_eq!(inactive.plain(), "1:0");
    }

    /// A provider label overrides `core.path`; without one, the empty
    /// scratch document falls back to the registry name slot.
    #[test]
    fn builtin_path_honours_provider_label() {
        let document = lattice_core::Document::empty();
        let mut editor = crate::editor::Editor::boot(document);
        let rs = editor.build_render_state();
        let pane = *editor.pane_tree.active();

        let overridden = resolve_builtin_content(CORE_PATH, &pane, true, &rs, Some("[tree] /root"));
        assert_eq!(overridden.plain(), "[tree] /root");

        // No provider: a fresh scratch buffer has no path; the segment
        // is non-empty (a name slot or "[no name]"), never a panic.
        let fallback = resolve_builtin_content(CORE_PATH, &pane, true, &rs, None);
        assert!(!fallback.plain().is_empty());
    }
}
