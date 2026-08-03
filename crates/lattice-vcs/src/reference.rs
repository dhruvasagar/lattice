use crate::{Repository, Result, VcsError};

/// Reference operations — resolve a named ref to an object id.
pub struct Reference;

impl Reference {
    /// Resolve a reference name (e.g., `"HEAD"`, `"refs/heads/main"`,
    /// `"main"`) to its target object id.
    ///
    /// Uses `git rev-parse --verify <name>`.
    /// Short names like `"main"` are resolved via git's standard ref
    /// resolution rules.
    pub fn resolve(repo: &Repository, name: &str) -> Result<gix::ObjectId> {
        let hex = repo
            .run_git_str(["rev-parse", "--verify", name])
            .map_err(|e| VcsError::ReferenceNotFound(format!("{}: {}", name, e)))?;
        let hex = hex.trim();
        let oid: gix::ObjectId = hex
            .parse()
            .map_err(|_| VcsError::ReferenceNotFound(format!("invalid oid: {}", hex)))?;
        Ok(oid)
    }

    /// Return the symbolic target of a reference, if it is symbolic
    /// (e.g., `"HEAD"` → `"refs/heads/main"`).
    ///
    /// Uses `git symbolic-ref -q <name>`.
    pub fn symbolic_target(repo: &Repository, name: &str) -> Result<Option<String>> {
        match repo.run_git_str(["symbolic-ref", "-q", name]) {
            Ok(target) => Ok(Some(target.trim().to_string())),
            Err(_) => Ok(None),
        }
    }

    /// Every local branch, remote-tracking branch and tag, in one call.
    ///
    /// MG.35. [`crate::Branch::list`] answers "what branches are there";
    /// this answers "what refs are there, and where does each point" —
    /// the question magit's refs buffer exists for. One `for-each-ref`
    /// rather than three walks plus an ahead/behind count per branch:
    /// git computes `%(upstream:track)` while it is already reading the
    /// ref, so the whole buffer costs a single invocation regardless of
    /// how many branches the repository has.
    pub fn list(repo: &Repository) -> Result<Vec<RefEntry>> {
        let out = repo
            .run_git_str([
                "for-each-ref",
                REF_FORMAT,
                "refs/heads",
                "refs/remotes",
                "refs/tags",
            ])
            .map_err(|e| VcsError::ReferenceNotFound(format!("for-each-ref: {e}")))?;
        Ok(parse_for_each_ref(&out))
    }
}

/// What `Reference::list` asks `for-each-ref` for.
///
/// NUL-separated, because every one of these fields can contain a space
/// and `%(subject)` can contain almost anything. A tab-separated format
/// would work until someone writes a tab in a commit subject, which is
/// the kind of failure that shows up once and is never reproduced.
const REF_FORMAT: &str = "--format=%(refname)%00%(objectname)%00%(objectname:short)%00\
                          %(upstream:short)%00%(upstream:track)%00\
                          %(HEAD)%00%(contents:subject)";

/// What kind of thing a ref is. The three groups magit's refs buffer
/// shows, in the order it shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RefKind {
    /// `refs/heads/*`.
    Branch,
    /// `refs/remotes/*`.
    Remote,
    /// `refs/tags/*`.
    Tag,
}

/// One ref, and where it points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefEntry {
    pub kind: RefKind,
    /// The short name — `main`, `origin/main`, `v1.0.0`.
    pub name: String,
    /// Full object id. Callers that hand an id to another git command
    /// use this one: an abbreviation is ambiguous in principle, and git
    /// resolves the ambiguity by refusing — which would surface as a ref
    /// that opens nothing.
    pub id: String,
    /// Abbreviated object id, for display.
    pub short_id: String,
    /// The configured upstream's short name, empty when there is none.
    /// Only ever set for [`RefKind::Branch`].
    pub upstream: String,
    /// Git's own ahead/behind summary — `[ahead 2]`, `[behind 1]`,
    /// `[ahead 3, behind 1]`, `[gone]` — with the brackets stripped.
    /// Empty when the branch is level with its upstream or has none.
    pub track: String,
    /// Is this the checked-out branch?
    pub head: bool,
    /// The subject line of the commit (or of the tag, when annotated).
    pub subject: String,
}

/// Parse [`REF_FORMAT`] output into entries.
///
/// **A malformed line is skipped, not fatal** — the same judgement
/// [`crate::remote::parse_remote_v`] makes and for the same reason: one
/// unparseable ref must not blank the whole buffer and hide every ref
/// that is fine. A ref under none of the three prefixes is also skipped;
/// `refs/stash`, `refs/notes/*` and `refs/bisect/*` are real refs that
/// this buffer deliberately does not show.
pub fn parse_for_each_ref(out: &str) -> Vec<RefEntry> {
    out.lines().filter_map(parse_ref_line).collect()
}

