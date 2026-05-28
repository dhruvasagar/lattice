//! D.2.a (2026-05-28) — `DiffSubsystem` skeleton.
//!
//! Registry of [`DiffSession`] entries keyed by
//! [`BufferId`]. One session per diffed document. Sessions are
//! `Arc`-shared so consumers (the future inline overlay D.3,
//! side-by-side D.4, hunk-transfer ops D.5, `:describe-diff`
//! D.2.d) can hold a stable handle while the registry continues
//! to mutate around them.
//!
//! ## What this slice lands (D.2.a)
//!
//! - [`DiffSubsystem`] — `Mutex<HashMap<BufferId, Arc<DiffSession>>>`
//!   with `register` / `lookup` / `drop_session` / `iter_sessions`.
//! - [`DiffSession`] — published `ArcSwap<HunkIndex>` per buffer,
//!   algorithm tag, monotonic `revision` counter.
//! - No compute, no baseline, no edit-event subscription. Those
//!   land in D.2.b (compute + RCU publish on `spawn_blocking`) and
//!   D.2.c (edit-event subscription + debounce).
//!
//! ## Why a skeleton slice
//!
//! Per CLAUDE.md heuristic #1 ("best long-term fit beats easy
//! implementation"), the lifecycle surface is the load-bearing
//! contract — every later consumer reads through `lookup` and
//! enumerates through `iter_sessions`. Landing it first as a
//! standalone unit means D.2.b can drop in pure compute behind
//! the existing API without churning call sites.
//!
//! ## Concurrency model
//!
//! - Registry mutation (`register` / `drop_session`) goes through
//!   a `std::sync::Mutex`. Mutation is buffer-open / buffer-close
//!   frequency — never per-frame.
//! - Per-session published `hunks: ArcSwap<HunkIndex>` is read
//!   lock-free from any thread (the renderer, the upcoming
//!   `:describe-diff` query, etc.).
//! - The session's `Arc` itself is cloned out of the registry
//!   under the registry lock, then released. Holders may keep the
//!   `Arc` past a `drop_session` call — the registry forgets the
//!   entry, but in-flight readers see a coherent snapshot until
//!   they release their clone. This matches the standard
//!   `BufferRegistry` / `cells_matrix_cell` pattern in this crate.
//!
//! ## Design fragment
//!
//! [`../../../docs/dev/architecture/diff-system.md`](../../../docs/dev/architecture/diff-system.md)
//! §6 (subsystem) and §3.1 (data model). Synopsis in
//! `design.md` §5.13.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;

use lattice_core::BufferId;
use lattice_diff::{DiffAlgorithm, HunkIndex};

/// Per-document diff state. Wraps an `ArcSwap<HunkIndex>` so
/// consumers read the latest published hunks without holding the
/// registry lock.
///
/// Construction goes through [`DiffSubsystem::register`]; direct
/// instantiation is fine for unit tests but bypasses the registry.
#[derive(Debug)]
pub struct DiffSession {
	/// The buffer this session diffs. Stable across the session's
	/// lifetime — the registry guarantees one session per id.
	buffer_id: BufferId,
	/// Algorithm selected for this session's recomputes. Fixed at
	/// `register` time; changing algorithm is a drop + re-register
	/// (decision deferred to D.2.b once compute lands).
	algorithm: DiffAlgorithm,
	/// Published hunks. Initialised to `HunkIndex::empty(algorithm)`
	/// at construction and replaced by D.2.b's recompute path.
	hunks: ArcSwap<HunkIndex>,
}

impl DiffSession {
	/// Public constructor used by both [`DiffSubsystem::register`]
	/// and tests. Starts with an empty `HunkIndex` tagged with the
	/// session's algorithm.
	pub fn new(buffer_id: BufferId, algorithm: DiffAlgorithm) -> Self {
		Self {
			buffer_id,
			algorithm,
			hunks: ArcSwap::from_pointee(HunkIndex::empty(algorithm)),
		}
	}

	pub fn buffer_id(&self) -> BufferId {
		self.buffer_id
	}

	pub fn algorithm(&self) -> DiffAlgorithm {
		self.algorithm
	}

