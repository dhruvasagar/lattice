//! Asking the user before a tool runs, for every producer that asks (LL.18).
//!
//! The `*ai-permission*` popup ([`mode`]) began as ACP's (PU-B). Its data was
//! already protocol-neutral (LL.1's [`ConversationStore`] permission blocks);
//! what tied it to ACP was that it read ACP's one store. An llm chat has a
//! store per session, so the popup now reads every store that can ask, through
//! [`PermissionSources`]: each producer adds a function listing its stores,
//! and the popup, its auto-open and `:ai-permission` look across all of them.
//!
//! The popup resolves a request on the store that holds it, so two producers'
//! request ids never need to be distinct.

pub mod mode;

use std::sync::{Arc, RwLock};

use lattice_grammar::effect::Effect;
use lattice_mode::SubsystemBoot;

use crate::conversation::{ConversationStore, PendingPermissionView};

pub use mode::{
    AiPermissionMode, PERMISSION_BUFFER_NAME, PermissionMenuCoordinator,
    PermissionMenuCoordinatorHandle,
};

/// Lists a producer's stores now. Asked each time, so a session made later is
/// seen without registering it.
pub type StoresFn = Arc<dyn Fn() -> Vec<ConversationStore> + Send + Sync>;

/// Every store a permission request may sit in.
#[derive(Default)]
pub struct PermissionSources {
    sources: RwLock<Vec<StoresFn>>,
}

/// The service handle (the `Arc<X>` registration convention).
pub type PermissionSourcesHandle = Arc<PermissionSources>;

impl PermissionSources {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a producer's stores.
    pub fn add(&self, stores: StoresFn) {
        self.sources
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .push(stores);
    }

    fn stores(&self) -> Vec<ConversationStore> {
        let sources = self
            .sources
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        sources.iter().flat_map(|f| f()).collect()
    }

    /// The first pending request whose id `keep` accepts, with the store
    /// that holds it: producers in the order they were added, each store's
    /// requests in the order they arrived.
    pub fn oldest_pending_where(
        &self,
        keep: impl Fn(&str) -> bool,
    ) -> Option<(ConversationStore, PendingPermissionView)> {
        self.stores().into_iter().find_map(|store| {
            let pending = store.oldest_pending_permission_where(&keep)?;
            Some((store, pending))
        })
    }
}

/// The popup that opens `*ai-permission*` over the oldest request.
pub fn open_popup() -> Effect {
    Effect::OpenPopup {
        name: PERMISSION_BUFFER_NAME.to_string(),
        mode_id: AiPermissionMode::mode_id().as_str().to_string(),
        placement: lattice_core::ui::popup::PopupPlacement::Centered,
        focus: lattice_core::ui::popup::PopupFocus::Steal,
    }
}

/// Wire the popup: its mode and actions, `:ai-permission`, the auto-open, and
/// the sources service the producers add to. Returns the sources.
pub fn install(boot: &mut impl SubsystemBoot) -> PermissionSourcesHandle {
    boot.modes_mut()
        .register(AiPermissionMode::new())
        .expect("ai-permission-mode register");
    mode::register_ai_permission_actions(boot.commands_mut());
    register_ex_command(boot.commands_mut());

    let sources: PermissionSourcesHandle = Arc::new(PermissionSources::new());
    boot.register_service::<PermissionSourcesHandle>(sources.clone());
    let coordinator: PermissionMenuCoordinatorHandle = Arc::new(PermissionMenuCoordinator::new());
    boot.register_service::<PermissionMenuCoordinatorHandle>(coordinator.clone());

    // The auto-open runs on the actor's wake, so a request that goes pending
    // while the user is idle opens the popup with no keystroke. Every
    // producer's store publishes `ConversationUpdated` when a request is
    // pushed; waking on it is what makes that reach the screen.
    {
        let sources = sources.clone();
        boot.tick_callback(Box::new(move || {
            mode::auto_open_tick(&sources, &coordinator)
        }));
    }
    boot.wake_on_event::<crate::conversation::ConversationUpdated>();
    sources
}

fn register_ex_command(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};
    // Data-only: the host ensures the popup buffer under `ai-permission-mode`
    // and its `on_activate` projects the request. `Esc` defers.
    registry.register_ex_command(
        "ai-permission",
        "Open the permission menu for the oldest pending request to run a tool, \
         from an agent or a language model.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(lattice_agent::parse_no_args),
            apply: Arc::new(|_ctx| Ok(open_popup())),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}
