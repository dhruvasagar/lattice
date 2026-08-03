//! MG.26: `git blame --line-porcelain` → chunks, and a chunk → a
//! heading row's text.
//!
//! Pure. The mode around it fetches, activates and paints; the
//! decisions worth testing are all here.
//!
//! Design: [`../../../docs/dev/architecture/magit-blame.md`]. The
//! shape being replaced rendered blame as *text*, one row per source
//! line, which is why the code lost its highlighting — it was no
//! longer the file. Chunk headings annotate the real buffer instead,
//! so what this module produces is one heading per run of lines
//! sharing a commit, not a row per line.

/// One run of consecutive lines attributed to the same commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameChunk {
    pub sha: String,
    pub author: String,
    /// `author-time`, seconds since the epoch.
    pub time: i64,
    pub summary: String,
    /// 0-based line in the file being blamed — the anchor a heading
    /// row sits above.
    pub start_line: u32,
    pub line_count: u32,
    /// MG.33: for a **reverse** blame, what became of this run of lines
    /// after `sha`. `None` for a forward blame, where the question does
    /// not arise, and for a reverse blame whose resolution has not run
    /// yet.
    pub removal: Option<Removal>,
}

/// MG.33: what happened to a run of lines after the last commit that
/// still contained it.
///
/// **Why this exists.** `git blame --reverse` answers "the last commit
/// in which this line still existed", and magit renders that with a
/// heading indistinguishable from a forward-blame one — a SHA, an
/// author, a date. So it *reads* as "this commit removed the line",
/// when the commit that removed it is that commit's child. The feature
/// is advertised as "when did this line go away?", and it was showing
/// the answer's parent.
///
/// The three cases are kept distinct rather than collapsed because
/// naming the wrong commit is worse than naming none: a confidently
/// wrong attribution in a blame heading is a UX regression on the
/// honest-but-incomplete version it replaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// Resolved: this commit removed the lines.
    By(RemovalCommit),
    /// The lines are still in the file at HEAD, so nothing removed
    /// them. Common, and worth saying rather than leaving the reader to
    /// infer it from the SHA happening to be HEAD's.
    StillPresent,
    /// History forked at the blamed commit and more than one branch
    /// touched the file, so several commits qualify. We decline to
    /// guess — see [`Removal::By`]'s note on wrong attributions.
    Ambiguous,
}

/// The commit [`Removal::By`] names, with the metadata a heading shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovalCommit {
    pub sha: String,
    pub author: String,
    /// `author-time`, seconds since the epoch.
    pub time: i64,
    pub summary: String,
}

impl BlameChunk {
    /// The last line this chunk covers, 0-based and inclusive.
    ///
    /// A zero-length chunk cannot exist — the parser only creates one
    /// when it sees a content line — so this never underflows.
    pub fn end_line(&self) -> u32 {
        self.start_line + self.line_count.saturating_sub(1)
    }

    /// True if `line` (0-based) falls inside this chunk. What the
    /// chunk-at-cursor chords resolve through.
    pub fn contains(&self, line: u32) -> bool {
        line >= self.start_line && line <= self.end_line()
    }
}

