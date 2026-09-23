//! IM.7 — WASM inline media: producer → per-buffer blocks → virtual rows.
//!
//! The media twin of [`wasm_decorations`](crate::wasm_decorations), and the
//! same shape for the same reason: a media plugin's producer runs OFF the
//! render path (paramount goal #1), and the renderer reads only a native cache.
//!
//! What is different is what the cache feeds. Decorations end up as gutter
//! marks; media blocks end up as **virtual rows**, which means they change the
//! document's display-row count and therefore its scroll arithmetic. The
//! reservation is built here, host-side, from a size the host resolves — the
//! guest never says how tall anything is.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_core::BufferId;
use lattice_mode::{MediaBlockRequest, MediaSourceRegistryHandle};

use crate::editor::Editor;
use crate::per_buffer_cache::{PerBufferCache, PerBufferCacheExt};

/// Per-buffer cache of a media plugin's blocks, resolved and sized.
#[derive(Debug, Clone, Default)]
pub struct WasmMediaCache {
    /// Document version the blocks were produced against — the staleness key.
    pub document_version: u64,
    /// One entry per block: the descriptor plus the rows it reserves.
    pub blocks: Vec<(Arc<lattice_cells::MediaBlock>, u32, u16)>,
}

/// The [`Editor`]'s cohesive WASM-media wiring. Defaults to inert, so
/// `Editor::default()` test fixtures get no media seam at all.
#[derive(Debug, Default)]
pub struct WasmMediaState {
    pub cache: PerBufferCache<WasmMediaCache>,
    pub registry: Option<MediaSourceRegistryHandle>,
    /// Off-keystroke paint gate, bumped on every cache write.
    pub generation: Arc<AtomicU64>,
    /// Single-flight guard for a `(buffer, version)` already in flight.
    pending: Option<(BufferId, u64)>,
    /// Buffers this state has registered a [`MediaVirtualRowProvider`] for, so
    /// registration happens once per buffer and can be undone when the last
    /// producer goes away.
    registered: std::collections::HashSet<BufferId>,
    /// Pointer identity of the last registry snapshot driven — a change means
    /// producers were added or removed, forcing an immediate refresh.
    last_registry_epoch: usize,
}

impl WasmMediaState {
    pub fn with_registry(registry: MediaSourceRegistryHandle) -> Self {
        Self {
            registry: Some(registry),
            ..Default::default()
        }
    }
}

/// How tall a block is, in display rows, before its file has been measured.
///
/// A provisional reservation, replaced once the header read lands. It is not
/// zero and not one: zero would make the block invisible while still holding a
/// matrix slot, and one would make every image visibly jump from a single line
/// to its real height as the reads complete — the reflow the whole design is
/// arranged to avoid. Eight rows is roughly a small figure, so the common case
/// settles with little or no movement.
pub const PROVISIONAL_ROWS: u16 = 8;

