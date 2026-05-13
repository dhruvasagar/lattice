//! `App::resolved_option`, `OptionCache`, and per-option
//! lookup helpers -- the App-side surface above
//! `lattice-config`.
//!
//! Methods that live here:
//! - `recompute_options_for_buffer`,
//!   `resolved_option<D: OptionDecl>` (the read API the
//!   render hot path uses).
//! - `rebuild_option_cache` (refreshes the renderer-visible
//!   projection from the canonical config).
//! - `do_set` (`:set foo=bar` body), `drain_option_changes`
//!   + `apply_option_cascade` (the per-option cascade
//!   driven from the OptionChanged event bus).
//! - `do_describe_option` (`:describe-option <name>`),
//!   `do_list_options` (`:options` listing).
//!
//! What does NOT live here: the option *registry* (typed
//! definitions registered via `linkme` distributed slice
//! in `lattice-config`), or the `OptionCache` struct (lives
//! in `app.rs` next to the App field).

use lattice_core::FoldMethod;
use lattice_protocol::Event;

use super::{App, EchoLevel, OptionCache};
use crate::help::HelpContent;

impl App {
    // ---- Typed-options accessors (DESIGN.md §5.12) ----
    //
    // The current value of each option lives in `self.config`
    // behind an `ArcSwap` (single source of truth). These
    // accessors read from `self.option_cache` -- a derived
    // projection refreshed via the §5.10 cascade hook on every
    // `Event::OptionChanged` -- so the renderer's per-line option
    // checks stay at field-access speed (~1ns) instead of the
    // ~33ns mutex+ArcSwap+downcast dance per call.

    /// `:set number`. Default `true`. Reads the active buffer's
    /// resolved value via the hot-path cache.
    pub fn show_line_numbers(&self) -> bool {
        self.option_cache.show_line_numbers
    }

    /// `:set number` for an arbitrary buffer (per-pane resolution).
    /// Used by inactive-pane render paths so each pane's mode
    /// stack can drive its own gutter independently of the active
    /// buffer's settings.
    pub fn show_line_numbers_for(&self, buffer: crate::buffers::BufferId) -> bool {
        *self.resolved_option::<lattice_config::Number>(buffer)
    }

    /// `:set relativenumber`. Default `false`. When true the
    /// gutter shows distance from the cursor; the cursor's line
    /// shows its absolute number. Implies `number` (vim's
    /// behaviour) -- the private `apply_option_cascade` cascade
    /// hook mirrors that cascade.
    pub fn relative_line_numbers(&self) -> bool {
        self.option_cache.relative_line_numbers
    }

    /// `:set relativenumber` for an arbitrary buffer (per-pane
    /// resolution). Same shape as [`Self::show_line_numbers_for`].
    pub fn relative_line_numbers_for(&self, buffer: crate::buffers::BufferId) -> bool {
        *self.resolved_option::<lattice_config::RelativeNumber>(buffer)
    }

    /// `:set wrap`. Default `false`. (v1 renderer always
    /// horizontal-scrolls; this flag is read by future B.3 polish.)
    pub fn wrap_lines(&self) -> bool {
        self.option_cache.wrap_lines
    }

    /// `:set ignorecase`. Default `false`.
    pub fn ignorecase(&self) -> bool {
        self.option_cache.ignorecase
    }

    /// `:set tabstop=N`. Default `8`. Stored as `i64` in config
    /// (the typed system's integer type) and cast back to `u32`
    /// at cache-rebuild time -- the validate closure on the option
    /// caps the range to `1..=32` so the cast can never lose bits.
    pub fn tabstop(&self) -> u32 {
        self.option_cache.tabstop
    }

    /// `:set scrolloff=N`. Default `0`. Same `i64`→`u32` shape
    /// as [`Self::tabstop`]; range `0..=64`.
    pub fn scrolloff(&self) -> u32 {
        self.option_cache.scrolloff
    }

    /// `:set foldmethod=...`. Default [`FoldMethod::Manual`].
    pub fn foldmethod(&self) -> FoldMethod {
        self.option_cache.foldmethod
    }

    /// `:set foldenable` / `:set nofoldenable` (`zi`). Default `true`.
    pub fn foldenable(&self) -> bool {
        self.option_cache.foldenable
    }

    /// `:set completion.auto_insert_single`. Default `true`.
    pub fn completion_auto_insert_single(&self) -> bool {
        self.option_cache.completion_auto_insert_single
    }

    // ---- Test-only typed setters (kept on the public surface
    //      because integration tests in render.rs reach for them).
    //      Production code uses `do_set` which goes through the
    //      cmdline path. These mirror what `do_set` does sans the
    //      cmdline parse, calling `apply_post_set` so side effects
    //      (foldmethod ⇒ recompute, ui.* ⇒ theme refresh, ...) match
    //      the user-driven path. ----

    /// Set `foldmethod` directly. Drains the cascade afterwards
    /// so the option cache + recompute_folds run synchronously
    /// for the caller -- mirrors what production's `do_set` does
    /// after the cmdline path.
    pub fn set_foldmethod_for_test(&mut self, fm: FoldMethod) {
        self.config
            .set_typed::<lattice_config::FoldMethodOption>(fm)
            .expect("set foldmethod");
        self.drain_option_changes();
    }

    /// Set `foldenable` directly. Drains the cascade so the cache
    /// reflects the new value before the caller observes it.
    pub fn set_foldenable_for_test(&mut self, on: bool) {
        let _ = self.config.set_typed::<lattice_config::FoldEnable>(on);
        self.drain_option_changes();
    }

    /// Set `completion.auto_insert_single` directly. Drains the
    /// cascade so the cache reflects the new value before the
    /// caller observes it.
    pub fn set_completion_auto_insert_single_for_test(&mut self, on: bool) {
        let _ = self
            .config
            .set_typed::<lattice_config::CompletionAutoInsertSingle>(on);
        self.drain_option_changes();
    }