/// Parse `git blame --line-porcelain` into chunks.
///
/// **Commit metadata appears once per commit, not once per line.**
/// Porcelain repeats a full stanza the first time it sees a commit and
/// then emits header-plus-content only. So the parser carries a map
/// from sha to metadata; reading only the current stanza would leave
/// every line after the first occurrence with an empty author, which
/// is the sort of thing that looks like "blame is broken for old
/// commits".
///
/// Consecutive lines with the same sha collapse into one chunk. A
/// commit that appears in two separate places in the file gets two
/// chunks, which is correct — they are two runs, and each wants its
/// own heading.
pub fn parse_blame_chunks(porcelain: &str) -> Vec<BlameChunk> {
    use std::collections::HashMap;

    #[derive(Default, Clone)]
    struct Meta {
        author: String,
        time: i64,
        summary: String,
    }

    let mut meta: HashMap<String, Meta> = HashMap::new();
    let mut chunks: Vec<BlameChunk> = Vec::new();
    let mut sha = String::new();
    let mut final_line: u32 = 0;
    let mut pending = Meta::default();

    for line in porcelain.lines() {
        if let Some(_code) = line.strip_prefix('\t') {
            // The one line per stanza carrying the file's own text —
            // and therefore the point at which this line is attributed.
            if sha.is_empty() {
                continue;
            }
            let m = meta.entry(sha.clone()).or_insert_with(|| pending.clone());
            // A later stanza for a commit we have seen may carry no
            // metadata; the map keeps the first one. A first stanza
            // whose fields arrived AFTER the map entry was created
            // cannot happen — the header always precedes them.
            let m = m.clone();
            match chunks.last_mut() {
                Some(last) if last.sha == sha && last.end_line() + 1 == final_line => {
                    last.line_count += 1;
                }
                _ => chunks.push(BlameChunk {
                    sha: sha.clone(),
                    author: m.author,
                    time: m.time,
                    summary: m.summary,
                    start_line: final_line,
                    line_count: 1,
                    removal: None,
                }),
            }
            pending = Meta::default();
        } else if let Some(rest) = line.strip_prefix("author ") {
            pending.author = rest.to_string();
        } else if let Some(rest) = line.strip_prefix("author-time ") {
            pending.time = rest.trim().parse().unwrap_or(0);
        } else if let Some(rest) = line.strip_prefix("summary ") {
            pending.summary = rest.to_string();
        } else if let Some((header_sha, header_final_line)) = porcelain_header(line) {
            sha = header_sha;
            // Porcelain's line numbers are 1-based; every consumer here
            // (anchors, cursor lookups) is 0-based.
            final_line = header_final_line.saturating_sub(1);
        }
    }
    chunks
}

/// The `(sha, final-line)` of a porcelain stanza header, or `None`.
///
/// Checked rather than assumed: `author-mail`, `summary` and friends
/// are also space-separated key/value lines, and a summary beginning
/// with a hex word would otherwise be read as a new commit. The second
/// field must parse as a number for the same reason.
fn porcelain_header(line: &str) -> Option<(String, u32)> {
    let mut parts = line.split(' ');
    let sha = parts.next()?;
    if sha.len() < 8 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    // `<sha> <orig-line> <final-line> [<num-lines>]`
    let _orig: u32 = parts.next()?.parse().ok()?;
    let final_line: u32 = parts.next()?.parse().ok()?;
    Some((sha.to_string(), final_line))
}

/// An uncommitted line — git reports these under an all-zero sha.
pub fn is_uncommitted(sha: &str) -> bool {
    !sha.is_empty() && sha.chars().all(|c| c == '0')
}

/// The text of a chunk's heading row.
///
/// `<short-sha> <author> <relative-date> <summary>`, or a plain
/// sentence for lines that are not committed yet — showing
/// `00000000  Not Committed Yet` would be git's internals leaking into
/// a row whose whole job is to be read at a glance.
pub fn heading_text(chunk: &BlameChunk, now_secs: i64) -> String {
    if is_uncommitted(&chunk.sha) {
        return "Uncommitted changes".to_string();
    }
    // MG.33: a reverse blame answers a different question, so its
    // heading names a different commit. See [`Removal`].
    match &chunk.removal {
        Some(Removal::By(c)) => {
            // The REMOVING commit is the answer, so it is what the
            // heading shows — leading with the commit that last
            // contained the line would be answering the question the
            // user did not ask, in a format that looks like an answer
            // to the one they did.
            let mut out = commit_line(&c.sha, &c.author, c.time, &c.summary, now_secs);
            out.push_str("  · removed");
            out
        }
        // The blamed SHA is the right one to show here: these lines are
        // still present, and that commit is what last touched them —
        // the same fact a forward blame reports.
        Some(Removal::StillPresent) => {
            let mut out = commit_line(
                &chunk.sha,
                &chunk.author,
                chunk.time,
                &chunk.summary,
                now_secs,
            );
            out.push_str("  · still present");
            out
        }
        // Explicitly NOT claiming removal. The wording states exactly
        // what git established and nothing beyond it.
        Some(Removal::Ambiguous) => {
            let mut out = commit_line(
                &chunk.sha,
                &chunk.author,
                chunk.time,
                &chunk.summary,
                now_secs,
            );
            out.push_str("  · last contained here");
            out
        }
        None => commit_line(
            &chunk.sha,
            &chunk.author,
            chunk.time,
            &chunk.summary,
            now_secs,
        ),
    }
}

