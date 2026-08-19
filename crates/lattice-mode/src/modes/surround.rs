//! surround-mode -- vim-surround semantics as a native minor mode.
//!
//! Three grammar operators: `surround-delete` (`ds{char}`),
//! `surround-change` (`cs{char1}{char2}`), `surround-add`
//! (`yss{char}` / `S{char}` in visual). All three are registered
//! in the shared `CommandRegistry` at boot.
//!
//! Design: [`docs/dev/architecture/surround-mode.md`];
//! slice plan: [`docs/dev/operations/slice-plans/surround-mode.md`].

use std::sync::Arc;

use lattice_grammar::CommandError;

use lattice_core::Document;
use lattice_core::buffer::Buffer;
use lattice_grammar::args::{ArgKind, ArgSpec, ArgValue, Args};
use lattice_grammar::effect::{Effect, YankKind};
use lattice_grammar::registry::{CommandRegistry, OperatorId, OperatorSpec};
use lattice_grammar::source::SourceLocation;
use lattice_keymap::contribution::Keymap;
use lattice_protocol::ChordPattern;
use lattice_protocol::chord::KeyChord;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

use crate::mode::ActivationPolicy;
use crate::{CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};

// ── Pair mapping ──────────────────────────────────────────────

/// Map a user-provided character to its canonical (open, close) pair.
///
/// Brackets map to themselves: `(` → `('(' , ')')`, `)` → `('(' , ')')`.
/// Symmetric pairs (`"`, `'`, `` ` ``) map to self.
/// Unknown characters return `None`.
pub fn open_close_pair(ch: char) -> Option<(char, char)> {
    Some(match ch {
        '(' | ')' => ('(', ')'),
        '[' | ']' => ('[', ']'),
        '{' | '}' => ('{', '}'),
        '<' | '>' => ('<', '>'),
        '"' => ('"', '"'),
        '\'' => ('\'', '\''),
        '`' => ('`', '`'),
        _ => return None,
    })
}

fn is_opener(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<')
}

fn is_closer(ch: char) -> bool {
    matches!(ch, ')' | ']' | '}' | '>')
}

fn is_symmetric(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '`')
}

fn matching_closer(opener: char) -> Option<char> {
    open_close_pair(opener).map(|(_, close)| close)
}

fn matching_opener(closer: char) -> Option<char> {
    open_close_pair(closer).map(|(open, _)| open)
}

// ── SU.3g: the opening form pads, the closing form does not ───

/// Does typing `ch` as a surround *wrapper* mean "pad the inside"?
///
/// vim-surround's rule, and the one piece of the grammar where the two
/// halves of a bracket pair are not interchangeable: `ysiw(` gives
/// `( hello )` and `ysiw)` gives `(hello)`. The distinction only exists
/// for asymmetric pairs — a symmetric wrapper (`"`, `'`, backtick) is its
/// own closer, so there is no opening form to mean anything different,
/// and it never pads however it is typed.
fn pads_inside(ch: char) -> bool {
    matches!(open_close_pair(ch), Some((open, close)) if open != close && ch == open)
}

/// One padding string per side. Separate function from [`pads_inside`]
/// so the two call sites that build text read as prose.
fn padding_for(ch: char) -> &'static str {
    if pads_inside(ch) { " " } else { "" }
}

