//! Conceal rules — the per-language declaration of what to hide.
//!
//! A rule is a regex over one line plus the 1-based capture groups
//! whose spans are elided. Org's link rendering is two of them and
//! nothing else; the mechanism, the coordinate maths and the mode
//! scoping are the host's.
//!
//! Design anchor:
//! [`docs/dev/architecture/conceal.md`](../../../docs/dev/architecture/conceal.md).
//!
//! ## Why `regex` and not `fancy-regex`
//!
//! The workspace carries both: `fancy-regex` for `/`, `?` and `:s`,
//! where users expect lookaround, and `regex` for everything that
//! must not backtrack. Conceal patterns run against every rebuilt
//! display line, supplied by a *plugin*, so a catastrophic
//! backtracking pattern would be a plugin's ability to freeze the
//! renderer. `regex`'s RE2-style engine has no such input, which
//! turns "don't ship a pathological pattern" from a plugin-author
//! obligation into something they cannot do.
//!
//! ## Why a bad rule is dropped rather than fatal
//!
//! A malformed tree-sitter *query* rejects the whole language
//! (`LanguageRegistrationError::QueryCompile`), and that asymmetry
//! is deliberate rather than an oversight. A broken `folds.scm`
//! means the language cannot fold at all — structural, and silence
//! there is indistinguishable from the feature not existing. A
//! broken conceal rule means one pattern does not hide, the other
//! rules are unaffected, and the language is otherwise entirely
//! usable. Losing org over a typo in a cosmetic regex would be the
//! disproportionate answer.

use regex::Regex;

/// The most rules one language may declare.
///
/// Not a backtracking guard — the engine has none. It bounds how
/// much of *someone else's configuration* a display-matrix rebuild
/// has to walk: every rule is tried against every rebuilt line, so
/// an unbounded list turns a rebuild into a linear scan of a
/// plugin's ambition.
pub const MAX_CONCEAL_RULES: usize = 32;

/// The longest pattern accepted, in bytes. Compilation cost and
/// per-line match cost both scale with program size, and no
/// legitimate line-level rule comes close.
pub const MAX_CONCEAL_PATTERN_LEN: usize = 512;

/// One compiled display-time elision rule.
#[derive(Debug, Clone)]
pub struct ConcealRule {
    pattern: Regex,
    hide: Vec<u32>,
}

impl ConcealRule {
    /// The compiled pattern, for matching a line.
    pub fn pattern(&self) -> &Regex {
        &self.pattern
    }

    /// 1-based capture-group indices this rule hides.
    pub fn hide(&self) -> &[u32] {
        &self.hide
    }

    /// The pattern source, for diagnostics and equality.
    pub fn source(&self) -> &str {
        self.pattern.as_str()
    }
}

/// Compiled rules compare by their declaration, not by their
/// compiled program — `Regex` has no `PartialEq`, and two rules are
/// the same rule exactly when a plugin declared them the same way.
impl PartialEq for ConcealRule {
    fn eq(&self, other: &Self) -> bool {
        self.pattern.as_str() == other.pattern.as_str() && self.hide == other.hide
    }
}

impl Eq for ConcealRule {}

/// Why one rule was refused.
///
/// Every variant names the rule, because the message reaches a
/// plugin author who has several and needs to know which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConcealRuleError {
    EmptyPattern,
    PatternTooLong {
        len: usize,
    },
    BadRegex {
        detail: String,
    },
    /// No groups listed, so the rule would hide nothing and cost a
    /// match per line to do it.
    NothingHidden,
    /// Group 0 is the whole match. Hiding it is a deletion rather
    /// than a concealment and is almost always a pattern that
    /// forgot its capture parentheses.
    HidesWholeMatch,
    /// A `hide` index past the pattern's group count. Caught here
    /// rather than per line, where it would log at rebuild rate.
    UnknownGroup {
        group: u32,
        groups: u32,
    },
    /// The language already declared [`MAX_CONCEAL_RULES`].
    TooManyRules {
        limit: usize,
    },
}