/// `<short-sha>  <author>  <relative-date>  <summary>`, the shape every
/// heading variant is built from. Extracted so the reverse-blame
/// variants cannot drift from the forward one in spacing or in which
/// fields they omit when empty.
fn commit_line(sha: &str, author: &str, time: i64, summary: &str, now_secs: i64) -> String {
    let mut out: String = sha.chars().take(8).collect();
    if !author.is_empty() {
        out.push_str("  ");
        out.push_str(author);
    }
    if time > 0 {
        out.push_str("  ");
        out.push_str(&relative_date(time, now_secs));
    }
    if !summary.is_empty() {
        out.push_str("  ");
        out.push_str(summary);
    }
    out
}

/// A coarse "3 days ago", computed rather than pulled in.
///
/// **No calendar library, deliberately.** The units below (minute,
/// hour, day, week) are all fixed-length, so this needs arithmetic and
/// not a date crate; months and years are approximated, which is what
/// a relative date is for — nobody reads "14 months ago" expecting it
/// to be exact. Taking `now` as a parameter is what keeps it testable:
/// a function reading the clock cannot be asserted against.
pub fn relative_date(then_secs: i64, now_secs: i64) -> String {
    let d = now_secs.saturating_sub(then_secs);
    // A commit stamped in the future (clock skew across machines is
    // ordinary in a shared repository) reads as "just now" rather than
    // as a negative age.
    if d < 60 {
        return "just now".to_string();
    }
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const WEEK: i64 = 7 * DAY;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    let (n, unit) = if d < HOUR {
        (d / MINUTE, "minute")
    } else if d < DAY {
        (d / HOUR, "hour")
    } else if d < WEEK {
        (d / DAY, "day")
    } else if d < MONTH {
        (d / WEEK, "week")
    } else if d < YEAR {
        (d / MONTH, "month")
    } else {
        (d / YEAR, "year")
    };
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{plural} ago")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two stanzas of the same commit, then a different one. The
    /// second stanza carries NO metadata, which is what porcelain
    /// really emits and the reason the parser keeps a map.
    const TWO_COMMITS: &str = "\
9a17f8e18e0e5e2b3c4d5e6f708192a3b4c5d6e7 1 1 2
author Jane Doe
author-mail <jane@example.com>
author-time 1700000000
author-tz +0530
summary add the thing
filename src/main.rs
\tuse std::fs;
9a17f8e18e0e5e2b3c4d5e6f708192a3b4c5d6e7 2 2 2
\tuse std::io;
1111222233334444555566667777888899990000 3 3 1
author Sam Roe
author-time 1700100000
summary fix the other thing
filename src/main.rs
\tfn main() {}
";

    #[test]
    fn consecutive_lines_of_one_commit_are_one_chunk() {
        let chunks = parse_blame_chunks(TWO_COMMITS);
        assert_eq!(chunks.len(), 2, "{chunks:#?}");
        assert_eq!(chunks[0].start_line, 0, "porcelain is 1-based, we are not");
        assert_eq!(chunks[0].line_count, 2);
        assert_eq!(chunks[0].author, "Jane Doe");
        assert_eq!(chunks[0].summary, "add the thing");
        assert_eq!(chunks[0].time, 1700000000);
    }

    /// The bug this map exists to prevent: a later stanza for a commit
    /// already seen carries no `author` / `summary` lines at all.
    #[test]
    fn a_repeated_commit_keeps_the_metadata_from_its_first_stanza() {
        let porcelain =
            format!("{TWO_COMMITS}9a17f8e18e0e5e2b3c4d5e6f708192a3b4c5d6e7 4 4 1\n\tlater line\n");
        let chunks = parse_blame_chunks(&porcelain);
        let last = chunks.last().expect("a chunk");
        assert_eq!(last.sha, "9a17f8e18e0e5e2b3c4d5e6f708192a3b4c5d6e7");
        assert_eq!(
            last.author, "Jane Doe",
            "a repeated commit's stanza carries no author: {last:#?}"
        );
        assert_eq!(last.summary, "add the thing");
    }

    /// The same commit in two separate places is two chunks — they are
    /// two runs, and each wants its own heading.
    #[test]
    fn a_commit_appearing_twice_gets_two_chunks() {
        let porcelain =
            format!("{TWO_COMMITS}9a17f8e18e0e5e2b3c4d5e6f708192a3b4c5d6e7 4 4 1\n\tlater line\n");
        let chunks = parse_blame_chunks(&porcelain);
        assert_eq!(chunks.len(), 3, "{chunks:#?}");
        assert_eq!(chunks[0].sha, chunks[2].sha);
        assert_eq!(chunks[2].start_line, 3);
    }

    #[test]
    fn chunk_ranges_answer_which_line_belongs_to_them() {
        let chunks = parse_blame_chunks(TWO_COMMITS);
        assert_eq!(chunks[0].end_line(), 1);
        assert!(chunks[0].contains(0) && chunks[0].contains(1));
        assert!(!chunks[0].contains(2));
        assert!(chunks[1].contains(2));
    }

    /// A summary beginning with a hex word must not be read as a new
    /// commit header — the reason the header check is two fields, not
    /// one.
    #[test]
    fn a_hex_looking_summary_is_not_a_stanza_header() {
        let porcelain = "\
9a17f8e18e0e5e2b3c4d5e6f708192a3b4c5d6e7 1 1 1
author Jane Doe
summary a1b2c3d4 looks like a sha but is a summary
\tuse std::fs;
";
        let chunks = parse_blame_chunks(porcelain);
        assert_eq!(chunks.len(), 1, "{chunks:#?}");
        assert_eq!(
            chunks[0].summary,
            "a1b2c3d4 looks like a sha but is a summary"
        );
    }

    #[test]
    fn empty_or_garbage_porcelain_yields_no_chunks() {
        assert!(parse_blame_chunks("").is_empty());
        assert!(parse_blame_chunks("not porcelain at all\n").is_empty());
        // Content lines with no header before them are attributed to
        // nothing rather than to an empty sha.
        assert!(parse_blame_chunks("\torphan code line\n").is_empty());
    }

    #[test]
    fn a_heading_reads_sha_author_date_and_summary() {
        let chunk = BlameChunk {
            sha: "9a17f8e18e0e".into(),
            author: "Jane Doe".into(),
            time: 1_700_000_000,
            summary: "add the thing".into(),
            start_line: 0,
            line_count: 2,
            removal: None,
        };
        assert_eq!(
            heading_text(&chunk, 1_700_000_000 + 3 * 24 * 3600),
            "9a17f8e1  Jane Doe  3 days ago  add the thing"
        );
    }

    /// git attributes not-yet-committed lines to an all-zero sha.
    /// Rendering `00000000  Not Committed Yet` would put git's
    /// internals in a row whose whole job is to be read at a glance.
    #[test]
    fn uncommitted_lines_say_so_in_words() {
        let chunk = BlameChunk {
            sha: "0".repeat(40),
            author: "Not Committed Yet".into(),
            time: 0,
            summary: "".into(),
            start_line: 4,
            line_count: 1,
            removal: None,
        };
        assert!(is_uncommitted(&chunk.sha));
        assert_eq!(heading_text(&chunk, 1_700_000_000), "Uncommitted changes");
    }

    #[test]
    fn a_heading_omits_fields_git_did_not_give() {
        let chunk = BlameChunk {
            sha: "9a17f8e18e0e".into(),
            author: String::new(),
            time: 0,
            summary: String::new(),
            start_line: 0,
            line_count: 1,
            removal: None,
        };
        assert_eq!(
            heading_text(&chunk, 1_700_000_000),
            "9a17f8e1",
            "no empty separators for fields that are not there"
        );
    }

    #[test]
    fn relative_dates_pick_a_sensible_unit_and_pluralise() {
        let now = 2_000_000_000;
        assert_eq!(relative_date(now - 30, now), "just now");
        assert_eq!(relative_date(now - 60, now), "1 minute ago");
        assert_eq!(relative_date(now - 3 * 3600, now), "3 hours ago");
        assert_eq!(relative_date(now - 24 * 3600, now), "1 day ago");
        assert_eq!(relative_date(now - 10 * 24 * 3600, now), "1 week ago");
        assert_eq!(relative_date(now - 60 * 24 * 3600, now), "2 months ago");
        assert_eq!(relative_date(now - 800 * 24 * 3600, now), "2 years ago");
    }

    /// Clock skew across machines is ordinary in a shared repository;
    /// a future commit must not render as a negative age.
    #[test]
    fn a_commit_from_the_future_reads_as_just_now() {
        let now = 2_000_000_000;
        assert_eq!(relative_date(now + 10_000, now), "just now");
    }
}

