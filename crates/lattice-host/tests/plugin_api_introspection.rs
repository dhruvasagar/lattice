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
    // Hermetic boot: these tests assert on the loaded-plugin set, so they must
    // NOT pick up a developer's real `~/.config/lattice` (init.rs / on-disk
    // plugins). Disable boot-time auto-discovery process-wide before the first
    // boot spawns it (safe process-global flag — no env/unsafe).
    lattice_plugin_loader::disable_autoload();
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
    assert!(
        body.contains("host-services +"),
        "heading (kind_icon: plugin-api → +):\n{body}"
    );
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
    assert!(
        anchors.contains(&"functions"),
        "functions anchor: {anchors:?}"
    );
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
    assert!(
        body.contains("plugin-api"),
        "plugin-api kind column missing"
    );
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
    assert!(
        body.contains("\"name\": \"host-services\""),
        "seam entry missing"
    );
    assert!(
        body.contains("\"capability\": \"fs\""),
        "capability token missing"
    );
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

// --- PI.3: :list-commands + plugin-name provenance seam ---

#[test]
fn list_commands_groups_by_source() {
    let ed = editor();
    let body = text(&ed.build_list_commands_content());
    assert!(body.contains("# Commands ("), "header missing:\n{body}");
    // Every builtin command lands under the Built-in group.
    assert!(body.contains("## Built-in"), "Built-in group missing");
    // `:list-commands` enumerates itself (registered as `ex:list-commands`).
    assert!(body.contains("list-commands"), "should enumerate commands");
}

#[test]
fn plugin_name_seam_resolves_id_to_manifest_name() {
    let ed = editor();
    // Empty by default (no plugin loader yet) → falls back to <plugin:id>.
    assert!(ed.plugin_display_name(7).is_none());
    // The Phase-8 loader populates it via `register_plugin_name`.
    ed.register_plugin_name(7, "git-gutter");
    assert_eq!(ed.plugin_display_name(7).as_deref(), Some("git-gutter"));
    assert!(
        ed.plugin_display_name(99).is_none(),
        "unknown id stays None"
    );
}

// --- PI.4: :describe-plugin / :list-plugins (loaded-plugin introspection) ---

#[test]
fn list_plugins_empty_until_a_plugin_loads() {
    let ed = editor();
    let body = text(&ed.build_list_plugins_content());
    assert!(
        body.contains("# Plugins (0 loaded)"),
        "empty count:\n{body}"
    );
    assert!(body.contains("No plugins are loaded"), "empty-state line");
}

#[test]
fn describe_plugin_renders_registered_metadata_and_lists_it() {
    let mut ed = editor();
    // The Phase-8 loader would call this once at load, doc resolved from the
    // plugin's embedded WIT / manifest `doc`.
    ed.register_plugin(7, "git-gutter", "Shows git diff signs in the gutter.");

    let content = ed
        .build_describe_plugin_content("git-gutter")
        .expect("a registered plugin is describable");
    let body = text(&content);
    assert!(
        body.contains("git-gutter +"),
        "heading (kind_icon: plugin → +):\n{body}"
    );
    assert!(
        body.contains("Shows git diff signs in the gutter."),
        "the plugin's own doc renders"
    );

    // :list-plugins now shows it (with a :describe-plugin exec-link).
    let list = ed.build_list_plugins_content();
    let list_body = text(&list);
    assert!(list_body.contains("# Plugins (1 loaded)"));
    assert!(list_body.contains("git-gutter"));
    assert!(has_exec_link(&list, "describe-plugin git-gutter"));
}

#[test]
fn describe_unknown_plugin_returns_none() {
    let mut ed = editor();
    assert!(
        ed.build_describe_plugin_content("no-such-plugin").is_none(),
        "an unloaded plugin echoes an error, no help buffer"
    );
}

// --- PH7.8b: plugin-defined events surface in the event introspection ---

#[test]
fn plugin_defined_event_surfaces_in_describe_events() {
    use lattice_protocol::event_registry::{register_runtime_event, unregister_runtime_event};
    let mut ed = editor();
    let name = "test.host-describe-runtime-evt";
    assert!(register_runtime_event(
        name,
        "a custom plugin event",
        "plugin:demo"
    ));

    // :describe-events (plural) lists it beside built-ins.
    let all = text(&ed.build_describe_events_content());
    assert!(all.contains(name), "runtime event listed:\n{all}");

    // :describe-event <name> resolves + renders it as a plugin event.
    let one = ed
        .build_describe_event_content(name)
        .expect("a runtime event is describable");
    let body = text(&one);
    assert!(body.contains(name));
    assert!(body.contains("plugin"), "kind should read plugin:\n{body}");
    assert!(body.contains("a custom plugin event"));

    unregister_runtime_event(name);
}
