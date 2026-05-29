//! D.2.a / D.2.b / D.2.c — `DiffSubsystem`.
//!
//! Registry + compute + routing layer for the diff subsystem.
//! Sessions are `Arc`-shared so consumers (the future inline
//! overlay D.3, side-by-side D.4, hunk-transfer ops D.5,
//! `:describe-diff` D.2.d) can hold a stable handle while the
//! registry continues to mutate around them.
//!
//! ## Slice landing order
//!
//! - **D.2.a (2026-05-28)** — registry skeleton. [`DiffSubsystem`]
//!   keying [`Arc<DiffSession>`] by [`BufferId`] behind a
//!   `std::sync::Mutex`. `register` / `lookup` / `drop_session` /
//!   `iter_sessions`. Per-session `ArcSwap<HunkIndex>` for
//!   RCU-published reads.
//! - **D.2.b (2026-05-28)** — compute path. [`BaselineSource`]
//!   trait (initial impl [`StaticBaseline`]). Monotonic revision
//!   allocator on the session; gated publish via
//!   [`DiffSession::try_publish_if_newer`]. Sync recompute body
//!   [`DiffSession::recompute_blocking`]; tokio orchestration
//!   [`DiffSubsystem::schedule_recompute`] spawns it on
//!   `spawn_blocking` and returns a join handle.
//! - **D.2.c (2026-05-29)** — routing + debounce + bus
//!   subscription. [`BufferTextProvider`] (one host seam),
//!   [`CurrentSource`] (mirror of `BaselineSource`),
//!   [`BufferBaseline`] / [`BufferCurrentSource`] live-rope impls,
//!   [`DiffDescriptor`] (sources + explicit `watch: Vec<BufferId>`).
//!   Centralized inverse `watchers` index + per-session lazy
//!   [`Debouncer`]. [`DiffSubsystem::bind`] takes a [`DocumentBufferResolver`]
//!   and an `Arc<EventBus>` and returns a [`DiffSubscriptionGuard`]
//!   that aborts the drainer task and unsubscribes the bus on
//!   `Drop`. See
//!   [`../../../docs/dev/architecture/diff-system.md`](../../../docs/dev/architecture/diff-system.md)
//!   §3.4 for the full data + routing model and the
//!   per-session-actor / direct-call alternatives that were
//!   considered and rejected.
//!
//! ## Concurrency model
//!
//! - Registry / descriptor / watchers / debouncer mutation goes
//!   through `std::sync::Mutex`. Mutation is buffer-open /
//!   buffer-close / lazy-debouncer-spawn frequency — never
//!   per-frame.
//! - Per-session published `hunks: ArcSwap<HunkIndex>` is read
//!   lock-free from any thread (the renderer, `:describe-diff`,
//!   etc.).
//! - The session's `Arc` itself is cloned out of the registry
//!   under the registry lock, then released. Holders may keep
//!   the `Arc` past a `drop_session` call — the registry forgets
//!   the entry, but in-flight readers see a coherent snapshot
//!   until they release their clone (RCU). Matches the standard
//!   `BufferRegistry` / `cells_matrix_cell` pattern in this
//!   crate.
//! - Bus subscription is **centralized** (one subscription on
//!   the subsystem; one drainer task). On each `DocumentChanged`
//!   the drainer resolves DocumentId → BufferId, looks up
//!   dependents in the `watchers` inverse index, and pokes each
//!   session's `Debouncer`. The per-session debouncer is
//!   **lazy** — no task at rest; a tokio task spawns on first
//!   poke, sleeps the debounce window, and self-terminates after
//!   the burst quiesces. Rationale + scaling discussion in §3.4.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arc_swap::ArcSwap;
use ropey::Rope;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::debug;

use lattice_core::BufferId;
use lattice_diff::{compute_two_way, DiffAlgorithm, HunkIndex};
use lattice_protocol::event::{Event, EventKind};
use lattice_protocol::ids::DocumentId;
use lattice_runtime::{EventBus, EventFilter, SubscriptionId, SubscriptionTarget};

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

/// D.3.a (2026-05-29): on-disk file baseline.
///
/// `snapshot` re-reads the file at `path` and parses it into a
/// fresh `Rope`. Used by `:diff` (no args) — "diff against
/// the on-disk version of this file." Cheap enough to do
/// inside `spawn_blocking` per the [`BaselineSource`]
/// contract; D.3's first consumer is single-file inline
/// overlay so per-recompute file re-reads are acceptable.
/// Future D.7 (`:Gdiff`) introduces a separate `GitBaseline`
/// that reads through `gix` against a fixed ref.
///
/// On I/O error (missing path, permissions, mid-read crash)
/// snapshot returns an empty rope. The session then recomputes
/// the diff against empty baseline (all-Add hunks), which is
/// the "everything is new" presentation — a noisy but
/// defensible degradation that the user can resolve via
/// `:diffoff` and a corrected path. We log the error at
/// `tracing::debug` so the failure surfaces under
/// `RUST_LOG=lattice_host::diff_subsystem=debug` without
/// blocking the recompute path.
#[derive(Clone, Debug)]
pub struct OnDiskBaseline {
	path: std::path::PathBuf,
}

impl OnDiskBaseline {
	pub fn new(path: std::path::PathBuf) -> Self {
		Self { path }
	}

	pub fn path(&self) -> &std::path::Path {
		&self.path
	}
}

impl BaselineSource for OnDiskBaseline {
	fn snapshot(&self) -> Rope {
		match std::fs::read_to_string(&self.path) {
			Ok(s) => Rope::from(s),
			Err(err) => {
				debug!(
					target: "lattice_host::diff_subsystem",
					path = ?self.path,
					?err,
					"OnDiskBaseline::snapshot failed; returning empty rope"
				);
				Rope::new()
			}
		}
	}
}

// ──────────────────────────────────────────────────────────────
// D.2.c: CurrentSource trait + concrete sources
// ──────────────────────────────────────────────────────────────

/// Source of the "current" side of a diff session — the rope a
/// session is comparing against its baseline.
///
/// Mirror of [`BaselineSource`]. The split is semantic, not
/// structural: both traits expose the same shape, but the
/// asymmetry makes the descriptor's intent explicit at the call
/// site (`descriptor.baseline.snapshot()` vs.
/// `descriptor.current.snapshot()`). Saves a comment per
/// recompute closure.
pub trait CurrentSource: Send + Sync + 'static + std::fmt::Debug {
	/// Produce the current rope. Called once per recompute from
	/// inside the `spawn_blocking` body — must not hold the UI
	/// thread.
	fn snapshot(&self) -> Rope;
}

/// One-trait seam between the diff subsystem and the host's
/// buffer storage. Required for [`BufferBaseline`] /
/// [`BufferCurrentSource`] to resolve a [`BufferId`] to its live
/// rope at snapshot time.
///
/// The host supplies a single impl backed by `BufferRegistry`.
/// Future ephemeral-buffer providers (e.g. plugin-owned virtual
/// buffers, AI-proposed-edits views) plug into the same trait.
///
/// `buffer_rope(id)` returns `None` when the buffer has been
/// dropped. Concrete buffer-backed sources treat `None` as an
/// empty rope so a recompute against a closed buffer still
/// produces a well-defined `HunkIndex` (all-Add or all-Remove
/// depending on which side was the dropped buffer) rather than
/// panicking. The session's `drop_session` lifecycle will
/// remove the entry shortly after.
pub trait BufferTextProvider: Send + Sync + 'static + std::fmt::Debug {
	fn buffer_rope(&self, id: BufferId) -> Option<Rope>;
}

