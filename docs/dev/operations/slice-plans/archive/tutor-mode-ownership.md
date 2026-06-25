# Tutor mode-ownership — slice plan

Design fragment: the tutor's *what/why* lives in
`docs/dev/architecture/tutor-mode.md`; the `feedback_mode_owns_its_surface`
principle is the standing rule this plan applies. This file owns the *when /
in what order* of relocating the tutor's engine off the host and into
`TutorMode`.

Status: ✅ done via TM.1 cohesion-relocation (2026-06-25, commit `b6795c45`).

> **CORRECTION (2026-06-25, on starting the work).** Reading the engine revealed
> the original target below (a mode-owned per-buffer store + tick-callback + "zero
> `Editor::` methods" acid test) **over-reached**. The tutor is a **host-native
> builtin that keeps its `&mut Editor` seam** (BC.final lists "tutor" explicitly),
> and its engine is irreducibly cursor/document/buffer-local-coupled on the
> keystroke path — a tick-callback (`FnMut() -> Vec<Effect>`) genuinely cannot
> reach that state. So "ownership" here is **cohesion-relocation**: move the engine
> `impl Editor` methods out of `dispatch.rs` into the tutor module's own
> `tutor/engine.rs` (the methods STAY `Editor` methods, so call sites are
> unchanged). **Done in one slice** (`b6795c45`): all 7 methods (`do_tutor`,
> `do_tutor_advance`/`retreat`, `tutor_after_advance`/`open_next`/`update_headerline`,
> `check_tutor_session`) moved to `tutor/engine.rs`; `dispatch.rs` went from ~137
> tutor refs to **5** thin call-site hooks (the hot-loop check, the
> `ex:tutor-next`/`-prev` routing) — the acceptable host-builtin seam. Green: host
> lib 568 · tutor 20 · BC.2 pins 14.
>
> The TM.2–TM.5 mode-store slices below are **superseded** (kept as the rejected
> heavier alternative). The HUD *rendering* was already mode-side (`tutor/mode.rs`);
> the tutor module is now self-contained. If a future need arises to make the
> tutor a *true* mode-store subsystem (e.g. a WASM-plugin tutor), revisit — but for
> a host-builtin, cohesion-relocation is the fit.

## Context — the audit (the inversion to fix)

The tutor's **data model** is mode-side (`crates/lattice-host/src/tutor/`:
`session.rs` `TutorSession`/exercises, `scores.rs` `TutorScores`, `mode.rs`
`TutorMode` + `render_tutor_headerline`), but its **engine** — lesson-open
seeding, the gamification (advance / retreat / lives / score / lesson-clear),
the per-keystroke exercise check, and the headerline HUD updates — lives
**host-side** in `dispatch.rs` (~137 tutor references). This inverts
mode-ownership: the host does the mode's work; the mode is a thin shell.

Host-side surface to relocate (all in `dispatch.rs`):

