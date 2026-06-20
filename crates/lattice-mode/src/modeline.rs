//! Modeline element model + descriptor registry (slice ML.0a).
//!
//! The configurable modeline (see
//! `docs/dev/architecture/modeline.md`) is a registry of styled,
//! positioned, optionally-interactive **elements** contributed by host
//! built-ins, modes, and (later) plugins. This module holds the
//! mode-facing data model + the descriptor registry.
//!
//! Split of concerns (mirrors `lsp_progress`): the **descriptor**
//! ([`ModelineElement`]) is registered once and changes rarely; the
//! **content** ([`ElementContent`]) churns and lives in the host
//! content store, published as a render snapshot and updated over the
//! event bus (ML.0b / ML.3). The renderers lay out zones (ML.1 / ML.2).
//! Interaction ([`Interaction`]) is *designed here* but wired in ML.4 —
//! shipping the field now keeps that slice additive (no model churn).

use std::collections::HashMap;
use std::sync::Arc;

use lattice_protocol::ids::CommandId;

/// Stable, namespaced element identifier — `"core.mode"`, `"lsp"`,
/// `"<plugin-id>.<name>"`. The namespace doubles as the **owner** key
/// (`feedback_mode_owns_its_surface`): a mode/plugin owns the elements
/// under its namespace end to end.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ElementId(pub Arc<str>);

impl ElementId {
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self(id.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Horizontal placement zone. `Left` fills left→right, `Right` fills
/// right→left, `Center` sits in the gap between them and is the default
/// zone for custom / plugin content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Left,
    Center,
    Right,
}

/// Whether an element renders on every pane (`PaneLocal`, default) or
/// only the active pane (`Global`). `Global` carries project-wide
/// content (clock, git branch) without per-pane duplication and without
/// reintroducing a global chrome bar (Option A stays).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    PaneLocal,
    Global,
}

/// Theme role key for a [`Span`]. Resolved by the renderer against the
/// `ResolvedTheme` (T-series). Kept as a string key so `lattice-mode`
/// need not depend on the theme crate (dep-inversion, same pattern as
/// the service registry). Unknown roles fall back to the default
/// modeline style at render time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelineRole(pub Arc<str>);

impl ModelineRole {
    pub fn new(role: impl Into<Arc<str>>) -> Self {
        Self(role.into())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A styled run of text within an element's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub role: ModelineRole,
}

impl Span {
    pub fn new(text: impl Into<String>, role: ModelineRole) -> Self {
        Self {
            text: text.into(),
            role,
        }
    }
}

/// The dynamic, frequently-updated value of an element. Empty (no
/// non-empty span text) ⇒ the element is hidden this frame — the cheap
/// way a producer hides itself without deregistering.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ElementContent {
    pub spans: Vec<Span>,
}

impl ElementContent {
    /// Convenience: a single-span content.
    pub fn text(text: impl Into<String>, role: ModelineRole) -> Self {
        Self {
            spans: vec![Span::new(text, role)],
        }
    }

    /// True when there is nothing to paint (no spans, or all blank).
    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(|s| s.text.is_empty())
    }

    /// Plain concatenated text — renderer-agnostic; used for width
    /// estimation + tests.
    pub fn plain(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// Hover payload — a GPUI tooltip; ignored in the terminal (no hover).
/// Realized in ML.4.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverSpec {
    pub content: ElementContent,
}

/// Interaction spec — **designed in ML.0, behaviour wired in ML.4**.
/// `on_click` is dispatched through the host action registry; the
/// handler body lives in the registering mode/plugin crate
/// (`feedback_mode_owns_its_surface`,
/// `feedback_effect_vocabulary_is_host_boundary`) — the host is only a
/// router. `hover` is GPUI-only.
#[derive(Debug, Clone, Default)]
pub struct Interaction {
    pub on_click: Option<CommandId>,
    pub hover: Option<HoverSpec>,
}

/// Static descriptor for a modeline element. Registered once into the
/// [`ModelineRegistry`]; its [`ElementContent`] lives separately in the
/// host content store and updates over the event bus (ML.3).
#[derive(Debug, Clone)]
pub struct ModelineElement {
    pub id: ElementId,
    pub zone: Zone,
    /// Order within the zone (see [`ModelineRegistry::zone_ordered`]).
    pub priority: i32,
    pub scope: Scope,
    /// Designed now; honoured by the renderer in ML.4.
    pub interaction: Option<Interaction>,
}

impl ModelineElement {
    /// Minimal descriptor: pane-local, no interaction.
    pub fn new(id: ElementId, zone: Zone, priority: i32) -> Self {
        Self {
            id,
            zone,
            priority,
            scope: Scope::PaneLocal,
            interaction: None,
        }
    }

    pub fn with_scope(mut self, scope: Scope) -> Self {
        self.scope = scope;
        self
    }

    pub fn with_interaction(mut self, interaction: Interaction) -> Self {
        self.interaction = Some(interaction);
        self
    }
}