/// D.3.a (2026-05-29): the production [`BufferTextProvider`]
/// impl. Bridges the trait to the host's [`crate::buffer_registry::BufferRegistry`]:
/// `buffer_rope(id)` walks `BufferRegistry::document_handle(id)
/// -> DocumentHandle::snapshot() -> snapshot.buffer.to_rope()`.
///
/// All operations are RCU-style reads (registry mutex held only
/// long enough to clone an `Arc<DocumentSnapshot>`; rope clone
/// is `Arc`-share of chunks). Safe to call from
/// `spawn_blocking`. Returns `None` for non-document buffers
/// or for ids the registry has dropped — the diff subsystem's
/// `BufferBaseline` / `BufferCurrentSource` impls map `None`
/// to an empty rope per their documented contract.
#[derive(Clone, Debug)]
pub struct BufferRegistryTextProvider {
	registry: crate::buffer_registry::BufferRegistry,
}

impl BufferRegistryTextProvider {
	pub fn new(registry: crate::buffer_registry::BufferRegistry) -> Self {
		Self { registry }
	}
}

impl BufferTextProvider for BufferRegistryTextProvider {
	fn buffer_rope(&self, id: BufferId) -> Option<Rope> {
		let handle = self.registry.document_handle(id)?;
		Some(handle.snapshot().buffer.to_rope())
	}
}

/// D.3.a.1 (2026-05-29): production [`DocumentBufferResolver`]
/// impl. Bridges `DocumentId` → `BufferId` via
/// `BufferRegistry::buffer_id_for_document`. Stored on `Editor`
/// for the editor's lifetime and handed to
/// `DiffSubsystem::bind` so the drainer task can translate
/// bus events.
#[derive(Clone, Debug)]
pub struct BufferRegistryDocumentResolver {
	registry: crate::buffer_registry::BufferRegistry,
}

impl BufferRegistryDocumentResolver {
	pub fn new(registry: crate::buffer_registry::BufferRegistry) -> Self {
		Self { registry }
	}
}

impl DocumentBufferResolver for BufferRegistryDocumentResolver {
	fn buffer_id_for(&self, document_id: DocumentId) -> Option<BufferId> {
		self.registry.buffer_id_for_document(document_id)
	}
}

/// Live-rope baseline backed by a sibling buffer. The
/// unsaved-buffer case: when neither side of a diff has a
/// filesystem path, both sides resolve through
/// [`BufferTextProvider`] at snapshot time. The session's
/// descriptor must include this buffer in its `watch` list so
/// edits to it wake the session.
#[derive(Clone, Debug)]
pub struct BufferBaseline {
	provider: Arc<dyn BufferTextProvider>,
	buffer_id: BufferId,
}

impl BufferBaseline {
	pub fn new(provider: Arc<dyn BufferTextProvider>, buffer_id: BufferId) -> Self {
		Self { provider, buffer_id }
	}

	pub fn buffer_id(&self) -> BufferId {
		self.buffer_id
	}
}

impl BaselineSource for BufferBaseline {
	fn snapshot(&self) -> Rope {
		self.provider.buffer_rope(self.buffer_id).unwrap_or_default()
	}
}

/// Live-rope current source backed by a buffer. Sibling of
/// [`BufferBaseline`]. The session's descriptor must include
/// this buffer in its `watch` list (which it almost always
/// already does — the session is registered under
/// `current.buffer_id`).
#[derive(Clone, Debug)]
pub struct BufferCurrentSource {
	provider: Arc<dyn BufferTextProvider>,
	buffer_id: BufferId,
}

impl BufferCurrentSource {
	pub fn new(provider: Arc<dyn BufferTextProvider>, buffer_id: BufferId) -> Self {
		Self { provider, buffer_id }
	}

	pub fn buffer_id(&self) -> BufferId {
		self.buffer_id
	}
}

impl CurrentSource for BufferCurrentSource {
	fn snapshot(&self) -> Rope {
		self.provider.buffer_rope(self.buffer_id).unwrap_or_default()
	}
}

/// The "what to diff against what" pair for a session.
///
/// `baseline` + `current` are the source sides; `watch` is the
/// **explicit dependency declaration**: every [`BufferId`] whose
/// edits should wake this session. The descriptor's author
/// (a future `:diffsplit` / `:Gdiff` / AI-host call site) knows
/// which sources are buffer-backed and contributes those
/// `BufferId`s into `watch`. Static or git-blob sources
/// contribute nothing.
///
/// `Clone` because the runtime sometimes wants a stable
/// snapshot of the descriptor to feed a debounced recompute —
/// the inner `Arc<dyn ...>` and `Vec<BufferId>` clones are
/// cheap (one Arc bump per source + a small heap allocation).
#[derive(Clone, Debug)]
pub struct DiffDescriptor {
	pub baseline: Arc<dyn BaselineSource>,
	pub current: Arc<dyn CurrentSource>,
	pub watch: Vec<BufferId>,
}

// ──────────────────────────────────────────────────────────────
// D.2.c: DocumentBufferResolver
// ──────────────────────────────────────────────────────────────

/// Translates the protocol-layer [`DocumentId`] carried in
/// `Event::DocumentChanged` / `Event::DocumentClosed` back to a
/// host-layer [`BufferId`]. The host supplies an impl backed by
/// `BufferRegistry`. Kept as a trait so the subsystem stays
/// independent of buffer-registry layout (and so tests can
/// inject a stub mapping).
pub trait DocumentBufferResolver: Send + Sync + 'static + std::fmt::Debug {
	fn buffer_id_for(&self, document_id: DocumentId) -> Option<BufferId>;
}

// ──────────────────────────────────────────────────────────────
// D.2.c: Lazy per-session Debouncer
// ──────────────────────────────────────────────────────────────

/// Default debounce window. Matches Helix's diff debounce
/// (50ms); short enough that the visual lag is imperceptible
/// during sustained typing, long enough that a burst of 5–10
/// keystrokes collapses to a single recompute. Overridable via
/// [`DiffSubsystem::with_debounce_window`] for tests and host
/// configuration.
pub const DEFAULT_DEBOUNCE_WINDOW: Duration = Duration::from_millis(50);

/// Per-session debounce controller.
///
/// State is two atomics — an epoch counter bumped on every
/// [`Self::poke`], and a `pending` flag that gates spawning the
/// debounce task. The task itself is **lazy**: it spawns on the
/// first `poke` after an idle period, sleeps the debounce
/// window, re-reads the epoch, and either re-sleeps (more pokes
/// arrived during the window) or invokes the supplied
/// `runner` and exits. No task runs while the session is
/// quiescent.
///
/// `runner` is `Arc<dyn Fn>` — shareable across the loop. The
/// caller (`DiffSubsystem::poke_session`) captures the
/// subsystem `Arc` + session key into the closure.
#[derive(Debug)]
pub struct Debouncer {
	inner: Arc<DebouncerInner>,
}

#[derive(Debug)]
struct DebouncerInner {
	epoch: AtomicU64,
	pending: AtomicBool,
	window: Duration,
}

impl Debouncer {
	pub fn new(window: Duration) -> Self {
		Self {
			inner: Arc::new(DebouncerInner {
				epoch: AtomicU64::new(0),
				pending: AtomicBool::new(false),
				window,
			}),
		}
	}

	pub fn window(&self) -> Duration {
		self.inner.window
	}

	/// Bump the epoch. If no debounce task is in flight, spawn
	/// one that sleeps the window, re-reads the epoch, and
	/// either re-sleeps (more pokes) or invokes `runner` and
	/// exits.
	///
	/// `runner` runs on the tokio runtime (the debounce task is
	/// itself a tokio task). Inside the runner, the production
	/// call site schedules the actual recompute on
	/// `spawn_blocking`; the runner closure stays light.
	pub fn poke<F>(&self, runner: F)
	where
		F: Fn() + Send + Sync + 'static,
	{
		let inner = Arc::clone(&self.inner);
		// Bump the epoch first so a concurrent task observes the
		// new value even if we don't end up spawning.
		inner.epoch.fetch_add(1, Ordering::Relaxed);
		// Try to claim the spawn slot. If we lose, another
		// debounce task is already in flight and our epoch bump
		// will be observed when it re-reads.
		if inner
			.pending
			.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
			.is_ok()
		{
			let runner = Arc::new(runner);
			let inner_task = Arc::clone(&inner);
			tokio::spawn(async move {
				loop {
					let observed = inner_task.epoch.load(Ordering::Acquire);
					tokio::time::sleep(inner_task.window).await;
					let after = inner_task.epoch.load(Ordering::Acquire);
					if after == observed {
						// Quiet. Clear pending, fire, exit.
						// Race: a poke that lands between this
						// store and the next read of `pending`
						// will spawn a new task — at worst one
						// extra recompute, dropped by the
						// revision gate (see D.2.b).
						inner_task.pending.store(false, Ordering::Release);
						runner();
						return;
					}
					// More pokes arrived during sleep — loop and
					// sleep again.
				}
			});
		}
	}
}

