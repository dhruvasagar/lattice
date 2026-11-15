//! `refactor` — refactoring, owned by one mode (RF.2).
//!
//! Grows and shrinks the selection one syntax node at a time, from the
//! tree-sitter parse the editor already holds for highlighting — so it works
//! in every language with a grammar, with or without a language server.
//! Design: `docs/dev/architecture/refactor.md`.
//!
//! **Language-agnostic by construction.** Every rule here reads only named
//! nodes and their parents, never a node *kind*, so there is no per-language
//! table to fall out of date and no grammar it silently does nothing in.
//!
//! **Expand / shrink are actions, not `:` commands.** Typing `:` leaves
//! Visual, and an ex-command is not handed the selection, so a
//! `:refactor-shrink` could never know what to shrink. They are reached by
//! `<M-o>` / `<M-i>` and from the command palette, which both see the live
//! selection.
//!
//! **Shrink retraces expand.** Each expand remembers the selection it grew
//! from, per buffer, in this instance's memory: the loader instantiates a
//! grammar plugin once, at load, and every call reuses it (design §6). The
//! history is dropped as soon as the selection is not the one the last
//! expand made — the user moved, so the next shrink would retrace a path
//! they have left.

// The manifest is embedded in the component (PMF.11) — no `plugin.toml`.
//
// Bundled ⇒ pre-granted. It reaches no filesystem, network or subprocess; it
// reads the buffer and its parse tree through the handles `apply-action` is
// given. `tree-sitter` is the editor capability that hands it the tree.
//
// `default_modes` turns `refactor-mode` on out of the box; the loader
// registers `refactor.enabled` (default true) to gate it.
lattice_plugin_sdk::manifest!(
    r#"
id = "refactor"
provides = ["grammar", "modes", "help"]
capabilities = ["grammar:chord"]
editor_capabilities = ["tree-sitter"]
default_modes = ["refactor-mode"]
"#
);

wit_bindgen::generate!({
    world: "refactor-plugin",
    path: "../../crates/lattice-wit/wit",
});

use std::cell::RefCell;
use std::collections::HashMap;

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, ApplyEditPayload, Args, EchoLevel, EchoPayload, Edit, EditKind,
    Effect, ExCommandContext, ExCommandSpec, LatencyClass, ModalState, MotionContext, MotionResult,
    OperatorContext, OperatorSpec, Position, Range, Selection, SelectionSet, SurfaceForm,
    TextObjectContext, VisualKind, VisualMode,
};
use lattice::plugin_host::{config, grammar, help, modes};

mod raise;
mod span;
mod split;
mod swap;

use span::{Pos, Span};

struct Component;

// ── callback ids (guest-local) ────────────────────────────────────────────────
const CB_EXPAND: u32 = 1;
const CB_SHRINK: u32 = 2;
const CB_SWAP_NEXT: u32 = 3;
const CB_SWAP_PREV: u32 = 4;
const CB_RAISE: u32 = 5;
const CB_SPLIT: u32 = 6;
const CB_JOIN: u32 = 7;
/// Operators (RF.4): their own callback space, read by `apply-operator`.
const CB_RAISE_OP: u32 = 50;
const CB_SPLIT_OP: u32 = 51;
const CB_JOIN_OP: u32 = 52;
/// The one parse callback every argument-less ex-command shares.
const CB_PARSE_NO_ARGS: u32 = 100;

// ── shrink history ────────────────────────────────────────────────────────────

/// What an expand grew from: the selection before it, or the bare cursor when
/// it started in Normal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Before {
    Cursor(Pos),
    Selection(Span),
}

/// One buffer's run of expands.
struct Run {
    /// What each expand grew from, innermost first.
    before: Vec<Before>,
    /// The selection the last expand made. A shrink retraces only while the
    /// selection is still this one.
    last: Span,
}

thread_local! {
    /// Per buffer id. A component instance is single-threaded, so the
    /// thread-local is the instance's memory, not a lock-free trick.
    static RUNS: RefCell<HashMap<u32, Run>> = RefCell::new(HashMap::new());
}