/// MG.33: the three reverse-blame headings.
#[cfg(test)]
mod removal_headings {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn chunk(removal: Option<Removal>) -> BlameChunk {
        BlameChunk {
            sha: "aaaa1111bbbb2222".into(),
            author: "Jane Doe".into(),
            time: NOW - 60 * 60 * 24 * 3,
            summary: "add the thing".into(),
            start_line: 0,
            line_count: 1,
            removal,
        }
    }

    /// A forward blame is untouched by MG.33 — same heading, no suffix.
    #[test]
    fn a_forward_blame_heading_is_unchanged() {
        let text = heading_text(&chunk(None), NOW);
        assert_eq!(text, "aaaa1111  Jane Doe  3 days ago  add the thing");
    }

    /// **The bug MG.33 fixes.** The heading must name the commit that
    /// removed the lines, not the one that last contained them — and it
    /// must say which it is, because the two are indistinguishable by
    /// shape alone.
    #[test]
    fn a_removed_chunk_names_the_removing_commit_and_says_so() {
        let text = heading_text(
            &chunk(Some(Removal::By(RemovalCommit {
                sha: "dead9999beef8888".into(),
                author: "Sam Patel".into(),
                time: NOW - 60 * 60 * 24,
                summary: "drop the legacy path".into(),
            }))),
            NOW,
        );
        assert!(
            text.starts_with("dead9999"),
            "the REMOVING commit leads the heading: {text}"
        );
        assert!(
            !text.contains("aaaa1111"),
            "showing the last-containing sha too invites reading it as the \
             answer: {text}"
        );
        assert!(text.contains("Sam Patel"), "{text}");
        assert!(text.contains("drop the legacy path"), "{text}");
        assert!(
            text.contains("removed"),
            "a heading shaped like a forward blame must say what it means: {text}"
        );
    }

