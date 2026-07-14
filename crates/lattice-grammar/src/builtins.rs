//! Built-in motions, text objects, and operators that ship native (off the
//! WASM boundary; per DESIGN.md §5.5.2 "built-ins stay native").
//!
//! Phase 1 implements the minimum necessary to demonstrate end-to-end
//! dispatch:
//! - `motion::word_forward` (next word start)
//! - `operator::delete`
//!
//! Subsequent revisions populate the full vim catalog. Each new built-in is
//! a registration here; no new dispatcher wiring needed.

use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::effect::{Effect, YankKind};
use crate::error::CommandError;
use crate::registry::{
    CommandRegistry, MotionContext, MotionId, MotionResult, MotionSpec, OperatorContext,
    OperatorId, OperatorSpec, TextObjectContext, TextObjectId, TextObjectSpec,
};

/// Register all Phase 1 built-ins. Returns the ids needed by the keystroke
/// parser / tests.
pub fn populate(registry: &mut CommandRegistry) -> Builtins {
    let word_forward = registry.register_motion(
        "motion:word-forward",
        "Move to the start of the next word.",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_word_forward),
            args_schema: vec![],
        },
    );
    let word_backward = registry.register_motion(
        "motion:word-backward",
        "Move to the start of the previous word (vim's `b`).",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_word_backward),
            args_schema: vec![],
        },
    );
    let word_end = registry.register_motion(
        "motion:word-end",
        "Move to the last byte of the current or next word (vim's `e`). Inclusive.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_word_end),
            args_schema: vec![],
        },
    );
    let first_non_blank = registry.register_motion(
        "motion:first-non-blank",
        "Move to the first non-whitespace byte of the current line (vim's `^`).",
        MotionSpec {
            jump: false,
            // `^` is exclusive in vim. (Was mis-registered inclusive while
            // the dispatcher ignored the flag; now honoured, so it must be
            // correct.)
            exclusive: true,
            apply: Box::new(motion_first_non_blank),
            args_schema: vec![],
        },
    );
    let find_char_forward = registry.register_motion(
        "motion:find-char-forward",
        "Move to the next occurrence of `args.char` on the current line (vim's `f`).",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_find_char_forward),
            args_schema: vec![],
        },
    );
    let find_char_backward = registry.register_motion(
        "motion:find-char-backward",
        "Move to the previous occurrence of `args.char` on the current line (vim's `F`).",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_find_char_backward),
            args_schema: vec![],
        },
    );
    let till_char_forward = registry.register_motion(
        "motion:till-char-forward",
        "Move to one byte before the next occurrence of `args.char` (vim's `t`).",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_till_char_forward),
            args_schema: vec![],
        },
    );
    let till_char_backward = registry.register_motion(
        "motion:till-char-backward",
        "Move to one byte after the previous occurrence of `args.char` (vim's `T`).",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_till_char_backward),
            args_schema: vec![],
        },
    );
    let big_word_forward = registry.register_motion(
        "motion:big-word-forward",
        "Move to the start of the next WORD (vim's `W` -- whitespace-delimited).",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_big_word_forward),
            args_schema: vec![],
        },
    );
    // Vim word-motion special case (`:help word-motions`): under an
    // operator, `dw` / `cw` / `yw` on the last word of a line stop at the
    // line end instead of reaching into the next line's first word. Tag
    // both word-forward motions so the operator range resolver applies it.
    registry.tag_word_forward_motion(word_forward);
    registry.tag_word_forward_motion(big_word_forward);

    let big_word_backward = registry.register_motion(
        "motion:big-word-backward",
        "Move to the start of the previous WORD (vim's `B`).",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_big_word_backward),
            args_schema: vec![],
        },
    );
    let big_word_end = registry.register_motion(
        "motion:big-word-end",
        "Move to the last byte of the current or next WORD (vim's `E`).",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_big_word_end),
            args_schema: vec![],
        },
    );
    let paragraph_forward = registry.register_motion(
        "motion:paragraph-forward",
        "Move to the next paragraph boundary -- the next blank line at or after the cursor (vim's `}`).",
        MotionSpec {
            jump: true,
            // `}` is exclusive in vim (`d}` does not include the blank line).
            exclusive: true,
            apply: Box::new(motion_paragraph_forward),
            args_schema: vec![],
        },
    );
    let paragraph_backward = registry.register_motion(
        "motion:paragraph-backward",
        "Move to the previous paragraph boundary (vim's `{`).",
        MotionSpec {
            jump: true,
            // `{` is exclusive in vim.
            exclusive: true,
            apply: Box::new(motion_paragraph_backward),
            args_schema: vec![],
        },
    );
    let sentence_forward = registry.register_motion(
        "motion:sentence-forward",
        "Move to the start of the next sentence (vim's `)`).",
        MotionSpec {
            jump: true,
            // `)` is exclusive in vim.
            exclusive: true,
            apply: Box::new(motion_sentence_forward),
            args_schema: vec![],
        },
    );
    let sentence_backward = registry.register_motion(
        "motion:sentence-backward",
        "Move to the start of the previous sentence (vim's `(`).",
        MotionSpec {
            jump: true,
            // `(` is exclusive in vim.
            exclusive: true,
            apply: Box::new(motion_sentence_backward),
            args_schema: vec![],
        },
    );
    let char_left = registry.register_motion(
        "motion:char-left",
        "Move one byte to the left within the current line.",
        MotionSpec {
            jump: false,
            // `h` is exclusive in vim. (Backward motion, so behaviour is
            // unchanged either way, but the registration should be correct.)
            exclusive: true,
            apply: Box::new(motion_char_left),
            args_schema: vec![],
        },
    );
    let char_right = registry.register_motion(
        "motion:char-right",
        "Move one byte to the right within the current line.",
        MotionSpec {
            jump: false,
            exclusive: true,
            apply: Box::new(motion_char_right),
            args_schema: vec![],
        },
    );
    let line_up = registry.register_motion(
        "motion:line-up",
        "Move one line up.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_line_up),
            args_schema: vec![],
        },
    );
    let line_down = registry.register_motion(
        "motion:line-down",
        "Move one line down.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_line_down),
            args_schema: vec![],
        },
    );
    let line_start = registry.register_motion(
        "motion:line-start",
        "Move to the first byte of the current line.",
        MotionSpec {
            jump: false,
            // `0` is exclusive in vim.
            exclusive: true,
            apply: Box::new(motion_line_start),
            args_schema: vec![],
        },
    );
    let line_end = registry.register_motion(
        "motion:line-end",
        "Move to the last byte of the current line.",
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(motion_line_end),
            args_schema: vec![],
        },
    );
    let goto_first_line = registry.register_motion(
        "motion:goto-first-line",
        "Jump to the first line of the buffer (vim's `gg`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(motion_goto_first_line),
            args_schema: vec![],
        },
    );
    let goto_last_line = registry.register_motion(
        "motion:goto-last-line",
        "Jump to the last line of the buffer (vim's `G`).",
        MotionSpec {
            jump: true,
            exclusive: false,
            apply: Box::new(motion_goto_last_line),
            args_schema: vec![],
        },
    );

    let delete = registry.register_operator(
        "operator:delete",
        "Delete the bytes covered by the target range; yank to the unnamed register.",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_delete),
            args_schema: vec![],
            blockwise_per_row: true,
        },
    );
    let change = registry.register_operator(
        "operator:change",
        "Delete the bytes covered by the target range and enter Insert mode (vim's `c`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_change),
            args_schema: vec![],
            blockwise_per_row: true,
        },
    );
    let yank = registry.register_operator(
        "operator:yank",
        "Copy the bytes covered by the target range into the named register without modifying the buffer (vim's `y`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_yank),
            args_schema: vec![],
            blockwise_per_row: true,
        },
    );
    let indent_left = registry.register_operator(
        "operator:indent-left",
        "Strip leading indentation (4 spaces or one tab) from each line in the range (vim's `<`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_indent_left),
            args_schema: vec![],
            // Linewise effect -- one batched edit covers every line
            // in the visual span, regardless of charwise / blockwise.
            blockwise_per_row: false,
        },
    );
    let indent_right = registry.register_operator(
        "operator:indent-right",
        "Prepend 4 spaces to each line in the range (vim's `>`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_indent_right),
            args_schema: vec![],
            blockwise_per_row: false,
        },
    );
    let upper = registry.register_operator(
        "operator:upper",
        "Uppercase ASCII letters in the range (vim's `gU`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_upper),
            args_schema: vec![],
            blockwise_per_row: false,
        },
    );
    let lower = registry.register_operator(
        "operator:lower",
        "Lowercase ASCII letters in the range (vim's `gu`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_lower),
            args_schema: vec![],
            blockwise_per_row: false,
        },
    );
    let toggle_case = registry.register_operator(
        "operator:toggle-case",
        "Toggle case of ASCII letters in the range (vim's `g~`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_toggle_case),
            args_schema: vec![],
            blockwise_per_row: false,
        },
    );
    let replace_char = registry.register_operator(
        "operator:replace-char",
        "Overwrite each non-newline char in the range with the captured char (vim's `r{char}` and Visual `r`).",
        OperatorSpec {
            repeatable: true,
            apply: Box::new(operator_replace_char),
            // Args::Char(replacement) is folded in by the keymap's
            // wildcard capture (same shape as `f{char}`, which also
            // leaves its schema empty).
            args_schema: vec![],
            // Blockwise visual `r` overwrites each row's column slice
            // independently, exactly like `d` / `y` / `c`.
            blockwise_per_row: true,
        },
    );

    let inner_sentence = registry.register_text_object(
        "text-object:inner-sentence",
        "Inner sentence -- text up to the next .!? that ends a sentence (vim's `is`).",
        TextObjectSpec {
            apply: Box::new(text_object_inner_sentence),
            args_schema: vec![],
        },
    );
    let around_sentence = registry.register_text_object(
        "text-object:around-sentence",
        "Around sentence -- inner_sentence plus trailing whitespace (vim's `as`).",
        TextObjectSpec {
            apply: Box::new(text_object_around_sentence),
            args_schema: vec![],
        },
    );

    let inner_paragraph = registry.register_text_object(
        "text-object:inner-paragraph",
        "Inner paragraph -- the run of non-blank lines containing the cursor (vim's `ip`).",
        TextObjectSpec {
            apply: Box::new(text_object_inner_paragraph),
            args_schema: vec![],
        },
    );
    let around_paragraph = registry.register_text_object(
        "text-object:around-paragraph",
        "Around paragraph -- inner_paragraph plus trailing blank lines (vim's `ap`).",
        TextObjectSpec {
            apply: Box::new(text_object_around_paragraph),
            args_schema: vec![],
        },
    );

    let inner_word = registry.register_text_object(
        "text-object:inner-word",
        "Inner word -- alphanum + underscore run containing the cursor (vim's `iw`).",
        TextObjectSpec {
            apply: Box::new(text_object_inner_word),
            args_schema: vec![],
        },
    );
    let around_word = registry.register_text_object(
        "text-object:around-word",
        "Around word -- inner_word plus trailing whitespace (vim's `aw`).",
        TextObjectSpec {
            apply: Box::new(text_object_around_word),
            args_schema: vec![],
        },
    );
    let inner_quote_double = registry.register_text_object(
        "text-object:inner-quote-double",
        "Inner double-quoted string -- text between the surrounding `\"` chars (vim's `i\"`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_inner_quote(ctx, '"')),
            args_schema: vec![],
        },
    );
    let around_quote_double = registry.register_text_object(
        "text-object:around-quote-double",
        "Around double-quoted string -- includes the surrounding `\"` chars (vim's `a\"`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_around_quote(ctx, '"')),
            args_schema: vec![],
        },
    );
    let inner_quote_single = registry.register_text_object(
        "text-object:inner-quote-single",
        "Inner single-quoted string (vim's `i'`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_inner_quote(ctx, '\'')),
            args_schema: vec![],
        },
    );
    let around_quote_single = registry.register_text_object(
        "text-object:around-quote-single",
        "Around single-quoted string (vim's `a'`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_around_quote(ctx, '\'')),
            args_schema: vec![],
        },
    );
    let inner_quote_backtick = registry.register_text_object(
        "text-object:inner-quote-backtick",
        "Inner backtick-quoted string (vim's ``i` ``).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_inner_quote(ctx, '`')),
            args_schema: vec![],
        },
    );
    let around_quote_backtick = registry.register_text_object(
        "text-object:around-quote-backtick",
        "Around backtick-quoted string (vim's ``a` ``).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_around_quote(ctx, '`')),
            args_schema: vec![],
        },
    );
    let inner_paren = registry.register_text_object(
        "text-object:inner-paren",
        "Inside the innermost enclosing `()` pair (vim's `i(`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_inner_brackets(ctx, '(', ')')),
            args_schema: vec![],
        },
    );
    let around_paren = registry.register_text_object(
        "text-object:around-paren",
        "Around the innermost enclosing `()` pair, including the brackets (vim's `a(`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_around_brackets(ctx, '(', ')')),
            args_schema: vec![],
        },
    );
    let inner_bracket = registry.register_text_object(
        "text-object:inner-bracket",
        "Inside the innermost enclosing `[]` pair (vim's `i[`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_inner_brackets(ctx, '[', ']')),
            args_schema: vec![],
        },
    );
    let around_bracket = registry.register_text_object(
        "text-object:around-bracket",
        "Around the innermost enclosing `[]` pair, including the brackets (vim's `a[`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_around_brackets(ctx, '[', ']')),
            args_schema: vec![],
        },
    );
    let inner_tag = registry.register_text_object(
        "text-object:inner-tag",
        "Inside the innermost enclosing XML/HTML tag pair (vim's `it`).",
        TextObjectSpec {
            apply: Box::new(text_object_inner_tag),
            args_schema: vec![],
        },
    );
    let around_tag = registry.register_text_object(
        "text-object:around-tag",
        "Around the innermost enclosing XML/HTML tag pair, including the tags (vim's `at`).",
        TextObjectSpec {
            apply: Box::new(text_object_around_tag),
            args_schema: vec![],
        },
    );

    let inner_brace = registry.register_text_object(
        "text-object:inner-brace",
        "Inside the innermost enclosing `{}` pair (vim's `i{`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_inner_brackets(ctx, '{', '}')),
            args_schema: vec![],
        },
    );
    let around_brace = registry.register_text_object(
        "text-object:around-brace",
        "Around the innermost enclosing `{}` pair, including the brackets (vim's `a{`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_around_brackets(ctx, '{', '}')),
            args_schema: vec![],
        },
    );
    let inner_big_word = registry.register_text_object(
        "text-object:inner-big-word",
        "Inner WORD -- whitespace-delimited run containing the cursor (vim's `iW`).",
        TextObjectSpec {
            apply: Box::new(text_object_inner_big_word),
            args_schema: vec![],
        },
    );
    let around_big_word = registry.register_text_object(
        "text-object:around-big-word",
        "Around WORD -- inner WORD plus trailing whitespace (vim's `aW`).",
        TextObjectSpec {
            apply: Box::new(text_object_around_big_word),
            args_schema: vec![],
        },
    );
    let inner_angle = registry.register_text_object(
        "text-object:inner-angle",
        "Inside the innermost enclosing `<>` pair (vim's `i<`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_inner_brackets(ctx, '<', '>')),
            args_schema: vec![],
        },
    );
    let around_angle = registry.register_text_object(
        "text-object:around-angle",
        "Around the innermost enclosing `<>` pair, including the brackets (vim's `a<`).",
        TextObjectSpec {
            apply: Box::new(|ctx| text_object_around_brackets(ctx, '<', '>')),
            args_schema: vec![],
        },
    );

    // N.1.6: comment text objects (commentstring-driven; the leader is
    // injected per-buffer via `TextObjectContext.comment_syntax`).
    let inner_comment = registry.register_text_object(
        "text-object:inner-comment",
        "Inner comment -- the comment text with the first line's leader stripped (`iC`).",
        TextObjectSpec {
            apply: Box::new(text_object_inner_comment),
            args_schema: vec![],
        },
    );
    let around_comment = registry.register_text_object(
        "text-object:around-comment",
        "A comment -- the contiguous run of comment lines including the markers (`aC`).",
        TextObjectSpec {
            apply: Box::new(text_object_around_comment),
            args_schema: vec![],
        },
    );

    Builtins {
        word_forward,
        word_backward,
        word_end,
        first_non_blank,
        find_char_forward,
        find_char_backward,
        till_char_forward,
        till_char_backward,
        big_word_forward,
        big_word_backward,
        big_word_end,
        paragraph_forward,
        paragraph_backward,
        sentence_forward,
        sentence_backward,
        char_left,
        char_right,
        line_up,
        line_down,
        line_start,
        line_end,
        goto_first_line,
        goto_last_line,
        delete,
        change,
        yank,
        indent_left,
        indent_right,
        upper,
        lower,
        toggle_case,
        replace_char,
        inner_paragraph,
        around_paragraph,
        inner_sentence,
        around_sentence,
        inner_word,
        around_word,
        inner_quote_double,
        around_quote_double,
        inner_quote_single,
        around_quote_single,
        inner_quote_backtick,
        around_quote_backtick,
        inner_paren,
        around_paren,
        inner_bracket,
        around_bracket,
        inner_brace,
        around_brace,
        inner_tag,
        around_tag,
        inner_big_word,
        around_big_word,
        inner_angle,
        around_angle,
        inner_comment,
        around_comment,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Builtins {
    pub word_forward: MotionId,
    pub word_backward: MotionId,
    pub word_end: MotionId,
    pub first_non_blank: MotionId,
    pub find_char_forward: MotionId,
    pub find_char_backward: MotionId,
    pub till_char_forward: MotionId,
    pub till_char_backward: MotionId,
    pub big_word_forward: MotionId,
    pub big_word_backward: MotionId,
    pub big_word_end: MotionId,
    pub paragraph_forward: MotionId,
    pub paragraph_backward: MotionId,
    pub sentence_forward: MotionId,
    pub sentence_backward: MotionId,
    pub char_left: MotionId,
    pub char_right: MotionId,
    pub line_up: MotionId,
    pub line_down: MotionId,
    pub line_start: MotionId,
    pub line_end: MotionId,
    pub goto_first_line: MotionId,
    pub goto_last_line: MotionId,
    pub delete: OperatorId,
    pub change: OperatorId,
    pub yank: OperatorId,
    pub indent_left: OperatorId,
    pub indent_right: OperatorId,
    pub upper: OperatorId,
    pub lower: OperatorId,
    pub toggle_case: OperatorId,
    pub replace_char: OperatorId,
    pub inner_paragraph: TextObjectId,
    pub around_paragraph: TextObjectId,
    pub inner_sentence: TextObjectId,
    pub around_sentence: TextObjectId,
    pub inner_word: TextObjectId,
    pub around_word: TextObjectId,
    pub inner_quote_double: TextObjectId,
    pub around_quote_double: TextObjectId,
    pub inner_quote_single: TextObjectId,
    pub around_quote_single: TextObjectId,
    pub inner_quote_backtick: TextObjectId,
    pub around_quote_backtick: TextObjectId,
    pub inner_paren: TextObjectId,
    pub around_paren: TextObjectId,
    pub inner_bracket: TextObjectId,
    pub around_bracket: TextObjectId,
    pub inner_brace: TextObjectId,
    pub around_brace: TextObjectId,
    pub inner_tag: TextObjectId,
    pub around_tag: TextObjectId,
    pub inner_big_word: TextObjectId,
    pub around_big_word: TextObjectId,
    pub inner_angle: TextObjectId,
    pub around_angle: TextObjectId,
    pub inner_comment: TextObjectId,
    pub around_comment: TextObjectId,
}

// ---- Motion: word-forward ----

fn motion_word_forward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    // Advance `count` words. A word boundary here is the conventional vim
    // "word" (alphanumeric + underscore) -- not "WORD" (whitespace-delimited).
    // Phase 1 keeps the implementation simple: walk byte-wise over UTF-8
    // characters in the current line and the next ones until we've crossed
    // `count` word boundaries forward.
    let text = ctx.buffer.as_string();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();

    let mut line = ctx.from.line as usize;
    let mut byte = ctx.from.byte as usize;
    let count = ctx.count.get().max(1);

    for _ in 0..count {
        // Skip the current word's remaining word chars.
        while line < lines.len() {
            let l = lines[line];
            let bytes = l.as_bytes();
            if byte < bytes.len() && is_word_byte(bytes[byte]) {
                byte += 1;
            } else {
                break;
            }
            if byte >= bytes.len() {
                line += 1;
                byte = 0;
                break;
            }
        }
        // Skip non-word characters (whitespace, punctuation) until we hit the
        // next word start, or run out of buffer.
        loop {
            if line >= lines.len() {
                break;
            }
            let l = lines[line];
            let bytes = l.as_bytes();
            if byte >= bytes.len() {
                line += 1;
                byte = 0;
                continue;
            }
            if is_word_byte(bytes[byte]) {
                break;
            }
            byte += 1;
        }
    }

    // Clamp to last position if we walked off the end.
    let target = if line >= lines.len() {
        let last_line = lines.len().saturating_sub(1);
        let last_len = lines
            .get(last_line)
            .map(|l| l.trim_end_matches('\n').len())
            .unwrap_or(0);
        Position::new(last_line as u32, last_len as u32)
    } else {
        Position::new(line as u32, byte as u32)
    };

    Ok(MotionResult {
        target,
        linewise: false,
    })
}

fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_blank_byte(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

/// "WORD" boundary in vim: any non-whitespace run is a WORD; whitespace
/// (incl. newline) separates WORDs.
fn is_big_word_byte(b: u8) -> bool {
    !b.is_ascii_whitespace()
}

// ---- Motion: word-backward (vim's `b`) ----

fn motion_word_backward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;

    for _ in 0..count {
        if idx == 0 {
            break;
        }
        // Step one byte back, then skip non-word bytes (whitespace, newlines,
        // punctuation), then walk back through the word's body to its start.
        idx -= 1;
        while idx > 0 && !is_word_byte(bytes[idx]) {
            idx -= 1;
        }
        while idx > 0 && is_word_byte(bytes[idx - 1]) {
            idx -= 1;
        }
    }

    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult {
        target,
        linewise: false,
    })
}

// ---- Motion: word-end (vim's `e`) ----

fn motion_word_end(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let total = bytes.len();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;

    for _ in 0..count {
        if idx >= total.saturating_sub(1) {
            break;
        }
        // Step one byte forward, skip non-word bytes, then advance to the
        // last byte of the current word (the position where the next byte
        // would be non-word or past EOF).
        idx += 1;
        while idx < total && !is_word_byte(bytes[idx]) {
            idx += 1;
        }
        if idx >= total {
            idx = total.saturating_sub(1);
            break;
        }
        while idx + 1 < total && is_word_byte(bytes[idx + 1]) {
            idx += 1;
        }
    }

    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult {
        target,
        linewise: false,
    })
}

// ---- Paragraph motions (vim's `}`, `{`) ----
//
// A paragraph is a maximal run of non-blank lines. `}` lands on the
// next blank line at or after `cursor.line + 1`; `{` lands on the
// previous blank line. If no boundary exists, lands at end / start of
// buffer respectively.

fn line_is_blank(text: &str, line: u32) -> bool {
    text.split_inclusive('\n')
        .nth(line as usize)
        .map(|l| l.trim_end_matches('\n').trim().is_empty())
        .unwrap_or(true)
}

fn buffer_last_line(text: &str) -> u32 {
    let lc = text.split_inclusive('\n').count() as u32;
    if text.ends_with('\n') {
        lc.saturating_sub(2)
    } else {
        lc.saturating_sub(1)
    }
}

fn motion_paragraph_forward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let count = ctx.count.get().max(1);
    let last = buffer_last_line(&text);
    let mut line = ctx.from.line;
    for _ in 0..count {
        // Step forward at least one line.
        if line > last {
            line = last;
            break;
        }
        line = line.saturating_add(1);
        // Skip current paragraph (non-blank lines).
        while line <= last && !line_is_blank(&text, line) {
            line = line.saturating_add(1);
        }
        if line > last {
            line = last;
            break;
        }
    }
    Ok(MotionResult {
        target: Position::new(line, 0),
        linewise: false,
    })
}

fn motion_paragraph_backward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let count = ctx.count.get().max(1);
    let mut line = ctx.from.line;
    for _ in 0..count {
        if line == 0 {
            break;
        }
        line = line.saturating_sub(1);
        while line > 0 && !line_is_blank(&text, line) {
            line = line.saturating_sub(1);
        }
    }
    Ok(MotionResult {
        target: Position::new(line, 0),
        linewise: false,
    })
}

// ---- Sentence motions and text objects (vim's `)`, `(`, `is`, `as`) ----
//
// A sentence ends with `.`, `!`, or `?` followed by whitespace (or
// EOL). v1 doesn't honor vim's nuance around closing brackets / quotes
// after the punctuation; just the simple form.

fn is_sentence_end(b: u8) -> bool {
    matches!(b, b'.' | b'!' | b'?')
}

/// Find the byte offset of the start of the next sentence at or after
/// `from`. Returns the index of the first non-whitespace byte AFTER a
/// sentence-ending punctuation followed by whitespace (or the end of
/// the buffer).
fn next_sentence_start(bytes: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 1 < bytes.len() {
        if is_sentence_end(bytes[i]) && bytes[i + 1].is_ascii_whitespace() {
            // Skip the punctuation + whitespace.
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < bytes.len() {
                return Some(j);
            }
            return None;
        }
        i += 1;
    }
    None
}

/// Find the byte offset of the start of the previous sentence (the
/// non-whitespace byte after a previous sentence end). Returns 0 if
/// there's no earlier sentence boundary.
fn prev_sentence_start(bytes: &[u8], from: usize) -> usize {
    if from == 0 {
        return 0;
    }
    // Walk back to find a sentence-end + whitespace pattern.
    let mut i = from.saturating_sub(1);
    while i > 0 {
        if is_sentence_end(bytes[i]) && i + 1 < bytes.len() && bytes[i + 1].is_ascii_whitespace() {
            // Skip whitespace following the punctuation.
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < from {
                return j;
            }
        }
        i -= 1;
    }
    0
}

fn motion_sentence_forward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    for _ in 0..count {
        match next_sentence_start(bytes, idx) {
            Some(next) => idx = next,
            None => {
                idx = bytes.len().saturating_sub(1);
                break;
            }
        }
    }
    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult {
        target,
        linewise: false,
    })
}

