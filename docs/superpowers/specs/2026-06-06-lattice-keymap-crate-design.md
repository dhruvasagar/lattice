# Design: `lattice-keymap` crate + keymap resolution overhaul

**Date:** 2026-06-06  
**Status:** Approved — pending implementation  
**Approach:** Approach 3 (crate-first, features-second)

---

## Problem statement

Keymap logic is currently scattered across at least six files in two crates
(`lattice-mode` and `lattice-host`), creating three concrete problems:

1. **Wrong authoritative source for `:describe-key`.** `build_describe_key_content`
   queries the static `keymap_entry!` catalog *and* the runtime registry separately.
   Mode-contributed bindings (rust-mode's `gd`, completion-mode's `<C-n>`) appear in
   the runtime registry but may not match or appear in the catalog, producing stale or
   silent mismatches.

2. **No layer-resolution trace.** `:describe-key` shows "what wins" but not *why* —
   which layers competed, which was overridden, what source line registered it. This
   makes diagnosing binding conflicts opaque.

3. **Wrong crate home.** `lattice-mode`, `lattice-oil`, `lattice-lsp`, and eventually
   WASM plugins all need to declare keymaps. With logic split between `lattice-mode`
   and `lattice-host`, adding a new mode crate today requires depending on `lattice-host`
   just to write a keymap — a layering violation.

---

## Decisions made during design

| Question | Decision |
|---|---|
| Crate home | New `lattice-keymap` crate (option A) |
| `describe-key` output shape | Sparse trace — only layers with a binding, lowest→highest, winner marked (option B) |
| Binding mode coverage | All modes shown by default; mode-prefix targets one |
| Mode-prefix syntax | Vim-style: `n_`, `i_`, `v_`, `r_`, `c_`, `s_` (option A) |
| Implementation order | Crate extraction first (pure refactor), features second |

---

## Section 1: `lattice-keymap` crate structure

### Dependency graph (after migration)

```
lattice-protocol
      ↓
lattice-keymap              ← single home for all keymap logic
    ↙              ↘
lattice-mode       lattice-host
    ↓                   ↓
lattice-oil         lattice-cli
lattice-lsp
(future plugins)
```

`lattice-keymap` depends on: `lattice-protocol`, `arc-swap`, `parking_lot`.  
Neither `lattice-mode` nor `lattice-host` imports keymap types from each other.  
Future plugin host crate can depend on `lattice-keymap` without pulling in `lattice-host`.

### Module layout

```
crates/lattice-keymap/src/
  lib.rs              — flat public re-exports
  binding_mode.rs     — BindingMode enum (Normal / Insert / Visual / Replace / Command / Search / …)
  layer.rs            — KeymapLayer, BoundCommand, SourceLocation
  trie.rs             — KeymapTrie, LookupResult  (internal resolution engine)
  entry.rs            — KeymapEntry, keymap_entry! macro, default_keymap()
  contribution.rs     — Keymap, KeymapBinding  (declaration surface for modes + plugins)
  registry.rs         — KeymapRegistry, KeymapHandle, KeymapCapability
  resolution.rs       — KeymapResolution, LayerHit  (NEW — Slice 2)
  mode_contributions.rs — translate_mode_keymaps()  (host translation pass)
```

### Migration map

| Current file | Destination |
|---|---|
| `lattice-mode/src/keymap_entry.rs` | `lattice-keymap/src/entry.rs` |
| `lattice-mode/src/contributions.rs` | `lattice-keymap/src/contribution.rs` |
| `lattice-host/src/keymap_trie.rs` | `lattice-keymap/src/trie.rs` |
| `lattice-host/src/keymap_registry.rs` | `lattice-keymap/src/registry.rs` |
| `lattice-host/src/keymap_mode_contributions.rs` | `lattice-keymap/src/mode_contributions.rs` |
| `lattice-host/src/keymap.rs` | deleted (re-export from `lattice-keymap` directly) |

---

## Section 2: `KeymapResolution` API

### New types (`resolution.rs`)

