# Tutor Mode — Architecture

**Design fragment.** For the slice sequencing see
`docs/dev/operations/slice-plans/tutor.md`.

---

## 1. Goal

Make `:tutor` interactive: as the user performs exercises on the tutor
buffer, TutorMode detects completion and advances through the lesson,
providing a progress headerline and per-exercise hints. All
tutor-specific keybindings, state, and UI contributions live exclusively
inside TutorMode — zero new host methods, zero new host `Action` variants.

---

## 2. Design evaluation

> **UX (higher court):** Success-condition evaluation runs on the editor
> actor thread on every buffer-version bump for the tutor buffer only.
> It is a bounded string comparison — O(1) per exercise anchor — so it
> cannot affect keystroke latency. The headerline virtual row is computed
> off the UI thread by the VirtualRowProvider and published via RCU;
> the renderer reads the published snapshot. No degradation to unedited
> content; no flicker.
>
> **Paramount goals:** Protects #1 — validation is a fast string check,
> headerline update is off-thread. Protects #2 — TutorMode is a full
> minor-mode demonstration: keymaps, VirtualRowProvider, BufferLocals,
> and a per-buffer version watcher stress-test the mode surface. Protects
> #3 — all TutorMode chords live at `MinorMode(tutor-mode)`, never at
> `Builtin`; vim grammar is unaffected. Protects #4 — VirtualRow rebuild
> is async; the success check is synchronous but bounded.
>
> **Heuristic #1 (long-term fit):** TutorMode as a first-class minor mode
> is genuinely better: self-contained, zero host coupling, the right shape
> for an eventual WASM-plugin port. Not kept out of risk-aversion; the
> earlier Option A/B designs were inferior because they had no feedback
> loop and left host logic in the dispatch path.
>
> **Heuristic #2 (paramount goals):** Driven by Lattice's mode-ownership
> model and existing VirtualRowProvider infrastructure — not by what
> "interactive tutorials in other editors" look like.
>
> **Standing rule (mode ownership):** TutorMode owns the TutorSession
> BufferLocal, the VirtualRowProvider registration, the
> `MinorMode(tutor-mode)` keymap layer, and all action-handler bodies.
> The host's `do_tutor` remains the buffer creator (same pattern as
> help-mode, oil-mode, file-tree-mode) and calls `TutorMode::activate_on`
> after opening the buffer; everything after that belongs to the mode.
> Half-migration test: zero new `Editor::` methods, zero new host `Action`
> variants.

---

## 3. Data types  (`lattice-host/src/tutor.rs`)

### 3.1 `SuccessCondition`

```rust
pub enum SuccessCondition {
    /// Any edit to the anchor line counts.
    TextChanged,
    /// Anchor line must contain this substring after the edit.
    TextContains(String),
    /// Anchor line must no longer contain this substring.
    TextNotContains(String),
    /// Anchor line must equal this string exactly.
    TextEquals(String),
    /// Observational exercise: user must run :tutor-next explicitly.
    ManualAdvance,
}
```

`SuccessCondition::is_met(initial: &str, current: &str) -> bool` is a
pure, synchronous function with no I/O. `ManualAdvance` always returns
`false`; the `:tutor-next` action handler bypasses `is_met` and calls
`TutorSession::advance` directly.

### 3.2 `TutorExercise`

```rust
pub struct TutorExercise {
    pub id: String,           // "2.3.1"
    pub description: String,  // shown in the headerline
    /// Exact text of the practice line in the lesson at load time.
    /// TutorSession scans the initial buffer text to resolve the line index.
    pub anchor: String,
    pub success: SuccessCondition,
    pub hint: String,
}
```

### 3.3 `TutorSession`  (stored as a `BufferLocal`)

```rust
pub struct TutorSession {
    pub lesson: u32,
    pub total_lessons: u32,       // 5 at launch
    pub exercises: Vec<TutorExercise>,
    pub current: usize,           // index into exercises
    pub attempt_count: usize,     // edits since last advance without success
    /// Line index in the buffer for each exercise's anchor,
    /// resolved at load time from the initial lesson text.
    pub anchor_lines: Vec<usize>,
    /// Text snapshot of each anchor line at load time.
    pub initial_texts: Vec<String>,
    /// Last buffer version seen; used to detect changes.
    pub last_version: u64,
}
```

`TutorSession::load(lesson, text, exercises_toml)` parses the sidecar
TOML and scans `text` to resolve each anchor to its line index.

`TutorSession::check(lines)` evaluates the current exercise's condition
by comparing `lines[anchor_lines[current]]` to `initial_texts[current]`.
Returns `Some(true)` on success, `None` while in-progress.

`TutorSession::advance()` increments `current`, resets `attempt_count`.
Returns `Option<&TutorExercise>` (None when all exercises are done).

---

## 4. Exercise sidecar format  (`docs/user/tutor/lesson-N.exercises.toml`)

Plain TOML, one file per lesson, compiled alongside the lesson text via
`include_str!` in `do_tutor`. `success` is a TOML string for unit variants
and a TOML inline table for the payload variants.

