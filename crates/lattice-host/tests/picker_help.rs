//! PH.1 — `<C-h>` in a picker opens that picker's own help page.
//!
//! Design: [`docs/dev/architecture/picker.md`](../../../docs/dev/architecture/picker.md)
//! §4.2quinquies. Slice plan: `slice-plans/picker-help.md` PH.1.
//!
//! ## What is at risk
//!
//! The resolution has three rungs — the topic the source DECLARES, then the
//! `picker-<id>` convention a plugin can meet without a spec field, then the
//! general `picker` page — and each rung can be skipped by a wiring that looks
//! right from the others. The fallback is the dangerous one: a build that
//! always opened `picker` would pass a "help opened" test while every
//! dedicated page stayed unreachable, so each rung asserts the TITLE of the
//! page that opened, not merely that one did.
//!
//! Every press goes through `dispatch_chord`, so the key binding is under test
//! and not just the handler.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::action::EchoLevel;
use lattice_host::dispatch::RendererSignal;
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{PickerAcceptOutcome, PickerContext, RoutingPayload, SourceResult};
use lattice_protocol::KeyChord;

const DECLARED: &str = "ph1-declared";
const CONVENTIONAL: &str = "ph1-conventional";
const UNDOCUMENTED: &str = "ph1-undocumented";
const MISDECLARED: &str = "ph1-misdeclared";
/// Owned by plugin 7, which registers its page through the namespaced seam.
const PLUGIN_OWNED: &str = "ph1-plugin-owned";
/// Owned by plugin 7 too — but only plugin 8 registered a page for it.
const PLUGIN_HIJACKED: &str = "ph1-plugin-hijacked";

struct EmptySource {
    spec: PickerSourceSpec,
    /// Stands in for `WasmPickerSource::owner_plugin`.
    owner: Option<u64>,
}

impl PickerSourceGenerator for EmptySource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        Ok(PickerInitResult::Inline(Vec::new()))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        Err("PH.1 fixture never accepts".to_string())
    }

    fn owner_plugin(&self) -> Option<u64> {
        self.owner
    }
}

/// Register a topic exactly as the plugin help seam does: namespaced name,
/// owning plugin recorded.
fn register_plugin_topic(editor: &Editor, name: String, plugin_id: u64) {
    editor.help_topics.rcu(|current| {
        let mut next = (**current).clone();
        next.register(lattice_help::topics::HelpTopic {
            name: name.clone(),
            summary: "PH.1 plugin fixture page.".to_string(),
            body: lattice_help::topics::HelpTopicBody::Owned("# plugin page\n".to_string()),
            related_command_patterns: Vec::new(),
            plugin_id: Some(plugin_id),
        });
        Arc::new(next)
    });
}

fn boot() -> Editor {
    let editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let mut pickers = (**editor.picker_registry.load()).clone();
    for spec in [
        // Declares a topic that exists: `oil-mode` is a builtin page, and any
        // builtin would do — what matters is that it is NOT `picker-<id>`, so
        // only the declaration can have found it.
        PickerSourceSpec::no_args(DECLARED, "PH.1: declares its topic.")
            .with_help_topic("oil-mode"),
        PickerSourceSpec::no_args(CONVENTIONAL, "PH.1: meets the convention."),
        PickerSourceSpec::no_args(UNDOCUMENTED, "PH.1: has no page at all."),
        PickerSourceSpec::no_args(MISDECLARED, "PH.1: declares a page that is not there.")
            .with_help_topic("ph1-no-such-page"),
    ] {
        pickers.register_generator(Arc::new(EmptySource { spec, owner: None }));
    }
    for id in [PLUGIN_OWNED, PLUGIN_HIJACKED] {
        pickers.register_generator(Arc::new(EmptySource {
            spec: PickerSourceSpec::no_args(id, "PH.1: a plugin's source."),
            owner: Some(7),
        }));
    }
    editor.picker_registry.store(Arc::new(pickers));
    register_plugin_topic(&editor, format!("fixture.picker-{PLUGIN_OWNED}"), 7);
    register_plugin_topic(&editor, format!("impostor.picker-{PLUGIN_HIJACKED}"), 8);

    // The convention's page, registered the way a plugin's help seam does it.
    editor.help_topics.rcu(|current| {
        let mut next = (**current).clone();
        next.register(lattice_help::topics::HelpTopic {
            name: format!("picker-{CONVENTIONAL}"),
            summary: "PH.1 fixture page.".to_string(),
            body: lattice_help::topics::HelpTopicBody::Owned(
                "# conventional\n\nKeys.\n".to_string(),
            ),
            related_command_patterns: Vec::new(),
            plugin_id: None,
        });
        Arc::new(next)
    });
    editor
}

