# Host ↔ provider inversion — slice plan

> **Status: 🚧 active (started 2026-06-17).** Sequencing companion to
> [`../../architecture/host-provider-boundary.md`](../../architecture/host-provider-boundary.md)
> (the *what + why*: the core/feature boundary + the two enforcement
> mechanisms). This file owns *when + in what order + status*.

**Scope (locked, narrowed 2026-06-17):** extract **oil** and **file-tree** out
of `lattice-host` via dependency inversion. **Core stays core** — LSP, diff,
terminal, snippet, multibuffer-substrate are NOT inverted. No performance
compromise. Zero user-visible change.

**Invariant at the end:** `lattice-host/Cargo.toml` depends on neither
`lattice-oil` nor `lattice-file-tree` — so `Editor::do_oil_*` / `do_file_tree_*`
stop compiling (the types leave host scope).

> **The "sealed constructors" mechanism was dropped (2026-06-17) — it didn't
> survive the code.** The original plan paired the extraction with
> type-sealing the drift classes. On inspection all three sub-ideas
> collapsed:
> - **Keymap-at-Builtin is already sealed.** `keymap_entry!` / `KeymapEntry`
>   carries no layer; `PushLayerKind` has no `Builtin` variant; no mode crate
>   holds a `KeymapHandle`. A mode literally cannot express "Builtin" through
>   any API it has. A private field on `KeymapLayer::Builtin` would only close
>   a theoretical residual (the `pub bind(raw layer)`) nothing can reach,
>   at a ~54-site churn — not worth it.
> - **Kind-branching is already disciplined.** The one renderer
>   `match buffer_kind` (`render.rs:2064`) is the permitted aligned-by-fallback
>   form (Document → cursor hot-slot; `_` → pane state) with the required
>   enumeration comment. Compliant, not a violation.
> - **Sealing `Document` would be *anti-extensibility*.** Providers (and future
>   plugins) MUST implement `Document` for their buffer kinds — that is
>   "everything is a buffer." Sealing it blocks the exact extensibility it was
>   meant to protect. `#[non_exhaustive] Action` likewise breaks internal
>   dispatch for nil gain (provider Action variants are prevented by the
>   inversion, not by non_exhaustive).
>
> So the one genuine type-enforcement win is the **dependency inversion** —
> nothing else.

## Slices

| Slice | Title | Status |
|-------|-------|--------|
| **HPI.1** | `ActionContext` primitive audit + extension (the WASM-API surface) | 🚧 |
| **HPI.2** | Extract **oil** → `lattice-oil`; drop the host dep; CI guard | 🗒 |
| **HPI.3** | Extract **file-tree** → `lattice-file-tree`; drop the host dep; CI guard | 🗒 |
| **HPI.4** | Rewrite `comparison-zed.md` §5 to the earned end-state; ledger + memory | 🗒 |

HPI.1 gates HPI.2/3 (the handlers can't move until the primitives they need are
on `ActionContext`). HPI.2 is the template; HPI.3 mirrors it.

---

### HPI.1 — Seal `KeymapLayer::Builtin`

**Goal.** Make "a mode registered a chord at the universal `Builtin` layer"
a compile error instead of a drift caught by tests.

**What lands.**
- `KeymapLayer::Builtin` gains a private field / `BuiltinToken` so it is not
  constructible outside the host's builtin registrar module.
- The mode-facing registration API (`register_mode_keymap(handle, mode_id, …)` /
  the `Keymap` builder path used by `Mode::keymap()`) can only target
  `MinorMode(mode_id)` / `MajorMode(mode_id)`.
- The host's own builtin catalog (`keymap_normal` / `keymap_visual` / … —
  genuine universal vim grammar) keeps a privileged constructor inside the
  registrar.

**Tests.** A compile-fail (`trybuild`) or API-shape test proving a mode crate
cannot produce a `Builtin` binding; existing builtin registration unaffected;
full keymap suite green.

**Risk.** Low — additive visibility tightening. Audit current `Builtin`
construction sites first; any mode-side one is a *pre-existing violation* to
relocate to `MinorMode` as part of this slice.

---

