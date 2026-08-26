//! Plugin teardown — reversing every contribution a plugin made (PH7.12b.3).
//!
//! The seam a plugin **unload** (and, composed with a fresh `spawn_*`, a
//! **reload**) drives. Each contribution surface got a provenance/id-driven
//! `unregister_*` in PH7.12b.1; this module aggregates the *tokens* those
//! registrations produced ([`PluginTeardown`]) and applies each reversal
//! against the host-owned registries ([`TeardownRegistries`]).
//!
//! **Why an explicit driver, not `Drop`.** The registries this touches have
//! mixed mutability — `CommandRegistry` / `PickerRegistry` / `ModeRegistry` are
//! `&mut`-owned by the editor, while `ConfigRegistry` / `KeymapHandle` /
//! `EventBus` are `Arc`-shared with interior mutability. A `Drop` impl would
//! have to capture all six, forcing every registry behind `Arc<Mutex<_>>` just
//! to fit RAII — a strictly weaker foundation. So teardown is an explicit call
//! the caller makes when it holds the registry set (a `&mut Editor` context in
//! Phase 8; the test harness in Phase 7).
//!
//! **Why no `reload` method.** Re-instantiation is just re-invoking the same
//! `spawn_*` that produced the plugin, minting a fresh `Store` with a fresh,
//! untripped [`Quarantine`](crate::Quarantine). So reload = `unload` + `spawn_*`,
//! composed by the caller (the Phase-8 plugin manager), not a bespoke method.
//!
//! **Completion** is absent by design: the host never registers it with plugin
//! provenance (it goes through the generic builtin-stamped `register_generator`),
//! so its teardown is pure channel-drop — dropping the client ends the actor
//! loop. **Decoration** (PL8.E) *does* have a registry — the loader RCU-registers
//! its producer into the [`GutterDecorationSourceRegistry`] — so its teardown
//! unregisters by producer id, like the picker surface.
//!
//! **Error parsers** (CM.6b) have a registry too, but no token: the compilation
//! parser-factory registry keys entries by the host-issued plugin id, so
//! reversal is by provenance like the command surface, and there is no
//! per-contribution `Vec` on [`PluginTeardown`] to populate or to forget.

use lattice_config::ConfigRegistry;
use lattice_grammar::CommandRegistry;
use lattice_keymap::{KeymapCapability, KeymapHandle, KeymapLayer, ModeId};
use lattice_mode::{ContextSourceRegistry, GutterDecorationSourceRegistry, ModeRegistry};
use lattice_picker::source::PickerRegistry;
use lattice_protocol::event_registry::unregister_runtime_event;
use lattice_runtime::{EventBus, SubscriptionId};

use crate::PluginId;

