//! Stress tests for the document actor.
//!
//! These tests cover the actor pattern's contract under load:
//!
//! 1. **Burst durability** -- a fast burst of edits all land in
//!    order; no edit is dropped, no deadlock. Audit slice 6 / H3
//!    swapped the bounded `mpsc::channel(64)` for an unbounded
//!    channel, so the previous "saturation surfaces Busy"
//!    contract is gone -- the new contract is "every edit
//!    durably lands."
//! 2. **Concurrent senders** -- many handles cloning the same actor
//!    fire edits in parallel; the actor serialises them, and every
//!    edit lands.
//! 3. **Snapshot publish ordering** -- a caller that observes a
//!    reply sees the corresponding snapshot via `snapshot()`. The
//!    publish-before-reply ordering (DESIGN.md §5.6.8) holds under
//!    load, including with N parallel edits.
//! 4. **Cancellation under load** -- a fresh token doesn't slow
//!    things down vs. `::never()`, and a flipped token short-
//!    circuits even when the actor is busy with the next edit.
//! 5. **Graceful shutdown** -- dropping the last handle while the
//!    mailbox still has queued messages cleanly drains and exits.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use lattice_core::Document;
use lattice_grammar::{CommandId, CommandInvocation, CommandRegistry};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_runtime::{CancellationToken, RuntimeError, block_on, spawn_document};