/// Bytes of horizontal whitespace running forward from `from` in `line`,
/// capped at `limit`.
fn space_run_forward(line: &str, from: usize, limit: usize) -> usize {
    line.get(from..limit)
        .unwrap_or("")
        .bytes()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

/// Bytes of horizontal whitespace running backward from `until` in
/// `line`, not reaching before `floor`.
fn space_run_backward(line: &str, until: usize, floor: usize) -> usize {
    line.get(floor..until)
        .unwrap_or("")
        .bytes()
        .rev()
        .take_while(|b| *b == b' ' || *b == b'\t')
        .count()
}

/// The two spans `ds` / `cs` replace for a pair whose delimiters sit at
/// `open_pos` / `close_pos`.
///
/// With `absorb_padding` (the user typed the opening form) each span
/// grows inward over one run of horizontal whitespace, so `ds(` undoes
/// what `ysiw(` did. Without it the spans are the delimiters alone and
/// `ds)` leaves any padding in place.
///
/// The two runs are clamped against each other: in `(   )` the forward
/// run would otherwise take all three spaces and the backward run would
/// take them again, producing overlapping edits in one batch.
fn delimiter_spans(
    buffer: &Buffer,
    open_pos: Position,
    open_len: u32,
    close_pos: Position,
    close_len: u32,
    absorb_padding: bool,
) -> (
    lattice_protocol::position::Range,
    lattice_protocol::position::Range,
) {
    let open_end = open_pos.byte + open_len;
    let close_end = close_pos.byte + close_len;

    let (pad_after_open, pad_before_close) = if !absorb_padding {
        (0, 0)
    } else if open_pos.line == close_pos.line {
        let line = buffer.line(open_pos.line).unwrap_or_default().to_string();
        let inner_end = close_pos.byte as usize;
        let forward = space_run_forward(&line, open_end as usize, inner_end);
        let backward = space_run_backward(
            &line,
            inner_end,
            (open_end as usize + forward).min(inner_end),
        );
        (forward as u32, backward as u32)
    } else {
        let open_line = buffer.line(open_pos.line).unwrap_or_default().to_string();
        let close_line = buffer.line(close_pos.line).unwrap_or_default().to_string();
        let forward = space_run_forward(&open_line, open_end as usize, open_line.len());
        let backward = space_run_backward(&close_line, close_pos.byte as usize, 0);
        (forward as u32, backward as u32)
    };

    (
        lattice_protocol::position::Range::new(
            open_pos,
            Position::new(open_pos.line, open_end + pad_after_open),
        ),
        lattice_protocol::position::Range::new(
            Position::new(close_pos.line, close_pos.byte - pad_before_close),
            Position::new(close_pos.line, close_end),
        ),
    )
}

// ── Pair detection ────────────────────────────────────────────

/// Find the nearest enclosing pair matching `target` around the
/// cursor position.
///
/// Returns `Some((opener_byte, closer_byte))` (byte offsets in the
/// buffer) or `None` if no matching pair encloses the cursor.
///
/// Algorithm: scan backward from cursor tracking closers, scan
/// forward tracking openers. Stacks handle nesting correctly.
///
/// SU.3f: a delimiter sitting *under* the cursor counts as part of the
/// pair it belongs to, matching vim — `ds"` works with the caret on
/// either quote. The two scans are half-open around the cursor
/// (backward is `byte < cursor_byte`, forward is `byte >= cursor_byte`),
/// so a closer under the cursor was already found by the forward scan
/// while an opener under it was skipped by both. Nudging the effective
/// cursor one character right when it sits on an opener puts it just
/// inside its own pair, which is the position the scans already handle.
pub fn find_surround_pair(
    buffer: &Buffer,
    cursor: Position,
    target: char,
) -> Option<(usize, usize)> {
    let text = buffer.as_string();
    let cursor_byte = buffer.position_to_byte(cursor).ok()?;

    // Determine target's opener/closer identity.
    let (target_open, target_close) = open_close_pair(target)?;

    let cursor_byte = match text.get(cursor_byte..).and_then(|s| s.chars().next()) {
        // Symmetric target (`"`, `'`, backtick): the character is its own
        // closer, so "am I on an opener?" cannot be read off the character
        // and is decided by how many precede it on the line — an even count
        // means this one opens. Skipping this and always nudging would
        // resolve `"a" "b"` with the caret on the second pair's opening
        // quote to the *gap* between the pairs (quotes 2 and 4), which is a
        // real enclosing pair and the wrong one.
        Some(ch) if ch == target_open && target_open == target_close => {
            let line = buffer.line(cursor.line).unwrap_or_default().to_string();
            let preceding = line
                .get(..cursor.byte as usize)
                .unwrap_or("")
                .chars()
                .filter(|c| *c == target_open)
                .count();
            if preceding % 2 == 0 {
                cursor_byte + ch.len_utf8()
            } else {
                cursor_byte
            }
        }
        // Asymmetric: the character says which end it is. On the closer,
        // leave the cursor alone — the forward scan starts there and finds
        // it.
        Some(ch) if ch == target_open => cursor_byte + ch.len_utf8(),
        _ => cursor_byte,
    };

    // ---- Backward scan: find the unmatched opener ----
    let mut closer_stack: Vec<char> = Vec::new();
    let mut opener_byte: Option<usize> = None;
    let char_indices: Vec<(usize, char)> = text.char_indices().collect();

    for (byte, ch) in char_indices.iter().rev() {
        let byte = *byte;
        let ch = *ch;
        if byte >= cursor_byte {
            continue;
        }

        if is_symmetric(ch) && ch == target_close {
            // Symmetric: alternate open/close semantics via stack.
            if closer_stack.last() == Some(&ch) {
                // Found a balancing closer → pop.
                closer_stack.pop();
            } else if closer_stack.is_empty() {
                // Stack empty: this is the unmatched opener.
                opener_byte = Some(byte);
                break;
            } else {
                // Different char on stack: treat as closer.
                closer_stack.push(ch);
            }
        } else if is_closer(ch) {
            let complement = matching_opener(ch);
            if complement == Some(ch) {
                // Self-complementary closer.
                closer_stack.push(ch);
            } else {
                closer_stack.push(ch);
            }
        } else if is_opener(ch) {
            let complement = matching_closer(ch);
            if let Some(comp) = complement {
                if closer_stack.last() == Some(&comp) {
                    closer_stack.pop();
                }
                // If stack is now empty and ch matches our target opener:
                if closer_stack.is_empty() && ch == target_open {
                    opener_byte = Some(byte);
                    break;
                }
            }
        } else if is_symmetric(ch) {
            // Non-target symmetric char — treat as opener (pop) or closer (push).
            if closer_stack.last() == Some(&ch) {
                closer_stack.pop();
            } else {
                closer_stack.push(ch);
            }
        }
    }

    let opener_byte = opener_byte?;

    // ---- Forward scan: find the unmatched closer ----
    let mut opener_stack: Vec<char> = Vec::new();
    let mut closer_byte: Option<usize> = None;

    for (byte, ch) in char_indices.iter() {
        let byte = *byte;
        let ch = *ch;
        if byte < cursor_byte {
            continue;
        }

        if is_symmetric(ch) && ch == target_open {
            if opener_stack.last() == Some(&ch) {
                opener_stack.pop();
            } else if opener_stack.is_empty() {
                // Stack empty: this is the unmatched closer.
                closer_byte = Some(byte);
                break;
            } else {
                opener_stack.push(ch);
            }
        } else if is_opener(ch) {
            opener_stack.push(ch);
        } else if is_closer(ch) {
            if let Some(open) = opener_stack.last() {
                let comp = matching_closer(*open);
                if comp == Some(ch) || (is_symmetric(*open) && *open == ch) {
                    opener_stack.pop();
                }
            }
            if opener_stack.is_empty() && ch == target_close {
                closer_byte = Some(byte);
                break;
            }
        } else if is_symmetric(ch) {
            if opener_stack.last() == Some(&ch) {
                opener_stack.pop();
            } else {
                opener_stack.push(ch);
            }
        }
    }

    let closer_byte = closer_byte?;
    Some((opener_byte, closer_byte))
}

// ── Operator: surround-delete ─────────────────────────────────

fn operator_surround_delete(
    ctx: &mut lattice_grammar::registry::OperatorContext,
) -> Result<Effect, CommandError> {
    let target = match &ctx.args {
        Args::Char(c) => *c,
        _ => return Err(CommandError::InvalidArgs("ds requires Args::Char")),
    };

    // Scope the immutable buffer borrow — compute everything needed
    // before we apply mutable edits.
    let (open_pos, close_pos, open_text, close_text) = {
        let buffer = ctx.document.buffer();
        let cursor = get_cursor_from_document(ctx.document);
        let (open_byte, close_byte) = match find_surround_pair(buffer, cursor, target) {
            Some(pair) => pair,
            None => return Ok(Effect::None),
        };
        let open_pos = buffer.byte_to_position(open_byte)?;
        let close_pos = buffer.byte_to_position(close_byte)?;

        let yanked_open = buffer.slice(lattice_protocol::position::Range::new(
            open_pos,
            Position::new(open_pos.line, open_pos.byte + target.len_utf8() as u32),
        ))?;

        let target_close = open_close_pair(target).map(|(_, c)| c).unwrap_or(target);
        let yanked_close = buffer.slice(lattice_protocol::position::Range::new(
            close_pos,
            Position::new(
                close_pos.line,
                close_pos.byte + target_close.len_utf8() as u32,
            ),
        ))?;

        (open_pos, close_pos, yanked_open, yanked_close)
    };

    let target_close = open_close_pair(target).map(|(_, c)| c).unwrap_or(target);

    // SU.3g: `ds(` takes the inner padding with it, `ds)` does not — so
    // `ds(` undoes `ysiw(` exactly and a round trip does not accumulate
    // spaces.
    let (open_span, close_span) = {
        let buffer = ctx.document.buffer();
        delimiter_spans(
            buffer,
            open_pos,
            target.len_utf8() as u32,
            close_pos,
            target_close.len_utf8() as u32,
            pads_inside(target),
        )
    };

    // Apply deletions as one undo batch (closer first so byte offsets stay valid).
    let edit_close = Edit::delete(close_span);
    let edit_open = Edit::delete(open_span);

    let applied = ctx.document.apply_edit_batch(vec![edit_close, edit_open])?;

    let new_cursor = if open_pos.line == close_pos.line {
        Position::new(open_pos.line, open_pos.byte)
    } else {
        open_pos
    };

    Ok(Effect::Many(vec![
        Effect::Edits(applied),
        Effect::Yank {
            register: ctx.register,
            content: format!("{}{}", open_text, close_text),
            kind: YankKind::Charwise,
            explicit_yank: false,
        },
        Effect::CursorMove(new_cursor),
    ]))
}

// ── Operator: surround-change ─────────────────────────────────

fn operator_surround_change(
    ctx: &mut lattice_grammar::registry::OperatorContext,
) -> Result<Effect, CommandError> {
    let (target, replacement) = match &ctx.args {
        Args::List(values) if values.len() == 2 => {
            let t = match &values[0] {
                ArgValue::Char(c) => *c,
                _ => return Err(CommandError::InvalidArgs("cs arg[0] must be Char")),
            };
            let r = match &values[1] {
                ArgValue::Char(c) => *c,
                _ => return Err(CommandError::InvalidArgs("cs arg[1] must be Char")),
            };
            (t, r)
        }
        _ => {
            return Err(CommandError::InvalidArgs(
                "cs requires Args::List([Char, Char])",
            ));
        }
    };

    let (new_open, new_close) = match open_close_pair(replacement) {
        Some(pair) => pair,
        None => return Ok(Effect::None),
    };
    let target_close = open_close_pair(target).map(|(_, c)| c).unwrap_or(target);

    let (open_pos, close_pos, open_text, close_text) = {
        let buffer = ctx.document.buffer();
        let cursor = get_cursor_from_document(ctx.document);
        let (open_byte, close_byte) = match find_surround_pair(buffer, cursor, target) {
            Some(pair) => pair,
            None => return Ok(Effect::None),
        };
        let open_pos = buffer.byte_to_position(open_byte)?;
        let close_pos = buffer.byte_to_position(close_byte)?;

        let yanked_open = buffer.slice(lattice_protocol::position::Range::new(
            open_pos,
            Position::new(open_pos.line, open_pos.byte + target.len_utf8() as u32),
        ))?;
        let yanked_close = buffer.slice(lattice_protocol::position::Range::new(
            close_pos,
            Position::new(
                close_pos.line,
                close_pos.byte + target_close.len_utf8() as u32,
            ),
        ))?;

        (open_pos, close_pos, yanked_open, yanked_close)
    };

    // SU.3g: both halves of the rule meet here. The *target* decides
    // whether the old pair's padding comes out (`cs(` yes, `cs)` no) and
    // the *replacement* decides whether new padding goes in — so
    // `cs("` turns `( hello )` into `"hello"` and `cs")` turns
    // `"hello"` into `(hello)`.
    let pad = padding_for(replacement);
    let open_text_repl = format!("{new_open}{pad}");
    let close_text_repl = format!("{pad}{new_close}");

    let (open_span, close_span) = {
        let buffer = ctx.document.buffer();
        delimiter_spans(
            buffer,
            open_pos,
            target.len_utf8() as u32,
            close_pos,
            target_close.len_utf8() as u32,
            pads_inside(target),
        )
    };

    let edit_close = Edit::replace(close_span, close_text_repl.clone());
    let edit_open = Edit::replace(open_span, open_text_repl);

    let applied = ctx.document.apply_edit_batch(vec![edit_close, edit_open])?;

    let new_cursor = if open_pos.line == close_pos.line {
        Position::new(open_pos.line, open_pos.byte + 1)
    } else {
        Position::new(open_pos.line, open_pos.byte + new_open.len_utf8() as u32)
    };
    Ok(Effect::Many(vec![
        Effect::Edits(applied),
        Effect::Yank {
            register: ctx.register,
            content: format!("{}{}", open_text, close_text),
            kind: YankKind::Charwise,
            explicit_yank: false,
        },
        Effect::CursorMove(new_cursor),
    ]))
}

// ── Operator: surround-add ────────────────────────────────────

fn operator_surround_add(
    ctx: &mut lattice_grammar::registry::OperatorContext,
) -> Result<Effect, CommandError> {
    let wrapper = match &ctx.args {
        Args::Char(c) => *c,
        _ => {
            return Err(CommandError::InvalidArgs(
                "surround-add requires Args::Char",
            ));
        }
    };

    let (open, close) = match open_close_pair(wrapper) {
        Some(pair) => pair,
        None => return Ok(Effect::None),
    };

    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }

    // Scope immutable buffer borrow — compute wrap content and cursor
    // position before applying the mutable edit.
    let (wrap_start, wrap_end, wrap_text, new_cursor) = {
        let buffer = ctx.document.buffer();

        let (wrap_start, wrap_end, wrap_text) = if ctx.linewise {
            let line = buffer.line(ctx.range.start.line).unwrap_or_default();
            let start = Position::new(ctx.range.start.line, 0);
            let line_byte_len = buffer.line_byte_len(ctx.range.start.line);
            let end = Position::new(ctx.range.start.line, line_byte_len);
            let text = line.to_string();
            (start, end, text)
        } else {
            let text = buffer.slice(ctx.range)?;
            (ctx.range.start, ctx.range.end, text)
        };

        // SU.3g: the cursor lands just inside the pair, which with the
        // padding form means past the pad as well as past the delimiter.
        let inside = open.len_utf8() + padding_for(wrapper).len();

        let new_cursor = if ctx.linewise {
            Position::new(wrap_start.line, inside as u32)
        } else {
            let new_byte = buffer.position_to_byte(wrap_start)? + inside;
            buffer.byte_to_position(new_byte)?
        };

        (wrap_start, wrap_end, wrap_text, new_cursor)
    };

    // SU.3g: `ysiw(` → `( hello )`, `ysiw)` → `(hello)`.
    let pad = padding_for(wrapper);
    let wrapped = format!("{open}{pad}{wrap_text}{pad}{close}");

    let wrap_range = lattice_protocol::position::Range::new(wrap_start, wrap_end);
    let edit = Edit::replace(wrap_range, wrapped);
    let applied = ctx.document.apply_edit(edit)?;

    Ok(Effect::Many(vec![
        Effect::Edits(vec![applied]),
        Effect::CursorMove(new_cursor),
    ]))
}

