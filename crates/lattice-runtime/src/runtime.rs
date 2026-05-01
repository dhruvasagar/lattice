//! Tokio runtime singleton shared across the process.
//!
//! Exactly one multi-threaded `tokio::runtime::Runtime` is created
//! lazily on first call to [`shared_runtime`]. All document actors
//! spawn onto it; sync callers (the TUI input loop, tests) bridge
//! into async via [`block_on`] which forwards to
//! [`tokio::runtime::Handle::block_on`].
//!
//! Why a singleton:
//!
//! - The runtime owns its own threadpool. Spawning per-test or
//!   per-App runtimes is wasteful and serialises tests poorly.
//! - All actor-bound code paths share the same scheduler; cross-
//!   actor interactions (post-Phase-7 plugin host invoking the
//!   document actor) work without extra plumbing.
//! - Dropping a `Runtime` blocks until its tasks finish; a global
//!   one stays alive for the process lifetime.
//!
//! Why isolation across tests still works: each
//! [`crate::spawn_document`] call creates its own actor task with
//! its own mailbox. Tests don't share state at the actor level even
//! though they share the runtime.

use std::sync::OnceLock;

use tokio::runtime::{Builder, Handle, Runtime};

static SHARED: OnceLock<Runtime> = OnceLock::new();

/// Get (or lazily build) the shared runtime's `Handle`. Cheap to
/// call repeatedly; the underlying `Runtime` is built once and
/// stays alive for the process lifetime.
///
/// The runtime is multi-threaded with the default worker count
/// (`num_cpus::get()` per tokio's defaults). Two workers would be
/// enough for v1, but matching tokio's default keeps behaviour
/// predictable when LSP / plugin tasks land in Phase 4 / 7.
pub fn shared_runtime() -> &'static Handle {
    SHARED
        .get_or_init(|| {
            Builder::new_multi_thread()
                .enable_all()
                .thread_name("lattice-runtime")
                .build()
                .expect("tokio runtime build failed")
        })
        .handle()
}

/// Sync-to-async bridge. Forwards to the shared runtime's
/// `block_on`. Used by the TUI input loop and by App methods that
/// need to wait on a [`crate::Pending`] from outside an async
/// context.
///
/// Calling this from inside an async context (a `#[tokio::test]`,
/// for instance) panics with tokio's "cannot block_on inside a
/// runtime" message -- callers in async contexts should `await` the
/// `Pending` instead.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    shared_runtime().block_on(fut)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn block_on_runs_a_future_and_returns_its_value() {
        let value = block_on(async { 1 + 2 });
        assert_eq!(value, 3);
    }

    #[test]
    fn shared_runtime_is_idempotent() {
        let h1 = shared_runtime();
        let h2 = shared_runtime();
        // Both handles refer to the same runtime; their `Handle::id`
        // representations are equal.
        assert_eq!(format!("{h1:?}"), format!("{h2:?}"));
    }
}