/// The union of teardown tokens a plugin's contributions produce, aggregated at
/// spawn time and consumed by [`unload`](Self::unload). A given plugin populates
/// only the surfaces it exercised; the rest stay empty and their reversal is a
/// no-op. Every field is `pub` so the spawning caller fills it from the tokens
/// the `spawn_*` fns already return (`Vec<SubscriptionId>`, `Vec<ModeId>`,
/// `Vec<String>` option names, …).
#[derive(Debug, Clone)]
pub struct PluginTeardown {
    /// The host-issued identity — drives `CommandRegistry::unregister_plugin`,
    /// which unconditionally removes every `SourceLayer::Plugin(plugin_id)`
    /// command (grammar contributions + the modes seam's `:<mode>` toggles) by
    /// provenance. No per-command token or "did I register grammar?" flag: the
    /// provenance IS the token.
    pub plugin_id: PluginId,
    /// **Every** host id this plugin's seams were issued, `plugin_id`
    /// included.
    ///
    /// Each `spawn_*` issues its own id — deliberately, because a provenance
    /// id must never be derived from guest-controlled input, so it cannot be
    /// keyed on the manifest's string id. A plugin providing N seams therefore
    /// has N provenances, and reversing only one of them leaves the rest
    /// registered: bundled `auto-pair` (grammar, modes, config, help) leaked
    /// its `:help` pages on unload exactly that way.
    ///
    /// Token-based reversals below are unaffected — the drains capture their
    /// tokens on the record. This is only for the provenance-keyed ones.
    pub seam_ids: Vec<PluginId>,
    /// Picker source ids the plugin registered (`PickerRegistry::unregister`).
    pub picker_sources: Vec<String>,
    /// Modes the plugin registered — each reversed via `ModeRegistry::unregister`
    /// *and* `KeymapHandle::remove_layer(MinorMode(id))` (both halves of the mode
    /// surface, PH7.11).
    pub modes: Vec<ModeId>,
    /// Config option names the plugin registered (`ConfigRegistry::unregister`);
    /// mirrors `PluginState::config_contributions`.
    pub config_options: Vec<String>,
    /// Plugin-defined event names (`unregister_runtime_event`, process-wide).
    pub events_defined: Vec<String>,
    /// Event-bus subscription ids the plugin's `subscribe` calls produced
    /// (`EventBus::unsubscribe`); the `Vec` `spawn_event_plugin` returns.
    pub subscriptions: Vec<SubscriptionId>,
    /// User keybindings the plugin bound via the `keymap` seam (PL8.D) — each
    /// reversed by `KeymapHandle::try_unbind_chord_string` from `KeymapLayer::User`.
    /// The `Vec` `spawn_keymap_plugin` returns.
    pub keymap_bindings: Vec<crate::keymap_host::KeymapBindingToken>,
    /// PL8.E: decoration producer ids the plugin registered into the
    /// [`GutterDecorationSourceRegistry`] — each reversed via
    /// `GutterDecorationSourceRegistry::unregister`. Mirrors `picker_sources`.
    pub decoration_sources: Vec<u64>,
    /// IM.6b: media producers to unregister, by plugin id. Mirrors
    /// `decoration_sources` — without this a `:plugin-reload` would leave the
    /// old producer registered and every image would be requested twice.
    pub media_sources: Vec<u64>,
    /// OM.A1: agenda producers to unregister, by plugin id. Mirrors
    /// `media_sources` — without this a `:plugin-reload` would leave the old
    /// producer registered and every agenda row would appear twice.
    pub agenda_sources: Vec<u64>,
    /// TC.2: context producer ids the plugin registered into the
    /// [`ContextSourceRegistry`] — each reversed via
    /// `ContextSourceRegistry::unregister`. Mirrors `decoration_sources`.
    pub context_sources: Vec<u64>,
    /// TR.2b: transient-menu names the plugin registered. Reversed by the
    /// LOADER, not by [`unload`](Self::unload) — the registry is
    /// `Arc`-shared rather than one of the `&mut` snapshots
    /// [`TeardownRegistries`] carries, and it is the same placement
    /// `help_topics` / `dashboard_sections` use for the same reason.
    ///
    /// Leaving a name registered after unload is not cosmetic: the entry holds
    /// a `TransientClient` whose actor has ended, so the chord would report a
    /// host error instead of "unknown source".
    pub transient_sources: Vec<String>,
    /// TC.4: namespaced theme-element names the plugin registered. Reversed by
    /// `ThemeRegistry::unregister_element` so an unloaded plugin's elements stop
    /// appearing in `:customize` and stop resolving.
    pub theme_elements: Vec<String>,
}

impl PluginTeardown {
    /// A bundle for `plugin_id` with no contributions recorded yet.
    pub fn new(plugin_id: PluginId) -> Self {
        Self {
            plugin_id,
            seam_ids: Vec::new(),
            picker_sources: Vec::new(),
            modes: Vec::new(),
            config_options: Vec::new(),
            events_defined: Vec::new(),
            subscriptions: Vec::new(),
            keymap_bindings: Vec::new(),
            decoration_sources: Vec::new(),
            media_sources: Vec::new(),
            agenda_sources: Vec::new(),
            context_sources: Vec::new(),
            transient_sources: Vec::new(),
            theme_elements: Vec::new(),
        }
    }

