//! App-state-dependent completion generators.
//!
//! `lattice-completion`'s built-in generators (`gen:commands`,
//! `gen:files`) and `lattice-config`'s `gen:options` cover the
//! state-free / config-tier sources. `gen:help-topics` lives in
//! `lattice-help` next to the topic registry. Everything else --
//! sources that need App-level handles (mode registry, event
//! descriptors, LSP supervisor) -- lives here so the cmdline can
//! offer `<Tab>` candidates for every described command in
//! `lattice_grammar::ex_commands`.
//!
//! Each generator captures the minimal `Arc<...>` slice of state
//! it needs and is registered in `App::new` next to the existing
//! `gen:options` registration. Names are stable -- they appear as
//! string literals in `ArgSpec::completion` -- so adding a new
//! generator means registering it here and pointing the relevant
//! arg schemas at it.

use std::sync::Weak;

use lattice_completion::candidate::{CandidateData, CandidateKind, RawCandidate};
use lattice_completion::traits::{CandidateGenerator, GenerateContext};
use lattice_picker::{
    PickerAcceptOutcome, PickerContext, PickerInitResult, PickerSourceGenerator, PickerSourceSpec,
    RoutingPayload, SourceResult,
};
use lattice_theme::ThemeRegistryHandle;

/// `gen:modes` -- one candidate per registered mode. Walks
/// `ModeRegistry::iter_meta` so the candidate set follows the
/// runtime registry (built-in foundation modes plus any feature-
/// crate `register_*_modes` additions).
///
/// Holds a [`Weak`] to the registry so the generator doesn't keep
/// the Arc alive past the App's lifetime, and -- the operational
/// reason -- so `Arc::get_mut(&mut app.mode_registry)` still
/// succeeds in tests that need to register a test-only mode
/// post-boot. Upgrade-on-demand: a dropped registry yields an
/// empty candidate set (no panic).
pub struct ModesGenerator {
    pub registry: Weak<arc_swap::ArcSwap<lattice_mode::ModeRegistry>>,
}

impl CandidateGenerator for ModesGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let Some(registry) = self.registry.upgrade() else {
            return Vec::new();
        };
        let registry = registry.load();
        let mut out: Vec<RawCandidate> = registry
            .iter_meta()
            .map(|(id, _kind)| RawCandidate {
                insert_text: None,
                text: id.as_str().to_string(),
                display: id.as_str().to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect();
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out
    }
}

/// `gen:events` -- one candidate per registered event, from the unified view
/// (`event_registry::all_events`): the compile-time `EVENT_DESCRIPTORS` linkme
/// slice (built-ins) PLUS the runtime registry (plugin-defined events, PH7.8b),
/// so a plugin's custom events complete here the moment they register.
pub struct EventsGenerator;

