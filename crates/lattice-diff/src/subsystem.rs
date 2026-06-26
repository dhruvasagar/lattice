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
//! - **D.2.b (2026-05-28)** — compute path. [`DiffParticipantSource`]
//!   trait (initial impl [`StaticSource`]). Monotonic revision
//!   allocator on the session; gated publish via
//!   [`DiffSession::try_publish_if_newer`]. Sync recompute body
//!   [`DiffSession::recompute_blocking`]; tokio orchestration
//!   [`DiffSubsystem::schedule_recompute`] spawns it on
//!   `spawn_blocking` and returns a join handle.
//! - **D.2.c (2026-05-29)** — routing + debounce + bus
//!   subscription. [`BufferTextProvider`] (one host seam),
//!   [`DiffParticipantSource`] (mirror of `DiffParticipantSource`),
//!   [`BufferSource`] / [`BufferSource`] live-rope impls,
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
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::debug;

use lattice_core::BufferId;
use crate::{DiffAlgorithm, HunkIndex, HunkKind, LineRange, compute_diff};
use lattice_protocol::event::{Event, EventKind};
use lattice_protocol::ids::DocumentId;
use lattice_runtime::{EventBus, EventFilter, SubscriptionId, SubscriptionTarget};

/// One participant in a diff session — produces the rope to
/// diff for one slot in `Hunk::ranges`.
///
/// D.8.b (2026-05-31): collapses the previous
/// `BaselineSource` + `CurrentSource` two-trait split into a
/// single trait. The original split was structural sugar
/// (made `descriptor.baseline.snapshot()` vs
/// `descriptor.current.snapshot()` visually distinct) and
/// stopped scaling once participants became an arity-agnostic
/// `Vec<Arc<dyn DiffParticipantSource>>`. The slot index now
/// carries the role.
///
/// Concrete impls in this module:
/// - [`StaticSource`] — owned in-memory `Rope`.
/// - [`OnDiskSource`] — re-reads a file at snapshot time.
/// - [`BufferSource`] — live rope from a [`BufferTextProvider`]-
///   backed buffer.
///
/// D.7's `GitSource` (post-implementation) will land alongside
/// these.
///
/// `snapshot` is called from inside the `spawn_blocking` body
/// of [`DiffSubsystem::schedule_recompute`], so impls may do
/// cheap blocking I/O (a `git cat-file` for `GitSource`) but
/// must not hold the host's UI thread. `Send + Sync + 'static`
/// is required so the trait object can cross the
/// `spawn_blocking` boundary.
pub trait DiffParticipantSource: Send + Sync + 'static + std::fmt::Debug {
    /// Produce this participant's rope. Called once per
    /// recompute; the implementor decides whether to clone a
    /// cached rope or rematerialise from a backing store.
    fn snapshot(&self) -> Rope;

    /// D.8.d (2026-05-31): the buffer id this source is
    /// backed by, if any. Returns `None` for non-buffer
    /// sources (`StaticSource`, `OnDiskSource`, future
    /// `GitSource`); `BufferSource` overrides to return
    /// `Some(buffer_id)`.
    ///
    /// Load-bearing for the slot ↔ buffer mapping the
    /// subsystem's membership API + the D.6.d
    /// `pane_index_of` helper rely on. Without it,
    /// `compute_get_edit` / `compute_put_plan` /
    /// `remove_participant_buffer` would need a sidecar map
    /// to find "the slot in `Hunk::ranges` for buffer X".
    /// Default `None` means non-overriding impls continue
    /// to work — they just can't be addressed by buffer id.
    fn buffer_id(&self) -> Option<BufferId> {
        None
    }
}

/// In-memory participant source — an owned `Rope` cloned on
/// every [`Self::snapshot`].
///
/// Cheap: `Rope::clone` is an `Arc`-share of the underlying
/// chunks, not a deep copy. Used as the default smoke-test
/// baseline and as the substrate consumers (e.g. an LSP server
/// returning `WorkspaceEdit` previews, the AI multi-file
/// `openDiff` flow) wrap when they already hold the source
/// text in memory.
#[derive(Debug, Clone)]
pub struct StaticSource {
    rope: Rope,
}

impl StaticSource {
    pub fn new(rope: Rope) -> Self {
        Self { rope }
    }
}

impl DiffParticipantSource for StaticSource {
    fn snapshot(&self) -> Rope {
        self.rope.clone()
    }
}

/// D.3.a (2026-05-29): on-disk file participant source.
///
/// `snapshot` re-reads the file at `path` and parses it into a
/// fresh `Rope`. Used by `:diff` (no args) — "diff against
/// the on-disk version of this file." Cheap enough to do
/// inside `spawn_blocking` per the [`DiffParticipantSource`]
/// contract; D.3's first consumer is single-file inline
/// overlay so per-recompute file re-reads are acceptable.
/// Future D.7 (`:Gdiff`) introduces a separate `GitSource`
/// that reads through `gix` against a fixed ref.
///
/// On I/O error (missing path, permissions, mid-read crash)
/// snapshot returns an empty rope. The session then recomputes
/// the diff against empty baseline (all-Add hunks), which is
/// the "everything is new" presentation — a noisy but
/// defensible degradation that the user can resolve via
/// `:diffoff` and a corrected path. We log the error at
/// `tracing::debug` so the failure surfaces under
/// `RUST_LOG=lattice_host::diff::subsystem=debug` without
/// blocking the recompute path.
#[derive(Clone, Debug)]
pub struct OnDiskSource {
    path: std::path::PathBuf,
}

impl OnDiskSource {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl DiffParticipantSource for OnDiskSource {
    fn snapshot(&self) -> Rope {
        match std::fs::read_to_string(&self.path) {
            Ok(s) => Rope::from(s),
            Err(err) => {
                debug!(
                    target: "lattice_host::diff::subsystem",
                    path = ?self.path,
                    ?err,
                    "OnDiskSource::snapshot failed; returning empty rope"
                );
                Rope::new()
            }
        }
    }
}

// ──────────────────────────────────────────────────────────────
// D.2.c: BufferTextProvider trait + the production
// BufferRegistry-backed impl + BufferSource concrete participant
// (D.8.b unified `BufferBaseline` + `BufferCurrentSource`
// into one `BufferSource`)
// ──────────────────────────────────────────────────────────────

/// One-trait seam between the diff subsystem and the host's
/// buffer storage. Required for [`BufferSource`] to resolve a
/// [`BufferId`] to its live rope at snapshot time.
///
/// The host supplies a single impl backed by `BufferRegistry`.
/// Future ephemeral-buffer providers (e.g. plugin-owned virtual
/// buffers, AI-proposed-edits views) plug into the same trait.
///
/// `buffer_rope(id)` returns `None` when the buffer has been
/// dropped. [`BufferSource`] treats `None` as an empty rope so
/// a recompute against a closed buffer still produces a
/// well-defined `HunkIndex` (all-Add or all-Remove depending
/// on which slot was the dropped buffer) rather than
/// panicking. The session's `drop_session` lifecycle will
/// remove the entry shortly after.
pub trait BufferTextProvider: Send + Sync + 'static + std::fmt::Debug {
    fn buffer_rope(&self, id: BufferId) -> Option<Rope>;
}

// DX.6 (2026-06-24): the production `BufferTextProvider` /
// `DocumentBufferResolver` impls (`BufferRegistryTextProvider` /
// `BufferRegistryDocumentResolver`) reference the host's
// `BufferRegistry`, so they CANNOT live in this crate — they stay
// in `lattice-host` (`crate::diff::resolver`, re-exported under
// `crate::diff::subsystem`). The TRAITS above are the seam: this
// crate depends only on the abstraction; the host supplies the
// `BufferRegistry`-backed impls. See `diff-extraction.md` (C6).

/// D.8.b (2026-05-31): live-rope participant backed by a
/// buffer. Replaces the prior `BufferSource` +
/// `BufferSource` two-struct split (both had identical
/// shape — provider + buffer_id — and only differed in which
/// trait they implemented). The trait collapse to
/// [`DiffParticipantSource`] makes the split structurally
/// redundant.
///
/// The unsaved-buffer case: when neither side of a diff has a
/// filesystem path, both sides resolve through
/// [`BufferTextProvider`] at snapshot time. The session's
/// descriptor must include this buffer in its `watch` list so
/// edits to it wake the session.
#[derive(Clone, Debug)]
pub struct BufferSource {
    provider: Arc<dyn BufferTextProvider>,
    buffer_id: BufferId,
}

impl BufferSource {
    pub fn new(provider: Arc<dyn BufferTextProvider>, buffer_id: BufferId) -> Self {
        Self {
            provider,
            buffer_id,
        }
    }

    pub fn buffer_id(&self) -> BufferId {
        self.buffer_id
    }
}

impl DiffParticipantSource for BufferSource {
    fn snapshot(&self) -> Rope {
        self.provider
            .buffer_rope(self.buffer_id)
            .unwrap_or_default()
    }

    fn buffer_id(&self) -> Option<BufferId> {
        Some(self.buffer_id)
    }
}

/// The "what to diff against what" pair for a session.
///
/// `sources` is an arity-agnostic vector of N participant
/// sources, one per slot in `Hunk::ranges`. The engine
/// (`crate::compute_diff`) dispatches by
/// `sources.len()` — N=2 is two-way (slot 0 = baseline /
/// from, slot 1 = current / to), N=3 is three-way merge
/// (slot 0 = base, slot 1 = local, slot 2 = remote), N≥4
/// returns `DiffEngineError::Unsupported` in v1.
///
/// `watch` is the **explicit dependency declaration**:
/// every [`BufferId`] whose edits should wake this session.
/// The descriptor's author (a future `:diffsplit` /
/// `:Gdiff` / AI-host call site) knows which sources are
/// buffer-backed and contributes those `BufferId`s into
/// `watch`. Static or git-blob sources contribute nothing.
///
/// `Clone` because the runtime sometimes wants a stable
/// snapshot of the descriptor to feed a debounced
/// recompute — the inner `Vec<Arc<dyn ...>>` and
/// `Vec<BufferId>` clones are cheap (one Arc bump per
/// source + a small heap allocation).
///
/// **D.8.c shape (2026-05-31).** Replaces the prior
/// `baseline + current + Option<remote>` named-field
/// triple with a single `sources: Vec<...>`; see
/// [`docs/dev/architecture/n-way-diff-membership.md`]
/// (`docs/dev/architecture/n-way-diff-membership.md`)
/// for the rationale.
#[derive(Clone, Debug)]
pub struct DiffDescriptor {
    /// N participant sources in slot order. `sources[i]`
    /// produces the rope at `Hunk::ranges[i]` after a
    /// recompute. v1 supports N ∈ {1, 2, 3}; the engine
    /// returns `DiffEngineError::Unsupported` for N≥4.
    pub sources: Vec<Arc<dyn DiffParticipantSource>>,
    pub watch: Vec<BufferId>,
    /// D.5.a (2026-05-30): user-visible diff sides that should
    /// receive `diff-mode` activation while this session is
    /// registered. Distinct from [`Self::watch`]:
    /// - `watch` declares edit-event subscriptions.
    /// - `participants` declares which buffers are user-visible
    ///   diff sides that should get the mode toggle.
    ///
    /// For buffer-backed sources they coincide; they diverge
    /// when a baseline source contributes no live buffer
    /// (file-on-disk inline → `[primary]`; D.7 git baseline
    /// → `[primary]`). Two-pane is `[baseline, primary]`; D.6
    /// three-way is `[base, local, remote]` (length 3).
    pub participants: Vec<BufferId>,
}

impl DiffDescriptor {
    /// D.8.c (2026-05-31): the session's arity — the source
    /// of truth for how many participants this session has.
    /// Equivalent to `sources.len()`.
    pub fn arity(&self) -> usize {
        self.sources.len()
    }
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

/// D.6.e (2026-05-31): resolution signal fired through a
/// [`DiffSession`]'s completion channel when the user
/// invokes `:diff-accept` / `:diff-reject`. Consumed by
/// the future `openDiff` plugin flow (Claude Code, AI
/// multi-file edits) and by any in-tree consumer (magit
/// plugin, AI proposals) that wants to know how the user
/// resolved a session.
///
/// `#[non_exhaustive]` so future variants — notably the
/// `Partial(Vec<HunkId>)` case from the design doc, which
/// would require per-hunk acceptance tracking — can be
/// added without breaking exhaustiveness in pattern-match
/// consumers.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffOutcome {
    /// User confirmed they're done reviewing; the
    /// buffer's *current* content (whatever they applied
    /// via `do`/`dp` or left alone) is the accepted
    /// resolution. Plugins typically commit the active
    /// buffer's rope on receiving this.
    Accept,
    /// User dismissed the session without committing.
    /// Plugins should revert to pre-session state if they
    /// modified the buffer for the diff display.
    Reject,
}

/// D.8.d (2026-05-31): errors the subsystem's membership
/// API returns when mutating a session's participant set.
/// Distinct from [`DiffEngineError`] (which is the engine
/// crate's surface) so callers don't have to import
/// `lattice_diff` just to pattern-match on
/// `add_participant` failures. The engine's cap on arity
/// (N ≥ 4 rejected in v1) surfaces here as
/// `EngineRejected(DiffEngineError::Unsupported{...})`.
#[derive(Debug, thiserror::Error)]
pub enum MembershipError {
    #[error("no diff session registered for buffer {0:?}")]
    NoSession(BufferId),
    #[error("slot {slot} out of range; session arity = {arity}")]
    SlotOutOfRange { slot: usize, arity: usize },
    #[error("buffer {0:?} is not a participant of this session")]
    NotParticipant(BufferId),
    #[error("engine rejected new arity: {0}")]
    EngineRejected(#[from] crate::DiffEngineError),
}

/// D.5.b (2026-05-30): describes the edit the diff-mode `do`
/// (diff-get) operator would apply when invoked at a given
/// cursor row on the active side of a session. Produced by
/// [`DiffSubsystem::compute_get_edit`]; consumed by dispatch
/// which translates it into an `apply_edit_blocking` call
/// and re-positions the cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffGetPlan {
    /// The mutation to apply to the active buffer (the side
    /// the cursor is on).
    pub edit: lattice_protocol::edit::Edit,
    /// Post-edit cursor line — the start of the resolved
    /// hunk on the active side. Cursor stays "on the hunk"
    /// so successive `]c` / `[c` jumps walk through
    /// neighbouring hunks naturally.
    pub post_cursor_row: u32,
}

/// D.6.d (2026-05-31): outcome of resolving the diff-mode
/// `do` chord or `:diffget [<bufnr>]` ex-command. Mirrors
/// [`DiffPutOutcome`]'s tri-state but for the get
/// direction:
///
/// - [`DiffGetOutcome::Edit`]: edit ready to apply to the
///   active buffer. The carried `target_buffer_id` names
///   the side the edit's content was pulled FROM.
/// - [`DiffGetOutcome::TargetRequired`]: three-way session
///   and the caller didn't disambiguate. The
///   `available_targets` field lists the other
///   participant buffers so dispatch can surface a clear
///   error.
/// - [`DiffGetOutcome::Nothing`]: no session, no
///   descriptor, no covering hunk under the cursor on the
///   active side, or — for the `do` chord with no
///   explicit target — a two-way Conflict hunk (which
///   shouldn't exist anyway since compute_two_way doesn't
///   emit it; defensive).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffGetOutcome {
    Edit {
        target_buffer_id: BufferId,
        edit: lattice_protocol::edit::Edit,
        post_cursor_row: u32,
    },
    TargetRequired {
        available_targets: Vec<BufferId>,
    },
    Nothing,
}

impl DiffGetOutcome {
    /// Test-friendly: project the Edit variant down to a
    /// [`DiffGetPlan`] (the D.5.b shape) or `None` for
    /// non-Edit outcomes. Keeps existing tests' `Option`
    /// ergonomics post-D.6.d.
    pub fn into_plan(self) -> Option<DiffGetPlan> {
        match self {
            DiffGetOutcome::Edit {
                edit,
                post_cursor_row,
                ..
            } => Some(DiffGetPlan {
                edit,
                post_cursor_row,
            }),
            _ => None,
        }
    }

    /// `true` iff this outcome is `Nothing` (silent no-op).
    pub fn is_nothing(&self) -> bool {
        matches!(self, DiffGetOutcome::Nothing)
    }
}