impl std::fmt::Display for ConcealRuleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPattern => write!(f, "pattern is empty"),
            Self::PatternTooLong { len } => {
                write!(
                    f,
                    "pattern is {len} bytes, limit is {MAX_CONCEAL_PATTERN_LEN}"
                )
            }
            Self::BadRegex { detail } => write!(f, "pattern does not compile: {detail}"),
            Self::NothingHidden => write!(f, "hide is empty, so the rule would hide nothing"),
            Self::HidesWholeMatch => write!(
                f,
                "hide names group 0 (the whole match) — that is a deletion, not a \
                 concealment; the pattern is probably missing its capture parentheses"
            ),
            Self::UnknownGroup { group, groups } => {
                write!(f, "hide names group {group} but the pattern has {groups}")
            }
            Self::TooManyRules { limit } => {
                write!(f, "more than {limit} conceal rules")
            }
        }
    }
}

impl std::error::Error for ConcealRuleError {}

/// Compile one rule, or say precisely why not.
pub fn compile_rule(pattern: &str, hide: &[u32]) -> Result<ConcealRule, ConcealRuleError> {
    if pattern.trim().is_empty() {
        return Err(ConcealRuleError::EmptyPattern);
    }
    if pattern.len() > MAX_CONCEAL_PATTERN_LEN {
        return Err(ConcealRuleError::PatternTooLong { len: pattern.len() });
    }
    if hide.is_empty() {
        return Err(ConcealRuleError::NothingHidden);
    }
    if hide.contains(&0) {
        return Err(ConcealRuleError::HidesWholeMatch);
    }
    let compiled = Regex::new(pattern).map_err(|e| ConcealRuleError::BadRegex {
        // `regex`'s error renders as a multi-line diagram; the log
        // line wants one line.
        detail: e.to_string().replace('\n', " ").trim().to_string(),
    })?;
    // `captures_len` counts group 0, so the highest addressable
    // 1-based group is one less.
    let groups = compiled.captures_len().saturating_sub(1) as u32;
    if let Some(&bad) = hide.iter().find(|g| **g > groups) {
        return Err(ConcealRuleError::UnknownGroup { group: bad, groups });
    }
    let mut hide = hide.to_vec();
    // Sorted + deduped so the matcher can trust the order and a rule
    // listing a group twice does not subtract its width twice.
    hide.sort_unstable();
    hide.dedup();
    Ok(ConcealRule {
        pattern: compiled,
        hide,
    })
}

/// Compile a language's whole declaration, keeping what works.
///
/// Returns the accepted rules and one rejection per refused rule.
/// **A refusal never fails the language** — see the module header
/// for why this is asymmetric with query compilation. The caller
/// logs each rejection once, at registration; logging per line
/// would emit at rebuild rate, which is the `debug!`-not-`info!`
/// mistake wearing a different hat.
pub fn compile_rules(
    declared: &[(String, Vec<u32>)],
) -> (Vec<ConcealRule>, Vec<(usize, ConcealRuleError)>) {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for (i, (pattern, hide)) in declared.iter().enumerate() {
        if ok.len() >= MAX_CONCEAL_RULES {
            errs.push((
                i,
                ConcealRuleError::TooManyRules {
                    limit: MAX_CONCEAL_RULES,
                },
            ));
            continue;
        }
        match compile_rule(pattern, hide) {
            Ok(r) => ok.push(r),
            Err(e) => errs.push((i, e)),
        }
    }
    (ok, errs)
}