    /// Repopulate [`Self::option_cache`] from the *active buffer's*
    /// resolved values (M.4: renderer through `ResolvedOptions`).
    /// Falls back to the registry's current value when
    /// `resolved_options` doesn't yet have a cache entry for the
    /// active buffer (transient state during boot before the first
    /// `recompute_options_for_buffer`). Called at App-init time, on
    /// every `Event::OptionChanged` cascade, and -- post-M.4 -- on
    /// active-buffer switch so the cache always tracks the active
    /// buffer's resolved settings (mode contributions included).
    /// Cheap: 9 typed reads.
    pub(super) fn rebuild_option_cache(&mut self) {
        use lattice_config::{
            CompletionAutoInsertSingle, CursorLine, FoldEnable, FoldMethodOption, IgnoreCase,
            Number, RelativeNumber, Scrolloff, Tabstop, Whitespace, WhitespaceEol,
            WhitespaceLeading, WhitespaceSpace, WhitespaceTab, WhitespaceTrailing, Wrap,
        };
        let buffer = self.document_buffer_id;
        // M.7.3.a: parse a typed-option String into a single
        // glyph. Empty string ⇒ category is not decorated.
        // First-char semantics keeps v1 simple; future combining
        // sequences land without an option-shape change because
        // the cache layer can grow into something richer (e.g.
        // `SmallString`) without changing the boundary.
        let glyph = |s: &str| -> Option<char> { s.chars().next() };
        self.option_cache = OptionCache {
            show_line_numbers: *self.resolved_option::<Number>(buffer),
            relative_line_numbers: *self.resolved_option::<RelativeNumber>(buffer),
            wrap_lines: *self.resolved_option::<Wrap>(buffer),
            ignorecase: *self.resolved_option::<IgnoreCase>(buffer),
            tabstop: *self.resolved_option::<Tabstop>(buffer) as u32,
            foldenable: *self.resolved_option::<FoldEnable>(buffer),
            foldmethod: *self.resolved_option::<FoldMethodOption>(buffer),
            scrolloff: *self.resolved_option::<Scrolloff>(buffer) as u32,
            completion_auto_insert_single: *self
                .resolved_option::<CompletionAutoInsertSingle>(buffer),
            show_whitespace: *self.resolved_option::<Whitespace>(buffer),
            current_line_highlight: *self.resolved_option::<CursorLine>(buffer),
            whitespace_tab: glyph(&self.resolved_option::<WhitespaceTab>(buffer)),
            whitespace_trailing: glyph(&self.resolved_option::<WhitespaceTrailing>(buffer)),
            whitespace_leading: glyph(&self.resolved_option::<WhitespaceLeading>(buffer)),
            whitespace_space: glyph(&self.resolved_option::<WhitespaceSpace>(buffer)),
            whitespace_eol: glyph(&self.resolved_option::<WhitespaceEol>(buffer)),
        };
    }

    /// Recompute the resolved-options cache for `buffer` by
    /// stitching every layer of the resolution stack
    /// (`mode-architecture.md` §6.1) and writing the result
    /// into [`Self::resolved_options`].
    ///
    /// Layer ordering (highest priority first):
    /// 1. Modal-state override -- M.7+ when modal-state-keyed
    ///    options exist; today the layer is empty.
    /// 2. Buffer-local explicit set
    ///    ([`Self::buffer_local_overrides`] for this buffer).
    /// 3. Active minor modes' contributions, in activation order.
    /// 4. Active major mode's contributions.
    /// 5. Global registry value (the canonical
    ///    [`Self::config`] current value -- bootstrap layer).
    /// 6. Built-in default (implicitly the registry's initial
    ///    value before any `:set`).
    ///
    /// Eager whole-cache recompute (§6.3.1). For a buffer with
    /// 10 active minor modes and ~30 options, the call is
    /// ~3µs end-to-end. Within the §6.3.2 perf gate.
    ///
    /// Called whenever any resolution layer for `buffer`
    /// changes: mode toggle (via `activate_*` /
    /// `deactivate_*`), buffer-local set, modal-state
    /// transition for modal-keyed options, or option write
    /// (the cascade in `drain_option_changes` propagates global
    /// `:set` writes to every buffer's cache).
    pub fn recompute_options_for_buffer(&mut self, buffer: crate::buffers::BufferId) {
        let mut resolved = lattice_config::ResolvedOptions::new();
        // Layer 5/6: bootstrap with current registry values.
        self.config
            .bootstrap_resolved_with_current_values(&mut resolved);

        // Active modes (layers 4 + 3): walk in activation order
        // for minors, prepend major. Pulled from
        // `self.active_modes[buffer]`; absent ⇒ empty (no major,
        // no minors). M.3 lands the per-kind majors that
        // populate this map at buffer creation.
        let modes_snapshot = self.active_modes.get(&buffer).cloned().unwrap_or_default();

        let mut mode_contributions: Vec<lattice_config::OptionOverrideSet> =
            Vec::with_capacity(modes_snapshot.minors().len() + 1);
        // Major first (lower priority than minors per §6.1).
        if let Some(major_id) = modes_snapshot.major()
            && let Some(major) = self.mode_registry.get(major_id)
        {
            mode_contributions.push(major.options());
        }
        // Minors in activation order.
        for &minor_id in modes_snapshot.minors() {
            if let Some(minor) = self.mode_registry.get(minor_id) {
                mode_contributions.push(minor.options());
            }
        }

        // Buffer-local overrides (layer 2).
        let buffer_local = self
            .buffer_local_overrides
            .get(&buffer)
            .cloned()
            .unwrap_or_default();

        // Layer order for the resolver: highest priority first.
        // Modal-state layer (1) is empty for now; M.7 wires it.
        let modal_layer = lattice_config::OptionOverrideSet::new();

        // Build the layer iter. The resolver pulls in
        // declaration order (highest first); we put modal
        // first, then buffer-local, then the *reversed* mode
        // contributions so the last-activated minor is highest
        // in the layered walk (per §6.2 last-activated-wins
        // for ties).
        let mut layered: Vec<&lattice_config::OptionOverrideSet> = Vec::new();
        layered.push(&modal_layer);
        layered.push(&buffer_local);
        for set in mode_contributions.iter().rev() {
            layered.push(set);
        }

        let resolver = lattice_config::Resolver::new();
        resolver.resolve_into(layered, &mut resolved);

        self.resolved_options.insert(buffer, resolved);
        // M.4: keep `option_cache` in lockstep with the active
        // buffer's resolved options so the renderer's hot-path
        // accessors (`app.show_line_numbers()` etc.) reflect mode
        // contributions for the buffer the user is looking at.
        if buffer == self.document_buffer_id {
            self.rebuild_option_cache();
        }
    }

    /// CSM.3 (insert-completion.md §12.4): recompute the
    /// `ActiveCompletionSources` buffer-local for `buffer` by
    /// walking `active_modes[buffer]` and calling
    /// `mode.completion_sources()` on each. Aggregator reads
    /// the cached result on the popup-open / refilter path; this
    /// runs at mode-transition rate, not keystroke rate, so the
    /// allocation cost is amortised away from the hot path.
    ///
    /// Empty in practice today -- no mode contributes a source
    /// yet. CSM.4 (`buffer-words-mode`) is the first slice that
    /// lights up the cache; CSM.5 -- CSM.8 add the rest. Until
    /// then, `populate_insert_completion_sync` reads the cache,
    /// finds it empty, and falls through to the v1 hardcoded
    /// calls -- proving the read path works without changing
    /// behaviour.
    pub fn recompute_active_completion_sources_for(&mut self, buffer: crate::buffers::BufferId) {
        let mut merged: Vec<lattice_completion::CompletionSourceContribution> = Vec::new();
        if let Some(modes_snapshot) = self.active_modes.get(&buffer).cloned() {
            if let Some(major_id) = modes_snapshot.major()
                && let Some(major) = self.mode_registry.get(major_id)
            {
                merged.extend(major.completion_sources());
            }
            for &minor_id in modes_snapshot.minors() {
                if let Some(minor) = self.mode_registry.get(minor_id) {
                    merged.extend(minor.completion_sources());
                }
            }
        }
        // Always seed -- empty is meaningful ("this buffer has
        // zero contributed sources"). Absent vs empty would be
        // equivalent to the reader, but the always-seed shape
        // keeps `:describe-buffer` honest: a buffer where the
        // popup *could* open but doesn't have any active sources
        // shows up with a count of 0, rather than just looking
        // like the cache hasn't run yet.
        self.buffer_locals
            .entry(buffer)
            .or_default()
            .insert(lattice_mode::ActiveCompletionSources(merged));
    }

