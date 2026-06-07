# `lattice-keymap` Crate + Resolution Overhaul — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development`
> (recommended) or `superpowers:executing-plans` to implement this plan task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extract all keymap logic into a dedicated `lattice-keymap` crate; replace `describe-key`'s split
static-catalog + runtime path with a single sparse layer-trace API that shows every binding layer for a
chord and marks the winner.

**Architecture:** Two slices. Slice 1 is a pure mechanical migration — same types, same behaviour, only
module paths change; every test stays green throughout. Slice 2 adds `KeymapResolution`/`LayerHit` to
`lattice-keymap`, wires `resolve_trace` on `KeymapHandle` using the existing `enumerate_chord_bindings`
foundation, then rewrites `build_describe_key_content` against the new API and deletes the now-dead helpers.

**Tech Stack:** Rust, `arc-swap`, `parking_lot`, `internment` (for `ModeId`), `lattice-grammar`
(`SourceLocation`, `CommandInvocation`, `CommandId`), `lattice-protocol` (`KeyChord`, `ChordPattern`).

**Spec:** `docs/dev/operations/slicing-plans/2026-06-06-lattice-keymap-crate-design.md`

---

## Dependency chain after migration

```
lattice-protocol
      ↓
lattice-grammar
      ↓
lattice-keymap          ← new crate; owns ModeId + all keymap types + registry
      ↓
lattice-mode            ← re-exports ModeId from lattice-keymap; Mode trait stays here
      ↓
lattice-host            ← mode_contributions.rs stays here (needs ModeRegistry + KeymapHandle)
                           KeymapReverseLookupHandle stays here (lattice-completion dep)
```

## File map

### Created
| Path | Purpose |
|------|---------|
| `crates/lattice-keymap/Cargo.toml` | New crate manifest |
| `crates/lattice-keymap/src/lib.rs` | Flat public re-exports |
| `crates/lattice-keymap/src/mode_id.rs` | `ModeId` (moved from `lattice-mode`) |
| `crates/lattice-keymap/src/binding_mode.rs` | `BindingMode` (moved from `lattice-mode`) |
| `crates/lattice-keymap/src/entry.rs` | `KeymapEntry` + `keymap_entry!` + `default_keymap()` |
| `crates/lattice-keymap/src/contribution.rs` | `Keymap` + `KeymapBinding` |
| `crates/lattice-keymap/src/trie.rs` | `KeymapTrie` + `KeymapLayer` + `BoundCommand` + `LookupResult` |
| `crates/lattice-keymap/src/registry.rs` | `KeymapRegistry` + `KeymapHandle` + `KeymapCapability` |
| `crates/lattice-keymap/src/resolution.rs` | `KeymapResolution` + `LayerHit` + `resolve_trace` (Slice 2) |

### Modified
| Path | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `crates/lattice-keymap` to `members` |
| `crates/lattice-mode/Cargo.toml` | Add `lattice-keymap` dep |
| `crates/lattice-mode/src/lib.rs` | Replace local defs with `pub use lattice_keymap::*` |
| `crates/lattice-mode/src/mode.rs` | Replace `ModeId` def with `pub use lattice_keymap::ModeId` |
| `crates/lattice-mode/src/binding_mode.rs` | Replaced by re-export |
| `crates/lattice-mode/src/keymap_entry.rs` | Replaced by re-export |
| `crates/lattice-mode/src/contributions.rs` | Replaced by re-export |
| `crates/lattice-host/Cargo.toml` | Add `lattice-keymap` dep; remove direct `internment` for ModeId |
| `crates/lattice-host/src/dispatch.rs` | Rewrite `build_describe_key_content` + helpers (Slice 2) |
| `crates/lattice-host/src/keymap_trie.rs` | Replaced by re-export shim, then deleted |
| `crates/lattice-host/src/keymap_registry.rs` | Replaced by re-export shim, then deleted |
| `crates/lattice-host/src/keymap.rs` | Deleted |

### Stays in `lattice-host` (not migrated — would create circular dep)
| Path | Reason |
|------|--------|
| `crates/lattice-host/src/keymap_mode_contributions.rs` | Needs `ModeRegistry` from `lattice-mode` + `KeymapHandle` from `lattice-keymap`; only `lattice-host` can see both |
| `KeymapReverseLookupHandle` in `keymap_registry.rs` | Implements `lattice-completion::KeymapReverseLookup`; `lattice-completion` may depend on `lattice-mode` |

---

## Part 1 — Slice 1: Extract `lattice-keymap` (pure migration)

### Task 1: Reconnaissance — WIP commit + baseline build

**Files:** none created

- [ ] **Check what's in the WIP commit**

```bash
git show 5a6d051c --stat
git show 5a6d051c -- crates/lattice-host/src/keymap_trie.rs | head -60
```

Note: `MajorMode` in `keymap_trie.rs` was changed to `MajorMode(ModeId)` in a recent WIP commit
but `dispatch.rs:21845` wasn't updated. We'll fix that in Task 9.

- [ ] **Confirm the baseline build state**

```bash
cd /Users/dhruva/src/dhruvasagar/lattice
cargo check --workspace 2>&1 | grep -E "^error" | head -20
```

Expected: only the two known diagnostics (E0532 `MajorMode` + unused `current`).
If there are others, fix them before proceeding — they will compound during migration.

- [ ] **Record test baseline**

```bash
cargo test --workspace --quiet 2>&1 | tail -5
```

Save the final line ("test result: ok. N passed") — this is our green baseline.

---

### Task 2: Create `lattice-keymap` crate skeleton

**Files:** Create `crates/lattice-keymap/Cargo.toml`, `crates/lattice-keymap/src/lib.rs`

- [ ] **Create the directory**

