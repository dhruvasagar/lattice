//! `EditorActor` — the editor runs on its own thread.
//!
//! Phase 5.8.AF.5 / Slice 3c.0.
//!
//! ## Why this exists
//!
//! Paramount goal #4 (CLAUDE.md): "Three-layer architecture
//! (UI / Core / Plugins) communicating via typed message passing.
//! Multi-threaded by construction. Nothing blocks the UI -- enforced
//! architecturally, not by discipline."
//!
//! After Slices 3a + 3b.* moved every per-buffer LSP cache off
//! the renderer thread via wait-free read primitives, the last
//! piece of architectural debt is `Editor` itself living on the
//! renderer thread. While the renderer holds `&mut Editor`, the
//! UI thread *can* (in principle) do editor work synchronously,
//! and any future feature that does so silently regresses the
//! architecture. The fix: relocate `Editor` to its own thread so
//! the renderer is *physically incapable* of touching it
//! directly.
//!
//! ## Shape
//!
//! - `EditorActorHandle` — what the renderer holds. Carries the
//!   command-send half (`cmd_tx`), the signal-receive half
//!   (`signal_rx`), and a clone of the editor's
//!   `Arc<ArcSwap<RenderState>>`. Not `Clone` because it owns
//!   the unique receiver; `send_action` / `send_command` go
//!   through `&self`.
//! - `EditorCommand` — the typed mailbox payload. Renderer-to-
//!   editor messages. Includes `Apply(Action)`, `HandleEffect`,
//!   `DispatchBlocking { invocation, reply }`, `Tick`, `Ping`,
//!   `Shutdown`. Extensible: subsequent slices add variants.
//! - `spawn_editor_actor(editor) -> EditorActorHandle` — takes
//!   ownership of an `Editor`, spawns a dedicated thread with a
//!   `current_thread` tokio runtime, returns the handle. The
//!   thread name is `"lattice-editor"` for observability.
//!
//! ## Slice 3c.0 status
//!
//! Everything in this module is **dormant** -- no production call
//! site wires `spawn_editor_actor` yet. The follow-on sub-slices:
//!
//! - **3c.1**: populate `ActiveDocumentRenderState` (the read
//!   contract for cursor/scroll/modal/etc.) so renderers can
//!   migrate off direct `editor.X` reads.
//! - **3c.2 / 3c.3**: TUI / GPUI renderers switch their reads
//!   to `RenderState`.
//! - **3c.4**: renderers wire `EditorActorHandle`; `App::apply`
//!   becomes `handle.send_action(action)`.
//! - **3c.5**: sever `Arc<Editor>` from renderer entirely.
//! - **3c.6 / 3c.7**: polish + docs.
//!
//! Tests in this file verify the substrate works end-to-end:
//! spawn the actor, send a `Ping`, await the reply.

use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_grammar::CommandInvocation;
use lattice_grammar::effect::Effect;
use lattice_runtime::RuntimeError;
use tokio::sync::{mpsc, oneshot};

use crate::action::Action;
use crate::dispatch::{DispatchOutcome, RendererSignal};
use crate::editor::Editor;
use crate::render_state::RenderState;

/// Renderer-to-editor mailbox payload. Each variant corresponds
/// to a way the renderer (or another task) drives the editor's
/// state.
pub enum EditorCommand {
    /// Apply a renderer-translated `Action`. The action's
    /// `DispatchOutcome` is processed inside the actor: signals
    /// fan to `signal_tx`; queued `next_actions` and `effects`
    /// are processed iteratively (same shape as
    /// [`crate::editor::Editor::dispatch`]'s caller-side drain).
    Apply(Action),
    /// Apply a renderer-emitted `Effect` directly (bypasses
    /// dispatch). Used by Effect routers that already mapped
    /// the effect upstream.
    HandleEffect(Effect),
    /// Synchronous dispatch with a single-reply oneshot. The
    /// caller awaits the response. Used today by the `:g` /
    /// `:v` body-replay loop and any other path that needs the
    /// effect *immediately* (not via signal fan-out).
    DispatchBlocking {
        invocation: CommandInvocation,
        reply: oneshot::Sender<Result<Effect, RuntimeError>>,
    },
    /// Fire the periodic `run_tick_pending` aggregator (LSP
    /// drains, fs events, request fires). Sent by the renderer
    /// at frame cadence today; once the editor task owns a
    /// periodic timer of its own, this command retires.
    Tick,
    /// Empty roundtrip — replies as soon as the actor receives
    /// the message. Used by tests to await processing without
    /// observing side effects. Also useful for synchronous
    /// barriers in subsequent slices.
    Ping {
        reply: oneshot::Sender<()>,
    },
    /// Tear down the actor. The thread joins cleanly after the
    /// in-flight command completes.
    Shutdown,

