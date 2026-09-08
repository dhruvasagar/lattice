//! PL8.E — the async gutter-decoration producer seam.
//!
//! `Mode::gutter_decorations` (see [`crate::contributions`]) is the **sync**,
//! read-per-frame decoration trait native modes (diff, LSP severity) satisfy
//! inline. A WASM plugin can NOT satisfy it that way — running the guest per
//! frame from the renderer would violate paramount goal #1 (no UI-thread WASM).
//!
//! Instead a plugin decoration provider is an [`AsyncGutterDecorationSource`]:
//! the host polls it OFF the render path on a trigger (edit / scroll /
//! producer (de)registration), caches the returned `Vec<GutterDecoration>` per
//! buffer, and the renderer reads only that native cache — never the producer.
//!
//! The trait + registry live here (not in `lattice-plugin-host`) so the
//! substrate-neutral `lattice-mode` and the renderers (which read only the
//! cached `GutterDecoration`s) stay free of any wasmtime dependency. The single
//! implementor today is the plugin host's `WasmDecorationSource`; the
//! indirection mirrors `AsyncCompletionSource` (lattice-completion) and
//! `PickerSourceGenerator` (lattice-picker) — a generic native trait a WASM
//! source implements, so the loader hands a trait object across the seam.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::GutterDecoration;

/// The boxed future an [`AsyncGutterDecorationSource::produce`] returns.
///
/// `Ok(marks)` replaces the buffer's cached decorations; `Err(reason)` means
/// **keep the prior cached snapshot** (no flicker, §8) — never "clear".
pub type DecorationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<GutterDecoration>, String>> + Send + 'a>>;

/// An async, off-render-path producer of a buffer's gutter decorations.
///
/// The renderer NEVER calls this. The host's per-tick decoration pump drives it
/// on a trigger, writes the result into the per-buffer cache, and the renderer
/// merges the cached marks into the same gutter partition it walks for
/// [`Mode::gutter_decorations`](crate::Mode::gutter_decorations).
pub trait AsyncGutterDecorationSource: Send + Sync + std::fmt::Debug {
    /// Stable id of the producing plugin — the teardown key
    /// ([`GutterDecorationSourceRegistry::unregister`]). Two producers with the
    /// same id are the same plugin (reload replaces rather than duplicates).
    fn source_id(&self) -> u64;

    /// Produce this buffer's gutter decorations off the render path. `path` is
    /// the buffer's on-disk path when it has one (a provider may key marks off
    /// it); `line_count` bounds the addressable lines. See [`DecorationFuture`]
    /// for the `Ok`/`Err` contract.
    fn produce(
        &self,
        buffer_id: u64,
        path: Option<PathBuf>,
        line_count: u32,
    ) -> DecorationFuture<'_>;
}

/// Runtime-mutable registry of [`AsyncGutterDecorationSource`]s.
///
/// The plugin loader RCU-registers a loaded decoration plugin's producer here
/// (`drain_decorations`); the host's per-tick refresh reads a wait-free
/// snapshot to drive them. Named generically (not `Wasm…`) because it holds
/// native trait objects — mirrors `PickerRegistry`, whose WASM source is one
/// implementor among potential natives.
#[derive(Default, Clone)]
pub struct GutterDecorationSourceRegistry {
    sources: Vec<Arc<dyn AsyncGutterDecorationSource>>,
}

impl std::fmt::Debug for GutterDecorationSourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Trait objects aren't usefully printable beyond their count; keep the
        // Debug impl cheap (this rides `Editor: Debug` through the handle).
        f.debug_struct("GutterDecorationSourceRegistry")
            .field("sources", &self.sources.len())
            .finish()
    }
}

impl GutterDecorationSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a producer. Idempotent per `source_id`: a re-register (reload)
    /// replaces the prior producer for that id rather than accumulating a
    /// duplicate.
    pub fn register(&mut self, source: Arc<dyn AsyncGutterDecorationSource>) {
        let id = source.source_id();
        self.sources.retain(|s| s.source_id() != id);
        self.sources.push(source);
    }

    /// Unregister every producer for `source_id`; returns the count removed
    /// (the teardown-report increment). No-op when absent — idempotent, per the
    /// teardown contract.
    pub fn unregister(&mut self, source_id: u64) -> usize {
        let before = self.sources.len();
        self.sources.retain(|s| s.source_id() != source_id);
        before - self.sources.len()
    }

    /// A wait-free snapshot of the registered producers (cheap `Arc` clones) —
    /// what the host's refresh iterates.
    pub fn sources(&self) -> Vec<Arc<dyn AsyncGutterDecorationSource>> {
        self.sources.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }
}

/// Boot-service handle: `Arc<ArcSwap<…>>` so the loader RCU-registers producers
/// at runtime while the host reads wait-free. Register **and** look up with this
/// exact alias (the `ServiceRegistry` TypeId rule).
pub type GutterDecorationSourceRegistryHandle = Arc<ArcSwap<GutterDecorationSourceRegistry>>;

/// OA.30 — the counter a guest bumps to say "my decorations changed, though the
/// document did not".
///
/// ## The hole this fills
///
/// `maybe_refresh_wasm_decorations` re-runs producers on exactly two triggers:
/// the producer registry changed, or this buffer's cached `document_version`
/// moved. Both are about things the HOST can see. A producer whose output
/// depends on its own view-local state — the agenda's bulk marks are the case
/// that found this — changes neither, so its first answer is cached forever and
/// every later toggle paints nothing.
///
/// That is the same shape `refresh-view` (OA.15a) closed one level up: a guest
/// could compute a new answer and had no way to say so. This is the gutter's
/// version of it.
///
/// ## Why a bare counter, and not a per-buffer map
///
/// The refresh pump only ever refetches the ACTIVE buffer, so a bump means "the
/// next tick should refetch" and nothing finer would be read. A per-buffer key
/// would be a more precise answer to a question nobody asks, and the cost of
/// being coarse is one extra producer call — off the actor thread, on a tick,
/// for a buffer that is on screen anyway.
#[derive(Debug, Default)]
pub struct DecorationEpoch(std::sync::atomic::AtomicU64);

impl DecorationEpoch {
    /// Say that a producer's answer has changed. Cheap enough to call per
    /// keystroke: one relaxed increment.
    pub fn bump(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// The current value, for the refresh gate to compare against its last.
    pub fn get(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Register **and** look up with this exact alias (the `ServiceRegistry` TypeId
/// rule).
pub type DecorationEpochHandle = Arc<DecorationEpoch>;
