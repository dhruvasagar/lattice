//! TM.1 (tutor-mode-ownership): the tutor's gameplay engine — lesson open/seed, advance/retreat, score/lives gamification, the per-keystroke success check, and the HUD update — relocated here from dispatch.rs so the tutor module owns it. These stay `impl Editor` methods: a host-native builtin keeps its &mut Editor seam (BC.final).

use crate::action::EchoLevel;
use crate::buffers::BufferId;
use crate::dispatch::{DoEditOutcome, RendererSignal};
use crate::editor::Editor;

impl Editor {
    /// `:tutor [N]` -- open the interactive Lattice tutor lesson
    /// `N`. Embeds the lesson via `include_str!`; copies to a temp
    /// path so the user can edit. Phase 5.8.AD.5: returns
    /// `Vec<RendererSignal>` because `do_edit` may emit signals.
    pub fn do_tutor(&mut self, lesson: Option<u32>) -> Vec<RendererSignal> {
        let lesson_num = lesson.unwrap_or(1);
        let (lesson_text, exercises_toml): (&'static str, &'static str) = match lesson_num {
            1 => (
                include_str!("../../../../docs/user/tutor/lesson-1.md"),
                include_str!("../../../../docs/user/tutor/lesson-1.exercises.toml"),
            ),
            2 => (
                include_str!("../../../../docs/user/tutor/lesson-2.md"),
                include_str!("../../../../docs/user/tutor/lesson-2.exercises.toml"),
            ),
            3 => (
                include_str!("../../../../docs/user/tutor/lesson-3.md"),
                include_str!("../../../../docs/user/tutor/lesson-3.exercises.toml"),
            ),
            4 => (
                include_str!("../../../../docs/user/tutor/lesson-4.md"),
                include_str!("../../../../docs/user/tutor/lesson-4.exercises.toml"),
            ),
            5 => (
                include_str!("../../../../docs/user/tutor/lesson-5.md"),
                include_str!("../../../../docs/user/tutor/lesson-5.exercises.toml"),
            ),
            6 => (
                include_str!("../../../../docs/user/tutor/lesson-6.md"),
                include_str!("../../../../docs/user/tutor/lesson-6.exercises.toml"),
            ),
            7 => (
                include_str!("../../../../docs/user/tutor/lesson-7.md"),
                include_str!("../../../../docs/user/tutor/lesson-7.exercises.toml"),
            ),
            n => {
                self.set_message(
                    EchoLevel::Error,
                    format!(
                        "lesson {n} doesn't exist (lessons 1-7 available); \
                         contributions welcome"
                    ),
                );
                return Vec::new();
            }
        };
        let mut path = std::env::temp_dir();
        path.push(format!("lattice-tutor-lesson-{lesson_num}.md"));
        if let Err(e) = std::fs::write(&path, lesson_text) {
            self.set_message(
                EchoLevel::Error,
                format!("tutor: failed to write lesson file: {e}"),
            );
            return Vec::new();
        }
        let outcome = self.do_edit(Some(path), false);
        let signals = match outcome {
            DoEditOutcome::Opened(s) | DoEditOutcome::Activated(s) | DoEditOutcome::Reloaded(s) => {
                s
            }
            DoEditOutcome::Directory(d) => self.do_open_oil(Some(d)),
            DoEditOutcome::NoFileName | DoEditOutcome::Failed => return Vec::new(),
        };
        // T.4: seed TutorSession, TutorHeaderlineProvider, and tutor-mode
        // on the buffer that was just opened (now self.document_buffer_id).
        let buffer_id = self.document_buffer_id;
        let mut session =
            match crate::tutor::TutorSession::load(lesson_num, 7, lesson_text, exercises_toml) {
                Ok(s) => s,
                Err(e) => {
                    self.set_message(
                        EchoLevel::Error,
                        format!("tutor: failed to load session: {e}"),
                    );
                    return signals;
                }
            };
        // T.B: seed the HUD's HI: field from persisted scores.
        session.high_score = crate::tutor::TutorScores::load_or_default().high_score(lesson_num);
        // Provider — unregister any stale one (re-open path) then register fresh.
        let provider_id = buffer_id.0 as u64 ^ crate::tutor::TUTOR_PROVIDER_TAG;
        self.virtual_row_providers
            .unregister(buffer_id, provider_id);
        let handle = lattice_cells::SimpleHeaderlineHandle::new(
            crate::tutor::TutorViewState::default(),
            crate::tutor::render_tutor_headerline,
        );
        handle.update(|s| s.update_for_display(&session, crate::tutor::TutorHudKind::Normal));
        self.virtual_row_providers
            .register(buffer_id, std::sync::Arc::new(handle.provider(provider_id)));
        // Store the state handle and session as buffer-locals.
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(crate::tutor::TutorHeaderlineState(handle));
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(session);
        // Activate tutor-mode on this buffer.
        let proto_id = lattice_protocol::ids::BufferId::new(buffer_id.0 as u64);
        let mut active = self.active_modes.remove(&buffer_id).unwrap_or_default();
        if let Err(e) = self.mode_registry.load_full().activate_minor(
            &mut active,
            &self.mode_guards,
            &self.config,
            &self.event_bus,
            &self.services,
            proto_id,
            crate::tutor::TutorMode::mode_id(),
            self.capabilities_for_proto(proto_id),
        ) {
            self.note_activation_failure(buffer_id, crate::tutor::TutorMode::mode_id(), &e);
        }
        self.active_modes.insert(buffer_id, active);
        signals
    }

