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
    ActionContext, ActionSpec, Args, EchoLevel, EchoPayload, Effect, ExCommandContext, ModalState,
    MotionContext, MotionResult, OperatorContext, Position, Range, Selection, SelectionSet,
    TextObjectContext, VisualKind, VisualMode,
};
use lattice::plugin_host::{grammar, help, modes};

mod span;

use span::{Pos, Span};

struct Component;

// ── callback ids (guest-local) ────────────────────────────────────────────────
const CB_EXPAND: u32 = 1;
const CB_SHRINK: u32 = 2;

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

// ── registration ──────────────────────────────────────────────────────────────

impl Guest for Component {
    fn register_grammar() {
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
        _c: u32,
        _ctx: OperatorContext,
        _doc: &Document,
    ) -> Result<Vec<Effect>, String> {
        Err("refactor: no operators".into())
    }
    fn apply_text_object(
        _c: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        Err("refactor: no text objects".into())
    }
    fn parse_ex_args(_c: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("refactor: no ex-commands".into())
    }
    fn apply_ex_command(
        _c: u32,
        _ctx: ExCommandContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Err("refactor: no ex-commands".into())
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
impl From<Range> for Span {
    fn from(r: Range) -> Self {
        Span {
            start: r.start.into(),
            end: r.end.into(),
        }
    }
}
