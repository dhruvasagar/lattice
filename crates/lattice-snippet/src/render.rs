//! Render a parsed [`SnippetBody`] to literal text plus
//! per-tabstop byte ranges. Output feeds the editor's edit
//! pipeline (insert the text, then track the ranges as the
//! user types into placeholders).
//!
//! Variables are resolved against the supplied
//! [`VariableContext`] -- unknown variables fall back to their
//! `default` body when present, else the empty string. Choice
//! placeholders render their first option as the initial
//! placeholder text; the host's choice-picker UI lets the
//! user swap.
//!
//! Transformations parse but render as the bound text un-
//! transformed in v1; full regex support lands as polish.
//! Snippet bodies in the wild rarely use transformations.

use std::collections::BTreeMap;

use crate::token::{SnippetBody, SnippetToken, TransformTarget};
use crate::variables::VariableContext;

/// Byte range a tabstop covers in the rendered text. Multiple
/// occurrences of the same tabstop index produce one
/// [`TabstopRange`] per occurrence -- they're "mirrors"; edits
/// in one ripple to the others.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabstopRange {
    /// Tabstop index. `0` is the snippet's exit position.
    pub index: u32,
    /// Byte range in the rendered text.
    pub range: std::ops::Range<usize>,
    /// Whether this range was a `Placeholder` with default
    /// text vs a bare `Tabstop`. Affects the host's "select
    /// placeholder text on focus" UX (placeholders auto-
    /// select their default for easy overwrite; bare tabstops
    /// just place the cursor).
    pub has_default: bool,
    /// The options of a `Choice` placeholder (`${1|a,b,c|}`), in body
    /// order; empty for any other range. The first is inserted; `<C-n>` /
    /// `<C-p>` cycle the rest in place.
    pub choices: Vec<String>,
    /// A transformed copy of `$index` (`${1/(.*)/${1:/upcase}/}`): its text
    /// is this transform of the placeholder's, rewritten when the user
    /// leaves the placeholder (VS Code's rule). Not a stop of its own.
    pub transform: Option<crate::transform::TransformSpec>,
}

/// Output of [`render`]. Contains the literal text the editor
/// inserts plus the tabstop ranges grouped by index for easy
/// navigation.
#[derive(Debug, Clone)]
pub struct RenderedSnippet {
    /// Text the editor splices into the buffer.
    pub text: String,
    /// All tabstop ranges in render order (`$1`'s ranges
    /// before `$2`'s, etc.).
    pub tabstops: Vec<TabstopRange>,
    /// Index of the `$0` (exit) tabstop in `tabstops`, when
    /// the body included one.
    pub exit_index: Option<usize>,
}

impl RenderedSnippet {
    /// Group tabstop ranges by index. Returns a sorted map so
    /// the host can iterate `$1` -> `$2` -> ... -> `$0` in
    /// order, with each group containing all mirrors for that
    /// index.
    pub fn grouped_by_index(&self) -> BTreeMap<u32, Vec<&TabstopRange>> {
        let mut out: BTreeMap<u32, Vec<&TabstopRange>> = BTreeMap::new();
        for r in &self.tabstops {
            out.entry(r.index).or_default().push(r);
        }
        out
    }

    /// This snippet laid out for a line indented by `base`, with `level`
    /// as one indent step — the shape VS Code and the LSP's
    /// `adjustIndentation` give a body (snippet-integrity SI.6).
    ///
    /// Snippet bodies are written flush-left with `\t` for each nesting
    /// level, for whichever editor reads them. So every line after the
    /// first gets `base` (the trigger line's indentation) in front, and
    /// each leading `\t` becomes `level` (spaces under `expandtab`). The
    /// first line is not touched: it continues the trigger line.
    ///
    /// Placeholder ranges are remapped with the text. A placeholder at
    /// the start of a continuation line lands after the new indentation,
    /// where the cursor belongs. An empty continuation line stays empty —
    /// no trailing whitespace — unless a placeholder sits on it.
    pub fn reindented(&self, base: &str, level: &str) -> RenderedSnippet {
        if !self.text.contains('\n') && !self.text.starts_with('\t')
            || base.is_empty() && level == "\t"
        {
            return self.clone();
        }
        let has_stop_at = |offset: usize| self.tabstops.iter().any(|t| t.range.start == offset);
        // One entry per line: (old start, new start of its content after
        // the added indentation, leading tabs, the prefix length added).
        let mut lines: Vec<(usize, usize, usize, usize)> = Vec::new();
        let mut text = String::with_capacity(self.text.len() + base.len() * 4);
        let mut old_start = 0usize;
        for (i, line) in self.text.split('\n').enumerate() {
            if i > 0 {
                text.push('\n');
            }
            let tabs = line.bytes().take_while(|b| *b == b'\t').count();
            let indent = i > 0 && (!line.is_empty() || has_stop_at(old_start));
            let prefix_start = text.len();
            if indent {
                text.push_str(base);
            }
            for _ in 0..tabs {
                text.push_str(level);
            }
            lines.push((old_start, prefix_start, tabs, text.len() - prefix_start));
            text.push_str(&line[tabs..]);
            old_start += line.len() + 1;
        }
        let map = |offset: usize| -> usize {
            let idx = lines
                .partition_point(|(start, ..)| *start <= offset)
                .saturating_sub(1);
            let (start, new_start, tabs, added) = lines[idx];
            let rel = offset - start;
            if rel <= tabs {
                // In the leading tabs (or at the line start): after the
                // base indentation and `rel` levels.
                new_start + (added - tabs * level.len()) + rel * level.len()
            } else {
                new_start + added + (rel - tabs)
            }
        };
        RenderedSnippet {
            text,
            tabstops: self
                .tabstops
                .iter()
                .map(|t| TabstopRange {
                    range: map(t.range.start)..map(t.range.end),
                    ..t.clone()
                })
                .collect(),
            exit_index: self.exit_index,
        }
    }
}