    /// Reverse every recorded contribution against the host registries, returning
    /// a [`TeardownReport`] of what was removed. Idempotent — each underlying
    /// `unregister_*` is a no-op on an already-removed entry, so a double-unload
    /// (or an unload after a partial crash) is safe and simply reports zeros the
    /// second time. Order is irrelevant: the surfaces are independent (grammar
    /// commands, picker sources, modes+their keymap layer, options, events, and
    /// subscriptions never share an entry).
    /// Every provenance this plugin stamped contributions with.
    ///
    /// Falls back to `plugin_id` alone when `seam_ids` was never populated, so
    /// a hand-built `PluginTeardown` (tests, and any caller predating
    /// `seam_ids`) still reverses its one id rather than silently reversing
    /// nothing.
    pub fn provenances(&self) -> Vec<PluginId> {
        if self.seam_ids.is_empty() {
            vec![self.plugin_id]
        } else {
            self.seam_ids.clone()
        }
    }

    pub fn unload(&self, reg: &mut TeardownRegistries<'_>) -> TeardownReport {
        let mut report = TeardownReport::default();

        // Reversed by provenance: remove every `SourceLayer::Plugin(plugin_id)`
        // command — grammar contributions AND the `:<mode>` toggle ex-commands
        // the modes seam registers (`drain_mode`). Unconditional + idempotent
        // (returns 0 if the plugin contributed none), so no per-seam "did I
        // register commands?" flag exists to forget; the `run_teardown`
        // clone/store around this happens regardless of the count.
        // Over EVERY seam id, not just `plugin_id`: see `seam_ids`.
        for id in self.provenances() {
            report.commands += reg.commands.unregister_plugin(id.0);
        }
        for id in &self.picker_sources {
            if reg.pickers.unregister(id) {
                report.pickers += 1;
            }
        }
        for mode in &self.modes {
            // OM.2: which layer the chords went into follows the mode's KIND,
            // so read it BEFORE unregistering — afterwards the registry no
            // longer knows, and a plugin major would leak its keymap layer.
            let kind = reg.modes.get(*mode).map(|m| m.kind());
            if reg.modes.unregister(*mode) {
                report.modes += 1;
            }
            // Both halves of the mode surface: the registry entry AND the gated
            // keymap layer its chords were bound into (PH7.11b / PH7.12b.1c).
            match kind {
                Some(lattice_mode::ModeKind::Major) => {
                    reg.keymap.remove_layer(KeymapLayer::MajorMode(*mode));
                }
                // `None` (already gone) takes the minor branch: the id was
                // never a major we bound, and `remove_layer` on an absent
                // layer is a no-op.
                _ => {
                    reg.keymap.remove_layer(KeymapLayer::MinorMode(*mode));
                }
            }
        }
        for name in &self.config_options {
            if reg.config.unregister(name) {
                report.config_options += 1;
            }
        }
        for name in &self.events_defined {
            unregister_runtime_event(name);
            report.events_defined += 1;
        }
        for id in &self.subscriptions {
            if reg.bus.unsubscribe(*id) {
                report.subscriptions += 1;
            }
        }
        for binding in &self.keymap_bindings {
            // Reverse the `KeymapLayer::User` binding by the same chord string the
            // plugin bound with (`KeymapCapability::User`). `Ok(Some(_))` = a
            // binding was dropped; an already-removed / unparseable entry is a
            // graceful no-op (idempotent re-unload).
            if matches!(
                reg.keymap.try_unbind_chord_string(
                    KeymapCapability::User,
                    KeymapLayer::User,
                    binding.mode,
                    &binding.chord,
                ),
                Ok(Some(_))
            ) {
                report.keymap_bindings += 1;
            }
        }
        for source_id in &self.decoration_sources {
            report.decoration_sources += reg.decorations.unregister(*source_id);
        }
        for source_id in &self.media_sources {
            report.media_sources += reg.media.unregister(*source_id);
        }
        for source_id in &self.agenda_sources {
            report.agenda_sources += reg.agenda.unregister(*source_id);
        }
        for source_id in &self.context_sources {
            report.context_sources += reg.contexts.unregister(*source_id);
        }
        for name in &self.theme_elements {
            if reg
                .theme
                .unregister_element(&lattice_theme::ElementName::from(name.clone()))
            {
                report.theme_elements += 1;
            }
        }
        // CM.6b: compilation parser factories, reversed by PROVENANCE like
        // the command surface above — there is no per-factory token to
        // record or forget, because the registry already keys them by the
        // host-issued plugin id. Guarded on non-empty so the overwhelmingly
        // common unload (no error-parser plugin anywhere) does not churn the
        // `ArcSwap` a compilation run reads.
        let snapshot = reg.parsers.load();
        if !snapshot.is_empty() {
            let mut next = (**snapshot).clone();
            for id in self.provenances() {
                report.parser_factories += next.unregister_plugin(id.0 as u64);
            }
            if report.parser_factories > 0 {
                reg.parsers.store(std::sync::Arc::new(next));
            }
        }

        report
    }
}