    /// Read a resolved option's value for `buffer`. Returns the
    /// option's bootstrap default if the cache for `buffer`
    /// hasn't been recomputed yet (transient state during boot
    /// before the first `recompute_options_for_buffer`).
    ///
    /// Hot-path read; O(1) `TypeId` lookup on the cached
    /// `ResolvedOptions`. The fallback to the registry's
    /// current value covers the buffer-creation race window
    /// before mode activation has triggered a recompute.
    pub fn resolved_option<D: lattice_config::OptionDecl>(
        &self,
        buffer: crate::buffers::BufferId,
    ) -> std::sync::Arc<D::Value>
    where
        D::Value: Clone + Send + Sync + 'static,
    {
        if let Some(cache) = self.resolved_options.get(&buffer)
            && let Some(v) = cache.get::<D>()
        {
            return v;
        }
        self.config.get_typed::<D>().expect("option not registered")
    }

    pub(super) fn do_set(&mut self, option: &str) {
        let echo = match self.config.parse_and_set_command(option) {
            Ok(echo) => echo,
            Err(err) => {
                self.set_message(EchoLevel::Error, err.to_string());
                return;
            }
        };
        // Drain any cascade events the set just enqueued so the
        // user sees the side effects (recompute folds, theme
        // refresh, ...) before the next frame draws. The runtime's
        // main_loop also drains once per iteration as a backstop
        // for writes that originate outside the keystroke path
        // (plugin tasks, future LSP-driven config writes).
        self.drain_option_changes();
        self.set_message(EchoLevel::Info, echo);
    }

    /// Drain queued [`Event::OptionChanged`] events from the App's
    /// own bus subscription and apply per-option cascades on the
    /// App's main thread.
    ///
    /// Why a channel and not a callback: typed-option writes can
    /// originate from anywhere -- the cmdline, plugin tasks
    /// (Phase 7), the customize buffer view (post-1.0), or future
    /// LSP-driven config writes. The publisher closure on the
    /// registry runs *on the calling thread*, which may not be
    /// the App's. Routing every cascade through this channel
    /// gives us:
    ///
    /// - **No re-entrancy on the registry mutex**: the cascade
    ///   runs after the publish path drops every lock. A cascade
    ///   that itself calls `config.set` (e.g. `relativenumber=true`
    ///   ⇒ `number=true`) just queues another event -- the
    ///   `while let Ok` loop picks it up on the next iteration.
    /// - **No render-thread blocking**: drains happen at known
    ///   points (top of main_loop iteration, end of `do_set`).
    ///   Plugins doing heavy work in their own subscriptions
    ///   never delay a keystroke.
    /// - **One source of truth for the cascade logic**: any
    ///   typed-option write goes through this hook regardless of
    ///   how the write was triggered. Pre-bus the cascade lived
    ///   on the cmdline path only and direct `config.set` calls
    ///   silently skipped it.
    ///
    /// `Manual` foldmethod, no-op cascades, and unmatched options
    /// all return early so the drain is cheap when nothing
    /// substantive needs to happen.
    pub fn drain_option_changes(&mut self) {
        // Take the receiver to dodge the borrow checker (we want
        // to mutate `self` for cascades while reading from the rx).
        // Always restored after the loop; the `Option` is purely a
        // borrow gymnastic, never observed in any other state.
        let mut rx = match self.option_change_rx.take() {
            Some(rx) => rx,
            None => return,
        };
        while let Ok(event) = rx.try_recv() {
            if let Event::OptionChanged { name, .. } = event {
                self.apply_option_cascade(&name);
            }
        }
        self.option_change_rx = Some(rx);
    }

    /// Run the per-option cascade for `canonical_name` (already
    /// resolved by `Event::OptionChanged.name`, which is always
    /// the canonical name regardless of which alias the user
    /// typed).
    fn apply_option_cascade(&mut self, canonical_name: &str) {
        // M.4: a global `:set` write updates the config layer
        // (the lowest-priority resolver layer). Re-resolve the
        // active buffer so its `ResolvedOptions` reflects the new
        // value -- otherwise the cache rebuild below reads stale
        // resolved data and the user-visible option doesn't
        // change. Inactive buffers re-resolve lazily on their
        // next `recompute_options_for_buffer` (mode toggle, etc.).
        let active_id = self.document_buffer_id;
        self.recompute_options_for_buffer(active_id);
        // Refresh the hot-path cache so subsequent reads from
        // `app.show_line_numbers()` etc. see the new value.
        // Cheap (~300ns total for all 9 options); only runs when
        // an option actually changed, never on every frame.
        // Note: `recompute_options_for_buffer` already calls
        // `rebuild_option_cache` when `buffer == active`; this
        // belt-and-braces call covers the bootstrap window.
        self.rebuild_option_cache();
        // M.7.1: declarative mode-mirror cascade. Each
        // registered mode that declares `mirrors_option ==
        // Some(canonical_name)` gets its active state synced to
        // the option's new value. Replaces the hardcoded
        // per-mode `match` branches that used to live here --
        // adding a new display mode no longer requires touching
        // this method.
        self.mirror_option_to_modes(canonical_name);
        match canonical_name {
            "relativenumber" => {
                // Vim cascade: `:set rnu` implies `:set nu` so the
                // gutter renders at all. The reverse (`:set nornu`)
                // does NOT clear `nu` -- preserves user intent.
                if self.relative_line_numbers() {
                    let _ = self.config.set_typed::<lattice_config::Number>(true);
                }
            }
            "foldmethod" => {
                // Recompute folds against the new method. Idempotent
                // and cheap when method is `Manual` (the recompute
                // returns immediately).
                self.recompute_folds();
            }
            n if n.starts_with("ui.") => {
                self.sync_theme_from_config();
                if n == "ui.nerd_fonts" {
                    // The file-tree rope embeds the icon glyphs, so
                    // a palette flip must re-render every existing
                    // tree. Oil rebuilds its rope without icons; its
                    // renderer reads the toggle each frame and needs
                    // no rope-side refresh.
                    let nerd_fonts = self.theme.nerd_fonts;
                    for id in self.buffers.file_tree_ids() {
                        self.set_file_tree_nerd_fonts(id, nerd_fonts);
                    }
                }
            }
            // 4.4.k: any change under `lsp.<server-id>.*` is a
            // server-scoped config edit -- fan out
            // `workspace/didChangeConfiguration` to every actor
            // matching that server-id with the freshly merged
            // `lsp.<server-id>` subtree. `lsp.<host-knob>` keys
            // (e.g. `lsp.log_level`, `lsp.log_capacity`) have
            // only one dot after `lsp` and stop here -- they
            // configure the host, not any server, and shouldn't
            // page every attached language server.
            n => {
                if let Some(server_id) = lsp_server_scope(n) {
                    let server_id = server_id.to_string();
                    self.fan_out_did_change_configuration(&server_id);
                }
            }
        }
    }

