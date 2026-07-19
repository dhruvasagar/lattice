//! `auto-pair` — the first bundled plugin (AP.2: the `auto` style).
//!
//! ONE `wasm32-wasip2` component providing three seams (the multi-seam shape
//! proven by AP.1.0):
//!   - **grammar** — the pairing actions, fired on insert-mode chords. Each
//!     opener/closer is its OWN action because a mode keymap binding carries no
//!     args, so the action can't otherwise know which pair fired (`(` vs `[`).
//!   - **modes** — `auto-pairs-mode`, a `global` minor mode (active on document
//!     buffers) that OWNS the insert-mode keymap: chords bind at
//!     `MinorMode(auto-pairs-mode)`, never the builtin layer (mode-ownership).
//!   - **config** — `auto-pairs-style` (`auto` | `manual`) + `auto-pairs-close-key`.
//!
//! **AP.2 — the `auto` style** (round-bracket pair; AP.3 adds `[] {} "" '' `` ``):
//!   - **open** `(` → insert `()` with the caret BETWEEN (a precise-cursor
//!     `apply-edit`, AP.2's edit-model extension),
//!   - **close** `)` → if a `)` already sits after the caret, STEP OVER it (a
//!     pure caret move via `selection-change`, no text change); otherwise insert
//!     `)`.
//!
//! **Backspace is deferred to Wave 2.** Deleting the empty pair on `<BS>` needs
//! the action to DECLINE to the builtin backspace when the caret is not inside a
//! pair (AP.0.2 fall-through). Binding `<BS>` without that would force the plugin
//! to reimplement normal backspace (grapheme deletion, line-joins) — reinventing
//! the builtin, the wrong trade. It lands with AP.0.2.

wit_bindgen::generate!({
    world: "auto-pair-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::config::OptionType;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, ApplyEditPayload, Args, Edit, EditKind, Effect, ExCommandContext,
    MotionContext, MotionResult, OperatorContext, Position, Range, Selection, SelectionSet,
    TextObjectContext,
};
use lattice::plugin_host::{config, grammar, modes};

struct Component;

// ── callback ids (guest-local; the host passes them back to apply_action) ─────
const CB_OPEN_ROUND: u32 = 1; // `(` → insert `()`, caret between
const CB_CLOSE_ROUND: u32 = 2; // `)` → step over a matching `)`, else insert

/// One byte to the right of `pos` on the same line — the "between the pair" caret
/// after an open, and the "stepped over" caret after a close-skip.
fn one_right(pos: Position) -> Position {
    Position {
        line: pos.line,
        byte: pos.byte + 1,
    }
}

/// An empty range at `pos` (an insertion point).
fn at(pos: Position) -> Range {
    Range {
        start: pos,
        end: pos,
    }
}

impl Guest for Component {
    /// grammar seam — one action per opener/closer (the keymap binds each chord
    /// to the matching action; the names are what the mode keymap resolves).
    fn register_grammar() {
        let spec = || ActionSpec {
            args_schema: Vec::new(),
        };
        grammar::register_action(
            "auto-pair-open-round",
            "insert a matching )",
            &spec(),
            CB_OPEN_ROUND,
        );
        grammar::register_action(
            "auto-pair-close-round",
            "step over a matching )",
            &spec(),
            CB_CLOSE_ROUND,
        );
    }

    /// modes seam — `auto-pairs-mode` owns its insert-mode keymap. `global`:
    /// active on document buffers (never in `*plugin-trace*`, help, the file
    /// tree). Bindings target the plugin's OWN grammar actions by bare name —
    /// resolvable because `provides` lists `grammar` before `modes`.
    fn register_modes() {
        let bind = |chord: &str, command: &str| ModeKeymapBinding {
            binding_mode: BindingMode::Insert,
            chord: chord.to_string(),
            command: command.to_string(),
        };
        modes::register_mode(&ModeDeclaration {
            id: "auto-pairs-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Global,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![
                bind("(", "auto-pair-open-round"),
                bind(")", "auto-pair-close-round"),
            ],
        });
    }

    /// config seam — the style switch (read by the handlers at AP.3 for `manual`)
    /// + the manual close key. Behavior is option-gated inside the handlers, so
    /// the keymap set stays stable across `:set auto-pairs-style=…` (no re-binding).
    fn register_options() {
        config::register_option(
            "auto-pairs-style",
            OptionType::String,
            "auto",
            "auto = complete pairs on the opening key; manual = the close key emits the pair",
        );
        config::register_option(
            "auto-pairs-close-key",
            OptionType::String,
            "<C-j>",
            "insert-mode key that closes the nearest unmatched pair (manual style)",
        );
    }
}

impl GrammarCallbacks for Component {
    fn apply_action(
        callback: u32,
        ctx: ActionContext,
        doc: &Document,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            // `(` → insert `()` and park the caret BETWEEN the pair.
            CB_OPEN_ROUND => Ok(vec![Effect::ApplyEdit(ApplyEditPayload {
                target: ctx.buffer_id,
                edit: Edit {
                    range: at(ctx.cursor),
                    kind: EditKind::Replace("()".to_string()),
                },
                cursor: Some(one_right(ctx.cursor)),
            })]),

            // `)` → if a `)` already sits after the caret, step over it (pure
            // caret move, no text change); otherwise insert `)`.
            CB_CLOSE_ROUND => {
                let next = doc
                    .get_text_range(Range {
                        start: ctx.cursor,
                        end: one_right(ctx.cursor),
                    })
                    .unwrap_or_default();
                if next == ")" {
                    // Step over — move the caret past the existing `)` via a
                    // collapsed selection (no edit ⇒ no spurious change event).
                    Ok(vec![Effect::SelectionChange(SelectionSet {
                        selections: vec![Selection {
                            anchor: one_right(ctx.cursor),
                            head: one_right(ctx.cursor),
                            visual: None,
                        }],
                        primary: 0,
                    })])
                } else {
                    Ok(vec![Effect::ApplyEdit(ApplyEditPayload {
                        target: ctx.buffer_id,
                        edit: Edit {
                            range: at(ctx.cursor),
                            kind: EditKind::Replace(")".to_string()),
                        },
                        cursor: Some(one_right(ctx.cursor)),
                    })])
                }
            }

            other => Err(format!("auto-pair: unknown action callback {other}")),
        }
    }

    fn apply_motion(_c: u32, _ctx: MotionContext) -> Result<MotionResult, String> {
        Err("auto-pair: no motions".into())
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("auto-pair: no operators".into())
    }
    fn apply_text_object(_c: u32, _ctx: TextObjectContext) -> Result<Range, String> {
        Err("auto-pair: no text objects".into())
    }
    fn parse_ex_args(_c: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("auto-pair: no ex-commands".into())
    }
    fn apply_ex_command(_c: u32, _ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        Err("auto-pair: no ex-commands".into())
    }
}

export!(Component);