fn motion_sentence_backward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    for _ in 0..count {
        let next = prev_sentence_start(bytes, idx);
        if next == idx {
            break;
        }
        idx = next;
    }
    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult {
        target,
        linewise: false,
    })
}

fn text_object_inner_sentence(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let start_byte = prev_sentence_start(bytes, cursor.saturating_add(1));
    let mut end_byte = cursor;
    while end_byte < bytes.len() {
        if is_sentence_end(bytes[end_byte]) {
            // Inner stops at the punctuation (exclusive).
            break;
        }
        end_byte += 1;
    }
    let start_pos = ctx
        .buffer
        .byte_to_position(start_byte)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(end_byte)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

fn text_object_around_sentence(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    let inner = text_object_inner_sentence(ctx)?;
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let inner_end_byte = ctx
        .buffer
        .position_to_byte(inner.end)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    // Around: include the trailing punctuation + whitespace.
    let mut end = inner_end_byte;
    if end < bytes.len() && is_sentence_end(bytes[end]) {
        end += 1;
    }
    while end < bytes.len() && bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    let end_pos = ctx
        .buffer
        .byte_to_position(end)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(inner.start, end_pos))
}

// ---- WORD motions (vim's W, B, E) ----

fn motion_big_word_forward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let total = bytes.len();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;

    for _ in 0..count {
        if idx >= total {
            break;
        }
        // Skip the current WORD's remaining non-whitespace bytes.
        while idx < total && is_big_word_byte(bytes[idx]) {
            idx += 1;
        }
        // Skip whitespace until the next WORD start.
        while idx < total && !is_big_word_byte(bytes[idx]) {
            idx += 1;
        }
    }
    if idx >= total {
        idx = total.saturating_sub(1);
    }
    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult {
        target,
        linewise: false,
    })
}

fn motion_big_word_backward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;

    for _ in 0..count {
        if idx == 0 {
            break;
        }
        idx -= 1;
        while idx > 0 && !is_big_word_byte(bytes[idx]) {
            idx -= 1;
        }
        while idx > 0 && is_big_word_byte(bytes[idx - 1]) {
            idx -= 1;
        }
    }
    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult {
        target,
        linewise: false,
    })
}

fn motion_big_word_end(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let total = bytes.len();
    let count = ctx.count.get().max(1);
    let mut idx = ctx
        .buffer
        .position_to_byte(ctx.from)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;

    for _ in 0..count {
        if idx >= total.saturating_sub(1) {
            break;
        }
        idx += 1;
        while idx < total && !is_big_word_byte(bytes[idx]) {
            idx += 1;
        }
        if idx >= total {
            idx = total.saturating_sub(1);
            break;
        }
        while idx + 1 < total && is_big_word_byte(bytes[idx + 1]) {
            idx += 1;
        }
    }
    let target = ctx
        .buffer
        .byte_to_position(idx)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(MotionResult {
        target,
        linewise: false,
    })
}

// ---- Motion: find-char / till-char (vim's f, F, t, T) ----
//
// Each takes `Args::Char(c)` and searches the current line. v1 ignores
// `count` (most users press `;` / `,` for repeat); count support lands
// later. Forward variants return the cursor's original line if the char
// is not found (matching vim's no-op behavior).

fn args_to_char(args: &crate::args::Args) -> Result<char, CommandError> {
    match args {
        crate::args::Args::Char(c) => Ok(*c),
        _ => Err(CommandError::InvalidArgs("f/F/t/T require Args::Char")),
    }
}

fn line_text(text: &str, line: u32) -> &str {
    text.split_inclusive('\n')
        .nth(line as usize)
        .map(|l| l.trim_end_matches('\n'))
        .unwrap_or("")
}

fn motion_find_char_forward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let needle = args_to_char(&ctx.args)?;
    let text = ctx.buffer.as_string();
    let line = line_text(&text, ctx.from.line);
    let bytes = line.as_bytes();
    let start = (ctx.from.byte as usize).saturating_add(1);
    let mut buf = [0u8; 4];
    let needle_bytes = needle.encode_utf8(&mut buf).as_bytes();
    let nlen = needle_bytes.len();
    let mut idx = start;
    while idx + nlen <= bytes.len() {
        if &bytes[idx..idx + nlen] == needle_bytes {
            return Ok(MotionResult {
                target: Position::new(ctx.from.line, idx as u32),
                linewise: false,
            });
        }
        idx += 1;
    }
    // No match -- vim no-ops.
    Ok(MotionResult {
        target: ctx.from,
        linewise: false,
    })
}

fn motion_find_char_backward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let needle = args_to_char(&ctx.args)?;
    let text = ctx.buffer.as_string();
    let line = line_text(&text, ctx.from.line);
    let bytes = line.as_bytes();
    let mut buf = [0u8; 4];
    let needle_bytes = needle.encode_utf8(&mut buf).as_bytes();
    let nlen = needle_bytes.len();
    if (ctx.from.byte as usize) < nlen {
        return Ok(MotionResult {
            target: ctx.from,
            linewise: false,
        });
    }
    let mut idx = (ctx.from.byte as usize) - nlen;
    loop {
        if idx + nlen <= bytes.len() && &bytes[idx..idx + nlen] == needle_bytes {
            return Ok(MotionResult {
                target: Position::new(ctx.from.line, idx as u32),
                linewise: false,
            });
        }
        if idx == 0 {
            break;
        }
        idx -= 1;
    }
    Ok(MotionResult {
        target: ctx.from,
        linewise: false,
    })
}

fn motion_till_char_forward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    // `t<c>`: identical to `f<c>` but the target is one byte before the match.
    let result = motion_find_char_forward(ctx)?;
    if result.target == ctx.from {
        return Ok(result);
    }
    let target_byte = result.target.byte.saturating_sub(1);
    Ok(MotionResult {
        target: Position::new(result.target.line, target_byte),
        linewise: false,
    })
}

fn motion_till_char_backward(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    // `T<c>`: identical to `F<c>` but the target is one byte after the match.
    let result = motion_find_char_backward(ctx)?;
    if result.target == ctx.from {
        return Ok(result);
    }
    let line_len = result
        .target
        .byte
        .saturating_add(args_to_char(&ctx.args)?.len_utf8() as u32);
    Ok(MotionResult {
        target: Position::new(result.target.line, line_len),
        linewise: false,
    })
}

// ---- Motion: first-non-blank (vim's `^`) ----

fn motion_first_non_blank(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let text = ctx.buffer.as_string();
    let line_text = text
        .split_inclusive('\n')
        .nth(ctx.from.line as usize)
        .map(|l| l.trim_end_matches('\n'))
        .unwrap_or("");
    let bytes = line_text.as_bytes();
    let mut col = 0usize;
    while col < bytes.len() && is_blank_byte(bytes[col]) {
        col += 1;
    }
    Ok(MotionResult {
        target: Position::new(ctx.from.line, col as u32),
        linewise: false,
    })
}

// ---- Motion: char-left ----

fn motion_char_left(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let mut pos = ctx.from;
    let text = ctx.buffer.as_string();
    let line = line_text(&text, pos.line);
    // Step by whole UTF-8 scalars, not bytes: a byte-step could land mid-glyph
    // (e.g. inside `│` U+2502, 3 bytes) and produce a delete range that panics
    // ropey on a non-char-boundary slice. `min` also snaps a mis-seeded
    // starting `pos.byte` onto the line before walking.
    let mut byte = (pos.byte as usize).min(line.len());
    for _ in 0..count {
        if byte == 0 {
            break;
        }
        byte -= 1;
        while byte > 0 && !line.is_char_boundary(byte) {
            byte -= 1;
        }
    }
    pos.byte = byte as u32;
    Ok(MotionResult {
        target: pos,
        linewise: false,
    })
}

// ---- Motion: char-right ----

fn motion_char_right(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let mut pos = ctx.from;
    let text = ctx.buffer.as_string();
    let line = line_text(&text, pos.line);
    let line_len = line.len();
    // Step by whole UTF-8 scalars, not bytes (see `motion_char_left`): a
    // byte-step over a multibyte glyph produced a delete range ending mid-char
    // and panicked ropey's `byte_slice` on the editor actor thread.
    let mut byte = (pos.byte as usize).min(line_len);
    for _ in 0..count {
        if byte >= line_len {
            break;
        }
        byte += 1;
        while byte < line_len && !line.is_char_boundary(byte) {
            byte += 1;
        }
    }
    pos.byte = byte as u32;
    Ok(MotionResult {
        target: pos,
        linewise: false,
    })
}

// ---- Motion: line-up / line-down ----

fn motion_line_up(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let line = ctx.from.line.saturating_sub(count);
    let max_byte = line_byte_len(ctx.buffer, line);
    let byte = ctx.from.byte.min(max_byte);
    Ok(MotionResult {
        target: Position::new(line, byte),
        linewise: false,
    })
}

fn motion_line_down(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let count = ctx.count.get().max(1);
    let last = last_addressable_line(ctx.buffer);
    let line = ctx.from.line.saturating_add(count).min(last);
    let max_byte = line_byte_len(ctx.buffer, line);
    let byte = ctx.from.byte.min(max_byte);
    Ok(MotionResult {
        target: Position::new(line, byte),
        linewise: false,
    })
}

// ---- Motion: line-start / line-end ----

fn motion_line_start(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    Ok(MotionResult {
        target: Position::new(ctx.from.line, 0),
        linewise: false,
    })
}

fn motion_line_end(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    let len = line_byte_len(ctx.buffer, ctx.from.line);
    Ok(MotionResult {
        target: Position::new(ctx.from.line, len),
        linewise: false,
    })
}

// ---- Motion: goto-first-line / goto-last-line ----

fn motion_goto_first_line(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    // `{count}gg` goes to line count (1-indexed); bare `gg` → line 1.
    let target_line = if ctx.has_explicit_count {
        let last = last_addressable_line(ctx.buffer);
        ctx.count.get().saturating_sub(1).min(last)
    } else {
        0
    };
    Ok(MotionResult {
        target: Position::new(target_line, 0),
        linewise: true,
    })
}

fn motion_goto_last_line(ctx: &MotionContext) -> Result<MotionResult, CommandError> {
    // `{count}G` goes to line count (1-indexed); bare `G` → last line.
    let last = last_addressable_line(ctx.buffer);
    let target_line = if ctx.has_explicit_count {
        ctx.count.get().saturating_sub(1).min(last)
    } else {
        last
    };
    Ok(MotionResult {
        target: Position::new(target_line, 0),
        linewise: true,
    })
}

// ---- Helpers ----

fn line_byte_len(buffer: &lattice_core::Buffer, line: u32) -> u32 {
    let s = buffer.as_string();
    s.split_inclusive('\n')
        .nth(line as usize)
        .map(|l| l.trim_end_matches('\n').len() as u32)
        .unwrap_or(0)
}

fn last_addressable_line(buffer: &lattice_core::Buffer) -> u32 {
    let lc = buffer.line_count();
    let s = buffer.as_string();
    if lc == 0 {
        0
    } else if s.ends_with('\n') {
        lc.saturating_sub(2)
    } else {
        lc.saturating_sub(1)
    }
}

/// Extend a linewise content range to consume the trailing
/// newline so `dd` / `Ndd` reduces the buffer's line count by
/// `count` (vim semantics). When the range ends at the buffer's
/// last addressable line (no trailing newline available),
/// consume the LEADING newline before `start_line` instead --
/// otherwise the previous line would acquire a phantom newline.
/// A whole-buffer range (start at line 0 AND end at last line)
/// has no surrounding newline to claim and passes through
/// unchanged.
///
/// Used by `dd` (edit + slice). `yy` doesn't use this -- a
/// linewise yank's content always ends with `\n` regardless of
/// whether the source line had a trailing newline, so yank
/// appends `\n` to the original slice rather than walking the
/// buffer.
fn extend_linewise_range(buffer: &lattice_core::Buffer, range: ProtoRange) -> ProtoRange {
    let last = last_addressable_line(buffer);
    let end_line = range.end.line;
    if end_line < last {
        ProtoRange::new(range.start, Position::new(end_line + 1, 0))
    } else if range.start.line > 0 {
        let prev_line = range.start.line - 1;
        let prev_len = line_byte_len(buffer, prev_line);
        ProtoRange::new(Position::new(prev_line, prev_len), range.end)
    } else {
        range
    }
}

// ---- Paragraph text objects (vim's `ip`, `ap`) ----

fn text_object_inner_paragraph(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let last_line = buffer_last_line(&text);
    let cursor_line = ctx.at.line.min(last_line);
    let blank_at_cursor = line_is_blank(&text, cursor_line);
    // Walk back to the start of the contiguous run-of-same-blank-status.
    let mut start = cursor_line;
    while start > 0 && line_is_blank(&text, start - 1) == blank_at_cursor {
        start -= 1;
    }
    // Walk forward to the end of the run.
    let mut end = cursor_line;
    while end < last_line && line_is_blank(&text, end + 1) == blank_at_cursor {
        end += 1;
    }
    let end_byte = line_byte_len(ctx.buffer, end);
    Ok(ProtoRange::new(
        Position::new(start, 0),
        Position::new(end, end_byte),
    ))
}

