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

    #[test]
    fn a_regex_error_renders_on_one_line() {
        // `regex` renders errors as a multi-line diagram; a log line
        // that spans five rows is a log line nobody reads.
        let e = compile_rule("(unclosed", &[1]).unwrap_err();
        assert!(!e.to_string().contains('\n'), "{e}");
    }
}