// ── Helpers ───────────────────────────────────────────────────

fn get_cursor_from_document(doc: &Document) -> Position {
    doc.selections().primary().head
}

// ── Operator registration ─────────────────────────────────────

/// Typed handles for the surround operators.
#[derive(Debug, Clone)]
pub struct SurroundOperators {
    pub delete: OperatorId,
    pub change: OperatorId,
    pub add: OperatorId,
}

/// Register the three surround operators in the shared
/// `CommandRegistry`. Called from the host's grammar bootstrap.
pub fn register_surround_operators(registry: &mut CommandRegistry) -> SurroundOperators {
    let delete = registry.register_operator(
        "operator:surround-delete",
        "Delete the nearest surrounding pair (vim's `ds{char}`).",
        OperatorSpec {
            repeatable: true,
            apply: Arc::new(operator_surround_delete),
            args_schema: vec![ArgSpec::required(
                "target",
                ArgKind::Char,
                "The surrounding pair character to delete (e.g. `\"`, `(`, `[`)",
            )],
            blockwise_per_row: false,
            post_motion_char: false,
        },
    );

    let change = registry.register_operator(
        "operator:surround-change",
        "Change the nearest surrounding pair to a different one (vim's `cs{old}{new}`).",
        OperatorSpec {
            repeatable: true,
            apply: Arc::new(operator_surround_change),
            args_schema: vec![
                ArgSpec::required(
                    "target",
                    ArgKind::Char,
                    "The current surrounding pair character",
                ),
                ArgSpec::required(
                    "replacement",
                    ArgKind::Char,
                    "The replacement surrounding pair character",
                ),
            ],
            blockwise_per_row: false,
            post_motion_char: false,
        },
    );

    let add = registry.register_operator(
        "operator:surround-add",
        "Wrap the target range in a surrounding pair (vim's `yss{char}` / visual `S{char}` / `ys{motion}{char}`).",
        OperatorSpec {
            repeatable: true,
            apply: Arc::new(operator_surround_add),
            args_schema: vec![
                ArgSpec::required("wrapper", ArgKind::Char, "The pair character to wrap with"),
            ],
            blockwise_per_row: false,
            post_motion_char: true,
        },
    );

    SurroundOperators {
        delete,
        change,
        add,
    }
}

