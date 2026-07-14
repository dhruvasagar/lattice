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
//! PH7.11a lands minor-mode declaration + registration only. Keymap bindings
//! (PH7.11b), lifecycle callbacks, decorations, typed option-overrides, and major
//! modes are deferred (fragment / Phase 8).

use lattice_grammar::source::SourceLocation;
use lattice_grammar::{CommandInvocation, CommandRegistry};
use lattice_keymap::{BindingMode, KeymapCapability, KeymapHandle, KeymapLayer};
use lattice_mode::{
    ActivationPolicy, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier, arm_store,
    classify_trap,
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

/// A plugin-declared minor mode — a marker `Mode` (the `EmacsKeysMode` shape):
/// it carries an id + activation policy + capability requirements, allocates no
/// per-buffer resources (`Guard = ()`), and its `on_activate` is a no-op. The
/// mode's *behavior* is composed from the other seams — keymap bindings (PH7.11b)
/// bind its chords to commands; action bodies arrive via the grammar
/// `register-action` trampoline (PH7.7). Lifecycle callbacks are Phase 8.
struct PluginMode {
    id: ModeId,
    policy: ActivationPolicy,
    caps: CapabilitySet,
}

impl Mode for PluginMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        self.id.clone()
    }

    fn kind(&self) -> ModeKind {
        // PH7.11a registers minor modes only (majors are Phase 8; the register
        // path rejects `major` before constructing a `PluginMode`).
        ModeKind::Minor
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

/// The `register-mode` host-service body (PH7.11a). Builds a [`PluginMode`] from
/// `decl` and registers it into the SAME `ModeRegistry` builtins use, returning
/// the registered [`ModeId`] on success. Returns `None` (registering nothing) if
/// the kind is `major` (Phase 8) OR `ModeRegistry::register` rejects it (missing
/// `-mode` suffix, id collision) — logged, never a panic (graceful degradation).
pub(crate) fn register_plugin_mode(
    registry: &mut ModeRegistry,
    decl: &PluginModeDecl,
) -> Option<ModeId> {
    if decl.kind != PluginModeKind::Minor {
        tracing::warn!(
            mode = %decl.id,
            "register-mode skipped: only minor modes are supported in PH7.11a (majors are Phase 8)"
        );
        return None;
    }
    let mode = PluginMode {
        id: ModeId::new(&decl.id),
        policy: decl.policy.clone(),
        caps: decl.caps,
    };
    match registry.register(mode) {
        Ok(id) => Some(id),
        Err(error) => {
            tracing::warn!(mode = %decl.id, %error, "register-mode rejected by the registry");
            None
        }
    }
}

/// Bind a plugin mode's declared keymap into its OWN `MinorMode(mode_id)` layer
/// (PH7.11b), returning the number of bindings that landed. Each binding resolves
/// its command name against `commands` and installs a capability-gated write with
/// [`KeymapCapability::OwnedLayer`] — so the mode can write ONLY its own layer
/// (the write-gate). An unparseable chord, an unknown command, or a capability
/// denial skips that one binding with a `warn!` (graceful degradation), never a
/// panic. Provenance is `SourceLocation::plugin(plugin_id)` — host-issued, so the
/// binding traces to the plugin (§6).
pub(crate) fn bind_mode_keymap(
    keymap: &KeymapHandle,
    commands: &CommandRegistry,
    plugin_id: u32,
    mode_id: &ModeId,
    bindings: &[PluginKeymapBinding],
) -> usize {
    let capability = KeymapCapability::OwnedLayer {
        mode_id: mode_id.clone(),
    };
    let layer = KeymapLayer::MinorMode(mode_id.clone());
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
            layer.clone(),
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
    /// `Arc<ConfigRegistry>` handle. A declaration the registry rejects is logged
    /// + skipped (not in the returned ids); the teardown seam (PH7.12) will
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
    ) -> Result<Vec<ModeId>, PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "mode plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget)?;
        let bindings =
            bindings::ModesPlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let plugin_id = self.alloc_id();

        arm_store(&mut store, budget)?;
        bindings
            .call_register_modes(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-modes",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        // Register each mode, then bind its keymap into its OWN MinorMode layer
        // (PH7.11b, capability-gated). A mode the registry rejects contributes no
        // keymap (its layer never exists).
        let recorded = store.data_mut().mode_contributions.take();
        let mut ids = Vec::with_capacity(recorded.len());
        for decl in recorded {
            if let Some(id) = register_plugin_mode(registry, &decl) {
                bind_mode_keymap(keymap, commands, plugin_id.0, &id, &decl.keymap);
                ids.push(id);
            }
        }
        Ok(ids)
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

    #[test]
    fn a_major_kind_is_rejected_in_phase_7() {
        let mut registry = ModeRegistry::default();
        let mut decl = minor("rust-mode");
        decl.kind = PluginModeKind::Major;
        assert!(
            register_plugin_mode(&mut registry, &decl).is_none(),
            "majors are Phase 8"
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
        let bound = bind_mode_keymap(&keymap, &commands, 7, &mode_id, &bindings);
        assert_eq!(bound, 1, "the well-formed binding landed");

        // Build the lookup chord the same way the binding parsed it.
        let chord = lattice_protocol::parse_chord_sequence("<C-s>").expect("chord parses");

        // The binding resolves ONLY when the mode is active (gated layer).
        assert!(
            matches!(
                keymap.lookup_with_context(BindingMode::Normal, &chord, &[mode_id.clone()]),
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
        let bound = bind_mode_keymap(&keymap, &commands, 1, &ModeId::new("x-mode"), &bindings);
        assert_eq!(
            bound, 0,
            "an unknown command binds nothing (logged + skipped)"
        );
    }
}
