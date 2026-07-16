+++
title = "Headerline"
+++

# Headerline

Authoritative design for Lattice's generic sticky headerline mechanism:
the one surface for a buffer to emit a pinned status row above line 0,
the trait-based extensibility model that lets modes and subsystems own
their header rendering, and the cheap-clone handle pattern that avoids
state duplication.

This document is a *companion* to `design.md` (§5.6 rendering, §5.15
virtual rows), to `virtual-rows.md` (the underlying sticky-row
primitive), and to `multibuffer-views.md` (§6 first production consumer).

## 1. The design goal

A buffer's header is a read-only sticky row anchored above line 0,
pinned to the viewport. Many subsystems produce content for this row:

- **Tutor mode** — lesson name, score, timer.
- **Multibuffer search** — scan progress ("searching 42 files..."),
  completion status.
- **LSP session** — "Initializing..." / "Ready" / "Error" status.
- **VCS** — current branch, dirty state.
- **Diagnostics** — summary count or worst severity.

Each producer is independent. Each owns its state and render logic.
The substrate must provide one unified surface that lets modes
register their header without baking producer-specific logic into the
host.

Two saved invariants frame the design:

> Don't add features beyond what the task requires.
> -- `CLAUDE.md`, design discipline rule

> Modes own their full surface. -- saved feedback
> `feedback_mode_owns_its_surface.md`

The first rules out a fixed set of ex-commands for each producer
(`:tutor-header`, `:lsp-header`, `:search-header`). The second rules
out a central `Editor::update_headerline_for_<mode>` dispatcher. Both
push the same way: one generic mechanism, many producers, uniform
render surface.

## 2. The shape

```
            ┌─────────────────────────────┐
            │    Headerline (trait)       │
            │                             │
            │  version() -> u64           │
            │  render() -> Option<Row>    │
            └────────┬────────────────────┘
                     │
        ┌────────────┴──────────────┐
        │                           │
        ▼                           ▼
┌──────────────────────┐  ┌─────────────────────┐
│SimpleHeaderline<S>   │  │Direct Headerline    │
│ (owned state)        │  │impl (existing state)│
└──────────────────────┘  └─────────────────────┘
        │                           │
        ▼                           ▼
┌──────────────────────┐  ┌─────────────────────┐
│SimpleHeaderlineHandle│  │Arc<dyn Headerline>  │
│ (cheap-clone)        │  │(mode-provided)      │
└──────────────────────┘  └─────────────────────┘
        │                           │
        └────────────┬──────────────┘
                     │
                     ▼
            ┌─────────────────────────────┐
            │  HeaderlineProvider         │
            │  (VirtualRowProvider impl)  │
            │                             │
            │  emits one VirtualRow       │
            │  at anchor_line=0, Above    │
            └─────────────────────────────┘
```

The trait is the load-bearing abstraction. Two paths feed it:

1. **`SimpleHeaderlineHandle<S>`** — for modes that own dedicated
   header state (tutor, multibuffer search). Zero boilerplate: the
   handle wraps the state, stores a renderer closure, and bumps
   version on update.

2. **Direct `Headerline` impl** — for modes whose header is a view
   into existing state (LSP session, VCS context) or for WASM plugin
   shims that wrap a WIT callback. Avoids state duplication.

`HeaderlineProvider` wraps any `Headerline` impl and registers as a
`VirtualRowProvider` (the underlying sticky-row primitive from
`virtual-rows.md`). It always emits one row anchored at line 0 above;
returns empty vec when the impl returns `None` (hide the row).

## 3. The data model

### 3.1 `HeaderlineRow`

```rust
pub struct HeaderlineRow {
	pub cells: Arc<[Cell]>,
	pub bg: Option<u32>,
}
```

`cells` is the row content — the same `Arc<[Cell]>` shape that backs
`CellRow`, so renderers paint headerlines through the existing fast
path. `bg: None` means the renderer picks the theme-defined sticky-row
background; `bg: Some(0xRRGGBB)` overrides it (tutor's retro palette
does this).

### 3.2 `Headerline` trait

```rust
pub trait Headerline: Send + Sync + 'static {
	fn version(&self) -> u64;
	fn render(&self) -> Option<HeaderlineRow>;
}
```

