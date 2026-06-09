# Mode ownership cleanup — slice plan

Sequencing companion to
[`docs/dev/architecture/mode-architecture.md`](../../architecture/mode-architecture.md)
§13. The architecture fragment owns *what + why* (the principle:
modes own their full surface — keymaps, decorations,
subscriptions, status-line, completion sources, options); this
file owns *when + in what order + status*.

The cleanup migrates feature-specific surface that today lives
host-side into the mode that gates it. K.1.c's per-keystroke
filter then scopes the chord to mode-active buffers
automatically. Surfaced 2026-06-01 during the M.6.1
SearchProvider review (`gr` shadow check) — modes are
under-utilised and the host owns lots of mode-specific surface
that should live in the mode itself.

## Scheduling constraint

**Each phase lands AFTER the M-series multibuffer work
completes** (M.6.2 / M.7 / M.8 / A.1+ provider catalogue).
Each phase is independently revertible. Touching the same
per-area code as in-flight M-series slices would create churn
and merge conflict; the deferral has a clear unblock signal
(M-series done) so this isn't open-ended waiting.

## Phases

| Slice | Title | What lands |
|-------|-------|------------|
| **MO.1** | ✅ LSP keymap migration | Move 7 bindings (`K`, `gd`, `gD`, `gy`, `gI`, `gr`, `gx`) from `keymap_normal.rs` Builtin layer to a new `register_lsp_mode_keymap(handle, action_ids)` helper in `lattice-lsp/src/modes.rs`, alongside `LspMode`. Boot calls it under `PushLayerKind::MinorMode(LspMode::mode_id())`. Drop the 7 entries from `keymap_normal.rs`. Tests verify K.1.c filtering — bindings fire only when `lsp-mode` is in the active buffer's `ActiveModes`. Touches: `lattice-lsp/src/modes.rs`, `lattice-host/src/keymap_normal.rs`, `lattice-host/src/editor_boot.rs`. |
| **MO.2** | ✅ Oil keymap migration | Single binding (`-` → `oil_navigate_up`). Added `oil_mode_keymap_entries()` + `fn keymap()` to `OilMode` in `lattice-oil/src/modes.rs`. Dropped `handle.bind(- → oil_navigate_up)` from `keymap_normal.rs`. K.2.4 registers the entry at `KeymapLayer::MinorMode("oil-mode")`; K.1.c scopes it to oil buffers automatically. Touches: `lattice-oil/src/modes.rs`, `lattice-host/src/keymap_normal.rs`. |
| **MO.3** | ✅ Snippet keymap migration | Added `SnippetActiveMode` (minor) in `lattice-snippet/src/modes.rs` with `fn keymap()` contributing `<Tab>`/`<S-Tab>`/`<Esc>` Insert-mode bindings. `register_snippet_modes` now also registers it. Replaced `push_layer`/`pop_layer` in `sync_keymap_overlays` with `activate_minor`/`deactivate_minor`. Removed `active_snippet_layer_bindings`, `active_snippet_mode_id`, and `Editor::snippet_layer` field. K.2.4 registers the entries statically; K.1.c gates them to snippet-active buffers. TUI test helpers updated to use `translate_mode_keymaps` (K.2.4 path). Touches: `lattice-snippet/src/modes.rs`, `lattice-host/src/dispatch.rs`, `lattice-host/src/editor.rs`, `lattice-host/src/keymap_insert.rs`, `lattice-ui-tui/src/keymap_insert.rs`, `lattice-ui-tui/src/input.rs`. |
| **MO.4** | 🚫 Decoration / subscription / status-line audit | **Blocked — infrastructure stubs not yet landed.** Design pass (2026-06-09) identified three sub-areas, all pre-emptive: **MO.4.a** (`DecorationProvider` is `_private:()` stub in `lattice-mode/src/contributions.rs`; annotated "M.4 replaces with the real type" — gutter-sign migration blocked on M.4 decoration registry); **MO.4.b** (`Mode::status_line_items()` does not exist; `pane_status_label` has a "can later route through mode-contributed renderers" comment but no hook; LSP-ready / diff-stats / macro-indicator not yet in the per-pane status line at all — new API + renderer wiring required); **MO.4.c** (`Subscription` is `_private:()` stub annotated "when the typed event bus stabilises"; five host-side `subscribe_typed` calls in `editor_boot.rs` for `LspProgressUpdate`, `LspDiagnosticRefresh`, `LspSemanticTokensRefresh`, `LspCodeLensRefresh`, `LspInlayRefresh` cannot be moved until that type lands). **Unblock signals:** M.4 decoration registry lands → MO.4.a; `Subscription` type + event-bus shape finalised → MO.4.c; both + `Mode::status_line_items()` designed → MO.4.b. |

