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
#[derive(Clone, Default)]
pub struct DashboardRegistry {
    sections: Vec<Arc<dyn DashboardSection>>,
}

impl DashboardRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a section. A later registration with the same `id` replaces
    /// the earlier one (this is the DB.8 plugin replace-by-id semantics, and
    /// keeps a re-run idempotent).
    pub fn register(&mut self, section: Arc<dyn DashboardSection>) -> &mut Self {
        let id = section.id().to_string();
        if let Some(slot) = self.sections.iter_mut().find(|s| s.id() == id) {
            *slot = section;
        } else {
            self.sections.push(section);
        }
        self
    }

    /// All registered ids (registration order; not the display order).
    pub fn ids(&self) -> Vec<&str> {
        self.sections.iter().map(|s| s.id()).collect()
    }

    /// Resolve the display order for a selection.
    ///
    /// Unknown ids in an explicit selection are skipped with a logged
    /// warning (never a hard error — a stale `dashboard.sections` entry must
    /// not break the page).
    pub fn ordered(&self, selection: &SectionSelection) -> Vec<Arc<dyn DashboardSection>> {
        match selection {
            SectionSelection::Default => {
                let mut enabled: Vec<Arc<dyn DashboardSection>> = self
                    .sections
                    .iter()
                    .filter(|s| s.default_enabled())
                    .cloned()
                    .collect();
                enabled.sort_by(|a, b| a.order().cmp(&b.order()).then_with(|| a.id().cmp(b.id())));
                enabled
            }
            SectionSelection::Explicit(ids) => ids
                .iter()
                .filter_map(|id| {
                    let found = self.sections.iter().find(|s| s.id() == id).cloned();
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
        fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
            let mut f = DashboardFragment::new();
            f.line(self.id, DashboardRole::Body);
            f
        }
    }

    fn stub(id: &'static str, order: i32, enabled: bool) -> Arc<dyn DashboardSection> {
        Arc::new(StubSection { id, order, enabled })
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
        assert_eq!(
            ordered_ids(&reg, &SectionSelection::Default),
            ["branding"]
        );
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
