//! The `dashboard` guest→host section seam (CR.4).
//!
//! Design:
//! [`contributable-registries.md`](../../../docs/dev/architecture/contributable-registries.md)
//! §3.2, and the `dashboard` WIT interface.
//!
//! A plugin implementing `dashboard-plugin` declares its section ids once at
//! load (`register-dashboard-sections`) and renders them on demand
//! (`render-section`). The host wraps each declared id in a
//! [`WasmDashboardSection`], which IS a [`lattice_dashboard::DashboardSection`]
//! — so the registry's ordering, the `dashboard.sections` selection and the
//! compositor treat a plugin section and a built-in identically, which is what
//! the trait was written for.
//!
//! ## Sync, and holding a live `Store`
//!
//! Unlike `help` — which crosses its data once and drops the guest — a
//! section is a function of a `DashboardCtx` the guest cannot know at load, so
//! the instance stays alive for the editor's lifetime. `render` takes `&self`
//! (the trait's shape, because the registry shares sections behind `Arc`),
//! and a `Store` needs `&mut`, so the store sits behind a `Mutex`. Contention
//! is nil in practice: composition happens on the actor thread, one section at
//! a time.
//!
//! ## Guest output is untrusted
//!
//! Every row is validated host-side and dropped on failure, never trapped on.
//! A trap poisons the section — it renders nothing further this session — and
//! the rest of the page still composes, the `WasmErrorParser` contract
//! verbatim. A plugin that breaks must cost its own block, not the launch
//! page.

use std::sync::Mutex;

use lattice_dashboard::{
    Align, DashboardCtx, DashboardFragment, DashboardRole, DashboardRow, DashboardSpan, LinkTarget,
};

use crate::{Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "dashboard-plugin",
        path: "../../wit",
        // Sync exports — see the module docs. `render-section` runs inside
        // the dashboard compositor on the actor thread and must not suspend.
        with: {
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
            "lattice:plugin-host/project": crate::lattice::plugin_host::project,
        },
    });
}

use bindings::lattice::plugin_host::dashboard as wit;

/// How many rows a single guest section may contribute.
///
/// Not a safety limit — fuel already bounds the guest's *time*. This bounds
/// the output of a guest that returns quickly with an absurd row count, which
/// fuel does not catch and which would otherwise be composed into a buffer
/// and painted. 512 is far above any plausible section (the largest built-in
/// is under 20) and far below anything that would stall the compositor.
const MAX_ROWS: usize = 512;

/// What a guest declared during `register-dashboard-sections`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardSectionSpec {
    pub id: String,
    pub order: i32,
    pub default_enabled: bool,
}

/// Validate a declaration, or reject it.
///
/// The one rejection is an empty id: `dashboard.sections` addresses sections
/// by id, and a section nothing can name is one the user can neither order
/// nor disable.
pub fn validate_section(
    id: &str,
    order: i32,
    default_enabled: bool,
) -> Result<DashboardSectionSpec, String> {
    let id = id.trim();
    if id.is_empty() {
        return Err("register-section: id is empty; `dashboard.sections` \
                    addresses sections by id, so an unnamed one cannot be \
                    ordered or disabled"
            .to_string());
    }
    Ok(DashboardSectionSpec {
        id: id.to_string(),
        order,
        default_enabled,
    })
}

fn role_from_wit(r: wit::Role) -> DashboardRole {
    match r {
        wit::Role::Logo => DashboardRole::Logo,
        wit::Role::Cursor => DashboardRole::Cursor,
        wit::Role::Title => DashboardRole::Title,
        wit::Role::Tagline => DashboardRole::Tagline,
        wit::Role::SectionHeading => DashboardRole::SectionHeading,
        wit::Role::Body => DashboardRole::Body,
        wit::Role::Key => DashboardRole::Key,
        wit::Role::Hint => DashboardRole::Hint,
        wit::Role::Link => DashboardRole::Link,
    }
}

fn align_from_wit(a: wit::Align) -> Align {
    match a {
        wit::Align::Left => Align::Left,
        wit::Align::Center => Align::Center,
    }
}

/// Convert a guest link target, or drop it.
///
/// A `topic:`/`cmd:`/`url:` value that is blank would render as a live-looking
/// link that silently does nothing on `<CR>`, which is worse than plain text —
/// so the span keeps its label and loses the link rather than keeping a dead
/// one.
fn link_from_wit(t: wit::LinkTarget) -> Option<LinkTarget> {
    let (value, build): (String, fn(String) -> LinkTarget) = match t {
        wit::LinkTarget::Command(v) => (v, LinkTarget::Command),
        wit::LinkTarget::Topic(v) => (v, LinkTarget::Topic),
        wit::LinkTarget::Url(v) => (v, LinkTarget::Url),
    };
    let value = value.trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(build(value))
}