// ── effects ───────────────────────────────────────────────────────────────────

fn echo(text: &str) -> Vec<Effect> {
    vec![Effect::Echo(EchoPayload {
        level: EchoLevel::Info,
        text: text.to_string(),
    })]
}

/// Select `span` charwise. `head` is the span's last character, because a
/// Visual selection is inclusive of the character under its head.
fn select(doc: &Document, span: Span, in_visual: bool) -> Vec<Effect> {
    let mut effects = Vec::with_capacity(2);
    if !in_visual {
        effects.push(Effect::EnterMode(ModalState::Visual(VisualKind::Charwise)));
    }
    effects.push(Effect::SelectionChange(SelectionSet {
        selections: vec![Selection {
            anchor: span.start.into(),
            head: span::last_char(doc, span).into(),
            visual: Some(VisualMode::Charwise),
        }],
        primary: 0,
    }));
    effects
}

/// The selection the action fired with, as an exclusive span — `None` in
/// Normal. The host hands an inclusive range (its `end` is the head's
/// character), so the end steps past that character.
fn current_selection(doc: &Document, ctx: &ActionContext) -> Option<Span> {
    let range = ctx.selection?;
    let start = Pos::from(range.start);
    let head = Pos::from(range.end);
    Some(Span {
        start,
        end: span::after_char(doc, head),
    })
}

/// The smallest named node covering `within` — strictly larger than it when
/// `strict`. Walks up from the node at `within.start`, so the cost is the
/// tree's depth there, never the file's size.
fn enclosing_node(tree: &TreeSnapshot, within: Span, strict: bool) -> Option<Span> {
    let mut node = tree.node_at(within.start.into());
    while let Some(n) = node {
        let r = Span::from(n.byte_range());
        if n.is_named() && r.covers(within) && !(strict && r == within) {
            return Some(r);
        }
        node = n.parent();
    }
    None
}

// ── the actions ───────────────────────────────────────────────────────────────

fn expand(ctx: &ActionContext, doc: &Document, tree: Option<&TreeSnapshot>) -> Vec<Effect> {
    let Some(tree) = tree else {
        return echo("refactor: no syntax tree for this buffer");
    };
    let selection = current_selection(doc, ctx);
    let cursor = Pos::from(ctx.cursor);
    let buffer = ctx.buffer_id;

    // A count grows that many levels — `3<M-o>` is three presses, and each
    // is a step `<M-i>` can come back through.
    let steps = ctx.count.max(1);
    let mut from = selection;
    let mut grown: Vec<(Before, Span)> = Vec::new();
    for _ in 0..steps {
        let next = match from {
            Some(s) => enclosing_node(tree, s, true),
            None => enclosing_node(
                tree,
                Span {
                    start: cursor,
                    end: span::after_char(doc, cursor),
                },
                false,
            ),
        };
        let Some(next) = next else { break };
        let before = match from {
            Some(s) => Before::Selection(s),
            None => Before::Cursor(cursor),
        };
        grown.push((before, next));
        from = Some(next);
    }
    let Some(&(_, last)) = grown.last() else {
        return echo(if selection.is_some() {
            "refactor: already the whole tree"
        } else {
            "refactor: no syntax node here"
        });
    };

    RUNS.with(|runs| {
        let mut runs = runs.borrow_mut();
        // Continue the run only when the selection is the one the last expand
        // made; anything else means the user moved, and the history is theirs
        // no longer.
        let continuing = matches!(
            (runs.get(&buffer), selection),
            (Some(run), Some(sel)) if run.last == sel
        );
        if !continuing {
            runs.insert(
                buffer,
                Run {
                    before: Vec::new(),
                    last,
                },
            );
        }
        if let Some(run) = runs.get_mut(&buffer) {
            run.before.extend(grown.iter().map(|(b, _)| *b));
            run.last = last;
        }
    });

    select(doc, last, selection.is_some())
}

