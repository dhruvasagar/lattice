//! TC.3a — WASM sticky context: producer → per-buffer scope cache → resolver.
//!
//! The sibling of [`wasm_decorations`](crate::wasm_decorations), and it exists
//! for the same reason: a plugin's producer runs OFF the render path (paramount
//! goal #1) and the rest of the editor reads only a native cache. What differs
//! is the *staleness key*, and the difference is the whole point of the design.
//!
//! Decorations are per-line marks keyed on the **document** version. Scopes are
//! a pure function of the **parse tree**, so they are keyed on the syntax
//! snapshot's version instead. A cursor move, a scroll, or an edit whose reparse
//! has not landed yet all leave the cached scopes valid — which is what keeps
//! the producer off the keystroke path entirely. Per-pane resolution
//! ([`resolve_context`](lattice_cells::context::resolve_context)) then runs
//! natively at cursor rate against this cache.
//!
//! This module owns:
//!
//! - [`ContextScopeCache`] — the per-buffer cache value (the scopes + the parse
//!   version they were produced against).
//! - [`WasmContextState`] — the bundle the [`Editor`] holds as one field.
//! - [`Editor::maybe_refresh_wasm_context`] — the per-tick refresh pump.
//!
//! The producer trait + registry live in `lattice-mode`
//! ([`AsyncContextSource`](lattice_mode::AsyncContextSource)) so this crate
//! never depends on `lattice-plugin-host`. The loader (`drain_context`)
//! registers the WASM producer; the host reads it here.
//!
//! Design: `docs/dev/architecture/treesitter-context.md`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_cells::context::ContextScope;
use lattice_core::BufferId;
use lattice_mode::ContextSourceRegistryHandle;

use crate::editor::Editor;
use crate::per_buffer_cache::{PerBufferCache, PerBufferCacheExt};

/// Per-buffer cache of a context plugin's structural scopes.
///
/// The producer task writes this via `insert_for`; the host reads it wait-free
/// when it resolves each pane's context lines.
#[derive(Debug, Clone, Default)]
pub struct ContextScopeCache {
    /// **Parse** version the scopes were produced against — not the document
    /// version. Scopes describe the tree, so an edit whose reparse has not
    /// landed does not invalidate them; the strip shows the last coherent
    /// structure rather than blanking, which is the eventual-consistency the UX
    /// contract permits for content the user did not edit.
    pub parse_version: u64,
    /// Merged scopes from every registered producer for this buffer, sorted by
    /// `scope_start` so the resolver's own sort is near-free.
    pub scopes: Vec<ContextScope>,
}

/// The [`Editor`]'s cohesive WASM-context wiring. Bundled into one field so the
/// boot struct literal grows by a single line. Every field defaults, so
/// `Editor::default()` test fixtures get an inert context seam.
#[derive(Debug)]
pub struct WasmContextState {
    /// Per-buffer scope cache the producer tasks write and the host reads.
    pub cache: PerBufferCache<ContextScopeCache>,
    /// The registered async context producers — a clone of the boot
    /// [`ContextSourceRegistryHandle`] service the loader RCU-registers into.
    /// `None` in `Editor::default()`; the refresh then no-ops.
    pub registry: Option<ContextSourceRegistryHandle>,
    /// Off-keystroke paint gate. A producer task bumps this on every cache
    /// write so a scope arrival with no keystroke in flight still repaints.
    pub generation: Arc<AtomicU64>,
    /// Single-flight guard: the `(buffer, parse_version)` a refetch is already
    /// in flight for, so a burst of ticks doesn't spawn duplicate producers.
    pending: Option<(BufferId, u64)>,
    /// Pointer identity of the last registry snapshot the refresh drove. The
    /// registry `ArcSwap` swaps on every register/unregister, so a changed epoch
    /// means producers were added or removed — forcing an immediate refresh so
    /// a just-loaded plugin's scopes appear without waiting for an edit, and an
    /// unloaded one's clear.
    last_registry_epoch: usize,
    /// TC.8a: the plugin's `context.*` options, resolved once per refresh pump
    /// rather than per pane per publish.
    ///
    /// `resolve_sticky_context_lines` runs at cursor rate, once for every pane,
    /// and `ConfigRegistry` reads take a `Mutex` — so reading six options there
    /// would put six uncontended lock acquisitions on the keystroke path for
    /// values that change only when the user runs `:set` or a plugin loads.
    /// Both of those already wake this pump, so caching here costs nothing in
    /// freshness.
    ///
    /// The viewport fields are NOT cached: they are per-pane, and the resolver
    /// overwrites them from the pane it is resolving for.
    pub options: lattice_cells::context::ContextOptions,
    /// TC.8: `context.line-numbers` — a PAINT option, not a resolution one, so
    /// it rides beside [`Self::options`] rather than inside it
    /// (`resolve_context` has no business knowing about gutters).
    pub line_numbers: bool,
}