impl CandidateGenerator for EventsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let mut out: Vec<RawCandidate> = lattice_protocol::event_registry::all_events()
            .into_iter()
            .map(|d| RawCandidate {
                insert_text: None,
                text: d.name.clone(),
                display: d.name.clone(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect();
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out
    }
}

/// `gen:elements` -- one candidate per registered theme element / face.
/// Walks [`ThemeRegistry::element_names`] so the candidate set follows the
/// live registry (core elements plus any mode/plugin-contributed ones).
/// Drives `:describe-element <Tab>` / `:describe-face <Tab>`.
///
/// Holds a strong [`ThemeRegistryHandle`] clone, mirroring
/// [`ThemePickerSource`] (the theme registry is a host ServiceRegistry
/// service that renderers read; nothing takes `Arc::get_mut` of it
/// post-boot, so the [`Weak`] discipline the mode-registry generators use
/// doesn't apply here).
pub struct ElementsGenerator {
    pub registry: ThemeRegistryHandle,
}

impl CandidateGenerator for ElementsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        // `element_names()` is already sorted.
        self.registry
            .element_names()
            .into_iter()
            .map(|name| RawCandidate {
                insert_text: None,
                text: name.clone(),
                display: name,
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// `gen:log-levels` -- the five canonical log levels accepted by
/// `:lsp-log-level`. Returned in severity order so the popup reads
/// the same way the level enum does.
pub struct LogLevelsGenerator;

impl CandidateGenerator for LogLevelsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        ["error", "warn", "info", "debug", "trace"]
            .iter()
            .map(|name| RawCandidate {
                insert_text: None,
                text: (*name).to_string(),
                display: (*name).to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// `gen:picker-sources` -- one candidate per source id registered
/// with the host's [`PickerRegistry`](lattice_picker::PickerRegistry).
/// Drives `:picker <Tab>` completion; the registry's contents
/// dictate the candidate set, so feature crates that register new
/// picker sources automatically surface in the popup.
///
/// Holds a [`Weak`] (mirror of [`ModesGenerator`]) so the App can
/// still take ownership of the registry on shutdown / replacement;
/// dropped-registry yields an empty candidate set rather than a
/// panic.
pub struct PickerSourcesGenerator {
    pub registry: Weak<arc_swap::ArcSwap<lattice_picker::PickerRegistry>>,
}

impl CandidateGenerator for PickerSourcesGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let Some(registry) = self.registry.upgrade() else {
            return Vec::new();
        };
        // Wait-free snapshot of the current registry (a plugin load may have
        // RCU-swapped a fresh one in). `PickerRegistry::iter` is id-sorted
        // already; mirror its order so popup ordering stays stable across runs.
        let registry = registry.load();
        registry
            .iter()
            .map(|(id, _spec)| RawCandidate {
                insert_text: None,
                text: id.to_string(),
                display: id.to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// `gen:lsp-servers` -- one candidate per currently running LSP
/// server id. Reads through the supervisor's wait-free snapshot
/// (`ArcSwap`-backed) so completion never blocks on the supervisor
/// task. Dedup-by-id across multi-workspace runs: the same server
/// id attached to two different roots collapses to one candidate.
pub struct LspServersGenerator {
    pub lsp: lattice_lsp::LspSupervisorHandle,
}

impl CandidateGenerator for LspServersGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let mut ids: Vec<String> = self
            .lsp
            .running_actors()
            .into_iter()
            .map(|((_root, id), _handle)| id)
            .collect();
        ids.sort();
        ids.dedup();
        ids.into_iter()
            .map(|id| RawCandidate {
                insert_text: None,
                text: id.clone(),
                display: id,
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// `gen:customize` -- candidates for `:customize <name>`. Merges
/// two sources:
///
/// - Every group declared via the `groups!` macro
///   (`GROUP_DECLS` distributed slice) -- bare names like
///   `editor`, `display`, `lsp`.
/// - Every registered mode -- names ending in `-mode` like
///   `lsp-completion-mode`, `text-mode`.
/// - Every plugin option namespace in the live [`ConfigRegistry`]
///   -- `treesitter-context`, and whatever else is loaded.
///
/// The `:customize` parser routes on the `-mode` suffix, so a
/// single candidate set covers both forms. Same [`Weak`] discipline
/// as [`ModesGenerator`].
///
/// The third source exists because the first two are COMPILE-time
/// slices. A plugin registers its options at runtime, so a
/// decl-only candidate set silently omits every plugin group --
/// `:customize <plugin>` works but is never offered, which reads as
/// "the feature isn't there". The config handle is a strong `Arc`
/// (matching `gen:options`, which holds the same registry) because
/// the registry outlives the completion registry rather than the
/// other way round.
pub struct CustomizeNamesGenerator {
    pub registry: Weak<arc_swap::ArcSwap<lattice_mode::ModeRegistry>>,
    pub config: std::sync::Arc<lattice_config::ConfigRegistry>,
}

impl CandidateGenerator for CustomizeNamesGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let mut out: Vec<RawCandidate> = Vec::new();
        for meta in lattice_config::group::GROUP_DECLS.iter() {
            out.push(RawCandidate {
                insert_text: None,
                text: meta.name.to_string(),
                display: meta.name.to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            });
        }
        for ns in lattice_config::plugin_option_groups(&self.config).keys() {
            out.push(RawCandidate {
                insert_text: None,
                text: ns.clone(),
                display: ns.clone(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            });
        }
        if let Some(registry) = self.registry.upgrade() {
            let registry = registry.load();
            for (id, _kind) in registry.iter_meta() {
                out.push(RawCandidate {
                    insert_text: None,
                    text: id.as_str().to_string(),
                    display: id.as_str().to_string(),
                    kind: CandidateKind::Plain,
                    data: CandidateData::Plain,
                    source: None,
                    accept_action: None,
                    annotations: Vec::new(),
                    display_spans: Vec::new(),
                });
            }
        }
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out.dedup_by(|a, b| a.text == b.text);
        out
    }
}

/// T.12a: `:colorscheme` (no arg) — the live-preview theme picker.
/// A trait-driven [`PickerSourceGenerator`] holding a clone of the
/// [`ThemeRegistryHandle`] so it can enumerate registered theme names
/// at `init` time. Arrowing through candidates LIVE-PREVIEWS each
/// theme (the host applies the
/// [`PickerPreviewOutcome::Colorscheme`](lattice_picker::PickerPreviewOutcome::Colorscheme)
/// the [`Self::preview`] hook returns); `<Esc>` restores the theme
/// active when the picker opened; `<CR>` keeps the highlighted theme.
///
/// Mode-owned-surface note: the host owns the theme subsystem (the
/// `ThemeRegistry` is a host ServiceRegistry service the renderers
/// read), so this source + its handler body both live host-side — no
/// half-migration.
pub struct ThemePickerSource {
    spec: PickerSourceSpec,
    registry: ThemeRegistryHandle,
}

impl ThemePickerSource {
    pub fn new(registry: ThemeRegistryHandle) -> Self {
        Self {
            spec: PickerSourceSpec::no_args("colorscheme", "Pick a colour theme (live preview)."),
            registry,
        }
    }
}

impl PickerSourceGenerator for ThemePickerSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        let pairs = self
            .registry
            .theme_names()
            .into_iter()
            .map(|name| {
                let cand = RawCandidate::plain(name.clone(), CandidateKind::Plain);
                (cand, RoutingPayload::Colorscheme { name })
            })
            .collect();
        Ok(PickerInitResult::Inline(pairs))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::Colorscheme { name } => {
                Ok(PickerAcceptOutcome::ApplyColorscheme { name: name.clone() })
            }
            other => Err(format!("colorscheme: unexpected routing payload {other:?}")),
        }
    }

    fn preview(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> Option<lattice_picker::PickerPreviewOutcome> {
        match routing {
            RoutingPayload::Colorscheme { name } => {
                Some(lattice_picker::PickerPreviewOutcome::Colorscheme { name: name.clone() })
            }
            _ => None,
        }
    }
}

/// `gen:themes` — one candidate per registered colour theme. Drives
/// `:colorscheme <Tab>`.
///
/// The same `theme_names()` the `colorscheme` picker enumerates, so the two
/// surfaces cannot disagree about which themes exist. `:colorscheme` keeps its
/// picker as well: `ArgSpec` carries `completion` and `picker` as separate
/// fields precisely so an argument can offer inline `<Tab>` AND the richer
/// surface — here the picker adds live preview, which completion cannot.
pub struct ThemesGenerator {
    pub registry: ThemeRegistryHandle,
}

impl CandidateGenerator for ThemesGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        self.registry
            .theme_names()
            .into_iter()
            .map(|name| RawCandidate {
                insert_text: None,
                text: name.clone(),
                display: name,
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// `gen:plugin-api-seams` — one candidate per WIT interface in the plugin-API
/// catalog. Drives `:describe-plugin-api <Tab>`.
///
/// The catalog is generated from the WIT at build time and is static, so this
/// needs no handle — and a seam renamed in the WIT changes the candidates with
/// no second place to update.
pub struct PluginApiSeamsGenerator;

impl CandidateGenerator for PluginApiSeamsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        lattice_plugin_api::catalog()
            .interfaces
            .iter()
            .map(|i| RawCandidate {
                insert_text: None,
                text: i.name.clone(),
                display: i.name.clone(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// `gen:plugin-api-formats` — the two export formats `:export-plugin-api`
/// accepts. A two-element set is still worth completing: the alternative is
/// reading the command's doc to learn that `md` is spelled `markdown`.
pub struct PluginApiFormatsGenerator;

impl CandidateGenerator for PluginApiFormatsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        ["markdown", "json"]
            .into_iter()
            .map(|s| RawCandidate {
                insert_text: None,
                text: s.to_string(),
                display: s.to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// `gen:plugins` — one candidate per loaded plugin. Drives `:describe-plugin <Tab>` and the plugin manager's
/// `:plugin-unload` / `:plugin-reload` / `:plugin-update`.
///
/// Both call sites carried this as a deferred comment — "`gen:plugins`
/// completion is a follow-up … empty until Phase-8" in `ex_commands.rs`, and
/// the same note on `lattice-plugin-loader`'s `string_arg`. Phase 8 landed and
/// the comments did not get grepped, which is why the rule is now a test
/// (`ex_string_args_have_completion.rs`) rather than a convention.
///
/// Holds a strong `Arc<PluginMetaRegistry>`, the same instance the host's
/// `register_plugin` writes through and `:list-plugins` reads. It is registered
/// once at boot and never swapped, so there is no stale-Arc hazard; the
/// interior `RwLock` is what a mid-session load mutates, and reading it per
/// `generate` is what makes a plugin loaded after boot appear under `<Tab>`.
pub struct PluginsGenerator {
    pub meta: std::sync::Arc<crate::dispatch::PluginMetaRegistry>,
}

impl CandidateGenerator for PluginsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let Ok(map) = self.meta.0.read() else {
            return Vec::new();
        };
        let mut names: Vec<String> = map.values().map(|m| m.name.clone()).collect();
        names.sort();
        names
            .into_iter()
            .map(|name| RawCandidate {
                insert_text: None,
                text: name.clone(),
                display: name,
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

/// MB.5: `gen:history-kinds` — completion source for `:history <Tab>`.
/// Returns the two valid kind arguments: `commands` and `searches`.
pub struct HistoryKindsGenerator;

impl CandidateGenerator for HistoryKindsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        vec!["commands", "searches", "pane-buffers"]
            .into_iter()
            .map(|s| RawCandidate {
                insert_text: None,
                text: s.to_string(),
                display: s.to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::Document;
    use lattice_grammar::CommandRegistry;
    use std::sync::Arc;

    /// T.9.d follow-up: `gen:elements` (`:describe-element <Tab>`) enumerates
    /// the live theme-element registry. Confirms the generator surfaces builtin
    /// element names in sorted order, mirroring `ThemeRegistry::element_names`.
    #[test]
    fn elements_generator_produces_sorted_theme_element_names() {
        let registry: ThemeRegistryHandle =
            Arc::new(lattice_theme::InMemoryThemeRegistry::with_defaults());
        let g = ElementsGenerator {
            registry: registry.clone(),
        };
        let doc = Document::from_text("");
        let buf = doc.buffer();
        let cmd_reg = CommandRegistry::new();
        let ctx = GenerateContext {
            prefix: "",
            buffer: buf,
            registry: &cmd_reg,
            case_sensitive: false,
        };

        let out = g.generate(&ctx);
        let names: Vec<String> = out.iter().map(|c| c.text.clone()).collect();

        assert!(
            names.contains(&"syntax.keyword".to_string()),
            "a builtin theme element must complete for `:describe-element`"
        );
        // The generator returns exactly what the registry enumerates, in the
        // same (sorted) order — no drift, so popup ordering is stable.
        assert_eq!(names, registry.element_names());
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted, "candidates must be sorted");
    }

    /// `gen:plugins` enumerates the LOADED set, read per `generate` rather than
    /// snapshotted, so a plugin loaded after boot completes without a restart.
    #[test]
    fn plugins_generator_enumerates_loaded_plugins_sorted() {
        let meta = Arc::new(crate::dispatch::PluginMetaRegistry::default());
        let g = PluginsGenerator { meta: meta.clone() };
        let doc = Document::from_text("");
        let buf = doc.buffer();
        let cmd_reg = CommandRegistry::new();
        let ctx = GenerateContext {
            prefix: "",
            buffer: buf,
            registry: &cmd_reg,
            case_sensitive: false,
        };

        assert!(
            g.generate(&ctx).is_empty(),
            "nothing loaded yet ⇒ no candidates"
        );

        // Registered the way the loader does it, AFTER the generator was built.
        // A boot-time snapshot would list nothing here forever.
        {
            let mut map = meta.0.write().expect("meta registry lock");
            for (id, name) in [(7u32, "zebra"), (8, "auto-pair")] {
                map.insert(
                    id,
                    crate::dispatch::PluginMeta {
                        name: name.to_string(),
                        doc: String::new(),
                    },
                );
            }
        }

        let names: Vec<String> = g.generate(&ctx).iter().map(|c| c.text.clone()).collect();
        assert_eq!(
            names,
            vec!["auto-pair".to_string(), "zebra".to_string()],
            "every loaded plugin completes, sorted — popup order must be stable \
             even though the backing map is a HashMap"
        );
    }

    /// A plugin's option namespace must be offered by `:customize <Tab>`.
    ///
    /// The regression this pins: the generator was built from
    /// `GROUP_DECLS` + modes, both of which are populated at COMPILE time. A
    /// plugin registers its options at RUNTIME, so `:customize
    /// treesitter-context` worked while `<Tab>` never once mentioned it —
    /// making a working group indistinguishable from an absent one, which is
    /// the form the report arrived in.
    #[test]
    fn customize_completion_offers_runtime_plugin_namespaces() {
        use lattice_config::ConfigRegistry;
        use lattice_core::Document;
        use lattice_grammar::CommandRegistry;

        // Registered exactly the way the plugin config seam does it
        // (`register_plugin_option` -> `try_register(ConfigOption::new(..))`),
        // so the test cannot pass against a shape production never produces.
        let config = std::sync::Arc::new(ConfigRegistry::default());
        for (name, doc) in [
            ("treesitter-context.enabled", "Enable it."),
            ("treesitter-context.max-lines", "Cap the strip."),
        ] {
            config
                .try_register(lattice_config::option::Option::<bool>::new(
                    name.to_owned(),
                    true,
                    doc.to_owned(),
                ))
                .expect("fresh registry");
        }

        let modes = std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(
            lattice_mode::ModeRegistry::new(),
        ));
        let g = CustomizeNamesGenerator {
            registry: std::sync::Arc::downgrade(&modes),
            config: config.clone(),
        };

        let doc = Document::from_text("");
        let buf = doc.buffer();
        let cmd_reg = CommandRegistry::new();
        let ctx = GenerateContext {
            prefix: "",
            buffer: buf,
            registry: &cmd_reg,
            case_sensitive: false,
        };

        let names: Vec<String> = g.generate(&ctx).into_iter().map(|c| c.text).collect();

        assert!(
            names.contains(&"treesitter-context".to_string()),
            "the plugin namespace must be offered; got {names:?}"
        );
        // The namespace is offered ONCE, not once per option in it.
        assert_eq!(
            names.iter().filter(|n| *n == "treesitter-context").count(),
            1
        );
        // And the compile-time groups are still there — this is an addition.
        assert!(names.contains(&"editor".to_string()));
    }
}
