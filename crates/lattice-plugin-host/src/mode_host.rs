//! The `modes` guest→host mode-declaration seam (PH7.11a).
//!
//! A mode plugin implements the `modes-plugin` world: it **imports** the `modes`
//! API (`register-mode`) and **exports** `register-modes` (the host calls it once
//! to drive declaration). This module holds the `bindgen!` for that world plus
//! the host-side registration logic — building a marker [`Mode`] impl
//! ([`PluginMode`], the `EmacsKeysMode` template) from the declaration and
//! registering it into the SAME [`ModeRegistry`](lattice_mode::ModeRegistry)
//! builtins use, so `:describe-mode` / mode introspection treat it uniformly.
//!
//! **The canonical API is the WIT** (`modes.wit`) — any component-model language
//! calls `register-mode` directly. The mapping + register logic live here so they
//! are unit-testable without a `Store` (the `config_host` / `host_services`
//! precedent).
//!
//! Registration flow (the `register-grammar` drain precedent): `register-mode`
//! records the declaration into the Store's [`ModeContributions`]; after the
//! guest's `register-modes` export returns, [`PluginHost::spawn_mode_plugin`]
//! drains them and registers each into a `&mut ModeRegistry` (registration needs
//! `&mut`, not a live handle — unlike config's `Arc<ConfigRegistry>`).
//!
//! PH7.11a lands mode declaration + registration; PH7.11b the keymap bindings.
//! **OM.2 adds major modes**, because a plugin-contributed *language* can get a
//! major no other way: `major_mode_id_for_lang` is a hand-written match over the
//! `Lang` enum and has no arm for `Lang::Plugin(_)`. A declared major claims its
//! language through `target-language`, the registry indexes it
//! (`ModeRegistry::find_major_for_lang`), and a document of that language
//! activates it through the same resolver a built-in major uses.
//!
//! Lifecycle callbacks, decorations and typed option-overrides remain deferred
//! (fragment / Phase 8).

use lattice_grammar::source::SourceLocation;
use lattice_grammar::{CommandInvocation, CommandRegistry};
use lattice_keymap::{BindingMode, KeymapCapability, KeymapHandle, KeymapLayer};
use lattice_mode::{
    ActivationPolicy, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, TrustTier,
    arm_store, classify_trap,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "modes-plugin",
        path: "../../wit",
        // `register-modes` is wired into the same async linker as WASI + the
        // `modes` host func, so the export is async (the `config` / `events`
        // precedent: async export, sync `register-mode` host func).
        exports: { default: async },
    });
}

/// Major vs minor — the host-side mirror of the WIT `mode-kind`, kept as a plain
/// enum (no bindgen type) so the registration logic is unit-testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PluginModeKind {
    Major,
    Minor,
}

/// One keymap binding a mode contributes (PH7.11b) — the native projection of
/// the WIT `mode-keymap-binding`. `command` is resolved by name against the
/// `CommandRegistry` at bind time.
pub(crate) struct PluginKeymapBinding {
    pub mode: BindingMode,
    pub chord: String,
    pub command: String,
}

/// A native intermediate for one declared mode, projected from the WIT
/// `mode-declaration` at the Host-impl boundary so the register logic here needs
/// no bindgen types.
pub(crate) struct PluginModeDecl {
    pub id: String,
    pub kind: PluginModeKind,
    pub policy: ActivationPolicy,
    pub caps: CapabilitySet,
    pub keymap: Vec<PluginKeymapBinding>,
    /// OM.2: the language a MAJOR claims (`Some("org")`). Ignored on a minor.
    pub target_language: Option<String>,
}

/// The per-plugin accumulator the `modes::Host` impl records into during
/// `register-modes` (`lib.rs`). Drained by [`PluginHost::spawn_mode_plugin`]
/// after the export returns (the `GrammarContributions` precedent).
#[derive(Default)]
pub(crate) struct ModeContributions {
    recorded: Vec<PluginModeDecl>,
}

impl ModeContributions {
    /// Record a declaration (the `register-mode` host-func body).
    pub fn record(&mut self, decl: PluginModeDecl) {
        self.recorded.push(decl);
    }

