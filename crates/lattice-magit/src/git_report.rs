//! NC.3: pick the line of git's output that says what happened.
//!
//! A notification shows one line, and the first line git prints is
//! often not the one that matters:
//!
//! - `git push` writes `To <url>` before the ref it updated, so a
//!   finished push read `push: To github.com:o/r.git`.
//! - A merge conflict writes `CONFLICT …` to **stdout** and nothing to
//!   stderr, so the failure read `merge failed: ` with nothing after it.
//! - A rejected push puts `! [rejected]` on the second line.
//! - `pull --ff-only` leads with `hint:` lines.
//!
//! The functions here are pure, so each case is pinned without running
//! git. They do not *shorten* the output: the chosen line goes first
//! and git's complete output follows it. `finish_task` publishes the
//! first line and logs the whole text, so `*messages*` still carries
//! everything git said.

/// The text a successful git run reports: the summary line (if any),
/// then everything git printed.
pub(crate) fn success_report(argv: &[String], stdout: &str, stderr: &str) -> String {
    let summary = success_summary(argv, stdout, stderr);
    with_detail(summary, stdout, stderr)
}

/// The text a failed git run reports: the line that explains the
/// failure, then everything git printed. Never empty — a failure with
/// no output at all still names its exit status, because `merge
/// failed — ` with nothing after it is exactly the report this module
/// exists to replace.
pub(crate) fn failure_report(stdout: &str, stderr: &str, status: Option<i32>) -> String {
    let reason = failure_line(stdout, stderr).unwrap_or_else(|| match status {
        Some(code) => format!("git exited with status {code}"),
        None => "git was terminated by a signal".to_string(),
    });
    with_detail(reason, stdout, stderr)
}

/// `summary`, then the full output on the following lines — unless
/// the summary is empty, in which case the report must *start* empty
/// so the notification shows the label alone rather than git's first
/// line of noise.
fn with_detail(summary: String, stdout: &str, stderr: &str) -> String {
    let detail = [stdout.trim(), stderr.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if detail.is_empty() {
        return summary;
    }
    format!("{summary}\n{detail}")
}

/// Lines worth reading: trimmed, non-empty, and not git's advice or
/// progress chatter.
fn meaningful(text: &str) -> impl Iterator<Item = &str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| !l.starts_with("hint:"))
        .filter(|l| !is_progress(l))
}

fn is_progress(line: &str) -> bool {
    const PREFIXES: [&str; 8] = [
        "remote: ",
        "Enumerating objects",
        "Counting objects",
        "Compressing objects",
        "Writing objects",
        "Receiving objects",
        "Resolving deltas",
        "Total ",
    ];
    PREFIXES.iter().any(|p| line.starts_with(p))
}

/// The line that says why it failed, searched in order of how precisely
/// each kind names the problem. stdout is searched too: git reports
/// merge and apply conflicts there.
fn failure_line(stdout: &str, stderr: &str) -> Option<String> {
    let all: Vec<&str> = meaningful(stderr).chain(meaningful(stdout)).collect();
    let find = |pred: &dyn Fn(&str) -> bool| all.iter().find(|l| pred(l)).map(|l| l.to_string());
    find(&|l| l.starts_with("CONFLICT"))
        .or_else(|| find(&|l| l.starts_with("! [")))
        .or_else(|| find(&|l| l.starts_with("fatal:")))
        .or_else(|| find(&|l| l.starts_with("error:")))
        .or_else(|| find(&|l| !l.starts_with("To ") && !l.starts_with("From ")))
        .map(|l| {
            // `! [rejected]        main -> main (non-fast-forward)` reads
            // better without its column padding.
            l.split_whitespace().collect::<Vec<_>>().join(" ")
        })
}

/// What a successful command did, in a few words. Empty when the
/// label already says everything.
fn success_summary(argv: &[String], stdout: &str, stderr: &str) -> String {
    match argv.first().map(String::as_str) {
        Some("push") => push_summary(stderr),
        Some("fetch") => fetch_summary(stderr),
        Some("pull") => pull_summary(stdout, stderr),
        // Both print status noise to stdout ("Your branch is up to
        // date…") and the one line that matters to stderr.
        Some("checkout") | Some("switch") => meaningful(stderr)
            .find(|l| l.starts_with("Switched to") || l.starts_with("Updated "))
            .map(str::to_string)
            .unwrap_or_default(),
        // "Cloning into 'x'..." is fire-time text; on completion it
        // reads as still running. The label names the destination.
        Some("clone") => String::new(),
        // `stash apply` / `pop` print `git status` to stdout — "On
        // branch main" is not what happened. `pop` ends with the one
        // line that is.
        Some("stash") => meaningful(stdout)
            .find(|l| l.starts_with("Dropped ") || l.starts_with("Saved "))
            .map(str::to_string)
            .or_else(|| {
                meaningful(stdout)
                    .find(|l| l.starts_with("No local changes"))
                    .map(str::to_string)
            })
            .unwrap_or_default(),
        _ => meaningful(stdout)
            .chain(meaningful(stderr))
            .next()
            .map(str::to_string)
            .unwrap_or_default(),
    }
}

