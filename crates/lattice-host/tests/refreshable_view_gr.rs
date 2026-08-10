//! RV.1 (2026-08-10): `gr` is ONE shared chord over
//! `Mode::refresh_action()`, not a per-mode copy.
//!
//! Design: `docs/dev/architecture/mode-architecture.md` §5.5.
//! Slice plan: `docs/dev/operations/slice-plans/refreshable-views.md`.
//!
//! The regression that matters most is the last one: `gr` in an
//! ordinary document buffer is **LSP references**, and the shared
//! refresh minor must never attach there. Everything else in RV.1 is
//! additive; that one would break a chord people use constantly.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;

use lattice_core::Document as CoreDocument;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::ActionSpec;
use lattice_host::editor::Editor;
use lattice_mode::{
    ActivationPolicy, LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind,
    RefreshableViewMode,
};

/// A stand-in view mode that declares a refresh target, like
/// `magit-core-mode` / `compilation-mode` / the search provider do.
struct DeclaringMode {
    id: ModeId,
    kind: ModeKind,
    action: &'static str,
}

impl Mode for DeclaringMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        self.id
    }
    fn kind(&self) -> ModeKind {
        self.kind
    }
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }
    fn refresh_action(&self) -> Option<&'static str> {
        Some(self.action)
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn register_mode(editor: &Editor, mode: DeclaringMode) -> ModeId {
    let id = <DeclaringMode as Mode>::id(&mode);
    let mut next = (**editor.mode_registry.load()).clone();
    next.register(mode).unwrap();
    editor.mode_registry.store(Arc::new(next));
    id
}

/// Register an action name so `resolve_refresh_action`'s name →
/// `CommandId` step resolves, the same way each owning crate registers
/// its real refresh action at boot.
fn register_action(editor: &Editor, name: &'static str) {
    let handle = editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .expect("command registry is registered at boot");
    let mut next = (**handle.load()).clone();
    next.register_action(
        name,
        "test refresh action",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
    handle.store(Arc::new(next));
}

fn command_id(editor: &Editor, name: &str) -> lattice_protocol::ids::CommandId {
    editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .unwrap()
        .load()
        .id_by_name(name)
        .unwrap()
}

#[test]
fn shared_refresh_minor_is_registered_at_boot() {
    let editor = Editor::boot(CoreDocument::from_text("hello\n"));
    assert!(
        editor
            .mode_registry
            .load()
            .is_registered(RefreshableViewMode::mode_id()),
        "the cascade can only pull in a mode that is registered"
    );
}

#[test]
fn view_refresh_action_resolves_at_boot() {
    let editor = Editor::boot(CoreDocument::from_text("hello\n"));
    assert!(
        editor
            .services
            .get::<lattice_grammar::CommandRegistryHandle>()
            .unwrap()
            .load()
            .id_by_name(lattice_mode::VIEW_REFRESH_ACTION)
            .is_some(),
        "`gr`'s command name must resolve or the binding never lands"
    );
}

/// THE regression guard. `gr` in an ordinary document is LSP
/// references; the shared refresh minor attaching there would shadow
/// it.
#[test]
fn plain_document_does_not_get_the_shared_refresh_minor() {
    let editor = Editor::boot(CoreDocument::from_text("fn main() {}\n"));
    let buf = editor.active_buffer_id();
    let active = editor.active_modes.get(&buf);
    if let Some(modes) = active {
        assert!(
            !modes.has_minor(RefreshableViewMode::mode_id()),
            "`gr` must stay LSP references on ordinary buffers"
        );
    }
    assert_eq!(
        editor.resolve_refresh_action(buf),
        None,
        "an ordinary document declares no refresh"
    );
}

#[test]
fn a_declaring_minor_resolves_to_its_own_action() {
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    register_action(&editor, "action:test-view-refresh");
    let id = register_mode(
        &editor,
        DeclaringMode {
            id: ModeId::new("test-view-mode"),
            kind: ModeKind::Minor,
            action: "action:test-view-refresh",
        },
    );
    let buf = editor.active_buffer_id();
    editor.activate_minor_by_id(buf, id);

    assert_eq!(
        editor.resolve_refresh_action(buf),
        Some(command_id(&editor, "action:test-view-refresh")),
        "the walk must find the declaring mode's own action"
    );
}

/// Most-specific-wins: a provider minor on a multibuffer must beat the
/// generic major underneath it.
#[test]
fn a_minor_declaration_beats_the_major() {
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    register_action(&editor, "action:major-refresh");
    register_action(&editor, "action:minor-refresh");
    let major = register_mode(
        &editor,
        DeclaringMode {
            id: ModeId::new("test-major-mode"),
            kind: ModeKind::Major,
            action: "action:major-refresh",
        },
    );
    let minor = register_mode(
        &editor,
        DeclaringMode {
            id: ModeId::new("test-minor-mode"),
            kind: ModeKind::Minor,
            action: "action:minor-refresh",
        },
    );
    let buf = editor.active_buffer_id();
    editor.activate_major_by_id(buf, major);
    editor.activate_minor_by_id(buf, minor);

    assert_eq!(
        editor.resolve_refresh_action(buf),
        Some(command_id(&editor, "action:minor-refresh")),
        "the provider minor owns the view; its refresh wins over the major's"
    );
}

/// Declaring a refresh must be the ONLY line a mode author writes —
/// activation of the shared minor is automatic, or the chord dies as
/// silently as the copied keymaps did.
#[test]
fn declaring_a_refresh_action_activates_the_shared_minor() {
    let mut editor = Editor::boot(CoreDocument::from_text("hello\n"));
    register_action(&editor, "action:test-view-refresh");
    let id = register_mode(
        &editor,
        DeclaringMode {
            id: ModeId::new("test-view-mode"),
            kind: ModeKind::Minor,
            action: "action:test-view-refresh",
        },
    );
    let buf = editor.active_buffer_id();
    editor.activate_minor_by_id(buf, id);

    assert!(
        editor
            .active_modes
            .get(&buf)
            .map(|m| m.has_minor(RefreshableViewMode::mode_id()))
            .unwrap_or(false),
        "refresh_action() alone must pull in the shared `gr` minor"
    );
}
