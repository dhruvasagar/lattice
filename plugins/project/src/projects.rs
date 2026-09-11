//! PC.4 — the remembered-projects list: its order, its format, and nothing else.
//!
//! Design: `docs/dev/architecture/project-commands.md` §4.
//!
//! Every function here is pure. The seam wiring — the `document-opened`
//! subscription, the `store-*` calls, the ex-commands — lives in `lib.rs`, so
//! the list's semantics can be tested without a host.
//!
//! ## The format IS the ordering
//!
//! One absolute path per line, most-recently-visited FIRST.
//!
//! The design fragment originally specified `msgpack(Vec<Remembered { root,
//! last_visited_seq }>)`, and the counter existed for exactly one purpose:
//! ordering the picker so switching back and forth between two projects does
//! not require typing. A list is already ordered. So the counter was a second
//! encoding of the thing the container encodes for free, and both it and the
//! msgpack dependency are gone — a bundled guest should not pull serde and rmp
//! into a wasm artifact to persist a list of paths.
//!
//! What that buys beyond size: the store value is readable in a hex dump when
//! something is wrong, and a format change cannot produce a schema-skew failure
//! mode, only a line that does not parse.
//!
//! ## A path containing a newline is refused, not escaped
//!
//! It is legal on unix and it is pathological. Escaping would put a decoder in
//! the one place this module exists to keep simple, and the honest failure is to
//! decline to remember that one project — which [`remember`] reports so the
//! caller can say so, rather than silently dropping it.

/// The most projects kept. A bound, not a policy: the picker is fuzzy-matched
/// so a long list costs nothing to USE, but an unbounded store value grows
/// forever in a long-lived config directory, and the tail of it is projects the
/// user has not opened in months.
///
/// Dropping happens at the BACK — least-recently-visited — so the entry lost is
/// always the one the user is least likely to want.
pub const MAX_REMEMBERED: usize = 256;

/// Why a `remember` did not store the path.
///
/// One variant, because there is only one reason this layer can refuse. The
/// other refusal a reader will look for — "that was not a project, it was the
/// working directory standing in" — happens at the RESOLUTION boundary in
/// `lib.rs`, where `project-kind = pwd` is filtered before a root ever reaches
/// this module. Keeping it there means this module never has to know what a
/// project *is*, only what it can store.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Empty, or a path carrying a newline (see the module note).
    Unstorable,
}

/// Normalise a root for comparison and storage.
///
/// Trailing slashes only: `~/src/lattice` and `~/src/lattice/` are one project,
/// and the host hands back whichever the marker walk produced. No canonicalising
/// beyond that — the plugin holds no `fs:` grant, so it cannot resolve symlinks
/// and must not pretend to. Two spellings of one tree via a symlink therefore
/// remember as two entries, which is visible in the picker (both paths show) and
/// is the honest outcome rather than a guess.
pub fn normalize(root: &str) -> String {
    let trimmed = root.trim();
    if trimmed.len() > 1 {
        trimmed.trim_end_matches('/').to_string()
    } else {
        trimmed.to_string()
    }
}

/// Is this storable in the line format?
fn storable(root: &str) -> bool {
    !root.is_empty() && !root.contains('\n') && !root.contains('\r')
}

/// Put `root` at the front, removing any earlier occurrence.
///
/// `Ok(true)` when the list actually changed — the caller writes the store only
/// then. `Ok(false)` is the already-at-the-front case, which is the
/// overwhelmingly common one: every file you open in the project you are
/// already working in. It matters because this is on the `document-opened`
/// path, so an unconditional write would touch the store once per file opened
/// to store bytes it already held.
///
/// Returns `Err` rather than silently doing nothing, so a caller can echo why —
/// a `:project-remember` that appears to succeed and stores nothing is the
/// failure this signature exists to prevent.
pub fn remember(list: &mut Vec<String>, root: &str) -> Result<bool, Refused> {
    let root = normalize(root);
    if !storable(&root) {
        return Err(Refused::Unstorable);
    }
    // Already at the front: nothing to reorder, and the caller skips the store
    // write. This is the overwhelmingly common case — every file you open in
    // the project you are already working in — and it is on the
    // `document-opened` path, which fires per file opened. Reported through
    // `Changed` rather than by comparing lists at the call site, because only
    // this function knows whether the move mattered.
    if list.first().is_some_and(|first| first == &root) {
        return Ok(false);
    }
    // Remove-then-push-front rather than "skip if present": re-visiting a
    // project must MOVE it, or the ordering stops meaning most-recent and the
    // picker's first row goes stale the moment you switch back and forth.
    list.retain(|p| p != &root);
    list.insert(0, root);
    list.truncate(MAX_REMEMBERED);
    Ok(true)
}

