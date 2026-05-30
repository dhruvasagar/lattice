//! `diff-mode` minor mode + the host-side bridge that toggles
//! it on participating buffers as `DiffSession`s open and close.
//!
//! ## The invariant
//!
//! A buffer participates in any [`DiffSession`] ⟺ `diff-mode`
//! is active on that buffer. The toggle lives inside the
//! [`DiffSubsystem`] (`register_with_sources` /
//! `drop_session`) so every present and future `:diff*`
//! ex-command (`:diff <buf>`, `:diffthis`, `:diffsplit`,
//! future `:Gdiff` D.7, future `:diff-accept` / `:diff-reject`
//! D.6) and the doc-close auto-drop
//! (`note_buffer_closed → drop_session`) inherit the toggle
//! without each call site repeating the activation logic.
//! See `docs/dev/architecture/diff-system.md` §3.4.7.
//!
//! ## Why a bridge instead of a direct call
//!
//! The subsystem holds an `Arc<DiffSubsystem>` and may be
//! invoked from a tokio worker (the bus-subscription drainer
//! that translates `DocumentClosed` events into
//! `drop_session`). The mode-activation API
//! (`ModeRegistry::activate_minor` / `deactivate_minor`)
//! needs `&mut ActiveModes` per buffer plus references to
//! mode-guards, config, event bus, and services — all of which
//! live on `Editor` and aren't shareable across threads.
//!
//! The bridge resolves this by queueing mode changes from any
//! thread into a `Mutex<Vec<DiffModeChange>>` and exposing a
//! `drain_pending` call the editor invokes from the dispatch
//! tail (`publish_render_state`). The dispatch tail applies
//! each change using the existing `mode_registry` pattern.
//!
//! ## Why ref-counted per buffer
//!
//! v1's `:diff*` flows don't intentionally put the same
//! buffer in two sessions, but `lookup_session_for` indexes
//! only the primary side. A baseline-side buffer can therefore
//! be staged into a second session without the existing
//! rejection check catching it; D.6 three-way will plausibly
//! see a `MergeBase` participant also diffed against working
//! tree. Naive "session opens → mode on / session closes →
//! mode off" would flip diff-mode off prematurely when the
//! first of two sessions sharing a buffer closes.
//!
//! The bridge therefore keeps a per-buffer refcount keyed by
//! the participating session keys. Activation appends the
//! session key to the buffer's bucket; deactivation removes
//! it; the bridge enqueues a `Deactivate` change only when
//! the bucket empties.

use lattice_core::BufferId;
use lattice_mode::registry::ModeRegistry;
use lattice_mode::{LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};
use std::collections::HashMap;
use std::sync::Mutex;

// ──────────────────────────────────────────────────────────────
// DiffMode — the minor mode itself
// ──────────────────────────────────────────────────────────────

/// `diff-mode` minor. Empty marker in v1 (D.5.a): the bit other
/// layers consult. D.5.b/c land the `do`/`dp` chord bindings
/// against the `MinorMode(ModeId::new("diff-mode"))` keymap
/// layer (K.1.b shape).
pub struct DiffMode;

impl DiffMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("diff-mode")
    }
}

impl Mode for DiffMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `diff-mode` against `registry`. Called from the
/// editor boot path alongside the other feature-crate
/// `register_*_modes` helpers.
pub fn register_diff_modes(registry: &mut ModeRegistry) {
    registry
        .register(DiffMode)
        .expect("diff-mode must register without conflict");
}

/// D.5.b/c (2026-05-30): chord bindings for the `diff-mode`
/// keymap layer.
///
/// - `do` → `action:diff-get` (D.5.b): rewrite the current
///   side's hunk to match the baseline.
/// - `dp` → `action:diff-put` (D.5.c): push the current
///   side's hunk into the peer buffer.
///
/// The layer is pushed once at editor boot under
/// `PushLayerKind::MinorMode(diff-mode)`; K.1.c's
/// per-keystroke filter gates the bindings so they only
/// fire on buffers where `diff-mode` is in
/// `ActiveModes.minors()`. Buffers without diff-mode active
/// fall through to the normal `d`-operator resolution — the
/// diff-mode bindings are invisible to them.
pub fn diff_mode_layer_bindings(
    actions: &crate::actions::ActionIds,
) -> std::collections::HashMap<
    crate::keymap::BindingMode,
    crate::keymap_trie::KeymapTrie,
