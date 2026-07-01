//! The eight built-in dashboard sections (native Rust — the built-in surface
//! stays native, like the vim grammar). Each is a small pure renderer; the
//! branding art and custom theme roles are layered on in later slices, but
//! the text content and roles are real here.
//!
//! [`builtin_registry`] returns a [`DashboardRegistry`] with all eight
//! registered in their default order.

use std::sync::Arc;

use crate::fragment::{DashboardFragment, DashboardRole, DashboardRow, DashboardSpan, LinkTarget};
use crate::registry::DashboardRegistry;
use crate::section::{DashboardCtx, DashboardSection};

/// The canonical GitHub repository.
const REPO_URL: &str = "https://github.com/dhruvasagar/lattice";
/// The one-line tagline (matches the brand assets).

/// Build a registry pre-loaded with every built-in section.
///
/// Note: the brand mark + wordmark are NOT a document section — they render
/// as the DB.4 branding virtual-row block above the body (see
/// [`crate::branding`]). The body starts with `about`.
pub fn builtin_registry() -> DashboardRegistry {
    let mut reg = DashboardRegistry::new();
    reg.register(Arc::new(About));
    reg.register(Arc::new(Links));
    reg.register(Arc::new(GettingStarted));
    reg.register(Arc::new(Tutor));
    reg.register(Arc::new(HelpAndBindings));
    reg.register(Arc::new(Describe));
    reg.register(Arc::new(HelpTopics));
    reg
}

/// Convenience: a heading row followed by a blank line separator.
fn heading(frag: &mut DashboardFragment, text: &str) {
    frag.push(DashboardRow::line(text, DashboardRole::SectionHeading));
}

/// Convenience: a body line that ends in a followable link.
///
/// e.g. `body_link("Open the interactive tutorial: ", ":tutor", cmd:tutor)`.
fn body_link(prefix: &str, label: &str, target: LinkTarget) -> DashboardRow {
    DashboardRow::line(prefix, DashboardRole::Body).push(DashboardSpan::link(label, target))
}

// ---------------------------------------------------------------------------
// about
// ---------------------------------------------------------------------------

/// Identity + the four paramount goals.
struct About;

impl DashboardSection for About {
    fn id(&self) -> &str {
        "about"
    }
    fn order(&self) -> i32 {
        10
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "About");
        f.line(
            "Lattice combines vim's modal editing with emacs's extensibility",
            DashboardRole::Body,
        );
        f.line(
            "on a non-blocking, multi-threaded, GPU-accelerated core.",
            DashboardRole::Body,
        );
        f.blank();
        f.line("Paramount goals:", DashboardRole::Body);
        f.line("  1. Performance — imperceptible keystroke latency", DashboardRole::Body);
        f.line("  2. Extensibility — WebAssembly plugins from day one", DashboardRole::Body);
        f.line("  3. Extensible vim modal editing", DashboardRole::Body);
        f.line("  4. Asynchronicity — nothing blocks the UI", DashboardRole::Body);
        f
    }
}

// ---------------------------------------------------------------------------
// links
// ---------------------------------------------------------------------------

/// External links (repo, docs).
struct Links;

impl DashboardSection for Links {
    fn id(&self) -> &str {
        "links"
    }
    fn order(&self) -> i32 {
        20
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "Links");
        f.push(body_link(
            "GitHub    ",
            REPO_URL,
            LinkTarget::Url(REPO_URL.to_string()),
        ));
        f.push(body_link(
            "Issues    ",
            "report a bug",
            LinkTarget::Url(format!("{REPO_URL}/issues")),
        ));
        f
    }
}

// ---------------------------------------------------------------------------
// getting-started
// ---------------------------------------------------------------------------

/// First steps.
struct GettingStarted;

impl DashboardSection for GettingStarted {
    fn id(&self) -> &str {
        "getting-started"
    }
    fn order(&self) -> i32 {
        30
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "Getting started");
        f.push(
            DashboardRow::line("Open a file        ", DashboardRole::Body)
                .push(DashboardSpan::new(":e <path>", DashboardRole::Key)),
        );
        f.push(
            DashboardRow::line("Command line       ", DashboardRole::Body)
                .push(DashboardSpan::new(":", DashboardRole::Key)),
        );
        f.push(
            DashboardRow::line("Back to Normal     ", DashboardRole::Body)
                .push(DashboardSpan::new("<Esc>", DashboardRole::Key)),
        );
        f.push(
            DashboardRow::line("Quit               ", DashboardRole::Body)
                .push(DashboardSpan::new(":q", DashboardRole::Key)),
        );
        f
    }
}

// ---------------------------------------------------------------------------
// tutor
// ---------------------------------------------------------------------------