/// Descriptor registry. Host-owned storage; modes register in
/// `on_activate` and remove in `on_deactivate` (plugins via WIT, ML.6).
/// Holds only descriptors — the churning content lives in the host
/// content store, not here, so registration is rare and cheap.
#[derive(Debug, Default)]
pub struct ModelineRegistry {
    elements: HashMap<ElementId, ModelineElement>,
}

impl ModelineRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) a descriptor. Last-write-wins on a
    /// duplicate id — well-defined, matching the action-handler
    /// registry's semantics.
    pub fn register(&mut self, element: ModelineElement) {
        self.elements.insert(element.id.clone(), element);
    }

    /// Remove a descriptor (idempotent). Returns the removed element.
    pub fn remove(&mut self, id: &ElementId) -> Option<ModelineElement> {
        self.elements.remove(id)
    }

    pub fn get(&self, id: &ElementId) -> Option<&ModelineElement> {
        self.elements.get(id)
    }

    pub fn len(&self) -> usize {
        self.elements.len()
    }

    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// Descriptors in `zone`, ordered for rendering: `Left` / `Center`
    /// ascending by `priority` (left→right); `Right` descending (so the
    /// highest-priority element sits at the far right). Ties break by
    /// `ElementId` for deterministic layout.
    pub fn zone_ordered(&self, zone: Zone) -> Vec<&ModelineElement> {
        let mut v: Vec<&ModelineElement> =
            self.elements.values().filter(|e| e.zone == zone).collect();
        v.sort_by(|a, b| {
            let ord = a.priority.cmp(&b.priority);
            let ord = if zone == Zone::Right { ord.reverse() } else { ord };
            ord.then_with(|| a.id.0.cmp(&b.id.0))
        });
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(id: &str, zone: Zone, priority: i32) -> ModelineElement {
        ModelineElement::new(ElementId::new(id), zone, priority)
    }

    #[test]
    fn register_get_len() {
        let mut r = ModelineRegistry::new();
        assert!(r.is_empty());
        r.register(el("core.mode", Zone::Left, 0));
        r.register(el("lsp", Zone::Right, 10));
        assert_eq!(r.len(), 2);
        assert_eq!(r.get(&ElementId::new("lsp")).unwrap().zone, Zone::Right);
        assert!(r.get(&ElementId::new("missing")).is_none());
    }

    #[test]
    fn register_replaces_duplicate_id() {
        let mut r = ModelineRegistry::new();
        r.register(el("lsp", Zone::Left, 0));
        r.register(el("lsp", Zone::Right, 5)); // same id, new descriptor
        assert_eq!(r.len(), 1);
        let e = r.get(&ElementId::new("lsp")).unwrap();
        assert_eq!(e.zone, Zone::Right);
        assert_eq!(e.priority, 5);
    }

    #[test]
    fn remove_is_idempotent() {
        let mut r = ModelineRegistry::new();
        r.register(el("lsp", Zone::Left, 0));
        assert!(r.remove(&ElementId::new("lsp")).is_some());
        assert!(r.remove(&ElementId::new("lsp")).is_none());
        assert!(r.is_empty());
    }

    #[test]
    fn zone_ordered_left_ascending_right_descending() {
        let mut r = ModelineRegistry::new();
        r.register(el("l.b", Zone::Left, 20));
        r.register(el("l.a", Zone::Left, 10));
        r.register(el("r.b", Zone::Right, 20));
        r.register(el("r.a", Zone::Right, 10));
        r.register(el("c", Zone::Center, 0));

        let left: Vec<&str> = r
            .zone_ordered(Zone::Left)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(left, ["l.a", "l.b"]); // ascending

        let right: Vec<&str> = r
            .zone_ordered(Zone::Right)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(right, ["r.b", "r.a"]); // descending (top priority far right)

        let center: Vec<&str> = r
            .zone_ordered(Zone::Center)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(center, ["c"]);
    }

    #[test]
    fn zone_ordered_ties_broken_by_id() {
        let mut r = ModelineRegistry::new();
        r.register(el("b", Zone::Left, 5));
        r.register(el("a", Zone::Left, 5));
        let left: Vec<&str> = r
            .zone_ordered(Zone::Left)
            .iter()
            .map(|e| e.id.as_str())
            .collect();
        assert_eq!(left, ["a", "b"]); // tie → id ascending
    }

    #[test]
    fn content_empty_and_plain() {
        let role = ModelineRole::new("modeline.normal");
        assert!(ElementContent::default().is_empty());
        let c = ElementContent {
            spans: vec![Span::new("lsp ", role.clone()), Span::new("✓", role)],
        };
        assert!(!c.is_empty());
        assert_eq!(c.plain(), "lsp ✓");
    }

    #[test]
    fn descriptor_builders() {
        let e = ModelineElement::new(ElementId::new("x"), Zone::Center, 0)
            .with_scope(Scope::Global)
            .with_interaction(Interaction::default());
        assert_eq!(e.scope, Scope::Global);
        assert!(e.interaction.is_some());
        assert_eq!(e.zone, Zone::Center);
    }
}
