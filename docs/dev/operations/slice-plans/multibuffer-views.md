# Multibuffer views — slice plan

Sequencing companion to
[`docs/dev/architecture/multibuffer-views.md`](../../architecture/multibuffer-views.md).
The design fragment is the source of truth for *what* and *why*;
this file owns *when* and *in what order*. Authoritative status
per slice lives in [`../implementation.md`](../implementation.md).

Each slice ships green-on-merge with the four artefacts
CLAUDE.md mandates: architecture documentation (the design
fragment, updated as needed), benchmark coverage where
load-bearing, test coverage of the new scenarios + failure
modes, graceful error handling.

| Slice   | Title                                       | What lands |
|---------|---------------------------------------------|------------|
| **M.-0** | ✅ Bench prep — Document hot-path baselines (2026-05-31) | Landed `document_read_p99_us` + `document_edit_p99_us` in `crates/lattice-core/benches/document_hotpath.rs` baselining the inner `Document` struct's hot-path reads / writes. Originally framed as the M.0 no-regression gate; after the M.0 design shifted to the handle-layer trait (Path B) the benches stay as general-purpose regression infrastructure for the inner struct. See [`benchmarks.md`](../benchmarks.md) for numbers. |
| **M.0** | ✅ `Document` trait at the handle layer (2026-05-31) | Path B per design fragment §3.1. Trait `Document: Send + Sync + 'static + Debug` in `lattice-runtime` abstracts over `RopeDocumentHandle` (today's renamed handle) and the future `MultibufferDocumentHandle` (M.1). Five-phase landing: A (`6d57483`) trait + impl; B+C (`7bcd1dc`) slot-replacement only + remove `replace` API + rewrite `do_edit`; D (`418fd64`) `Editor.document` typed `ActiveDocument` (newtype around `Arc<dyn Document>` with Default/Clone/Deref/Debug), `BufferRegistry::DocumentEntry.handle` typed `Arc<dyn Document>`; E (`9eaafd4`) workspace-wide rename `DocumentHandle` → `RopeDocumentHandle` (60 subs, 17 files). User-visible semantic shift: `:edit current_path.rs` allocates a fresh `BufferId` rather than swapping content in-place (vim quirk → emacs/find-file alignment); old buffer remains reachable via `:bn` / `:b N` / `:ls`. Zero change to inner `lattice-core::Document` struct, `DocumentActor`, `PublishedSnapshot`, `DocumentSnapshot`, or the mpsc write path. 1461 workspace tests pass. |
| **M.1** | ✅ `MultibufferDocumentHandle` (static read-only, 2026-05-31) | New `MultibufferDocumentHandle` impl of the `Document` trait (Path B handle-layer sibling to `RopeDocumentHandle`). Holds `Vec<Excerpt>` (line-range positions; anchor sliding deferred to M.4) + `HashMap<BufferId, Arc<dyn Document>>` source map + `ArcSwap<RowTranslation>` cache + `PublishedSnapshot` so `snapshot()` returns the same `Arc<DocumentSnapshot>` shape renderers already consume. Composed `rope()` view built eagerly at construction; manual `recompose()` for tests to re-pull source content after caller-driven mutation. Registered in `BufferRegistry` through the existing `BufferData::Document` slot (no kind branching — `DocumentEntry.handle: Arc<dyn Document>` from M.0 Phase D already accepts multibuffer). **Writes are rejected** (`apply_edit` / `undo` / `redo` / `save` / `save_as` / `set_selections` / `dispatch_with_cancel`) via the new `RuntimeError::ReadOnly` variant + `Pending::ready(...)` synthetic resolver. **Disambiguation note (2026-05-31):** automatic live updates (event-driven recompose, debounced rebuild, source-close auto-remove, anchor sliding) live in M.4 — M.1 ships the static composition + manual-recompose foundation that M.4 makes live. Tests landed: composed snapshot correctness, multi-source ordering, write rejection across all mutating methods, typed construction errors (`EmptyExcerpts` / `UnknownSource`), trait-object dispatch end-to-end, source-edit propagation via manual `recompose()`, registry round-trip through `BufferStore::handle_for`. |
| **M.2** | Excerpt rendering — resumes 2026-06-01 with M.2.b.2 design locked | Excerpt headers render as virtual rows + `]e` / `[e` / `]E` / `[E` motions land. Carved per design §3.6 (2026-05-31) — multibuffer concerns extract into `lattice-multibuffer`; `MultibufferMode` is the **major mode** for `BufferKind::Multibuffer`. **Paused** after M.2.b.1 (2026-05-31) for the H-series infrastructure pass; **resumed** 2026-06-01 after the H-series closed at H.2 with H.3 deferred (see [`kind-agnostic-buffers.md`](kind-agnostic-buffers.md) §10). M.2.b.2 design locked to introduce `ModeActivator` + `MultibufferRegistry` + `create_multibuffer_view` + the headerline-status convention for async providers (see architecture §3.7). See M.2.a / M.2.b.0 / M.2.b.1 / M.2.b.2 / M.2.b.3 / M.2.c below. |
| **M.2.a** | ✅ `MultibufferHeaderProvider` (2026-05-31) | Initial `VirtualRowProvider` impl in `lattice-host::multibuffer` plus `ExcerptHeader` / `ExcerptHeaderStyle` on `Excerpt`. Five tests. Lives in `lattice-host` temporarily; M.2.b.1 moves it into the dedicated crate. Commit `7b436e3`. |
| **M.2.b.0** | ✅ Reconnaissance — mode + grammar APIs (2026-05-31) | Read `lattice-mode` + `lattice-grammar`. **Findings:** (1) `Mode` trait already has typed `Guard: Send + 'static` associated type — `MultibufferMode::Guard = MultibufferModeGuard` carries the typed handle directly. (2) `BufferKind` → major-mode activation pipeline exists (`crate::modes::resolve_major_mode(kind, lang)` returns a `ModeId` consumed by `mode_registry.activate_major(...)`); `BufferKind::Multibuffer` slots in symmetrically. (3) `MotionContext` is intentionally narrow (`buffer, from, count, args, cancel`) — no `buffer_id`, no service access. Built-in motions like `]p` (next paragraph) derive structure from buffer content (blank lines); **multibuffer motions can't** because excerpt boundaries aren't in the rope text. **Prerequisite slice surfaced**: M.2.b.0.A extends `MotionContext` with `buffer_id`. |
| **M.2.b.0.A** | ✅ Grammar API: `MotionContext.buffer_id` (2026-05-31) | Adds `buffer_id: BufferId` to `MotionContext`. Threads through `execute(...)` + `execute_motion_only(...)` (new parameter), `execute_motion` / `execute_operator` / `resolve_target` internals, `ActorMsg::Dispatch` (new field), `RopeDocumentHandle` (new field + `buffer_id()` accessor; `spawn_document(buffer_id, document, registry)` is the new signature). All call sites updated to pass either `editor.document_buffer_id` (host dispatch) or `lattice_core::BufferId(0)` (test sentinel). Built-in motions ignore the new field; kind-specific motion handlers (M.2.b.3 multibuffer, future plugin-defined kinds) use it to look up active-mode state via service registries their closures captured at registration. Grammar stays free of `lattice-mode` / `ServiceRegistry` coupling. Workspace build clean. 1461 tests pass. Commit `6ec958d`. |
| **M.2.b.1** | ✅ Extract `lattice-multibuffer` crate (landed 2026-05-31, commit `1e16fd7`) | New crate `crates/lattice-multibuffer`. Moved from `lattice-runtime::multibuffer`: `Excerpt`, `ExcerptId`, `ExcerptHeader`, `ExcerptHeaderStyle`, `RowEntry`, `RowTranslation`, `MultibufferDocumentHandle`, `MultibufferError`. Moved from `lattice-host::multibuffer`: `MultibufferHeaderProvider`, `multibuffer_header_provider_id`, `default_header_cells`, `compose_header_rows`. Added `BufferKind::Multibuffer` variant in `lattice-core`. **Gap closed by H.1 (`22ee033`):** the matching `BufferData::Multibuffer(DocumentEntry)` variant was added then; before H.1 the kind was declared-but-unreachable. |
| **M.2.b.2** | ✅ `ModeActivator` + `MultibufferMode` + `MultibufferRegistry` + `create_multibuffer_view` (landed 2026-06-01, commit `501bb41`) | Architecture: [`multibuffer-views.md`](../../architecture/multibuffer-views.md) §3.7. Ships the **five abstractions** the design pass locked in: (1) `ModeActivator` trait in `lattice-mode` + `impl ModeActivator for Editor` in `lattice-host` (thin wrappers over the existing fat helper; `services()` returns a cloned `Arc<ServiceRegistry>`; activation cascade signals drain into Editor's internal queue, trait returns `()`). (2) `MultibufferRegistry` trait + `InMemoryMultibufferRegistry` impl in `lattice-multibuffer/src/registry.rs` keyed by view BufferId returning `Arc<MultibufferDocumentHandle>` (typed, downcast-free). (3) `MultibufferMode` in `lattice-multibuffer/src/mode.rs` declaring `target_buffer_kind() = Some(BufferKind::Multibuffer)` (consumed by H.2's `find_major_for_kind`) and contributing `ReadOnly = true` + `NoFile = true` (M.3 makes `ReadOnly` conditional on edit-propagation policy). (4) `register_multibuffer_modes(&mut registry, &events, &mut services)` boot helper: registers the mode, registers `InMemoryMultibufferRegistry` as a service, wires the `DocumentClosed` cleanup subscriber. (5) `create_multibuffer_view(activator, sources, excerpts, name, flags) -> BufferId` in `lattice-multibuffer/src/view.rs` — atomically allocates BufferId, builds `MultibufferDocumentHandle`, registers typed handle in MultibufferRegistry, inserts via `BufferStore::insert_document_buffer` (H.1), activates major via `activator.activate_major_for_kind(id, Multibuffer)`. Accepts empty `sources` + `excerpts` for the async-provider pattern. Plus: `MultibufferDocumentHandle::append_excerpts` + `replace_excerpts` (the minimum update API for the worked-example provider). Host wiring: one boot-time `register_multibuffer_modes` call; zero `match BufferKind::Multibuffer` arms. Tests: `create_multibuffer_view_inserts_and_activates_major`, `multibuffer_registry_handle_lookup_succeeds`, `document_closed_cleans_up_registry_entry`, `stub_provider_can_pull_typed_handle_from_ctx`, `append_excerpts_publishes_updated_event`. Mock `ModeActivator` for unit tests so they don't spin a full Editor. |
| **M.2.b.3** | ✅ Excerpt motions (landed 2026-06-01, commit `3274214`) | Register `]e` / `[e` / `]E` / `[E` as `lattice-grammar` motions with motion ids `multibuffer.next-excerpt-start` / `prev-excerpt-start` / `next-excerpt-end` / `prev-excerpt-end`. Bind in `MultibufferMode`'s keymap. Motion handlers compute new cursor row from current row + excerpt geometry (read through the mode's `MultibufferModeContext`). Pure functions, unit-testable without grammar plumbing. Tests: motion lands on first row of next excerpt; landing past last excerpt is a no-op (cursor unchanged); operator composition (`d]e` deletes through next excerpt start); count composition (`3]e` jumps three excerpts ahead). |
| **M.2.c** | ✅ Multibuffer benches (landed 2026-06-01, commit `7a7793b`) | Criterion bench measuring composed-snapshot read + virtual-row interleave cost at 50 visible excerpts. Gate ≤ 200µs per the §7 performance posture. Lives in `lattice-multibuffer/benches/` for crate-local CI. |
| **M.3** | ✅ Edit propagation (landed 2026-06-01) | `MultibufferDocumentHandle::apply_edit` translates composed coordinates → source coordinates via the row-translation walk, forwards to the source `Document::apply_edit`. Boundary clipping (architecture §4): edits spanning multiple excerpts clip end to the start excerpt's last source row end-of-line via `build_source_edit`'s snapshot read; the tail dropped from subsequent excerpts. `apply_edit_batch` translates up-front per edit, then awaits sequentially via `Pending::spawn` (new helper in `lattice-runtime`) — combines N source Pendings into one without blocking the runtime. `undo` / `redo` fan out across every source the view references; each source's response is collected (errors per-source tolerated — "nothing to undo" is fine). `save` / `save_as` / `set_selections` / `dispatch_with_cancel` stay `ReadOnly` (multibuffers aren't files; selections are view-owned; grammar runs at host layer). `MultibufferMode::options()` drops `ReadOnly` from the major's contribution — providers that need a read-only view layer a minor contributing `ReadOnly = true`. Tests: 7 new M.3 tests covering insert translation, off-zero-start excerpt translation, in-excerpt delete, out-of-range no-op, boundary clip (B-source untouched on cross-excerpt delete), batch serialisation, undo fan-out. `dispatches_via_dyn_document` updated for the propagating path. Workspace tests green. |
| **M.4** | ✅ Live updates from source buffers + headerline API (landed 2026-06-01) | `MultibufferDocumentHandle::attach_event_subscriptions(events)` subscribes the view to `EventKind::DocumentChanged` + `DocumentClosed` on the bus. Forwarder task (spawned via tokio) filters by source `DocumentId` (mapped to BufferId via `source_buffer_for_document_id`); on DocumentChanged it recomposes; on DocumentClosed it prunes the source from the map AND publishes `MultibufferSourceClosed { view, source }` so providers choose policy. Forwarder holds a `Weak<MultibufferInner>`; subscriptions tracked in a `SubscriptionBookkeeping` on Inner so `Drop` unsubscribes. Idempotent (re-attach is no-op). Graceful no-runtime path: when called outside a tokio runtime, bus is stashed (so `set_headerline` can still publish) but the forwarder task is skipped — matches `register_multibuffer_modes`'s shape. **Headerline API**: `HeaderlineStatus` enum (`Idle / InProgress { label, count } / Complete { summary } / Failed { reason }`), `set_headerline(status)`, `headerline() -> Arc<HeaderlineStatus>`; publishes `MultibufferHeaderlineChanged { view, status }`. View-header renderer adoption follows in a later UI slice. `create_multibuffer_view` auto-calls `attach_event_subscriptions` when the EventBus service is registered; boot wiring exposes `Arc<EventBus>` in the `ServiceRegistry`. **Deferred to M.4.1**: anchor sliding (`Anchor { line, col, generation }` refactor of `Excerpt::{start_line, end_line}` — significant data-model change), self-loop debouncer (currently recompose runs on multibuffer-originated source edits too; idempotent so correct, just slightly wasteful), bench (`multibuffer_source_edit_p99_us` gate). 5 new M.4 tests cover headerline idempotency, headerline publish, source-change recompose, source-close pruning + typed-event publish, attach idempotency. Workspace tests green. |
| **M.4.1** | ✅ Anchor sliding + source-edit bench (landed 2026-06-01) | `slide_anchors_for_source` walks `Vec<AppliedEdit>` from the source's `DocumentChanged` event and shifts excerpts whose `start_line` sits strictly below the edit's `original_range.end.line` by the row delta. Conservative bias: edits AT or overlapping the excerpt's start row don't slide — false-positive slides during boundary edits would surprise the user more than a stale row position. The recompose path picks up new content for in-excerpt edits; below-excerpt edits don't affect position. Excerpt struct kept as `start_line` / `end_line` integers (a first-class `Anchor { line, col, generation }` primitive lands later if column-precise tracking proves load-bearing — none of the M.4.1 worked examples need it). 5 new tests: insert-above slides down, delete-above slides up, edit-below no-op, overlapping-edit no-slide, other-source no-cross-source-slide. Bench `multibuffer_source_edit_p99_us`: **27 µs at 1k excerpts** (CI gate ≤ 200 µs at 1k excerpts — 7× under budget). Self-loop debouncer deferred to M.4.2 if profiling warrants — current recompose is correct on self-loops, just slightly wasteful. |
| **M.4.2** | Self-loop debouncer (deferred — concrete revisit triggers below) | Skip recompose when the multibuffer's own `apply_edit` caused the source change. Track per-source version of last self-originated edit; on DocumentChanged, if version matches, skip. **Deferral rationale (2026-06-01, heuristic-checked):** paramount goal #1 (8 ms keystroke budget) has 295× headroom over the 27 µs wasted recompose; goal #4 (async) not violated (the redundant recompose is non-blocking); heuristic #1 (easy ≠ tiebreaker) doesn't force the harder path here because deferring doesn't contradict any paramount goal; the debouncer is purely additive — landing it later doesn't repay anything. **Concrete signal to revisit any of**: (a) a future feature needs the "I caused this" state for UI distinction (provider edit-acknowledgement, "saved" indicators, source-of-truth detection), (b) profiling shows per-keystroke multibuffer overhead exceeds 1 ms in a real workload, (c) a provider lands that creates 10k+ excerpt views with per-keystroke edits and the flat-scaling observation from M.4.1 stops holding. Until one of these triggers fires, the redundant recompose stays. |
| **M.5** | Expand-context affordance                   | `:multibuffer-expand [n]` / `:multibuffer-contract [n]` ex-commands and the bound keys (`+` / `-` on excerpt header). Translates to anchor-range mutation on the relevant excerpt; translation rebuild fires through the standard path. Tests: expand grows the excerpt; contract shrinks; expand below 1 row is a no-op; expand past the source buffer's end clips. |
| **M.6** | First provider — `lattice-multibuffer::providers::search` (locked 2026-06-01) | Architecture: [`multibuffer-views.md`](../../architecture/multibuffer-views.md) §3.7 "Provider model" + "Async provider pattern" + "Worked example — SearchProvider end-to-end". **Lives within `lattice-multibuffer`** in `src/providers/search.rs` (per the 2026-06-01 decision that all in-tree providers ship within the multibuffer crate, feature-gated). Adds the `search` cargo feature pulling `ignore`, `grep-matcher`, `aho-corasick`. Ships: (1) `ProjectSearchService` trait + `InMemoryProjectSearchService` impl + `ProjectSearchServiceHandle` registered in `ServiceRegistry` at boot; per-view state keyed by view BufferId (query, options, results, scan-task `JoinHandle`). (2) Typed events: `ProjectSearchBatchReady { view, files }`, `ProjectSearchCompleted { view, total }`, `ProjectSearchRefreshed { view, new_query }`, `ProjectSearchProgressUpdated { view, files_scanned }`. (3) `ProjectSearchMultibufferMode` (minor) — keymap (`<CR>` jump-to-source, `r` re-run, `q` close), `on_activate` subscribes to the typed events via per-view forwarder tasks, Guard cleans up on Drop. (4) `project_search(activator, query, options) -> BufferId` public trigger: creates EMPTY view via `create_multibuffer_view`, seeds state, sets headerline to `InProgress`, activates minor, spawns async scan task via the service. Scan task runs on tokio worker threads (never UI thread); publishes batches sized by `BATCH_FILES` OR `BATCH_INTERVAL`; updates headerline to `Complete` on done. (5) Host's `:search <query>` ex-command in `lattice-host` calls `lattice_multibuffer::providers::search::project_search` and threads its `&mut editor` as the activator. Tests: trigger returns a BufferId synchronously without I/O; scan task publishes batched events; minor's forwarder appends excerpts under filter-by-view; query refresh cancels prior scan + spawns new; source-close auto-removes excerpts (via M.4's `MultibufferSourceClosed` subscriber); view close clears service state. Bench: `project_search_first_batch_p99_ms` ≤ 50ms on a 1k-file corpus; `project_search_full_scan_throughput_files_per_sec`. |
| **M.7** | Excerpt fold provider                       | `ExcerptFoldProvider` registers one fold range per excerpt's composed-row range into the existing fold registry. **No new keymaps** — the standard `z*` vocabulary (`za` / `zo` / `zc` / `zR` / `zM`, `foldlevel=N`, `:foldopen` / `:foldclose`) covers excerpts identically to syntactic / marker folds (§6.5). `za` on a row inside an excerpt collapses to the excerpt's M.2 header virtual row. Composition: when a hunk (D.3.f) sits inside an excerpt, the smallest enclosing fold wins on `za`; repeated presses walk outwards. Lands after M.4 (live-updates) so fold ranges stay current as source edits shift anchors. Tests: excerpt fold range matches the composed-row span; `za` toggles without affecting other folds; nested hunk-inside-excerpt prefers innermost on `za`; expand-context (M.5) widens the fold range. |
| **M.8** | File-boundary fold provider                 | `FileBoundaryFoldProvider` registers one fold range per distinct `source: BufferId` covering the union of that file's excerpts. **No new keymaps** — same vocabulary as M.7. Surfaces "collapse all hunks/excerpts in this file to one summary row" as a natural `za` action on the file-header row. Essential for project-wide diff (A.1) + AI multi-file diff (A.2) where a user reviewing 50 files wants a top-down outline. Composition: file > excerpt > hunk nesting on `za`. Lands after M.6 so provider-driven excerpt sets are stable before fold ranges union across them. Tests: file fold range covers excerpts contiguously; `za` toggle collapses all excerpts of one file; multi-file multibuffer with 100 files × 5 excerpts collapses into a 100-row outline at `:set foldlevel=0`. |

Slice sequencing:

- **M.-0 (landed 2026-05-31).** Zero-behaviour bench slice
  baselining the inner `Document` struct's hot-path reads
  + writes. Not strictly load-bearing for M.0 under the
  Path B design (M.0 doesn't touch the inner struct), but
  kept as general-purpose regression infrastructure — the
  inner Document remains perf-sensitive and any future
  change there should clear the same gate.
- **M.0 is the load-bearing slice** — the handle-layer trait
  abstraction. Lands green standalone; everything else
  depends on it.
- **M.1** depends on M.0.
- **M.2** depends on M.1 + D.0 (consumes the virtual-row
  primitive; lands D.0 if D.0 hasn't already shipped).
- **M.3** depends on M.2 (need rendering to test edit
  visibility) + M.1.
- **M.4** depends on M.3 (need editable multibuffer to test
  cross-pane edit visibility).
- **M.5** depends on M.4.
- **M.6** depends on M.4 (provider needs editable +
  live-updating multibuffer).
- **M.7** depends on M.4 (fold ranges shift with anchor
  updates; live-update must be stable first).
- **M.8** depends on M.6 (provider-driven excerpt sets must
  be stable before fold ranges union across them).

## N.1 — Narrow mode (follow-on, depends on M.3)

| Slice   | Title       | What lands |
|---------|-------------|------------|
| **N.1** | Narrow mode | `NarrowProvider` (one excerpt with `ExcerptEdgeMode::Strict`); `:narrow` over the active visual region; `:narrow-to-defun` (tree-sitter scope at point); `:narrow-to-paragraph`; `:widen` sugar over `:bd`. Tests: narrow over a line range renders only that range; edits propagate to the source; widen restores; partial-line edges render exactly `[start.col, end.col]`; stacked narrow (narrow within a narrow's output buffer) edits propagate two hops to the real source; two panes narrowed to different ranges of the same source stay live-synced through M.4. No bench gate (composed primitive — perf is covered by M.3 / M.4 gates). |

`N.1` depends on **M.3** (editable multibuffer + edit
propagation) — that's the load-bearing prerequisite for
"edits in the narrowed view save to the original buffer."
Multiple parallel narrows and stacked narrow additionally
rely on **M.4** (live source-edit propagation) for cross-pane
consistency; lands once M.4 is green.

Full design notes live in the design fragment's §10
"Follow-on consumers" appendix (A.5).

## Consumer providers — slice catalogue

Each entry below is a provider submodule in
`crates/lattice-multibuffer/src/providers/<name>.rs`, gated
behind a cargo feature. The repeated shape (locked 2026-06-01
in architecture §3.7 "Provider model"): per-provider service
trait + impl + handle, per-provider minor mode, public trigger
function, typed events, `register_<provider>` boot helper. M.6
lands `lattice-multibuffer::providers::search` as the worked
example; every entry below follows the same template.

**Locked 2026-06-01:** all in-tree providers ship within
`lattice-multibuffer` (not as standalone crates). Each provider
adds its own cargo feature pulling its data-source dependencies
(`search` → `ignore`/`grep-matcher`/`aho-corasick`;
`lsp-references` → `lattice-lsp`; `diff` → `lattice-diff` after
the diff subsystem extraction; `diagnostics` → `lattice-diagnostics`
after that extraction; `ai-edits` → AI integration crate when
it lands). Default-features ships all providers; opt-out builds
disable per feature.

**Dep-direction constraint** (audited 2026-06-01):
`lattice-host` already depends on `lattice-multibuffer`, so
`lattice-multibuffer` MUST NOT depend on `lattice-host`. Any
provider whose data source currently lives in `lattice-host`
(diff, diagnostics, host-side LSP fan-in) requires extracting
the data source to its own crate first.

**Universal prerequisites** (assume for every entry unless
noted): **M.2.b.2** (`ModeActivator` + `MultibufferRegistry` +
`create_multibuffer_view`), **M.3** (edit propagation — for
editable providers), **M.4** (live source updates + headerline
status API), **M.7 / M.8** (excerpt + file folds — recommended
for any N≥10-excerpt view).

**Priority labels:**

- **A** — daily-driver workflows; high impact, high frequency.
- **B** — power features / plugin foundations; specialised but
  unlock workflows lattice will be defined by (magit-style VCS,
  LSP-driven refactors, AI multi-file edits).
- **C** — speculative / experimental; useful once the v1 core
  is stable.

### Priority A — daily-driver workflows

**A.1 `ProjectDiffProvider`** — Composes every changed file
in the working tree (or a comparison range) into one multibuffer.
One excerpt per hunk; file-boundary folds (M.8) group hunks per
file so a 50-file diff collapses to a 50-row outline at
`:set foldlevel=0`.
- Extra deps: **D.7** (VCS subsystem, deferred) for the hunk
  source-of-truth; falls back to a `git diff`-shell-out
  implementation in the interim.
- User surface: `:project-diff` (working tree vs HEAD),
  `:project-diff staged`, `:project-diff <rev1>..<rev2>`,
  `:project-diff branch <name>`.
- Edit propagation: hunk edits write back to the working-tree
  file via the standard apply_edit path; staged-diff edits
  rewrite the index entry (post-D.7).
- Tests: 10-file diff → expected excerpt count; edit a hunk
  excerpt → working-tree file updates and the hunk recomputes;
  closing a source file auto-removes its excerpts; foldlevel=0
  shows one row per file.
- Compose: A.1 is the canonical AI-multi-file-diff base too —
  A.2 reuses A.1's rendering with a different edit-commit path.

**A.2 `AIProposedEditsProvider`** — Renders an LSP-shape
`WorkspaceEdit` from an AI plugin as a multibuffer with
per-excerpt accept/reject. Solves the "AI proposed changes to
14 files — review each, accept most, reject some" workflow
that's currently fragmented across editors.
- Extra deps: AI plugin API (post-v1 — uses the WIT plugin
  Resource shape `Arc<dyn Document>`).
- User surface: AI plugin calls `host.show_workspace_edit(edits,
  on_commit_callback)` → multibuffer opens; user navigates
  excerpts via `]e` / `[e`; per-excerpt `:accept-excerpt` /
  `:reject-excerpt`; bulk `:accept-all` / `:reject-all`;
  `:commit` invokes the callback with the surviving edits.
- Tests: synthesize a 5-file `WorkspaceEdit`; accept 3, reject
  2; verify only accepted edits land in source files; callback
  receives the committed-edits subset.
- Compose: shares the file-boundary-fold rendering with A.1;
  per-excerpt accept/reject UI generalises into A.8.

**A.3 `LspReferencesProvider`** — `gr` (find references) /
`:lsp-references` opens every reference site as a multibuffer.
Each excerpt shows the call-site with configurable surrounding
context (default ±2 lines). Edit-in-place propagates to source
files for cross-callsite refactors.
- Extra deps: LSP subsystem (already in lattice).
- User surface: `gr` keymap, `:lsp-references [<symbol>]` ex-
  command (without symbol arg, uses cursor's symbol).
- Edit propagation: standard via M.3; per-source LSP version
  match enforced — if a source-file version changed mid-edit
  (e.g., LSP server reformatted), surface a warning before
  applying.
- Tests: mock LSP returns 8 reference locations across 3 files
  → 8 excerpts; edit one → source file updates; version-
  mismatch path: pre-edit snapshot vs LSP-reported version
  diverged → user prompted before commit.

**A.4 `DiagnosticsProvider`** — All LSP diagnostics across the
workspace as one multibuffer. Header per excerpt encodes
severity (Error/Warning/Info/Hint), file:line, and the message;
the excerpt body shows the offending line + context.
- Extra deps: LSP subsystem.
- User surface: `:diagnostics` (all severities),
  `:diagnostics warning` (filter), `:diagnostics file <path>`
  (single-file scope); `]d` / `[d` motions for next/prev
  diagnostic *inside* the multibuffer (distinct from the
  same-named diagnostics motions in regular buffers).
- Edit propagation: fixing the offending line in the excerpt
  updates the source; LSP re-runs on the next debounce tick;
  diagnostic excerpts auto-remove when the diagnostic clears.
- Tests: 12 diagnostics across 4 files → 12 excerpts; filter
  by severity → reduced set; edit clears matching diagnostic
  after the next LSP republish.

**A.5 `NarrowProvider`** — covered by [N.1](#n1--narrow-mode-follow-on-depends-on-m3)
above. Cross-ref.

**A.6 `QuickfixProvider`** — The canonical refactor surface for
compiler errors / test failures / lint warnings. Replaces
vim's traditionally read-only quickfix list with an editable
multibuffer. The compile → multibuffer → fix-in-place →
recompile loop collapses into one surface.
- Extra deps: none beyond M-series.
- User surface: `:copen` opens the active quickfix list as a
  multibuffer; `:make` populates it from compiler output;
  `:cgetexpr <cmd>` from arbitrary tool output; `:cnext` /
  `:cprev` motions navigate *between* excerpts (mirroring
  vim's existing keys); `:lopen` for the location-list
  variant.
- Edit propagation: standard via M.3; the source file is
  updated; the quickfix entry auto-recomputes on the next
  populate tick.
- Tests: synthetic compiler-error list of 6 entries across 3
  files → 6 excerpts; edit excerpt → source file changes;
  `:cnext` from within an excerpt jumps to the next excerpt's
  cursor position.
- Compose: A.6 is the bridge between vim's quickfix grammar
  and the multibuffer abstraction — every other "list of
  locations" surface (A.3, A.4, A.10, A.11, A.12) can
  optionally route through quickfix for uniform navigation.

### Priority B — power features + plugin foundations

**A.7 `GitHunksProvider`** — magit-style cross-file VCS
workflow. All staged hunks (or working-tree hunks, or both)
across the workspace as one editable multibuffer. Per-excerpt
stage / unstage / discard actions. This is one of magit's
defining surfaces; lattice-vcs (D.7) + a magit-style plugin
will compose on top of it.
- Extra deps: **D.7** (VCS subsystem) + the magit-style plugin
  layer that consumes lattice-vcs.
- User surface: `:Gstaged` (staged-hunks multibuffer),
  `:Gunstaged` (working-tree hunks), `:Gdiff <rev>` (working
  vs rev). Per-excerpt: `s` stages from working tree, `u`
  unstages, `x` discards. `:Gcommit` opens the commit-message
  editor with the staged multibuffer's content shown for
  reference.
- Edit propagation: working-tree hunks propagate via the
  standard apply_edit path; staged hunks rewrite the index
  via lattice-vcs.
- Tests: stage 4 hunks across 2 files → `:Gstaged` shows 4
  excerpts; unstage one → excerpt disappears, reappears in
  `:Gunstaged`; staged-hunk edit rewrites the index entry,
  working-tree file is untouched.

**A.8 `LspRefactorPreviewProvider`** — `<F2>` /
`:lsp-rename Foo` returns a `WorkspaceEdit`; this provider
renders it as a multibuffer with per-excerpt accept/reject
before commit. Today most editors apply LSP rename blind;
this gives a review step.
- Extra deps: LSP subsystem.
- User surface: rename keymap + `:lsp-rename <new-name>` ex-
  command. Per-excerpt `:accept` / `:reject`; `:accept-all`;
  `:cancel` discards.
- Edit propagation: only at commit time. Excerpts are read-
  only previews of the proposed change; the actual write
  happens via a batched edit on `:accept-all` (or per-excerpt
  on `:accept`).
- Tests: 15 call-sites of `foo` renaming to `bar`; reject 2 →
  13 commits land, 2 sites unchanged; cancel discards
  everything.
- Compose: shares the per-excerpt accept/reject UI with A.2;
  factor the affordance into a shared `acceptable_excerpt`
  trait surface once both ship.

**A.9 `LspWorkspaceSymbolsProvider`** — `:WorkspaceSymbols Foo`
opens every definition + key call-site of the symbol as a
multibuffer. The canonical cross-codebase signature-refactor
surface: change one type, see every place that breaks, fix
in-place.
- Extra deps: LSP subsystem (`workspace/symbol` request).
- User surface: `:WorkspaceSymbols <query>` ex-command;
  workspace-symbol picker (`gO`) optionally opens result in a
  multibuffer instead of jumping to the top match.
- Edit propagation: standard.
- Tests: workspace with 3 definitions of `Foo` + 8 callers →
  11 excerpts; edit a definition → source updates; mock LSP
  with multi-file symbol results.

**A.15 `OutlineBufferProvider`** — Symbol outline of the
current file (or N files, or a directory) as a multibuffer.
Each symbol an excerpt with its signature line and (optionally)
its body. Folds via M.7 collapse symbols to one-line summaries,
giving a navigable cross-file outline.
- Extra deps: tree-sitter outline OR LSP `textDocument/
  documentSymbol`.
- User surface: `:outline` (active file), `:outline-workspace`,
  `:outline-dir <path>`; `]s` / `[s` motions for next/prev
  symbol within the outline; standard `z*` folding.
- Edit propagation: standard — editing a symbol's body in the
  outline propagates to its source file.
- Tests: file with 10 functions → 10 excerpts; toggle fold →
  outline collapses; cross-file outline of 5-file module
  composes correctly.

### Priority C — specialised / experimental

**A.10 `LspCallHierarchyProvider`** — Incoming / outgoing
calls of a function as a multibuffer. Useful for refactoring
across a call chain.
- Extra deps: LSP `callHierarchy/*` requests.
- User surface: `:call-hierarchy incoming` / `outgoing` from
  cursor; navigate with `]C` / `[C`.
- Tests: function with 4 callers in 2 files → 4 incoming-call
  excerpts; outgoing calls shape similarly.

**A.11 `LspTypeHierarchyProvider`** — Supertypes / subtypes /
trait impls as a multibuffer. Useful for inheritance / trait
work.
- Extra deps: LSP `typeHierarchy/*` requests.
- User surface: `:type-hierarchy super` / `sub` / `impls`
  from cursor.
- Tests: trait with 6 impls → 6 excerpts; class with 3 sub-
  types → 3 excerpts.

**A.12 `TodoProvider`** — Workspace-wide TODO/FIXME/XXX/HACK
scan. Each match an excerpt with ±N context lines. Edit-in-
place to update or remove. Live-updates on debounce as files
change.
- Extra deps: none.
- User surface: `:todos`, `:fixmes`, `:scan <pattern>` for
  arbitrary regex; option `multibuffer.todo_patterns` for
  user-configured tags.
- Tests: workspace with 8 TODOs across 4 files → 8 excerpts;
  edit TODO comment out of existence → excerpt auto-removes
  on next scan.

**A.13 `MergeConflictTriageProvider`** — Every conflict marker
(`<<<<<<<` / `=======` / `>>>>>>>`) in the working tree as one
multibuffer. `:diffput` / `:diffget` (already shipped in D.6)
work per-excerpt; `:resolve-all-ours` / `:resolve-all-theirs`
for batch.
- Extra deps: none — D.6 conflict-marker recognition already
  shipped.
- User surface: `:conflicts` opens; navigate with `]C` / `[C`;
  resolve per excerpt with existing diff grammar.
- Tests: working tree with 5 conflicts across 3 files → 5
  excerpts; resolve one → excerpt auto-removes on next scan;
  `:resolve-all-ours` clears all.

**A.14 `PRReviewProvider`** — Read-only multibuffer of a pull
request's diff with inline comment threads as excerpt
decorations. Magit/forge plugin territory.
- Extra deps: magit-style plugin API (post-v1) + GitHub /
  GitLab API integration.
- User surface: `:gh-pr <number>` (or magit equivalent);
  navigate hunks with `]h` / `[h`; comment with
  `:pr-comment`.
- Tests: synthetic PR with 7 file changes + 3 inline comments
  → 7 excerpts with 3 carrying comment decorations.

**A.16 `CompilationOutputProvider`** — `cargo build` /
`cargo test` output, parsed and rendered as a multibuffer.
One excerpt per compiler error / test failure linked back to
source. Live-updating as the build emits.
- Extra deps: process-launcher subsystem.
- User surface: `:cargo build`, `:cargo test`, `:cargo clippy`
  open the result-multibuffer; live updates as cargo runs.
- Edit propagation: standard.
- Compose: largely redundant with A.6 (quickfix); ship A.16
  only if cargo-specific decorations (test status icons,
  timing) add enough value over the generic quickfix shape.

**A.17 `NotebookProvider`** — Markdown cells + code cells
from N source files composed into a "notebook" view. Edit
cells in-place; save propagates. Lattice's design includes
notebook support per `design.md` §5.9; this is the natural
composition primitive.
- Extra deps: rich-buffer rendering primitives (post-v1).
- User surface: `:notebook <glob>` opens.

**A.18 `ReplTranscriptProvider`** — REPL session transcript
as a multibuffer. Each input/output pair an excerpt;
"edit and re-run from here" is the natural interaction.
- Extra deps: REPL subsystem (post-v1).

**A.19 `HelpCrossReferenceProvider`** — `:help <topic>` opens
a multibuffer with the relevant `:help` sections side-by-
side. Browse related help topics without losing context.
- Extra deps: `:help` subsystem (already in lattice).
- User surface: `:help-cross <topic1> [<topic2> ...]`.

**A.20 `ProjectConfigProvider`** — `.gitignore` +
`.editorconfig` + `Cargo.toml` (or a config-set per project
type) composed for cross-file consistency edits. "Show me all
my config in one view."
- Extra deps: none.
- User surface: `:project-config`; configurable file glob via
  `multibuffer.project_config_globs`.

**A.21 `LogTailProvider`** — `tail -F` over N log files
composed and live-updating, with timestamp-decorated headers.
Useful for distributed-system debugging when correlated
events span multiple log files.
- Extra deps: file-watcher subsystem (already in lattice).
- User surface: `:tail <file> [<file> ...]`.

### Notes on sequencing across priorities

- **A.5 NarrowProvider** lands as N.1 — single-source
  primitive, not a `MultibufferProvider`. Independent path.
- **A.1 / A.6 / A.4 / A.3** are the four most-touched
  workflows; ordering between them is preference-driven (any
  order works). Recommend: **A.6 first** (the most general —
  unlocks A.3 / A.4 / A.10 / A.11 / A.12 / A.16 to optionally
  route through quickfix grammar).
- **A.2 / A.7 / A.8** unlock magit and AI-driven workflows;
  they're prerequisites for surfaces lattice's roadmap is
  betting on but have plugin / subsystem dependencies that
  push them past v1.0.
- **C-tier** entries are deliberately under-specified — pick
  up when a user need surfaces or when they enable a
  follow-on subsystem.
- Per-PR scope discipline: one A.x slice = one
  `MultibufferProvider` impl + one ex-command + tests. Sibling
  affordances (e.g., quickfix navigation) that get reused
  across slices land in their own slice the first time two
  consumers need them, not pre-emptively.