### HPI.2 — Seal `Document` + `#[non_exhaustive] Action`

**What lands.**
- `Document` becomes a sealed trait (sealed-supertrait pattern) — removes the
  foothold for kind-branching that constructs/matches outside the intended set.
- `Action` gains `#[non_exhaustive]`; confirm `Effect::Action(AppEffect{ActionId})`
  is the documented canonical extension path.

**Honest scope.** This does NOT stop a *host* dev adding a core `Action` variant
(nor should it). It steers extensions to `ActionId` and prevents external/
provider variants once provider types leave host scope (HPI.4/5).

**Tests.** Internal exhaustive matches still compile; a sealed-trait negative
test; suite green.

---

### HPI.3 — `ActionContext` primitive audit + extension

**Goal + why it matters.** The handlers in HPI.4/5 must reach the editor through
generic primitives only — this surface IS the embryonic WASM host API.

**What lands.**
- Inventory every `self.<primitive>()` the `do_oil_*` / `do_file_tree_*` /
  `run_oil_invocation` / `run_file_tree_invocation` bodies call (seed:
  `active_pane_buffer_id`; expect pane-open, dir-navigate, buffer create/open).
- Classify: already on `ActionContext` / a host-service trait → reuse; missing →
  add to the `ActionContext` surface in `lattice-mode` (or the relevant
  host-service trait), implemented by the host.
- No behaviour change — pure surface exposure.

**Risk.** This is the load-bearing slice. If the missing-primitive set is large
or entangles pane mutation semantics, **split HPI.3** rather than bolt on. Tests:
each new primitive has a unit test against the host impl.

---

### HPI.4 — Extract oil

**What lands.**
- Move `do_oil_follow` / `do_oil_navigate_up` / `run_oil_invocation` bodies from
  `lattice-host/src/dispatch.rs` into `lattice-oil` as `ActionHandler` closures
  registered by `OilMode` (using HPI.3 primitives via `ActionContext`).
- Delete the host methods + the `oil.rs` shim (7 lines).
- Remove `lattice-oil` from `lattice-host/Cargo.toml`.
- CI guard asserting host does not re-add the dep.

**Tests.** Oil follow / navigate-up work end-to-end through the registry;
`:Oil` / `-` behaviour preserved; host source references nothing `lattice_oil::`.
TUI + GPUI parity if any paint path is touched (oil is a Document — likely none).

**Risk.** Low-moderate — mechanical once HPI.3 lands. The oil↔file-tree shared
ref (1 site) is factored here or in HPI.5.

---

### HPI.5 — Extract file-tree

Mirror of HPI.4 for `do_file_tree_*` / `run_file_tree_invocation` →
`lattice-file-tree` / `FileTreeMode`. Drop `lattice-file-tree` from host
`Cargo.toml`; CI guard. Tests: file-tree follow / expand / collapse preserved;
host names nothing `lattice_file_tree::`.

---

### HPI.6 — Close

- Rewrite [`../../architecture/comparison-zed.md`](../../architecture/comparison-zed.md)
  §5 to the earned end-state: the recurring drift classes (feature leakage,
  Builtin keymaps, provider actions) are now compile errors; a narrow residue
  (core-feature surface contribution; migration completeness) stays
  convention-checked, the latter being a limit of any type system.
- Update `implementation.md`; add a project memory recording the boundary
  decision (core vs feature) so it isn't re-litigated.

## Four-artefact discipline

Each slice ships green-on-merge with: the design fragment kept in sync, tests
(incl. the compile-fail / negative tests that ARE the new guarantee), no new
bench needed (registration-time only, no hot-path change), graceful handling
(behaviour preserved; the migration is mechanical).

## Cross-references

- Boundary decision + rejected alternatives:
  [`../../architecture/host-provider-boundary.md`](../../architecture/host-provider-boundary.md).
- Precedent: `mode-ownership-cleanup` (archived) did the *surface*-contribution
  half (keymaps/decorations/status); this arc does the *crate-dependency* half.
- The drift evidence motivating it: `comparison-zed.md` §5 + the 2026-06-17
  audit (`do_oil_*` / `do_file_tree_*` host methods are the present violations).
