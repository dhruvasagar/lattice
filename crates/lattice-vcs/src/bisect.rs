use crate::{Repository, Result, VcsError};

/// Bisect operations — start, good, bad, skip, reset, and reading the
/// state of a bisect already in progress.
///
/// Uses the git CLI. Every mutating call is a local operation; none
/// touches the network.
pub struct Bisect;

/// A bisect in progress.
///
/// Produced by [`Bisect::state`], which returns `None` when no bisect
/// is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BisectState {
    /// How many revisions are left to test *after* the one currently
    /// checked out — `bisect_nr`, the number git prints in its own
    /// "Bisecting: N revisions left to test after this" line.
    ///
    /// `None` until both ends of the range are known: with only a bad
    /// commit marked there is no range yet, and reporting `0` there
    /// would read as "done" when nothing has been narrowed at all.
    pub revisions_left: Option<usize>,
    /// Roughly how many more marks are needed — git's `bisect_steps`,
    /// the parenthesised half of the same line.
    pub steps: Option<usize>,
    /// The ref the bisect started from, so `reset` has something to
    /// name. Empty when `.git/BISECT_START` is unreadable.
    pub start_ref: String,
}

/// NC.6: what one bisect step did, read from git's own report.
///
/// A mark's whole result is in what git prints: the commit it checked
/// out next, or the culprit. Discarding that output — which `mark` did
/// until NC.6 — left the caller unable to say "found it", which is the
/// one message a bisect exists to produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BisectStep {
    /// git checked out another commit to test.
    Testing {
        /// Full sha, as git printed it.
        commit: String,
        subject: String,
        /// "N revisions left to test after this".
        revisions_left: Option<usize>,
        /// "(roughly N steps)".
        steps: Option<usize>,
    },
    /// The search is over: this is the first bad commit.
    Found { commit: String, subject: String },
    /// Anything else git said — a range still missing an end, only
    /// skipped commits left, a contradictory mark. The first
    /// meaningful line, verbatim.
    Other(String),
}

/// Parse the stdout of `git bisect start|good|bad|skip`.
///
/// Pure, so each shape git prints is pinned without a repository. The
/// shapes are git's porcelain messages, which have been stable for
/// years; an unrecognised one falls to [`BisectStep::Other`] rather
/// than being misread.
pub fn parse_bisect_step(out: &str) -> BisectStep {
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if let Some(found) = lines
        .iter()
        .find_map(|l| l.strip_suffix(" is the first bad commit"))
    {
        // `git show`-style block follows: the subject is the first
        // indented line after the headers, which trimming flattened —
        // so it is the first line after `Date:`.
        let subject = lines
            .iter()
            .skip_while(|l| !l.starts_with("Date:"))
            .nth(1)
            .map(|l| l.to_string())
            .unwrap_or_default();
        return BisectStep::Found {
            commit: found.trim().to_string(),
            subject,
        };
    }
    if let Some(i) = lines.iter().position(|l| l.starts_with("Bisecting: ")) {
        let head = lines[i];
        let number_after = |marker: &str| -> Option<usize> {
            head.split(marker)
                .nth(1)?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        };
        let checked_out = lines.get(i + 1).and_then(|l| {
            let rest = l.strip_prefix('[')?;
            let (sha, subject) = rest.split_once(']')?;
            Some((sha.to_string(), subject.trim().to_string()))
        });
        if let Some((commit, subject)) = checked_out {
            return BisectStep::Testing {
                commit,
                subject,
                revisions_left: number_after("Bisecting: "),
                steps: number_after("(roughly "),
            };
        }
    }
    BisectStep::Other(lines.first().map(|l| l.to_string()).unwrap_or_default())
}

/// Parse `git rev-list --bisect-vars` output into
/// `(revisions_left, steps)`.
///
/// **This is plumbing git provides precisely so callers do not
/// reimplement the bisection arithmetic.** An earlier attempt here
/// computed "revisions left" as `count(rev-list bad ^good) - 1`, which
/// is wrong and quietly so: for eight commits it said 6 where git says
/// 3. Git does not report the size of the remaining range — it reports
/// the size of the worst-case half *after* the midpoint it just chose,
/// which is the bisection algorithm, not a subtraction. Showing a
/// number that disagrees with what `git bisect` prints in the same
/// terminal is worse than showing none.
///
/// The format is `key='value'` or `key=value`, one per line.
pub fn parse_bisect_vars(out: &str) -> (Option<usize>, Option<usize>) {
    let get = |key: &str| -> Option<usize> {
        out.lines()
            .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim().trim_matches('\''))
            .and_then(|v| v.parse::<usize>().ok())
    };
    (get("bisect_nr"), get("bisect_steps"))
}