    /// Drain the recorded declarations, leaving the accumulator empty.
    pub fn take(&mut self) -> Vec<PluginModeDecl> {
        std::mem::take(&mut self.recorded)
    }
}

/// A plugin-declared mode — a marker `Mode` (the `EmacsKeysMode` shape): it
/// carries an id + kind + activation policy + capability requirements, allocates
/// no per-buffer resources (`Guard = ()`), and its `on_activate` is a no-op. The
/// mode's *behavior* is composed from the other seams — keymap bindings (PH7.11b)
/// bind its chords to commands; action bodies arrive via the grammar
/// `register-action` trampoline (PH7.7). Lifecycle callbacks are Phase 8.
struct PluginMode {
    id: ModeId,
    kind: ModeKind,
    policy: ActivationPolicy,
    caps: CapabilitySet,
    /// OM.2: the language this mode is the default major for. Already filtered
    /// to majors by `register_plugin_mode`, and filtered again by the registry
    /// — belt and braces, because installing a minor as a buffer's major is
    /// not a failure that announces itself.
    target_language: Option<String>,
}

impl Mode for PluginMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        self.id
    }

    fn kind(&self) -> ModeKind {
        self.kind
    }

    fn target_language(&self) -> Option<&str> {
        self.target_language.as_deref()
    }

    fn activation_policy(&self) -> ActivationPolicy {
        self.policy.clone()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        self.caps
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// The `register-mode` host-service body (PH7.11a; majors OM.2). Builds a
/// [`PluginMode`] from `decl` and registers it into the SAME `ModeRegistry`
/// builtins use, returning the registered [`ModeId`] on success. Returns `None`
/// (registering nothing) when `ModeRegistry::register` rejects it (missing
/// `-mode` suffix, id collision) — logged, never a panic (graceful
/// degradation).
///
/// A `target-language` on a MINOR is dropped with a warning rather than
/// carried: a buffer has exactly one major, so indexing a minor's claim would
/// install it as that major. The registry refuses the same thing independently;
/// saying it here means the plugin author reads a message naming their mode.
pub(crate) fn register_plugin_mode(
    registry: &mut ModeRegistry,
    decl: &PluginModeDecl,
) -> Option<ModeId> {
    let kind = match decl.kind {
        PluginModeKind::Major => ModeKind::Major,
        PluginModeKind::Minor => ModeKind::Minor,
    };
    let target_language = match (decl.kind, decl.target_language.as_deref()) {
        (PluginModeKind::Major, lang) => lang.map(str::to_owned),
        (PluginModeKind::Minor, Some(lang)) => {
            tracing::warn!(
                mode = %decl.id,
                %lang,
                "register-mode: target-language ignored on a minor mode; only a \
                 major can own a language (a minor rides one via activation-policy)"
            );
            None
        }
        (PluginModeKind::Minor, None) => None,
    };
    let mode = PluginMode {
        id: ModeId::new(&decl.id),
        kind,
        policy: decl.policy.clone(),
        caps: decl.caps,
        target_language,
    };
    // CI.3: a plugin mode registers **available but not enabled** — the user
    // enables it (`enable-mode` / init.rs), the plugin author does not seize
    // auto-activation (config-and-init.md §6).
    match registry.register_available(mode) {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::warn!(mode = %decl.id, %error, "register-mode rejected by the registry");
            None
        }
    }
}

