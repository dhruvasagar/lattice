//! Where should this line start?
//!
//! The indent engine, beside its peers: [`crate::text_objects`] and
//! [`crate::motions`] are the same shape -- computation over the parse
//! tree, driven by `.scm` files this crate already owns. IN.2 adds the
//! `indents.scm` query evaluator here; IN.1 ships the **lexical**
//! half below, which is what runs when no tree is available.
//!
//! Pure and synchronous by construction -- no I/O, no async, no host
//! state -- because every consumer sits on the keystroke path
//! (`docs/dev/architecture/auto-indent.md` §2).
//!
//! The [`IndentUnit`] value itself lives in `lattice-core`, not here:
//! the `>` / `<` operators in `lattice-grammar` consume it, and this
//! crate depends on `lattice-grammar`, so owning it here would be a
//! cycle.
//!
//! Two policies over one mechanism
//! ------------------------------
//!
//! - [`IndentMethod::Keep`] -- copy the previous non-blank line's
//!   indent. Vim's `autoindent`. No scan, no cleverness.
//! - [`IndentMethod::Syntax`]'s **fallback** -- the copy, plus one level
//!   if the previous line leaves a bracket unclosed, minus one if the
//!   target line opens with a closer. Vim's `smartindent`, roughly.
//!
//! Vim keeps these separate for a reason worth preserving: the bracket
//! rule misfires in a language where `{` is not a block opener, and
//! `keep` is what a user picks when they want the dumbest predictable
//! thing. Rather than special-casing that, the bracket sets are
//! **per-language** and languages with no bracket notion (plain text,
//! markdown) have empty sets -- at which point the bridge degrades to
//! `keep` on its own.
//!
//! What this deliberately does not do
//! ----------------------------------
//!
//! The scan is **lexical**, so it cannot tell a brace in code from a
//! brace in a string or a comment. `println!("{")` counts as an opener
//! here. That is a known and accepted wrong answer: the whole point of
//! this half is to be the thing that still works when no parse tree is
//! available, and a scan sophisticated enough to track string state
//! per-language would be a worse, slower duplicate of what IN.2's
//! tree-sitter path does properly. When the tree is available,
//! `syntax` uses it and never reaches here.

use lattice_core::{IndentMethod, IndentUnit};

use crate::Lang;
use crate::syntax::SyntaxSnapshot;

// ──────────────────────────────────────────────────────────────
// IN.2 — the tree-sitter half
// ──────────────────────────────────────────────────────────────

// Embedded `indents.scm` sources. Files live at
// `crates/lattice-syntax/queries/<lang>/indents.scm`, beside their
// `folds` / `symbols` / `textobjects` siblings; shipped in the binary
// via `include_str!` with no runtime path lookup.
const RUST_INDENTS_QUERY: &str = include_str!("../queries/rust/indents.scm");
// IN.3 — the brace family. One query shape, node names swapped.
const C_INDENTS_QUERY: &str = include_str!("../queries/c/indents.scm");
const CPP_INDENTS_QUERY: &str = include_str!("../queries/cpp/indents.scm");
const CSS_INDENTS_QUERY: &str = include_str!("../queries/css/indents.scm");
const GO_INDENTS_QUERY: &str = include_str!("../queries/go/indents.scm");
const JAVA_INDENTS_QUERY: &str = include_str!("../queries/java/indents.scm");
const JAVASCRIPT_INDENTS_QUERY: &str = include_str!("../queries/javascript/indents.scm");
const JSON_INDENTS_QUERY: &str = include_str!("../queries/json/indents.scm");
const TSX_INDENTS_QUERY: &str = include_str!("../queries/tsx/indents.scm");
const TYPESCRIPT_INDENTS_QUERY: &str = include_str!("../queries/typescript/indents.scm");
// IN.4 — indent-sensitive + scripting. These close with WORDS (`end`,
// `fi`, `done`, `esac`) as often as with punctuation, and two of them
// carry indentation as data inside heredocs / docstrings.
const BASH_INDENTS_QUERY: &str = include_str!("../queries/bash/indents.scm");
const LUA_INDENTS_QUERY: &str = include_str!("../queries/lua/indents.scm");
const PYTHON_INDENTS_QUERY: &str = include_str!("../queries/python/indents.scm");
const RUBY_INDENTS_QUERY: &str = include_str!("../queries/ruby/indents.scm");
// IN.5 — data + markup. `sql` and `markdown` deliberately ship NO
// query; see `indents_source` for why.
const HTML_INDENTS_QUERY: &str = include_str!("../queries/html/indents.scm");
const TOML_INDENTS_QUERY: &str = include_str!("../queries/toml/indents.scm");
const YAML_INDENTS_QUERY: &str = include_str!("../queries/yaml/indents.scm");

/// The `indents.scm` source for a registry language name, or `None`
/// when that language does not ship one yet.
///
/// Keyed by the registry's language *name* rather than [`Lang`] because
/// the registry compiles queries per registered config (including
/// `markdown_inline`, which has no `Lang` variant of its own).
///
/// A language absent from this table is not broken -- predictive indent
/// falls back to the lexical bridge for it, which is vim's
/// `smartindent`. IN.3–IN.5 fill the table in.
pub(crate) fn indents_source(name: &str) -> Option<&'static str> {
    match name {
        "rust" => Some(RUST_INDENTS_QUERY),
        // IN.3 — the brace family.
        "c" => Some(C_INDENTS_QUERY),
        "cpp" => Some(CPP_INDENTS_QUERY),
        "css" => Some(CSS_INDENTS_QUERY),
        "go" => Some(GO_INDENTS_QUERY),
        "java" => Some(JAVA_INDENTS_QUERY),
        "javascript" => Some(JAVASCRIPT_INDENTS_QUERY),
        "json" => Some(JSON_INDENTS_QUERY),
        "tsx" => Some(TSX_INDENTS_QUERY),
        "typescript" => Some(TYPESCRIPT_INDENTS_QUERY),
        // IN.4 — indent-sensitive + scripting.
        "bash" => Some(BASH_INDENTS_QUERY),
        "lua" => Some(LUA_INDENTS_QUERY),
        "python" => Some(PYTHON_INDENTS_QUERY),
        "ruby" => Some(RUBY_INDENTS_QUERY),
        // IN.5 — data + markup.
        "html" => Some(HTML_INDENTS_QUERY),
        "toml" => Some(TOML_INDENTS_QUERY),
        "yaml" => Some(YAML_INDENTS_QUERY),
        // `markdown` and `sql` ship NO query, deliberately:
        //
        // - **markdown** nests by CONTENT WIDTH, not by a fixed unit.
        //   A nested list item aligns under its parent's text, which
        //   depends on the marker (`-` vs `10.`), so a fixed
        //   `shiftwidth` step is the wrong model and would fight the
        //   user. The lexical bridge's copy-the-previous-line is
        //   closer to right, and markdown's `BracketSyntax::NONE`
        //   already stops a stray brace from indenting prose.
        // - **sql** is parsed by `tree-sitter-sequel`, a deliberately
        //   permissive multi-dialect grammar, and SQL indentation
        //   convention varies more between houses than between
        //   dialects (leading vs trailing commas, `AND` alignment,
        //   river style). There is no default worth imposing; `=`
        //   plus an `equalprg` formatter is the honest answer.
        //
        // Both degrade to the lexical bridge, which is the cascade
        // working as designed rather than a gap.
        _ => None,
    }
}

