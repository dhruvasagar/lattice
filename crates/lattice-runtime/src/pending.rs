//! `Pending<T>` -- the typed handle returned by every mutating
//! actor call (DESIGN.md §5.2.1).
//!
//! A `Pending` wraps a `tokio::sync::oneshot::Receiver`. Three usage
//! patterns:
//!
//! 1. **Async caller** (LSP client, plugin host, future async UI):
//!    `pending.await` yields the typed result.
//! 2. **Sync caller in a tokio context** (test fixtures running
//!    `#[tokio::test]`): same as above.
//! 3. **Sync caller outside tokio** (the TUI input loop, which is a
//!    blocking `crossterm::event::read` loop on the main thread):
//!    `pending.blocking_recv()` parks the current thread until the
//!    actor responds. The TUI uses
//!    [`crate::runtime::block_on`] which forwards to this.
//!
//! Errors are kept narrow: [`RuntimeError::ActorGone`] when the
//! actor task has shut down before it could respond, and
//! [`RuntimeError::Core`] for any inner [`lattice_core::CoreError`]
//! (range out of bounds, etc.). The previous `Busy` variant was
//! removed in audit slice 6 / H3 -- the document actor's mailbox
//! is now unbounded, so backpressure surfaces as queue depth
//! rather than per-call drops.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_core::CoreError;
use lattice_grammar::CommandError;
use thiserror::Error;
use tokio::sync::oneshot;

/// Monotonic id assigned to every actor-bound invocation. Unique
/// across the process -- not reused if an actor task dies and is
/// respawned. Useful for telemetry, logging, and (post-Phase-7)
/// for plugin-side correlation of request/response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InvocationId(pub u64);

impl InvocationId {
    /// Allocate the next id. Lock-free.
    pub fn next() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        Self(SEQ.fetch_add(1, Ordering::Relaxed))
    }
}

impl fmt::Display for InvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Outcome of an actor-bound mutation. Wraps a oneshot receiver so
/// the caller can await (or block on) the result.
///
/// `Pending` is neither `Clone` nor `Copy` -- the receiver is
/// single-use, matching the "one response per request" contract.
/// Dropping a `Pending` cancels the wait but does not interrupt the
/// actor; the response is silently discarded.
#[must_use = "the actor result is dropped if the Pending is not awaited or block_on'd"]
pub struct Pending<T> {
    pub id: InvocationId,
    rx: oneshot::Receiver<Result<T, RuntimeError>>,
    /// 2026-06-02: optional on-success transform applied at
    /// poll / blocking_recv time. Lets a producer adapt the
    /// inner actor's result coordinate space WITHOUT spawning
    /// a second task — the transform runs on whichever thread
    /// is driving the Pending (the consumer's thread).
    /// `None` for vanilla actor-bound Pendings.
    transform: Option<Box<dyn FnOnce(T) -> T + Send + 'static>>,
}

impl<T> Pending<T> {
    pub(crate) fn new(id: InvocationId, rx: oneshot::Receiver<Result<T, RuntimeError>>) -> Self {
        Self {
            id,
            rx,
            transform: None,
        }
    }

    /// 2026-06-02: attach an on-success transform that runs
    /// when the inner result resolves. The transform runs on
    /// the *consumer's* polling thread, NOT in a separately
    /// spawned task — critical when the producer is inside a
    /// `current_thread` runtime that's about to block in
    /// `block_on`. Use this instead of `Pending::spawn` for
    /// purely synchronous result-shape adaptation (e.g.
    /// translating coordinate spaces).
    ///
    /// `map_ok` may be called only once per Pending. A second
    /// call replaces the prior transform (last-write-wins).
    pub fn map_ok<F>(mut self, f: F) -> Self
    where
        F: FnOnce(T) -> T + Send + 'static,
    {
        self.transform = Some(Box::new(f));
        self
    }

    /// M.1 (2026-05-31): build a `Pending<T>` that resolves
    /// immediately with `result`. Used by read-only impls of
    /// [`crate::Document`] (`MultibufferDocumentHandle`) to
    /// reject writes without ever spawning an actor — every
    /// mutating method returns `Pending::ready(Err(
    /// RuntimeError::ReadOnly))`.
    pub fn ready(result: Result<T, RuntimeError>) -> Self {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(result);
        Self {
            id: InvocationId::next(),
            rx,
            transform: None,
        }
    }

