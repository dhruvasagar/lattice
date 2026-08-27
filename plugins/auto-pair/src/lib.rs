//! `auto-pair` — the first bundled plugin (AP.2 `auto` style, full pair set).
//!
//! ONE `wasm32-wasip2` component providing three seams (the multi-seam shape
//! proven by AP.1.0):
//!   - **grammar** — the pairing actions, fired on insert-mode chords. Each
//!     opener/closer/quote is its OWN action because a mode keymap binding carries
//!     no args, so the action can't otherwise know which pair fired.
//!   - **modes** — `auto-pair-mode`, a `global`-scope minor mode that OWNS the
//!     insert-mode keymap. Declared the plugin's `default_mode` (AP.4/PM.3), so the
//!     loader's `auto-pair.enabled` gate (default true) enables it out of the box;
//!     `:set auto-pair.enabled=false` turns it off. No init.rs needed.
//!   - **config** — `auto-pair.style` (`auto` | `manual`) + `auto-pair.close-key`.
//!
//! **The `auto` style** for the bracket pairs `() [] {}` and the quote pairs
//! `"" '' `` `` (all share three primitives):
//!   - **open** (`(` `[` `{`) → insert the pair, caret BETWEEN,
//!   - **close** (`)` `]` `}`) → step over a matching closer if it sits after the
//!     caret (a pure `selection-change`, no edit), else insert it,
//!   - **quote** (`"` `'` `` ` ``, same-char pairs) → step over if the same quote
//!     is next, else insert the pair caret-between.
//!
//! **The `manual` style (AP.3)** — the pair keys self-insert; a single close key
//! (default `<C-j>`) closes the nearest unmatched opener, found by scanning the
//! enclosing lexical scope backward (`find_pair`, §3), bounded via the
//! tree-sitter seam's `enclosing` query (§7) with a line-capped fallback where
//! there's no parse tree. The style is read live from `auto-pair.style` (the
//! grammar guest reads the shared config registry), so `:set` flips it without
//! re-registration. Backspace inside an empty pair deletes both chars, else
//! declines to the builtin. This makes auto-pair the first end-to-end consumer of
//! the tree-sitter seam (TS.3).
//!
//! Word-boundary / string-comment suppression (don't pair a `'` inside `don't`)
//! + per-language pair tables are deferred to v2.