/// What an `indents.scm` capture asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    /// `@indent` — children sit one level deeper. Collapses with
    /// another `@indent` starting on the same row, so
    /// `foo(bar(` opens one level, not two.
    Indent,
    /// `@indent.always` — as `Indent`, without the same-row collapse.
    IndentAlways,
    /// `@outdent` — a line starting with this node sits one level
    /// shallower, cancelling the enclosing `@indent`.
    Outdent,
    /// `@outdent.always` — as `Outdent`, without collapsing.
    OutdentAlways,
}

impl Capture {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "indent" => Some(Self::Indent),
            "indent.always" => Some(Self::IndentAlways),
            "outdent" => Some(Self::Outdent),
            "outdent.always" => Some(Self::OutdentAlways),
            // `@extend` / `@extend.prevent-once` / `@align` are
            // recognised by the dialect but not evaluated in v1 (design
            // §4.1). Unknown captures are ignored rather than rejected,
            // so a query written against the fuller vocabulary still
            // loads and simply contributes less.
            _ => None,
        }
    }
}

/// Every captured node in the queried range, keyed by node id.
type CaptureMap = std::collections::HashMap<usize, Capture>;

/// Run the language's `indents.scm` over `scope` and index the results
/// by node id.
///
/// **Scoped to one top-level item, and that is a hard perf
/// requirement rather than an optimisation.** An earlier revision ran
/// the query over the whole file and benched at 623 µs on an 800-line
/// buffer and 2.57 ms on a 3200-line one — linear in file size, on the
/// keystroke path, which would put `dispatch.rs` at ~30 ms per `<CR>`
/// and blow the frame by 4×.
///
/// Scoping is sound, not a trade: the only nodes ever consulted are
/// ancestors of the query position, and every ancestor except the root
/// is contained in the root's child that contains the position. The
/// root itself is never captured. So a whole-file run computes
/// thousands of captures to look up a handful.
fn capture_map(snapshot: &SyntaxSnapshot, scope: tree_sitter::Node<'_>) -> Option<CaptureMap> {
    use tree_sitter::{QueryCursor, StreamingIterator};

    let query = snapshot.registry().indents_query(snapshot.lang().name())?;
    let source = snapshot.source();

    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(scope.start_byte()..scope.end_byte());
    let mut matches = cursor.matches(query, scope, source);

    let names = query.capture_names();
    let mut map = CaptureMap::new();
    while let Some(m) = matches.next() {
        for cap in m.captures {
            let Some(name) = names.get(cap.index as usize) else {
                continue;
            };
            let Some(kind) = Capture::parse(name) else {
                continue;
            };
            map.insert(cap.node.id(), kind);
        }
    }
    Some(map)
}

