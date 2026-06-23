# Diff extraction — slice plan (`lattice-host::diff` → the `lattice-diff` crate, as diff modes)

**Status:** 🗒 planned (not started). This is **BC.6** of the boot-composition
initiative (`docs/dev/operations/slice-plans/boot-composition.md`), carved into
its own file because it is a multi-slice cross-crate extraction, not a one-line
`install` migration like BC.3b/BC.4/BC.5.

**Directive (Dhruva, 2026-06-23):** "properly extract all diff related
functionality into diff modes within `lattice-diff`" — and "identify any
specialized diff features to be modelled within separate minor modes, wherever
it makes sense." Mode decomposition **confirmed**: `diff-mode` +
`diff-conflict-mode` (§4).

A design fragment (`docs/dev/architecture/diff-extraction.md`) should be split
out from the "Design context & decisions" sections below when execution starts
(project discipline: design = what/why, slice plan = when/how). It is inlined
here so this single doc is self-sufficient to take forward into a new session.

---

## 1. Goal

Move the entire diff **subsystem** out of `lattice-host` (`crate::diff`, 7 files)
into the **existing `lattice-diff` crate** (today the pure diff *algorithm*), so
`lattice-diff` becomes the full diff crate (algorithm + subsystem + modes), and
diff installs into the editor through the BC `SubsystemBoot` seam
(`lattice_diff::install(boot)`) — the same shape as claude-code (BC.3b) and
terminal (BC.4). Decompose the diff feature surface into minor modes (§4). When
done, **diff's only footprint in `editor_boot` is one Phase-B install-list
line**, and the host retains only the host-owned *impls* of the traits diff
abstracts over (the buffer resolver) — not diff logic.

## 2. Why a crate (not a `lattice-mode` builtin like emacs-keys)

BC.5 made `emacs-keys-mode` a `lattice-mode` builtin because it is a marker mode
+ a keymap whose every type lives below `lattice-mode`. **Diff is the opposite:**
it is a real subsystem — a `DiffSubsystem` with a debounced event drainer + a
document resolver, overlay/fold/filler **render providers**, a modeline element,
a `DiffModeBridge`, and per-session forwarders. Too large and machinery-heavy to
be a foundation builtin; it deserves its own crate. (Heuristic #1: the
genuinely-better long-term home for a self-contained subsystem with its own
algorithm + rendering + lifecycle is a crate, not the shared mode substrate.)

### Crate target + cycle-safety (verified 2026-06-23)

- `lattice-diff` already exists: `compute`/`patch`/`types`
  (`compute_diff`, `DiffAlgorithm`, `Hunk`, `HunkIndex`, `HunkKind`, `LineRange`).
  Deps today: only `imara-diff` / `ropey` / `smallvec` / `thiserror` (no
  `lattice-*`). **Reverse-deps: only `lattice-host`.**
- Moving the subsystem in grows `lattice-diff`'s deps to: `lattice-core`,
  `lattice-protocol`, `lattice-runtime`, `lattice-mode`, `lattice-grammar`,
  `lattice-keymap`, `lattice-cells`, `lattice-theme` (+ the existing four).
- **No cycle:** nothing below `lattice-host` depends on `lattice-diff`, so it may
  freely depend on those mid-level crates. (Re-confirm at execution time:
  `grep -rl 'lattice-diff' crates/*/Cargo.toml` should list only `lattice-diff`
  + `lattice-host` before DX.6.)

## 3. Coupling resolution — the host references that must be severed

`lattice-host → lattice-diff`, so the moved code must contain **no** `crate::`
(host) reference. Each coupling found in the 7 files and its resolution:

