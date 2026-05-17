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

use super::{App, EchoLevel};

impl App {
    // ---- Typed-options accessors (DESIGN.md §5.12) ----
    //
    // The current value of each option lives in `self.editor.config`
    // behind an `ArcSwap` (single source of truth). These
    // accessors read from `self.editor.option_cache` -- a derived
    // projection refreshed via the §5.10 cascade hook on every
    // `Event::OptionChanged` -- so the renderer's per-line option
    // checks stay at field-access speed (~1ns) instead of the
    // ~33ns mutex+ArcSwap+downcast dance per call.

    /// `:set number`. Default `true`. Reads the active buffer's
    /// resolved value via the hot-path cache.
    pub fn show_line_numbers(&self) -> bool {
        self.editor.option_cache.show_line_numbers
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
        self.editor.option_cache.relative_line_numbers
    }

    /// `:set relativenumber` for an arbitrary buffer (per-pane
    /// resolution). Same shape as [`Self::show_line_numbers_for`].
    pub fn relative_line_numbers_for(&self, buffer: crate::buffers::BufferId) -> bool {
        *self.resolved_option::<lattice_config::RelativeNumber>(buffer)
    }

    /// `:set wrap`. Default `false`. (v1 renderer always
    /// horizontal-scrolls; this flag is read by future B.3 polish.)
    pub fn wrap_lines(&self) -> bool {
        self.editor.option_cache.wrap_lines
    }

    /// `:set ignorecase`. Default `false`.
    pub fn ignorecase(&self) -> bool {
        self.editor.option_cache.ignorecase
    }

    /// `:set tabstop=N`. Default `8`. Stored as `i64` in config
    /// (the typed system's integer type) and cast back to `u32`
    /// at cache-rebuild time -- the validate closure on the option
    /// caps the range to `1..=32` so the cast can never lose bits.
    pub fn tabstop(&self) -> u32 {
        self.editor.option_cache.tabstop
    }

    /// `:set scrolloff=N`. Default `0`. Same `i64`→`u32` shape
    /// as [`Self::tabstop`]; range `0..=64`.
    pub fn scrolloff(&self) -> u32 {
        self.editor.option_cache.scrolloff
    }

