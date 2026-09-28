# Slice plan — listing mode-ownership (LM series)

Design: `docs/dev/architecture/listing-mode-ownership.md`. Complements
the DL series (presentation minor; archived plan
`slice-plans/archive/directory-listing-mode.md`).

Migrate oil-mode / file-tree-mode's navigation + open surface off the
host input gate into the majors, add `<C-s>`/`<C-v>`/`<C-t>`
open-in-target, and keep many listings independent (design §3.2).

Legend: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.

## Sequencing

Each slice is one commit, green before commit (`scripts/precommit.sh`
scoped to touched crates; `--features window` auto-added when
`lattice-ui-gpui` is in scope). Targeted tests per slice; no whole-crate
gate per slice.

- **LM.0 ✅ (`2e82ac66`) — `OpenInTarget` effect primitive.**
  - Move `OpenTarget` (`Default|Split|VSplit|Tab`) from `lattice-picker`
    to `lattice-core` (beside `SplitOrientation`); re-export from
    `lattice-picker` so existing `lattice_picker::OpenTarget` sites are
    untouched.
  - Add peer-applied `Effect::OpenInTarget { path, position, target }`
    (`lattice-grammar`). Peer arm in **both** TUI (`app/dispatch.rs`) and
    GPUI (`lib.rs`): `prepare_open_target_pane(target)` then
    `open_buffer_at(path, position, false, None, None)`.
  - WIT boundary (`boundary_effect.rs`): host-only, typed "no WIT
    mirror" error (not a silent drop).
  - Add to the effect-classification exhaustive matches in both renderers
    + host `handle_effect`.
  - Test: `OpenInTarget{target:Split}` lands the file in a new pane
    (pane-tree assertion); `Default` behaves as `OpenBufferAt`. No
    behaviour change to any existing path.
  - Deps: none. Lowest-risk, independent.

- **LM.1 ✅ (`2ced3d9e`) — `ActionContext` exposes the buffer's locals.**
  - Pivoted from the original registry design (rejected mid-build: it
    dropped oil/tree state from `:describe-buffer` introspection; see
    design §3.1). Instead: `ActionContext` gains
    `buffer_locals: Option<&BufferLocals>` + a `buffer_local::<T>()`
    accessor. State stays in `Editor::buffer_locals` — no relocation, no
    service, no `Editor` field, no dual-boot wiring.
  - Host chord-dispatch site passes the active buffer's locals; auxiliary
    firing paths (prompt/transient/confirm) pass `None`; the accessor
    degrades to `None`. Every existing `ActionContext { .. }` literal
    (magit/snippet/lsp/plugin-manager/tests) gains `buffer_locals: None`.
  - Test: a handler reads its buffer's local via `ctx.buffer_local::<T>()`;
    `None` on a no-locals path. Introspection preserved (the 6 tests that
    assert oil/tree locals via `iter_descriptors` keep passing, untouched).
  - Deps: none (parallel to LM.0); LM.3/LM.4 consume the accessor.

- **LM.2 📝 — re-list / toggle data-effects.**
  - `Effect::OilNavigate { view, dir }` (re-list oil `view` to `dir`),
    `Effect::FileTreeToggle { view, entry_index }`. Appliers reuse
    `write_oil_listing` / `set_file_tree_entries` (now via the registry)
    + `replace_owned_buffer` owner-write; reset cursor/scroll for that
    view. Both renderers classify (host-applied, peer no-op).
  - Async re-list cursor landing via `CursorMoveIn { target, position }`
    (design §3.2).
  - Test: each effect re-lists/toggles the named view only; a second
    listing buffer is byte-for-byte unchanged (independence guard).
  - Deps: LM.1.

- **LM.3 📝 — oil-mode owns its surface.**
  - `oil-mode` keymap: `<CR>`, `-`, `<C-s>`, `<C-v>`, `<C-t>` at
    `MajorMode(oil-mode)` via `Keymap::from_entries`. Register the
    `action:oil-*` command names in a `lattice-listing` `install` path.
  - Mode-owned handler closures (read the buffer's locals via
    `ctx.buffer_local::<T>()`, resolve the entry at `ctx.cursor`, emit
    `OpenInTarget` / `OpenBufferAt` / `OilNavigate` / `OpenOil`). File →
    open (targeted); dir → re-list (`<CR>`/`-`) or oil-in-target
    (`<C-s/v/t>`).
  - Delete the `BufferKind::Oil` gate block in `input.rs`, the
    `Action::FollowLink` Oil arm, and `Editor::do_oil_follow` /
    `do_oil_navigate_up` (bodies now live in effect appliers / handler).
  - Test: chords resolve only in oil buffers; open-in-split works;
    grammar (`gg`/motions) still resolves; two oil buffers independent.
  - Deps: LM.0, LM.1, LM.2.

- **LM.4 📝 — file-tree-mode owns its surface.**
  - Same shape for `file-tree-mode`. **Split `FileTree` out of the shared
    `Help | FileTree` gate block** in `input.rs` — Help + Dashboard keep
    Esc-dismiss + `<CR>`-follow unchanged; only file-tree's keys migrate.
  - Delete the `Action::FollowLink` FileTree arm + `do_file_tree_follow`.
  - Test: file-tree chords scoped; Help/Dashboard unaffected (regression
    guard); two file trees independent (toggling one leaves the other
    unchanged).
  - Deps: LM.0, LM.1, LM.2 (LM.3 optional, but they share the
    handler-registration shape).

## Artefact checklist (heuristic #5, per slice)

- Design fragment: `listing-mode-ownership.md` (done; update if the
  design itself changes, e.g. a rejected alternative becomes chosen).
- Tests: per the design §8 test contract, including the independence
  guards and the Help/Dashboard regression guard.
- Benchmarks: none new unless the keystroke→glyph ratchet regresses
  (design §9); watch CI.
- Error handling: log + skip on a listing that fails to reload (fs
  error), never panic on the dispatch path; unresolved `action:*` names
  `warn!` + skip at boot (existing K.2.4 behaviour).

## Cross-refs to repoint when LM lands

- `directory-listing-mode.md` §2 ("entry navigation is major-owned") →
  link here for the how.
- `docs/dev/operations/implementation.md` ledger → add the LM series.
- mode-architecture.md §13 Oil cleanup-debt → mark closed when LM.3/LM.4
  land.
