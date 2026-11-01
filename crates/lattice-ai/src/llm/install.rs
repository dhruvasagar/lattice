//! Boot wiring for the llm layer: registers the [`RequestRegistry`] service.

use std::sync::Arc;

use lattice_mode::SubsystemBoot;

use super::request::{RequestRegistry, RequestRegistryHandle};

/// Register the request registry on the boot runtime, publishing state changes
/// to the event bus. Look it up as [`RequestRegistryHandle`] (the same type used
/// here -- `Arc<X>` registered under a different `T` would silently miss).
pub fn install(boot: &mut impl SubsystemBoot) {
    let registry: RequestRegistryHandle = Arc::new(RequestRegistry::new(
        boot.runtime_handle().clone(),
        Some(boot.event_bus().clone()),
        None,
    ));
    boot.register_service::<RequestRegistryHandle>(registry);
}