    /// `:set foldmethod=...`. Default [`FoldMethod::Manual`].
    pub fn foldmethod(&self) -> FoldMethod {
        self.editor.option_cache.foldmethod
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::foldenable`]. Retained as a
    /// 1-line delegate while the renderer's gutter + motions still
    /// reach the App surface.
    pub fn foldenable(&self) -> bool {
        self.editor.foldenable()
    }

    /// 5.5.G.23.cmdline: body migrated to
    /// [`lattice_host::dispatch::Editor::completion_auto_insert_single`].
    /// Retained as a 1-line delegate; deletion follows when the
    /// remaining App callers (cmdline + completion arms) retire.
    pub fn completion_auto_insert_single(&self) -> bool {
        self.editor.completion_auto_insert_single()
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
        self.editor
            .config
            .set_typed::<lattice_config::FoldMethodOption>(fm)
            .expect("set foldmethod");
        self.drain_option_changes();
    }

    /// Set `foldenable` directly. Drains the cascade so the cache
    /// reflects the new value before the caller observes it.
    pub fn set_foldenable_for_test(&mut self, on: bool) {
        let _ = self
            .editor
            .config
            .set_typed::<lattice_config::FoldEnable>(on);
        self.drain_option_changes();
    }

    /// Set `completion.auto_insert_single` directly. Drains the
    /// cascade so the cache reflects the new value before the
    /// caller observes it.
    pub fn set_completion_auto_insert_single_for_test(&mut self, on: bool) {
        let _ = self
            .editor
            .config
            .set_typed::<lattice_config::CompletionAutoInsertSingle>(on);
        self.drain_option_changes();
    }

    /// Delegate to [`lattice_host::editor::Editor::rebuild_option_cache`].
    /// Phase 5.5.E.6 moved the body host-side; this wrapper exists
    /// only so the existing App / test call sites keep compiling
    /// unchanged. Future slices can drop the wrapper once those sites
    /// migrate to `app.editor.rebuild_option_cache()`.
    pub(super) fn rebuild_option_cache(&mut self) {
        self.editor.rebuild_option_cache();
    }

    /// Delegate to
    /// [`lattice_host::editor::Editor::recompute_options_for_buffer`].
    /// Phase 5.5.E.6 moved the body host-side; layer ordering and the
    /// resolver walk are documented on the host method.
    pub fn recompute_options_for_buffer(&mut self, buffer: crate::buffers::BufferId) {
        self.editor.recompute_options_for_buffer(buffer);
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
    /// 5.5.F.5.1: see [`lattice_host::dispatch::Editor::recompute_active_completion_sources_for`].
    pub fn recompute_active_completion_sources_for(&mut self, buffer: crate::buffers::BufferId) {
        self.editor.recompute_active_completion_sources_for(buffer);
    }

    /// Delegate to [`lattice_host::editor::Editor::resolved_option`].
    /// Phase 5.5.E.6 moved the body host-side; this wrapper keeps the
    /// existing hot-path call sites (`app.show_line_numbers_for` etc.)
    /// compiling against `&App` without a per-site rewrite.
    pub fn resolved_option<D: lattice_config::OptionDecl>(
        &self,
        buffer: crate::buffers::BufferId,
    ) -> std::sync::Arc<D::Value>
    where
        D::Value: Clone + Send + Sync + 'static,
    {
        self.editor.resolved_option::<D>(buffer)
    }

    /// Delegate to [`lattice_host::editor::Editor::do_set`]. Phase
    /// 5.5.E.6 moved the cmdline-set body host-side; the host returns
    /// the [`RendererSignal`] list its cascade enqueued and the
    /// renderer fans them out through [`Self::handle_renderer_signal`].
    /// The grammar's `Effect::SetOption` arm now routes through
    /// [`lattice_host::dispatch::handle_effect`] directly; this
    /// wrapper is retained for the App-level integration tests that
    /// exercise the cascade through `a.do_set("...")` (`mode.rs`,
    /// `completion.rs`, `options.rs` test modules) -- production no
    /// longer calls it, so `#[allow(dead_code)]` covers the non-test
    /// build's lint.
    #[allow(dead_code)]
    pub(super) fn do_set(&mut self, option: &str) {
        let signals = self.editor.do_set(option);
        for sig in signals {
            self.handle_renderer_signal(sig);
        }
    }

    /// Delegate to
    /// [`lattice_host::editor::Editor::drain_option_changes`]. Phase
    /// 5.5.E.6 moved the cascade body host-side; the host returns the
    /// [`RendererSignal`] list its cascade enqueued and the renderer
    /// fans them out through [`Self::handle_renderer_signal`].
    /// Why a channel and not a callback: typed-option writes can
    /// originate from anywhere -- the cmdline, plugin tasks
    /// (Phase 7), the customize buffer view (post-1.0), or future
    /// LSP-driven config writes. Routing every cascade through this
    /// channel keeps the per-option cascade off the publish thread.
    pub fn drain_option_changes(&mut self) {
        let signals = self.editor.drain_option_changes();
        for sig in signals {
            self.handle_renderer_signal(sig);
        }
    }

    // 5.5.F.5.4: `mirror_option_to_modes` relocated to
    // [`lattice_host::dispatch::Editor::mirror_option_to_modes`].
    // The App-side wrapper deletes entirely — the cascade now runs
    // synchronously inside `Editor::apply_option_cascade`, and the
    // App-side `RendererSignal::MirrorOptionToModes` handler retired
    // alongside.

    // 5.5.F.3: `:describe-option` / `:options` content builders
    // relocated to
    // [`lattice_host::dispatch::Editor::build_describe_option_content`]
    // and [`build_list_options_content`]; the corresponding
    // `Effect::*` arms now run inside `Editor::handle_effect` and
    // emit `RendererSignal::DisplayBuffer`. Both were Effect-only
    // paths with no direct in-App callers, so no thin wrappers
    // remain App-side.

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
        self.editor
            .pending_config_structural_sections
            .remove(full_path)
    }