// ── Mode ──────────────────────────────────────────────────────

/// Minor mode providing vim-surround operations.
///
/// Owns its operators and keymap surface. All behavior lives in
/// the grammar operators registered at boot; the mode contributes
/// only the keymap (with wildcard capture paths for `ds{char}`
/// and `cs{char1}{char2}`).
pub struct SurroundMode {
    operators: SurroundOperators,
}

impl SurroundMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("surround-mode")
    }
}

impl Mode for SurroundMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Global
    }

    fn keymap(&self) -> Keymap {
        use lattice_grammar::command::CommandInvocation;
        use lattice_keymap::binding_mode::BindingMode;
        use lattice_keymap::contribution::KeymapBinding;

        let lit_ch = |c: char| ChordPattern::Literal(KeyChord::char(c));

        // Chain-form only. Every binding here ends in a `CharLiteral`
        // wildcard, which the table form cannot express.
        //
        // SU.3e: there used to be four table-form entries appended below,
        // on the belief that they were an inert `:describe-key` / `:keymap`
        // catalog while these bindings did the dispatching. They were not
        // inert. `push_mode_keymap` resolves entries into real
        // `KeymapBinding`s and pushes them into the SAME trie, so the
        // Visual row `chord: "S"` bound `[S]` at one chord and the trie —
        // which returns a node's own binding before descending — could
        // never reach `[S, CharLiteral]`. The mode shadowed itself; that is
        // the `S`-does-nothing-but-`:describe-key`-lists-it report.
        //
        // The three Normal rows were worse than redundant: they wrote their
        // chords space-separated (`"d s"`), and `parse_chord_sequence` reads
        // a space as a literal Space chord, so they bound `d<Space>s`,
        // `c<Space>s` and `y<Space>s<Space>s`. Nothing resolved those, and
        // three stray bindings sat in the trie. They were the only
        // space-separated `keymap_entry!` chords in the workspace.
        //
        // Nothing was lost by deleting them. No production code reads the
        // static catalog: `:describe-key` resolves against the trie
        // (`Editor::build_describe_key_content` → `resolve_trace`), so the
        // `with_doc` strings below are what it already showed.
        Keymap::new()
            .bind(
                KeymapBinding::new(
                    BindingMode::Normal,
                    vec![lit_ch('d'), lit_ch('s'), ChordPattern::CharLiteral],
                    CommandInvocation::of(self.operators.delete.0)
                        .with_range(lattice_grammar::range::Range::CurrentLine),
                    SourceLocation::builtin_file(file!(), line!()),
                )
                .with_doc("Delete the nearest surrounding pair (vim's `ds{char}`)."),
            )
            .bind(
                KeymapBinding::new(
                    BindingMode::Normal,
                    vec![
                        lit_ch('c'),
                        lit_ch('s'),
                        ChordPattern::CharLiteral,
                        ChordPattern::CharLiteral,
                    ],
                    CommandInvocation::of(self.operators.change.0)
                        .with_range(lattice_grammar::range::Range::CurrentLine),
                    SourceLocation::builtin_file(file!(), line!()),
                )
                .with_doc("Change the nearest surrounding pair (vim's `cs{old}{new}`)."),
            )
            .bind(
                KeymapBinding::new(
                    BindingMode::Normal,
                    vec![
                        lit_ch('y'),
                        lit_ch('s'),
                        lit_ch('s'),
                        ChordPattern::CharLiteral,
                    ],
                    CommandInvocation::of(self.operators.add.0)
                        .with_range(lattice_grammar::range::Range::CurrentLine),
                    SourceLocation::builtin_file(file!(), line!()),
                )
                .with_doc("Wrap the current line in a surrounding pair (vim's `yss{char}`)."),
            )
            .bind(
                KeymapBinding::new(
                    BindingMode::Visual,
                    vec![lit_ch('S'), ChordPattern::CharLiteral],
                    CommandInvocation::of(self.operators.add.0)
                        .with_range(lattice_grammar::range::Range::Selection),
                    SourceLocation::builtin_file(file!(), line!()),
                )
                .with_doc(
                    "Wrap the visual selection in a surrounding pair (vim's visual `S{char}`).",
                ),
            )
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
}

