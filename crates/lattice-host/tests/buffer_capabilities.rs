//! TC.9 — the mode-capability gate, both halves.
//!
//! `ModeRegistry` has always enforced `required_capabilities() - buffer_caps`,
//! but nothing ever populated the buffer side: every activation site passed
//! `CapabilitySet::empty()`, and no native mode had declared a requirement, so
//! the enforcement half had never once been exercised. The consequence was
//! that ANY mode declaring ANY capability was unsatisfiable — and it announced
//! itself as a `warn` at activation, not as anything that fails a build or a
//! test. `treesitter-context-mode` hit exactly that and had to declare nothing.
//!
//! These tests pin both directions. A mode that requires more than the buffer
//! offers must still be refused (the gate is not being weakened), and a mode
//! whose requirement the buffer DOES meet must activate (the gate is no longer
//! a wall).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::{
    ActivationPolicy, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
};

/// A minor that declares a capability requirement — the thing no native mode
/// had ever done, which is why the gate's buffer half was never noticed.
struct RequiringMode {
    id: ModeId,
    required: CapabilitySet,
}

impl Mode for RequiringMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        self.id
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }
    fn required_capabilities(&self) -> CapabilitySet {
        self.required
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn register(editor: &Editor, id: &'static str, required: CapabilitySet) -> ModeId {
    let id = ModeId::new(id);
    let mut next = (**editor.mode_registry.load()).clone();
    next.register(RequiringMode { id, required }).unwrap();
    editor.mode_registry.store(Arc::new(next));
    id
}

/// A buffer with a real, parsed tree-sitter handle — the state that should
/// grant `TREE_SITTER`.
fn editor_with_a_parse() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {\n    let x = 1;\n}\n"));
    let mut syn = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
        .unwrap()
        .unwrap();
    syn.parse("fn main() {\n    let x = 1;\n}\n");
    editor.syntax = Some(lattice_syntax::SyntaxHandle::seeded(syn));
    editor
}

#[test]
fn a_parsed_buffer_offers_tree_sitter() {
    let editor = editor_with_a_parse();
    let caps = editor.buffer_capabilities(editor.document_buffer_id);
    assert!(
        caps.contains(CapabilitySet::TREE_SITTER),
        "a buffer with a completed parse offers TREE_SITTER: {caps:?}"
    );
}

/// The gate must still bite. A buffer with no parser offers no `TREE_SITTER`,
/// and the mode is refused — this is the half that already worked, asserted so
/// the fix cannot be "grant everything to everyone", which would pass every
/// other test here.
#[test]
fn an_unparsed_buffer_does_not_offer_tree_sitter() {
    let editor = Editor::boot(CoreDocument::from_text("plain text\n"));
    let caps = editor.buffer_capabilities(editor.document_buffer_id);
    assert!(
        !caps.contains(CapabilitySet::TREE_SITTER),
        "no parser attached, so no TREE_SITTER: {caps:?}"
    );
}

/// A mode declaring a requirement the buffer MEETS must activate. This is the
/// report: `treesitter-context-mode` declaring `TREE_SITTER` failed on every
/// buffer, because the buffer side was hardcoded empty.
#[test]
fn a_mode_requiring_a_capability_the_buffer_has_activates() {
    let mut editor = editor_with_a_parse();
    let id = register(&editor, "ts-requiring-mode", CapabilitySet::TREE_SITTER);
    let buffer = editor.document_buffer_id;

    let _ = editor.activate_mode_by_id(buffer, id);

    assert!(
        editor.minor_mode_enabled_for(buffer, id),
        "the buffer offers TREE_SITTER, so the mode must be active — this \
         failing is the original report, one layer down"
    );
}

/// ... and one requiring a capability the buffer LACKS is still refused, with
/// the mode left inactive rather than half-activated.
#[test]
fn a_mode_requiring_a_capability_the_buffer_lacks_is_refused() {
    let mut editor = Editor::boot(CoreDocument::from_text("plain text\n"));
    let id = register(&editor, "lsp-requiring-mode", CapabilitySet::LSP);
    let buffer = editor.document_buffer_id;

    let _ = editor.activate_mode_by_id(buffer, id);

    assert!(
        !editor.minor_mode_enabled_for(buffer, id),
        "no LSP attached, so the gate refuses"
    );
}

/// TC.9b: a requirement that is not met YET must be retried, not dropped.
///
/// This is the timing half, and it is the half that actually bites in the
/// editor. A buffer opens, its modes activate, and its first parse has not
/// run — so a mode requiring `TREE_SITTER` is refused at exactly the moment
/// every mode is activated, and nothing ever asks again. The user sees a mode
/// that is simply never on, with a `warn` scrolled past several seconds
/// earlier as the only trace.
///
/// The parse landing is simulated by swapping in a parsed handle: the retry
/// path does not care HOW the capability arrived, only that the buffer's set
/// gained a bit it was waiting for.
#[test]
fn a_requirement_met_later_activates_without_a_keypress() {
    // A handle with no completed parse — `snapshot().tree()` is `None`.
    let mut editor = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let unparsed = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
        .unwrap()
        .unwrap();
    editor.syntax = Some(lattice_syntax::SyntaxHandle::seeded(unparsed));
    let buffer = editor.document_buffer_id;
    assert!(
        !editor
            .buffer_capabilities(buffer)
            .contains(CapabilitySet::TREE_SITTER),
        "precondition: the parse has not run"
    );

    let id = register(&editor, "deferred-ts-mode", CapabilitySet::TREE_SITTER);
    let _ = editor.activate_mode_by_id(buffer, id);
    assert!(
        !editor.minor_mode_enabled_for(buffer, id),
        "refused for now — the buffer genuinely cannot satisfy it yet"
    );

    // The parse lands.
    let mut parsed = lattice_syntax::Syntax::for_language(lattice_syntax::Lang::Rust)
        .unwrap()
        .unwrap();
    parsed.parse("fn main() {}\n");
    editor.syntax = Some(lattice_syntax::SyntaxHandle::seeded(parsed));

    // The same pump every other async arrival reaches the screen through. No
    // action is dispatched — asserting after a keypress would pass on the
    // broken version too, which is the whole point.
    editor.run_tick_pending();

    assert!(
        editor.minor_mode_enabled_for(buffer, id),
        "the capability arrived, so the deferred activation must fire — \
         without the user pressing anything"
    );
}

/// A requirement that never arrives must not retry forever, and must not
/// resurrect after the user turns the mode off.
#[test]
fn a_deferred_activation_is_dropped_once_the_user_deactivates_it() {
    let mut editor = Editor::boot(CoreDocument::from_text("plain\n"));
    let id = register(&editor, "never-satisfied-mode", CapabilitySet::LSP);
    let buffer = editor.document_buffer_id;

    let _ = editor.activate_mode_by_id(buffer, id);
    assert!(!editor.minor_mode_enabled_for(buffer, id));

    // Asking for it to be off cancels the pending request: a deferred
    // activation that outlives an explicit "no" would switch the mode on
    // later, which is the opposite of what the user said.
    editor.cancel_deferred_activation(buffer, id);
    editor.run_tick_pending();
    assert!(!editor.minor_mode_enabled_for(buffer, id));
    assert_eq!(editor.deferred_activation_count(), 0);
}
