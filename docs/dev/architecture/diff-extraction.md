# Diff extraction — design fragment (`lattice-host::diff` → the `lattice-diff` crate)

**What + why** of moving the host-side diff *subsystem* into the existing
`lattice-diff` crate as diff *modes*, installed through the BC `SubsystemBoot`
seam. The **when + how** (slice IDs, sequencing, status) lives in the slice plan:
`docs/dev/operations/slice-plans/diff-extraction.md`. This is **BC.6** of the
boot-composition initiative (`docs/dev/architecture/boot-composition.md`).

**Directive (Dhruva, 2026-06-23):** "properly extract all diff related
functionality into diff modes within `lattice-diff`" — and "identify any
specialized diff features to be modelled within separate minor modes, wherever
it makes sense." Mode decomposition **confirmed**: `diff-mode` +
`diff-conflict-mode` (§4).

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
  `lattice-keymap`, `lattice-cells`, `lattice-theme`, **`lattice-syntax`**
  (+ the existing four). (`lattice-syntax` added at DX.2: the diff overlay
  already uses `lattice_syntax::{Lang, LangRegistry, oneshot_highlight_lines,
  StyledSpan, Style}` and now reaches `lattice_syntax::resolve_syntax_style`
  there too — see C5.)
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
| C5 | `ui::theme::resolve_syntax_style` (overlay) | the fn | was host `ui/theme.rs:78` | **(C) move down** ✅ **DX.2** → relocated to `lattice-syntax` (`theme_style.rs`), NOT `lattice-theme`: the fn is syntax-aware (takes `lattice_syntax::Style`), so it lives in the higher crate depending DOWN on the theme leaf. Host keeps a façade re-export; overlay's import becomes `lattice_syntax::resolve_syntax_style` at DX.6. |
| C6 | `buffer_registry::BufferRegistry` (subsystem, mod) | `BufferRegistryTextProvider`, the `DocumentBufferResolver` impl | host concrete type | **(B) trait** → `DocumentBufferResolver` + the text-provider trait live in `lattice-diff`; the `BufferRegistry…` impls **stay in `lattice-host`** (resolver pattern, like terminal's `TerminalStore`) — **DX.6/DX.7** |
| C7 | `fold_provider::{FoldProvider, FoldContext}` (fold) | the provider trait + ctx | host `fold_provider.rs` | ✅ **DX.3-C7 (resolved differently).** Planned "move to `lattice-core`" was BLOCKED: `FoldContext` carries `lattice_syntax::SyntaxSnapshot` + `lattice_diff::HunkIndex`, which `lattice-core` (bottom of the graph) cannot reference without a cycle. Real fix: diff's `HunkFoldProvider` → a self-contained `lattice_core::FoldSource` (`HunkFoldSource`) registered by `diff-mode::on_activate` via the `FoldOverlayService` (the multibuffer pattern). `FoldProvider`/`FoldContext` STAY host-side; `FoldContext.diff_hunks` removed. Nothing relocates. |
| C8 | `pane_group::RowMapper` (pane_group, filler) | the `RowMapper` trait | host `pane_group.rs` | ✅ **DX.3-C8** → moved to `lattice_core::ui::pane` (zero type coupling: `map_row(&self, usize, usize, u32) -> u32`), beside its `PaneGroupId`/`PaneId` siblings. Host re-exports it; `PaneGroup` registry + `Identity`/`Offset` impls stay host. `HunkRowMapper` impls the moved trait (import flips at DX.6). |
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
  this mode is where conflict *resolution* is a first-class,
  separately-activatable surface. DX.8 established the mode shell + the
  `sign_map_has_conflicts` activation predicate only (forward-looking); the
  resolution itself **landed in CR.x** (2026-06-24) — see
  `slice-plans/diff-conflict-resolution.md`. The chords are the vim-fugitive
  3-way family `d2o`/`d3o`/`d2p`/`d3p`/`dB` (keep-ours / keep-theirs /
  put-ours / put-theirs / keep-both) + `]c`/`[c` (next/prev hunk, conflicts
  included), mode-owned via `DiffConflictMode::keymap()` +
  `action_handlers()`; activation is bridge-driven off the published sign map
  (`DiffModeBridge::note_conflict_state`). In the marker-free model the cursor
  sits on `local` (= ours), so `d2o`/`d2p` are degenerate self-targets that
  echo rather than edit (decided with Dhruva). The decomposition (separating
  conflict resolution from 2-way diffing) is the win.

> **UX (higher court):** no runtime-UX change — the move is structural; the same
> gutter signs, modeline element, overlay/filler rows, and `do`/`dp` chords
> render identically. The mode split changes *where* chords live, not *what* the
> user sees. (DX.1 pins + cross-renderer parity are the guard.)
> **Paramount goals:** protects #2 (extensibility — diff installs through the
> uniform `SubsystemBoot` seam, the cleanest extension shape) and #3 (the diff
> grammar `do`/`dp` + conflict chords are mode-owned, the standing rule); does
> not sacrifice #1 (the overlay/fold/filler paths stay viewport-bounded — the
> move must not turn any O(file)).
> **Heuristic #1 (long-term fit):** separating conflict-resolution matches the
> cross-editor convention (vim diff vs `smerge`/conflict modes; magit) and keeps
> `diff-mode` focused; render providers stay mode-owned (not over-split).
> **Heuristic #3 (third option) — considered + rejected:** a monolithic single
> `diff-mode` is simpler but bundles conflict resolution into the 2-way surface,
> muddying activation (conflicts aren't always present).
> **Standing-rule check (mode ownership):** the extraction keeps BOTH the chord
> choice AND the handler bodies with the mode — `diff_mode_layer_bindings` moves
> into `lattice-diff` AND diff registers its own `action:diff-*` handlers (C10),
> so no half-migration (substrate-publishes-but-host-still-binds) remains.

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

## 6. Cross-references

- **Slice plan (when + how + status):**
  `docs/dev/operations/slice-plans/diff-extraction.md` (DX.0–DX.final).
- Parent initiative + the `SubsystemBoot` seam diff installs through:
  `docs/dev/operations/slice-plans/boot-composition.md` (BC.6) +
  `docs/dev/architecture/boot-composition.md`.
- Existing diff *algorithm/subsystem* design (not the extraction):
  `docs/dev/architecture/diff-system.md`; the moved module docs in
  `crates/lattice-host/src/diff/*` travel with the files.
- The emacs-keys move (BC.5) is the template for C1–C4 + the name-based keymap
  builder (C10): `crates/lattice-mode/src/emacs_keys_mode.rs`.