wit_bindgen::generate!({
    world: "auto-pair-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::config::OptionType;
use lattice::plugin_host::help;
// TS.1: the tree-snapshot handle rides `apply-action` (unused until AP.3's
// manual style queries `enclosing`; the `auto` style reads only raw text).
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, ApplyEditPayload, Args, Edit, EditKind, Effect, ExCommandContext,
    MotionContext, MotionResult, OperatorContext, Position, Range, TextObjectContext,
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
// AP.3 — the manual close key + backspace.
const CB_CLOSE_MANUAL: u32 = 10;
const CB_BACKSPACE: u32 = 11;

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

/// Step the caret one right with no text change — a cursor-only
/// move, no spurious `DocumentChanged`.
fn step_over(ctx: &ActionContext) -> Vec<Effect> {
    vec![Effect::CursorMove(one_right(ctx.cursor))]
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

// ── the pairs table (§4) — the plugin's own data; the host never interprets it ──
fn is_closer(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '>' | '\'' | '"' | '`')
}
fn is_symmetric(c: char) -> bool {
    matches!(c, '\'' | '"' | '`')
}
/// The closer for an opener (symmetric chars map to themselves), or `None` if `c`
/// isn't an opener.
fn closer_for_opener(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        '\'' => Some('\''),
        '"' => Some('"'),
        '`' => Some('`'),
        _ => None,
    }
}

/// The manual-close algorithm (§3), a faithful port of `vim-pairify#find_pair`:
/// scan `text` **backward** maintaining a stack of unmatched closers, and return
/// the closer to insert at the nearest UNMATCHED opener — or `None` (fall
/// through) when nothing above is open. `text` is the enclosing scope up to the
/// caret (§7), so the scan is bounded regardless of file size, and `find_pair`'s
/// early-exit trims it to the first unmatched opener.
fn find_pair(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut stack: Vec<char> = Vec::new();
    let mut i = chars.len();
    while i > 0 {
        i -= 1;
        let c = chars[i];
        if is_closer(c) {
            // `>` preceded by a space is a comparison operator (`a -> b`), not a
            // bracket — skip.
            if c == '>' && i > 0 && chars[i - 1] == ' ' {
                continue;
            }
            if stack.last() == Some(&c) && is_symmetric(c) {
                stack.pop(); // a balanced symmetric (quote) pair
            } else {
                stack.push(c);
            }
        } else if let Some(close) = closer_for_opener(c) {
            // `<` followed by a space is a comparison operator (`a < b`) — skip.
            if c == '<' && i + 1 < chars.len() && chars[i + 1] == ' ' {
                continue;
            }
            match stack.last() {
                // A bracket that matches the pending closer on top → balanced.
                Some(&top) if closer_for_opener(c) == Some(top) => {
                    stack.pop();
                }
                // Nothing pending above → this opener is the nearest unmatched.
                None => return Some(close.to_string()),
                // An opener under a non-matching closer: keep scanning.
                _ => {}
            }
        }
    }
    // A stray closer with no opener above: vim-pairify returns the stack bottom.
    stack.first().map(|c| c.to_string())
}

/// Read the live style option (AP.3). `auto` (default) or `manual`. The plugin
/// uses the SHORT name `style`; the host auto-namespaces it to `auto-pair.style`
/// (the name a user sets). The grammar guest reads the SHARED editor config
/// registry (wired at instantiate time), so `:set auto-pair.style=manual` flips
/// behavior live — no keymap re-registration.
fn is_manual() -> bool {
    config::get_option("style").as_deref() == Some("manual")
}

fn one_left(pos: Position) -> Position {
    Position {
        line: pos.line,
        byte: pos.byte.saturating_sub(1),
    }
}

/// The single byte before the caret (empty at BOL / on a read error).
fn char_before(ctx: &ActionContext, doc: &Document) -> String {
    if ctx.cursor.byte == 0 {
        return String::new();
    }
    doc.get_text_range(Range {
        start: one_left(ctx.cursor),
        end: ctx.cursor,
    })
    .unwrap_or_default()
}

/// The block/function kinds that bound the manual backward scan (§7). Root kinds
/// (`source_file`/`module`) are deliberately omitted — matching them would scope
/// to the whole file, defeating the bound; a cursor in none of these falls back
/// to a line-capped slice.
fn scope_kinds() -> Vec<String> {
    [
        "block",
        "statement_block",
        "function_item",
        "function_definition",
        "function_declaration",
        "arrow_function",
        "closure_expression",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// The scope text from the enclosing lexical scope's start up to the caret (§7).
/// Uses the tree-sitter seam's `enclosing` to bound the scan; with no parse tree
/// (or no enclosing scope), degrades to a line-capped cursor-backward slice —
/// never a whole-buffer materialization.
fn scope_text_before_cursor(
    ctx: &ActionContext,
    doc: &Document,
    tree: Option<&TreeSnapshot>,
) -> String {
    let scan_start = tree
        .and_then(|t| t.enclosing(ctx.cursor, &scope_kinds()))
        .map(|node| node.byte_range().start)
        .unwrap_or_else(|| Position {
            line: ctx.cursor.line.saturating_sub(200),
            byte: 0,
        });
    doc.get_text_range(Range {
        start: scan_start,
        end: ctx.cursor,
    })
    .unwrap_or_default()
}

/// Manual close key: scan the enclosing scope backward and close the nearest
/// unmatched opener, or DECLINE (fall through — §6) when nothing is open.
fn manual_close(ctx: &ActionContext, doc: &Document, tree: Option<&TreeSnapshot>) -> Vec<Effect> {
    let text = scope_text_before_cursor(ctx, doc, tree);
    match find_pair(&text) {
        Some(closer) => insert_one(ctx, &closer),
        None => vec![Effect::Declined],
    }
}

/// Backspace inside an empty pair (`()` / `""` with the caret between) deletes
/// BOTH chars; otherwise DECLINES to the builtin backspace (never reimplements
/// it). Active in both styles.
fn backspace(ctx: &ActionContext, doc: &Document) -> Vec<Effect> {
    let before = char_before(ctx, doc);
    let after = char_after(ctx, doc);
    let empty_pair = match (before.chars().next(), after.chars().next()) {
        (Some(b), Some(a)) => closer_for_opener(b) == Some(a),
        _ => false,
    };
    if empty_pair {
        vec![Effect::ApplyEdit(ApplyEditPayload {
            target: ctx.buffer_id,
            edit: Edit {
                range: Range {
                    start: one_left(ctx.cursor),
                    end: one_right(ctx.cursor),
                },
                kind: EditKind::Replace(String::new()),
            },
            cursor: Some(one_left(ctx.cursor)),
        })]
    } else {
        vec![Effect::Declined]
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
            (
                "auto-pair-close-manual",
                "close the nearest unmatched opener in scope (manual style)",
                CB_CLOSE_MANUAL,
            ),
            (
                "auto-pair-backspace",
                "delete an empty pair, else fall through to normal backspace",
                CB_BACKSPACE,
            ),
        ] {
            grammar::register_action(name, doc, &spec(), cb);
        }
    }

    /// `auto-pair-mode` owns its insert-mode keymap — bindings land at
    /// `MinorMode(auto-pair-mode)`, never the builtin layer. Bindings target the
    /// plugin's OWN grammar actions by bare name (`provides` lists grammar before
    /// modes, so they resolve at bind time).
    fn register_modes() {
        let bind = |chord: &str, command: &str| ModeKeymapBinding {
            binding_mode: BindingMode::Insert,
            chord: chord.to_string(),
            command: command.to_string(),
        };
        modes::register_mode(&ModeDeclaration {
            id: "auto-pair-mode".to_string(),
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
                // AP.3: the manual close key (default `<C-j>`) and backspace. Both
                // read state at dispatch and DECLINE when they have nothing to do,
                // so they compose with the rest of the keymap (the close key only
                // acts in `manual` style; backspace only on an empty pair).
                bind("<C-j>", "auto-pair-close-manual"),
                bind("<BS>", "auto-pair-backspace"),
            ],
            // OM.2: majors claim a language; this is a minor, so `none`.
            target_language: None,
            // MO.1: this mode sets no options for its buffers.
            options: vec![],
        });
    }

    /// CR.3: this plugin's own `:help auto-pair` page.
    ///
    /// The markdown is `include_str!`'d from this plugin's `doc/`, so it is
    /// compiled into this component. The manual therefore ships with the
    /// plugin, is removed when the plugin is, and never enters lattice's own
    /// embedded-doc budget. An empty topic name registers at the bare plugin
    /// id, so the page answers to `:help auto-pair` rather than
    /// `:help auto-pair.auto-pair`.
    fn register_help_topics() {
        let _ = help::register_topic(
            "",
            "Auto-close brackets and quotes, or close the nearest unmatched opener on one key.",
            include_str!("../doc/auto-pair.md"),
            &["auto-pair".to_string()],
        );
    }

    fn register_options() {
        // Short names — the host auto-namespaces them by plugin id, so these
        // register as `auto-pair.style` / `auto-pair.close-key`.
        config::register_option(
            "style",
            OptionType::String,
            "auto",
            "auto = complete pairs on the opening key; manual = the close key emits the pair",
        );
        config::register_option(
            "close-key",
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
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        // AP.3: in `manual` style the pair keys (1..=9) self-insert — the action
        // DECLINES so the typed char lands via the builtin, and only the close key
        // + backspace act. In `auto` style the close key declines instead.
        let manual = is_manual();
        if manual && (CB_OPEN_ROUND..=CB_QUOTE_BACKTICK).contains(&callback) {
            return Ok(vec![Effect::Declined]);
        }
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
            // The manual close key acts only in `manual` style; in `auto` it
            // declines so `<C-j>` does whatever else it's bound to.
            CB_CLOSE_MANUAL if manual => manual_close(&ctx, doc, tree),
            CB_CLOSE_MANUAL => vec![Effect::Declined],
            CB_BACKSPACE => backspace(&ctx, doc),
            other => return Err(format!("auto-pair: unknown action callback {other}")),
        })
    }

    fn apply_motion(
        _c: u32,
        _ctx: MotionContext,
        _doc: &Document,
    ) -> Result<MotionResult, String> {
        Err("auto-pair: no motions".into())
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("auto-pair: no operators".into())
    }
    fn apply_text_object(
        _c: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
    ) -> Result<Range, String> {
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
