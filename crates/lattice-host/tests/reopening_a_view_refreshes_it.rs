//! Re-opening a snapshot view re-runs its refresh.
//!
//! Synthetic buffers are created once and reused by name:
//! `ensure_named_synthetic_document` returns the existing buffer and does
//! NOT re-activate its major, so the mode's `on_activate` — which is what
//! fills the buffer — runs on the first open only. For a view whose content
//! is a snapshot of external state that made every later open a time
//! capsule: `C-x g` on an already-open `*magit:status*` showed the repository
//! as it was when the buffer was first created.
//!
//! The failure is quiet by construction — a stale status buffer looks exactly
//! like a current one — so the test asserts the refresh action was actually
//! RUN, not that the buffer looks plausible.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::{
    ActionHandlerContribution, ActivationPolicy, CapabilitySet, Keymap, LifecycleFuture, Mode,
    ModeContext, ModeId, ModeKind,
};

const REFRESH_ACTION: &str = "action:test-view-refresh";

/// A synthetic view that counts how many times its refresh ran.
struct CountingView {
    refreshes: Arc<AtomicUsize>,
    on_open: bool,
}

struct CountingViewGuard;

impl Mode for CountingView {
    type Guard = CountingViewGuard;

    fn id(&self) -> ModeId {
        ModeId::new("counting-view-mode")
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::default()
    }
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }
    fn refresh_action(&self) -> Option<&'static str> {
        Some(REFRESH_ACTION)
    }
    fn refresh_on_open(&self) -> bool {
        self.on_open
    }
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        let refreshes = Arc::clone(&self.refreshes);
        vec![ActionHandlerContribution {
            action_name: REFRESH_ACTION,
            handler: Arc::new(move |_ctx| {
                refreshes.fetch_add(1, Ordering::SeqCst);
                // Self-contained, per the `refresh_on_open` contract: a real
                // view spawns its work here (magit's `trigger_refresh` does)
                // and returns nothing.
                None
            }),
        }]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(CountingViewGuard) })
    }
}

/// Register the mode + its action, and return the refresh counter.
///
/// The returned registration must be HELD: `ActionHandlerRegistry::register`
/// hands back an RAII token that unregisters the handler when dropped, so a
/// `let _ = ...` here would leave the action resolvable with no body — which
/// is exactly what the first draft of this test did, and it read as a
/// production bug for a minute.
fn seat_mode(
    editor: &mut Editor,
    on_open: bool,
) -> (
    Arc<AtomicUsize>,
    Vec<lattice_mode::ActionHandlerRegistration>,
) {
    let refreshes = Arc::new(AtomicUsize::new(0));
    let mode = CountingView {
        refreshes: Arc::clone(&refreshes),
        on_open,
    };
    // The action has to exist in the command registry for
    // `resolve_refresh_action` to turn the declared name into a CommandId.
    {
        let mut reg = (*editor.registry.load_full()).clone();
        reg.register_action(
            REFRESH_ACTION,
            "test view refresh",
            lattice_grammar::registry::ActionSpec {
                apply: Arc::new(|_| Ok(lattice_grammar::effect::Effect::None)),
                args_schema: vec![],
            },
        );
        editor.registry.store(Arc::new(reg));
    }
    let mut registrations = Vec::new();
    if let Some(handlers) = editor
        .services
        .get::<lattice_mode::ActionHandlerRegistryHandle>()
    {
        for c in mode.action_handlers() {
            if let Some(id) = editor.registry.load().id_by_name(c.action_name) {
                registrations.push(handlers.register(id, c.handler));
            }
        }
    }
    let mut modes = (*editor.mode_registry.load_full()).clone();
    modes.register(mode).expect("registers");
    editor.mode_registry.store(Arc::new(modes));
    (refreshes, registrations)
}

#[test]
fn the_first_open_does_not_refresh_but_the_second_does() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let (refreshes, _registrations) = seat_mode(&mut editor, true);

    // First open: `on_activate` has just built the content, so refreshing
    // again would be a second scan for the same answer.
    editor.open_synthetic_buffer("*counting-view*", "counting-view-mode");
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        0,
        "the first open builds the buffer through on_activate; nothing to re-run"
    );

    // Switch away, then re-open — the case `C-x g` hits every time after
    // the first.
    let scratch = editor.document_buffer_id;
    editor.activate_buffer(scratch);
    editor.open_synthetic_buffer("*counting-view*", "counting-view-mode");
    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        1,
        "re-opening a reused buffer must re-run the view's declared refresh"
    );

    editor.activate_buffer(scratch);
    editor.open_synthetic_buffer("*counting-view*", "counting-view-mode");
    assert_eq!(refreshes.load(Ordering::SeqCst), 2, "…every time, not once");
}

/// Opt-in, and the default is off: a view whose content is authored in the
/// editor (help, a transcript) has nothing to re-derive, and refreshing it
/// would throw away scroll position for no gain.
#[test]
fn a_view_that_does_not_ask_for_it_is_left_alone() {
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    let (refreshes, _registrations) = seat_mode(&mut editor, false);

    editor.open_synthetic_buffer("*counting-view*", "counting-view-mode");
    let scratch = editor.document_buffer_id;
    editor.activate_buffer(scratch);
    editor.open_synthetic_buffer("*counting-view*", "counting-view-mode");

    assert_eq!(
        refreshes.load(Ordering::SeqCst),
        0,
        "refresh_on_open defaults to false and must stay opt-in"
    );
}
