+++
title = "Multibuffer is a regular buffer (integration audit)"
+++


**Status:** 2026-06-01 — K.4 audit, in progress. Triggered by
the M.6 testing arc that surfaced four latent integration
failures (silent EventBus lookup, current_thread freeze,
`contains_document` excluding Multibuffer, vim grammar
broken on multibuffer views). All four are the same anti-
pattern: M.2 shipped declaring Multibuffer support but no
test exercised the end-to-end "is this actually a regular
buffer" property.

This document is the authoritative answer to *"what does it
mean for `BufferKind::Multibuffer` to be a regular buffer?"*
It enumerates every seam where the integration could branch
on kind, classifies each as Aligned / Excluded / Unclear,
and lists the required change.

## 1. Contract

A `BufferKind::Multibuffer` buffer is **a regular buffer**
in the sense of `feedback_buffers_no_special_case`:

- It carries a polymorphic `Document` handle (M.2.b.1) that
  can be read and (where the mode permits) mutated.
- It participates in the activation pipeline the same way
  Document/Messages do: `:b N`, `:bn`/`:bp`, position
  history, pane snapshot/restore.
- It receives keystrokes through the standard dispatch
  path: motions, operators, text-objects, visual mode,
  insert mode (if its active minor allows writes).
- It renders through the standard cells / virtual-row /
  syntax-highlight pipelines.
- Its UX differs only where a minor mode it carries
  contributes a different rule (e.g.
  `ProjectSearchMultibufferMode` setting `ReadOnly = true`
  to reject writes — same mechanism Help / Oil / FileTree
  use).

Anywhere a code path branches on `BufferKind::Multibuffer`
and treats it *less* uniformly than `BufferKind::Document`
without a written reason, that's a bug.

## 2. Seam audit

Enumerated via:

```sh
grep -rn "BufferKind::Multibuffer\|BufferData::Multibuffer" \
  crates/lattice-host/src/ \
  crates/lattice-multibuffer/src/ \
  crates/lattice-ui-tui/src/ \
  crates/lattice-ui-gpui/src/
```

35 sites (lattice-host + lattice-multibuffer). **Zero sites
in lattice-ui-tui or lattice-ui-gpui** — see §3 for why
that's the danger zone.

### 2.1 Constructor / registry plumbing

| Site | Behavior | Verdict |
|---|---|---|
| `buffer_registry.rs:125` | `BufferData::Multibuffer(_)` → `BufferKind::Multibuffer` | ✅ Aligned |
| `buffer_registry.rs:137,146` | `document()` / `document_mut()` accept Multibuffer | ✅ Aligned |
| `buffer_registry.rs:475` | `contains_document` post-K.4-fix includes Multibuffer | ✅ Aligned (this commit) |
| `buffer_registry.rs:936` | Boot insert routes Multibuffer kind to `BufferData::Multibuffer` | ✅ Aligned |
| `buffer_registry.rs:1159–1175` | M.2.b.0.A regression test | ✅ Aligned |
| `view.rs:137,144` | `create_multibuffer_view` inserts with kind tag + activates major | ✅ Aligned |
| `mode.rs:60` | `MultibufferMode::target_buffer_kind()` returns Multibuffer | ✅ Aligned |
| `editor_boot.rs:280` | Boot registers MultibufferMode | ✅ Aligned |
| `modes.rs:417` | Boot `major_mode_id_for_buffer_kind(Multibuffer)` | ✅ Aligned |

### 2.2 Dispatch — active state accessors

| Site | Behavior | Verdict |
|---|---|---|
| `dispatch.rs:4099` (`active_buffer_id`) | Returns pane id (uniform) | ✅ Aligned |
| `dispatch.rs:4144` (`active_text`) | Returns `self.document.snapshot().buffer` (uniform) | ✅ Aligned |
| `dispatch.rs:4178` (`active_cursor`) | Returns `self.cursor` (uniform) | ✅ Aligned |
| `dispatch.rs:14810` (registry contains) | Includes Multibuffer | ✅ Aligned |

### 2.3 Dispatch — pane-switch / activation