impl Default for WasmContextState {
    fn default() -> Self {
        Self {
            cache: Default::default(),
            registry: None,
            generation: Default::default(),
            pending: None,
            last_registry_epoch: 0,
            options: Default::default(),
            // The option's registered default, spelled out because a DERIVED
            // `false` would mean a strip built before the first refresh
            // silently drops its numbers — a difference visible only in the
            // first frames, which is the hardest kind to notice.
            line_numbers: true,
        }
    }
}

impl WasmContextState {
    /// Construct the context state wired to the boot producer registry — the
    /// boot path. `Editor::default()` uses the `Default` impl (no registry).
    pub fn with_registry(registry: ContextSourceRegistryHandle) -> Self {
        Self {
            registry: Some(registry),
            ..Default::default()
        }
    }

    /// The cached scopes for `buffer`, or an empty slice when none have landed.
    /// The read the per-pane resolution does every publish.
    pub fn scopes_for(&self, buffer: BufferId) -> Arc<ContextScopeCache> {
        self.cache.get_for(buffer).unwrap_or_default()
    }
}

impl Editor {
    /// Per-tick context refresh pump — the off-render-path drive.
    ///
    /// Called from `run_tick_pending` beside `maybe_refresh_wasm_decorations`.
    /// Cheap when nothing changed (registry-epoch + parse-version gated). When a
    /// refresh is due it spawns the registered producers on the background
    /// runtime (NOT the actor thread), each writing the merged result into the
    /// per-buffer cache, bumping the paint generation and waking the render
    /// pipeline so the result lands WITHOUT a keypress.
    ///
    /// Graceful / no-blanking (§8): a producer whose call errs contributes
    /// nothing, and the cache is overwritten only when at least one producer
    /// answered — an all-error refresh keeps the prior scopes rather than
    /// clearing them. A failed refresh must not read as the feature breaking.
    pub fn maybe_refresh_wasm_context(&mut self) {
        // Before the producer gate: the options are read even when no producer
        // is registered yet, because a plugin registers its OPTIONS and its
        // producer in the same load and the order between them is not ours to
        // rely on.
        self.refresh_context_options();
        let Some(registry) = self.wasm_context.registry.clone() else {
            return;
        };
        let snapshot_reg = registry.load_full();
        let epoch = Arc::as_ptr(&snapshot_reg) as usize;
        let registry_changed = epoch != self.wasm_context.last_registry_epoch;
        let sources = snapshot_reg.sources();

        if sources.is_empty() {
            // Every producer unloaded: clear the stale cache so unloaded scopes
            // stop painting, then record the epoch so we don't loop. Only when
            // the registry actually changed — a steady-state editor with no
            // context plugin takes the cheap path and never touches the cache.
            if registry_changed {
                self.wasm_context
                    .cache
                    .store(Arc::new(HashMap::<BufferId, Arc<ContextScopeCache>>::new()));
                self.wasm_context.generation.fetch_add(1, Ordering::Relaxed);
                self.wasm_context.last_registry_epoch = epoch;
                self.wasm_context.pending = None;
            }
            return;
        }

        let buffer_id = self.document_buffer_id;
        // The tree and the line count are acquired together so the two agree on
        // version — the tree-sitter seam's §7 rule. A buffer with no parse still
        // drives the producer: "no tree" is a normal state the guest answers
        // with an empty set, and skipping would leave stale scopes painted after
        // a language change.
        let syntax = self
            .document_syntax_for(buffer_id)
            .map(|handle| handle.snapshot());
        let parse_version = syntax.as_ref().map(|s| s.text_version()).unwrap_or(0);
        let line_count = self.document.snapshot().buffer.content_line_count();

        let cache_current = self
            .wasm_context
            .cache
            .get_for(buffer_id)
            .map(|c| c.parse_version == parse_version)
            .unwrap_or(false);
        if !registry_changed && cache_current {
            return;
        }
        if !registry_changed && self.wasm_context.pending == Some((buffer_id, parse_version)) {
            return;
        }

        self.wasm_context.last_registry_epoch = epoch;
        self.wasm_context.pending = Some((buffer_id, parse_version));

        let path = self.buffers.document_path(buffer_id);
        let cache_slot = self.wasm_context.cache.clone();
        let async_landed = self.async_landed.clone();
        let generation = self.wasm_context.generation.clone();
        // Type-erase for the native trait — `lattice-mode` must not name
        // `lattice-syntax` (the `ActionContext::syntax` precedent); the
        // plugin-host adapter downcasts on the far side.
        let erased: Option<Arc<dyn std::any::Any + Send + Sync>> =
            syntax.map(|s| s as Arc<dyn std::any::Any + Send + Sync>);

        // Off the actor thread: the editor actor runs a current-thread runtime,
        // so a plain `tokio::spawn` would land here. The shared background
        // runtime hosts the channel round-trip to the plugin's context actor.
        lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
            let mut merged: Vec<ContextScope> = Vec::new();
            let mut any_ok = false;
            for source in sources {
                match source
                    .produce(buffer_id.0 as u64, path.clone(), line_count, erased.clone())
                    .await
                {
                    Ok(scopes) => {
                        any_ok = true;
                        merged.extend(scopes);
                    }
                    Err(reason) => {
                        tracing::debug!(
                            source = source.source_id(),
                            error = %reason,
                            "context producer errored; keeping prior scopes"
                        );
                    }
                }
            }
            if any_ok {
                // Sort once here rather than on every per-pane resolution: the
                // resolver runs at cursor rate, this runs per reparse.
                merged.sort_by_key(|s| s.scope_start);
                cache_slot.insert_for(
                    buffer_id,
                    ContextScopeCache {
                        parse_version,
                        scopes: merged,
                    },
                );
                generation.fetch_add(1, Ordering::Relaxed);
                // The wake is what makes the strip appear with no keypress. A
                // bare cache write would sit until the user happened to press
                // something, and the symptom reads as a rendering bug.
                async_landed.notify_one();
            }
        });
    }
}