/// The top-level item that governs the indent at `at`: the root child
/// containing it, or -- when `at` sits between items -- the one
/// immediately before it.
///
/// Serves two purposes at once, which is why it is one function:
///
/// 1. It bounds [`capture_map`] to a single item instead of the file.
/// 2. It is where the parse error that matters would be. Tree-sitter
///    does not represent incomplete code as "a block that happens to
///    be unclosed" -- it collapses the region into an `ERROR` node with
///    no structure inside. Parsing `fn f() {\n    x();\n\n` yields
///    exactly one `ERROR [0..17]` and **no `block` node at all**, so
///    `@indent` matches nothing and a naive engine answers zero for a
///    cursor plainly inside a function body.
///
/// The "immediately before" case is what catches that: the cursor sits
/// at byte 18, past the ERROR's end, so no *containing* item exists and
/// only the preceding sibling reveals the broken parse.
///
/// Returns `None` when the governing item is errored -- the tree has no
/// answer, and the caller falls back to the lexical bridge, which
/// handles unclosed openers correctly by construction because a bracket
/// scan does not need the block to be finished. Scoping the check this
/// way also means a syntax error elsewhere in the file does not disable
/// indentation here.
fn governing_scope(root: tree_sitter::Node<'_>, at: usize) -> Option<tree_sitter::Node<'_>> {
    // Binary search, not a scan: root children are in byte order, and a
    // linear walk makes this O(top-level items). Measured 29.7 µs at
    // 3200 lines against 8.4 µs at 80 -- still under budget, but growing,
    // and a 36k-line file is one people actually open. Searching flattens
    // it to 9.5 µs.
    //
    // This is the same bug TS.1 hit and fixed (benchmarks.md): a linear
    // `0..child_count` walk at the root of a large file, measured there
    // at 175 µs. It is an easy shape to write and invisible without a
    // size sweep, which is why both benches keep their row.
    let count = root.child_count() as u32;
    let (mut lo, mut hi) = (0u32, count);
    let mut best: Option<tree_sitter::Node<'_>> = None;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let Some(child) = root.child(mid) else { break };
        if child.start_byte() <= at {
            best = Some(child);
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    // `best` is the last child starting at or before `at` — the one
    // containing it, or the one immediately before when `at` sits in
    // the gap between items.
    let scope = best.unwrap_or(root);
    if scope.has_error() || scope.is_error() || scope.is_missing() {
        return None;
    }
    Some(scope)
}

/// The chain of nodes containing `at`, deepest first.
fn ancestor_chain(snapshot: &SyntaxSnapshot, at: usize) -> Option<Vec<tree_sitter::Node<'_>>> {
    let tree = snapshot.tree()?;
    let at = at.min(snapshot.source().len());
    let mut node = tree.root_node().descendant_for_byte_range(at, at)?;
    let mut chain = vec![node];
    while let Some(parent) = node.parent() {
        chain.push(parent);
        node = parent;
    }
    Some(chain)
}

/// Indent level, in steps, for a **new** line inserted at byte `at`.
///
/// Counts the `@indent` nodes that are genuinely *open* at `at`:
/// `start_byte < at < end_byte`. Both bounds are strict, and the upper
/// one matters — with `at <= end_byte`, a cursor sitting just past a
/// block's closing brace (`fn f() {}|`) would still count the block and
/// indent the next line by one.
///
/// Returns `None` when there is no tree or no query for the language,
/// which is the caller's signal to use the lexical bridge.
pub fn tree_levels_for_new_line(snapshot: &SyntaxSnapshot, at: usize) -> Option<i32> {
    let at = at.min(snapshot.source().len());
    // IN.4: inside a string, indentation is CONTENT. A Python
    // docstring, a Bash heredoc, a Rust raw string or a JS template
    // literal all carry their leading whitespace as data, so applying
    // the enclosing block's structural indent would silently edit the
    // string's value -- a correctness bug, not a cosmetic one.
    //
    // Declining hands off to the lexical bridge, which copies the
    // previous line's indent. That is also what vim does inside a
    // string, so the behaviour is both safer and unsurprising.
    if snapshot.cursor_in_string_scope(at) {
        return None;
    }
    let scope = governing_scope(snapshot.tree()?.root_node(), at)?;
    let chain = ancestor_chain(snapshot, at)?;
    let map = capture_map(snapshot, scope)?;

    let mut level = 0i32;
    let mut counted_rows = std::collections::HashSet::new();
    for node in chain {
        let Some(kind) = map.get(&node.id()) else {
            continue;
        };
        let open = node.start_byte() < at && at < node.end_byte();
        match kind {
            Capture::Indent if open => {
                // Collapse siblings opening on the same row: `foo(bar(`
                // is one level, not two.
                if counted_rows.insert(node.start_position().row) {
                    level += 1;
                }
            }
            Capture::IndentAlways if open => level += 1,
            // Outdent captures describe a line that STARTS with the
            // node; a newly created empty line starts with nothing, so
            // they contribute nothing here. The moved tail's leading
            // closer is handled by the caller.
            _ => {}
        }
    }
    Some(level.max(0))
}

/// Indent level, in steps, for the **existing** line `row`.
///
/// Used by `=` and electric reindent (IN.6 / IN.7). Row-based rather
/// than byte-based because the question is different: a block that
/// *starts* on this row must not indent it, and a closer that starts on
/// this row must dedent it.
pub fn tree_levels_for_line(snapshot: &SyntaxSnapshot, row: u32) -> Option<i32> {
    let source = snapshot.source();
    let (line_start, line_end) = line_bounds(source, row)?;
    // Anchor on the line's first non-whitespace byte: that is the token
    // whose alignment is being decided, and it is what `@outdent` has
    // to match against.
    let at = (line_start..line_end)
        .find(|i| !matches!(source.get(*i), Some(b' ') | Some(b'\t')))
        .unwrap_or(line_start);

    let scope = governing_scope(snapshot.tree()?.root_node(), at)?;
    let chain = ancestor_chain(snapshot, at)?;
    let map = capture_map(snapshot, scope)?;

    let mut level = 0i32;
    let mut counted_rows = std::collections::HashSet::new();
    for node in chain {
        let Some(kind) = map.get(&node.id()) else {
            continue;
        };
        let start_row = node.start_position().row as u32;
        let end_row = node.end_position().row as u32;
        match kind {
            Capture::Indent if start_row < row && row <= end_row => {
                if counted_rows.insert(start_row) {
                    level += 1;
                }
            }
            Capture::IndentAlways if start_row < row && row <= end_row => level += 1,
            Capture::Outdent | Capture::OutdentAlways if start_row == row => level -= 1,
            _ => {}
        }
    }
    Some(level.max(0))
}

/// Byte bounds of `row`, excluding its newline.
fn line_bounds(source: &[u8], row: u32) -> Option<(usize, usize)> {
    let mut start = 0usize;
    let mut seen = 0u32;
    for (i, b) in source.iter().enumerate() {
        if seen == row {
            break;
        }
        if *b == b'\n' {
            seen += 1;
            start = i + 1;
        }
    }
    if seen < row {
        return None;
    }
    let end = source[start..]
        .iter()
        .position(|b| *b == b'\n')
        .map(|o| start + o)
        .unwrap_or(source.len());
    Some((start, end))
}

/// Per-language bracket sets for the opener/closer scan.
///
/// Empty sets are meaningful, not a null case: they are how a language
/// with no bracket-block notion opts out of the scan and gets pure
/// `keep` behaviour from the same code path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BracketSyntax {
    pub openers: &'static [u8],
    pub closers: &'static [u8],
}

impl BracketSyntax {
    /// `{`, `(`, `[` -- the C-family set, correct for every bundled
    /// language whose blocks are brace-delimited.
    pub const BRACES: Self = Self {
        openers: b"{([",
        closers: b"})]",
    };

    /// No brackets: the scan is skipped entirely and the bridge
    /// behaves as `keep`.
    pub const NONE: Self = Self {
        openers: b"",
        closers: b"",
    };

    /// The bracket sets for a language.
    ///
    /// Prose languages get [`Self::NONE`] so a stray brace in a
    /// sentence does not indent the next line. Everything else gets
    /// [`Self::BRACES`] -- including the indent-sensitive languages
    /// (Python, YAML), where brackets still bound continuation lines
    /// even though blocks are not brace-delimited, which is exactly
    /// the case the scan gets right.
    pub fn for_lang(lang: Lang) -> Self {
        match lang {
            Lang::Plain | Lang::Markdown => Self::NONE,
            _ => Self::BRACES,
        }
    }

    fn is_empty(&self) -> bool {
        self.openers.is_empty() && self.closers.is_empty()
    }