    /// `<CR>` / `<C-j>` in tutor-mode.
    ///
    /// - **GameOver**: skip past the current exercise (lose this one;
    ///   advance without scoring).
    /// - **Lesson complete**: open the next lesson, or show AllComplete
    ///   if on the final lesson.
    /// - **ManualAdvance exercise**: always advance (no penalty).
    /// - **Condition met** (auto-detected by tick): advance.
    /// - **Condition NOT met**: drain one life. If lives hit 0 the
    ///   session transitions to `GameOver` and the headerline updates.
    pub fn do_tutor_advance(&mut self) -> Vec<RendererSignal> {
        use crate::tutor::{SuccessCondition, TutorGameState};

        let buffer_id = self.document_buffer_id;
        let Some(mut session) = self
            .buffer_locals
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::tutor::TutorSession>())
            .cloned()
        else {
            return Vec::new();
        };

        // GameOver: <CR> skips — advance without scoring.
        if session.state == TutorGameState::GameOver {
            session.advance();
            self.buffer_locals
                .entry(buffer_id)
                .or_default()
                .insert(session.clone());
            return self.tutor_after_advance(buffer_id, session);
        }

        // Lesson already complete: open the next lesson.
        if session.is_complete() {
            return self.tutor_open_next_or_complete(session);
        }

        let is_manual = matches!(
            session.current_exercise().map(|e| &e.success),
            Some(SuccessCondition::ManualAdvance)
        );

        if is_manual {
            session.advance();
            self.buffer_locals
                .entry(buffer_id)
                .or_default()
                .insert(session.clone());
            return self.tutor_after_advance(buffer_id, session);
        }

        // Peek: is the condition currently met?
        let text = self.document.snapshot().buffer.as_string();
        let owned: Vec<String> = text.lines().map(|l| l.to_owned()).collect();
        let lines: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();

        if session.is_condition_met(&lines) {
            session.advance();
            self.buffer_locals
                .entry(buffer_id)
                .or_default()
                .insert(session.clone());
            return self.tutor_after_advance(buffer_id, session);
        }