    /// OA.23b: wrap a channel that a task ALREADY RUNNING will
    /// complete.
    ///
    /// The peer of [`Self::spawn`] for producers that have no task
    /// to spawn — the work is already queued somewhere else and all
    /// they hold is the reply end. `MultibufferDocumentHandle::
    /// apply_to_source` is the case: the edit rides the view's
    /// source-forwarder FIFO (spawned once, on the shared runtime,
    /// at view construction) and the forwarder answers this channel.
    ///
    /// Not a stylistic preference over `spawn`. `spawn` calls
    /// `tokio::spawn` and so needs a runtime in scope at the point
    /// of construction; this path is reached from the editor actor —
    /// a `current_thread` runtime about to `block_on` the result —
    /// where spawning the awaiter onto the caller's own runtime is
    /// the deadlock `map_ok` was added to avoid.
    ///
    /// A dropped sender resolves to `ActorGone`, as with any other
    /// `Pending`.
    pub fn from_channel(rx: oneshot::Receiver<Result<T, RuntimeError>>) -> Self {
        Self::new(InvocationId::next(), rx)
    }

    /// M.3 (2026-06-01): build a `Pending<T>` that resolves when
    /// the spawned future completes. Lets callers compose
    /// multiple `Pending`s into one without blocking the runtime
    /// (used by `MultibufferDocumentHandle::apply_edit_batch` to
    /// fan a translated batch across N source handles, await each
    /// source's Pending asynchronously, and combine the results).
    ///
    /// **Spawns on the SHARED runtime, never on the ambient one**, and that
    /// is the whole of its correctness.
    ///
    /// It used to call bare `tokio::spawn`, which lands the task on whatever
    /// runtime happens to be in scope at the point of construction. Every
    /// consumer of a `Pending` is a document operation, and the host reaches
    /// those from the editor actor — a `current_thread` runtime that then
    /// **blocks itself** awaiting the result (`block_on(document.save())`).
    /// The task is queued on the one thread that is now blocked, so nothing
    /// drives it and the editor wedges permanently, with the operation never
    /// performed.
    ///
    /// That is not a hypothetical. It shipped three times: the pre-M.11
    /// `apply_edit` freeze, the `undo` freeze, and `MultibufferDocument::save`
    /// — the user-visible form of the last being "`:w` on the agenda hangs
    /// forever and the change is not on disk". The first two were fixed by
    /// making those paths synchronous, which left this constructor still
    /// carrying the trap for the one caller that genuinely needs async work.
    ///
    /// Naming the target runtime fixes the class rather than the instance:
    /// document actors all live on the shared runtime, so it is where a
    /// composition over them belongs. It also drops the old "requires a
    /// current tokio runtime context" requirement — there is nothing left to
    /// get wrong at a call site.
    ///
    /// [`Self::map_ok`] and [`Self::from_channel`] remain the right tools for
    /// their own cases (a synchronous transform, and a task that is already
    /// running); they are no longer *workarounds* for this hazard.
    pub fn spawn<F>(future: F) -> Self
    where
        F: std::future::Future<Output = Result<T, RuntimeError>> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        crate::runtime::shared_runtime().spawn(async move {
            let result = future.await;
            let _ = tx.send(result);
        });
        Self {
            id: InvocationId::next(),
            rx,
            transform: None,
        }
    }

    /// Block the current thread until the actor responds. Used by
    /// the TUI input loop and by tests that don't drive a tokio
    /// reactor explicitly. Panics only if the oneshot's internal
    /// invariants are violated, which can't happen in safe code.
    pub fn blocking_recv(mut self) -> Result<T, RuntimeError> {
        match self.rx.blocking_recv() {
            Ok(Ok(t)) => {
                if let Some(f) = self.transform.take() {
                    Ok(f(t))
                } else {
                    Ok(t)
                }
            }
            Ok(Err(e)) => Err(e),
            // Sender dropped without sending -- actor died mid-call.
            Err(_) => Err(RuntimeError::ActorGone),
        }
    }
}