// ── Mode registration ─────────────────────────────────────────

/// Register surround-mode in the ModeRegistry. Must be called
/// AFTER `register_surround_operators` so the operator IDs are
/// available for keymap construction.
pub fn register_surround_modes(
    registry: &mut crate::registry::ModeRegistry,
    operators: SurroundOperators,
) {
    registry
        .register(SurroundMode { operators })
        .expect("surround-mode must register without conflict");
}

// ── Tests ─────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_core::buffer::Buffer;
    use lattice_protocol::position::Position;

    #[test]
    fn open_close_pair_maps_brackets() {
        assert_eq!(open_close_pair('('), Some(('(', ')')));
        assert_eq!(open_close_pair(')'), Some(('(', ')')));
        assert_eq!(open_close_pair('['), Some(('[', ']')));
        assert_eq!(open_close_pair(']'), Some(('[', ']')));
        assert_eq!(open_close_pair('{'), Some(('{', '}')));
        assert_eq!(open_close_pair('}'), Some(('{', '}')));
        assert_eq!(open_close_pair('<'), Some(('<', '>')));
        assert_eq!(open_close_pair('>'), Some(('<', '>')));
    }

    #[test]
    fn open_close_pair_maps_symmetric() {
        assert_eq!(open_close_pair('"'), Some(('"', '"')));
        assert_eq!(open_close_pair('\''), Some(('\'', '\'')));
        assert_eq!(open_close_pair('`'), Some(('`', '`')));
    }

    #[test]
    fn open_close_pair_unknown_returns_none() {
        assert_eq!(open_close_pair('x'), None);
        assert_eq!(open_close_pair(' '), None);
    }

    #[test]
    fn find_surround_pair_simple_quotes() {
        let buf = Buffer::from_text("hello \"world\" foo");
        // Cursor at 'o' in "world" (byte 8 within line 0).
        let cursor = buf.position_to_byte(Position::new(0, 8)).unwrap();
        let cursor_pos = buf.byte_to_position(cursor).unwrap();
        let pair = find_surround_pair(&buf, cursor_pos, '"').unwrap();
        let open_pos = buf.byte_to_position(pair.0).unwrap();
        let close_pos = buf.byte_to_position(pair.1).unwrap();
        assert_eq!(open_pos, Position::new(0, 6)); // first "
        assert_eq!(close_pos, Position::new(0, 12)); // second "
    }

    #[test]
    fn find_surround_pair_brackets() {
        let buf = Buffer::from_text("fn foo(x: i32) {}");
        // Cursor at 'i' in i32 (byte 10).
        let cursor = buf.position_to_byte(Position::new(0, 10)).unwrap();
        let cursor_pos = buf.byte_to_position(cursor).unwrap();
        let pair = find_surround_pair(&buf, cursor_pos, '(').unwrap();
        let open_pos = buf.byte_to_position(pair.0).unwrap();
        let close_pos = buf.byte_to_position(pair.1).unwrap();
        // f(0)n(1) (2)f(3)o(4)o(5)((6)x(7):(8) (9)i(10)3(11)2(12))(13) (14){(15)}(16)
        assert_eq!(open_pos, Position::new(0, 6)); // '('
        assert_eq!(close_pos, Position::new(0, 13)); // ')'
    }

    #[test]
    fn find_surround_pair_nested() {
        let buf = Buffer::from_text("a (b (c) d) e");
        // Cursor at 'c' (byte 7).
        let cursor = buf.position_to_byte(Position::new(0, 7)).unwrap();
        let cursor_pos = buf.byte_to_position(cursor).unwrap();
        let pair = find_surround_pair(&buf, cursor_pos, '(').unwrap();
        let open_pos = buf.byte_to_position(pair.0).unwrap();
        let close_pos = buf.byte_to_position(pair.1).unwrap();
        // Inner pair: `(c)`
        // a(0) (1)(2)b(3) (4)((5)c(6))(7) (8)d(9))(10) (11)e(12)
        assert_eq!(open_pos, Position::new(0, 5)); // inner '('
        assert_eq!(close_pos, Position::new(0, 7)); // inner ')'
    }

    #[test]
    fn find_surround_pair_no_match() {
        let buf = Buffer::from_text("hello world");
        let pair = find_surround_pair(&buf, Position::ZERO, '"');
        assert!(pair.is_none());
    }

    // ── SU.3f: a delimiter sitting under the cursor ───────────
    //
    // The scans are half-open around the cursor — backward is
    // `byte < cursor_byte`, forward is `byte >= cursor_byte` — so a
    // closer under the cursor was always found (the forward scan starts
    // on it) while an opener under the cursor never was (the backward
    // scan starts just past it). `ds"` with the caret on the opening
    // quote did nothing; vim deletes the pair from either delimiter.

    /// Helper: byte-offset pair → (open col, close col) on line 0.
    fn pair_cols(buf: &Buffer, cursor_col: u32, target: char) -> Option<(u32, u32)> {
        let pair = find_surround_pair(buf, Position::new(0, cursor_col), target)?;
        let open = buf.byte_to_position(pair.0).ok()?;
        let close = buf.byte_to_position(pair.1).ok()?;
        Some((open.byte, close.byte))
    }

    #[test]
    fn a_cursor_on_the_opening_quote_finds_its_pair() {
        let buf = Buffer::from_text("\"hello\"");
        assert_eq!(pair_cols(&buf, 0, '"'), Some((0, 6)));
    }

    #[test]
    fn a_cursor_on_the_closing_quote_finds_its_pair() {
        let buf = Buffer::from_text("\"hello\"");
        assert_eq!(pair_cols(&buf, 6, '"'), Some((0, 6)));
    }

    #[test]
    fn a_cursor_on_the_opening_bracket_finds_its_pair() {
        let buf = Buffer::from_text("(hello)");
        assert_eq!(pair_cols(&buf, 0, '('), Some((0, 6)));
    }

    #[test]
    fn a_cursor_on_the_closing_bracket_finds_its_pair() {
        let buf = Buffer::from_text("(hello)");
        assert_eq!(pair_cols(&buf, 6, '('), Some((0, 6)));
    }

    /// The case that makes "is this delimiter an opener or a closer?"
    /// a real question rather than a formality. A symmetric char is
    /// both, so the answer comes from how many precede it on the line:
    /// an even count means this one opens. Without that rule, the
    /// cursor on the second pair's opening quote resolves the *gap*
    /// between the two pairs — quotes 2 and 4 — which is a plausible
    /// enclosing pair and entirely the wrong one.
    #[test]
    fn a_cursor_on_a_later_opening_quote_takes_its_own_pair() {
        let buf = Buffer::from_text("\"a\" \"b\"");
        // "(0)a(1)"(2) (3)"(4)b(5)"(6)
        assert_eq!(pair_cols(&buf, 4, '"'), Some((4, 6)));
    }

    #[test]
    fn a_cursor_on_a_later_closing_quote_takes_its_own_pair() {
        let buf = Buffer::from_text("\"a\" \"b\"");
        assert_eq!(pair_cols(&buf, 6, '"'), Some((4, 6)));
    }

    /// Nesting still resolves innermost-first when the cursor is on an
    /// inner delimiter rather than on text.
    #[test]
    fn a_cursor_on_an_inner_bracket_takes_the_inner_pair() {
        let buf = Buffer::from_text("a (b (c) d) e");
        // a(0) (1)((2)b(3) (4)((5)c(6))(7) (8)d(9))(10)
        assert_eq!(pair_cols(&buf, 5, '('), Some((5, 7)));
    }

    /// An unmatched delimiter under the cursor still finds nothing,
    /// rather than pairing with something across the buffer.
    #[test]
    fn a_lone_delimiter_under_the_cursor_finds_nothing() {
        let buf = Buffer::from_text("(hello");
        assert_eq!(pair_cols(&buf, 0, '('), None);
    }

    #[test]
    fn find_surround_pair_close_char_target() {
        let buf = Buffer::from_text("(hello)");
        let cursor = buf.position_to_byte(Position::new(0, 3)).unwrap(); // on 'l'
        let cursor_pos = buf.byte_to_position(cursor).unwrap();
        // Using ')' as target should also find the pair.
        let pair = find_surround_pair(&buf, cursor_pos, ')').unwrap();
        let open_pos = buf.byte_to_position(pair.0).unwrap();
        let close_pos = buf.byte_to_position(pair.1).unwrap();
        assert_eq!(open_pos, Position::new(0, 0)); // '('
        assert_eq!(close_pos, Position::new(0, 6)); // ')'
    }

    #[test]
    fn find_surround_pair_large_line_performance() {
        // Worst-case: scan a long line with no matching pair.
        let line = "x".repeat(10_000);
        let buf = Buffer::from_text(&line);
        let cursor = buf.position_to_byte(Position::new(0, 5000)).unwrap();
        let cursor_pos = buf.byte_to_position(cursor).unwrap();
        let start = std::time::Instant::now();
        let _ = find_surround_pair(&buf, cursor_pos, '"');
        let elapsed = start.elapsed();
        // 10k chars should be well under 1ms (linear scan).
        assert!(
            elapsed.as_micros() < 1000,
            "find_surround_pair on 10k chars took {:?}",
            elapsed
        );
    }
}