    // ------------------------------------------------------------
    // 3c.atomic.F: typed setter commands. Mirror the in-process
    // setters added in 3c.atomic.C (`Editor::set_cursor` /
    // `set_cursor_line` / `set_cursor_byte` / `set_scroll` /
    // `set_modal`) and the publish wrapper around
    // `set_viewport_height` introduced in 3c.atomic.D.
    //
    // These commands cover the OUT-OF-DISPATCH write surface --
    // viewport resize signalled by the renderer, test fixtures
    // that wanted to seed cursor/scroll/modal directly, and
    // future paths where a non-editor task needs to nudge active-
    // document state. Each variant routes to the corresponding
    // setter, which writes the field and publishes a fresh
    // `RenderState`. The renderer observes the change via its
    // shared `Arc<ArcSwap<RenderState>>` clone -- identical
    // contract to the in-process method, executed on the editor
    // thread.
    // ------------------------------------------------------------
    /// Replace `Editor::cursor` and publish render-state.
    SetCursor(lattice_protocol::position::Position),
    /// Replace `Editor::cursor.line` and publish render-state.
    SetCursorLine(u32),
    /// Replace `Editor::cursor.byte` and publish render-state.
    SetCursorByte(u32),
    /// Replace `Editor::scroll` and publish render-state.
    SetScroll(u32),
    /// Replace `Editor::modal` and publish render-state.
    SetModal(lattice_grammar::ModalState),
    /// Resize the active pane's viewport. Mirrors the App-side
    /// `set_viewport_height` wrapper (clamp to >= 1, run
    /// `ensure_cursor_visible`, publish). The actor wraps the
    /// no-publish editor field write + the visibility fan-out
    /// into one atomic command so the renderer's per-frame "tell
    /// editor the pane size" hand-off is a single `cmd_tx.send`.
    SetViewportHeight(u32),
}

/// Renderer-side handle to the editor actor.
///
/// Holds:
/// - `cmd_tx` — unbounded; the renderer sends commands without
///   blocking. Backpressure is the actor's per-command processing
///   cost; today every command processes in microseconds so the
///   queue stays effectively empty.
/// - `signal_rx` — unbounded; the renderer drains signals once
///   per frame via [`Self::poll_signal`]. The actor pushes
///   signals as it processes commands.
/// - `render_state` — a clone of the editor's
///   `Arc<ArcSwap<RenderState>>`. The renderer loads via
///   `handle.render_state.load()` (wait-free); the editor's
///   `publish_render_state` calls are observable through this
///   shared Arc.
///
/// Not `Clone`: the `signal_rx` is uniquely owned. Renderers
/// that need to send commands from multiple places can clone
/// the underlying `mpsc::UnboundedSender` via
/// [`Self::cmd_sender`], which is freely `Clone`.
pub struct EditorActorHandle {
    cmd_tx: mpsc::UnboundedSender<EditorCommand>,
    signal_rx: mpsc::UnboundedReceiver<RendererSignal>,
    render_state: Arc<ArcSwap<RenderState>>,
    /// Join handle to the dedicated editor thread. Held so
    /// shutdown can join cleanly when the handle drops. `Some`
    /// in normal construction; `None` after explicit
    /// [`Self::shutdown_and_join`].
    join: Option<std::thread::JoinHandle<()>>,
}

