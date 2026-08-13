//! PL8.E — WASM gutter decorations: producer → per-buffer cache → renderer.
//!
//! The one hot-path-sensitive plugin seam. A decoration plugin's producer runs
//! OFF the render path (paramount goal #1); the renderer reads only a native
//! per-buffer cache. This module owns:
//!
//! - [`WasmGutterDecorationCache`] — the per-buffer cache value (the merged
//!   marks + the document version they were produced against).
//! - [`WasmDecorationState`] — the cohesive bundle of decoration wiring the
//!   [`Editor`](crate::dispatch::Editor) holds (one field, so the boot struct
//!   literal grows by one line): the cache, the producer registry handle, the
//!   off-keystroke paint generation, and the single-flight / registry-epoch
//!   bookkeeping.
//! - [`Editor::maybe_refresh_wasm_decorations`] — the per-tick refresh pump,
//!   modelled on `maybe_request_inlay_hint`: version/registry-gated,
//!   single-flight, spawns producers off the actor thread, writes the cache via
//!   `insert_for`, bumps the paint generation, and wakes the render pipeline.
//!
//! The producer trait + registry themselves live in `lattice-mode`
//! ([`AsyncGutterDecorationSource`](lattice_mode::AsyncGutterDecorationSource))
//! so this crate — and the renderers reading the cache — never depend on
//! `lattice-plugin-host`. The loader (`drain_decorations`) registers the WASM
//! producer into the registry service; the host reads it here.
//!
//! Slice plan: `docs/dev/operations/slice-plans/plugin-loader.md` (PL8.E).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_core::BufferId;
use lattice_mode::{GutterDecoration, GutterDecorationSourceRegistryHandle};

use crate::editor::Editor;
use crate::per_buffer_cache::{PerBufferCache, PerBufferCacheExt};

/// Per-buffer cache of a decoration plugin's gutter marks.
///
/// The producer task writes this via `insert_for`; the renderer reads it
/// wait-free via `rs.wasm_gutter_decorations.get_for(buffer_id)` and merges
/// `decorations` into the same partition it walks for `Mode::gutter_decorations`.
#[derive(Debug, Clone, Default)]
pub struct WasmGutterDecorationCache {
    /// Document version the marks were produced against — the staleness key
    /// (`maybe_refresh_wasm_decorations` refetches when it moves).
    pub document_version: u64,
    /// Merged decorations from every registered producer for this buffer.
    pub decorations: Vec<GutterDecoration>,
}

/// The [`Editor`]'s cohesive WASM-decoration wiring (PL8.E). Bundled into one
/// field so the boot struct literal grows by a single line and the state stays
/// together. Every field defaults (empty cache / no registry / generation 0),
/// so `Editor::default()` test fixtures get an inert decoration seam.
#[derive(Debug, Default)]
pub struct WasmDecorationState {
    /// Per-buffer cache the producer tasks write and the renderer reads. Cloned
    /// into the published `RenderState` so off-render-path writes are observed
    /// without republishing the snapshot.
    pub cache: PerBufferCache<WasmGutterDecorationCache>,
    /// The registered async decoration producers — a clone of the boot
    /// [`GutterDecorationSourceRegistryHandle`] service the loader RCU-registers
    /// into. `None` in `Editor::default()`; the refresh then no-ops.
    pub registry: Option<GutterDecorationSourceRegistryHandle>,
    /// Off-keystroke paint gate. A producer task bumps this on every cache
    /// write; [`Editor::compute_paint_revision`](crate::dispatch) folds it in so
    /// a decoration arrival with no keystroke in flight repaints the gutter.
    pub generation: Arc<AtomicU64>,
    /// Single-flight guard: the `(buffer, version)` a refetch is already in
    /// flight for, so a burst of ticks doesn't spawn duplicate producers.
    pending: Option<(BufferId, u64)>,
    /// Pointer identity of the last registry snapshot the refresh drove. The
    /// registry `ArcSwap` swaps its `Arc` on every register/unregister, so a
    /// changed epoch means "producers were added/removed" — forcing an
    /// immediate refresh (a `:plugin-load`ed producer paints without waiting for
    /// an edit; an unloaded one's marks clear).
    last_registry_epoch: usize,
}

impl WasmDecorationState {
    /// Construct the decoration state wired to the boot producer registry — the
    /// boot path. Keeps the single-flight / epoch bookkeeping private (they
    /// start zeroed); `Editor::default()` uses the `Default` impl (no registry).
    pub fn with_registry(registry: GutterDecorationSourceRegistryHandle) -> Self {
        Self {
            registry: Some(registry),
            ..Default::default()
        }
    }
}