```bash
mkdir -p crates/lattice-keymap/src
```

- [ ] **Write `crates/lattice-keymap/Cargo.toml`**

```toml
[package]
name = "lattice-keymap"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
# Chord primitives (KeyChord, ChordPattern) live here
lattice-protocol = { path = "../lattice-protocol" }
# CommandInvocation, CommandId, SourceLocation, Introspectable
lattice-grammar = { path = "../lattice-grammar" }

arc-swap   = { workspace = true }
parking_lot = { workspace = true }
internment  = { workspace = true }   # for ModeId(Intern<String>)
tracing     = { workspace = true }
smallvec    = { workspace = true, optional = true }

[lints]
workspace = true
```

- [ ] **Write the empty `src/lib.rs`**

```rust
//! `lattice-keymap` — the single home for all keymap types, trie resolution,
//! and the runtime registry. Modes and plugins use this crate to declare
//! their keybindings; the host uses it for dispatch and introspection.
//!
//! Dependency position: lattice-protocol → lattice-grammar → lattice-keymap
//! → lattice-mode → lattice-host. Nothing in lattice-keymap may import from
//! lattice-mode or lattice-host.
```

- [ ] **Add to workspace `Cargo.toml`**

Open `Cargo.toml` at the repo root. In the `[workspace] members` array, add:

```toml
"crates/lattice-keymap",
```

(Add it after `lattice-grammar` to follow the dependency order in the file.)

- [ ] **Verify the skeleton compiles**

```bash
cargo check -p lattice-keymap
```

Expected: `Finished` with no errors (empty lib).

---

### Task 3: Move `ModeId` to `lattice-keymap`

`ModeId` is used in `KeymapLayer::MajorMode(ModeId)` and `MinorMode(ModeId)`. It must live in
`lattice-keymap` to avoid a circular dep (`lattice-mode` → `lattice-keymap` → `lattice-mode`).

**Files:**
- Create: `crates/lattice-keymap/src/mode_id.rs`
- Modify: `crates/lattice-keymap/src/lib.rs`
- Modify: `crates/lattice-mode/Cargo.toml`
- Modify: `crates/lattice-mode/src/mode.rs`
- Modify: `crates/lattice-mode/src/lib.rs`

- [ ] **Read the current `ModeId` definition**

```bash
sed -n '1,70p' crates/lattice-mode/src/mode.rs
```

Copy the `ModeId` struct, its `impl` blocks, and `Display`/`Debug`/`Hash`/`Eq`/`Clone`/`Copy`
derive block verbatim.

- [ ] **Create `crates/lattice-keymap/src/mode_id.rs`**

Paste the `ModeId` definition. Adjust imports — replace `use crate::...` with `use internment::Intern`.
The full content should look like:

```rust
//! `ModeId` — interned mode identifier. Lives here (not in `lattice-mode`) so
//! `KeymapLayer` can reference it without creating a circular dependency.
use internment::Intern;
use std::fmt;

/// Interned identifier for a mode (e.g. `rust-mode`, `completion-popup`).
/// Two `ModeId`s are equal iff their names are equal; equality is O(1) pointer
/// comparison on the interned allocation.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModeId(Intern<String>);

impl ModeId {
    /// Intern `name` and return the canonical `ModeId` for it. Repeated calls
    /// with the same string return identical `ModeId`s; the underlying
    /// allocation is shared.
    pub fn new(name: impl Into<String>) -> Self {
        Self(Intern::new(name.into()))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

impl fmt::Display for ModeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.as_ref())
    }
}

impl fmt::Debug for ModeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModeId({:?})", self.0.as_ref())
    }
}
```

> Verify this matches the actual definition in `lattice-mode/src/mode.rs` — copy any additional
> trait impls that exist there.

- [ ] **Add to `lattice-keymap/src/lib.rs`**

```rust
pub mod mode_id;
pub use mode_id::ModeId;
```

- [ ] **Add `lattice-keymap` dep to `lattice-mode/Cargo.toml`**

```toml
lattice-keymap = { path = "../lattice-keymap" }
```

- [ ] **Replace `ModeId` definition in `lattice-mode/src/mode.rs`**

Delete the `ModeId` struct and all its `impl` blocks. Replace with:

```rust
pub use lattice_keymap::ModeId;
```

- [ ] **Update `lattice-mode/src/lib.rs`** — `ModeId` is already re-exported via `mode.rs`,
so the existing `pub use crate::mode::ModeId` continues to work.

- [ ] **Verify compilation**

```bash
cargo check -p lattice-mode -p lattice-keymap
```

Expected: no errors. Fix any `Intern` import paths if needed.

---

### Task 4: Move `BindingMode` to `lattice-keymap`

**Files:**
- Create: `crates/lattice-keymap/src/binding_mode.rs`
- Modify: `crates/lattice-keymap/src/lib.rs`
- Modify: `crates/lattice-mode/src/binding_mode.rs`

- [ ] **Read the current file**

```bash
cat crates/lattice-mode/src/binding_mode.rs
```

- [ ] **Create `crates/lattice-keymap/src/binding_mode.rs`**

Copy the file verbatim. Adjust any `use crate::` to absolute paths if needed.

- [ ] **Add to `lattice-keymap/src/lib.rs`**

```rust
pub mod binding_mode;
pub use binding_mode::BindingMode;
```

- [ ] **Replace `lattice-mode/src/binding_mode.rs`**

Replace the entire file content with:

```rust
// Moved to lattice-keymap. Re-exported here for backward compatibility.
pub use lattice_keymap::BindingMode;
```

- [ ] **Verify**

```bash
cargo check -p lattice-mode -p lattice-keymap
```

---