| # | Host coupling (file) | What diff uses | Real definition | Resolution |
|---|---|---|---|---|
| C1 | `keymap_trie::*` (mode) | `BoundCommand`, `KeymapTrie`, `KeymapLayer` | `lattice-keymap` (host re-export shim) | **(A)** import `lattice_keymap` directly |
| C2 | `chord::*` (mode) | `ChordPattern`, `KeyChord`, … | `lattice-protocol` (shim) | **(A)** import `lattice_protocol` directly |
| C3 | `keymap::BindingMode` (mode) | `BindingMode` | `lattice-keymap` via `lattice-mode` | **(A)** `lattice_mode::BindingMode` |
| C4 | `ui::theme::{ResolvedTheme, BuiltinElementIds}` (overlay) | the two types | `lattice-theme` | **(A)** import `lattice_theme` directly |
| C5 | `ui::theme::resolve_syntax_style` (overlay) | the fn | host `crates/lattice-host/src/ui/theme.rs:78` | **(C) move down** → relocate to `lattice-theme` (pure styling fn over lattice-theme types) — **DX.2** |
| C6 | `buffer_registry::BufferRegistry` (subsystem, mod) | `BufferRegistryTextProvider`, the `DocumentBufferResolver` impl | host concrete type | **(B) trait** → `DocumentBufferResolver` + the text-provider trait live in `lattice-diff`; the `BufferRegistry…` impls **stay in `lattice-host`** (resolver pattern, like terminal's `TerminalStore`) — **DX.6/DX.7** |
| C7 | `fold_provider::{FoldProvider, FoldContext}` (fold) | the provider trait + ctx | host `fold_provider.rs` | **(B/C)** move the provider trait to `lattice-core` (where `FoldOverlayServiceHandle` lives); diff's `HunkFoldProvider` impl rides along — **DX.3** |
| C8 | `pane_group::RowMapper` (pane_group, filler) | the `RowMapper` trait | host `pane_group.rs` | **(B/C)** move the `RowMapper` trait to a shared crate (`lattice-core`/`lattice-cells`); host keeps any host-specific impl — **DX.3** |
| C9 | `modeline::ROLE_MODE_ITEM` (mode) | a `&str` const | host `modeline.rs:58` | **(C-trivial)** move const to `lattice-mode`'s modeline module — **DX.4** |
| C10 | `actions::ActionIds` (mode — `diff_mode_layer_bindings(&ActionIds)`) | `ids.diff_get` / `ids.diff_put` | host `actions.rs` registers `action:diff-get`/`-put` | **(C) move** → diff registers its own `action:diff-*` commands in `lattice-diff`; the keymap builder resolves them **by name** against `&CommandRegistry` (the emacs-keys pattern), dropping the typed `ActionIds` dep — **DX.5** |

C1–C4 are the **same shims** the emacs-keys move used — clean. The real work is
C5–C10 (three small type-relocations + diff owning its actions + two trait
abstractions).

## 4. Mode decomposition — DECIDED (Dhruva, 2026-06-23)

`diff-mode` + `diff-conflict-mode`:

- **`diff-mode`** (base, 2-way diff): a buffer participates in a diff session →
  sign gutter (`DiffSignKind` Add/Remove/Change), `+N ~M` modeline element, and
  the `do`/`dp` hunk get/put chords. **It owns its render providers** — the
  hunk-fold (`HunkFoldProvider`), filler-row (`FillerRowProvider`), and overlay
  virtual-row (`DiffOverlayVirtualRowProvider`) providers register from
  `diff-mode`'s `on_activate` (via the fold-overlay-service +
  virtual-row-provider-registry services), NOT from host boot. They are render
  providers, not activatable feature surfaces, so they stay mode-owned rather
  than becoming pseudo-modes.
- **`diff-conflict-mode`** (smerge-style): activates only when the session has
  conflict regions (`DiffSignKind::Conflict`), contributing conflict-resolution
  chords (keep-ours / keep-theirs / keep-both / next-conflict) + conflict gutter.
  Today "conflict" exists only as a sign kind; this mode is where conflict
  *resolution* becomes a first-class, separately-activatable surface. **Partly
  forward-looking** — at DX.8, if conflict-resolution actions don't exist yet,
  establish the mode shell + the activation predicate only, and track the
  resolution chords as a follow-up. Do NOT invent behaviour to justify the mode;
  the decomposition (separating conflict resolution from 2-way diffing) is the
  win.

