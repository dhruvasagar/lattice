//! B3: repository discovery, in one place.
//!
//! Every magit mode needs "where is the repository", and every one of
//! them spelled it out — eleven copies of
//! `Repository::discover(…).ok().and_then(|r| r.workdir().map(…))`.
//!
//! **This is not tidying.** `gix::discover` takes a *directory*, and
//! passing it a file path fails silently: `discover` returns `Err`, the
//! `.ok()` swallows it, and the caller gets a default. MG.11 found
//! three sites doing exactly that, one of them in `lattice-host`'s
//! auto-head-diff subsystem — which meant gutter diff signs had never
//! worked, for any file, since they landed. Two functions, one per
//! question, make that mistake unrepresentable: a caller with a file
//! path cannot reach the directory-taking one.

use std::path::{Path, PathBuf};

use lattice_vcs::Repository;

/// The working directory of the repository containing the current
/// directory.
///
/// `None` for "not in a repository" and for a bare one (which has no
/// working tree, so there is nothing for a magit buffer to show). Most
/// callers want `.unwrap_or_default()` — an empty path makes the
/// subsequent git call fail and the buffer say so, which is the same
/// outcome the hand-written copies produced.
pub(crate) fn magit_workdir() -> Option<PathBuf> {
    magit_workdir_from(Path::new("."))
}

/// [`magit_workdir`] discovering from `start` rather than the process's
/// working directory.
///
/// PR.5: the no-arg form discovers from `"."`, which is the **process**
/// cwd — and `:cd` sets `editor.current_dir` without ever calling
/// `set_current_dir`, so after `:cd /other/repo` a fresh `C-x g` still
/// opened the repository the editor was launched in. Callers that can
/// reach the project resolver pass its answer here instead
/// (`RepoScopes::discovery_start`).
///
/// `start` is where discovery *begins*, not the answer: magit needs a
/// git worktree specifically and a project root need not be one, so
/// `gix` still walks up and still returns `None` outside a repository.
pub(crate) fn magit_workdir_from(start: &Path) -> Option<PathBuf> {
    let repo = Repository::discover(start).ok()?;
    let workdir = repo.workdir()?.to_path_buf();
    // MR.3b: discovery from `.` reports the workdir RELATIVE to the
    // current directory, so an editor started in `crates/lattice-ui-tui`
    // gets `../..`. That was harmless while the path was only ever
    // handed to `git -C`; it is not harmless now that the path is
    // labelled and compared:
    //
    //   - `repo_label("../..")` has no final component, so the buffer
    //     came out as `*magit:log:../..*` — a name for no repository in
    //     particular, and a different one from every other directory.
    //   - the record compares workdir paths to detect a basename
    //     collision, and `../..` and `/Users/…/lattice` are different
    //     paths for the same checkout.
    //
    // Canonicalising here rather than at each caller keeps the one
    // relative source in one place. Falls back to the raw path if the
    // filesystem refuses, which is what it did before.
    Some(std::fs::canonicalize(&workdir).unwrap_or(workdir))
}

/// The `(workdir, repo-relative-path)` pair for a file on disk.
///
/// Takes the file's **parent** to `discover`, which is the whole point
/// of this existing separately — see the module note. The relative
/// path falls back to the input when it does not sit under the
/// workdir, which is what a symlinked or otherwise unexpected path
/// does; git will then reject it by name rather than silently acting
/// on something else.
pub(crate) fn workdir_for_file(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let repo = Repository::discover(path.parent()?).ok()?;
    let workdir = repo.workdir()?.to_path_buf();
    let rel = path.strip_prefix(&workdir).unwrap_or(path).to_path_buf();
    Some((workdir, rel))
}

/// MR.1: which repository a magit trigger should act on.
///
/// Design: `docs/dev/architecture/magit-repo-scoping.md` §2. Three
/// questions, in this order, and the order is the design:
///
/// 1. **`from_magit_buffer`** — the active buffer is itself a magit
///    buffer, so use the repository it is already showing. Without this,
///    `C-x g` inside repo B's log would jump back to the cwd repo: a
///    magit chord pressed inside magit would silently change which
///    repository you are working on.
/// 2. **`active_file`** — the file in front of you decides. This is the
///    point of the change: opening a file from another checkout and
///    acting on *its* repository, without restarting the editor there.
/// 3. **cwd** — what every magit surface did unconditionally before.
///    Kept as the fallback so a fresh editor with nothing open still
///    answers `C-x g`; that is what makes this a widening rather than a
///    trade.
///
/// Takes its inputs rather than reaching for the active buffer itself:
/// the callers (an action handler, an ex-command, a transient row) reach
/// the active buffer by different routes, and a resolver that picked one
/// would be usable from only some of them.
pub(crate) fn repo_for_trigger(
    from_magit_buffer: Option<PathBuf>,
    active_file: Option<&Path>,
    fallback_start: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(workdir) = from_magit_buffer {
        return Some(workdir);
    }
    if let Some(path) = active_file
        && let Some((workdir, _rel)) = workdir_for_file(path)
    {
        return Some(workdir);
    }
    // PR.5: step 3 discovers from the project resolver's answer when one
    // is reachable, so `:cd` is honoured. `None` keeps the pre-PR.5
    // behaviour for harnesses with no resolver registered.
    magit_workdir_from(fallback_start.unwrap_or(Path::new(".")))
}

