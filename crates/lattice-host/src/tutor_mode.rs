//! T.3 — TutorMode minor mode + headerline provider.
//!
//! `TutorMode` is a marker minor mode activated on the tutor buffer
//! by `do_tutor`. It owns:
//!   - the `<CR>` / `:tutor-next` / `:tutor-prev` keymap layer
//!   - `TutorHeaderlineProvider` — emits a single virtual row above
//!     line 0 showing lesson/exercise progress and the current hint.
//!
//! T.4 wires `TutorSession` as a `BufferLocal` and populates the
//! headerline's `TutorViewState` from the live session.

use std::sync::{Arc, Mutex};

use lattice_cells::{
    AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};
use lattice_mode::registry::ModeRegistry;
use lattice_mode::{Keymap, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

// ──────────────────────────────────────────────────────────────
// TutorMode — the minor mode
// ──────────────────────────────────────────────────────────────

/// `tutor-mode` minor mode. Marker bit: activated on the tutor
/// buffer by `do_tutor`; the keymap layer (keyed by this mode's
/// `ModeId`) gates the `<CR>`/`:tutor-next`/`:tutor-prev` chords
/// so they are invisible on non-tutor buffers.
///
/// Bindings are pushed via `tutor_mode_layer_bindings` at boot
/// (same explicit `push_layer` path as `diff-mode`; migrating to
/// the `Mode::keymap()` translate pass is tracked separately).
pub struct TutorMode;

impl TutorMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("tutor-mode")
    }
}

