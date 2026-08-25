//! IM.6b — the registry of inline-media producers.
//!
//! The media twin of [`decoration_source`](crate::decoration_source), and the
//! same contract: an async, off-render-path producer the host drives on a
//! trigger, whose result it caches. The renderer NEVER calls this.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use arc_swap::ArcSwap;

/// One inline media block a producer wants drawn.
///
/// The native mirror of the WIT `media-block` (`wit/media.wit`). Note what is
/// absent: any size. The producer names a file and a line; the HOST resolves
/// the intrinsic dimensions and decides how many rows it reserves, so sizing
/// policy lives in one place and a plugin cannot claim arbitrary vertical
/// space in a buffer it does not own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaBlockRequest {
    /// 0-based source line the block hangs below.
    pub anchor_line: u32,
    /// Path to the image, already resolved against the buffer's directory.
    pub path: PathBuf,
    /// What a renderer that cannot draw shows instead. `None` falls back to
    /// the file name — never nothing.
    pub alt: Option<String>,
    pub fit: lattice_cells::MediaFit,
}

/// The boxed future an [`AsyncMediaSource::produce`] returns.
///
/// `Ok(blocks)` replaces the buffer's cached blocks; `Err(reason)` means
/// **keep the prior cached set**, never "clear". A transient failure mid-edit
/// must not make every image in the document blink out.
pub type MediaFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<MediaBlockRequest>, String>> + Send + 'a>>;

/// An async, off-render-path producer of a buffer's inline media blocks.
pub trait AsyncMediaSource: Send + Sync + std::fmt::Debug {
    /// Stable id of the producing plugin — the teardown key. Two producers
    /// with the same id are the same plugin, so a reload replaces rather than
    /// duplicates.
    fn source_id(&self) -> u64;

    /// Produce this buffer's media blocks off the render path.
    fn produce(&self, buffer_id: u64, path: Option<PathBuf>, line_count: u32) -> MediaFuture<'_>;
}

/// Runtime-mutable registry of [`AsyncMediaSource`]s.
#[derive(Default, Clone)]
pub struct MediaSourceRegistry {
    sources: Vec<Arc<dyn AsyncMediaSource>>,
}

impl std::fmt::Debug for MediaSourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaSourceRegistry")
            .field("sources", &self.sources.len())
            .finish()
    }
}

impl MediaSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a producer. Idempotent per `source_id`: a re-register (reload)
    /// replaces rather than accumulating a duplicate — otherwise every
    /// `:plugin-reload` would double the images in a buffer.
    pub fn register(&mut self, source: Arc<dyn AsyncMediaSource>) {
        let id = source.source_id();
        self.sources.retain(|s| s.source_id() != id);
        self.sources.push(source);
    }

    /// Unregister every producer for `source_id`; returns the count removed.
    /// No-op when absent, per the teardown contract.
    pub fn unregister(&mut self, source_id: u64) -> usize {
        let before = self.sources.len();
        self.sources.retain(|s| s.source_id() != source_id);
        before - self.sources.len()
    }

    /// A wait-free snapshot of the registered producers.
    pub fn sources(&self) -> Vec<Arc<dyn AsyncMediaSource>> {
        self.sources.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }
}

/// Boot-service handle. Register **and** look up with this exact alias (the
/// `ServiceRegistry` TypeId rule).
pub type MediaSourceRegistryHandle = Arc<ArcSwap<MediaSourceRegistry>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Fake(u64);
    impl AsyncMediaSource for Fake {
        fn source_id(&self) -> u64 {
            self.0
        }
        fn produce(&self, _b: u64, _p: Option<PathBuf>, _l: u32) -> MediaFuture<'_> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    /// A reload must REPLACE its producer, not add a second one — otherwise
    /// every `:plugin-reload` doubles the images in the buffer.
    #[test]
    fn re_registering_the_same_source_id_replaces_rather_than_duplicates() {
        let mut r = MediaSourceRegistry::new();
        r.register(Arc::new(Fake(7)));
        r.register(Arc::new(Fake(7)));
        assert_eq!(r.len(), 1);
        r.register(Arc::new(Fake(8)));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn unregister_reports_what_it_removed_and_is_idempotent() {
        let mut r = MediaSourceRegistry::new();
        r.register(Arc::new(Fake(7)));
        assert_eq!(r.unregister(7), 1);
        assert_eq!(r.unregister(7), 0, "idempotent, per the teardown contract");
        assert!(r.is_empty());
    }
}