/// The host-owned registries a plugin's contributions live in — the set
/// [`PluginTeardown::unload`] reverses against. Grouped as a borrow struct so
/// `unload` takes one argument instead of six, and so the caller passes exactly
/// the registries a `&mut Editor` context already holds. All are required: a
/// plugin can contribute to any surface, and passing the whole set is cheaper
/// than threading option-ness through the driver (an unexercised surface's
/// reversal is already a no-op).
pub struct TeardownRegistries<'a> {
    pub commands: &'a mut CommandRegistry,
    pub pickers: &'a mut PickerRegistry,
    pub modes: &'a mut ModeRegistry,
    pub keymap: &'a KeymapHandle,
    pub config: &'a ConfigRegistry,
    pub bus: &'a EventBus,
    /// PL8.E: the decoration-producer registry (`unregister` by producer id).
    pub decorations: &'a mut GutterDecorationSourceRegistry,
    /// IM.6b: the media-producer registry, for the same reversal.
    pub media: &'a mut lattice_mode::MediaSourceRegistry,
    /// OM.A1: the agenda-producer registry, for the same reversal.
    pub agenda: &'a mut lattice_mode::AgendaSourceRegistry,
    /// TC.2: the context-producer registry (`unregister` by producer id).
    pub contexts: &'a mut ContextSourceRegistry,
    /// TC.4: the theme registry (`unregister_element` by namespaced name).
    pub theme: &'a dyn lattice_theme::ThemeRegistry,
    /// CM.6b: the compilation parser-factory registry (`unregister_plugin`
    /// by host-issued id, RCU'd like the other `ArcSwap`-held registries).
    pub parsers: &'a lattice_compilation::CompilationParserFactoriesHandle,
}

