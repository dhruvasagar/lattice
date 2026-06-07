//! T.1 — Interactive tutor data types.
//!
//! `TutorExercise` + `SuccessCondition` + `TutorSession` are pure data;
//! no dependency on the mode infrastructure.  `TutorMode` (T.3) wraps
//! `TutorSession` in a `BufferLocal` and drives the UI.

use serde::Deserialize;

pub const MAX_LIVES: u8 = 3;

/// High-level game state for the tutor session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TutorGameState {
    /// Normal play: lives > 0, exercises remaining.
    Active,
    /// Player ran out of lives on the current exercise.
    GameOver,
    /// All lessons in the build are complete.
    AllComplete,
}

// ---- Success conditions -----------------------------------------------

/// How the tutor decides an exercise is complete.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessCondition {
    /// Any edit to the anchor line counts as success.
    TextChanged,
    /// The anchor line must now contain this substring.
    TextContains(String),
    /// The anchor line must no longer contain this substring.
    TextNotContains(String),
    /// The anchor line must equal this string exactly.
    TextEquals(String),
    /// Observational exercise: the user must run `:tutor-next` explicitly.
    /// `is_met` always returns `false`; the action handler bypasses it.
    ManualAdvance,
}

impl SuccessCondition {
    /// Pure evaluation: `initial` is the anchor line at load time,
    /// `current` is its text after edits.
    pub fn is_met(&self, initial: &str, current: &str) -> bool {
        match self {
            Self::TextChanged => current != initial,
            Self::TextContains(pat) => current.contains(pat.as_str()),
            Self::TextNotContains(pat) => !current.contains(pat.as_str()),
            Self::TextEquals(expected) => current == expected.as_str(),
            Self::ManualAdvance => false,
        }
    }
}

// ---- Exercise descriptor ----------------------------------------------

/// One practice exercise in a tutor lesson.
#[derive(Debug, Clone, Deserialize)]
pub struct TutorExercise {
    /// Short identifier, e.g. `"2.3.1"`.
    pub id: String,
    /// One-line description shown in the TutorMode headerline.
    pub description: String,
    /// Exact text of the practice line in the lesson at load time.
    /// `TutorSession::load` scans the lesson text to resolve this to a
    /// line index.  The match is whitespace-trimmed on both sides.
    pub anchor: String,
    /// What counts as successful completion.
    pub success: SuccessCondition,
    /// Shown in the headerline after 3 unsuccessful edit attempts.
    pub hint: String,
}

// ---- Sidecar TOML wire format -----------------------------------------

/// Top-level shape of `lesson-N.exercises.toml`.
#[derive(Debug, Deserialize)]
struct ExerciseSidecar {
    exercises: Vec<TutorExercise>,
}

// ---- Session ----------------------------------------------------------

/// Per-tutor-buffer state.  Stored as a `BufferLocal` by `TutorMode`.
#[derive(Debug, Clone)]
pub struct TutorSession {
    pub lesson: u32,
    /// Total lesson count in this build (5 at launch).
    pub total_lessons: u32,
    pub exercises: Vec<TutorExercise>,
    /// Index of the current exercise.
    pub current: usize,
    /// Failed-attempt count since the last advance/retreat.
    /// Used to gate hint display (shows after ≥ 2).
    pub attempt_count: usize,
    /// Resolved line index in the buffer for each exercise's anchor.
    /// Parallel to `exercises`.
    pub anchor_lines: Vec<usize>,
    /// Text snapshot of each anchor line as of session load.
    /// Parallel to `exercises`.
    pub initial_texts: Vec<String>,
    /// Last buffer version seen by the post-dispatch watcher.
    pub last_version: u64,
    /// Lives remaining for the current exercise.
    pub lives: u8,
    /// Cumulative score across all passed exercises this session.
    pub score: u32,
    /// Wall-clock start time for the current exercise; used for speed bonuses.
    pub exercise_started_at: Option<std::time::Instant>,
    /// High-level game state.
    pub state: TutorGameState,
    /// All-time high score for this lesson loaded from disk at session
    /// start.  Shown in the HUD as `HI:`.
    pub high_score: u32,
}

impl TutorSession {
    /// Build a `TutorSession` by parsing the exercise sidecar and
    /// scanning the lesson text for each anchor.
    ///
    /// Returns `Err` (human-readable) if:
    /// - the sidecar TOML is malformed, or
    /// - any anchor text is not found in `lesson_text`.
    pub fn load(
        lesson: u32,
        total_lessons: u32,
        lesson_text: &str,
        exercises_toml: &str,
    ) -> Result<Self, String> {
        let sidecar: ExerciseSidecar = toml::from_str(exercises_toml)
            .map_err(|e| format!("tutor: lesson {lesson} exercises malformed: {e}"))?;

        let lines: Vec<&str> = lesson_text.lines().collect();

        let mut anchor_lines = Vec::with_capacity(sidecar.exercises.len());
        let mut initial_texts = Vec::with_capacity(sidecar.exercises.len());

        for ex in &sidecar.exercises {
            let idx = lines
                .iter()
                .position(|l| l.trim() == ex.anchor.trim())
                .ok_or_else(|| {
                    format!(
                        "tutor: lesson {lesson} exercise '{}': anchor not found: {:?}",
                        ex.id, ex.anchor
                    )
                })?;
            anchor_lines.push(idx);
            initial_texts.push(lines[idx].to_owned());
        }

        Ok(Self {
            lesson,
            total_lessons,
            exercises: sidecar.exercises,
            current: 0,
            attempt_count: 0,
            anchor_lines,
            initial_texts,
            last_version: 0,
            lives: MAX_LIVES,
            score: 0,
            exercise_started_at: Some(std::time::Instant::now()),
            state: TutorGameState::Active,
            high_score: 0,
        })
    }

