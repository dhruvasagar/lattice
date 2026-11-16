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
provides = ["grammar", "modes", "config", "help"]
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
use lattice::plugin_host::config::OptionType;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, ApplyEditPayload, Args, CodeActionKind, CodeActionRequest,
    CommandRef, EchoLevel, EchoPayload, Edit, EditKind, Effect, ExCommandContext, ExCommandSpec,
    InsertSnippetPayload, LatencyClass, ModalState, MotionContext, MotionResult, OperatorContext,
    OperatorSpec, Position, Range, Selection, SelectionSet, SurfaceForm, TextObjectContext,
    TrackRangePayload, UntrackRangePayload, VisualKind, VisualMode,
};
use lattice::plugin_host::{config, grammar, help, modes};

mod exchange;
mod extract;
mod inline;
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
const CB_EXCHANGE_CLEAR: u32 = 8;
/// RF.3c: the server's refactorings, and extract and inline, as `:` commands.
const CB_LSP_REFACTOR: u32 = 9;
const CB_EXTRACT_EX: u32 = 10;
const CB_INLINE_EX: u32 = 11;
/// Operators (RF.4): their own callback space, read by `apply-operator`.
const CB_RAISE_OP: u32 = 50;
const CB_SPLIT_OP: u32 = 51;
const CB_JOIN_OP: u32 = 52;
const CB_EXCHANGE_OP: u32 = 53;
const CB_EXTRACT_OP: u32 = 54;
const CB_INLINE_OP: u32 = 55;
const CB_EXTRACT_NATIVE: u32 = 56;
const CB_INLINE_NATIVE: u32 = 57;

/// RF.4g: the fallback commands an extract / inline request names, run by
/// the host when the server has nothing to offer.
const EXTRACT_NATIVE: &str = "refactor-extract-native";
const INLINE_NATIVE: &str = "refactor-inline-native";
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

/// RF.4d: the exchange mark — the first `cx`'s range, until the second.
/// One per instance, as vim-exchange keeps one: a second buffer's `cx`
/// moves it there.
struct Mark {
    buffer: u32,
    /// Where it was marked, and the text it named. The text is how an edit
    /// to the marked text is noticed; the span is read only if the
    /// document has not yet taken the `track-range` (its mailbox is FIFO,
    /// but the next keystroke can be dispatched against an older snapshot).
    span: Span,
    text: String,
}

/// The `track-range` id of the exchange mark, in this plugin's own space.
const EXCHANGE_MARK: u32 = 1;

/// The tint the mark is shown in: the current search hit's, vim-exchange's
/// `IncSearch` link. A plugin-owned element waits on grammar-world theme
/// registration (refactor.md §9).
const EXCHANGE_TINT: &str = "search.current";