```toml
[[exercises]]
id          = "2.1"
description = "Delete a word with  d i w"
anchor      = "The quick brown fox jumps over the lazy dog."
success     = "text_changed"
hint        = "Place cursor on any word and type:  d  i  w"

[[exercises]]
id          = "2.3.2"
description = "Delete a function call's args with  d a ("
anchor      = "result = compute(some, arguments, here) + offset;"
success     = { text_not_contains = "some, arguments, here" }
hint        = "Place cursor inside the parens and type:  d a ("

[[exercises]]
id          = "4.2.1"
description = "Press <C-h> k then type j to describe the j chord"
anchor      = "-- Exercise 1: press <C-h> k in Normal mode"
success     = "manual_advance"
hint        = "In Normal mode: press <C-h>, then k, then type j and Enter"
```

---

## 5. TutorMode  (`lattice-host/src/tutor_mode.rs`)

### 5.1 Mode identity

```
mode_id()  ->  "tutor-mode"
kind()     ->  ModeKind::Minor
```

### 5.2 Keymap contributions  (at `MinorMode(tutor-mode)` only)

| Binding          | ActionId          | Description                              |
|------------------|-------------------|------------------------------------------|
| `<CR>` (Normal)  | `tutor:advance`   | Manual-advance (observational exercises) |
| `:tutor-next`    | `tutor:advance`   | Explicit advance                         |
| `:tutor-prev`    | `tutor:retreat`   | Return to previous exercise              |

`tutor:advance` and `tutor:retreat` are contributed `ActionId`s; their
handler bodies live in `tutor_mode.rs`. They are not in the host's
`Action` enum.

`:tutor-next` and `:tutor-prev` are registered as ex-command aliases by
TutorMode's `activate_on`, not in the global excommand table. They are
invisible until TutorMode is active.

### 5.3 VirtualRowProvider  (headerline)

TutorMode registers a `VirtualRowProvider` for the tutor buffer.
The provider emits one virtual row at position 0 (above document line 0).

In-progress:
```
 Lattice Tutor — Lesson 2/5 | Exercise 3/8: Delete a word with  d i w
```

After a success (before advancing):
```
 ✓ Correct! — Lesson 2/5 | Exercise 4/8: Yank and paste with  yy / p
```

After 3 failed attempts, hint appended:
```
 Lattice Tutor — Lesson 2/5 | Exercise 3/8: ...   Hint: d  i  w
```

Lesson complete:
```
 Lesson 2 complete — run  :tutor 3  for the next lesson, or  :tutor  to restart
```

Provider `version()` bumps whenever `session.current` or the hint state
changes; the VirtualRowWorker rebuilds only on version change.

### 5.4 Buffer-version watcher

On each post-dispatch callback (registered at activation), TutorMode reads
the tutor buffer's version. If `version != session.last_version`:

1. Snapshot buffer lines.
2. `session.check(lines)` → if success: bump provider version (headerline
   shows ✓), schedule `session.advance()` after 500 ms via the tick
   registry.
3. Else: increment `attempt_count`; if ≥ 3 bump provider version to show
   the hint.
4. Update `session.last_version`.

Watcher is scoped to the tutor buffer's `BufferId` and is deregistered
when TutorMode deactivates (e.g., `:bd` on the tutor buffer).

---

## 6. Host interface  (`do_tutor` in `dispatch.rs`)

```rust
// Existing: write lesson text to temp file, do_edit to open buffer.
// New, after buffer is open:
let session = TutorSession::load(lesson_num, lesson_text, exercises_toml)
    .unwrap_or_else(|e| { self.set_message(Error, e); return; });
TutorMode::activate_on(buffer_id, session, &mut self.editor);
```

`activate_on` is the single host-to-mode call. It:
1. Seeds `TutorSession` as a `BufferLocal`.
2. Registers the VirtualRowProvider.
3. Registers `MinorMode(tutor-mode)` keymap layer on the buffer.
4. Registers the post-dispatch version watcher.

No new `Editor::` methods. No new `Action` variants.

---

## 7. `--tutor` CLI flag  (`lattice-cli/src/main.rs`)

```rust
/// Start the interactive tutor at lesson N (default 1).
/// Mutually exclusive with FILE.
#[arg(long = "tutor", value_name = "N", default_missing_value = "1",
      num_args = 0..=1, conflicts_with = "file")]
tutor: Option<u32>,
```

Passed through to the TUI/GPUI entry as a synthetic initial command
`format!(":tutor {n}\n")` injected before the event loop.

---

## 8. Crate placement

All new code lives in `lattice-host`:

```
lattice-host/src/tutor.rs        — TutorExercise, SuccessCondition, TutorSession
lattice-host/src/tutor_mode.rs   — TutorMode, VirtualRowProvider impl, watcher
```

`tutor.rs` has no dependency on the mode infrastructure — only `serde` and
`toml`. `tutor_mode.rs` depends on `tutor.rs` plus the existing host/mode
surface.

---

## 9. Rejected alternatives

**Option A/B — static buffer with optional lesson navigation.** No feedback
loop; the mode owns no handler bodies, half-migrated at best. Inferior by
heuristic #1.

**Dedicated `TutorBuffer` kind.** Requires kind-branching in the renderer —
the "buffers must not have kind-specific logic" anti-pattern. TutorMode
overlays on a plain Document buffer; the renderer sees no difference.

**WASM plugin for the tutor.** Good eventual target once the plugin host
lands. TutorMode as a minor mode documents the shape the WASM port follows.
