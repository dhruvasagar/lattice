//! Cancellation tokens for evaluator interruption (DESIGN.md §5.2.5).
//!
//! Every evaluator (motion / operator / text-object / ex-command)
//! that does meaningful work behind a loop receives a
//! [`CancellationToken`] and polls it on a regular cadence.
//!
//! The token is a clone-cheap `Arc<AtomicBool>`. Setting it costs
//! one atomic store; reading is one atomic load. The DESIGN.md
//! contract requires evaluators to observe a flip within 100µs --
//! polling once per inner-loop iteration is more than enough on
//! modern CPUs (a polled load is ~ns).
//!
//! # Sources of cancellation
//!
//! - **User Esc.** While an evaluator is running on the document
//!   actor, the user can interrupt by pressing Esc. The TUI input
//!   loop flips the token of the in-flight invocation.
//! - **Deadline timer.** Per `LatencyClass` budget (Reflex < 2ms,
//!   Display < 10ms). NOT YET WIRED in v1 -- the token type is in
//!   place so deadline plumbing layers on without touching
//!   evaluators. v1 uses user-Esc cancellation only.
//! - **Supersede.** A newer same-event request invalidates the
//!   in-flight one (e.g. a newer `CompletionRequested` cancels the
//!   prior LSP request). Used by Display-class commands; Reflex
//!   commands don't supersede.
//!
//! # Cancellation semantics (§5.2.5)
//!
//! On observation of a flipped token an evaluator returns
//! [`crate::CommandError::Cancelled`]. The dispatcher and the
//! actor see this as a soft failure: **no `Effect` is committed**,
//! the document stays at its pre-call version, and any
//! caller-observable state (cursor, selections, undo stack) is
//! unchanged. From the user's perspective the keystroke had no
//! effect -- which is the correct framing since they cancelled it.
//!
//! # Default token
//!
//! Callers that don't care about cancellation use
//! [`CancellationToken::never`] -- a singleton-style token that
//! can never be cancelled. Spawned via clone of a static
//! `AtomicBool::new(false)`; cost is identical to a custom token,
//! which keeps the evaluator polling logic uniform.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{CommandError, GrammarResult};

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
    /// default for callers that don't drive cancellation. Two
    /// `never()` tokens are independent (each owns its own
    /// `AtomicBool`); calling `cancel()` on one has no effect on
    /// other clones unless they were `Clone`'d from the same
    /// `Arc`. The "can never be cancelled" guarantee comes from
    /// not exposing the token after construction -- the caller
    /// keeps it private and never flips it.
    pub fn never() -> Self {
        Self::default()
    }

    /// Flip the token. Subsequent `is_cancelled` / `check` calls
    /// (on this clone or any other) will observe the cancellation.
    pub fn cancel(&self) {
        // Release ordering so any state the canceller wrote before
        // flipping is visible to readers that observe the flag.
        self.flag.store(true, Ordering::Release);
    }

    /// Cheap (one atomic load) check. Returns `true` iff the
    /// token has been cancelled by some clone.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// Convenience for evaluator hot loops: returns
    /// `Err(CommandError::Cancelled)` if the token has been
    /// flipped, `Ok(())` otherwise. The `?` operator threads it
    /// through naturally:
    ///
    /// ```ignore
    /// for chunk in chunks {
    ///     ctx.token.check()?;
    ///     // ... work ...
    /// }
    /// ```
    pub fn check(&self) -> GrammarResult<()> {
        if self.is_cancelled() {
            Err(CommandError::Cancelled)
        } else {
            Ok(())
        }
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
        assert!(t.check().is_ok());
    }

    #[test]
    fn cancel_flips_observed_state() {
        let t = CancellationToken::new();
        t.cancel();
        assert!(t.is_cancelled());
        assert!(matches!(t.check(), Err(CommandError::Cancelled)));
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
