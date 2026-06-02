//! M.2.b.2 (2026-06-01): `create_multibuffer_view` — the atomic
//! "make me a multibuffer view" entry point that providers (and
//! tests) call.
//!
//! Composes the five steps so providers can't forget any:
//!
//! 1. Allocate a `BufferId`.
//! 2. Build the typed `MultibufferDocumentHandle` from `sources`
//!    + `excerpts` (empty inputs are valid — async providers
//!    open empty views and stream content via
//!    [`MultibufferDocumentHandle::append_excerpts`]).
//! 3. Register the typed handle in `MultibufferRegistry` (pulled
//!    from `activator.services()`).
//! 4. Insert the upcast handle into `BufferStore` via H.1's
//!    `insert_document_buffer(id, BufferKind::Multibuffer, ...)`.
//! 5. Activate `multibuffer-mode` on the buffer via
//!    `activator.activate_major_for_kind(id, Multibuffer)` —
//!    H.2's `ModeRegistry::find_major_for_kind` resolves the
//!    mode id from the kind.
//!
//! Failures (missing `BufferStore` / `MultibufferRegistry`
//! service) log + return early. The activation cascade's own
//! failure path publishes through `ModeEvent::ModeActivationFailed`.
//! Return type is `BufferId` (not `Result`) to match
//! `Editor::activate_major_for_buffer_kind`'s existing shape.
//!
//! See `docs/dev/architecture/multibuffer-views.md` §3.7.

use std::collections::HashMap;
use std::sync::Arc;

use lattice_core::{BufferFlags, BufferId, BufferKind};
use lattice_mode::{BufferStoreHandle, ModeActivator};
use lattice_runtime::{Document, EventBus};

use crate::registry::MultibufferRegistryHandle;
use crate::{Excerpt, MultibufferDocumentHandle, MultibufferHeaderProvider};

/// Atomic insert + activate-major for a multibuffer view. Returns
/// the freshly-allocated view `BufferId`. After this call:
///
/// - The view buffer exists in `BufferRegistry` (as a
///   `BufferData::Multibuffer` entry).
/// - `MultibufferRegistry::handle(buffer_id)` returns the typed
///   handle.
/// - `multibuffer-mode` is the active major on the buffer.
///
/// The caller (typically a provider's public trigger function,
/// e.g. `project_search`) is responsible for activating its own
/// provider-minor mode after this returns (see §3.7 worked
/// example).
///
/// **Empty `sources` + empty `excerpts` are valid.** Async
/// providers (project-search, lsp-references, etc.) call this
/// with empty inputs to open the view immediately, then stream
/// content in via [`MultibufferDocumentHandle::append_excerpts`]
/// as their scan progresses.
///
/// **Missing services log + return a fresh `BufferId`** — the
/// caller can detect by checking
/// `activator.services().get::<MultibufferRegistryHandle>()
/// .and_then(|r| r.handle(id))` returning `Some`. Production
/// boot wires both services unconditionally; failure here means
/// boot order is broken.
pub fn create_multibuffer_view(
    activator: &mut dyn ModeActivator,
    sources: HashMap<BufferId, Arc<dyn Document>>,
    excerpts: Vec<Excerpt>,
    name: Option<String>,
    flags: BufferFlags,
    registry: Arc<lattice_grammar::CommandRegistry>,
) -> BufferId {
    let services = activator.services();

    let buffer_store = match services.get::<BufferStoreHandle>() {
        Some(h) => h,
        None => {
            tracing::warn!(
                "create_multibuffer_view: BufferStoreHandle service not registered; \
                 returning sentinel BufferId — boot order is broken"
            );
            return BufferId::next();
        }
    };

    let mb_registry = match services.get::<MultibufferRegistryHandle>() {
        Some(h) => h,
        None => {
            tracing::warn!(
                "create_multibuffer_view: MultibufferRegistryHandle service not \
                 registered; returning sentinel BufferId — boot order is broken"
            );
            return BufferId::next();
        }
    };

    let handle = match MultibufferDocumentHandle::new(sources, excerpts, registry) {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(
                error = ?e,
                "create_multibuffer_view: MultibufferDocumentHandle::new failed; \
                 returning sentinel BufferId"
            );
            return BufferId::next();
        }
    };
    let buffer_id = handle.buffer_id();

    // Step 2.5 (M.4 2026-06-01): auto-subscribe to source events
    // so the view recomposes when a source publishes
    // DocumentChanged and prunes + publishes
    // MultibufferSourceClosed when a source closes. No-op if the
    // EventBus service isn't registered (test paths without the
    // bus wired up).
    // EventBus is registered as `Arc<EventBus>` at boot
    // (`editor_boot.rs` -> `s.register(event_bus.clone())` where
    // `event_bus: Arc<EventBus>`); lookup must query the same
    // shape and unwrap one Arc layer. Earlier
    // `services.get::<EventBus>()` returned None silently —
    // tests still pass because they wire the bus directly via
    // `attach_event_subscriptions`, but production paths missed
    // their subscriptions.
    if let Some(events_outer) = services.get::<Arc<EventBus>>() {
        let events: Arc<EventBus> = (*events_outer).clone();
        handle.attach_event_subscriptions(&events);
    }

    let typed_handle = Arc::new(handle);

    // Step 3: typed-handle registry insert (providers reach
    // through this via service lookup).
    mb_registry.insert(buffer_id, typed_handle.clone());

    // Step 4: buffer-registry insert via H.1's primitive method.
    // Upcast to `Arc<dyn Document>`.
    let dyn_handle: Arc<dyn Document> = typed_handle.clone();
    buffer_store.insert_document_buffer(buffer_id, BufferKind::Multibuffer, dyn_handle, flags, name);

    // K.4.6 (2026-06-02): register the excerpt-header provider so
    // the virtual-rows worker emits one VirtualRow per excerpt
    // (anchored Above the excerpt's first composed row, content
    // = excerpt header label). MultibufferHeaderProvider holds a
    // cheap clone of the typed handle and reads excerpts on each
    // collect() call; the worker picks it up on its next wake.
    // Default ModeActivator impl returns false (no-op test
    // activators); Editor's override forwards to
    // virtual_row_providers.register().
    let header_provider =
        Arc::new(MultibufferHeaderProvider::new((*typed_handle).clone()));
    let registered = activator.register_virtual_row_provider(buffer_id, header_provider);
    if !registered {
        tracing::debug!(
            buffer = ?buffer_id,
            "create_multibuffer_view: header provider not registered \
             (test activator with default impl, or duplicate ProviderId)"
        );
    }

    // Step 5: activate the major via H.2's kind dispatch. The
    // activator's impl runs the full cascade (default minor +
    // auto minors + options recompute + completion sources +
    // maybe-auto-LSP) and queues any renderer signals into the
    // implementor's pending-signals queue.
    activator.activate_major_for_kind(buffer_id, BufferKind::Multibuffer);

    buffer_id
}
