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

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_core::BufferId;
use lattice_mode::{MediaBlockRequest, MediaSourceRegistryHandle};

use crate::editor::Editor;
use crate::per_buffer_cache::{PerBufferCache, PerBufferCacheExt};

/// `(line_height_px, pane_width_px)` — what sizing a block needs, and the
/// only pixel geometry the host holds. `None` means no peer that draws
/// images has published its cell metrics.
pub type MediaGeometry = (f32, f32);

/// What a refresh is single-flighted on: which buffer, at which document
/// version, measured against which geometry. The geometry is part of the key
/// because a resize changes neither of the other two.
type RefreshKey = (BufferId, u64, Option<MediaGeometry>);

/// Per-buffer cache of a media plugin's blocks, resolved and sized.
#[derive(Debug, Clone, Default)]
pub struct WasmMediaCache {
    /// Document version the blocks were produced against — the staleness key.
    pub document_version: u64,
    /// IM.7a: the geometry the blocks were SIZED against, so a window resize
    /// re-measures. Without this a block keeps the row count it earned at the
    /// old pane width: widen the window and a `Contain` image is drawn larger
    /// inside a box still reserved for the smaller one.
    pub geometry: Option<MediaGeometry>,
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
    /// Single-flight guard for a refresh already in flight.
    ///
    /// Keyed on the GEOMETRY as well as the buffer and version: a resize
    /// changes neither of the other two, so a version-only key made the
    /// re-measure unreachable — the staleness check let it through and this
    /// guard turned it straight back, and an image kept the row count it
    /// earned at the old pane width for the rest of the session.
    pending: Option<RefreshKey>,
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

/// Rows reserved for a block whose file could not be measured.
///
/// One, not [`PROVISIONAL_ROWS`]: a header read that failed is not a pending
/// answer, it IS the answer — the file is missing, unreadable or not an image
/// this build decodes, and no later frame will improve on it. The alt text
/// stands in, and it needs one row. Eight blank rows around it would reserve
/// most of a screen for a picture that is never coming.
pub const UNREADABLE_ROWS: u16 = 1;

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

        let geometry = self.media_geometry();
        let cache_current = self
            .wasm_media
            .cache
            .get_for(buffer_id)
            .map(|c| c.document_version == version && c.geometry == geometry)
            .unwrap_or(false);
        if !registry_changed && cache_current {
            return;
        }
        if !registry_changed && self.wasm_media.pending == Some((buffer_id, version, geometry)) {
            return;
        }

        self.wasm_media.last_registry_epoch = epoch;
        self.wasm_media.pending = Some((buffer_id, version, geometry));

        // Measurements already taken, keyed by path. The pump refreshes on
        // every document version — that is, on every keystroke in the buffer
        // — so without this an org file with twenty images would open twenty
        // files per keypress. A header read is cheap; doing it per keystroke
        // per image is not, and it is I/O nobody asked for.
        //
        // Carried across a RESIZE too: an intrinsic size does not depend on
        // the pane, so a resize re-runs `block_geometry`, which is
        // arithmetic, and reads nothing.
        let known: std::collections::HashMap<PathBuf, (u32, u32)> = self
            .wasm_media
            .cache
            .get_for(buffer_id)
            .map(|c| {
                c.blocks
                    .iter()
                    .filter_map(|(b, _, _)| Some((b.path()?.to_path_buf(), b.intrinsic?)))
                    .collect()
            })
            .unwrap_or_default();

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
            // IM.7a — measure each block, then size it. `inline-media.md` §7:
            // the HOST resolves the intrinsic size and computes rows +
            // `height_lh`, so sizing policy lives in one place and both peers
            // reserve the same rows.
            //
            // On `spawn_blocking` because a probe is a FILE READ. This task
            // runs on the LSP runtime beside other async work, and a batch of
            // header reads parked on one of its threads is the pattern the
            // provider rules exist to forbid.
            let blocks = tokio::task::spawn_blocking(move || size_blocks(merged, geometry, &known))
                .await
                .unwrap_or_default();
            // Did anything actually change? The pump runs on every document
            // version — that is, on every keystroke — and a buffer's blocks
            // are the same after almost all of them. Writing the cache is
            // cheap and has to happen (the version stamp is what stops the
            // next tick re-running), but the WAKE is not: bumping the
            // generation moves the provider's fingerprint, which rebuilds the
            // virtual rows, and `notify_one` publishes render state and asks
            // for a paint. Doing that per keystroke for an unchanged picture
            // is exactly the per-keystroke work paramount #1 forbids.
            let unchanged = cache_slot
                .get_for(buffer_id)
                .is_some_and(|prior| same_blocks(&prior.blocks, &blocks));
            cache_slot.insert_for(
                buffer_id,
                WasmMediaCache {
                    document_version: version,
                    geometry,
                    blocks,
                },
            );
            if !unchanged {
                generation.fetch_add(1, Ordering::Relaxed);
                async_landed.notify_one();
            }
        });
    }
}