        // Wrong answer: drain a life.
        session.drain_life();
        self.tutor_update_headerline(buffer_id, &session, crate::tutor::TutorHudKind::Normal);
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(session);
        Vec::new()
    }

    /// `<C-k>` in tutor-mode.
    ///
    /// - **GameOver**: reload the current lesson from scratch (retry).
    /// - **Active**: retreat to the previous exercise.
    pub fn do_tutor_retreat(&mut self) -> Vec<RendererSignal> {
        use crate::tutor::TutorGameState;

        let buffer_id = self.document_buffer_id;
        let Some(session) = self
            .buffer_locals
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::tutor::TutorSession>())
            .cloned()
        else {
            return Vec::new();
        };

        if session.state == TutorGameState::GameOver {
            // Full lesson reload: resets buffer text + spawns a fresh session.
            return self.do_tutor(Some(session.lesson));
        }

        let mut session = session;
        session.retreat();
        self.tutor_update_headerline(buffer_id, &session, crate::tutor::TutorHudKind::Normal);
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(session);
        Vec::new()
    }

    /// After a successful `session.advance()`, either open the next lesson
    /// (lesson complete) or update the headerline for the new exercise.
    fn tutor_after_advance(
        &mut self,
        buffer_id: BufferId,
        session: crate::tutor::TutorSession,
    ) -> Vec<RendererSignal> {
        if session.is_complete() {
            return self.tutor_open_next_or_complete(session);
        }
        self.tutor_update_headerline(buffer_id, &session, crate::tutor::TutorHudKind::Normal);
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(session);
        Vec::new()
    }

    /// Open the next lesson, or mark AllComplete on the final lesson.
    /// Saves the lesson's score to disk on every completion.
    fn tutor_open_next_or_complete(
        &mut self,
        mut session: crate::tutor::TutorSession,
    ) -> Vec<RendererSignal> {
        use crate::tutor::TutorGameState;
        // T.B: persist high score whenever a lesson is cleared.
        let mut scores = crate::tutor::TutorScores::load_or_default();
        let new_record = scores.record(session.lesson, session.score);
        scores.save();
        if new_record {
            session.high_score = session.score;
        }
        let next = session.lesson + 1;
        if next <= session.total_lessons {
            return self.do_tutor(Some(next));
        }
        // Final lesson complete.
        let buffer_id = self.document_buffer_id;
        session.state = TutorGameState::AllComplete;
        self.tutor_update_headerline(buffer_id, &session, crate::tutor::TutorHudKind::Normal);
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(session);
        Vec::new()
    }

    /// Ask the tutor headerline provider for `buffer_id` to recompute
    /// its HUD from `session`. All formatting stays in `TutorViewState`.
    fn tutor_update_headerline(
        &self,
        buffer_id: BufferId,
        session: &crate::tutor::TutorSession,
        kind: crate::tutor::TutorHudKind,
    ) {
        if let Some(state) = self
            .buffer_locals
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::tutor::TutorHeaderlineState>())
        {
            state.0.update(|s| s.update_for_display(session, kind));
        }
    }

    /// Auto-detect success per tick. Awards score when the condition
    /// is first met; updates the headerline to a "STAGE CLEAR" prompt.
    /// Does NOT drain lives — that only happens on explicit `<CR>` via
    /// `do_tutor_advance`. Called from `publish_render_state`.
    pub(crate) fn check_tutor_session(&mut self) {
        let buffer_id = self.document_buffer_id;
        let Some(mut session) = self
            .buffer_locals
            .get(&buffer_id)
            .and_then(|l| l.get::<crate::tutor::TutorSession>())
            .cloned()
        else {
            return;
        };
        use crate::tutor::TutorGameState;
        if session.state == TutorGameState::GameOver || session.is_complete() {
            return;
        }
        let current_version = self.document.version();
        if current_version == session.last_version {
            return;
        }
        session.last_version = current_version;
        let text = self.document.snapshot().buffer.as_string();
        let owned: Vec<String> = text.lines().map(|l| l.to_owned()).collect();
        let lines: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
        let success = session.check(&lines);
        let kind = if success {
            crate::tutor::TutorHudKind::StageClear
        } else {
            crate::tutor::TutorHudKind::Normal
        };
        self.tutor_update_headerline(buffer_id, &session, kind);
        self.buffer_locals
            .entry(buffer_id)
            .or_default()
            .insert(session);
    }
}