/// MR.1: the human-readable repository label that goes in a magit
/// buffer's name — the repository directory's basename.
///
/// **Not the source of truth for where git runs.** A basename cannot
/// round-trip to a path, and two checkouts can share one
/// (`~/work/api`, `~/oss/api`), so the workdir is carried beside the
/// buffer rather than parsed back out of its name (design §3.1). This
/// exists because `:ls` is read far more often than a buffer name is
/// parsed, and `*magit:status:/Users/…/src/lattice*` is not a name.
pub(crate) fn repo_label(workdir: &Path) -> String {
    workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        // A workdir with no final component is the filesystem root, or a
        // path ending in `..`. Rare and not worth an unnamed buffer, so
        // fall back to the whole path rather than to an empty label.
        .unwrap_or_else(|| workdir.to_string_lossy().into_owned())
}

/// MR.1: the label qualified with its parent directory, for when two
/// repositories share a basename.
///
/// Reached only on collision (design §3.1): the alternative is two
/// repositories sharing one buffer, with the staging chords acting on
/// whichever was recorded last — the worst outcome available here.
pub(crate) fn qualified_repo_label(workdir: &Path) -> String {
    let base = repo_label(workdir);
    match workdir.parent().and_then(|p| p.file_name()) {
        Some(parent) => format!("{}/{base}", parent.to_string_lossy()),
        None => base,
    }
}

/// MR.3: the shape every magit view buffer's name has.
///
/// ```text
/// *magit:<view>[:<repo>[:<rest>]]*
///          │       │        └─ whatever the view encodes: a path, a
///          │       │           revision, a diff scope, a stash index
///          │       └─ the repository's label — ALWAYS segment 2
///          └─ which view this is
/// ```
///
/// **Segment 2 is the repository, always.** That rule is what makes the
/// name parseable at all once views carry both a repository and their
/// own parameters: `*magit:log:src*` is otherwise unreadable — repo
/// `src`, or the log of the path `src`? Fixing the position answers it
/// without asking the record, and the only way to keep the rule true is
/// to never produce a `rest` without a repo segment before it (hence the
/// empty segment in [`magit_buffer_name_with`] when there is no repo).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct MagitName<'a> {
    pub view: &'a str,
    /// `None` outside a repository, where the trigger has nothing to
    /// label with. Not an error — the view opens and says so.
    pub repo: Option<&'a str>,
    /// The view's own encoded parameters, unsplit: each view knows its
    /// own shape and parses this itself.
    pub rest: Option<&'a str>,
}

/// MR.2/MR.3: the one producer of a magit view's buffer name.
///
/// `("status", "lattice")` → `*magit:status:lattice*`. The label comes
/// from [`repo_label`] (or [`qualified_repo_label`] on a collision), not
/// from the workdir directly, because the caller — not this function —
/// is the one that knows whether the plain form is already taken.
///
/// One producer and one parser ([`parse_magit_name`]), every caller
/// through them: MG.15 lost every stash chord to a hand-rolled producer
/// drifting from its parser, and this name has more callers than that
/// one did.
pub(crate) fn magit_buffer_name(view: &str, label: &str) -> String {
    if label.is_empty() {
        // The repo-less form: what every magit buffer was called before
        // MR.2, and still what a trigger outside any repository opens.
        return format!("*magit:{view}*");
    }
    format!("*magit:{view}:{label}*")
}

/// The same producer for a view that encodes parameters of its own —
/// a path, a revision, a diff scope.
///
/// With no repository the repo segment is left **empty** rather than
/// omitted (`*magit:diff::staged:src/main.rs*`). Omitting it would slide
/// `staged` into segment 2 and the parser would report it as the
/// repository — the ambiguity the fixed position exists to prevent. The
/// name is admittedly odd, and it is only reachable outside any
/// repository, where the view's whole content is "Not a git repository."
pub(crate) fn magit_buffer_name_with(view: &str, label: &str, rest: &str) -> String {
    format!("*magit:{view}:{label}:{rest}*")
}