/// Are two sized block lists the same picture in the same place?
///
/// Compared by VALUE, not by `Arc` identity: every refresh builds fresh
/// `MediaBlock`s, so pointer equality would report "changed" every time and
/// defeat the whole check.
fn same_blocks(
    a: &[(Arc<lattice_cells::MediaBlock>, u32, u16)],
    b: &[(Arc<lattice_cells::MediaBlock>, u32, u16)],
) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|((ab, aa, ar), (bb, ba, br))| aa == ba && ar == br && **ab == **bb)
}

/// IM.7a — measure each request and turn it into a sized block.
///
/// Off the actor thread and off the LSP runtime's async threads (the caller
/// puts this on `spawn_blocking`), because every `probe` is a file read.
///
/// `geometry` is `(line_height_px, pane_width_px)` from the drawing peer.
/// `None` — no peer published cell metrics, which is the TUI — means the
/// block keeps its provisional reservation and **no file is read at all**:
/// a renderer that draws alt text has nothing to learn from an image header.
fn size_blocks(
    requests: Vec<MediaBlockRequest>,
    geometry: Option<MediaGeometry>,
    known: &std::collections::HashMap<PathBuf, (u32, u32)>,
) -> Vec<(Arc<lattice_cells::MediaBlock>, u32, u16)> {
    requests
        .into_iter()
        .map(|req| {
            let mut block = lattice_cells::MediaBlock::new(req.path.clone(), req.alt);
            block.fit = req.fit;
            let rows = match geometry {
                None => PROVISIONAL_ROWS,
                Some((line_height_px, pane_width_px)) => match known
                    .get(&req.path)
                    .copied()
                    .map(Ok)
                    .unwrap_or_else(|| lattice_media::probe(&req.path))
                {
                    Ok(intrinsic) => {
                        let (rows, height_lh) = lattice_media::block_geometry(
                            intrinsic,
                            req.fit,
                            line_height_px,
                            pane_width_px,
                        );
                        block.intrinsic = Some(intrinsic);
                        block.height_lh = Some(height_lh);
                        rows
                    }
                    Err(err) => {
                        // `debug!`, not `warn!`: a buffer full of links to
                        // images that are not there would otherwise log on
                        // every refresh forever. The alt text is the visible
                        // report, and it names the file.
                        tracing::debug!(
                            path = %req.path.display(),
                            error = %err,
                            "inline media could not be measured; alt text stands in"
                        );
                        UNREADABLE_ROWS
                    }
                },
            };
            (Arc::new(block), req.anchor_line, rows)
        })
        .collect()
}

impl Editor {
    /// IM.7a — `(line_height_px, pane_width_px)` for the active pane, if a
    /// peer that draws images has published its cell metrics.
    ///
    /// The pane's width comes from the column count it already publishes,
    /// multiplied by the column advance — which is why the metric channel is
    /// two scalars rather than a per-pane pixel rectangle.
    fn media_geometry(&self) -> Option<MediaGeometry> {
        let m = self.cell_metrics?;
        let cols = match self.pane_tree.active().viewport_width {
            0 => u32::from(self.terminal_width?),
            w => w,
        };
        let pane_width_px = cols as f32 * m.col_px;
        (pane_width_px > 0.0).then_some((m.row_px, pane_width_px))
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
                geometry: None,
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
