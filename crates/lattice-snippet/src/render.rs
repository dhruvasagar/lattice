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
    /// Whether this range came from a `Choice` placeholder.
    /// The host's choice-picker UI fires on these.
    pub is_choice: bool,
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
}

/// Render the parsed body to text + tabstop ranges.
pub fn render(body: &SnippetBody, vars: &VariableContext) -> RenderedSnippet {
    let mut out = String::new();
    let mut tabstops: Vec<TabstopRange> = Vec::new();
    walk(&body.tokens, vars, &mut out, &mut tabstops);
    let exit_index = tabstops.iter().position(|t| t.index == 0);
    RenderedSnippet {
        text: out,
        tabstops,
        exit_index,
    }
}

fn walk(
    tokens: &[SnippetToken],
    vars: &VariableContext,
    out: &mut String,
    tabstops: &mut Vec<TabstopRange>,
) {
    for token in tokens {
        match token {
            SnippetToken::Literal(s) => {
                out.push_str(s);
            }
            SnippetToken::Tabstop(idx) => {
                let start = out.len();
                tabstops.push(TabstopRange {
                    index: *idx,
                    range: start..start,
                    has_default: false,
                    is_choice: false,
                });
            }
            SnippetToken::Placeholder { idx, default } => {
                let start = out.len();
                walk(&default.tokens, vars, out, tabstops);
                let end = out.len();
                tabstops.push(TabstopRange {
                    index: *idx,
                    range: start..end,
                    has_default: !default.is_empty(),
                    is_choice: false,
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
                    is_choice: true,
                });
            }
            SnippetToken::Variable { name, default } => {
                if let Some(value) = vars.resolve(name) {
                    out.push_str(&value);
                } else if let Some(default) = default {
                    walk(&default.tokens, vars, out, tabstops);
                }
                // else: variable unknown + no default -> emit
                // nothing. Matches VS Code behaviour.
            }
            SnippetToken::Transform { target, .. } => {
                // v1: render as the bound text un-transformed.
                // Tabstop targets emit nothing yet (they'll
                // re-render once the user types into the bound
                // tabstop in 4.2.g.7); variable targets emit
                // the variable's value.
                match target {
                    TransformTarget::Variable(name) => {
                        if let Some(v) = vars.resolve(name) {
                            out.push_str(&v);
                        }
                    }
                    TransformTarget::Tabstop(_) => {
                        // No bound text yet at render time;
                        // emit nothing. Future polish: re-
                        // render after each placeholder edit.
                    }
                }
            }
        }
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
        assert!(r.tabstops[0].is_choice);
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

    #[test]
    fn unknown_variable_with_no_default_emits_nothing() {
        let body = parse::parse("foo$NOPE bar").unwrap();
        let r = render(&body, &VariableContext::default());
        assert_eq!(r.text, "foo bar");
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