	/// Snapshot the latest published hunks. Lock-free; returns a
	/// fresh `Arc` whose contents are stable for the holder's
	/// lifetime (RCU semantics).
	pub fn current_hunks(&self) -> Arc<HunkIndex> {
		self.hunks.load_full()
	}

	/// Publish a freshly-computed `HunkIndex`. D.2.b's recompute
	/// path is the production caller; tests use it to verify the
	/// RCU read path.
	///
	/// Intentionally `pub` (not `pub(crate)`) so the future
	/// `diff_worker` (D.2.b) — which may live in a different
	/// module — can publish without a backdoor. Recompute
	/// orchestration in D.2.b owns deciding *when* to call this;
	/// the method itself is just a typed `ArcSwap::store`.
	pub fn publish(&self, hunks: Arc<HunkIndex>) {
		self.hunks.store(hunks);
	}
}

/// Process-wide registry of [`DiffSession`] entries.
///
/// Lifecycle:
/// - `register(buffer_id, algorithm)` — idempotent. Returns the
///   existing session if one is already registered for the id;
///   the requested `algorithm` is ignored in that case (changing
///   algorithm is a drop + re-register). Otherwise inserts a fresh
///   session and returns its handle.
/// - `lookup(buffer_id)` — returns `Some(Arc<DiffSession>)` if a
///   session is registered, else `None`.
/// - `drop_session(buffer_id)` — removes the registry entry.
///   Returns `true` if an entry was removed. In-flight `Arc`
///   holders are unaffected.
/// - `iter_sessions()` — snapshot of all currently-registered
///   sessions. Powers `:describe-diff` (D.2.d) and is the
///   enumeration surface for D.6 `:diffput <bufnr>` /
///   `:diffget <bufnr>`.
///
/// The registry is `Default`-able and zero-cost to construct; the
/// host owns one instance, threaded through `Editor`.
#[derive(Debug, Default)]
pub struct DiffSubsystem {
	sessions: Mutex<HashMap<BufferId, Arc<DiffSession>>>,
}

impl DiffSubsystem {
	pub fn new() -> Self {
		Self::default()
	}

	/// Register a session for `buffer_id`. Idempotent: returns the
	/// existing `Arc<DiffSession>` if one is already registered
	/// (the `algorithm` argument is ignored in that case).
	pub fn register(
		&self,
		buffer_id: BufferId,
		algorithm: DiffAlgorithm,
	) -> Arc<DiffSession> {
		let mut sessions = self.sessions.lock().expect("DiffSubsystem mutex poisoned");
		sessions
			.entry(buffer_id)
			.or_insert_with(|| Arc::new(DiffSession::new(buffer_id, algorithm)))
			.clone()
	}

	/// Look up the session for `buffer_id`. Returns `None` if no
	/// session is registered.
	pub fn lookup(&self, buffer_id: BufferId) -> Option<Arc<DiffSession>> {
		self.sessions
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.get(&buffer_id)
			.cloned()
	}

	/// Drop the registry entry for `buffer_id`. Returns `true` if
	/// an entry was removed. Called from buffer-close lifecycle
	/// in D.2.c; safe to call on a non-registered id.
	pub fn drop_session(&self, buffer_id: BufferId) -> bool {
		self.sessions
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.remove(&buffer_id)
			.is_some()
	}

	/// `true` if no sessions are registered. Test-friendly.
	pub fn is_empty(&self) -> bool {
		self.sessions
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.is_empty()
	}

	/// Number of registered sessions. Test-friendly.
	pub fn len(&self) -> usize {
		self.sessions
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.len()
	}