impl<T> std::future::Future for Pending<T> {
    type Output = Result<T, RuntimeError>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use std::task::Poll;
        match std::pin::Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(Ok(t))) => {
                let transformed = if let Some(f) = self.transform.take() {
                    f(t)
                } else {
                    t
                };
                Poll::Ready(Ok(transformed))
            }
            Poll::Ready(Ok(Err(e))) => Poll::Ready(Err(e)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(RuntimeError::ActorGone)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> fmt::Debug for Pending<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending").field("id", &self.id).finish()
    }
}

/// Failure modes a runtime caller can observe. Kept narrow so the
/// UI can branch on the failure shape rather than a generic message.
#[derive(Debug, Error)]
pub enum RuntimeError {
    /// The actor task has terminated (panic, drop, or graceful
    /// shutdown) before it could respond. Treated as a permanent
    /// failure -- the caller should re-spawn the document.
    #[error("document actor is no longer running")]
    ActorGone,

    /// An inner [`lattice_core::CoreError`] from
    /// `Document::apply_edit` / `undo` / `redo`. The actor is healthy;
    /// the operation itself was invalid.
    #[error(transparent)]
    Core(#[from] CoreError),

    /// A [`lattice_grammar::CommandError`] from a
    /// `lattice_grammar::execute` dispatch (unknown command, bad
    /// args, motion out-of-bounds, ...). The actor is healthy; the
    /// invocation was invalid.
    #[error(transparent)]
    Grammar(#[from] CommandError),

    /// M.1 (2026-05-31): write attempted against a read-only
    /// document. Returned by `MultibufferDocumentHandle`'s
    /// mutating methods until M.3 lands edit propagation.
    /// Distinguishes "this buffer doesn't accept writes by
    /// design" from `ActorGone` (transient / recoverable) and
    /// `Core` / `Grammar` (the write was tried but invalid).
    #[error("document is read-only")]
    ReadOnly,

    /// SS.3 (2026-08-11): a multibuffer save refused one or more
    /// sources because the file changed on disk after the view
    /// snapshotted it.
    ///
    /// Typed rather than a message blob because the caller wants the
    /// paths: the user has to go look at those files, and the recovery
    /// (refresh the view — `gr` / `:copen` / `:search` — which re-reads
    /// from disk) is per-view, not per-error-string.
    ///
    /// The other sources in the same save DID persist; this is a
    /// partial-success report, not a failed write. See
    /// `docs/dev/architecture/multibuffer-stale-sources.md` §2.1.
    #[error(
        "changed on disk since this view opened, not overwritten: {}. \
         Refresh the view to pick up the new content.",
        .paths.iter().map(|p| p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string()))
            .collect::<Vec<_>>().join(", ")
    )]
    SourcesChangedOnDisk { paths: Vec<std::path::PathBuf> },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn invocation_ids_are_monotonic() {
        let a = InvocationId::next();
        let b = InvocationId::next();
        let c = InvocationId::next();
        assert!(a.0 < b.0);
        assert!(b.0 < c.0);
    }

    #[tokio::test]
    async fn pending_resolves_to_sent_value() {
        let (tx, rx) = oneshot::channel();
        let p: Pending<i32> = Pending::new(InvocationId::next(), rx);
        tx.send(Ok(42)).unwrap();
        assert_eq!(p.await.unwrap(), 42);
    }

    #[tokio::test]
    async fn pending_yields_actor_gone_when_sender_dropped() {
        let (tx, rx) = oneshot::channel::<Result<i32, RuntimeError>>();
        let p = Pending::new(InvocationId::next(), rx);
        drop(tx);
        match p.await {
            Err(RuntimeError::ActorGone) => {}
            other => panic!("expected ActorGone, got {other:?}"),
        }
    }

    #[test]
    fn pending_blocking_recv_returns_actor_gone_on_drop() {
        let (tx, rx) = oneshot::channel::<Result<i32, RuntimeError>>();
        let p = Pending::new(InvocationId::next(), rx);
        drop(tx);
        match p.blocking_recv() {
            Err(RuntimeError::ActorGone) => {}
            other => panic!("expected ActorGone, got {other:?}"),
        }
    }
}
