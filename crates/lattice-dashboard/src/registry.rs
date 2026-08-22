//! The section registry — the single extensibility seam.
//!
//! Config selects and orders built-in sections today; the same registry will
//! accept plugin-contributed sections tomorrow (DB.8). Composition is: pick +
//! order the enabled sections, render each to a fragment. Turning fragments
//! into buffer content is the compositor's job (DB.2+).

use std::sync::Arc;

use crate::fragment::DashboardFragment;
use crate::section::{DashboardCtx, DashboardSection};

/// Which sections to show, and in what order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionSelection {
    /// User did not customise: every `default_enabled` section, sorted by
    /// `order` (ties by `id`).
    Default,
    /// User pinned an explicit ordered list of ids (from `dashboard.sections`).
    /// Only these show, in this order; unknown ids are skipped with a warning.
    Explicit(Vec<String>),
}

impl SectionSelection {
    /// Parse the `dashboard.sections` option value. Empty/whitespace ⇒
    /// [`SectionSelection::Default`]; otherwise the ids split on commas
    /// and/or whitespace, in order, de-duplicated (first occurrence wins).
    pub fn parse(raw: &str) -> Self {
        let mut ids: Vec<String> = Vec::new();
        for tok in raw.split([',', ' ', '\t', '\n']) {
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            if !ids.iter().any(|existing| existing == tok) {
                ids.push(tok.to_string());
            }
        }
        if ids.is_empty() {
            SectionSelection::Default
        } else {
            SectionSelection::Explicit(ids)
        }
    }
}

/// Ordered, id-keyed collection of sections.
///
/// **Shadowing, not overwriting.** A later registration for an id that is
/// already taken is *appended*, and the id resolves to the LAST entry.
/// That makes replace-by-id (CR.4's stated capability — a plugin replacing
/// `getting-started`) a stack rather than a destructive write, so
/// [`unregister_plugin`](Self::unregister_plugin) is a plain `retain` and
/// the displaced builtin resurfaces on its own. The alternative —
/// overwrite, and save the previous occupant somewhere for unload to put
/// back — is explicit bookkeeping across three unload paths, and the kind
/// that gets forgotten on one of them.
#[derive(Clone, Default)]
pub struct DashboardRegistry {
    sections: Vec<Arc<dyn DashboardSection>>,
}

/// The runtime-mutable handle, registered as a boot service under this
/// exact alias (the `ServiceRegistry` Arc/TypeId convention).
///
/// Copy-on-write RCU: `Editor::compose_dashboard_sections` takes one
/// wait-free `.load()` snapshot per compose; writes happen only on plugin
/// load and unload.
pub type DashboardRegistryHandle = Arc<arc_swap::ArcSwap<DashboardRegistry>>;

