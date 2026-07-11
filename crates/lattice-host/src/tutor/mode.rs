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

use std::sync::Arc;

use lattice_cells::{Cell, HeaderlineRow, SimpleHeaderlineHandle};
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
    /// Colored spans: `(text, 0xRRGGBB fg)`. Empty → provider emits nothing.
    pub spans: Vec<(String, u32)>,
    /// Row background color (`0xRRGGBB`); `0` = transparent.
    pub row_bg: u32,
}

impl TutorViewState {
    /// Recompute spans from `session` using the retro HUD palette.
    /// `kind` selects the display variant (normal vs. stage-clear flash).
    /// Version tracking is handled by [`SimpleHeaderlineHandle::update`].
    pub fn update_for_display(&mut self, session: &super::TutorSession, kind: TutorHudKind) {
        let (spans, row_bg) = match kind {
            TutorHudKind::Normal => tutor_headerline_spans(session),
            TutorHudKind::StageClear => tutor_stage_clear_spans(session),
        };
        self.spans = spans;
        self.row_bg = row_bg;
    }
}

/// Selects which HUD variant to render.
///
/// `dispatch.rs` passes this to `TutorViewState::update_for_display`
/// so that all formatting stays inside `tutor_mode`.
#[derive(Copy, Clone, Debug)]
pub enum TutorHudKind {
    /// Normal display — derives presentation from `session.state`.
    Normal,
    /// Stage-clear flash — shown immediately when the auto-detector
    /// confirms the condition is met, before the user presses `<CR>`.
    StageClear,
}

// ──────────────────────────────────────────────────────────────
// TutorHeaderlineProvider — virtual row above line 0
// ──────────────────────────────────────────────────────────────

pub const TUTOR_PROVIDER_TAG: u64 = 0x7475_746F_725F_6865; // "tutor_he"

// ──────────────────────────────────────────────────────────────
// TutorHeaderlineState — BufferLocal handle to the shared view state
// ──────────────────────────────────────────────────────────────

/// Buffer-local handle for updating the tutor headerline. Wraps a
/// [`SimpleHeaderlineHandle<TutorViewState>`] so dispatch can push
/// session state without holding a reference to the provider itself.
#[derive(Clone)]
pub struct TutorHeaderlineState(pub SimpleHeaderlineHandle<TutorViewState>);

impl lattice_mode::BufferLocal for TutorHeaderlineState {
    const NAME: &'static str = "tutor-mode.headerline";
    const DOC: &'static str = "SimpleHeaderlineHandle for the tutor buffer's sticky HUD row.";
    const OWNER_MODE: &'static str = "tutor-mode";

    fn describe(&self) -> String {
        format!("version={}", self.0.version())
    }
}

// ──────────────────────────────────────────────────────────────
// Headerline text helper
// ──────────────────────────────────────────────────────────────

// ──────────────────────────────────────────────────────────────
// Retro HUD palette — all values are 0xRRGGBB
// ──────────────────────────────────────────────────────────────

mod pal {
    pub const BG: u32 = 0x0a0a1a;
    pub const HEART_ON: u32 = 0xff2a2a;
    pub const HEART_OFF: u32 = 0x4a2020;
    pub const SEP: u32 = 0x333355;
    pub const LV_LABEL: u32 = 0x00ccaa;
    pub const LV_NUM: u32 = 0xe0e0e0;
    pub const SCORE_LABEL: u32 = 0xffd700;
    pub const SCORE_VAL: u32 = 0xffffff;
    pub const HI_LABEL: u32 = 0xff8c00;
    pub const HI_VAL: u32 = 0xffd700;
    pub const DESC: u32 = 0x88ccff;
    pub const HINT_LABEL: u32 = 0xff8c00;
    pub const HINT_VAL: u32 = 0xffcc44;
    pub const CLEAR: u32 = 0x00ff88;
    pub const GAME_OVER: u32 = 0xff4444;
    pub const WIN: u32 = 0xff00ff;
    pub const DONE: u32 = 0xffd700;
    pub const KEY_HINT: u32 = 0x666699;
}