```rust
/// One layer's contribution to a chord lookup.
/// Only present if that layer has a binding (sparse).
pub struct LayerHit {
    pub layer: KeymapLayer,    // Builtin / MajorMode(id) / MinorMode(id) / User / Buffer
    pub label: String,         // "Built-in", "rust-mode", "lsp-mode", …
    pub command: BoundCommand, // CommandInvocation + SourceLocation
    pub wins: bool,            // true for the highest-priority hit
}

/// Full resolution trace for one (chord sequence, binding mode) pair.
/// hits is ordered lowest priority → highest.
/// Only layers that have a binding appear (empty = completely unbound).
pub struct KeymapResolution {
    pub binding_mode: BindingMode,
    pub hits: Vec<LayerHit>,
    pub winner: Option<BoundCommand>,
}
```

### New methods on `KeymapHandle`

```rust
impl KeymapHandle {
    /// Trace resolution for one specific binding mode.
    /// `active_modes` is the per-buffer activation list from `Editor::active_modes(buf)`.
    pub fn resolve_trace(
        &self,
        binding_mode: BindingMode,
        chords: &[KeyChord],
        active_modes: &ActiveModes,   // existing type in lattice-host::editor
    ) -> KeymapResolution;

    /// Trace all binding modes at once.
    /// Modes with no hits at any layer are omitted from the result.
    pub fn resolve_trace_all_modes(
        &self,
        chords: &[KeyChord],
        active_modes: &ActiveModes,   // existing type in lattice-host::editor
    ) -> Vec<KeymapResolution>;
}
```

`resolve_trace` walks each registered layer in priority order (Builtin → MajorMode →
MinorMode(s) in activation order → User → Buffer), queries the per-layer trie, and
collects hits. Read-only; zero impact on the hot keystroke path.

---

## Section 3: `:describe-key` rebuild

### Mode-prefix parsing

| Prefix | BindingMode |
|--------|-------------|
| `n_`   | Normal |
| `i_`   | Insert |
| `v_`   | Visual |
| `x_`   | Visual (alias) |
| `r_`   | Replace |
| `c_`   | Command |
| `s_`   | Search |

Parse rule: if the `:describe-key` arg begins with a known two-char prefix `{letter}_`,
strip it and call `resolve_trace(mode, …)`. Otherwise call `resolve_trace_all_modes(…)`.
`<Tab>` completion surfaces the prefix table.

### Output format (sparse trace, B)

**`:describe-key j`** (all modes):
```
j

Normal     [Builtin]   motion:line-down           keymap_normal.rs:42
Visual     [Builtin]   motion:line-down           keymap_normal.rs:42

(Insert, Replace, Command, Search: unbound — omitted)
```

**`:describe-key n_<C-n>`** with `completion-mode` active:
```
n_<C-n>  (Normal mode)

  Builtin          motion:next-search-result    keymap_normal.rs:118
  completion-mode   completion:select-next       lattice-completion/src/mode.rs:34   ← wins
```

**`:describe-key n_gd`** in a Rust buffer:
```
n_gd  (Normal mode)

  rust-mode  lsp:goto-definition    lattice-lsp/src/rust_mode.rs:88   ← wins
```

**`:describe-key n_z`** (unbound everywhere):
```
n_z  (Normal mode)

  (unbound)
```

### Static catalog fate

The `keymap_entry!` static catalog (`default_keymap()`) is **removed from `:describe-key`**.
Runtime registry is the sole authoritative source.

The catalog is retained in `entry.rs` for:
- `:keymap` listing (all known bindings with docstrings)
- The drift test (validates every catalog entry produces a non-None Action)
- Future help topic cross-links (`n_j` appears in help as a clickable link)

---

## Slicing plan

### Slice 1 — Pure migration (zero behavior change)

**Goal:** `lattice-keymap` crate exists; all tests pass; no API change, only import paths.