    /// `true` when all exercises in this lesson have been advanced past.
    pub fn is_complete(&self) -> bool {
        self.current >= self.exercises.len()
    }

    /// The current exercise, or `None` if the lesson is complete.
    pub fn current_exercise(&self) -> Option<&TutorExercise> {
        self.exercises.get(self.current)
    }

    /// Auto-detect pass/fail against live buffer lines.  Awards score
    /// on pass (base + first-try bonus + speed bonus).  Increments
    /// `attempt_count` on failure.  Does NOT drain lives — that only
    /// happens on explicit `<CR>` presses via `drain_life`.
    ///
    /// Returns `false` when the session is not `Active` or the lesson
    /// is already complete.
    pub fn check(&mut self, lines: &[&str]) -> bool {
        if self.state != TutorGameState::Active {
            return false;
        }
        let Some(ex) = self.exercises.get(self.current) else {
            return false;
        };
        let anchor_idx = self.anchor_lines[self.current];
        let current_text = lines.get(anchor_idx).copied().unwrap_or("");
        let initial_text = self.initial_texts[self.current].as_str();

        if ex.success.is_met(initial_text, current_text) {
            let base = 100u32;
            let first_try = if self.attempt_count == 0 { 50 } else { 0 };
            let speed = self
                .exercise_started_at
                .map(|t| {
                    let s = t.elapsed().as_secs();
                    if s < 10 { 100 } else if s < 30 { 50 } else if s < 60 { 25 } else { 0 }
                })
                .unwrap_or(0);
            self.score = self.score.saturating_add(base + first_try + speed);
            true
        } else {
            self.attempt_count += 1;
            false
        }
    }

    /// Peek: is the success condition currently met?  Pure read — no
    /// side effects.  Used by `do_tutor_advance` to decide whether an
    /// explicit `<CR>` press is a valid advance or a penalised miss.
    pub fn is_condition_met(&self, lines: &[&str]) -> bool {
        let Some(ex) = self.exercises.get(self.current) else {
            return false;
        };
        let anchor_idx = self.anchor_lines[self.current];
        let current_text = lines.get(anchor_idx).copied().unwrap_or("");
        let initial_text = self.initial_texts[self.current].as_str();
        ex.success.is_met(initial_text, current_text)
    }

    /// Drain one life on an explicit wrong attempt (`<CR>` press when
    /// the condition is not met).  Increments `attempt_count` and
    /// transitions to `GameOver` when lives reach zero.
    pub fn drain_life(&mut self) {
        self.attempt_count += 1;
        self.lives = self.lives.saturating_sub(1);
        if self.lives == 0 {
            self.state = TutorGameState::GameOver;
        }
    }

    /// Advance to the next exercise; resets lives, attempt_count, and
    /// restarts the exercise timer.  Returns the new current exercise,
    /// or `None` if the lesson is now complete.
    pub fn advance(&mut self) -> Option<&TutorExercise> {
        self.current += 1;
        self.attempt_count = 0;
        self.lives = MAX_LIVES;
        self.state = TutorGameState::Active;
        self.exercise_started_at = Some(std::time::Instant::now());
        self.exercises.get(self.current)
    }

    /// Return to the previous exercise; resets lives, attempt_count,
    /// and clears any GameOver state.
    pub fn retreat(&mut self) {
        if self.current > 0 {
            self.current -= 1;
        }
        self.attempt_count = 0;
        self.lives = MAX_LIVES;
        self.state = TutorGameState::Active;
        self.exercise_started_at = Some(std::time::Instant::now());
    }
}

// ---- BufferLocal impl -------------------------------------------------

impl lattice_mode::BufferLocal for TutorSession {
    const NAME: &'static str = "tutor-mode.session";
    const DOC: &'static str =
        "Active tutor session: current lesson, exercise index, \
         anchor line positions, attempt count.";
    const OWNER_MODE: &'static str = "tutor-mode";

    fn describe(&self) -> String {
        if self.is_complete() {
            format!("lesson {} complete", self.lesson)
        } else {
            format!(
                "lesson {}/{} exercise {}/{}",
                self.lesson,
                self.total_lessons,
                self.current + 1,
                self.exercises.len()
            )
        }
    }
}