/// Build colored HUD spans for the normal session display.
///
/// Returns `(spans, row_bg)` where each span is `(text, 0xRRGGBB fg)`.
fn tutor_headerline_spans(session: &super::TutorSession) -> (Vec<(String, u32)>, u32) {
    use super::TutorGameState;

    let mut s: Vec<(String, u32)> = Vec::new();
    let sep = (" | ".to_owned(), pal::SEP);

    match &session.state {
        TutorGameState::AllComplete => {
            s.push((" *** YOU WIN! *** ".to_owned(), pal::WIN));
            s.push(sep.clone());
            s.push(("ALL LESSONS DONE".to_owned(), pal::DONE));
            s.push(sep.clone());
            s.push(("FINAL SCORE: ".to_owned(), pal::SCORE_LABEL));
            s.push((format!("{}", session.score), pal::SCORE_VAL));
            s.push((" ".to_owned(), pal::SCORE_VAL));
        }
        TutorGameState::GameOver => {
            s.push((" === GAME OVER === ".to_owned(), pal::GAME_OVER));
            s.push(sep.clone());
            s.push(("LV.".to_owned(), pal::LV_LABEL));
            s.push((
                format!("{}-{}", session.lesson, session.current + 1),
                pal::LV_NUM,
            ));
            s.push(sep.clone());
            s.push(("SCORE: ".to_owned(), pal::SCORE_LABEL));
            s.push((format!("{:>5}", session.score), pal::SCORE_VAL));
            s.push(sep.clone());
            s.push(("<CR>=skip  <C-k>=retry ".to_owned(), pal::KEY_HINT));
        }
        TutorGameState::Active => {
            if session.is_complete() {
                let new_record = session.score > session.high_score && session.high_score > 0;
                s.push((" *** LESSON CLEAR! *** ".to_owned(), pal::CLEAR));
                s.push(sep.clone());
                s.push(("LV.".to_owned(), pal::LV_LABEL));
                s.push((format!("{}", session.lesson), pal::LV_NUM));
                s.push(sep.clone());
                s.push(("SCORE: ".to_owned(), pal::SCORE_LABEL));
                s.push((format!("{:>5}", session.score), pal::SCORE_VAL));
                if session.high_score > 0 {
                    s.push(sep.clone());
                    s.push(("HI: ".to_owned(), pal::HI_LABEL));
                    s.push((format!("{:>5}", session.high_score), pal::HI_VAL));
                }
                if new_record {
                    s.push(("  NEW RECORD!".to_owned(), pal::WIN));
                }
                s.push(sep.clone());
                s.push(("<CR>=next lesson ".to_owned(), pal::KEY_HINT));
            } else {
                s.push((" ".to_owned(), pal::SEP));
                for i in 0..super::MAX_LIVES {
                    if i < session.lives {
                        s.push(("♥".to_owned(), pal::HEART_ON));
                    } else {
                        s.push(("♡".to_owned(), pal::HEART_OFF));
                    }
                }
                s.push(sep.clone());
                s.push(("LV.".to_owned(), pal::LV_LABEL));
                s.push((
                    format!("{}-{}", session.lesson, session.current + 1),
                    pal::LV_NUM,
                ));
                s.push(sep.clone());
                s.push(("SCORE: ".to_owned(), pal::SCORE_LABEL));
                s.push((format!("{:>5}", session.score), pal::SCORE_VAL));
                if session.high_score > 0 {
                    s.push(sep.clone());
                    s.push(("HI: ".to_owned(), pal::HI_LABEL));
                    s.push((format!("{:>5}", session.high_score), pal::HI_VAL));
                }
                if let Some(ex) = session.current_exercise() {
                    s.push(sep.clone());
                    s.push((ex.description.clone(), pal::DESC));
                    if session.attempt_count >= 2 && !ex.hint.is_empty() {
                        s.push(("  Hint: ".to_owned(), pal::HINT_LABEL));
                        s.push((ex.hint.clone(), pal::HINT_VAL));
                    }
                }
                s.push((" ".to_owned(), pal::SEP));
            }
        }
    }

    (s, pal::BG)
}

/// STAGE CLEAR flash — shown when auto-detect fires, before `<CR>`.
fn tutor_stage_clear_spans(session: &super::TutorSession) -> (Vec<(String, u32)>, u32) {
    let ex_id = session
        .current_exercise()
        .map(|e| e.id.clone())
        .unwrap_or_default();
    let sep = (" | ".to_owned(), pal::SEP);
    let s = vec![
        (" *** STAGE CLEAR! *** ".to_owned(), pal::CLEAR),
        sep.clone(),
        ("LV.".to_owned(), pal::LV_LABEL),
        (
            format!("{}-{}", session.lesson, session.current + 1),
            pal::LV_NUM,
        ),
        sep.clone(),
        ("SCORE: ".to_owned(), pal::SCORE_LABEL),
        (format!("{:>5}", session.score), pal::SCORE_VAL),
        sep.clone(),
        (format!("Ex {} done", ex_id), pal::DESC),
        ("  <CR>=next ".to_owned(), pal::KEY_HINT),
    ];
    (s, pal::BG)
}

/// Render function for [`SimpleHeaderlineHandle<TutorViewState>`].
///
/// Converts the current session spans into cells and returns a
/// [`HeaderlineRow`] with the tutor's retro background, or `None`
/// when there is nothing to display yet.
pub fn render_tutor_headerline(s: &TutorViewState) -> Option<HeaderlineRow> {
    if s.spans.is_empty() {
        return None;
    }
    let cells: Arc<[Cell]> = s
        .spans
        .iter()
        .flat_map(|(text, fg)| {
            let fg = *fg;
            text.chars().map(move |c| Cell::new(c as u32, fg, 0, 0))
        })
        .collect::<Vec<_>>()
        .into();
    Some(HeaderlineRow {
        cells,
        bg: Some(s.row_bg),
    })
}