Steps:
1. Create `crates/lattice-keymap/Cargo.toml`; add to workspace `Cargo.toml`
2. Move the six file groups per migration map above; fix `mod` declarations + `use` paths
3. Add `lattice-keymap` as a dep in `lattice-mode`, `lattice-host`, `lattice-oil`, `lattice-lsp`
4. Remove keymap-related exports from `lattice-mode/src/lib.rs`
5. Fix two live diagnostics bundled here (pure correctness, zero-behavior-change):
   - `dispatch.rs:21845` — `MajorMode(_)` tuple pattern in `friendly_layer_label`
   - `subsystem.rs:2657` — rename `current` → `_current`
6. Delete `lattice-host/src/keymap.rs` (re-export shim, now redundant)
7. `cargo test --workspace` green
8. `cargo clippy --workspace` clean

**Commit message:** `feat(keymap): extract lattice-keymap crate — pure migration, zero behavior change`

**Impact surface:** import paths in ~6 crates; no public API changes; all existing tests compile unchanged.

---

### Slice 2 — `KeymapResolution` API + `describe-key` rebuild

**Goal:** `:describe-key` shows the full layer trace from the runtime registry; mode-prefix targeting works.

Steps:
1. Add `resolution.rs` to `lattice-keymap` with `LayerHit` + `KeymapResolution`
2. Implement `KeymapHandle::resolve_trace` + `resolve_trace_all_modes`
3. Add mode-prefix parser (`parse_describe_key_arg(s) -> (Option<BindingMode>, chord_str)`)
4. Rewrite `build_describe_key_content` in `lattice-host/src/dispatch.rs`:
   - Use `resolve_trace` / `resolve_trace_all_modes` (runtime registry only)
   - Remove `crate::keymap::lookup(chord)` static-catalog path
   - Remove K.1.d "runtime-registry section" (superseded by the trace)
   - Render sparse-trace output per approved format
5. Wire `<Tab>` completion on `:describe-key` to surface mode prefix options
6. Add unit tests for `resolve_trace` (builtin-only, major-override, minor-override, unbound)
7. Add unit tests for mode-prefix parser
8. `cargo test --workspace` green

**Commit message:** `feat(keymap): KeymapResolution layer-trace API + describe-key rebuild (K.3.1)`

**Impact surface:** new public types in `lattice-keymap`; `build_describe_key_content` rewritten; all existing describe-key tests updated to match new output format.

---

## Testing plan

| Test | Location | Slice |
|---|---|---|
| All existing keymap tests compile + pass after path changes | existing test files | 1 |
| `MajorMode(_)` pattern compiles | `dispatch.rs` | 1 |
| `resolve_trace` returns empty for unbound chord | `resolution.rs` unit test | 2 |
| `resolve_trace` builtin-only: one hit, wins=true | `resolution.rs` unit test | 2 |
| `resolve_trace` major-mode override: two hits, MajorMode wins | `resolution.rs` unit test | 2 |
| `resolve_trace` minor-mode override: minor wins over builtin | `resolution.rs` unit test | 2 |
| `parse_describe_key_arg("n_j")` → `(Some(Normal), "j")` | `resolution.rs` unit test | 2 |
| `parse_describe_key_arg("j")` → `(None, "j")` | `resolution.rs` unit test | 2 |
| `build_describe_key_content` renders sparse trace | `dispatch.rs` integration test | 2 |
| `build_describe_key_content` renders "(unbound)" for unknown chord | `dispatch.rs` integration test | 2 |

---

## Open questions / deferred

- **Partial chord tracing:** If `:describe-key g` is given and `g` is only a prefix (leading
  to `gd`, `gg`, `G`, etc.), should the output list all chords reachable under that prefix?
  Deferred — the current spec only handles complete chord sequences. A follow-on
  `:describe-prefix` command can address this.
- **Plugin-contributed keymaps:** When the WASM plugin host lands (Phase 7), plugin layers
  will appear in `resolve_trace` automatically since they register via `KeymapHandle` —
  no API change needed. The `KeymapCapability` system already scopes plugin writes.
- **`KeymapLayer` ordering for multiple active minors:** The current K.1.c spec says
  "last-activated wins". This is preserved in Slice 2. A future `:describe-modes` command
  showing activation order would make this visible to the user.