/// Render the parsed body to text + tabstop ranges.
pub fn render(body: &SnippetBody, vars: &VariableContext) -> RenderedSnippet {
    let mut out = String::new();
    let mut tabstops: Vec<TabstopRange> = Vec::new();
    let mut defaults = BTreeMap::new();
    collect_defaults(&body.tokens, &mut defaults);
    let mut next_free = max_index(&body.tokens) + 1;
    walk(
        &body.tokens,
        vars,
        &defaults,
        &mut out,
        &mut tabstops,
        &mut next_free,
    );
    let exit_index = tabstops.iter().position(|t| t.index == 0);
    RenderedSnippet {
        text: out,
        tabstops,
        exit_index,
    }
}

/// What a mirror of `$N` shows: the text of the first placeholder or
/// choice that gives `$N` one.
enum MirrorText<'a> {
    Body(&'a SnippetBody),
    Text(&'a str),
}

/// The first default each tabstop index is given anywhere in the body
/// (nested placeholders included), so its bare `$N` mirrors can show it:
/// `$1 + $1 = ${1:two}` renders `two + two = two`, as in VS Code. Before
/// snippet-integrity SI.7 the mirrors rendered empty.
fn collect_defaults<'a>(tokens: &'a [SnippetToken], out: &mut BTreeMap<u32, MirrorText<'a>>) {
    for token in tokens {
        match token {
            SnippetToken::Placeholder { idx, default } => {
                if !default.is_empty() {
                    out.entry(*idx).or_insert(MirrorText::Body(default));
                }
                collect_defaults(&default.tokens, out);
            }
            SnippetToken::Choice { idx, options } => {
                if let Some(first) = options.first() {
                    out.entry(*idx).or_insert(MirrorText::Text(&first.text));
                }
            }
            SnippetToken::Variable {
                default: Some(default),
                ..
            } => collect_defaults(&default.tokens, out),
            _ => {}
        }
    }
}