> {
    use crate::chord::{KeyChord, KeyKind, KeyMods};
    use crate::keymap::BindingMode;
    use crate::keymap_trie::{
        BoundCommand, ChordPattern, KeymapLayer, KeymapTrie,
    };
    use lattice_grammar::CommandInvocation;
    use lattice_grammar::source::SourceLocation;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn lit_char(c: char) -> ChordPattern {
        ChordPattern::Literal(KeyChord {
            key: KeyKind::Char(c),
            mods: KeyMods::NONE,
        })
    }

    let layer = KeymapLayer::MinorMode(DiffMode::mode_id());
    let mut trie = KeymapTrie::new();
    trie.insert(
        &[lit_char('d'), lit_char('o')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(actions.diff_get),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );
    trie.insert(
        &[lit_char('d'), lit_char('p')],
        Arc::new(BoundCommand::from_invocation(
            CommandInvocation::of(actions.diff_put),
            SourceLocation::builtin_file(file!(), line!()),
            layer,
        )),
    );

    let mut modes = HashMap::new();
    modes.insert(BindingMode::Normal, trie);
    modes
}

// ──────────────────────────────────────────────────────────────
// DiffModeBridge — refcount + cross-thread queue
// ──────────────────────────────────────────────────────────────

/// Direction of a pending [`DiffModeChange`]. `Activate` adds
/// `diff-mode` to the buffer's `ActiveModes.minors()`;
/// `Deactivate` removes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffModeAction {
    Activate,
    Deactivate,
}

/// One queued toggle. Buffered until the dispatch tail drains
/// the bridge and applies them through the mode registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffModeChange {
    pub buffer: BufferId,
    pub action: DiffModeAction,
}

#[derive(Debug, Default)]
struct BridgeState {
    /// session_key → the participants that session activated
    /// for. Stored so `note_session_closed(session_key)` can
    /// look up which buffers to decrement without re-reading
    /// the descriptor (which has been removed by the time
    /// `drop_session` calls the bridge).
    sessions: HashMap<BufferId, Vec<BufferId>>,
    /// participant buffer → set of session keys currently
    /// holding it active. Refcount via vec length.
    refcounts: HashMap<BufferId, Vec<BufferId>>,
    /// Pending toggle queue drained from the dispatch tail.
    pending: Vec<DiffModeChange>,
}

/// Host-side bridge between [`DiffSubsystem`] session lifecycle
/// and the per-buffer `ActiveModes` set. See module docs for
/// the why.
#[derive(Debug, Default)]
pub struct DiffModeBridge {
    state: Mutex<BridgeState>,
}

impl DiffModeBridge {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that `session_key` opened with `participants` as
    /// the user-visible diff sides. For each participant whose
    /// refcount transitions 0 → 1, queue an `Activate` change.
    /// Idempotent on re-open of the same session_key
    /// (subsystem `register_with_sources` is itself idempotent
    /// on session identity).
    pub fn note_session_opened(&self, session_key: BufferId, participants: &[BufferId]) {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        // Re-open of a known session_key: scrub the previous
        // participants first so the refcount stays correct
        // (descriptor may have changed sides — D.4.d.3 plans
        // for `:diff-mode <inline|split|three-way>`).
        if let Some(old) = state.sessions.remove(&session_key) {
            for b in old {
                Self::dec_refcount(&mut state, session_key, b);
            }
        }
        let owned: Vec<BufferId> = participants.to_vec();
        state.sessions.insert(session_key, owned.clone());
        for b in owned {
            Self::inc_refcount(&mut state, session_key, b);
        }
    }

