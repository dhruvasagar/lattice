//! Native inline variable (RF.4f, refactor.md §9.1): `cri{motion}` over a
//! variable's name, at its declaration or at a use, replaces every use with
//! the value and deletes the declaration, in one replacement.
//!
//! What a name means comes from `locals.scm` (the editor's, through
//! `language-query`): a reference resolves to the visible definition in the
//! innermost scope around it. Visible means its declaration has ended, so
//! the `x` in `let x = x + 1` is the outer one. What the code around it
//! does comes from the plugin's `refactor.scm`: which declarations bind one
//! name to one value, which names are written to, and which values run code
//! or make a new object.
//!
//! Inline is refused, never guessed, when copying the value would change
//! what the code does:
//!
//! - the variable is assigned again (each use would read a different
//!   value);
//! - a name the value reads means something else at a use, or is assigned
//!   after the declaration;
//! - the value runs code or makes a new object (a call, `new`, a list or
//!   object literal, a closure) and is used more than once, or its one use
//!   sits below a hoist barrier (in a branch, a loop, a closure), so it
//!   would run a different number of times;
//! - a format string may read the name with no node for it (Rust's
//!   `"{x}"`), so a use would be missed.
//!
//! A value whose root is an operator is parenthesised at each use, unless
//! the use is a whole argument, statement or right-hand side, where no
//! operator can bind tighter.

use crate::lattice::plugin_host::buffer::Document;
use crate::lattice::plugin_host::tree_sitter::{Query, TreeSnapshot};
use crate::span::{advance, text, trim_list_edges, Lines, Pos, Span};

/// The facts, as plain spans, and the decision on them: tested without a
/// tree.
pub mod plan {
    use crate::extract::facts::reason;
    use crate::span::{innermost, Pos, Span};