`version()` is a monotonic counter. The cells worker calls it on every
tick; when the version has advanced since the last call, it invokes
`render()` to rebuild the row. `render()` returns `None` to hide the
row entirely.

`render()` must not block — cache results; background tasks push
updates via the owning handle. The cells worker publishes the result
through the `VirtualRowMatrix` path, not a separate channel.

### 3.3 `SimpleHeaderlineHandle<S>` (convenience path)

```rust
pub struct SimpleHeaderlineHandle<S: Send + Sync + 'static>(
	Arc<SimpleHeaderline<S>>
);

impl<S: Send + Sync + 'static> SimpleHeaderlineHandle<S> {
	pub fn new(
		initial: S,
		renderer: impl Fn(&S) -> Option<HeaderlineRow> + Send + Sync + 'static,
	) -> Self;

	pub fn update(&self, f: impl FnOnce(&mut S));

	pub fn version(&self) -> u64;

	pub fn provider(&self, provider_id: ProviderId) -> HeaderlineProvider;
}
```

The handle is cheap-clone (manual `Clone` impl, only the `Arc` is
cloned, so `S` does not need to be `Clone`). It owns the state in an
`Arc<RwLock<S>>`, the renderer as an `Arc<dyn Fn>` closure, and the
version as an `AtomicU64`.

`update(f)` acquires the write lock, calls the closure to mutate
state, and bumps the version with `Acquire`/`Release` ordering. The
next `render()` call sees the new state. Updates are immediate; no
batching or debouncing at this layer.

`provider(provider_id)` returns a `HeaderlineProvider` that can be
registered with `register_virtual_row_provider`. The provider holds an
`Arc<dyn Headerline>` to the same underlying state, so updates via the
handle are immediately visible to the provider's `collect()` call.

### 3.4 `HeaderlineProvider` (substrate adapter)

```rust
pub struct HeaderlineProvider {
	provider_id: ProviderId,
	inner: Arc<dyn Headerline>,
}

impl VirtualRowProvider for HeaderlineProvider {
	fn id(&self) -> ProviderId;
	fn version(&self) -> u64;
	fn collect(&self) -> Vec<VirtualRow>;
}
```

Wraps any `Headerline` impl (either `SimpleHeaderline<S>` via the
handle, or a direct impl). `collect()` calls `inner.render()`; when it
returns `Some(row)`, emits one `VirtualRow` at `anchor_line=0,
position=Above, kind=Sticky, height=1`. When it returns `None`,
returns an empty vec.

The provider can be constructed either from a `SimpleHeaderlineHandle`
(via `.provider(id)`) or directly from an `Arc<dyn Headerline>` (via
`new(id, inner)`).

## 4. Two paths: when to use which

### Path 1: `SimpleHeaderlineHandle<S>` (owned state)

Use this when the mode has **dedicated header state** that doesn't
live elsewhere. Examples:

- **Tutor mode** — `TutorViewState` with lesson info, score, timer.
- **Multibuffer search** — `HeaderlineStatus` with file count, match
  count, scan progress.

Pattern:

```rust
// In mode boot:
let handle = SimpleHeaderlineHandle::new(
	TutorViewState::default(),
	render_tutor_headerline,  // closure in the mode
);
let provider = handle.provider(PROVIDER_ID);
register_virtual_row_provider(provider);

// Store the handle in buffer-local state so dispatch can update it:
buffer.set_headerline_handle(handle.clone());

// Later, on state change:
if let Some(handle) = buffer.get_headerline_handle::<TutorViewState>() {
	handle.update(|s| {
		s.score += 10;
		s.timer = elapsed;
	});
}
```

Benefit: no manual version management; the handle owns versioning.

### Path 2: Direct `Headerline` impl (existing state)

Use this when the header is a **view into existing state**, or when
the producer is a WASM plugin. Examples:

- **LSP mode** — the session struct carries ready/error/initializing
  flags. Implement `Headerline` directly on the session.
- **Plugin provider** — the plugin's WIT result is wrapped in a
  thin `Headerline` shim that calls the plugin's render export.

Pattern:

