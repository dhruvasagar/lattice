//! Async runtime for lattice (DESIGN.md §5.2.1, §5.6.8, §5.7).
//!
//! Sits between [`lattice_core`] (the synchronous data layer -- `Buffer`,
//! `Document`, `Edit`) and [`lattice_ui_tui`] (rendering + input). Owns
//! three load-bearing async primitives:
//!
//! 1. **Document actor** ([`actor`]): one tokio task per open document.
//!    The task owns the writable [`lattice_core::Document`]; mutations
//!    arrive via a bounded mpsc mailbox. No callsite holds a lock on
//!    document state -- exclusive access is statically guaranteed by
//!    the actor pattern. Backpressure surfaces as
//!    [`RuntimeError::Busy`] when the mailbox is full.
//!
//! 2. **Document snapshots** ([`snapshot`]): after every committed
//!    mutation the actor builds an immutable [`DocumentSnapshot`] and
//!    publishes it to a single `arc_swap::ArcSwap` cell. Renderers
//!    read with one wait-free atomic load per visible document per
//!    frame and use that snapshot for the entire frame. There are no
//!    actor round-trips on the render hot path. (DESIGN.md §5.6.8.)
//!
//! 3. **`Pending<T>`** ([`pending`]): every mutating call returns a
//!    `Pending` -- a typed handle wrapping a oneshot receiver. Callers
//!    that want the result `await` (or `.blocking_recv()`); callers
//!    that don't (input loop, macro replay) drop it. This is the
//!    seam DESIGN.md §5.2.1 specifies.
//!
//! ## Why `lattice-grammar` stays sync
//!
//! `lattice_grammar::execute` is a pure function from
//! `(registry, document, cursor, invocation)` to `Effect`. Async
//! coordination is a runtime concern, not a grammar concern -- the
//! actor calls `execute` *inside* its own task, then publishes the
//! resulting snapshot. Grammar gets no tokio dependency, no async
//! signature, and no per-evaluator scheduling complexity.
//!
//! ## Tokio runtime
//!
//! A single multi-threaded runtime is created lazily on first use
//! ([`runtime::shared`]) and shared across the process. Tests share
//! it but isolate at the actor-task level: each [`spawn_document`]
//! call creates a fresh task with its own mailbox + snapshot.
//!
//! ## Cancellation
//!
//! [`DocumentHandle::dispatch_with_cancel`] threads a
//! [`lattice_grammar::CancellationToken`] into the grammar
//! `execute` call. The caller (App) holds a clone and flips it
//! (e.g. on user Esc) to short-circuit a long-running motion or
//! operator. The plain [`DocumentHandle::dispatch`] form uses a
//! no-op token; use it when no cancellation seam is required.
//!
//! ## What's NOT here in v1
//!
//! - **`LatencyClass` deadline timers** (DESIGN.md §5.2.5) --
//!   arrive when `CommandSpec` grows the field. v1 supports
//!   user-Esc cancellation only.
//! - **Veto / observation hook split** (DESIGN.md §5.2.1, §5.10) --
//!   needs the event-bus primitive that doesn't exist yet.
//! - **Multi-document / cross-document atoms** -- one actor per
//!   document, but the App still tracks a single document.

pub mod actor;
pub mod handle;
pub mod pending;
pub mod runtime;
pub mod snapshot;

pub use actor::DocumentActor;
pub use handle::{DocumentHandle, spawn_document};
pub use lattice_grammar::CancellationToken;
pub use pending::{InvocationId, Pending, RuntimeError};
pub use runtime::{block_on, shared_runtime};
pub use snapshot::{DocumentSnapshot, PublishedSnapshot};