impl Editor {
    /// PL8.E per-tick decoration refresh pump — the off-render-path drive.
    ///
    /// Called from `run_tick_pending` next to the `maybe_request_*` LSP pumps.
    /// Cheap when nothing changed (registry-epoch + cache-version gated). When a
    /// refresh is due it spawns the registered producers on the background
    /// runtime (NOT the actor thread), each writing the merged result into the
    /// per-buffer cache via `insert_for`, bumping the paint generation and
    /// waking the render pipeline. NO per-frame WASM — the renderer reads only
    /// the cache this fills.
    ///
    /// Graceful / no-flicker (§8): a producer whose call errs (trap, quarantine,
    /// or a benign "empty buffer" `Err`) contributes nothing; the cache is
    /// overwritten only when at least one producer answered, so an all-error
    /// refresh keeps the prior marks painted rather than blanking them. (With a
    /// single producer — the common case — this is exactly the
    /// `WasmDecorationSource` doc contract: `Err` ⇒ keep prior.)
    pub fn maybe_refresh_wasm_decorations(&mut self) {
        let Some(registry) = self.wasm_decorations.registry.clone() else {
            return;
        };
        let snapshot_reg = registry.load_full();
        let epoch = Arc::as_ptr(&snapshot_reg) as usize;
        let registry_changed = epoch != self.wasm_decorations.last_registry_epoch;
        let sources = snapshot_reg.sources();

        if sources.is_empty() {
            // Every producer unloaded: clear the stale cache so unloaded marks
            // stop painting, then record the epoch so we don't loop. Only when
            // the registry actually changed (steady-state no-producer editors —
            // the overwhelming majority — take the cheap early return above via
            // the empty snapshot without touching the cache).
            if registry_changed {
                self.wasm_decorations
                    .cache
                    .store(Arc::new(
                        HashMap::<BufferId, Arc<WasmGutterDecorationCache>>::new(),
                    ));
                self.wasm_decorations
                    .generation
                    .fetch_add(1, Ordering::Relaxed);
                self.wasm_decorations.last_registry_epoch = epoch;
                self.wasm_decorations.pending = None;
            }
            return;
        }

        let buffer_id = self.document_buffer_id;
        let snapshot = self.document.snapshot();
        let version = snapshot.version;
        // CV.3: content space — decorations address real lines.
        let line_count = snapshot.buffer.content_line_count();

        // Up to date only when the producer set is unchanged AND this buffer's
        // cache matches the current document version. A changed registry always
        // forces a refetch (new/removed producer).
        let cache_current = self
            .wasm_decorations
            .cache
            .get_for(buffer_id)
            .map(|c| c.document_version == version)
            .unwrap_or(false);
        if !registry_changed && cache_current {
            return;
        }
        // Single-flight: skip re-spawning for a (buffer, version) already in
        // flight — unless the registry changed (the in-flight batch used the
        // stale producer set and must be superseded).
        if !registry_changed && self.wasm_decorations.pending == Some((buffer_id, version)) {
            return;
        }

        self.wasm_decorations.last_registry_epoch = epoch;
        self.wasm_decorations.pending = Some((buffer_id, version));

        let path = self.buffers.document_path(buffer_id);
        let cache_slot = self.wasm_decorations.cache.clone();
        let async_landed = self.async_landed.clone();
        let generation = self.wasm_decorations.generation.clone();

        // Off the actor thread: the editor actor runs a current-thread runtime,
        // so a plain `tokio::spawn` would land here. The shared background
        // runtime hosts this channel round-trip to the plugin's decoration actor
        // (which runs the guest), exactly like the LSP request pumps.
        lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
            let mut merged: Vec<GutterDecoration> = Vec::new();
            let mut any_ok = false;
            for source in sources {
                match source
                    .produce(buffer_id.0 as u64, path.clone(), line_count)
                    .await
                {
                    Ok(decorations) => {
                        any_ok = true;
                        merged.extend(decorations);
                    }
                    Err(reason) => {
                        // Graceful skip: keep this producer's prior contribution
                        // (no clear). Debug, not info — a per-refresh event.
                        tracing::debug!(
                            source = source.source_id(),
                            error = %reason,
                            "decoration producer errored; keeping prior marks"
                        );
                    }
                }
            }
            // No-flicker: only overwrite when a producer answered. An all-error
            // refresh leaves the last-good snapshot in place.
            if any_ok {
                cache_slot.insert_for(
                    buffer_id,
                    WasmGutterDecorationCache {
                        document_version: version,
                        decorations: merged,
                    },
                );
                generation.fetch_add(1, Ordering::Relaxed);
                async_landed.notify_one();
            }
        });
    }
}