fn shrink(ctx: &ActionContext, doc: &Document) -> Vec<Effect> {
    let Some(selection) = current_selection(doc, ctx) else {
        return echo("refactor: nothing to shrink — expand a selection first");
    };
    let buffer = ctx.buffer_id;
    let steps = ctx.count.max(1);
    let back = RUNS.with(|runs| {
        let mut runs = runs.borrow_mut();
        let run = runs.get_mut(&buffer).filter(|run| run.last == selection)?;
        let mut back = None;
        for _ in 0..steps {
            match run.before.pop() {
                Some(b) => back = Some(b),
                None => break,
            }
        }
        match back {
            Some(Before::Selection(s)) => run.last = s,
            Some(Before::Cursor(_)) | None => {
                runs.remove(&buffer);
            }
        }
        back
    });
    match back {
        Some(Before::Selection(s)) => select(doc, s, true),
        Some(Before::Cursor(p)) => vec![
            Effect::EnterMode(ModalState::Normal),
            Effect::CursorMove(p.into()),
        ],
        None => {
            echo("refactor: nothing to shrink back to — this selection was not grown by expand")
        }
    }
}

/// The effects of one structural edit at the cursor: the replacement with
/// the cursor where the command put it, or an echo saying why not.
fn edit_effects(buffer_id: u32, outcome: span::Outcome) -> Vec<Effect> {
    match outcome {
        span::Outcome::Edit {
            range,
            text,
            cursor,
        } => vec![Effect::ApplyEdit(ApplyEditPayload {
            target: buffer_id,
            edit: Edit {
                range: range.into(),
                kind: EditKind::Replace(text),
            },
            cursor: Some(cursor.into()),
        })],
        span::Outcome::Refuse(why) => echo(&format!("refactor: {why}")),
    }
}

// @example config.get-option-in-buffer: Read an option as one buffer sees it — a mode's `noexpandtab` included
/// One indent level as `buffer` is configured — buffer-local, so a mode's
/// `noexpandtab` (Go) is honoured.
fn buffer_indent_unit(buffer: u32) -> String {
    let read = |name: &str| config::get_option_in_buffer(u64::from(buffer), name);
    split::indent_unit(read("shiftwidth").as_deref(), read("expandtab").as_deref())
}
// @end-example

/// The empty range at an ex-command's cursor — what the `:` forms of the
/// operators act on.
fn at_cursor(ctx: &ExCommandContext) -> Span {
    let p = Pos::from(ctx.cursor);
    Span { start: p, end: p }
}

/// Run `edit` against the tree, or say there is none.
fn with_tree(
    ctx: &ExCommandContext,
    tree: Option<&TreeSnapshot>,
    edit: impl FnOnce(&TreeSnapshot) -> span::Outcome,
) -> Vec<Effect> {
    match tree {
        Some(tree) => edit_effects(ctx.buffer_id, edit(tree)),
        None => echo("refactor: no syntax tree for this buffer"),
    }
}

// ── registration ──────────────────────────────────────────────────────────────

