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
    DashboardCtx, DashboardFragment, DashboardRegistry, DashboardRole, DashboardRow, DashboardSource,
    LinkTarget, SectionSelection,
};

use crate::dispatch::RendererSignal;
use crate::editor::Editor;

impl Editor {
    /// Open (or re-compose + activate) the `*dashboard*` buffer. Idempotent:
    /// a second call re-seeds the existing buffer in place rather than
    /// creating a duplicate.
    pub fn do_open_dashboard(&mut self) -> Vec<RendererSignal> {
        let content = self.build_dashboard_content();
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
        if self.activate_buffer(id) {
            self.activate_buffer_state()
        } else {
            Vec::new()
        }
    }

    /// Compose the enabled sections (config-selected) into a `HelpContent`.
    /// A `dashboard.source` override is handled in DB.6; DB.2 composes the
    /// built-in sections only.
    fn build_dashboard_content(&self) -> lattice_help::HelpContent {
        let selection = self
            .config
            .get_typed::<lattice_dashboard::DashboardSections>()
            .map(|raw| SectionSelection::parse(raw.as_str()))
            .unwrap_or(SectionSelection::Default);

        // DB.2: pane_width / nerd_fonts are placeholders until DB.4 (branding
        // centring) wires the live viewport + config; the built-in sections
        // do not yet consume them.
        let _ = self.config.get_typed::<DashboardSource>();
        let ctx = DashboardCtx {
            pane_width: 80,
            nerd_fonts: false,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };

        let fragments = match self.services.get::<DashboardRegistry>() {
            Some(registry) => registry.compose(&ctx, &selection),
            None => {
                tracing::warn!("dashboard registry service missing; rendering an empty dashboard");
                Vec::new()
            }
        };
        dashboard_fragments_to_help_content(fragments)
    }
}

/// Render one dashboard row to a markdown line. Link spans become
/// `[label](scheme:value)` using the help-link schemes (`execute:` for
/// commands, `topic:` for topics); a `Title` / `SectionHeading` first span
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
/// follow handler consumes. `execute:` runs an ex-command; `topic:` opens a
/// help topic. A URL has no external opener in help-follow yet, so it renders
/// as a plain URL (classified as `Unresolved` — a logged no-op on `<CR>`).
fn help_link_scheme(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Command(cmd) => format!("execute:{cmd}"),
        LinkTarget::Topic(topic) => format!("topic:{topic}"),
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
    fn command_link_becomes_execute_scheme() {
        let row = DashboardRow::line("Run ", DashboardRole::Body)
            .push(DashboardSpan::link(":tutor", LinkTarget::Command("tutor".into())));
        assert_eq!(render_row(&row), "Run [:tutor](execute:tutor)");
    }

    #[test]
    fn topic_link_becomes_topic_scheme() {
        let row = DashboardRow::line("", DashboardRole::Body)
            .push(DashboardSpan::link(":help modes", LinkTarget::Topic("modes".into())));
        assert_eq!(render_row(&row), "[:help modes](topic:modes)");
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