impl Mode for TutorMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// K.2.4 path: bindings contributed by the mode itself, picked up
    /// by `translate_mode_keymaps` at boot. Resolves `"ex:tutor-next"`
    /// and `"ex:tutor-prev"` against the `CommandRegistry` (registered
    /// in `lattice_grammar::ex_commands::populate`).
    fn keymap(&self) -> Keymap {
        use std::sync::OnceLock;
        static ENTRIES: OnceLock<Vec<lattice_mode::KeymapEntry>> = OnceLock::new();
        Keymap::from_entries(ENTRIES.get_or_init(|| {
            vec![
                lattice_mode::keymap_entry! {
                    mode: Normal, chord: "<CR>",
                    doc: "Advance to the next tutor exercise (or lesson)",
                    cmd: "ex:tutor-next"
                },
                lattice_mode::keymap_entry! {
                    mode: Normal, chord: "<C-j>",
                    doc: "Advance to the next tutor exercise (or lesson)",
                    cmd: "ex:tutor-next"
                },
                lattice_mode::keymap_entry! {
                    mode: Normal, chord: "<C-k>",
                    doc: "Retreat to the previous tutor exercise",
                    cmd: "ex:tutor-prev"
                },
            ]
        }))
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `tutor-mode` against `registry`. Called from the
/// editor boot path before `translate_mode_keymaps` runs the
/// K.2.4 pass that picks up `TutorMode::keymap()`.
pub fn register_tutor_modes(registry: &mut ModeRegistry) {
    registry
        .register(TutorMode)
        .expect("tutor-mode must register without conflict");
}

// ──────────────────────────────────────────────────────────────
// TutorViewState — shared between mode and provider
// ──────────────────────────────────────────────────────────────

/// Display state for the tutor headerline. Updated by T.4 when
/// the session advances or a hint becomes active.
#[derive(Debug, Default)]
pub struct TutorViewState {
    /// Current headerline text. Empty → provider emits nothing.
    pub text: String,
    /// Monotonic counter; bumped on every update so the worker
    /// detects staleness without comparing text.
    pub version: u64,
}

impl TutorViewState {
    /// Update text and bump version. Returns the new version.
    pub fn update(&mut self, text: String) -> u64 {
        self.text = text;
        self.version = self.version.wrapping_add(1);
        self.version
    }
}

// ──────────────────────────────────────────────────────────────
// TutorHeaderlineProvider — virtual row above line 0
// ──────────────────────────────────────────────────────────────

pub const TUTOR_PROVIDER_TAG: u64 = 0x7475_746F_725F_6865; // "tutor_he"

// ──────────────────────────────────────────────────────────────
// TutorHeaderlineState — BufferLocal handle to the shared view state
// ──────────────────────────────────────────────────────────────

/// Buffer-local handle to the shared `TutorViewState` arc. Stored
/// alongside `TutorSession` so `check_tutor_session` can update the
/// headerline text without holding a reference to the provider itself.
#[derive(Clone)]
pub struct TutorHeaderlineState(pub Arc<Mutex<TutorViewState>>);

impl lattice_mode::BufferLocal for TutorHeaderlineState {
    const NAME: &'static str = "tutor-mode.headerline";
    const DOC: &'static str = "Shared arc for the tutor headerline provider's view state.";
    const OWNER_MODE: &'static str = "tutor-mode";

    fn describe(&self) -> String {
        self.0
            .lock()
            .map(|s| format!("version={}", s.version))
            .unwrap_or_default()
    }
}

// ──────────────────────────────────────────────────────────────
// Headerline text helper
// ──────────────────────────────────────────────────────────────

/// Format the retro-game HUD headerline for `session`.
///
/// Active:   ` ♥♥♥ | LV.1-3 | SCORE:   350 | Delete a word  Hint: diw `
/// Pass:     ` *** STAGE CLEAR! *** | LV.1-3 | SCORE:   500 | <CR>=next `
/// GameOver: ` === GAME OVER === | LV.1-3 | SCORE:   200 | <CR>=skip  <C-k>=retry `
/// Complete: ` *** LESSON CLEAR! *** | LV.1 | SCORE:  2850 | <CR>=next lesson `
/// AllDone:  ` *** YOU WIN! *** | ALL LESSONS DONE | FINAL SCORE: 12500 `
pub fn tutor_headerline_text(session: &crate::tutor::TutorSession) -> String {
    use crate::tutor::TutorGameState;

    let lv = format!("LV.{}-{}", session.lesson, session.current + 1);
    let sc = format!("SCORE: {:>5}", session.score);

    match &session.state {
        TutorGameState::AllComplete => {
            format!(" *** YOU WIN! *** | ALL LESSONS DONE | FINAL SCORE: {} ", session.score)
        }
        TutorGameState::GameOver => {
            format!(" === GAME OVER === | {} | {} | <CR>=skip  <C-k>=retry ", lv, sc)
        }
        TutorGameState::Active => {
            if session.is_complete() {
                // Lesson complete — waiting for user to press <CR> to open next.
                format!(
                    " *** LESSON CLEAR! *** | LV.{} | {} | <CR>=next lesson ",
                    session.lesson, sc
                )
            } else {
                let hearts = format_hearts(session.lives, crate::tutor::MAX_LIVES);
                match session.current_exercise() {
                    None => format!(" {} | {} | {} ", hearts, lv, sc),
                    Some(ex) => {
                        let hint = if session.attempt_count >= 2 && !ex.hint.is_empty() {
                            format!("  Hint: {}", ex.hint)
                        } else {
                            String::new()
                        };
                        format!(" {} | {} | {} | {}{}", hearts, lv, sc, ex.description, hint)
                    }
                }
            }
        }
    }
}

fn format_hearts(lives: u8, max: u8) -> String {
    (0..max)
        .map(|i| if i < lives { '♥' } else { '♡' })
        .collect()
}

/// Emits one virtual row above line 0 of the tutor buffer showing
/// lesson/exercise progress and the exercise description or hint.
///
/// T.4 populates `state` via `TutorViewState::update` and wakes
/// the virtual-rows worker so the renderer sees fresh rows.
#[derive(Debug)]
pub struct TutorHeaderlineProvider {
    /// Stable id: XOR of the buffer's numeric id with the tag
    /// constant so each buffer gets a unique provider id.
    provider_id: ProviderId,
    pub state: Arc<Mutex<TutorViewState>>,
}

impl TutorHeaderlineProvider {
    pub fn new(buffer_id: u64) -> Self {
        Self {
            provider_id: buffer_id ^ TUTOR_PROVIDER_TAG,
            state: Arc::new(Mutex::new(TutorViewState::default())),
        }
    }
}

impl VirtualRowProvider for TutorHeaderlineProvider {
    fn id(&self) -> ProviderId {
        self.provider_id
    }

    fn version(&self) -> u64 {
        self.state.lock().map(|s| s.version).unwrap_or(0)
    }

    fn collect(&self) -> Vec<VirtualRow> {
        let text = self
            .state
            .lock()
            .map(|s| s.text.clone())
            .unwrap_or_default();

        if text.is_empty() {
            return Vec::new();
        }

        let cells: Arc<[Cell]> = text
            .chars()
            .map(|c| Cell::with_codepoint(c as u32))
            .collect::<Vec<_>>()
            .into();

        vec![VirtualRow {
            anchor_line: 0,
            position: AnchorPosition::Above,
            cells,
            height: 1,
            kind: VirtualRowKind::Generic,
        }]
    }
}
