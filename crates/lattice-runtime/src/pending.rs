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
}

impl<T> Pending<T> {
    pub(crate) fn new(id: InvocationId, rx: oneshot::Receiver<Result<T, RuntimeError>>) -> Self {
        Self { id, rx }
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
        }
    }

    /// Block the current thread until the actor responds. Used by
    /// the TUI input loop and by tests that don't drive a tokio
    /// reactor explicitly. Panics only if the oneshot's internal
    /// invariants are violated, which can't happen in safe code.
    pub fn blocking_recv(self) -> Result<T, RuntimeError> {
        match self.rx.blocking_recv() {
            Ok(res) => res,
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
            Poll::Ready(Ok(res)) => Poll::Ready(res),
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
