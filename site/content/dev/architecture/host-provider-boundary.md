+++
title = "Host ↔ provider boundary"
+++

# Host ↔ provider boundary

> **The decision.** `lattice-host` owns the substrate **and the core features**;
> only genuinely-separable **feature-buffers** are inverted out so the host
> cannot name them. The line is drawn on merit — *"can the editor be itself
> without this, and is it meant to be replaceable?"* — not on a reflexive
> "everything is a provider." Enforcement moves from convention + tests toward
> the type system where it cheaply can, and stops where it would cost real
> flexibility, performance, or UX.

Sequencing + status: [`../operations/slice-plans/host-provider-inversion.md`](../operations/slice-plans/host-provider-inversion).
Motivation: the recurring-drift cost analysed in [`comparison-zed.md`](comparison-zed) §5.

## 1. The boundary

**Core — host owns, holds state directly, no indirection, no perf compromise:**

| | Why core |
|---|---|
| text / grammar / rendering substrate, the registries | the editor *is* these |
| **LSP** | UX-critical language intelligence; the `Editor` legitimately holds its state (624 refs); native like the grammar — the plugin *seam* lets others add intelligence, the built-in stays native |
| **diff** | a committed UX; the diff subsystem is core editing |
| **terminal** | an integrated dev-editor concern; the `SyntheticDoc` seam is core (terminal-as-Document) |
| **snippet** | the `Editor` holds `snippet_registry` / activation policy / completion-meta; woven into the core completion pipeline; the completion-*source* seam already lets plugins add sources |
| multibuffer-substrate, completion/picker, syntax (tree-sitter), folds, messages, help | substrate or core editing |

**Feature-buffers — invert; host must not name them:**

| | Why a feature |
|---|---|
| **oil** | a removable filesystem-editor buffer; **zero** `Editor`-struct coupling — pure `run_oil_invocation` dispatch; the editor is fully itself without it |
| **file-tree** | a removable navigation buffer; same shape as oil (zero struct coupling, `run_file_tree_invocation`) |

(narrow / search providers already live in `lattice-multibuffer/src/providers/` — out of host.)

**Why this line and not "zero providers".** Lattice's own philosophy already
accepts native-core that the host depends on — *"built-in vim grammar stays
native; WASM is for extensions."* LSP / diff / terminal / snippet sit in that
same category: central, UX-defining, performance-sensitive concerns the host
legitimately owns. Forcing them behind `Arc<dyn>` would pay indirection on hot,
core paths to abstract things that will not be swapped — purity without a merit
win (heuristic #1's explicit anti-pattern). The plugin *seam* (registries / WIT)
makes built-ins and plugins **symmetric where it matters** — contribution — not
by demoting every built-in to a plugin.

## 2. The enforcement mechanism — dependency inversion

`lattice-host` drops its `lattice-oil` and `lattice-file-tree` dependencies. The
`do_oil_*` / `do_file_tree_*` / `run_*_invocation` bodies move into their crates
as `ActionHandler` closures registered by `OilMode` / `FileTreeMode`, reaching
the editor through the **`ActionContext`** generic-primitive surface (§3). After
this, `Editor::do_oil_*` is a **compile error** — the types are not in scope.

**Invariant:** `lattice-host/Cargo.toml` lists neither `lattice-oil` nor
`lattice-file-tree`. Checkable in CI; ultimately a fact of the build graph.

> **A "sealed constructors" mechanism was considered and dropped (2026-06-17)
> — it did not survive the code.** Type-sealing the drift classes sounded
> complementary, but each sub-idea collapsed on inspection: keymap-at-Builtin is
> *already* sealed (`keymap_entry!` carries no layer; `PushLayerKind` has no
> `Builtin` variant; modes hold no `KeymapHandle`); the one renderer
> `match buffer_kind` (`render.rs:2064`) is the *permitted* aligned-by-fallback
> form, not a violation; and **sealing the `Document` trait would be
> anti-extensibility** — providers and future plugins MUST implement it for
> their buffer kinds. So dependency inversion is the one genuine
> type-enforcement win. Full analysis in the slice plan.

## 3. The `ActionContext` primitive surface — the load-bearing piece

For the feature handlers to live in their crates without `lattice-host`, the
generic editor primitives they call (pane open, directory navigate,
`active_pane_buffer_id`, buffer create/open, …) must be exposed on the
**`ActionContext`** / host-service traits in `lattice-mode`. This surface is not
incidental: **it is the WASM host API in embryo.** What a feature-buffer needs
from the core is what a plugin needs — so designing it cleanly here is the
plugin dividend, paid early. If the surface turns out large, that slice (HPI.3)
splits rather than bolting primitives on ad hoc.

## 4. What is NOT in scope (deliberately)

- **Inverting LSP / diff / terminal / snippet.** Core by the §1 decision.
- **Type-enforcing the *residue*.** After (a)+(b), two things stay
  convention-checked: that the small, stable set of *core* features contribute
  decorations/status through the registry rather than reading directly; and that
  a migration is *complete*. The first is low-frequency; the second is a
  fundamental limit of any type system. Pushing further — e.g. making
  render-state feature data unreadable except through the seam — would inject
  indirection into the hot render path for cosmetic gain. **Not worth the cost
  to flexibility / UX.** Accepted as residue.

## 5. Rejected alternatives

- **Blanket "host depends on zero provider crates" (incl. LSP).** Rejected —
  purity at the cost of hot-path indirection on genuinely-core concerns; not
  asked for by lattice's own native-core philosophy.
- **"Just add more architecture tests."** That is the status quo and the cost
  being reduced. Demoted to a backstop for the completeness residue.
- **One big-bang extraction.** Rejected — sliced per feature, each green and
  revertible.

## 6. Paramount-goal alignment

- **#1 Performance** — neutral; the seam is `ActionHandler` + `ActionContext`
  (already on the dispatch path); core features keep direct state, no new
  indirection.
- **#2 Extensibility** — the load-bearing win: built-in feature-buffers use the
  exact seam plugins will; the `ActionContext` surface *is* the WASM API draft.
- **#3 Vim modal editing** — strengthened: "mode keymap at Builtin" becomes a
  compile error.
- **#4 Asynchronicity** — unaffected.

## 7. UX (higher court)

Zero user-visible change by construction. Pure structural refactor, sliced so
each step lands green with behaviour preserved. Any visible change is a
regression, not a feature.