	/// Snapshot of all currently-registered sessions. Returns
	/// fresh `Arc` clones — callers may hold them past a
	/// concurrent `drop_session` without affecting registry
	/// state. Order is unspecified (HashMap iteration); D.2.d
	/// sorts for display.
	pub fn iter_sessions(&self) -> Vec<Arc<DiffSession>> {
		self.sessions
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.values()
			.cloned()
			.collect()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn bid(n: u32) -> BufferId {
		BufferId(n)
	}

	#[test]
	fn fresh_registry_is_empty() {
		let sub = DiffSubsystem::new();
		assert!(sub.is_empty());
		assert_eq!(sub.len(), 0);
		assert!(sub.lookup(bid(1)).is_none());
	}

	#[test]
	fn register_returns_session_and_grows_registry() {
		let sub = DiffSubsystem::new();
		let s = sub.register(bid(1), DiffAlgorithm::Histogram);
		assert_eq!(s.buffer_id(), bid(1));
		assert_eq!(s.algorithm(), DiffAlgorithm::Histogram);
		assert_eq!(sub.len(), 1);
		assert!(!sub.is_empty());
	}

	#[test]
	fn register_is_idempotent() {
		let sub = DiffSubsystem::new();
		let first = sub.register(bid(7), DiffAlgorithm::Histogram);
		let second = sub.register(bid(7), DiffAlgorithm::Myers);
		// Same Arc — registry returns the existing session and
		// ignores the second algorithm argument.
		assert!(Arc::ptr_eq(&first, &second));
		assert_eq!(second.algorithm(), DiffAlgorithm::Histogram);
		assert_eq!(sub.len(), 1);
	}

	#[test]
	fn distinct_buffers_get_distinct_sessions() {
		let sub = DiffSubsystem::new();
		let a = sub.register(bid(1), DiffAlgorithm::Histogram);
		let b = sub.register(bid(2), DiffAlgorithm::Histogram);
		assert!(!Arc::ptr_eq(&a, &b));
		assert_eq!(sub.len(), 2);
	}

	#[test]
	fn lookup_returns_same_arc_as_register() {
		let sub = DiffSubsystem::new();
		let registered = sub.register(bid(1), DiffAlgorithm::Histogram);
		let looked_up = sub.lookup(bid(1)).expect("session should be present");
		assert!(Arc::ptr_eq(&registered, &looked_up));
	}

	#[test]
	fn drop_session_removes_entry_and_returns_true_only_once() {
		let sub = DiffSubsystem::new();
		sub.register(bid(1), DiffAlgorithm::Histogram);
		assert!(sub.drop_session(bid(1)));
		assert!(sub.is_empty());
		assert!(sub.lookup(bid(1)).is_none());
		// Second drop is a no-op.
		assert!(!sub.drop_session(bid(1)));
	}

	#[test]
	fn drop_does_not_invalidate_held_arc() {
		let sub = DiffSubsystem::new();
		let held = sub.register(bid(1), DiffAlgorithm::Histogram);
		sub.drop_session(bid(1));
		// Caller still has a coherent session — the registry just
		// forgot the entry.
		assert_eq!(held.buffer_id(), bid(1));
		let snap = held.current_hunks();
		assert!(snap.is_empty());
	}

	#[test]
	fn iter_sessions_enumerates_all_registered() {
		let sub = DiffSubsystem::new();
		sub.register(bid(1), DiffAlgorithm::Histogram);
		sub.register(bid(2), DiffAlgorithm::Histogram);
		sub.register(bid(3), DiffAlgorithm::Myers);
		let mut ids: Vec<BufferId> =
			sub.iter_sessions().iter().map(|s| s.buffer_id()).collect();
		ids.sort();
		assert_eq!(ids, vec![bid(1), bid(2), bid(3)]);
	}

	#[test]
	fn session_starts_with_empty_hunks_tagged_with_algorithm() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::MyersMinimal);
		let snap = s.current_hunks();
		assert!(snap.is_empty());
		assert_eq!(snap.algorithm, DiffAlgorithm::MyersMinimal);
		assert_eq!(snap.revision, 0);
	}

	#[test]
	fn publish_replaces_current_hunks() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		let new_idx = Arc::new(HunkIndex {
			hunks: Vec::new(),
			algorithm: DiffAlgorithm::Histogram,
			revision: 42,
		});
		s.publish(new_idx);
		let snap = s.current_hunks();
		assert_eq!(snap.revision, 42);
	}

	#[test]
	fn publish_is_visible_across_arc_clones() {
		// RCU semantics: two clones of the session see the same
		// latest publish.
		let s = Arc::new(DiffSession::new(bid(1), DiffAlgorithm::Histogram));
		let reader = Arc::clone(&s);
		s.publish(Arc::new(HunkIndex {
			hunks: Vec::new(),
			algorithm: DiffAlgorithm::Histogram,
			revision: 9,
		}));
		assert_eq!(reader.current_hunks().revision, 9);
	}
}
