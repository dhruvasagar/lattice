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

use lattice_protocol::Event;

use super::{App, EchoLevel, OptionCache};
use crate::help::HelpBuffer;

impl App {
    /// Repopulate [`Self::option_cache`] from the canonical values
    /// in [`Self::config`]. Called at App-init time and from the
    /// `Event::OptionChanged` cascade so any write source (cmdline,
    /// plugin, customize buffer) refreshes the renderer-visible
    /// projection. Cheap: 9 typed reads (~30ns each).
    pub(super) fn rebuild_option_cache(&mut self) {
        use lattice_config::{
            CompletionAutoInsertSingle, FoldEnable, FoldMethodOption, IgnoreCase, Number,
            RelativeNumber, Scrolloff, Tabstop, Wrap,
        };
        // Type-keyed reads against the post-boot registry.
        // `expect` is fine here -- registration happens in
        // `App::new` before this function is reachable, and a
        // missing option is a build-config bug, not a runtime
        // condition we recover from.
        self.option_cache = OptionCache {
            show_line_numbers: *self.config.get_typed::<Number>().expect("Number"),
            relative_line_numbers: *self
                .config
                .get_typed::<RelativeNumber>()
                .expect("RelativeNumber"),
            wrap_lines: *self.config.get_typed::<Wrap>().expect("Wrap"),
            ignorecase: *self.config.get_typed::<IgnoreCase>().expect("IgnoreCase"),
            tabstop: *self.config.get_typed::<Tabstop>().expect("Tabstop") as u32,
            foldenable: *self.config.get_typed::<FoldEnable>().expect("FoldEnable"),
            foldmethod: *self
                .config
                .get_typed::<FoldMethodOption>()
                .expect("FoldMethodOption"),
            scrolloff: *self.config.get_typed::<Scrolloff>().expect("Scrolloff") as u32,
            completion_auto_insert_single: *self
                .config
                .get_typed::<CompletionAutoInsertSingle>()
                .expect("CompletionAutoInsertSingle"),
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
        let modes_snapshot = self
            .active_modes
            .get(&buffer)
            .cloned()
            .unwrap_or_default();

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
        self.config
            .get_typed::<D>()
            .expect("option not registered")
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
        // Refresh the hot-path cache so subsequent reads from
        // `app.show_line_numbers()` etc. see the new value.
        // Cheap (~300ns total for all 9 options); only runs when
        // an option actually changed, never on every frame.
        self.rebuild_option_cache();
        match canonical_name {
            "relativenumber" => {
                // Vim cascade: `:set rnu` implies `:set nu` so the
                // gutter renders at all. The reverse (`:set nornu`)
                // does NOT clear `nu` -- preserves user intent.
                // Conditional on the new value being `true`, which
                // we re-read through the typed handle (cheap).
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
            }
            _ => {}
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
        self.open_help(
            HelpBuffer::from_lines(format!("describe-option {name}"), lines)
                .with_markdown_syntax(self.lang_registry.clone()),
        );
    }

    /// `:options` -- list every registered option in a help view.
    pub(super) fn do_list_options(&mut self) {
        let mut lines: Vec<String> = Vec::new();
        let mut specs = self.config.iter();
        specs.sort_by_key(|s| s.name());
        lines.push(format!("{} registered option(s):", specs.len()));
        lines.push(String::new());
        for spec in specs {
            lines.push(format!(
                "  {:<32} {:<10} = {}",
                spec.name(),
                spec.type_label(),
                spec.get_formatted()
            ));
        }
        self.open_help(
            HelpBuffer::from_lines("options", lines)
                .with_markdown_syntax(self.lang_registry.clone()),
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
    pub(super) fn pending_structural_section_paths(
        &self,
        namespace: &str,
    ) -> Vec<String> {
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
                format!("config: {count} per-language warnings (first: {})", warnings[0])
            };
            self.set_message(EchoLevel::Warn, body);
        }
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
                        .filter_map(|v| {
                            v.as_str().map(lattice_completion::canonical_source_id)
                        })
                        .collect();
                    out.sources = Some(sources);
                }
                None => warnings.push(format!(
                    "{section_path}.sources: expected array of strings",
                )),
            },
            "auto_trigger" => match value.as_bool() {
                Some(b) => out.auto_trigger = Some(b),
                None => warnings.push(format!(
                    "{section_path}.auto_trigger: expected bool",
                )),
            },
            "auto_insert_single" => match value.as_bool() {
                Some(b) => out.auto_insert_single = Some(b),
                None => warnings.push(format!(
                    "{section_path}.auto_insert_single: expected bool",
                )),
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

    use crate::app::*;
    use crate::app::test_helpers::{app_with, subscribe_all_events, submit_ex};
    use lattice_grammar::ModalState;
    use lattice_protocol::Event;

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
        let h = a.help_buffer.as_ref().expect("describe-option help");
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
        let h = a.help_buffer.as_ref().expect("options help");
        let body = h.content.as_string();
        assert!(body.contains("number"));
        assert!(body.contains("tabstop"));
        assert!(body.contains("scrolloff"));
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
        let registry = std::sync::Arc::get_mut(&mut a.mode_registry)
            .expect("mode_registry should be uniquely held in test setup");
        let mode_id = registry
            .register(OptionContributingMode::new())
            .expect("register");
        let mut active = lattice_mode::ActiveModes::new();
        let mut locs = lattice_mode::BufferLocals::new();
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locs,
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
        let registry = std::sync::Arc::get_mut(&mut a.mode_registry)
            .expect("mode_registry uniquely held");
        let mode_id = registry
            .register(OptionContributingMode::new())
            .expect("register");
        let mut active = lattice_mode::ActiveModes::new();
        let mut locs = lattice_mode::BufferLocals::new();
        a.mode_registry
            .activate_minor(
                &mut active,
                &mut locs,
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
        let help = crate::help::HelpBuffer::from_lines(
            "test",
            vec!["line one".to_string()],
        );
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
}