impl Guest for Component {
    fn register_grammar() {
        // RF.4: every refactor is an operator over a range — Visual, a motion
        // or a text object (`aa`, `af`, …) — on `refactor-mode`'s layer. The
        // `:` commands below are the cursor shorthand.
        let operator = |chord: &str| OperatorSpec {
            repeatable: true,
            args_schema: Vec::new(),
            // One range, never per row: a refactor acts on the node a range
            // covers, and a `<C-v>` block split into rows covers none.
            blockwise_per_row: false,
            post_motion_char: false,
            chord: Some(chord.to_string()),
            // No doubled form: raising "the current line" names no node.
            doubled: None,
        };
        grammar::register_operator(
            "refactor-raise-operator",
            "Raise the syntax node the range covers: it replaces the expression around it (`<leader>rria` lifts an argument out of its call).",
            &operator("<leader>rr"),
            CB_RAISE_OP,
        );
        grammar::register_operator(
            "refactor-split-operator",
            "Split the comma-separated list around the range one item per line, one indent deeper (`<leader>rsi(` splits a call's arguments).",
            &operator("<leader>rs"),
            CB_SPLIT_OP,
        );
        grammar::register_operator(
            "refactor-join-operator",
            "Join the comma-separated list around the range onto one line; the inverse of `<leader>rs`.",
            &operator("<leader>rj"),
            CB_JOIN_OP,
        );
        let spec = || ActionSpec {
            args_schema: Vec::new(),
        };
        grammar::register_action(
            "refactor-expand",
            "Grow the selection to the next syntax node around it (from Normal: the node at the cursor). A count grows that many levels.",
            &spec(),
            CB_EXPAND,
        );
        grammar::register_action(
            "refactor-shrink",
            "Step back to the selection before the last refactor-expand; past the first, back to the cursor.",
            &spec(),
            CB_SHRINK,
        );
        // Swap acts on the item at the cursor, so it needs no selection and
        // can be a `:` command — unlike expand / shrink.
        let no_args = || ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            args_schema: Vec::new(),
            surface_form: SurfaceForm::Keyword,
        };
        grammar::register_ex_command(
            "refactor-swap-next",
            "Swap the list item at the cursor (an argument, element, field or statement) with the next one; the separators stay put. A count moves it that many places.",
            &no_args(),
            CB_PARSE_NO_ARGS,
            CB_SWAP_NEXT,
        );
        grammar::register_ex_command(
            "refactor-swap-prev",
            "Swap the list item at the cursor with the previous one; the separators stay put. A count moves it that many places.",
            &no_args(),
            CB_PARSE_NO_ARGS,
            CB_SWAP_PREV,
        );
        grammar::register_ex_command(
            "refactor-raise",
            "Replace the expression around the node at the cursor with the node: on `bar` in `foo(bar)`, the call becomes `bar`.",
            &no_args(),
            CB_PARSE_NO_ARGS,
            CB_RAISE,
        );
        grammar::register_ex_command(
            "refactor-split",
            "Put each item of the comma-separated list at the cursor (arguments, parameters, an array, a struct literal) on its own line, one indent deeper; a trailing comma is kept or left out as it was.",
            &no_args(),
            CB_PARSE_NO_ARGS,
            CB_SPLIT,
        );
        grammar::register_ex_command(
            "refactor-join",
            "Put the items of the comma-separated list at the cursor on one line, `, ` between them; the inverse of `:refactor-split`.",
            &no_args(),
            CB_PARSE_NO_ARGS,
            CB_JOIN,
        );
    }

    /// `refactor-mode` owns the chords, at its own minor-mode layer, never
    /// the builtin one. `global`: every document buffer. A buffer with no
    /// grammar still gets the mode, and the actions say there is no tree
    /// rather than the keys going dead — there is no activation policy for
    /// "has a parse tree", and a key that silently does nothing in one file
    /// and works in the next reads as broken.
    fn register_modes() {
        let bind = |binding_mode: BindingMode, chord: &str, command: &str| ModeKeymapBinding {
            binding_mode,
            chord: chord.to_string(),
            command: command.to_string(),
        };
        modes::register_mode(&ModeDeclaration {
            id: "refactor-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Global,
            // Not `TREE_SITTER`: requiring it keeps the mode off buffers
            // with no parse, which is exactly where the actions must still
            // answer (with "no syntax tree") rather than the keys falling
            // through to something else. The tree reaches the actions
            // through the manifest's `tree-sitter` editor capability.
            capabilities: ModeCapabilities::empty(),
            keymap: vec![
                bind(BindingMode::Normal, "<M-o>", "refactor-expand"),
                bind(BindingMode::Visual, "<M-o>", "refactor-expand"),
                bind(BindingMode::Visual, "<M-i>", "refactor-shrink"),
                // In Normal there is nothing to shrink, and `refactor-shrink`
                // says so. Bound anyway: Normal's lookup strips an unbound
                // Alt, so a bare `<M-i>` would run `i` and enter Insert.
                bind(BindingMode::Normal, "<M-i>", "refactor-shrink"),
            ],
            target_language: None,
            options: vec![],
        });
    }

    fn register_help_topics() {
        let _ = help::register_topic(
            "",
            "Select, swap and reshape code by syntax node, in every language with a grammar.",
            include_str!("../doc/refactor.md"),
            &["refactor".to_string()],
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
        Ok(match callback {
            CB_EXPAND => expand(&ctx, doc, tree),
            CB_SHRINK => shrink(&ctx, doc),
            other => return Err(format!("refactor: unknown action callback {other}")),
        })
    }

    fn apply_motion(
        _c: u32,
        _ctx: MotionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<MotionResult, String> {
        Err("refactor: no motions".into())
    }
    fn apply_operator(
        callback: u32,
        ctx: OperatorContext,
        doc: &Document,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        let Some(tree) = tree else {
            return Ok(echo("refactor: no syntax tree for this buffer"));
        };
        let range = Span::from(ctx.range);
        Ok(match callback {
            CB_RAISE_OP => edit_effects(ctx.buffer_id, raise::raise_range(tree, doc, range)),
            CB_SPLIT_OP => {
                let unit = buffer_indent_unit(ctx.buffer_id);
                let outcome = split::reshape(tree, doc, range, split::Layout::Split, &unit);
                edit_effects(ctx.buffer_id, outcome)
            }
            CB_JOIN_OP => edit_effects(
                ctx.buffer_id,
                split::reshape(tree, doc, range, split::Layout::Join, ""),
            ),
            other => return Err(format!("refactor: unknown operator callback {other}")),
        })
    }
    fn apply_text_object(
        _c: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        Err("refactor: no text objects".into())
    }
    fn parse_ex_args(callback: u32, rest: String, _bang: bool) -> Result<Args, String> {
        match callback {
            CB_PARSE_NO_ARGS if rest.trim().is_empty() => Ok(Args::None),
            CB_PARSE_NO_ARGS => Err("takes no arguments".into()),
            other => Err(format!("refactor: unknown parse callback {other}")),
        }
    }
    fn apply_ex_command(
        callback: u32,
        ctx: ExCommandContext,
        doc: &Document,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Ok(match callback {
            CB_SWAP_NEXT => with_tree(&ctx, tree, |t| {
                swap::swap(t, doc, ctx.cursor.into(), swap::Dir::Next, ctx.count)
            }),
            CB_SWAP_PREV => with_tree(&ctx, tree, |t| {
                swap::swap(t, doc, ctx.cursor.into(), swap::Dir::Prev, ctx.count)
            }),
            CB_RAISE => with_tree(&ctx, tree, |t| raise::raise(t, doc, ctx.cursor.into())),
            CB_SPLIT => {
                let unit = buffer_indent_unit(ctx.buffer_id);
                with_tree(&ctx, tree, |t| {
                    split::reshape(t, doc, at_cursor(&ctx), split::Layout::Split, &unit)
                })
            }
            CB_JOIN => with_tree(&ctx, tree, |t| {
                split::reshape(t, doc, at_cursor(&ctx), split::Layout::Join, "")
            }),
            other => return Err(format!("refactor: unknown ex-command callback {other}")),
        })
    }
}

export!(Component);

// `Position` / `Range` are the WIT records; `span` works in its own ordered
// `Pos` / `Span` and converts at the boundary.
impl From<Position> for Pos {
    fn from(p: Position) -> Self {
        Pos {
            line: p.line,
            byte: p.byte,
        }
    }
}
impl From<Pos> for Position {
    fn from(p: Pos) -> Self {
        Position {
            line: p.line,
            byte: p.byte,
        }
    }
}
impl From<Span> for Range {
    fn from(s: Span) -> Self {
        Range {
            start: s.start.into(),
            end: s.end.into(),
        }
    }
}
impl From<Range> for Span {
    fn from(r: Range) -> Self {
        Span {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}
