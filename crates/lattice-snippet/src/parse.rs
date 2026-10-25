//! TextMate / LSP snippet body parser. Produces a
//! [`SnippetBody`] from a string; consumers (renderer +
//! active-snippet state machine) walk the resulting tokens.
//!
//! Grammar (matching VS Code's parser; see
//! [LSP 3.17 §Snippet Syntax](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/#snippet_syntax)):
//!
//! ```text
//! body          ::= ( token )*
//! token         ::= literal | dollar
//! literal       ::= ( escaped | safe_char )+
//! escaped       ::= '\\' ( '$' | '\\' | '}' )
//! safe_char     ::= any char except '$' | '}' | escape opener
//!
//! dollar        ::= '$' ( int | name | '{' block '}' )
//! block         ::= int                            -- '${1}'
//!                 | int ':' body                   -- '${1:foo}'
//!                 | int '|' choices '|'            -- '${1|a,b,c|}'
//!                 | int '/' pat '/' repl '/' flags -- '${1/.../.../i}'
//!                 | name                           -- '${VAR}'
//!                 | name ':' body                  -- '${VAR:fallback}'
//!                 | name '/' pat '/' repl '/' flags
//! choices       ::= choice ( ',' choice )*
//! choice        ::= ( '\\,' | not(',' | '|') )*
//! pat / repl    ::= ( '\\/' | not('/') )*
//! flags         ::= [a-zA-Z]*
//! int           ::= [0-9]+
//! name          ::= [a-zA-Z_][a-zA-Z0-9_]*
//! ```
//!
//! On a malformed dollar block (e.g. unclosed `${`), the parser
//! falls back to literal output for that span -- VS Code's
//! "be lenient" behaviour. This means a snippet body that
//! looks-like-but-isn't a placeholder still inserts as plain
//! text rather than failing the whole expansion. Returns a
//! [`ParseError`] only for genuinely catastrophic input
//! (currently never -- v1 always falls through to literal).

use crate::token::{ChoiceOption, SnippetBody, SnippetToken, TransformTarget};

/// Snippet parse error. Empty in v1 -- the parser is total and
/// falls back to literal output on malformed input. Reserved
/// for future strict-mode parsing if a use case appears.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("internal parser error: {0}")]
    Internal(&'static str),
}

/// Parse a TextMate / LSP snippet body string.
pub fn parse(body: &str) -> Result<SnippetBody, ParseError> {
    let mut p = Parser::new(body);
    let tokens = p.parse_body(None)?;
    Ok(SnippetBody::new(merge_literals(tokens)))
}

/// Merge consecutive `Literal` tokens. The parser may emit
/// runs of single-character literals while walking escapes;
/// the renderer is happier with a compact stream.
fn merge_literals(tokens: Vec<SnippetToken>) -> Vec<SnippetToken> {
    let mut out: Vec<SnippetToken> = Vec::with_capacity(tokens.len());
    for tok in tokens {
        if let SnippetToken::Literal(new) = &tok
            && let Some(SnippetToken::Literal(prev)) = out.last_mut()
        {
            prev.push_str(new);
            continue;
        }
        out.push(tok);
    }
    out
}

