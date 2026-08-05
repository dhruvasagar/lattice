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
// The one-line tagline (matches the brand assets).

/// Build a registry pre-loaded with every built-in section.
///
/// Note: the brand mark + wordmark are NOT a document section — they render
/// as the DB.4 branding virtual-row block above the body (see
/// [`crate::branding`]). The body starts with `about`.
pub fn builtin_registry() -> DashboardRegistry {
    let mut reg = DashboardRegistry::new();
    reg.register(Arc::new(About));
    reg.register(Arc::new(Links));
    reg.register(Arc::new(Tutor));
    reg.register(Arc::new(Commands));
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
        f.line(
            "  1. Performance — imperceptible keystroke latency",
            DashboardRole::Body,
        );
        f.line(
            "  2. Extensibility — WebAssembly plugins from day one",
            DashboardRole::Body,
        );
        f.line("  3. Extensible vim modal editing", DashboardRole::Body);
        f.line(
            "  4. Asynchronicity — nothing blocks the UI",
            DashboardRole::Body,
        );
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
        // Each entry opens the command's own help page (`:describe-command
        // <cmd>`) rather than *running* the bare command — several of these
        // (`describe-key`, `describe-option`) would otherwise sit waiting
        // for an interactive argument. `:describe-command` is always
        // available (every command is self-documenting), so none of these
        // are dead links. The `commands` section below is where the
        // click-to-run entries live.
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
                    LinkTarget::Command(format!("describe-command {cmd}")),
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
// commands
// ---------------------------------------------------------------------------

/// Commonly useful commands, run on click. Unlike the introspection
/// section (which opens help *about* each command), these entries invoke
/// the command directly — they are safe, non-destructive discovery tools.
struct Commands;

impl DashboardSection for Commands {
    fn id(&self) -> &str {
        "commands"
    }
    fn order(&self) -> i32 {
        45
    }
    fn render(&self, _ctx: &DashboardCtx) -> DashboardFragment {
        let mut f = DashboardFragment::new();
        heading(&mut f, "Commonly useful commands");
        for (cmd, what) in [
            ("files", "fuzzy-find files in the project"),
            ("buffers", "switch buffers (fuzzy picker)"),
            ("recent", "reopen a recently edited file"),
            ("marks", "jump to a mark"),
            ("registers", "inspect register contents"),
        ] {
            f.push(
                body_link("", &format!(":{cmd}"), LinkTarget::Command(cmd.to_string())).push(
                    DashboardSpan::new(format!("  — {what}"), DashboardRole::Hint),
                ),
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
        // Topic names must match a registered `:help` topic (the doc's
        // file stem under `docs/user/`), or the `<CR>`-follow is a dead
        // link. `commands`/`config` were such dead links — the actual
        // docs are `ex-commands.md` and `options.md`.
        for topic in ["getting-started", "modes", "ex-commands", "options"] {
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
                "tutor",
                "commands",
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

    /// Collect the `LinkTarget::Command` payloads from a section's fragment.
    fn command_links(section_id: &str) -> Vec<String> {
        let reg = builtin_registry();
        reg.ordered(&SectionSelection::Explicit(vec![section_id.into()]))
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("section {section_id} missing"))
            .render(&DashboardCtx::default())
            .rows
            .iter()
            .flat_map(|r| &r.spans)
            .filter_map(|s| match &s.link {
                Some(LinkTarget::Command(c)) => Some(c.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn describe_section_links_to_help_pages_not_bare_commands() {
        // Introspection entries open each command's help page
        // (`:describe-command <cmd>`), not the bare command — so clicking
        // `describe-key` explains it instead of waiting for a keypress.
        let cmds = command_links("describe");
        assert!(!cmds.is_empty(), "describe section should carry links");
        assert!(
            cmds.iter().all(|c| c.starts_with("describe-command ")),
            "all introspection links must open help pages: {cmds:?}"
        );
        assert!(
            cmds.iter().any(|c| c == "describe-command describe-key"),
            "expected a help-page link for describe-key: {cmds:?}"
        );
    }

    #[test]
    fn commands_section_runs_useful_commands() {
        // The commands section invokes each command directly (safe,
        // non-destructive discovery tools).
        let cmds = command_links("commands");
        for expect in ["files", "buffers", "recent", "marks", "registers"] {
            assert!(
                cmds.iter().any(|c| c == expect),
                "missing :{expect} run-link in {cmds:?}"
            );
        }
    }
}