fn walk(
    tokens: &[SnippetToken],
    vars: &VariableContext,
    defaults: &BTreeMap<u32, MirrorText<'_>>,
    out: &mut String,
    tabstops: &mut Vec<TabstopRange>,
    next_free: &mut u32,
) {
    for token in tokens {
        match token {
            SnippetToken::Literal(s) => {
                out.push_str(s);
            }
            SnippetToken::Tabstop(idx) => {
                // A mirror shows its placeholder's text — as plain text: the
                // stops nested in that default belong to the placeholder,
                // not to each copy of it.
                let start = out.len();
                match defaults.get(idx) {
                    Some(MirrorText::Body(body)) => {
                        walk(&body.tokens, vars, defaults, out, &mut Vec::new(), &mut 0);
                    }
                    Some(MirrorText::Text(text)) => out.push_str(text),
                    None => {}
                }
                let end = out.len();
                tabstops.push(TabstopRange {
                    index: *idx,
                    range: start..end,
                    has_default: end > start,
                    choices: Vec::new(),
                    transform: None,
                });
            }
            SnippetToken::Placeholder { idx, default } => {
                let start = out.len();
                walk(&default.tokens, vars, defaults, out, tabstops, next_free);
                let end = out.len();
                tabstops.push(TabstopRange {
                    index: *idx,
                    range: start..end,
                    has_default: !default.is_empty(),
                    choices: Vec::new(),
                    transform: None,
                });
            }
            SnippetToken::Choice { idx, options } => {
                let start = out.len();
                if let Some(first) = options.first() {
                    out.push_str(&first.text);
                }
                let end = out.len();
                tabstops.push(TabstopRange {
                    index: *idx,
                    range: start..end,
                    has_default: !options.is_empty(),
                    choices: options.iter().map(|o| o.text.clone()).collect(),
                    transform: None,
                });
            }
            SnippetToken::Variable { name, default } => {
                if let Some(value) = vars.resolve(name) {
                    if name == "TM_SELECTED_TEXT" {
                        push_at_line_indent(out, &value);
                    } else {
                        out.push_str(&value);
                    }
                } else if let Some(default) = default {
                    walk(&default.tokens, vars, defaults, out, tabstops, next_free);
                } else if !vars.is_known(name) {
                    // An unknown variable becomes a placeholder holding its
                    // name, after every numbered stop (the LSP rule) — a
                    // typo'd `$TM_FILNAME` is visible and fixable rather than
                    // silently empty. A known one with no value (no
                    // selection for `$TM_SELECTED_TEXT`) inserts nothing.
                    let start = out.len();
                    out.push_str(name);
                    tabstops.push(TabstopRange {
                        index: *next_free,
                        range: start..out.len(),
                        has_default: true,
                        choices: Vec::new(),
                        transform: None,
                    });
                    *next_free += 1;
                }
            }
            SnippetToken::Transform {
                target,
                pattern,
                replacement,
                flags,
            } => {
                let spec = crate::transform::TransformSpec {
                    pattern: pattern.clone(),
                    format: replacement.clone(),
                    options: flags.clone(),
                };
                match target {
                    TransformTarget::Variable(name) => {
                        out.push_str(&spec.apply(&vars.resolve(name).unwrap_or_default()));
                    }
                    TransformTarget::Tabstop(idx) => {
                        // The transform of the placeholder's default now; the
                        // session rewrites it when the user leaves `$idx`.
                        let mut source = String::new();
                        match defaults.get(idx) {
                            Some(MirrorText::Body(body)) => {
                                walk(
                                    &body.tokens,
                                    vars,
                                    defaults,
                                    &mut source,
                                    &mut Vec::new(),
                                    &mut 0,
                                );
                            }
                            Some(MirrorText::Text(text)) => source.push_str(text),
                            None => {}
                        }
                        let start = out.len();
                        out.push_str(&spec.apply(&source));
                        tabstops.push(TabstopRange {
                            index: *idx,
                            range: start..out.len(),
                            has_default: false,
                            choices: Vec::new(),
                            transform: Some(spec),
                        });
                    }
                }
            }
        }
    }
}

/// Append a multi-line `value` so each line after the first starts at the
/// indentation of the body line it lands on — VS Code's rule for the
/// selection, which is what makes `if ($1) {\n\t$TM_SELECTED_TEXT\n}`
/// indent every wrapped line, not just the first. The value arrives
/// relative to its own first line ([`crate::selection_relative_to`]), so
/// deeper lines keep their extra depth. Empty lines stay empty.
fn push_at_line_indent(out: &mut String, value: &str) {
    let line_start = out.rfind('\n').map_or(0, |i| i + 1);
    let indent: String = out[line_start..]
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    for (i, line) in value.split('\n').enumerate() {
        if i > 0 {
            out.push('\n');
            if !line.is_empty() {
                out.push_str(&indent);
            }
        }
        out.push_str(line);
    }
}

