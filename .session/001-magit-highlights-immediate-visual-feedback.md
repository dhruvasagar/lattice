# Session: Magit highlights + immediate visual feedback

## Problem
Magit status buffer syntax highlighting (applied as `ExtraHighlights` BufferLocal via `format_buffer_styled()`) only appeared on keystroke because `apply_edit_batch` through the Document actor doesn't fire `async_landed`. Every magit action (stage/unstage/discard/toggle-diff) produced stale highlights until the user pressed another key.

## Solution
Introduced `PendingSyntheticHighlights` service — a shared map + waker that decouples async refresh tasks (run on `spawn_blocking`) from the Editor's tick drain.

### Architecture
1. **`lattice-mode/src/pending_synthetic_highlights.rs`**: New module defining `PendingSyntheticHighlights` (map of `BufferId → Vec<Vec<StyledSpan>>` + waker `Arc<tokio::sync::Notify>`). `store_and_wake()` inserts spans and fires the waker. Re-exported through `lattice_mode` so both `lattice-host` and `lattice-magit` use the same type.

2. **`lattice-host/src/editor_boot.rs`**: Register service with `register_service::<PendingSyntheticHighlights>(...)`. After Editor construction, wire `async_landed` clone into the waker slot: `pending.waker.lock() = Some(editor.async_landed.clone())`.

3. **`lattice-host/src/dispatch.rs`**: `drain_pending_synthetic_highlights()` reads the map from the service, inserts each entry as `ExtraHighlights` BufferLocal. Called from `run_tick_pending()` — runs on every tick regardless of keystroke.

4. **`lattice-magit/src/refresh.rs`**: `refresh_and_apply()` now accepts `Option<PendingSyntheticHighlightsHandle>` and `BufferId`. After applying the buffer edit, calls `ph.store_and_wake(buffer_id, spans)`. New `build_status_styled()` returns `(String, Vec<Vec<StyledSpan>>)` instead of just text.

5. **`lattice-magit/src/actions.rs`**: Added `pending_highlights: Option<Arc<PendingSyntheticHighlights>>` to `StatusBufferState`. All mutation handlers (stage/unstage/discard/stage-patch/refresh/toggle-diff) now call `trigger_refresh()` helper that spawns a blocking refresh with the handle.

6. **`lattice-magit/src/magit_status_mode.rs`**: `on_activate` looks up `PendingSyntheticHighlights` from context services and passes it to both the initial refresh and the guard state.

### Key insight about ServiceRegistry
`ServiceRegistry::get::<T>()` already returns `Option<Arc<T>>` — it wraps the stored value in `Arc`. So you register with `register::<T>(value)` where `value: T`, and look up with `get::<T>()` which returns `Option<Arc<T>>`. A separate `XxxHandle = Arc<X>` type alias is NOT needed for service lookup (it creates double-Arc). The `Handle` aliases in the codebase (`BufferStoreHandle`, `ActionHandlerRegistryHandle`) exist because those types are `Arc<Mutex<...>>` internally — the handle IS the shared ownership type.

### Files changed
- `crates/lattice-mode/src/pending_synthetic_highlights.rs` (NEW)
- `crates/lattice-mode/src/lib.rs` (module decl + re-export)
- `crates/lattice-host/src/modes.rs` (removed local definition, now imports from lattice-mode)
- `crates/lattice-host/src/dispatch.rs` (drain method + call in run_tick_pending)
- `crates/lattice-host/src/editor_boot.rs` (service registration + waker wiring)
- `crates/lattice-magit/src/refresh.rs` (styled build, handle params, store_and_wake)
- `crates/lattice-magit/src/actions.rs` (trigger_refresh helper, modified all handlers)
- `crates/lattice-magit/src/magit_status_mode.rs` (service lookup, pass to refresh/guard)