    /// M.7.1 (Phase 1.5): drive the declarative
    /// `Mode::mirrors_option` cascade. Walks every registered
    /// mode and, for each that declares it mirrors
    /// `canonical_name`, toggles the mode's active state on the
    /// current buffer to match the option's `bool` value.
    ///
    /// Reads through `ConfigRegistry::get_bool_by_name` (the
    /// typed-option layer) rather than the resolved-options
    /// view -- the user's explicit `:set` gesture is the
    /// authority for the mode's active state, not the layered
    /// resolution. Non-bool options short-circuit at the
    /// `get_bool_by_name` step; the loop is a no-op.
    fn mirror_option_to_modes(&mut self, canonical_name: &str) {
        let Some(on) = self.config.get_bool_by_name(canonical_name) else {
            return;
        };
        // Collect mode ids first so the activate/deactivate
        // calls (which take `&mut self`) don't conflict with the
        // registry borrow inside `iter_meta`.
        let mirror_ids: Vec<lattice_mode::ModeId> = {
            let registry = &self.mode_registry;
            registry
                .iter_meta()
                .filter_map(|(id, _kind)| {
                    let mode = registry.get(id)?;
                    if mode.mirrors_option() == Some(canonical_name) {
                        Some(id)
                    } else {
                        None
                    }
                })
                .collect()
        };
        let buffer_id = self.document_buffer_id;
        for mode_id in mirror_ids {
            let currently_active = self
                .active_modes
                .get(&buffer_id)
                .map(|modes| modes.has_minor(mode_id))
                .unwrap_or(false);
            if on && !currently_active {
                self.activate_mode_by_id(buffer_id, mode_id);
            } else if !on && currently_active {
                self.deactivate_mode_by_id(buffer_id, mode_id);
            }
        }
    }