## Audited cluster details (the targets)

Audited via grep against `crates/lattice-host/src/keymap_*.rs`
on 2026-06-01:

### LSP — 7 bindings at Normal Builtin (MO.1)

| Chord | Action | Source line | Notes |
|---|---|---|---|
| `K` | `lsp_hover_request` | `keymap_normal.rs:319` | Hover popup |
| `gd` | `lsp_definition_request` | `keymap_normal.rs:535` | Go-to-definition |
| `gD` | `lsp_declaration_request` | `keymap_normal.rs:542` | Go-to-declaration |
| `gy` | `lsp_type_definition_request` | `keymap_normal.rs:549` | Go-to-type |
| `gI` | `lsp_implementation_request` | `keymap_normal.rs:556` | Go-to-implementation |
| `gr` | `lsp_references_request` | `keymap_normal.rs:563` | References picker |
| `gx` | `lsp_follow_link_at_cursor` | `keymap_normal.rs:571` | documentLink follow |

LSP auto-activates path-driven
(`maybe_auto_activate_lsp_mode` short-circuits on no-path
buffers like multibuffer views), so these bindings are
*de facto* no-ops outside LSP-attached buffers today — but
they STILL fire the `Action::Lsp*` dispatch and reach the
supervisor before the no-op happens. Mode-scoped registration
short-circuits at the keymap layer instead.

### Oil — 1 binding at Normal Builtin (MO.2)

| Chord | Action | Source line | Notes |
|---|---|---|---|
| `-` | `oil_navigate_up` | `keymap_normal.rs:389` | Oil parent-directory chord |

### Snippet — 4 bindings at Insert Builtin (MO.3)

| Chord | Action | Source line | Notes |
|---|---|---|---|
| `<Tab>` | `snippet_expand` | `keymap_insert.rs:192` | Expand template at cursor |
| `<Tab>` | `snippet_next_placeholder` | `keymap_insert.rs:371` | Move to next placeholder |
| `<S-Tab>` | `snippet_prev_placeholder` | `keymap_insert.rs:383` | Move to prev placeholder |
| `<Esc>` | `snippet_leave` | `keymap_insert.rs:389` | Exit snippet session |

The runtime `is_snippet_active` boolean check is a
poor-person's keymap layer. A `MinorMode(snippet-active-mode)`
layer with K.1.c precedence does this cleanly.

## Test discipline (per phase)

Each phase ships green-on-merge with:

- **Convention check** — the moved bindings appear at
  `MinorMode(<mode>::mode_id())` and DO NOT appear at
  `KeymapLayer::Builtin`. A unit test resolving the chord in a
  buffer without the mode active confirms the binding is
  inactive there.
- **Behaviour preservation** — a test activating the mode on
  a buffer + firing the chord + asserting the same downstream
  action runs. The action handler itself doesn't change.
- **Negative test** — a test confirming the chord is *not*
  resolvable on a buffer without the mode (e.g. multibuffer
  view + `gr`). This is the load-bearing user-visible win.
- **Graceful error handling** — same handlers as today; no
  new error paths introduced. The migration is mechanical.

No new benches needed. Keymap resolution is K.1.c per-keystroke
filter with no measurable per-binding cost — moving 7 bindings
between layers doesn't move the needle.

## What unblocks each phase

- **MO.1** depends on: M-series multibuffer work complete
  (M.6.2 + M.7 + M.8 done). No internal dep — LSP keymap is
  self-contained.
- **MO.2** depends on: M-series done. No dep on MO.1.
- **MO.3** depends on: M-series done + a design call on the
  `snippet-active-mode` shape (one new minor or extend the
  existing snippet-mode's contribution set conditionally).
- **MO.4** depends on: MO.1 / MO.2 / MO.3 landed so the
  keymap shape is uniform before tackling the broader
  contribution-surface audit.

## Cross-references

- Convention itself + the principle + going-forward rule:
  [`mode-architecture.md`](../../architecture/mode-architecture.md) §13.
- Project memory backing the convention:
  `feedback_mode_owns_its_surface` (project-local memory).
- Related principles already established:
  `feedback_mode_owns_its_buffers` (modes own their buffers,
  App is a host), `feedback_buffers_no_special_case` (no
  kind-specific branching at universal layers).
- Patterns already correct (positive precedent): diff-mode
  layer (`crates/lattice-host/src/diff/mode.rs`),
  multibuffer-mode layer
  (`crates/lattice-host/src/multibuffer_keymap.rs`),
  project-search-multibuffer-mode layer (same file).