    /// Surviving lines say so. Before MG.33 they rendered identically
    /// to removed ones, so the reader had to notice the sha happened to
    /// be HEAD's.
    #[test]
    fn a_surviving_chunk_says_still_present() {
        let text = heading_text(&chunk(Some(Removal::StillPresent)), NOW);
        assert!(text.starts_with("aaaa1111"), "{text}");
        assert!(text.contains("still present"), "{text}");
        assert!(
            !text.contains("removed"),
            "nothing removed these lines: {text}"
        );
    }

    /// The honest fallback. It states what git established — this is
    /// where the lines were last seen — and claims nothing about what
    /// removed them.
    #[test]
    fn an_ambiguous_chunk_claims_only_what_git_established() {
        let text = heading_text(&chunk(Some(Removal::Ambiguous)), NOW);
        assert!(text.starts_with("aaaa1111"), "{text}");
        assert!(text.contains("last contained here"), "{text}");
        assert!(
            !text.contains("removed"),
            "the ambiguous case must not imply a removal it could not \
             identify: {text}"
        );
    }

    /// Uncommitted lines short-circuit before the removal branch —
    /// `0000000..HEAD` is not a range, and the sentence is what the
    /// reader needs either way.
    #[test]
    fn uncommitted_wins_over_every_removal_state() {
        for removal in [None, Some(Removal::StillPresent), Some(Removal::Ambiguous)] {
            let mut c = chunk(removal);
            c.sha = "0".repeat(40);
            assert_eq!(heading_text(&c, NOW), "Uncommitted changes");
        }
    }
}