    /// Iterate the dotted paths of every pending structural
    /// section whose path starts with `namespace.` (e.g.
    /// `"completion.per-language"` returns the language ids).
    /// Returned as owned `String`s to keep the borrow short --
    /// callers typically follow up with
    /// `take_pending_structural_section(full)` mutating the map.
    pub(super) fn pending_structural_section_paths(&self, namespace: &str) -> Vec<String> {
        let prefix = format!("{namespace}.");
        self.editor
            .pending_config_structural_sections
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect()
    }

    /// Drain every `completion.per-language.<lang>` structural
    /// section the loader bucketed and merge each into
    /// `self.editor.per_language_completion`. Per-key TOML wins over
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
            self.editor
                .per_language_completion
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

// Phase 5.5.E.6: 4.4.k `lsp_server_scope` relocated to
// `lattice_host::dispatch::lsp_server_scope` alongside the migrated
// `apply_option_cascade`. No App-side caller remains.

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

    // 4.4.k `lsp_server_scope` and its standalone unit test moved
    // alongside the cascade in 5.5.E.6; coverage lives at
    // `lattice_host::dispatch` (host-side scope) plus the end-to-end
    // `fan_out_did_change_configuration` integration tests below
    // (which exercise the lsp.<server>.* cascade through the
    // RendererSignal::LspConfigChanged signal).

    // ---- Event::OptionChanged (DESIGN.md §5.10 + §5.12) ----

    #[test]
    fn event_bus_publishes_option_changed_on_set_assign() {
        let mut a = app_with("xx", 10);
        let mut rx = subscribe_all_events(&a);
        a.editor.command_line = "set tabstop=4".into();
        a.editor.modal = ModalState::Command;
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
        a.editor.command_line = "set nonumber".into();
        a.editor.modal = ModalState::Command;
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
        a.editor
            .config
            .set_typed::<lattice_config::FoldMethodOption>(FoldMethod::Indent)
            .unwrap();
        // Folds should not be populated yet -- the cascade is
        // queued but hasn't been drained.
        a.drain_option_changes();
        assert_eq!(a.foldmethod(), FoldMethod::Indent);
        assert!(
            !a.editor.folds.is_empty(),
            "drain_option_changes should run the foldmethod cascade and recompute folds"
        );
    }

    #[test]
    fn drain_option_changes_runs_relativenumber_to_number_cascade() {
        let mut a = app_with("xx", 10);
        a.editor
            .config
            .set_typed::<lattice_config::Number>(false)
            .unwrap();
        a.drain_option_changes();
        assert!(!a.show_line_numbers());
        a.editor
            .config
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
        let cache = &a.editor.option_cache;
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
        assert!(!a.editor.option_cache.current_line_highlight);
    }

    #[test]
    fn whitespace_glyph_set_via_set_propagates_to_cache() {
        // M.7.3.a: a `:set display.whitespace.tab=⇥` write
        // flows through the cascade and updates the cache's
        // `whitespace_tab` to the new glyph.
        let mut a = app_with("xx", 10);
        a.do_set("display.whitespace.tab=⇥");
        a.drain_option_changes();
        assert_eq!(a.editor.option_cache.whitespace_tab, Some('⇥'));
        // Empty string disables the category.
        a.do_set("display.whitespace.tab=");
        a.drain_option_changes();
        assert_eq!(a.editor.option_cache.whitespace_tab, None);
    }