/// Convert a guest fragment to the native one, dropping what does not survive
/// validation.
///
/// `plugin` is for the log lines only. Rejections are `debug!` rather than
/// `warn!`: this runs once per section per compose, and a plugin with a
/// systematically bad row would otherwise write a line every time the user
/// opens the dashboard.
pub fn fragment_from_wit(plugin: &str, f: wit::Fragment) -> DashboardFragment {
    let mut out = DashboardFragment::new();
    if f.rows.len() > MAX_ROWS {
        tracing::debug!(
            plugin,
            rows = f.rows.len(),
            cap = MAX_ROWS,
            "dashboard section returned more rows than the cap; truncating"
        );
    }
    for row in f.rows.into_iter().take(MAX_ROWS) {
        // A row with no spans is a blank line to the compositor, which is a
        // legitimate spacer — so it is kept, not dropped. Only spans are
        // filtered.
        let spans: Vec<DashboardSpan> = row
            .spans
            .into_iter()
            .map(|s| DashboardSpan {
                text: s.text,
                role: role_from_wit(s.role),
                link: s.link.and_then(link_from_wit),
            })
            .collect();
        out.push(DashboardRow {
            spans,
            align: align_from_wit(row.align),
        });
    }
    out
}

/// A plugin-backed dashboard section.
///
/// Holds its own `Store`, instantiated once at load and kept for the editor's
/// lifetime — see the module docs for why this cannot be data.
pub struct WasmDashboardSection {
    inner: Mutex<SectionGuest>,
    spec: DashboardSectionSpec,
    plugin: String,
    plugin_id: u64,
}

struct SectionGuest {
    store: wasmtime::Store<crate::PluginState>,
    bindings: bindings::DashboardPlugin,
    /// Set once the guest traps. A trapped component is dead until reloaded
    /// (wasmtime offers no rollback), and continuing to call it would trap on
    /// every compose for the rest of the session.
    poisoned: bool,
}

impl std::fmt::Debug for WasmDashboardSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmDashboardSection")
            .field("plugin", &self.plugin)
            .field("id", &self.spec.id)
            .finish()
    }
}

impl WasmDashboardSection {
    /// The plugin this section came from — teardown removes by it.
    pub fn plugin_name(&self) -> &str {
        &self.plugin
    }
}

impl lattice_dashboard::DashboardSection for WasmDashboardSection {
    fn id(&self) -> &str {
        &self.spec.id
    }

    fn order(&self) -> i32 {
        self.spec.order
    }

    fn default_enabled(&self) -> bool {
        self.spec.default_enabled
    }

    fn plugin_id(&self) -> Option<u64> {
        Some(self.plugin_id)
    }

    fn render(&self, ctx: &DashboardCtx) -> DashboardFragment {
        let Ok(mut guest) = self.inner.lock() else {
            // A poisoned mutex means a previous render panicked. The editor
            // stays up and this section stays blank; taking the page down
            // over one plugin's block would be the wrong trade.
            tracing::debug!(
                plugin = %self.plugin,
                id = %self.spec.id,
                "dashboard section mutex poisoned; rendering nothing"
            );
            return DashboardFragment::new();
        };
        if guest.poisoned {
            return DashboardFragment::new();
        }
        let wit_ctx = wit::Ctx {
            pane_width: ctx.pane_width.min(u32::MAX as usize) as u32,
            nerd_fonts: ctx.nerd_fonts,
            version: ctx.version.clone(),
        };
        let SectionGuest {
            store,
            bindings,
            poisoned,
        } = &mut *guest;
        match bindings.call_render_section(&mut *store, &self.spec.id, &wit_ctx) {
            Ok(fragment) => fragment_from_wit(&self.plugin, fragment),
            Err(e) => {
                *poisoned = true;
                tracing::warn!(
                    plugin = %self.plugin,
                    id = %self.spec.id,
                    error = %e,
                    "dashboard section trapped; it will render nothing further this session"
                );
                DashboardFragment::new()
            }
        }
    }
}

impl PluginHost {
    /// Instantiate a `dashboard-plugin` component, drive its
    /// `register-dashboard-sections` export once, and hand back one live
    /// section per id it declared.
    ///
    /// Each returned section owns its own instance. Sharing one store across
    /// several sections would serialise their renders behind a single mutex
    /// and let a trap in one blank all the others — a plugin's sections
    /// should fail independently, the way two plugins' do.
    pub fn spawn_dashboard_sections(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
    ) -> Result<(crate::PluginId, Vec<WasmDashboardSection>), PluginHostError> {
        let id = self.alloc_id();
        // The declaring instance. Its only job is to run the export and
        // report the specs; the per-section instances below are what the
        // registry keeps.
        let mut declarer =
            self.instantiate_dashboard_guest(component, manifest, tier, budget, id)?;
        declarer
            .bindings
            .call_register_dashboard_sections(&mut declarer.store)
            .map_err(|source| PluginHostError::Trap {
                func: "register-dashboard-sections",
                kind: crate::classify_trap(&source),
                source: source.into(),
            })?;
        let specs = std::mem::take(&mut declarer.store.data_mut().dashboard_contributions);
        drop(declarer);

        let mut sections = Vec::with_capacity(specs.len());
        for spec in specs {
            let guest = self.instantiate_dashboard_guest(component, manifest, tier, budget, id)?;
            sections.push(WasmDashboardSection {
                inner: Mutex::new(guest),
                spec,
                plugin: manifest.id.clone(),
                plugin_id: id.0 as u64,
            });
        }
        Ok((id, sections))
    }