| Host method | What it does | Smell |
|---|---|---|
| `check_tutor_session()` | per-tick exercise-condition check, called from the **dispatch hot loop** (`dispatch.rs:1596`) | tutor-specific logic in the keystroke path (paramount #1 surface) |
| `do_tutor_advance` (`<CR>`/`<C-j>`) | advance / lives drain / GameOver / lesson-complete | gamification body in the host |
| `do_tutor_retreat` | step back a lesson | host |
| `tutor_after_advance`, `tutor_open_next_or_complete` | progression + lesson-clear | host |
| `tutor_update_headerline` | recompute the HUD row | host |
| `do_tutor(lesson)` | load lesson `include_str!` → `do_edit` → seed `TutorSession` + `TutorScores.high_score` + the `SimpleHeaderlineHandle` virtual-row provider → store both as `buffer_locals` → `activate_minor(TutorMode)` | the whole seed is host-side |

Per-buffer state (`TutorSession`, `TutorHeaderlineState`) is held in the host's
`buffer_locals` — but a mode cannot reach `buffer_locals` (`ModeContext` exposes
none; "mode-private state is owned by the `Mode::Guard`", `context.rs`). **This
is the load-bearing design question for the refactor** (see TM.1 / TM.4): the
per-buffer session/HUD must move into the `TutorMode::Guard` (or a mode-owned
per-buffer store), not host buffer-locals.

## Target state (acid test)

- ZERO `Editor::do_tutor*` / `Editor::*tutor*` methods remain.
- The dispatch hot loop has NO tutor-specific call (`check_tutor_session` gone).
- `TutorMode` owns: session seeding (`on_activate`), the gamification engine,
  the HUD provider, the per-keystroke check (via the generic tick-callback /
  event subscription), advance/retreat (via `ActionHandler` bodies bound to the
  mode's chords).
- The host exposes only the SAME generic primitives the I-series / diff /
  multibuffer modes already use: buffer-store, virtual-row-provider registry,
  tick-callback registry, event bus, action-handler registry. A `:tutor`
  ex-command keeps a thin host route (write the embedded lesson file → generic
  `do_edit`); everything else is mode-owned.

## Slices

- **TM.1 — relocate the hot-loop check (the keystroke-sensitive one).**
  `check_tutor_session()` is per-tick tutor logic in the host loop
  (`dispatch.rs:1596`). Move it into a `TutorMode`-owned mechanism: either a
  tick-callback registered via the generic tick-callback registry in
  `on_activate`, or an event subscription (check after `Edit`/`SelectionsChanged`
  events). **Decide the per-buffer-state home first** — the check reads the
  session, so the session must be mode-reachable (Guard or mode store), not host
  `buffer_locals`. The host's `self.check_tutor_session()` call disappears.
  **Paramount #1 guard:** the relocated check must stay O(1)-per-tick + off the
  render path (it already is — keep it so). **Tests:** a session still advances
  on a condition met via the relocated path; the dispatch loop has no tutor call.

- **TM.2 — advance/retreat → `ActionHandler` bodies.** `do_tutor_advance`
  (`<CR>`/`<C-j>`) + `do_tutor_retreat` + the progression helpers
  (`tutor_after_advance`, `tutor_open_next_or_complete`) become
  `TutorMode`-contributed action handlers, bound to the mode's chords at
  `KeymapLayer::MinorMode(tutor)` via `register_tutor_keymap` (NOT `Builtin`).
  The host keeps only the generic chord dispatch + `do_edit` (open-next is a
  generic open). **Tests:** `<CR>` advance / GameOver / lesson-complete /
  manual-advance behavior preserved.

- **TM.3 — the HUD → the mode's headerline provider.** The
  `SimpleHeaderlineHandle` + `render_tutor_headerline` provider seeding
  (`dispatch.rs:19641-19649`) and `tutor_update_headerline` relocate into
  `TutorMode` (`on_activate` registers the provider; the gamification updates the
  handle directly). The virtual-row-provider registry stays the generic host
  primitive; only the tutor's registration moves mode-side. (Async-buffer-status
  in the headerline is already the documented convention,
  `project_async_buffer_status_in_headerline`.) **Tests:** the HUD reflects
  score / lives / progress after advance.

- **TM.4 — lesson open → `on_activate`.** `do_tutor` shrinks to a generic
  route: write the embedded lesson file (the `include_str!` lesson catalog stays
  build-time) → `do_edit`. The session/scores/HUD/mode seeding moves into
  `TutorMode::on_activate` (fires when the lesson buffer activates). Resolves the
  per-buffer-state home from TM.1 (Guard-owned). **Tests:** `:tutor N` still opens
  lesson N with its session + HUD.

- **TM.5 — delete + acid-test.** Remove the now-empty `Editor::do_tutor*` /
  `tutor_*` methods. Add a pin: zero `Editor` tutor methods (grep guard) + the
  acid test (a `BufferStore`/tick/action-only mode adds zero `Editor::` methods),
  in the style of the BC.2 boot pins.

## Risks / notes

1. **Per-buffer-state home (the crux).** Today `TutorSession` +
   `TutorHeaderlineState` are host `buffer_locals`; a mode can't reach those.
   The session/HUD must live in the `TutorMode::Guard` (per-activation,
   Drop-cleaned) or a mode-owned per-buffer store keyed by `BufferId`. Settle
   this in TM.1 before relocating the check.
2. **Hot-loop latency (paramount #1).** `check_tutor_session` runs per tick on
   the keystroke path. Whatever replaces it (tick-callback or event sub) must not
   add per-keystroke cost; the relocation is a move, not a rewrite.
3. **Scoped to tutor.** Other host-builtin modes (foundation, oil, file-tree)
   keep their `*_mut`/host seams — the BC.final finding stands ("`*_mut` is the
   PERMANENT seam for host-native builtins"). This refactor only fixes the
   tutor's *gameplay engine* leak, not the lesson-catalog embedding.

## Cross-reference

- Design / behavior: `docs/dev/architecture/tutor-mode.md`.
- The mode-ownership pattern to mirror: the diff (DX.x,
  `slice-plans/archive/diff-extraction.md`) + IDE-peer (I-series,
  `slice-plans/archive/ide-protocol.md`) relocations.