    /// Record that `session_key` closed. For each participant
    /// whose refcount drops to 0, queue a `Deactivate` change.
    /// No-op on unknown session keys (close-before-open or
    /// double-close from auto-drop racing the user's
    /// `:diffoff`).
    pub fn note_session_closed(&self, session_key: BufferId) {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        let Some(participants) = state.sessions.remove(&session_key) else {
            return;
        };
        for b in participants {
            Self::dec_refcount(&mut state, session_key, b);
        }
    }

    /// Drain every pending change. Callers (the dispatch tail)
    /// apply them via the mode registry, which mutates the
    /// per-buffer `ActiveModes`.
    pub fn drain_pending(&self) -> Vec<DiffModeChange> {
        let mut state = self.state.lock().expect("DiffModeBridge mutex poisoned");
        std::mem::take(&mut state.pending)
    }

    /// Test-only / introspection: current refcount for
    /// `buffer`. Returns 0 if no session activated diff-mode on
    /// it.
    pub fn refcount(&self, buffer: BufferId) -> usize {
        self.state
            .lock()
            .expect("DiffModeBridge mutex poisoned")
            .refcounts
            .get(&buffer)
            .map(Vec::len)
            .unwrap_or(0)
    }

    fn inc_refcount(state: &mut BridgeState, session_key: BufferId, buffer: BufferId) {
        let bucket = state.refcounts.entry(buffer).or_default();
        // Re-add of the same session_key is a no-op for the
        // refcount (idempotent re-open guard).
        if bucket.contains(&session_key) {
            return;
        }
        let was_empty = bucket.is_empty();
        bucket.push(session_key);
        if was_empty {
            state.pending.push(DiffModeChange {
                buffer,
                action: DiffModeAction::Activate,
            });
        }
    }

    fn dec_refcount(state: &mut BridgeState, session_key: BufferId, buffer: BufferId) {
        let Some(bucket) = state.refcounts.get_mut(&buffer) else {
            return;
        };
        if let Some(pos) = bucket.iter().position(|k| *k == session_key) {
            bucket.swap_remove(pos);
        }
        if bucket.is_empty() {
            state.refcounts.remove(&buffer);
            state.pending.push(DiffModeChange {
                buffer,
                action: DiffModeAction::Deactivate,
            });
        }
    }
}

