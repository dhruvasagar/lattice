//! Cooperative cancellation primitive (DESIGN.md §5.2.5).
//!
//! Lives at the protocol layer so every other crate can poll it
//! without taking a dependency on grammar. The grammar crate re-
//! exports this type as `CancellationToken` and adds the
//! `CommandError::Cancelled`-aware `check()` short-circuit on top.
//!
//! The token is a clone-cheap `Arc<AtomicBool>`. Cancelling costs
//! one atomic store; polling is one atomic load. Evaluators
//! (motions, operators, search loops, ex commands) poll on a
//! regular cadence -- typically once per inner-loop iteration --
//! and bail with the appropriate domain error on a flipped token.
//!
//! # Sources of cancellation
//!
//! - **User Esc.** While an evaluator runs on the document actor,
//!   the user can interrupt by pressing Esc. The TUI input loop
//!   flips the token of the in-flight invocation.
//! - **Deadline timer** (NOT YET WIRED). Per `LatencyClass` budget.
//! - **Supersede** (NOT YET WIRED). A newer same-event request
//!   invalidates the in-flight one.
//!
//! # Default token
//!
//! [`CancellationToken::never`] returns a token that callers have
//! no handle to flip. Use it when the call site doesn't drive
//! cancellation but the API requires a token.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cooperative cancellation handle. Cheap to clone (one Arc bump);
/// safe to share across threads / tasks.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    flag: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Build a fresh, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a token that can never be cancelled. Useful as a
    /// default for callers that don't drive cancellation.
    pub fn never() -> Self {
        Self::default()
    }

    /// Flip the token. Subsequent `is_cancelled` calls (on this
    /// clone or any other) will observe the cancellation.
    pub fn cancel(&self) {
        // Release so any state the canceller wrote before flipping
        // is visible to readers that observe the flag.
        self.flag.store(true, Ordering::Release);
    }

    /// Cheap (one atomic load) check. Returns `true` iff the
    /// token has been cancelled by some clone.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn fresh_token_is_not_cancelled() {
        let t = CancellationToken::new();
        assert!(!t.is_cancelled());
    }

    #[test]
    fn cancel_flips_observed_state() {
        let t = CancellationToken::new();
        t.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn clone_observes_same_state() {
        let t = CancellationToken::new();
        let t2 = t.clone();
        t.cancel();
        assert!(t2.is_cancelled());
    }

    #[test]
    fn cancel_via_clone_observed_by_original() {
        let t = CancellationToken::new();
        let t2 = t.clone();
        t2.cancel();
        assert!(t.is_cancelled());
    }

    #[test]
    fn never_token_starts_uncancelled() {
        let t = CancellationToken::never();
        assert!(!t.is_cancelled());
    }
}