fn parse_ref_line(line: &str) -> Option<RefEntry> {
    let mut f = line.split('\0');
    let refname = f.next()?;
    let id = f.next()?.to_string();
    let short_id = f.next()?.to_string();
    let upstream = f.next()?.to_string();
    let track = f.next()?.trim().to_string();
    let head = f.next()? == "*";
    // Subject last, and taken whole: it is free text and may contain
    // anything except the NUL that separates it.
    let subject = f.next().unwrap_or_default().to_string();

    let (kind, name) = if let Some(n) = refname.strip_prefix("refs/heads/") {
        (RefKind::Branch, n)
    } else if let Some(n) = refname.strip_prefix("refs/remotes/") {
        (RefKind::Remote, n)
    } else if let Some(n) = refname.strip_prefix("refs/tags/") {
        (RefKind::Tag, n)
    } else {
        return None;
    };
    if name.is_empty() {
        return None;
    }
    Some(RefEntry {
        kind,
        name: name.to_string(),
        id,
        short_id,
        upstream,
        // Git prints the summary bracketed; the brackets are
        // presentation, and the renderer supplies its own.
        track: track
            .strip_prefix('[')
            .and_then(|t| t.strip_suffix(']'))
            .unwrap_or(&track)
            .to_string(),
        head,
        subject,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three prefixes map to the three kinds, and the short name
    /// drops the prefix — `origin/main` keeps its remote, `main` does
    /// not keep `refs/heads/`.
    #[test]
    fn each_prefix_maps_to_its_kind_with_the_prefix_stripped() {
        let out = "refs/heads/main\0aaaa111\0a1b2c3d\0origin/main\0\0*\0the subject\n\
                   refs/remotes/origin/main\0aaaa111\0a1b2c3d\0\0\0\0the subject\n\
                   refs/tags/v1.0.0\0eeee444\0e4f5g6h\0\0\0\0tagged\n";
        let refs = parse_for_each_ref(out);
        assert_eq!(refs.len(), 3);
        assert_eq!((refs[0].kind, refs[0].name.as_str()), (RefKind::Branch, "main"));
        assert_eq!(
            (refs[1].kind, refs[1].name.as_str()),
            (RefKind::Remote, "origin/main")
        );
        assert_eq!((refs[2].kind, refs[2].name.as_str()), (RefKind::Tag, "v1.0.0"));
        assert!(refs[0].head, "the `*` marks the checked-out branch");
        assert!(!refs[1].head);
    }

    /// The brackets git prints around the tracking summary are stripped
    /// once, here, so no renderer has to know they were there. `[gone]`
    /// matters as much as the counts: it is how a branch whose upstream
    /// was deleted announces itself.
    #[test]
    fn the_tracking_summary_loses_its_brackets_and_keeps_its_text() {
        let out = "refs/heads/a\0aaa\0a1\0origin/a\0[ahead 3, behind 1]\0\0s\n\
                   refs/heads/b\0bbb\0b1\0origin/b\0[gone]\0\0s\n\
                   refs/heads/c\0ccc\0c1\0origin/c\0\0\0s\n";
        let refs = parse_for_each_ref(out);
        assert_eq!(refs[0].track, "ahead 3, behind 1");
        assert_eq!(refs[1].track, "gone");
        assert_eq!(refs[2].track, "", "level with upstream reports nothing");
    }

    /// A subject containing the field separator's *near misses* survives
    /// whole. This is the reason the format is NUL-separated rather than
    /// tab- or space-separated: commit subjects are free text.
    #[test]
    fn a_subject_with_tabs_and_brackets_survives_whole() {
        let out = "refs/heads/main\0aaa\0a1\0\0\0\0fix:\ttabs [and] brackets\n";
        let refs = parse_for_each_ref(out);
        assert_eq!(refs[0].subject, "fix:\ttabs [and] brackets");
    }

    /// Refs outside the three prefixes are dropped rather than rendered
    /// as an unnamed row — `refs/stash` and `refs/notes/*` are real refs
    /// this buffer deliberately does not show. Truncated lines are
    /// dropped for the same reason `parse_remote_v` drops them: one bad
    /// ref must not blank the list.
    #[test]
    fn unknown_prefixes_and_truncated_lines_are_skipped_not_fatal() {
        let out = "refs/stash\0aaa\0a1\0\0\0\0wip\n\
                   refs/heads/\0aaa\0a1\0\0\0\0empty name\n\
                   truncated\n\
                   refs/heads/good\0aaa\0a1\0\0\0\0kept\n";
        let refs = parse_for_each_ref(out);
        assert_eq!(refs.len(), 1, "only the good one survives: {refs:?}");
        assert_eq!(refs[0].name, "good");
    }
}