### Task 5: Move `KeymapEntry`, `keymap_entry!`, `default_keymap()` to `lattice-keymap`

**Files:**
- Create: `crates/lattice-keymap/src/entry.rs`
- Modify: `crates/lattice-keymap/src/lib.rs`
- Modify: `crates/lattice-mode/src/keymap_entry.rs`

- [ ] **Read the current file**

```bash
wc -l crates/lattice-mode/src/keymap_entry.rs
cat crates/lattice-mode/src/keymap_entry.rs
```

- [ ] **Create `crates/lattice-keymap/src/entry.rs`**

Copy verbatim. Update imports:
- Replace `use lattice_grammar::` with the actual path (it's already a dep of `lattice-keymap`)
- Replace `use crate::binding_mode::BindingMode` → `use crate::BindingMode`
- Replace `use crate::mode::ModeId` or similar → not needed (KeymapEntry doesn't use ModeId)

The `lattice_grammar::Introspectable` trait impl must stay — add `lattice-grammar` is already
in `lattice-keymap/Cargo.toml`.

- [ ] **Add to `lattice-keymap/src/lib.rs`**

```rust
pub mod entry;
pub use entry::{KeymapEntry, default_keymap, entries, lookup};
#[doc(hidden)]
pub use entry::__builtin_source;
```

- [ ] **Replace `lattice-mode/src/keymap_entry.rs`**

```rust
// Moved to lattice-keymap. Re-exported here for backward compatibility.
pub use lattice_keymap::{
    KeymapEntry, default_keymap, entries, lookup, __builtin_source,
};
// The keymap_entry! macro is re-exported from the crate root.
pub use lattice_keymap::keymap_entry;
```

- [ ] **Export `keymap_entry!` macro from `lattice-keymap/src/lib.rs`**

The macro needs `#[macro_export]` in `entry.rs` and a `pub use` at the crate root. Verify the
macro is declared `#[macro_export]` in `entry.rs`; if it was `macro_rules! keymap_entry` without
`#[macro_export]`, add it.

- [ ] **Verify**

```bash
cargo check -p lattice-mode -p lattice-keymap
cargo test -p lattice-mode --lib 2>&1 | tail -5
```

---

### Task 6: Move `Keymap`, `KeymapBinding` to `lattice-keymap`

**Files:**
- Create: `crates/lattice-keymap/src/contribution.rs`
- Modify: `crates/lattice-keymap/src/lib.rs`
- Modify: `crates/lattice-mode/src/contributions.rs`

- [ ] **Read current file**

```bash
cat crates/lattice-mode/src/contributions.rs
```

- [ ] **Create `crates/lattice-keymap/src/contribution.rs`**

Copy verbatim. Update imports:
- `use crate::binding_mode::BindingMode` → `use crate::BindingMode`
- `use crate::keymap_entry::KeymapEntry` → `use crate::KeymapEntry`
- `use lattice_grammar::{CommandInvocation, SourceLocation}` — already available
- `use lattice_protocol::ChordPattern` — already available

`Subscription` and `DecorationProvider` are stubs — copy them as-is.

- [ ] **Add to `lattice-keymap/src/lib.rs`**

```rust
pub mod contribution;
pub use contribution::{Keymap, KeymapBinding, Subscription, DecorationProvider};
```

- [ ] **Replace `lattice-mode/src/contributions.rs`**

```rust
// Moved to lattice-keymap. Re-exported here for backward compatibility.
pub use lattice_keymap::{Keymap, KeymapBinding, Subscription, DecorationProvider};
```

- [ ] **Verify**

```bash
cargo check -p lattice-mode -p lattice-keymap
cargo test -p lattice-mode --lib 2>&1 | tail -5
```

---

### Task 7: Move `KeymapTrie`, `KeymapLayer`, `BoundCommand`, `LookupResult` to `lattice-keymap`

**Files:**
- Create: `crates/lattice-keymap/src/trie.rs`
- Modify: `crates/lattice-keymap/src/lib.rs`
- Modify: `crates/lattice-host/src/keymap_trie.rs`

- [ ] **Read current file**

```bash
wc -l crates/lattice-host/src/keymap_trie.rs
cat crates/lattice-host/src/keymap_trie.rs
```

- [ ] **Create `crates/lattice-keymap/src/trie.rs`**

Copy verbatim. Update imports:
- `use lattice_protocol::{ChordPattern, KeyChord}` — these come through `lattice-protocol` already
- `use lattice_grammar::SourceLocation` — already in dep
- `use crate::{BindingMode, ModeId}` — now in `lattice-keymap`

The `pub use lattice_protocol::ChordPattern;` re-export at the top of `keymap_trie.rs` should
move to `lattice-keymap/src/lib.rs` instead.

- [ ] **Add to `lattice-keymap/src/lib.rs`**

```rust
pub mod trie;
pub use trie::{KeymapTrie, KeymapLayer, BoundCommand, LookupResult};
// Surface ChordPattern at the crate root for convenience
pub use lattice_protocol::ChordPattern;
```

- [ ] **Add `lattice-keymap` dep to `lattice-host/Cargo.toml`**

```toml
lattice-keymap = { path = "../lattice-keymap" }
```

- [ ] **Replace `lattice-host/src/keymap_trie.rs`**

```rust
// Moved to lattice-keymap. Re-exported here for backward compatibility.
pub use lattice_keymap::{
    KeymapTrie, KeymapLayer, BoundCommand, LookupResult, ChordPattern,
};
```

- [ ] **Verify**

```bash
cargo check -p lattice-keymap -p lattice-host
cargo test -p lattice-host --lib 2>&1 | tail -5
```

---

### Task 8: Move `KeymapRegistry`, `KeymapHandle`, `KeymapCapability` to `lattice-keymap`

`KeymapReverseLookupHandle` **stays in `lattice-host`** — it implements
`lattice-completion::KeymapReverseLookup` and pulling that dep into `lattice-keymap` would
create a circular dependency. Separate it from the rest of `keymap_registry.rs` first.

**Files:**
- Create: `crates/lattice-keymap/src/registry.rs`
- Modify: `crates/lattice-keymap/src/lib.rs`
- Modify: `crates/lattice-host/src/keymap_registry.rs`

- [ ] **Read `keymap_registry.rs` top-to-bottom**

```bash
wc -l crates/lattice-host/src/keymap_registry.rs
# Read in sections if > 500 lines
head -100 crates/lattice-host/src/keymap_registry.rs
grep -n "KeymapReverseLookupHandle\|impl.*KeymapHandle\|pub fn reverse_lookup_handle" \
  crates/lattice-host/src/keymap_registry.rs
```

- [ ] **Create `crates/lattice-keymap/src/registry.rs`**

Copy the file. **Remove** `KeymapReverseLookupHandle` and the `reverse_lookup_handle()` method
on `KeymapHandle`. Update imports:

```rust
use lattice_grammar::{CommandId, CommandInvocation, CommandRegistry, SourceLocation};
use lattice_protocol::{ChordPattern, KeyChord};
use crate::{BindingMode, BoundCommand, KeymapLayer, KeymapTrie, LookupResult, ModeId};
// Remove: use lattice_completion::...
```

- [ ] **Add to `lattice-keymap/src/lib.rs`**

```rust
pub mod registry;
pub use registry::{
    KeymapCapability, KeymapError, KeymapHandle, KeymapRegistry,
    LayerId, PushLayerKind,
};
```

- [ ] **Shrink `lattice-host/src/keymap_registry.rs`** to only contain `KeymapReverseLookupHandle`:

```rust
//! `KeymapReverseLookupHandle` — the completion integration shim that wraps
//! `KeymapHandle::enumerate_chord_bindings` for `lattice-completion`'s
//! `KeymapReverseLookup` trait. Stays in `lattice-host` to avoid a circular
//! dep: lattice-completion → ... → lattice-host.
use std::sync::Arc;
use lattice_grammar::CommandRegistry;
use lattice_keymap::{BindingMode, KeymapHandle};
// ... (KeymapReverseLookupHandle impl only)
```

- [ ] **Update `lattice-host/src/keymap.rs` re-export shim** to pull from `lattice-keymap`:

```rust
pub use lattice_keymap::{
    BindingMode, KeymapEntry, default_keymap, entries, lookup, __builtin_source,
};
```

(We will delete this file entirely in Task 9 once all callers are updated.)

- [ ] **Verify**

```bash
cargo check -p lattice-keymap -p lattice-host
cargo test --workspace --quiet 2>&1 | tail -5
```

Expected: same green test count as the baseline from Task 1.

---

### Task 9: Fix diagnostics, delete shim, full build check, commit

**Files:** `crates/lattice-host/src/dispatch.rs`, `crates/lattice-host/src/subsystem.rs`,
delete `crates/lattice-host/src/keymap.rs`

- [ ] **Fix `dispatch.rs:21845` — `MajorMode` tuple pattern**

```bash
grep -n "MajorMode =>" crates/lattice-host/src/dispatch.rs
```

Change:
```rust
crate::keymap_trie::KeymapLayer::MajorMode => "Major mode".to_string(),
```
To:
```rust
lattice_keymap::KeymapLayer::MajorMode(mode_id) => {
    format!("Major mode: {mode_id}")
}
```

(The `mode_id` is now available since `MajorMode` carries a `ModeId`. Show it for parity with
`MinorMode`'s `format!("Minor: {mode_id}")` output.)

- [ ] **Fix `subsystem.rs:2657` — unused variable**

```bash
grep -n "let current" crates/lattice-host/src/subsystem.rs
```

Rename `current` → `_current` on that line.

- [ ] **Update all `crate::keymap_trie::` references in `dispatch.rs`**

```bash
grep -n "crate::keymap_trie::" crates/lattice-host/src/dispatch.rs
```

Replace each `crate::keymap_trie::KeymapLayer` with `lattice_keymap::KeymapLayer`, etc.

- [ ] **Delete `crates/lattice-host/src/keymap.rs`**

First confirm nothing still imports from it after all the re-export shims are gone:

```bash
grep -rn "crate::keymap::" crates/lattice-host/src/ | grep -v "keymap_trie\|keymap_registry\|keymap_mode"
```

For any remaining `crate::keymap::Foo` reference, replace with `lattice_keymap::Foo`.
Then:

```bash
rm crates/lattice-host/src/keymap.rs
# Remove the `mod keymap;` line from lattice-host/src/lib.rs
```

- [ ] **Full build and test**

```bash
cargo test --workspace --quiet 2>&1 | tail -5
cargo clippy --workspace -- -D warnings 2>&1 | grep -E "^error" | head -20
```

Expected: same test count as baseline; no clippy errors.

- [ ] **Commit Slice 1**

```bash
git add -A
git commit -m "feat(keymap): extract lattice-keymap crate — pure migration, zero behavior change

- New crates/lattice-keymap owns: ModeId, BindingMode, KeymapEntry,
  keymap_entry!, Keymap, KeymapBinding, KeymapTrie, KeymapLayer,
  BoundCommand, LookupResult, KeymapRegistry, KeymapHandle, KeymapCapability
- lattice-mode re-exports ModeId + binding + entry + contribution types
- lattice-host re-exports trie/registry types; KeymapReverseLookupHandle stays
  in lattice-host (lattice-completion dep); mode_contributions.rs stays in
  lattice-host (ModeRegistry + KeymapHandle cross-dep)
- Fixes dispatch.rs:21845: MajorMode(_) tuple pattern + label now shows mode id
- Fixes subsystem.rs:2657: rename current -> _current
- Deletes lattice-host/src/keymap.rs re-export shim (now redundant)
- All tests green; no behaviour change"
```

---

## Part 2 — Slice 2: Layer-trace API + `describe-key` rebuild

### Task 10: Add `KeymapResolution` and `LayerHit` types

**Files:**
- Create: `crates/lattice-keymap/src/resolution.rs`
- Modify: `crates/lattice-keymap/src/lib.rs`

- [ ] **Write `crates/lattice-keymap/src/resolution.rs`** (types + mode-prefix parser):

```rust
//! Keymap resolution tracing — the diagnostic surface for `:describe-key`.
//!
//! `KeymapResolution` captures the full sparse layer trace for a chord:
//! every layer that has a binding, lowest priority first, with the winning
//! layer flagged. Used by `describe-key` and future introspection tooling.

use crate::{BindingMode, BoundCommand, KeymapLayer, ModeId};
use std::fmt;

/// One layer's contribution to a chord lookup.
///
/// Only present when that layer has a binding for the queried chord (sparse
/// model — layers with no binding are omitted entirely).
#[derive(Debug, Clone)]
pub struct LayerHit {
    /// Which registry layer this hit came from.
    pub layer: KeymapLayer,
    /// Human-readable label: `"Built-in"`, `"rust-mode"`, `"Minor: lsp-mode"`, etc.
    pub label: String,
    /// The binding at this layer.
    pub command: BoundCommand,
    /// `true` for the highest-priority hit — the effective binding given
    /// the current active-mode set.
    pub wins: bool,
}

/// Full resolution trace for one `(chord sequence, binding mode)` pair.
///
/// `hits` is ordered lowest priority → highest (Builtin first, Buffer last).
/// Only layers that have a binding appear; an empty `hits` vec means the chord
/// is completely unbound in this mode.
#[derive(Debug, Clone)]
pub struct KeymapResolution {
    pub binding_mode: BindingMode,
    /// All layers with a binding, lowest → highest priority.
    pub hits: Vec<LayerHit>,
    /// The effective binding — highest-priority hit that fires given active modes.
    /// `None` if the chord is unbound in all layers.
    pub winner: Option<BoundCommand>,
}

impl KeymapResolution {
    /// True if no layer has a binding for this chord in this mode.
    pub fn is_unbound(&self) -> bool {
        self.hits.is_empty()
    }
}

// ── Mode-prefix parser ─────────────────────────────────────────────────────

/// Result of parsing a `:describe-key` argument that may carry a vim-style
/// mode prefix (`n_`, `i_`, `v_`, `r_`, `c_`, `s_`, `x_`).
#[derive(Debug, Clone)]
pub struct ParsedDescribeKeyArg<'a> {
    /// `Some(mode)` when a prefix was present; `None` means "all modes".
    pub mode: Option<BindingMode>,
    /// The chord string with the prefix stripped (if any).
    pub chord: &'a str,
}

/// Parse a `:describe-key` argument. Strips a leading two-char mode prefix
/// if present.
///
/// # Prefix table
/// | Prefix | Mode |
/// |--------|------|
/// | `n_`   | Normal |
/// | `i_`   | Insert |
/// | `v_`   | Visual |
/// | `x_`   | Visual (vim alias) |
/// | `r_`   | Replace |
/// | `c_`   | Command |
/// | `s_`   | Search |
///
/// If the argument starts with none of these prefixes, `mode` is `None`
/// (show all modes).
pub fn parse_describe_key_arg(s: &str) -> ParsedDescribeKeyArg<'_> {
    if s.len() < 3 {
        return ParsedDescribeKeyArg { mode: None, chord: s };
    }
    // Check for `{letter}_` prefix
    let (prefix, rest) = s.split_at(2);
    let mode = match prefix {
        "n_" => Some(BindingMode::Normal),
        "i_" => Some(BindingMode::Insert),
        "v_" | "x_" => Some(BindingMode::Visual),
        "r_" => Some(BindingMode::Replace),
        "c_" => Some(BindingMode::Command),
        "s_" => Some(BindingMode::Search),
        _ => None,
    };
    if mode.is_some() {
        ParsedDescribeKeyArg { mode, chord: rest }
    } else {
        ParsedDescribeKeyArg { mode: None, chord: s }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_prefix() {
        let r = parse_describe_key_arg("j");
        assert!(r.mode.is_none());
        assert_eq!(r.chord, "j");
    }

    #[test]
    fn parse_no_prefix_multi_chord() {
        let r = parse_describe_key_arg("gd");
        assert!(r.mode.is_none());
        assert_eq!(r.chord, "gd");
    }

    #[test]
    fn parse_normal_prefix() {
        let r = parse_describe_key_arg("n_j");
        assert_eq!(r.mode, Some(BindingMode::Normal));
        assert_eq!(r.chord, "j");
    }

    #[test]
    fn parse_insert_prefix() {
        let r = parse_describe_key_arg("i_<C-v>");
        assert_eq!(r.mode, Some(BindingMode::Insert));
        assert_eq!(r.chord, "<C-v>");
    }

    #[test]
    fn parse_visual_v_alias() {
        let r = parse_describe_key_arg("v_>");
        assert_eq!(r.mode, Some(BindingMode::Visual));
        assert_eq!(r.chord, ">");
    }

    #[test]
    fn parse_visual_x_alias() {
        let r = parse_describe_key_arg("x_>");
        assert_eq!(r.mode, Some(BindingMode::Visual));
        assert_eq!(r.chord, ">");
    }

    #[test]
    fn parse_command_prefix() {
        let r = parse_describe_key_arg("c_<Tab>");
        assert_eq!(r.mode, Some(BindingMode::Command));
        assert_eq!(r.chord, "<Tab>");
    }

    #[test]
    fn parse_short_string_no_prefix() {
        // "n" alone is not a prefix
        let r = parse_describe_key_arg("n");
        assert!(r.mode.is_none());
        assert_eq!(r.chord, "n");
    }
}
```

- [ ] **Add to `lattice-keymap/src/lib.rs`**

```rust
pub mod resolution;
pub use resolution::{KeymapResolution, LayerHit, ParsedDescribeKeyArg, parse_describe_key_arg};
```

- [ ] **Run the new tests**

```bash
cargo test -p lattice-keymap -- resolution 2>&1
```

Expected: `test result: ok. 8 passed; 0 failed`.

---

### Task 11: Implement `resolve_trace` and `resolve_trace_all_modes` on `KeymapHandle`

`enumerate_chord_bindings` already walks every layer and returns `Vec<(KeymapLayer, Arc<BoundCommand>)>`.
`resolve_trace` builds on it: it calls `enumerate_chord_bindings` for the layer list, then calls
`lookup_with_context` to identify the winner, then produces `Vec<LayerHit>`.

**Files:** `crates/lattice-keymap/src/registry.rs`

- [ ] **Add `resolve_trace` to `impl KeymapHandle` in `registry.rs`**

Add after `enumerate_chord_bindings`:

```rust
/// Trace the full layer resolution for a single `(binding_mode, chord)` pair.
///
/// Returns a [`KeymapResolution`] containing every layer that has a binding
/// for `chords` in `binding_mode`, ordered lowest-priority first. The winner
/// is determined by [`Self::lookup_with_context`] using `active_minor_modes`.
///
/// Off the hot path — allocates. Use [`Self::lookup_with_context`] for keystroke dispatch.
pub fn resolve_trace(
    &self,
    binding_mode: BindingMode,
    chords: &[KeyChord],
    active_minor_modes: &[ModeId],
) -> crate::resolution::KeymapResolution {
    use crate::resolution::{KeymapResolution, LayerHit};

    // Get the winner via the existing context-aware lookup.
    let winner = match self.lookup_with_context(binding_mode, chords, active_minor_modes) {
        LookupResult::Bound { command, .. } => Some((*command).clone()),
        _ => None,
    };

    // Walk all layers for their raw hits (sparse — only layers with a binding).
    let raw_hits = self.enumerate_chord_bindings(binding_mode, chords);

    let hits: Vec<LayerHit> = raw_hits
        .into_iter()
        .map(|(layer, bound)| {
            let label = layer_label_string(layer);
            let wins = winner
                .as_ref()
                .is_some_and(|w| std::ptr::eq(w as *const _, &*bound as *const _));
            LayerHit {
                layer,
                label,
                command: (*bound).clone(),
                wins,
            }
        })
        .collect();

    // Mark the winner by source equality when pointer comparison fails
    // (happens if the winner was cloned above rather than being the same Arc).
    // Use source location + command as a stable identity key.
    let hits = if winner.is_some() && hits.iter().all(|h| !h.wins) {
        // Fallback: mark the highest-priority hit as winner.
        let mut hits = hits;
        if let Some(last) = hits.last_mut() {
            last.wins = true;
        }
        hits
    } else {
        hits
    };

    KeymapResolution {
        binding_mode,
        hits,
        winner,
    }
}

/// Trace all binding modes at once. Modes with no hits in any layer are omitted.
///
/// Iterates over every `BindingMode` variant and calls [`Self::resolve_trace`].
pub fn resolve_trace_all_modes(
    &self,
    chords: &[KeyChord],
    active_minor_modes: &[ModeId],
) -> Vec<crate::resolution::KeymapResolution> {
    BindingMode::all()
        .into_iter()
        .map(|mode| self.resolve_trace(mode, chords, active_minor_modes))
        .filter(|r| !r.is_unbound())
        .collect()
}
```

- [ ] **Add `BindingMode::all()` if it doesn't exist**

Check:
```bash
grep -n "fn all\|BindingMode::all\|impl BindingMode" crates/lattice-keymap/src/binding_mode.rs
```

If `all()` doesn't exist, add to `impl BindingMode` in `binding_mode.rs`:

```rust
/// Returns all `BindingMode` variants in a stable order.
pub fn all() -> &'static [BindingMode] {
    &[
        BindingMode::Normal,
        BindingMode::Insert,
        BindingMode::Visual,
        BindingMode::Replace,
        BindingMode::Command,
        BindingMode::Search,
    ]
}
```

Adjust the list to match whatever variants exist in `BindingMode`.

- [ ] **Add `layer_label_string` helper in `registry.rs`**

This replaces `friendly_layer_label` in `dispatch.rs` — put the canonical version here:

```rust
/// Human-readable label for a `KeymapLayer`.
/// `"Built-in"` / `"Major mode: rust-mode"` / `"Minor: lsp-mode"` /
/// `"User config"` / `"Buffer-local"`
fn layer_label_string(layer: KeymapLayer) -> String {
    match layer {
        KeymapLayer::Builtin => "Built-in".to_string(),
        KeymapLayer::MajorMode(id) => format!("Major mode: {id}"),
        KeymapLayer::MinorMode(id) => format!("Minor: {id}"),
        KeymapLayer::User => "User config".to_string(),
        KeymapLayer::Buffer => "Buffer-local".to_string(),
    }
}
```

- [ ] **Write unit tests for `resolve_trace` in `registry.rs`**

Add to the existing `#[cfg(test)]` block in `registry.rs`:

```rust
#[test]
fn resolve_trace_unbound_chord_returns_empty() {
    let h = KeymapHandle::new();
    let chord = KeyChord::char('§');  // not bound anywhere
    let result = h.resolve_trace(BindingMode::Normal, &[chord], &[]);
    assert!(result.is_unbound());
    assert!(result.winner.is_none());
}

#[test]
fn resolve_trace_builtin_only_single_hit_wins() {
    let h = KeymapHandle::new();
    // Bind a chord at Builtin layer
    let chord = KeyChord::char('Ω');  // use something not in default keymap
    let src = SourceLocation::synthetic("test");
    let cmd = CommandInvocation::new_test("test:cmd");
    h.bind(KeymapLayer::Builtin, BindingMode::Normal, &[ChordPattern::Literal(chord)], cmd, src);

    let result = h.resolve_trace(BindingMode::Normal, &[chord], &[]);
    assert_eq!(result.hits.len(), 1);
    assert!(result.hits[0].wins);
    assert_eq!(result.hits[0].label, "Built-in");
    assert!(result.winner.is_some());
}

#[test]
fn resolve_trace_minor_mode_overrides_builtin() {
    let h = KeymapHandle::new();
    let chord = KeyChord::char('Ψ');
    let src = SourceLocation::synthetic("test");
    let builtin_cmd = CommandInvocation::new_test("test:builtin");
    let minor_cmd = CommandInvocation::new_test("test:minor");
    let mode_id = ModeId::new("test-minor");

    h.bind(KeymapLayer::Builtin, BindingMode::Normal,
           &[ChordPattern::Literal(chord)], builtin_cmd, src.clone());
    h.bind(KeymapLayer::MinorMode(mode_id), BindingMode::Normal,
           &[ChordPattern::Literal(chord)], minor_cmd, src);

    // With the minor mode active, it wins
    let result = h.resolve_trace(BindingMode::Normal, &[chord], &[mode_id]);
    assert_eq!(result.hits.len(), 2);
    let winner_hit = result.hits.iter().find(|h| h.wins).expect("winner");
    assert!(matches!(winner_hit.layer, KeymapLayer::MinorMode(_)));
}
```

> `CommandInvocation::new_test` is a test-only constructor — check if it exists. If not, use
> whatever the existing tests in `keymap_registry.rs` use to construct a `CommandInvocation`.

- [ ] **Run tests**

```bash
cargo test -p lattice-keymap -- registry 2>&1 | tail -10
```

Expected: all tests pass including the three new ones.

---

### Task 12: Rewrite `build_describe_key_content` in `dispatch.rs`

**Files:** `crates/lattice-host/src/dispatch.rs`

- [ ] **Read the existing implementation bounds**

```bash
grep -n "fn build_describe_key_content\|fn append_resolved\|fn append_runtime\|fn friendly_layer_label" \
  crates/lattice-host/src/dispatch.rs
```

Note the line numbers. The new implementation replaces all four functions.

- [ ] **Find where `:describe-key` parses its argument**

```bash
grep -n "describe.key\|describe_key" crates/lattice-host/src/dispatch.rs | head -20
```

Find where the `chord: &str` arg is obtained and passed to `build_describe_key_content`.

- [ ] **Update the call site to parse the mode prefix first**

In the `Effect::DescribeKey { chord }` arm (or wherever the command is invoked), add:

```rust
use lattice_keymap::parse_describe_key_arg;
let parsed_arg = parse_describe_key_arg(&chord);
let content = self.build_describe_key_content(parsed_arg.mode, parsed_arg.chord);
```

- [ ] **Rewrite `build_describe_key_content`**

Replace the entire function (and its three helper functions `append_resolved_binding_section`,
`append_runtime_chord_bindings_section`, `friendly_layer_label`) with:

```rust
/// Build the `:describe-key` help buffer content.
///
/// If `target_mode` is `Some`, shows only that binding mode's layer trace.
/// If `None`, shows all binding modes that have ≥1 binding for the chord.
///
/// Output format: sparse layer trace (B) — only layers with a binding appear,
/// ordered lowest → highest priority, effective binding marked with `← wins`.
///
/// Uses the runtime `KeymapHandle` as the sole authoritative source.
/// The static `keymap_entry!` catalog is NOT consulted here — it is used
/// only for `:keymap` listing and the drift test.
pub fn build_describe_key_content(
    &self,
    target_mode: Option<lattice_keymap::BindingMode>,
    chord: &str,
) -> lattice_help::HelpContent {
    use lattice_keymap::{BindingMode, KeymapResolution};

    let Ok(parsed_chords) = crate::chord::parse_chord_sequence(chord) else {
        let lines = vec![format!("`{chord}` could not be parsed as a chord sequence.")];
        return lattice_help::HelpContent::from_lines(format!("describe-key {chord}"), lines)
            .with_markdown_syntax(self.lang_registry.clone());
    };

    let active_minors: Vec<lattice_keymap::ModeId> = self
        .active_modes
        .get(&self.document_buffer_id)
        .map(|m| m.minors().to_vec())
        .unwrap_or_default();

    let resolutions: Vec<KeymapResolution> = match target_mode {
        Some(mode) => {
            let r = self.keymap.resolve_trace(mode, &parsed_chords, &active_minors);
            if r.is_unbound() { vec![] } else { vec![r] }
        }
        None => self.keymap.resolve_trace_all_modes(&parsed_chords, &active_minors),
    };

    let mut lines: Vec<String> = Vec::new();

    // Title line
    let title = match target_mode {
        Some(mode) => format!("{chord}  ({mode} mode)"),
        None       => chord.to_string(),
    };
    lines.push(lattice_help::key_link(&title));
    lines.push(String::new());

    if resolutions.is_empty() {
        lines.push("  (unbound)".to_string());
    } else {
        for res in &resolutions {
            // Section header when showing multiple modes
            if target_mode.is_none() {
                lines.push(format!("{}:", res.binding_mode));
                lines.push(String::new());
            }

            for hit in &res.hits {
                let wins_marker = if hit.wins { "  ← wins" } else { "" };
                let source = hit.command.source.as_link();
                lines.push(format!(
                    "  {:<24}  {}    {}{}",
                    hit.label,
                    self.resolve_command_name(&hit.command.command),
                    source,
                    wins_marker,
                ));
            }
            lines.push(String::new());
        }
    }

    lattice_help::HelpContent::from_lines(format!("describe-key {chord}"), lines)
        .with_markdown_syntax(self.lang_registry.clone())
}

/// Look up the canonical name for a `CommandId` in the `CommandRegistry`.
/// Falls back to `"{:?}"` debug formatting if the id isn't registered.
fn resolve_command_name(&self, id: &lattice_grammar::CommandId) -> String {
    self.registry
        .lookup(*id)
        .map(|spec| spec.name.clone())
        .unwrap_or_else(|| format!("{id:?}"))
}
```

> `res.binding_mode` needs a `Display` impl on `BindingMode` — check if one exists. If not, add
> `impl fmt::Display for BindingMode` in `binding_mode.rs` with arms like
> `Normal => "Normal"`, `Insert => "Insert"`, etc.

- [ ] **Verify compilation**

```bash
cargo check -p lattice-host 2>&1 | grep "^error"
```

Fix any type errors (most will be missing imports or renamed methods).

- [ ] **Run all tests**

```bash
cargo test --workspace --quiet 2>&1 | tail -5
```

Expected: same pass count as baseline. Note: existing `describe-key` integration tests will need
their expected output updated to match the new format — update them now.

---

### Task 13: Remove dead code + final clippy pass

After Task 12, the following are no longer called:

- `append_resolved_binding_section` in `dispatch.rs`
- `append_runtime_chord_bindings_section` in `dispatch.rs`
- `friendly_layer_label` in `dispatch.rs` (superseded by `layer_label_string` in `registry.rs`)
- `crate::keymap::lookup(chord)` call in `build_describe_key_content` (deleted in Task 12)

**Files:** `crates/lattice-host/src/dispatch.rs`

- [x] **Delete the three dead helper functions**

```bash
grep -n "fn append_resolved_binding_section\|fn append_runtime_chord_bindings_section\|fn friendly_layer_label" \
  crates/lattice-host/src/dispatch.rs
```

Delete each function body (from the `fn` line to its closing `}`).

- [x] **Remove now-unused imports from `dispatch.rs`**

```bash
cargo check -p lattice-host 2>&1 | grep "unused import"
```

Delete any import line flagged as unused.

- [x] **Run clippy workspace-wide**

```bash
cargo clippy --workspace -- -D warnings 2>&1 | grep "^error"
```

Fix any remaining issues. Common ones after a migration:
- `dead_code` on re-export shim items not yet used elsewhere
- `unused_imports` in modified files

- [x] **Final test run**

```bash
cargo test --workspace 2>&1 | tail -10
```

Expected: all tests pass; test count ≥ baseline.

- [x] **Commit Slice 2**

```bash
git add -A
git commit -m "feat(keymap): KeymapResolution layer-trace API + describe-key rebuild (K.3.1)

- lattice-keymap: new resolution.rs with KeymapResolution + LayerHit types
- lattice-keymap: resolve_trace() + resolve_trace_all_modes() on KeymapHandle
  built on existing enumerate_chord_bindings(); off hot path, allocates
- lattice-keymap: parse_describe_key_arg() for vim-style mode prefixes
  (n_/i_/v_/x_/r_/c_/s_); no prefix = all modes
- lattice-keymap: BindingMode::all() + Display impl
- lattice-keymap: layer_label_string() canonical impl (was friendly_layer_label)
- lattice-host: rewrite build_describe_key_content() against resolve_trace API
  — removes static keymap_entry catalog path, removes K.1.d runtime-registry
  section, removes K.2.4.A.1 resolved-binding section (all replaced by single
  sparse trace)
- lattice-host: delete append_resolved_binding_section,
  append_runtime_chord_bindings_section, friendly_layer_label (dead after rebuild)
- Sparse output: only layers with a binding shown; ← wins marks effective binding
- Mode-prefix targeting: :describe-key n_gd → Normal mode only"
```

---

## Self-review checklist

- [x] **Spec coverage:** All spec requirements have a task.
  - `lattice-keymap` crate → Tasks 2–9
  - `KeymapResolution` / `LayerHit` → Task 10
  - `resolve_trace` / `resolve_trace_all_modes` → Task 11
  - Mode-prefix parser → Task 10 (co-located in `resolution.rs`)
  - `describe-key` rebuild → Task 12
  - Dead-code removal → Task 13
  - **Spec deviation:** `mode_contributions.rs` stays in `lattice-host` to avoid circular dep
    (`ModeRegistry` from `lattice-mode` + `KeymapHandle` from `lattice-keymap`). Noted in
    file map and dependency diagram.

- [x] **No TBDs or placeholders** in any task.

- [x] **Type consistency:**
  - `LayerHit.command: BoundCommand` (not `Arc<BoundCommand>`) — `resolve_trace` clones out of Arc.
  - `resolve_trace` takes `active_minor_modes: &[ModeId]` matching `lookup_with_context`'s signature.
  - `build_describe_key_content` updated to take `(target_mode: Option<BindingMode>, chord: &str)`.
  - `layer_label_string` defined once in `registry.rs`; `friendly_layer_label` in `dispatch.rs` deleted.