thread_local! {
    static MARK: RefCell<Option<Mark>> = const { RefCell::new(None) };
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

fn warn(text: &str) -> Vec<Effect> {
    vec![Effect::Echo(EchoPayload {
        level: EchoLevel::Warn,
        text: text.to_string(),
    })]
}

fn untrack_mark(buffer: u32) -> Effect {
    Effect::UntrackRange(UntrackRangePayload {
        target: buffer,
        id: EXCHANGE_MARK,
    })
}

/// The range an operator acts on, as text to exchange. A linewise range is
/// its lines without the last newline, so two lines swap their text and keep
/// the line break between them.
fn operated_span(ctx: &OperatorContext, doc: &Document) -> Span {
    let range = Span::from(ctx.range);
    if !ctx.linewise {
        return range;
    }
    // A linewise range may end at the start of the line after its last.
    let last = if range.end.byte == 0 && range.end.line > range.start.line {
        range.end.line - 1
    } else {
        range.end.line
    };
    let len = doc.line(last).map_or(0, |l| l.len() as u32);
    Span {
        start: Pos {
            line: range.start.line,
            byte: 0,
        },
        end: Pos {
            line: last,
            byte: len,
        },
    }
}

/// `cx{motion}`: mark the range, or, with a range marked, swap the two.
fn exchange_op(ctx: &OperatorContext, doc: &Document) -> Vec<Effect> {
    let buffer = ctx.buffer_id;
    let new = operated_span(ctx, doc);
    if new.start >= new.end {
        return echo("exchange: nothing to exchange in an empty range");
    }
    let marked = MARK.with(|m| m.borrow_mut().take());
    let mut effects = Vec::new();
    let mark = match marked {
        Some(mark) if mark.buffer == buffer => mark,
        other => {
            // Nothing marked, or marked in another buffer: this range is the
            // first of a new pair.
            if let Some(old) = other {
                effects.push(untrack_mark(old.buffer));
            }
            effects.push(Effect::TrackRange(TrackRangePayload {
                target: buffer,
                id: EXCHANGE_MARK,
                range: new.into(),
                element: Some(EXCHANGE_TINT.to_string()),
            }));
            let text = span::text(doc, new);
            MARK.with(|m| {
                *m.borrow_mut() = Some(Mark {
                    buffer,
                    span: new,
                    text,
                })
            });
            return effects;
        }
    };
    // @example buffer.document.tracked-range: Find where a range tracked earlier is now, after the edits since
    let current = match doc.tracked_range(EXCHANGE_MARK) {
        // An edit replaced more than the marked text: it is gone.
        Some(t) if t.collapsed => None,
        Some(t) => Some(Span::from(t.range)),
        // Not yet in this snapshot: where it was marked, if the text there is
        // still what was marked.
        None => Some(mark.span),
    };
    // @end-example
    let Some(marked) = current.filter(|s| span::text(doc, *s) == mark.text) else {
        effects.push(untrack_mark(buffer));
        effects.extend(warn(
            "exchange: the marked text was edited, so the mark was dropped — `cx` marks again",
        ));
        return effects;
    };
    let outcome = match exchange::plan(marked, new) {
        // `cx` twice on the same text: vim-exchange's cancel.
        exchange::Plan::Same => {
            effects.push(untrack_mark(buffer));
            return effects;
        }
        exchange::Plan::Overlap => {
            // Kept: the user meant to exchange the mark, just not with this.
            MARK.with(|m| *m.borrow_mut() = Some(mark));
            return echo(
                "exchange: the two ranges overlap — exchange the mark with text beside it, or `cxc` to clear it",
            );
        }
        exchange::Plan::Contains { outer, inner } => span::Outcome::Edit {
            range: outer,
            text: span::text(doc, inner),
            cursor: outer.start,
        },
        exchange::Plan::Disjoint { first, second } => {
            let gap = Span {
                start: first.end,
                end: second.start,
            };
            span::Outcome::Edit {
                range: Span {
                    start: first.start,
                    end: second.end,
                },
                text: exchange::swapped(
                    &span::text(doc, first),
                    &span::text(doc, gap),
                    &span::text(doc, second),
                ),
                cursor: first.start,
            }
        }
    };
    effects.push(untrack_mark(buffer));
    effects.extend(edit_effects(buffer, outcome));
    effects
}

/// Where extract and inline look, in order: `refactor.source`, a comma list
/// of `lsp` and `native` (refactor.md §9.1). Unknown words are skipped; an
/// empty or all-unknown list means the default, `lsp,native`.
#[derive(Debug, PartialEq, Eq)]
struct Sources {
    lsp: bool,
    native: bool,
    lsp_first: bool,
}

fn sources(value: Option<&str>) -> Sources {
    let words: Vec<&str> = value
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|w| matches!(*w, "lsp" | "native"))
        .collect();
    if words.is_empty() {
        return Sources {
            lsp: true,
            native: true,
            lsp_first: true,
        };
    }
    Sources {
        lsp: words.contains(&"lsp"),
        native: words.contains(&"native"),
        lsp_first: words[0] == "lsp",
    }
}

/// RF.4g: an extract or inline over `range`, server first when
/// `refactor.source` says so. The server's answer decides: actions open as
/// a picker; nothing runs the native command `native` names, over the same
/// range, on the text as it was (the host checks). The operators and the
/// `:` commands (RF.3c) both come through here, so the two cannot differ.
fn server_or_native(
    buffer_id: u32,
    range: Span,
    kind: CodeActionKind,
    native: &str,
    run_native: impl FnOnce(Span) -> Vec<Effect>,
) -> Vec<Effect> {
    let source =
        sources(config::get_option_in_buffer(u64::from(buffer_id), "refactor.source").as_deref());
    if source.native && !(source.lsp && source.lsp_first) {
        return run_native(range);
    }
    vec![Effect::LspCodeAction(CodeActionRequest {
        only: Some(kind),
        range: Some(range.into()),
        otherwise: source.native.then(|| CommandRef {
            id: native.to_string(),
            args: Args::Bytes(range.to_bytes()),
        }),
    })]
}