    /// Net bracket depth `line` leaves open. Negative when the line
    /// closes more than it opens.
    fn net_depth(&self, line: &str) -> i32 {
        let mut depth = 0i32;
        for b in line.bytes() {
            if self.openers.contains(&b) {
                depth += 1;
            } else if self.closers.contains(&b) {
                depth -= 1;
            }
        }
        depth
    }

    /// Whether `line`'s first non-whitespace byte is a closer.
    ///
    /// Public because the tree path needs it too: at query time the
    /// closer about to move down with the cursor is still on the line
    /// above, so the tree cannot see it and the caller applies the
    /// dedent itself.
    pub fn starts_with_closer(&self, line: &str) -> bool {
        line.bytes()
            .find(|b| *b != b' ' && *b != b'\t')
            .is_some_and(|b| self.closers.contains(&b))
    }
}

/// The whitespace a newly created line should start with.
///
/// `prev` is the text that ends up ABOVE the new line, which differs
/// by the key that created it:
///
/// - `o` / `O` -- the nearest non-blank line. (Vim takes `O`'s indent
///   from the line it pushes down, which is the line the cursor is on,
///   so both use the same source.)
/// - `<CR>` -- the **head**, i.e. the text before the cursor, not the
///   whole line. In `foo(a, |b)` the whole line is bracket-balanced
///   while the head leaves `(` open; only the head gives the right
///   answer.
///
/// `None` means there is nothing above, i.e. the top of the buffer.
///
/// `next` is the text that will follow on the new line, used only for
/// the closer check. Pass `None` for the ordinary
/// create-an-empty-line case; pass the tail for `<CR>` pressed
/// mid-line, where the text after the cursor moves down with it and a
/// leading `}` should dedent.
///
/// Returns the whitespace string, not a column count, because the
/// caller splices it into an edit and the rendering (tabs vs spaces)
/// is the unit's business.
pub fn indent_for_new_line(
    method: IndentMethod,
    prev: Option<&str>,
    next: Option<&str>,
    unit: IndentUnit,
    brackets: BracketSyntax,
) -> String {
    let columns = indent_columns_for_new_line(method, prev, next, unit, brackets);
    unit.render(columns)
}

/// [`indent_for_new_line`] in display columns, before rendering.
/// Separate so IN.2's engine can compare its own answer against the
/// fallback's without allocating.
pub fn indent_columns_for_new_line(
    method: IndentMethod,
    prev: Option<&str>,
    next: Option<&str>,
    unit: IndentUnit,
    brackets: BracketSyntax,
) -> u16 {
    if matches!(method, IndentMethod::None) {
        return 0;
    }
    let Some(prev) = prev else { return 0 };
    let base = unit.columns_of(prev);

    // `Keep` is a pure copy -- see the module doc. `Syntax` reaching
    // here means the tree was unavailable, and the bracket scan is the
    // best guess left.
    if matches!(method, IndentMethod::Keep) || brackets.is_empty() {
        return base;
    }

    let mut columns = base;
    if brackets.net_depth(prev) > 0 {
        columns = unit.shift(columns, 1);
    }
    if next.is_some_and(|n| brackets.starts_with_closer(n)) {
        columns = unit.shift(columns, -1);
    }
    columns
}

// ──────────────────────────────────────────────────────────────
// IN.6 — electric reindent
// ──────────────────────────────────────────────────────────────

/// Words that, when they *begin* a line, put it one level shallower.
///
/// The punctuation closers (`}`, `)`, `]`) are already handled by
/// [`BracketSyntax::starts_with_closer`]; this covers the languages
/// that close with words instead, plus the continuation keywords
/// (`else`, `elif`) that sit back at the construct's level while the
/// body carries on.
fn dedent_keywords(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::Lua => &["end", "until", "else", "elseif"],
        Lang::Ruby => &["end", "else", "elsif", "when", "rescue", "ensure"],
        Lang::Bash => &["fi", "done", "esac", "else", "elif"],
        // Python's suite has no closer, but these continuation
        // keywords do step back out of the preceding block.
        Lang::Python => &["else", "elif", "except", "finally"],
        _ => &[],
    }
}

/// The first whitespace-delimited word of `line`, if any.
fn leading_word(line: &str) -> &str {
    let t = line.trim_start();
    let end = t
        .find(|c: char| !c.is_alphanumeric() && c != '_')
        .unwrap_or(t.len());
    &t[..end]
}

/// Whether typing `typed` at the end of `line_head` should re-indent
/// the current line.
///
/// Two triggers, matching the two ways a language closes a block:
///
/// - `typed` is a bracket closer and nothing but whitespace precedes
///   it on the line. The "nothing but whitespace" part matters — a `}`
///   at the end of `let x = Foo { a };` must not re-indent the line.
/// - `line_head + typed` completes a dedent keyword that begins the
///   line. Checked as a whole word so `end` fires but `append` and
///   `defenders` do not.
pub fn is_electric_trigger(lang: Lang, line_head: &str, typed: char) -> bool {
    let brackets = BracketSyntax::for_lang(lang);
    let mut buf = [0u8; 4];
    let typed_str = typed.encode_utf8(&mut buf);

    if brackets.closers.contains(&(typed as u32 as u8)) && line_head.trim().is_empty() {
        return true;
    }

    let words = dedent_keywords(lang);
    if words.is_empty() {
        return false;
    }
    // The keyword must be the whole of the line's content so far --
    // otherwise `x = end` or a trailing `else` in a comment would fire.
    let candidate = format!("{}{}", line_head.trim_start(), typed_str);
    words.contains(&candidate.as_str())
        && line_head.trim_start() == &candidate[..candidate.len() - typed_str.len()]
}

/// The indent, in columns, for a line being electrically re-indented.
///
/// **Deliberately lexical, not tree-driven**, and that follows from
/// IN.2's finding rather than being a shortcut. At the instant the
/// closer is typed, two things are true at once: the published snapshot
/// has not caught up with the edit, and the code around the cursor is
/// half-written — so a *fresh* parse would produce an `ERROR` node with
/// no block structure and the engine would decline anyway. There is no
/// version of this that the tree can answer, so asking it would cost a
/// parse to learn nothing.
///
/// It reuses [`indent_columns_for_new_line`] with the current line
/// passed as `next`: "the indent a new line here would get, given what
/// this line starts with" is exactly the question, so no second
/// algorithm is needed. Word closers subtract the extra level the
/// bracket scan cannot see.
pub fn electric_columns(
    lang: Lang,
    prev_nonblank: Option<&str>,
    line: &str,
    unit: IndentUnit,
) -> u16 {
    let brackets = BracketSyntax::for_lang(lang);
    let mut columns = indent_columns_for_new_line(
        IndentMethod::Syntax,
        prev_nonblank,
        Some(line),
        unit,
        brackets,
    );
    if !brackets.starts_with_closer(line) && dedent_keywords(lang).contains(&leading_word(line)) {
        columns = unit.shift(columns, -1);
    }
    columns
}