/// Bind a plugin mode's declared keymap into its OWN layer (PH7.11b),
/// returning the number of bindings that landed. Each binding resolves its
/// command name against `commands` and installs a capability-gated write with
/// [`KeymapCapability::OwnedLayer`] — so the mode can write ONLY its own layer
/// (the write-gate). An unparseable chord, an unknown command, or a capability
/// denial skips that one binding with a `warn!` (graceful degradation), never a
/// panic. Provenance is `SourceLocation::plugin(plugin_id)` — host-issued, so the
/// binding traces to the plugin (§6).
///
/// OM.2: which layer follows the mode's KIND — `MajorMode(id)` for a major,
/// `MinorMode(id)` for a minor. Both are gated tries keyed by mode id, merged
/// in active-modes order at lookup, so a minor overlays its major and neither
/// touches the built-in vim grammar.
pub(crate) fn bind_mode_keymap(
    keymap: &KeymapHandle,
    commands: &CommandRegistry,
    plugin_id: u32,
    mode_id: &ModeId,
    kind: ModeKind,
    bindings: &[PluginKeymapBinding],
) -> usize {
    let capability = KeymapCapability::OwnedLayer { mode_id: *mode_id };
    let layer = match kind {
        ModeKind::Major => KeymapLayer::MajorMode(*mode_id),
        ModeKind::Minor => KeymapLayer::MinorMode(*mode_id),
    };
    let mut bound = 0;
    for binding in bindings {
        let Some(command_id) = commands.id_by_name(&binding.command) else {
            tracing::warn!(
                mode = %mode_id.as_str(),
                command = %binding.command,
                "mode keymap binding skipped: command not registered"
            );
            continue;
        };
        match keymap.try_bind_chord_string(
            capability,
            layer,
            binding.mode,
            &binding.chord,
            CommandInvocation::of(command_id),
            SourceLocation::plugin(plugin_id),
        ) {
            Ok(()) => bound += 1,
            Err(error) => {
                tracing::warn!(
                    mode = %mode_id.as_str(),
                    chord = %binding.chord,
                    %error,
                    "mode keymap binding skipped"
                );
            }
        }
    }
    bound
}