/// `<C-h>` through the real chord path, returning the title of the help page
/// it opened (or `None`).
fn press_help(editor: &mut Editor) -> Option<String> {
    let mut partial: Vec<KeyChord> = Vec::new();
    let (_, outcome) = editor.dispatch_chord_with_outcome(KeyChord::ctrl('h'), &mut partial);
    outcome.renderer_signals.iter().find_map(|s| match s {
        RendererSignal::DisplayBuffer(req)
            if matches!(
                req.category,
                lattice_core::ui::display::BufferDisplayCategory::HelpTopic
            ) =>
        {
            Some(req.content.buffer.title.clone())
        }
        _ => None,
    })
}

fn echo(editor: &Editor) -> Option<(EchoLevel, String)> {
    editor
        .last_message
        .as_ref()
        .map(|m| (m.level, m.text.clone()))
}

#[test]
fn a_declared_topic_opens_and_the_picker_closes() {
    let mut editor = boot();
    let _ = editor.open_picker(DECLARED.to_string(), Vec::new());
    assert!(editor.picker.is_some(), "precondition: the picker seated");

    assert_eq!(press_help(&mut editor).as_deref(), Some("help oil-mode"));
    assert!(
        editor.picker.is_none(),
        "the picker closes: it is a modal overlay, and a help page opened \
         beneath one could not be scrolled or dismissed"
    );
}

#[test]
fn the_picker_id_convention_opens_without_a_declaration() {
    let mut editor = boot();
    let _ = editor.open_picker(CONVENTIONAL.to_string(), Vec::new());
    assert_eq!(
        press_help(&mut editor).as_deref(),
        Some(format!("help picker-{CONVENTIONAL}").as_str()),
        "a plugin that registers `picker-<id>` through its help seam is found \
         without a spec field crossing WIT"
    );
}

#[test]
fn an_undocumented_source_falls_back_to_the_general_page_and_says_so() {
    let mut editor = boot();
    let _ = editor.open_picker(UNDOCUMENTED.to_string(), Vec::new());
    assert_eq!(press_help(&mut editor).as_deref(), Some("help picker"));
    let (level, text) = echo(&editor).expect("the fallback is announced");
    assert_eq!(level, EchoLevel::Info);
    assert!(
        text.contains(UNDOCUMENTED),
        "the echo names the source, so the user knows the page is the general \
         one and not this picker's: {text}"
    );
}

#[test]
fn a_declared_topic_that_is_missing_is_a_warning_not_silence() {
    let mut editor = boot();
    let _ = editor.open_picker(MISDECLARED.to_string(), Vec::new());
    assert_eq!(
        press_help(&mut editor).as_deref(),
        Some("help picker"),
        "the general page still opens — the user asked for help"
    );
    let (level, text) = echo(&editor).expect("a broken declaration is announced");
    assert_eq!(level, EchoLevel::Warn);
    assert!(
        text.contains("ph1-no-such-page"),
        "the echo names the missing topic, the one thing that points at the \
         wiring bug: {text}"
    );
}

/// The builtin path end to end: `files` declares `picker-files` (PH.2), and
/// until that page exists this still has to land somewhere sensible.
#[test]
fn a_builtin_source_opens_a_help_page() {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let _ = editor.open_picker("buffers".to_string(), Vec::new());
    assert!(editor.picker.is_some(), "precondition: `buffers` seats");
    assert!(
        press_help(&mut editor).is_some(),
        "`<C-h>` in a builtin picker opens a help page"
    );
}

#[test]
fn ctrl_h_with_no_picker_open_is_not_picker_help() {
    let mut editor = boot();
    // Normal mode: `<C-h>` is the describe prefix there (`<C-h>k`), and must
    // not have been captured by the picker binding.
    let title = press_help(&mut editor);
    assert_ne!(title.as_deref(), Some("help picker"));
}

/// The plugin rung. A guest registers `picker-<id>`; the host namespaces it to
/// `<plugin>.picker-<id>`, so an exact-name lookup can never find it — which
/// is what PH.1 first shipped, with a comment claiming otherwise.
#[test]
fn a_plugin_source_finds_the_page_its_own_plugin_registered() {
    let mut editor = boot();
    let _ = editor.open_picker(PLUGIN_OWNED.to_string(), Vec::new());
    assert_eq!(
        press_help(&mut editor).as_deref(),
        Some(format!("help fixture.picker-{PLUGIN_OWNED}").as_str())
    );
}

/// Ownership, not the suffix: another plugin's `.picker-<id>` page must not
/// answer for a picker it does not own.
#[test]
fn another_plugins_page_does_not_answer_for_a_source_it_does_not_own() {
    let mut editor = boot();
    let _ = editor.open_picker(PLUGIN_HIJACKED.to_string(), Vec::new());
    assert_eq!(press_help(&mut editor).as_deref(), Some("help picker"));
}