impl Editor {
    /// TC.3b — resolve the source lines this pane pins, for the publish that is
    /// about to happen.
    ///
    /// Runs at cursor rate (every pane-inputs publish), so it must stay cheap:
    /// a cache read plus [`resolve_context`], which is a linear scan over the
    /// buffer's scopes and a sort of the small enclosing subset. It touches no
    /// WASM — the producer that filled the cache ran off-thread on the last
    /// reparse.
    ///
    /// The host resolving this (rather than each renderer) is what makes the
    /// scroll model's reservation and the painted strip incapable of
    /// disagreeing: both read the list this returns.
    ///
    /// Empty is the fast path and the overwhelmingly common one — no context
    /// plugin loaded means no cached scopes means an empty `Arc<[u32]>` with no
    /// allocation beyond the shared empty slice.
    pub fn resolve_sticky_context_lines(
        &self,
        buffer_id: BufferId,
        cursor_line: u32,
        scroll: u32,
        viewport_height: u32,
    ) -> Arc<[u32]> {
        let cached = self.wasm_context.scopes_for(buffer_id);
        if cached.scopes.is_empty() {
            return Arc::from([] as [u32; 0]);
        }
        // The plugin's registered `context.*` options, cached by the refresh
        // pump; only the per-pane viewport fields are filled in here.
        let opts = lattice_cells::context::ContextOptions {
            viewport_height,
            viewport_top: scroll,
            ..self.wasm_context.options
        };
        let lines = lattice_cells::context::resolve_context(&cached.scopes, cursor_line, &opts);
        Arc::from(lines.into_boxed_slice())
    }

    /// Re-read the plugin's `treesitter-context.*` options into the cache.
    ///
    /// Every option is optional at every step: the plugin may not be loaded,
    /// may not have registered that option, or may have registered it with a
    /// type this cannot read. Each of those falls back to the compiled default
    /// INDIVIDUALLY rather than abandoning the whole read — a plugin that
    /// registers five of six options should get five honoured, not none.
    ///
    /// The names are the plugin id plus the option's own name, which is how
    /// the config seam namespaces every plugin option. That coupling is the
    /// cost of the host resolving a plugin's options natively (which is itself
    /// the cost of keeping WASM off the scroll path); it is spelled out here
    /// rather than spread across the reads.
    fn refresh_context_options(&mut self) {
        use lattice_cells::context::TrimScope;
        const NS: &str = "treesitter-context";
        let defaults = lattice_cells::context::ContextOptions::default();
        let int = |name: &str, fallback: u32| -> u32 {
            self.config
                .get_int_by_name(&format!("{NS}.{name}"))
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(fallback)
        };
        let trim = match self
            .config
            .get_string_by_name(&format!("{NS}.trim-scope"))
            .as_deref()
        {
            Some("inner") => TrimScope::Inner,
            Some("outer") => TrimScope::Outer,
            // An unrecognised value keeps the default rather than erroring:
            // the option is a plugin's free-form string, and a typo must not
            // take the strip away.
            _ => defaults.trim,
        };
        self.wasm_context.options = lattice_cells::context::ContextOptions {
            max_lines: int("max-lines", defaults.max_lines),
            trim,
            multiline_threshold: int("multiline-threshold", defaults.multiline_threshold),
            max_viewport_fraction: int("max-viewport-fraction", defaults.max_viewport_fraction),
            // Per-pane; overwritten by the resolver.
            ..defaults
        };
        // Defaults to ON: a strip whose rows have no line numbers is harder to
        // act on than one that does, and the plugin registers it `true`. The
        // fallback matches, so an unloaded plugin and a loaded one agree.
        self.wasm_context.line_numbers = self
            .config
            .get_bool_by_name(&format!("{NS}.line-numbers"))
            .unwrap_or(true);
    }
}
