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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use ropey::Rope;
use tokio::task::JoinHandle;

use lattice_core::BufferId;
use lattice_diff::{compute_two_way, DiffAlgorithm, HunkIndex};

/// Source of the baseline a [`DiffSession`] diffs against.
///
/// D.2.b's only concrete impl is [`StaticBaseline`] (an
/// in-memory `Rope` snapshot). Future impls follow the same
/// trait shape:
/// - **D.4** `BufferBaseline` — snapshots a sibling pane's
///   document for side-by-side diff.
/// - **D.7** `GitBaseline` — reads the `HEAD:path` blob through
///   `gix` for `:Gdiff`.
/// - **D.6** `MergeBaseSource` — pulls the merge base for the
///   three-way conflict view.
///
/// `snapshot` is called from inside the `spawn_blocking` body
/// of [`DiffSubsystem::schedule_recompute`], so impls may do
/// cheap blocking I/O (a `git cat-file` for `GitBaseline`) but
/// must not hold the host's UI thread. `Send + Sync + 'static`
/// is required so the trait object can cross the
/// `spawn_blocking` boundary.
pub trait BaselineSource: Send + Sync + 'static + std::fmt::Debug {
	/// Produce the current baseline rope. Called once per
	/// recompute; the implementor decides whether to clone a
	/// cached rope or rematerialise from a backing store.
	fn snapshot(&self) -> Rope;
}

/// In-memory baseline — an owned `Rope` cloned on every
/// [`Self::snapshot`].
///
/// Cheap: `Rope::clone` is an `Arc`-share of the underlying
/// chunks, not a deep copy. Used as the default smoke-test
/// baseline and as the substrate consumers (e.g. an LSP server
/// returning `WorkspaceEdit` previews, the AI multi-file
/// `openDiff` flow) wrap when they already hold the baseline
/// text in memory.
#[derive(Debug, Clone)]
pub struct StaticBaseline {
	rope: Rope,
}

impl StaticBaseline {
	pub fn new(rope: Rope) -> Self {
		Self { rope }
	}
}