#[cfg(test)]
mod electric_tests {
    use super::*;

    fn unit() -> IndentUnit {
        IndentUnit::new(4, true, 4)
    }

    #[test]
    fn a_closer_typed_alone_on_a_line_triggers() {
        assert!(is_electric_trigger(Lang::Rust, "        ", '}'));
        assert!(is_electric_trigger(Lang::Rust, "", ')'));
    }

    #[test]
    fn a_closer_after_content_does_not_trigger() {
        // `let x = Foo { a };` — the `}` closes an inline literal and
        // must not re-indent the line the user is writing.
        assert!(!is_electric_trigger(
            Lang::Rust,
            "    let x = Foo { a ",
            '}'
        ));
    }

    #[test]
    fn word_closers_trigger_only_as_whole_words() {
        assert!(is_electric_trigger(Lang::Lua, "  en", 'd'));
        assert!(is_electric_trigger(Lang::Ruby, "  en", 'd'));
        assert!(is_electric_trigger(Lang::Bash, "  f", 'i'));
        // `append` must not fire on its final `d`... it does not even
        // end in one, so use a real near-miss: `bend`.
        assert!(!is_electric_trigger(Lang::Lua, "  ben", 'd'));
        // Nor mid-expression.
        assert!(!is_electric_trigger(Lang::Lua, "  x = en", 'd'));
    }

    #[test]
    fn languages_without_word_closers_only_trigger_on_brackets() {
        assert!(!is_electric_trigger(Lang::Rust, "  en", 'd'));
        assert!(is_electric_trigger(Lang::Rust, "  ", '}'));
    }

    #[test]
    fn a_closing_brace_lands_at_the_openers_level() {
        // The canonical case: `}` typed under an over-indented body
        // snaps back to the `if`'s level.
        let cols = electric_columns(Lang::Rust, Some("        y();"), "        }", unit());
        assert_eq!(cols, 4);
    }

    #[test]
    fn a_closer_directly_under_its_opener_lands_at_the_openers_level() {
        let cols = electric_columns(Lang::Rust, Some("fn f() {"), "}", unit());
        assert_eq!(cols, 0);
    }

    #[test]
    fn word_closers_dedent_from_the_body() {
        let cols = electric_columns(Lang::Lua, Some("    y()"), "    end", unit());
        assert_eq!(cols, 0);
        let cols = electric_columns(Lang::Bash, Some("    y"), "    fi", unit());
        assert_eq!(cols, 0);
    }

    #[test]
    fn a_word_closer_is_not_double_counted_with_a_bracket() {
        // `}` is a bracket closer AND some languages have word
        // closers; a line starting with `}` must lose exactly one
        // level, not two.
        let cols = electric_columns(Lang::Ruby, Some("    y"), "    }", unit());
        assert_eq!(cols, 0);
    }
}

#[cfg(test)]
mod tree_tests {
    use super::*;
    use crate::syntax::Syntax;

    /// Parse `src` as Rust and return the owned snapshot the engine
    /// reads.
    fn rust(src: &str) -> crate::syntax::SyntaxSnapshot {
        let mut s = Syntax::for_language(Lang::Rust)
            .expect("rust registered")
            .expect("rust has a grammar");
        s.parse(src);
        s.snapshot_owned()
    }

    /// Level for a new line inserted at the byte marked `|` in `src`.
    fn levels_after_cursor(src_with_cursor: &str) -> Option<i32> {
        let at = src_with_cursor.find('|').expect("mark the cursor with |");
        let src = src_with_cursor.replace('|', "");
        tree_levels_for_new_line(&rust(src.as_str()), at)
    }

    #[test]
    fn a_language_without_a_query_yields_none() {
        // The signal the caller uses to fall back to the lexical
        // bridge. The contract under test is "a language with no query
        // degrades instead of failing", so this points at whatever is
        // currently uncovered -- Python at IN.2, TOML at IN.4, SQL now.
        // SQL is the stable home: it ships no query by DECISION rather
        // than by not-yet (see `indents_source`), so this should not
        // need moving again.
        let mut s = Syntax::for_language(Lang::Sql)
            .expect("sql registered")
            .expect("sql has a grammar");
        s.parse("select a\nfrom t;\n");
        assert_eq!(tree_levels_for_new_line(&s.snapshot_owned(), 9), None);
    }

    #[test]
    fn an_unparsed_snapshot_yields_none() {
        let s = Syntax::for_language(Lang::Rust).unwrap().unwrap();
        assert_eq!(tree_levels_for_new_line(&s.snapshot_owned(), 0), None);
    }

    #[test]
    fn inside_a_block_is_one_level() {
        assert_eq!(levels_after_cursor("fn f() {|}"), Some(1));
        assert_eq!(levels_after_cursor("fn f() {\n    x();|\n}\n"), Some(1));
    }

    #[test]
    fn past_the_closing_brace_is_zero() {
        // The strict upper bound. With `at <= end_byte` this returns
        // 1 and `<CR>` after a finished function indents for no
        // reason.
        assert_eq!(levels_after_cursor("fn f() {}|"), Some(0));
        assert_eq!(levels_after_cursor("fn f() {\n    x();\n}|\n"), Some(0));
    }

    #[test]
    fn before_the_opening_brace_is_zero() {
        assert_eq!(levels_after_cursor("fn f() |{}"), Some(0));
    }

    #[test]
    fn nesting_accumulates() {
        let src = "fn f() {\n    if x {\n        y();|\n    }\n}\n";
        assert_eq!(levels_after_cursor(src), Some(2));
    }