    #[test]
    fn drain_option_changes_runs_ui_theme_sync_for_direct_writes() {
        let mut a = app_with("xx", 10);
        a.editor
            .config
            .set_typed::<lattice_host::ui::theme_options::UiDimInactive>(false)
            .unwrap();
        a.drain_option_changes();
        assert!(
            !a.theme.dim_inactive_panes,
            "ui.dim_inactive=false should propagate to theme.dim_inactive_panes via the cascade"
        );
    }

    #[test]
    fn ui_set_cascade_keeps_host_theme_and_tui_theme_in_sync() {
        // Phase 5.3 contract: `host_theme` is the canonical
        // renderer-neutral state; `theme` is the cached
        // ratatui-typed adapter. `sync_theme_from_config` MUST
        // update both. We exercise a representative subset of
        // user-tweakable fields (`dim_inactive`, `nerd_fonts`,
        // separator glyph, separator color) and assert each
        // mutation lands in BOTH places.
        let mut a = app_with("xx", 10);
        // Flip dim_inactive false.
        a.editor
            .config
            .set_typed::<lattice_host::ui::theme_options::UiDimInactive>(false)
            .unwrap();
        a.drain_option_changes();
        assert!(
            !a.editor.host_theme.dim_inactive_panes,
            "host: dim_inactive flipped"
        );
        assert!(!a.theme.dim_inactive_panes, "tui: dim_inactive flipped");
        // Flip nerd_fonts on.
        a.editor
            .config
            .set_typed::<lattice_host::ui::theme_options::UiNerdFonts>(true)
            .unwrap();
        a.drain_option_changes();
        assert!(a.editor.host_theme.nerd_fonts);
        assert!(a.theme.nerd_fonts);
        // Change separator glyph.
        a.editor
            .config
            .set_typed::<lattice_host::ui::theme_options::UiSeparator>("┃".to_string())
            .unwrap();
        a.drain_option_changes();
        assert_eq!(a.editor.host_theme.pane_separator_vertical, '┃');
        assert_eq!(a.theme.pane_separator_vertical, '┃');
        // Change separator color (named).
        a.editor
            .config
            .set_typed::<lattice_host::ui::theme_options::UiSeparatorColor>("red".to_string())
            .unwrap();
        a.drain_option_changes();
        use lattice_host::ui::theme as ht;
        assert_eq!(
            a.editor.host_theme.pane_separator.fg,
            Some(ht::Color::Named(ht::NamedColor::Red)),
            "host: separator fg=red",
        );
        assert_eq!(
            a.theme.pane_separator.fg,
            Some(ratatui::style::Color::Red),
            "tui: separator fg=red",
        );
    }

    #[test]
    fn drain_option_changes_handles_chained_cascade_writes() {
        let mut a = app_with("xx", 10);
        a.editor
            .config
            .set_typed::<lattice_config::Number>(false)
            .unwrap();
        a.drain_option_changes();
        a.editor
            .config
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
        a.editor.command_line = "set number?".into();
        a.editor.modal = ModalState::Command;
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
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
        assert!(msg.text.contains("frobnicate"));
    }

    #[test]
    fn set_unknown_option_errors() {
        let mut a = app_with("xx", 10);
        a.editor.command_line = "set whatever".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.editor.last_message.as_ref().expect("error");
        assert!(msg.text.contains("Unknown option"), "got: {}", msg.text);
    }

    #[test]
    fn set_no_form_clears_boolean() {
        let mut a = app_with("xx", 10);
        a.editor.command_line = "set nonumber".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert!(!a.show_line_numbers());
    }

    #[test]
    fn set_no_form_rejects_non_boolean() {
        let mut a = app_with("xx", 10);
        a.editor.command_line = "set notabstop".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.editor.last_message.as_ref().expect("error");
        assert!(msg.text.contains("not a boolean"), "got: {}", msg.text);
    }

    #[test]
    fn set_int_out_of_range_errors() {
        let mut a = app_with("xx", 10);
        a.editor.command_line = "set tabstop=999".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        let msg = a.editor.last_message.as_ref().expect("error");
        assert!(msg.text.contains("out of range"), "got: {}", msg.text);
    }