impl Editor {
    /// IM.7 per-tick media refresh pump.
    ///
    /// Version- and registry-gated, single-flight, spawns producers off the
    /// actor thread, and writes the resolved blocks into the per-buffer cache.
    /// No per-frame WASM: the renderer reads only what this fills.
    ///
    /// Graceful: a producer that errs contributes nothing and the cache is
    /// overwritten only when at least one producer answered, so an all-error
    /// refresh keeps the prior blocks. That is what stops every image in a
    /// document blinking out on a transient failure mid-edit.
    pub fn maybe_refresh_wasm_media(&mut self) {
        let Some(registry) = self.wasm_media.registry.clone() else {
            return;
        };
        let snapshot_reg = registry.load_full();
        let epoch = Arc::as_ptr(&snapshot_reg) as usize;
        let registry_changed = epoch != self.wasm_media.last_registry_epoch;
        let sources = snapshot_reg.sources();

        if sources.is_empty() {
            if registry_changed {
                self.wasm_media
                    .cache
                    .store(Arc::new(std::collections::HashMap::<
                        BufferId,
                        Arc<WasmMediaCache>,
                    >::new()));
                self.wasm_media.generation.fetch_add(1, Ordering::Relaxed);
                self.wasm_media.last_registry_epoch = epoch;
                self.wasm_media.pending = None;
                // The cache is empty, so the providers would now draw nothing.
                // Unregister rather than leaving them: a provider that answers
                // `collect() -> []` still costs the worker a wake and a call,
                // and a `:plugin-unload` should leave no trace.
                for buffer in self.wasm_media.registered.drain().collect::<Vec<_>>() {
                    self.virtual_row_providers
                        .unregister(buffer, media_virtual_row_provider_id(buffer));
                }
            }
            return;
        }

        let buffer_id = self.document_buffer_id;
        let snapshot = self.document.snapshot();
        let version = snapshot.version;
        let line_count = snapshot.buffer.content_line_count();

        let cache_current = self
            .wasm_media
            .cache
            .get_for(buffer_id)
            .map(|c| c.document_version == version)
            .unwrap_or(false);
        if !registry_changed && cache_current {
            return;
        }
        if !registry_changed && self.wasm_media.pending == Some((buffer_id, version)) {
            return;
        }

        self.wasm_media.last_registry_epoch = epoch;
        self.wasm_media.pending = Some((buffer_id, version));

        self.ensure_media_virtual_rows(buffer_id);

        let path = self.buffers.document_path(buffer_id);
        // One copy of the buffer per refresh. A media scan reads every line, so
        // a per-line handle would cost one boundary crossing per line; this
        // runs on open / edit, not per frame, so the copy is the cheaper side.
        let text = snapshot.text().to_string();
        let cache_slot = self.wasm_media.cache.clone();
        let async_landed = self.async_landed.clone();
        let generation = self.wasm_media.generation.clone();

        lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
            let mut merged: Vec<MediaBlockRequest> = Vec::new();
            let mut any_ok = false;
            for source in sources {
                match source
                    .produce(buffer_id.0 as u64, path.clone(), line_count, text.clone())
                    .await
                {
                    Ok(blocks) => {
                        any_ok = true;
                        merged.extend(blocks);
                    }
                    Err(reason) => {
                        tracing::debug!(
                            source = source.source_id(),
                            error = %reason,
                            "media producer errored; keeping prior blocks"
                        );
                    }
                }
            }
            if !any_ok {
                return;
            }
            let blocks = merged
                .into_iter()
                .map(|req| {
                    let mut block = lattice_cells::MediaBlock::new(req.path, req.alt);
                    block.fit = req.fit;
                    (Arc::new(block), req.anchor_line, PROVISIONAL_ROWS)
                })
                .collect();
            cache_slot.insert_for(
                buffer_id,
                WasmMediaCache {
                    document_version: version,
                    blocks,
                },
            );
            generation.fetch_add(1, Ordering::Relaxed);
            async_landed.notify_one();
        });
    }
}

/// Namespace prefix for inline-media [`ProviderId`]s, with the buffer's id
/// mixed into the low bits — the same scheme the diff overlay uses, and for the
/// same reason: `:plugin-unload` has to be able to unregister without holding
/// the provider.
const MEDIA_PROVIDER_NAMESPACE: u64 = 0xED1A_0000_0000_0000;

/// The [`lattice_cells::ProviderId`] of `buffer_id`'s media provider.
pub fn media_virtual_row_provider_id(buffer_id: BufferId) -> lattice_cells::ProviderId {
    MEDIA_PROVIDER_NAMESPACE | u64::from(buffer_id.0)
}

impl Editor {
    /// Register `buffer_id`'s [`MediaVirtualRowProvider`], once.
    ///
    /// IM.7 shipped the producer pump and the provider and never connected
    /// them: nothing outside the provider's own tests ever constructed one, so
    /// the cache the pump fills had no reader and no image has ever reached a
    /// frame. This is that wire.
    ///
    /// Per buffer, not global, because the registry is buffer-scoped and the
    /// provider reads one buffer's cache. Called from the pump, which is
    /// already version- and registry-gated, so this runs on the ticks where a
    /// buffer's blocks are (re)produced rather than every tick.
    ///
    /// The width is the pane's, resolved once and then held: it only decides
    /// where the alt-text caption centres, so a stale value after a resize
    /// mis-centres a caption until the next produce — not worth a provider
    /// rebuild on every resize.
    fn ensure_media_virtual_rows(&mut self, buffer_id: BufferId) {
        if self.wasm_media.registered.contains(&buffer_id) {
            return;
        }
        // Prune buffers that have since been closed. Cheap here (this runs
        // once per buffer that gains media) and it keeps a long session from
        // accumulating providers for buffers nobody can look at.
        let closed: Vec<BufferId> = self
            .wasm_media
            .registered
            .iter()
            .copied()
            .filter(|b| !self.buffers.contains(*b))
            .collect();
        for buffer in closed {
            self.virtual_row_providers
                .unregister(buffer, media_virtual_row_provider_id(buffer));
            self.wasm_media.registered.remove(&buffer);
        }

        let pane = self.pane_tree.active();
        let width_cols = match (pane.viewport_width, self.terminal_width) {
            (w, _) if w > 0 => w as usize,
            (_, Some(w)) if w > 0 => w as usize,
            _ => 80,
        };
        let provider: Arc<dyn lattice_cells::VirtualRowProvider> =
            Arc::new(MediaVirtualRowProvider::new(
                media_virtual_row_provider_id(buffer_id),
                buffer_id,
                self.wasm_media.cache.clone(),
                self.wasm_media.generation.clone(),
                width_cols,
            ));
        self.virtual_row_providers.register(buffer_id, provider);
        self.wasm_media.registered.insert(buffer_id);
    }
}

