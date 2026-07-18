//! The `keymap` guest→host binding-registration seam (PL8.D.1).
//!
//! A keymap plugin implements the `keymap-plugin` world: it **imports** the
//! `keymap` API (`register-binding`) and **exports** `register-keymap` (the host
//! calls it once to drive registration). This module holds the `bindgen!` for
//! that world plus the host-side bind logic — factored here (the `config_host`
//! precedent) so it is unit-testable without a `Store`.
//!
//! **The canonical API is the WIT** (`keymap.wit`) — any component-model language
//! calls `register-binding` directly. The first consumer is the user's `init.rs`:
//! a plain global keybind is the one config kind with no other seam. Bindings
//! land in [`KeymapLayer::User`](lattice_keymap::KeymapLayer::User), gated by
//! [`KeymapCapability::User`](lattice_keymap::KeymapCapability::User) — above the
//! built-in vim grammar, never `KeymapLayer::Builtin` (the standing
//! keymap-ownership rule). Binding *resolution* on every keystroke stays native
//! (the `KeymapHandle` trie), so there is no per-keystroke WASM — the seam rides
//! the async linker, registration-only.

use std::sync::Arc;

use lattice_grammar::{CommandInvocation, CommandRegistry};
use lattice_keymap::{BindingMode, KeymapCapability, KeymapHandle, KeymapLayer};
use lattice_grammar::source::SourceLocation;

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginId, PluginManifest, TrustTier,
    arm_store, classify_trap,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "keymap-plugin",
        path: "../../wit",
        // `register-keymap` is wired into the SAME async linker as WASI + the
        // `keymap` host func (`lib.rs`), so the export is async (the `config`
        // world precedent). Registration is off any hot path, so async is free;
        // the `register-binding` host func itself is a sync, non-trapping `bool`.
        exports: { default: async },
    });
}

/// A user keybinding the plugin registered — the teardown token (PL8.D.2): the
/// `KeymapLayer::User` entry for `(mode, chord)` is unbound on unload. `chord`
/// is the vim-notation string as the guest supplied it (re-parsed at unbind).
#[derive(Debug, Clone)]
pub struct KeymapBindingToken {
    pub mode: BindingMode,
    pub chord: String,
}

/// The editor handles a keymap plugin's `register-binding` needs, set on
/// [`PluginState`](crate::PluginState) before `register-keymap` runs.
pub(crate) struct KeymapBindCtx {
    pub keymap: KeymapHandle,
    pub commands: Arc<CommandRegistry>,
    pub plugin_id: PluginId,
}

/// The `register-binding` host-service body: resolve `command` against the
/// command registry, then bind `chord` in `mode` into `KeymapLayer::User` under
/// `KeymapCapability::User`, stamped with the plugin's provenance. Returns
/// `false` (binding nothing) on an unregistered command, an unparseable chord, or
/// a withheld User-layer capability — logged, never a panic (graceful
/// degradation). Factored out of the `Host` impl so it is testable without a
/// guest / `Store`.
pub(crate) fn bind_user_keybinding(
    keymap: &KeymapHandle,
    commands: &CommandRegistry,
    plugin_id: u32,
    mode: BindingMode,
    chord: &str,
    command: &str,
) -> bool {
    let Some(command_id) = commands.id_by_name(command) else {
        tracing::warn!(
            command,
            chord,
            "user keybinding skipped: command not registered"
        );
        return false;
    };
    match keymap.try_bind_chord_string(
        KeymapCapability::User,
        KeymapLayer::User,
        mode,
        chord,
        CommandInvocation::of(command_id),
        SourceLocation::plugin(plugin_id),
    ) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(chord, command, %error, "user keybinding skipped");
            false
        }
    }
}

impl PluginHost {
    /// Instantiate a `keymap-plugin` component under its capability grant, run its
    /// `register-keymap` export to bind user keybindings into `keymap`
    /// (`KeymapLayer::User`), and return the tokens for teardown. Grant /
    /// data-dir / WASI are identical to
    /// [`instantiate_plugin`](PluginHost::instantiate_plugin).
    ///
    /// The keymap handle + command-registry snapshot are wired onto the `Store`
    /// BEFORE `register-keymap` runs, so the guest's imported `register-binding`
    /// reaches them. Bindings land synchronously (no drain / actor); the returned
    /// tokens are what the loader's teardown unbinds on unload.
    pub async fn spawn_keymap_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        keymap: &KeymapHandle,
        commands: &Arc<CommandRegistry>,
    ) -> Result<(PluginId, Vec<KeymapBindingToken>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "keymap plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget)?;
        let bindings =
            bindings::KeymapPlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        let plugin_id = self.alloc_id();

        // Wire the bind context BEFORE `register-keymap` runs so the guest's
        // imported `register-binding` reaches the live keymap + command registry.
        store.data_mut().keymap_ctx = Some(KeymapBindCtx {
            keymap: keymap.clone(),
            commands: Arc::clone(commands),
            plugin_id,
        });
        // PO.5: route this plugin's `logging` calls into the tracer (Layer 2) —
        // before register-keymap, so the guest may narrate from there.
        store.data_mut().log_ctx = self.log_ctx_for(plugin_id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_keymap(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-keymap",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        let tokens = store.data_mut().keymap_contributions.drain(..).collect();
        Ok((plugin_id, tokens))
    }
}

/// Project the WIT `binding-mode` onto the native [`BindingMode`] — the same
/// mapping the `modes` seam uses (the plugin-facing subset).
pub(crate) fn project_binding_mode(
    wit: bindings::lattice::plugin_host::keymap::BindingMode,
) -> BindingMode {
    use bindings::lattice::plugin_host::keymap::BindingMode as Wit;
    match wit {
        Wit::Normal => BindingMode::Normal,
        Wit::Insert => BindingMode::Insert,
        Wit::Visual => BindingMode::Visual,
        Wit::Select => BindingMode::Select,
        Wit::Replace => BindingMode::Replace,
        Wit::Command => BindingMode::Command,
        Wit::Search => BindingMode::Search,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_grammar::registry::CommandRegistry;

    /// A registry with one real ex-command the binding can resolve to.
    fn registry_with_write() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        let _ = lattice_grammar::ex_commands::populate(&mut r);
        r
    }

    #[test]
    fn binds_a_user_keybinding_into_the_user_layer() {
        let commands = registry_with_write();
        let keymap = KeymapHandle::new();
        // `ex:write` is a populated builtin; bind `<leader>w` to it.
        let bound = bind_user_keybinding(
            &keymap,
            &commands,
            7,
            BindingMode::Normal,
            "<C-s>",
            "ex:write",
        );
        assert!(bound, "a well-formed binding to a real command lands");
        assert!(keymap.binding_count() >= 1, "the User-layer binding is live");
    }

    #[test]
    fn unknown_command_binds_nothing() {
        let commands = registry_with_write();
        let keymap = KeymapHandle::new();
        assert!(
            !bind_user_keybinding(&keymap, &commands, 7, BindingMode::Normal, "<C-s>", "nope:nope"),
            "an unregistered command binds nothing"
        );
        assert_eq!(keymap.binding_count(), 0, "no binding leaked");
    }

    #[test]
    fn unparseable_chord_binds_nothing() {
        let commands = registry_with_write();
        let keymap = KeymapHandle::new();
        assert!(
            !bind_user_keybinding(&keymap, &commands, 7, BindingMode::Normal, "<not-a-chord", "ex:write"),
            "a malformed chord binds nothing"
        );
        assert_eq!(keymap.binding_count(), 0);
    }
}
