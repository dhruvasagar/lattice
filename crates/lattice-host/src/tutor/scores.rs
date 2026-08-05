//! T.B — Tutor high-score persistence.
//!
//! `TutorScores` is a thin wrapper around a per-lesson high-score map
//! written to `~/.local/share/lattice/tutor-scores.toml` (XDG data
//! dir on Linux/macOS; `%APPDATA%\lattice\` on Windows via `dirs`).
//!
//! The file is read at `do_tutor` time so the HUD can show the current
//! high score, and written whenever a lesson is completed or the final
//! lesson is beaten.  All I/O errors are logged at `debug!` and
//! silently swallowed — the game continues without persistence rather
//! than surfacing a confusing error to the user.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ---- Data model -------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct TutorScores {
    /// lesson number (1-based) → all-time high score for that lesson.
    #[serde(default)]
    pub lessons: HashMap<u32, u32>,
}

impl TutorScores {
    /// High score for `lesson`, or 0 if never completed.
    pub fn high_score(&self, lesson: u32) -> u32 {
        self.lessons.get(&lesson).copied().unwrap_or(0)
    }

    /// Update the high score for `lesson` if `score` beats the current
    /// record.  Returns `true` when the record was broken.
    pub fn record(&mut self, lesson: u32, score: u32) -> bool {
        let entry = self.lessons.entry(lesson).or_insert(0);
        if score > *entry {
            *entry = score;
            true
        } else {
            false
        }
    }

    // ---- I/O -----------------------------------------------------------

    /// Load from the XDG data dir, returning a default (empty) instance
    /// on any I/O or parse error.
    pub fn load_or_default() -> Self {
        let Some(path) = scores_path() else {
            return Self::default();
        };
        let Ok(bytes) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(
                    target: "lattice_host::tutor_scores",
                    path = %path.display(),
                    error = %e,
                    "tutor-scores: parse error, starting fresh"
                );
                Self::default()
            }
        }
    }

    /// Persist to the XDG data dir.  Logs and swallows any I/O errors.
    pub fn save(&self) {
        let Some(path) = scores_path() else { return };
        if let Some(parent) = path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            tracing::debug!(
                target: "lattice_host::tutor_scores",
                path = %parent.display(),
                error = %e,
                "tutor-scores: could not create data dir"
            );
            return;
        }
        match toml::to_string_pretty(self) {
            Ok(text) => {
                if let Err(e) = std::fs::write(&path, text) {
                    tracing::debug!(
                        target: "lattice_host::tutor_scores",
                        path = %path.display(),
                        error = %e,
                        "tutor-scores: write failed"
                    );
                }
            }
            Err(e) => {
                tracing::debug!(
                    target: "lattice_host::tutor_scores",
                    error = %e,
                    "tutor-scores: serialization failed"
                );
            }
        }
    }
}

fn scores_path() -> Option<PathBuf> {
    dirs::data_local_dir().map(|d| d.join("lattice").join("tutor-scores.toml"))
}