    /// A declaration of one name with one value.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Binding {
        pub whole: Span,
        pub name: Span,
        pub value: Span,
    }

    /// What `locals.scm` and `refactor.scm` say about the file.
    #[derive(Debug, Default)]
    pub struct Facts {
        pub scopes: Vec<Span>,
        pub definitions: Vec<(Span, String)>,
        /// Net of definitions and members (the locals contract's rule).
        pub references: Vec<(Span, String)>,
        pub bindings: Vec<Binding>,
        pub assigned: Vec<(Span, String)>,
        pub effects: Vec<Span>,
        pub compounds: Vec<Span>,
        pub shorthands: Vec<Span>,
        /// With their text.
        pub opaque: Vec<(Span, String)>,
        pub declarator_only: Vec<Span>,
        pub barriers: Vec<(Span, String)>,
    }

    /// One use to replace.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct Use {
        pub span: Span,
        /// `S { x }` / `{ x }`: the use names a field too.
        pub shorthand: bool,
    }

    /// What inlining comes to.
    #[derive(Debug, PartialEq, Eq)]
    pub struct Plan {
        pub binding: Binding,
        pub name: String,
        pub uses: Vec<Use>,
        /// The value's root is an operator.
        pub compound: bool,
    }

    impl Facts {
        /// The innermost scope around `at`; `None` is the file.
        fn scope_at(&self, at: Span) -> Option<Span> {
            innermost(self.scopes.iter().copied().filter(|s| s.covers(at)))
        }

        /// Where a definition becomes visible: the end of its declaration,
        /// or of the name when it has none (a parameter, a pattern).
        fn visible_from(&self, def: Span) -> Pos {
            self.bindings
                .iter()
                .find(|b| b.name == def)
                .map_or(def.end, |b| b.whole.end)
        }

        /// What `name` means at `at`: the visible definition in the
        /// innermost scope, the latest when one scope has several. `None`
        /// when nothing in the file defines it there (a global, a builtin).
        pub fn resolve(&self, name: &str, at: Span) -> Option<Span> {
            let file = (
                Pos { line: 0, byte: 0 },
                Pos {
                    line: u32::MAX,
                    byte: u32::MAX,
                },
            );
            self.definitions
                .iter()
                .filter(|(d, n)| n == name && self.visible_from(*d) <= at.start)
                .filter_map(|(d, _)| {
                    let scope = self.scope_at(*d);
                    scope.is_none_or(|s| s.covers(at)).then_some((*d, scope))
                })
                // Innermost scope first: the latest start, then the earliest
                // end. The file is outermost.
                .max_by_key(|(d, scope)| {
                    let (start, end) = scope.map_or(file, |s| (s.start, s.end));
                    (
                        scope.is_some(),
                        start,
                        std::cmp::Reverse(end),
                        self.visible_from(*d),
                    )
                })
                .map(|(d, _)| d)
        }

        /// Whether writing `name` at `site` changes what `def` holds: an
        /// assignment to a reference that resolves to it, or (where binding
        /// is assignment, as in Python) another definition in its scope.
        fn writes(&self, def: Option<Span>, name: &str, site: Span) -> bool {
            if Some(site) == def {
                return false;
            }
            if self.definitions.iter().any(|(d, _)| *d == site) {
                return def.is_some_and(|d| self.scope_at(d) == self.scope_at(site));
            }
            self.resolve(name, site) == def
        }
    }

    /// Plan inlining the variable `target` names.
    pub fn plan(target: Span, f: &Facts) -> Result<Plan, String> {
        let named = f
            .definitions
            .iter()
            .chain(&f.references)
            .filter(|(s, _)| s.covers(target))
            .min_by_key(|(s, _)| {
                (
                    s.end.line - s.start.line,
                    s.end.byte.wrapping_sub(s.start.byte),
                )
            });
        let Some((at, name)) = named else {
            return Err("not a variable's name".into());
        };
        let def = if f.definitions.iter().any(|(d, _)| d == at) {
            *at
        } else {
            f.resolve(name, *at)
                .ok_or_else(|| format!("`{name}` is not defined in this file"))?
        };
        let binding = f.bindings.iter().copied().find(|b| b.name == def).ok_or_else(|| {
            format!(
                "`{name}` is not a variable declared with one value (a parameter, a pattern, a `mut` binding)"
            )
        })?;
        let other = f
            .definitions
            .iter()
            .find(|(d, _)| *d != def && binding.whole.covers(*d));
        if let Some((d, n)) = other {
            return Err(if binding.value.covers(*d) {
                format!("its value defines `{n}` as well")
            } else {
                format!("its declaration also declares `{n}`")
            });
        }
        if f.declarator_only.contains(&binding.value) {
            return Err("its value only means something in the declaration".into());
        }

        let uses: Vec<Use> = f
            .references
            .iter()
            .filter(|(r, n)| {
                n == name && binding.whole.end <= r.start && f.resolve(name, *r) == Some(def)
            })
            .map(|(r, _)| Use {
                span: *r,
                shorthand: f.shorthands.contains(r),
            })
            .collect();
        // Before "no uses": a format string's read is a use with no node.
        let scope = f.scope_at(def);
        let after = |s: Span| binding.whole.end <= s.start && scope.is_none_or(|sc| sc.covers(s));
        if f.opaque.iter().any(|(s, t)| after(*s) && names(t, name)) {
            return Err(format!(
                "`{name}` may be read by name inside a format string, which inline cannot see"
            ));
        }
        if uses.is_empty() {
            return Err(format!("`{name}` has no uses to inline"));
        }

        if f.assigned
            .iter()
            .any(|(a, n)| n == name && f.writes(Some(def), name, *a))
        {
            return Err(format!(
                "`{name}` is assigned again, so its uses read different values"
            ));
        }

        for (r, read) in f
            .references
            .iter()
            .filter(|(r, _)| binding.value.covers(*r))
        {
            let here = f.resolve(read, *r);
            if uses.iter().any(|u| f.resolve(read, u.span) != here) {
                return Err(format!(
                    "its value reads `{read}`, which means something else at a use"
                ));
            }
            let written = f.assigned.iter().any(|(a, n)| {
                n == read && binding.whole.start < a.start && f.writes(here, read, *a)
            });
            if written {
                return Err(format!(
                    "its value reads `{read}`, which is assigned after the declaration"
                ));
            }
        }

        if f.effects.iter().any(|e| binding.value.covers(*e)) {
            if uses.len() > 1 {
                return Err(format!(
                    "its value runs code or makes a new object, and `{name}` is used {} times: inlined, it would run that often",
                    uses.len()
                ));
            }
            let barrier = f
                .barriers
                .iter()
                .find(|(b, _)| !b.covers(binding.whole) && uses.iter().any(|u| b.covers(u.span)));
            if let Some((_, why)) = barrier {
                return Err(format!(
                    "its value runs code or makes a new object, and the use {}",
                    reason(why).trim_start_matches("it ")
                ));
            }
        }

        Ok(Plan {
            binding,
            name: name.clone(),
            uses,
            compound: f.compounds.contains(&binding.value),
        })
    }

    /// Whether `text` contains `name` as a whole word.
    pub fn names(text: &str, name: &str) -> bool {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        text.match_indices(name).any(|(i, _)| {
            let before = text[..i].chars().next_back();
            let after = text[i + name.len()..].chars().next();
            !before.is_some_and(word) && !after.is_some_and(word)
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn sp(l0: u32, b0: u32, l1: u32, b1: u32) -> Span {
            Span {
                start: Pos { line: l0, byte: b0 },
                end: Pos { line: l1, byte: b1 },
            }
        }
        fn n(s: &str) -> String {
            s.to_string()
        }

        /// `let x = 1;` on line 1, then `f(x)` and `g(x)` on lines 2 and 3,
        /// in a scope over lines 0–4.
        fn simple() -> Facts {
            Facts {
                scopes: vec![sp(0, 0, 4, 1)],
                definitions: vec![(sp(1, 8, 1, 9), n("x"))],
                references: vec![(sp(2, 6, 2, 7), n("x")), (sp(3, 6, 3, 7), n("x"))],
                bindings: vec![Binding {
                    whole: sp(1, 4, 1, 14),
                    name: sp(1, 8, 1, 9),
                    value: sp(1, 12, 1, 13),
                }],
                ..Facts::default()
            }
        }

        #[test]
        fn from_the_declaration_or_a_use_the_plan_is_the_same() {
            let f = simple();
            let at_def = plan(sp(1, 8, 1, 9), &f).unwrap();
            let at_use = plan(sp(3, 6, 3, 7), &f).unwrap();
            assert_eq!(at_def, at_use);
            assert_eq!(at_def.uses.len(), 2);
        }

        #[test]
        fn a_shadowing_definition_takes_the_uses_after_it() {
            let mut f = simple();
            // An inner scope on line 3 declares its own `x` before the use.
            f.scopes.push(sp(3, 0, 3, 20));
            f.definitions.push((sp(3, 2, 3, 3), n("x")));
            let p = plan(sp(1, 8, 1, 9), &f).unwrap();
            assert_eq!(
                p.uses.iter().map(|u| u.span).collect::<Vec<_>>(),
                vec![sp(2, 6, 2, 7)]
            );
        }

        #[test]
        fn a_declaration_is_not_visible_inside_its_own_value() {
            // `let x = x + 1;`: the `x` in the value is not this one.
            let f = Facts {
                definitions: vec![(sp(0, 4, 0, 5), n("x"))],
                references: vec![(sp(0, 8, 0, 9), n("x"))],
                bindings: vec![Binding {
                    whole: sp(0, 0, 0, 14),
                    name: sp(0, 4, 0, 5),
                    value: sp(0, 8, 0, 13),
                }],
                ..Facts::default()
            };
            assert_eq!(f.resolve("x", sp(0, 8, 0, 9)), None);
        }

        #[test]
        fn an_assignment_to_the_variable_refuses() {
            let mut f = simple();
            f.assigned.push((sp(2, 6, 2, 7), n("x")));
            assert!(plan(sp(1, 8, 1, 9), &f)
                .unwrap_err()
                .contains("assigned again"));
        }

        #[test]
        fn a_second_definition_in_the_same_scope_refuses_where_binding_assigns() {
            // Python: `x = 1`, a use, `x = 2`. Both are definitions captured
            // as assigned, in one scope.
            let mut f = simple();
            f.definitions.push((sp(3, 0, 3, 1), n("x")));
            f.assigned.push((sp(3, 0, 3, 1), n("x")));
            f.assigned.push((sp(1, 8, 1, 9), n("x")));
            assert!(plan(sp(1, 8, 1, 9), &f)
                .unwrap_err()
                .contains("assigned again"));
        }

        #[test]
        fn a_redeclaration_that_is_not_an_assignment_is_shadowing() {
            // Rust: a second `let x` in the same block shadows; the first
            // keeps the uses before it.
            let mut f = simple();
            f.definitions.push((sp(3, 0, 3, 1), n("x")));
            let p = plan(sp(1, 8, 1, 9), &f).unwrap();
            assert_eq!(p.uses.len(), 1);
        }

        #[test]
        fn a_value_reading_a_name_assigned_later_refuses() {
            let mut f = simple();
            f.definitions.push((sp(0, 2, 0, 3), n("w")));
            f.references.push((sp(1, 12, 1, 13), n("w")));
            f.assigned.push((sp(2, 0, 2, 1), n("w")));
            f.references.push((sp(2, 0, 2, 1), n("w")));
            let why = plan(sp(1, 8, 1, 9), &f).unwrap_err();
            assert!(
                why.contains("`w`") && why.contains("assigned after"),
                "{why}"
            );
        }

        #[test]
        fn a_value_reading_a_name_shadowed_at_a_use_refuses() {
            let mut f = simple();
            f.references.push((sp(1, 12, 1, 13), n("w")));
            f.scopes.push(sp(3, 0, 3, 20));
            f.definitions.push((sp(3, 2, 3, 3), n("w")));
            let why = plan(sp(1, 8, 1, 9), &f).unwrap_err();
            assert!(why.contains("means something else"), "{why}");
        }

        #[test]
        fn an_effect_used_twice_refuses_and_once_inlines() {
            let mut f = simple();
            f.effects.push(sp(1, 12, 1, 13));
            assert!(plan(sp(1, 8, 1, 9), &f).unwrap_err().contains("2 times"));
            f.references.pop();
            assert!(plan(sp(1, 8, 1, 9), &f).is_ok());
        }

        #[test]
        fn an_effect_used_once_below_a_barrier_refuses() {
            let mut f = simple();
            f.references.pop();
            f.effects.push(sp(1, 12, 1, 13));
            f.barriers.push((sp(2, 0, 2, 10), n("loop")));
            let why = plan(sp(1, 8, 1, 9), &f).unwrap_err();
            assert!(why.contains("the use is in a loop"), "{why}");
            // A barrier around the declaration too changes nothing.
            f.barriers = vec![(sp(0, 0, 4, 1), n("closure"))];
            assert!(plan(sp(1, 8, 1, 9), &f).is_ok());
        }

        #[test]
        fn no_binding_no_uses_and_two_names_each_say_so() {
            let mut f = simple();
            f.bindings.clear();
            assert!(plan(sp(1, 8, 1, 9), &f)
                .unwrap_err()
                .contains("not a variable declared"));
            let mut f = simple();
            f.references.clear();
            assert!(plan(sp(1, 8, 1, 9), &f).unwrap_err().contains("no uses"));
            let mut f = simple();
            f.definitions.push((sp(1, 10, 1, 11), n("y")));
            assert!(plan(sp(1, 8, 1, 9), &f)
                .unwrap_err()
                .contains("also declares `y`"));
        }

        #[test]
        fn a_name_in_a_format_string_refuses() {
            let mut f = simple();
            f.opaque.push((sp(2, 10, 2, 20), n("\"{x} and {xs}\"")));
            assert!(plan(sp(1, 8, 1, 9), &f)
                .unwrap_err()
                .contains("format string"));
            f.opaque = vec![(sp(2, 10, 2, 20), n("\"{xs}\""))];
            assert!(plan(sp(1, 8, 1, 9), &f).is_ok(), "`xs` is not `x`");
        }

        #[test]
        fn whole_words_only() {
            assert!(names("a x b", "x"));
            assert!(names("{x}", "x"));
            assert!(!names("xs", "x"));
            assert!(!names("_x", "x"));
            assert!(!names("éx", "x"));
        }
    }
}

/// Inline the variable `range` names: the range replaced and its new text,
/// and where the caret goes (the first use).
pub fn inline(
    tree: &TreeSnapshot,
    doc: &Document,
    range: Span,
) -> Result<(Span, String, Pos), String> {
    let language = tree.language();
    let query = crate::extract::refactor_query(tree, &language)?;
    let facts = gather(tree, doc, &query)?;
    let p = plan::plan(trim_list_edges(doc, range), &facts)?;

    let value = text(doc, p.binding.value);
    let removal = removal(doc, p.binding.whole);
    let mut edits: Vec<(Span, String)> = vec![(removal, String::new())];
    for u in &p.uses {
        let value = if !p.compound || stands_alone(tree, u.span, &facts) {
            value.clone()
        } else {
            format!("({value})")
        };
        let new = if u.shorthand {
            format!("{}: {value}", p.name)
        } else {
            value
        };
        edits.push((u.span, new));
    }
    edits.sort_by_key(|(s, _)| s.start);

    let region = Span {
        start: edits[0].0.start,
        end: edits
            .iter()
            .map(|(s, _)| s.end)
            .max()
            .unwrap_or(removal.end),
    };
    let old = text(doc, region);
    let mut out = String::with_capacity(old.len());
    let mut at = 0usize;
    let mut cursor = region.start;
    for (span, new) in &edits {
        let a = offset(region.start, &old, span.start);
        out.push_str(&old[at..a]);
        if span.start == p.uses[0].span.start {
            cursor = advance(region.start, &out);
        }
        out.push_str(new);
        at = offset(region.start, &old, span.end);
    }
    out.push_str(&old[at..]);
    Ok((region, out, cursor))
}

/// What the two queries say about the file. Inline is a one-off command, so
/// it reads the whole file once: resolving a name needs every scope a use,
/// or a name the value reads, could be in.
fn gather(tree: &TreeSnapshot, doc: &Document, query: &Query) -> Result<plan::Facts, String> {
    let Some(locals) = tree.language_query("locals") else {
        return Err(format!(
            "no locals.scm for {} — a language server can do this",
            tree.language()
        ));
    };
    let mut f = plan::Facts::default();
    let mut not_reads = Vec::new();
    let mut refs = Vec::new();
    for cap in tree.run_query_ranges(&locals, None) {
        let span = Span::from(cap.range);
        match cap.name.as_str() {
            "local.scope" => f.scopes.push(span),
            "local.definition" => {
                not_reads.push(span);
                f.definitions.push((span, text(doc, span)));
            }
            "local.member" => not_reads.push(span),
            "local.reference" => refs.push(span),
            _ => {}
        }
    }
    f.references = refs
        .into_iter()
        .filter(|r| !not_reads.contains(r))
        .map(|r| (r, text(doc, r)))
        .collect();

    // A binding's three captures arrive together, stamped with one match.
    type Parts = (u32, [Option<Span>; 3]);
    fn flush(parts: &mut Option<Parts>, f: &mut plan::Facts) {
        if let Some((_, [Some(whole), Some(name), Some(value)])) = parts.take() {
            f.bindings.push(plan::Binding { whole, name, value });
        }
    }
    let mut parts: Option<Parts> = None;
    for cap in tree.run_query_ranges(query, None) {
        let span = Span::from(cap.range);
        if let Some(part) = cap.name.strip_prefix("refactor.binding") {
            if parts.as_ref().is_some_and(|(m, _)| *m != cap.match_index) {
                flush(&mut parts, &mut f);
            }
            let slots = &mut parts.get_or_insert((cap.match_index, [None; 3])).1;
            match part {
                "" => slots[0] = Some(span),
                ".name" => slots[1] = Some(span),
                ".value" => slots[2] = Some(span),
                _ => {}
            }
            continue;
        }
        match cap.name.as_str() {
            "refactor.assigned" => f.assigned.push((span, text(doc, span))),
            "refactor.effect" => f.effects.push(span),
            "refactor.compound" => f.compounds.push(span),
            "refactor.shorthand" => f.shorthands.push(span),
            "refactor.opaque" => f.opaque.push((span, text(doc, span))),
            "refactor.declarator-only" => f.declarator_only.push(span),
            other => {
                if let Some(why) = other.strip_prefix("refactor.hoist-barrier") {
                    f.barriers
                        .push((span, why.trim_start_matches('.').to_string()));
                }
            }
        }
    }
    flush(&mut parts, &mut f);
    Ok(f)
}

/// Whether a use needs no parentheses whatever the value: it is a whole
/// argument, already parenthesised, a whole statement, or the whole value
/// of another declaration.
fn stands_alone(tree: &TreeSnapshot, use_span: Span, f: &plan::Facts) -> bool {
    if f.bindings.iter().any(|b| b.value == use_span) {
        return true;
    }
    let mut node = tree.node_at(use_span.start.into());
    while let Some(n) = node {
        let span = Span::from(n.byte_range());
        if span == use_span {
            let Some(parent) = n.parent() else {
                return true;
            };
            let kind = parent.kind();
            return kind.ends_with("arguments")
                || kind.ends_with("argument_list")
                || kind == "parenthesized_expression"
                || kind == "expression_statement";
        }
        if !use_span.covers(span) {
            return false;
        }
        node = n.parent();
    }
    false
}

/// What deleting the declaration removes: its whole lines when nothing else
/// is on them, else the declaration and the blanks after it.
fn removal(doc: &Document, whole: Span) -> Span {
    let first = doc.line_text(whole.start.line).unwrap_or_default();
    let last = doc.line_text(whole.end.line).unwrap_or_default();
    let before = &first[..(whole.start.byte as usize).min(first.len())];
    let after = &last[(whole.end.byte as usize).min(last.len())..];
    if before.trim().is_empty() && after.trim().is_empty() {
        return Span {
            start: Pos {
                line: whole.start.line,
                byte: 0,
            },
            end: Pos {
                line: whole.end.line + 1,
                byte: 0,
            },
        };
    }
    let blanks = after.len() - after.trim_start().len();
    Span {
        start: whole.start,
        end: Pos {
            line: whole.end.line,
            byte: whole.end.byte + blanks as u32,
        },
    }
}

/// The byte offset of `p` in `text`, which starts at `start`.
fn offset(start: Pos, text: &str, p: Pos) -> usize {
    let mut at = 0usize;
    for _ in start.line..p.line {
        at += text[at..].find('\n').map_or(text.len() - at, |i| i + 1);
    }
    let col = if p.line == start.line {
        p.byte - start.byte
    } else {
        p.byte
    };
    (at + col as usize).min(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::innermost;

    #[test]
    fn offset_counts_lines_from_the_region_start() {
        let t = "ab\ncde\nf";
        let s = Pos { line: 4, byte: 3 };
        assert_eq!(offset(s, t, Pos { line: 4, byte: 4 }), 1);
        assert_eq!(offset(s, t, Pos { line: 5, byte: 2 }), 5);
        assert_eq!(offset(s, t, Pos { line: 6, byte: 1 }), 8);
    }

    #[test]
    fn innermost_prefers_the_latest_start() {
        let sp = |a: u32, b: u32, c: u32, d: u32| Span {
            start: Pos { line: a, byte: b },
            end: Pos { line: c, byte: d },
        };
        // The same line count, nested: the inner one starts later.
        let outer = sp(1, 0, 5, 1);
        let inner = sp(1, 10, 5, 0);
        assert_eq!(innermost([outer, inner].into_iter()), Some(inner));
    }
}
