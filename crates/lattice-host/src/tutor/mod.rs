//! Tutor subsystem — session data, minor mode, headerline, and score persistence.

pub mod engine;
pub mod mode;
pub mod scores;
pub mod session;

pub use mode::{
    TUTOR_PROVIDER_TAG, TutorHeaderlineState, TutorHudKind, TutorMode, TutorViewState,
    register_tutor_modes, render_tutor_headerline,
};
pub use scores::TutorScores;
pub use session::{MAX_LIVES, SuccessCondition, TutorExercise, TutorGameState, TutorSession};