/// IM.7 — the virtual-row provider that turns cached media blocks into rows.
///
/// Reads only the cache the pump above fills; `collect` never blocks and never
/// touches WASM, per the provider contract. `version` is the paint generation,
/// so a landed produce invalidates the worker's fingerprint and the rows are
/// rebuilt without a keystroke.
#[derive(Debug)]
pub struct MediaVirtualRowProvider {
    id: lattice_cells::virtual_rows::ProviderId,
    buffer_id: BufferId,
    cache: PerBufferCache<WasmMediaCache>,
    generation: Arc<AtomicU64>,
    /// Pane width in columns, for centring the alt text.
    width_cols: usize,
}

impl MediaVirtualRowProvider {
    pub fn new(
        id: lattice_cells::virtual_rows::ProviderId,
        buffer_id: BufferId,
        cache: PerBufferCache<WasmMediaCache>,
        generation: Arc<AtomicU64>,
        width_cols: usize,
    ) -> Self {
        Self {
            id,
            buffer_id,
            cache,
            generation,
            width_cols,
        }
    }
}

impl lattice_cells::virtual_rows::VirtualRowProvider for MediaVirtualRowProvider {
    fn id(&self) -> lattice_cells::virtual_rows::ProviderId {
        self.id
    }

    fn version(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    fn collect(&self) -> Vec<lattice_cells::virtual_rows::VirtualRow> {
        let Some(cached) = self.cache.get_for(self.buffer_id) else {
            return Vec::new();
        };
        cached
            .blocks
            .iter()
            .flat_map(|(block, anchor, rows)| {
                lattice_cells::media::media_block_rows(
                    block.clone(),
                    *anchor,
                    *rows,
                    self.width_cols,
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_cells::virtual_rows::VirtualRowProvider;

    fn provider(
        blocks: Vec<(Arc<lattice_cells::MediaBlock>, u32, u16)>,
    ) -> MediaVirtualRowProvider {
        let cache: PerBufferCache<WasmMediaCache> = Default::default();
        cache.insert_for(
            BufferId(1),
            WasmMediaCache {
                document_version: 1,
                blocks,
            },
        );
        MediaVirtualRowProvider::new(99, BufferId(1), cache, Arc::new(AtomicU64::new(7)), 40)
    }

    /// One block of N rows becomes N virtual rows anchored to its line, each
    /// carrying the shared descriptor.
    #[test]
    fn a_cached_block_becomes_its_reserved_rows() {
        let block = Arc::new(lattice_cells::MediaBlock::new("/x.png", None));
        let p = provider(vec![(block.clone(), 4, 5)]);
        let rows = p.collect();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|r| r.anchor_line == 4
            && r.kind == lattice_cells::VirtualRowKind::MediaBlock
            && r.media.is_some()));
    }

    /// A buffer with nothing cached emits nothing — the overwhelmingly common
    /// case, and it must not allocate or block.
    #[test]
    fn an_uncached_buffer_emits_no_rows() {
        let cache: PerBufferCache<WasmMediaCache> = Default::default();
        let p =
            MediaVirtualRowProvider::new(99, BufferId(2), cache, Arc::new(AtomicU64::new(0)), 40);
        assert!(p.collect().is_empty());
    }

    /// `version` tracks the paint generation, so a produce that lands with no
    /// keystroke in flight still invalidates the worker's fingerprint and the
    /// rows get rebuilt.
    #[test]
    fn version_follows_the_paint_generation() {
        let generation = Arc::new(AtomicU64::new(3));
        let p = MediaVirtualRowProvider::new(
            1,
            BufferId(1),
            Default::default(),
            generation.clone(),
            40,
        );
        assert_eq!(p.version(), 3);
        generation.fetch_add(1, Ordering::Relaxed);
        assert_eq!(p.version(), 4, "a landed produce moves the fingerprint");
    }
}