/// Deepest `${…}` nesting the parser follows. Past it a `$` is literal: a
/// real snippet nests two or three levels, and following an arbitrary depth
/// would let a malformed body overflow the stack.
const MAX_DEPTH: usize = 32;

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
    /// Current `${…}` nesting.
    depth: usize,
    /// `$` positions already found not to start a well-formed block. A
    /// failed block falls back to a literal `$` and parsing resumes inside
    /// it, which reaches the inner `$`s again; without this an unclosed
    /// `${1:${1:${1:…` re-parsed each level once per enclosing level —
    /// exponential in the nesting (snippet-integrity SI.8).
    failed: std::collections::HashSet<usize>,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            src: s.as_bytes(),
            pos: 0,
            depth: 0,
            failed: std::collections::HashSet::new(),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let b = self.peek();
        if b.is_some() {
            self.pos += 1;
        }
        b
    }

    #[allow(dead_code)]
    fn starts_with(&self, lit: &[u8]) -> bool {
        self.src
            .get(self.pos..self.pos + lit.len())
            .map(|s| s == lit)
            .unwrap_or(false)
    }

    /// Parse tokens until either end-of-input or one of the
    /// stop bytes appears at the current position. The stop
    /// list is `Some` for nested bodies (e.g. inside a
    /// `${1:default}` block, `}` closes the block).
    fn parse_body(&mut self, stop: Option<&[u8]>) -> Result<Vec<SnippetToken>, ParseError> {
        let mut tokens = Vec::new();
        while let Some(b) = self.peek() {
            if let Some(stop) = stop
                && stop.contains(&b)
            {
                break;
            }
            match b {
                b'\\' => {
                    self.bump();
                    if let Some(next) = self.peek() {
                        match next {
                            b'$' | b'\\' | b'}' => {
                                self.bump();
                                tokens.push(SnippetToken::Literal(char::from(next).to_string()));
                            }
                            // Unknown escape: keep the backslash
                            // verbatim. VS Code lenient mode.
                            _ => {
                                tokens.push(SnippetToken::Literal("\\".to_string()));
                            }
                        }
                    } else {
                        tokens.push(SnippetToken::Literal("\\".to_string()));
                    }
                }
                b'$' => {
                    let saved = self.pos;
                    self.bump();
                    let parsed = if self.failed.contains(&saved) || self.depth >= MAX_DEPTH {
                        None
                    } else {
                        self.depth += 1;
                        let tok = self.try_parse_dollar();
                        self.depth -= 1;
                        tok
                    };
                    match parsed {
                        Some(tok) => tokens.push(tok),
                        None => {
                            // Fall back to literal '$'.
                            self.failed.insert(saved);
                            self.pos = saved + 1;
                            tokens.push(SnippetToken::Literal("$".to_string()));
                        }
                    }
                }
                _ => {
                    // Walk one Unicode scalar at a time so
                    // multi-byte UTF-8 chars (héllo, 你好,
                    // emoji) round-trip cleanly. Lone-byte
                    // bumps would yield Latin-1-shaped output
                    // and corrupt non-ASCII content.
                    let ch = self.peek_char_at(self.pos);
                    self.pos += ch.len_utf8();
                    tokens.push(SnippetToken::Literal(ch.to_string()));
                }
            }
        }
        Ok(tokens)
    }

    /// Decode the UTF-8 char starting at `pos`. The bytes are
    /// guaranteed valid UTF-8 because the parser was constructed
    /// from a `&str`; if `pos` somehow lands mid-codepoint we
    /// fall back to a `?` so the parser stays total.
    fn peek_char_at(&self, pos: usize) -> char {
        let s = std::str::from_utf8(&self.src[pos..])
            .ok()
            .and_then(|s| s.chars().next());
        s.unwrap_or('?')
    }

    /// Called after consuming a `$`. Parses the rest of the
    /// dollar form. Returns `None` to signal "this isn't a
    /// valid dollar form, treat the `$` as literal."
    fn try_parse_dollar(&mut self) -> Option<SnippetToken> {
        match self.peek()? {
            // Bare integer tabstop: $1, $9, $123.
            b'0'..=b'9' => {
                let n = self.parse_uint()?;
                Some(SnippetToken::Tabstop(n))
            }
            // Bare variable: $TM_FILENAME.
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let name = self.parse_name()?;
                Some(SnippetToken::Variable {
                    name,
                    default: None,
                })
            }
            // Block form: ${...}.
            b'{' => {
                self.bump();
                self.parse_block()
            }
            _ => None,
        }
    }

    fn parse_uint(&mut self) -> Option<u32> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() {
                self.bump();
            } else {
                break;
            }
        }
        if start == self.pos {
            return None;
        }
        let s = std::str::from_utf8(&self.src[start..self.pos]).ok()?;
        s.parse().ok()
    }

    fn parse_name(&mut self) -> Option<String> {
        let start = self.pos;
        // First char is alpha or `_`.
        match self.peek()? {
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                self.bump();
            }
            _ => return None,
        }
        // Subsequent chars: alpha, digit, or `_`.
        while let Some(b) = self.peek() {
            if matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_') {
                self.bump();
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.src[start..self.pos])
            .ok()
            .map(|s| s.to_string())
    }

    /// Called after consuming `${`. Parses everything up to and
    /// including the closing `}`. Returns `None` to signal a
    /// malformed block; the caller falls through to literal.
    fn parse_block(&mut self) -> Option<SnippetToken> {
        match self.peek()? {
            b'0'..=b'9' => {
                let idx = self.parse_uint()?;
                match self.peek()? {
                    b'}' => {
                        self.bump();
                        Some(SnippetToken::Tabstop(idx))
                    }
                    b':' => {
                        self.bump();
                        // Default body: parse until `}`.
                        let inner = self.parse_body(Some(b"}")).ok()?;
                        if self.peek()? != b'}' {
                            return None;
                        }
                        self.bump();
                        Some(SnippetToken::Placeholder {
                            idx,
                            default: SnippetBody::new(merge_literals(inner)),
                        })
                    }
                    b'|' => {
                        self.bump();
                        let options = self.parse_choices()?;
                        if self.peek()? != b'}' {
                            return None;
                        }
                        self.bump();
                        Some(SnippetToken::Choice { idx, options })
                    }
                    b'/' => {
                        self.bump();
                        let (pattern, replacement, flags) = self.parse_transform()?;
                        Some(SnippetToken::Transform {
                            target: TransformTarget::Tabstop(idx),
                            pattern,
                            replacement,
                            flags,
                        })
                    }
                    _ => None,
                }
            }
            b'A'..=b'Z' | b'a'..=b'z' | b'_' => {
                let name = self.parse_name()?;
                match self.peek()? {
                    b'}' => {
                        self.bump();
                        Some(SnippetToken::Variable {
                            name,
                            default: None,
                        })
                    }
                    b':' => {
                        self.bump();
                        let inner = self.parse_body(Some(b"}")).ok()?;
                        if self.peek()? != b'}' {
                            return None;
                        }
                        self.bump();
                        Some(SnippetToken::Variable {
                            name,
                            default: Some(SnippetBody::new(merge_literals(inner))),
                        })
                    }
                    b'/' => {
                        self.bump();
                        let (pattern, replacement, flags) = self.parse_transform()?;
                        Some(SnippetToken::Transform {
                            target: TransformTarget::Variable(name),
                            pattern,
                            replacement,
                            flags,
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn parse_choices(&mut self) -> Option<Vec<ChoiceOption>> {
        let mut options = Vec::new();
        let mut current = String::new();
        loop {
            match self.peek()? {
                b'|' => {
                    self.bump();
                    options.push(ChoiceOption {
                        text: std::mem::take(&mut current),
                    });
                    return Some(options);
                }
                b',' => {
                    self.bump();
                    options.push(ChoiceOption {
                        text: std::mem::take(&mut current),
                    });
                }
                b'\\' => {
                    self.bump();
                    match self.peek() {
                        Some(esc @ (b',' | b'|' | b'\\' | b'$' | b'}')) => {
                            self.bump();
                            current.push(char::from(esc));
                        }
                        _ => current.push('\\'),
                    }
                }
                _ => current.push(self.bump_char()),
            }
        }
    }

    /// Consume one whole UTF-8 scalar. Choice and transform text was once
    /// built a byte at a time, which turned `é` into two Latin-1 characters.
    fn bump_char(&mut self) -> char {
        let ch = self.peek_char_at(self.pos);
        self.pos += ch.len_utf8();
        ch
    }

    /// After `${N/` or `${NAME/`: `pattern/format/options}`. The format may
    /// itself hold `${1:/upcase}`-style references, whose `/` and `}` belong
    /// to them, so braces are tracked; a `/` or `}` that ends a part is the
    /// first one at brace depth zero. `None` (a literal `$`) for anything
    /// unterminated.
    fn parse_transform(&mut self) -> Option<(String, String, String)> {
        let pattern = self.parse_transform_part(true)?;
        if self.peek()? != b'/' {
            return None;
        }
        self.bump();
        let replacement = self.parse_transform_part(false)?;
        if self.peek()? != b'/' {
            return None;
        }
        self.bump();
        let mut flags = String::new();
        loop {
            match self.peek()? {
                b'}' => {
                    self.bump();
                    return Some((pattern, replacement, flags));
                }
                b if b.is_ascii_alphabetic() => {
                    self.bump();
                    flags.push(char::from(b));
                }
                _ => return None,
            }
        }
    }

    /// A pattern or format, up to the `/` that ends it. In a pattern `\/` is
    /// a slash and every other escape is kept for the regex engine
    /// (`\d`, `\.`); in a format the escapes are kept for the format parser,
    /// which resolves them. A `}` at depth zero means the transform is
    /// unterminated.
    fn parse_transform_part(&mut self, pattern: bool) -> Option<String> {
        let mut out = String::new();
        let mut depth = 0usize;
        loop {
            match self.peek()? {
                b'/' if depth == 0 => return Some(out),
                b'}' if depth == 0 => return None,
                b'\\' => {
                    self.bump();
                    let esc = self.bump_char();
                    if !(pattern && esc == '/') {
                        out.push('\\');
                    }
                    out.push(esc);
                }
                b'$' if !pattern && self.src.get(self.pos + 1) == Some(&b'{') => {
                    depth += 1;
                    self.pos += 2;
                    out.push_str("${");
                }
                b'}' => {
                    depth -= 1;
                    self.bump();
                    out.push('}');
                }
                _ => out.push(self.bump_char()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(s: &str) -> SnippetBody {
        parse(s).expect("parse succeeds (parser is total)")
    }

    #[test]
    fn pure_literal_parses_to_one_literal_token() {
        let b = parse_ok("hello world");
        assert_eq!(b.tokens, vec![SnippetToken::Literal("hello world".into())]);
    }

    #[test]
    fn bare_tabstop_parses() {
        let b = parse_ok("$1");
        assert_eq!(b.tokens, vec![SnippetToken::Tabstop(1)]);
    }

    #[test]
    fn final_tabstop_parses() {
        let b = parse_ok("$0");
        assert_eq!(b.tokens, vec![SnippetToken::Tabstop(0)]);
    }

    #[test]
    fn block_tabstop_parses() {
        let b = parse_ok("${42}");
        assert_eq!(b.tokens, vec![SnippetToken::Tabstop(42)]);
    }

    #[test]
    fn placeholder_with_default_parses() {
        let b = parse_ok("${1:foo}");
        match &b.tokens[0] {
            SnippetToken::Placeholder { idx, default } => {
                assert_eq!(*idx, 1);
                assert_eq!(default.tokens, vec![SnippetToken::Literal("foo".into())]);
            }
            other => panic!("expected Placeholder, got {other:?}"),
        }
    }

    #[test]
    fn nested_placeholder_inside_default_parses() {
        let b = parse_ok("${1:outer ${2:inner} more}");
        match &b.tokens[0] {
            SnippetToken::Placeholder { idx, default } => {
                assert_eq!(*idx, 1);
                assert_eq!(
                    default.tokens,
                    vec![
                        SnippetToken::Literal("outer ".into()),
                        SnippetToken::Placeholder {
                            idx: 2,
                            default: SnippetBody::new(vec![SnippetToken::Literal("inner".into())]),
                        },
                        SnippetToken::Literal(" more".into()),
                    ]
                );
            }
            other => panic!("expected Placeholder, got {other:?}"),
        }
    }

    #[test]
    fn choice_placeholder_parses() {
        let b = parse_ok("${1|alpha,beta,gamma|}");
        match &b.tokens[0] {
            SnippetToken::Choice { idx, options } => {
                assert_eq!(*idx, 1);
                let texts: Vec<&str> = options.iter().map(|c| c.text.as_str()).collect();
                assert_eq!(texts, vec!["alpha", "beta", "gamma"]);
            }
            other => panic!("expected Choice, got {other:?}"),
        }
    }

    #[test]
    fn bare_variable_parses() {
        let b = parse_ok("$TM_FILENAME");
        match &b.tokens[0] {
            SnippetToken::Variable { name, default } => {
                assert_eq!(name, "TM_FILENAME");
                assert!(default.is_none());
            }
            other => panic!("expected Variable, got {other:?}"),
        }
    }

    #[test]
    fn block_variable_parses() {
        let b = parse_ok("${TM_FILENAME}");
        match &b.tokens[0] {
            SnippetToken::Variable { name, default } => {
                assert_eq!(name, "TM_FILENAME");
                assert!(default.is_none());
            }
            other => panic!("expected Variable, got {other:?}"),
        }
    }

    #[test]
    fn variable_with_fallback_parses() {
        let b = parse_ok("${TM_FILENAME:default.txt}");
        match &b.tokens[0] {
            SnippetToken::Variable { name, default } => {
                assert_eq!(name, "TM_FILENAME");
                let d = default.as_ref().expect("has default");
                assert_eq!(d.tokens, vec![SnippetToken::Literal("default.txt".into())]);
            }
            other => panic!("expected Variable, got {other:?}"),
        }
    }

    #[test]
    fn escapes_dollar_brace_backslash() {
        let b = parse_ok("\\$1 \\\\ \\}");
        // Expected: literal "$1 \ }" (the escapes consumed).
        assert_eq!(b.tokens, vec![SnippetToken::Literal("$1 \\ }".into())]);
    }

    #[test]
    fn malformed_block_falls_back_to_literal_dollar() {
        // `${` without a recognised follow -- the `$` lands as
        // a literal, the rest re-parses (the `{` becomes a
        // literal too).
        let b = parse_ok("$ no-block");
        assert_eq!(b.tokens, vec![SnippetToken::Literal("$ no-block".into())]);
    }

    #[test]
    fn transform_token_parses_pattern_replacement_flags() {
        let b = parse_ok("${1/foo/bar/g}");
        match &b.tokens[0] {
            SnippetToken::Transform {
                target,
                pattern,
                replacement,
                flags,
            } => {
                assert!(matches!(target, TransformTarget::Tabstop(1)));
                assert_eq!(pattern, "foo");
                assert_eq!(replacement, "bar");
                assert_eq!(flags, "g");
            }
            other => panic!("expected Transform, got {other:?}"),
        }
    }

    #[test]
    fn complex_body_round_trips_to_tokens() {
        // From friendly-snippets `for-in.json`-shape.
        let b = parse_ok("for ${1:i} in ${2:iter} {\n\t$0\n}");
        // Just verify the structure -- 6 tokens: literal,
        // placeholder, literal, placeholder, literal, tabstop,
        // literal.
        assert_eq!(b.tokens.len(), 7);
        assert!(matches!(b.tokens[0], SnippetToken::Literal(ref s) if s == "for "));
        assert!(matches!(
            b.tokens[1],
            SnippetToken::Placeholder { idx: 1, .. }
        ));
        assert!(matches!(b.tokens[2], SnippetToken::Literal(ref s) if s == " in "));
        assert!(matches!(
            b.tokens[3],
            SnippetToken::Placeholder { idx: 2, .. }
        ));
        assert!(matches!(b.tokens[4], SnippetToken::Literal(ref s) if s.starts_with(" {")));
        assert!(matches!(b.tokens[5], SnippetToken::Tabstop(0)));
        assert!(matches!(b.tokens[6], SnippetToken::Literal(ref s) if s.contains("}")));
    }

    #[test]
    fn unicode_literals_round_trip() {
        let b = parse_ok("héllo wörld");
        assert_eq!(b.tokens, vec![SnippetToken::Literal("héllo wörld".into())]);
    }

    /// SI.8: a transform's format can hold its own `${…}` references, with
    /// `/` and `}` inside them. The parser used to stop at the first of
    /// either and fall back to literal text, leaving `{TM_FILENAME/(.*)/` in
    /// the buffer.
    #[test]
    fn a_transform_with_a_nested_format_parses() {
        let body = parse_ok("${TM_FILENAME/(.*)\\.rs$/${1:/upcase}/g}");
        assert_eq!(
            body.tokens,
            vec![SnippetToken::Transform {
                target: TransformTarget::Variable("TM_FILENAME".into()),
                pattern: "(.*)\\.rs$".into(),
                replacement: "${1:/upcase}".into(),
                flags: "g".into(),
            }]
        );
        let body = parse_ok("${1/(a)|(b)/${1:?x:y}/}");
        assert!(matches!(
            &body.tokens[..],
            [SnippetToken::Transform { replacement, .. }] if replacement == "${1:?x:y}"
        ));
    }

    #[test]
    fn an_escaped_slash_in_a_pattern_is_a_slash() {
        let body = parse_ok("${1/a\\/b/c/}");
        assert!(matches!(
            &body.tokens[..],
            [SnippetToken::Transform { pattern, .. }] if pattern == "a/b"
        ));
    }

    #[test]
    fn an_unterminated_transform_is_literal_text() {
        let body = parse_ok("${1/abc}");
        assert_eq!(body.tokens, vec![SnippetToken::Literal("${1/abc}".into())]);
    }

    /// SI.8: choice text is decoded as UTF-8, not byte by byte.
    #[test]
    fn choices_keep_non_ascii_text_and_take_the_spec_escapes() {
        let body = parse_ok("${1|é,ü,\\$x\\}|}");
        let SnippetToken::Choice { options, .. } = &body.tokens[0] else {
            panic!("a choice: {:?}", body.tokens);
        };
        let texts: Vec<&str> = options.iter().map(|o| o.text.as_str()).collect();
        assert_eq!(texts, vec!["é", "ü", "$x}"]);
    }

    /// SI.8: unclosed nesting parses in bounded time. Each level used to be
    /// re-parsed once per enclosing level — 2^40 steps here.
    #[test]
    fn deeply_unclosed_nesting_parses_quickly_as_text() {
        let body: String = "${1:".repeat(40);
        let started = std::time::Instant::now();
        let parsed = parse_ok(&body);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(parsed.tokens, vec![SnippetToken::Literal(body)]);
    }

    /// SI.8: nesting deeper than the cap is text, not a stack overflow.
    #[test]
    fn nesting_past_the_cap_is_literal() {
        let deep = format!("{}x{}", "${1:".repeat(10_000), "}".repeat(10_000));
        let parsed = parse_ok(&deep);
        assert!(!parsed.tokens.is_empty());
    }
}
