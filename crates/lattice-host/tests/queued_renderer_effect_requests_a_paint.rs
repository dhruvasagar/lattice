//! An effect queued for the renderer off-keystroke must request a paint.
//!
//! Some host paths run with no renderer to hand an effect back to: a plugin
//! picker's accept resolves off-thread and is applied on the actor's
//! `async_landed` arm, and so are a picker's fill target and delete verb.
//! What they leave for a renderer (a prompt, a transient, a picker) is queued
//! on the editor, and each peer applies the queue from its repaint wake. The
//! actor sends that wake only when `publish_render_state` reports that the
//! paint revision moved. If the queue is not part of the revision, the effect
//! is applied only when something else on screen happens to change, which is
//! usually the next key.
//!
//! The keystroke's own tail is the other place the queue fills. A plugin
//! picker's accept usually resolves within the `<CR>` that started it, so the
//! fused tail's `run_tick_pending` applies the outcome in-actor, and the
//! renderer's fused branch then applies only the effects the outcome carries.
//! The tail must hand the queue back in that outcome.
//!
//! That was org-clients' "Decision" capture: pick the client, and the
//! `Decision:` prompt appeared only after another key press, on most runs and
//! not all. With debug logging on it appeared at once, because the log line
//! changed `*messages*` and moved the revision for an unrelated reason. So the
//! paint tests settle the published state first and change nothing but the
//! queue.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use lattice_core::Document as CoreDocument;
use lattice_grammar::{Args, Effect};
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{
    AcceptFuture, PickerAcceptOutcome, PickerContext, RoutingPayload, SourceResult,
};

const SOURCE_ID: &str = "queued-effect-test";
const ACTION: &str = "queued-effect-ask";

/// The `Effect::OpenPrompt` org's capture returns for a template's `%^{…}`.
fn prompt() -> Effect {
    Effect::OpenPrompt {
        prompt: "Decision: ".to_string(),
        initial: String::new(),
        on_submit_action: "org-capture-answer".to_string(),
        buffer_name: None,
    }
}

/// An editor whose published state is current, so the next publish reports
/// a paint only for what changed after this.
fn settled() -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("x\n"));
    let _ = editor.publish_render_state();
    assert!(
        !editor.publish_render_state(),
        "nothing changed, so no paint"
    );
    editor
}

#[test]
fn a_queued_effect_requests_a_paint() {
    let mut editor = settled();
    // What `apply_off_renderer_effect` does with an effect it has no arm for.
    editor.pending_renderer_effects.push(prompt());
    assert!(
        editor.publish_render_state(),
        "the actor wakes a renderer only on a paint, and only the renderer applies the queue"
    );
}

#[test]
fn draining_the_queue_requests_a_paint_and_then_settles() {
    let mut editor = settled();
    editor.pending_renderer_effects.push(prompt());
    let _ = editor.publish_render_state();
    let drained = editor.drain_pending_renderer_effects();
    assert_eq!(drained.len(), 1);
    assert!(
        editor.publish_render_state(),
        "the published flag clears, so the next queued effect moves it again"
    );
    assert!(
        !editor.publish_render_state(),
        "and an empty queue stays quiet"
    );
}

/// A picker source whose accept resolves off-thread, as a plugin's does.
struct AsyncSource {
    spec: PickerSourceSpec,
}

impl PickerSourceGenerator for AsyncSource {
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
        Err("resolves through accept_async".to_string())
    }

    fn accept_async(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> Option<AcceptFuture> {
        Some(Box::pin(async move {
            Ok(PickerAcceptOutcome::InvokeCommand {
                id: ACTION.to_string(),
                args: Args::None,
            })
        }))
    }
}

/// An editor with the source above and an action that asks a question, the
/// way org's capture asks a template's `%^{…}`.
fn boot_with_async_source() -> Editor {
    let editor = Editor::boot(CoreDocument::from_text("x\n"));
    let mut registry = (**editor.registry.load()).clone();
    registry.register_action(
        ACTION,
        "Ask a question (stands in for a capture template's prompt)",
        lattice_grammar::registry::ActionSpec {
            args_schema: Vec::new(),
            apply: Arc::new(|_ctx: &lattice_grammar::registry::ActionContext| Ok(prompt())),
        },
    );
    editor.registry.store(Arc::new(registry));
    let mut pickers = (**editor.picker_registry.load()).clone();
    pickers.register_generator(Arc::new(AsyncSource {
        spec: PickerSourceSpec::no_args(SOURCE_ID, "An async accept that asks a question."),
    }));
    editor.picker_registry.store(Arc::new(pickers));
    editor
}

#[tokio::test(flavor = "current_thread")]
async fn an_accept_landing_in_the_keystroke_tail_hands_its_prompt_to_the_renderer() {
    let mut editor = boot_with_async_source();
    let cand = lattice_completion::candidate::RawCandidate::plain(
        "acme".to_string(),
        lattice_completion::candidate::CandidateKind::Plain,
    );
    editor.seat_picker_from_pairs(
        SOURCE_ID.to_string(),
        vec![(cand, RoutingPayload::Buffer { id: 0 })],
    );
    let _ = editor.do_picker_accept();
    assert!(
        editor.pending_picker_accept.is_some(),
        "the accept is deferred"
    );
    // Let the accept resolve before the tail runs, as it does when the guest
    // answers within the keystroke.
    let woken = editor.async_landed.clone();
    tokio::time::timeout(Duration::from_secs(5), woken.notified())
        .await
        .expect("the accept resolved");

    let kind = editor.active_buffer;
    let fused = editor.dispatch_fused(Action::None, kind, false, false);
    assert!(fused.tail_signals.is_some(), "an inert key fuses");
    assert!(
        editor.pending_picker_accept.is_none(),
        "the tail applied the accept"
    );
    assert!(
        fused
            .outcome
            .effects
            .iter()
            .any(|e| matches!(e, Effect::OpenPrompt { .. })),
        "the prompt comes back in the outcome the renderer's fused branch applies"
    );
    assert!(
        editor.pending_renderer_effects.is_empty(),
        "and is not left queued for a wake that may never come"
    );
}
