//! Host-side applier for the `*dashboard*` launch page (DB.2).
//!
//! `lattice-dashboard` owns the section registry, the fragment content
//! contract, the `dashboard-mode` major, and the config. The host owns what a
//! crate cannot: buffer creation (mutates `&mut Editor`) and the fragment →
//! `HelpContent` conversion (the host owns the help machinery). `:dashboard`
//! and the startup trigger (DB.5) both route through `Effect::OpenDashboard`
//! to [`Editor::do_open_dashboard`] — the sanctioned lifecycle boundary,
//! mirroring `:messages`→`do_open_messages`.
//!
//! See `docs/dev/architecture/dashboard.md` §9.

use lattice_dashboard::{
    DashboardBrandingProvider, DashboardCtx, DashboardFragment, DashboardRegistry, DashboardRole,
    DashboardRow, DashboardSource, LinkTarget, SectionSelection,
};

use crate::dispatch::RendererSignal;
use crate::editor::Editor;

impl Editor {
    /// Open (or re-compose + activate) the `*dashboard*` buffer. Idempotent:
    /// a second call re-seeds the existing buffer in place rather than
    /// creating a duplicate.
    pub fn do_open_dashboard(&mut self) -> Vec<RendererSignal> {
        let content = self.build_dashboard_content();
        // DB.4: the content block width (widest body line vs the branding
        // block) drives gutter-based horizontal centring — the renderer pads
        // the gutter by `(viewport_width - block_width)/2` so the banner + body
        // share one centred margin, with no text mutation (markdown intact).
        let body_max = content
            .buffer
            .content
            .as_string()
            .lines()
            .map(|l| l.chars().count() as u32)
            .max()
            .unwrap_or(0);
        let block_width = body_max.max(lattice_dashboard::branding_block_width());
        let id = match self.buffers.by_name("*dashboard*") {
            Some(existing) => {
                // Re-compose in place (config / future refresh triggers) —
                // keeps the BufferId stable like the help back-stack swap.
                let text = content.buffer.content.as_string();
                self.replace_owned_document_text(existing, &text);
                self.seed_help_metadata_locals(existing, content.metadata);
                existing
            }
            None => {
                let id = self.register_dashboard_document(content, Self::SYNTHETIC_BUFFER_FLAGS);
                // Assign the major explicitly (synthetic buffers bypass
                // language detection); this also recomputes options so
                // dashboard-mode's ReadOnly/NoFile take effect.
                self.activate_major_by_id(id, lattice_dashboard::DashboardMode::mode_id());
                id
            }
        };
        // DB.4: mark the buffer for gutter-based horizontal centring (read by
        // rebuild_option_cache to compute content_left_pad).
        self.buffer_locals
            .entry(id)
            .or_default()
            .insert(crate::modes::CenterContentWidth(block_width));
        // (re-)register the branding virtual-row block for this buffer.
        self.register_dashboard_branding(id);
        let signals = if self.activate_buffer(id) {
            self.activate_buffer_state()
        } else {
            Vec::new()
        };
        // Activate help-mode as a companion minor for its good defaults —
        // read-only, wrap, no-file, gutterless (no line numbers / signcolumn).
        // Done after activate_buffer_state so its major re-activation doesn't
        // clobber the minor; then recompute so the renderer's option cache
        // reflects the gutterless treatment.
        use lattice_mode::ModeActivator;
        self.activate_minor_by_id(id, lattice_mode::HelpMode::mode_id());
        self.recompute_options_for_buffer(id);
        self.rebuild_option_cache();
        // Dashboard is a static splash page: force cursor + scroll to
        // the top so the user lands on the branding on every open.
        // Without this, `activate_document`'s `load_active_pane`
        // restores the stale pane cursor from the previous buffer
        // (snapshot_active_pane writes it, then load_active_pane
        // overrides the explicit ZERO set at dispatch.rs:27914).
        self.cursor = lattice_protocol::position::Position::ZERO;
        self.scroll = 0;
        // OWC: populating the dashboard body is an owner write, which the
        // in-core selection transform clamps to EOF (owner-write-caret.md
        // §4.1: a caret inside a fully-replaced range lands at the
        // replacement's end). The forced top-of-page caret above must be
        // authoritative, so sync it through to the document's selection and
        // mark the text version seen — otherwise the next keystroke's
        // `maybe_adopt_owner_write` adopts that EOF selection back into
        // `Editor::cursor` and the whole buffer scrolls to the bottom.
        self.write_through_caret();
        self.last_seen_text_version
            .insert(id, self.document.snapshot().text_version);
        signals
    }