/// Drop `root`. Returns whether it was there — `:project-forget` on a path that
/// was never remembered should say so rather than report a removal that did not
/// happen.
pub fn forget(list: &mut Vec<String>, root: &str) -> bool {
    let root = normalize(root);
    let before = list.len();
    list.retain(|p| p != &root);
    list.len() != before
}

/// Decode a stored list. Blank lines are skipped; nothing else can fail.
///
/// Lossy UTF-8 rather than a hard error: a corrupt store must degrade to a
/// shorter list, never to a plugin that cannot start. The alternative — refusing
/// to load — turns one bad byte into "the feature is gone" with no way for the
/// user to see why.
pub fn decode(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// Encode for the store.
pub fn encode(list: &[String]) -> Vec<u8> {
    let mut out = list.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out.into_bytes()
}

/// The name shown in the picker — the last path component.
///
/// The full path rides alongside as the annotation rather than replacing this:
/// `magit-repo-scoping.md` §3.1's rule, and its reasoning — names are read far
/// more often than they are parsed, and two checkouts can share a basename
/// (`~/work/api` and `~/oss/api`), so the basename is for humans and the path
/// is the identity.
pub fn basename(root: &str) -> &str {
    root.rsplit('/').find(|s| !s.is_empty()).unwrap_or(root)
}

/// PB.1: does `path` live inside `root`?
///
/// **Component-wise, not a string prefix**, which is the whole reason this is
/// a function rather than a `starts_with` at the call site: `~/src/lattice`
/// and `~/src/lattice-old` share a prefix and are different projects, and a
/// buffer from the second appearing in the first's list is a wrong answer that
/// looks like a right one.
///
/// The root itself counts as inside it, so a directory buffer at the root is
/// not excluded on a technicality.
///
/// No canonicalising — the plugin holds no `fs:` grant, so it cannot resolve
/// symlinks and must not pretend to. Same honesty [`normalize`] already
/// records: a tree reached through a symlink reads as a different path, and
/// the list says so rather than guessing.
pub fn is_under(root: &str, path: &str) -> bool {
    let root = normalize(root);
    if root.is_empty() {
        return false;
    }
    let Some(rest) = path.strip_prefix(&root) else {
        return false;
    };
    // `rest` is what follows the root: empty (the root itself), or something
    // that must begin at a component boundary. Without this check
    // `lattice-old` strips to `-old` and passes.
    rest.is_empty() || rest.starts_with('/')
}

/// PB.1: `path` spelled relative to `root`, for display.
///
/// Returns `path` verbatim when it is not under `root` — a caller that
/// filtered with [`is_under`] never sees that, and a caller that did not gets
/// something readable rather than a mangled suffix.
pub fn relative_to(root: &str, path: &str) -> String {
    if !is_under(root, path) {
        return path.to_string();
    }
    let root = normalize(root);
    path[root.len()..].trim_start_matches('/').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// **The prefix trap, and the whole reason `is_under` is a function.**
    /// `~/src/lattice` and `~/src/lattice-old` share a string prefix and are
    /// different projects; a buffer from the second showing up in the first's
    /// list is a wrong answer that looks like a right one.
    #[test]
    fn containment_is_component_wise_not_a_string_prefix() {
        assert!(is_under("/src/lattice", "/src/lattice/crates/a.rs"));
        assert!(
            !is_under("/src/lattice", "/src/lattice-old/crates/a.rs"),
            "a sibling sharing the prefix is a different project"
        );
        assert!(
            !is_under("/src/lattice", "/src/other/a.rs"),
            "an unrelated tree is not inside it"
        );
    }

    /// The root itself is inside itself — a directory buffer sitting at the
    /// root must not be excluded on a technicality.
    #[test]
    fn the_root_counts_as_inside_itself() {
        assert!(is_under("/src/lattice", "/src/lattice"));
        assert!(is_under("/src/lattice", "/src/lattice/"));
        assert!(
            is_under("/src/lattice/", "/src/lattice/a.rs"),
            "a trailing slash on the root changes nothing — `normalize`'s job"
        );
    }

    /// An empty root matches nothing. It is what a context arrives with before
    /// a project resolves, and treating `""` as a prefix would put every open
    /// buffer in the list.
    #[test]
    fn an_empty_root_contains_nothing() {
        assert!(!is_under("", "/src/lattice/a.rs"));
    }

    #[test]
    fn a_path_is_shown_relative_to_its_root() {
        assert_eq!(
            relative_to("/src/lattice", "/src/lattice/crates/host/a.rs"),
            "crates/host/a.rs"
        );
        assert_eq!(
            relative_to("/src/lattice", "/src/lattice/README.md"),
            "README.md",
            "a file at the root keeps no leading slash"
        );
        assert_eq!(
            relative_to("/src/lattice", "/elsewhere/a.rs"),
            "/elsewhere/a.rs",
            "something outside the root is returned whole rather than mangled"
        );
    }

    #[test]
    fn remembering_puts_a_project_at_the_front() {
        let mut l = list(&["/a"]);
        remember(&mut l, "/b").unwrap();
        assert_eq!(l, list(&["/b", "/a"]));
    }

    /// The property the whole ordering exists for: re-visiting MOVES rather
    /// than duplicating or leaving in place. A "skip if present" implementation
    /// passes a dedupe test and fails this one.
    #[test]
    fn revisiting_moves_a_project_to_the_front() {
        let mut l = list(&["/a", "/b", "/c"]);
        remember(&mut l, "/c").unwrap();
        assert_eq!(l, list(&["/c", "/a", "/b"]));
        assert_eq!(l.len(), 3, "moved, not duplicated");
    }

    #[test]
    fn a_trailing_slash_is_the_same_project() {
        let mut l = Vec::new();
        remember(&mut l, "/a/b").unwrap();
        remember(&mut l, "/a/b/").unwrap();
        assert_eq!(l, list(&["/a/b"]), "one project, not two");
        assert!(
            forget(&mut l, "/a/b/"),
            "and forgetting matches either spelling"
        );
    }

    /// Root itself must survive normalisation — `trim_end_matches('/')` on "/"
    /// yields the empty string, which would then be refused as unstorable.
    #[test]
    fn the_filesystem_root_normalises_to_itself() {
        assert_eq!(normalize("/"), "/");
        let mut l = Vec::new();
        assert!(remember(&mut l, "/").is_ok());
    }

    /// The already-first case reports "nothing changed", which is what lets the
    /// caller skip the store write on the hot path — every file opened in the
    /// project you are already in.
    #[test]
    fn remembering_the_current_front_reports_no_change() {
        let mut l = list(&["/a", "/b"]);
        assert_eq!(remember(&mut l, "/a"), Ok(false));
        assert_eq!(l, list(&["/a", "/b"]), "and leaves the order alone");
        assert_eq!(remember(&mut l, "/b"), Ok(true), "a real move still reports");
    }

    #[test]
    fn a_path_with_a_newline_is_refused_rather_than_stored() {
        let mut l = Vec::new();
        assert_eq!(remember(&mut l, "/a\nb"), Err(Refused::Unstorable));
        assert_eq!(remember(&mut l, ""), Err(Refused::Unstorable));
        assert!(l.is_empty());
    }

    #[test]
    fn forgetting_reports_whether_it_removed_anything() {
        let mut l = list(&["/a"]);
        assert!(forget(&mut l, "/a"));
        assert!(!forget(&mut l, "/a"), "already gone");
        assert!(l.is_empty());
    }

    #[test]
    fn the_list_is_bounded_and_drops_from_the_back() {
        let mut l: Vec<String> = (0..MAX_REMEMBERED).map(|n| format!("/p{n}")).collect();
        let oldest = l[MAX_REMEMBERED - 1].clone();
        remember(&mut l, "/fresh").unwrap();
        assert_eq!(l.len(), MAX_REMEMBERED);
        assert_eq!(l[0], "/fresh");
        assert!(
            !l.contains(&oldest),
            "the LEAST recently visited is the one dropped"
        );
    }

    #[test]
    fn a_basename_is_the_last_component() {
        assert_eq!(basename("/src/lattice"), "lattice");
        assert_eq!(basename("/src/lattice/"), "lattice");
        assert_eq!(basename("lattice"), "lattice");
        assert_eq!(basename("/"), "/");
    }

    #[test]
    fn encode_and_decode_round_trip() {
        let l = list(&["/a", "/b/c"]);
        assert_eq!(decode(&encode(&l)), l);
        assert_eq!(decode(&encode(&[])), Vec::<String>::new());
    }

    /// A corrupt store degrades to a shorter list rather than to a plugin that
    /// cannot start.
    #[test]
    fn a_corrupt_store_decodes_to_what_survives() {
        assert_eq!(decode(b"/a\n\n\n/b\n"), list(&["/a", "/b"]));
        assert_eq!(decode(&[0xff, 0xfe]).len(), 1, "lossy, not fatal");
    }
}
