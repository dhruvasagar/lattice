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

// Phase 5.5.LSP.1: a second multi-threaded runtime dedicated to
// LSP supervisor + per-server actors + read/write loops +
// diagnostic pumps. Kept separate from `SHARED` so a slow LSP
// server can't starve the document-actor scheduling band. The
// helper used to live in `lattice_ui_tui::runtime` -- moving it
// here lets `lattice_host` host-side dispatchers spawn LSP
// requests without a back-edge through the renderer crate.
// Consolidating onto a single shared runtime is a deliberate
// post-1.0 decision; for now the two-runtime topology is
// preserved verbatim.
static LSP_RUNTIME: OnceLock<Runtime> = OnceLock::new();

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

/// Fire-and-forget spawn onto the shared runtime. The future
/// runs on a tokio worker thread; the returned `JoinHandle` is
/// detached (the caller doesn't await). Used by the mode
/// dispatcher (M-async.2): activation validation runs
/// synchronously on the App thread, then the lifecycle future
/// is `spawn_task`'d so the App thread doesn't block on the
/// future's `.await` points.
pub fn spawn_task<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    shared_runtime().spawn(fut)
}

/// Phase 5.5.LSP.1: shared LSP runtime accessor. Multi-threaded,
/// thread-name `lattice-lsp`. Owns the supervisor actor + per-
/// server actors + read/write loops + diagnostic pumps + the
/// debounced flush task. Survives for the editor's lifetime.
///
/// Used by `App::new` to hand the runtime's handle to
/// `LspSupervisor::spawn` (the supervisor's command-mailbox
/// semantics require an explicit runtime affinity) and by every
/// per-feature dispatcher (hover, definition, references, ...)
/// that needs to fire a request *off* the UI thread.
pub fn lsp_runtime() -> &'static Runtime {
    LSP_RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .thread_name("lattice-lsp")
            .build()
            .expect("LSP tokio runtime should build")
    })
}

/// Phase 5.5.LSP.1: spawn a fire-and-forget future on the shared
/// LSP runtime. Used by the App's + host's per-feature LSP
/// dispatchers so the request awaits the actor's response *off*
/// the main UI thread; the result flows back through a per-
/// feature mpsc channel that the App drains before each draw.
///
/// Returning a `JoinHandle` lets the caller cancel by dropping
/// it -- though for LSP cooperative cancellation runs through
/// the `CancellationToken` plumbed into the typed wrappers, so
/// the handle is mostly informational.
pub fn spawn_on_lsp_runtime<F>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    lsp_runtime().spawn(future)
}

/// Sync-to-async bridge. Forwards to the shared multi-thread
/// runtime's `block_on`. Used by the TUI input loop and by App
/// methods that need to wait on a [`crate::Pending`] from outside
/// an async context.
///
/// **Three execution contexts** to handle:
///
/// 1. **Non-tokio caller** (sync `main`, sync test): no current
///    handle, fall through to `target.block_on(fut)` directly.
/// 2. **Multi-thread tokio caller** (e.g. spawned task on the
///    shared LSP runtime): relinquish the worker via
///    [`tokio::task::block_in_place`] so other tasks keep running
///    while we block on `target`.
/// 3. **Non-multi-thread tokio caller** (e.g. the editor actor's
///    dedicated `current_thread` runtime per slice
///    `3c.final.E.swap`): `block_in_place` panics here because
///    the current runtime isn't `MultiThread`; we instead escape
///    to a fresh OS thread via [`std::thread::scope`] and drive
///    the future on `target` from outside any tokio context.
///
/// The third case is the fix for slice `3c.fixup.actor-block-on`:
/// before this, `block_on` calls from inside the editor actor's
/// runtime (file save, `document.dispatch_with_cancel`, LSP
/// completion-resolve, code-action apply, synthetic-buffer seed)
/// panicked with "can call blocking only when running on the
/// multi-threaded runtime" — caught only by `cargo bench`
/// (release builds), not by `cargo test` (which preserves direct
/// `App.editor: Editor` via the `cfg(test)` escape hatch and
/// thus never spawns the actor).
///
/// **Send bound**: `F: Send` + `F::Output: Send` are required by
/// `std::thread::scope` in the third case. Every existing caller
/// satisfies these (Arc-backed handles, owned move-captures); if
/// a future caller doesn't, the type system catches it at the
/// call site.
pub fn block_on<F>(fut: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    let target = shared_runtime();
    match Handle::try_current() {
        Ok(handle)
            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::MultiThread
            ) =>
        {
            // Already inside a multi-thread runtime -- relinquish
            // the worker before driving `fut` on `target`.
            tokio::task::block_in_place(|| target.block_on(fut))
        }
        Ok(_) => {
            // Inside a non-multi-thread runtime (e.g. the editor
            // actor's `current_thread`). `block_in_place` would
            // panic; re-entering `target.block_on` from inside
            // the current runtime would also panic. Escape to a
            // fresh OS thread (no tokio context) so `target.block_on`
            // runs cleanly. `std::thread::scope` lets us borrow
            // non-`'static` data from the future without copying.
            std::thread::scope(|s| {
                s.spawn(|| target.block_on(fut))
                    .join()
                    .expect("nested-block_on bridge thread completed")
            })
        }
        Err(_) => target.block_on(fut),
    }
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