/// Count of what an [`unload`](PluginTeardown::unload) actually removed, per
/// surface — for structured logs and test assertions. A field being lower than
/// the bundle's recorded token count means those entries were already gone (a
/// prior unload, or a crash that never completed registration): expected under
/// idempotent re-unload, not an error.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TeardownReport {
    pub commands: usize,
    pub pickers: usize,
    pub modes: usize,
    pub config_options: usize,
    pub events_defined: usize,
    pub subscriptions: usize,
    pub keymap_bindings: usize,
    /// PL8.E: decoration producers unregistered.
    pub decoration_sources: usize,
    /// IM.6b: media producers unregistered.
    pub media_sources: usize,
    /// OM.A1: agenda producers unregistered.
    pub agenda_sources: usize,
    /// TC.2: context producers unregistered.
    pub context_sources: usize,
    /// TC.4: theme elements unregistered.
    pub theme_elements: usize,
    /// TR.2b: transient menus unregistered.
    ///
    /// Filled by the LOADER, like `help_topics` — see the field's doc on
    /// [`PluginTeardown`].
    pub transient_sources: usize,
    /// CM.6b: compilation parser factories unregistered.
    pub parser_factories: usize,
    /// CR.4: dashboard sections unregistered. Filled by the loader, for the
    /// same crate-boundary reason as `help_topics` below.
    pub dashboard_sections: usize,
    /// CR.3: `:help` topics unregistered.
    ///
    /// Filled by the LOADER after `unload` returns, not by `unload` itself —
    /// the help registry lives in `lattice-help`, and reversing it here would
    /// pull that crate into the host purely to name a field. The seam crosses
    /// plain data in both directions, so nothing else about help belongs on
    /// this side of the line.
    pub help_topics: usize,
    /// LG.3c: plugin-contributed languages unregistered.
    ///
    /// Filled by the LOADER, like `help_topics`, and for the same reason: the
    /// language registry lives in `lattice-syntax`. Unlike every other field
    /// here this one can never be skipped for want of a handle — the registry
    /// is process-global, so there is no `Option` to be `None`.
    pub languages: usize,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use std::sync::Arc;

    use super::*;

    use lattice_config::option::Option as ConfigOption;
    use lattice_grammar::CommandInvocation;
    use lattice_grammar::registry::{MotionResult, MotionSpec};
    use lattice_grammar::source::SourceLocation;
    use lattice_keymap::{BindingMode, ChordPattern, KeymapCapability};
    use lattice_picker::source::PickerSourceSpec;
    use lattice_protocol::EventKind;
    use lattice_protocol::chord::KeyChord;
    use lattice_protocol::ids::CommandId;
    use lattice_runtime::{EventFilter, SubscriptionTarget};

    fn dummy_motion() -> MotionSpec {
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Arc::new(|ctx| {
                Ok(MotionResult {
                    target: ctx.from,
                    linewise: false,
                })
            }),
            args_schema: vec![],
        }
    }

    /// The driver reverses every surface a plugin touched, in one `unload`, and
    /// leaves a co-resident *built-in / native* contribution on each registry
    /// untouched. Host-layer proof of the reload teardown; the wasm reload cycle
    /// (spawn → quarantine → unload → respawn) is `tests/plugin_teardown.rs`. The
    /// `ModeRegistry` half is proven in `lattice-mode`'s own `unregister` test —
    /// here we prove the driver runs the mode's *keymap-layer* teardown.
    #[test]
    fn unload_reverses_every_surface_and_spares_the_rest() {
        let plugin = PluginId(7);

        // --- Grammar: one plugin motion + one built-in motion. ---
        let mut commands = CommandRegistry::new();
        commands.register_motion("builtin:w", "builtin", dummy_motion());
        commands.register_plugin_motion(plugin.0, "p7:down", "", dummy_motion());

        // --- Picker: the plugin source + a native source. ---
        let mut pickers = PickerRegistry::new();
        pickers.register(PickerSourceSpec::no_args("files", "native"));
        pickers.register(PickerSourceSpec::no_args("p7:things", "plugin"));

        // --- Mode keymap half: a chord bound into the plugin mode's layer. ---
        let mode_id = ModeId::new("p7-mode");
        let mut modes = ModeRegistry::new();
        let keymap = KeymapHandle::new();
        keymap
            .try_bind(
                KeymapCapability::OwnedLayer { mode_id },
                KeymapLayer::MinorMode(mode_id),
                BindingMode::Normal,
                &[ChordPattern::Literal(KeyChord::char('j'))],
                CommandInvocation::of(CommandId::new(1)),
                SourceLocation::plugin(plugin.0),
            )
            .unwrap();

        // --- Config: the plugin option + a native option. ---
        let config = ConfigRegistry::new();
        config.register(ConfigOption::<i64>::new("native.opt", 1, "native"));
        config.register(ConfigOption::<i64>::new("p7.opt", 8, "plugin"));

        // --- Events: a native subscriber + the plugin's subscription. ---
        let bus = EventBus::new();
        let (native_tx, _native_rx) = tokio::sync::mpsc::unbounded_channel();
        let native_sub = bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Channel(native_tx),
        );
        let (plugin_tx, _plugin_rx) = tokio::sync::mpsc::unbounded_channel();
        let plugin_sub = bus.subscribe(
            EventFilter::kind(EventKind::DocumentSaved),
            SubscriptionTarget::Channel(plugin_tx),
        );

        // Build the bundle the way a spawning caller would from returned tokens.
        let mut teardown = PluginTeardown::new(plugin);
        teardown.picker_sources = vec!["p7:things".to_string()];
        teardown.modes = vec![mode_id];
        teardown.config_options = vec!["p7.opt".to_string()];
        teardown.subscriptions = vec![plugin_sub];

        let mut decorations = GutterDecorationSourceRegistry::new();
        let mut contexts = ContextSourceRegistry::new();
        let theme_reg = lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette());
        let parsers = lattice_compilation::CompilationParserFactories::new_handle();
        let report = {
            let mut reg = TeardownRegistries {
                media: &mut Default::default(),
                agenda: &mut Default::default(),
                commands: &mut commands,
                pickers: &mut pickers,
                modes: &mut modes,
                keymap: &keymap,
                config: &config,
                bus: &bus,
                decorations: &mut decorations,
                contexts: &mut contexts,
                theme: &theme_reg,
                parsers: &parsers,
            };
            teardown.unload(&mut reg)
        };

        // Report: the plugin's registry contributions were removed. `modes` is 0
        // because no mode was registered in this host-layer test (the keymap-layer
        // half is asserted below via binding_count).
        assert_eq!(
            report,
            TeardownReport {
                commands: 1,
                pickers: 1,
                modes: 0,
                config_options: 1,
                events_defined: 0,
                subscriptions: 1,
                keymap_bindings: 0,
                decoration_sources: 0,
                media_sources: 0,
                agenda_sources: 0,
                context_sources: 0,
                theme_elements: 0,
                parser_factories: 0,
                transient_sources: 0,
                // CR.3 / CR.4 / LG.3c: always 0 from `unload` — the loader
                // fills these after reversing the help, dashboard and
                // language registries, which live on its side of the crate
                // boundary.
                help_topics: 0,
                dashboard_sections: 0,
                languages: 0,
            }
        );

        // Grammar: plugin motion gone, built-in survives.
        assert!(commands.lookup_by_name("p7:down").is_none());
        assert!(commands.lookup_by_name("builtin:w").is_some());
        // Picker: plugin source gone, native survives.
        assert!(pickers.get("p7:things").is_none());
        assert!(pickers.get("files").is_some());
        // Mode keymap layer: the plugin's bound chord is gone.
        assert_eq!(keymap.binding_count(), 0);
        // Config: plugin option gone, native survives.
        assert!(config.lookup("p7.opt").is_none());
        assert!(config.lookup("native.opt").is_some());
        // Events: the plugin's subscription is gone, the native one still fires.
        assert!(!bus.unsubscribe(plugin_sub), "plugin sub already removed");
        assert!(bus.unsubscribe(native_sub), "native sub was untouched");

        // Idempotent: a second unload removes nothing (all zeros).
        let mut reg = TeardownRegistries {
            media: &mut Default::default(),
            agenda: &mut Default::default(),
            commands: &mut commands,
            pickers: &mut pickers,
            modes: &mut modes,
            keymap: &keymap,
            config: &config,
            bus: &bus,
            decorations: &mut decorations,
            contexts: &mut contexts,
            theme: &theme_reg,
            parsers: &parsers,
        };
        assert_eq!(teardown.unload(&mut reg), TeardownReport::default());
    }
}