impl PluginHost {
    /// Instantiate a `modes-plugin` component under its capability grant, run its
    /// `register-modes` export to declare modes, register each into `registry`,
    /// and return the successfully-registered [`ModeId`]s. Grant / data-dir /
    /// WASI are identical to
    /// [`instantiate_plugin`](PluginHost::instantiate_plugin) (shared
    /// `build_plugin_wasi` + `new_store`).
    ///
    /// Registration is drained AFTER `register-modes` returns because
    /// `ModeRegistry::register` needs `&mut ModeRegistry` — unlike config's live
    /// `Arc<ConfigRegistry>` handle. A declaration the registry rejects is logged +
    /// skipped (not in the returned ids); the teardown seam (PH7.12) will
    /// remove a plugin's modes.
    pub async fn spawn_mode_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        registry: &mut ModeRegistry,
        commands: &CommandRegistry,
        keymap: &KeymapHandle,
    ) -> Result<(PluginId, Vec<ModeId>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "mode plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            bindings::ModesPlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let plugin_id = self.alloc_id();
        // PO.5: route this plugin's `logging` calls into the tracer (Layer 2) —
        // before register-modes, so the guest may narrate from there.
        store.data_mut().log_ctx = self.log_ctx_for(plugin_id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_modes(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-modes",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        // Register each mode, then bind its keymap into its OWN layer
        // (PH7.11b, capability-gated) — `MajorMode` or `MinorMode` per the
        // declared kind (OM.2). A mode the registry rejects contributes no
        // keymap (its layer never exists).
        let recorded = store.data_mut().mode_contributions.take();
        let mut ids = Vec::with_capacity(recorded.len());
        for decl in recorded {
            if let Some(id) = register_plugin_mode(registry, &decl) {
                let kind = match decl.kind {
                    PluginModeKind::Major => ModeKind::Major,
                    PluginModeKind::Minor => ModeKind::Minor,
                };
                bind_mode_keymap(keymap, commands, plugin_id.0, &id, kind, &decl.keymap);
                ids.push(id);
            }
        }
        // Surface the host-issued `plugin_id` alongside the accepted modes: the
        // loader records it for provenance (`:list-plugins`) + teardown-by-id
        // (PL8.C), consistent with the other seam spawns (picker / config /
        // events). The modes themselves are declarative data now living in the
        // registry, so the guest `store` / `bindings` drop here — no handle to
        // keep alive.
        Ok((plugin_id, ids))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn minor(id: &str) -> PluginModeDecl {
        PluginModeDecl {
            id: id.to_string(),
            kind: PluginModeKind::Minor,
            policy: ActivationPolicy::Manual,
            caps: CapabilitySet::empty(),
            keymap: Vec::new(),
            target_language: None,
        }
    }

    /// OM.2: a major claiming a language — the org shape.
    fn major_for(id: &str, lang: &str) -> PluginModeDecl {
        PluginModeDecl {
            id: id.to_string(),
            kind: PluginModeKind::Major,
            policy: ActivationPolicy::Manual,
            caps: CapabilitySet::empty(),
            keymap: Vec::new(),
            target_language: Some(lang.to_string()),
        }
    }

    #[test]
    fn registers_a_minor_mode_into_the_registry() {
        let mut registry = ModeRegistry::default();
        let id = register_plugin_mode(&mut registry, &minor("git-blame-mode"))
            .expect("a well-formed minor mode registers");
        assert_eq!(id.as_str(), "git-blame-mode");
        assert!(registry.is_registered(ModeId::new("git-blame-mode")));
    }

    #[test]
    fn a_bare_id_without_the_mode_suffix_is_rejected() {
        let mut registry = ModeRegistry::default();
        assert!(
            register_plugin_mode(&mut registry, &minor("git-blame")).is_none(),
            "the registry enforces the `-mode` suffix"
        );
        assert!(!registry.is_registered(ModeId::new("git-blame")));
    }

    /// OM.2 — the inverse of what this test used to assert. A plugin
    /// contributing a language must be able to contribute its major, because
    /// the host's `major_mode_id_for_lang` table has no arm for a language it
    /// has never heard of.
    #[test]
    fn a_major_registers_and_claims_its_language() {
        let mut registry = ModeRegistry::default();
        let id = register_plugin_mode(&mut registry, &major_for("org-mode", "org"))
            .expect("a well-formed major registers");
        assert_eq!(id.as_str(), "org-mode");
        assert_eq!(
            registry.get(id).expect("registered").kind(),
            ModeKind::Major,
            "it registers AS a major, not silently downgraded to a minor"
        );
        assert_eq!(
            registry.find_major_for_lang("org"),
            Some(id),
            "and the language index resolves documents onto it"
        );
    }

    /// A major need not claim a language — that is manual activation, and the
    /// index must stay empty rather than gaining a `None` key.
    #[test]
    fn a_major_without_a_language_registers_but_claims_nothing() {
        let mut registry = ModeRegistry::default();
        let mut decl = major_for("scratch-mode", "unused");
        decl.target_language = None;
        let id = register_plugin_mode(&mut registry, &decl).expect("registers");
        assert_eq!(
            registry.get(id).expect("registered").kind(),
            ModeKind::Major
        );
        assert_eq!(registry.find_major_for_lang("unused"), None);
    }

    /// A minor's language claim is dropped, not honoured — indexing it would
    /// install the minor as every org buffer's major.
    #[test]
    fn a_minor_claiming_a_language_registers_without_the_claim() {
        let mut registry = ModeRegistry::default();
        let mut decl = minor("org-todo-mode");
        decl.target_language = Some("org".to_string());
        let id = register_plugin_mode(&mut registry, &decl).expect("the mode still registers");
        assert_eq!(
            registry.get(id).expect("registered").kind(),
            ModeKind::Minor
        );
        assert_eq!(
            registry.find_major_for_lang("org"),
            None,
            "the claim was dropped, so org documents do not resolve onto a minor"
        );
    }

    /// The write-gate follows the kind: a major's bindings land in its own
    /// `MajorMode` layer under the same `OwnedLayer` capability a minor uses.
    #[test]
    fn a_majors_keymap_lands_in_its_own_major_layer() {
        let keymap = KeymapHandle::new();
        let mut commands = CommandRegistry::new();
        let _ = lattice_grammar::ex_commands::populate(&mut commands);
        let mode_id = ModeId::new("org-mode");
        let bindings = vec![PluginKeymapBinding {
            mode: BindingMode::Normal,
            chord: "<C-s>".to_string(),
            command: "ex:write".to_string(),
        }];

        let bound = bind_mode_keymap(&keymap, &commands, 3, &mode_id, ModeKind::Major, &bindings);
        assert_eq!(bound, 1, "the binding landed");

        let chord = lattice_protocol::parse_chord_sequence("<C-s>").unwrap();
        assert!(
            matches!(
                keymap.lookup_with_context(BindingMode::Normal, &chord, &[mode_id]),
                lattice_keymap::LookupResult::Bound { .. }
            ),
            "resolves when the major is active"
        );
        assert!(
            matches!(
                keymap.lookup_with_context(BindingMode::Normal, &chord, &[]),
                lattice_keymap::LookupResult::Unbound
            ),
            "a major's layer is gated too — it is not always-on (K.1.c)"
        );
    }

    #[test]
    fn a_duplicate_id_is_rejected_keeping_the_original() {
        let mut registry = ModeRegistry::default();
        assert!(register_plugin_mode(&mut registry, &minor("dup-mode")).is_some());
        assert!(
            register_plugin_mode(&mut registry, &minor("dup-mode")).is_none(),
            "a second registration under the same id is refused"
        );
    }

    #[test]
    fn capabilities_and_policy_are_carried_onto_the_registered_mode() {
        let mut registry = ModeRegistry::default();
        let decl = PluginModeDecl {
            id: "lsp-lens-mode".to_string(),
            kind: PluginModeKind::Minor,
            policy: ActivationPolicy::Universal,
            caps: CapabilitySet::LSP | CapabilitySet::DIAGNOSTICS,
            keymap: Vec::new(),
            target_language: None,
        };
        let id = register_plugin_mode(&mut registry, &decl).unwrap();
        let mode = registry.get(id).expect("registered");
        assert!(matches!(
            mode.activation_policy(),
            ActivationPolicy::Universal
        ));
        assert_eq!(
            mode.required_capabilities(),
            CapabilitySet::LSP | CapabilitySet::DIAGNOSTICS
        );
    }

    #[test]
    fn keymap_binding_lands_in_the_owned_layer_and_resolves() {
        use lattice_keymap::LookupResult;

        // A command the binding targets by name.
        let mut commands = CommandRegistry::new();
        let _ = lattice_grammar::ex_commands::populate(&mut commands);
        let target = "ex:write";
        assert!(
            commands.id_by_name(target).is_some(),
            "sanity: target exists"
        );

        let keymap = KeymapHandle::new();
        let mode_id = ModeId::new("git-blame-mode");
        let bindings = vec![PluginKeymapBinding {
            mode: BindingMode::Normal,
            chord: "<C-s>".to_string(),
            command: target.to_string(),
        }];
        let bound = bind_mode_keymap(&keymap, &commands, 7, &mode_id, ModeKind::Minor, &bindings);
        assert_eq!(bound, 1, "the well-formed binding landed");

        // Build the lookup chord the same way the binding parsed it.
        let chord = lattice_protocol::parse_chord_sequence("<C-s>").expect("chord parses");

        // The binding resolves ONLY when the mode is active (gated layer).
        assert!(
            matches!(
                keymap.lookup_with_context(BindingMode::Normal, &chord, &[mode_id]),
                LookupResult::Bound { .. }
            ),
            "the chord resolves in the mode's owned layer when active"
        );
        assert!(
            matches!(
                keymap.lookup_with_context(BindingMode::Normal, &chord, &[]),
                LookupResult::Unbound
            ),
            "with the mode inactive the gated binding does not fire"
        );
    }

    #[test]
    fn keymap_binding_to_an_unknown_command_is_skipped() {
        let commands = CommandRegistry::new();
        let keymap = KeymapHandle::new();
        let bindings = vec![PluginKeymapBinding {
            mode: BindingMode::Normal,
            chord: "gx".to_string(),
            command: "ex:does-not-exist".to_string(),
        }];
        let bound = bind_mode_keymap(
            &keymap,
            &commands,
            1,
            &ModeId::new("x-mode"),
            ModeKind::Minor,
            &bindings,
        );
        assert_eq!(
            bound, 0,
            "an unknown command binds nothing (logged + skipped)"
        );
    }
}
