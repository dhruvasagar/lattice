//! Boot / config-load / sync paths the runtime calls before the
//! main loop starts -- the App's once-per-launch infrastructure.
//!
//! Methods that live here:
//! - `sync_keymap_overlays` (re-stack the popup / snippet
//!   minor-mode keymap layers in lockstep with overlay state).
//! - `sync_theme_from_config` (re-derive `App.theme`'s renderer-
//!   specific `Style` values from `ui.*` typed options).
//! - `load_persistent_config` (read user + project TOML and
//!   apply scalar overrides + bucket structural sub-tables).
//! - `App::new` (the once-per-launch constructor) and
//!   `build_lsp_subsystem` (its sub-helper).
//!
//! What does NOT live here: the option resolver itself
//! (`lattice-config`), the keymap registry
//! (`crate::keymap_registry`), the theme parser
//! (`crate::theme`). This module is the App's *boot wiring*
//! over those.

use super::{App, EchoLevel};

impl App {
    /// Re-stack the Insert-mode minor-mode overlays
    /// (completion popup + active snippet) so the layered
    /// keymap registry mirrors the App's overlay state. Called
    /// from the apply loop after every `Action`; cheap when
    /// nothing changed (single mutex acquisition + early
    /// return).
    ///
    /// Push order is enforced here so popup always sits at the
    /// top of the stack when both overlays are active: the
    /// method pops everything, then pushes snippet (if active),
    /// then popup (if active). Popup's `LayerId` is therefore
    /// always higher than snippet's, and popup wins on
    /// overlapping chords (preserving the legacy "popup
    /// precedes snippet" gating in `input::translate`).
    ///
    /// Slice 8.f.
    pub fn sync_keymap_overlays(&mut self) {
        let want_popup = self.insert_completion.is_some();
        let want_snippet = self.active_snippet.is_some();
        let have_popup = self.completion_popup_layer.is_some();
        let have_snippet = self.snippet_layer.is_some();
        if want_popup == have_popup && want_snippet == have_snippet {
            return;
        }
        // Re-stack: pop everything, then push in the canonical
        // order (snippet first, popup second).
        if let Some(id) = self.completion_popup_layer.take() {
            self.keymap.pop_layer(id);
        }
        if let Some(id) = self.snippet_layer.take() {
            self.keymap.pop_layer(id);
        }
        if want_snippet {
            let id = self.keymap.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode,
                "active-snippet",
                crate::keymap_insert::active_snippet_layer_bindings(&self.action_ids),
            );
            self.snippet_layer = Some(id);
        }
        if want_popup {
            let id = self.keymap.push_layer(
                crate::keymap_registry::PushLayerKind::MinorMode,
                "completion-popup",
                crate::keymap_insert::completion_popup_layer_bindings(&self.action_ids),
            );
            self.completion_popup_layer = Some(id);
        }
    }

    /// Re-derive `App.theme`'s renderer-specific [`Style`] values
    /// from the current `ui.*` option values in the config. Called
    /// at App-init time (after registration) and on every `:set
    /// ui.*` so the cached theme stays in lockstep with the
    /// canonical primitives in config.
    pub fn sync_theme_from_config(&mut self) {
        use crate::tui_options::{
            UiDimInactive, UiSeparator, UiSeparatorColor, UiStatuslineActiveFg,
            UiStatuslineInactiveFg,
        };
        use ratatui::style::Style;
        // ui.dim_inactive -- bool flag projected directly.
        self.theme.dim_inactive_panes =
            *self.config.get_typed::<UiDimInactive>().expect("UiDimInactive");
        // ui.separator -- one-character glyph for the vertical
        // pane divider. Validated to len==1 at parse; fall back to
        // the default if a forged value sneaks through.
        let sep = self.config.get_typed::<UiSeparator>().expect("UiSeparator");
        self.theme.pane_separator_vertical = sep.chars().next().unwrap_or('│');
        // ui.separator_color -- color name; parse_color returned
        // Ok during validate so unwrap-via-fallback is safe.
        let sep_color = self
            .config
            .get_typed::<UiSeparatorColor>()
            .expect("UiSeparatorColor");
        if let Ok(c) = crate::theme::parse_color(&sep_color) {
            self.theme.pane_separator = Style::default().fg(c);
        }
        // ui.statusline_active_fg -- foreground only; preserve any
        // existing modifiers / background by chaining `.fg(c)` on
        // the current style.
        let active_fg = self
            .config
            .get_typed::<UiStatuslineActiveFg>()
            .expect("UiStatuslineActiveFg");
        if let Ok(c) = crate::theme::parse_color(&active_fg) {
            self.theme.pane_status_active = self.theme.pane_status_active.fg(c);
        }
        let inactive_fg = self
            .config
            .get_typed::<UiStatuslineInactiveFg>()
            .expect("UiStatuslineInactiveFg");
        if let Ok(c) = crate::theme::parse_color(&inactive_fg) {
            self.theme.pane_status_inactive = self.theme.pane_status_inactive.fg(c);
        }
    }

    /// Load `~/.config/lattice/lattice.toml` (user) and
    /// `<workspace_root>/.lattice/config.toml` (project) in
    /// precedence order, applying scalar overrides to
    /// `self.config` and bucketing structural sub-tables (per-
    /// language overrides, plugin sections) into
    /// `self.pending_config_structural_sections` for their
    /// owners to drain.
    ///
    /// Called once by the runtime startup before the main loop
    /// (so the first frame already reflects user overrides).
    /// NOT called from `App::new` -- tests stay isolated from
    /// the user's real `~/.config/lattice/`. Test fixtures that
    /// want to exercise the load path can call this directly
    /// with a synthesized workspace root.
    ///
    /// Loader diagnostics (parse errors, unknown keys,
    /// validation rejects) collapse into a single echo at the
    /// most-severe level: `Error` if any file failed to
    /// parse / read, `Warn` if any key was rejected, otherwise
    /// silent. Per-file `path:body` detail rides the message
    /// body so the user can see *which* file complained.
    pub fn load_persistent_config(&mut self, workspace_root: Option<&std::path::Path>) {
        // The structural prefixes the App / future plugin host
        // own. The per-language layer drains
        // `completion.per-language.*`; the plugin host (Phase 7)
        // will drain `plugin.*`; `lsp` is bucketed so the
        // loader doesn't fire unknown-option warnings for
        // server-namespaced keys (the cached raw_tree carries
        // the values; `workspace/configuration` walks it).
        let prefixes = ["completion.per-language", "plugin", "lsp"];
        let outcome = lattice_config::load_default_paths(
            &self.config,
            workspace_root,
            &prefixes,
        );
        // Re-derive theme + hot-path option cache after the
        // loader's writes. ui.* and the cached options may have
        // changed; missing this would leave the first frame
        // rendering with stale derived state.
        self.sync_theme_from_config();
        self.rebuild_option_cache();
        // Stash structural sections for the layers that own
        // them. Subsequent slices drain via
        // `take_pending_structural_section(prefix)`.
        for (k, v) in outcome.structural {
            self.pending_config_structural_sections.insert(k, v);
        }
        // Cache the merged TOML tree so
        // `workspace/configuration` can walk server-namespaced
        // keys (Phase 4.1 follow-up). Project files override
        // user files at deep-merge time so an `[lsp.X.Y]`
        // sibling key in the user config survives a project
        // override of `[lsp.X.Z]`.
        self.lsp_config_tree = outcome.raw_tree;
        // Apply editor-side LSP options that live in the same
        // `[lsp]` table as server-namespaced keys. These are scalars
        // the editor consumes itself (not forwarded via
        // `workspace/configuration`); the loader buckets the whole
        // `lsp` subtree as structural so they're reachable here via
        // `lsp_config_tree`.
        self.apply_persistent_lsp_editor_options();
        // Surface a single echo summarising loader diagnostics.
        // The renderer's modeline only shows the latest echo,
        // so multi-warn configs collapse into "<count> issues
        // (first: <body>)". Severity is the max across the run.
        if outcome.messages.is_empty() {
            return;
        }
        let max_level = outcome
            .messages
            .iter()
            .map(|m| m.level)
            .max_by_key(|l| match l {
                lattice_config::LoadMessageLevel::Error => 1,
                lattice_config::LoadMessageLevel::Warning => 0,
            })
            .unwrap_or(lattice_config::LoadMessageLevel::Warning);
        let echo_level = match max_level {
            lattice_config::LoadMessageLevel::Error => EchoLevel::Error,
            lattice_config::LoadMessageLevel::Warning => EchoLevel::Warn,
        };
        let count = outcome.messages.len();
        let first = &outcome.messages[0];
        let body = if count == 1 {
            format!(
                "config: {}: {}",
                first.source.display(),
                first.body,
            )
        } else {
            format!(
                "config: {count} issues (first: {}: {})",
                first.source.display(),
                first.body,
            )
        };
        self.set_message(echo_level, body);
    }
}