fn text_object_around_paragraph(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    // Inner paragraph + trailing blank lines (or leading if at end of buffer).
    let inner = text_object_inner_paragraph(ctx)?;
    let text = ctx.buffer.as_string();
    let last_line = buffer_last_line(&text);
    let mut end_line = inner.end.line;
    let blank_at_inner = line_is_blank(&text, end_line);
    if !blank_at_inner {
        while end_line < last_line && line_is_blank(&text, end_line + 1) {
            end_line += 1;
        }
        let end_byte = line_byte_len(ctx.buffer, end_line);
        Ok(ProtoRange::new(
            inner.start,
            Position::new(end_line, end_byte),
        ))
    } else {
        while end_line < last_line && !line_is_blank(&text, end_line + 1) {
            end_line += 1;
        }
        let end_byte = line_byte_len(ctx.buffer, end_line);
        Ok(ProtoRange::new(
            inner.start,
            Position::new(end_line, end_byte),
        ))
    }
}

// ---- N.1.6: comment text objects (aC / iC) ----
//
// Commentstring-driven, NOT tree-sitter (works for any language with a
// known line-comment leader, even without a parse tree). `aC` = the
// contiguous run of full comment lines (markers included); `iC` = the
// comment text (the first line's leader stripped). The leader comes from
// `ctx.comment_syntax.line`, populated by the host from the active
// buffer's language. No leader (plain buffer / unknown language) or a
// cursor not on a comment line -> empty range -> the paired operator
// no-ops, matching vim's "daC with no comment does nothing".

/// True when `line`'s first non-whitespace run starts with `leader`.
/// `///` and `//!` match because they begin with `//`.
fn line_is_comment(buffer: &lattice_core::Buffer, line: u32, leader: &str) -> bool {
    buffer
        .line(line)
        .map(|t| t.trim_start().starts_with(leader))
        .unwrap_or(false)
}

/// Byte offset on `line` of the comment CONTENT: just past the leader
/// and one optional following space. `    // foo` (leader `//`) returns
/// the offset of `foo`.
fn comment_content_start(buffer: &lattice_core::Buffer, line: u32, leader: &str) -> u32 {
    let Some(text) = buffer.line(line) else {
        return 0;
    };
    let indent = text.len() - text.trim_start().len();
    let mut off = indent + leader.len();
    if text.as_bytes().get(off) == Some(&b' ') {
        off += 1;
    }
    off.min(text.len()) as u32
}

/// Shared scan for `aC` / `iC`: expand over the contiguous run of full
/// comment lines containing the cursor. `aC` (`inner = false`) starts at
/// column 0; `iC` (`inner = true`) starts after the first line's leader.
/// Both end at the last comment line's content end (matching the
/// paragraph objects' linewise-ish shape).
fn comment_text_object(ctx: &TextObjectContext, inner: bool) -> Result<ProtoRange, CommandError> {
    let Some(leader) = ctx.comment_syntax.and_then(|c| c.line.as_deref()) else {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    };
    let line_count = ctx.buffer.line_count();
    if leader.is_empty() || line_count == 0 {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    }
    let cursor_line = ctx.at.line.min(line_count - 1);
    if !line_is_comment(ctx.buffer, cursor_line, leader) {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    }
    let mut start = cursor_line;
    while start > 0 && line_is_comment(ctx.buffer, start - 1, leader) {
        start -= 1;
    }
    let mut end = cursor_line;
    while end + 1 < line_count && line_is_comment(ctx.buffer, end + 1, leader) {
        end += 1;
    }
    let start_byte = if inner {
        comment_content_start(ctx.buffer, start, leader)
    } else {
        0
    };
    Ok(ProtoRange::new(
        Position::new(start, start_byte),
        Position::new(end, line_byte_len(ctx.buffer, end)),
    ))
}

fn text_object_around_comment(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    comment_text_object(ctx, false)
}

fn text_object_inner_comment(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    comment_text_object(ctx, true)
}

// ---- Text objects ----
//
// Each text object computes a `ProtoRange` covering the relevant span
// at the cursor. Vim's text objects are inclusive of their ends; the
// dispatcher's [start, end) range is constructed to match by extending
// `end` past the last byte we want included.