```rust
// In LSP session struct:
impl Headerline for LspSession {
	fn version(&self) -> u64 {
		self.state_version  // bump this when state changes
	}

	fn render(&self) -> Option<HeaderlineRow> {
		match self.state {
			LspState::Ready => Some(render_ready_status()),
			LspState::Initializing => Some(render_initializing()),
			LspState::Error(ref msg) => Some(render_error(msg)),
		}
	}
}

// At registration:
let provider = HeaderlineProvider::new(
	PROVIDER_ID,
	Arc::new(session) as Arc<dyn Headerline>,
);
register_virtual_row_provider(provider);
```

Benefit: no state duplication; the session is the source of truth.

## 5. Paramount-goal alignment

- **Goal #1 (performance):** `update()` is an `Arc<RwLock<S>>` write
  + `AtomicU64` bump — trivially cheap O(1). `collect()` is one
  read-lock + one `VirtualRow` build — O(1). The cells worker calls
  `version()` every tick; `render()` is called only when version
  advances. Sub-frame cost across the hot path.

- **Goal #2 (extensibility):** LSP, diagnostics, VCS, and WASM
  plugins all adopt the `Headerline` trait without coupling to a
  specific state shape. New producers add a `Headerline` impl in their
  own crate; zero host changes.

- **Goal #3 (vim semantics):** the render closure (or impl) is
  mode-owned. The substrate owns only the sticky-row mechanics. Modes
  control when and how the header updates.

- **Goal #4 (asynchronicity):** header state lives in an
  `Arc<RwLock<S>>` that can be mutated from any thread. The cells
  worker reads it on its own tick without blocking dispatch. Async
  producers cache results and push updates via `handle.update(f)` from
  a background task.

## 6. H1: Tutor validation case

`TutorHeaderlineProvider` (a bespoke `VirtualRowProvider` with
`Arc<Mutex<TutorViewState>>`) was replaced by:

1. **`render_tutor_headerline(s: &TutorViewState) -> Option<HeaderlineRow>`**
   — the mode's render function, lives in `lattice-host/src/tutor/mode.rs`.
   Pure function of the state.

2. **`SimpleHeaderlineHandle<TutorViewState>`** stored as
   `buffer.set_local("headerline", handle)`.
   Boot code creates the handle with `SimpleHeaderlineHandle::new(initial_state,
   render_tutor_headerline)`.

3. **Registration** — `handle.provider(provider_id)` registered as
   the `VirtualRowProvider`. The provider holds an `Arc<dyn Headerline>`
   to the same underlying state.

4. **Update calls** — `tutor_update_headerline` calls
   `state_handle.update(|s| s.update_for_display(session, kind))`
   instead of a `Mutex::lock`. Atomic version bump happens
   automatically.

5. **Version management** — `TutorViewState` loses its manual
   `version: u64` field. Versioning is now owned by
   `SimpleHeaderline::version: AtomicU64`.

Result: tutor mode is now the canonical consumer of the generic
`Headerline` surface. No substrate-level tutor-specific knowledge.

## 7. Future consumers

- **M.6.5 (multibuffer search status)** — `MultibufferStatusProvider`
  becomes `SimpleHeaderlineHandle<HeaderlineStatus>` with fields like
  `scanned_count`, `total_count`, `match_count`. Render closure
  formats these into a status row.

- **LSP session** — direct `Headerline` impl on the session struct,
  no handle needed. Session already owns initialization state + error
  messages; just expose them via `render()`.

- **VCS branch / diagnostics summary** — `SimpleHeaderlineHandle<T>`
  or direct impl, depending on whether state is owned locally or
  shared with another consumer.

All producers use the same surface; the substrate never special-cases
them.

## 8. Integration with virtual rows

`HeaderlineProvider` is a `VirtualRowProvider` (from `virtual-rows.md`
§3.3). It plugs into the virtual-rows worker like any other provider:

1. Boot registers `provider` via `register_virtual_row_provider()`.
2. Cells worker ticks, calls `provider.version()` to check for
   changes.
3. If version advanced, calls `provider.collect()`.
4. Provider calls `inner.render()`, wraps the result in a
   `VirtualRow` at `anchor_line=0, position=Above, kind=Sticky`.
5. Worker merges all registered providers' outputs into
   `VirtualRowMatrix`, publishes via `ArcSwap`.
6. Renderer reads `VirtualRowMatrix`, interleaves virtual rows into
   the display stream via `display_slice()`.