/// A stamp identifying a rule set, for the matrix's `conceal` axis.
///
/// **Zero for an empty rule set, and that is the load-bearing case.**
/// Every buffer whose language declares no rules gets a constant, so
/// the axis cannot move for it — which is what keeps `i` in a Rust
/// file from costing a viewport rebuild once H.4 folds the modal state
/// in here. The check is `is_empty()` before any hashing, so those
/// buffers pay a branch.
///
/// Hashes the declarations rather than the `Arc`'s address: a pointer
/// would change on any reallocation that did not change a rule, and
/// would compare equal across two different rule sets that happened to
/// land at the same address after a free.
pub fn rules_version(rules: &[ConcealRule]) -> u64 {
    if rules.is_empty() {
        return 0;
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for r in rules {
        r.source().hash(&mut h);
        r.hide().hash(&mut h);
    }
    // Fold in a non-zero marker so a rule set cannot hash to 0 and
    // become indistinguishable from "no rules".
    h.finish() | 1
}

/// The byte ranges `rules` hide in `line` — sorted and coalesced.
///
/// Rules are tried in declaration order and every match of every rule
/// contributes; the result is the **union** of their hidden groups.
///
/// ## Why the union is coalesced before it is returned
///
/// Two rules hiding overlapping spans must produce one hidden span.
/// The consumer subtracts `end - start` per range to find a display
/// column, so an un-merged overlap subtracts its shared width twice
/// and every column past it on that line is wrong. Merging here rather
/// than trusting callers means the invariant holds at the one place it
/// can be established.
///
/// ## Declaration order does not affect the result
///
/// Stated because the design originally claimed the opposite, and a
/// test caught it. Every rule is tried at every position and the
/// hidden spans are unioned, so no rule can consume text before
/// another sees it — the output is the same under any permutation of
/// the rule list.
///
/// The worry that motivated the retracted claim was that a described
/// org link `[[a][b]]` would be matched as a *bare* one, hiding the
/// outer brackets and leaving `a][b` on screen. It cannot happen, for
/// a second and independent reason: the bare pattern's `[^]]+` stops
/// at the first `]`, so it never reaches the closing `]]` of a
/// described link and does not match it at all. The two patterns are
/// disjoint by construction, which is a property of how they are
/// written rather than of the order they are declared in.
///
/// What a rule author must therefore get right is the *pattern*, not
/// its position: a rule that matches more than it means to will hide
/// more than it means to no matter where it sits.
///
/// Returns empty immediately when there are no rules, which is the
/// path every buffer in the editor but one takes.
pub fn conceal_spans(rules: &[ConcealRule], line: &str) -> Vec<(u32, u32)> {
    if rules.is_empty() || line.is_empty() {
        return Vec::new();
    }
    let mut spans: Vec<(u32, u32)> = Vec::new();
    for rule in rules {
        for caps in rule.pattern.captures_iter(line) {
            for g in rule.hide() {
                // A group that did not participate in this match is
                // `None` — legal and common with alternations, and not
                // an error: it hid nothing here. An empty match is
                // dropped for the same reason, before it can become a
                // zero-width range the coalescer has to reason about.
                if let Some(m) = caps.get(*g as usize)
                    && m.start() < m.end()
                {
                    spans.push((m.start() as u32, m.end() as u32));
                }
            }
        }
    }
    if spans.len() > 1 {
        spans.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(spans.len());
        for (s, e) in spans {
            match merged.last_mut() {
                // `<=` not `<`: two ranges that merely touch are one
                // hidden run, and leaving them adjacent-but-separate
                // would be a correct-but-noisier list the consumer has
                // to walk twice.
                Some(last) if s <= last.1 => last.1 = last.1.max(e),
                _ => merged.push((s, e)),
            }
        }
        return merged;
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Org's own two rules — the acceptance case this exists for.
    const DESCRIBED: &str = r"(\[\[[^]]+\]\[)[^]]+(\]\])";
    const BARE: &str = r"(\[\[)([^]]+)(\]\])";

    #[test]
    fn orgs_two_rules_compile() {
        let (ok, errs) = compile_rules(&[
            (DESCRIBED.to_string(), vec![1, 2]),
            (BARE.to_string(), vec![1, 3]),
        ]);
        assert_eq!(ok.len(), 2);
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(ok[0].hide(), &[1, 2]);
        assert_eq!(ok[1].hide(), &[1, 3]);
    }

    #[test]
    fn an_uncompilable_pattern_drops_exactly_one_rule() {
        // The property the whole module exists for: a plugin does
        // not lose its language over a typo in a cosmetic regex.
        let (ok, errs) = compile_rules(&[
            (DESCRIBED.to_string(), vec![1, 2]),
            ("(unclosed".to_string(), vec![1]),
            (BARE.to_string(), vec![1, 3]),
        ]);
        assert_eq!(ok.len(), 2, "the two good rules survive");
        assert_eq!(errs.len(), 1);
        assert_eq!(errs[0].0, 1, "the rejection names which rule");
        assert!(matches!(errs[0].1, ConcealRuleError::BadRegex { .. }));
    }

    #[test]
    fn hiding_group_zero_is_refused() {
        let e = compile_rule(r"\[\[.+\]\]", &[0]).unwrap_err();
        assert_eq!(e, ConcealRuleError::HidesWholeMatch);
        // And the message says what to do about it, because the
        // cause is almost always missing capture parentheses.
        assert!(e.to_string().contains("capture parentheses"));
    }

    #[test]
    fn a_group_the_pattern_does_not_have_is_refused_at_registration() {
        // Not per line — this is the check that keeps a bad index
        // from logging at rebuild rate.
        let e = compile_rule(DESCRIBED, &[1, 2, 5]).unwrap_err();
        assert_eq!(
            e,
            ConcealRuleError::UnknownGroup {
                group: 5,
                groups: 2
            }
        );
    }

    #[test]
    fn an_empty_hide_list_is_refused() {
        assert_eq!(
            compile_rule(DESCRIBED, &[]).unwrap_err(),
            ConcealRuleError::NothingHidden
        );
    }

    #[test]
    fn an_empty_pattern_is_refused() {
        assert_eq!(
            compile_rule("   ", &[1]).unwrap_err(),
            ConcealRuleError::EmptyPattern
        );
    }

    #[test]
    fn an_overlong_pattern_is_refused() {
        let long = format!("({})", "a".repeat(MAX_CONCEAL_PATTERN_LEN));
        let e = compile_rule(&long, &[1]).unwrap_err();
        assert!(matches!(e, ConcealRuleError::PatternTooLong { .. }));
    }

    #[test]
    fn the_cap_refuses_the_overflow_and_keeps_the_rest() {
        let declared: Vec<(String, Vec<u32>)> = (0..MAX_CONCEAL_RULES + 3)
            .map(|_| (BARE.to_string(), vec![1, 3]))
            .collect();
        let (ok, errs) = compile_rules(&declared);
        assert_eq!(ok.len(), MAX_CONCEAL_RULES);
        assert_eq!(errs.len(), 3);
        assert!(
            errs.iter()
                .all(|(_, e)| matches!(e, ConcealRuleError::TooManyRules { .. }))
        );
    }

    #[test]
    fn a_duplicated_group_is_normalised_away() {
        // Listing a group twice would otherwise subtract its width
        // twice once the matcher runs.
        let r = compile_rule(DESCRIBED, &[2, 1, 2]).unwrap();
        assert_eq!(r.hide(), &[1, 2]);
    }

    #[test]
    fn rules_compare_by_declaration_not_by_compiled_program() {
        let a = compile_rule(BARE, &[1, 3]).unwrap();
        let b = compile_rule(BARE, &[1, 3]).unwrap();
        let c = compile_rule(BARE, &[1]).unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    // ---- H.3: matching ----

    fn org_rules() -> Vec<ConcealRule> {
        // Described BEFORE bare — the order org declares them in, and
        // the reason is asserted below.
        let (ok, errs) = compile_rules(&[
            (DESCRIBED.to_string(), vec![1, 2]),
            (BARE.to_string(), vec![1, 3]),
        ]);
        assert!(errs.is_empty());
        ok
    }

    /// Apply the spans to get what the user would actually see.
    fn rendered(rules: &[ConcealRule], line: &str) -> String {
        let spans = conceal_spans(rules, line);
        let mut out = String::new();
        let mut at = 0usize;
        for (s, e) in spans {
            out.push_str(&line[at..s as usize]);
            at = e as usize;
        }
        out.push_str(&line[at..]);
        out
    }

    #[test]
    fn no_rules_is_free_and_hides_nothing() {
        assert!(conceal_spans(&[], "[[id:X][Title]]").is_empty());
    }

    #[test]
    fn a_described_link_collapses_to_its_description() {
        let r = org_rules();
        assert_eq!(
            rendered(&r, "* See [[id:6F39][Project Kickoff]] before Friday."),
            "* See Project Kickoff before Friday."
        );
    }

    #[test]
    fn a_bare_link_keeps_its_target() {
        // Emacs draws the same line: a link whose only text IS its
        // target has nothing left to show once the target is hidden.
        let r = org_rules();
        assert_eq!(
            rendered(&r, "see [[https://example.com]] ok"),
            "see https://example.com ok"
        );
    }

    /// The design claimed org's described rule "must be tried first",
    /// or a described link would be matched as a bare one and render
    /// as `id:6F39][Project Kickoff`. This test was written to pin
    /// that and instead disproved it, twice over. Kept in the shape
    /// that found the error.
    #[test]
    fn declaration_order_cannot_change_what_is_hidden() {
        let described_first = org_rules();
        let (bare_first, errs) = compile_rules(&[
            (BARE.to_string(), vec![1, 3]),
            (DESCRIBED.to_string(), vec![1, 2]),
        ]);
        assert!(errs.is_empty());

        for line in [
            "[[id:6F39][Project Kickoff]]",
            "see [[https://example.com]] ok",
            "[[id:A][one]] and [[id:B][two]]",
            "[[id:A][one]] and [[https://x.test]]",
        ] {
            assert_eq!(
                conceal_spans(&described_first, line),
                conceal_spans(&bare_first, line),
                "spans must be order-independent: {line}"
            );
        }
    }

    /// The second, independent reason the ordering worry was unfounded:
    /// the two patterns cannot both match the same link. `[^]]+` stops
    /// at the first `]`, so the bare pattern never reaches a described
    /// link's closing `]]`.
    ///
    /// This is a property of how the patterns are WRITTEN, so it is
    /// asserted here — a future edit to either pattern that made them
    /// overlap would silently reintroduce the failure the retracted
    /// ordering rule was worried about.
    #[test]
    fn orgs_two_patterns_are_disjoint_by_construction() {
        let described = compile_rule(DESCRIBED, &[1, 2]).unwrap();
        let bare = compile_rule(BARE, &[1, 3]).unwrap();
        let link = "[[id:6F39][Project Kickoff]]";
        assert!(
            bare.pattern().find(link).is_none(),
            "the bare pattern must not match a described link"
        );
        assert!(described.pattern().find(link).is_some());
        // And the converse, so neither pattern is silently doing the
        // other's job.
        let plain = "[[https://example.com]]";
        assert!(described.pattern().find(plain).is_none());
        assert!(bare.pattern().find(plain).is_some());
    }

    #[test]
    fn two_links_on_one_line_both_collapse() {
        let r = org_rules();
        assert_eq!(
            rendered(&r, "[[id:A][one]] and [[id:B][two]]"),
            "one and two"
        );
    }

    #[test]
    fn a_malformed_link_is_left_entirely_alone() {
        let r = org_rules();
        let line = "[[id:6F39][unterminated";
        assert!(conceal_spans(&r, line).is_empty());
        assert_eq!(rendered(&r, line), line);
    }

    #[test]
    fn overlapping_rules_coalesce_into_one_span() {
        // The invariant the consumer depends on: an un-merged overlap
        // would have its shared width subtracted twice and every
        // column past it would be wrong.
        let (rules, _) = compile_rules(&[
            (r"(abcd)ef".to_string(), vec![1]),
            (r"ab(cdef)".to_string(), vec![1]),
        ]);
        assert_eq!(conceal_spans(&rules, "abcdef"), vec![(0, 6)]);
    }

    #[test]
    fn touching_spans_merge_rather_than_staying_adjacent() {
        let (rules, _) = compile_rules(&[(r"(ab)(cd)".to_string(), vec![1, 2])]);
        assert_eq!(conceal_spans(&rules, "abcd"), vec![(0, 4)]);
    }

    #[test]
    fn spans_come_back_sorted() {
        let r = org_rules();
        let spans = conceal_spans(&r, "[[id:A][one]] and [[id:B][two]]");
        assert!(
            spans.windows(2).all(|w| w[0].1 <= w[1].0),
            "sorted and disjoint: {spans:?}"
        );
    }

    #[test]
    fn a_group_that_did_not_participate_is_not_an_error() {
        // An alternation leaves one branch's group unmatched. That hid
        // nothing here; it is not a refusal.
        let (rules, errs) = compile_rules(&[(r"(?:(aa)|(bb))cc".to_string(), vec![1, 2])]);
        assert!(errs.is_empty());
        assert_eq!(conceal_spans(&rules, "aacc"), vec![(0, 2)]);
        assert_eq!(conceal_spans(&rules, "bbcc"), vec![(0, 2)]);
    }

    #[test]
    fn a_link_inside_a_source_block_still_conceals() {
        // Documented behaviour, not a bug to be surprised by later:
        // conceal is textual and knows nothing about blocks. The
        // tree-driven alternative that WOULD know is rejected in
        // conceal.md for flickering on every reparse.
        let r = org_rules();
        assert_eq!(rendered(&r, "  [[id:A][shown]]"), "  shown");
    }

    #[test]
    fn a_regex_error_renders_on_one_line() {
        // `regex` renders errors as a multi-line diagram; a log line
        // that spans five rows is a log line nobody reads.
        let e = compile_rule("(unclosed", &[1]).unwrap_err();
        assert!(!e.to_string().contains('\n'), "{e}");
    }
}
