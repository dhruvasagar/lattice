//! CG.2 — the foreground-cancellation seam, as a registered service.
//!
//! Design: `docs/dev/architecture/cancellation.md`; sequencing:
//! `docs/dev/operations/slice-plans/cancellation.md`.
//!
//! # Why a service rather than a method on `Editor`
//!
//! CG.1 put `arm_cancel()` on `Editor`, which needs `&mut`. That is
//! reachable from a provider's *initial* trigger — `open_search_view`
//! holds a `&mut dyn ModeActivator` — but not from the places that
//! respawn the same work later. Project search's `gr` refresh is an
//! `ActionHandlerRegistration` closure holding only `&self` services,
//! and it spawns a replacement scan; LSP requests (CG.3) and plugin
//! calls (CG.4) sit behind the same wall.
//!
//! Enrolling only where `&mut` happens to be available is the
//! half-migration this project keeps re-discovering: `<C-g>` would
//! cancel a *fresh* search and silently do nothing to a refreshed one,
//! and nothing about the code would say so. So the arming surface takes
//! `&self` and lives in the `ServiceRegistry`, where every subsystem
//! already looks things up.
//!
//! # Contract
//!
//! One token armed at a time. [`ForegroundCancel::arm`] cancels its
//! predecessor before handing out the new one, which means **supersede
//! is the same mechanism as cancel** — a second `:search` before the
//! first finishes abandons the first scan, and `gr` refreshing a view
//! no longer needs a private flag to do it.
//!
//! v1 is deliberately single-slot: a second user-initiated op cancels
//! whatever was running. `cancellation.md` §8 records why (and what a
//! stack would change) — the user pressed cancel, or started something
//! new, and either way the old work is unwanted.
//!
//! # Registration
//!
//! Register and look up under [`ForegroundCancelHandle`], never under
//! `ForegroundCancel`. `ServiceRegistry::register::<T>` keys on
//! `TypeId::of::<T>()`, so registering an `Arc<ForegroundCancel>` and
//! asking for a `ForegroundCancel` silently returns `None`.

use std::sync::{Arc, Mutex};

use lattice_protocol::CancellationToken;

/// The armed foreground token, shared between the host (which cancels)
/// and every subsystem that spawns cancellable work (which arms).
///
/// `Mutex` rather than `ArcSwap` because arming is a compare-and-swap
/// in spirit — cancel the old, install the new — and it happens once
/// per user-initiated operation, not per keystroke or per frame. There
/// is no hot-path read: the *token* is what gets polled in tight loops,
/// and that is a plain atomic the caller already holds by then.
#[derive(Debug, Default)]
pub struct ForegroundCancel {
    armed: Mutex<Option<CancellationToken>>,
}

/// Register and look up under this alias — see the module docs on the
/// `TypeId` pitfall.
pub type ForegroundCancelHandle = Arc<ForegroundCancel>;

impl ForegroundCancel {
    /// Arm a fresh token for a user-initiated operation, cancelling any
    /// predecessor, and hand back the clone the spawned task holds.
    ///
    /// Takes `&self` on purpose: the callers that need it most are
    /// action-handler closures and event subscriptions, which never see
    /// `&mut Editor`.
    ///
    /// A poisoned lock is treated as "nothing was armed" rather than a
    /// panic — losing the ability to cancel one operation is a far
    /// smaller failure than taking the editor down, and the fresh token
    /// this returns is still valid for the caller about to spawn.
    #[must_use = "the returned token must be handed to the spawned task, \
                  or the operation is unstoppable"]
    pub fn arm(&self) -> CancellationToken {
        let token = CancellationToken::new();
        match self.armed.lock() {
            Ok(mut slot) => {
                if let Some(previous) = slot.take() {
                    previous.cancel();
                }
                *slot = Some(token.clone());
            }
            Err(poisoned) => {
                tracing::warn!(
                    "foreground-cancel: lock poisoned; the previous operation \
                     cannot be cancelled, arming the new one anyway"
                );
                let mut slot = poisoned.into_inner();
                if let Some(previous) = slot.take() {
                    previous.cancel();
                }
                *slot = Some(token.clone());
            }
        }
        token
    }

    /// Cancel whatever is armed and clear the slot.
    ///
    /// Idempotent — cancelling nothing is the common case, since the
    /// binding is pressed far more often than an operation is running.
    pub fn cancel(&self) {
        let taken = match self.armed.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(token) = taken {
            token.cancel();
        }
    }

    /// Whether an operation is currently armed. For tests and for a
    /// future status indicator (`cancellation.md` §7).
    pub fn is_armed(&self) -> bool {
        match self.armed.lock() {
            Ok(slot) => slot.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    /// The predecessor must die when a new op arms. Without this, a
    /// second `:search` before the first completes leaves two scans
    /// racing to write the same view.
    #[test]
    fn arming_cancels_the_previous_operation() {
        let fc = ForegroundCancel::default();
        let first = fc.arm();
        let second = fc.arm();

        assert!(first.is_cancelled(), "the predecessor must be cancelled");
        assert!(!second.is_cancelled(), "the new token starts live");
        assert!(fc.is_armed());
    }

    #[test]
    fn cancel_flips_and_clears() {
        let fc = ForegroundCancel::default();
        let token = fc.arm();

        fc.cancel();

        assert!(token.is_cancelled());
        assert!(
            !fc.is_armed(),
            "a cancelled token must not stay armed, or the next arm() \
             would 'cancel' it a second time"
        );
    }

    /// The binding is pressed far more often than work is running.
    #[test]
    fn cancelling_when_idle_is_a_noop() {
        let fc = ForegroundCancel::default();
        fc.cancel();
        fc.cancel();
        assert!(!fc.is_armed());
    }

    /// Supersede and cancel are the SAME mechanism — this is what lets
    /// project search drop its private `AtomicBool` rather than run a
    /// second, parallel scheme that `<C-g>` would not reach.
    #[test]
    fn a_superseded_scan_and_a_cancelled_one_observe_the_same_flag() {
        let fc = ForegroundCancel::default();
        let scan = fc.arm();

        // `gr` refresh: arming the replacement supersedes the first.
        let refreshed = fc.arm();
        assert!(scan.is_cancelled());

        // `<C-g>` then reaches the REPLACEMENT, not just the original.
        fc.cancel();
        assert!(refreshed.is_cancelled());
    }

    /// Shared through an `Arc` across threads, which is how the host
    /// and a `spawn_blocking` scan actually hold it.
    #[test]
    fn the_handle_is_shareable_across_threads() {
        let fc: ForegroundCancelHandle = Arc::new(ForegroundCancel::default());
        let token = fc.arm();
        let other = Arc::clone(&fc);
        std::thread::spawn(move || other.cancel())
            .join()
            .expect("canceller thread");
        assert!(token.is_cancelled());
    }
}
