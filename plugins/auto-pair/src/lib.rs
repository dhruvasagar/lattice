//! `auto-pair` — the first bundled plugin (AP.2 `auto` style, full pair set).
//!
//! ONE `wasm32-wasip2` component providing three seams (the multi-seam shape
//! proven by AP.1.0):
//!   - **grammar** — the pairing actions, fired on insert-mode chords. Each
//!     opener/closer/quote is its OWN action because a mode keymap binding carries
//!     no args, so the action can't otherwise know which pair fired.
//!   - **modes** — `auto-pairs-mode`, a `global`-scope minor mode that OWNS the
//!     insert-mode keymap. **Off by default** (CI.3 available-but-off); the user
//!     enables it from `init.rs` (`on_plugin_loaded("auto-pair") → enable_mode`).
//!   - **config** — `auto-pairs-style` (`auto` | `manual`) + `auto-pairs-close-key`.
//!
//! **The `auto` style** for the bracket pairs `() [] {}` and the quote pairs
//! `"" '' `` `` (all share three primitives):
//!   - **open** (`(` `[` `{`) → insert the pair, caret BETWEEN,
//!   - **close** (`)` `]` `}`) → step over a matching closer if it sits after the
//!     caret (a pure `selection-change`, no edit), else insert it,
//!   - **quote** (`"` `'` `` ` ``, same-char pairs) → step over if the same quote
//!     is next, else insert the pair caret-between.
//!
//! Word-boundary / string-comment suppression (don't pair a `'` inside `don't`)
//! is deferred to v2; the `manual` style + backspace need AP.0.2 / AP.0.3.

wit_bindgen::generate!({
    world: "auto-pair-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::config::OptionType;
// TS.1: the tree-snapshot handle rides `apply-action` (unused until AP.3's
// manual style queries `enclosing`; the `auto` style reads only raw text).
use lattice::plugin_host::tree_sitter::TreeSnapshot;
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

// ── callback ids (guest-local) ────────────────────────────────────────────────
const CB_OPEN_ROUND: u32 = 1;
const CB_OPEN_SQUARE: u32 = 2;
const CB_OPEN_CURLY: u32 = 3;
const CB_CLOSE_ROUND: u32 = 4;
const CB_CLOSE_SQUARE: u32 = 5;
const CB_CLOSE_CURLY: u32 = 6;
const CB_QUOTE_DOUBLE: u32 = 7;
const CB_QUOTE_SINGLE: u32 = 8;
const CB_QUOTE_BACKTICK: u32 = 9;

/// One byte to the right of `pos` on the same line — the caret "between the pair"
/// after an open, and the "stepped over" caret after a close/quote-skip.
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

/// The single byte after the caret (empty string at EOL / on a read error —
/// which just means "nothing to step over", so insert).
fn char_after(ctx: &ActionContext, doc: &Document) -> String {
    doc.get_text_range(Range {
        start: ctx.cursor,
        end: one_right(ctx.cursor),
    })
    .unwrap_or_default()
}

/// Insert `open`+`close` at the caret and park it BETWEEN them.
fn insert_pair(ctx: &ActionContext, open: &str, close: &str) -> Vec<Effect> {
    vec![Effect::ApplyEdit(ApplyEditPayload {
        target: ctx.buffer_id,
        edit: Edit {
            range: at(ctx.cursor),
            kind: EditKind::Replace(format!("{open}{close}")),
        },
        cursor: Some(one_right(ctx.cursor)),
    })]
}

/// Insert a single char at the caret, caret after it.
fn insert_one(ctx: &ActionContext, ch: &str) -> Vec<Effect> {
    vec![Effect::ApplyEdit(ApplyEditPayload {
        target: ctx.buffer_id,
        edit: Edit {
            range: at(ctx.cursor),
            kind: EditKind::Replace(ch.to_string()),
        },
        cursor: Some(one_right(ctx.cursor)),
    })]
}

/// Step the caret one right with no text change (a collapsed selection — no
/// spurious `DocumentChanged`).
fn step_over(ctx: &ActionContext) -> Vec<Effect> {
    vec![Effect::SelectionChange(SelectionSet {
        selections: vec![Selection {
            anchor: one_right(ctx.cursor),
            head: one_right(ctx.cursor),
            visual: None,
        }],
        primary: 0,
    })]
}

/// A closer (`)` `]` `}`): step over a matching closer already after the caret,
/// else insert it.
fn close(ctx: &ActionContext, doc: &Document, ch: &str) -> Vec<Effect> {
    if char_after(ctx, doc) == ch {
        step_over(ctx)
    } else {
        insert_one(ctx, ch)
    }
}

/// A same-char quote (`"` `'` `` ` ``): step over if the same quote is next
/// (closing a just-opened pair), else insert the pair caret-between.
fn quote(ctx: &ActionContext, doc: &Document, q: &str) -> Vec<Effect> {
    if char_after(ctx, doc) == q {
        step_over(ctx)
    } else {
        insert_pair(ctx, q, q)
    }
}

impl Guest for Component {
    fn register_grammar() {
        let spec = || ActionSpec {
            args_schema: Vec::new(),
        };
        for (name, doc, cb) in [
            ("auto-pair-open-round", "insert ()", CB_OPEN_ROUND),
            ("auto-pair-open-square", "insert []", CB_OPEN_SQUARE),
            ("auto-pair-open-curly", "insert {}", CB_OPEN_CURLY),
            ("auto-pair-close-round", "step over )", CB_CLOSE_ROUND),
            ("auto-pair-close-square", "step over ]", CB_CLOSE_SQUARE),
            ("auto-pair-close-curly", "step over }", CB_CLOSE_CURLY),
            ("auto-pair-quote-double", "pair \"\"", CB_QUOTE_DOUBLE),
            ("auto-pair-quote-single", "pair ''", CB_QUOTE_SINGLE),
            ("auto-pair-quote-backtick", "pair ``", CB_QUOTE_BACKTICK),
        ] {
            grammar::register_action(name, doc, &spec(), cb);
        }
    }

    /// `auto-pairs-mode` owns its insert-mode keymap — bindings land at
    /// `MinorMode(auto-pairs-mode)`, never the builtin layer. Bindings target the
    /// plugin's OWN grammar actions by bare name (`provides` lists grammar before
    /// modes, so they resolve at bind time).
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
                bind("[", "auto-pair-open-square"),
                bind("{", "auto-pair-open-curly"),
                bind(")", "auto-pair-close-round"),
                bind("]", "auto-pair-close-square"),
                bind("}", "auto-pair-close-curly"),
                bind("\"", "auto-pair-quote-double"),
                bind("'", "auto-pair-quote-single"),
                bind("`", "auto-pair-quote-backtick"),
            ],
        });
    }

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
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Ok(match callback {
            CB_OPEN_ROUND => insert_pair(&ctx, "(", ")"),
            CB_OPEN_SQUARE => insert_pair(&ctx, "[", "]"),
            CB_OPEN_CURLY => insert_pair(&ctx, "{", "}"),
            CB_CLOSE_ROUND => close(&ctx, doc, ")"),
            CB_CLOSE_SQUARE => close(&ctx, doc, "]"),
            CB_CLOSE_CURLY => close(&ctx, doc, "}"),
            CB_QUOTE_DOUBLE => quote(&ctx, doc, "\""),
            CB_QUOTE_SINGLE => quote(&ctx, doc, "'"),
            CB_QUOTE_BACKTICK => quote(&ctx, doc, "`"),
            other => return Err(format!("auto-pair: unknown action callback {other}")),
        })
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