impl EditorActorHandle {
    /// Send a command to the editor actor. Returns `Err` only
    /// when the actor has already shut down (channel closed).
    /// In production this should never happen during normal
    /// operation; in tests the failure mode is "test forgot to
    /// keep the handle alive."
    pub fn send(&self, cmd: EditorCommand) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.cmd_tx.send(cmd)
    }

    /// Convenience: send an `Apply(action)` command.
    pub fn send_action(
        &self,
        action: Action,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::Apply(action))
    }

    // ------------------------------------------------------------
    // 3c.atomic.F: typed setter helpers. Each wraps the
    // corresponding command variant so callers write
    // `handle.set_cursor(p)` instead of
    // `handle.send(EditorCommand::SetCursor(p))`. Symmetric with
    // the in-process `Editor::set_*` setters added in
    // 3c.atomic.C: same call shape, same semantics, different
    // dispatch (in-actor vs. in-process).
    // ------------------------------------------------------------

    /// Replace the editor's cursor and republish.
    pub fn set_cursor(
        &self,
        cursor: lattice_protocol::position::Position,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetCursor(cursor))
    }

    /// Replace `cursor.line` and republish.
    pub fn set_cursor_line(
        &self,
        line: u32,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetCursorLine(line))
    }

    /// Replace `cursor.byte` and republish.
    pub fn set_cursor_byte(
        &self,
        byte: u32,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetCursorByte(byte))
    }

    /// Replace `scroll` and republish.
    pub fn set_scroll(
        &self,
        scroll: u32,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetScroll(scroll))
    }

    /// Replace `modal` and republish.
    pub fn set_modal(
        &self,
        modal: lattice_grammar::ModalState,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetModal(modal))
    }

    /// Resize the active pane's viewport. Runs the same body as
    /// the App-side wrapper: clamp to >= 1, run
    /// `ensure_cursor_visible`, publish.
    pub fn set_viewport_height(
        &self,
        height: u32,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetViewportHeight(height))
    }

    /// Drain a single pending signal from the editor's
    /// signal stream. Returns `None` when the queue is empty
    /// (steady-state on most frames). Called per-frame by the
    /// renderer; the typical drain pattern is
    /// `while let Some(sig) = handle.poll_signal() { ... }`.
    pub fn poll_signal(&mut self) -> Option<RendererSignal> {
        self.signal_rx.try_recv().ok()
    }

    /// Cheap clone of the command-send side. Use this when
    /// multiple call sites in the renderer need to send
    /// commands without sharing a `&self` to the full handle.
    pub fn cmd_sender(&self) -> mpsc::UnboundedSender<EditorCommand> {
        self.cmd_tx.clone()
    }

    /// Wait-free read of the current `RenderState` snapshot.
    /// Returns an `Arc<RenderState>` the renderer holds across
    /// the frame.
    pub fn render_state(&self) -> Arc<RenderState> {
        self.render_state.load_full()
    }

    /// Shared `ArcSwap` reference for callers that need to
    /// store it alongside other Arc-shared state (e.g., a
    /// renderer that wants to clone the cell into a paint
    /// closure).
    pub fn render_state_arc(&self) -> Arc<ArcSwap<RenderState>> {
        self.render_state.clone()
    }

    /// Send `Shutdown` and join the editor thread. Idempotent
    /// in the sense that subsequent calls return `Ok(())`
    /// without re-joining. Returns `Err` only if the thread
    /// panicked.
    pub fn shutdown_and_join(&mut self) -> std::thread::Result<()> {
        let _ = self.cmd_tx.send(EditorCommand::Shutdown);
        if let Some(handle) = self.join.take() {
            handle.join()
        } else {
            Ok(())
        }
    }
}

impl Drop for EditorActorHandle {
    fn drop(&mut self) {
        // Best-effort clean shutdown. If the renderer holds the
        // handle as a long-lived field, this only fires at
        // editor exit; the join inside is short because the
        // actor's command loop is fast.
        if self.join.is_some() {
            let _ = self.cmd_tx.send(EditorCommand::Shutdown);
            if let Some(h) = self.join.take() {
                let _ = h.join();
            }
        }
    }
}

/// Spawn the editor actor on a dedicated thread with a
/// `current_thread` tokio runtime. Takes ownership of `editor`.
///
/// The dedicated thread is named `"lattice-editor"` for
/// observability. The actor's tokio runtime is `current_thread`
/// (single-threaded executor); editor work that needs the
/// multi-thread LSP runtime spawns onto it via
/// `lattice_runtime::runtime::spawn_on_lsp_runtime` as before
/// (unchanged).
///
/// Returns an [`EditorActorHandle`]. Drop the handle to shut
/// the actor down.
pub fn spawn_editor_actor(editor: Editor) -> EditorActorHandle {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<EditorCommand>();
    let (signal_tx, signal_rx) = mpsc::unbounded_channel::<RendererSignal>();
    let render_state = editor.render_state.clone();

    let join = std::thread::Builder::new()
        .name("lattice-editor".to_string())
        .spawn(move || {
            // current_thread runtime: single-task executor on
            // this thread. The editor task can spawn onto the
            // multi-thread `lsp_runtime` for LSP work as
            // before; only the editor's *own* work runs here.
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .thread_name("lattice-editor")
                .build()
                .expect("editor runtime should build");
            rt.block_on(run_actor(editor, cmd_rx, signal_tx));
        })
        .expect("spawn editor thread");

    EditorActorHandle {
        cmd_tx,
        signal_rx,
        render_state,
        join: Some(join),
    }
}