/// Inner-word run shared between `iw` and `iW`. The only difference
/// between the two is the byte classifier: `is_word_byte` for `iw`
/// (alphanum + underscore), `is_big_word_byte` for `iW` (any
/// non-whitespace run).
fn text_object_inner_word_class(
    ctx: &TextObjectContext,
    is_class: fn(u8) -> bool,
) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    if cursor >= bytes.len() || !is_class(bytes[cursor]) {
        // Not on a word -- range is the cursor position alone.
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    }
    let mut start = cursor;
    while start > 0 && is_class(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end + 1 < bytes.len() && is_class(bytes[end + 1]) {
        end += 1;
    }
    // [start, end] inclusive of word -> half-open is end + 1.
    let start_pos = ctx
        .buffer
        .byte_to_position(start)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(end + 1)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

/// Around-word run shared between `aw` and `aW`. Trailing whitespace
/// extension (or leading if none) is identical for both classes --
/// vim's `aw` and `aW` both stay on the line and use space/tab as
/// the whitespace to absorb.
fn text_object_around_word_class(
    ctx: &TextObjectContext,
    is_class: fn(u8) -> bool,
) -> Result<ProtoRange, CommandError> {
    let inner = text_object_inner_word_class(ctx, is_class)?;
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let inner_end_byte = ctx
        .buffer
        .position_to_byte(inner.end)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let mut end = inner_end_byte;
    while end < bytes.len() && (bytes[end] == b' ' || bytes[end] == b'\t') {
        end += 1;
    }
    if end == inner_end_byte {
        // No trailing whitespace -- try leading.
        let inner_start_byte = ctx
            .buffer
            .position_to_byte(inner.start)
            .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
        let mut start = inner_start_byte;
        while start > 0 && (bytes[start - 1] == b' ' || bytes[start - 1] == b'\t') {
            start -= 1;
        }
        let start_pos = ctx
            .buffer
            .byte_to_position(start)
            .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
        Ok(ProtoRange::new(start_pos, inner.end))
    } else {
        let end_pos = ctx
            .buffer
            .byte_to_position(end)
            .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
        Ok(ProtoRange::new(inner.start, end_pos))
    }
}

fn text_object_inner_word(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    text_object_inner_word_class(ctx, is_word_byte)
}

fn text_object_around_word(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    text_object_around_word_class(ctx, is_word_byte)
}

fn text_object_inner_big_word(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    text_object_inner_word_class(ctx, is_big_word_byte)
}

fn text_object_around_big_word(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    text_object_around_word_class(ctx, is_big_word_byte)
}

fn find_quote_pair(bytes: &[u8], cursor: usize, q: u8) -> Option<(usize, usize)> {
    // Restrict to the current line: walk back to start-of-line, then
    // forward, collecting quote positions; pair them naively (no escape
    // handling for v1).
    let mut line_start = cursor;
    while line_start > 0 && bytes[line_start - 1] != b'\n' {
        line_start -= 1;
    }
    let mut line_end = cursor;
    while line_end < bytes.len() && bytes[line_end] != b'\n' {
        line_end += 1;
    }
    let mut quotes = Vec::new();
    for (offset, &b) in bytes[line_start..line_end].iter().enumerate() {
        if b == q {
            quotes.push(line_start + offset);
        }
    }
    // Find a pair where the cursor is inside (or on a quote).
    let mut i = 0;
    while i + 1 < quotes.len() {
        let (l, r) = (quotes[i], quotes[i + 1]);
        if cursor >= l && cursor <= r {
            return Some((l, r));
        }
        i += 2;
    }
    None
}

fn text_object_inner_quote(ctx: &TextObjectContext, q: char) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    if !q.is_ascii() {
        return Err(CommandError::InvalidArgs("non-ASCII quote not supported"));
    }
    let q_byte = q as u8;
    let Some((l, r)) = find_quote_pair(bytes, cursor, q_byte) else {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    };
    // Inner: between the quotes, exclusive of both.
    let start_pos = ctx
        .buffer
        .byte_to_position(l + 1)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(r)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

fn text_object_around_quote(ctx: &TextObjectContext, q: char) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    if !q.is_ascii() {
        return Err(CommandError::InvalidArgs("non-ASCII quote not supported"));
    }
    let q_byte = q as u8;
    let Some((l, r)) = find_quote_pair(bytes, cursor, q_byte) else {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    };
    let start_pos = ctx
        .buffer
        .byte_to_position(l)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(r + 1)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

fn find_bracket_pair(bytes: &[u8], cursor: usize, open: u8, close: u8) -> Option<(usize, usize)> {
    if bytes.is_empty() {
        return None;
    }
    // Walk left to find the most recent unmatched `open`.
    let mut depth = 0i32;
    let mut i = cursor.min(bytes.len() - 1);
    let open_pos = loop {
        let b = bytes[i];
        if b == close && i != cursor {
            // Same-position cursor on a close-bracket counts as "inside",
            // not as an additional level.
            depth += 1;
        } else if b == open {
            if depth == 0 {
                break i;
            }
            depth -= 1;
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    };
    // Walk right from open_pos+1 to find the matching close.
    let mut depth = 0i32;
    let mut j = open_pos + 1;
    while j < bytes.len() {
        let b = bytes[j];
        if b == open {
            depth += 1;
        } else if b == close {
            if depth == 0 {
                return Some((open_pos, j));
            }
            depth -= 1;
        }
        j += 1;
    }
    None
}

fn text_object_inner_brackets(
    ctx: &TextObjectContext,
    open: char,
    close: char,
) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let Some((l, r)) = find_bracket_pair(bytes, cursor, open as u8, close as u8) else {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    };
    let start_pos = ctx
        .buffer
        .byte_to_position(l + 1)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(r)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

/// Find the nearest XML/HTML tag pair enclosing the cursor. Returns
/// `(open_start, open_end, close_start, close_end)` byte offsets where
/// `open_*` covers `<tag...>` and `close_*` covers `</tag>`. v1 only
/// supports same-line tag pairs and ignores attributes.
fn find_enclosing_tag(bytes: &[u8], cursor: usize) -> Option<(usize, usize, usize, usize)> {
    // Walk back to the nearest unmatched '<...>' open tag.
    let mut i = cursor.min(bytes.len().saturating_sub(1));
    let mut depth = 0i32;
    let open_start;
    loop {
        if bytes[i] == b'>' && i > 0 && bytes[i - 1] != b'/' {
            // a `>` closing some tag (open or self-close)
            // We want to track whether it's a close-tag or open-tag.
            // For simplicity we just look back at the matching `<`.
            // Skip for now; the depth tracking happens at `<`.
        }
        if bytes[i] == b'<' {
            // Look ahead to determine if this is open or close tag.
            if i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                depth += 1;
            } else if depth == 0 {
                open_start = i;
                break;
            } else {
                depth -= 1;
            }
        }
        if i == 0 {
            return None;
        }
        i -= 1;
    }
    // Find the end of the open tag.
    let mut open_end = open_start + 1;
    while open_end < bytes.len() && bytes[open_end] != b'>' {
        open_end += 1;
    }
    if open_end >= bytes.len() {
        return None;
    }
    // Extract tag name.
    let mut name_end = open_start + 1;
    while name_end < open_end
        && bytes[name_end] != b' '
        && bytes[name_end] != b'>'
        && bytes[name_end] != b'/'
    {
        name_end += 1;
    }
    let name = &bytes[open_start + 1..name_end];
    if name.is_empty() {
        return None;
    }
    // Walk forward from `open_end + 1` to find matching `</name>`.
    let close_marker: Vec<u8> = {
        let mut v = Vec::with_capacity(name.len() + 3);
        v.push(b'<');
        v.push(b'/');
        v.extend_from_slice(name);
        v.push(b'>');
        v
    };
    let mut j = open_end + 1;
    while j + close_marker.len() <= bytes.len() {
        if &bytes[j..j + close_marker.len()] == close_marker.as_slice() {
            return Some((open_start, open_end + 1, j, j + close_marker.len()));
        }
        j += 1;
    }
    None
}

fn text_object_inner_tag(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let Some((_, open_end, close_start, _)) = find_enclosing_tag(bytes, cursor) else {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    };
    let start_pos = ctx
        .buffer
        .byte_to_position(open_end)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(close_start)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

fn text_object_around_tag(ctx: &TextObjectContext) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let Some((open_start, _, _, close_end)) = find_enclosing_tag(bytes, cursor) else {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    };
    let start_pos = ctx
        .buffer
        .byte_to_position(open_start)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(close_end)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

fn text_object_around_brackets(
    ctx: &TextObjectContext,
    open: char,
    close: char,
) -> Result<ProtoRange, CommandError> {
    let text = ctx.buffer.as_string();
    let bytes = text.as_bytes();
    let cursor = ctx
        .buffer
        .position_to_byte(ctx.at)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let Some((l, r)) = find_bracket_pair(bytes, cursor, open as u8, close as u8) else {
        return Ok(ProtoRange::new(ctx.at, ctx.at));
    };
    let start_pos = ctx
        .buffer
        .byte_to_position(l)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    let end_pos = ctx
        .buffer
        .byte_to_position(r + 1)
        .map_err(|_| CommandError::InvalidArgs("position out of bounds"))?;
    Ok(ProtoRange::new(start_pos, end_pos))
}

// ---- Operator: delete ----
//
// Vim's `d` -- delete the bytes covered by the target range. Also yanks
// the deleted content into the unnamed register (vim's behavior). The
// returned Effect is a `Many` so callers see both the buffer mutation
// and the yank in a single dispatched commit.

fn operator_delete(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    // Slice 8.i.4.g: vim's `dd` / `Ndd` consumes the line(s) AND
    // the trailing newline so the line count drops by `count`.
    // Pure-content delete (charwise / non-linewise visual)
    // continues to leave the line structure intact, matching
    // `dw` / `d$` semantics.
    let edit_range = if ctx.linewise {
        extend_linewise_range(ctx.document.buffer(), ctx.range)
    } else {
        ctx.range
    };
    // Yank content always ends with `\n` for linewise -- the
    // register kind drives paste behaviour, but the content
    // string itself follows vim's clipboard convention. The
    // edit consumes the same range, so this keeps yank-on-
    // delete and the actual edit aligned.
    let yanked = if ctx.linewise {
        let mut s = ctx.document.buffer().slice(ctx.range)?;
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s
    } else {
        ctx.document.buffer().slice(ctx.range)?
    };
    let edit = Edit::delete(edit_range);
    let applied = ctx.document.apply_edit(edit)?;
    let yank_kind = if ctx.linewise {
        YankKind::Linewise
    } else {
        YankKind::Charwise
    };
    Ok(Effect::Many(vec![
        Effect::Edits(vec![applied]),
        Effect::Yank {
            register: ctx.register,
            content: yanked,
            kind: yank_kind,
            // Delete populates registers but is NOT an explicit yank —
            // it must not mirror to the system clipboard (yank-only rule).
            explicit_yank: false,
        },
    ]))
}

// ---- Operator: change ----
//
// Vim's `c` -- delete the target range, yank it (matching delete's
// behavior), and enter Insert mode at the deleted position. Composition
// expressed in the Effect, not a flag on the spec.

fn operator_change(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        // No-op change still drops into Insert (vim does this for `cw` on
        // an empty buffer / past EOF).
        return Ok(Effect::EnterMode(crate::modal::ModalState::Insert));
    }
    let yanked = ctx.document.buffer().slice(ctx.range)?;
    let edit = Edit::delete(ctx.range);
    let applied = ctx.document.apply_edit(edit)?;
    let yank_kind = if ctx.linewise {
        YankKind::Linewise
    } else {
        YankKind::Charwise
    };
    Ok(Effect::Many(vec![
        Effect::Edits(vec![applied]),
        Effect::Yank {
            register: ctx.register,
            content: yanked,
            kind: yank_kind,
            // Change deletes + yanks the range like delete — not an
            // explicit yank, so no clipboard mirror (yank-only rule).
            explicit_yank: false,
        },
        Effect::EnterMode(crate::modal::ModalState::Insert),
    ]))
}

// ---- Operator: replace-char (vim's `r{char}` and Visual `r`) ----
//
// Overwrite each non-newline char in the target range with the captured
// replacement char, producing one `Effect::Edits` -- so both renderers
// pick it up through the standard edit path with no per-renderer arm.
//
// One body serves both entry points:
//
// * Normal `Nr{char}`: the keymap binds the range to `char_right x count`
//   (pre-clamped to the current line, exactly as `x` = delete+char_right).
//   Vim's "no-op when fewer than N chars remain" falls out of the
//   `n_chars < count` guard -- the clamped range simply holds fewer chars
//   than the requested count. Stays in Normal.
// * Visual `r{char}`: dispatched with `Range::Selection` (count defaults
//   to 1), so the guard degenerates to "no-op on an empty span" and every
//   selected char is overwritten. Blockwise routes per-row via
//   `blockwise_per_row`. The host auto-exits Visual after any operator
//   (like `d` / `c` / `y`), so no explicit mode transition is emitted.
//
// Newlines inside the range are preserved, so a charwise / linewise
// multi-line selection keeps its line structure (vim replaces characters,
// never line breaks).
fn operator_replace_char(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    let replacement = match &ctx.args {
        crate::args::Args::Char(c) => *c,
        _ => return Err(CommandError::InvalidArgs("r requires Args::Char")),
    };
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    let text = ctx.document.buffer().slice(ctx.range)?;
    let n_chars = text.chars().filter(|c| *c != '\n').count() as u32;
    // Vim's `Nr{char}` requires N characters on the line; a too-large
    // count is a no-op (vim bells). For Visual the count is 1, so any
    // non-empty span replaces.
    if n_chars < ctx.count.get().max(1) {
        return Ok(Effect::None);
    }
    let replaced: String = text
        .chars()
        .map(|c| if c == '\n' { '\n' } else { replacement })
        .collect();
    let applied = ctx
        .document
        .apply_edit(Edit::replace(ctx.range, replaced))?;
    Ok(Effect::Edits(vec![applied]))
}

// ---- Operator: yank ----
//
// Vim's `y` -- copy the target range into the unnamed register without
// touching the buffer.

fn operator_yank(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    // Slice 8.i.4.g: vim's `yy` / `Nyy` yanks line(s) WITH the
    // trailing newline so the register content can be pasted
    // linewise without splicing newlines in by hand. Append `\n`
    // when missing rather than walking the buffer for the
    // trailing newline -- linewise yank on the last line still
    // gets a trailing `\n` to match the canonical convention.
    let content = if ctx.linewise {
        let mut s = ctx.document.buffer().slice(ctx.range)?;
        if !s.ends_with('\n') {
            s.push('\n');
        }
        s
    } else {
        ctx.document.buffer().slice(ctx.range)?
    };
    let kind = if ctx.linewise {
        YankKind::Linewise
    } else {
        YankKind::Charwise
    };
    // Yank ALSO populates `"0` per vim semantics. Two Effect::Yanks --
    // first the requested register (which the App layer's store_yank
    // also mirrors into the unnamed register), then "0.
    Ok(Effect::Many(vec![
        Effect::Yank {
            register: ctx.register,
            content: content.clone(),
            kind,
            // The one explicit-yank write: eligible for the clipboard
            // mirror under the `clipboard` option.
            explicit_yank: true,
        },
        Effect::Yank {
            register: crate::register::Register::Numbered(0),
            content,
            kind,
            // The `"0` mirror is a register-only bookkeeping write; the
            // primary write above already handled the clipboard.
            explicit_yank: false,
        },
    ]))
}

// ---- Indent operators (>, <) ----

const INDENT_UNIT: &str = "    ";

/// Vim's `>` -- prepend INDENT_UNIT to each line in the range.
///
/// The whole indent operation lands as a single undo unit -- we
/// build the per-line edits up front and commit via
/// `apply_edit_batch` so `2>>` / visual-`>` over N lines is one
/// `u` away from being undone, not N.
fn operator_indent_right(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    let first_line = ctx.range.start.line;
    let last_line = ctx.range.end.line;
    // Bottom-up edit construction so earlier inserts don't shift
    // later positions when the buffer applies the batch in order.
    let edits: Vec<Edit> = (first_line..=last_line)
        .rev()
        .map(|line| Edit::insert(Position::new(line, 0), INDENT_UNIT))
        .collect();
    let applied = ctx.document.apply_edit_batch(edits)?;
    // Restore top-down ordering for the returned AppliedEdits so
    // downstream `handle_edits` lands the cursor on the topmost
    // line, matching the previous one-edit-at-a-time behavior.
    let mut applied = applied;
    applied.reverse();
    Ok(Effect::Edits(applied))
}

/// Vim's `<` -- strip up to INDENT_UNIT bytes of leading whitespace from
/// each line in the range. A leading tab also counts as one indent unit
/// for v1. Whole operation lands as one undo unit (see
/// [`operator_indent_right`]).
fn operator_indent_left(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    let first_line = ctx.range.start.line;
    let last_line = ctx.range.end.line;
    let buffer_text = ctx.document.text();
    let lines: Vec<&str> = buffer_text.split_inclusive('\n').collect();
    let mut edits: Vec<Edit> = Vec::new();
    // Bottom-up so earlier deletes don't shift later positions
    // when the batch applies in order.
    for line in (first_line..=last_line).rev() {
        let line_text = lines
            .get(line as usize)
            .map(|l| l.trim_end_matches('\n'))
            .unwrap_or("");
        let bytes = line_text.as_bytes();
        let mut strip = 0usize;
        if !bytes.is_empty() && bytes[0] == b'\t' {
            strip = 1;
        } else {
            while strip < INDENT_UNIT.len() && strip < bytes.len() && bytes[strip] == b' ' {
                strip += 1;
            }
        }
        if strip == 0 {
            continue;
        }
        let range = lattice_protocol::position::Range::new(
            Position::new(line, 0),
            Position::new(line, strip as u32),
        );
        edits.push(Edit::delete(range));
    }
    if edits.is_empty() {
        return Ok(Effect::None);
    }
    let applied = ctx.document.apply_edit_batch(edits)?;
    let mut applied = applied;
    applied.reverse();
    Ok(Effect::Edits(applied))
}

// ---- Case operators (gU, gu, g~) ----

fn case_transform_in_range<F: Fn(u8) -> u8>(
    ctx: &mut OperatorContext,
    map: F,
) -> Result<Effect, CommandError> {
    if ctx.range.is_empty() {
        return Ok(Effect::None);
    }
    let original = ctx.document.buffer().slice(ctx.range)?;
    let transformed: String = original
        .as_bytes()
        .iter()
        .map(|&b| map(b) as char)
        .collect();
    if transformed == original {
        return Ok(Effect::None);
    }
    let edit = Edit::replace(ctx.range, &transformed);
    let applied = ctx.document.apply_edit(edit)?;
    Ok(Effect::Edits(vec![applied]))
}

fn operator_upper(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    case_transform_in_range(ctx, |b| b.to_ascii_uppercase())
}

fn operator_lower(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    case_transform_in_range(ctx, |b| b.to_ascii_lowercase())
}

fn operator_toggle_case(ctx: &mut OperatorContext) -> Result<Effect, CommandError> {
    case_transform_in_range(ctx, |b| match b {
        b'a'..=b'z' => b - 32,
        b'A'..=b'Z' => b + 32,
        other => other,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::CancellationToken;
    use crate::command::{CommandInvocation, Count};
    use crate::dispatcher::execute;
    use crate::effect::Effect;
    use crate::target::Target;
    use lattice_core::Document;

    fn fixture(text: &str) -> (CommandRegistry, Builtins, Document) {
        let mut r = CommandRegistry::new();
        let b = populate(&mut r);
        let d = Document::from_text(text);
        (r, b, d)
    }

    #[test]
    fn comment_object_around_keeps_markers_inner_strips_leader() {
        use crate::dispatcher::execute_with_env;
        use crate::registry::{CommentSyntax, TextObjectEnv};
        // line 1 `    // first`, line 2 `    // second` form one block.
        let src = "fn f() {\n    // first\n    // second\n    let x = 1;\n}\n";
        let (registry, b, mut doc) = fixture(src);
        let cs = CommentSyntax {
            line: Some("//".to_string()),
            block: None,
        };
        let cancel = CancellationToken::never();
        let cursor = Position::new(1, 8); // on `// first`
        // The yank operator yields `Effect::Many([Yank-unnamed,
        // Yank-numbered])`; pull the content out of whichever shape.
        fn yanked(eff: Effect) -> String {
            match eff {
                Effect::Yank { content, .. } => content,
                Effect::Many(effs) => effs
                    .into_iter()
                    .find_map(|e| match e {
                        Effect::Yank { content, .. } => Some(content),
                        _ => None,
                    })
                    .expect("a Yank inside Many"),
                other => panic!("expected a Yank, got {other:?}"),
            }
        }

        // yaC -> the whole comment block (lines 1-2), markers included.
        let inv = CommandInvocation::of(b.yank.0).with_target(Target::TextObject(
            b.around_comment,
            crate::args::Args::None,
        ));
        let eff = execute_with_env(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            cursor,
            inv,
            &cancel,
            TextObjectEnv {
                scope_resolver: None,
                comment_syntax: Some(&cs),
            },
        )
        .unwrap();
        let content = yanked(eff);
        assert!(
            content.contains("// first") && content.contains("// second"),
            "aC yanks the comment block including markers, got {content:?}"
        );

        // yiC -> the comment text; the first line's leader is stripped.
        let inv = CommandInvocation::of(b.yank.0)
            .with_target(Target::TextObject(b.inner_comment, crate::args::Args::None));
        let eff = execute_with_env(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            cursor,
            inv,
            &cancel,
            TextObjectEnv {
                scope_resolver: None,
                comment_syntax: Some(&cs),
            },
        )
        .unwrap();
        let content = yanked(eff);
        assert!(
            content.starts_with("first"),
            "iC strips the first line's leader (no `//`), got {content:?}"
        );
    }

    #[test]
    fn comment_object_no_leader_is_a_noop() {
        // No comment_syntax in the env -> empty range -> the operator
        // no-ops (vim's `daC` with no comment does nothing).
        use crate::dispatcher::execute_with_env;
        use crate::registry::TextObjectEnv;
        let src = "// a comment\n";
        let (registry, b, mut doc) = fixture(src);
        let cancel = CancellationToken::never();
        let inv = CommandInvocation::of(b.delete.0).with_target(Target::TextObject(
            b.around_comment,
            crate::args::Args::None,
        ));
        let eff = execute_with_env(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 3),
            inv,
            &cancel,
            TextObjectEnv::default(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::None), "no leader -> no-op");
        assert_eq!(doc.text(), src, "document unchanged");
    }

    #[test]
    fn populate_registers_known_builtins_by_name() {
        let mut r = CommandRegistry::new();
        let _ = populate(&mut r);
        assert!(r.lookup_by_name("motion:word-forward").is_some());
        assert!(r.lookup_by_name("operator:delete").is_some());
    }

    #[test]
    fn pre_flipped_token_short_circuits_dispatch() {
        // DESIGN.md §5.2.5: an evaluator that observes a flipped
        // token returns Cancelled and commits no Effect.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.word_forward.0);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let result = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &cancel,
        );
        match result {
            Err(CommandError::Cancelled) => {}
            other => panic!("expected Cancelled, got {other:?}"),
        }
        // Document state unchanged: no edits land.
        assert_eq!(doc.text(), "hello world");
    }

    #[test]
    fn fresh_token_does_not_short_circuit() {
        // Sanity-check the negative case: a fresh token leaves the
        // dispatcher's behaviour identical to the no-token path.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.word_forward.0);
        let cancel = CancellationToken::new();
        let result = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &cancel,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn word_forward_advances_to_next_word_start() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.word_forward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                assert_eq!(s.primary().head, Position::new(0, 6));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_forward_with_count_advances_by_count() {
        let (registry, b, mut doc) = fixture("one two three four");
        let inv = CommandInvocation::of(b.word_forward.0).with_count(Count(2));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // "one two THREE four" -- two words forward from origin lands
                // at the start of "three" (byte 8).
                assert_eq!(s.primary().head, Position::new(0, 8));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_forward_across_newline() {
        let (registry, b, mut doc) = fixture("hello\nworld");
        let inv = CommandInvocation::of(b.word_forward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                assert_eq!(s.primary().head, Position::new(1, 0));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn delete_with_word_forward_target_dw_semantics() {
        // The classic `dw`: from origin, delete to next word start.
        // Delete now also yanks the deleted content into the unnamed
        // register, so the Effect is `Many([Edits, Yank])`.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    Effect::Edits(applied) => {
                        assert_eq!(applied.len(), 1);
                        assert_eq!(applied[0].replaced_text, "hello ");
                    }
                    other => panic!("expected Edits at [0], got {other:?}"),
                }
                match &parts[1] {
                    Effect::Yank { content, kind, .. } => {
                        assert_eq!(content, "hello ");
                        assert_eq!(*kind, YankKind::Charwise);
                    }
                    other => panic!("expected Yank at [1], got {other:?}"),
                }
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "world");
    }

    #[test]
    fn delete_with_explicit_whole_range_deletes_buffer_and_yanks_linewise() {
        let (registry, b, mut doc) = fixture("a\nb\nc");
        let inv = CommandInvocation::of(b.delete.0).with_range(crate::range::Range::Whole);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => {
                assert!(matches!(parts[0], Effect::Edits(_)));
                match &parts[1] {
                    Effect::Yank { kind, .. } => assert_eq!(*kind, YankKind::Linewise),
                    other => panic!("expected Yank at [1], got {other:?}"),
                }
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn delete_with_current_line_range_clears_just_that_line() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(b.delete.0).with_range(crate::range::Range::CurrentLine);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Slice 8.i.4.g: `dd` consumes BBB AND its trailing
        // newline so the line count drops by 1 (vim semantics).
        assert_eq!(doc.text(), "aaa\nccc");
    }

    #[test]
    fn unknown_command_id_errors() {
        let (registry, _, mut doc) = fixture("abc");
        let bogus = lattice_protocol::ids::CommandId::new(99_999);
        let inv = CommandInvocation::of(bogus);
        assert!(matches!(
            execute(
                &registry,
                &mut doc,
                lattice_core::BufferId(0),
                Position::ZERO,
                inv,
                &CancellationToken::never()
            ),
            Err(CommandError::UnknownCommand)
        ));
    }

    #[test]
    fn operator_without_target_or_range_errors() {
        let (registry, b, mut doc) = fixture("abc");
        let inv = CommandInvocation::of(b.delete.0);
        assert!(matches!(
            execute(
                &registry,
                &mut doc,
                lattice_core::BufferId(0),
                Position::ZERO,
                inv,
                &CancellationToken::never()
            ),
            Err(CommandError::MissingTarget)
        ));
    }

    #[test]
    fn char_left_at_origin_stays_put() {
        let (registry, b, mut doc) = fixture("abc");
        let inv = CommandInvocation::of(b.char_left.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn char_right_advances_one_byte() {
        let (registry, b, mut doc) = fixture("abc");
        let inv = CommandInvocation::of(b.char_right.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 1)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn char_right_advances_whole_multibyte_scalar() {
        // `│` (U+2502) is 3 bytes. char_right must land on the next scalar
        // boundary (byte 3), never mid-glyph (byte 1), or a subsequent delete
        // panics ropey on a non-char-boundary slice.
        let (registry, b, mut doc) = fixture("│x");
        let inv = CommandInvocation::of(b.char_right.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 3)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn char_left_retreats_whole_multibyte_scalar() {
        // Cursor just after `│` (byte 3); char_left must return to byte 0,
        // not byte 2 (mid-glyph).
        let (registry, b, mut doc) = fixture("│x");
        let inv = CommandInvocation::of(b.char_left.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 3),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn char_right_at_end_of_line_stays_put() {
        let (registry, b, mut doc) = fixture("ab");
        let inv = CommandInvocation::of(b.char_right.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 2),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_down_moves_one_line_and_clamps_byte() {
        let (registry, b, mut doc) = fixture("hello\nhi");
        let inv = CommandInvocation::of(b.line_down.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(1, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_up_at_top_stays_put() {
        let (registry, b, mut doc) = fixture("a\nb");
        let inv = CommandInvocation::of(b.line_up.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_start_resets_byte_to_zero() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.line_start.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 7),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 0)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn line_end_jumps_to_line_byte_length() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.line_end.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 11)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn goto_first_line_returns_to_origin() {
        let (registry, b, mut doc) = fixture("a\nb\nc");
        let inv = CommandInvocation::of(b.goto_first_line.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(2, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn goto_last_line_jumps_to_last_addressable_line() {
        let (registry, b, mut doc) = fixture("a\nb\nc");
        let inv = CommandInvocation::of(b.goto_last_line.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(2, 0)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn goto_last_line_with_count_goes_to_specific_line() {
        // `3G` on a 5-line buffer → line 3 (1-indexed → row 2)
        let (registry, b, mut doc) = fixture("a\nb\nc\nd\ne");
        let inv = CommandInvocation::of(b.goto_last_line.0).with_count(Count(3));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(2, 0)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn goto_first_line_with_count_goes_to_specific_line() {
        // `3gg` on a 5-line buffer → line 3 (1-indexed → row 2)
        let (registry, b, mut doc) = fixture("a\nb\nc\nd\ne");
        let inv = CommandInvocation::of(b.goto_first_line.0).with_count(Count(3));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(4, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(2, 0)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn x_semantics_via_delete_with_char_right_target() {
        // Vim's `x`: delete the char under the cursor (and yank it).
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.char_right, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => {
                assert!(matches!(parts[0], Effect::Edits(_)));
                assert!(matches!(parts[1], Effect::Yank { .. }));
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "ello");
    }

    // ---- word-backward (b) ----

    #[test]
    fn word_backward_from_mid_word_lands_at_start_of_word() {
        let (registry, b, mut doc) = fixture("hello world");
        // Cursor on 'r' of "world" (byte 8) -- vim's `b` lands on 'w' (byte 6).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 8),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 6)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_from_start_of_word_lands_at_start_of_previous_word() {
        let (registry, b, mut doc) = fixture("one two three");
        // Cursor on 't' of "three" (byte 8). `b` -> 't' of "two" (byte 4).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 8),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_at_origin_stays_put() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_crosses_newlines() {
        let (registry, b, mut doc) = fixture("foo\nbar");
        // Cursor on 'b' (line 1 byte 0). `b` -> 'f' (line 0 byte 0).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(1, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_with_count_jumps_count_words() {
        let (registry, b, mut doc) = fixture("one two three four");
        // Cursor on 'f' of "four" (byte 14). `2b` -> 't' of "two" (byte 4).
        let inv = CommandInvocation::of(b.word_backward.0).with_count(Count(2));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 14),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_backward_skips_punctuation_as_non_word() {
        let (registry, b, mut doc) = fixture("alpha, beta");
        // Cursor on 'b' of "beta" (byte 7). `b` -> 'a' of "alpha" (byte 0).
        let inv = CommandInvocation::of(b.word_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 7),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- word-end (e) ----

    #[test]
    fn word_end_from_start_of_word_lands_at_last_byte_of_word() {
        let (registry, b, mut doc) = fixture("hello world");
        // From 'h' (byte 0) `e` -> 'o' of "hello" (byte 4).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_from_end_of_word_lands_at_end_of_next_word() {
        let (registry, b, mut doc) = fixture("hello world");
        // From 'o' of "hello" (byte 4) `e` -> 'd' of "world" (byte 10).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 4),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 10)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_at_buffer_end_stays_put() {
        let (registry, b, mut doc) = fixture("hi");
        // From 'i' (byte 1) `e` -> stays at byte 1 (no further word).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 1),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 1)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_crosses_newlines() {
        let (registry, b, mut doc) = fixture("foo\nbar");
        // From 'o' of "foo" (line 0 byte 2) `e` -> 'r' of "bar" (line 1 byte 2).
        let inv = CommandInvocation::of(b.word_end.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 2),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(1, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn word_end_with_count_advances_by_count() {
        let (registry, b, mut doc) = fixture("one two three four");
        // From 'o' (byte 0) `2e` -> end of "two" = 'o' of "two" (byte 6).
        let inv = CommandInvocation::of(b.word_end.0).with_count(Count(2));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 6)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- first-non-blank (^) ----

    #[test]
    fn first_non_blank_skips_leading_spaces() {
        let (registry, b, mut doc) = fixture("    hello");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_skips_leading_tabs() {
        let (registry, b, mut doc) = fixture("\t\thello");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_on_already_non_blank_line_returns_zero() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 3),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_on_blank_only_line_returns_end() {
        let (registry, b, mut doc) = fixture("    ");
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // No non-blank chars; cursor lands at end of line (byte 4).
                assert_eq!(s.primary().head, Position::new(0, 4));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn first_non_blank_respects_current_line() {
        let (registry, b, mut doc) = fixture("a\n  bc");
        // Cursor on line 1; first non-blank is at byte 2 of line 1.
        let inv = CommandInvocation::of(b.first_non_blank.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(1, 4),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(1, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- delete + new motions composition ----

    #[test]
    fn delete_with_word_backward_target_db_semantics() {
        // `db` from past-EOL deletes the last word: word_backward from byte 11
        // lands at byte 6 (start of "world"); the [6, 11) range covers "world".
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_backward, crate::args::Args::None));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 11),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello ");
    }

    // ---- Indent operators (>, <) ----

    #[test]
    fn indent_right_current_line_prepends_four_spaces() {
        let (registry, b, mut doc) = fixture("hello");
        let inv =
            CommandInvocation::of(b.indent_right.0).with_range(crate::range::Range::CurrentLine);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "    hello");
    }

    #[test]
    fn indent_left_current_line_strips_four_spaces() {
        let (registry, b, mut doc) = fixture("    hello");
        let inv =
            CommandInvocation::of(b.indent_left.0).with_range(crate::range::Range::CurrentLine);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello");
    }

    #[test]
    fn indent_left_strips_partial_indent() {
        let (registry, b, mut doc) = fixture("  hello");
        let inv =
            CommandInvocation::of(b.indent_left.0).with_range(crate::range::Range::CurrentLine);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Only 2 spaces present; strips both.
        assert_eq!(doc.text(), "hello");
    }

    #[test]
    fn indent_left_strips_leading_tab() {
        let (registry, b, mut doc) = fixture("\thello");
        let inv =
            CommandInvocation::of(b.indent_left.0).with_range(crate::range::Range::CurrentLine);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello");
    }

    #[test]
    fn indent_left_no_indent_is_no_op() {
        let (registry, b, mut doc) = fixture("hello");
        let inv =
            CommandInvocation::of(b.indent_left.0).with_range(crate::range::Range::CurrentLine);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello");
    }

    #[test]
    fn indent_right_with_whole_range_indents_every_line() {
        let (registry, b, mut doc) = fixture("a\nb\nc");
        let inv = CommandInvocation::of(b.indent_right.0).with_range(crate::range::Range::Whole);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "    a\n    b\n    c");
    }

    #[test]
    fn indent_left_with_whole_range_dedents_every_line() {
        let (registry, b, mut doc) = fixture("    a\n    b\n    c");
        let inv = CommandInvocation::of(b.indent_left.0).with_range(crate::range::Range::Whole);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "a\nb\nc");
    }

    // ---- Case operators (gU, gu, g~) ----

    #[test]
    fn upper_uppercases_word_target() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.upper.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // word_forward from 0 lands at byte 6 -> [0, 6) = "hello " -> "HELLO ".
        assert_eq!(doc.text(), "HELLO world");
    }

    #[test]
    fn lower_lowercases_range() {
        let (registry, b, mut doc) = fixture("HELLO WORLD");
        let inv = CommandInvocation::of(b.lower.0).with_range(crate::range::Range::Whole);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello world");
    }

    #[test]
    fn toggle_case_inverts_each_letter() {
        let (registry, b, mut doc) = fixture("Hello World");
        let inv = CommandInvocation::of(b.toggle_case.0).with_range(crate::range::Range::Whole);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hELLO wORLD");
    }

    #[test]
    fn upper_with_no_letters_is_no_op() {
        let (registry, b, mut doc) = fixture("123 !@# 456");
        let inv = CommandInvocation::of(b.upper.0).with_range(crate::range::Range::Whole);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // No transformation needed -> Effect::None.
        assert!(matches!(effect, Effect::None));
        assert_eq!(doc.text(), "123 !@# 456");
    }

    #[test]
    fn case_operators_preserve_non_letter_bytes() {
        let (registry, b, mut doc) = fixture("foo_bar.baz");
        let inv = CommandInvocation::of(b.upper.0).with_range(crate::range::Range::Whole);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Underscore and dot pass through unchanged.
        assert_eq!(doc.text(), "FOO_BAR.BAZ");
    }

    // ---- Text objects ----

    fn invoke_textobj(
        op: crate::registry::OperatorId,
        tobj: crate::registry::TextObjectId,
    ) -> CommandInvocation {
        CommandInvocation::of(op.0).with_target(Target::TextObject(tobj, crate::args::Args::None))
    }

    #[test]
    fn iw_inner_word_covers_word_at_cursor() {
        // d iw on "hello world" with cursor on 'l' (byte 2) deletes "hello".
        let (registry, b, mut doc) = fixture("hello world");
        let inv = invoke_textobj(b.delete, b.inner_word);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 2),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), " world");
    }

    #[test]
    fn iw_at_start_of_word_works() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = invoke_textobj(b.delete, b.inner_word);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), " world");
    }

    #[test]
    fn iw_on_whitespace_is_no_op() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = invoke_textobj(b.delete, b.inner_word);
        // Cursor on space at byte 5 -- not on a word.
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello world");
    }

    #[test]
    fn aw_around_word_includes_trailing_whitespace() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = invoke_textobj(b.delete, b.around_word);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 2),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // around_word: "hello" + trailing space deleted -> "world".
        assert_eq!(doc.text(), "world");
    }

    #[test]
    fn aw_on_last_word_takes_leading_whitespace() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = invoke_textobj(b.delete, b.around_word);
        // Cursor on 'w' at byte 6 -- no trailing whitespace, so leading.
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 6),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello");
    }

    #[test]
    #[allow(non_snake_case)]
    fn iW_treats_punctuation_as_part_of_big_word() {
        // `iw` would split on `.`; `iW` does not because `.` is
        // non-whitespace -> part of the WORD.
        let (registry, b, mut doc) = fixture("foo.bar baz");
        let inv = invoke_textobj(b.delete, b.inner_big_word);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 2),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), " baz");
    }

    #[test]
    #[allow(non_snake_case)]
    fn iW_on_whitespace_is_no_op() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = invoke_textobj(b.delete, b.inner_big_word);
        // Cursor on space at byte 5 -- not on a WORD.
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello world");
    }

    #[test]
    #[allow(non_snake_case)]
    fn aW_around_big_word_includes_trailing_whitespace() {
        let (registry, b, mut doc) = fixture("foo.bar baz");
        let inv = invoke_textobj(b.delete, b.around_big_word);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // around_big_word: "foo.bar" + trailing space -> "baz".
        assert_eq!(doc.text(), "baz");
    }

    #[test]
    fn i_angle_covers_inside_pair() {
        let (registry, b, mut doc) = fixture("Vec<String>");
        let inv = invoke_textobj(b.delete, b.inner_angle);
        // Cursor inside angles (byte 5 = 'S' in "String").
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "Vec<>");
    }

    #[test]
    fn a_angle_covers_pair_including_brackets() {
        let (registry, b, mut doc) = fixture("Vec<String>");
        let inv = invoke_textobj(b.delete, b.around_angle);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "Vec");
    }

    #[test]
    fn i_double_quote_covers_quoted_content_only() {
        let (registry, b, mut doc) = fixture(r#"foo "bar baz" qux"#);
        let inv = invoke_textobj(b.delete, b.inner_quote_double);
        // Cursor inside quotes (byte 6 = 'a' of "bar").
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 6),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), r#"foo "" qux"#);
    }

    #[test]
    fn a_double_quote_covers_quoted_content_and_quotes() {
        let (registry, b, mut doc) = fixture(r#"foo "bar baz" qux"#);
        let inv = invoke_textobj(b.delete, b.around_quote_double);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 6),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "foo  qux");
    }

    #[test]
    fn i_single_quote_works() {
        let (registry, b, mut doc) = fixture("foo 'bar' baz");
        let inv = invoke_textobj(b.delete, b.inner_quote_single);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 6),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "foo '' baz");
    }

    #[test]
    fn i_paren_covers_inside_outermost_parens() {
        let (registry, b, mut doc) = fixture("call(arg1, arg2)");
        let inv = invoke_textobj(b.delete, b.inner_paren);
        // Cursor on 'a' of "arg2" at byte 11.
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 11),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "call()");
    }

    #[test]
    fn a_paren_includes_brackets() {
        let (registry, b, mut doc) = fixture("call(arg1, arg2)");
        let inv = invoke_textobj(b.delete, b.around_paren);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 11),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "call");
    }

    #[test]
    fn nested_parens_picks_innermost() {
        let (registry, b, mut doc) = fixture("a(b(c)d)e");
        let inv = invoke_textobj(b.delete, b.inner_paren);
        // Cursor at byte 4 ('c').
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 4),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "a(b()d)e");
    }

    #[test]
    fn i_bracket_works_for_square_brackets() {
        let (registry, b, mut doc) = fixture("arr[1, 2, 3]");
        let inv = invoke_textobj(b.delete, b.inner_bracket);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 4),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "arr[]");
    }

    #[test]
    fn i_brace_works_for_curly_braces() {
        let (registry, b, mut doc) = fixture("fn body { return 42; }");
        let inv = invoke_textobj(b.delete, b.inner_brace);
        // Cursor inside the braces.
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 12),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "fn body {}");
    }

    #[test]
    fn unmatched_bracket_is_no_op() {
        let (registry, b, mut doc) = fixture("no brackets here");
        let inv = invoke_textobj(b.delete, b.inner_paren);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "no brackets here");
    }

    #[test]
    fn ciw_change_inner_word_enters_insert() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.change.0)
            .with_target(Target::TextObject(b.inner_word, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 2),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => {
                assert!(matches!(
                    parts[parts.len() - 1],
                    Effect::EnterMode(crate::ModalState::Insert)
                ));
            }
            other => panic!("expected Many, got {other:?}"),
        }
        assert_eq!(doc.text(), " world");
    }

    // ---- Tag text objects (it / at) ----

    #[test]
    fn it_selects_inside_tag() {
        let (registry, b, mut doc) = fixture("<p>hello world</p>");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::TextObject(b.inner_tag, crate::args::Args::None));
        // Cursor inside <p>: byte 5 ('e' of "hello").
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "<p></p>");
    }

    #[test]
    fn at_selects_around_tag() {
        let (registry, b, mut doc) = fixture("<p>hello world</p>");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::TextObject(b.around_tag, crate::args::Args::None));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn it_with_no_enclosing_tag_is_no_op() {
        let (registry, b, mut doc) = fixture("plain text");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::TextObject(b.inner_tag, crate::args::Args::None));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 4),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "plain text");
    }

    // ---- Sentence motions and text objects ----

    #[test]
    fn sentence_forward_advances_after_period_space() {
        let (registry, b, mut doc) = fixture("First sentence. Second sentence. Third.");
        let inv = CommandInvocation::of(b.sentence_forward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // After "First sentence. " -> 'S' of "Second" at byte 16.
                assert_eq!(s.primary().head, Position::new(0, 16));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn sentence_backward_returns_to_previous_start() {
        let (registry, b, mut doc) = fixture("First. Second.");
        // Cursor on 'S' of "Second" at byte 7.
        let inv = CommandInvocation::of(b.sentence_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 7),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn dis_deletes_inner_sentence() {
        let (registry, b, mut doc) = fixture("First sentence. Second sentence.");
        // Cursor on byte 5 (inside "First sentence").
        let inv = CommandInvocation::of(b.delete.0).with_target(Target::TextObject(
            b.inner_sentence,
            crate::args::Args::None,
        ));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Inner stops before the period; period stays.
        assert!(doc.text().starts_with('.'));
    }

    // ---- Paragraph motions and text objects ----

    #[test]
    fn paragraph_forward_lands_on_next_blank_line() {
        let (registry, b, mut doc) = fixture("foo\nbar\n\nbaz");
        let inv = CommandInvocation::of(b.paragraph_forward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // First blank line is line 2.
                assert_eq!(s.primary().head, Position::new(2, 0));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_backward_lands_on_previous_blank_line() {
        let (registry, b, mut doc) = fixture("foo\n\nbar\nbaz");
        let inv = CommandInvocation::of(b.paragraph_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(3, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(1, 0)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn paragraph_forward_at_end_of_buffer_clamps() {
        let (registry, b, mut doc) = fixture("foo\nbar");
        let inv = CommandInvocation::of(b.paragraph_forward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // No blank line; lands at last addressable line.
                assert_eq!(s.primary().head.line, 1);
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn dap_deletes_paragraph_with_blank_lines() {
        let (registry, b, mut doc) = fixture("foo\nbar\n\nbaz");
        let inv = CommandInvocation::of(b.delete.0).with_target(Target::TextObject(
            b.around_paragraph,
            crate::args::Args::None,
        ));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Around paragraph: first non-blank run + trailing blank line.
        assert!(doc.text().contains("baz"));
    }

    #[test]
    fn dip_deletes_paragraph_only() {
        let (registry, b, mut doc) = fixture("foo\nbar\n\nbaz");
        let inv = CommandInvocation::of(b.delete.0).with_target(Target::TextObject(
            b.inner_paragraph,
            crate::args::Args::None,
        ));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Inner paragraph: just the non-blank run, blank line preserved.
        assert!(doc.text().starts_with('\n') || doc.text().starts_with("\nbaz"));
    }

    // ---- WORD motions (W, B, E) ----

    #[test]
    fn big_word_forward_treats_punctuation_as_part_of_word() {
        // word_forward stops at punctuation; big_word_forward doesn't.
        let (registry, b, mut doc) = fixture("foo,bar baz");
        let inv = CommandInvocation::of(b.big_word_forward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // From byte 0, "foo,bar" is one WORD; next WORD is "baz" at byte 8.
                assert_eq!(s.primary().head, Position::new(0, 8));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn big_word_backward_skips_to_word_start() {
        let (registry, b, mut doc) = fixture("foo,bar baz");
        // From byte 8 ('b' of "baz") `B` -> byte 0 ('f' of "foo,bar").
        let inv = CommandInvocation::of(b.big_word_backward.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 8),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn big_word_end_lands_at_last_byte_of_big_word() {
        let (registry, b, mut doc) = fixture("foo,bar baz");
        let inv = CommandInvocation::of(b.big_word_end.0);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => {
                // End of "foo,bar" is byte 6 (the 'r').
                assert_eq!(s.primary().head, Position::new(0, 6));
            }
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- find-char / till-char (f, F, t, T) ----

    fn invoke_with_char(id: crate::registry::MotionId, c: char) -> CommandInvocation {
        CommandInvocation::of(id.0).with_args(crate::args::Args::Char(c))
    }

    #[test]
    fn find_char_forward_lands_on_next_occurrence_on_line() {
        let (registry, b, mut doc) = fixture("hello world");
        // From byte 0 ('h') `fo` -> 'o' of "hello" at byte 4.
        let inv = invoke_with_char(b.find_char_forward, 'o');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn find_char_forward_skips_current_byte_so_it_advances() {
        let (registry, b, mut doc) = fixture("oxox");
        // From 'o' at byte 0, `fo` -> next 'o' at byte 2.
        let inv = invoke_with_char(b.find_char_forward, 'o');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 2)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn find_char_forward_no_match_is_no_op() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = invoke_with_char(b.find_char_forward, 'z');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn find_char_forward_does_not_cross_newlines() {
        let (registry, b, mut doc) = fixture("hello\nworld");
        // From byte 0, `fw` should NOT find 'w' on line 1.
        let inv = invoke_with_char(b.find_char_forward, 'w');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::ZERO),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn find_char_backward_lands_on_previous_occurrence() {
        let (registry, b, mut doc) = fixture("hello world");
        // From byte 8 ('r' of "world") `Fo` -> 'o' of "world" at byte 7.
        let inv = invoke_with_char(b.find_char_backward, 'o');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 8),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 7)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn find_char_backward_no_match_is_no_op() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = invoke_with_char(b.find_char_backward, 'z');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 4),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 4)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn till_char_forward_lands_one_byte_before_match() {
        let (registry, b, mut doc) = fixture("hello world");
        // `tw` from byte 0 -> byte 5 (space, one before 'w' at byte 6).
        let inv = invoke_with_char(b.till_char_forward, 'w');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 5)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn till_char_backward_lands_one_byte_after_match() {
        let (registry, b, mut doc) = fixture("hello world");
        // `Th` from byte 8 -> byte 1 (one after 'h' at byte 0).
        let inv = invoke_with_char(b.till_char_backward, 'h');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 8),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 1)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    #[test]
    fn find_char_with_no_args_errors() {
        let (registry, b, mut doc) = fixture("hello");
        // No Args::Char supplied -> InvalidArgs.
        let inv = CommandInvocation::of(b.find_char_forward.0);
        let err = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::InvalidArgs(_)));
    }

    #[test]
    fn find_char_forward_handles_unicode_target() {
        let (registry, b, mut doc) = fixture("café au lait");
        // `fé` from byte 0 -> byte 3 ('é' starts at byte 3 in "café").
        let inv = invoke_with_char(b.find_char_forward, 'é');
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::SelectionChange(s) => assert_eq!(s.primary().head, Position::new(0, 3)),
            other => panic!("expected SelectionChange, got {other:?}"),
        }
    }

    // ---- change operator (c) ----

    #[test]
    fn change_with_word_forward_emits_edits_yank_and_enter_insert() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.change.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Effect::Many([Edits, Yank, EnterMode(Insert)]).
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 3);
                assert!(matches!(parts[0], Effect::Edits(_)));
                assert!(matches!(parts[1], Effect::Yank { .. }));
                assert!(matches!(
                    parts[2],
                    Effect::EnterMode(crate::ModalState::Insert)
                ));
            }
            other => panic!("expected Effect::Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "world");
    }

    #[test]
    fn change_current_line_clears_line_and_enters_insert() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let cursor = Position::new(1, 0);
        let inv = CommandInvocation::of(b.change.0).with_range(crate::range::Range::CurrentLine);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            cursor,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 3);
                // CurrentLine -> linewise yank.
                match &parts[1] {
                    Effect::Yank { kind, .. } => assert_eq!(*kind, YankKind::Linewise),
                    other => panic!("expected Yank at [1], got {other:?}"),
                }
                assert!(matches!(
                    parts[2],
                    Effect::EnterMode(crate::ModalState::Insert)
                ));
            }
            other => panic!("expected Effect::Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "aaa\n\nccc");
    }

    #[test]
    fn change_with_line_end_target_truncates_line_and_enters_insert() {
        // `c$` from byte 5 of "hello world" leaves "hello" and enters Insert.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.change.0)
            .with_target(Target::Motion(b.line_end, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 5),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 3);
                assert!(matches!(
                    parts[2],
                    Effect::EnterMode(crate::ModalState::Insert)
                ));
            }
            other => panic!("expected Effect::Many, got {other:?}"),
        }
        assert_eq!(doc.text(), "hello");
    }

    // ---- replace-char operator (r{char} / Visual r) ----

    /// Helper: build a single visual selection on `doc`.
    fn set_visual(
        doc: &mut Document,
        anchor: Position,
        head: Position,
        visual: lattice_protocol::selection::VisualMode,
    ) {
        use lattice_protocol::selection::{Selection, SelectionSet};
        doc.set_selections(SelectionSet::single(Selection {
            anchor,
            head,
            visual: Some(visual),
        }));
    }

    #[test]
    fn replace_char_overwrites_single_char_under_cursor() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_target(Target::Motion(b.char_right, crate::args::Args::None))
            .with_args(crate::args::Args::Char('a'));
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::Edits(_)));
        assert_eq!(doc.text(), "aello");
    }

    #[test]
    fn replace_char_with_count_overwrites_count_chars() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_target(Target::Motion(b.char_right, crate::args::Args::None))
            .with_args(crate::args::Args::Char('a'))
            .with_count(Count(3));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "aaalo");
    }

    #[test]
    fn replace_char_count_too_large_is_a_no_op() {
        // Vim's `3r` on a 2-char line replaces nothing (bells).
        let (registry, b, mut doc) = fixture("ab");
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_target(Target::Motion(b.char_right, crate::args::Args::None))
            .with_args(crate::args::Args::Char('x'))
            .with_count(Count(3));
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::None));
        assert_eq!(doc.text(), "ab");
    }

    #[test]
    fn replace_char_on_empty_line_is_a_no_op() {
        let (registry, b, mut doc) = fixture("");
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_target(Target::Motion(b.char_right, crate::args::Args::None))
            .with_args(crate::args::Args::Char('x'));
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert!(matches!(eff, Effect::None));
        assert_eq!(doc.text(), "");
    }

    #[test]
    fn visual_replace_char_overwrites_selection() {
        use lattice_protocol::selection::VisualMode;
        let (registry, b, mut doc) = fixture("hello world");
        // Charwise select "hello": anchor byte 0, head byte 4
        // (inclusive-head extends to [0, 5)).
        set_visual(
            &mut doc,
            Position::new(0, 0),
            Position::new(0, 4),
            VisualMode::Charwise,
        );
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_range(crate::range::Range::Selection)
            .with_args(crate::args::Args::Char('x'));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "xxxxx world");
    }

    #[test]
    fn visual_replace_char_preserves_newlines_across_lines() {
        use lattice_protocol::selection::VisualMode;
        let (registry, b, mut doc) = fixture("ab\ncd");
        // Charwise from (0,1) through (1,0): covers "b\nc" (inclusive
        // head extends the end to (1,1)).
        set_visual(
            &mut doc,
            Position::new(0, 1),
            Position::new(1, 0),
            VisualMode::Charwise,
        );
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_range(crate::range::Range::Selection)
            .with_args(crate::args::Args::Char('X'));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 1),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Line structure preserved; only the two chars overwritten.
        assert_eq!(doc.text(), "aX\nXd");
    }

    #[test]
    fn visual_linewise_replace_char_overwrites_whole_lines() {
        use lattice_protocol::selection::VisualMode;
        let (registry, b, mut doc) = fixture("aaa\nbbb\nccc");
        set_visual(
            &mut doc,
            Position::new(0, 0),
            Position::new(1, 0),
            VisualMode::Linewise,
        );
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_range(crate::range::Range::Selection)
            .with_args(crate::args::Args::Char('z'));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "zzz\nzzz\nccc");
    }

    #[test]
    fn visual_blockwise_replace_char_overwrites_each_row_slice() {
        use lattice_protocol::selection::VisualMode;
        let (registry, b, mut doc) = fixture("abcd\nefgh\nijkl");
        // Block from (0,1) to (2,2): columns 1..=2 on each row.
        set_visual(
            &mut doc,
            Position::new(0, 1),
            Position::new(2, 2),
            VisualMode::Blockwise,
        );
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_range(crate::range::Range::Selection)
            .with_args(crate::args::Args::Char('*'));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 1),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "a**d\ne**h\ni**l");
    }

    #[test]
    fn replace_char_without_char_arg_errors() {
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.replace_char.0)
            .with_target(Target::Motion(b.char_right, crate::args::Args::None));
        let err = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .expect_err("replace-char requires Args::Char");
        assert!(matches!(err, CommandError::InvalidArgs(_)));
    }

    // ---- yank operator (y) ----

    #[test]
    fn yank_with_word_forward_emits_charwise_yank_and_zero_register() {
        let (registry, b, mut doc) = fixture("hello world");
        let original_text = doc.text();
        let inv = CommandInvocation::of(b.yank.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Yank does NOT touch the buffer.
        assert_eq!(doc.text(), original_text);
        // Yank emits Many([requested-register, "0]).
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 2);
                match &parts[0] {
                    Effect::Yank {
                        content,
                        kind,
                        register,
                        explicit_yank,
                    } => {
                        assert_eq!(content, "hello ");
                        assert_eq!(*kind, YankKind::Charwise);
                        assert_eq!(*register, crate::register::Register::Unnamed);
                        assert!(
                            *explicit_yank,
                            "the primary yank write is clipboard-eligible"
                        );
                    }
                    other => panic!("expected Yank at [0], got {other:?}"),
                }
                match &parts[1] {
                    Effect::Yank { register, .. } => {
                        assert_eq!(*register, crate::register::Register::Numbered(0));
                    }
                    other => panic!("expected Yank(\"0) at [1], got {other:?}"),
                }
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn yank_with_current_line_range_emits_linewise_yank() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::CurrentLine);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(1, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "aaa\nBBB\nccc");
        match effect {
            Effect::Many(parts) => match &parts[0] {
                Effect::Yank { content, kind, .. } => {
                    // Slice 8.i.4.g: linewise yank content always
                    // ends with `\n` so paste can splice cleanly.
                    assert_eq!(content, "BBB\n");
                    assert_eq!(*kind, YankKind::Linewise);
                }
                other => panic!("expected Yank, got {other:?}"),
            },
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn yank_with_whole_range_emits_linewise_full_buffer() {
        let (registry, b, mut doc) = fixture("hello\nworld");
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::Whole);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => match &parts[0] {
                Effect::Yank { content, kind, .. } => {
                    // Slice 8.i.4.g: linewise yank content always
                    // ends with `\n` so paste can splice cleanly,
                    // even when the source had no trailing
                    // newline (here: `:y` of "hello\nworld").
                    assert_eq!(content, "hello\nworld\n");
                    assert_eq!(*kind, YankKind::Linewise);
                }
                other => panic!("expected Yank, got {other:?}"),
            },
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn yank_does_not_modify_buffer() {
        let (registry, b, mut doc) = fixture("immutable text");
        let original = doc.text();
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::Whole);
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), original);
    }

    #[test]
    fn yank_empty_range_returns_none() {
        let (registry, b, mut doc) = fixture("");
        let inv = CommandInvocation::of(b.yank.0).with_range(crate::range::Range::Whole);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        // Empty buffer / empty range -> Effect::None.
        assert!(matches!(effect, Effect::None));
    }

    // ---- delete-yanks-into-register (composite verification) ----

    #[test]
    fn delete_charwise_yanks_charwise() {
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => match &parts[1] {
                Effect::Yank { kind, content, .. } => {
                    assert_eq!(*kind, YankKind::Charwise);
                    assert_eq!(content, "hello ");
                }
                other => panic!("expected Yank, got {other:?}"),
            },
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn delete_linewise_yanks_linewise() {
        let (registry, b, mut doc) = fixture("aaa\nBBB\nccc");
        let inv = CommandInvocation::of(b.delete.0).with_range(crate::range::Range::CurrentLine);
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(1, 0),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => match &parts[1] {
                Effect::Yank { kind, .. } => assert_eq!(*kind, YankKind::Linewise),
                other => panic!("expected Yank, got {other:?}"),
            },
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn delete_emits_many_with_edits_and_yank_only() {
        // Sanity: delete emits Many([Edits, Yank]) -- never EnterMode.
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        let effect = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match effect {
            Effect::Many(parts) => {
                assert_eq!(parts.len(), 2);
                assert!(!parts.iter().any(|e| matches!(e, Effect::EnterMode(_))));
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    #[test]
    fn delete_with_word_end_target_de_semantics() {
        // `de` from start of "hello world": word_end (`e`) is INCLUSIVE, so
        // it lands on the last char of "hello" ('o', byte 4) and the operator
        // covers that char too -- range [0, 5) -> deletes "hello", leaving
        // " world". Vim parity; previously the inclusive flag was ignored and
        // this left the trailing 'o' ("o world").
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_end, crate::args::Args::None));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), " world");
    }

    #[test]
    fn change_with_word_end_target_ce_is_inclusive() {
        // `ce` deletes through the end of the word (inclusive `e`), same as
        // `de` but leaving the operator in change-mode. From byte 0 of
        // "hello world" it removes "hello" -> " world".
        let (registry, b, mut doc) = fixture("hello world");
        let inv = CommandInvocation::of(b.change.0)
            .with_target(Target::Motion(b.word_end, crate::args::Args::None));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), " world");
    }

    #[test]
    fn delete_with_find_char_target_df_is_inclusive() {
        // `dfl` deletes up to AND including the found char (inclusive `f`).
        // From byte 0 of "hello", `f` for 'l' lands on byte 2; deletion
        // covers [0, 3) -> "lo".
        let (registry, b, mut doc) = fixture("hello");
        let inv = CommandInvocation::of(b.delete.0).with_target(Target::Motion(
            b.find_char_forward,
            crate::args::Args::Char('l'),
        ));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "lo");
    }

    #[test]
    fn dw_on_last_word_of_line_stops_at_line_end() {
        // Vim word-motion special case: `dw` on the last word of a line
        // deletes to the line end and does NOT join the next line. Cursor on
        // 'f' of "foo" (byte 6 of line 0); `w` would cross to "bar" on line 1,
        // but under the operator the range is clamped to the end of line 0.
        let (registry, b, mut doc) = fixture("hello foo\nbar");
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(b.word_forward, crate::args::Args::None));
        execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::new(0, 6),
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        assert_eq!(doc.text(), "hello \nbar", "dw must not cross the newline");
    }

    #[test]
    fn delete_with_motion_target_for_motion_id_kind_mismatch_is_caught() {
        // Constructing a Target::Motion from a *non-motion* command id should
        // surface a kind mismatch, not silently succeed.
        let (registry, b, mut doc) = fixture("abc");
        // Use `delete` (an operator) as if it were a motion. Should fail.
        let bogus = MotionId(b.delete.0);
        let inv = CommandInvocation::of(b.delete.0)
            .with_target(Target::Motion(bogus, crate::args::Args::None));
        let err = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::KindMismatch { .. }));
    }
}