    /// Instantiate one `dashboard-plugin` guest against the **sync** linker.
    ///
    /// Named for grammar because grammar was its first user, but it is the
    /// host's one sync import table — sync WASI plus the sync host funcs —
    /// and instantiating against a superset of a world's imports is what the
    /// multi-seam path already does.
    fn instantiate_dashboard_guest(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        id: crate::PluginId,
    ) -> Result<SectionGuest, PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "dashboard plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            bindings::DashboardPlugin::instantiate(&mut store, component, &self.grammar_linker)
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        store.data_mut().log_ctx = self.log_ctx_for(id);
        crate::arm_store(&mut store, budget)?;
        Ok(SectionGuest {
            store,
            bindings,
            poisoned: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(text: &str, link: Option<wit::LinkTarget>) -> wit::Span {
        wit::Span {
            text: text.to_string(),
            role: wit::Role::Body,
            link,
        }
    }

    fn frag(rows: Vec<wit::Row>) -> wit::Fragment {
        wit::Fragment { rows }
    }

    #[test]
    fn a_well_formed_fragment_converts() {
        let f = fragment_from_wit(
            "p",
            frag(vec![wit::Row {
                spans: vec![
                    span("Open ", None),
                    span(
                        ":tutor",
                        Some(wit::LinkTarget::Command("tutor".to_string())),
                    ),
                ],
                align: wit::Align::Center,
            }]),
        );
        assert_eq!(f.rows.len(), 1);
        assert_eq!(f.rows[0].text(), "Open :tutor");
        assert_eq!(f.rows[0].align, Align::Center);
        assert_eq!(
            f.rows[0].spans[1].link,
            Some(LinkTarget::Command("tutor".into()))
        );
    }

    /// A blank link value would render as a live-looking link whose `<CR>`
    /// does nothing. The label survives; the dead link does not.
    #[test]
    fn a_blank_link_target_is_dropped_but_the_label_stays() {
        let f = fragment_from_wit(
            "p",
            frag(vec![wit::Row {
                spans: vec![span(
                    "dead",
                    Some(wit::LinkTarget::Topic("   ".to_string())),
                )],
                align: wit::Align::Left,
            }]),
        );
        assert_eq!(f.rows[0].spans[0].text, "dead");
        assert!(f.rows[0].spans[0].link.is_none());
    }

    /// An empty row is a spacer, which sections legitimately use — it must
    /// survive the filter that drops bad spans.
    #[test]
    fn an_empty_row_is_kept_as_a_spacer() {
        let f = fragment_from_wit(
            "p",
            frag(vec![
                wit::Row {
                    spans: vec![],
                    align: wit::Align::Left,
                },
                wit::Row {
                    spans: vec![span("after", None)],
                    align: wit::Align::Left,
                },
            ]),
        );
        assert_eq!(f.rows.len(), 2);
        assert_eq!(f.rows[0].text(), "");
        assert_eq!(f.rows[1].text(), "after");
    }

    /// Fuel bounds the guest's time, not its output. A guest that returns
    /// quickly with an absurd row count is what this catches.
    #[test]
    fn a_fragment_over_the_row_cap_is_truncated() {
        let rows = (0..MAX_ROWS + 10)
            .map(|_| wit::Row {
                spans: vec![span("x", None)],
                align: wit::Align::Left,
            })
            .collect();
        let f = fragment_from_wit("p", frag(rows));
        assert_eq!(f.rows.len(), MAX_ROWS);
    }

    #[test]
    fn every_role_maps() {
        for (w, native) in [
            (wit::Role::Logo, DashboardRole::Logo),
            (wit::Role::Cursor, DashboardRole::Cursor),
            (wit::Role::Title, DashboardRole::Title),
            (wit::Role::Tagline, DashboardRole::Tagline),
            (wit::Role::SectionHeading, DashboardRole::SectionHeading),
            (wit::Role::Body, DashboardRole::Body),
            (wit::Role::Key, DashboardRole::Key),
            (wit::Role::Hint, DashboardRole::Hint),
            (wit::Role::Link, DashboardRole::Link),
        ] {
            assert_eq!(role_from_wit(w), native);
        }
    }

    #[test]
    fn a_section_with_no_id_is_rejected() {
        assert!(validate_section("", 0, true).is_err());
        assert!(validate_section("   ", 0, true).is_err());
        assert_eq!(
            validate_section(" recent ", 5, false).expect("accepted"),
            DashboardSectionSpec {
                id: "recent".to_string(),
                order: 5,
                default_enabled: false,
            }
        );
    }
}