/// Actor event loop. Processes commands one at a time; the
/// `Editor` is owned exclusively by this task so all mutations
/// are single-writer. Signals flow to `signal_tx`;
/// `DispatchOutcome` cascades (`effects`, `next_actions`) are
/// processed iteratively to avoid stack growth on long chains.
async fn run_actor(
    mut editor: Editor,
    mut cmd_rx: mpsc::UnboundedReceiver<EditorCommand>,
    signal_tx: mpsc::UnboundedSender<RendererSignal>,
) {
    while let Some(cmd) = cmd_rx.recv().await {
        match cmd {
            EditorCommand::Apply(action) => {
                let outcome = editor.dispatch(action);
                drain_outcome(outcome, &mut editor, &signal_tx);
            }
            EditorCommand::HandleEffect(effect) => {
                let outcome = editor.handle_effect(effect);
                drain_outcome(outcome, &mut editor, &signal_tx);
            }
            EditorCommand::DispatchBlocking { invocation, reply } => {
                let result = editor.dispatch_blocking(invocation);
                let _ = reply.send(result);
            }
            EditorCommand::Tick => {
                let signals = editor.run_tick_pending();
                for sig in signals {
                    let _ = signal_tx.send(sig);
                }
            }
            EditorCommand::Ping { reply } => {
                let _ = reply.send(());
            }
            EditorCommand::Shutdown => break,
            // 3c.atomic.F: typed setter commands. Each delegates
            // to the in-process setter, which writes the field
            // and publishes. The renderer's shared
            // `Arc<ArcSwap<RenderState>>` clone observes the new
            // pointer wait-free.
            EditorCommand::SetCursor(p) => editor.set_cursor(p),
            EditorCommand::SetCursorLine(line) => editor.set_cursor_line(line),
            EditorCommand::SetCursorByte(byte) => editor.set_cursor_byte(byte),
            EditorCommand::SetScroll(s) => editor.set_scroll(s),
            EditorCommand::SetModal(m) => editor.set_modal(m),
            EditorCommand::SetViewportHeight(h) => {
                // Mirrors the App-side wrapper:
                // clamp to >= 1, run ensure_cursor_visible (which
                // may adjust `scroll`), publish once at the tail.
                editor.viewport_height = h.max(1);
                editor.ensure_cursor_visible();
                editor.publish_render_state();
            }
        }
    }
}