/// The highest tabstop index the body uses, so unknown variables can be
/// numbered after it.
fn max_index(tokens: &[SnippetToken]) -> u32 {
    tokens
        .iter()
        .map(|t| match t {
            SnippetToken::Tabstop(i) | SnippetToken::Choice { idx: i, .. } => *i,
            SnippetToken::Placeholder { idx, default } => (*idx).max(max_index(&default.tokens)),
            SnippetToken::Variable {
                default: Some(d), ..
            } => max_index(&d.tokens),
            SnippetToken::Transform {
                target: TransformTarget::Tabstop(i),
                ..
            } => *i,
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod reindent_tests {
    use super::*;
    use crate::parse;

    fn rendered(body: &str) -> RenderedSnippet {
        render(&parse::parse(body).unwrap(), &VariableContext::default())
    }

    /// Each stop's text in `r`.
    fn stops(r: &RenderedSnippet) -> Vec<(u32, &str)> {
        r.tabstops
            .iter()
            .map(|t| (t.index, &r.text[t.range.clone()]))
            .collect()
    }

    #[test]
    fn continuation_lines_take_the_base_indent_and_tabs_become_levels() {
        let r = rendered("if ${1:cond} {\n\t$0\n}").reindented("    ", "    ");
        assert_eq!(r.text, "if cond {\n        \n    }");
        assert_eq!(stops(&r), vec![(1, "cond"), (0, "")]);
        // `$0` sits after the two levels of indentation on line 2.
        let zero = r.tabstops.iter().find(|t| t.index == 0).unwrap();
        assert_eq!(zero.range.start, "if cond {\n        ".len());
    }

    #[test]
    fn with_tabs_kept_only_the_base_is_added() {
        let r = rendered("fn ${1:f}() {\n\t${2:todo!()}\n}").reindented("\t", "\t");
        assert_eq!(r.text, "fn f() {\n\t\ttodo!()\n\t}");
        assert_eq!(stops(&r), vec![(1, "f"), (2, "todo!()")]);
    }

    #[test]
    fn an_empty_line_stays_empty_unless_a_stop_is_on_it() {
        let r = rendered("a\n\nb").reindented("  ", "  ");
        assert_eq!(r.text, "a\n\n  b");
        let r = rendered("a\n$1\nb").reindented("  ", "  ");
        assert_eq!(r.text, "a\n  \n  b");
        assert_eq!(r.tabstops[0].range, 4..4, "the stop is after the indent");
    }

    /// SI.7: a bare `$N` mirror shows its placeholder's text, wherever the
    /// placeholder is, and is a stop of its own range.
    #[test]
    fn mirrors_show_their_placeholders_text() {
        let r = rendered("$1 + $1 = ${1:two}");
        assert_eq!(r.text, "two + two = two");
        assert_eq!(stops(&r), vec![(1, "two"), (1, "two"), (1, "two")]);
        assert!(r.tabstops.iter().all(|t| t.has_default));
        let r = rendered("${1|yes,no|} or $1");
        assert_eq!(r.text, "yes or yes");
    }

    #[test]
    fn a_single_line_body_is_untouched() {
        let r = rendered("for ${1:i} in ${2:iter}");
        assert_eq!(r.reindented("        ", "    ").text, r.text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    fn render_str(s: &str) -> RenderedSnippet {
        let body = parse::parse(s).unwrap();
        render(&body, &VariableContext::default())
    }

    #[test]
    fn pure_literal_renders_verbatim() {
        let r = render_str("hello world");
        assert_eq!(r.text, "hello world");
        assert!(r.tabstops.is_empty());
    }

    #[test]
    fn tabstop_emits_zero_width_range() {
        let r = render_str("foo$1bar");
        assert_eq!(r.text, "foobar");
        assert_eq!(r.tabstops.len(), 1);
        assert_eq!(r.tabstops[0].index, 1);
        assert_eq!(r.tabstops[0].range, 3..3);
        assert!(!r.tabstops[0].has_default);
    }

    #[test]
    fn placeholder_emits_default_text_in_range() {
        let r = render_str("for ${1:i} in ${2:iter}");
        assert_eq!(r.text, "for i in iter");
        assert_eq!(r.tabstops.len(), 2);
        assert_eq!(r.tabstops[0].index, 1);
        assert_eq!(r.tabstops[0].range, 4..5); // "i"
        assert!(r.tabstops[0].has_default);
        assert_eq!(r.tabstops[1].index, 2);
        assert_eq!(r.tabstops[1].range, 9..13); // "iter"
    }

    #[test]
    fn final_tabstop_is_marked_in_exit_index() {
        let r = render_str("foo$0bar");
        assert_eq!(r.text, "foobar");
        assert_eq!(r.exit_index, Some(0));
        assert_eq!(r.tabstops[0].index, 0);
    }

    #[test]
    fn choice_renders_first_option_initially() {
        let r = render_str("Hello ${1|world,Earth,planet|}!");
        assert_eq!(r.text, "Hello world!");
        assert_eq!(r.tabstops[0].range, 6..11); // "world"
        assert_eq!(r.tabstops[0].choices, ["world", "Earth", "planet"]);
    }

    #[test]
    fn variable_resolves_via_context() {
        let body = parse::parse("$TM_FILENAME").unwrap();
        let ctx = VariableContext {
            filename: Some("foo.rs".into()),
            ..Default::default()
        };
        let r = render(&body, &ctx);
        assert_eq!(r.text, "foo.rs");
    }

    #[test]
    fn variable_falls_back_to_default_when_unset() {
        let body = parse::parse("${TM_FILENAME:fallback.txt}").unwrap();
        let r = render(&body, &VariableContext::default());
        assert_eq!(r.text, "fallback.txt");
    }

    /// SI.8: an unknown variable becomes a placeholder holding its name,
    /// numbered after the body's own stops (the LSP rule); it used to vanish.
    #[test]
    fn an_unknown_variable_becomes_a_placeholder_of_its_name() {
        let body = parse::parse("foo$NOPE bar$1").unwrap();
        let r = render(&body, &VariableContext::default());
        assert_eq!(r.text, "fooNOPE bar");
        let nope = r
            .tabstops
            .iter()
            .find(|t| t.index == 2)
            .expect("after `$1`");
        assert_eq!(&r.text[nope.range.clone()], "NOPE");
        assert!(nope.has_default);
    }

    /// A known variable with no value inserts nothing — not a placeholder.
    #[test]
    fn a_known_variable_without_a_value_emits_nothing() {
        let body = parse::parse("foo$TM_SELECTED_TEXT bar").unwrap();
        let r = render(&body, &VariableContext::default());
        assert_eq!(r.text, "foo bar");
        assert!(r.tabstops.is_empty());
    }

    /// SI.10: a multi-line selection's later lines start at the indentation
    /// of the body line it is placed on; empty lines stay empty, and other
    /// variables are inserted as they are.
    #[test]
    fn a_multi_line_selection_follows_its_body_lines_indentation() {
        let vars = VariableContext {
            selected_text: Some("a();\n    b();\n\nc();".into()),
            clipboard: Some("x\ny".into()),
            ..VariableContext::default()
        };
        let body = parse::parse("{\n\t$TM_SELECTED_TEXT\n}").unwrap();
        assert_eq!(
            render(&body, &vars).text,
            "{\n\ta();\n\t    b();\n\n\tc();\n}"
        );
        let body = parse::parse("\t$CLIPBOARD").unwrap();
        assert_eq!(render(&body, &vars).text, "\tx\ny");
    }

    /// SI.8: a variable transform is applied at render.
    #[test]
    fn a_variable_transform_is_applied() {
        let body = parse::parse("${TM_FILENAME/(.*)\\.rs$/${1:/pascalcase}/}").unwrap();
        let vars = VariableContext {
            filename: Some("my_module.rs".into()),
            ..VariableContext::default()
        };
        assert_eq!(render(&body, &vars).text, "MyModule");
    }

    /// SI.8: a tabstop transform renders its placeholder's default
    /// transformed, as a non-navigable occurrence carrying the transform.
    #[test]
    fn a_tabstop_transform_renders_from_its_placeholder() {
        let body = parse::parse("${1:name} ${1/(.*)/${1:/upcase}/}").unwrap();
        let r = render(&body, &VariableContext::default());
        assert_eq!(r.text, "name NAME");
        let copy = r
            .tabstops
            .iter()
            .find(|t| t.transform.is_some())
            .expect("the copy");
        assert_eq!(copy.index, 1);
        assert_eq!(&r.text[copy.range.clone()], "NAME");
    }

    #[test]
    fn nested_placeholder_default_renders_inner_text() {
        let r = render_str("${1:outer ${2:inner} more}");
        assert_eq!(r.text, "outer inner more");
        // Two tabstop ranges: inner $2 emitted before outer
        // $1 because we walk default body first.
        assert_eq!(r.tabstops.len(), 2);
        assert_eq!(r.tabstops[0].index, 2); // inner first (walked first)
        assert_eq!(r.tabstops[0].range, 6..11); // "inner"
        assert_eq!(r.tabstops[1].index, 1); // outer wraps
        assert_eq!(r.tabstops[1].range, 0..16);
    }

    #[test]
    fn grouped_by_index_collects_mirrors() {
        // Two `$1` mirrors -- common in for-loops where the
        // counter appears in both header and body.
        let r = render_str("for $1 = 0; $1 < n; $1++");
        let groups = r.grouped_by_index();
        let group_one = groups.get(&1).expect("$1 group");
        assert_eq!(group_one.len(), 3);
    }

    #[test]
    fn render_round_trips_friendly_snippets_for_loop_shape() {
        let body = parse::parse("for ${1:i} in ${2:iter} {\n\t$0\n}").unwrap();
        let r = render(&body, &VariableContext::default());
        assert_eq!(r.text, "for i in iter {\n\t\n}");
        assert_eq!(r.tabstops.len(), 3);
        assert_eq!(r.exit_index, Some(2));
    }
}
