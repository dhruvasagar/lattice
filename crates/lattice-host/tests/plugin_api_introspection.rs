//! PI.2: the plugin-API introspection ex-commands render the
//! `lattice-plugin-api` catalog through the shared help spine.
//!
//! `:describe-plugin-api [<seam>]` / `:list-plugin-apis` / the `:apropos`
//! extension all produce a `HelpContent` host-side; these tests exercise the
//! content builders directly via a booted `Editor` (the emacs-dispatch harness
//! precedent). They assert on the rendered buffer text + anchors, so a
//! regression in the catalog → help rendering is caught.

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn editor() -> Editor {
    Editor::boot(CoreDocument::from_text("fn main() {}\n"))
}

fn text(content: &lattice_help::HelpContent) -> String {
    content.buffer.content.as_string()
}

/// True if the help content carries an `exec:`-link running `:cmdline`. Links
/// are stripped from the visible buffer text into `metadata.links`, so an
/// exec-link is asserted here, not via `text(...)`.
fn has_exec_link(content: &lattice_help::HelpContent, cmdline: &str) -> bool {
    content
        .metadata
        .links
        .iter()
        .any(|l| matches!(&l.target, lattice_help::HelpLinkTarget::Execute(c) if c == cmdline))
}

#[test]
fn list_plugin_apis_lists_every_seam() {
    let ed = editor();
    let content = ed.build_list_plugin_apis_content();
    let body = text(&content);
    // The header + a couple of load-bearing seams appear, each as a runnable
    // `:describe-plugin-api <seam>` exec-link.
    assert!(body.contains("Plugin API"), "header missing:\n{body}");
    assert!(body.contains("host-services"), "host-services row missing");
    assert!(body.contains("picker-source"), "picker-source row missing");
    assert!(
        has_exec_link(&content, "describe-plugin-api host-services"),
        "seam should link to :describe-plugin-api"
    );
    // Capability/direction short labels render (host-services is fs + imports).
    assert!(body.contains("imports"), "direction label missing");
    assert!(body.contains("fs"), "capability label missing");
}

#[test]
fn describe_plugin_api_renders_one_seam_via_the_spine() {
    let mut ed = editor();
    let content = ed
        .build_describe_plugin_api_content(Some("host-services"))
        .expect("host-services is a real seam");
    let body = text(&content);
    // Uniform `Introspectable` heading + the two extra sections.
    assert!(body.contains("host-services  (plugin-api)"), "heading:\n{body}");
    assert!(body.contains("capability:  filesystem"), "capability prose");
    assert!(
        body.contains("guest calls into the host"),
        "direction prose"
    );
    assert!(body.contains("walk"), "the `walk` function is listed");
    // The `Introspectable` extra-section anchors are recorded for scroll-to.
    let anchors: Vec<&str> = content
        .metadata
        .anchors
        .iter()
        .map(|a| a.name.as_str())
        .collect();
    assert!(anchors.contains(&"seam"), "seam anchor: {anchors:?}");
    assert!(anchors.contains(&"functions"), "functions anchor: {anchors:?}");
}

#[test]
fn describe_unknown_seam_returns_none() {
    let mut ed = editor();
    assert!(
        ed.build_describe_plugin_api_content(Some("no-such-seam"))
            .is_none(),
        "an unknown seam must not open a help buffer (echoes an error instead)"
    );
}

#[test]
fn describe_with_no_seam_delegates_to_the_list() {
    let mut ed = editor();
    let content = ed
        .build_describe_plugin_api_content(None)
        .expect("no-seam form renders the full list");
    assert!(text(&content).contains("Plugin API"));
}

#[test]
fn apropos_surfaces_plugin_api_seams() {
    let mut ed = editor();
    let content = ed
        .build_apropos_content("picker")
        .expect("non-empty pattern");
    let body = text(&content);
    // The `picker-source` seam appears with the `plugin-api` kind and an
    // exec-link (not a `:describe-command` link).
    assert!(body.contains("picker-source"), "seam missing:\n{body}");
    assert!(body.contains("plugin-api"), "plugin-api kind column missing");
    assert!(
        has_exec_link(&content, "describe-plugin-api picker-source"),
        "plugin-api hit should link to :describe-plugin-api"
    );
}

#[test]
fn export_plugin_api_json_opens_a_savable_buffer() {
    let mut ed = editor();
    ed.do_export_plugin_api(Some("json"));
    // The export buffer is activated, so `active_text` is the dump.
    let body = ed.active_text().as_string();
    assert!(body.contains("\"seams\""), "json root missing:\n{body}");
    assert!(body.contains("\"name\": \"host-services\""), "seam entry missing");
    assert!(body.contains("\"capability\": \"fs\""), "capability token missing");
    assert!(body.contains("\"walk\""), "function missing");
}

#[test]
fn export_plugin_api_defaults_to_markdown() {
    let mut ed = editor();
    ed.do_export_plugin_api(None);
    let body = ed.active_text().as_string();
    assert!(body.contains("# Lattice Plugin API"), "md header:\n{body}");
    assert!(body.contains("## host-services"), "md seam section");
}

#[test]
fn re_export_replaces_rather_than_appends() {
    let mut ed = editor();
    ed.do_export_plugin_api(Some("json"));
    let len1 = ed.active_text().as_string().len();
    // The buffer is reused by name, so a second export must overwrite.
    ed.do_export_plugin_api(Some("json"));
    let len2 = ed.active_text().as_string().len();
    assert_eq!(len1, len2, "re-export must replace, not double the content");
}