impl Bisect {
    /// Whether a bisect is currently running.
    ///
    /// A file-existence check, deliberately: this is called from the
    /// transient builder on the actor thread to decide which rows to
    /// show, and spawning `git` there to answer a yes/no question
    /// would be process-spawn latency on a keystroke path.
    /// `.git/BISECT_LOG` is what git itself creates on `bisect start`
    /// and removes on `bisect reset`.
    pub fn in_progress(repo: &Repository) -> bool {
        repo.gitdir().join("BISECT_LOG").exists()
    }

    /// The state of the bisect in progress, or `None` if none is.
    pub fn state(repo: &Repository) -> Result<Option<BisectState>> {
        if !Self::in_progress(repo) {
            return Ok(None);
        }
        let start_ref = std::fs::read_to_string(repo.gitdir().join("BISECT_START"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let (revisions_left, steps) = Self::progress(repo);
        Ok(Some(BisectState {
            revisions_left,
            steps,
            start_ref,
        }))
    }

    /// Ask git how far the bisect has narrowed.
    ///
    /// `(None, None)` when the range is not yet established (a bad end
    /// marked but no good one) or when git refuses the walk. A failed
    /// count must not fail the whole state read: the headerline still
    /// has "a bisect is running" to say, which is the part that
    /// matters.
    fn progress(repo: &Repository) -> (Option<usize>, Option<usize>) {
        let Ok(goods) =
            repo.run_git_lines(["for-each-ref", "--format=%(refname)", "refs/bisect/good-*"])
        else {
            return (None, None);
        };
        if goods.is_empty() {
            return (None, None);
        }
        let mut args: Vec<String> = vec![
            "rev-list".into(),
            "--bisect-vars".into(),
            "refs/bisect/bad".into(),
        ];
        for good in &goods {
            args.push(format!("^{good}"));
        }
        match repo.run_git_str(args) {
            Ok(out) => parse_bisect_vars(&out),
            Err(_) => (None, None),
        }
    }

    /// Start a bisect.
    ///
    /// `bad` and `good` are optional: `git bisect start` with neither
    /// begins an unbounded bisect the user then narrows with `good` /
    /// `bad`, which is git's own behaviour and worth preserving.
    pub fn start(repo: &Repository, bad: Option<&str>, good: Option<&str>) -> Result<BisectStep> {
        let mut args: Vec<String> = vec!["bisect".into(), "start".into()];
        // Order matters to git: bad first, then good.
        if let Some(bad) = bad {
            args.push(bad.to_string());
        }
        if let Some(good) = good {
            args.push(good.to_string());
        }
        repo.run_git_str(args)
            .map(|out| parse_bisect_step(&out))
            .map_err(|e| VcsError::Bisect(format!("bisect start: {}", e)))
    }

    /// Mark a revision good (`None` = the one checked out).
    pub fn good(repo: &Repository, rev: Option<&str>) -> Result<BisectStep> {
        Self::mark(repo, "good", rev)
    }

    /// Mark a revision bad (`None` = the one checked out).
    pub fn bad(repo: &Repository, rev: Option<&str>) -> Result<BisectStep> {
        Self::mark(repo, "bad", rev)
    }

    /// Skip a revision that cannot be tested (`None` = the one checked
    /// out).
    pub fn skip(repo: &Repository, rev: Option<&str>) -> Result<BisectStep> {
        Self::mark(repo, "skip", rev)
    }

    fn mark(repo: &Repository, verb: &str, rev: Option<&str>) -> Result<BisectStep> {
        let mut args: Vec<String> = vec!["bisect".into(), verb.to_string()];
        if let Some(rev) = rev {
            args.push(rev.to_string());
        }
        repo.run_git_str(args)
            .map(|out| parse_bisect_step(&out))
            .map_err(|e| VcsError::Bisect(format!("bisect {}: {}", verb, e)))
    }

    /// End the bisect and return to the ref it started from.
    pub fn reset(repo: &Repository) -> Result<()> {
        repo.run_git(["bisect", "reset"])
            .map(|_| ())
            .map_err(|e| VcsError::Bisect(format!("bisect reset: {}", e)))
    }

    /// The bisect log — every mark made so far, in git's replayable
    /// format.
    pub fn log(repo: &Repository) -> Result<String> {
        repo.run_git_str(["bisect", "log"])
            .map_err(|e| VcsError::Bisect(format!("bisect log: {}", e)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verbatim `git rev-list --bisect-vars` output, from a real
    /// eight-commit bisect. Git printed "Bisecting: 3 revisions left
    /// to test after this (roughly 2 steps)" for this same state.
    const REAL_VARS: &str = "bisect_rev='4ffe4a2f5796f503ef813ff4603b30b752732432'\n\
                             bisect_nr=3\n\
                             bisect_good=3\n\
                             bisect_bad=2\n\
                             bisect_all=7\n\
                             bisect_steps=2\n";

    /// Verbatim, from git 2.39 on an eight-commit history.
    const REAL_TESTING: &str = "Bisecting: 1 revision left to test after this (roughly 1 step)\n\
                                [e73283741f49bb9fa2ad3edd7564e3fbedff4383] c6\n";
    const REAL_FOUND: &str = "4408d81987933a1275554215cacb0eb9b26df0ce is the first bad commit\n\
                              commit 4408d81987933a1275554215cacb0eb9b26df0ce\n\
                              Author: t <t@t>\n\
                              Date:   Thu Sep 17 10:24:13 2026 +0530\n\
                              \n    c5\n\n f | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n";

    #[test]
    fn a_step_that_checks_out_the_next_commit_is_testing() {
        assert_eq!(
            parse_bisect_step(REAL_TESTING),
            BisectStep::Testing {
                commit: "e73283741f49bb9fa2ad3edd7564e3fbedff4383".into(),
                subject: "c6".into(),
                revisions_left: Some(1),
                steps: Some(1),
            }
        );
    }

    #[test]
    fn the_last_step_names_the_culprit_and_its_subject() {
        assert_eq!(
            parse_bisect_step(REAL_FOUND),
            BisectStep::Found {
                commit: "4408d81987933a1275554215cacb0eb9b26df0ce".into(),
                subject: "c5".into(),
            }
        );
    }

    #[test]
    fn anything_else_is_passed_through_not_misread() {
        let skipped = "There are only 'skip'ped commits left to test.\n\
                       The first bad commit could be any of:\nabc\ndef\n";
        assert_eq!(
            parse_bisect_step(skipped),
            BisectStep::Other("There are only 'skip'ped commits left to test.".into())
        );
        assert_eq!(
            parse_bisect_step("status: waiting for good commit(s), bad commit known\n"),
            BisectStep::Other("status: waiting for good commit(s), bad commit known".into())
        );
    }

    #[test]
    fn the_parsed_numbers_are_the_ones_git_prints() {
        assert_eq!(parse_bisect_vars(REAL_VARS), (Some(3), Some(2)));
    }

    #[test]
    fn a_quoted_value_parses_the_same_as_a_bare_one() {
        assert_eq!(parse_bisect_vars("bisect_nr='7'\n"), (Some(7), None));
        assert_eq!(parse_bisect_vars("bisect_nr=7\n"), (Some(7), None));
    }

    #[test]
    fn a_prefix_collision_does_not_match_the_wrong_key() {
        // `bisect_nr` must not be read out of `bisect_nrx=...`, and
        // `bisect_bad` must not satisfy a lookup for `bisect_b`.
        assert_eq!(parse_bisect_vars("bisect_nrx=9\n").0, None);
    }

    #[test]
    fn missing_or_unparseable_vars_yield_nothing_rather_than_zero() {
        // Not `Some(0)`: "0 left" reads as finished, and nothing has
        // been narrowed when git said nothing at all.
        assert_eq!(parse_bisect_vars(""), (None, None));
        assert_eq!(parse_bisect_vars("bisect_nr=\n"), (None, None));
        assert_eq!(parse_bisect_vars("bisect_nr=lots\n"), (None, None));
    }
}
