//! Tutor subsystem — session data, minor mode, headerline, and score persistence.

pub mod engine;
pub mod mode;
pub mod scores;
pub mod session;

pub use mode::{
    register_tutor_modes, render_tutor_headerline, TutorHeaderlineState, TutorHudKind, TutorMode,
    TutorViewState, TUTOR_PROVIDER_TAG,
};
pub use scores::TutorScores;
pub use session::{SuccessCondition, TutorExercise, TutorGameState, TutorSession, MAX_LIVES};