/// RF.3c: what a `:` command acts on: the range its `:` line named
/// (resolved by the host), else the cursor, as an empty span. Native inline
/// finds the name the cursor is on; native extract needs an expression and
/// says so.
fn ex_range(ctx: &ExCommandContext) -> Span {
    ctx.range.map(Span::from).unwrap_or_else(|| at_cursor(ctx))
}

/// The range a fallback command was handed, or why it has none.
fn fallback_range(ctx: &ActionContext) -> Result<Span, Vec<Effect>> {
    match &ctx.args {
        Args::Bytes(bytes) => Span::from_bytes(bytes).ok_or_else(|| echo("refactor: bad range")),
        _ => Err(echo(
            "refactor: this runs when a language server has nothing to offer; use `cre` / `cri`",
        )),
    }
}

/// `cri{motion}`'s native half (RF.4f): every use of the variable the range
/// names becomes its value, and the declaration goes, in one replacement.
fn inline_native(
    buffer_id: u32,
    doc: &Document,
    tree: Option<&TreeSnapshot>,
    range: Span,
) -> Vec<Effect> {
    let Some(tree) = tree else {
        return echo("refactor: no syntax tree for this buffer");
    };
    match inline::inline(tree, doc, range) {
        Ok((range, text, cursor)) => edit_effects(
            buffer_id,
            span::Outcome::Edit {
                range,
                text,
                cursor,
            },
        ),
        Err(why) => echo(&format!("refactor: not inlined: {why}")),
    }
}

/// `cre{motion}`: extract the expression the range covers into a variable
/// declared before its statement, the name typed once at both sites.
fn extract_op(ctx: &OperatorContext, doc: &Document, tree: Option<&TreeSnapshot>) -> Vec<Effect> {
    server_or_native(
        ctx.buffer_id,
        Span::from(ctx.range),
        CodeActionKind::Extract,
        EXTRACT_NATIVE,
        |range| extract_native(ctx.buffer_id, doc, tree, range),
    )
}

/// The native extract over `range`, from the operator or from the host's
/// fallback.
fn extract_native(
    buffer_id: u32,
    doc: &Document,
    tree: Option<&TreeSnapshot>,
    range: Span,
) -> Vec<Effect> {
    let Some(tree) = tree else {
        return echo("refactor: no syntax tree for this buffer");
    };
    // Read through the buffer, so a mode (or `:setlocal`) can supply the
    // declaration form of one language (RF.4e′). Empty: the table's.
    let template = config::get_option_in_buffer(u64::from(buffer_id), "refactor.declaration")
        .filter(|t| !t.trim().is_empty());
    match extract::extract(tree, doc, range, template.as_deref()) {
        Ok((range, body)) => vec![Effect::InsertSnippet(InsertSnippetPayload {
            range: range.into(),
            body,
        })],
        Err(why) => echo(&format!("refactor: {why}")),
    }
}

