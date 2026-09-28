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

- **LM.0 📝 — `OpenInTarget` effect primitive.**
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

- **LM.1 📝 — `ListingRegistry` service + state relocation.**
  - New `lattice-listing::registry`: `ListingRegistry` trait +
    `InMemoryListingRegistry` + `ListingRegistryHandle = Arc<dyn ...>`,
    keyed `BufferId → ListingState` (oil `{dir,snapshot}` / file-tree
    `{root,entries,nerd_fonts}`), `RwLock` interior mutability. Mirror
    `MultibufferRegistry`.
  - `lattice-listing::install(boot)` registering the handle in
    `ServiceRegistry`; wire into `editor_boot`. `DocumentClosed`
    subscriber for cleanup (multibuffer precedent).
  - Relocate oil/file-tree state out of `Editor::buffer_locals` into the
    registry as source of truth; repoint host accessors
    (`oil_dir_for`/`oil_snapshot_for`/`file_tree_entries_for`/…), host
    writers (`set_oil_dir`/`set_oil_snapshot`/`set_file_tree_entries` +
    open/close paths), and renderer presentation reads.
  - **Pure refactor — no behaviour change.** All existing oil/file-tree
    tests stay green; that is the slice's proof. Split LM.1a (oil) /
    LM.1b (file-tree) if the diff is too large to review as one.
  - Deps: none (parallel to LM.0), but LM.3/LM.4 need it.

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
  - Mode-owned handler closures (read `ListingRegistry` via
    `ctx.services`, resolve the entry at `ctx.cursor`, emit
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