/// MR.3: the parser half — `*magit:<view>[:<repo>[:<rest>]]*` split into
/// its three parts.
///
/// `None` for anything that is not a magit buffer name at all. An empty
/// repo segment reads back as `None`, which is the round-trip of
/// [`magit_buffer_name_with`]'s no-repository form.
pub(crate) fn parse_magit_name(name: &str) -> Option<MagitName<'_>> {
    let body = name.strip_prefix("*magit:")?.strip_suffix('*')?;
    let mut parts = body.splitn(3, ':');
    let view = parts.next().filter(|v| !v.is_empty())?;
    Some(MagitName {
        view,
        repo: parts.next().filter(|r| !r.is_empty()),
        rest: parts.next().filter(|r| !r.is_empty()),
    })
}

/// MR.2: does this name belong to a magit buffer at all?
///
/// The first of design §2's three questions — "is the active buffer a
/// magit buffer" — asked of the only thing a trigger has to hand: the
/// name. A magit chord pressed inside magit must not change which
/// repository you are working on.
pub(crate) fn is_magit_buffer_name(name: &str) -> bool {
    name.starts_with("*magit:") && name.ends_with('*')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// The bug this module exists to prevent, stated directly: a file
    /// path handed to the directory-taking discovery finds nothing.
    ///
    /// Asserted against `gix` itself rather than against our wrapper,
    /// because the behaviour being relied on is `gix`'s — if a future
    /// version starts accepting file paths, this test says so instead
    /// of the split quietly becoming pointless.
    #[test]
    fn discovery_needs_a_directory_which_is_why_the_file_form_is_separate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        std::fs::write(p.join("a.txt"), "x\n").unwrap();

        assert!(
            Repository::discover(p).is_ok(),
            "a directory discovers the repo"
        );
        assert!(
            Repository::discover(p.join("a.txt")).is_err(),
            "a FILE path does not — and `.ok()` on this is what made \
             three sites fail silently before MG.11 found them"
        );
    }

    #[test]
    fn workdir_for_file_resolves_the_path_relative_to_the_repository() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        std::fs::create_dir(p.join("src")).unwrap();
        let file = p.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let (workdir, rel) = workdir_for_file(&file).expect("inside a repo");
        assert_eq!(rel, Path::new("src/main.rs"), "repo-relative, not absolute");
        assert!(
            file.starts_with(&workdir),
            "and the workdir must contain it: {workdir:?}"
        );
    }

    /// A path with no parent cannot be discovered from, and saying so
    /// beats discovering from the process's current directory — which
    /// would silently resolve to whatever repo the editor was launched
    /// in.
    #[test]
    fn a_parentless_path_is_declined_rather_than_guessed() {
        assert!(workdir_for_file(Path::new("")).is_none());
    }

    // ── MR.1: the resolver ───────────────────────────────────────

    /// The magit buffer's own repository wins, and it wins over a file
    /// that would resolve elsewhere. This is the rule that stops a magit
    /// chord pressed inside magit from switching repositories under you.
    #[test]
    fn a_magit_buffers_own_repo_wins_over_everything() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        std::fs::create_dir_all(p.join("src")).unwrap();
        let file = p.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let showing = PathBuf::from("/somewhere/else");
        assert_eq!(
            repo_for_trigger(Some(showing.clone()), Some(&file), None),
            Some(showing),
            "the buffer in front of you decides, not the file underneath it"
        );
    }

    /// The point of the change: the active file's repository, not the
    /// process's. Asserted with a REAL second repo, since the failure
    /// mode is "silently resolved to the cwd one".
    #[test]
    fn the_active_files_repo_beats_the_working_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        std::fs::create_dir_all(p.join("src")).unwrap();
        let file = p.join("src").join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        let resolved = repo_for_trigger(None, Some(&file), None).expect("the file is in a repo");
        // `canonicalize` because a tempdir under /var is a symlink to
        // /private/var on macOS, and git reports the resolved form —
        // comparing the raw paths fails for a reason that has nothing to
        // do with the behaviour under test.
        assert_eq!(
            resolved.canonicalize().ok(),
            p.canonicalize().ok(),
            "resolved {resolved:?}, wanted the file's own repo"
        );
    }

    /// No magit buffer and no file (a scratch buffer, `*messages*`) —
    /// fall through to the working directory rather than refusing, so a
    /// fresh editor still answers `C-x g`.
    #[test]
    fn with_neither_it_falls_back_to_the_working_directory() {
        assert_eq!(
            repo_for_trigger(None, None, None),
            magit_workdir(),
            "the fallback IS the old behaviour, unchanged"
        );
    }

    /// PR.5: the fallback discovers from where it is TOLD, not from the
    /// process's working directory.
    ///
    /// This is the bug. `:cd` sets `editor.current_dir` and never calls
    /// `set_current_dir`, so before PR.5 step 3 was
    /// `Repository::discover(".")` — after `:cd /other/repo`, a fresh
    /// `C-x g` still opened the repository the editor was *launched* in.
    #[test]
    fn the_fallback_discovers_from_the_directory_it_is_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("elsewhere");
        std::fs::create_dir_all(&repo).expect("mkdir");
        std::process::Command::new("git")
            .arg("init")
            .current_dir(&repo)
            .output()
            .expect("git init");
        let canonical = std::fs::canonicalize(&repo).expect("canonicalize");

        let got = repo_for_trigger(None, None, Some(&canonical)).expect("a repo was found");
        assert_eq!(
            got, canonical,
            "step 3 must use the directory it was handed, not the process cwd"
        );
        // And the pre-PR.5 path is genuinely different — otherwise this
        // would pass without the fix whenever the suite runs inside a
        // repository, which it always does.
        assert_ne!(
            got,
            magit_workdir().expect("the suite runs inside a repository"),
            "the process cwd must not coincidentally be the same repo"
        );
    }

    /// A file outside any repository falls through too — it does not
    /// resolve to nothing, because the editor's own repo is still a
    /// sensible answer for `C-x g`.
    #[test]
    fn a_file_outside_any_repo_falls_through_to_the_working_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let orphan = dir.path().join("loose.txt");
        std::fs::write(&orphan, "x\n").unwrap();
        // No `git init` here on purpose. (If the tempdir happened to sit
        // inside a repo, discovery would find THAT — which is still the
        // fall-through behaviour being asserted.)
        // The claim is the fall-through itself: an unresolvable file
        // gives the same answer as no file at all, rather than being
        // more fatal than having opened nothing.
        assert_eq!(
            repo_for_trigger(None, Some(&orphan), None),
            repo_for_trigger(None, None, None),
            "a file we cannot place must fall through, not refuse"
        );
    }

    // ── MR.1: the label ──────────────────────────────────────────

    #[test]
    fn the_label_is_the_repo_directorys_name() {
        assert_eq!(repo_label(Path::new("/Users/x/src/lattice")), "lattice");
        assert_eq!(repo_label(Path::new("/Users/x/work/api")), "api");
    }

    /// Two checkouts sharing a basename are what the qualified form
    /// exists for: the same label would otherwise name one buffer for
    /// two repositories, and the staging chords would act on whichever
    /// was recorded last.
    #[test]
    fn colliding_basenames_qualify_differently() {
        let a = Path::new("/Users/x/work/api");
        let b = Path::new("/Users/x/oss/api");
        assert_eq!(repo_label(a), repo_label(b), "the collision is real");
        assert_ne!(
            qualified_repo_label(a),
            qualified_repo_label(b),
            "…and qualifying must break it"
        );
        assert_eq!(qualified_repo_label(a), "work/api");
    }

    /// A root path has no final component. Falling back to the whole
    /// path beats an unnamed buffer.
    #[test]
    fn a_rootish_path_still_produces_a_label() {
        assert!(!repo_label(Path::new("/")).is_empty());
    }

    /// MR.3b: the working directory is absolute, whatever directory the
    /// editor was started in.
    ///
    /// `gix` reports the workdir relative to where discovery started,
    /// so running from `crates/lattice-ui-tui` yields `../..` — which
    /// has no final component and labelled every buffer
    /// `*magit:<view>:../..*`. This test runs with the cwd at its own
    /// package directory (cargo's doing), so it exercises exactly that
    /// case rather than simulating it.
    #[test]
    fn the_working_directory_is_absolute_even_from_a_subdirectory() {
        let Some(workdir) = magit_workdir() else {
            // Not in a repository at all; nothing to assert.
            return;
        };
        assert!(
            workdir.is_absolute(),
            "workdir must be absolute, got {workdir:?}"
        );
        let label = repo_label(&workdir);
        assert!(
            !label.contains(".."),
            "a relative workdir leaks into the buffer name: {label}"
        );
    }

    // ── MR.2: the naming pair ────────────────────────────────────

    /// Every label the two label functions can produce must survive the
    /// producer intact — including the qualified form, which contains a
    /// `/` and is the one a careless parser would split wrongly later.
    #[test]
    fn the_producer_puts_the_label_in_the_name() {
        assert_eq!(
            magit_buffer_name("status", &repo_label(Path::new("/Users/x/src/lattice"))),
            "*magit:status:lattice*"
        );
        assert_eq!(
            magit_buffer_name(
                "status",
                &qualified_repo_label(Path::new("/Users/x/work/api"))
            ),
            "*magit:status:work/api*"
        );
    }

    /// The repo-less form is the old name, unchanged — a trigger
    /// outside any repository must not open `*magit:status:*`, which is
    /// a name that means nothing and would not match the buffer the
    /// pre-MR.2 tests and the TUI bindings look for.
    #[test]
    fn no_label_produces_the_name_magit_always_had() {
        assert_eq!(magit_buffer_name("status", ""), "*magit:status*");
    }

    // ── MR.3: the name grammar ───────────────────────────────────

    /// Everything the producers can make must read back as what went in
    /// — the property the pair exists for, and the one MG.15 lost.
    #[test]
    fn every_name_the_producers_make_the_parser_reads_back() {
        let cases: &[(String, &str, Option<&str>, Option<&str>)] = &[
            (
                magit_buffer_name("status", "lattice"),
                "status",
                Some("lattice"),
                None,
            ),
            (
                magit_buffer_name("status", "work/api"),
                "status",
                Some("work/api"),
                None,
            ),
            (magit_buffer_name("status", ""), "status", None, None),
            (
                magit_buffer_name_with("diff", "lattice", "staged:src/main.rs"),
                "diff",
                Some("lattice"),
                Some("staged:src/main.rs"),
            ),
            (
                magit_buffer_name_with("file", "api", "a1b2c3d:src/main.rs"),
                "file",
                Some("api"),
                Some("a1b2c3d:src/main.rs"),
            ),
        ];
        for (name, view, repo, rest) in cases {
            let parsed = parse_magit_name(name).unwrap_or_else(|| panic!("{name} must parse"));
            assert_eq!(
                parsed,
                MagitName {
                    view,
                    repo: *repo,
                    rest: *rest
                },
                "{name}"
            );
        }
    }

    /// The rule the fixed position exists for, asserted as the pair of
    /// names it disambiguates: the repository `src` and the log of the
    /// path `src` are different buffers, and before MR.3 they were the
    /// same string.
    #[test]
    fn segment_two_is_the_repository_even_when_it_looks_like_a_parameter() {
        let repo_only = magit_buffer_name("log", "src");
        let path_in_repo = magit_buffer_name_with("log", "lattice", "src");

        assert_eq!(
            parse_magit_name(&repo_only).and_then(|n| n.repo),
            Some("src")
        );
        assert_eq!(parse_magit_name(&repo_only).and_then(|n| n.rest), None);

        assert_eq!(
            parse_magit_name(&path_in_repo).and_then(|n| n.repo),
            Some("lattice")
        );
        assert_eq!(
            parse_magit_name(&path_in_repo).and_then(|n| n.rest),
            Some("src")
        );
    }

    /// Outside a repository a parameterised view leaves the repo segment
    /// EMPTY rather than omitting it. Omitting would slide the parameter
    /// into segment 2 and the parser would report `staged` as the
    /// repository — which is the whole failure the fixed position
    /// prevents, arrived at from the producer's side.
    #[test]
    fn a_parameterised_name_with_no_repo_keeps_the_slot_open() {
        let name = magit_buffer_name_with("diff", "", "staged:src/main.rs");
        let parsed = parse_magit_name(&name).expect("parses");
        assert_eq!(parsed.repo, None, "there is no repository");
        assert_eq!(
            parsed.rest,
            Some("staged:src/main.rs"),
            "…and the parameter survives intact: {name}"
        );
    }

    #[test]
    fn names_that_are_not_magit_buffers_do_not_parse() {
        for name in ["src/main.rs", "*messages*", "*magit:*", "*magit:status"] {
            assert!(
                parse_magit_name(name).is_none(),
                "{name} must not parse as a magit view name"
            );
        }
    }

    #[test]
    fn magit_buffers_are_recognised_by_name() {
        assert!(is_magit_buffer_name("*magit:status*"));
        assert!(is_magit_buffer_name("*magit:status:lattice*"));
        assert!(is_magit_buffer_name("*magit:diff:staged:src/main.rs*"));
        assert!(!is_magit_buffer_name("src/main.rs"));
        assert!(!is_magit_buffer_name("*messages*"));
    }
}