// ---- Tests ------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const TOTAL: u32 = 5;

    fn lesson_text() -> &'static str {
        "Header line\n\
         Some explanation text.\n\
         --->  The quick brown fox jumps over the lazy dog.\n\
         More explanation.\n\
         --->  result = compute(some, arguments, here) + offset;\n"
    }

    fn exercises_toml() -> &'static str {
        r#"
[[exercises]]
id          = "2.1"
description = "Delete a word"
anchor      = "--->  The quick brown fox jumps over the lazy dog."
success     = "text_changed"
hint        = "type d i w"

[[exercises]]
id          = "2.2"
description = "Delete function args"
anchor      = "--->  result = compute(some, arguments, here) + offset;"
success     = { text_not_contains = "some, arguments, here" }
hint        = "type d a ("
"#
    }

    #[test]
    fn load_resolves_anchor_lines() {
        let s = TutorSession::load(2, TOTAL, lesson_text(), exercises_toml()).unwrap();
        assert_eq!(s.anchor_lines[0], 2);
        assert_eq!(s.anchor_lines[1], 4);
        assert_eq!(s.initial_texts[0], "--->  The quick brown fox jumps over the lazy dog.");
    }

    #[test]
    fn load_rejects_missing_anchor() {
        let bad = r#"
[[exercises]]
id = "x"
description = "test"
anchor = "THIS LINE DOES NOT EXIST IN THE LESSON"
success = "text_changed"
hint = "n/a"
"#;
        assert!(TutorSession::load(2, TOTAL, lesson_text(), bad).is_err());
    }

    #[test]
    fn success_text_changed() {
        let c = SuccessCondition::TextChanged;
        assert!(!c.is_met("original", "original"));
        assert!(c.is_met("original", "modified"));
    }

    #[test]
    fn success_text_contains() {
        let c = SuccessCondition::TextContains("dog".into());
        assert!(c.is_met("", "the lazy dog"));
        assert!(!c.is_met("", "the lazy cat"));
    }

    #[test]
    fn success_text_not_contains() {
        let c = SuccessCondition::TextNotContains("REMOVE".into());
        assert!(c.is_met("has REMOVE", "gone"));
        assert!(!c.is_met("has REMOVE", "still REMOVE here"));
    }

    #[test]
    fn success_text_equals() {
        let c = SuccessCondition::TextEquals("exact".into());
        assert!(c.is_met("", "exact"));
        assert!(!c.is_met("", "exact "));
    }

    #[test]
    fn success_manual_advance_never_auto() {
        let c = SuccessCondition::ManualAdvance;
        assert!(!c.is_met("", ""));
        assert!(!c.is_met("anything", "completely different"));
    }

    #[test]
    fn check_increments_attempt_count_on_failure() {
        let mut s = TutorSession::load(2, TOTAL, lesson_text(), exercises_toml()).unwrap();
        let lines: Vec<&str> = lesson_text().lines().collect();
        assert!(!s.check(&lines));
        assert_eq!(s.attempt_count, 1);
        assert!(!s.check(&lines));
        assert_eq!(s.attempt_count, 2);
    }

    #[test]
    fn check_succeeds_when_anchor_line_changed() {
        let mut s = TutorSession::load(2, TOTAL, lesson_text(), exercises_toml()).unwrap();
        let edited = "Header line\n\
                      Some explanation text.\n\
                      --->  The brown fox jumps over the lazy dog.\n\
                      More explanation.\n\
                      --->  result = compute(some, arguments, here) + offset;\n";
        let lines: Vec<&str> = edited.lines().collect();
        assert!(s.check(&lines));
    }

    #[test]
    fn advance_increments_current_and_resets_count() {
        let mut s = TutorSession::load(2, TOTAL, lesson_text(), exercises_toml()).unwrap();
        let lines: Vec<&str> = lesson_text().lines().collect();
        s.check(&lines); // bumps attempt_count to 1
        s.advance();
        assert_eq!(s.current, 1);
        assert_eq!(s.attempt_count, 0);
    }

    #[test]
    fn retreat_returns_to_previous() {
        let mut s = TutorSession::load(2, TOTAL, lesson_text(), exercises_toml()).unwrap();
        s.advance();
        assert_eq!(s.current, 1);
        s.retreat();
        assert_eq!(s.current, 0);
    }

    #[test]
    fn retreat_noop_at_zero() {
        let mut s = TutorSession::load(2, TOTAL, lesson_text(), exercises_toml()).unwrap();
        s.retreat();
        assert_eq!(s.current, 0);
    }

    #[test]
    fn is_complete_after_all_advances() {
        let mut s = TutorSession::load(2, TOTAL, lesson_text(), exercises_toml()).unwrap();
        assert!(!s.is_complete());
        s.advance();
        assert!(!s.is_complete());
        s.advance();
        assert!(s.is_complete());
        assert!(s.current_exercise().is_none());
    }
}
