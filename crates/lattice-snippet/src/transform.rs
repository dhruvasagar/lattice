//! Snippet transforms: `${1/(.*)/${1:/upcase}/g}`, `${TM_FILENAME/[.]rs$//}`.
//!
//! A transform runs a regular expression over a value — a variable's, or
//! the text of the placeholder a tabstop mirrors — and rewrites each match
//! with a *format*: literal text mixed with references to the match's
//! capture groups, optionally case-converted or chosen by whether the group
//! matched. The grammar is the LSP specification's (VS Code's):
//!
//! ```text
//! format   ::= ( text | '$' int | '${' int '}'
//!              | '${' int ':/' ( 'upcase' | 'downcase' | 'capitalize'
//!                             | 'camelcase' | 'pascalcase' ) '}'
//!              | '${' int ':+' if '}'          -- if the group matched
//!              | '${' int ':?' if ':' else '}' -- one or the other
//!              | '${' int ':-' else '}'        -- if it did not
//!              | '${' int ':' else '}' )*      -- the same
//! options  ::= [gim]*                          -- global, ignore case, multi-line
//! ```
//!
//! Patterns are compiled with the `regex` crate, which is JavaScript's
//! syntax for everything snippet packs use but lookaround. A pattern that
//! does not compile leaves the value unchanged — a snippet is never refused
//! for its transform.

use regex::{Captures, RegexBuilder};

/// A transform as a snippet body carries it: `${N/pattern/format/options}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransformSpec {
    pub pattern: String,
    pub format: String,
    pub options: String,
}

impl TransformSpec {
    /// `value` rewritten by this transform ([`apply`]).
    pub fn apply(&self, value: &str) -> String {
        apply(value, &self.pattern, &self.format, &self.options)
    }
}

/// Rewrite `value` by `pattern` → `format` with `options` (`g`, `i`, `m`).
/// Without `g` only the first match is rewritten; text outside matches is
/// kept, as in JavaScript's `String.prototype.replace`.
pub fn apply(value: &str, pattern: &str, format: &str, options: &str) -> String {
    let regex = match RegexBuilder::new(pattern)
        .case_insensitive(options.contains('i'))
        .multi_line(options.contains('m'))
        .build()
    {
        Ok(regex) => regex,
        Err(e) => {
            tracing::debug!(
                target: "lattice_snippet::transform",
                pattern,
                error = %e,
                "snippet transform pattern does not compile; value left as is"
            );
            return value.to_string();
        }
    };
    let format = parse_format(format);
    let render = |caps: &Captures<'_>| -> String {
        let mut out = String::new();
        for part in &format {
            part.render(caps, &mut out);
        }
        out
    };
    if options.contains('g') {
        regex.replace_all(value, render).into_owned()
    } else {
        regex.replace(value, render).into_owned()
    }
}

/// One piece of a parsed format.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Text(String),
    Group(usize),
    Case(usize, Case),
    IfElse {
        group: usize,
        matched: String,
        unmatched: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Case {
    Upcase,
    Downcase,
    Capitalize,
    Camelcase,
    Pascalcase,
}

impl Part {
    fn render(&self, caps: &Captures<'_>, out: &mut String) {
        let group = |n: usize| caps.get(n).map(|m| m.as_str());
        match self {
            Part::Text(t) => out.push_str(t),
            Part::Group(n) => out.push_str(group(*n).unwrap_or_default()),
            Part::Case(n, case) => out.push_str(&convert(group(*n).unwrap_or_default(), *case)),
            Part::IfElse {
                group: n,
                matched,
                unmatched,
            } => match group(*n) {
                Some(g) if !g.is_empty() => out.push_str(matched),
                _ => out.push_str(unmatched),
            },
        }
    }
}

fn convert(s: &str, case: Case) -> String {
    match case {
        Case::Upcase => s.to_uppercase(),
        Case::Downcase => s.to_lowercase(),
        Case::Capitalize => {
            let mut chars = s.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        }
        Case::Camelcase | Case::Pascalcase => {
            let mut out = String::new();
            for (i, word) in s
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .enumerate()
            {
                let mut chars = word.chars();
                if let Some(first) = chars.next() {
                    if i == 0 && case == Case::Camelcase {
                        out.extend(first.to_lowercase());
                    } else {
                        out.extend(first.to_uppercase());
                    }
                    out.push_str(chars.as_str());
                }
            }
            out
        }
    }
}

/// Parse a format string. Total: anything that is not a well-formed
/// reference is text, so a stray `$` or `}` is inserted as written.
fn parse_format(format: &str) -> Vec<Part> {
    let chars: Vec<char> = format.chars().collect();
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                text.push(match chars[i + 1] {
                    'n' => '\n',
                    't' => '\t',
                    other => other,
                });
                i += 2;
            }
            '$' => match reference(&chars, i) {
                Some((part, next)) => {
                    if !text.is_empty() {
                        parts.push(Part::Text(std::mem::take(&mut text)));
                    }
                    parts.push(part);
                    i = next;
                }
                None => {
                    text.push('$');
                    i += 1;
                }
            },
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    if !text.is_empty() {
        parts.push(Part::Text(text));
    }
    parts
}