/// `cxc`: forget the mark.
fn exchange_clear() -> Vec<Effect> {
    match MARK.with(|m| m.borrow_mut().take()) {
        Some(mark) => vec![untrack_mark(mark.buffer)],
        None => echo("exchange: nothing is marked"),
    }
}

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
        //
        // RF.4j: the keys are the `c` family. `cx` is vim-exchange's; the
        // rest take `cr`, free in vim because `r` is not a motion. The chord
        // binds Normal's operator-pending path only; each Visual form is a
        // `refactor-mode` binding below, on `<leader>r`, because Visual `c`
        // changes the selection at once. vim-abolish's coercion letters
        // (`s` `c` `m` `u` `-` `.` `t`) are left free, so split is
        // `cr<CR>`, not `crs`.
        grammar::register_operator(
            "refactor-exchange-operator",
            "Exchange two ranges: the first `cx{motion}` marks one (tinted), the second swaps them. `cxx` is the line; `cxc` clears the mark.",
            &OperatorSpec {
                repeatable: true,
                args_schema: Vec::new(),
                blockwise_per_row: false,
                post_motion_char: false,
                chord: Some("cx".to_string()),
                // vim-exchange's `cxx`: the current line.
                doubled: Some("x".to_string()),
            },
            CB_EXCHANGE_OP,
        );
        grammar::register_action(
            "refactor-exchange-clear",
            "Forget the range the first `cx` marked, and its tint.",
            &ActionSpec {
                args_schema: Vec::new(),
            },
            CB_EXCHANGE_CLEAR,
        );
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
            "Raise the syntax node the range covers: it replaces the expression around it (`crria` lifts an argument out of its call; Visual `<leader>rr`).",
            &operator("crr"),
            CB_RAISE_OP,
        );
        grammar::register_operator(
            "refactor-extract-operator",
            "Extract the expression the range covers into a variable declared before its statement; the name is typed once, at both sites (`cre{motion}`; Visual `<leader>re`). Variables only: extracting a function needs a language server.",
            &operator("cre"),
            CB_EXTRACT_OP,
        );
        grammar::register_operator(
            "refactor-inline-operator",
            "Inline the variable the range names: its value replaces each use and the declaration goes (`cri{motion}`; Visual `<leader>ri`). Asks the language server first; without one, natively.",
            &operator("cri"),
            CB_INLINE_OP,
        );
        // RF.4g: what the host runs when the server has nothing to offer.
        // Not for a key: each takes the range its operator asked about.
        grammar::register_action(
            EXTRACT_NATIVE,
            "Extract, without a language server, over the range a `cre` request carried. Run by the host when the server offers nothing.",
            &ActionSpec {
                args_schema: Vec::new(),
            },
            CB_EXTRACT_NATIVE,
        );
        grammar::register_action(
            INLINE_NATIVE,
            "Inline, without a language server, over the range a `cri` request carried. Run by the host when the server offers nothing.",
            &ActionSpec {
                args_schema: Vec::new(),
            },
            CB_INLINE_NATIVE,
        );
        grammar::register_operator(
            "refactor-split-operator",
            "Split the comma-separated list around the range one item per line, one indent deeper (`cr<CR>i(` splits a call's arguments; Visual `<leader>r<CR>`).",
            &operator("cr<CR>"),
            CB_SPLIT_OP,
        );
        grammar::register_operator(
            "refactor-join-operator",
            "Join the comma-separated list around the range onto one line; the inverse of `cr<CR>` (`crj{motion}`; Visual `<leader>rj`).",
            &operator("crj"),
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
        // RF.3c: the `:` forms of the server's refactorings and of extract
        // and inline. They take a range — `:'<,'>` is exactly what was
        // selected, any other range its whole lines — and without one, the
        // cursor. Extract and inline take the operators' path: server first,
        // natively when it has nothing (`refactor.source`).
        let ranged = || ExCommandSpec {
            accepts_range: true,
            ..no_args()
        };
        grammar::register_ex_command(
            "lsp-refactor",
            "Open a picker over the language server's refactorings at the cursor, or over the range (`:'<,'>lsp-refactor`): extract, inline, rewrite, whatever it offers. Needs a server.",
            &ranged(),
            CB_PARSE_NO_ARGS,
            CB_LSP_REFACTOR,
        );
        grammar::register_ex_command(
            "refactor-extract",
            "Extract the selection (`:'<,'>refactor-extract`) into a variable, or what the language server offers (a function, a constant); without a server, natively, as `cre` does.",
            &ranged(),
            CB_PARSE_NO_ARGS,
            CB_EXTRACT_EX,
        );
        grammar::register_ex_command(
            "refactor-inline",
            "Inline the variable under the cursor (or the range): its value replaces each use and the declaration goes. Asks the language server first; without one, natively, as `cri` does.",
            &ranged(),
            CB_PARSE_NO_ARGS,
            CB_INLINE_EX,
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
                // vim-exchange's clear. `c` is not a motion, so this does not
                // shadow any `cx{motion}`.
                bind(BindingMode::Normal, "cxc", "refactor-exchange-clear"),
                // vim-exchange's Visual form. Not `cx`: in Visual `c` is
                // change and fires at once.
                bind(BindingMode::Visual, "X", "refactor-exchange-operator"),
                // RF.4j: the Visual forms of the `cr` operators, for the same
                // reason. The same letter after the prefix.
                bind(BindingMode::Visual, "<leader>rr", "refactor-raise-operator"),
                bind(
                    BindingMode::Visual,
                    "<leader>re",
                    "refactor-extract-operator",
                ),
                bind(
                    BindingMode::Visual,
                    "<leader>ri",
                    "refactor-inline-operator",
                ),
                bind(BindingMode::Visual, "<leader>rj", "refactor-join-operator"),
                bind(
                    BindingMode::Visual,
                    "<leader>r<CR>",
                    "refactor-split-operator",
                ),
            ],
            target_language: None,
            options: vec![],
        });
    }

    /// `refactor.declaration`: how extract declares a variable, for a buffer
    /// whose language the plugin's table does not cover (or covers in a way
    /// a project does not want). Short name; the host namespaces it.
    fn register_options() {
        config::register_option(
            "source",
            OptionType::String,
            "lsp,native",
            "where `cre` / `cri` look, in order: `lsp` (the language server's refactorings), `native` (the plugin's own), or both, comma-separated. With `lsp` first, native runs only when the server has nothing to offer.",
        );
        config::register_option(
            "declaration",
            OptionType::String,
            "",
            "how `cre` declares the extracted variable: a template over `{name}` and `{value}` (e.g. `const {name} = {value};`). Empty: the language's own form.",
        );
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
            CB_EXCHANGE_CLEAR => exchange_clear(),
            CB_EXTRACT_NATIVE => match fallback_range(&ctx) {
                Ok(range) => extract_native(ctx.buffer_id, doc, tree, range),
                Err(effects) => effects,
            },
            CB_INLINE_NATIVE => match fallback_range(&ctx) {
                Ok(range) => inline_native(ctx.buffer_id, doc, tree, range),
                Err(effects) => effects,
            },
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
        // Exchange is text only: it works in a buffer with no parse.
        if callback == CB_EXCHANGE_OP {
            return Ok(exchange_op(&ctx, doc));
        }
        // RF.4g: extract and inline ask the server first, which needs no
        // parse; their native half checks for the tree itself.
        if callback == CB_EXTRACT_OP {
            return Ok(extract_op(&ctx, doc, tree));
        }
        if callback == CB_INLINE_OP {
            return Ok(server_or_native(
                ctx.buffer_id,
                Span::from(ctx.range),
                CodeActionKind::Inline,
                INLINE_NATIVE,
                |range| inline_native(ctx.buffer_id, doc, tree, range),
            ));
        }
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
            CB_LSP_REFACTOR => vec![Effect::LspCodeAction(CodeActionRequest {
                only: Some(CodeActionKind::Refactor),
                range: ctx.range,
                otherwise: None,
            })],
            CB_EXTRACT_EX => server_or_native(
                ctx.buffer_id,
                ex_range(&ctx),
                CodeActionKind::Extract,
                EXTRACT_NATIVE,
                |range| extract_native(ctx.buffer_id, doc, tree, range),
            ),
            CB_INLINE_EX => server_or_native(
                ctx.buffer_id,
                ex_range(&ctx),
                CodeActionKind::Inline,
                INLINE_NATIVE,
                |range| inline_native(ctx.buffer_id, doc, tree, range),
            ),
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

#[cfg(test)]
mod tests {
    use super::{sources, Sources};
    use crate::span::{Pos, Span};

    #[test]
    fn refactor_source_orders_the_rungs() {
        let both = Sources {
            lsp: true,
            native: true,
            lsp_first: true,
        };
        assert_eq!(sources(None), both);
        assert_eq!(sources(Some("")), both, "empty is the default");
        assert_eq!(sources(Some("lsp, native")), both);
        assert_eq!(
            sources(Some("native,lsp")),
            Sources {
                lsp: true,
                native: true,
                lsp_first: false
            }
        );
        assert_eq!(
            sources(Some("lsp")),
            Sources {
                lsp: true,
                native: false,
                lsp_first: true
            }
        );
        assert_eq!(
            sources(Some("native")),
            Sources {
                lsp: false,
                native: true,
                lsp_first: false
            }
        );
        assert_eq!(sources(Some("bogus")), both, "unknown words are skipped");
    }

    #[test]
    fn a_span_survives_its_bytes() {
        let span = Span {
            start: Pos { line: 3, byte: 7 },
            end: Pos {
                line: 70_000,
                byte: 12,
            },
        };
        assert_eq!(Span::from_bytes(&span.to_bytes()), Some(span));
        assert_eq!(Span::from_bytes(&[1, 2, 3]), None);
    }
}