// ──────────────────────────────────────────────────────────────
// Bridge tests — pure refcount semantics. End-to-end tests that
// drive the dispatch tail + assert ActiveModes live next to the
// `do_diff_open` / `register_two_pane_diff` call sites.
// ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn bid(n: u32) -> BufferId {
        BufferId(n)
    }

    #[test]
    fn open_session_queues_activates_for_each_participant() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().any(|c| c.buffer == bid(1)
            && c.action == DiffModeAction::Activate));
        assert!(changes.iter().any(|c| c.buffer == bid(2)
            && c.action == DiffModeAction::Activate));
    }

    #[test]
    fn close_session_queues_deactivates() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        let _ = bridge.drain_pending();
        bridge.note_session_closed(bid(2));
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|c| c.action == DiffModeAction::Deactivate));
        assert!(changes.iter().any(|c| c.buffer == bid(1)));
        assert!(changes.iter().any(|c| c.buffer == bid(2)));
    }

    #[test]
    fn close_unknown_session_is_noop() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_closed(bid(99));
        assert!(bridge.drain_pending().is_empty());
    }

    #[test]
    fn empty_participants_queues_nothing() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[]);
        assert!(bridge.drain_pending().is_empty());
        bridge.note_session_closed(bid(2));
        assert!(bridge.drain_pending().is_empty());
    }

    #[test]
    fn two_independent_sessions_do_not_interfere() {
        // Session A: file1 ↔ file2. Session B: file3 ↔ file4.
        // Each open queues two activates; each close queues
        // two deactivates. The buffers never refcount-overlap,
        // so the changes are independent.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        bridge.note_session_opened(bid(4), &[bid(3), bid(4)]);
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 4);

        bridge.note_session_closed(bid(2));
        let after_close_a = bridge.drain_pending();
        assert_eq!(after_close_a.len(), 2);
        assert!(after_close_a.iter().all(|c| c.action == DiffModeAction::Deactivate));
        assert!(after_close_a.iter().any(|c| c.buffer == bid(1)));
        assert!(after_close_a.iter().any(|c| c.buffer == bid(2)));

        // Session B is untouched.
        assert_eq!(bridge.refcount(bid(3)), 1);
        assert_eq!(bridge.refcount(bid(4)), 1);
        assert_eq!(bridge.refcount(bid(1)), 0);
        assert_eq!(bridge.refcount(bid(2)), 0);
    }

    #[test]
    fn shared_buffer_in_two_sessions_holds_mode_until_last_closes() {
        // Pathological but legal: file1 participates in both
        // session A (file1 ↔ file2) and session B (file1 ↔
        // file3). Closing A must NOT deactivate diff-mode on
        // file1 because B still needs it.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        bridge.note_session_opened(bid(3), &[bid(1), bid(3)]);
        let _ = bridge.drain_pending();

        assert_eq!(bridge.refcount(bid(1)), 2);
        assert_eq!(bridge.refcount(bid(2)), 1);
        assert_eq!(bridge.refcount(bid(3)), 1);

        // Close session A: file2 deactivates; file1 must NOT
        // (still held by session B).
        bridge.note_session_closed(bid(2));
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 1, "only file2 should deactivate, got {changes:?}");
        assert_eq!(changes[0].buffer, bid(2));
        assert_eq!(changes[0].action, DiffModeAction::Deactivate);
        assert_eq!(bridge.refcount(bid(1)), 1, "file1 still held by B");

        // Close session B: file1 + file3 deactivate.
        bridge.note_session_closed(bid(3));
        let changes = bridge.drain_pending();
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|c| c.action == DiffModeAction::Deactivate));
        assert!(changes.iter().any(|c| c.buffer == bid(1)));
        assert!(changes.iter().any(|c| c.buffer == bid(3)));
    }

    #[test]
    fn reopen_same_session_key_is_idempotent_on_refcount() {
        // `register_with_sources` is idempotent on session
        // identity; the bridge mirrors that — re-opening the
        // same session_key with the same participants must not
        // double-count.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        assert_eq!(bridge.refcount(bid(1)), 1);
        assert_eq!(bridge.refcount(bid(2)), 1);
    }

    #[test]
    fn reopen_with_different_participants_releases_old() {
        // A future `:diff-mode split` may swap a session's
        // participants. The bridge must scrub old refcounts
        // and account the new set.
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1), bid(2)]);
        let _ = bridge.drain_pending();

        // Swap baseline from file1 to file9.
        bridge.note_session_opened(bid(2), &[bid(9), bid(2)]);
        assert_eq!(bridge.refcount(bid(1)), 0, "old baseline released");
        assert_eq!(bridge.refcount(bid(9)), 1, "new baseline held");
        assert_eq!(bridge.refcount(bid(2)), 1, "primary still held");

        let changes = bridge.drain_pending();
        // Expect: file1 → Deactivate; file9 → Activate. file2's
        // refcount went 1 → 0 → 1 in one call, so its change
        // sequence is Deactivate + Activate.
        assert!(changes.iter().any(|c| c.buffer == bid(1)
            && c.action == DiffModeAction::Deactivate));
        assert!(changes.iter().any(|c| c.buffer == bid(9)
            && c.action == DiffModeAction::Activate));
    }

    #[test]
    fn drain_pending_is_idempotent() {
        let bridge = DiffModeBridge::new();
        bridge.note_session_opened(bid(2), &[bid(1)]);
        assert_eq!(bridge.drain_pending().len(), 1);
        assert!(bridge.drain_pending().is_empty(), "second drain empty");
    }
}