impl BaselineSource for StaticBaseline {
	fn snapshot(&self) -> Rope {
		self.rope.clone()
	}
}

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
	/// `register` time; changing algorithm is a drop + re-register.
	algorithm: DiffAlgorithm,
	/// Published hunks. Initialised to `HunkIndex::empty(algorithm)`
	/// (revision = 0) at construction; replaced by recompute via
	/// the revision-gated [`Self::try_publish_if_newer`] path.
	hunks: ArcSwap<HunkIndex>,
	/// D.2.b: monotonically-increasing revision allocator. Each
	/// recompute consumes one value via [`Self::allocate_revision`];
	/// the value stamps the resulting `HunkIndex` and is also
	/// what [`Self::try_publish_if_newer`] gates against. Starts
	/// at 1 (the initial empty index uses revision 0).
	next_revision: AtomicU64,
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
			next_revision: AtomicU64::new(1),
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

	/// Publish a freshly-computed `HunkIndex` unconditionally.
	/// D.2.a kept this as the simple "replace published" path
	/// for tests; D.2.b's recompute path prefers
	/// [`Self::try_publish_if_newer`] for monotonic ordering
	/// under concurrent `spawn_blocking` completion.
	pub fn publish(&self, hunks: Arc<HunkIndex>) {
		self.hunks.store(hunks);
	}

	/// D.2.b: allocate a fresh revision tag. Monotonically
	/// increasing; one tag per recompute.
	pub fn allocate_revision(&self) -> u64 {
		self.next_revision.fetch_add(1, Ordering::Relaxed)
	}

	/// D.2.b: peek the next revision the session would allocate.
	/// Test-friendly; does not consume the slot.
	pub fn peek_next_revision(&self) -> u64 {
		self.next_revision.load(Ordering::Relaxed)
	}

	/// D.2.b: publish `idx` only if its revision strictly exceeds
	/// the currently-published revision. Returns `true` on take,
	/// `false` if the publish was dropped as stale.
	///
	/// Preserves monotonic ordering when multiple recomputes run
	/// concurrently on `spawn_blocking` and may finish out of
	/// order. The `rcu` loop retries internally on contention; the
	/// `took` flag captures whichever decision the *winning*
	/// closure invocation made.
	pub fn try_publish_if_newer(&self, idx: Arc<HunkIndex>) -> bool {
		let new_rev = idx.revision;
		let mut took = false;
		self.hunks.rcu(|current| {
			if new_rev > current.revision {
				took = true;
				Arc::clone(&idx)
			} else {
				took = false;
				Arc::clone(current)
			}
		});
		took
	}

	/// D.2.b: synchronous recompute. Allocates a revision, calls
	/// `lattice_diff::compute_two_way`, builds a `HunkIndex`
	/// stamped with the allocated revision + the session's
	/// algorithm, and publishes via the revision-gated path.
	///
	/// Returns `Some(idx)` on successful publish, `None` if a
	/// newer revision was already published (stale result
	/// dropped). This is the body the [`DiffSubsystem::schedule_recompute`]
	/// `spawn_blocking` closure executes; tests call it directly
	/// to exercise the compute path without tokio.
	pub fn recompute_blocking(
		&self,
		baseline: &Rope,
		current: &Rope,
	) -> Option<Arc<HunkIndex>> {
		let revision = self.allocate_revision();
		let raw = compute_two_way(baseline, current, self.algorithm);
		let idx = Arc::new(HunkIndex {
			hunks: raw.hunks,
			algorithm: self.algorithm,
			revision,
		});
		if self.try_publish_if_newer(Arc::clone(&idx)) {
			Some(idx)
		} else {
			None
		}
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

	/// D.2.b: schedule a recompute of `buffer_id`'s session on
	/// the tokio blocking pool. Returns `None` if no session is
	/// registered.
	///
	/// The returned `JoinHandle` resolves to the publish result
	/// (`Some(Arc<HunkIndex>)` on take, `None` if dropped as
	/// stale). Production callers can fire-and-forget — the
	/// session's `ArcSwap<HunkIndex>` is the source of truth and
	/// reads stay coherent regardless of whether anyone awaits
	/// the handle. Tests and `:describe-diff` await for
	/// observability.
	///
	/// Supersede semantics: when two recomputes are scheduled in
	/// rapid succession, both run to completion on the blocking
	/// pool — there is no abort. Whichever finishes second
	/// allocates the higher revision and wins the
	/// [`DiffSession::try_publish_if_newer`] gate; whichever
	/// finishes first either gets there first (and is then
	/// superseded by the second's publish) or loses the gate.
	/// Either way the final state is the latest scheduled
	/// recompute's hunks. D.2.c's debounce will eliminate most
	/// of the redundant spawn cost before it hits the pool.
	pub fn schedule_recompute(
		&self,
		buffer_id: BufferId,
		baseline: Arc<dyn BaselineSource>,
		current: Rope,
	) -> Option<JoinHandle<Option<Arc<HunkIndex>>>> {
		let session = self.lookup(buffer_id)?;
		Some(tokio::task::spawn_blocking(move || {
			let base = baseline.snapshot();
			session.recompute_blocking(&base, &current)
		}))
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

	// ──────────────────────────────────────────────────────────
	// D.2.b: BaselineSource + recompute + schedule
	// ──────────────────────────────────────────────────────────

	#[test]
	fn static_baseline_clones_rope_on_snapshot() {
		let base = StaticBaseline::new(Rope::from("alpha\nbeta\n"));
		let snap = base.snapshot();
		assert_eq!(snap.to_string(), "alpha\nbeta\n");
		// Second snapshot is independent.
		let snap2 = base.snapshot();
		assert_eq!(snap2.to_string(), "alpha\nbeta\n");
	}

	#[test]
	fn allocate_revision_is_monotonic() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		assert_eq!(s.peek_next_revision(), 1);
		assert_eq!(s.allocate_revision(), 1);
		assert_eq!(s.allocate_revision(), 2);
		assert_eq!(s.allocate_revision(), 3);
		assert_eq!(s.peek_next_revision(), 4);
	}

	#[test]
	fn recompute_blocking_on_identical_ropes_produces_empty_hunks() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		let r = Rope::from("alpha\nbeta\ngamma\n");
		let published = s
			.recompute_blocking(&r, &r)
			.expect("first publish should always take");
		assert!(published.is_empty());
		assert_eq!(published.algorithm, DiffAlgorithm::Histogram);
		assert_eq!(published.revision, 1);
		// And the session's published state matches.
		assert_eq!(s.current_hunks().revision, 1);
	}

	#[test]
	fn recompute_blocking_on_changed_ropes_produces_change_hunk() {
		use lattice_diff::HunkKind;
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		let a = Rope::from("alpha\nbeta\ngamma\n");
		let b = Rope::from("alpha\nBETA\ngamma\n");
		let idx = s
			.recompute_blocking(&a, &b)
			.expect("first publish should take");
		assert_eq!(idx.len(), 1);
		assert_eq!(idx.hunks[0].kind, HunkKind::Change);
		assert_eq!(idx.revision, 1);
	}

	#[test]
	fn revision_strictly_increases_across_recomputes() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		let a = Rope::from("alpha\n");
		let b = Rope::from("beta\n");
		let r1 = s.recompute_blocking(&a, &b).unwrap();
		let r2 = s.recompute_blocking(&a, &b).unwrap();
		let r3 = s.recompute_blocking(&a, &b).unwrap();
		assert_eq!(r1.revision, 1);
		assert_eq!(r2.revision, 2);
		assert_eq!(r3.revision, 3);
		// Final published state matches the last recompute.
		assert_eq!(s.current_hunks().revision, 3);
	}

	#[test]
	fn try_publish_if_newer_drops_stale_revision() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		// Land revision=5 first.
		let r5 = Arc::new(HunkIndex {
			hunks: Vec::new(),
			algorithm: DiffAlgorithm::Histogram,
			revision: 5,
		});
		assert!(s.try_publish_if_newer(r5));
		assert_eq!(s.current_hunks().revision, 5);

		// Stale revision=3 is dropped.
		let r3 = Arc::new(HunkIndex {
			hunks: Vec::new(),
			algorithm: DiffAlgorithm::Histogram,
			revision: 3,
		});
		assert!(!s.try_publish_if_newer(r3));
		assert_eq!(s.current_hunks().revision, 5);

		// Equal revision is also dropped (strict greater-than).
		let r5_again = Arc::new(HunkIndex {
			hunks: Vec::new(),
			algorithm: DiffAlgorithm::Histogram,
			revision: 5,
		});
		assert!(!s.try_publish_if_newer(r5_again));
		assert_eq!(s.current_hunks().revision, 5);

		// Newer revision lands.
		let r9 = Arc::new(HunkIndex {
			hunks: Vec::new(),
			algorithm: DiffAlgorithm::Histogram,
			revision: 9,
		});
		assert!(s.try_publish_if_newer(r9));
		assert_eq!(s.current_hunks().revision, 9);
	}

	#[test]
	fn recompute_blocking_returns_none_when_publish_is_stale() {
		// Force a stale outcome by landing a high revision first,
		// then running a recompute (which allocates revision=1)
		// — the gate drops it.
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		let high = Arc::new(HunkIndex {
			hunks: Vec::new(),
			algorithm: DiffAlgorithm::Histogram,
			revision: 100,
		});
		assert!(s.try_publish_if_newer(high));
		let r = Rope::from("x\n");
		let result = s.recompute_blocking(&r, &r);
		assert!(result.is_none(), "stale recompute should not publish");
		assert_eq!(s.current_hunks().revision, 100);
	}

	#[test]
	fn schedule_recompute_returns_none_for_unregistered_buffer() {
		let sub = DiffSubsystem::new();
		let baseline: Arc<dyn BaselineSource> =
			Arc::new(StaticBaseline::new(Rope::from("x\n")));
		let handle = sub.schedule_recompute(bid(999), baseline, Rope::from("y\n"));
		assert!(handle.is_none());
	}

	#[tokio::test]
	async fn schedule_recompute_runs_on_blocking_pool_and_publishes() {
		let sub = DiffSubsystem::new();
		let session = sub.register(bid(1), DiffAlgorithm::Histogram);
		let baseline: Arc<dyn BaselineSource> =
			Arc::new(StaticBaseline::new(Rope::from("alpha\nbeta\n")));
		let current = Rope::from("alpha\nBETA\n");

		let handle = sub
			.schedule_recompute(bid(1), baseline, current)
			.expect("registered buffer has a session");
		let result = handle.await.expect("blocking task didn't panic");
		let idx = result.expect("first recompute publishes");
		assert_eq!(idx.revision, 1);
		assert_eq!(idx.len(), 1);
		// And the session sees it via RCU read.
		assert_eq!(session.current_hunks().revision, 1);
	}

	#[tokio::test]
	async fn schedule_recompute_serial_pair_revisions_monotonic() {
		let sub = DiffSubsystem::new();
		let session = sub.register(bid(1), DiffAlgorithm::Histogram);
		let baseline: Arc<dyn BaselineSource> =
			Arc::new(StaticBaseline::new(Rope::from("alpha\n")));

		let h1 = sub
			.schedule_recompute(bid(1), Arc::clone(&baseline), Rope::from("alpha\n"))
			.unwrap();
		h1.await.unwrap().unwrap();

		let h2 = sub
			.schedule_recompute(bid(1), Arc::clone(&baseline), Rope::from("beta\n"))
			.unwrap();
		let idx2 = h2.await.unwrap().unwrap();

		assert_eq!(idx2.revision, 2);
		assert_eq!(session.current_hunks().revision, 2);
	}
}