/// A ref line from push or fetch's porcelain-ish report:
/// `   3f2a1c..9b8d7e  main -> main`, `* [new branch]      x -> x`,
/// `+ 1a2b...3c4d main -> main (forced update)`.
fn ref_update(line: &str) -> Option<(String, String)> {
    let (left, right) = line.split_once(" -> ")?;
    let src = left.split_whitespace().last()?;
    let dst = right.split_whitespace().next()?;
    Some((src.to_string(), dst.to_string()))
}

fn push_summary(stderr: &str) -> String {
    if stderr.contains("Everything up-to-date") {
        return "already up to date".to_string();
    }
    let forced = stderr.contains("(forced update)");
    let updates: Vec<String> = meaningful(stderr)
        .filter_map(ref_update)
        .map(|(src, dst)| {
            if src == dst {
                src
            } else {
                format!("{src} \u{2192} {dst}")
            }
        })
        .collect();
    let what = match updates.as_slice() {
        [] => return String::new(),
        [one] => one.clone(),
        many => format!("{} refs", many.len()),
    };
    if forced {
        format!("{what} (forced)")
    } else {
        what
    }
}

/// stdout is not read: `fetch --all` names each remote there even when
/// nothing moved, which is not news.
fn fetch_summary(stderr: &str) -> String {
    let is_prune = |l: &&str| l.starts_with("- [deleted]");
    let updates: Vec<(String, String)> = meaningful(stderr)
        .filter(|l| !is_prune(l))
        .filter_map(ref_update)
        .collect();
    let pruned = meaningful(stderr).filter(is_prune).count();
    let mut parts = Vec::new();
    match updates.as_slice() {
        [] => {}
        [(_, dst)] => parts.push(format!("updated {dst}")),
        many => parts.push(format!("{} refs updated", many.len())),
    }
    if pruned > 0 {
        parts.push(format!("{pruned} pruned"));
    }
    if parts.is_empty() {
        return "up to date".to_string();
    }
    parts.join(", ")
}

