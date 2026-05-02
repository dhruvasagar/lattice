//! Cancellation tokens for evaluator interruption (DESIGN.md §5.2.5).
//!
//! The primitive lives at the protocol layer (so search loops in
//! [`lattice_core`] can poll it without depending on grammar). This
//! module re-exports it as [`CancellationToken`] and adds the
//! grammar-domain `check()` short-circuit that converts a flipped
//! token into [`crate::CommandError::Cancelled`].
//!
//! Every evaluator (motion / operator / text-object / ex-command)
//! that does meaningful work behind a loop receives a token via its
//! context struct and polls it on a regular cadence. The DESIGN.md
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
//!   prior LSP request). NOT YET WIRED.
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

use crate::error::{CommandError, GrammarResult};

/// Re-export of the protocol-layer cancellation primitive. Grammar
/// callers access it under this name; lower crates (search loops in
/// `lattice-core`) use [`lattice_protocol::CancellationToken`]
/// directly. The two are the same type.
pub use lattice_protocol::CancellationToken;

/// Grammar extension: convert a flipped token into a
/// [`CommandError::Cancelled`] result. The `?` operator threads it
/// through naturally:
///
/// ```ignore
/// for chunk in chunks {
///     ctx.cancel.check()?;
///     // ... work ...
/// }
/// ```
pub trait CheckCancelled {
    fn check(&self) -> GrammarResult<()>;
}

impl CheckCancelled for CancellationToken {
    fn check(&self) -> GrammarResult<()> {
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
    fn check_on_fresh_token_is_ok() {
        let t = CancellationToken::new();
        assert!(t.check().is_ok());
    }

    #[test]
    fn check_on_flipped_token_returns_cancelled() {
        let t = CancellationToken::new();
        t.cancel();
        assert!(matches!(t.check(), Err(CommandError::Cancelled)));
    }
}
