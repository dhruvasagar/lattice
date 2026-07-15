//! SN.3c.0: boot-time registration of modes' declarative *global*
//! action handlers ([`Mode::action_handlers`](lattice_mode::Mode::action_handlers)).
//!
//! Sibling to [`crate::keymap_mode_contributions::translate_mode_keymaps`]:
//! both walk the mode registry once at boot to apply a declarative
//! `Mode` contribution. This one resolves each contribution's
//! `action_name` to a `CommandId` and registers the handler in the
//! shared [`ActionHandlerRegistry`](lattice_mode::ActionHandlerRegistry),
//! returning the RAII tokens. The host holds them for the app's
//! lifetime — the correct scope for *buffer-agnostic* handlers that
//! read the active buffer/cursor/services from the `ActionContext`
//! at call time (see `feedback_effect_vocabulary_is_host_boundary`).
//! Per-buffer, session-scoped handlers register in
//! `Mode::on_activate` instead, so their tokens drop with the Guard.

use lattice_grammar::CommandRegistry;
use lattice_mode::{ActionHandlerRegistration, ActionHandlerRegistryHandle, ModeRegistry};

/// Walk every registered mode's `action_handlers()`, resolve each
/// contribution's `action_name` → `CommandId`, register the handler
/// globally, and return the registration tokens (held by the caller
/// for the app's lifetime).
///
/// An unknown `action_name` (the mode declared a handler for a
/// command the registry doesn't know — a wiring bug) is skipped
/// with a `debug!`, non-fatal: the chord falls through to the
/// default dispatch path.
pub fn register_mode_action_handlers(
    action_handlers: &ActionHandlerRegistryHandle,
    mode_registry: &ModeRegistry,
    command_registry: &CommandRegistry,
) -> Vec<ActionHandlerRegistration> {
    let mut tokens = Vec::new();
    for (mode_id, mode) in mode_registry.iter() {
        for contribution in mode.action_handlers() {
            match command_registry.id_by_name(contribution.action_name) {
                Some(id) => tokens.push(action_handlers.register(id, contribution.handler)),
                None => tracing::debug!(
                    mode = %mode_id,
                    action = contribution.action_name,
                    "mode action handler skipped: command not registered"
                ),
            }
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_grammar::registry::ActionSpec;
    use lattice_mode::{
        ActionContext, ActionHandler, ActionHandlerContribution, ActionHandlerRegistry,
        CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    };
    use std::sync::Arc;

    /// Minimal mode that contributes one global action handler for
    /// a configurable command name.
    struct HandlerMode {
        id: ModeId,
        action_name: &'static str,
    }

    impl Mode for HandlerMode {
        type Guard = ();
        fn id(&self) -> ModeId {
            self.id
        }
        fn kind(&self) -> ModeKind {
            ModeKind::Minor
        }
        fn required_capabilities(&self) -> CapabilitySet {
            CapabilitySet::empty()
        }
        fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
            let handler: ActionHandler = Arc::new(|_ctx: &ActionContext<'_>| None);
            vec![ActionHandlerContribution {
                action_name: self.action_name,
                handler,
            }]
        }
        fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
            Box::pin(async { Ok(()) })
        }
    }

    fn cmd_registry_with(name: &str) -> (CommandRegistry, lattice_protocol::ids::CommandId) {
        let mut cmd = CommandRegistry::new();
        let id = cmd.register_action(
            name,
            "test action",
            ActionSpec {
                apply: Arc::new(|_| Ok(lattice_grammar::effect::Effect::None)),
                args_schema: Vec::new(),
            },
        );
        (cmd, id)
    }

    #[test]
    fn registers_contributed_handler_and_tokens_control_lifetime() {
        let (cmd, id) = cmd_registry_with("test:act");
        let mut modes = ModeRegistry::new();
        modes
            .register(HandlerMode {
                id: ModeId::new("handler-mode"),
                action_name: "test:act",
            })
            .unwrap();
        let action_handlers: ActionHandlerRegistryHandle = Arc::new(ActionHandlerRegistry::new());

        let tokens = register_mode_action_handlers(&action_handlers, &modes, &cmd);
        assert_eq!(tokens.len(), 1, "one contribution registered");
        assert!(
            action_handlers.lookup(id).is_some(),
            "handler resolvable globally after the boot walk"
        );

        // The returned tokens own the registration's lifetime
        // (app-lifetime when boot holds them); dropping them
        // unregisters.
        drop(tokens);
        assert!(
            action_handlers.lookup(id).is_none(),
            "dropping the boot tokens unregisters the handler"
        );
    }

    #[test]
    fn unknown_action_name_is_skipped_not_panicked() {
        let cmd = CommandRegistry::new(); // empty — name won't resolve
        let mut modes = ModeRegistry::new();
        modes
            .register(HandlerMode {
                id: ModeId::new("handler-mode"),
                action_name: "test:nonexistent",
            })
            .unwrap();
        let action_handlers: ActionHandlerRegistryHandle = Arc::new(ActionHandlerRegistry::new());

        let tokens = register_mode_action_handlers(&action_handlers, &modes, &cmd);
        assert!(tokens.is_empty(), "unresolvable name contributes no token");
        assert_eq!(action_handlers.registered_count(), 0);
    }
}