    /// `:describe-option <name>` (DESIGN.md §5.11). Renders the
    /// option's metadata + current value into a help buffer.
    pub(super) fn do_describe_option(&mut self, name: &str) {
        let Some(spec) = self.config.lookup(name) else {
            self.set_message(EchoLevel::Error, format!("E518: Unknown option: {name}"));
            return;
        };
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# {}", spec.name()));
        if !spec.aliases().is_empty() {
            lines.push(format!("aliases: {}", spec.aliases().join(", ")));
        }
        lines.push(format!("type:    {}", spec.type_label()));
        lines.push(format!("default: {}", spec.default_formatted()));
        lines.push(format!("current: {}", spec.get_formatted()));
        if let Some(values) = spec.enumerate_values() {
            lines.push(format!("values:  {}", values.join(", ")));
        }
        lines.push(String::new());
        lines.push(spec.doc().to_string());
        self.display_buffer(
            HelpContent::from_lines(format!("describe-option {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpDescribe,
        );
    }

    /// `:options` -- live reference of every registered option,
    /// grouped by [`OptionGroup`] (DESIGN.md §5.11). Each option
    /// shows its canonical name + aliases, type label, current
    /// value, default value, and doc string. Self-updating: walks
    /// the [`lattice_config::OPTION_DECLS`] linkme slice, so adding
    /// a new option via `options! { ... }` lights it up here at
    /// the next build with no extra wiring.
    ///
    /// Pairs with `:help options` (conceptual prose: `:set` syntax,
    /// types, layered resolution, TOML) and `:describe-option <name>`
    /// (per-option deep-dive). `customizable = false` options are
    /// hidden from this view -- they're mode-driven engine state
    /// (`read-only`, ...), not user-typed config.
    pub(super) fn do_list_options(&mut self) {
        use lattice_config::{GROUP_DECLS, OPTION_DECLS};
        use std::collections::BTreeMap;

        // Bucket every customizable option by its group. The doc /
        // type / default come from the linkme metadata; the *current*
        // value comes from the registry by name (the metadata can't
        // carry a runtime value -- it's a `&'static`).
        let mut by_group: BTreeMap<&'static str, Vec<&'static lattice_config::OptionDeclMetadata>> =
            BTreeMap::new();
        for meta in OPTION_DECLS.iter() {
            if !meta.customizable {
                continue;
            }
            by_group.entry(meta.group_name).or_default().push(*meta);
        }
        for v in by_group.values_mut() {
            v.sort_by_key(|m| m.name);
        }

        // Group docs (one-liner each) so each section gets a header
        // explaining what the group is for.
        let group_doc: BTreeMap<&'static str, &'static str> =
            GROUP_DECLS.iter().map(|g| (g.name, g.doc)).collect();

        let total: usize = by_group.values().map(|v| v.len()).sum();
        let mut lines: Vec<String> = Vec::new();
        lines.push(format!("# Options ({total} customisable)"));
        lines.push(String::new());
        lines.push(
            "Live reference of every registered option, grouped by group. \
             For per-option detail run `:describe-option <name>`. For \
             concepts (`:set` syntax, layered resolution, TOML, plugin \
             options) read `:help options`."
                .into(),
        );
        lines.push(String::new());

        for (group, options) in &by_group {
            lines.push(format!("## {} ({})", group, options.len()));
            if let Some(doc) = group_doc.get(group) {
                lines.push(String::new());
                lines.push((*doc).to_string());
            }
            lines.push(String::new());
            for meta in options {
                let spec = self.config.lookup(meta.name);
                let aliases = spec
                    .as_ref()
                    .map(|s| s.aliases())
                    .filter(|a| !a.is_empty())
                    .map(|a| format!(" [{}]", a.join(", ")))
                    .unwrap_or_default();
                let type_label = (meta.type_label)();
                let default = (meta.default_formatted)();
                let current = spec
                    .as_ref()
                    .map(|s| s.get_formatted())
                    .unwrap_or_else(|| "?".into());
                let header = if current == default {
                    format!(
                        "- **{}**{} : {} = {}",
                        meta.name, aliases, type_label, current
                    )
                } else {
                    format!(
                        "- **{}**{} : {} = {} (default: {})",
                        meta.name, aliases, type_label, current, default,
                    )
                };
                lines.push(header);
                for doc_line in meta.doc.lines() {
                    let trimmed = doc_line.trim();
                    if !trimmed.is_empty() {
                        lines.push(format!("  {trimmed}"));
                    }
                }
                if let Some(values) = spec.as_ref().and_then(|s| s.enumerate_values()) {
                    lines.push(format!("  values: {}", values.join(", ")));
                }
                lines.push(String::new());
            }
        }

        self.display_buffer(
            HelpContent::from_lines("options", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
            lattice_core::ui::display::BufferDisplayCategory::HelpList,
        );
    }

    /// Drain the structural section at `prefix` (an entry
    /// matching one of the loader's structural prefixes,
    /// e.g. `"completion.per-language.markdown"`). Removes it
    /// from `pending_config_structural_sections`; subsequent
    /// drains return `None`. Used by the per-language /
    /// plugin-host layers to consume their TOML config without
    /// leaving stale entries behind.
    pub(super) fn take_pending_structural_section(
        &mut self,
        full_path: &str,
    ) -> Option<toml::Table> {
        self.pending_config_structural_sections.remove(full_path)
    }

    /// Iterate the dotted paths of every pending structural
    /// section whose path starts with `namespace.` (e.g.
    /// `"completion.per-language"` returns the language ids).
    /// Returned as owned `String`s to keep the borrow short --
    /// callers typically follow up with
    /// `take_pending_structural_section(full)` mutating the map.
    pub(super) fn pending_structural_section_paths(&self, namespace: &str) -> Vec<String> {
        let prefix = format!("{namespace}.");
        self.pending_config_structural_sections
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect()
    }

    /// Drain every `completion.per-language.<lang>` structural
    /// section the loader bucketed and merge each into
    /// `self.per_language_completion`. Per-key TOML wins over
    /// the spec defaults seeded at `App::new`; unset keys leave
    /// the default in place.
    ///
    /// Called by the runtime startup right after
    /// `load_persistent_config` finishes. Idempotent (the bucket
    /// empties as we drain). Per-key parse warnings collapse
    /// into a single echo at `Warn` level the same way the
    /// loader's other diagnostics do.
    pub fn apply_per_language_toml_overrides(&mut self) {
        let paths = self.pending_structural_section_paths("completion.per-language");
        let mut warnings: Vec<String> = Vec::new();
        for path in paths {
            let lang = match path.strip_prefix("completion.per-language.") {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            let Some(table) = self.take_pending_structural_section(&path) else {
                continue;
            };
            let parsed = parse_per_language_overrides_table(&path, &table, &mut warnings);
            self.per_language_completion
                .entry(lang)
                .or_default()
                .merge(parsed);
        }
        if !warnings.is_empty() {
            let count = warnings.len();
            let body = if count == 1 {
                format!("config: {}", warnings[0])
            } else {
                format!(
                    "config: {count} per-language warnings (first: {})",
                    warnings[0]
                )
            };
            self.set_message(EchoLevel::Warn, body);
        }
    }
}

/// 4.4.k: returns `Some(server_id)` when `canonical_name`
/// names a server-scoped config key (`lsp.<server_id>.<...>`),
/// `None` otherwise. Used by [`App::apply_option_cascade`] to
/// decide whether an option change should fan out
/// `workspace/didChangeConfiguration` to a language server.
///
/// Single-dot `lsp.foo` keys are host-side (the `log_level` /
/// `log_capacity` family); the spec is that we never page
/// servers for host-side knob changes.
pub(crate) fn lsp_server_scope(canonical_name: &str) -> Option<&str> {
    let rest = canonical_name.strip_prefix("lsp.")?;
    let dot = rest.find('.')?;
    let server_id = &rest[..dot];
    if server_id.is_empty() {
        None
    } else {
        Some(server_id)
    }
}

/// Parse a `[completion.per-language.<lang>]` TOML sub-table
/// into [`PerLanguageOverrides`]. Unknown keys + wrong-typed
/// values append warnings to `warnings` (caller surfaces them
/// in one echo); recognised keys with valid values populate the
/// struct.
fn parse_per_language_overrides_table(
    section_path: &str,
    table: &toml::Table,
    warnings: &mut Vec<String>,
) -> lattice_completion::PerLanguageOverrides {
    let mut out = lattice_completion::PerLanguageOverrides::default();
    for (key, value) in table {
        match key.as_str() {
            "sources" => match value.as_array() {
                Some(arr) => {
                    let sources: Vec<lattice_completion::SourceId> = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(lattice_completion::canonical_source_id))
                        .collect();
                    out.sources = Some(sources);
                }
                None => {
                    warnings.push(format!("{section_path}.sources: expected array of strings",))
                }
            },
            "auto_trigger" => match value.as_bool() {
                Some(b) => out.auto_trigger = Some(b),
                None => warnings.push(format!("{section_path}.auto_trigger: expected bool",)),
            },
            "auto_insert_single" => match value.as_bool() {
                Some(b) => out.auto_insert_single = Some(b),
                None => warnings.push(format!("{section_path}.auto_insert_single: expected bool",)),
            },
            "suppress_in" => match value.as_array() {
                Some(arr) => {
                    out.suppress_in = Some(
                        arr.iter()
                            .filter_map(|v| v.as_str().map(String::from))
                            .collect(),
                    );
                }
                None => warnings.push(format!(
                    "{section_path}.suppress_in: expected array of strings",
                )),
            },
            other => warnings.push(format!(
                "{section_path}.{other}: unknown per-language key (recognised: \
                 sources, auto_trigger, auto_insert_single, suppress_in)",
            )),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::test_helpers::{
        app_in_command_mode, app_with, submit_ex, subscribe_all_events,
    };
    use crate::app::*;
    use lattice_grammar::ModalState;
    use lattice_protocol::Event;

    // ---- 4.4.k: lsp_server_scope ----

    /// 4.4.k: `lsp.<server>.<key>` returns the server-id;
    /// anything shallower (`lsp.<host-knob>`) is host-side and
    /// returns None. The host-side knobs (e.g. `lsp.log_level`)
    /// must NOT trigger workspace/didChangeConfiguration, since
    /// they configure the host's behaviour, not the server's.
    #[test]
    fn lsp_server_scope_picks_server_id_segment() {
        use super::lsp_server_scope;
        assert_eq!(
            lsp_server_scope("lsp.rust-analyzer.checkOnSave"),
            Some("rust-analyzer")
        );
        assert_eq!(
            lsp_server_scope("lsp.gopls.completeUnimported"),
            Some("gopls")
        );
        // Single-dot under lsp.* -> host knob, NOT a fan-out
        // target.
        assert_eq!(lsp_server_scope("lsp.log_level"), None);
        assert_eq!(lsp_server_scope("lsp.log_capacity"), None);
        // Non-lsp options are unaffected.
        assert_eq!(lsp_server_scope("tabstop"), None);
        assert_eq!(lsp_server_scope("ui.theme"), None);
        // Empty server-id (`lsp..foo`) is rejected -- malformed
        // config should never page a phantom server.
        assert_eq!(lsp_server_scope("lsp..foo"), None);
    }

    // ---- Event::OptionChanged (DESIGN.md §5.10 + §5.12) ----

    #[test]
    fn event_bus_publishes_option_changed_on_set_assign() {
        let mut a = app_with("xx", 10);
        let mut rx = subscribe_all_events(&a);
        a.command_line = "set tabstop=4".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let mut found_opt = None;
        while let Ok(evt) = rx.try_recv() {
            if let Event::OptionChanged { name, old, new } = evt {
                found_opt = Some((name, old, new));
                break;
            }
        }
        let (name, old, new) = found_opt.expect("OptionChanged should fire on :set tabstop=4");
        assert_eq!(name, "tabstop");
        assert_eq!(old.as_deref(), Some("8"));
        assert_eq!(new, "4");
    }

    #[test]
    fn event_bus_publishes_option_changed_on_set_negate() {
        let mut a = app_with("xx", 10);
        let mut rx = subscribe_all_events(&a);
        a.command_line = "set nonumber".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let mut found = false;
        while let Ok(evt) = rx.try_recv() {
            if let Event::OptionChanged { name, new, .. } = evt
                && name == "number"
                && new == "false"
            {
                found = true;
                break;
            }
        }
        assert!(found, ":set nonumber should publish OptionChanged");
    }

    #[test]
    fn drain_option_changes_runs_foldmethod_cascade_for_direct_config_writes() {
        // Architectural test: writes that bypass `:set` -- e.g. a
        // plugin or the future customize buffer view calling
        // `config.set` directly -- still trigger the cascade once
        // `drain_option_changes` runs. Pre-bus the cascade lived
        // on the cmdline path only; this confirms the migration to
        // the bus-subscription model fixes that gap.
        let mut a = app_with("def f():\n    pass\n    pass\n", 10);
        // No :set involved -- direct write to the registry.
        a.config
            .set_typed::<lattice_config::FoldMethodOption>(FoldMethod::Indent)
            .unwrap();
        // Folds should not be populated yet -- the cascade is
        // queued but hasn't been drained.
        a.drain_option_changes();
        assert_eq!(a.foldmethod(), FoldMethod::Indent);
        assert!(
            !a.folds.is_empty(),
            "drain_option_changes should run the foldmethod cascade and recompute folds"
        );
    }

    #[test]
    fn drain_option_changes_runs_relativenumber_to_number_cascade() {
        let mut a = app_with("xx", 10);
        a.config.set_typed::<lattice_config::Number>(false).unwrap();
        a.drain_option_changes();
        assert!(!a.show_line_numbers());
        a.config
            .set_typed::<lattice_config::RelativeNumber>(true)
            .unwrap();
        a.drain_option_changes();
        assert!(a.relative_line_numbers());
        assert!(
            a.show_line_numbers(),
            "relativenumber=true should cascade to number=true via the bus subscription"
        );
    }

    #[test]
    fn rebuild_option_cache_picks_up_whitespace_glyphs() {
        // M.7.3.a: the OptionCache surfaces 5 typed-option
        // glyphs as `Option<char>`. Defaults match emacs
        // whitespace-mode's visible set: tab + trailing +
        // leading on, space + EOL off.
        let a = app_with("xx", 10);
        let cache = &a.option_cache;
        // Whitespace decoration starts off (the option is
        // false by default; the mode isn't auto-active).
        assert!(!cache.show_whitespace);
        // Glyph defaults from `lattice-config::core_options`.
        assert_eq!(cache.whitespace_tab, Some('→'));
        assert_eq!(cache.whitespace_trailing, Some('·'));
        assert_eq!(cache.whitespace_leading, Some('·'));
        // Off-by-default categories.
        assert_eq!(cache.whitespace_space, None);
        assert_eq!(cache.whitespace_eol, None);
    }

    #[test]
    fn rebuild_option_cache_picks_up_current_line_highlight() {
        // M.7.3.a: cursor-line option projects through the
        // cache. Default off.
        let a = app_with("xx", 10);
        assert!(!a.option_cache.current_line_highlight);
    }

    #[test]
    fn whitespace_glyph_set_via_set_propagates_to_cache() {
        // M.7.3.a: a `:set display.whitespace.tab=⇥` write
        // flows through the cascade and updates the cache's
        // `whitespace_tab` to the new glyph.
        let mut a = app_with("xx", 10);
        a.do_set("display.whitespace.tab=⇥");
        a.drain_option_changes();
        assert_eq!(a.option_cache.whitespace_tab, Some('⇥'));
        // Empty string disables the category.
        a.do_set("display.whitespace.tab=");
        a.drain_option_changes();
        assert_eq!(a.option_cache.whitespace_tab, None);
    }

    #[test]
    fn drain_option_changes_runs_ui_theme_sync_for_direct_writes() {
        let mut a = app_with("xx", 10);
        a.config
            .set_typed::<crate::tui_options::UiDimInactive>(false)
            .unwrap();
        a.drain_option_changes();
        assert!(
            !a.theme.dim_inactive_panes,
            "ui.dim_inactive=false should propagate to theme.dim_inactive_panes via the cascade"
        );
    }

    #[test]
    fn drain_option_changes_handles_chained_cascade_writes() {
        let mut a = app_with("xx", 10);
        a.config.set_typed::<lattice_config::Number>(false).unwrap();
        a.drain_option_changes();
        a.config
            .set_typed::<lattice_config::RelativeNumber>(true)
            .unwrap();
        a.drain_option_changes();
        assert!(a.relative_line_numbers());
        assert!(a.show_line_numbers());
        a.drain_option_changes();
        assert!(a.relative_line_numbers());
        assert!(a.show_line_numbers());
    }

    #[test]
    fn event_bus_does_not_publish_option_changed_on_query() {
        let mut a = app_with("xx", 10);
        let mut rx = subscribe_all_events(&a);
        a.command_line = "set number?".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        while let Ok(evt) = rx.try_recv() {
            assert!(
                !matches!(evt, Event::OptionChanged { .. }),
                "query should not publish OptionChanged"
            );
        }
    }

    // ---- :set ----

    #[test]
    fn set_unknown_option_emits_error() {
        let mut a = app_with("hello", 10);
        submit_ex(&mut a, "set frobnicate");
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("frobnicate"));
    }

    #[test]
    fn set_unknown_option_errors() {
        let mut a = app_with("xx", 10);
        a.command_line = "set whatever".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("Unknown option"), "got: {}", msg.text);
    }

    #[test]
    fn set_no_form_clears_boolean() {
        let mut a = app_with("xx", 10);
        a.command_line = "set nonumber".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(!a.show_line_numbers());
    }

    #[test]
    fn set_no_form_rejects_non_boolean() {
        let mut a = app_with("xx", 10);
        a.command_line = "set notabstop".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("not a boolean"), "got: {}", msg.text);
    }

    #[test]
    fn set_int_out_of_range_errors() {
        let mut a = app_with("xx", 10);
        a.command_line = "set tabstop=999".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.last_message.as_ref().expect("error");
        assert!(msg.text.contains("out of range"), "got: {}", msg.text);
    }

    // ---- :describe-option / :options ----

    #[test]
    fn describe_option_renders_help_with_metadata() {
        let mut a = app_with("xx", 10);
        a.command_line = "describe-option tabstop".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("describe-option help");
        let body = h.content.as_string();
        assert!(body.contains("tabstop"));
        assert!(body.contains("integer"));
        assert!(body.contains("default"));
    }

    #[test]
    fn list_options_includes_every_registered_option() {
        let mut a = app_with("xx", 10);
        a.command_line = "options".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("options help");
        let body = h.content.as_string();
        assert!(body.contains("number"));
        assert!(body.contains("tabstop"));
        assert!(body.contains("scrolloff"));
    }

    #[test]
    fn list_options_groups_by_group_and_includes_docs() {
        let mut a = app_with("xx", 10);
        a.command_line = "options".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("options help");
        let body = h.content.as_string();
        // Group section headers (markdown ##) for the built-in
        // groups that own options today.
        assert!(body.contains("## editor"), "missing editor section\n{body}");
        assert!(
            body.contains("## completion"),
            "missing completion section\n{body}"
        );
        assert!(
            body.contains("## display"),
            "missing display section\n{body}"
        );
        // Each option's doc string is included alongside the row.
        // `tabstop`'s doc starts with "Number of spaces a hard tab".
        assert!(
            body.contains("Number of spaces a hard tab"),
            "tabstop doc not rendered\n{body}",
        );
        // Aliases are surfaced in the option header (`tabstop [ts]`,
        // `number [nu]`).
        assert!(body.contains("[ts]"), "tabstop alias missing\n{body}");
        assert!(body.contains("[nu]"), "number alias missing\n{body}");
    }

    #[test]
    fn list_options_hides_non_customizable_options() {
        // `read-only` is `customizable = false` (mode-driven, not
        // user-typed); the live reference should hide it.
        let mut a = app_with("xx", 10);
        a.command_line = "options".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let h = a.popup_help().expect("options help");
        let body = h.content.as_string();
        assert!(
            !body.contains("**read-only**"),
            "read-only should be hidden from :options reference\n{body}",
        );
    }

    // ---- M.2.1: option resolution per buffer ----

    /// Test fixture: a registered minor mode that contributes
    /// option overrides via the `overrides!` macro.
    struct OptionContributingMode {
        id: lattice_mode::ModeId,
    }

    impl OptionContributingMode {
        fn new() -> Self {
            Self {
                id: lattice_mode::ModeId::new("test-option-contrib-mode"),
            }
        }
    }

    impl lattice_mode::Mode for OptionContributingMode {
        fn id(&self) -> lattice_mode::ModeId {
            self.id
        }
        fn kind(&self) -> lattice_mode::ModeKind {
            lattice_mode::ModeKind::Minor
        }
        fn options(&self) -> lattice_config::OptionOverrideSet {
            lattice_config::overrides! {
                lattice_config::Tabstop = 4i64,
                lattice_config::Number = false,
            }
        }
    }

    #[test]
    fn recompute_options_uses_registry_defaults_when_no_modes() {
        let mut a = app_with("hi", 5);
        let buf = a.document_buffer_id;
        a.recompute_options_for_buffer(buf);
        let v = a.resolved_option::<lattice_config::Tabstop>(buf);
        assert_eq!(*v, 8);
        let n = a.resolved_option::<lattice_config::Number>(buf);
        assert!(*n);
    }

    #[test]
    fn recompute_options_overlays_active_mode_contributions() {
        let mut a = app_with("hi", 5);
        let buf = a.document_buffer_id;
        // See dispatch.rs notes for `make_mut` vs `get_mut`.
        let registry = std::sync::Arc::make_mut(&mut a.mode_registry);
        let mode_id = registry
            .register(OptionContributingMode::new())
            .expect("register");
        let mut active = lattice_mode::ActiveModes::new();
        let mut locs = lattice_mode::BufferLocals::new();
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locs,
                &a.config,
                &a.event_bus,
                &a.services,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");
        a.active_modes.insert(buf, active);

        a.recompute_options_for_buffer(buf);

        let v = a.resolved_option::<lattice_config::Tabstop>(buf);
        assert_eq!(*v, 4);
        let n = a.resolved_option::<lattice_config::Number>(buf);
        assert!(!*n);
    }

    #[test]
    fn buffer_local_override_beats_mode_contribution() {
        let mut a = app_with("hi", 5);
        let buf = a.document_buffer_id;
        // See dispatch.rs notes for `make_mut` vs `get_mut`.
        let registry = std::sync::Arc::make_mut(&mut a.mode_registry);
        let mode_id = registry
            .register(OptionContributingMode::new())
            .expect("register");
        let mut active = lattice_mode::ActiveModes::new();
        let mut locs = lattice_mode::BufferLocals::new();
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locs,
                &a.config,
                &a.event_bus,
                &a.services,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");
        a.active_modes.insert(buf, active);

        let mut local = lattice_config::OptionOverrideSet::new();
        local.push(lattice_config::OptionOverride::new(
            std::any::TypeId::of::<lattice_config::Tabstop>(),
            16i64,
        ));
        a.buffer_local_overrides.insert(buf, local);

        a.recompute_options_for_buffer(buf);

        let v = a.resolved_option::<lattice_config::Tabstop>(buf);
        assert_eq!(*v, 16);
        let n = a.resolved_option::<lattice_config::Number>(buf);
        assert!(!*n);
    }

    #[test]
    fn resolved_option_falls_back_to_registry_pre_recompute() {
        let a = app_with("hi", 5);
        let buf = a.document_buffer_id;
        let v = a.resolved_option::<lattice_config::Tabstop>(buf);
        assert_eq!(*v, 8);
    }

    #[test]
    fn show_line_numbers_for_resolves_per_buffer() {
        // Two buffers: the active doc keeps the global default
        // (true); the second sets a buffer-local override to false.
        // `show_line_numbers_for` must return the per-buffer
        // resolved value, not the active buffer's setting.
        use crate::buffer_registry::{BufferData, BufferEntry, DocumentEntry};
        use crate::buffers::{BufferFlags, BufferId};
        let mut a = app_with("hi", 5);
        let active = a.document_buffer_id;
        // Manufacture a second document buffer.
        let other = BufferId::next();
        let handle = a.document.clone();
        a.buffers.insert(BufferEntry {
            id: other,
            flags: BufferFlags::default(),
            data: BufferData::Document(DocumentEntry { id: other, handle }),
            name: None,
        });
        // Buffer-local: Number = false on `other`.
        let mut local = lattice_config::OptionOverrideSet::new();
        local.push(lattice_config::OptionOverride::new(
            std::any::TypeId::of::<lattice_config::Number>(),
            false,
        ));
        a.buffer_local_overrides.insert(other, local);
        a.recompute_options_for_buffer(other);
        // Active buffer keeps the default (true); the override
        // only applies to `other`.
        assert!(a.show_line_numbers_for(active));
        assert!(!a.show_line_numbers_for(other));
    }

    // ---- M.3.1: ReadOnly option flows from major modes ----

    #[test]
    fn document_buffer_resolves_read_only_false() {
        let a = app_with("hi", 5);
        let buf = a.document_buffer_id;
        let read_only: bool = *a.resolved_option::<lattice_config::ReadOnly>(buf);
        assert!(!read_only, "Document buffer should be writable by default");
    }

    #[test]
    fn help_buffer_resolves_read_only_true() {
        let mut a = app_with("hi", 5);
        let help = crate::help::HelpContent::from_lines("test", vec!["line one".to_string()]);
        let help_id = a.open_help_in_pane(help);
        let read_only: bool = *a.resolved_option::<lattice_config::ReadOnly>(help_id);
        assert!(
            read_only,
            "Help buffer should resolve ReadOnly = true via help-mode"
        );
    }

    #[test]
    fn read_only_option_is_marked_internal() {
        use lattice_config::OptionDecl;
        assert!(
            !lattice_config::ReadOnly::CUSTOMIZABLE,
            "ReadOnly should be non-customizable (mode-driven)"
        );
    }

    #[test]
    fn set_number_and_nonumber_toggle_show_line_numbers() {
        let mut a = app_with("hello", 10);
        assert!(a.show_line_numbers());
        submit_ex(&mut a, "set nonumber");
        assert!(!a.show_line_numbers());
        submit_ex(&mut a, "set number");
        assert!(a.show_line_numbers());
    }

    #[test]
    fn set_relativenumber_toggles_flag() {
        let mut a = app_with("hello\nworld", 10);
        assert!(!a.relative_line_numbers());
        submit_ex(&mut a, "set relativenumber");
        assert!(a.relative_line_numbers());
        assert!(a.show_line_numbers());
        submit_ex(&mut a, "set norelativenumber");
        assert!(!a.relative_line_numbers());
    }

    #[test]
    fn typing_after_popup_open_live_refilters_candidates() {
        // Vertico-style: typing while the popup is open keeps it
        // open and re-runs the pipeline against the longer prefix.
        let mut a = app_in_command_mode("descr");
        a.apply(Action::CommandLineCompleteOrAdvance);
        assert!(a.completion_state.is_some());
        let initial_count = a.completion_state.as_ref().unwrap().candidates.len();

        a.apply(Action::CommandLineAppend('i'));
        assert!(
            a.completion_state.is_some(),
            "popup must stay open while filtering"
        );
        assert_eq!(a.command_line, "descri");
        // Typing narrows the prefix -> candidate set should shrink
        // or stay equal, never grow.
        let narrowed = a.completion_state.as_ref().unwrap().candidates.len();
        assert!(narrowed <= initial_count);
        // Selection resets to first match (the candidate set
        // changed; previous index would be meaningless).
        assert_eq!(a.completion_state.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn typing_no_match_keeps_popup_open_with_empty_candidates() {
        // Vertico-style: typing past the matchable region leaves the
        // popup alive (just empty), so a single backspace can recover.
        let mut a = app_in_command_mode("descri");
        a.apply(Action::CommandLineCompleteOrAdvance);
        for c in "zxqzxqzxq".chars() {
            a.apply(Action::CommandLineAppend(c));
        }
        let state = a
            .completion_state
            .as_ref()
            .expect("popup must stay open on no-match");
        assert!(state.candidates.is_empty());
        // Backspacing the noise restores matches.
        for _ in 0.."zxqzxqzxq".len() {
            a.apply(Action::CommandLineBackspace);
        }
        assert!(a.completion_state.is_some());
        assert!(!a.completion_state.as_ref().unwrap().candidates.is_empty());
    }

    #[test]
    fn typing_with_no_popup_open_does_not_open_one() {
        // Refresh only fires when a popup is already open; bare
        // typing without a prior <Tab> stays as it was.
        let mut a = app_in_command_mode("desc");
        a.apply(Action::CommandLineAppend('r'));
        assert!(a.completion_state.is_none());
        assert_eq!(a.command_line, "descr");
    }

    #[test]
    fn fresh_app_has_one_document_pane() {
        let a = app_with("xx", 10);
        assert_eq!(a.pane_tree.len(), 1);
        assert_eq!(a.active_buffer, BufferKind::Document);
        let active = a.pane_tree.active();
        assert_eq!(active.buffer, BufferKind::Document);
        assert_eq!(active.buffer_id, a.document_buffer_id);
    }

    #[test]
    fn fresh_app_registers_initial_document() {
        let a = app_with("xx", 10);
        // Listed-buffer view filters out synthetic LSP / messages
        // buffers, leaving just the user's document.
        assert_eq!(a.buffers.listed_ids_sorted().len(), 1);
        assert!(a.buffers.document(a.document_buffer_id).is_some());
    }

    #[test]
    fn set_tabstop_assignment_updates_field() {
        let mut a = app_with("xx", 10);
        a.command_line = "set tabstop=4".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.tabstop(), 4);
    }

    #[test]
    fn set_tabstop_via_alias() {
        let mut a = app_with("xx", 10);
        a.command_line = "set ts=2".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.tabstop(), 2);
    }

    #[test]
    fn effective_completion_for_markdown_default_excludes_lsp() {
        // Spec default: markdown drops LSP for prose. The
        // App's seeded map should reflect this without any
        // TOML being loaded.
        let a = app_with("", 5);
        let eff = a.effective_completion_for("markdown");
        let lsp_id =
            lattice_completion::SourceId::new(lattice_completion::LSP_COMPLETION_SOURCE_ID);
        assert!(!eff.source_enabled(&lsp_id), "markdown default drops LSP");
        let snippet_id = lattice_completion::SourceId::new(lattice_completion::SNIPPET_SOURCE_ID);
        assert!(eff.source_enabled(&snippet_id), "markdown keeps snippet");
        assert!(!eff.auto_trigger);
    }

    #[test]
    fn effective_completion_for_language_with_no_override_allows_all_sources() {
        // A language without any per-language entry returns
        // `sources = None` -> every source contributes
        // (`source_enabled` is unconditionally true).
        let a = app_with("", 5);
        let eff = a.effective_completion_for("zigzig-not-a-language");
        let any_id = lattice_completion::SourceId::new("plugin:custom");
        assert!(eff.source_enabled(&any_id));
        assert!(eff.sources.is_none());
    }

    #[test]
    fn reload_snippets_with_no_dirs_reports_empty() {
        let mut a = app_with("", 10);
        a.do_reload_snippets();
        // Idle; registry stays empty. Message echoed at Info.
        assert_eq!(a.snippet_registry.load().len(), 0);
    }

    #[test]
    fn reload_snippets_walks_configured_dirs_and_keys_by_filename() {
        // Build a tempdir with `_global.json` (any-language)
        // and `rust.json` (language-specific). Reload should
        // route them into the right per-language slots.
        let dir = std::env::temp_dir().join(format!("lattice-snippet-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("_global.json"),
            r#"{ "anywhere": { "prefix": "any", "body": "anywhere" } }"#,
        )
        .unwrap();
        std::fs::write(
            dir.join("rust.json"),
            r#"{ "rust-for": { "prefix": "for", "body": "for $1 {}" } }"#,
        )
        .unwrap();
        let mut a = app_with("", 10);
        a.snippet_dirs.push(dir.clone());
        a.do_reload_snippets();
        // 2 snippets registered total (one per language).
        assert_eq!(a.snippet_registry.load().len(), 2);
        assert!(!a.snippet_registry.load().lookup("rust", "for").is_empty());
        assert!(!a.snippet_registry.load().lookup("*", "any").is_empty());
        // Global snippets are visible from any language --
        // `lookup` walks the per-language slot then `*`.
        assert!(!a.snippet_registry.load().lookup("rust", "any").is_empty());
        // A rust-only snippet should NOT be visible from a
        // different language slot.
        assert!(a.snippet_registry.load().lookup("python", "for").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