/// Pointer to the interactive tutor.
struct Tutor;

impl DashboardSection for Tutor {
    fn id(&self) -> &str {
        "tutor"
    }
    fn order(&self) -> i32 {
        40
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "Learn");
        f.push(body_link(
            "Interactive lessons  ",
            ":tutor",
            LinkTarget::Command("tutor".to_string()),
        ));
        f
    }
}

// ---------------------------------------------------------------------------
// help-and-bindings
// ---------------------------------------------------------------------------

/// Pointers to help + key-binding discovery.
struct HelpAndBindings;

impl DashboardSection for HelpAndBindings {
    fn id(&self) -> &str {
        "help-and-bindings"
    }
    fn order(&self) -> i32 {
        50
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "Help & key bindings");
        f.push(body_link(
            "Help browser         ",
            ":help",
            LinkTarget::Command("help".to_string()),
        ));
        f.push(body_link(
            "What does a key do?  ",
            ":describe-key",
            LinkTarget::Command("describe-key".to_string()),
        ));
        f.push(body_link(
            "All key bindings     ",
            ":keymap",
            LinkTarget::Command("keymap".to_string()),
        ));
        f
    }
}

// ---------------------------------------------------------------------------
// describe
// ---------------------------------------------------------------------------

/// The `:describe-*` introspection family.
struct Describe;

impl DashboardSection for Describe {
    fn id(&self) -> &str {
        "describe"
    }
    fn order(&self) -> i32 {
        60
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "Introspection (:describe-*)");
        for (cmd, what) in [
            ("describe-key", "what a key is bound to"),
            ("describe-command", "what a command does"),
            ("describe-option", "an option's type and value"),
            ("describe-mode", "the active modes"),
            ("apropos", "search everything by keyword"),
        ] {
            f.push(
                body_link(
                    "",
                    &format!(":{cmd}"),
                    LinkTarget::Command(cmd.to_string()),
                )
                .push(DashboardSpan::new(
                    format!("  — {what}"),
                    DashboardRole::Hint,
                )),
            );
        }
        f
    }
}

// ---------------------------------------------------------------------------
// help-topics
// ---------------------------------------------------------------------------

/// Entry points into `:help <topic>`.
struct HelpTopics;

impl DashboardSection for HelpTopics {
    fn id(&self) -> &str {
        "help-topics"
    }
    fn order(&self) -> i32 {
        70
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "Help topics");
        for topic in ["getting-started", "modes", "commands", "config"] {
            f.push(body_link(
                "",
                &format!(":help {topic}"),
                LinkTarget::Topic(topic.to_string()),
            ));
        }
        f
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::SectionSelection;

    #[test]
    fn builtin_registry_sections_in_order() {
        // Branding is a virtual-row block (DB.4), not a document section.
        let reg = builtin_registry();
        let ids: Vec<String> = reg
            .ordered(&SectionSelection::Default)
            .iter()
            .map(|s| s.id().to_string())
            .collect();
        assert_eq!(
            ids,
            [
                "about",
                "links",
                "getting-started",
                "tutor",
                "help-and-bindings",
                "describe",
                "help-topics",
            ]
        );
    }

    #[test]
    fn every_builtin_renders_non_empty() {
        let reg = builtin_registry();
        let ctx = DashboardCtx::default();
        for section in reg.ordered(&SectionSelection::Default) {
            let frag = section.render(&ctx);
            assert!(
                !frag.is_empty(),
                "section {} rendered an empty fragment",
                section.id()
            );
        }
    }

    #[test]
    fn links_section_carries_followable_links() {
        let reg = builtin_registry();
        let ctx = DashboardCtx::default();
        let links = reg
            .ordered(&SectionSelection::Explicit(vec!["links".into()]))
            .into_iter()
            .next()
            .unwrap();
        let frag = links.render(&ctx);
        let has_link = frag
            .rows
            .iter()
            .flat_map(|r| &r.spans)
            .any(|s| matches!(&s.link, Some(LinkTarget::Url(u)) if u.contains("github.com")));
        assert!(has_link, "links section should carry a GitHub url link");
    }

    #[test]
    fn tutor_section_links_to_tutor_command() {
        let reg = builtin_registry();
        let frag = reg
            .ordered(&SectionSelection::Explicit(vec!["tutor".into()]))
            .into_iter()
            .next()
            .unwrap()
            .render(&DashboardCtx::default());
        let has_cmd = frag
            .rows
            .iter()
            .flat_map(|r| &r.spans)
            .any(|s| matches!(&s.link, Some(LinkTarget::Command(c)) if c == "tutor"));
        assert!(has_cmd, "tutor section should link to cmd:tutor");
    }
}