    /// DB.4: register the branding virtual-row provider for the dashboard
    /// buffer (idempotent — unregister-first, mirroring tutor). The provider
    /// resolves the `dashboard.*` colours (DB.3) at collect time. Skipped when
    /// the theme service is absent (headless harness); production always has
    /// it.
    fn register_dashboard_branding(&mut self, id: lattice_core::BufferId) {
        let provider_id = DashboardBrandingProvider::provider_id_for(id.0 as u64);
        self.virtual_row_providers.unregister(id, provider_id);
        let Some(theme) = self
            .services
            .get::<lattice_theme::ThemeRegistryHandle>()
            .map(|outer| (*outer).clone())
        else {
            return;
        };
        let owner = lattice_theme::ElementOwner::Mode(
            lattice_dashboard::DashboardMode::mode_id().as_str().to_string().into(),
        );
        // Idempotent: returns the same interned ids on-activate already
        // registered.
        let ids = lattice_dashboard::register_dashboard_theme_elements(theme.as_ref(), owner);
        let provider = DashboardBrandingProvider::new(provider_id, Some(theme), ids);
        self.virtual_row_providers.register(id, std::sync::Arc::new(provider));
    }

    /// Compose the dashboard body into a `HelpContent`. `dashboard.source`
    /// (DB.6, design §8) is the "author the entire page" escape hatch: when
    /// set to a readable path, its content REPLACES section composition
    /// entirely. Unset, empty, missing, or unreadable ⇒ fall back to the
    /// normal registry-composed sections — never a panic, never an empty
    /// page (a read error is logged once per call, not silently dropped).
    fn build_dashboard_content(&self) -> lattice_help::HelpContent {
        if let Some(path) = self.dashboard_source_path() {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let lines: Vec<String> = text.lines().map(str::to_string).collect();
                    return lattice_help::HelpContent::from_lines("*dashboard*", lines);
                }
                Err(err) => {
                    tracing::warn!(
                        path = %path,
                        error = %err,
                        "dashboard.source unreadable; falling back to section composition"
                    );
                }
            }
        }
        self.compose_dashboard_sections()
    }

    /// `dashboard.source`, trimmed and filtered to `None` when unset/blank
    /// (the config system has no native `Option<String>`, so empty-string is
    /// the "unset" sentinel — design §8).
    fn dashboard_source_path(&self) -> Option<String> {
        self.config
            .get_typed::<DashboardSource>()
            .map(|raw| raw.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Compose the config-selected built-in sections into a `HelpContent`.
    /// `pane_width` / `nerd_fonts` read the live viewport + `ui.nerd_fonts`
    /// (DB.6 — no built-in section consumes either yet, but the plumbing is
    /// real rather than a placeholder, so a future icon-aware section needs
    /// no host-side change).
    fn compose_dashboard_sections(&self) -> lattice_help::HelpContent {
        let selection = self
            .config
            .get_typed::<lattice_dashboard::DashboardSections>()
            .map(|raw| SectionSelection::parse(raw.as_str()))
            .unwrap_or(SectionSelection::Default);

        let nerd_fonts = self
            .config
            .get_typed::<crate::ui::theme_options::UiNerdFonts>()
            .map(|v| *v)
            .unwrap_or(false);
        let ctx = DashboardCtx {
            pane_width: self.pane_tree.active().viewport_width as usize,
            nerd_fonts,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };

        let fragments = match self.services.get::<DashboardRegistry>() {
            Some(registry) => registry.compose(&ctx, &selection),
            None => {
                tracing::warn!("dashboard registry service missing; rendering an empty dashboard");
                Vec::new()
            }
        };
        // Body is left-aligned for now: block-centring by padding the text
        // broke markdown header styling (leading spaces => indented code
        // block). Centring-with-markdown needs renderer-side content align
        // (see DB.4 slice plan) — pending.
        dashboard_fragments_to_help_content(fragments)
    }
}

/// Render one dashboard row to a markdown line. Link spans become
/// `[label](scheme:value)` using the help-link schemes (`exec:` for
/// commands, `help:` for topics); a `Title` / `SectionHeading` first span
/// gets a markdown heading prefix so the help markdown highlighter styles it.
/// Centre alignment is ignored here (DB.4 wires branding centring).
fn render_row(row: &DashboardRow) -> String {
    let mut body = String::new();
    for span in &row.spans {
        match &span.link {
            Some(target) => {
                body.push_str(&format!("[{}]({})", span.text, help_link_scheme(target)));
            }
            None => body.push_str(&span.text),
        }
    }
    match row.spans.first().map(|s| s.role) {
        Some(DashboardRole::Title) => format!("# {body}"),
        Some(DashboardRole::SectionHeading) => format!("## {body}"),
        _ => body,
    }
}

/// Map a dashboard [`LinkTarget`] to the help-link `scheme:value` form the
/// follow handler consumes. These scheme names MUST match
/// `lattice_help::classify_link_url` exactly, or the seeded link classifies
/// as `Unresolved` and the `<CR>`-follow is a silent no-op.
///
/// - `exec:CMD` → `HelpLinkTarget::Execute` → runs `:CMD` (so a dashboard
///   `LinkTarget::Command("tutor")` actually STARTS the tutor, not describes
///   it — every dashboard command link is an action to run).
/// - `help:TOPIC` → `HelpLinkTarget::Topic` → opens the `:help TOPIC` page.
///
/// A URL renders verbatim; `classify_link_url` maps a real `scheme://…`
/// (or `mailto:`) form to `HelpLinkTarget::Url`, which follow-link opens
/// via the OS handler (default browser / app).
fn help_link_scheme(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Command(cmd) => format!("exec:{cmd}"),
        LinkTarget::Topic(topic) => format!("help:{topic}"),
        LinkTarget::Url(url) => url.clone(),
    }
}