// ──────────────────────────────────────────────────────────────
// D.2.c: Bus-subscription guard
// ──────────────────────────────────────────────────────────────

/// RAII guard returned by [`DiffSubsystem::bind`]. Holds the
/// bus `SubscriptionId` + the drainer task `JoinHandle`. On
/// `Drop`, unsubscribes the bus subscription and aborts the
/// drainer task.
///
/// Hosts hold one of these for the editor's lifetime. Tests
/// drop it to verify cleanup.
#[derive(Debug)]
pub struct DiffSubscriptionGuard {
	bus: Arc<EventBus>,
	subscription: SubscriptionId,
	drainer: JoinHandle<()>,
}

impl Drop for DiffSubscriptionGuard {
	fn drop(&mut self) {
		self.bus.unsubscribe(self.subscription);
		self.drainer.abort();
	}
}

// ──────────────────────────────────────────────────────────────
// D.2.d: introspection types
// ──────────────────────────────────────────────────────────────

/// One row of `:describe-diff` output. Produced by
/// [`DiffSubsystem::describe_sessions`] and rendered into the
/// help-buffer body by
/// [`DiffSubsystem::build_describe_diff_content`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffSessionDescription {
	pub buffer_id: BufferId,
	pub algorithm: DiffAlgorithm,
	pub revision: u64,
	pub hunk_count: usize,
	/// Buffers this session watches for edit-triggered
	/// recomputes (from `descriptor.watch`). Empty if the
	/// session was registered without sources via
	/// [`DiffSubsystem::register`] (the test path).
	pub watch: Vec<BufferId>,
}