    #[test]
    fn same_row_openers_collapse_to_one_level() {
        // `foo(bar(` opens two `@indent` nodes on one row. Vim and
        // every editor worth copying indent one level there, not two.
        assert_eq!(
            levels_after_cursor("fn f() {\n    foo(bar(|))\n}\n"),
            Some(2)
        );
    }

    #[test]
    fn a_brace_inside_a_string_is_not_an_opener() {
        // The case the lexical bridge gets wrong on purpose
        // (`a_brace_inside_a_string_is_a_known_wrong_answer`). The
        // tree knows it is a string literal, so the level stays 1 --
        // this is the concrete improvement IN.2 buys.
        let level = levels_after_cursor("fn f() {\n    println!(\"{\");|\n}\n");
        assert_eq!(level, Some(1));
    }

    #[test]
    fn existing_line_levels_dedent_the_closing_brace() {
        let src = "fn f() {\n    x();\n}\n";
        let snap = rust(src);
        assert_eq!(tree_levels_for_line(&snap, 0), Some(0), "fn line");
        assert_eq!(tree_levels_for_line(&snap, 1), Some(1), "body");
        assert_eq!(tree_levels_for_line(&snap, 2), Some(0), "closing brace");
    }

    #[test]
    fn existing_line_levels_handle_nesting() {
        let src = "fn f() {\n    if x {\n        y();\n    }\n}\n";
        let snap = rust(src);
        assert_eq!(tree_levels_for_line(&snap, 1), Some(1), "if");
        assert_eq!(tree_levels_for_line(&snap, 2), Some(2), "if body");
        assert_eq!(tree_levels_for_line(&snap, 3), Some(1), "inner close");
        assert_eq!(tree_levels_for_line(&snap, 4), Some(0), "outer close");
    }

    #[test]
    fn existing_line_levels_are_independent_of_current_indentation() {
        // `=` has to fix a scrambled file, so the answer must come
        // from the tree rather than from what the line already has.
        let scrambled = "fn f() {\nx();\n            y();\n}\n";
        let snap = rust(scrambled);
        assert_eq!(tree_levels_for_line(&snap, 1), Some(1));
        assert_eq!(tree_levels_for_line(&snap, 2), Some(1));
    }

    #[test]
    fn a_row_past_the_end_yields_none() {
        assert_eq!(tree_levels_for_line(&rust("fn f() {}\n"), 99), None);
    }

    // ---- IN.3: the brace family ----

    /// Parse `src` as `lang` and return the owned snapshot.
    fn parsed(lang: Lang, src: &str) -> crate::syntax::SyntaxSnapshot {
        let mut s = Syntax::for_language(lang)
            .unwrap_or_else(|e| panic!("{lang:?} registry error: {e}"))
            .unwrap_or_else(|| panic!("{lang:?} has no grammar"));
        s.parse(src);
        s.snapshot_owned()
    }

    /// Every language whose `indents_source` returns a query must
    /// compile it. An invalid node kind does not degrade — `Query::new`
    /// rejects it and the whole registry build fails for that language,
    /// so this is the difference between "no indent for Go" and "Go
    /// buffers do not parse".
    #[test]
    fn every_shipped_indents_query_compiles() {
        let registry = crate::LangRegistry::standard().expect("registry builds");
        let shipped = [
            "rust",
            // IN.3 — brace family.
            "c",
            "cpp",
            "css",
            "go",
            "java",
            "javascript",
            "json",
            "tsx",
            "typescript",
            // IN.4 — indent-sensitive + scripting.
            "bash",
            "lua",
            "python",
            "ruby",
            // IN.5 — data + markup. `markdown` / `sql` absent by
            // decision; see `markdown_and_sql_deliberately_ship_no_query`.
            "html",
            "toml",
            "yaml",
        ];
        for name in shipped {
            assert!(
                indents_source(name).is_some(),
                "{name} listed here but absent from indents_source"
            );
            assert!(
                registry.indents_query(name).is_some(),
                "{name} ships indents.scm but the registry has no compiled query"
            );
        }
    }

