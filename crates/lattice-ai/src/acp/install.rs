//! ACP transport boot wiring.
//!
//! Spawns the supervisor (owns the provider child process, the ACP connection,
//! and the active session for the program's lifetime), wires the
//! `:opencode` / `:ai-prompt` / `:ai-stop` ex-commands, and registers the
//! `AiClientHandle` service. Called by the crate-root `install` (behind
//! `#[cfg(feature = "acp")]`), which owns the transport-neutral log substrate.

use std::sync::Arc;

use lattice_agent::AiLogger;
use lattice_mode::SubsystemBoot;

use crate::acp::commands::register_ai_ex_commands;
use crate::acp::conversation::ConversationStore;
use crate::acp::conversation_mode::AiConversationMode;
use crate::acp::handle::AiClientHandle;

/// Wire the ACP (Agent Client Protocol) transport into the editor at boot. The
/// `logger` is the port-level `AiLogger` the supervisor streams *trace* records
/// into; the crate-root `install` already registered it as a service.
pub fn install(boot: &mut impl SubsystemBoot, logger: &AiLogger) {
    // The structured conversation store: the supervisor folds agent *conversation*
    // output into it and publishes `ConversationUpdated` on the event bus so the
    // `ai-conversation` mode (AU-2) can live-tail. Registered as a service so the
    // mode can read snapshots.
    let bus = boot.event_bus().clone();
    let conv_store = ConversationStore::new(Arc::new(move |event| bus.publish_typed(event)));

    // The `ai-conversation` major mode backs the `*ai:opencode*` buffer; it
    // reads the ConversationStore service and live-tails via ConversationUpdated.
    boot.modes_mut()
        .register(AiConversationMode::new())
        .expect("ai-conversation-mode register");

    // PU-B: the `ai-permission` major mode backs the `*ai-permission*` popup
    // menu; its `on_activate` reads the ConversationStore and projects the
    // oldest pending request.
    boot.modes_mut()
        .register(crate::acp::permission_mode::AiPermissionMode::new())
        .expect("ai-permission-mode register");

    // AU‑4: the host-registered programmatic-diff bus (the `review_diff`
    // producer side), shared with MCP's `openDiff`. `None` if absent → edit
    // permissions can't be reviewed and are denied (graceful). The host owns
    // the receiver and resolves verdicts on `:diff-accept` / `:diff-reject`.
    let diff_bus = boot
        .service::<lattice_diff::ProgrammaticDiffBus>()
        .map(|h| (*h).clone());

    // Spawn the supervisor with a logger clone (trace) + the conversation store
    // + the diff bus (edit review).
    let handle = AiClientHandle::spawn(
        boot.runtime_handle(),
        logger.clone(),
        conv_store.clone(),
        diff_bus,
    );

    // Crate-owned ex-commands: `:opencode` / `:ai-prompt` / `:ai-stop`.
    register_ai_ex_commands(boot.commands_mut(), handle.clone());

    // AU‑3: the modal-input action commands (`action:ai-conv-*`) the mode's
    // keymap binds. Declaration here (mode owns its surface); the handler
    // bodies live in `AiConversationMode::action_handlers`.
    crate::acp::conversation_mode::register_ai_conversation_actions(boot.commands_mut());

    // PU-B: the `ai-permission` menu's action commands (`action:ai-perm-*`).
    crate::acp::permission_mode::register_ai_permission_actions(boot.commands_mut());

    // PU-B.3: the permission-menu auto-open coordinator (menu-open gate +
    // Esc-deferred set), shared by `ai-permission-mode` and the auto-open tick
    // callback below. Registered as a service (the `Arc<X>` handle convention).
    let coordinator: crate::acp::permission_mode::PermissionMenuCoordinatorHandle =
        Arc::new(crate::acp::permission_mode::PermissionMenuCoordinator::new());
    boot.register_service::<crate::acp::permission_mode::PermissionMenuCoordinatorHandle>(
        coordinator.clone(),
    );

    // PU-B.3: the auto-open tick callback. `run_tick_pending` runs it on the
    // actor's `async_landed` wake (below) — no keystroke — so a permission that
    // becomes pending while the user is idle opens the menu on its own. The body
    // is mode-owned (`permission_mode::auto_open_tick`); the host only runs the
    // closure + applies the returned `Effect::OpenPopup`.
    {
        let conv_store = conv_store.clone();
        let coordinator = coordinator.clone();
        boot.tick_callback(Box::new(move || {
            crate::acp::permission_mode::auto_open_tick(&conv_store, &coordinator)
        }));
    }

    // Services: `AiClientHandle` for a future modeline/UI; `ConversationStore`
    // for the `ai-conversation` mode's projection.
    boot.register_service::<AiClientHandle>(handle);
    boot.register_service::<ConversationStore>(conv_store);

    // Repaint wake: the `ai-conversation` drain fires `ConversationProjected`
    // AFTER each re-projection edit lands. Wake the editor actor on it so a
    // streamed agent response repaints (and the prompt-focus tick callback runs)
    // without needing a keystroke. Sequenced after the edit — via this event,
    // not `ConversationUpdated` — so the wake never paints stale content.
    boot.wake_on_event::<crate::acp::conversation::ConversationProjected>();

    // PU-B.3: also wake on `ConversationUpdated` (fired by the supervisor's
    // `push_permission_request`) so the auto-open tick callback runs the instant
    // a permission goes pending — even when the `*ai:opencode*` buffer isn't
    // focused, so its `ConversationProjected` drain isn't the one waking us.
    boot.wake_on_event::<crate::acp::conversation::ConversationUpdated>();
}
