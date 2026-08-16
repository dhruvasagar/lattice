//! TC.2 — the native seam for an async producer of structural context scopes.
//!
//! The tree-sitter-context analogue of [`AsyncGutterDecorationSource`]: the host
//! drives it OFF the render path on a trigger (a completed reparse), caches the
//! returned scopes per buffer, and every later read is native. The renderer
//! never touches a producer.
//!
//! The trait lives here rather than in `lattice-plugin-host` for the reason the
//! decoration one does: `lattice-mode` and the renderers must stay free of any
//! wasmtime dependency, so the loader hands a trait object across the seam. The
//! single implementor today is the plugin host's `WasmContextSource`.
//!
//! **Why scopes and not rows.** A [`ContextScope`] is a pure function of the
//! parse tree — no viewport, no cursor, no options — so the host can cache the
//! set per parse version and answer "which apply to THIS pane right now" itself
//! with [`resolve_context`](lattice_cells::context::resolve_context). A producer
//! that returned finished rows would have to be re-driven on every scroll and
//! cursor move, which is a WASM call on the keystroke path (paramount #1) and a
//! cache that thrashes by construction.
//!
//! Design anchor: `docs/dev/architecture/treesitter-context.md`.
//!
//! [`AsyncGutterDecorationSource`]: crate::AsyncGutterDecorationSource

use std::any::Any;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_cells::context::ContextScope;

/// The boxed future an [`AsyncContextSource::produce`] returns.
///
/// `Ok(scopes)` replaces the buffer's cached scope set; `Err(reason)` means
/// **keep the prior cached set** — never "clear". A failed refresh must not
/// blank the sticky strip: a transient error would otherwise read as the
/// feature breaking rather than as one refresh that did not land. Same contract
/// as [`DecorationFuture`](crate::DecorationFuture).
pub type ContextFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<ContextScope>, String>> + Send + 'a>>;

/// An async, off-render-path producer of a buffer's structural context scopes.
///
/// The renderer NEVER calls this, and neither does anything on the keystroke
/// path. The host drives it when a reparse completes, stamps the result with the
/// parse version, and publishes it; resolution against a pane's anchor and
/// viewport is native and happens per publish.
pub trait AsyncContextSource: Send + Sync + std::fmt::Debug {
    /// Stable id of the producing plugin — the teardown key. Two producers with
    /// the same id are the same plugin (a reload replaces rather than
    /// duplicates), mirroring
    /// [`AsyncGutterDecorationSource::source_id`](crate::AsyncGutterDecorationSource::source_id).
    fn source_id(&self) -> u64;

    /// Produce this buffer's context scopes off the render path.
    ///
    /// `path` is the buffer's on-disk path when it has one; `line_count` bounds
    /// the addressable lines.
    ///
    /// `syntax` is the buffer's parse snapshot, **type-erased** — the same
    /// `Option<Arc<dyn Any + Send + Sync>>` shape `ActionContext::syntax` uses
    /// (`lattice-grammar`), and for the same reason: `lattice-mode` must not
    /// depend on `lattice-syntax`, so the implementor downcasts. `None` means
    /// the buffer has no parse (plain text, or one still pending), and a
    /// producer with nothing to work from returns an empty list rather than an
    /// error — "no tree yet" is a normal state, not a failure.
    ///
    /// The caller acquires `syntax` at the same instant as `line_count` so the
    /// tree and the text agree on version (the tree-sitter seam's §7 rule).
    ///
    /// See [`ContextFuture`] for the `Ok`/`Err` contract.
    fn produce(
        &self,
        buffer_id: u64,
        path: Option<PathBuf>,
        line_count: u32,
        syntax: Option<Arc<dyn Any + Send + Sync>>,
    ) -> ContextFuture<'_>;
}

/// Runtime-mutable registry of [`AsyncContextSource`]s.
///
/// The plugin loader RCU-registers a loaded context plugin's producer here
/// (`drain_context`); the host's reparse-driven refresh reads a wait-free
/// snapshot to drive them. Named generically (not `Wasm…`) because it holds
/// native trait objects — the WASM source is one implementor among potential
/// natives, exactly as with [`GutterDecorationSourceRegistry`].
///
/// [`GutterDecorationSourceRegistry`]: crate::GutterDecorationSourceRegistry
#[derive(Default, Clone)]
pub struct ContextSourceRegistry {
    sources: Vec<Arc<dyn AsyncContextSource>>,
}

impl std::fmt::Debug for ContextSourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Trait objects aren't usefully printable beyond their count; keep the
        // Debug impl cheap (this rides `Editor: Debug` through the handle).
        f.debug_struct("ContextSourceRegistry")
            .field("sources", &self.sources.len())
            .finish()
    }
}

impl ContextSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a producer. Idempotent per `source_id`: a re-register (reload)
    /// replaces the prior producer for that id rather than accumulating a
    /// duplicate.
    pub fn register(&mut self, source: Arc<dyn AsyncContextSource>) {
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
    pub fn sources(&self) -> Vec<Arc<dyn AsyncContextSource>> {
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
pub type ContextSourceRegistryHandle = Arc<ArcSwap<ContextSourceRegistry>>;