/// The reference starting at the `$` at `at`, and where it ends.
fn reference(chars: &[char], at: usize) -> Option<(Part, usize)> {
    let digits = |from: usize| -> Option<(usize, usize)> {
        let end = (from..chars.len())
            .find(|&j| !chars[j].is_ascii_digit())
            .unwrap_or(chars.len());
        let n: String = chars.get(from..end)?.iter().collect();
        n.parse().ok().map(|n| (n, end))
    };
    if chars.get(at + 1) != Some(&'{') {
        let (n, end) = digits(at + 1)?;
        return Some((Part::Group(n), end));
    }
    let (n, mut j) = digits(at + 2)?;
    match chars.get(j)? {
        '}' => return Some((Part::Group(n), j + 1)),
        ':' => j += 1,
        _ => return None,
    }
    // The rest up to the matching `}`, with `\}` escapes.
    let mut body = String::new();
    let mut k = j;
    loop {
        match chars.get(k)? {
            '\\' => {
                body.push(*chars.get(k + 1)?);
                k += 2;
            }
            '}' => break,
            c => {
                body.push(*c);
                k += 1;
            }
        }
    }
    let end = k + 1;
    let part = if let Some(name) = body.strip_prefix('/') {
        let case = match name {
            "upcase" => Case::Upcase,
            "downcase" => Case::Downcase,
            "capitalize" => Case::Capitalize,
            "camelcase" => Case::Camelcase,
            "pascalcase" => Case::Pascalcase,
            _ => return None,
        };
        Part::Case(n, case)
    } else if let Some(matched) = body.strip_prefix('+') {
        Part::IfElse {
            group: n,
            matched: matched.to_string(),
            unmatched: String::new(),
        }
    } else if let Some(rest) = body.strip_prefix('?') {
        let (matched, unmatched) = rest.split_once(':').unwrap_or((rest, ""));
        Part::IfElse {
            group: n,
            matched: matched.to_string(),
            unmatched: unmatched.to_string(),
        }
    } else {
        let unmatched = body.strip_prefix('-').unwrap_or(&body);
        Part::IfElse {
            group: n,
            matched: String::new(),
            unmatched: unmatched.to_string(),
        }
    };
    Some((part, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_and_case_conversion() {
        assert_eq!(apply("main.rs", "(.*)\\.rs", "${1:/upcase}", ""), "MAIN");
        assert_eq!(
            apply("hello world", "(.*)", "${1:/capitalize}", ""),
            "Hello world"
        );
        assert_eq!(
            apply("my-file_name", "(.*)", "${1:/pascalcase}", ""),
            "MyFileName"
        );
        assert_eq!(
            apply("my-file_name", "(.*)", "${1:/camelcase}", ""),
            "myFileName"
        );
        assert_eq!(apply("ABC", "(.*)", "${1:/downcase}", ""), "abc");
        assert_eq!(apply("ab", "(a)(b)", "$2$1", ""), "ba");
        assert_eq!(apply("ab", "(a)(b)", "${2}-${1}", ""), "b-a");
    }

    #[test]
    fn conditionals_follow_whether_the_group_matched() {
        let f = "${1:+yes}${1:-no}";
        assert_eq!(apply("x", "(x)?", f, ""), "yes");
        assert_eq!(apply("y", "(x)?", f, ""), "noy");
        assert_eq!(apply("x", "(x)?", "${1:?one:other}", ""), "one");
        assert_eq!(apply("y", "(x)?", "${1:?one:other}", ""), "othery");
        assert_eq!(apply("y", "(x)?", "${1:fallback}", ""), "fallbacky");
    }

    #[test]
    fn options_global_and_ignore_case() {
        assert_eq!(apply("a-b-c", "-", "_", ""), "a_b-c");
        assert_eq!(apply("a-b-c", "-", "_", "g"), "a_b_c");
        assert_eq!(apply("ABC", "b", "x", "i"), "AxC");
    }

    #[test]
    fn text_outside_the_match_is_kept_and_no_match_is_identity() {
        assert_eq!(apply("foo.rs", "\\.rs$", "", ""), "foo");
        assert_eq!(apply("foo", "bar", "baz", ""), "foo");
    }

    #[test]
    fn a_bad_pattern_leaves_the_value_unchanged() {
        assert_eq!(apply("foo", "(", "x", ""), "foo");
    }

    #[test]
    fn stray_dollars_and_braces_are_text() {
        assert_eq!(apply("a", "a", "$ ${x} }", ""), "$ ${x} }");
        assert_eq!(apply("a", "a", "\\$1", ""), "$1");
    }
}