/// Convert composed fragments into a `HelpContent` titled `*dashboard*`
/// (the title becomes the buffer name). Sections are separated by a blank
/// spacer line.
fn dashboard_fragments_to_help_content(fragments: Vec<DashboardFragment>) -> lattice_help::HelpContent {
    let mut lines: Vec<String> = Vec::new();
    for (i, fragment) in fragments.iter().enumerate() {
        if i > 0 {
            lines.push(String::new());
        }
        for row in &fragment.rows {
            lines.push(render_row(row));
        }
    }
    lattice_help::HelpContent::from_lines("*dashboard*", lines)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_dashboard::{DashboardRow, DashboardSpan};

    #[test]
    fn command_link_becomes_exec_scheme() {
        // `exec:` (not `execute:`) — the scheme `classify_link_url`
        // recognizes as "run this ex-command". `execute:` classifies as
        // Unresolved, i.e. a dead link.
        let row = DashboardRow::line("Run ", DashboardRole::Body)
            .push(DashboardSpan::link(":tutor", LinkTarget::Command("tutor".into())));
        assert_eq!(render_row(&row), "Run [:tutor](exec:tutor)");
    }

    #[test]
    fn topic_link_becomes_help_scheme() {
        // `help:` (not `topic:`) — the scheme `classify_link_url` maps to
        // `HelpLinkTarget::Topic`, opening the `:help` page.
        let row = DashboardRow::line("", DashboardRole::Body)
            .push(DashboardSpan::link(":help modes", LinkTarget::Topic("modes".into())));
        assert_eq!(render_row(&row), "[:help modes](help:modes)");
    }

    #[test]
    fn title_and_heading_get_markdown_prefixes() {
        assert_eq!(
            render_row(&DashboardRow::line("Lattice", DashboardRole::Title)),
            "# Lattice"
        );
        assert_eq!(
            render_row(&DashboardRow::line("About", DashboardRole::SectionHeading)),
            "## About"
        );
    }
}