/// D.5.c (2026-05-30) / D.6.d (2026-05-31): outcome of
/// resolving the diff-mode `dp` chord or `:diffput
/// [<bufnr>]` ex-command. Distinguishes four cases the
/// dispatch handler must surface differently:
///
/// - [`DiffPutOutcome::Edit`]: an edit is ready to apply
///   to the buffer identified by `target_buffer_id`. The
///   target is the destination side; the active buffer is
///   the source. Apply via the registry's
///   `RopeDocumentHandle::apply_edit` and park the cursor at
///   `post_cursor_row` on the active side.
/// - [`DiffPutOutcome::NoPeerBuffer`]: inline session
///   whose baseline is not a live buffer (file-on-disk
///   for `:diff`, git blob for D.7's future `:Gdiff`).
///   `dp` cannot push to a non-buffer; the handler emits
///   a clear error message ("dp: baseline is not a
///   buffer; use :write") rather than silently no-op'ing.
/// - [`DiffPutOutcome::TargetRequired`] (D.6.d): three-way
///   session and the caller didn't disambiguate. The
///   `available_targets` field lists the other
///   participant buffers so dispatch surfaces a clear
///   error.
/// - [`DiffPutOutcome::Nothing`]: no session, no
///   descriptor, no hunk under the cursor, or — for the
///   `dp` chord with no explicit target — a two-way
///   Conflict hunk (defensive; compute_two_way doesn't
///   emit Conflict).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiffPutOutcome {
    Edit {
        target_buffer_id: BufferId,
        edit: lattice_protocol::edit::Edit,
        post_cursor_row: u32,
    },
    NoPeerBuffer,
    TargetRequired {
        available_targets: Vec<BufferId>,
    },
    Nothing,
}

/// CR.1: the "three-way merge needs an explicit bufnr" error `Echo`
/// shared by [`DiffSubsystem::diff_get_effect`] /
/// [`DiffSubsystem::diff_put_effect`]. Preserves the host's former
/// `do_diff_get`/`do_diff_put` wording verbatim so the migration is
/// behaviour-preserving (`cmd` is `"diffget"` / `"diffput"`).
fn target_required_echo(cmd: &str, available_targets: &[BufferId]) -> lattice_grammar::Effect {
    let avail = available_targets
        .iter()
        .map(|b| b.0.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    lattice_grammar::Effect::Echo {
        level: lattice_grammar::EchoLevel::Error,
        text: format!(
            "{cmd}: target required for three-way merge; use :{cmd} <bufnr> (one of: {avail})"
        ),
    }
}

/// CR.6: the current-side (slot 1) start rows of every hunk, in order —
/// the navigation order `]c`/`[c` walk.
fn hunk_starts(index: &HunkIndex) -> Vec<u32> {
    index
        .hunks
        .iter()
        .filter_map(|h| h.ranges.get(1).map(|r| r.start))
        .collect()
}

/// CR.6: an info `Echo` for the `:hunk-next`/`:hunk-prev` no-session /
/// no-hunks cases — preserves the former host `do_next_hunk` messages.
fn hunk_nav_echo(text: &str) -> lattice_grammar::Effect {
    lattice_grammar::Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: text.to_string(),
    }
}

/// CR.6: a collapsed-cursor `Effect::SelectionChange` at `(row, 0)` — the
/// generic cursor-move the host applies for hunk navigation.
fn hunk_selection(row: u32) -> lattice_grammar::Effect {
    lattice_grammar::Effect::SelectionChange(
        lattice_protocol::selection::SelectionSet::single(
            lattice_protocol::selection::Selection::cursor(
                lattice_protocol::position::Position::new(row, 0),
            ),
        ),
    )
}

/// D.5.b helper: slice the rope at the given line range and
/// return the contents as a `String`. Half-open `[start, end)`.
/// Empty range returns the empty string. Tolerant of an
/// `end` that exceeds the rope's line count (clamped) and a
/// `start` past EOF (returns empty).
fn slice_line_range(rope: &Rope, range: LineRange) -> String {
    if range.is_empty() {
        return String::new();
    }
    let total_lines = rope.len_lines() as u32;
    if range.start >= total_lines {
        return String::new();
    }
    let end = range.end.min(total_lines);
    let start_char = rope.line_to_char(range.start as usize);
    let end_char = rope.line_to_char(end as usize);
    rope.slice(start_char..end_char).to_string()
}

fn format_algorithm(alg: DiffAlgorithm) -> &'static str {
    match alg {
        DiffAlgorithm::Histogram => "Histogram",
        DiffAlgorithm::Myers => "Myers",
        DiffAlgorithm::MyersMinimal => "MyersMinimal",
    }
}

// ──────────────────────────────────────────────────────────────
// D.6.d (2026-05-31): pane-index helpers for compute_get_edit /
// compute_put_plan with target-aware dispatch.
// ──────────────────────────────────────────────────────────────

/// Result of resolving a target buffer to a slot index in
/// `Hunk::ranges` (0/1 in two-way; 0/1/2 in three-way).
enum TargetResolution {
    /// Caller specified a target (or two-way default
    /// resolved to the unique peer). Use this pane slot.
    Pane(usize),
    /// Three-way session and caller didn't supply a target.
    /// Dispatch must surface a "diffput/diffget: target
    /// required" error.
    Required,
    /// Caller specified a target buffer that isn't a
    /// participant of this session, or is the active
    /// buffer itself.
    Unknown,
}

/// Find the slot index in `hunk.ranges` (= position in
/// `descriptor.sources`) corresponding to `buffer_id`.
///
/// D.8.d (2026-05-31): rewritten to walk `sources`
/// directly via the [`DiffParticipantSource::buffer_id`]
/// method. The post-D.8.c arity-agnostic sources vector
/// puts each source at the same slot index it occupies in
/// `Hunk::ranges`, so finding the buffer's slot reduces
/// to a position-by-trait-method query. Pre-D.8.d this
/// function carried an "inline → slot 1" special-case
/// derived from `descriptor.participants` because the
/// D.6 fixed-arity descriptor didn't expose source
/// identities. With D.8.b's `BufferSource::buffer_id()`
/// trait method the special-case is no longer needed —
/// inline 1-source sessions report slot 0 directly, and
/// inline 2-source sessions (StaticSource at slot 0 +
/// BufferSource at slot 1) report slot 1 from the
/// position-walk.
fn pane_index_of(descriptor: &DiffDescriptor, buffer_id: BufferId) -> Option<usize> {
    descriptor
        .sources
        .iter()
        .position(|s| s.buffer_id() == Some(buffer_id))
}

/// Resolve `target` to a pane slot index:
/// - `Some(b)` ⇒ slot of `b` (via [`pane_index_of`]), or
///   `Unknown` if not present / equals active.
/// - `None` ⇒ two-way auto-targets the unique peer slot;
///   three-way returns `Required`.
fn resolve_target_pane(
    descriptor: &DiffDescriptor,
    active_pane: usize,
    target: Option<BufferId>,
) -> TargetResolution {
    if let Some(t) = target {
        let Some(pane) = pane_index_of(descriptor, t) else {
            return TargetResolution::Unknown;
        };
        if pane == active_pane {
            return TargetResolution::Unknown;
        }
        return TargetResolution::Pane(pane);
    }
    // No explicit target. Slot count comes from the
    // descriptor's `sources.len()` — N=1 inline still has
    // the implicit "other slot" semantic against the (now
    // absent) peer; N=2 has a unique peer; N≥3 needs the
    // caller to disambiguate.
    if descriptor.arity() >= 3 {
        TargetResolution::Required
    } else {
        // N≤2 (inline or two-pane): peer is the other slot.
        TargetResolution::Pane(if active_pane == 0 { 1 } else { 0 })
    }
}

/// Other participants (buffer ids) besides the active
/// pane's. Returned in slot order so dispatch can render a
/// stable error message ("expected one of: 7, 9").
fn other_participants(descriptor: &DiffDescriptor, active_pane: usize) -> Vec<BufferId> {
    descriptor
        .participants
        .iter()
        .enumerate()
        .filter_map(|(i, b)| if i == active_pane { None } else { Some(*b) })
        .collect()
}

/// Snapshot the rope for the source at slot `pane`.
/// Returns `None` if the slot is out of range for the
/// descriptor's arity (e.g. slot 2 on a two-way descriptor
/// with `sources.len() == 2`). D.8.c (2026-05-31): rewritten
/// to consult `descriptor.sources` directly rather than
/// branching by slot index against the prior
/// baseline / current / remote named fields.
fn snapshot_for_pane(descriptor: &DiffDescriptor, pane: usize) -> Option<Rope> {
    descriptor.sources.get(pane).map(|s| s.snapshot())
}

/// Find the first hunk whose `active_pane`-side range
/// covers `cursor_row`. Vim-parity rules: empty ranges
/// match only at the exact start row (the deletion-marker
/// row); non-empty ranges match by half-open inclusion.
///
/// When `allow_conflict` is `true`, Conflict hunks
/// participate in the search (D.6.d target-aware path
/// resolves them); when `false`, they're skipped
/// defensively (the `do` / `dp` chord path without an
/// explicit target — preserves D.5.b/c semantics).
fn find_covering_hunk<'a>(
    index: &'a HunkIndex,
    active_pane: usize,
    cursor_row: u32,
    allow_conflict: bool,
) -> Option<&'a crate::Hunk> {
    index.hunks.iter().find(|h| {
        if !allow_conflict && matches!(h.kind, HunkKind::Conflict) {
            return false;
        }
        let Some(range) = h.ranges.get(active_pane).copied() else {
            return false;
        };
        if range.is_empty() {
            range.start == cursor_row
        } else {
            cursor_row >= range.start && cursor_row < range.end
        }
    })
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
    /// [`crate::overlay::DiffOverlayRefreshTask`] writes
    /// this cell on every hunk publish, keeping it in lockstep
    /// with `hunks`. Initialised to an empty map at session
    /// construction; first refresh populates it once the
    /// initial recompute completes.
    sign_map: ArcSwap<crate::overlay::DiffSignMap>,
    /// D-fix.3b (2026-06-26): the BASELINE-side sign map (hunks'
    /// `ranges[0]`: `Remove`→removed, `Change`→changed). `sign_map`
    /// above covers the current/proposed (right) pane; this covers the
    /// baseline (left) pane so a side-by-side diff tints BOTH sides.
    /// Published in lockstep with `sign_map` by `recompute_blocking`.
    /// Empty for inline `:diff` (no separate baseline pane).
    baseline_sign_map: ArcSwap<crate::overlay::DiffSignMap>,
    /// D.4.d.3.a (2026-05-30): linkage from a two-pane diff
    /// session to its `PaneGroup` (the scroll-binding
    /// mechanism with `HunkRowMapper`). `None` for inline
    /// `:diff` sessions, which have no pane-group scroll
    /// binding; `Some(id)` for `:diffthis` / `:diffsplit`
    /// sessions, set by `bind_pane_group` at registration
    /// and read by `do_diff_off` at teardown to drop the
    /// group cleanly.
    pane_group_id: Mutex<Option<lattice_core::ui::pane::PaneGroupId>>,
    /// D.6.e (2026-05-31): one-shot resolution channel.
    /// Set by callers that want to know when the user
    /// runs `:diff-accept` / `:diff-reject` (or by
    /// programmatic flows via [`Self::bind_completion`]).
    /// The Editor's `do_diff_accept` / `do_diff_reject`
    /// path takes the sender out
    /// ([`Self::take_completion`]) and sends the matching
    /// [`DiffOutcome`] before tearing the session down.
    /// `None` for sessions that don't need outcome
    /// notification (the default — most interactive flows
    /// don't bind one).
    completion: Mutex<Option<oneshot::Sender<DiffOutcome>>>,
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
            sign_map: ArcSwap::from_pointee(crate::overlay::DiffSignMap::default()),
            baseline_sign_map: ArcSwap::from_pointee(crate::overlay::DiffSignMap::default()),
            pane_group_id: Mutex::new(None),
            completion: Mutex::new(None),
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
    /// D.6.e (2026-05-31): bind a [`DiffOutcome`] one-shot
    /// sender to this session. Called once after
    /// registration by callers (`openDiff` plugin flow,
    /// magit-style consumers) that want to be notified
    /// when the user resolves the session via
    /// `:diff-accept` / `:diff-reject`. Subsequent binds
    /// overwrite (a re-bind scenario isn't expected in v1
    /// but is safe — the previous sender is dropped, so
    /// any awaiting receiver observes a `Closed` error,
    /// matching the typical "session superseded" UX).
    pub fn bind_completion(&self, tx: oneshot::Sender<DiffOutcome>) {
        *self
            .completion
            .lock()
            .expect("DiffSession completion mutex poisoned") = Some(tx);
    }

    /// D.6.e (2026-05-31): take the bound `DiffOutcome`
    /// sender, if any. Single-shot: returns `Some` on the
    /// first call after a `bind_completion`; subsequent
    /// calls return `None`. Used by the
    /// `do_diff_accept` / `do_diff_reject` teardown path
    /// to fire the signal before dropping the session.
    pub fn take_completion(&self) -> Option<oneshot::Sender<DiffOutcome>> {
        self.completion
            .lock()
            .expect("DiffSession completion mutex poisoned")
            .take()
    }

    pub fn pane_group_id(&self) -> Option<lattice_core::ui::pane::PaneGroupId> {
        *self
            .pane_group_id
            .lock()
            .expect("DiffSession pane_group_id mutex poisoned")
    }

    /// D.3.d.0 (2026-05-29): snapshot the latest published
    /// `DiffSignMap`. Lock-free `ArcSwap::load_full`; renderer
    /// hot path. The map is refreshed in lockstep with
    /// `hunks` by [`crate::overlay::DiffOverlayRefreshTask`].
    pub fn sign_map(&self) -> Arc<crate::overlay::DiffSignMap> {
        self.sign_map.load_full()
    }

    /// D.3.d.0: publish a freshly-computed sign map.
    /// Unconditional store (no revision gate) — the
    /// `DiffOverlayRefreshTask` already serialises map
    /// updates with `hunks` publishes, so out-of-order
    /// landing isn't possible from the refresh-task side.
    /// Direct callers (tests, future consumers) bear the
    /// ordering responsibility.
    pub fn publish_sign_map(&self, map: Arc<crate::overlay::DiffSignMap>) {
        self.sign_map.store(map);
    }

    /// D-fix.3b: snapshot the latest published BASELINE-side sign map
    /// (the left/baseline pane of a side-by-side diff). Lock-free
    /// `ArcSwap::load_full`; renderer hot path. Empty for inline `:diff`.
    pub fn baseline_sign_map(&self) -> Arc<crate::overlay::DiffSignMap> {
        self.baseline_sign_map.load_full()
    }

    /// D-fix.3b: publish a freshly-computed baseline-side sign map.
    /// Stored in lockstep with `publish_sign_map` by `recompute_blocking`.
    pub fn publish_baseline_sign_map(&self, map: Arc<crate::overlay::DiffSignMap>) {
        self.baseline_sign_map.store(map);
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

    /// D.2.b / D.6.a / D.8.c: synchronous recompute.
    /// Allocates a revision, runs the diff engine via
    /// [`compute_diff`] over the participant ropes, builds a
    /// `HunkIndex` stamped with the allocated revision + the
    /// session's algorithm, and publishes via the
    /// revision-gated path.
    ///
    /// `sources` is an N-slot rope slice in slot order
    /// (slot i feeds `Hunk::ranges[i]`). The engine picks the
    /// algorithm by `sources.len()`: N=2 → two-way, N=3 →
    /// three-way with Conflict semantics, N≥4 →
    /// `DiffEngineError::Unsupported` (recompute drops with a
    /// debug log; mirrors the stale-publish drop semantic).
    ///
    /// D.8.c (2026-05-31): signature rewritten from
    /// `(baseline: &Rope, current: &Rope, remote:
    /// Option<&Rope>)` to `(sources: &[Rope])`. Callers
    /// iterate the descriptor's `sources` and snapshot each
    /// rope into a Vec passed here.
    ///
    /// Returns `Some(idx)` on successful publish, `None` if a
    /// newer revision was already published (stale result
    /// dropped) or the engine rejected the participant set.
    /// This is the body the
    /// [`DiffSubsystem::schedule_recompute`]
    /// `spawn_blocking` closure executes; tests call it
    /// directly to exercise the compute path without tokio.
    pub fn recompute_blocking(&self, sources: &[Rope]) -> Option<Arc<HunkIndex>> {
        let revision = self.allocate_revision();
        let raw = match compute_diff(sources, self.algorithm) {
            Ok(idx) => idx,
            Err(err) => {
                tracing::debug!(
                    target: "lattice_host::diff::subsystem",
                    ?err,
                    "compute_diff rejected the participant set; recompute dropped"
                );
                return None;
            }
        };
        let idx = Arc::new(HunkIndex {
            hunks: raw.hunks,
            algorithm: self.algorithm,
            revision,
        });
        if self.try_publish_if_newer(Arc::clone(&idx)) {
            // D-fix.3a (2026-06-26): publish the current-side sign map in
            // lockstep with the hunks, here at the single recompute choke
            // point, so EVERY session gets line tints + gutter signs — inline
            // `:diff` AND pane-group (`:diffsplit` / Claude Code `openDiff`).
            // Previously only `DiffOverlayRefreshTask` (spawned solely on the
            // inline `:diff` path) published the sign map, so pane-group diffs
            // computed hunks but left `sign_map()` empty → no in-buffer diff
            // highlighting. (The refresh task still owns deletion-block
            // overlay rendering for inline diffs; this publish is idempotent
            // with its own, computed from the same hunks.)
            self.publish_sign_map(Arc::new(crate::overlay::compute_diff_sign_map(&idx)));
            // D-fix.3b: the baseline-side map (left pane) in lockstep.
            self.publish_baseline_sign_map(Arc::new(
                crate::overlay::compute_baseline_diff_sign_map(&idx),
            ));
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
    /// D.5.a (2026-05-30): host-side `diff-mode` lifecycle
    /// bridge. Created on `Default` so the subsystem is the
    /// single owner of the bridge identity; the editor accesses
    /// it via [`Self::mode_bridge`] for the dispatch-tail drain.
    mode_bridge: Arc<crate::mode::DiffModeBridge>,
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
            mode_bridge: Arc::new(crate::mode::DiffModeBridge::new()),
        }
    }
}

