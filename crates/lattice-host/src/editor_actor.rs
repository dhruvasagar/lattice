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

/// Error returned by the synchronous handle methods when the
/// actor thread has shut down. Production code maps this to a
/// fatal condition: the editor thread dying is unrecoverable.
/// In tests, surfaces as a `panic!` via `unwrap` (acceptable —
/// test fixtures keep the handle alive for their duration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorGone;

impl std::fmt::Display for ActorGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "editor actor thread has terminated")
    }
}

impl std::error::Error for ActorGone {}

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
    /// Synchronous variant of [`Self::Apply`]: dispatch the
    /// action, then signal completion on `reply`. Phase 5.8.AF.5
    /// / Slice 3c.final.E: used by the renderer's synchronous
    /// `App::apply` wrapper so existing call sites that expect
    /// to read editor-published state on the next line keep
    /// working unchanged. The actor processes commands serially,
    /// so awaiting this reply is a barrier — any previously-
    /// queued commands (including signals) drain first.
    ApplyAndReply {
        action: Action,
        reply: oneshot::Sender<()>,
    },
    /// Closure escape hatch (3c.final.E): run an arbitrary
    /// mutation against `Editor` on the actor thread. Fire-and-
    /// forget — no reply. Used by the renderer's `mutate_async`
    /// helper for callsites that don't need a synchronous
    /// barrier (e.g., LSP response handlers that simply stash a
    /// cache and let the next publish flow downstream).
    Mutate(Box<dyn FnOnce(&mut Editor) + Send>),
    /// Synchronous variant of [`Self::Mutate`]: run the closure
    /// against `Editor`, then signal completion. Used by the
    /// majority of App-side helpers during the slice-E sweep;
    /// the existing helper bodies move into the closure verbatim
    /// and the caller blocks on `reply.recv()`. As helpers gain
    /// typed `Action::*` variants over follow-up slices, their
    /// callers retire `MutateAndReply` in favour of
    /// `ApplyAndReply`.
    MutateAndReply {
        closure: Box<dyn FnOnce(&mut Editor) + Send>,
        reply: oneshot::Sender<()>,
    },
    /// Read-side RPC (3c.final.E.swap): run a closure against
    /// `&Editor` (immutable borrow) and reply with the boxed
    /// result. The closure's return type is erased via
    /// `Box<dyn Any + Send>` so the actor's command enum stays
    /// non-generic; the caller's `with_editor<R>` helper
    /// downcasts back to `R`. Used by every `&self`-receiver
    /// helper on `App` / `GpuiApp` that previously did
    /// `self.editor.X(...)` for a read.
    Read {
        closure: Box<dyn FnOnce(&Editor) -> Box<dyn std::any::Any + Send> + Send>,
        reply: oneshot::Sender<Box<dyn std::any::Any + Send>>,
    },
    /// Read-and-mutate RPC (3c.final.E.swap): like `MutateAndReply`
    /// but the closure returns a value. Used by `mutate_editor_with`
    /// post-swap so closures returning `Vec<RendererSignal>` or
    /// `Result<_, _>` still work. Same `Box<dyn Any>` erasure
    /// pattern as [`Self::Read`].
    MutateWithReply {
        closure: Box<dyn FnOnce(&mut Editor) -> Box<dyn std::any::Any + Send> + Send>,
        reply: oneshot::Sender<Box<dyn std::any::Any + Send>>,
    },
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
    Ping { reply: oneshot::Sender<()> },
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
    /// Per-pane geometry update. Issue #25 (2026-05-22): the
    /// renderer's per-frame layout pass walks the pane tree and
    /// fires one of these per leaf with the leaf's computed
    /// height + width in screen rows / columns. The host stores
    /// them on `PaneState`; the active leaf's viewport_height
    /// is mirrored into `Editor::viewport_height` for the
    /// cursor-clamp + highlights-worker code paths that don't
    /// carry a pane index. Publishes RS at the tail.
    SetPaneViewport { idx: usize, height: u32, width: u32 },
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
    /// I.3: the editor's `paint_request` `Notify`, fired by the async
    /// workers after a republish. Cloned to the TUI event loop so a
    /// background publish wakes a repaint without polling.
    paint_request: Arc<tokio::sync::Notify>,
    /// Join handle to the dedicated editor thread. Held so
    /// shutdown can join cleanly when the handle drops. `Some`
    /// in normal construction; `None` after explicit
    /// [`Self::shutdown_and_join`].
    join: Option<std::thread::JoinHandle<()>>,
}