| Site | Behavior | Verdict |
|---|---|---|
| `dispatch.rs:14843` (position-history jump) | `Multibuffer => activate_document` (uniform with Doc/Messages) | ✅ Aligned |
| `dispatch.rs:21758` (pane stash hot-path) | `Multibuffer => {}` (uniform with Doc/Messages — both empty) | ✅ Aligned |
| `dispatch.rs:21907` (`activate_buffer`) | Routes Multibuffer to `activate_document` | ✅ Aligned |
| `dispatch.rs:1861, 2063` (action-arm empty cases) | Empty, uniform with Doc/Terminal | ✅ Aligned |

### 2.4 Dispatch — introspection / pickers (presentation only)

| Site | Behavior | Verdict |
|---|---|---|
| `dispatch.rs:21056` (`:ls` listing) | Multibuffer paired with `Messages`, labelled `msg` | ⚠ Cosmetic — should be its own row with `mb` label, not folded into msg |
| `dispatch.rs:22991` (buffers picker rows) | Multibuffer gets `mb` prefix correctly | ✅ Aligned |
| `dispatch.rs:23124` (picker entry) | `mb` kind label | ✅ Aligned |

### 2.5 Mode defaults

| Site | Behavior | Verdict |
|---|---|---|
| `modes.rs:273` (`default_minor_mode_id_for_buffer_kind`) | `Multibuffer => None` (uniform with Doc — Multibuffer's minor is added explicitly by `project_search`, not by default) | ✅ Aligned |
| `modes.rs:313` (`auto_added_minor_modes_for_buffer_kind`) | `Multibuffer => Vec::new()` (uniform with Doc) | ✅ Aligned |

### 2.6 Renderer (lattice-ui-tui) — **danger zone**

Renderer has **zero** mentions of `BufferKind::Multibuffer`.
Has 30+ mentions of `BufferKind::Document`. That gap is
the source of the user-visible breakage. Two distinct sub-
patterns to disentangle:

- **(a) Document-or-anything-else** — code says "if active
  is Document do A; else do B". Multibuffer correctly
  takes the `else` branch when `else` is the kind-agnostic
  fallback. These are aligned by accident; an audit
  comment is enough.
- **(b) Document-only** — code says "if active is Document
  do A; else skip A." Multibuffer silently loses
  functionality. **These are the real bugs.**

Concrete instances found:

| Site | Pattern | Verdict | Required change |
|---|---|---|---|
| `render.rs:1848–1858` (popup cursor anchor) | (a) — falls back to `pane.cursor / pane.scroll` for non-Document kinds. Reads from the pane state, which `activate_document` keeps current. | ✅ Aligned (audit comment) | None |
| `render.rs:2708–2715` (syntax highlight `active_doc_id`) | (b) — `active_doc_id = Some(id)` only if `Document`, else `None`. With `None`, the fallback path at 2724 (`active_doc_id == Some(pane.buffer_id) && scroll matches`) is dead, and the renderer hits the empty-highlights branch at 2706. Multibuffer rendered without tree-sitter highlights. | ❌ Bug — Multibuffer should also resolve through the active-doc syntax path | Extend the matcher to include Multibuffer; treat the multibuffer's virtual document like Document for the purpose of resolving its syntax cell |
| `app/dispatch.rs:219` (popup state-A check) | (b)? — `popup_in_state_a` only true if `pre_active == Document`. Effect: hover popups don't trigger their State-A dismissal on Multibuffer. | ⚠ Likely intentional — hover overlays are LSP-driven on Document panes — verify | Document why Multibuffer is excluded (LSP isn't active) or extend |
| `app/dispatch.rs:610, 620` (LSP-related document-kind gates) | (b) — gates LSP behaviour to Document only. Probably intentional (LSP doesn't run on multibuffer views per memory `feedback_buffers_no_special_case` exception). | ⚠ Document the intent | Add comment naming the exception |
| `input.rs:1594` (active_buffer dispatch?) | unclear — needs read | ⚠ Unread | Audit |

### 2.7 Cells / virtual-row matrices

Renderer reads from `cells_matrix_for(buffer_id)` and
`virtual_rows_matrix_for(buffer_id)` (`editor.rs:1449,1477`)
— both keyed by ID, kind-agnostic. The matrix entries
themselves are populated by the cells worker.

**Open question (test required):** does the cells worker
populate cells for Multibuffer's virtual document the same
way it does for Document? If the worker gates on
`BufferKind::Document` anywhere, the multibuffer's cells
matrix stays empty → the user sees nothing.

This is the single highest-impact open question — investigate
in K.4.2's audit pass.

### 2.8 Excerpt headers (virtual-row drawing)

Excerpts already carry `ExcerptHeader::new(format!("{}",
path.display()))` per `providers/search.rs:449`. The virtual-
row matrix has slots for excerpt headers (M.2.b.0). User
report: no file labels visible. Two hypotheses:

- (a) Virtual-row matrix not being populated for Multibuffer
  (linked to §2.7).
- (b) Virtual-row matrix is populated but the renderer's
  virtual-row drawing pass is gated on a kind it excludes.

Bisect via the integration test in K.4.1.

## 3. Why renderer-side absence is the danger

When `lattice-host` is full of explicit `Multibuffer`
arms, the integration is *visible* — if you add a new
operation and forget Multibuffer, the match expression
fails to compile (because Rust's exhaustiveness checker
flags it). When `lattice-ui-tui` has zero mentions of
Multibuffer, the integration is *invisible* — code says
"if Document do X" with an implicit "else do nothing" or
"else fallback", and the failure mode is silent visual
breakage, never a build-time error.

This is the reverse of M.6.0's bench problem — the bench
existed but measured the wrong thing. Here the kind tag
exists but the renderer paths never look at it, so the
"this code touches Multibuffer" question never gets asked.

## 4. Required artefacts (K.4 slice plan)

See [slice plan: multibuffer-is-a-regular-buffer](../operations/slice-plans/archive/multibuffer-is-a-regular-buffer).

- **K.4.0** Audit doc (this file).
- **K.4.1** Integration test that drives a multibuffer
  through `j` / `k` / `gg` / `G` / `w` / `b` / `]e` / `[e`
  chords on a real Editor + asserts cursor advances + cells
  matrix is populated + virtual-row matrix carries excerpt
  headers. Failing test = the bug enumeration.
- **K.4.2** Cells/virtual-row worker audit — does the
  worker populate matrices for Multibuffer kind, or does it
  gate on Document? Single highest-impact answer.
- **K.4.3** Render syntax-cell gate (`render.rs:2708`) — fix
  `active_doc_id` resolution to include Multibuffer.
- **K.4.4** `:ls` listing format — give Multibuffer its own
  row with `mb` label, not folded into `msg`.
- **K.4.5** Audit comment pass — every aligned-by-fallback
  renderer arm gets an explicit comment naming Multibuffer
  as covered, so the future Rust author doesn't add a
  Document-only gate by accident.

## 5. Convention codification

After K.4 lands, update `feedback_buffers_no_special_case`
with a "this is what 'no special case' looks like in
practice" section:

- The integration test from K.4.1 is the canonical bar.
  Any new buffer kind (current count: 7 — Document, Help,
  FileTree, Oil, Terminal, Messages, Multibuffer) must
  either pass the test verbatim *or* document each chord
  whose behaviour is intentionally different and why.
- Renderer paths that gate on `BufferKind` carry a comment
  explicitly listing every kind that hits the branch — so
  the next reader sees "Document, Messages, Multibuffer
  fall through here" instead of guessing what the empty
  `else` means.

## See also

- [`kind-agnostic-buffers.md`](kind-agnostic-buffers) — H-series, upstream of K.4 (generic kind infrastructure).
- [`multibuffer-views.md`](multibuffer-views) — M-series, what K.4 verifies.
- [`feedback_buffers_no_special_case`](../../../.claude/projects/-home-h4x0rdud3-src-dhruvasagar-lattice/memory/feedback_buffers_no_special_case).