/// Drain a `DispatchOutcome`'s signals + cascade follow-ups.
/// Iterative (not recursive) to bound stack usage on long
/// `next_actions` chains.
fn drain_outcome(
    outcome: DispatchOutcome,
    editor: &mut Editor,
    signal_tx: &mpsc::UnboundedSender<RendererSignal>,
) {
    let mut work: Vec<DispatchOutcome> = vec![outcome];
    while let Some(mut o) = work.pop() {
        for sig in o.renderer_signals.drain(..) {
            let _ = signal_tx.send(sig);
        }
        for effect in o.effects.drain(..) {
            work.push(editor.handle_effect(effect));
        }
        for action in o.next_actions.drain(..) {
            work.push(editor.dispatch(action));
        }
        // `consumed` is a renderer-side flag (used by
        // `sync_keymap_overlays`); irrelevant inside the actor.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn the actor, send a Ping, await the reply. Verifies
    /// the substrate: thread spawns, runtime initializes,
    /// command channel works, oneshot reply works.
    #[test]
    fn actor_ping_pong_roundtrip() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(EditorCommand::Ping { reply: reply_tx })
            .expect("send ping");
        reply_rx.blocking_recv().expect("ping reply");
    }

    /// `Apply(Action::None)` causes the editor's render_state
    /// publication to fire — observable via Arc identity change
    /// on `handle.render_state()`.
    #[test]
    fn actor_apply_action_publishes_render_state() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        let before = handle.render_state();
        handle.send_action(Action::None).expect("send action");
        // Synchronize: Ping reply after the Apply guarantees the
        // actor processed the Apply (commands are serialized).
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(EditorCommand::Ping { reply: reply_tx })
            .expect("send ping");
        reply_rx.blocking_recv().expect("ping reply");
        let after = handle.render_state();
        assert!(
            !Arc::ptr_eq(&before, &after),
            "dispatch() tail must publish a fresh RenderState Arc"
        );
    }

    /// Dropping the handle cleanly shuts the thread down. No
    /// hang; no panic; thread joins.
    #[test]
    fn actor_drop_handle_joins_thread() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(EditorCommand::Ping { reply: reply_tx })
            .expect("send ping");
        reply_rx.blocking_recv().expect("ping reply");
        drop(handle);
        // If we reach here without hanging, the thread joined.
    }

    /// Explicit shutdown_and_join works without panic and is
    /// idempotent on subsequent calls.
    #[test]
    fn actor_explicit_shutdown_join_idempotent() {
        let editor = Editor::default();
        let mut handle = spawn_editor_actor(editor);
        handle.shutdown_and_join().expect("first join");
        // Second call returns Ok without re-joining (no
        // handle to join).
        handle.shutdown_and_join().expect("second join");
    }

    /// `Tick` runs `run_tick_pending` and forwards any signals.
    /// On a fresh editor with no pending drains, the signal
    /// stream stays empty.
    #[test]
    fn actor_tick_with_no_pending_work_emits_no_signals() {
        let editor = Editor::default();
        let mut handle = spawn_editor_actor(editor);
        handle.send(EditorCommand::Tick).expect("send tick");
        // Synchronize.
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(EditorCommand::Ping { reply: reply_tx })
            .expect("send ping");
        reply_rx.blocking_recv().expect("ping reply");
        // Drain any signals.
        let mut sig_count = 0usize;
        while handle.poll_signal().is_some() {
            sig_count += 1;
        }
        // A fresh-default editor with no LSP attach has no
        // signal-producing drains; tick should be silent.
        assert_eq!(sig_count, 0, "fresh editor Tick should be silent");
    }

    // ------------------------------------------------------------
    // 3c.atomic.F: typed setter command tests.
    //
    // Each test spawns a fresh actor, sends a setter command,
    // synchronizes with a Ping, and asserts that the published
    // `RenderState` reflects the mutation. The published-Arc
    // identity changes on each setter (different `Arc::ptr_eq`),
    // which is the canonical signal that `publish_render_state`
    // fired inside the actor body.
    // ------------------------------------------------------------

    /// Synchronize on the actor's command queue. Ping reply
    /// is sent after the preceding command's handler returns,
    /// because commands are processed serially.
    fn await_actor(handle: &EditorActorHandle) {
        let (reply_tx, reply_rx) = oneshot::channel();
        handle
            .send(EditorCommand::Ping { reply: reply_tx })
            .expect("send ping");
        reply_rx.blocking_recv().expect("ping reply");
    }

    #[test]
    fn actor_set_cursor_publishes_new_position() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        let before = handle.render_state();
        let target = lattice_protocol::position::Position::new(3, 7);
        handle.set_cursor(target).expect("send set_cursor");
        await_actor(&handle);
        let after = handle.render_state();
        assert!(
            !Arc::ptr_eq(&before, &after),
            "SetCursor must publish a fresh RenderState Arc"
        );
        assert_eq!(after.active_document.cursor, target);
    }

    #[test]
    fn actor_set_cursor_line_byte_publish_independently() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        handle.set_cursor_line(5).expect("send set_cursor_line");
        await_actor(&handle);
        let after_line = handle.render_state();
        assert_eq!(after_line.active_document.cursor.line, 5);
        // byte stays at default.
        assert_eq!(after_line.active_document.cursor.byte, 0);

        handle.set_cursor_byte(11).expect("send set_cursor_byte");
        await_actor(&handle);
        let after_byte = handle.render_state();
        assert_eq!(after_byte.active_document.cursor.line, 5);
        assert_eq!(after_byte.active_document.cursor.byte, 11);
    }

    #[test]
    fn actor_set_scroll_publishes_new_scroll() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        handle.set_scroll(42).expect("send set_scroll");
        await_actor(&handle);
        let after = handle.render_state();
        assert_eq!(after.active_document.scroll, 42);
    }

    #[test]
    fn actor_set_modal_publishes_new_modal() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        handle
            .set_modal(lattice_grammar::ModalState::Insert)
            .expect("send set_modal");
        await_actor(&handle);
        let after = handle.render_state();
        assert!(matches!(
            after.active_document.modal,
            lattice_grammar::ModalState::Insert
        ));
    }

    #[test]
    fn actor_set_viewport_height_clamps_and_publishes() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        // 0 must clamp to 1 -- mirror App-side wrapper.
        handle.set_viewport_height(0).expect("send vh=0");
        await_actor(&handle);
        assert_eq!(handle.render_state().active_document.viewport_height, 1);

        handle.set_viewport_height(24).expect("send vh=24");
        await_actor(&handle);
        assert_eq!(handle.render_state().active_document.viewport_height, 24);
    }
}