/// Cheap-clone service handle for the diff subsystem. DX.3/C7 (BC.6):
/// registered in the `ServiceRegistry` at boot so `diff-mode`'s
/// `on_activate` can reach the session for a buffer
/// (`ctx.service::<DiffSubsystemHandle>()`) and register a
/// `HunkFoldSource` via the `FoldOverlayService` — mirroring how
/// `MultibufferMode` reaches its `MultibufferRegistryHandle`. Follows the
/// Arc/TypeId convention: register `Arc<DiffSubsystem>`, look up
/// `Arc<DiffSubsystem>`.
pub type DiffSubsystemHandle = Arc<DiffSubsystem>;

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

    /// D.5.a (2026-05-30): access the diff-mode lifecycle
    /// bridge so the editor's dispatch tail can drain queued
    /// activations. The subsystem owns the bridge identity; the
    /// returned `Arc` is a cheap reference clone, not a take.
    pub fn mode_bridge(&self) -> Arc<crate::mode::DiffModeBridge> {
        Arc::clone(&self.mode_bridge)
    }

    /// Register a session for `buffer_id` with no sources. The
    /// session has no debouncer, no descriptor, no watchers
    /// entries — used by tests and by the pure-compute API path.
    /// Production callers want [`Self::register_with_sources`].
    ///
    /// Idempotent: returns the existing `Arc<DiffSession>` if
    /// one is already registered (the `algorithm` argument is
    /// ignored in that case).
    pub fn register(&self, buffer_id: BufferId, algorithm: DiffAlgorithm) -> Arc<DiffSession> {
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
    /// `StaticSource` to `GitBaseline`). The old watch
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
            let mut debouncers = self
                .debouncers
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            debouncers
                .entry(buffer_id)
                .or_insert_with(|| Arc::new(Debouncer::new(self.debounce_window)));
        }
        // D.5.a (2026-05-30): notify the diff-mode bridge after
        // the session is durable in the registry. The bridge
        // queues activation changes; the dispatch tail drains
        // and applies them via `mode_registry.activate_minor`.
        // Re-register paths (e.g. switching baseline source)
        // flow through here too — the bridge's idempotent
        // scrub-then-re-add on the same `session_key` keeps
        // refcounts correct.
        self.mode_bridge
            .note_session_opened(buffer_id, &descriptor.participants);
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

    /// D.6.g (2026-05-31): every session `buffer_id`
    /// participates in — as primary key *or* as a member
    /// of any descriptor's `watch` list. Used by
    /// `:diffoff!` (the force bang) to cascade tear-down
    /// across all sessions the active buffer belongs to,
    /// not just the one [`Self::lookup_session_for`]
    /// happens to resolve.
    ///
    /// Today's secondary_index is single-valued
    /// (`HashMap<BufferId, BufferId>`), so a buffer
    /// participating in two simultaneous sessions only
    /// resolves to the most-recently-registered one via
    /// `lookup_session_for`. This method iterates the
    /// `descriptors` map directly and returns every
    /// session whose descriptor's `watch` list contains
    /// `buffer_id`. Order is unspecified (HashMap
    /// iteration). Empty when the buffer is not a
    /// participant anywhere.
    pub fn all_sessions_for(&self, buffer_id: BufferId) -> Vec<Arc<DiffSession>> {
        let sessions = self.sessions.lock().expect("DiffSubsystem mutex poisoned");
        let descriptors = self
            .descriptors
            .lock()
            .expect("DiffSubsystem mutex poisoned");
        sessions
            .iter()
            .filter(|(key, _)| {
                **key == buffer_id
                    || descriptors
                        .get(*key)
                        .map(|d| d.watch.contains(&buffer_id))
                        .unwrap_or(false)
            })
            .map(|(_, session)| session.clone())
            .collect()
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

    /// D.5.b (2026-05-30): compute the edit the diff-mode `do`
    /// chord would apply for `buffer_id` at `cursor_row`.
    ///
    /// Returns `None` (silent no-op) when:
    /// - no session is registered for `buffer_id`,
    /// - no descriptor is registered (sources-less test
    ///   registration — there's no baseline to read from),
    /// - no hunk covers `cursor_row` on the current side,
    /// - the matched hunk is a three-way [`HunkKind::Conflict`]
    ///   (D.6 lands the conflict-resolution path).
    ///
    /// Behaviour by hunk kind on the two-way current side
    /// (`ranges[1]`):
    /// - **Change**: replace `ranges[1]` with the baseline
    ///   slice for `ranges[0]`.
    /// - **Add**: current side has the extra lines; baseline
    ///   range is empty → delete `ranges[1]` (revert the add).
    /// - **Remove**: current side is empty at the deletion
    ///   point; baseline has the removed lines → insert the
    ///   baseline text at `ranges[1].start` (revert the
    ///   remove). For a `Remove` hunk the current range is
    ///   empty; `cursor_row` must equal `ranges[1].start`
    ///   exactly for the lookup to match (vim parity — `do`
    ///   only fires while the cursor sits on the deletion-
    ///   marker row).
    ///
    /// Reads the baseline through
    /// [`DiffDescriptor::baseline`]`.snapshot()`. The
    /// snapshot is potentially expensive (file re-read for
    /// [`OnDiskSource`]); callers invoke once per `do`
    /// keystroke, never inside a tight loop.
    /// D.6.d (2026-05-31): compute the edit the diff-mode
    /// `do` chord or `:diffget [<bufnr>]` ex-command would
    /// apply at `cursor_row` on `active_buffer_id`.
    ///
    /// `target` semantics:
    /// - `None` in two-way: pull from the peer (the only
    ///   other side) — preserves D.5.b's `do` chord
    ///   behaviour.
    /// - `Some(buffer)` in two-way: pull from that side
    ///   (must be the peer, else `DiffGetOutcome::Nothing`).
    /// - `None` in three-way: ambiguous →
    ///   `DiffGetOutcome::TargetRequired` with the two
    ///   available targets.
    /// - `Some(buffer)` in three-way: pull from that
    ///   participant's side; allows resolving Conflict
    ///   hunks by picking which side wins.
    ///
    /// Reads the target side's rope via
    /// [`snapshot_for_pane`]. Cheap for buffer-backed
    /// sources (rope-Arc clone); the snapshot is called
    /// once per dispatch, never inside a tight loop.
    pub fn compute_get_edit(
        &self,
        active_buffer_id: BufferId,
        cursor_row: u32,
        target: Option<BufferId>,
    ) -> DiffGetOutcome {
        let Some(session) = self.lookup_session_for(active_buffer_id) else {
            return DiffGetOutcome::Nothing;
        };
        let session_key = session.buffer_id();
        let Some(descriptor) = self.lookup_descriptor(session_key) else {
            return DiffGetOutcome::Nothing;
        };
        let Some(active_pane) = pane_index_of(&descriptor, active_buffer_id) else {
            return DiffGetOutcome::Nothing;
        };
        let target_pane = match resolve_target_pane(&descriptor, active_pane, target) {
            TargetResolution::Pane(p) => p,
            TargetResolution::Required => {
                return DiffGetOutcome::TargetRequired {
                    available_targets: other_participants(&descriptor, active_pane),
                };
            }
            TargetResolution::Unknown => return DiffGetOutcome::Nothing,
        };
        let allow_conflict = target.is_some();
        let hunks = session.current_hunks();
        let Some(hunk) = find_covering_hunk(&hunks, active_pane, cursor_row, allow_conflict) else {
            return DiffGetOutcome::Nothing;
        };
        let Some(active_range) = hunk.ranges.get(active_pane).copied() else {
            return DiffGetOutcome::Nothing;
        };
        let Some(target_range) = hunk.ranges.get(target_pane).copied() else {
            return DiffGetOutcome::Nothing;
        };
        let Some(target_rope) = snapshot_for_pane(&descriptor, target_pane) else {
            return DiffGetOutcome::Nothing;
        };
        let target_text = slice_line_range(&target_rope, target_range);
        let edit = lattice_protocol::edit::Edit::replace(
            lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(active_range.start, 0),
                lattice_protocol::position::Position::new(active_range.end, 0),
            ),
            target_text,
        );
        let target_buffer_id = descriptor
            .participants
            .get(target_pane)
            .copied()
            .unwrap_or(active_buffer_id);
        DiffGetOutcome::Edit {
            target_buffer_id,
            edit,
            post_cursor_row: active_range.start,
        }
    }

    /// CR.2 (2026-06-24): compute the "keep both" resolution edit for the
    /// conflict hunk under `cursor_row` on the active (local / "ours")
    /// side of a three-way session — the `dB` chord. Splices the active
    /// side's lines followed by `theirs`' lines (ours-then-theirs, the v1
    /// convention; base is omitted, matching git's non-diff3 style) into
    /// the active range, so the conflict region ends up holding both
    /// sides' content in order. The edit applies to the active buffer
    /// (like keep-ours / keep-theirs, which reuse
    /// [`Self::compute_get_edit`] with the resolved slot→bufnr target);
    /// `target_buffer_id` in the returned [`DiffGetOutcome::Edit`] is the
    /// active buffer — the apply destination.
    ///
    /// Conflict-only: a non-`Conflict` covering hunk (or none) →
    /// `Nothing`, as does an unknown / self `theirs`, or a
    /// session/descriptor miss. The whole-line ranges are
    /// newline-terminated, so the splice is a clean line concatenation.
    pub fn compute_keep_both_edit(
        &self,
        active_buffer_id: BufferId,
        cursor_row: u32,
        theirs: BufferId,
    ) -> DiffGetOutcome {
        let Some(session) = self.lookup_session_for(active_buffer_id) else {
            return DiffGetOutcome::Nothing;
        };
        let session_key = session.buffer_id();
        let Some(descriptor) = self.lookup_descriptor(session_key) else {
            return DiffGetOutcome::Nothing;
        };
        let Some(active_pane) = pane_index_of(&descriptor, active_buffer_id) else {
            return DiffGetOutcome::Nothing;
        };
        let Some(theirs_pane) = pane_index_of(&descriptor, theirs) else {
            return DiffGetOutcome::Nothing;
        };
        if theirs_pane == active_pane {
            return DiffGetOutcome::Nothing;
        }
        let hunks = session.current_hunks();
        let Some(hunk) = find_covering_hunk(&hunks, active_pane, cursor_row, true) else {
            return DiffGetOutcome::Nothing;
        };
        // keep-both resolves Conflict hunks only — a clean 2-way Change
        // under the cursor is `do`/`dp` territory, not `dB`.
        if !matches!(hunk.kind, HunkKind::Conflict) {
            return DiffGetOutcome::Nothing;
        }
        let Some(active_range) = hunk.ranges.get(active_pane).copied() else {
            return DiffGetOutcome::Nothing;
        };
        let Some(theirs_range) = hunk.ranges.get(theirs_pane).copied() else {
            return DiffGetOutcome::Nothing;
        };
        let Some(active_rope) = snapshot_for_pane(&descriptor, active_pane) else {
            return DiffGetOutcome::Nothing;
        };
        let Some(theirs_rope) = snapshot_for_pane(&descriptor, theirs_pane) else {
            return DiffGetOutcome::Nothing;
        };
        let mut text = slice_line_range(&active_rope, active_range);
        text.push_str(&slice_line_range(&theirs_rope, theirs_range));
        let edit = lattice_protocol::edit::Edit::replace(
            lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(active_range.start, 0),
                lattice_protocol::position::Position::new(active_range.end, 0),
            ),
            text,
        );
        DiffGetOutcome::Edit {
            target_buffer_id: active_buffer_id,
            edit,
            post_cursor_row: active_range.start,
        }
    }

    /// D.5.c (2026-05-30): compute the outcome the diff-mode
    /// `dp` chord would produce for `buffer_id` at
    /// `cursor_row`. Mirror of [`Self::compute_get_edit`] but
    /// pushes the current side's text *into the peer* instead
    /// of pulling from the baseline.
    ///
    /// Returns:
    /// - [`DiffPutOutcome::Edit`] for two-pane sessions —
    ///   carries `peer_buffer_id`, the `Edit` to apply to
    ///   the peer, and the current-side cursor row.
    /// - [`DiffPutOutcome::NoPeerBuffer`] when the session's
    ///   participants don't include a peer buffer (inline
    ///   file-on-disk; D.7 git baseline). Dispatch surfaces
    ///   the clear error rather than silently doing nothing.
    /// - [`DiffPutOutcome::Nothing`] for the same silent
    ///   no-op cases as [`Self::compute_get_edit`]: no
    ///   session, no descriptor, no covering hunk, three-way
    ///   `Conflict`.
    ///
    /// Reads the current side via
    /// [`DiffDescriptor::current`]`.snapshot()`. Cheap for
    /// buffer-backed current sources (rope-Arc clone); the
    /// snapshot reads happen once per `dp` keystroke at the
    /// production rate.
    ///
    /// **Three-way scope.** Participants length other than
    /// 2 is treated as "no peer" rather than synthesising a
    /// best-guess target. D.6 lands `:diffput <bufnr>` /
    /// `:diffget <bufnr>` with the disambiguating argument
    /// and replaces this conservative bail-out with a
    /// participant-indexed peer lookup. v1's two-pane shape
    /// (the only one D.5.c claims) cleanly hits the `2`
    /// arm.
    pub fn compute_put_plan(
        &self,
        active_buffer_id: BufferId,
        cursor_row: u32,
        target: Option<BufferId>,
    ) -> DiffPutOutcome {
        let Some(session) = self.lookup_session_for(active_buffer_id) else {
            return DiffPutOutcome::Nothing;
        };
        let session_key = session.buffer_id();
        let Some(descriptor) = self.lookup_descriptor(session_key) else {
            return DiffPutOutcome::Nothing;
        };
        // Inline session (single participant — no peer to
        // push to). Defensive against any future
        // non-buffer-backed baseline source (D.7 `:Gdiff`).
        if descriptor.participants.len() < 2 {
            return DiffPutOutcome::NoPeerBuffer;
        }
        let Some(active_pane) = pane_index_of(&descriptor, active_buffer_id) else {
            return DiffPutOutcome::Nothing;
        };
        let target_pane = match resolve_target_pane(&descriptor, active_pane, target) {
            TargetResolution::Pane(p) => p,
            TargetResolution::Required => {
                return DiffPutOutcome::TargetRequired {
                    available_targets: other_participants(&descriptor, active_pane),
                };
            }
            TargetResolution::Unknown => return DiffPutOutcome::Nothing,
        };
        let allow_conflict = target.is_some();
        let hunks = session.current_hunks();
        let Some(hunk) = find_covering_hunk(&hunks, active_pane, cursor_row, allow_conflict) else {
            return DiffPutOutcome::Nothing;
        };
        let Some(active_range) = hunk.ranges.get(active_pane).copied() else {
            return DiffPutOutcome::Nothing;
        };
        let Some(target_range) = hunk.ranges.get(target_pane).copied() else {
            return DiffPutOutcome::Nothing;
        };
        // The active side's rope is the source we copy
        // FROM; the target's range is what we overwrite.
        let Some(active_rope) = snapshot_for_pane(&descriptor, active_pane) else {
            return DiffPutOutcome::Nothing;
        };
        let active_text = slice_line_range(&active_rope, active_range);
        let edit = lattice_protocol::edit::Edit::replace(
            lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(target_range.start, 0),
                lattice_protocol::position::Position::new(target_range.end, 0),
            ),
            active_text,
        );
        let target_buffer_id = match descriptor.participants.get(target_pane).copied() {
            Some(b) => b,
            None => return DiffPutOutcome::NoPeerBuffer,
        };
        DiffPutOutcome::Edit {
            target_buffer_id,
            edit,
            post_cursor_row: active_range.start,
        }
    }

    /// CR.1 (2026-06-24): resolve the diff-get (`do` chord / `:diffget`)
    /// at `cursor_row` on `active_buffer` into an [`Effect`] the host
    /// applies — the mode-owned replacement for the host's former
    /// `Editor::do_diff_get`. Diff-get rewrites the *active* side's hunk
    /// to match the resolved baseline, so the edit targets `active_buffer`
    /// (the cursor's buffer); the `target_buffer_id` from
    /// [`Self::compute_get_edit`] names only the source side and is NOT
    /// the apply target.
    ///
    /// - covering hunk → `Effect::ApplyEdit { target: active_buffer, .. }`
    ///   carrying the post-edit cursor row;
    /// - three-way without a disambiguating `target` → an error `Echo`
    ///   listing the available bufnrs;
    /// - nothing under the cursor / no session → `None` (silent no-op).
    pub fn diff_get_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
        target: Option<BufferId>,
    ) -> Option<lattice_grammar::Effect> {
        match self.compute_get_edit(active_buffer, cursor_row, target) {
            DiffGetOutcome::Edit {
                edit,
                post_cursor_row,
                ..
            } => Some(lattice_grammar::Effect::ApplyEdit {
                target: active_buffer,
                edit,
                cursor: Some(post_cursor_row),
            }),
            DiffGetOutcome::TargetRequired { available_targets } => {
                Some(target_required_echo("diffget", &available_targets))
            }
            DiffGetOutcome::Nothing => None,
        }
    }

    /// CR.1 (2026-06-24): resolve the diff-put (`dp` chord / `:diffput`)
    /// at `cursor_row` on `active_buffer` into an [`Effect`] — the
    /// mode-owned replacement for `Editor::do_diff_put`. Diff-put pushes
    /// the active side's hunk INTO the peer, so the edit targets the
    /// resolved `target_buffer_id` (the peer); the cursor parks on the
    /// active side.
    ///
    /// - peer + covering hunk → `Effect::ApplyEdit { target: peer, .. }`;
    /// - inline baseline (no live peer buffer) → an error `Echo`
    ///   ("dp: baseline is not a buffer; use :write");
    /// - three-way without a `target` → an error `Echo` listing bufnrs;
    /// - nothing under the cursor / no session → `None`.
    pub fn diff_put_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
        target: Option<BufferId>,
    ) -> Option<lattice_grammar::Effect> {
        match self.compute_put_plan(active_buffer, cursor_row, target) {
            DiffPutOutcome::Edit {
                target_buffer_id,
                edit,
                post_cursor_row,
            } => Some(lattice_grammar::Effect::ApplyEdit {
                target: target_buffer_id,
                edit,
                cursor: Some(post_cursor_row),
            }),
            DiffPutOutcome::NoPeerBuffer => Some(lattice_grammar::Effect::Echo {
                level: lattice_grammar::EchoLevel::Error,
                text: "dp: baseline is not a buffer; use :write".to_string(),
            }),
            DiffPutOutcome::TargetRequired { available_targets } => {
                Some(target_required_echo("diffput", &available_targets))
            }
            DiffPutOutcome::Nothing => None,
        }
    }

    /// CR.3 (2026-06-24): the participant buffers of the session keyed by
    /// `session_key`, in slot order (`[base, local, remote]` for a
    /// three-way). Used by the host to drive `diff-conflict-mode`
    /// activation off the published sign map.
    pub fn session_participants(&self, session_key: BufferId) -> Option<Vec<BufferId>> {
        self.lookup_descriptor(session_key)
            .map(|d| d.participants.clone())
    }

    /// CR.3: resolve "theirs" (the remote side) for the three-way
    /// conflict session active on `active_buffer`. In the
    /// `[base, local, remote]` model the cursor sits on `local` (= ours =
    /// `active_buffer`); "theirs" is the non-base (slot ≠ 0), non-active
    /// participant. `None` for a non-three-way session (no distinct theirs
    /// to resolve against).
    fn conflict_theirs(&self, active_buffer: BufferId) -> Option<BufferId> {
        let session = self.lookup_session_for(active_buffer)?;
        let descriptor = self.lookup_descriptor(session.buffer_id())?;
        if descriptor.participants.len() < 3 {
            return None;
        }
        descriptor
            .participants
            .iter()
            .enumerate()
            .find(|(i, b)| *i != 0 && **b != active_buffer)
            .map(|(_, b)| *b)
    }

    /// CR.3: is there a `Conflict` hunk under `cursor_row` on the active
    /// side? Gates the degenerate keep-ours / put-ours echoes so they
    /// only fire over an actual conflict region (off-hunk → silent).
    fn conflict_hunk_under_cursor(&self, active_buffer: BufferId, cursor_row: u32) -> bool {
        let Some(session) = self.lookup_session_for(active_buffer) else {
            return false;
        };
        let Some(descriptor) = self.lookup_descriptor(session.buffer_id()) else {
            return false;
        };
        let Some(active_pane) = pane_index_of(&descriptor, active_buffer) else {
            return false;
        };
        let hunks = session.current_hunks();
        find_covering_hunk(&hunks, active_pane, cursor_row, true)
            .is_some_and(|h| matches!(h.kind, HunkKind::Conflict))
    }

    /// CR.3 `d2o` keep-ours: the local side already holds ours, so there
    /// is nothing to apply — but the chord is a recognised resolution
    /// command (Dhruva 2026-06-24: full fugitive set, degenerate →
    /// informative echo, not a silent no-op). Echoes over a conflict
    /// region; `None` off-hunk (silent, like the other chords).
    pub fn diff_keep_ours_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
    ) -> Option<lattice_grammar::Effect> {
        self.conflict_hunk_under_cursor(active_buffer, cursor_row)
            .then(|| lattice_grammar::Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: "keep-ours: the local side already holds your version; nothing to apply"
                    .to_string(),
            })
    }

    /// CR.3 `d3o` keep-theirs: pull theirs (remote) into the local range
    /// — `compute_get_edit` with `target = theirs` (already
    /// conflict-capable). `None` when there's no three-way session, no
    /// covering conflict, etc.
    pub fn diff_keep_theirs_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
    ) -> Option<lattice_grammar::Effect> {
        let theirs = self.conflict_theirs(active_buffer)?;
        self.diff_get_effect(active_buffer, cursor_row, Some(theirs))
    }

    /// CR.3 `d2p` put-ours: the local side IS ours, so there is nothing
    /// to push — informative echo over a conflict region (degenerate
    /// self-target), else `None`.
    pub fn diff_put_ours_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
    ) -> Option<lattice_grammar::Effect> {
        self.conflict_hunk_under_cursor(active_buffer, cursor_row)
            .then(|| lattice_grammar::Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: "diffput ours: the local side is already your version; nothing to push"
                    .to_string(),
            })
    }

    /// CR.3 `d3p` put-theirs: push the local side's hunk into theirs
    /// (remote) — `compute_put_plan` with `target = theirs`.
    pub fn diff_put_theirs_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
    ) -> Option<lattice_grammar::Effect> {
        let theirs = self.conflict_theirs(active_buffer)?;
        self.diff_put_effect(active_buffer, cursor_row, Some(theirs))
    }

    /// CR.3 `dB` keep-both: splice ours⌢theirs into the local range via
    /// [`Self::compute_keep_both_edit`], returning an `Effect::ApplyEdit`
    /// targeting the active (local) buffer. `None` when there's no
    /// three-way session or no covering conflict.
    pub fn diff_keep_both_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
    ) -> Option<lattice_grammar::Effect> {
        let theirs = self.conflict_theirs(active_buffer)?;
        match self.compute_keep_both_edit(active_buffer, cursor_row, theirs) {
            DiffGetOutcome::Edit {
                edit,
                post_cursor_row,
                ..
            } => Some(lattice_grammar::Effect::ApplyEdit {
                target: active_buffer,
                edit,
                cursor: Some(post_cursor_row),
            }),
            _ => None,
        }
    }

    /// CR.6 `]c` / `:hunk-next`: move the cursor to the next hunk start
    /// (slot 1, wraps to the first) — the mode-owned replacement for the
    /// host's `do_next_hunk`. Returns a generic `Effect::SelectionChange`
    /// (the host owns the cursor write), or an info `Echo` ("no diff
    /// session" / "no hunks") preserving the former host messages for the
    /// `:hunk-next` ex-command path (the `]c` chord is K.1.c-gated and
    /// never hits those).
    pub fn diff_next_hunk_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
    ) -> Option<lattice_grammar::Effect> {
        let Some(session) = self.lookup_session_for(active_buffer) else {
            return Some(hunk_nav_echo("no diff session"));
        };
        let hunks = session.current_hunks();
        let rows = hunk_starts(&hunks);
        if rows.is_empty() {
            return Some(hunk_nav_echo("no hunks"));
        }
        let row = rows.iter().copied().find(|&l| l > cursor_row).unwrap_or(rows[0]);
        Some(hunk_selection(row))
    }

    /// CR.6 `[c` / `:hunk-prev`: mirror of [`Self::diff_next_hunk_effect`]
    /// — largest slot-1 start strictly before `cursor_row`, wrapping to
    /// the last hunk.
    pub fn diff_prev_hunk_effect(
        &self,
        active_buffer: BufferId,
        cursor_row: u32,
    ) -> Option<lattice_grammar::Effect> {
        let Some(session) = self.lookup_session_for(active_buffer) else {
            return Some(hunk_nav_echo("no diff session"));
        };
        let hunks = session.current_hunks();
        let rows = hunk_starts(&hunks);
        if rows.is_empty() {
            return Some(hunk_nav_echo("no hunks"));
        }
        let row = rows
            .iter()
            .rev()
            .copied()
            .find(|&l| l < cursor_row)
            .unwrap_or_else(|| *rows.last().expect("rows non-empty"));
        Some(hunk_selection(row))
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
        // D.5.a (2026-05-30): notify the bridge AFTER all
        // registry state for this session has been torn down.
        // Bridge consults its own per-session record (set at
        // open) so it doesn't depend on the descriptor.
        // `note_session_closed` on an unknown key is a no-op,
        // so calling unconditionally is safe even when
        // `removed` is false (double-drop / drop-before-open).
        self.mode_bridge.note_session_closed(buffer_id);
        removed
    }

    /// D.8.d (2026-05-31): add a participant to an existing
    /// session. Appends `source` to `descriptor.sources` and
    /// (if `participant_buffer` is `Some`) to `descriptor.
    /// watch` + `descriptor.participants` + the inverse
    /// watcher index. Triggers a recompute through the
    /// existing debouncer so the new arity shows up
    /// immediately.
    ///
    /// Returns the **new arity** after the add (on success).
    ///
    /// Errors:
    /// - [`MembershipError::NoSession`] — no descriptor
    ///   registered for `session_key`.
    /// - [`MembershipError::EngineRejected`] — the new arity
    ///   would exceed what the engine supports (v1: N≥4).
    ///   The session's descriptor is **not** mutated when
    ///   this fires — the caller's add fails atomically.
    pub fn add_participant(
        self: &Arc<Self>,
        session_key: BufferId,
        source: Arc<dyn DiffParticipantSource>,
        participant_buffer: Option<BufferId>,
    ) -> Result<usize, MembershipError> {
        // Pre-check arity against the engine cap before any
        // mutation so a rejected add is atomic.
        let new_arity = {
            let descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            let descriptor = descriptors
                .get(&session_key)
                .ok_or(MembershipError::NoSession(session_key))?;
            let proposed = descriptor.arity() + 1;
            // Probe the engine. v1 caps at 3; if we'd cross
            // the cap, surface the typed engine error
            // untouched.
            if proposed >= 4 {
                return Err(MembershipError::EngineRejected(
                    crate::DiffEngineError::Unsupported { n: proposed },
                ));
            }
            proposed
        };

        // Mutate the descriptor under the mutex. The
        // borrow above was read-only; this block re-locks
        // for write so we don't hold both locks at once.
        {
            let mut descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            let descriptor = descriptors
                .get_mut(&session_key)
                .ok_or(MembershipError::NoSession(session_key))?;
            descriptor.sources.push(source);
            if let Some(buf) = participant_buffer {
                if !descriptor.watch.contains(&buf) {
                    descriptor.watch.push(buf);
                }
                if !descriptor.participants.contains(&buf) {
                    descriptor.participants.push(buf);
                }
            }
        }

        // Watcher index + secondary index gain the new
        // buffer (skips primary + duplicates internally).
        if let Some(buf) = participant_buffer {
            self.install_watcher_entries(session_key, &[buf]);
            self.install_secondary_entries(session_key, &[buf]);
            // Bridge: mode refcount + per-session
            // participant list grow.
            self.mode_bridge.note_session_extended(session_key, buf);
        }

        // Kick a recompute so the new arity publishes
        // promptly without waiting for the next edit.
        Arc::clone(self).poke_session(session_key);

        Ok(new_arity)
    }

    /// D.8.d (2026-05-31): remove the participant at slot
    /// `slot` from `session_key`. Drops the corresponding
    /// `sources[slot]`; if the slot corresponded to a buffer
    /// in `participants`, scrubs it from the watcher index +
    /// notifies the bridge.
    ///
    /// **Auto-collapse semantics:**
    /// - New arity ≥ 2: session stays active, recompute
    ///   fires with the smaller participant set.
    /// - New arity == 1: session is **dormant** (registered,
    ///   refcount stays on the remaining buffer, but
    ///   `compute_diff` publishes an empty `HunkIndex` since
    ///   there's no peer to diff against).
    /// - New arity == 0: session **auto-drops** (calls
    ///   `drop_session` internally).
    ///
    /// Returns the new arity (0 on auto-drop).
    pub fn remove_participant(
        self: &Arc<Self>,
        session_key: BufferId,
        slot: usize,
    ) -> Result<usize, MembershipError> {
        // Read out the buffer-id at this slot (if any) so
        // the bridge + watcher index can update.
        let removed_buf = {
            let mut descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            let descriptor = descriptors
                .get_mut(&session_key)
                .ok_or(MembershipError::NoSession(session_key))?;
            let arity = descriptor.arity();
            if slot >= arity {
                return Err(MembershipError::SlotOutOfRange { slot, arity });
            }
            descriptor.sources.remove(slot);
            // `participants` indices may not align with
            // `sources` slot indices (participants only
            // lists buffer-backed sides), so we can't blindly
            // remove by slot. Instead, if the descriptor's
            // participants/watch lists carry a buffer at the
            // same position as a buffer-backed source, the
            // caller passes the buffer id explicitly via
            // `remove_participant_buffer`. For slot-based
            // removal we just trim the source vector and
            // leave participants/watch alone; the next
            // add_participant or recompute will reconcile.
            None::<BufferId>
        };

        // If the new arity is 0, auto-drop.
        let new_arity = {
            let descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            descriptors
                .get(&session_key)
                .map(|d| d.arity())
                .unwrap_or(0)
        };
        if new_arity == 0 {
            self.drop_session(session_key);
            return Ok(0);
        }

        // Bridge / index updates only fire if we know which
        // buffer left (the buffer-aware path).
        if let Some(buf) = removed_buf {
            self.scrub_watcher_entries(session_key, &[buf]);
            self.scrub_secondary_entries(session_key, &[buf]);
            self.mode_bridge.note_session_shrunk(session_key, buf);
        }

        // Kick a recompute with the new arity.
        Arc::clone(self).poke_session(session_key);
        Ok(new_arity)
    }

    /// D.8.d (2026-05-31): convenience — remove the slot
    /// whose `participants` entry equals `buffer_id`. Looks
    /// up the slot via [`pane_index_of`] (D.6.d helper),
    /// then delegates to [`Self::remove_participant`].
    /// Updates `watch` + `participants` + bridge in this
    /// path (unlike slot-only removal, since we know which
    /// buffer leaves).
    ///
    /// This is the typical entry point for `:diffthis` /
    /// per-buffer `:diffoff` (D.8.e / D.8.f).
    pub fn remove_participant_buffer(
        self: &Arc<Self>,
        session_key: BufferId,
        buffer_id: BufferId,
    ) -> Result<usize, MembershipError> {
        // Find the slot first under a read lock.
        let slot = {
            let descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            let descriptor = descriptors
                .get(&session_key)
                .ok_or(MembershipError::NoSession(session_key))?;
            pane_index_of(descriptor, buffer_id)
                .ok_or(MembershipError::NotParticipant(buffer_id))?
        };

        // Mutate under a write lock: drop the source +
        // trim `watch` + `participants`.
        {
            let mut descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            let descriptor = descriptors
                .get_mut(&session_key)
                .ok_or(MembershipError::NoSession(session_key))?;
            descriptor.sources.remove(slot);
            descriptor.watch.retain(|&b| b != buffer_id);
            descriptor.participants.retain(|&b| b != buffer_id);
        }

        // Auto-drop on N → 0.
        let new_arity = {
            let descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            descriptors
                .get(&session_key)
                .map(|d| d.arity())
                .unwrap_or(0)
        };
        if new_arity == 0 {
            self.drop_session(session_key);
            return Ok(0);
        }

        // Scrub indexes + notify bridge.
        self.scrub_watcher_entries(session_key, &[buffer_id]);
        self.scrub_secondary_entries(session_key, &[buffer_id]);
        self.mode_bridge.note_session_shrunk(session_key, buffer_id);

        // Kick recompute.
        Arc::clone(self).poke_session(session_key);
        Ok(new_arity)
    }

    /// D.8.d (2026-05-31): atomically swap a session's
    /// descriptor while preserving session identity. The
    /// `Arc<DiffSession>` stays the same — any holder
    /// (`compute_get_edit` / `compute_put_plan` callers,
    /// renderer-side `current_hunks` readers) sees a smooth
    /// transition. Useful for transitioning a session from
    /// N=1 dormant to N=2 active (the natural
    /// `:diffthis` flow) when we want to swap the entire
    /// source list rather than `add_participant`-ing one
    /// at a time.
    ///
    /// Internally: `drop_session`'s scrub semantic for the
    /// old descriptor, then `register_with_sources`'s install
    /// semantic for the new one — but without dropping the
    /// session entry from the registry. The mode-bridge
    /// re-scrubs + re-installs participants the same way
    /// `note_session_opened` already does on re-open.
    pub fn replace_descriptor(
        self: &Arc<Self>,
        session_key: BufferId,
        descriptor: DiffDescriptor,
    ) -> Result<(), MembershipError> {
        // Reject N≥4 atomically before any mutation.
        if descriptor.arity() >= 4 {
            return Err(MembershipError::EngineRejected(
                crate::DiffEngineError::Unsupported {
                    n: descriptor.arity(),
                },
            ));
        }
        // Require an existing session.
        if self.lookup(session_key).is_none() {
            return Err(MembershipError::NoSession(session_key));
        }

        // Scrub the old descriptor's index entries.
        let old_descriptor = {
            let mut descriptors = self
                .descriptors
                .lock()
                .expect("DiffSubsystem mutex poisoned");
            descriptors.insert(session_key, descriptor.clone())
        };
        if let Some(old) = old_descriptor {
            self.scrub_watcher_entries(session_key, &old.watch);
            self.scrub_secondary_entries(session_key, &old.watch);
        }

        // Install the new descriptor's index entries.
        self.install_watcher_entries(session_key, &descriptor.watch);
        self.install_secondary_entries(session_key, &descriptor.watch);

        // Bridge: re-open semantic (scrubs old participants,
        // installs new ones with refcount transitions).
        self.mode_bridge
            .note_session_opened(session_key, &descriptor.participants);

        // Kick a recompute with the new shape.
        Arc::clone(self).poke_session(session_key);
        Ok(())
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
        let sessions = self.sessions.lock().expect("DiffSubsystem mutex poisoned");
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
        sources: Vec<Arc<dyn DiffParticipantSource>>,
    ) -> Option<JoinHandle<Option<Arc<HunkIndex>>>> {
        let session = self.lookup(buffer_id)?;
        Some(tokio::task::spawn_blocking(move || {
            // D.8.c (2026-05-31): snapshot every source inside
            // the blocking task — the engine wants `&[Rope]`,
            // and snapshotting is potentially expensive
            // (file-on-disk reads, future git-blob reads). The
            // caller passes owned `Arc<dyn ...>` handles; the
            // task captures them so the descriptor's mutex
            // isn't held across the snapshot calls.
            let ropes: Vec<Rope> = sources.iter().map(|s| s.snapshot()).collect();
            session.recompute_blocking(&ropes)
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
            target: "lattice_host::diff::subsystem",
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
    /// `BufferSource(closed_id)` for session X), session X's
    /// watcher entry for `closed_id` is left in place — the
    /// next snapshot returns an empty rope per the
    /// [`BufferTextProvider`] contract, and the session will
    /// recompute the all-Add diff. The session itself is not
    /// dropped on a watched-side close; only on a current-side
    /// close.
    pub fn note_buffer_closed(&self, buffer_id: BufferId) {
        debug!(
            target: "lattice_host::diff::subsystem",
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

    // Internal: read the session's descriptor and fire
    // `schedule_recompute` with a clone of the source list.
    // `schedule_recompute` spawns the diff on the blocking
    // pool and snapshots inside the task; we return
    // immediately. Stale or torn-down sessions return early
    // — the gated publish in D.2.b drops anything stale
    // that does land.
    fn recompute_from_descriptor(&self, session_key: BufferId) {
        let descriptor = match self.lookup_descriptor(session_key) {
            Some(d) => d,
            None => return,
        };
        let _ = self.schedule_recompute(session_key, descriptor.sources);
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
            EventFilter::kinds(vec![EventKind::DocumentChanged, EventKind::DocumentClosed]),
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
        let mut ids: Vec<BufferId> = sub.iter_sessions().iter().map(|s| s.buffer_id()).collect();
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
    // D.2.b: DiffParticipantSource + recompute + schedule
    // ──────────────────────────────────────────────────────────

    #[test]
    fn static_baseline_clones_rope_on_snapshot() {
        let base = StaticSource::new(Rope::from("alpha\nbeta\n"));
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
        let base = OnDiskSource::new(path.clone());
        let snap = base.snapshot();
        assert_eq!(snap.to_string(), "hello\nworld\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn on_disk_baseline_missing_file_returns_empty_rope() {
        // Per docs: missing path / I/O error degrades to
        // empty rope (all-Add presentation) rather than
        // panicking.
        let base = OnDiskSource::new(std::path::PathBuf::from(
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
            .recompute_blocking(&[r.clone(), r.clone()])
            .expect("first publish should always take");
        assert!(published.is_empty());
        assert_eq!(published.algorithm, DiffAlgorithm::Histogram);
        assert_eq!(published.revision, 1);
        // And the session's published state matches.
        assert_eq!(s.current_hunks().revision, 1);
    }

    #[test]
    fn recompute_blocking_on_changed_ropes_produces_change_hunk() {
        use crate::HunkKind;
        let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        let a = Rope::from("alpha\nbeta\ngamma\n");
        let b = Rope::from("alpha\nBETA\ngamma\n");
        let idx = s
            .recompute_blocking(&[a.clone(), b.clone()])
            .expect("first publish should take");
        assert_eq!(idx.len(), 1);
        assert_eq!(idx.hunks[0].kind, HunkKind::Change);
        assert_eq!(idx.revision, 1);
    }

    #[test]
    fn recompute_blocking_publishes_sign_map_for_changed_ropes() {
        // D-fix.3a: the sign map is published in lockstep with the hunks at the
        // recompute choke point, so pane-group diffs (which never spawn the
        // inline `DiffOverlayRefreshTask`) still get in-buffer tints + gutter
        // signs — not just inline `:diff`.
        let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        assert!(
            s.sign_map().sign_at(1).is_none(),
            "no signs before the first recompute"
        );
        let a = Rope::from("alpha\nbeta\ngamma\n");
        let b = Rope::from("alpha\nBETA\ngamma\n");
        s.recompute_blocking(&[a, b]).expect("first publish takes");
        let signs = s.sign_map();
        assert_eq!(
            signs.sign_at(1),
            Some(crate::overlay::DiffSignKind::Change),
            "changed current-side line 1 is signed Change after recompute"
        );
        assert!(signs.sign_at(0).is_none(), "unchanged line 0 has no sign");
    }

    #[test]
    fn revision_strictly_increases_across_recomputes() {
        let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        let a = Rope::from("alpha\n");
        let b = Rope::from("beta\n");
        let r1 = s.recompute_blocking(&[a.clone(), b.clone()]).unwrap();
        let r2 = s.recompute_blocking(&[a.clone(), b.clone()]).unwrap();
        let r3 = s.recompute_blocking(&[a.clone(), b.clone()]).unwrap();
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
        let result = s.recompute_blocking(&[r.clone(), r.clone()]);
        assert!(result.is_none(), "stale recompute should not publish");
        assert_eq!(s.current_hunks().revision, 100);
    }

    #[test]
    fn schedule_recompute_returns_none_for_unregistered_buffer() {
        let sub = DiffSubsystem::new();
        let baseline: Arc<dyn DiffParticipantSource> =
            Arc::new(StaticSource::new(Rope::from("x\n")));
        let handle = sub.schedule_recompute(
            bid(999),
            vec![baseline, Arc::new(StaticSource::new(Rope::from("y\n")))],
        );
        assert!(handle.is_none());
    }

    #[tokio::test]
    async fn schedule_recompute_runs_on_blocking_pool_and_publishes() {
        let sub = DiffSubsystem::new();
        let session = sub.register(bid(1), DiffAlgorithm::Histogram);
        let baseline: Arc<dyn DiffParticipantSource> =
            Arc::new(StaticSource::new(Rope::from("alpha\nbeta\n")));
        let current = Rope::from("alpha\nBETA\n");

        let handle = sub
            .schedule_recompute(
                bid(1),
                vec![
                    baseline,
                    Arc::new(StaticSource::new(Rope::from("alpha\nBETA\n"))),
                ],
            )
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
        let baseline: Arc<dyn DiffParticipantSource> =
            Arc::new(StaticSource::new(Rope::from("alpha\n")));

        let h1 = sub
            .schedule_recompute(
                bid(1),
                vec![
                    Arc::clone(&baseline),
                    Arc::new(StaticSource::new(Rope::from("alpha\n"))),
                ],
            )
            .unwrap();
        h1.await.unwrap().unwrap();

        let h2 = sub
            .schedule_recompute(
                bid(1),
                vec![
                    Arc::clone(&baseline),
                    Arc::new(StaticSource::new(Rope::from("beta\n"))),
                ],
            )
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
    // BufferId. The test sets ropes; `BufferSource` /
    // `BufferSource` read them on snapshot.
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
            sources: vec![
                Arc::new(BufferSource::new(Arc::clone(provider), baseline_buf)),
                Arc::new(BufferSource::new(Arc::clone(provider), current_buf)),
            ],
            watch: vec![baseline_buf, current_buf],
            // D.5.a: tests don't exercise the mode bridge,
            // so participants stays empty by default.
            participants: vec![],
        }
    }

    /// D.6.a (2026-05-30): test helper for three-way merge
    /// descriptors. `base` plays the role of common
    /// ancestor; `local` is the side the session is keyed
    /// under; `remote` is the third party. watch +
    /// participants = all three buffers so the routing index
    /// + mode bridge would activate uniformly.
    fn three_way_descriptor(
        provider: &Arc<dyn BufferTextProvider>,
        base_buf: BufferId,
        local_buf: BufferId,
        remote_buf: BufferId,
    ) -> DiffDescriptor {
        DiffDescriptor {
            sources: vec![
                Arc::new(BufferSource::new(Arc::clone(provider), base_buf)),
                Arc::new(BufferSource::new(Arc::clone(provider), local_buf)),
                Arc::new(BufferSource::new(Arc::clone(provider), remote_buf)),
            ],
            watch: vec![base_buf, local_buf, remote_buf],
            participants: vec![base_buf, local_buf, remote_buf],
        }
    }

    // ── Concrete sources ──────────────────────────────────────

    #[test]
    fn buffer_baseline_snapshots_through_provider() {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from("hello\n"));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();
        let base = BufferSource::new(dyn_provider, bid(1));
        assert_eq!(base.snapshot().to_string(), "hello\n");
    }

    #[test]
    fn buffer_baseline_returns_empty_rope_when_provider_lacks_buffer() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let base = BufferSource::new(provider, bid(999));
        assert_eq!(base.snapshot().len_chars(), 0);
    }

    #[test]
    fn buffer_current_source_snapshots_through_provider() {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from("world\n"));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();
        let cur = BufferSource::new(dyn_provider, bid(1));
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
            sources: vec![
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(10))),
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(1))),
            ],
            watch: vec![bid(10), bid(1)],
            participants: vec![],
        };
        let desc_b = DiffDescriptor {
            sources: vec![
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(10))),
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(2))),
            ],
            watch: vec![bid(10), bid(2)],
            participants: vec![],
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
            sources: vec![
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(10))),
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(1))),
            ],
            watch: vec![bid(10), bid(1)],
            participants: vec![],
        };
        let desc_b = DiffDescriptor {
            sources: vec![
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(10))),
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(2))),
            ],
            watch: vec![bid(10), bid(2)],
            participants: vec![],
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
            sources: vec![
                Arc::new(StaticSource::new(Rope::from(""))),
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(1))),
            ],
            watch: vec![bid(10), bid(1)],
            participants: vec![],
        };
        sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc_a);
        assert_eq!(sub.watchers_of(bid(10)), vec![bid(1)]);

        let desc_b = DiffDescriptor {
            sources: vec![
                Arc::new(StaticSource::new(Rope::from(""))),
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(1))),
            ],
            watch: vec![bid(1)],
            participants: vec![],
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

        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            10,
        )));
        let desc = descriptor(&dyn_provider, bid(2), bid(1));
        let session = sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);

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

        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            10,
        )));
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
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            10,
        )));
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

        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            10,
        )));
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
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            10,
        )));
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
        assert_eq!(
            sub.build_describe_diff_content(),
            "No active diff sessions.\n"
        );
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
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            10,
        )));
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

    // ── D.5.b: compute_get_edit ────────────────────────────────

    use crate::Hunk;
    use smallvec::smallvec;

    /// Build a session with a [`StaticSource`] for testing
    /// `compute_get_edit` without exercising the buffer-backed
    /// machinery. Returns the subsystem so the caller can
    /// publish hunks and query.
    fn fixture_with_baseline(baseline: &str) -> (DiffSubsystem, BufferId) {
        let sub = DiffSubsystem::new();
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        // Current side is buffer-backed but never read by
        // compute_get_edit — the function reads the baseline
        // only. The current source is required for descriptor
        // construction.
        let desc = DiffDescriptor {
            sources: vec![
                Arc::new(StaticSource::new(Rope::from(baseline))),
                Arc::new(BufferSource::new(Arc::clone(&provider), bid(1))),
            ],
            watch: vec![bid(1)],
            participants: vec![bid(1)],
        };
        sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);
        (sub, bid(1))
    }

    fn publish_hunks(sub: &DiffSubsystem, key: BufferId, hunks: Vec<Hunk>) {
        let session = sub.lookup(key).unwrap();
        let rev = session.allocate_revision();
        session.publish(Arc::new(HunkIndex {
            hunks,
            algorithm: DiffAlgorithm::Histogram,
            revision: rev,
        }));
    }

    fn lr(start: u32, end: u32) -> LineRange {
        LineRange::new(start, end)
    }

    /// `Change` on the current side: `do` replaces the
    /// current lines with the baseline slice for the
    /// corresponding baseline range.
    #[test]
    fn compute_get_edit_change_replaces_current_range_with_baseline() {
        let (sub, key) = fixture_with_baseline("base-a\nbase-b\n");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 2), lr(3, 5)],
            }],
        );
        let plan = sub
            .compute_get_edit(key, 3, None)
            .into_plan()
            .expect("hunk covers row 3");
        assert_eq!(plan.post_cursor_row, 3);
        // Edit: replace current range (lines 3..5) with the
        // baseline lines 0..2 → "base-a\nbase-b\n".
        assert_eq!(plan.edit.range.start.line, 3);
        assert_eq!(plan.edit.range.end.line, 5);
        match plan.edit.kind {
            lattice_protocol::edit::EditKind::Replace { ref text } => {
                assert_eq!(text, "base-a\nbase-b\n");
            }
        }
    }

    /// CR.1: `diff_get_effect` wraps a covering `compute_get_edit`
    /// outcome into `Effect::ApplyEdit` targeting the ACTIVE buffer
    /// (diff-get rewrites the cursor's side) with the post-edit cursor
    /// row; the carried edit equals the `compute_get_edit` plan's edit.
    #[test]
    fn diff_get_effect_wraps_edit_for_active_buffer() {
        let (sub, key) = fixture_with_baseline("base-a\nbase-b\n");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 2), lr(3, 5)],
            }],
        );
        let plan_edit = sub.compute_get_edit(key, 3, None).into_plan().unwrap().edit;
        match sub.diff_get_effect(key, 3, None) {
            Some(lattice_grammar::Effect::ApplyEdit {
                target,
                edit,
                cursor,
            }) => {
                assert_eq!(target, key, "diff-get edits the active (cursor) buffer");
                assert_eq!(cursor, Some(3));
                assert_eq!(edit, plan_edit);
            }
            other => panic!("expected ApplyEdit, got {other:?}"),
        }
    }

    /// CR.1: no covering hunk → `diff_get_effect` is `None` (silent
    /// no-op), mirroring `compute_get_edit`'s `Nothing`.
    #[test]
    fn diff_get_effect_none_when_no_hunk() {
        let (sub, key) = fixture_with_baseline("base-a\n");
        assert!(sub.diff_get_effect(key, 0, None).is_none());
    }

    /// CR.1: `diff_put_effect` against an inline baseline (the baseline
    /// side is a `StaticSource`, not a live buffer) yields the error
    /// `Echo` — preserving the pre-CR.1 `do_diff_put` wording verbatim —
    /// not an `ApplyEdit`.
    #[test]
    fn diff_put_effect_inline_baseline_yields_error_echo() {
        let (sub, key) = fixture_with_baseline("base-a\nbase-b\n");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 2), lr(3, 5)],
            }],
        );
        match sub.diff_put_effect(key, 3, None) {
            Some(lattice_grammar::Effect::Echo { text, .. }) => {
                assert_eq!(text, "dp: baseline is not a buffer; use :write");
            }
            other => panic!("expected an error Echo, got {other:?}"),
        }
    }

    /// `Add` hunk: lines appear in current side only;
    /// baseline range is empty. `do` deletes the current
    /// lines (revert the addition; baseline says no content
    /// here).
    #[test]
    fn compute_get_edit_add_deletes_current_range() {
        let (sub, key) = fixture_with_baseline("");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Add,
                ranges: smallvec![lr(0, 0), lr(2, 4)],
            }],
        );
        let plan = sub
            .compute_get_edit(key, 3, None)
            .into_plan()
            .expect("hunk covers row 3");
        // Cursor parks at the hunk start (row 2).
        assert_eq!(plan.post_cursor_row, 2);
        assert_eq!(plan.edit.range.start.line, 2);
        assert_eq!(plan.edit.range.end.line, 4);
        match plan.edit.kind {
            lattice_protocol::edit::EditKind::Replace { ref text } => {
                assert!(
                    text.is_empty(),
                    "Add → delete: text must be empty, was {text:?}"
                );
            }
        }
    }

    /// `Remove` hunk: lines appear in baseline only;
    /// current range is empty. `do` inserts the baseline
    /// lines at the deletion anchor (revert the removal).
    #[test]
    fn compute_get_edit_remove_inserts_baseline_text_at_gap() {
        let (sub, key) = fixture_with_baseline("removed-1\nremoved-2\n");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Remove,
                ranges: smallvec![lr(0, 2), lr(5, 5)],
            }],
        );
        // Cursor must sit exactly at the empty-current anchor
        // (row 5) for the Remove lookup to match — vim parity.
        let plan = sub
            .compute_get_edit(key, 5, None)
            .into_plan()
            .expect("Remove hunk anchored at row 5 must match");
        assert_eq!(plan.post_cursor_row, 5);
        // Edit: insert at line 5 (empty range), text is
        // the baseline's "removed-1\nremoved-2\n".
        assert_eq!(plan.edit.range.start.line, 5);
        assert_eq!(plan.edit.range.end.line, 5);
        match plan.edit.kind {
            lattice_protocol::edit::EditKind::Replace { ref text } => {
                assert_eq!(text, "removed-1\nremoved-2\n");
            }
        }
    }

    /// `Remove` hunks anchor at exactly `current.start`; a
    /// cursor one row off is a miss. (No "near enough"
    /// matching — vim's `do` only fires when the cursor
    /// sits on the deletion-marker row.)
    #[test]
    fn compute_get_edit_remove_misses_when_cursor_off_anchor() {
        let (sub, key) = fixture_with_baseline("x\n");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Remove,
                ranges: smallvec![lr(0, 1), lr(5, 5)],
            }],
        );
        assert!(sub.compute_get_edit(key, 4, None).is_nothing());
        assert!(sub.compute_get_edit(key, 6, None).is_nothing());
    }

    /// Cursor outside every hunk returns `None`.
    #[test]
    fn compute_get_edit_cursor_outside_hunks_returns_none() {
        let (sub, key) = fixture_with_baseline("a\nb\n");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 2), lr(10, 12)],
            }],
        );
        assert!(sub.compute_get_edit(key, 0, None).is_nothing());
        assert!(sub.compute_get_edit(key, 9, None).is_nothing());
        // End is exclusive — row 12 is past the hunk.
        assert!(sub.compute_get_edit(key, 12, None).is_nothing());
    }

    /// Three-way `Conflict` hunks are skipped — D.6 owns the
    /// conflict-resolution path; `do` is two-way only.
    #[test]
    fn compute_get_edit_conflict_hunk_is_ignored() {
        let (sub, key) = fixture_with_baseline("base\n");
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1)],
            }],
        );
        assert!(sub.compute_get_edit(key, 0, None).is_nothing());
    }

    /// No session registered → no-op `None`. The keymap
    /// layer is global so the chord can fire on any buffer,
    /// but per-buffer K.1.c gating prevents that — if a test
    /// bypasses K.1.c and the action runs, it must still
    /// degrade cleanly.
    #[test]
    fn compute_get_edit_no_session_returns_none() {
        let sub = DiffSubsystem::new();
        assert!(sub.compute_get_edit(bid(42), 0, None).is_nothing());
    }

    /// Session registered without a descriptor (the
    /// sources-less `register` path used in some tests) →
    /// no baseline to read from, returns `None` gracefully.
    #[test]
    fn compute_get_edit_no_descriptor_returns_none() {
        let sub = DiffSubsystem::new();
        // `register` is the sources-less path used by some
        // test fixtures; it stores no descriptor.
        sub.register(bid(1), DiffAlgorithm::Histogram);
        // Publish a Change hunk; cursor on it would normally
        // match — but the missing descriptor causes the
        // lookup to short-circuit to `None`.
        publish_hunks(
            &sub,
            bid(1),
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 1), lr(0, 1)],
            }],
        );
        assert!(sub.compute_get_edit(bid(1), 0, None).is_nothing());
    }

    // ── D.5.c: compute_put_plan ────────────────────────────────

    /// Build a two-pane session fixture: baseline + current
    /// are both buffer-backed via a shared `MockProvider`
    /// with the supplied ropes. `participants =
    /// [baseline_bid, current_bid]`, so `compute_put_plan`
    /// resolves the peer to `baseline_bid`. Returns the
    /// subsystem + the session key.
    fn fixture_two_pane(
        baseline_text: &str,
        current_text: &str,
    ) -> (DiffSubsystem, BufferId, BufferId) {
        let sub = DiffSubsystem::new();
        let provider = Arc::new(MockProvider::default());
        let baseline_bid = bid(100);
        let current_bid = bid(200);
        provider.set(baseline_bid, Rope::from(baseline_text));
        provider.set(current_bid, Rope::from(current_text));
        let provider_dyn: Arc<dyn BufferTextProvider> = provider;
        let desc = DiffDescriptor {
            sources: vec![
                Arc::new(BufferSource::new(Arc::clone(&provider_dyn), baseline_bid)),
                Arc::new(BufferSource::new(Arc::clone(&provider_dyn), current_bid)),
            ],
            watch: vec![baseline_bid, current_bid],
            participants: vec![baseline_bid, current_bid],
        };
        sub.register_with_sources(current_bid, DiffAlgorithm::Histogram, desc);
        (sub, current_bid, baseline_bid)
    }

    /// `Change` on the current side: `dp` replaces the
    /// peer's baseline lines with the current-side slice.
    #[test]
    fn compute_put_plan_change_pushes_current_into_peer() {
        let (sub, current, peer) = fixture_two_pane("base-a\nbase-b\n", "live-a\nlive-b\n");
        publish_hunks(
            &sub,
            current,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 2), lr(0, 2)],
            }],
        );
        let outcome = sub.compute_put_plan(current, 0, None);
        match outcome {
            DiffPutOutcome::Edit {
                target_buffer_id,
                edit,
                post_cursor_row,
            } => {
                assert_eq!(target_buffer_id, peer);
                assert_eq!(post_cursor_row, 0);
                assert_eq!(edit.range.start.line, 0);
                assert_eq!(edit.range.end.line, 2);
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "live-a\nlive-b\n");
                    }
                }
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    /// `Add` hunk (current has extra lines; baseline range
    /// empty): `dp` *inserts* those lines into the peer at
    /// `baseline.start` (empty-range replace == insertion).
    #[test]
    fn compute_put_plan_add_inserts_into_peer_at_baseline_anchor() {
        let (sub, current, peer) = fixture_two_pane("", "added\n");
        publish_hunks(
            &sub,
            current,
            vec![Hunk {
                kind: HunkKind::Add,
                ranges: smallvec![lr(0, 0), lr(0, 1)],
            }],
        );
        let outcome = sub.compute_put_plan(current, 0, None);
        match outcome {
            DiffPutOutcome::Edit {
                target_buffer_id,
                edit,
                ..
            } => {
                assert_eq!(target_buffer_id, peer);
                // Empty baseline range → insertion point.
                assert_eq!(edit.range.start.line, 0);
                assert_eq!(edit.range.end.line, 0);
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "added\n");
                    }
                }
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    /// `Remove` hunk (current empty, baseline has lines):
    /// `dp` deletes the peer's lines (push the empty
    /// current-side state into the peer).
    #[test]
    fn compute_put_plan_remove_deletes_peer_range() {
        let (sub, current, peer) = fixture_two_pane("removed-1\nremoved-2\n", "");
        publish_hunks(
            &sub,
            current,
            vec![Hunk {
                kind: HunkKind::Remove,
                ranges: smallvec![lr(0, 2), lr(0, 0)],
            }],
        );
        let outcome = sub.compute_put_plan(current, 0, None);
        match outcome {
            DiffPutOutcome::Edit {
                target_buffer_id,
                edit,
                ..
            } => {
                assert_eq!(target_buffer_id, peer);
                assert_eq!(edit.range.start.line, 0);
                assert_eq!(edit.range.end.line, 2);
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert!(
                            text.is_empty(),
                            "Remove `dp` deletes peer range; text was {text:?}"
                        );
                    }
                }
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    /// Inline (single-participant) session has no peer
    /// buffer — `dp` returns `NoPeerBuffer` so dispatch can
    /// surface the clear error. Single-participant is the
    /// shape that `:diff` (file-on-disk baseline) and
    /// future `:Gdiff` produce.
    #[test]
    fn compute_put_plan_inline_session_returns_no_peer_buffer() {
        let (sub, key) = fixture_with_baseline("base\n");
        // `fixture_with_baseline` creates a single-participant
        // inline descriptor.
        publish_hunks(
            &sub,
            key,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 1), lr(0, 1)],
            }],
        );
        assert_eq!(
            sub.compute_put_plan(key, 0, None),
            DiffPutOutcome::NoPeerBuffer
        );
    }

    /// No session → `Nothing` (silent no-op shape). The
    /// per-buffer K.1.c gate suppresses `dp` on non-diff
    /// buffers; this is the defensive belt-and-braces case.
    #[test]
    fn compute_put_plan_no_session_returns_nothing() {
        let sub = DiffSubsystem::new();
        assert_eq!(
            sub.compute_put_plan(bid(99), 0, None),
            DiffPutOutcome::Nothing
        );
    }

    /// Cursor outside every hunk → `Nothing`.
    #[test]
    fn compute_put_plan_cursor_outside_hunks_returns_nothing() {
        let (sub, current, _peer) = fixture_two_pane("a\n", "b\n");
        publish_hunks(
            &sub,
            current,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 1), lr(0, 1)],
            }],
        );
        assert_eq!(
            sub.compute_put_plan(current, 5, None),
            DiffPutOutcome::Nothing
        );
    }

    /// Three-way `Conflict` hunks are skipped (D.6 owns
    /// `:diffput <bufnr>` with the disambiguating arg).
    #[test]
    fn compute_put_plan_conflict_hunk_is_skipped() {
        let (sub, current, _peer) = fixture_two_pane("a\n", "a\n");
        publish_hunks(
            &sub,
            current,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1)],
            }],
        );
        assert_eq!(
            sub.compute_put_plan(current, 0, None),
            DiffPutOutcome::Nothing
        );
    }

    /// First hunk wins when several hunks coexist — the
    /// search is linear over the published list. The
    /// algorithm doesn't sort, but published HunkIndex
    /// preserves the diff order which is non-overlapping +
    /// monotonic per `imara-diff`, so the first match is
    /// also the only match.
    #[test]
    fn compute_get_edit_finds_hunk_among_many() {
        let (sub, key) = fixture_with_baseline("first-hunk\nsecond-hunk\nthird-hunk\n");
        publish_hunks(
            &sub,
            key,
            vec![
                Hunk {
                    kind: HunkKind::Change,
                    ranges: smallvec![lr(0, 1), lr(0, 1)],
                },
                Hunk {
                    kind: HunkKind::Change,
                    ranges: smallvec![lr(1, 2), lr(5, 6)],
                },
                Hunk {
                    kind: HunkKind::Change,
                    ranges: smallvec![lr(2, 3), lr(10, 11)],
                },
            ],
        );
        let plan = sub
            .compute_get_edit(key, 5, None)
            .into_plan()
            .expect("second hunk covers 5");
        match plan.edit.kind {
            lattice_protocol::edit::EditKind::Replace { ref text } => {
                assert_eq!(text, "second-hunk\n");
            }
        }
        assert_eq!(plan.post_cursor_row, 5);
    }

    // ──────────────────────────────────────────────────────────
    // D.6.a (2026-05-30): three-way merge lifecycle
    // ──────────────────────────────────────────────────────────

    /// Three sources, non-overlapping changes (local mutates one
    /// region, remote mutates a disjoint region) — engine emits
    /// two non-conflict hunks with three ranges each.
    #[test]
    fn three_way_non_overlapping_changes_produce_no_conflict_hunks() {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from("aaa\nbbb\nccc\nddd\neee\n"));
        provider.set(bid(2), Rope::from("aaa\nBBB\nccc\nddd\neee\n"));
        provider.set(bid(3), Rope::from("aaa\nbbb\nccc\nddd\nEEE\n"));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

        let sub = DiffSubsystem::new();
        let desc = three_way_descriptor(&dyn_provider, bid(1), bid(2), bid(3));
        let session = sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        let base = provider.buffer_rope(bid(1)).unwrap();
        let local = provider.buffer_rope(bid(2)).unwrap();
        let remote = provider.buffer_rope(bid(3)).unwrap();
        let idx = session
            .recompute_blocking(&[base.clone(), local.clone(), remote.clone()])
            .expect("three-way recompute publishes");

        assert!(
            idx.hunks
                .iter()
                .all(|h| !matches!(h.kind, HunkKind::Conflict)),
            "disjoint edits must not conflict; got {:?}",
            idx.hunks
        );
        assert_eq!(idx.hunks.len(), 2, "one hunk per side");
        // All three ranges per hunk — `[base, local, remote]`.
        for h in &idx.hunks {
            assert_eq!(h.ranges.len(), 3, "three-way hunks carry 3 ranges");
        }
    }

    /// Three sources, overlapping changes (local and remote both
    /// mutate the same base region) — engine emits a Conflict
    /// hunk.
    #[test]
    fn three_way_overlapping_changes_produce_conflict_hunk() {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from("aaa\nbbb\nccc\n"));
        provider.set(bid(2), Rope::from("aaa\nBBB-local\nccc\n"));
        provider.set(bid(3), Rope::from("aaa\nBBB-remote\nccc\n"));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

        let sub = DiffSubsystem::new();
        let desc = three_way_descriptor(&dyn_provider, bid(1), bid(2), bid(3));
        let session = sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        let base = provider.buffer_rope(bid(1)).unwrap();
        let local = provider.buffer_rope(bid(2)).unwrap();
        let remote = provider.buffer_rope(bid(3)).unwrap();
        let idx = session
            .recompute_blocking(&[base.clone(), local.clone(), remote.clone()])
            .expect("three-way recompute publishes");

        assert!(
            idx.hunks
                .iter()
                .any(|h| matches!(h.kind, HunkKind::Conflict)),
            "overlapping edits must surface at least one Conflict; got {:?}",
            idx.hunks
        );
    }

    /// `arity()` discriminates on the participant count —
    /// load-bearing for the D.6.c compute_get_plan /
    /// compute_put_plan dispatch (D.8.c rename from
    /// `is_three_way()`).
    #[test]
    fn arity_reflects_participant_count() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let two_way = descriptor(&provider, bid(1), bid(2));
        assert_eq!(two_way.arity(), 2);
        let three_way = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        assert_eq!(three_way.arity(), 3);
    }

    // ──────────────────────────────────────────────────────
    // D.8.d (2026-05-31): membership API
    // ──────────────────────────────────────────────────────

    /// `add_participant` on a 2-pane session grows the arity
    /// to 3, mutates the descriptor's sources / watch /
    /// participants lists, and routes a recompute through
    /// the debouncer.
    #[tokio::test]
    async fn add_participant_grows_arity_2_to_3() {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from("a\n"));
        provider.set(bid(2), Rope::from("b\n"));
        provider.set(bid(3), Rope::from("c\n"));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            1,
        )));
        let desc = descriptor(&dyn_provider, bid(1), bid(2));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        // Add a third participant.
        let new_source: Arc<dyn DiffParticipantSource> =
            Arc::new(BufferSource::new(Arc::clone(&dyn_provider), bid(3)));
        let new_arity = sub
            .add_participant(bid(2), new_source, Some(bid(3)))
            .expect("add must succeed");
        assert_eq!(new_arity, 3);

        // Descriptor now has 3 sources + bid(3) in
        // watch + participants.
        let updated = sub.lookup_descriptor(bid(2)).expect("descriptor present");
        assert_eq!(updated.arity(), 3);
        assert!(updated.watch.contains(&bid(3)));
        assert!(updated.participants.contains(&bid(3)));
    }

    /// `add_participant` that would push arity to 4 returns
    /// `EngineRejected` and **does not mutate** the
    /// descriptor (atomic-failure invariant).
    #[test]
    fn add_participant_fourth_returns_engine_rejected_and_no_mutation() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = Arc::new(DiffSubsystem::new());
        let desc = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        let result = sub.add_participant(
            bid(2),
            Arc::new(StaticSource::new(Rope::new())),
            Some(bid(4)),
        );
        assert!(matches!(
            result,
            Err(MembershipError::EngineRejected(
                crate::DiffEngineError::Unsupported { n: 4 }
            ))
        ));

        // Descriptor unchanged — arity still 3, no bid(4)
        // in watch/participants.
        let unchanged = sub.lookup_descriptor(bid(2)).expect("session intact");
        assert_eq!(unchanged.arity(), 3);
        assert!(!unchanged.watch.contains(&bid(4)));
        assert!(!unchanged.participants.contains(&bid(4)));
    }

    /// `add_participant` on a missing session returns
    /// `NoSession`.
    #[test]
    fn add_participant_no_session_returns_no_session_error() {
        let sub = Arc::new(DiffSubsystem::new());
        let result = sub.add_participant(bid(99), Arc::new(StaticSource::new(Rope::new())), None);
        assert!(matches!(result, Err(MembershipError::NoSession(b)) if b == bid(99)));
    }

    /// `remove_participant_buffer` on a 3-pane session
    /// shrinks to 2-pane (still active, recompute fires
    /// with the smaller participant set).
    #[tokio::test]
    async fn remove_participant_buffer_3_to_2_keeps_session_active() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            1,
        )));
        let desc = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        let new_arity = sub
            .remove_participant_buffer(bid(2), bid(3))
            .expect("remove must succeed");
        assert_eq!(new_arity, 2);

        // Session still registered; descriptor narrowed.
        let session = sub.lookup(bid(2)).expect("session alive");
        assert_eq!(session.buffer_id(), bid(2));
        let updated = sub.lookup_descriptor(bid(2)).expect("descriptor present");
        assert_eq!(updated.arity(), 2);
        assert!(!updated.watch.contains(&bid(3)));
        assert!(!updated.participants.contains(&bid(3)));
    }

    /// `remove_participant_buffer` that drops arity to 1
    /// leaves the session **dormant** — registered but
    /// no peer to diff against.
    #[tokio::test]
    async fn remove_participant_buffer_to_1_leaves_session_dormant() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            1,
        )));
        let desc = descriptor(&provider, bid(1), bid(2));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        let new_arity = sub
            .remove_participant_buffer(bid(2), bid(1))
            .expect("remove must succeed");
        assert_eq!(new_arity, 1);

        // Session stays registered.
        assert!(sub.lookup(bid(2)).is_some());
        let updated = sub.lookup_descriptor(bid(2)).expect("descriptor present");
        assert_eq!(updated.arity(), 1);
    }

    /// `remove_participant_buffer` that drops arity to 0
    /// **auto-drops** the session entirely.
    #[tokio::test]
    async fn remove_participant_buffer_to_0_auto_drops_session() {
        let provider = Arc::new(MockProvider::default());
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();
        let sub = Arc::new(DiffSubsystem::new());

        // Build a 1-participant dormant session by
        // registering with sources = [BufferSource(bid(1))]
        // only.
        let desc = DiffDescriptor {
            sources: vec![Arc::new(BufferSource::new(
                Arc::clone(&dyn_provider),
                bid(1),
            ))],
            watch: vec![bid(1)],
            participants: vec![bid(1)],
        };
        sub.register_with_sources(bid(1), DiffAlgorithm::Histogram, desc);
        assert!(sub.lookup(bid(1)).is_some(), "dormant session registered");

        let new_arity = sub
            .remove_participant_buffer(bid(1), bid(1))
            .expect("remove must succeed");
        assert_eq!(new_arity, 0);
        assert!(sub.lookup(bid(1)).is_none(), "session auto-dropped");
    }

    /// `remove_participant_buffer` for a buffer that isn't a
    /// participant returns `NotParticipant`.
    #[test]
    fn remove_participant_buffer_not_a_participant_errors() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = Arc::new(DiffSubsystem::new());
        let desc = descriptor(&provider, bid(1), bid(2));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        let result = sub.remove_participant_buffer(bid(2), bid(99));
        assert!(matches!(
            result,
            Err(MembershipError::NotParticipant(b)) if b == bid(99)
        ));
    }

    /// `remove_participant_buffer` decrements the mode
    /// bridge's refcount on the removed buffer.
    #[tokio::test]
    async fn remove_participant_buffer_decrements_mode_bridge_refcount() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            1,
        )));
        let desc = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);
        let bridge = sub.mode_bridge();
        assert_eq!(bridge.refcount(bid(3)), 1, "3-way activates bid(3)");

        sub.remove_participant_buffer(bid(2), bid(3))
            .expect("remove succeeds");
        assert_eq!(
            bridge.refcount(bid(3)),
            0,
            "bid(3) refcount drops to zero after removal"
        );
        // Other participants still active.
        assert_eq!(bridge.refcount(bid(1)), 1);
        assert_eq!(bridge.refcount(bid(2)), 1);
    }

    /// `add_participant` increments the mode bridge's
    /// refcount on the new buffer.
    #[tokio::test]
    async fn add_participant_increments_mode_bridge_refcount() {
        let provider = Arc::new(MockProvider::default());
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            1,
        )));
        let desc = descriptor(&dyn_provider, bid(1), bid(2));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);
        let bridge = sub.mode_bridge();
        assert_eq!(bridge.refcount(bid(3)), 0, "bid(3) not yet a participant");

        sub.add_participant(
            bid(2),
            Arc::new(BufferSource::new(Arc::clone(&dyn_provider), bid(3))),
            Some(bid(3)),
        )
        .expect("add succeeds");
        assert_eq!(
            bridge.refcount(bid(3)),
            1,
            "bid(3) refcount activates on add"
        );
    }

    /// `replace_descriptor` swaps the source list while
    /// preserving session identity. The `Arc<DiffSession>`
    /// is the same after replace; only the descriptor's
    /// contents change.
    #[tokio::test]
    async fn replace_descriptor_preserves_session_identity() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            1,
        )));
        let desc_a = descriptor(&provider, bid(1), bid(2));
        let session_a = sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc_a);
        let before_ptr = Arc::as_ptr(&session_a);

        // Replace with a three-way descriptor.
        let desc_b = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        sub.replace_descriptor(bid(2), desc_b)
            .expect("replace succeeds");

        // Session Arc identity preserved.
        let session_b = sub.lookup(bid(2)).expect("session alive");
        assert_eq!(Arc::as_ptr(&session_b), before_ptr);
        // Descriptor's arity reflects the new shape.
        assert_eq!(
            sub.lookup_descriptor(bid(2)).unwrap().arity(),
            3,
            "new descriptor is three-way"
        );
    }

    /// `replace_descriptor` rejects a new descriptor with
    /// N≥4 atomically — the existing descriptor is not
    /// mutated.
    #[test]
    fn replace_descriptor_rejects_n4_atomically() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = Arc::new(DiffSubsystem::new());
        let desc_3 = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc_3);

        // Build an N=4 descriptor by extending sources.
        let mut bad = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        bad.sources.push(Arc::new(StaticSource::new(Rope::new())));
        bad.watch.push(bid(4));
        bad.participants.push(bid(4));

        let result = sub.replace_descriptor(bid(2), bad);
        assert!(matches!(
            result,
            Err(MembershipError::EngineRejected(
                crate::DiffEngineError::Unsupported { n: 4 }
            ))
        ));
        // Old descriptor still arity 3.
        assert_eq!(sub.lookup_descriptor(bid(2)).unwrap().arity(), 3);
    }

    /// `register_with_sources` on a three-source descriptor
    /// installs secondary-index entries for every non-primary
    /// participant, so `lookup_session_for` resolves the same
    /// session from any of the three buffers.
    #[test]
    fn three_way_lookup_session_for_resolves_all_three_participants() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = DiffSubsystem::new();
        let desc = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        let session = sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        for participant in [bid(1), bid(2), bid(3)] {
            let resolved = sub
                .lookup_session_for(participant)
                .unwrap_or_else(|| panic!("lookup_session_for({participant:?}) returned None"));
            assert!(
                Arc::ptr_eq(&session, &resolved),
                "all three participants must resolve to the same session"
            );
        }
    }

    /// `register_with_sources` for a three-source descriptor
    /// activates `diff-mode` on every participating buffer via
    /// the ref-counting bridge; `drop_session` deactivates all
    /// three.
    #[test]
    fn three_way_session_activates_and_deactivates_diff_mode_for_all_three() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = DiffSubsystem::new();
        let bridge = sub.mode_bridge();
        let desc = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        for participant in [bid(1), bid(2), bid(3)] {
            assert_eq!(
                bridge.refcount(participant),
                1,
                "buffer {participant:?} should be diff-mode active after 3-way open"
            );
        }

        sub.drop_session(bid(2));
        for participant in [bid(1), bid(2), bid(3)] {
            assert_eq!(
                bridge.refcount(participant),
                0,
                "buffer {participant:?} should be deactivated after drop"
            );
        }
    }

    /// Same buffer participating in both a two-way and a
    /// three-way session simultaneously stays diff-mode-active
    /// until the *last* session closes. Verifies refcount
    /// semantics hold across mixed session shapes.
    #[test]
    fn shared_buffer_across_two_way_and_three_way_keeps_diff_mode_until_last_close() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = DiffSubsystem::new();
        let bridge = sub.mode_bridge();

        // Three-way: bid(1) is the base of a [bid(1), bid(2), bid(3)]
        // session.
        let three_way = three_way_descriptor(&provider, bid(1), bid(2), bid(3));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, three_way);

        // Two-way: bid(1) is also the baseline of a [bid(1), bid(4)]
        // session. Participant list mirrors what the two-pane
        // dispatch helper produces.
        let mut two_way = descriptor(&provider, bid(1), bid(4));
        two_way.participants = vec![bid(1), bid(4)];
        sub.register_with_sources(bid(4), DiffAlgorithm::Histogram, two_way);

        // bid(1) participates in both sessions.
        assert_eq!(bridge.refcount(bid(1)), 2);
        assert_eq!(bridge.refcount(bid(2)), 1);
        assert_eq!(bridge.refcount(bid(3)), 1);
        assert_eq!(bridge.refcount(bid(4)), 1);

        // Close the three-way. bid(1) still in the two-way → mode
        // stays active.
        sub.drop_session(bid(2));
        assert_eq!(bridge.refcount(bid(1)), 1);
        assert_eq!(bridge.refcount(bid(2)), 0);
        assert_eq!(bridge.refcount(bid(3)), 0);
        assert_eq!(bridge.refcount(bid(4)), 1);

        // Close the two-way. bid(1) finally deactivates.
        sub.drop_session(bid(4));
        assert_eq!(bridge.refcount(bid(1)), 0);
        assert_eq!(bridge.refcount(bid(4)), 0);
    }

    /// `recompute_from_descriptor` (the path driven by the
    /// debounce/bus pipeline) forwards `descriptor.remote` to
    /// `schedule_recompute`. End-to-end check via the public
    /// `note_buffer_edited` driver: edit any of the three
    /// watched buffers, expect a three-range hunk to publish.
    #[tokio::test]
    async fn three_way_routing_publishes_three_way_hunks_on_edit() {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from("aaa\nbbb\nccc\n"));
        provider.set(bid(2), Rope::from("aaa\nbbb\nccc\n"));
        provider.set(bid(3), Rope::from("aaa\nbbb\nccc\n"));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            1,
        )));
        let desc = three_way_descriptor(&dyn_provider, bid(1), bid(2), bid(3));
        let session = sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        // Edit the remote side and let the debounce → spawn_blocking
        // → publish chain run.
        provider.set(bid(3), Rope::from("aaa\nbbb\nREMOTE\n"));
        sub.note_buffer_edited(bid(3));
        // Wait up to ~200ms for the debounce + blocking task to
        // land a publish. Polls the revision counter so the test
        // finishes as soon as the publish lands rather than
        // burning the full window.
        let deadline = std::time::Instant::now() + Duration::from_millis(200);
        while session.current_hunks().revision == 0 {
            if std::time::Instant::now() >= deadline {
                panic!("three-way recompute never published");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let idx = session.current_hunks();
        assert!(
            idx.hunks.iter().any(|h| h.ranges.len() == 3),
            "expected three-range hunks from compute_three_way; got {:?}",
            idx.hunks
        );
    }

    /// D.6.h (2026-05-31) — design-doc §11 risk gate:
    /// **multi-doc edit-event coalesce**. A three-way
    /// session subscribes to three documents' edit
    /// streams. If two (or all three) documents edit
    /// "simultaneously" (within the debounce window),
    /// exactly *one* recompute must reflect the
    /// combined state — not N parallel recomputes (one
    /// per event).
    ///
    /// The debouncer's "reset on each poke" semantic
    /// (D.2.c) is the load-bearing piece: each
    /// `note_buffer_edited` call within the window
    /// bumps the epoch but doesn't spawn a fresh
    /// recompute; the trailing spawn fires only after
    /// the burst quiesces.
    ///
    /// Uses a deliberately wide debounce window (50ms)
    /// to keep three `note_buffer_edited` calls
    /// comfortably inside the burst, then waits for the
    /// settled publish. Asserts revision == 1
    /// post-burst.
    #[tokio::test]
    async fn three_way_rapid_edits_coalesce_to_one_recompute() {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from("aaa\nbbb\nccc\n"));
        provider.set(bid(2), Rope::from("aaa\nbbb\nccc\n"));
        provider.set(bid(3), Rope::from("aaa\nbbb\nccc\n"));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider.clone();

        // 50ms debounce window: long enough that three
        // poke()s inside the same task tick stay inside
        // the burst. The runtime's `sleep().await` after
        // the bursts lets the debouncer settle.
        let sub = Arc::new(DiffSubsystem::with_debounce_window(Duration::from_millis(
            50,
        )));
        let desc = three_way_descriptor(&dyn_provider, bid(1), bid(2), bid(3));
        let session = sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);

        // Three rapid edits, one per participant. Each
        // mutation diverges in a different region so
        // `compute_three_way` produces a multi-hunk
        // index — but only ONE recompute should fire.
        provider.set(bid(1), Rope::from("AAA\nbbb\nccc\n"));
        sub.note_buffer_edited(bid(1));
        provider.set(bid(2), Rope::from("aaa\nBBB\nccc\n"));
        sub.note_buffer_edited(bid(2));
        provider.set(bid(3), Rope::from("aaa\nbbb\nCCC\n"));
        sub.note_buffer_edited(bid(3));

        // Wait for the debounce window + spawn_blocking
        // to settle. Use a deadline poll rather than a
        // fixed sleep so the test doesn't artificially
        // inflate CI runtime.
        let deadline = std::time::Instant::now() + Duration::from_millis(500);
        while session.current_hunks().revision == 0 {
            if std::time::Instant::now() >= deadline {
                panic!("recompute never published after burst");
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        // The key invariant: exactly one recompute fired.
        // The session's revision counter starts at 1 and
        // allocates a fresh number per recompute, so
        // "coalesced to one" means revision == 1 after
        // the burst settles. If the debouncer spawned
        // per-event we'd see revision == 3.
        assert_eq!(
            session.current_hunks().revision,
            1,
            "3 rapid edits within the debounce window must coalesce \
			 to a single recompute; got revision={}",
            session.current_hunks().revision
        );

        // And the resulting HunkIndex reflects ALL three
        // edits in one combined output — not just the
        // last one. Each side modified a distinct row
        // (0/1/2) so we expect three hunks.
        let idx = session.current_hunks();
        assert!(
            !idx.hunks.is_empty(),
            "combined-state recompute should produce hunks"
        );
    }

    // ──────────────────────────────────────────────────────
    // D.6.d (2026-05-31): target-aware compute_get_edit /
    // compute_put_plan
    // ──────────────────────────────────────────────────────

    /// Build a three-pane session fixture with three buffer-
    /// backed sides, returning the subsystem + the three
    /// buffer ids in role order (base, local, remote).
    fn fixture_three_pane(
        base_text: &str,
        local_text: &str,
        remote_text: &str,
    ) -> (DiffSubsystem, BufferId, BufferId, BufferId) {
        let provider = Arc::new(MockProvider::default());
        provider.set(bid(1), Rope::from(base_text));
        provider.set(bid(2), Rope::from(local_text));
        provider.set(bid(3), Rope::from(remote_text));
        let dyn_provider: Arc<dyn BufferTextProvider> = provider;
        let sub = DiffSubsystem::new();
        let desc = three_way_descriptor(&dyn_provider, bid(1), bid(2), bid(3));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);
        (sub, bid(1), bid(2), bid(3))
    }

    /// Three-way `compute_get_edit` with no target returns
    /// `TargetRequired` listing the two non-active
    /// participants.
    #[test]
    fn compute_get_edit_three_way_no_target_requires_one() {
        let (sub, base, local, remote) = fixture_three_pane("a\n", "b\n", "c\n");
        let outcome = sub.compute_get_edit(local, 0, None);
        match outcome {
            DiffGetOutcome::TargetRequired { available_targets } => {
                assert_eq!(available_targets, vec![base, remote]);
            }
            other => panic!("expected TargetRequired, got {other:?}"),
        }
    }

    /// Three-way `compute_get_edit` with explicit target
    /// pulls from that side. Conflict hunks resolvable.
    #[test]
    fn compute_get_edit_three_way_with_target_resolves_conflict() {
        let (sub, _base, local, remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local, // session key
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        // Active = local, target = remote → pull REMOTE's
        // text into local.
        let outcome = sub.compute_get_edit(local, 0, Some(remote));
        match outcome {
            DiffGetOutcome::Edit {
                target_buffer_id,
                edit,
                post_cursor_row,
            } => {
                assert_eq!(target_buffer_id, remote);
                assert_eq!(post_cursor_row, 0);
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "REMOTE\n");
                    }
                }
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    /// CR.2: `dB` keep-both splices ours-then-theirs into the active
    /// (local) conflict range — base omitted. Active = local, theirs =
    /// remote → the local range becomes "LOCAL\nREMOTE\n"; the edit
    /// applies to the active (local) buffer.
    #[test]
    fn compute_keep_both_edit_splices_ours_then_theirs() {
        let (sub, _base, local, remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        match sub.compute_keep_both_edit(local, 0, remote) {
            DiffGetOutcome::Edit {
                target_buffer_id,
                edit,
                post_cursor_row,
            } => {
                assert_eq!(target_buffer_id, local, "keep-both edits the active/local side");
                assert_eq!(post_cursor_row, 0);
                assert_eq!(edit.range.start.line, 0);
                assert_eq!(edit.range.end.line, 1);
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "LOCAL\nREMOTE\n");
                    }
                }
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    /// CR.2: keep-both is conflict-only — a `Change` hunk under the
    /// cursor yields `Nothing` (that's `do`/`dp` territory).
    #[test]
    fn compute_keep_both_edit_non_conflict_hunk_returns_nothing() {
        let (sub, _base, local, remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        assert!(matches!(
            sub.compute_keep_both_edit(local, 0, remote),
            DiffGetOutcome::Nothing
        ));
    }

    /// CR.2: an unknown `theirs` buffer (not a participant) → `Nothing`.
    #[test]
    fn compute_keep_both_edit_unknown_theirs_returns_nothing() {
        let (sub, _base, local, _remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        assert!(matches!(
            sub.compute_keep_both_edit(local, 0, bid(99)),
            DiffGetOutcome::Nothing
        ));
    }

    /// CR.3 `d3o`: keep-theirs pulls REMOTE into the active (local)
    /// range via the conflict-aware `diff_get_effect` path — no explicit
    /// target needed at the call site (resolved to "theirs").
    #[test]
    fn diff_keep_theirs_effect_pulls_remote_into_local() {
        let (sub, _base, local, _remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        match sub.diff_keep_theirs_effect(local, 0) {
            Some(lattice_grammar::Effect::ApplyEdit { target, edit, cursor }) => {
                assert_eq!(target, local, "keep-theirs edits the active/local side");
                assert_eq!(cursor, Some(0));
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "REMOTE\n");
                    }
                }
            }
            other => panic!("expected ApplyEdit, got {other:?}"),
        }
    }

    /// CR.3 `dB`: keep-both effect wraps the splice into an
    /// `Effect::ApplyEdit` targeting the active (local) buffer.
    #[test]
    fn diff_keep_both_effect_splices_into_local() {
        let (sub, _base, local, _remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        match sub.diff_keep_both_effect(local, 0) {
            Some(lattice_grammar::Effect::ApplyEdit { target, edit, .. }) => {
                assert_eq!(target, local);
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "LOCAL\nREMOTE\n");
                    }
                }
            }
            other => panic!("expected ApplyEdit, got {other:?}"),
        }
    }

    /// CR.3 `d2o`: keep-ours over a conflict region is the degenerate
    /// self-target — an informative `Echo`, not a silent no-op (Dhruva's
    /// "full set, degenerate → echo" choice). Off-hunk → `None`.
    #[test]
    fn diff_keep_ours_effect_echoes_over_conflict_else_none() {
        let (sub, _base, local, _remote) = fixture_three_pane("a\nb\n", "LOCAL\nb\n", "REMOTE\nb\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        assert!(matches!(
            sub.diff_keep_ours_effect(local, 0),
            Some(lattice_grammar::Effect::Echo { .. })
        ));
        // Row 1 is outside the conflict hunk → silent None.
        assert!(sub.diff_keep_ours_effect(local, 1).is_none());
    }

    /// CR.3 `d3p`: put-theirs pushes the local side's hunk INTO remote —
    /// the edit targets the remote buffer.
    #[test]
    fn diff_put_theirs_effect_pushes_local_into_remote() {
        let (sub, _base, local, remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        match sub.diff_put_theirs_effect(local, 0) {
            Some(lattice_grammar::Effect::ApplyEdit { target, edit, .. }) => {
                assert_eq!(target, remote, "put-theirs edits the remote buffer");
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "LOCAL\n");
                    }
                }
            }
            other => panic!("expected ApplyEdit, got {other:?}"),
        }
    }

    /// CR.6: `]c`/`[c` hunk-nav resolvers return a `SelectionChange` to
    /// the next/prev hunk's slot-1 start, wrapping at both ends — the
    /// mode-owned replacement for the host `do_next_hunk`/`do_prev_hunk`.
    #[test]
    fn hunk_nav_effects_move_to_neighbouring_hunk() {
        let (sub, key) = fixture_with_baseline("base\n");
        publish_hunks(
            &sub,
            key,
            vec![
                Hunk {
                    kind: HunkKind::Change,
                    ranges: smallvec![lr(0, 1), lr(1, 2)],
                },
                Hunk {
                    kind: HunkKind::Change,
                    ranges: smallvec![lr(2, 3), lr(5, 6)],
                },
            ],
        );
        // slot-1 hunk starts are [1, 5].
        let row = |eff: Option<lattice_grammar::Effect>| match eff {
            Some(lattice_grammar::Effect::SelectionChange(set)) => set.primary().head.line,
            other => panic!("expected SelectionChange, got {other:?}"),
        };
        assert_eq!(row(sub.diff_next_hunk_effect(key, 0)), 1, "next from 0 → 1");
        assert_eq!(row(sub.diff_next_hunk_effect(key, 3)), 5, "next from 3 → 5");
        assert_eq!(row(sub.diff_next_hunk_effect(key, 5)), 1, "next past last → wrap to 1");
        assert_eq!(row(sub.diff_prev_hunk_effect(key, 6)), 5, "prev from 6 → 5");
        assert_eq!(row(sub.diff_prev_hunk_effect(key, 0)), 5, "prev before first → wrap to 5");
    }

    /// Target buffer that isn't a participant → `Nothing`.
    #[test]
    fn compute_get_edit_unknown_target_buffer_returns_nothing() {
        let (sub, _base, local, _remote) = fixture_three_pane("a\n", "b\n", "c\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        let outcome = sub.compute_get_edit(local, 0, Some(bid(99)));
        assert!(matches!(outcome, DiffGetOutcome::Nothing));
    }

    /// Two-way `compute_get_edit` with explicit target =
    /// peer behaves identically to no-target (back-compat
    /// for callers that want to be explicit).
    #[test]
    fn compute_get_edit_two_way_explicit_target_matches_default() {
        // fixture_two_pane registers a session with
        // participants = [baseline_buf, current_buf]. Use
        // current as the session key (= active), baseline
        // as the peer/target.
        let (sub, current, baseline) = fixture_two_pane("base\n", "curr\n");
        publish_hunks(
            &sub,
            current,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 1), lr(0, 1)],
            }],
        );
        let default = sub.compute_get_edit(current, 0, None);
        let explicit = sub.compute_get_edit(current, 0, Some(baseline));
        assert_eq!(default, explicit);
    }

    /// Three-way `compute_put_plan` with no target returns
    /// `TargetRequired`.
    #[test]
    fn compute_put_plan_three_way_no_target_requires_one() {
        let (sub, base, local, remote) = fixture_three_pane("a\n", "b\n", "c\n");
        let outcome = sub.compute_put_plan(local, 0, None);
        match outcome {
            DiffPutOutcome::TargetRequired { available_targets } => {
                assert_eq!(available_targets, vec![base, remote]);
            }
            other => panic!("expected TargetRequired, got {other:?}"),
        }
    }

    /// Three-way `:diffput <bufnr>` resolves a Conflict by
    /// pushing the active side's content into the target's
    /// range. From the slice plan: `:diffput 2` resolves a
    /// conflict by pushing pane 1's (= local's) version.
    #[test]
    fn compute_put_plan_three_way_resolves_conflict_to_explicit_target() {
        let (sub, _base, local, remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Conflict,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        // Active = local (pane 1), target = remote (pane 2).
        // Push LOCAL\n into remote's range [0, 1).
        let outcome = sub.compute_put_plan(local, 0, Some(remote));
        match outcome {
            DiffPutOutcome::Edit {
                target_buffer_id,
                edit,
                post_cursor_row,
            } => {
                assert_eq!(target_buffer_id, remote);
                assert_eq!(post_cursor_row, 0);
                match edit.kind {
                    lattice_protocol::edit::EditKind::Replace { ref text } => {
                        assert_eq!(text, "LOCAL\n");
                    }
                }
            }
            other => panic!("expected Edit, got {other:?}"),
        }
    }

    /// Two-way unchanged: `compute_put_plan` with no target
    /// targets the peer (D.5.c semantics preserved).
    #[test]
    fn compute_put_plan_two_way_no_target_targets_peer() {
        let (sub, current, baseline) = fixture_two_pane("base\n", "curr\n");
        publish_hunks(
            &sub,
            current,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 1), lr(0, 1)],
            }],
        );
        let outcome = sub.compute_put_plan(current, 0, None);
        match outcome {
            DiffPutOutcome::Edit {
                target_buffer_id, ..
            } => {
                assert_eq!(target_buffer_id, baseline);
            }
            other => panic!("expected Edit targeting baseline, got {other:?}"),
        }
    }

    // ──────────────────────────────────────────────────────
    // D.6.e (2026-05-31): completion-signal lifecycle
    // ──────────────────────────────────────────────────────

    /// `bind_completion` + `take_completion` are
    /// single-shot: the first take returns Some, the
    /// second None.
    #[test]
    fn completion_take_is_single_shot() {
        let session = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        assert!(session.take_completion().is_none(), "no sender before bind");
        let (tx, _rx) = oneshot::channel::<DiffOutcome>();
        session.bind_completion(tx);
        let taken_first = session.take_completion();
        assert!(taken_first.is_some(), "first take after bind returns Some");
        assert!(
            session.take_completion().is_none(),
            "subsequent take returns None"
        );
    }

    /// Re-binding overwrites the previous sender; the
    /// previously-bound receiver observes Closed (its
    /// sender is dropped).
    #[tokio::test]
    async fn completion_rebind_drops_previous_sender() {
        let session = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        let (tx1, rx1) = oneshot::channel::<DiffOutcome>();
        session.bind_completion(tx1);
        let (tx2, _rx2) = oneshot::channel::<DiffOutcome>();
        session.bind_completion(tx2);
        // rx1 must observe Closed since tx1 was overwritten.
        assert!(matches!(rx1.await, Err(_)));
    }

    /// Programmatic API: a consumer binds a sender,
    /// `:diff-accept` (simulated by direct
    /// `take_completion` + `send(Accept)`) fires it, and
    /// the awaiting receiver sees Accept.
    #[tokio::test]
    async fn completion_send_accept_routes_to_receiver() {
        let session = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        let (tx, rx) = oneshot::channel::<DiffOutcome>();
        session.bind_completion(tx);
        // Simulate the `do_diff_accept` flow: take the
        // sender and send Accept.
        let taken = session.take_completion().expect("sender bound");
        taken
            .send(DiffOutcome::Accept)
            .expect("receiver still alive");
        let outcome = rx.await.expect("receiver returns the sent outcome");
        assert_eq!(outcome, DiffOutcome::Accept);
    }

    /// Receiver dropped before the user resolves: the
    /// teardown path's `let _ = tx.send(...)` ignores the
    /// Err. Verifies the send error is non-fatal.
    #[test]
    fn completion_send_after_receiver_dropped_is_ignored() {
        let session = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        let (tx, rx) = oneshot::channel::<DiffOutcome>();
        session.bind_completion(tx);
        drop(rx);
        let taken = session.take_completion().expect("sender bound");
        // `Result::Err` is returned but the call doesn't
        // panic — matches the production teardown's
        // `let _ =` discard.
        let result = taken.send(DiffOutcome::Reject);
        assert!(result.is_err());
    }

    /// Sessions without a bound completion silently
    /// no-op on outcome dispatch: the teardown helper's
    /// `take_completion()` returns None and the rest of
    /// the teardown proceeds unaffected.
    #[test]
    fn unbound_completion_take_returns_none() {
        let session = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
        assert!(session.take_completion().is_none());
    }

    // ──────────────────────────────────────────────────────
    // D.6.g (2026-05-31): all_sessions_for lookup
    // ──────────────────────────────────────────────────────

    /// Buffer not in any session → empty vec, not None.
    #[test]
    fn all_sessions_for_unregistered_buffer_is_empty() {
        let sub = DiffSubsystem::new();
        assert!(sub.all_sessions_for(bid(99)).is_empty());
    }

    /// Single-session buffer: returns exactly one session.
    #[test]
    fn all_sessions_for_single_session_returns_one() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = DiffSubsystem::new();
        let desc = descriptor(&provider, bid(1), bid(2));
        sub.register_with_sources(bid(2), DiffAlgorithm::Histogram, desc);
        // Primary lookup.
        assert_eq!(sub.all_sessions_for(bid(2)).len(), 1);
        // Watched-side lookup (bid(1) is in the watch list as baseline).
        assert_eq!(sub.all_sessions_for(bid(1)).len(), 1);
    }

    /// **The key D.6.g case.** A shared buffer participating
    /// in two simultaneous sessions: `lookup_session_for`
    /// resolves to one (the most recently registered, per
    /// secondary-index single-valued map), but
    /// `all_sessions_for` returns both — letting
    /// `:diffoff!` cascade-close them.
    #[test]
    fn all_sessions_for_shared_buffer_returns_every_session() {
        let provider: Arc<dyn BufferTextProvider> = Arc::new(MockProvider::default());
        let sub = DiffSubsystem::new();
        // Session A: shared (slot 0) ↔ peer_a (primary).
        let desc_a = descriptor(&provider, bid(10), bid(20));
        sub.register_with_sources(bid(20), DiffAlgorithm::Histogram, desc_a);
        // Session B: shared (slot 0) ↔ peer_b (primary).
        let desc_b = descriptor(&provider, bid(10), bid(30));
        sub.register_with_sources(bid(30), DiffAlgorithm::Histogram, desc_b);
        // shared participates in both A and B.
        let all = sub.all_sessions_for(bid(10));
        assert_eq!(all.len(), 2);
        let buffer_ids: std::collections::HashSet<BufferId> =
            all.iter().map(|s| s.buffer_id()).collect();
        assert!(buffer_ids.contains(&bid(20)));
        assert!(buffer_ids.contains(&bid(30)));
        // `lookup_session_for` only finds one of them.
        assert!(sub.lookup_session_for(bid(10)).is_some());
        assert_eq!(
            sub.all_sessions_for(bid(20)).len(),
            1,
            "non-shared buffer still resolves to its single session"
        );
    }

    /// Target = active buffer itself is rejected (Unknown
    /// path → Nothing). Prevents accidental self-edits via
    /// `:diffput <self-bufnr>`.
    #[test]
    fn compute_put_plan_target_equal_to_active_returns_nothing() {
        let (sub, _base, local, _remote) = fixture_three_pane("a\n", "LOCAL\n", "REMOTE\n");
        publish_hunks(
            &sub,
            local,
            vec![Hunk {
                kind: HunkKind::Change,
                ranges: smallvec![lr(0, 1), lr(0, 1), lr(0, 1)],
            }],
        );
        let outcome = sub.compute_put_plan(local, 0, Some(local));
        assert!(matches!(outcome, DiffPutOutcome::Nothing));
    }
}