No special headerline plumbing. It's just another virtual-row producer.

## 9. Open questions

- **Per-buffer headerline masking.** Should a multibuffer view allow
  toggling the headerline off for a specific pane? The current
  primitive is buffer-scoped (one headerline per buffer, all panes
  see it). Per-pane gating is a layered concern — the virtual-rows
  worker can support per-pane provider filtering via a pane-id arg on
  `collect()`. Decide before M.6.5 lands the first multi-pane case
  that wants it.

- **Headerline height > 1.** Reserved for future multi-line headers
  (e.g. a two-line status row). Currently `height: 1` always. Tests
  and first consumers stay on `height = 1`. Multi-line consumers
  exercise it when they land.

- **Dynamic provider registration.** Today a mode registers its
  headerline at boot. If a future mode wants to dynamically show/hide
  the header based on user interaction (toggle the `*messages*`
  headerline on `:messages` open/close), the provider registration
  itself needs a lifecycle hook. Deferred until a real consumer asks.

## 10. Slice plan

Sequencing lives inline in
[`docs/dev/operations/slice-plans/multibuffer-views.md`](../operations/slice-plans/multibuffer-views)
(M.6.5 section); authoritative status per slice lives in
[`docs/dev/operations/implementation.md`](../operations/implementation).
This fragment owns *what* and *why*; the slice plan owns *when* and
*in what order*.

H0 (trait + `SimpleHeaderlineHandle`) and H1 (tutor migration)
shipped together as one coordinated slice. H0 validated the
abstraction; H1 proved it on a real consumer.

## 11. Testing strategy

- **Unit tests** (in `lattice-cells/src/headerline.rs`): `Headerline`
  trait methods callable; `SimpleHeaderlineHandle::new` constructs; `update`
  bumps version; `render()` returns the renderer's result; `provider()`
  returns a `VirtualRowProvider` that emits one row when render is
  `Some`, empty vec when `None`; version tracking across updates; manual
  `Headerline` impl works; `HeaderlineProvider::new` wraps any impl.

- **Tutor integration** (H1 validation): mode boots with headerline;
  tutor mode transitions bump the version; renderer calls `collect()`,
  gets a row with correct state; `:quit` on the tutor buffer cleans
  up the provider.

- **Future** (M.6.5 multibuffer): search scan pushes updates via
  `handle.update()`; headerline reflects match count in real time;
  scan completion hides the header (returns `None`).

## 12. Risks

- **Version-check busy loop.** The cells worker calls `version()` on
  every tick (even when the buffer is idle). If a producer's
  `version()` call is expensive, it costs the frame. Mitigation:
  `version()` is a simple atomic read; the renderer's per-frame cost
  is below 1µs for a headerline check. Bench gate (recorded) catches
  outliers.

- **State-lock contention.** If a producer updates state frequently
  from a background task while the renderer is reading `state` via
  the read-lock, update calls block. Mitigation: updates are `O(1)`
  mutations + atomic bump; read-lock is held only during `render()`.
  Contention is rare in practice (update happens on state change, not
  per-frame).

- **Provider registration leaks.** A mode that registers a headerline
  but never deregisters leaves a stale provider. Mitigation: modes
  register headerlines at boot (once, per buffer opening). Explicit
  deregister is called when the buffer closes or the provider
  explicitly unsubscribes. The buffer-local storage pattern makes
  this explicit.

## 13. Cross-references

- `crates/lattice-cells/src/headerline.rs` — implementation of trait,
  `SimpleHeaderline`, `SimpleHeaderlineHandle`, `HeaderlineProvider`.
- `crates/lattice-cells/src/virtual_rows.rs` — `VirtualRowProvider`
  trait that `HeaderlineProvider` implements.
- `crates/lattice-host/src/tutor/mode.rs` — `render_tutor_headerline`,
  `TutorHeaderlineState`, how the mode creates and updates the handle.
- `docs/dev/architecture/virtual-rows.md` — the underlying sticky-row
  primitive and the virtual-rows worker that calls `collect()`.
- `docs/dev/operations/slice-plans/multibuffer-views.md` § M.6.5 —
  multibuffer search status as the first production post-tutor
  consumer; M.6.5 owns sequencing for the search-status transition.
