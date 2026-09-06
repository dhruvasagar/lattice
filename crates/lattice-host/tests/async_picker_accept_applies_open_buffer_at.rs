//! OR.16 — the async picker-accept drain must not silently drop
//! `Effect::OpenBufferAt`.
//!
//! `Editor::drain_pending_picker_accept` commits the outcome of a picker
//! source whose `accept_async` deferred off-thread (a WASM plugin's accept
//! translation is always this shape — PH7.4c.2). The commit calls
//! `apply_picker_outcome`, whose `effects` are renderer-coupled and were
//! historically applied by whichever renderer called the SYNC path
//! (`do_picker_accept` -> `DispatchOutcome.effects` -> `apply_effect_app_arms`).
//! The async drain had no such renderer to hand the effects to, so it grew
//! its own small match and forwarded exactly one variant
//! (`Effect::OpenTransient`, OR.11b) — everything else fell through to a
//! `tracing::debug!` and was gone.
//!
//! `Effect::OpenBufferAt` hit that same hole (found via org-roam's
//! `<leader>onf` create-a-note flow, OR.13's investigation): the note gets
//! written (`Effect::WriteToFile` is host-applied inline, so it survives
//! regardless), but the buffer that is supposed to show it never opens.
//! Reproduced here WITHOUT the plugin: a native `CommandKind::Action`
//! stands in for the plugin's `ROAM_CREATE_NODE`, returning the exact
//! `Effect::Many([WriteToFile, OpenBufferAt])` shape the plugin fix
//! (`lattice-org-plugin` `98725df`) produces, invoked via
//! `PickerAcceptOutcome::InvokeCommand` from a source whose `accept_async`
//! resolves off-thread — the same seam `WasmPickerSource` uses for every
//! plugin picker source, per `lattice-plugin-host/src/picker_source.rs`.
//!
//! This test goes through the ASYNC drain deliberately — that asymmetry
//! with the sync path is the bug. A test that dispatched through
//! `do_picker_accept` alone (no deferred future) would pass on the broken
//! code, because the sync caller applies `out.effects` itself.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_grammar::{Args, Effect, FileAnchor};
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{
    AcceptFuture, PickerAcceptOutcome, PickerContext, RoutingPayload, SourceResult,
};
use lattice_protocol::position::Position;

const SOURCE_ID: &str = "or16-async-create-test";
const ACTION: &str = "or16-create-and-open";

fn tmp_target() -> PathBuf {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("lattice-or16-{nanos}-{n}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("new-note.org")
}

/// Stand-in for `WasmPickerSource`: `init` is irrelevant (the picker is
/// seated directly), and `accept_async` returns `Some`, deferring the
/// outcome off-thread through one `yield_now`, exactly like a real guest
/// round-trip. This is what forces the commit through
/// `drain_pending_picker_accept` instead of the synchronous return path.
struct AsyncCreateSource {
    spec: PickerSourceSpec,
}

impl AsyncCreateSource {
    fn new() -> Self {
        Self {
            spec: PickerSourceSpec::no_args(SOURCE_ID, "OR.16 regression: async create+open."),
        }
    }
}

impl PickerSourceGenerator for AsyncCreateSource {
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
        Err("must resolve via accept_async".to_string())
    }

    fn accept_async(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> Option<AcceptFuture> {
        Some(Box::pin(async move {
            tokio::task::yield_now().await;
            Ok(PickerAcceptOutcome::InvokeCommand {
                id: ACTION.to_string(),
                args: Args::None,
            })
        }))
    }
}

fn seat_one_candidate(editor: &mut Editor, source_id: &str) {
    let cand = lattice_completion::candidate::RawCandidate::plain(
        "new note".to_string(),
        lattice_completion::candidate::CandidateKind::Plain,
    );
    editor.seat_picker_from_pairs(
        source_id.to_string(),
        vec![(cand, RoutingPayload::Buffer { id: 0 })],
    );
}

/// Boot an editor with the async source above and a native action that
/// mirrors the org-roam plugin's stub-create fix exactly: write the note,
/// then open it — `Effect::Many([WriteToFile, OpenBufferAt])`.
fn boot(target: PathBuf) -> Editor {
    let editor = Editor::boot(CoreDocument::from_text("* Home\n"));

    let mut registry = (**editor.registry.load()).clone();
    let target_for_action = target.clone();
    registry.register_action(
        ACTION,
        "OR.16 regression fixture: write a note then open it (org-roam's stub-create shape)",
        lattice_grammar::registry::ActionSpec {
            args_schema: Vec::new(),
            apply: Arc::new(move |_ctx: &lattice_grammar::registry::ActionContext| {
                Ok(Effect::Many(vec![
                    Effect::WriteToFile {
                        path: target_for_action.clone(),
                        anchor: FileAnchor::End,
                        text: "#+title: New note\n".to_string(),
                        cut: None,
                        create_parents: false,
                    },
                    Effect::OpenBufferAt {
                        path: Some(target_for_action.clone()),
                        position: Position::new(0, 0),
                        force: false,
                    },
                ]))
            }),
        },
    );
    editor.registry.store(Arc::new(registry));

    let mut pickers = (**editor.picker_registry.load()).clone();
    pickers.register_generator(Arc::new(AsyncCreateSource::new()));
    editor.picker_registry.store(Arc::new(pickers));

    editor
}

/// The acceptance criterion for OR.16, reproduced host-side without the
/// plugin: after an ASYNC picker accept resolves an outcome that invokes an
/// action returning `Effect::Many([WriteToFile, OpenBufferAt])`, the note
/// exists AND is focused — not merely written.
#[test]
fn async_accept_opens_the_buffer_effect_created_not_just_writes_it() {
    let target = tmp_target();
    let mut editor = boot(target.clone());
    let origin = editor.active_pane_buffer_id();

    seat_one_candidate(&mut editor, SOURCE_ID);
    assert!(editor.picker.is_some(), "picker seated");

    // Accept defers: the async source's accept_async resolves off-thread,
    // so nothing is applied synchronously yet.
    let _ = editor.do_picker_accept();
    assert!(editor.picker.is_none(), "picker closes on accept");
    assert!(
        editor.pending_picker_accept.is_some(),
        "async accept is deferred, not applied synchronously"
    );

    // Pump the drain — the same aggregator production reaches via
    // `run_tick_pending` on the async-landed wake — until the outcome lands.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && editor.pending_picker_accept.is_some() {
        let _ = editor.drain_pending_picker_accept();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        editor.pending_picker_accept.is_none(),
        "drain committed + cleared the pending accept"
    );

    // WriteToFile: the note exists in the registry (this half survived even
    // on the broken code — WriteToFile is host-applied inline).
    let draft_id = editor
        .find_document_by_path(&target)
        .expect("WriteToFile resolved the path to a buffer");
    assert_eq!(
        editor
            .buffers
            .document_handle(draft_id)
            .unwrap()
            .snapshot()
            .text()
            .to_string(),
        "#+title: New note\n",
        "the note's content landed"
    );

    // OpenBufferAt: the buffer is FOCUSED, not just created. This is the
    // assertion that fails on the pre-fix code — `drain_pending_picker_accept`
    // dropped `Effect::OpenBufferAt` on the floor, so the active pane stayed
    // on the buffer the picker was opened from.
    assert_ne!(
        origin, draft_id,
        "sanity: the draft is a different buffer than where we started"
    );
    assert_eq!(
        editor.active_pane_buffer_id(),
        draft_id,
        "the draft is focused, not just created (OR.16: OpenBufferAt must not \
         be dropped by the async picker-accept drain)"
    );
}