/// Runtime-flavor-aware `oneshot::Receiver::blocking_recv`.
///
/// Slice `3c.fixup.actor-sync-rpc` (companion to
/// `3c.fixup.actor-block-on` in `lattice-runtime/src/runtime.rs`).
/// The sync-RPC methods below (`apply_blocking`, `mutate_blocking`,
/// `mutate_blocking_with`, `with_editor`) all park the caller's
/// thread on a `oneshot::Receiver` until the actor replies. Tokio's
/// `blocking_recv` panics with
///   "Cannot block the current thread from within a runtime"
/// when called from inside an async context.
///
/// The GPUI peer's main thread hosts a tokio runtime (current-thread,
/// for `tokio_main`-style entry); any App-side helper that reaches
/// the actor seam from that thread previously panicked at startup.
/// Caught by `cargo run --features window` on 2026-05-21; the fix
/// follows the same three-arm pattern as
/// `lattice_runtime::block_on`:
///
///   1. No current handle — direct `blocking_recv()` (the previous
///      contract).
///   2. MultiThread runtime — relinquish the worker via
///      `task::block_in_place(|| rx.blocking_recv())`.
///   3. Non-MultiThread runtime (the GPUI main thread case) —
///      escape to a fresh OS thread via `std::thread::scope` and
///      do the blocking recv there, outside any tokio context.
fn safe_blocking_recv<T: Send>(rx: oneshot::Receiver<T>) -> Result<T, oneshot::error::RecvError> {
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current() {
        Ok(handle) if matches!(handle.runtime_flavor(), RuntimeFlavor::MultiThread) => {
            tokio::task::block_in_place(|| rx.blocking_recv())
        }
        Ok(_) => std::thread::scope(|s| {
            s.spawn(|| rx.blocking_recv())
                .join()
                .expect("nested-blocking_recv bridge thread completed")
        }),
        Err(_) => rx.blocking_recv(),
    }
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

    /// Convenience: send an `Apply(action)` command. Fire-and-
    /// forget — does not wait for completion. The renderer
    /// keystroke handler uses this so the input loop returns
    /// immediately; the dispatched action runs on the actor
    /// thread and publishes RenderState when done (the existing
    /// `paint_request` Notify wakes the GPUI peer's next paint).
    pub fn send_action(&self, action: Action) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::Apply(action))
    }

    /// Synchronous dispatch — blocks until the actor finishes
    /// processing this action (and any cascade). Used by
    /// `App::apply` so existing call sites that read editor-
    /// derived state on the next line keep working. Returns
    /// `Err` only if the actor died mid-await.
    ///
    /// Safe to call from inside or outside a tokio runtime:
    /// `safe_blocking_recv` selects the right wait path based on
    /// the current runtime flavor (see its docstring).
    pub fn apply_blocking(&self, action: Action) -> Result<(), ActorGone> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send(EditorCommand::ApplyAndReply {
            action,
            reply: reply_tx,
        })
        .map_err(|_| ActorGone)?;
        safe_blocking_recv(reply_rx).map_err(|_| ActorGone)
    }

    /// Closure escape hatch — synchronous: send the closure +
    /// block until the actor runs it and publishes RS. Used by
    /// App-side helpers during the slice-E sweep when the
    /// helper's body mutates editor state directly. Each helper
    /// wraps its body in one closure; the caller's read-back of
    /// editor-derived state via `app.ad()` etc. sees the post-
    /// mutation snapshot.
    pub fn mutate_blocking(
        &self,
        closure: Box<dyn FnOnce(&mut Editor) + Send>,
    ) -> Result<(), ActorGone> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.send(EditorCommand::MutateAndReply {
            closure,
            reply: reply_tx,
        })
        .map_err(|_| ActorGone)?;
        // Slice 3c.fixup.actor-sync-rpc: runtime-flavor-aware wait.
        safe_blocking_recv(reply_rx).map_err(|_| ActorGone)
    }

    /// Closure escape hatch — fire-and-forget. For tokio task
    /// callers that can't block (e.g., LSP response handlers).
    pub fn mutate_async(
        &self,
        closure: Box<dyn FnOnce(&mut Editor) + Send>,
    ) -> Result<(), ActorGone> {
        self.send(EditorCommand::Mutate(closure))
            .map_err(|_| ActorGone)
    }

    /// Read-side RPC (3c.final.E.swap): synchronously run a
    /// closure against `&Editor` on the actor thread and return
    /// the result. Used by every `&self`-receiver helper on
    /// `App` / `GpuiApp` that needs to read editor state.
    /// Internally uses `Box<dyn Any>` erasure to keep the
    /// actor's command enum non-generic.
    pub fn with_editor<R, F>(&self, f: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&Editor) -> R + Send + 'static,
    {
        let (tx, rx) = oneshot::channel::<Box<dyn std::any::Any + Send>>();
        let closure: Box<dyn FnOnce(&Editor) -> Box<dyn std::any::Any + Send> + Send> =
            Box::new(move |e| Box::new(f(e)));
        self.send(EditorCommand::Read { closure, reply: tx })
            .expect("editor actor alive");
        // Slice 3c.fixup.actor-sync-rpc: runtime-flavor-aware wait.
        let any = safe_blocking_recv(rx).expect("editor actor alive");
        *any.downcast::<R>().expect("read RPC result type matches")
    }

    /// Sync mutate RPC with return value (3c.final.E.swap).
    /// Used by `mutate_editor_with` post-swap: the closure
    /// returns `R`; the actor publishes RS at its tail.
    pub fn mutate_blocking_with<R, F>(&self, f: F) -> R
    where
        R: Send + 'static,
        F: FnOnce(&mut Editor) -> R + Send + 'static,
    {
        let (tx, rx) = oneshot::channel::<Box<dyn std::any::Any + Send>>();
        let closure: Box<dyn FnOnce(&mut Editor) -> Box<dyn std::any::Any + Send> + Send> =
            Box::new(move |e| Box::new(f(e)));
        self.send(EditorCommand::MutateWithReply { closure, reply: tx })
            .expect("editor actor alive");
        // Slice 3c.fixup.actor-sync-rpc: runtime-flavor-aware wait.
        let any = safe_blocking_recv(rx).expect("editor actor alive");
        *any.downcast::<R>().expect("mutate RPC result type matches")
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
    pub fn set_cursor_line(&self, line: u32) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetCursorLine(line))
    }

    /// Replace `cursor.byte` and republish.
    pub fn set_cursor_byte(&self, byte: u32) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetCursorByte(byte))
    }

    /// Replace `scroll` and republish.
    pub fn set_scroll(&self, scroll: u32) -> Result<(), mpsc::error::SendError<EditorCommand>> {
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

    /// Issue #25 (2026-05-22): per-pane geometry setter. The
    /// renderer's per-frame layout pass walks the pane tree and
    /// fires one of these per leaf. The active leaf's height is
    /// auto-mirrored into `Editor::viewport_height` for cursor
    /// clamp + highlights worker.
    pub fn set_pane_viewport(
        &self,
        idx: usize,
        height: u32,
        width: u32,
    ) -> Result<(), mpsc::error::SendError<EditorCommand>> {
        self.send(EditorCommand::SetPaneViewport { idx, height, width })
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

    /// Shared `paint_request` `Notify` the actor's workers fire after an
    /// async republish (syntax recolour, LSP decoration, cells/virtual-rows).
    /// The TUI event loop (I.3) awaits this to repaint promptly; the GPUI peer
    /// uses the same notify natively.
    pub fn paint_request(&self) -> Arc<tokio::sync::Notify> {
        self.paint_request.clone()
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
    let paint_request = editor.paint_request.clone();

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
        paint_request,
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
    // Slice B.1 (2026-06-03): the loop wakes on EITHER an incoming
    // command OR `async_landed` — fired by async completions (today
    // the syntax reparse worker on publish) that produce
    // render-relevant state with no keystroke in flight. On that wake
    // we run the tick aggregator + re-publish, so e.g. an idle markdown
    // reparse repaints without waiting for the next key. Runs on the
    // single-writer actor thread, not the UI thread (paramount #1).
    let async_landed = editor.async_landed.clone();
    // L4a.2 (lsp-architecture.md §15): the inline cursor-line
    // diagnostic-summary idle gate. A pinned sleep, seeded far in the
    // future and retargeted each iteration to `editor`'s armed
    // deadline (set in `update_inline_diag_gate` during publish). The
    // guarded select! arm fires only for a live arm; on fire it makes
    // the summary visible + republishes, all on the actor thread
    // (paramount #1 — never the UI thread).
    let inline_diag_sleep = tokio::time::sleep(std::time::Duration::from_secs(60 * 60));
    tokio::pin!(inline_diag_sleep);
    loop {
        // Retarget the idle-gate sleep to the current deadline. Cheap;
        // when disarmed we point it an hour out and the `is_some()`
        // guard keeps the arm dormant.
        inline_diag_sleep
            .as_mut()
            .reset(editor.inline_diag_deadline.unwrap_or_else(|| {
                tokio::time::Instant::now() + std::time::Duration::from_secs(60 * 60)
            }));
        let cmd = tokio::select! {
            maybe_cmd = cmd_rx.recv() => match maybe_cmd {
                Some(cmd) => cmd,
                None => break,
            },
            _ = async_landed.notified() => {
                let signals = editor.run_tick_pending();
                // §12 paint gate: an async arrival that moved a non-cell
                // render-visible surface (LSP readiness badge, diagnostics
                // overlay, popup, …) must reach a frame WITHOUT a keystroke
                // — the cells / virtual-rows workers only paint on their
                // own content change. `publish_render_state` reports
                // whether `paint_revision` moved; fire `paint_request`
                // when it did. Gated so a no-op publish doesn't spin the
                // GPUI paint bridge.
                let painted = editor.publish_render_state();
                if painted {
                    editor.paint_request.notify_one();
                }
                // Notify cells via the event bus so the wake is
                // sequenced after the ArcSwap store in
                // publish_render_state. Cells wakes via the
                // AsyncRenderStatePublished bridge in editor_boot.rs.
                editor.event_bus.publish_typed(
                    crate::events::AsyncRenderStatePublished,
                );
                for sig in signals {
                    let _ = signal_tx.send(sig);
                }
                continue;
            }
            // L4a.2: the inline-diagnostic idle deadline elapsed.
            // Flip the gate visible, republish (so `build_render_state`
            // emits the cursor-line summary), and wake cells via the
            // same `AsyncRenderStatePublished` bridge the async_landed
            // arm uses. No `run_tick_pending` — the gate only changes
            // presentation, not pending async work.
            _ = &mut inline_diag_sleep, if editor.inline_diag_deadline.is_some() => {
                editor.fire_inline_diag_gate();
                // §12 paint gate: the idle gate flips the inline summary
                // visible — a non-cell surface — so fire `paint_request`
                // when the publish reports the change.
                let painted = editor.publish_render_state();
                if painted {
                    editor.paint_request.notify_one();
                }
                editor.event_bus.publish_typed(
                    crate::events::AsyncRenderStatePublished,
                );
                continue;
            }
        };
        match cmd {
            EditorCommand::Apply(action) => {
                let outcome = editor.dispatch(action);
                drain_outcome(outcome, &mut editor, &signal_tx);
            }
            EditorCommand::ApplyAndReply { action, reply } => {
                let outcome = editor.dispatch(action);
                drain_outcome(outcome, &mut editor, &signal_tx);
                // Signal completion after dispatch + cascade.
                // Render-state already published by dispatch tail
                // so the caller's next `render_state.load()` sees
                // the new state.
                let _ = reply.send(());
            }
            EditorCommand::Mutate(closure) => {
                closure(&mut editor);
                // The closure may or may not have published RS;
                // for the renderer's read contract to hold, fire
                // a publish here so the caller's next read sees
                // any field mutation the closure made.
                editor.publish_render_state();
            }
            EditorCommand::MutateAndReply { closure, reply } => {
                closure(&mut editor);
                editor.publish_render_state();
                let _ = reply.send(());
            }
            EditorCommand::Read { closure, reply } => {
                let any = closure(&editor);
                let _ = reply.send(any);
            }
            EditorCommand::MutateWithReply { closure, reply } => {
                let any = closure(&mut editor);
                editor.publish_render_state();
                let _ = reply.send(any);
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
            EditorCommand::SetPaneViewport { idx, height, width } => {
                // Issue #25 (2026-05-22): write per-pane geometry
                // onto the leaf and, when this is the active pane,
                // mirror its height into `Editor::viewport_height`
                // so cursor-clamp + highlights-worker keep reading
                // a single value but it now always reflects the
                // active pane's actual painted area.
                let active_idx = editor.pane_tree.active_index();
                let leaves = editor.pane_tree.leaves_mut();
                let pane_kind = leaves.get(idx).map(|l| (l.buffer, l.buffer_id));
                if idx < leaves.len() {
                    leaves[idx].viewport_height = height.max(1);
                    leaves[idx].viewport_width = width.max(1);
                }
                if idx == active_idx {
                    editor.viewport_height = height.max(1);
                    editor.ensure_cursor_visible();
                    // DB.4: content-centring pad depends on the pane width, so
                    // recompute it (and the rest of the option cache) when the
                    // active pane resizes — keeps the dashboard centred.
                    editor.rebuild_option_cache();
                }
                // T4.1 (2026-05-25): when the pane hosts a
                // terminal, propagate the new geometry to both
                // the alacritty grid (via SharedTerm::resize)
                // and the PTY (via PtyHandle::resize) so the
                // child sees a SIGWINCH and re-lays-out its UI.
                if let Some((lattice_core::BufferKind::Terminal, buf_id)) = pane_kind {
                    let rows = height.max(1).min(u16::MAX as u32) as u16;
                    let cols = width.max(1).min(u16::MAX as u32) as u16;
                    let _ = editor.buffers.with_terminal(buf_id, |t| {
                        t.term.resize(rows, cols);
                        let _ = t.pty.resize(rows, cols);
                    });
                }
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
        assert_eq!(after.active_document.load().cursor, target);
    }

    #[test]
    fn actor_set_cursor_line_byte_publish_independently() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        handle.set_cursor_line(5).expect("send set_cursor_line");
        await_actor(&handle);
        let after_line = handle.render_state();
        assert_eq!(after_line.active_document.load().cursor.line, 5);
        // byte stays at default.
        assert_eq!(after_line.active_document.load().cursor.byte, 0);

        handle.set_cursor_byte(11).expect("send set_cursor_byte");
        await_actor(&handle);
        let after_byte = handle.render_state();
        assert_eq!(after_byte.active_document.load().cursor.line, 5);
        assert_eq!(after_byte.active_document.load().cursor.byte, 11);
    }

    #[test]
    fn actor_set_scroll_publishes_new_scroll() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        handle.set_scroll(42).expect("send set_scroll");
        await_actor(&handle);
        let after = handle.render_state();
        assert_eq!(after.active_document.load().scroll, 42);
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
            after.active_document.load().modal,
            lattice_grammar::ModalState::Insert
        ));
    }

    /// Slice 3c.final.E: `apply_blocking` dispatches the action,
    /// blocks until the actor processes it, then returns. The
    /// caller's next render_state read sees the post-dispatch
    /// state.
    #[test]
    fn actor_apply_blocking_completes_synchronously() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        let before = handle.render_state();
        handle
            .apply_blocking(Action::None)
            .expect("apply blocking succeeds");
        let after = handle.render_state();
        assert!(
            !Arc::ptr_eq(&before, &after),
            "apply_blocking must publish a fresh RS before returning"
        );
    }

    /// Slice 3c.final.E: `mutate_blocking` runs the closure and
    /// fires `publish_render_state`. The mutation is visible
    /// through the published RS after the call returns.
    #[test]
    fn actor_mutate_blocking_applies_closure_and_publishes() {
        use lattice_protocol::position::Position;
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        handle
            .mutate_blocking(Box::new(|e| {
                e.cursor = Position::new(9, 4);
                e.scroll = 7;
            }))
            .expect("mutate blocking succeeds");
        let rs = handle.render_state();
        assert_eq!(rs.active_document.load().cursor, Position::new(9, 4));
        assert_eq!(rs.active_document.load().scroll, 7);
    }

    /// Slice 3c.final.E: `mutate_async` is fire-and-forget; a
    /// subsequent `apply_blocking` barrier ensures the mutation
    /// has landed before the assertion.
    #[test]
    fn actor_mutate_async_applies_when_drained() {
        use lattice_protocol::position::Position;
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        handle
            .mutate_async(Box::new(|e| e.cursor = Position::new(2, 1)))
            .expect("mutate async succeeds");
        // Barrier: serial command processing means this returns
        // only after the prior Mutate runs.
        handle
            .apply_blocking(Action::None)
            .expect("apply blocking barrier");
        assert_eq!(
            handle.render_state().active_document.load().cursor,
            Position::new(2, 1)
        );
    }

    #[test]
    fn actor_set_viewport_height_clamps_and_publishes() {
        let editor = Editor::default();
        let handle = spawn_editor_actor(editor);
        // 0 must clamp to 1 -- mirror App-side wrapper.
        handle.set_viewport_height(0).expect("send vh=0");
        await_actor(&handle);
        assert_eq!(
            handle.render_state().active_document.load().viewport_height,
            1
        );

        handle.set_viewport_height(24).expect("send vh=24");
        await_actor(&handle);
        assert_eq!(
            handle.render_state().active_document.load().viewport_height,
            24
        );
    }
}
