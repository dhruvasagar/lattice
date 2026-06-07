# Interactive Tutor — Slice Plan

**Design:** `docs/dev/architecture/tutor-mode.md`

---

## T.1 — Data types + sidecar format  🚧

**Files touched:**
- `lattice-host/src/tutor.rs` (new)
- `lattice-host/src/lib.rs` (expose module)
- `Cargo.toml` for `lattice-host` (add `toml` + `serde` deps if not present)

**Deliverables:**
- `TutorExercise`, `SuccessCondition`, `TutorSession` types
- `TutorSession::load(lesson, text, exercises_toml) -> Result<TutorSession, String>`
  - Parses sidecar TOML into `Vec<TutorExercise>`
  - Scans `text` line-by-line to resolve each anchor to its line index
  - Returns `Err` if any anchor text is not found in the lesson text
- `SuccessCondition::is_met(initial: &str, current: &str) -> bool`
- `impl BufferLocal for TutorSession` (owner = "tutor-mode")

**Tests** (unit, in `tutor.rs`):
- `load_resolves_anchor_lines` — anchor texts map to correct line indices
- `load_rejects_missing_anchor` — Err if anchor not in lesson text
- `success_text_changed` — any edit triggers success
- `success_text_not_contains` — missing substring triggers success
- `success_text_contains` — required substring triggers success
- `success_text_equals` — exact match required
- `success_manual_advance_never_auto` — `ManualAdvance::is_met` always false

---

## T.2 — Exercise sidecar files (content)  🗒

**Files touched:**
- `docs/user/tutor/lesson-1.exercises.toml` (new)
- `docs/user/tutor/lesson-2.exercises.toml` (new)
- `docs/user/tutor/lesson-3.exercises.toml` (new)
- `docs/user/tutor/lesson-4.exercises.toml` (new)
- `docs/user/tutor/lesson-5.exercises.toml` (new)
- `lattice-host/src/dispatch.rs` — add `include_str!` for each sidecar
  alongside the existing lesson text

**Deliverables:**
- One sidecar per lesson with every practice line covered
- Lessons 2-3: `text_changed` / `text_not_contains` / `text_contains` conditions
- Lessons 4-5: `manual_advance` conditions (observational exercises)
- Lesson 1: mostly `manual_advance` (mode-switching exercises are positional)

**Test:**
- `all_sidecars_parse` — each sidecar TOML parses without error
- `all_anchors_found` — for each lesson, `TutorSession::load` succeeds
  (anchor texts in the sidecar exist in the corresponding lesson file)

---

## T.3 — TutorMode skeleton: registration + headerline + keymaps  🗒

**Files touched:**
- `lattice-host/src/tutor_mode.rs` (new)
- `lattice-host/src/lib.rs` — expose module + register mode
- `lattice-grammar/src/ex_commands.rs` — register `tutor-next` and
  `tutor-prev` ex-command ids (contributed by TutorMode, not Builtin)

**Deliverables:**
- `TutorMode` struct implementing the mode trait
- `TutorMode::mode_id()` -> `"tutor-mode"`, `kind()` -> `Minor`
- `TutorMode::activate_on(buffer_id, session, editor)`:
  - Seeds `TutorSession` as `BufferLocal`
  - Registers the `VirtualRowProvider` for the tutor buffer
  - Registers `MinorMode(tutor-mode)` keymap layer with `tutor:advance`
    and `tutor:retreat` action ids
  - Registers ex-command aliases `:tutor-next` and `:tutor-prev`
- `TutorHeaderlineProvider` implementing `VirtualRowProvider`:
  - Holds an `Arc<Mutex<TutorViewState>>` (lesson/exercise/hint text)
  - Emits one virtual row at position 0 with the headerline text
  - `version()` reads a monotonic counter in `TutorViewState`

**Tests:**
- `headerline_text_in_progress` — row text matches expected format
- `headerline_text_lesson_complete` — shows lesson-complete message
- `tutor_next_only_active_in_tutor_mode` — `:tutor-next` chord resolves
  as `(registered but not active)` on a non-tutor buffer; resolves as
  `(fires now)` on the tutor buffer

---

## T.4 — Buffer-version watcher + success evaluation  🗒

**Files touched:**
- `lattice-host/src/tutor_mode.rs` — add post-dispatch callback
- `lattice-host/src/dispatch.rs` — call `TutorMode::activate_on` after `do_edit`

**Deliverables:**
- Post-dispatch callback: reads buffer version, calls `session.check`,
  handles success (bump provider version -> headerline shows ✓, schedule
  advance after 500 ms) and failed attempts (bump after 3 -> show hint)
- `tutor:advance` handler: calls `session.advance()` directly (bypasses
  `is_met`); primary path for `ManualAdvance` exercises
- `tutor:retreat` handler: decrements `session.current`
- `do_tutor` wired: calls `TutorMode::activate_on(buffer_id, session)`

**Tests:**
- `exercise_advances_on_text_change` — matching edit advances session
- `exercise_no_advance_on_wrong_edit` — non-matching edit does not advance
- `hint_appears_after_three_attempts` — provider version bumps after 3 edits
- `manual_advance_requires_tutor_next` — `ManualAdvance` condition never
  auto-fires; `:tutor-next` action fires it
- `all_exercises_complete_shows_lesson_complete` — advance past last exercise
  shows lesson-complete headerline

---

## T.5 — `--tutor` CLI flag  🗒

**Files touched:**
- `lattice-cli/src/main.rs` — add `tutor: Option<u32>` to `Cli` struct
- TUI entry / GPUI entry — detect `args.tutor` and inject initial command

**Deliverables:**
- `--tutor [N]` (optional N, default 1) conflicts with `file`
- Injected as synthetic `:tutor N\n` before the event loop

**Tests:**
- `--tutor` without N defaults to lesson 1
- `--tutor 3` passes lesson 3
- `--tutor` combined with a file argument is a clap error

---

## Status legend

✅ landed   🚧 in progress   🗒 planned
