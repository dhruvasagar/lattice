//! The `llm` modeline element: whether a request is running (LL.14c, design
//! `llm-layer.md` §7).
//!
//! The modeline carries only that, so a request in a hidden chat, or one
//! writing into another buffer, is never invisible: `llm` while one runs,
//! `llm 3` while three do, nothing otherwise. The rest (which request, where
//! its answer goes, its state) is `:llm-requests`, and the chat headerline.
//!
//! **Global scope**: one value, on the active pane, because a request is not a
//! property of the buffer being looked at. Produced here, by the layer that
//! owns requests, and pushed over the bus as [`ModelineElementUpdate`]; the
//! host's modeline wake repaints on each push, so the element appears and
//! goes without a keystroke. A forwarder task folds state changes into the
//! running count and publishes only when the count changes.

use std::sync::Arc;

use lattice_mode::{
    ElementContent, ElementId, ModelineElement, ModelineElementUpdate, ModelineKey, ModelineRole,
    ModelineService, Scope, Zone,
};
use lattice_runtime::EventBus;
use tokio::runtime::Handle;

use super::request::{RequestRegistryHandle, RequestStateChanged};

/// The element's id, owned by the llm layer.
pub const LLM_ELEMENT: &str = "llm";

/// The role modes tag their modeline content with.
const ROLE: &str = "modeline.mode_item";

/// Register the element: right zone, ahead of the LSP badge (priority 5).
pub fn register_element(svc: &ModelineService) {
    svc.register(
        ModelineElement::new(ElementId::new(LLM_ELEMENT), Zone::Right, 4).with_scope(Scope::Global),
    );
}

/// What the element shows with `running` requests in flight. Empty hides it.
pub fn content(running: usize) -> ElementContent {
    match running {
        0 => ElementContent::default(),
        1 => ElementContent::text("llm", ModelineRole::new(ROLE)),
        n => ElementContent::text(format!("llm {n}"), ModelineRole::new(ROLE)),
    }
}

/// Keep the element current: on each request state change, count the
/// running requests and push the content when the count moved.
pub fn spawn_forwarder(requests: RequestRegistryHandle, bus: Arc<EventBus>, runtime: &Handle) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RequestStateChanged>();
    bus.subscribe_typed::<RequestStateChanged>(tx);
    runtime.spawn(async move {
        let mut shown = 0usize;
        while rx.recv().await.is_some() {
            while rx.try_recv().is_ok() {}
            let running = requests
                .snapshot()
                .iter()
                .filter(|r| !r.state.is_terminal())
                .count();
            if running == shown {
                continue;
            }
            shown = running;
            bus.publish_typed(ModelineElementUpdate {
                key: ModelineKey::Global,
                id: ElementId::new(LLM_ELEMENT),
                content: content(running),
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_shows_whether_requests_run_and_how_many() {
        assert!(content(0).is_empty());
        assert_eq!(content(1).plain(), "llm");
        assert_eq!(content(3).plain(), "llm 3");
    }
}