#[cfg(test)]
mod operator_tests {
    use super::*;
    use lattice_core::BufferId;
    use lattice_grammar::CancellationToken;
    use lattice_grammar::args::{ArgValue, Args};
    use lattice_grammar::builtins::populate as grammar_builtins_populate;
    use lattice_grammar::command::CommandInvocation;
    use lattice_grammar::dispatcher::execute as grammar_execute;

    use lattice_grammar::builtins::Builtins;

    fn fixture(text: &str) -> (CommandRegistry, Builtins, SurroundOperators, Document) {
        let mut r = CommandRegistry::new();
        let builtins = grammar_builtins_populate(&mut r);
        let ops = register_surround_operators(&mut r);
        let d = Document::from_text(text);
        (r, builtins, ops, d)
    }

    fn doc_text(doc: &Document) -> String {
        doc.buffer().as_string()
    }

    fn set_cursor(doc: &mut Document, pos: Position) {
        use lattice_protocol::selection::{Selection, SelectionSet};
        doc.set_selections(SelectionSet::single(Selection::cursor(pos)));
    }

    #[test]
    fn surround_delete_removes_double_quotes() {
        let (registry, _builtins, ops, mut doc) = fixture("hello \"world\" foo");
        let cursor = Position::new(0, 8); // on 'o' in "world"
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.delete.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char('"'));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "hello world foo");
    }

    #[test]
    fn surround_delete_no_match_is_noop() {
        let (registry, _builtins, ops, mut doc) = fixture("hello world");
        let cursor = Position::ZERO;
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.delete.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char('"'));
        let eff = grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Effect::None is produced for no-op.
        assert!(matches!(eff, lattice_grammar::effect::Effect::None));
        assert_eq!(doc_text(&doc), "hello world");
    }

    #[test]
    fn surround_delete_removes_parens() {
        let (registry, _builtins, ops, mut doc) = fixture("fn foo(x: i32) {}");
        let cursor = Position::new(0, 10); // on 'i' in i32
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.delete.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char('('));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "fn foox: i32 {}");
    }

    #[test]
    fn surround_delete_with_close_char_target() {
        let (registry, _builtins, ops, mut doc) = fixture("(hello world)");
        let cursor = Position::new(0, 3); // on 'l'
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.delete.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char(')'));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "hello world");
    }

    #[test]
    fn surround_change_double_to_single_quotes() {
        let (registry, _builtins, ops, mut doc) = fixture("hello \"world\" foo");
        let cursor = Position::new(0, 8); // on 'o' in "world"
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.change.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::List(vec![ArgValue::Char('"'), ArgValue::Char('\'')]));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "hello 'world' foo");
    }

    /// SU.3g changed this expectation deliberately: the replacement here
    /// is `[`, the padding form, so the result gains inner spaces. The
    /// unpadded spelling is `cs(]`, covered in
    /// `surround_change_pads_when_the_replacement_is_an_opening_form`.
    #[test]
    fn surround_change_parens_to_brackets() {
        let (registry, _builtins, ops, mut doc) = fixture("(hello)");
        let cursor = Position::new(0, 2); // on 'e'
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.change.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::List(vec![ArgValue::Char('('), ArgValue::Char('[')]));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "[ hello ]");
    }

    #[test]
    fn surround_add_linewise_wraps_line() {
        let (registry, _builtins, ops, mut doc) = fixture("hello world\n");
        let cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(ops.add.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char('"'));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "\"hello world\"\n");
    }

    /// SU.3g changed this expectation deliberately: `(` is the padding
    /// form, so a linewise add with `(` now yields `( hello )`. The
    /// unpadded spelling moved to `surround_add_with_the_closing_form_does_not_pad`.
    #[test]
    fn surround_add_linewise_wraps_line_with_brackets() {
        let (registry, _builtins, ops, mut doc) = fixture("hello\n");
        let cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(ops.add.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char('('));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "( hello )\n");
    }

    // ── SU.3g: the opening form pads, the closing form does not ──

    /// Run `yss{ch}` (linewise add) over `text` and return the result.
    fn add_linewise(text: &str, wrapper: char) -> String {
        let (registry, _builtins, ops, mut doc) = fixture(text);
        let cursor = Position::new(0, 0);
        let inv = CommandInvocation::of(ops.add.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char(wrapper));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        doc_text(&doc)
    }

    /// Run `cs{target}{replacement}` with the cursor at `col`.
    fn change_at(text: &str, col: u32, target: char, replacement: char) -> String {
        let (registry, _builtins, ops, mut doc) = fixture(text);
        let cursor = Position::new(0, col);
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.change.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::List(vec![
                ArgValue::Char(target),
                ArgValue::Char(replacement),
            ]));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        doc_text(&doc)
    }

    /// Run `ds{target}` with the cursor at `col`.
    fn delete_at(text: &str, col: u32, target: char) -> String {
        let (registry, _builtins, ops, mut doc) = fixture(text);
        let cursor = Position::new(0, col);
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.delete.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char(target));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        doc_text(&doc)
    }

    #[test]
    fn surround_add_with_the_opening_form_pads() {
        assert_eq!(add_linewise("hello\n", '('), "( hello )\n");
        assert_eq!(add_linewise("hello\n", '['), "[ hello ]\n");
        assert_eq!(add_linewise("hello\n", '{'), "{ hello }\n");
        assert_eq!(add_linewise("hello\n", '<'), "< hello >\n");
    }

    #[test]
    fn surround_add_with_the_closing_form_does_not_pad() {
        assert_eq!(add_linewise("hello\n", ')'), "(hello)\n");
        assert_eq!(add_linewise("hello\n", ']'), "[hello]\n");
        assert_eq!(add_linewise("hello\n", '}'), "{hello}\n");
        assert_eq!(add_linewise("hello\n", '>'), "<hello>\n");
    }

    /// A symmetric wrapper has no opening form to distinguish, so it never
    /// pads however it is typed.
    #[test]
    fn surround_add_never_pads_a_symmetric_wrapper() {
        assert_eq!(add_linewise("hello\n", '"'), "\"hello\"\n");
        assert_eq!(add_linewise("hello\n", '\''), "'hello'\n");
    }

    #[test]
    fn surround_change_pads_when_the_replacement_is_an_opening_form() {
        assert_eq!(change_at("\"hello\"", 3, '"', '('), "( hello )");
        assert_eq!(change_at("\"hello\"", 3, '"', ')'), "(hello)");
    }

    /// The removal half, and the reason it is in this slice rather than a
    /// later one: without it `ds(` cannot undo what `ysiw(` just did, and
    /// the padding would accumulate on every round trip.
    #[test]
    fn surround_delete_with_the_opening_form_takes_the_padding_too() {
        assert_eq!(delete_at("( hello )", 3, '('), "hello");
        // Already unpadded — the opening form must not eat real text.
        assert_eq!(delete_at("(hello)", 3, '('), "hello");
    }

    /// The closing form deletes the delimiters and nothing else, so the
    /// padding survives. This is the pair to the test above: the two forms
    /// have to differ on removal or the distinction is decorative.
    #[test]
    fn surround_delete_with_the_closing_form_leaves_the_padding() {
        assert_eq!(delete_at("( hello )", 3, ')'), " hello ");
    }

    #[test]
    fn surround_change_from_a_padded_pair_drops_the_padding() {
        assert_eq!(change_at("( hello )", 3, '(', '"'), "\"hello\"");
        // ...and the closing form keeps it, symmetrically with delete.
        assert_eq!(change_at("( hello )", 3, ')', '"'), "\" hello \"");
    }

    /// Round trip: add the padded form, remove it with the same character,
    /// and the buffer is back where it started. This is the property the
    /// two halves exist to hold.
    #[test]
    fn the_padded_forms_round_trip() {
        for ch in ['(', '[', '{', '<'] {
            let added = add_linewise("hello\n", ch);
            let back = delete_at(added.trim_end_matches('\n'), 3, ch);
            assert_eq!(back, "hello", "round trip failed for {ch:?}");
        }
    }

    #[test]
    fn surround_add_on_selection_wraps_text() {
        let (registry, _builtins, ops, mut doc) = fixture("hello world");
        let cursor = Position::new(0, 0);
        // Set the document's selection to cover "hello" (bytes 0-5, half-open).
        use lattice_protocol::selection::{Selection, SelectionSet};
        doc.set_selections(SelectionSet::from_parts(
            vec![Selection {
                anchor: Position::ZERO,
                head: Position::new(0, 5),
                visual: None,
            }],
            0,
        ));
        let inv = CommandInvocation::of(ops.add.0)
            .with_range(lattice_grammar::range::Range::Selection)
            .with_args(Args::Char('"'));
        let _eff = grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        let text = doc_text(&doc);
        // The text should be wrapped. The exact cursor position after wrap
        // varies; assert that the wrapping occurred.
        assert!(text.starts_with("\"hello"));
        assert!(text.ends_with("\" world") || text.ends_with("\"world"));
    }

    #[test]
    fn surround_change_no_match_is_noop() {
        let (registry, _builtins, ops, mut doc) = fixture("hello world");
        let cursor = Position::ZERO;
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.change.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::List(vec![ArgValue::Char('"'), ArgValue::Char('\'')]));
        let eff = grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, lattice_grammar::effect::Effect::None));
        assert_eq!(doc_text(&doc), "hello world");
    }

    #[test]
    fn surround_add_with_unknown_wrapper_is_noop() {
        let (registry, _builtins, ops, mut doc) = fixture("hello\n");
        let cursor = Position::ZERO;
        let inv = CommandInvocation::of(ops.add.0)
            .with_range(lattice_grammar::range::Range::CurrentLine)
            .with_args(Args::Char('x')); // 'x' is not a known pair
        let eff = grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, lattice_grammar::effect::Effect::None));
    }

    // ── ys{motion}{char} operator dispatch tests ────────────────

    #[test]
    fn surround_add_via_motion_word_forward() {
        // ysw" → wrap from cursor to word end in double quotes.
        let (registry, builtins, ops, mut doc) = fixture("hello world");
        let cursor = Position::new(0, 0); // on 'h'
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.add.0)
            .with_target(lattice_grammar::Target::Motion(
                builtins.word_forward,
                Args::None,
            ))
            .with_args(Args::Char('"'));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // word_forward from 0 resolves past the space after "hello",
        // so the wrapped span is "hello " (vim-accurate: w goes to next word start).
        assert!(
            doc_text(&doc).starts_with("\"hello"),
            "expected wrapped text to start with \"hello, got: {}",
            doc_text(&doc)
        );
    }

    #[test]
    fn surround_add_via_inner_word_text_object() {
        // ysiw" → wrap inner word in double quotes.
        let (registry, builtins, ops, mut doc) = fixture("hello world");
        let cursor = Position::new(0, 2); // on 'l' in "hello"
        set_cursor(&mut doc, cursor);
        let inv = CommandInvocation::of(ops.add.0)
            .with_target(lattice_grammar::Target::TextObject(
                builtins.inner_word,
                Args::None,
            ))
            .with_args(Args::Char('"'));
        grammar_execute(
            &registry,
            &mut doc,
            BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc_text(&doc), "\"hello\" world");
    }
}