impl DashboardRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Wrap this registry in a fresh [`DashboardRegistryHandle`], so
    /// consumers do not each have to name `arc_swap`.
    pub fn into_handle(self) -> DashboardRegistryHandle {
        Arc::new(arc_swap::ArcSwap::from_pointee(self))
    }

    /// Register a section.
    ///
    /// A registration for an id already held by a **different** owner
    /// shadows it (see the type docs). A registration by the **same** owner
    /// for the same id replaces in place, so re-running `builtin_registry`
    /// stays idempotent and a plugin reload cannot grow the shadow stack.
    pub fn register(&mut self, section: Arc<dyn DashboardSection>) -> &mut Self {
        let id = section.id().to_string();
        let owner = section.plugin_id();
        if let Some(slot) = self
            .sections
            .iter_mut()
            .rev()
            .find(|s| s.id() == id && s.plugin_id() == owner)
        {
            *slot = section;
        } else {
            self.sections.push(section);
        }
        self
    }

    /// Drop every section contributed by `plugin_id`, returning how many
    /// were removed. Idempotent: a second call reports zero.
    ///
    /// A builtin the plugin had shadowed becomes visible again with no
    /// restore step, because it was never removed — that is the whole
    /// reason [`register`](Self::register) appends.
    pub fn unregister_plugin(&mut self, plugin_id: u64) -> usize {
        let before = self.sections.len();
        self.sections.retain(|s| s.plugin_id() != Some(plugin_id));
        before - self.sections.len()
    }

    /// The section that currently owns `id` — the last registration for it.
    pub fn resolve(&self, id: &str) -> Option<&Arc<dyn DashboardSection>> {
        self.sections.iter().rev().find(|s| s.id() == id)
    }

    /// All registered ids, de-duplicated, in FIRST-registration order (not
    /// the display order).
    ///
    /// First-registration order rather than last: a plugin replacing a
    /// builtin keeps the slot the builtin occupied, so loading a plugin
    /// does not reshuffle the list a user reads.
    pub fn ids(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in &self.sections {
            if !out.contains(&s.id()) {
                out.push(s.id());
            }
        }
        out
    }

    /// Resolve the display order for a selection.
    ///
    /// Unknown ids in an explicit selection are skipped with a logged
    /// warning (never a hard error — a stale `dashboard.sections` entry must
    /// not break the page).
    pub fn ordered(&self, selection: &SectionSelection) -> Vec<Arc<dyn DashboardSection>> {
        match selection {
            SectionSelection::Default => {
                // Resolve each distinct id to its current owner first — a
                // shadowed builtin must not render alongside the plugin
                // section that replaced it.
                let mut enabled: Vec<Arc<dyn DashboardSection>> = self
                    .ids()
                    .into_iter()
                    .filter_map(|id| self.resolve(id))
                    .filter(|s| s.default_enabled())
                    .cloned()
                    .collect();
                enabled.sort_by(|a, b| a.order().cmp(&b.order()).then_with(|| a.id().cmp(b.id())));
                enabled
            }
            SectionSelection::Explicit(ids) => ids
                .iter()
                .filter_map(|id| {
                    let found = self.resolve(id).cloned();
                    if found.is_none() {
                        tracing::warn!(
                            section = %id,
                            "dashboard.sections lists unknown section id; skipping"
                        );
                    }
                    found
                })
                .collect(),
        }
    }

    /// Render the selected sections in order to their fragments.
    pub fn compose(
        &self,
        ctx: &DashboardCtx,
        selection: &SectionSelection,
    ) -> Vec<DashboardFragment> {
        self.ordered(selection)
            .iter()
            .map(|s| s.render(ctx))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::{DashboardFragment, DashboardRole};

    struct StubSection {
        id: &'static str,
        order: i32,
        enabled: bool,
        /// What `render` emits. Distinct from `id` so a shadowing test can
        /// tell two sections claiming the SAME id apart — asserting on ids
        /// alone cannot see whether the builtin or its replacement ran.
        label: &'static str,
        plugin_id: Option<u64>,
    }

    impl DashboardSection for StubSection {
        fn id(&self) -> &str {
            self.id
        }
        fn order(&self) -> i32 {
            self.order
        }
        fn default_enabled(&self) -> bool {
            self.enabled
        }
        fn plugin_id(&self) -> Option<u64> {
            self.plugin_id
        }
        fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
            let mut f = DashboardFragment::new();
            f.line(self.label, DashboardRole::Body);
            f
        }
    }

    fn stub(id: &'static str, order: i32, enabled: bool) -> Arc<dyn DashboardSection> {
        Arc::new(StubSection {
            id,
            order,
            enabled,
            label: id,
            plugin_id: None,
        })
    }

    /// A plugin-contributed section claiming `id`, rendering `label`.
    fn plugin_stub(
        id: &'static str,
        label: &'static str,
        plugin_id: u64,
    ) -> Arc<dyn DashboardSection> {
        Arc::new(StubSection {
            id,
            order: 0,
            enabled: true,
            label,
            plugin_id: Some(plugin_id),
        })
    }

    fn rendered_labels(reg: &DashboardRegistry, sel: &SectionSelection) -> Vec<String> {
        reg.compose(&DashboardCtx::default(), sel)
            .iter()
            .flat_map(|f| f.rows.iter().map(|r| r.text()))
            .collect()
    }

    fn ordered_ids(reg: &DashboardRegistry, sel: &SectionSelection) -> Vec<String> {
        reg.ordered(sel)
            .iter()
            .map(|s| s.id().to_string())
            .collect()
    }

    #[test]
    fn default_orders_by_order_then_id() {
        let mut reg = DashboardRegistry::new();
        // Register out of order; equal order breaks by id.
        reg.register(stub("branding", 10, true));
        reg.register(stub("links", 30, true));
        reg.register(stub("about", 20, true));
        reg.register(stub("aaa", 20, true)); // ties with "about" at 20
        assert_eq!(
            ordered_ids(&reg, &SectionSelection::Default),
            ["branding", "aaa", "about", "links"]
        );
    }

    #[test]
    fn default_excludes_disabled() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("branding", 10, true));
        reg.register(stub("hidden", 20, false));
        assert_eq!(ordered_ids(&reg, &SectionSelection::Default), ["branding"]);
    }

    #[test]
    fn explicit_selects_and_reorders() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("branding", 10, true));
        reg.register(stub("about", 20, true));
        reg.register(stub("links", 30, true));
        let sel = SectionSelection::parse("links, branding");
        // Only the two listed, in listed order; "about" omitted.
        assert_eq!(ordered_ids(&reg, &sel), ["links", "branding"]);
    }

    #[test]
    fn explicit_can_show_a_default_disabled_section() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("branding", 10, true));
        reg.register(stub("hidden", 20, false));
        let sel = SectionSelection::parse("hidden branding");
        assert_eq!(ordered_ids(&reg, &sel), ["hidden", "branding"]);
    }

    #[test]
    fn explicit_skips_unknown_ids() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("branding", 10, true));
        let sel = SectionSelection::parse("nope, branding, also-nope");
        assert_eq!(ordered_ids(&reg, &sel), ["branding"]);
    }

    #[test]
    fn register_same_id_replaces() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("about", 20, true));
        reg.register(stub("about", 5, true)); // replace, new order
        assert_eq!(reg.ids(), ["about"]);
        assert_eq!(reg.ordered(&SectionSelection::Default)[0].order(), 5);
    }

    // ── CR.2: the runtime-writable handle + shadowing ────────────────

    #[test]
    fn a_plugin_section_shadows_the_builtin_with_the_same_id() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("getting-started", 10, true));
        reg.register(plugin_stub("getting-started", "from-plugin", 7));

        // One slot, not two — the shadowed builtin must not render
        // alongside its replacement.
        assert_eq!(reg.ids(), ["getting-started"]);
        assert_eq!(
            rendered_labels(&reg, &SectionSelection::Default),
            ["from-plugin"]
        );
    }

    #[test]
    fn unregister_plugin_restores_the_builtin_it_displaced() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("getting-started", 10, true));
        reg.register(stub("links", 30, true));
        reg.register(plugin_stub("getting-started", "from-plugin", 7));

        assert_eq!(reg.unregister_plugin(7), 1);
        // Back to the builtin, in the slot it always had — not dropped,
        // and not moved to the end.
        assert_eq!(
            rendered_labels(&reg, &SectionSelection::Default),
            ["getting-started", "links"]
        );
        // Idempotent: the teardown contract's double-unload case.
        assert_eq!(reg.unregister_plugin(7), 0);
    }

    #[test]
    fn two_plugins_shadowing_one_id_unwind_in_reverse_order() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("getting-started", 10, true));
        reg.register(plugin_stub("getting-started", "plugin-a", 1));
        reg.register(plugin_stub("getting-started", "plugin-b", 2));

        assert_eq!(
            rendered_labels(&reg, &SectionSelection::Default),
            ["plugin-b"]
        );
        reg.unregister_plugin(2);
        assert_eq!(
            rendered_labels(&reg, &SectionSelection::Default),
            ["plugin-a"]
        );
        reg.unregister_plugin(1);
        assert_eq!(
            rendered_labels(&reg, &SectionSelection::Default),
            ["getting-started"]
        );
    }

    /// A reload re-registers without an intervening unload in some paths;
    /// appending there would grow the shadow stack until a single unload
    /// could no longer clear it.
    #[test]
    fn the_same_owner_re_registering_does_not_grow_the_stack() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("getting-started", 10, true));
        reg.register(plugin_stub("getting-started", "v1", 7));
        reg.register(plugin_stub("getting-started", "v2", 7));

        assert_eq!(rendered_labels(&reg, &SectionSelection::Default), ["v2"]);
        assert_eq!(reg.unregister_plugin(7), 1);
        assert_eq!(
            rendered_labels(&reg, &SectionSelection::Default),
            ["getting-started"]
        );
    }

    #[test]
    fn unregister_plugin_removes_only_that_plugins_sections() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("branding", 10, true));
        reg.register(plugin_stub("a-one", "a1", 1));
        reg.register(plugin_stub("a-two", "a2", 1));
        reg.register(plugin_stub("b-one", "b1", 2));

        assert_eq!(reg.unregister_plugin(1), 2);
        assert_eq!(reg.ids(), ["branding", "b-one"]);
    }

    #[test]
    fn an_rcu_write_is_visible_through_a_handle_captured_beforehand() {
        let handle = {
            let mut reg = DashboardRegistry::new();
            reg.register(stub("branding", 10, true));
            reg.into_handle()
        };
        let captured = handle.clone();
        assert_eq!(captured.load().ids(), ["branding"]);

        handle.rcu(|current| {
            let mut next = (**current).clone();
            next.register(plugin_stub("extra", "from-plugin", 7));
            Arc::new(next)
        });

        assert_eq!(captured.load().ids(), ["branding", "extra"]);
    }

    /// Coherence: a compose that snapshotted before a plugin loaded keeps
    /// rendering the set it started from, rather than half of each.
    #[test]
    fn a_snapshot_taken_before_a_write_still_reads_the_old_set() {
        let handle = {
            let mut reg = DashboardRegistry::new();
            reg.register(stub("branding", 10, true));
            reg.into_handle()
        };
        let before = handle.load_full();

        handle.rcu(|current| {
            let mut next = (**current).clone();
            next.register(plugin_stub("extra", "from-plugin", 7));
            Arc::new(next)
        });

        assert_eq!(before.ids(), ["branding"]);
        assert_eq!(handle.load().ids(), ["branding", "extra"]);
    }

    #[test]
    fn parse_empty_is_default() {
        assert_eq!(SectionSelection::parse(""), SectionSelection::Default);
        assert_eq!(SectionSelection::parse("   \t"), SectionSelection::Default);
    }

    #[test]
    fn parse_dedupes_preserving_first() {
        assert_eq!(
            SectionSelection::parse("a, b, a"),
            SectionSelection::Explicit(vec!["a".into(), "b".into()])
        );
    }

    #[test]
    fn compose_renders_in_order() {
        let mut reg = DashboardRegistry::new();
        reg.register(stub("branding", 10, true));
        reg.register(stub("about", 20, true));
        let frags = reg.compose(&DashboardCtx::default(), &SectionSelection::Default);
        let texts: Vec<String> = frags.iter().map(|f| f.rows[0].text()).collect();
        assert_eq!(texts, ["branding", "about"]);
    }
}