fn format_algorithm(alg: DiffAlgorithm) -> &'static str {
	match alg {
		DiffAlgorithm::Histogram => "Histogram",
		DiffAlgorithm::Myers => "Myers",
		DiffAlgorithm::MyersMinimal => "MyersMinimal",
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
	/// D.3.a.1 (2026-05-29): wake signal fired on every
	/// successful publish (gated by `try_publish_if_newer` or
	/// the unconditional `publish`). Consumers — notably the
	/// virtual-rows-wake forwarder set up by `:diff` — await
	/// `notified()` to react immediately to hunk republishes
	/// without waiting for the next `publish_render_state`
	/// tick. Permit-style coalescing: a burst of publishes
	/// collapses to one consumer wake, which matches the
	/// expected debounce behavior on the worker side.
	publish_notify: Arc<tokio::sync::Notify>,
	/// D.3.d.0 (2026-05-29): published per-line sign
	/// classification derived from the current `HunkIndex`.
	/// Renderers read via `sign_map()` (lock-free `ArcSwap`
	/// load) per-frame; the
	/// [`crate::diff_overlay::DiffOverlayRefreshTask`] writes
	/// this cell on every hunk publish, keeping it in lockstep
	/// with `hunks`. Initialised to an empty map at session
	/// construction; first refresh populates it once the
	/// initial recompute completes.
	sign_map: ArcSwap<crate::diff_overlay::DiffSignMap>,
	/// D.4.d.3.a (2026-05-30): linkage from a two-pane diff
	/// session to its `PaneGroup` (the scroll-binding
	/// mechanism with `HunkRowMapper`). `None` for inline
	/// `:diff` sessions, which have no pane-group scroll
	/// binding; `Some(id)` for `:diffthis` / `:diffsplit`
	/// sessions, set by `bind_pane_group` at registration
	/// and read by `do_diff_off` at teardown to drop the
	/// group cleanly.
	pane_group_id: Mutex<Option<lattice_core::ui::pane::PaneGroupId>>,
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
			publish_notify: Arc::new(tokio::sync::Notify::new()),
			sign_map: ArcSwap::from_pointee(
				crate::diff_overlay::DiffSignMap::default(),
			),
			pane_group_id: Mutex::new(None),
		}
	}

	/// D.4.d.3.a (2026-05-30): bind a `PaneGroup` to this
	/// session — call once at two-pane session creation,
	/// before any wake or read. Subsequent calls overwrite
	/// (a re-binding scenario isn't expected in v1 but is
	/// safe).
	pub fn bind_pane_group(&self, id: lattice_core::ui::pane::PaneGroupId) {
		*self
			.pane_group_id
			.lock()
			.expect("DiffSession pane_group_id mutex poisoned") = Some(id);
	}

	/// D.4.d.3.a: read the linked `PaneGroupId`, if any.
	/// `None` for inline `:diff` sessions; `Some(id)` for
	/// two-pane sessions registered via `:diffthis` /
	/// `:diffsplit`. The teardown path (`do_diff_off`)
	/// reads this to drop the linked group atomically with
	/// the session.
	pub fn pane_group_id(&self) -> Option<lattice_core::ui::pane::PaneGroupId> {
		*self
			.pane_group_id
			.lock()
			.expect("DiffSession pane_group_id mutex poisoned")
	}

	/// D.3.d.0 (2026-05-29): snapshot the latest published
	/// `DiffSignMap`. Lock-free `ArcSwap::load_full`; renderer
	/// hot path. The map is refreshed in lockstep with
	/// `hunks` by [`crate::diff_overlay::DiffOverlayRefreshTask`].
	pub fn sign_map(&self) -> Arc<crate::diff_overlay::DiffSignMap> {
		self.sign_map.load_full()
	}

	/// D.3.d.0: publish a freshly-computed sign map.
	/// Unconditional store (no revision gate) — the
	/// `DiffOverlayRefreshTask` already serialises map
	/// updates with `hunks` publishes, so out-of-order
	/// landing isn't possible from the refresh-task side.
	/// Direct callers (tests, future consumers) bear the
	/// ordering responsibility.
	pub fn publish_sign_map(&self, map: Arc<crate::diff_overlay::DiffSignMap>) {
		self.sign_map.store(map);
	}

	/// D.3.a.1: shared `Notify` fired on every successful
	/// publish. The `:diff` handler clones this and awaits
	/// `notified()` in a forwarder task to wake the
	/// `virtual_rows_worker` immediately on hunk republish.
	pub fn publish_notify(&self) -> Arc<tokio::sync::Notify> {
		self.publish_notify.clone()
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
	///
	/// D.3.a.1: fires `publish_notify` so the diff-overlay
	/// wake forwarder observes the publish.
	pub fn publish(&self, hunks: Arc<HunkIndex>) {
		self.hunks.store(hunks);
		self.publish_notify.notify_one();
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
		// D.3.a.1: fire publish_notify on a successful take so
		// the diff-overlay wake forwarder reacts immediately
		// to the hunk republish.
		if took {
			self.publish_notify.notify_one();
		}
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

/// Process-wide registry + routing layer for diff sessions.
///
/// Lifecycle (D.2.a):
/// - `register(buffer_id, algorithm)` — idempotent; pure-compute
///   path with no sources, no debouncer, no routing entries.
///   For tests and the future `:describe-diff` standalone path.
/// - `register_with_sources(buffer_id, algorithm, descriptor)`
///   (D.2.c) — production registration. Installs the
///   descriptor + watch entries + a per-session [`Debouncer`].
///   Edits to any buffer in `descriptor.watch` will route to
///   this session.
/// - `lookup(buffer_id)` — returns `Some(Arc<DiffSession>)` if
///   registered.
/// - `lookup_descriptor(buffer_id)` (D.2.c) — returns
///   `Some(DiffDescriptor)` if registered with sources.
/// - `drop_session(buffer_id)` — removes session, descriptor,
///   debouncer, and the session's entries from every
///   `watchers` bucket. In-flight `Arc` holders are unaffected.
/// - `iter_sessions()` — snapshot of all currently-registered
///   sessions. Powers `:describe-diff` (D.2.d).
///
/// Routing (D.2.c):
/// - `bind(bus, resolver)` — installs the bus subscription +
///   drainer task. Returns a [`DiffSubscriptionGuard`] whose
///   `Drop` unsubscribes and aborts.
/// - `note_buffer_edited(buffer_id)` — pokes the debouncer for
///   every session in `watchers[buffer_id]`. Public so tests
///   and (future) non-bus drivers can fire it directly.
/// - `note_buffer_closed(buffer_id)` — calls `drop_session`.
///
/// The registry is `Default`-able and zero-cost to construct; the
/// host owns one instance, threaded through `Editor`. Debounce
/// window defaults to [`DEFAULT_DEBOUNCE_WINDOW`] and can be
/// overridden via [`Self::with_debounce_window`].
#[derive(Debug)]
pub struct DiffSubsystem {
	sessions: Mutex<HashMap<BufferId, Arc<DiffSession>>>,
	descriptors: Mutex<HashMap<BufferId, DiffDescriptor>>,
	/// Inverse index: edits to `watched_buffer` should wake the
	/// listed session keys. Rebuilt from descriptors on every
	/// register / drop.
	watchers: Mutex<HashMap<BufferId, Vec<BufferId>>>,
	/// D.4.d.3.a (2026-05-30): secondary-buffer → primary-buffer
	/// indirection. Populated from `descriptor.watch` minus the
	/// primary key at registration time; consulted by
	/// [`Self::lookup_session_for`] so a buffer participating
	/// in a two-pane diff (but not the session's primary key)
	/// still resolves to its session. Inline `:diff` sessions
	/// have a single-entry `watch` list that equals the primary,
	/// so they contribute no entries here.
	secondary_index: Mutex<HashMap<BufferId, BufferId>>,
	debouncers: Mutex<HashMap<BufferId, Arc<Debouncer>>>,
	debounce_window: Duration,
}

impl Default for DiffSubsystem {
	fn default() -> Self {
		Self {
			sessions: Mutex::new(HashMap::new()),
			descriptors: Mutex::new(HashMap::new()),
			watchers: Mutex::new(HashMap::new()),
			secondary_index: Mutex::new(HashMap::new()),
			debouncers: Mutex::new(HashMap::new()),
			debounce_window: DEFAULT_DEBOUNCE_WINDOW,
		}
	}
}

impl DiffSubsystem {
	pub fn new() -> Self {
		Self::default()
	}

	/// Construct a subsystem with a non-default debounce window.
	/// Primarily a test hook (so unit tests can run with
	/// `Duration::from_millis(1)` and avoid wall-clock waits) but
	/// hosts can also tune the window via the typed options
	/// registry once that wiring lands (D.2.e).
	pub fn with_debounce_window(window: Duration) -> Self {
		Self {
			debounce_window: window,
			..Self::default()
		}
	}

	pub fn debounce_window(&self) -> Duration {
		self.debounce_window
	}

	/// Register a session for `buffer_id` with no sources. The
	/// session has no debouncer, no descriptor, no watchers
	/// entries — used by tests and by the pure-compute API path.
	/// Production callers want [`Self::register_with_sources`].
	///
	/// Idempotent: returns the existing `Arc<DiffSession>` if
	/// one is already registered (the `algorithm` argument is
	/// ignored in that case).
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

	/// D.2.c: register a session with a full [`DiffDescriptor`].
	/// Inserts (or reuses) the session, stores the descriptor,
	/// rebuilds the inverse `watchers` entries to include this
	/// session for every buffer in `descriptor.watch`, and
	/// installs a per-session [`Debouncer`].
	///
	/// Idempotent on session identity (same `Arc<DiffSession>`
	/// returned on re-registration) but **descriptor is
	/// replaced** on re-registration — the caller may be
	/// updating sources (e.g. switching baseline from
	/// `StaticBaseline` to `GitBaseline`). The old watch
	/// entries are scrubbed before the new ones are installed
	/// so a re-register with a shrunken watch list doesn't
	/// leave stale routes.
	pub fn register_with_sources(
		&self,
		buffer_id: BufferId,
		algorithm: DiffAlgorithm,
		descriptor: DiffDescriptor,
	) -> Arc<DiffSession> {
		let session = {
			let mut sessions = self.sessions.lock().expect("DiffSubsystem mutex poisoned");
			sessions
				.entry(buffer_id)
				.or_insert_with(|| Arc::new(DiffSession::new(buffer_id, algorithm)))
				.clone()
		};
		// Replace descriptor; capture old to scrub stale
		// watcher entries.
		let old_descriptor = {
			let mut descriptors = self
				.descriptors
				.lock()
				.expect("DiffSubsystem mutex poisoned");
			descriptors.insert(buffer_id, descriptor.clone())
		};
		if let Some(old) = old_descriptor {
			self.scrub_watcher_entries(buffer_id, &old.watch);
			self.scrub_secondary_entries(buffer_id, &old.watch);
		}
		self.install_watcher_entries(buffer_id, &descriptor.watch);
		// D.4.d.3.a: maintain the BufferId → primary
		// indirection so two-pane sessions can be looked up
		// from *either* side. Inline sessions (`watch =
		// [primary]`) contribute no entries because
		// `scrub_secondary_entries` skips the primary.
		self.install_secondary_entries(buffer_id, &descriptor.watch);
		// Install or replace the debouncer (idempotent — a
		// re-register on an already-debouncing session keeps
		// the existing controller).
		{
			let mut debouncers = self.debouncers.lock().expect("DiffSubsystem mutex poisoned");
			debouncers
				.entry(buffer_id)
				.or_insert_with(|| Arc::new(Debouncer::new(self.debounce_window)));
		}
		session
	}

	/// D.4.d.3.a (2026-05-30): resolve a session from *any*
	/// buffer that participates in it (primary or secondary).
	/// Returns the same `Arc<DiffSession>` whether the caller
	/// passes the session's primary key or one of its
	/// descriptor's watched buffers. The teardown path
	/// (`do_diff_off`) uses this so `:diffoff` from either
	/// pane of a two-way diff finds the same session.
	pub fn lookup_session_for(&self, buffer_id: BufferId) -> Option<Arc<DiffSession>> {
		if let Some(session) = self.lookup(buffer_id) {
			return Some(session);
		}
		let primary = self
			.secondary_index
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.get(&buffer_id)
			.copied()?;
		self.lookup(primary)
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

	/// D.2.c: look up the descriptor for `buffer_id`. Returns
	/// `None` if the session was registered via [`Self::register`]
	/// (sources-less) or not registered at all.
	pub fn lookup_descriptor(&self, buffer_id: BufferId) -> Option<DiffDescriptor> {
		self.descriptors
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.get(&buffer_id)
			.cloned()
	}

	/// D.2.c: snapshot of the inverse routing index for
	/// `watched_buffer`. Returns the session keys whose
	/// descriptors include `watched_buffer` in their `watch`
	/// list. Empty if no sessions watch it.
	///
	/// Test-friendly; production code uses
	/// [`Self::note_buffer_edited`].
	pub fn watchers_of(&self, watched_buffer: BufferId) -> Vec<BufferId> {
		self.watchers
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.get(&watched_buffer)
			.cloned()
			.unwrap_or_default()
	}

	/// Drop the registry entry for `buffer_id`. Removes the
	/// session, descriptor, every watchers-bucket entry, and
	/// the debouncer. Returns `true` if a session entry was
	/// removed. Safe to call on a non-registered id.
	///
	/// In-flight `Arc<DiffSession>` holders stay coherent; the
	/// registry's job is naming, not lifetime enforcement.
	pub fn drop_session(&self, buffer_id: BufferId) -> bool {
		let removed = self
			.sessions
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.remove(&buffer_id)
			.is_some();
		let descriptor = self
			.descriptors
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.remove(&buffer_id);
		if let Some(d) = descriptor {
			self.scrub_watcher_entries(buffer_id, &d.watch);
			self.scrub_secondary_entries(buffer_id, &d.watch);
		}
		// Drop the debouncer Arc — any in-flight task holds its
		// own Arc clone and will run to completion (it'll call
		// runner() then exit), but no further pokes can arrive
		// for this session.
		self.debouncers
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.remove(&buffer_id);
		removed
	}

	// Internal helper: add this session_key to each watched
	// buffer's bucket. Called from register_with_sources.
	fn install_watcher_entries(&self, session_key: BufferId, watch: &[BufferId]) {
		let mut watchers = self.watchers.lock().expect("DiffSubsystem mutex poisoned");
		for &watched in watch {
			let bucket = watchers.entry(watched).or_default();
			if !bucket.contains(&session_key) {
				bucket.push(session_key);
			}
		}
	}

	// Internal helper: remove this session_key from each watched
	// buffer's bucket. Called from drop_session + register
	// (when replacing a descriptor).
	fn scrub_watcher_entries(&self, session_key: BufferId, watch: &[BufferId]) {
		let mut watchers = self.watchers.lock().expect("DiffSubsystem mutex poisoned");
		for watched in watch {
			if let Some(bucket) = watchers.get_mut(watched) {
				bucket.retain(|s| *s != session_key);
				if bucket.is_empty() {
					watchers.remove(watched);
				}
			}
		}
	}

	// D.4.d.3.a internal helper: for each watched buffer that
	// isn't the session's primary key, record the secondary
	// → primary mapping so `lookup_session_for` can resolve a
	// session from either side of a two-pane diff. Skips
	// entries that equal `session_key` (inline `:diff`
	// `watch = [primary]` contributes nothing). Idempotent on
	// repeat installs.
	fn install_secondary_entries(&self, session_key: BufferId, watch: &[BufferId]) {
		let mut secondary = self
			.secondary_index
			.lock()
			.expect("DiffSubsystem mutex poisoned");
		for &watched in watch {
			if watched == session_key {
				continue;
			}
			secondary.insert(watched, session_key);
		}
	}

	// D.4.d.3.a internal helper: remove any secondary entries
	// pointing at this `session_key`. Called from drop_session
	// and from register (when replacing a descriptor whose
	// watch list shrunk). Skips the primary entry like the
	// install path.
	fn scrub_secondary_entries(&self, session_key: BufferId, watch: &[BufferId]) {
		let mut secondary = self
			.secondary_index
			.lock()
			.expect("DiffSubsystem mutex poisoned");
		for watched in watch {
			if *watched == session_key {
				continue;
			}
			// Only remove if the entry still points at this
			// session — a re-register could have rerouted the
			// secondary to a different primary in between.
			if secondary.get(watched) == Some(&session_key) {
				secondary.remove(watched);
			}
		}
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

	// ──────────────────────────────────────────────────────
	// D.2.d: introspection
	// ──────────────────────────────────────────────────────

	/// Snapshot of all currently-registered sessions for
	/// `:describe-diff` introspection. Sorted by `BufferId` so
	/// the rendered output is stable across calls.
	///
	/// Each row carries everything the renderer needs to format
	/// one line of the help buffer: the session key, the
	/// algorithm, the currently-published revision + hunk
	/// count, and (when a descriptor is registered) the
	/// declared `watch` list.
	pub fn describe_sessions(&self) -> Vec<DiffSessionDescription> {
		let sessions = self
			.sessions
			.lock()
			.expect("DiffSubsystem mutex poisoned");
		let descriptors = self
			.descriptors
			.lock()
			.expect("DiffSubsystem mutex poisoned");
		let mut rows: Vec<DiffSessionDescription> = sessions
			.values()
			.map(|session| {
				let hunks = session.current_hunks();
				let watch = descriptors
					.get(&session.buffer_id())
					.map(|d| d.watch.clone())
					.unwrap_or_default();
				DiffSessionDescription {
					buffer_id: session.buffer_id(),
					algorithm: session.algorithm(),
					revision: hunks.revision,
					hunk_count: hunks.len(),
					watch,
				}
			})
			.collect();
		rows.sort_by_key(|row| row.buffer_id);
		rows
	}

	/// Build the `:describe-diff` help-buffer body — the
	/// human-readable text rendered into the synthetic
	/// Document buffer that `do_describe_diff` opens.
	///
	/// Output shape:
	/// ```text
	/// Active diff sessions: 2
	///
	/// BufferId  Algorithm     Rev  Hunks  Watches
	/// --------  ------------  ---  -----  -------
	/// 1         Histogram     5    3      [1, 2]
	/// 7         MyersMinimal  0    0      [7]
	/// ```
	pub fn build_describe_diff_content(&self) -> String {
		let rows = self.describe_sessions();
		if rows.is_empty() {
			return "No active diff sessions.\n".to_string();
		}
		let mut out = String::new();
		out.push_str(&format!("Active diff sessions: {}\n\n", rows.len()));
		out.push_str("BufferId  Algorithm     Rev  Hunks  Watches\n");
		out.push_str("--------  ------------  ---  -----  -------\n");
		for row in rows {
			let watches = if row.watch.is_empty() {
				"[]".to_string()
			} else {
				format!(
					"[{}]",
					row.watch
						.iter()
						.map(|b| b.0.to_string())
						.collect::<Vec<_>>()
						.join(", ")
				)
			};
			out.push_str(&format!(
				"{:<8}  {:<12}  {:<3}  {:<5}  {}\n",
				row.buffer_id.0,
				format_algorithm(row.algorithm),
				row.revision,
				row.hunk_count,
				watches
			));
		}
		out
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

	// ──────────────────────────────────────────────────────
	// D.2.c: routing entry points
	// ──────────────────────────────────────────────────────

	/// Notify the subsystem that `buffer_id` was edited. Walks
	/// the inverse `watchers` index and pokes the debouncer for
	/// every session whose descriptor's `watch` list includes
	/// `buffer_id`. Each poke schedules a recompute after the
	/// debounce window; multiple pokes during the window
	/// collapse to one recompute (see [`Debouncer`]).
	///
	/// Production driver is [`Self::bind`]'s drainer task; tests
	/// (and future non-bus drivers) can call this directly.
	pub fn note_buffer_edited(self: &Arc<Self>, buffer_id: BufferId) {
		let dependents = self.watchers_of(buffer_id);
		if dependents.is_empty() {
			return;
		}
		debug!(
			target: "lattice_host::diff_subsystem",
			?buffer_id,
			n_dependents = dependents.len(),
			"diff: buffer edited, poking debouncers"
		);
		for session_key in dependents {
			self.poke_session(session_key);
		}
	}

	/// Notify the subsystem that `buffer_id` was closed. Drops
	/// the session for that buffer. If the closed buffer was a
	/// watched-only dependency of some other session (e.g.
	/// `BufferBaseline(closed_id)` for session X), session X's
	/// watcher entry for `closed_id` is left in place — the
	/// next snapshot returns an empty rope per the
	/// [`BufferTextProvider`] contract, and the session will
	/// recompute the all-Add diff. The session itself is not
	/// dropped on a watched-side close; only on a current-side
	/// close.
	pub fn note_buffer_closed(&self, buffer_id: BufferId) {
		debug!(
			target: "lattice_host::diff_subsystem",
			?buffer_id,
			"diff: buffer closed, dropping session if registered"
		);
		self.drop_session(buffer_id);
	}

	// Internal: look up the descriptor + debouncer for
	// `session_key` and fire a debounced recompute via
	// `schedule_recompute`. The closure captured by the
	// debouncer holds an `Arc<Self>` so the subsystem stays
	// alive for the duration of the deferred work.
	fn poke_session(self: &Arc<Self>, session_key: BufferId) {
		let debouncer = match self
			.debouncers
			.lock()
			.expect("DiffSubsystem mutex poisoned")
			.get(&session_key)
			.cloned()
		{
			Some(d) => d,
			None => return,
		};
		let sub = Arc::clone(self);
		debouncer.poke(move || {
			sub.recompute_from_descriptor(session_key);
		});
	}

	// Internal: read the session's descriptor, snapshot the
	// current source, and fire `schedule_recompute`. The
	// schedule_recompute call spawns the diff on the blocking
	// pool; we return immediately. Stale or torn-down sessions
	// return early — the gated publish in D.2.b drops anything
	// stale that does land.
	fn recompute_from_descriptor(&self, session_key: BufferId) {
		let descriptor = match self.lookup_descriptor(session_key) {
			Some(d) => d,
			None => return,
		};
		let current = descriptor.current.snapshot();
		let _ = self.schedule_recompute(session_key, descriptor.baseline, current);
	}

	/// D.2.c: bind the subsystem to an event bus. Subscribes to
	/// `EventKind::DocumentChanged` + `EventKind::DocumentClosed`,
	/// spawns one drainer task that translates each event's
	/// `DocumentId` to `BufferId` via `resolver` and fans the
	/// signal into the routing path.
	///
	/// Returns a [`DiffSubscriptionGuard`] whose `Drop`
	/// unsubscribes the bus subscription and aborts the
	/// drainer task. Hosts hold the guard for the editor's
	/// lifetime; tests drop it to verify cleanup.
	pub fn bind(
		self: &Arc<Self>,
		bus: Arc<EventBus>,
		resolver: Arc<dyn DocumentBufferResolver>,
	) -> DiffSubscriptionGuard {
		let (tx, mut rx) = mpsc::unbounded_channel::<Event>();
		let subscription = bus.subscribe(
			EventFilter::kinds(vec![
				EventKind::DocumentChanged,
				EventKind::DocumentClosed,
			]),
			SubscriptionTarget::Channel(tx),
		);
		let sub_self = Arc::clone(self);
		let drainer = tokio::spawn(async move {
			while let Some(event) = rx.recv().await {
				match event {
					Event::DocumentChanged { id, .. } => {
						if let Some(buffer_id) = resolver.buffer_id_for(id) {
							sub_self.note_buffer_edited(buffer_id);
						}
					}
					Event::DocumentClosed { id } => {
						if let Some(buffer_id) = resolver.buffer_id_for(id) {
							sub_self.note_buffer_closed(buffer_id);
						}
					}
					_ => {}
				}
			}
		});
		DiffSubscriptionGuard {
			bus,
			subscription,
			drainer,
		}
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
	fn on_disk_baseline_reads_file() {
		// Write a tempfile, snapshot the baseline against it,
		// verify content.
		let dir = std::env::temp_dir();
		let path = dir.join(format!(
			"lattice-on-disk-baseline-test-{}.txt",
			std::process::id()
		));
		std::fs::write(&path, "hello\nworld\n").expect("write tempfile");
		let base = OnDiskBaseline::new(path.clone());
		let snap = base.snapshot();
		assert_eq!(snap.to_string(), "hello\nworld\n");
		let _ = std::fs::remove_file(&path);
	}

	#[test]
	fn on_disk_baseline_missing_file_returns_empty_rope() {
		// Per docs: missing path / I/O error degrades to
		// empty rope (all-Add presentation) rather than
		// panicking.
		let base = OnDiskBaseline::new(std::path::PathBuf::from(
			"/nonexistent/path/lattice-diff-test-does-not-exist",
		));
		let snap = base.snapshot();
		assert_eq!(snap.len_chars(), 0);
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

	// ──────────────────────────────────────────────────────────
	// D.2.c: routing + debounce + bus subscription
	// ──────────────────────────────────────────────────────────

	use std::sync::atomic::AtomicU64;

	// Mock impl of BufferTextProvider — stores ropes keyed by
	// BufferId. The test sets ropes; `BufferBaseline` /
	// `BufferCurrentSource` read them on snapshot.
	#[derive(Debug, Default)]
	struct MockProvider {
		ropes: Mutex<HashMap<BufferId, Rope>>,
	}

	impl MockProvider {
		fn set(&self, id: BufferId, rope: Rope) {
			self.ropes.lock().unwrap().insert(id, rope);
		}
	}

	impl BufferTextProvider for MockProvider {
		fn buffer_rope(&self, id: BufferId) -> Option<Rope> {
			self.ropes.lock().unwrap().get(&id).cloned()
		}
	}

	// Mock impl of DocumentBufferResolver — stores DocumentId →
	// BufferId pairs the test sets up before publishing events.
	#[derive(Debug, Default)]
	struct MockResolver {
		map: Mutex<HashMap<DocumentId, BufferId>>,
	}

	impl MockResolver {
		fn bind(&self, doc_id: DocumentId, buf_id: BufferId) {
			self.map.lock().unwrap().insert(doc_id, buf_id);
		}
	}

	impl DocumentBufferResolver for MockResolver {
		fn buffer_id_for(&self, document_id: DocumentId) -> Option<BufferId> {
			self.map.lock().unwrap().get(&document_id).copied()
		}
	}

	fn descriptor(
		provider: &Arc<dyn BufferTextProvider>,
		baseline_buf: BufferId,
		current_buf: BufferId,
	) -> DiffDescriptor {
		DiffDescriptor {
			baseline: Arc::new(BufferBaseline::new(
				Arc::clone(provider),
				baseline_buf,
			)),
			current: Arc::new(BufferCurrentSource::new(
				Arc::clone(provider),
				current_buf,
			)),
			watch: vec![baseline_buf, current_buf],
		}
	}

	// ── Concrete sources ──────────────────────────────────────

	#[test]
	fn buffer_baseline_snapshots_through_provider() {
		let provider = Arc::new(MockProvider::default());
		provider.set(bid(1), Rope::from("hello\n"));
		let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();
		let base = BufferBaseline::new(dyn_provider, bid(1));
		assert_eq!(base.snapshot().to_string(), "hello\n");
	}

	#[test]
	fn buffer_baseline_returns_empty_rope_when_provider_lacks_buffer() {
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let base = BufferBaseline::new(provider, bid(999));
		assert_eq!(base.snapshot().len_chars(), 0);
	}

	#[test]
	fn buffer_current_source_snapshots_through_provider() {
		let provider = Arc::new(MockProvider::default());
		provider.set(bid(1), Rope::from("world\n"));
		let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();
		let cur = BufferCurrentSource::new(dyn_provider, bid(1));
		assert_eq!(cur.snapshot().to_string(), "world\n");
	}

	// ── Descriptor + watchers ─────────────────────────────────

	#[test]
	fn register_with_sources_stores_descriptor_and_debouncer() {
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = DiffSubsystem::new();
		let desc = descriptor(&provider, bid(2), bid(1));
		let session = sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);
		assert_eq!(session.buffer_id(), bid(1));
		assert!(sub.lookup_descriptor(bid(1)).is_some());
		// Debouncer present (looked up via watchers_of → poke
		// path; here we just check the routing table directly).
		assert_eq!(sub.watchers_of(bid(1)), vec![bid(1)]);
		assert_eq!(sub.watchers_of(bid(2)), vec![bid(1)]);
	}

	#[test]
	fn multiple_sessions_share_a_watched_buffer_bucket() {
		// Sessions A and B both watch buffer X — `watchers_of(X)`
		// returns both.
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = DiffSubsystem::new();
		let desc_a = DiffDescriptor {
			baseline: Arc::new(BufferBaseline::new(Arc::clone(&provider), bid(10))),
			current: Arc::new(BufferCurrentSource::new(Arc::clone(&provider), bid(1))),
			watch: vec![bid(10), bid(1)],
		};
		let desc_b = DiffDescriptor {
			baseline: Arc::new(BufferBaseline::new(Arc::clone(&provider), bid(10))),
			current: Arc::new(BufferCurrentSource::new(Arc::clone(&provider), bid(2))),
			watch: vec![bid(10), bid(2)],
		};
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc_a);
		sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc_b);
		let mut watchers = sub.watchers_of(bid(10));
		watchers.sort();
		assert_eq!(watchers, vec![bid(1), bid(2)]);
	}

	#[test]
	fn drop_session_clears_descriptor_and_watcher_buckets() {
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = DiffSubsystem::new();
		let desc = descriptor(&provider, bid(2), bid(1));
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);
		assert!(sub.lookup_descriptor(bid(1)).is_some());
		assert_eq!(sub.watchers_of(bid(2)), vec![bid(1)]);

		assert!(sub.drop_session(bid(1)));
		assert!(sub.lookup_descriptor(bid(1)).is_none());
		assert!(sub.watchers_of(bid(2)).is_empty());
		assert!(sub.watchers_of(bid(1)).is_empty());
	}

	#[test]
	fn drop_session_only_scrubs_dropped_sessions_watchers() {
		// Session A watches X; session B also watches X. Drop A
		// → bucket only loses A, B remains.
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = DiffSubsystem::new();
		let desc_a = DiffDescriptor {
			baseline: Arc::new(BufferBaseline::new(Arc::clone(&provider), bid(10))),
			current: Arc::new(BufferCurrentSource::new(Arc::clone(&provider), bid(1))),
			watch: vec![bid(10), bid(1)],
		};
		let desc_b = DiffDescriptor {
			baseline: Arc::new(BufferBaseline::new(Arc::clone(&provider), bid(10))),
			current: Arc::new(BufferCurrentSource::new(Arc::clone(&provider), bid(2))),
			watch: vec![bid(10), bid(2)],
		};
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc_a);
		sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc_b);

		sub.drop_session(bid(1));
		assert_eq!(sub.watchers_of(bid(10)), vec![bid(2)]);
	}

	#[test]
	fn reregister_with_smaller_watch_scrubs_stale_entries() {
		// Initial watch [10, 1]; re-register with [1] only → 10
		// loses its entry for this session.
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = DiffSubsystem::new();
		let desc_a = DiffDescriptor {
			baseline: Arc::new(StaticBaseline::new(Rope::from(""))),
			current: Arc::new(BufferCurrentSource::new(Arc::clone(&provider), bid(1))),
			watch: vec![bid(10), bid(1)],
		};
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc_a);
		assert_eq!(sub.watchers_of(bid(10)), vec![bid(1)]);

		let desc_b = DiffDescriptor {
			baseline: Arc::new(StaticBaseline::new(Rope::from(""))),
			current: Arc::new(BufferCurrentSource::new(Arc::clone(&provider), bid(1))),
			watch: vec![bid(1)],
		};
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc_b);
		assert!(sub.watchers_of(bid(10)).is_empty());
		assert_eq!(sub.watchers_of(bid(1)), vec![bid(1)]);
	}

	// ── Debouncer ─────────────────────────────────────────────

	// Helper: under paused time, sleeping in the main task lets
	// the runtime auto-advance — when all tasks are idle, time
	// jumps to the next earliest deadline. So `sleep(60ms)`
	// here drives the debouncer's `sleep(50ms)` to completion
	// without wall-clock waits. The yield_now+advance pattern
	// used elsewhere is unreliable because a yield-spinning main
	// task isn't "idle" for auto-advance purposes.

	#[tokio::test(start_paused = true)]
	async fn debouncer_single_poke_fires_runner_after_window() {
		let counter = Arc::new(AtomicU64::new(0));
		let window = Duration::from_millis(50);
		let deb = Debouncer::new(window);
		let c = Arc::clone(&counter);
		deb.poke(move || {
			c.fetch_add(1, Ordering::Relaxed);
		});

		// Sleep just shy of the window: debouncer task is
		// still mid-sleep, not yet fired.
		tokio::time::sleep(Duration::from_millis(40)).await;
		assert_eq!(counter.load(Ordering::Relaxed), 0);

		// Sleep past the window: debouncer's sleep completes,
		// runner fires once.
		tokio::time::sleep(Duration::from_millis(20)).await;
		assert_eq!(counter.load(Ordering::Relaxed), 1);
	}

	#[tokio::test(start_paused = true)]
	async fn debouncer_rapid_pokes_coalesce_to_one_runner_invocation() {
		let counter = Arc::new(AtomicU64::new(0));
		let window = Duration::from_millis(50);
		let deb = Debouncer::new(window);
		let c = Arc::clone(&counter);
		let runner = move || {
			c.fetch_add(1, Ordering::Relaxed);
		};

		// Five pokes spaced 10ms apart — each one within the
		// debounce window of the previous. Total elapsed = 50ms.
		for _ in 0..5 {
			let r = runner.clone();
			deb.poke(r);
			tokio::time::sleep(Duration::from_millis(10)).await;
		}
		// Sleep past the window from the last poke (elapsed
		// ~50ms when last poke fired; debouncer task started a
		// new 50ms sleep at t=50 → wakes at t=100). Sleep 80ms
		// to land safely past it.
		tokio::time::sleep(Duration::from_millis(80)).await;
		assert_eq!(counter.load(Ordering::Relaxed), 1);
	}

	#[tokio::test(start_paused = true)]
	async fn debouncer_pokes_after_idle_fire_runner_twice() {
		let counter = Arc::new(AtomicU64::new(0));
		let window = Duration::from_millis(50);
		let deb = Debouncer::new(window);
		let c = Arc::clone(&counter);
		let runner = move || {
			c.fetch_add(1, Ordering::Relaxed);
		};

		// First poke → fires after window.
		deb.poke(runner.clone());
		tokio::time::sleep(Duration::from_millis(80)).await;
		assert_eq!(counter.load(Ordering::Relaxed), 1);

		// Idle gap, then second poke → fires again.
		tokio::time::sleep(Duration::from_millis(500)).await;
		deb.poke(runner);
		tokio::time::sleep(Duration::from_millis(80)).await;
		assert_eq!(counter.load(Ordering::Relaxed), 2);
	}

	// ── Routing integration ───────────────────────────────────

	// Routing / bus tests run with REAL tokio time: the
	// debounce → schedule_recompute path eventually calls
	// `tokio::task::spawn_blocking`, which runs on the blocking
	// thread pool (separate OS threads). The blocking pool
	// doesn't observe paused time, so wall-clock progress is
	// required to see published results. Debounce windows kept
	// to ~10ms so tests stay fast.

	async fn wait_for_revision_above(
		session: &Arc<DiffSession>,
		threshold: u64,
		deadline: Duration,
	) -> u64 {
		let start = std::time::Instant::now();
		loop {
			let rev = session.current_hunks().revision;
			if rev > threshold {
				return rev;
			}
			if start.elapsed() >= deadline {
				return rev;
			}
			tokio::time::sleep(Duration::from_millis(5)).await;
		}
	}

	#[tokio::test]
	async fn note_buffer_edited_triggers_debounced_recompute() {
		let provider = Arc::new(MockProvider::default());
		provider.set(bid(2), Rope::from("alpha\n"));
		provider.set(bid(1), Rope::from("alpha\nbeta\n"));
		let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

		let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(10)));
		let desc = descriptor(&dyn_provider, bid(2), bid(1));
		let session =
			sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);

		// Edit the buffer (simulated by changing the provider's
		// rope) then fire the routing entry point.
		provider.set(bid(1), Rope::from("alpha\nBETA\n"));
		Arc::clone(&sub).note_buffer_edited(bid(1));

		let rev = wait_for_revision_above(&session, 0, Duration::from_secs(2)).await;
		assert!(rev > 0, "session should have recomputed");
		let idx = session.current_hunks();
		assert_eq!(idx.len(), 1, "one hunk for the Change");
	}

	#[tokio::test]
	async fn note_buffer_edited_with_no_session_is_noop() {
		// No session registered for bid(99); call must not
		// panic, must not spawn anything.
		let sub = Arc::new(DiffSubsystem::new());
		Arc::clone(&sub).note_buffer_edited(bid(99));
		tokio::time::sleep(Duration::from_millis(20)).await;
	}

	#[tokio::test]
	async fn editing_watched_baseline_wakes_session() {
		// Session X watches its own buffer (bid 1) AND a
		// sibling buffer (bid 2 — the baseline). Editing the
		// baseline must wake X.
		let provider = Arc::new(MockProvider::default());
		provider.set(bid(2), Rope::from("alpha\n"));
		provider.set(bid(1), Rope::from("alpha\n"));
		let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

		let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(10)));
		let desc = descriptor(&dyn_provider, bid(2), bid(1));
		let session = sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);

		// Initially identical — recompute produces empty hunks.
		Arc::clone(&sub).note_buffer_edited(bid(1));
		let rev1 = wait_for_revision_above(&session, 0, Duration::from_secs(2)).await;
		assert!(rev1 > 0);
		assert!(session.current_hunks().is_empty());

		// Now edit the *baseline* buffer (bid 2). Session should
		// recompute and see hunks.
		provider.set(bid(2), Rope::from("alpha\nBETA\n"));
		Arc::clone(&sub).note_buffer_edited(bid(2));
		let rev2 = wait_for_revision_above(&session, rev1, Duration::from_secs(2)).await;
		assert!(rev2 > rev1);
		assert_eq!(session.current_hunks().len(), 1);
	}

	#[tokio::test]
	async fn note_buffer_closed_drops_session() {
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(10)));
		let desc = descriptor(&provider, bid(2), bid(1));
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);
		assert!(sub.lookup(bid(1)).is_some());

		sub.note_buffer_closed(bid(1));
		assert!(sub.lookup(bid(1)).is_none());
		assert!(sub.lookup_descriptor(bid(1)).is_none());
	}

	// ── Bus binding ───────────────────────────────────────────

	fn doc_id(n: u64) -> DocumentId {
		DocumentId::new(n)
	}

	#[tokio::test]
	async fn bind_routes_document_changed_to_debounced_recompute() {
		let provider = Arc::new(MockProvider::default());
		provider.set(bid(2), Rope::from("alpha\n"));
		provider.set(bid(1), Rope::from("alpha\nbeta\n"));
		let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

		let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(10)));
		let desc = descriptor(&dyn_provider, bid(2), bid(1));
		let session = sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);

		let bus = Arc::new(EventBus::new());
		let resolver = Arc::new(MockResolver::default());
		resolver.bind(doc_id(1001), bid(1));
		let resolver_dyn: Arc<dyn DocumentBufferResolver> = resolver;

		let _guard = sub.bind(Arc::clone(&bus), resolver_dyn);

		// Yield once to let the drainer task park on rx.recv().
		tokio::task::yield_now().await;

		// Publish a DocumentChanged for the bound DocumentId.
		bus.publish(Event::DocumentChanged {
			id: doc_id(1001),
			path: None,
			version: 2,
			edits: Vec::new(),
		});

		let rev = wait_for_revision_above(&session, 0, Duration::from_secs(2)).await;
		assert!(rev > 0, "session should have recomputed after bus event");
	}

	#[tokio::test]
	async fn bind_routes_document_closed_to_drop_session() {
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(10)));
		let desc = descriptor(&provider, bid(2), bid(1));
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);
		assert!(sub.lookup(bid(1)).is_some());

		let bus = Arc::new(EventBus::new());
		let resolver = Arc::new(MockResolver::default());
		resolver.bind(doc_id(1001), bid(1));
		let resolver_dyn: Arc<dyn DocumentBufferResolver> = resolver;
		let _guard = sub.bind(Arc::clone(&bus), resolver_dyn);

		tokio::task::yield_now().await;

		bus.publish(Event::DocumentClosed { id: doc_id(1001) });

		// Wait for drainer to process the event.
		let deadline = std::time::Instant::now() + Duration::from_secs(2);
		while sub.lookup(bid(1)).is_some() && std::time::Instant::now() < deadline {
			tokio::time::sleep(Duration::from_millis(5)).await;
		}
		assert!(sub.lookup(bid(1)).is_none());
	}

	// ── D.2.d introspection ───────────────────────────────────

	#[test]
	fn describe_sessions_empty_when_no_sessions() {
		let sub = DiffSubsystem::new();
		assert!(sub.describe_sessions().is_empty());
		assert_eq!(sub.build_describe_diff_content(), "No active diff sessions.\n");
	}

	#[test]
	fn describe_sessions_lists_sessions_sorted_by_buffer_id() {
		// Register out-of-order to verify the sort.
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = DiffSubsystem::new();
		sub.register_with_sources(
			bid(7),
			DiffAlgorithm::MyersMinimal,
			descriptor(&provider, bid(99), bid(7)),
		);
		sub.register_with_sources(
			bid(1),
			DiffAlgorithm::Histogram,
			descriptor(&provider, bid(2), bid(1)),
		);
		let rows = sub.describe_sessions();
		assert_eq!(rows.len(), 2);
		assert_eq!(rows[0].buffer_id, bid(1));
		assert_eq!(rows[0].algorithm, DiffAlgorithm::Histogram);
		assert_eq!(rows[0].watch, vec![bid(2), bid(1)]);
		assert_eq!(rows[1].buffer_id, bid(7));
		assert_eq!(rows[1].algorithm, DiffAlgorithm::MyersMinimal);
		assert_eq!(rows[1].watch, vec![bid(99), bid(7)]);
	}

	#[test]
	fn describe_sessions_reflects_current_published_hunks() {
		let s = DiffSubsystem::new();
		let session = s.register(bid(1), DiffAlgorithm::Histogram);
		session.publish(Arc::new(HunkIndex {
			hunks: vec![],
			algorithm: DiffAlgorithm::Histogram,
			revision: 12,
		}));
		let rows = s.describe_sessions();
		assert_eq!(rows.len(), 1);
		assert_eq!(rows[0].revision, 12);
		assert_eq!(rows[0].hunk_count, 0);
	}

	#[test]
	fn build_describe_diff_content_formats_columns() {
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = DiffSubsystem::new();
		sub.register_with_sources(
			bid(1),
			DiffAlgorithm::Histogram,
			descriptor(&provider, bid(2), bid(1)),
		);
		let body = sub.build_describe_diff_content();
		assert!(body.starts_with("Active diff sessions: 1\n"));
		assert!(body.contains("BufferId  Algorithm     Rev  Hunks  Watches"));
		assert!(body.contains("--------  ------------  ---  -----  -------"));
		assert!(
			body.contains("Histogram"),
			"algorithm column missing: {body}"
		);
		assert!(body.contains("[2, 1]"), "watch column missing: {body}");
	}

	#[test]
	fn describe_sessions_omits_watch_for_sources_less_register() {
		let s = DiffSubsystem::new();
		s.register(bid(1), DiffAlgorithm::Histogram);
		let rows = s.describe_sessions();
		assert_eq!(rows.len(), 1);
		assert!(
			rows[0].watch.is_empty(),
			"register() path has no descriptor → empty watch"
		);
	}

	#[tokio::test]
	async fn dropping_guard_unsubscribes_bus_and_aborts_drainer() {
		let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
		let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(10)));
		let desc = descriptor(&provider, bid(2), bid(1));
		sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);

		let bus = Arc::new(EventBus::new());
		let resolver = Arc::new(MockResolver::default());
		resolver.bind(doc_id(1001), bid(1));
		let resolver_dyn: Arc<dyn DocumentBufferResolver> = resolver;
		let guard = sub.bind(Arc::clone(&bus), resolver_dyn);

		tokio::task::yield_now().await;

		// Drop the guard — bus subscription should be cleaned.
		drop(guard);
		tokio::task::yield_now().await;

		// Publish — the drainer is gone, so the session does
		// NOT receive the event.
		bus.publish(Event::DocumentClosed { id: doc_id(1001) });
		tokio::time::sleep(Duration::from_millis(100)).await;
		// Session should still be present — the guard's drop
		// prevented the drainer from acting on the event.
		assert!(sub.lookup(bid(1)).is_some());
	}
}