fn pull_summary(stdout: &str, stderr: &str) -> String {
    let all: Vec<&str> = meaningful(stdout).chain(meaningful(stderr)).collect();
    if all.iter().any(|l| l.starts_with("Already up to date")) {
        return "already up to date".to_string();
    }
    if let Some(l) = all.iter().find(|l| l.starts_with("Successfully rebased")) {
        return l.trim_end_matches('.').to_string();
    }
    let range = all
        .iter()
        .find_map(|l| l.strip_prefix("Updating "))
        .map(str::to_string);
    let stat = all
        .iter()
        .find(|l| l.contains("changed") && l.contains("file"))
        .map(|l| l.to_string());
    match (range, stat) {
        (Some(range), Some(stat)) => format!("fast-forwarded {range}, {stat}"),
        (Some(range), None) => format!("fast-forwarded {range}"),
        (None, Some(stat)) => stat,
        (None, None) => all.first().map(|l| l.to_string()).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn first(report: &str) -> &str {
        report.lines().next().unwrap_or_default()
    }

    #[test]
    fn a_push_names_the_ref_not_the_url() {
        let stderr = "To github.com:o/r.git\n   3f2a1c0..9b8d7e1  main -> main\n";
        let report = success_report(&argv(&["push", "origin"]), "", stderr);
        assert_eq!(first(&report), "main");
        assert!(
            report.contains("To github.com:o/r.git"),
            "the detail is kept"
        );
    }

    #[test]
    fn a_push_to_a_differently_named_branch_shows_both_ends() {
        let stderr = "To host:r.git\n * [new branch]      feature -> review/feature\n";
        assert_eq!(
            first(&success_report(&argv(&["push"]), "", stderr)),
            "feature \u{2192} review/feature"
        );
    }

    /// A force-push and an ordinary push must not read the same.
    #[test]
    fn a_forced_push_says_so() {
        let stderr = "To host:r.git\n + 1a2b3c4...5d6e7f8 main -> main (forced update)\n";
        assert_eq!(
            first(&success_report(&argv(&["push"]), "", stderr)),
            "main (forced)"
        );
    }

    #[test]
    fn a_push_with_nothing_to_send_says_up_to_date() {
        assert_eq!(
            first(&success_report(
                &argv(&["push"]),
                "",
                "Everything up-to-date\n"
            )),
            "already up to date"
        );
    }

    #[test]
    fn a_fetch_that_moved_nothing_says_up_to_date() {
        assert_eq!(
            first(&success_report(
                &argv(&["fetch", "--all"]),
                "Fetching origin\n",
                ""
            )),
            "up to date"
        );
    }

    #[test]
    fn a_fetch_names_what_moved_and_what_was_pruned() {
        let stderr = "From host:r\n   1a..2b  main       -> origin/main\n - [deleted]         (none)     -> origin/old\n";
        assert_eq!(
            first(&success_report(&argv(&["fetch", "--prune"]), "", stderr)),
            "updated origin/main, 1 pruned"
        );
        let one = "From host:r\n   1a..2b  main       -> origin/main\n";
        assert_eq!(
            first(&success_report(&argv(&["fetch"]), "", one)),
            "updated origin/main"
        );
    }

    #[test]
    fn a_pull_says_how_it_moved() {
        let stdout = "Updating 1a2b3c4..5d6e7f8\nFast-forward\n src/a.rs | 2 +-\n 1 file changed, 1 insertion(+), 1 deletion(-)\n";
        assert_eq!(
            first(&success_report(
                &argv(&["pull", "--ff-only"]),
                stdout,
                "From host:r\n"
            )),
            "fast-forwarded 1a2b3c4..5d6e7f8, 1 file changed, 1 insertion(+), 1 deletion(-)"
        );
        assert_eq!(
            first(&success_report(
                &argv(&["pull"]),
                "Already up to date.\n",
                ""
            )),
            "already up to date"
        );
    }

    #[test]
    fn a_checkout_reports_the_switch_not_the_tracking_status() {
        let report = success_report(
            &argv(&["checkout", "dev"]),
            "Your branch is up to date with 'origin/dev'.\n",
            "Switched to branch 'dev'\n",
        );
        assert_eq!(first(&report), "Switched to branch 'dev'");
    }

    #[test]
    fn a_stash_pop_reports_the_drop_not_the_status() {
        let stdout = "On branch main\nChanges not staged for commit:\n\tmodified: a.rs\nDropped refs/stash@{0} (1a2b3c)\n";
        assert_eq!(
            first(&success_report(&argv(&["stash", "pop"]), stdout, "")),
            "Dropped refs/stash@{0} (1a2b3c)"
        );
    }

    /// An empty summary must lead the report, so the notification shows
    /// the label alone rather than git's first line of noise.
    #[test]
    fn a_clone_leads_with_nothing_but_keeps_the_detail() {
        let report = success_report(
            &argv(&["clone", "--", "u", "d"]),
            "",
            "Cloning into 'd'...\n",
        );
        assert_eq!(first(&report), "");
        assert!(report.contains("Cloning into"));
    }

    #[test]
    fn a_merge_conflict_on_stdout_is_the_reason() {
        let stdout = "Auto-merging a.rs\nCONFLICT (content): Merge conflict in a.rs\nAutomatic merge failed; fix conflicts and then commit the result.\n";
        assert_eq!(
            first(&failure_report(stdout, "", Some(1))),
            "CONFLICT (content): Merge conflict in a.rs"
        );
    }

    #[test]
    fn a_rejected_push_leads_with_the_rejection() {
        let stderr = "To host:r.git\n ! [rejected]        main -> main (non-fast-forward)\nerror: failed to push some refs to 'host:r.git'\nhint: Updates were rejected\n";
        assert_eq!(
            first(&failure_report("", stderr, Some(1))),
            "! [rejected] main -> main (non-fast-forward)"
        );
    }

    #[test]
    fn hints_are_skipped_for_the_fatal_line() {
        let stderr = "hint: Diverging branches can't be fast-forwarded, you need to either:\nhint:\nfatal: Not possible to fast-forward, aborting.\n";
        assert_eq!(
            first(&failure_report("", stderr, Some(128))),
            "fatal: Not possible to fast-forward, aborting."
        );
    }

    #[test]
    fn a_failure_with_output_only_on_stdout_is_not_empty() {
        let stdout = "On branch main\nnothing added to commit but untracked files present\n";
        assert_eq!(
            first(&failure_report(stdout, "", Some(1))),
            "On branch main"
        );
    }

    #[test]
    fn a_silent_failure_names_its_exit_status() {
        assert_eq!(
            first(&failure_report("", "", Some(5))),
            "git exited with status 5"
        );
    }
}