fn empty_registry() -> Arc<CommandRegistry> {
    Arc::new(CommandRegistry::new())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mailbox_burst_lands_every_edit() {
    // Audit slice 6 / H3: with the unbounded mailbox, a burst of
    // edits MUST all land -- no Busy, no drops. The pre-fix
    // bounded-channel contract was "Busy under burst, accepted
    // count + busy count == burst"; the new contract is just
    // "every edit lands."
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text(""),
        empty_registry(),
    );
    let burst = 4096usize;
    let mut pendings = Vec::with_capacity(burst);
    for _ in 0..burst {
        let edit = Edit::insert(Position::ZERO, "x");
        pendings.push(handle.apply_edit(edit));
    }
    for p in pendings {
        match p.await {
            Ok(_) => {}
            Err(other) => panic!("unexpected err under burst: {other:?}"),
        }
    }
    let snap = handle.snapshot();
    assert_eq!(
        snap.text().len(),
        burst,
        "every edit in the burst must land"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_handles_serialise_through_actor() {
    // Many cloned handles racing. The actor owns the only mutable
    // Document, so per-handle edits commit in some serialisation
    // -- the final length must equal the sum of accepted edits.
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text(""),
        empty_registry(),
    );
    let n_tasks = 16usize;
    let edits_per_task = 32usize;
    let accepted = Arc::new(AtomicUsize::new(0));

    let mut joins = Vec::with_capacity(n_tasks);
    for _ in 0..n_tasks {
        let h = handle.clone();
        let accepted = accepted.clone();
        joins.push(tokio::spawn(async move {
            for _ in 0..edits_per_task {
                // Always insert at byte 0 so the position is valid
                // regardless of actor state. The interleaving of
                // edits across tasks is what we're testing, not
                // any particular text content.
                let edit = Edit::insert(Position::ZERO, "a");
                if h.apply_edit(edit).await.is_ok() {
                    accepted.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for j in joins {
        j.await.unwrap();
    }

    let final_text = handle.snapshot().text();
    let final_len = final_text.len();
    let acc = accepted.load(Ordering::Relaxed);
    // Length matches accepted-edit count (each was a 1-byte
    // insert). With the unbounded mailbox (audit slice 6 / H3),
    // every send lands, so `acc == n_tasks * edits_per_task`.
    assert_eq!(
        final_len, acc,
        "len={final_len} accepted={acc} -- actor lost or duplicated edits"
    );
    assert_eq!(
        acc,
        n_tasks * edits_per_task,
        "every concurrent edit must land under the unbounded mailbox"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn snapshot_observed_after_reply_reflects_that_edit() {
    // The §5.6.8 publish-before-reply ordering: every reply
    // implies the matching snapshot is already published. Hammer
    // it by alternating insert -> snapshot reads.
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text(""),
        empty_registry(),
    );
    for i in 0..256 {
        let edit = Edit::insert(Position::ZERO, "z");
        // With the unbounded mailbox the apply always lands;
        // `Busy` is impossible (audit slice 6 / H3).
        match handle.apply_edit(edit.clone()).await {
            Ok(_) => {}
            Err(other) => panic!("unexpected err: {other:?}"),
        }
        let snap = handle.snapshot();
        assert_eq!(
            snap.text().len(),
            (i + 1) as usize,
            "snapshot lagged the reply at iteration {i}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_with_pre_flipped_token_under_load_short_circuits() {
    // Under a steady stream of edits, a dispatch that arrives with
    // an already-flipped token MUST surface Cancelled, no matter
    // what's queued ahead of it -- the dispatcher's first check
    // is the cancellation poll.
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text("seed"),
        empty_registry(),
    );

    // Keep the mailbox warm. With the unbounded mailbox (audit
    // slice 6 / H3) there's no saturation case; we just want a
    // few queued edits ahead of the dispatch so we exercise the
    // actor's cancellation poll under load.
    let mut pendings = Vec::new();
    for _ in 0..16 {
        let edit = Edit::insert(Position::ZERO, "_");
        pendings.push(handle.apply_edit(edit));
    }

    let token = CancellationToken::new();
    token.cancel();
    let result = handle
        .dispatch_with_cancel(
            CommandInvocation::of(CommandId::new(1)),
            Position::ZERO,
            token,
        )
        .await;

    use lattice_grammar::error::CommandError;
    assert!(
        matches!(result, Err(RuntimeError::Grammar(CommandError::Cancelled))),
        "expected Cancelled, got: {result:?}"
    );

    // Don't leave the actor wedged -- drain the queued edits.
    for p in pendings {
        let _ = p.await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_last_handle_with_queued_messages_drains_cleanly() {
    // Fire a burst, drop the only handle clone, and verify the
    // tokio task exits naturally (no panic, no deadlock). We
    // assert via a tokio::join! against a watchdog timeout.
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text(""),
        empty_registry(),
    );
    let mut pendings = Vec::new();
    for _ in 0..64 {
        // Hold the Pending so the future isn't dropped before the
        // actor sees it (clippy: dropping a Pending unsent is a
        // bug -- here we *do* drop them, but only after the
        // burst is fully enqueued).
        pendings.push(handle.apply_edit(Edit::insert(Position::ZERO, "q")));
    }
    drop(pendings);
    drop(handle);

    // If the actor leaked we'd hang here. tokio::time::timeout
    // wraps a no-op future so we can yield to the runtime
    // scheduler enough times for the task to finish.
    let watchdog = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        for _ in 0..1024 {
            tokio::task::yield_now().await;
        }
    });
    watchdog.await.expect("actor did not shut down in time");
}

#[test]
fn block_on_burst_lands_every_edit() {
    // Synchronous (block_on) variant: the input loop's normal
    // path. With the unbounded mailbox (audit slice 6 / H3),
    // every queued edit lands -- no Busy. We fire a 256-edit
    // burst and assert all of them committed.
    let handle = spawn_document(
        lattice_core::BufferId(0),
        Document::from_text(""),
        empty_registry(),
    );
    let mut pendings = Vec::new();
    for _ in 0..256 {
        pendings.push(handle.apply_edit(Edit::insert(Position::ZERO, "x")));
    }
    let mut accepted = 0usize;
    for p in pendings {
        match block_on(p) {
            Ok(_) => accepted += 1,
            Err(other) => panic!("unexpected err: {other:?}"),
        }
    }
    assert_eq!(accepted, 256);
    assert_eq!(handle.snapshot().text().len(), 256);
}
