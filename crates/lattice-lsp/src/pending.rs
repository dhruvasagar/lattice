//! `Pending<T>` -- the typed handle returned by every actor-side
//! request. Mirrors `lattice_runtime::Pending` (DESIGN.md §5.2.1)
//! but parameterised over [`LspError`] so server-side error codes
//! survive on the way back to the caller.
//!
//! Three usage patterns, identical to the runtime's `Pending`:
//!
//! 1. **Async caller** (the editor's input pipeline): `pending.await`.
//! 2. **Sync caller in tokio** (`#[tokio::test]`): same.
//! 3. **Sync caller outside tokio** (the TUI input loop):
//!    `pending.blocking_recv()` parks the current thread.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::oneshot;

use crate::error::{LspError, LspResult};

/// Process-monotonic id for an LSP request -- distinct from the
/// JSON-RPC `RequestId` on the wire (which is per-server). Useful
/// for telemetry: an `InvocationId` survives a server restart
/// while the wire id resets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InvocationId(pub u64);

impl InvocationId {
    /// Allocate the next id. Lock-free; never collides with the
    /// runtime's `InvocationId` because the two crates use
    /// independent counters.
    pub fn next() -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        Self(SEQ.fetch_add(1, Ordering::Relaxed))
    }
}

impl fmt::Display for InvocationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lsp#{}", self.0)
    }
}

/// One in-flight LSP request awaiting a response.
///
/// `Pending` is neither `Clone` nor `Copy`: the underlying
/// oneshot is single-use, matching the "one response per request"
/// LSP contract. Dropping a `Pending` cancels the wait but does
/// not interrupt the actor; callers that need true cancellation
/// pass a [`lattice_runtime::CancellationToken`] alongside the
/// request and the actor sends `$/cancelRequest` to the server.
#[must_use = "the LSP response is dropped if the Pending is not awaited or block_on'd"]
pub struct Pending<T> {
    pub id: InvocationId,
    rx: oneshot::Receiver<LspResult<T>>,
}

impl<T> Pending<T> {
    pub(crate) fn new(id: InvocationId, rx: oneshot::Receiver<LspResult<T>>) -> Self {
        Self { id, rx }
    }

    /// Eagerly resolved error -- used at construction sites where
    /// the request never reaches the actor (e.g. mailbox closed).
    pub(crate) fn ready_err(err: LspError) -> Self {
        let (tx, rx) = oneshot::channel();
        // Sender will only fail if the receiver was already
        // dropped, which it can't be since we just made it.
        let _ = tx.send(Err(err));
        Self {
            id: InvocationId::next(),
            rx,
        }
    }

    /// Block the current thread until the actor responds. Used by
    /// the TUI input loop and tests.
    pub fn blocking_recv(self) -> LspResult<T> {
        match self.rx.blocking_recv() {
            Ok(res) => res,
            Err(_) => Err(LspError::ResponseDropped),
        }
    }
}

impl<T> std::future::Future for Pending<T> {
    type Output = LspResult<T>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        use std::task::Poll;
        match std::pin::Pin::new(&mut self.rx).poll(cx) {
            Poll::Ready(Ok(res)) => Poll::Ready(res),
            Poll::Ready(Err(_)) => Poll::Ready(Err(LspError::ResponseDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> fmt::Debug for Pending<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pending").field("id", &self.id).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_resolves_with_value() {
        let (tx, rx) = oneshot::channel();
        let p: Pending<i32> = Pending::new(InvocationId::next(), rx);
        tx.send(Ok(42)).unwrap();
        assert_eq!(p.await.unwrap(), 42);
    }

    #[tokio::test]
    async fn pending_yields_response_dropped_when_sender_dies() {
        let (tx, rx) = oneshot::channel::<LspResult<i32>>();
        let p = Pending::new(InvocationId::next(), rx);
        drop(tx);
        match p.await {
            Err(LspError::ResponseDropped) => {}
            other => panic!("expected ResponseDropped, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ready_err_short_circuits() {
        let p: Pending<i32> = Pending::ready_err(LspError::NotInitialized);
        match p.await {
            Err(LspError::NotInitialized) => {}
            other => panic!("expected NotInitialized, got {other:?}"),
        }
    }

    #[test]
    fn invocation_ids_are_monotonic() {
        let a = InvocationId::next().0;
        let b = InvocationId::next().0;
        assert!(b > a);
    }
}
