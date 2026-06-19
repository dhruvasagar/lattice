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
    pub registry: Weak<lattice_mode::ModeRegistry>,
}

impl CandidateGenerator for ModesGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let Some(registry) = self.registry.upgrade() else {
            return Vec::new();
        };
        let mut out: Vec<RawCandidate> = registry
            .iter_meta()
            .map(|(id, _kind)| RawCandidate {
                text: id.as_str().to_string(),
                display: id.as_str().to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
            })
            .collect();
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out
    }
}

/// `gen:events` -- one candidate per typed event registered in the
/// process-wide `EVENT_DESCRIPTORS` distributed slice. Stateless;
/// the slice is link-time constant.
pub struct EventsGenerator;

impl CandidateGenerator for EventsGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let mut out: Vec<RawCandidate> = lattice_protocol::event_registry::registered_events()
            .map(|d| RawCandidate {
                text: d.name.to_string(),
                display: d.name.to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
            })
            .collect();
        out.sort_by(|a, b| a.text.cmp(&b.text));
        out
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
                text: (*name).to_string(),
                display: (*name).to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
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
    pub registry: Weak<lattice_picker::PickerRegistry>,
}

impl CandidateGenerator for PickerSourcesGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let Some(registry) = self.registry.upgrade() else {
            return Vec::new();
        };
        // PickerRegistry::iter is id-sorted already; mirror its
        // order in the candidate list so popup ordering stays
        // stable across runs.
        registry
            .iter()
            .map(|(id, _spec)| RawCandidate {
                text: id.to_string(),
                display: id.to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
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
                text: id.clone(),
                display: id,
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
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
///
/// The `:customize` parser routes on the `-mode` suffix, so a
/// single candidate set covers both forms. Same [`Weak`] discipline
/// as [`ModesGenerator`].
pub struct CustomizeNamesGenerator {
    pub registry: Weak<lattice_mode::ModeRegistry>,
}

impl CandidateGenerator for CustomizeNamesGenerator {
    fn generate(&self, _ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let mut out: Vec<RawCandidate> = Vec::new();
        for meta in lattice_config::group::GROUP_DECLS.iter() {
            out.push(RawCandidate {
                text: meta.name.to_string(),
                display: meta.name.to_string(),
                kind: CandidateKind::Plain,
                data: CandidateData::Plain,
                source: None,
                accept_action: None,
            });
        }
        if let Some(registry) = self.registry.upgrade() {
            for (id, _kind) in registry.iter_meta() {
                out.push(RawCandidate {
                    text: id.as_str().to_string(),
                    display: id.as_str().to_string(),
                    kind: CandidateKind::Plain,
                    data: CandidateData::Plain,
                    source: None,
                    accept_action: None,
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
/// theme (the host applies the [`PickerAcceptOutcome::ApplyColorscheme`]
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
    ) -> Option<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::Colorscheme { name } => {
                Some(PickerAcceptOutcome::ApplyColorscheme { name: name.clone() })
            }
            _ => None,
        }
    }
}