    /// One nesting case per language: a body line indents, and the
    /// closing delimiter dedents back. This is the whole contract the
    /// brace-family queries exist to satisfy, and it fails loudly if a
    /// node name is right for the grammar but wrong for the construct.
    #[test]
    fn brace_family_indents_bodies_and_dedents_closers() {
        /// One language's nesting fixture.
        ///
        /// A named struct rather than a 4-tuple of pairs: the tuple
        /// tripped `type_complexity`, and `case.body_level` reads
        /// better than `case.2.1` at the assertion site regardless.
        struct Case {
            lang: Lang,
            src: &'static str,
            body_row: u32,
            body_level: i32,
            closer_row: u32,
            closer_level: i32,
        }

        // Levels are stated rather than assumed 1/0: Java's fixture
        // nests a method inside a class, so its body sits at two and
        // its inner closer at one. An assertion that hardcoded 1/0
        // would have been wrong for the right reason and taught
        // nothing.
        let cases = [
            Case {
                lang: Lang::C,
                src: "int f(void) {\n    g();\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
            Case {
                lang: Lang::Cpp,
                src: "int f() {\n    g();\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
            Case {
                lang: Lang::Java,
                src: "class A {\n    void f() {\n        g();\n    }\n}\n",
                body_row: 2,
                body_level: 2,
                closer_row: 3,
                closer_level: 1,
            },
            Case {
                lang: Lang::JavaScript,
                src: "function f() {\n    g();\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
            Case {
                lang: Lang::TypeScript,
                src: "function f(): void {\n    g();\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
            Case {
                lang: Lang::Tsx,
                src: "function f() {\n    g();\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
            Case {
                lang: Lang::Go,
                src: "func f() {\n    g()\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
            Case {
                lang: Lang::Css,
                src: "a {\n    color: red;\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
            Case {
                lang: Lang::Json,
                src: "{\n    \"a\": 1\n}\n",
                body_row: 1,
                body_level: 1,
                closer_row: 2,
                closer_level: 0,
            },
        ];
        for case in &cases {
            let snap = parsed(case.lang, case.src);
            let lang = case.lang;
            assert_eq!(
                tree_levels_for_line(&snap, case.body_row),
                Some(case.body_level),
                "{lang:?}: body line level"
            );
            assert_eq!(
                tree_levels_for_line(&snap, case.closer_row),
                Some(case.closer_level),
                "{lang:?}: closing delimiter should dedent one level"
            );
        }
    }

    /// Argument / parameter lists indent their continuations. This is
    /// the case the *lexical* bridge also gets right, so it is not
    /// proof of much on its own — but a query that captured only block
    /// bodies would fail it, and every language in the family lists its
    /// delimited-list nodes for exactly this.
    #[test]
    fn brace_family_indents_wrapped_argument_lists() {
        let cases: &[(Lang, &str)] = &[
            (Lang::C, "int f(void) {\n    g(\n        1);\n}\n"),
            (Lang::JavaScript, "function f() {\n    g(\n        1);\n}\n"),
            (Lang::Go, "func f() {\n    g(\n        1)\n}\n"),
        ];
        for (lang, src) in cases {
            let snap = parsed(*lang, src);
            assert_eq!(
                tree_levels_for_line(&snap, 2),
                Some(2),
                "{lang:?}: wrapped argument should sit under the call"
            );
        }
    }

    // ---- IN.4: indent-sensitive + scripting ----

    /// Word closers (`end`, `fi`, `done`, `esac`) dedent exactly as `}`
    /// does. This is the whole difference between the IN.4 group and
    /// the brace family, and a query that captured only the block
    /// nodes would leave every `end` hanging one level too deep.
    #[test]
    fn word_closers_dedent() {
        struct Case {
            lang: Lang,
            src: &'static str,
            body_row: u32,
            closer_row: u32,
        }
        let cases = [
            Case {
                lang: Lang::Lua,
                src: "if x then\n    y()\nend\n",
                body_row: 1,
                closer_row: 2,
            },
            Case {
                lang: Lang::Lua,
                src: "while x do\n    y()\nend\n",
                body_row: 1,
                closer_row: 2,
            },
            Case {
                lang: Lang::Ruby,
                src: "def f\n  g\nend\n",
                body_row: 1,
                closer_row: 2,
            },
            Case {
                lang: Lang::Bash,
                src: "if x; then\n    y\nfi\n",
                body_row: 1,
                closer_row: 2,
            },
            Case {
                lang: Lang::Bash,
                src: "for i in a; do\n    y\ndone\n",
                body_row: 1,
                closer_row: 2,
            },
        ];
        for case in &cases {
            let snap = parsed(case.lang, case.src);
            let lang = case.lang;
            let src = case.src;
            assert_eq!(
                tree_levels_for_line(&snap, case.body_row),
                Some(1),
                "{lang:?}: body should indent\n{src}"
            );
            assert_eq!(
                tree_levels_for_line(&snap, case.closer_row),
                Some(0),
                "{lang:?}: word closer should dedent\n{src}"
            );
        }
    }

    /// Python's colon-suite indents, and the block has no closing token
    /// -- so the line *after* the suite returns to zero because the
    /// block node ended, not because a delimiter dedented it.
    #[test]
    fn python_suite_indents_and_ends_without_a_closer() {
        let snap = parsed(Lang::Python, "def f():\n    g()\n\nh()\n");
        assert_eq!(tree_levels_for_line(&snap, 1), Some(1), "suite body");
        assert_eq!(
            tree_levels_for_line(&snap, 3),
            Some(0),
            "after the suite, with no closing token involved"
        );
    }

    /// Indentation inside a string is CONTENT. Applying the enclosing
    /// block's structural indent there would edit the string's value --
    /// a correctness bug, not a cosmetic one. The engine declines and
    /// the lexical bridge copies the previous line instead.
    #[test]
    fn the_engine_refuses_to_answer_inside_a_string() {
        // Python docstring: byte offset inside the triple-quoted body.
        let src = "def f():\n    \"\"\"doc\n    more\n    \"\"\"\n";
        let snap = parsed(Lang::Python, src);
        let inside = src.find("more").expect("fixture contains the marker");
        assert_eq!(
            tree_levels_for_new_line(&snap, inside),
            None,
            "a docstring's leading whitespace is data, not structure"
        );

        // And the ordinary case still answers, so the guard is not
        // simply disabling the engine.
        let code_at = src.find("\"\"\"doc").expect("fixture marker");
        assert!(tree_levels_for_new_line(&snap, code_at - 1).is_some());
    }

    // ---- IN.5: data + markup ----

    #[test]
    fn data_and_markup_indent_nested_structure() {
        struct Case {
            lang: Lang,
            src: &'static str,
            row: u32,
            level: i32,
        }
        let cases = [
            // TOML: a wrapped array indents; keys under a `[table]`
            // header do NOT (that is the one judgement in the query).
            Case {
                lang: Lang::Toml,
                src: "[t]\na = [\n  1,\n]\n",
                row: 2,
                level: 1,
            },
            Case {
                lang: Lang::Toml,
                src: "[t]\na = 1\n",
                row: 1,
                level: 0,
            },
            // YAML: a nested mapping indents.
            Case {
                lang: Lang::Yaml,
                src: "a:\n  b: 1\n",
                row: 1,
                level: 1,
            },
            // HTML: children indent, the closing tag dedents.
            Case {
                lang: Lang::Html,
                src: "<div>\n  <p>x</p>\n</div>\n",
                row: 1,
                level: 1,
            },
            Case {
                lang: Lang::Html,
                src: "<div>\n  <p>x</p>\n</div>\n",
                row: 2,
                level: 0,
            },
        ];
        for case in &cases {
            let snap = parsed(case.lang, case.src);
            let lang = case.lang;
            let src = case.src;
            assert_eq!(
                tree_levels_for_line(&snap, case.row),
                Some(case.level),
                "{lang:?} row {}\n{src}",
                case.row
            );
        }
    }

    /// Indentation inside a heredoc or a YAML block scalar is the
    /// VALUE. The engine must decline so the lexical bridge preserves
    /// whatever the user typed.
    ///
    /// IN.4's query header and commit message claimed this protection
    /// for heredocs before it existed — `cursor_in_string_scope`'s node
    /// list covered neither `heredoc_body` nor `block_scalar`. This
    /// test is what makes the claim true, and is written for both so a
    /// future edit to that list cannot quietly drop one.
    #[test]
    fn literal_blocks_are_data_and_the_engine_declines() {
        let bash = "cat <<EOF\n    indented data\nEOF\n";
        let snap = parsed(Lang::Bash, bash);
        let inside = bash.find("indented").expect("fixture marker");
        assert_eq!(
            tree_levels_for_new_line(&snap, inside),
            None,
            "a heredoc body's leading whitespace is data"
        );

        let yaml = "script: |\n  line one\n    line two\nnext: 1\n";
        let snap = parsed(Lang::Yaml, yaml);
        let inside = yaml.find("line two").expect("fixture marker");
        assert_eq!(
            tree_levels_for_new_line(&snap, inside),
            None,
            "a YAML block scalar's leading whitespace is the value"
        );

        // The guard must not swallow ordinary YAML: every plain scalar
        // is wrapped in a `string_scalar`, so a too-eager node list
        // would disable indentation for essentially all YAML.
        let plain = "a:\n  b: 1\n";
        let snap = parsed(Lang::Yaml, plain);
        let at = plain.find("b: 1").expect("fixture marker");
        assert!(
            tree_levels_for_new_line(&snap, at).is_some(),
            "plain scalars must NOT count as a string scope"
        );
    }

    /// `markdown` and `sql` ship no query on purpose. Asserted so the
    /// absence reads as a decision rather than an oversight, and so
    /// adding one becomes a deliberate act that updates this test.
    #[test]
    fn markdown_and_sql_deliberately_ship_no_query() {
        assert!(indents_source("markdown").is_none());
        assert!(indents_source("sql").is_none());
    }

    /// A brace inside a string is not an opener — the property that
    /// distinguishes the tree path from the lexical bridge, checked
    /// once per language family rather than only for Rust.
    #[test]
    fn brace_family_ignores_braces_inside_strings() {
        let cases: &[(Lang, &str)] = &[
            (Lang::C, "int f(void) {\n    g(\"{\");\n    h();\n}\n"),
            (
                Lang::JavaScript,
                "function f() {\n    g(\"{\");\n    h();\n}\n",
            ),
            (Lang::Go, "func f() {\n    g(\"{\")\n    h()\n}\n"),
        ];
        for (lang, src) in cases {
            let snap = parsed(*lang, src);
            assert_eq!(
                tree_levels_for_line(&snap, 2),
                Some(1),
                "{lang:?}: a brace in a string must not open a level"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> IndentUnit {
        IndentUnit::new(4, true, 4)
    }

    fn syntax_indent(prev: Option<&str>, next: Option<&str>) -> String {
        indent_for_new_line(
            IndentMethod::Syntax,
            prev,
            next,
            unit(),
            BracketSyntax::BRACES,
        )
    }

    fn keep_indent(prev: Option<&str>) -> String {
        indent_for_new_line(
            IndentMethod::Keep,
            prev,
            None,
            unit(),
            BracketSyntax::BRACES,
        )
    }

    #[test]
    fn none_is_always_column_zero() {
        assert_eq!(
            indent_for_new_line(
                IndentMethod::None,
                Some("        deeply indented"),
                None,
                unit(),
                BracketSyntax::BRACES,
            ),
            ""
        );
    }

    #[test]
    fn keep_copies_and_does_not_scan() {
        assert_eq!(keep_indent(Some("    x();")), "    ");
        // The distinguishing case: an unclosed opener. `keep` must NOT
        // add a level -- that is `smartindent`, not `autoindent`.
        assert_eq!(keep_indent(Some("    if x {")), "    ");
        assert_eq!(keep_indent(Some("no indent")), "");
        assert_eq!(keep_indent(None), "");
    }

    #[test]
    fn syntax_fallback_adds_a_level_after_an_unclosed_opener() {
        assert_eq!(syntax_indent(Some("    if x {"), None), "        ");
        assert_eq!(syntax_indent(Some("fn f() {"), None), "    ");
        // Balanced on the line: no extra level.
        assert_eq!(syntax_indent(Some("    f(a);"), None), "    ");
        assert_eq!(syntax_indent(Some("    if x { y() }"), None), "    ");
    }

    #[test]
    fn syntax_fallback_dedents_when_the_moved_tail_opens_with_a_closer() {
        // `<CR>` pressed just before a `}` that moves down with it.
        assert_eq!(syntax_indent(Some("        x();"), Some("}")), "    ");
        // Opener and closer together: they cancel.
        assert_eq!(syntax_indent(Some("    if x {"), Some("}")), "    ");
    }

    #[test]
    fn a_language_with_no_brackets_degrades_to_keep() {
        // Prose: a brace in a sentence must not indent the next line.
        let prose = indent_for_new_line(
            IndentMethod::Syntax,
            Some("  a sentence with a { in it"),
            None,
            unit(),
            BracketSyntax::NONE,
        );
        assert_eq!(prose, "  ");
        assert_eq!(BracketSyntax::for_lang(Lang::Markdown), BracketSyntax::NONE);
        assert_eq!(BracketSyntax::for_lang(Lang::Plain), BracketSyntax::NONE);
        assert_eq!(BracketSyntax::for_lang(Lang::Rust), BracketSyntax::BRACES);
    }

    #[test]
    fn indent_is_rendered_through_the_unit() {
        // noexpandtab: the copied indent comes back as a tab.
        let tabs = IndentUnit::new(4, false, 4);
        let out = indent_for_new_line(
            IndentMethod::Keep,
            Some("    x"),
            None,
            tabs,
            BracketSyntax::BRACES,
        );
        assert_eq!(out, "\t");
    }

    #[test]
    fn a_tab_indented_previous_line_is_measured_in_columns() {
        // The previous line uses a tab; expandtab is on, so the new
        // line gets the equivalent in spaces.
        assert_eq!(keep_indent(Some("\tx();")), "    ");
    }

    #[test]
    fn closing_more_than_opening_does_not_go_negative() {
        // A line that only closes: depth is negative, so no extra
        // level, and the copy is clamped at zero by `shift`.
        assert_eq!(syntax_indent(Some("}"), None), "");
        assert_eq!(syntax_indent(Some("    }"), Some("}")), "");
    }

    #[test]
    fn a_brace_inside_a_string_is_a_known_wrong_answer() {
        // Documented in the module header: the lexical scan cannot see
        // string state. Asserted so the limitation is visible in the
        // suite rather than discovered later, and so IN.2's engine has
        // a concrete case to prove it improves on.
        assert_eq!(
            syntax_indent(Some(r#"    println!("{");"#), None),
            "        ",
            "lexical scan counts a brace in a string literal"
        );
    }
}