> **Heuristic #1 (long-term fit):** separating conflict-resolution matches the
> cross-editor convention (vim diff vs `smerge`/conflict modes; magit) and keeps
> `diff-mode` focused; render providers stay mode-owned (not over-split).
> **Heuristic #3 (third option) — considered + rejected:** a monolithic single
> `diff-mode` is simpler but bundles conflict resolution into the 2-way surface,
> muddying activation (conflicts aren't always present).

## 5. Diff module inventory (what moves + what the host consumes)

**Files (move all 7 into `lattice-diff/src/`):** `mod.rs`, `mode.rs`,
`subsystem.rs`, `overlay.rs`, `fold.rs`, `filler.rs`, `pane_group.rs`.

**Public items the host consumes (must stay `pub`; host keeps calling via
`lattice_diff::`):**
- subsystem: `DiffSession`, `DiffSubsystem`, `DiffDescriptor`,
  `BufferRegistryTextProvider`*, `BufferSource`, `OnDiskSource`,
  `DiffParticipantSource`, `DocumentBufferResolver`,
  `BufferRegistryDocumentResolver`*  (*the `BufferRegistry…` impls stay host-side,
  see C6)
- overlay: `DiffOverlayVirtualRowProvider`, `DiffOverlayRefreshTask`,
  `SyntaxContext`, `diff_overlay_provider_id`, `DiffSignMap`, `DiffSignKind`,
  `DiffOutcome`
- mode: `DiffMode`, `diff_mode_layer_bindings`, `register_diff_modes`,
  `register_diff_modeline_element`, `diff_content`, `DIFF_ELEMENT`,
  `DiffModeBridge`, `DiffModeChange`, `DiffModeAction`, `DiffDecorationData`
- fold: `HunkFoldProvider`, `HUNK_FOLD_PROVIDER_ID`
- filler: `FillerRowProvider`, `Side`, `diff_filler_provider_id`
- pane_group: `HunkRowMapper`

(Source: 2026-06-23 coupling scan. Re-verify the exact set with `grep -rn
'crate::diff::' crates/lattice-host/src` excluding `src/diff/` at DX.1 — the
host consumer sites to rewire are `editor_boot.rs` [register + subsystem bind +
modeline element + keymap push + the `diff_subsystem` / `diff_subscription_guard`
/ `diff_forwarders` Editor fields], `dispatch.rs` [`apply_pending_diff_mode_changes`
+ the `:diff`/`:diffoff` ex-commands], `render_state.rs`, `fold_provider.rs`.)

## 6. Slice sequence

Each slice lands green + behaviour-pinned. Migrate by *moving types down first*
(so the big file-move in DX.6 is a clean import swap), then the file move, then
the host rewire, then the mode decomposition.

- **DX.0 — design fragment + this slice plan.** ✅ (this doc; split the design
  fragment out when DX.1 starts).
- **DX.1 — diff regression pins (the gate).** A `crates/lattice-host/tests/
  diff_regression_pins.rs` (BC.2 style) pinning current behaviour BEFORE any
  move: `diff-mode` registered; `:diff`/`:diffoff` resolve; `DiffSubsystem`
  bound + its `DocumentChanged`/`DocumentClosed` drain wakes; the `+N ~M`
  modeline element registered; `do`/`dp` chords bound on a diff buffer; sign
  gutter decorations present. These are the arbiter for every later slice
  ("no behaviour change").
- **DX.2 — move `resolve_syntax_style` → `lattice-theme`** (C5). Pure relocation;
  host + overlay call `lattice_theme::resolve_syntax_style`. Green: workspace
  build + theme tests.
- **DX.3 — move the `FoldProvider`/`FoldContext` + `RowMapper` traits →
  `lattice-core`** (C7, C8). Host fold/pane-group impls re-point to the moved
  traits; diff's `HunkFoldProvider` / `HunkRowMapper` impl the moved traits at
  DX.6. Green: workspace build + host fold/pane tests.
- **DX.4 — move `ROLE_MODE_ITEM` → `lattice-mode` modeline module** (C9). Trivial
  const relocation; update host + (future) diff references.
- **DX.5 — diff owns its actions** (C10). Register `action:diff-get`/`-put` from
  diff (initially still host-side, moved into `lattice-diff` at DX.6) and convert
  `diff_mode_layer_bindings` to resolve **by name** against `&CommandRegistry`
  (drop the `&ActionIds` param) — the emacs-keys pattern. Green: `do`/`dp` still
  bind (DX.1 pins); name resolution works.
- **DX.6 — move the 7 files into `lattice-diff` + sever couplings.** Add the
  Cargo deps (§2). Fix imports (C1–C4 → direct lower-crate paths; C5/C9 → moved
  homes; C7/C8 → impl the lattice-core traits). Trait-abstract the resolver: the
  `DocumentBufferResolver` (+ text-provider) trait lives in `lattice-diff`; the
  `BufferRegistryDocumentResolver` / `BufferRegistryTextProvider` impls **stay in
  `lattice-host`** (host owns `BufferRegistry`). Green: `lattice-diff` builds
  standalone (`cargo build -p lattice-diff`); host builds against `lattice_diff::`.
- **DX.7 — rewire host onto `lattice_diff::` via `install(boot)`.** Add
  `lattice_diff::install(boot: &mut impl SubsystemBoot)` collapsing diff's
  editor_boot wiring (register modes + register actions + push the name-based
  keymap layer + register the modeline element + bind the subsystem with a
  host-provided resolver) into **one Phase-B install-list line** alongside
  claude-code + terminal. The resolver is host-provided (the
  `BufferRegistryDocumentResolver` passed via a `boot.service::<…>()` lookup or a
  registered handle — decide at execution, mirroring terminal's
  `TerminalStoreHandle` host-published primitive). The `diff_subsystem` /
  `diff_subscription_guard` / `diff_forwarders` Editor fields + the
  `apply_pending_diff_mode_changes` dispatch tail stay host-side (host actor-loop
  state) but read `lattice_diff::` types. Green: DX.1 pins + the `:diff`
  end-to-end path; **this also closes BC.6**.
- **DX.8 — mode decomposition** (§4). Make the render providers mode-owned
  (registered from `diff-mode::on_activate`, not host boot). Add
  `diff-conflict-mode` (shell + activation predicate at minimum; resolution
  chords per §4 note). Green: pins + new mode tests.
- **DX.final — four artefacts + cleanup.** Promote the design fragment
  (`architecture/diff-extraction.md`); add/confirm benches if the move touched a
  hot path (overlay build is viewport-bounded — confirm no regression); ensure
  graceful error handling on the resolver/provider seams (log + skip, never
  panic on the actor loop); mark BC.6 ✅ in `boot-composition.md`.

## 7. Risks / open questions (carry into the slices)

1. **`resolve_syntax_style` move (DX.2)** — confirm its body only touches
   `lattice-theme` types (it takes `&ResolvedTheme`, `&BuiltinElementIds`, a
   style); if it reaches any host type, that's a further C-blocker to resolve
   first.
2. **Fold/RowMapper trait homes (DX.3)** — confirm `lattice-core` is the right
   home (it already owns `FoldOverlayServiceHandle`); check no host-only types in
   the trait signatures. `RowMapper` may fit `lattice-cells` better (virtual-row
   substrate) — decide by what its methods reference.
3. **The resolver hand-off (DX.7)** — `DiffSubsystem::bind` needs an
   `Arc<dyn DocumentBufferResolver>` over the host `BufferRegistry`. Mirror
   terminal: host registers the impl as a service / passes it in `install`. This
   is the one genuine host↔diff seam; keep it a clean trait.
4. **`diff-conflict-mode` may be ahead of the code** — if conflict *resolution*
   actions don't exist, DX.8 establishes the mode + activation predicate only,
   and the resolution chords are a tracked follow-up.
5. **No behaviour change** is the contract of every slice — DX.1 pins are the
   arbiter. The overlay/fold/filler render paths are viewport-bounded
   (paramount #1); the move must not turn any of them O(file).
6. **Cross-renderer parity** — diff gutter signs + filler rows + the diff
   modeline element render in BOTH TUI and GPUI. Any `DiffSignKind` / effect /
   theme-field touched in the move updates both peers in the same slice
   (`feedback_tui_gpui_parity`).

## 8. Cross-references

- Parent initiative + the `SubsystemBoot` seam diff installs through:
  `docs/dev/operations/slice-plans/boot-composition.md` (BC.6) +
  `docs/dev/architecture/boot-composition.md`.
- Diff design/behaviour (existing): `crates/lattice-host/src/diff/*` module docs
  (move with the files).
- The emacs-keys move (BC.5) is the template for C1–C4 + the name-based keymap
  builder (C10): `crates/lattice-mode/src/emacs_keys_mode.rs`.