    // ---- :describe-option / :options ----

    #[test]
    fn describe_option_renders_help_with_metadata() {
        let mut a = app_with("xx", 10);
        a.editor.command_line = "describe-option tabstop".into();
        a.editor.modal = ModalState::Command;
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
        a.editor.command_line = "options".into();
        a.editor.modal = ModalState::Command;
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
        a.editor.command_line = "options".into();
        a.editor.modal = ModalState::Command;
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
        a.editor.command_line = "options".into();
        a.editor.modal = ModalState::Command;
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
        type Guard = ();
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
        fn on_activate(
            &self,
            _ctx: lattice_mode::ModeContext,
        ) -> lattice_mode::LifecycleFuture<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn recompute_options_uses_registry_defaults_when_no_modes() {
        let mut a = app_with("hi", 5);
        let buf = a.editor.document_buffer_id;
        a.recompute_options_for_buffer(buf);
        let v = a.resolved_option::<lattice_config::Tabstop>(buf);
        assert_eq!(*v, 8);
        let n = a.resolved_option::<lattice_config::Number>(buf);
        assert!(*n);
    }

    #[test]
    fn recompute_options_overlays_active_mode_contributions() {
        let mut a = app_with("hi", 5);
        let buf = a.editor.document_buffer_id;
        // See dispatch.rs notes for `make_mut` vs `get_mut`.
        let registry = std::sync::Arc::make_mut(&mut a.editor.mode_registry);
        let mode_id = registry
            .register(OptionContributingMode::new())
            .expect("register");
        let mut active = lattice_mode::ActiveModes::new();
        let guards = lattice_mode::GuardStoreHandle::new();
        a.editor
            .mode_registry
            .activate_minor(
                &mut active,
                &guards,
                &a.editor.config,
                &a.editor.event_bus,
                &a.editor.services,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");
        a.editor.active_modes.insert(buf, active);

        a.recompute_options_for_buffer(buf);

        let v = a.resolved_option::<lattice_config::Tabstop>(buf);
        assert_eq!(*v, 4);
        let n = a.resolved_option::<lattice_config::Number>(buf);
        assert!(!*n);
    }

    #[test]
    fn buffer_local_override_beats_mode_contribution() {
        let mut a = app_with("hi", 5);
        let buf = a.editor.document_buffer_id;
        // See dispatch.rs notes for `make_mut` vs `get_mut`.
        let registry = std::sync::Arc::make_mut(&mut a.editor.mode_registry);
        let mode_id = registry
            .register(OptionContributingMode::new())
            .expect("register");
        let mut active = lattice_mode::ActiveModes::new();
        let guards = lattice_mode::GuardStoreHandle::new();
        a.editor
            .mode_registry
            .activate_minor(
                &mut active,
                &guards,
                &a.editor.config,
                &a.editor.event_bus,
                &a.editor.services,
                lattice_protocol::ids::BufferId::new(0),
                mode_id,
                lattice_mode::CapabilitySet::empty(),
            )
            .expect("activate");
        a.editor.active_modes.insert(buf, active);

        let mut local = lattice_config::OptionOverrideSet::new();
        local.push(lattice_config::OptionOverride::new(
            std::any::TypeId::of::<lattice_config::Tabstop>(),
            16i64,
        ));
        a.editor.buffer_local_overrides.insert(buf, local);

        a.recompute_options_for_buffer(buf);

        let v = a.resolved_option::<lattice_config::Tabstop>(buf);
        assert_eq!(*v, 16);
        let n = a.resolved_option::<lattice_config::Number>(buf);
        assert!(!*n);
    }

    #[test]
    fn resolved_option_falls_back_to_registry_pre_recompute() {
        let a = app_with("hi", 5);
        let buf = a.editor.document_buffer_id;
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
        let active = a.editor.document_buffer_id;
        // Manufacture a second document buffer.
        let other = BufferId::next();
        let handle = a.editor.document.clone();
        a.editor.buffers.insert(BufferEntry {
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
        a.editor.buffer_local_overrides.insert(other, local);
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
        let buf = a.editor.document_buffer_id;
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
        assert!(a.editor.completion_state.is_some());
        let initial_count = a.editor.completion_state.as_ref().unwrap().candidates.len();

        a.apply(Action::CommandLineAppend('i'));
        assert!(
            a.editor.completion_state.is_some(),
            "popup must stay open while filtering"
        );
        assert_eq!(a.editor.command_line, "descri");
        // Typing narrows the prefix -> candidate set should shrink
        // or stay equal, never grow.
        let narrowed = a.editor.completion_state.as_ref().unwrap().candidates.len();
        assert!(narrowed <= initial_count);
        // Selection resets to first match (the candidate set
        // changed; previous index would be meaningless).
        assert_eq!(a.editor.completion_state.as_ref().unwrap().selected, 0);
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
            .editor
            .completion_state
            .as_ref()
            .expect("popup must stay open on no-match");
        assert!(state.candidates.is_empty());
        // Backspacing the noise restores matches.
        for _ in 0.."zxqzxqzxq".len() {
            a.apply(Action::CommandLineBackspace);
        }
        assert!(a.editor.completion_state.is_some());
        assert!(
            !a.editor
                .completion_state
                .as_ref()
                .unwrap()
                .candidates
                .is_empty()
        );
    }

    #[test]
    fn typing_with_no_popup_open_does_not_open_one() {
        // Refresh only fires when a popup is already open; bare
        // typing without a prior <Tab> stays as it was.
        let mut a = app_in_command_mode("desc");
        a.apply(Action::CommandLineAppend('r'));
        assert!(a.editor.completion_state.is_none());
        assert_eq!(a.editor.command_line, "descr");
    }

    #[test]
    fn fresh_app_has_one_document_pane() {
        let a = app_with("xx", 10);
        assert_eq!(a.editor.pane_tree.len(), 1);
        assert_eq!(a.editor.active_buffer, BufferKind::Document);
        let active = a.editor.pane_tree.active();
        assert_eq!(active.buffer, BufferKind::Document);
        assert_eq!(active.buffer_id, a.editor.document_buffer_id);
    }

    #[test]
    fn fresh_app_registers_initial_document() {
        let a = app_with("xx", 10);
        // Listed-buffer view filters out synthetic LSP / messages
        // buffers, leaving just the user's document.
        assert_eq!(a.editor.buffers.listed_ids_sorted().len(), 1);
        assert!(
            a.editor
                .buffers
                .contains_document(a.editor.document_buffer_id)
        );
    }

    #[test]
    fn set_tabstop_assignment_updates_field() {
        let mut a = app_with("xx", 10);
        a.editor.command_line = "set tabstop=4".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.tabstop(), 4);
    }

    #[test]
    fn set_tabstop_via_alias() {
        let mut a = app_with("xx", 10);
        a.editor.command_line = "set ts=2".into();
        a.editor.modal = ModalState::Command;
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
        assert_eq!(a.editor.snippet_registry.load().len(), 0);
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
        a.editor.snippet_dirs.push(dir.clone());
        a.do_reload_snippets();
        // 2 snippets registered total (one per language).
        assert_eq!(a.editor.snippet_registry.load().len(), 2);
        assert!(
            !a.editor
                .snippet_registry
                .load()
                .lookup("rust", "for")
                .is_empty()
        );
        assert!(
            !a.editor
                .snippet_registry
                .load()
                .lookup("*", "any")
                .is_empty()
        );
        // Global snippets are visible from any language --
        // `lookup` walks the per-language slot then `*`.
        assert!(
            !a.editor
                .snippet_registry
                .load()
                .lookup("rust", "any")
                .is_empty()
        );
        // A rust-only snippet should NOT be visible from a
        // different language slot.
        assert!(
            a.editor
                .snippet_registry
                .load()
                .lookup("python", "for")
                .is_empty()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
