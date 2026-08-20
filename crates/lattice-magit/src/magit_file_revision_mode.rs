//! `*magit:file:<ref>:<path>*` — a file's content at a fixed
//! reference point, read-only.
//!
//! `<CR>` on a file line inside any magit buffer tied to a specific
//! commit or index state (magit-revision, the staged region of
//! magit-commit/magit-diff) resolves to this buffer instead of the
//! live working-tree file — the diff/commit you were looking at
//! describes a SPECIFIC version of the file, and the working-tree
//! copy may already have diverged from it (mid-rebase, after a
//! later edit, or simply because the commit is historical). Visiting
//! the CURRENT file for editing stays `<CR>` in buffers that are
//! themselves about current state (magit-status, magit-diff's
//! Unstaged scope) — see `magit.md` §6.3 for the full uniformity
//! rule across views.
//!
//! `<ref>` is either a real commit-ish (sha/tag/branch) or the
//! literal token `staged`, meaning "the index's blob for this path"
//! (`git show :<path>`) rather than a commit.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, keymap_entry,
};

use crate::headerline;

pub struct MagitFileRevisionMode;

impl MagitFileRevisionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-file-revision-mode")
    }
}

fn magit_file_revision_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            // MG.23f: blob navigation. Walking one file's history is
            // what this buffer is for — without these you can open a
            // revision but not step through them, so every step means
            // going back to the log.
            //
            // magit binds these to `n` / `p`; evil-collection-magit
            // remaps them to `gk` / `gj` and lattice follows it, for
            // the reason the remap exists: `n` is search-repeat, and a
            // read-only view of a file is exactly where you search.
            keymap_entry! { mode: Normal, chord: "gj", doc: "This file at the next revision", cmd: "action:magit-blob-next" },
            keymap_entry! { mode: Normal, chord: "gk", doc: "This file at the previous revision", cmd: "action:magit-blob-previous" },
        ]
    })
}

/// MG.23f: which way [`blob_step`] walks the file's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobStep {
    /// Older — the next entry in `git rev-list` order.
    Previous,
    /// Newer.
    Next,
}

/// The revision one step from `current` in `revisions`, which is
/// newest-first (`git rev-list` order).
///
/// `None` at either end rather than wrapping: a file's history has two
/// ends and silently jumping from the first commit to HEAD would read
/// as a glitch rather than as an edge. `None` too when `current` is not
/// in the list at all — which is what a `staged` pseudo-ref is, since
/// the index is not a commit and has no place in the walk.
///
/// Pure, so the walk is testable without a repository.
pub(crate) fn blob_step(revisions: &[String], current: &str, step: BlobStep) -> Option<String> {
    let at = revisions.iter().position(|r| r == current)?;
    let target = match step {
        BlobStep::Previous => at.checked_add(1)?,
        BlobStep::Next => at.checked_sub(1)?,
    };
    revisions.get(target).cloned()
}

/// Every commit that touched `path`, newest first.
///
/// `--follow` is deliberately absent: it would make the walk cross
/// renames, and the buffer name carries one path — a step that silently
/// changed which file you were reading would be worse than stopping.
fn file_revisions(workdir: &Path, path: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .args(["log", "--format=%H", "--", &path.to_string_lossy()])
        .current_dir(workdir)
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// MG.23f: `p` / `n` — the same file one revision older / newer.
fn blob_step_handlers() -> Vec<lattice_mode::ActionHandlerContribution> {
    fn step(ctx: &lattice_mode::ActionContext<'_>, step: BlobStep) -> Option<Effect> {
        let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
        let store = ctx.services.get::<lattice_mode::BufferStoreHandle>()?;
        let (git_ref, path) = store
            .name_for(buffer_id)
            .and_then(|name| parse_buffer_name(&name))?;
        // MR.4: the revisions walked are this blob's repository's, not
        // the process's — `p` / `n` must stay inside the checkout the
        // buffer came from.
        let workdir = crate::repo_scope::action_workdir(ctx);
        let revisions = file_revisions(&workdir, &path);
        match blob_step(&revisions, &git_ref, step) {
            Some(next) => Some(Effect::OpenSyntheticBuffer {
                // MR.3b: stay in the repository this blob came from.
                name: blob_buffer_name(
                    &crate::repo_scope::label_of_buffer(&store, buffer_id),
                    &next,
                    &path,
                ),
                mode_id: MagitFileRevisionMode::mode_id().to_string(),
            }),
            // Saying which end you are at beats a key that appears
            // broken — and `staged` lands here too, since the index is
            // not a commit and has no place in the walk.
            None => Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Info,
                text: match step {
                    BlobStep::Previous => "magit: no earlier revision of this file".to_string(),
                    BlobStep::Next => "magit: already at the newest revision".to_string(),
                },
            }),
        }
    }

    vec![
        lattice_mode::ActionHandlerContribution {
            action_name: "action:magit-blob-previous",
            handler: std::sync::Arc::new(|ctx| step(ctx, BlobStep::Previous)),
        },
        lattice_mode::ActionHandlerContribution {
            action_name: "action:magit-blob-next",
            handler: std::sync::Arc::new(|ctx| step(ctx, BlobStep::Next)),
        },
    ]
}

impl Mode for MagitFileRevisionMode {
    /// MG.14: the headerline registration — this mode's only
    /// per-activation resource. Dropping it removes the sticky row.
    type Guard = Option<crate::headerline::HeaderlineRegistration>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn action_handlers(&self) -> Vec<lattice_mode::ActionHandlerContribution> {
        blob_step_handlers()
    }

    fn keymap(&self) -> Keymap {
        // No mode-specific chords — `q`/`gr`/nav come from magit-core
        // (this mode is in its `ActivationPolicy::Majors` list). `gr`
        // is a harmless no-op here (no refresh handler registered):
        // a fixed ref's blob content doesn't change.
        Keymap::from_entries(magit_file_revision_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<lattice_mode::BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            // MR.3: the repository the trigger resolved for THIS
            // buffer, not the one the editor was started in.
            let workdir =
                crate::repo_scope::view_workdir(&ctx, buffer_id, &handle).unwrap_or_default();

            let parsed = store
                .name_for(buffer_id)
                .and_then(|name| parse_buffer_name(&name));

            // MG.14: `<path> @ <ref>`. Without it this buffer is
            // indistinguishable from the live file — which is exactly
            // the mistake the mode exists to prevent. Both fields come
            // out of the buffer name, so the header is complete before
            // `git show` runs.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            if let Some((git_ref, path)) = parsed.as_ref() {
                headerline::publish(&hl, headerline::file_revision_fields(git_ref, path));
            }

            // MG.26c: the content and its highlighting come out of the
            // SAME blocking task. Parsing a whole file is exactly the
            // work paramount goal #1 keeps off the actor thread, and
            // splitting it into a second task would mean reading the
            // text back out of the buffer to parse it.
            let wd = workdir.clone();
            let registry = ctx
                .service::<std::sync::Arc<lattice_syntax::LangRegistry>>()
                .map(|outer| (*outer).clone());
            let (text, spans) = tokio::task::spawn_blocking(move || match parsed {
                Some((git_ref, path)) => {
                    let text = run_show_file(&wd, &git_ref, &path);
                    let spans = registry
                        .as_ref()
                        .and_then(|r| crate::highlight::file_syntax_spans(&path, &text, r));
                    (text, spans)
                }
                None => ("No file/ref given.\n".to_string(), None),
            })
            .await
            .unwrap_or_else(|_| (String::new(), None));
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                match spans {
                    // An unrecognised extension or a missing grammar
                    // leaves the buffer plain, which is what it was
                    // before — never a failure.
                    Some(spans) => ph.store_and_wake(buffer_id, spans),
                    None => ph.wake(),
                }
            }

            Ok(hl_registration)
        })
    }
}

/// `(ref, path)` → `"*magit:file:<ref>:<path>*"`.
///
/// **One producer, one parser.** Nine sites used to build this string
/// by hand while [`parse_buffer_name`] read it, and the pairing is
/// load-bearing in a way that fails silently: MG.26b's reverse blame
/// leaves a request keyed by this exact name for `on_activate` to
/// consume, so a single formatting difference means the request is
/// never found and the buffer forward-blames instead — the wrong
/// answer, with no error. MG.15 lost every stash chord to the same
/// producer/parser split.
///
/// `git_ref` is a commit-ish, the literal `staged`, or a `stash@{N}`.
pub(crate) const FILE_VIEW: &str = "file";

/// MR.3b: this view's `rest` — `<ref>:<path>`, behind the repository.
pub(crate) fn file_view_rest(git_ref: &str, path: &std::path::Path) -> String {
    format!("{git_ref}:{}", path.display())
}

/// The full name, for the producers that hold a repository label rather
/// than a trigger's services — `<CR>` on a file row, a hunk, a picker
/// result. They are all *inside* a magit buffer already, so the label
/// they pass is the one their own name carries
/// ([`crate::repo_scope::label_of_buffer`]), and the opened buffer
/// recovers the path from it at activation.
pub(crate) fn blob_buffer_name(repo: &str, git_ref: &str, path: &std::path::Path) -> String {
    crate::workdir::magit_buffer_name_with(FILE_VIEW, repo, &file_view_rest(git_ref, path))
}

/// `"*magit:file:<ref>:<path>*"` → `(ref, path)`. `ref` never
/// contains `:` (it's a sha/branch name or the literal `staged`
/// token), so the FIRST `:` in the stripped name is the ref/path
/// boundary — everything after it is the path, even if the path
/// itself contains further `:` characters (rare on POSIX, but not
/// disallowed).
pub(crate) fn parse_buffer_name(name: &str) -> Option<(String, PathBuf)> {
    let parsed = crate::workdir::parse_magit_name(name)?;
    (parsed.view == FILE_VIEW).then_some(())?;
    let (git_ref, path) = parsed.rest?.split_once(':')?;
    if git_ref.is_empty() || path.is_empty() {
        return None;
    }
    Some((git_ref.to_string(), PathBuf::from(path)))
}

/// The object git wants for "this path at this ref" — or, for the
/// `staged` pseudo-ref, `:<path>`, git's own syntax for "stage 0 of the
/// index" (the staged blob). One producer, because the preview path and
/// the open path must ask git for the same object.
fn blob_spec(git_ref: &str, path: &Path) -> String {
    if git_ref == "staged" {
        format!(":{}", path.display())
    } else {
        format!("{git_ref}:{}", path.display())
    }
}

/// `git show <ref>:<path>`.
fn run_show_file(workdir: &Path, git_ref: &str, path: &Path) -> String {
    let spec = blob_spec(git_ref, path);
    std::process::Command::new("git")
        .args(["show", &spec])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| format!("Could not show {spec}\n"))
}

/// MG.54: the biggest blob worth fetching **synchronously** for a
/// preview. Matches the host's own bounded preview read, so a blob and a
/// file of the same size cost the same peek.
const PREVIEW_MAX_BYTES: u64 = 256 * 1024;

/// MG.54: the same blob, fetched for a PREVIEW rather than to open.
///
/// Three things separate it from [`run_show_file`], and each is the
/// reason it is not simply that function with a cap bolted on:
///
/// - **It asks the size first.** `git cat-file -s` reads the object
///   header, not the object, so refusing a 40MB blob costs nothing —
///   whereas capping the output of `git show` would already have paid
///   for it. Over the limit it returns a note, which is a preview pane
///   saying why it is empty rather than an editor that stopped
///   responding.
/// - **It never returns raw bytes.** A blob at a revision can be a PNG;
///   its escape sequences would reach the terminal and corrupt the
///   alternate screen. NUL ⇒ binary placeholder, control characters
///   stripped otherwise (tab kept).
/// - **It is bounded in lines as well as bytes**, since a 200k-line
///   minified file is under the byte cap and still nothing anyone reads.
///
/// Returns `None` when git has no such object — a file that did not
/// exist at that revision is the ordinary case (it is why you are
/// looking), and an error pane would be noise. The caller leaves the
/// previous preview up.
pub(crate) fn preview_blob(workdir: &Path, git_ref: &str, path: &Path) -> Option<String> {
    const MAX_LINES: usize = 2000;
    let spec = blob_spec(git_ref, path);
    let size = std::process::Command::new("git")
        .args(["cat-file", "-s", &spec])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok())?;
    if size > PREVIEW_MAX_BYTES {
        return Some(format!(
            "{spec}\n\n{} KiB — too large to preview.\n\
             Accept the revision to open it, or raise nothing: the limit \
             exists so choosing a revision never blocks on a fetch.\n",
            size / 1024
        ));
    }
    let out = std::process::Command::new("git")
        .args(["show", &spec])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    if out.stdout.contains(&0) {
        return Some(format!("{spec}\n\n<binary file — no preview>\n"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .take(MAX_LINES)
            .map(|line| {
                line.chars()
                    .filter(|c| !c.is_control() || *c == '\t')
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

#[cfg(test)]
mod blob_name {
    use super::*;

    /// Producer and parser are one pair, and every caller uses the
    /// producer.
    ///
    /// The failure this prevents is silent: MG.26b's reverse blame
    /// leaves a request keyed by this exact string for `on_activate` to
    /// consume, so one formatting difference means the request is never
    /// found and the buffer forward-blames instead — the wrong answer
    /// with no error. Nine sites used to build it by hand.
    #[test]
    fn every_ref_form_round_trips_through_the_parser() {
        for (git_ref, path) in [
            ("a1b2c3d", "src/main.rs"),
            ("staged", "src/main.rs"),
            ("stash@{2}", "src/main.rs"),
            ("HEAD", "a.txt"),
            ("feature/branch-name", "deep/nested/path.rs"),
        ] {
            let name = blob_buffer_name("lattice", git_ref, std::path::Path::new(path));
            let (back_ref, back_path) =
                parse_buffer_name(&name).unwrap_or_else(|| panic!("`{name}` must parse back"));
            assert_eq!(back_ref, git_ref, "ref lost in {name}");
            assert_eq!(back_path, std::path::Path::new(path), "path lost in {name}");
        }
    }

    /// A path containing `:` is rare but legal on POSIX, and the parser
    /// splits on the FIRST colon precisely so it survives.
    #[test]
    fn a_path_containing_a_colon_survives_the_round_trip() {
        let name = blob_buffer_name("lattice", "HEAD", std::path::Path::new("weird:name.rs"));
        let (r, p) = parse_buffer_name(&name).expect("parses");
        assert_eq!(r, "HEAD");
        assert_eq!(p, std::path::Path::new("weird:name.rs"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_buffer_name_splits_ref_and_path() {
        let (r, p) = parse_buffer_name("*magit:file:lattice:a1b2c3d:src/main.rs*").unwrap();
        assert_eq!(r, "a1b2c3d");
        assert_eq!(p, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn parse_buffer_name_handles_staged_pseudo_ref() {
        let (r, p) = parse_buffer_name("*magit:file:lattice:staged:src/main.rs*").unwrap();
        assert_eq!(r, "staged");
        assert_eq!(p, PathBuf::from("src/main.rs"));
    }

    #[test]
    fn parse_buffer_name_rejects_malformed_names() {
        assert!(parse_buffer_name("*magit:file:lattice:*").is_none());
        assert!(parse_buffer_name("*magit:file:lattice:onlyref*").is_none());
        // MR.3b: a name with no `rest` is not a blob at all — there is
        // no ref and no path, only a repository.
        assert!(parse_buffer_name("*magit:file:lattice*").is_none());
        assert!(parse_buffer_name("not a magit buffer").is_none());
    }

    /// The list is newest-first, so "previous" walks *forward* through
    /// it — the one inversion in this module and the one worth pinning.
    #[test]
    fn blob_step_walks_newest_first_order_in_both_directions() {
        let revs = vec!["new".to_string(), "mid".to_string(), "old".to_string()];
        assert_eq!(
            blob_step(&revs, "mid", BlobStep::Previous).as_deref(),
            Some("old")
        );
        assert_eq!(
            blob_step(&revs, "mid", BlobStep::Next).as_deref(),
            Some("new")
        );
    }

    #[test]
    fn blob_step_stops_at_both_ends_rather_than_wrapping() {
        let revs = vec!["new".to_string(), "old".to_string()];
        assert!(blob_step(&revs, "old", BlobStep::Previous).is_none());
        assert!(blob_step(&revs, "new", BlobStep::Next).is_none());
    }

    /// `staged` is not a commit, so it is not in the walk. Landing on
    /// `None` is what turns `p` there into the echo rather than a jump
    /// to whatever happened to sit at index 0.
    #[test]
    fn blob_step_refuses_a_ref_that_is_not_in_the_history() {
        let revs = vec!["new".to_string(), "old".to_string()];
        assert!(blob_step(&revs, "staged", BlobStep::Previous).is_none());
        assert!(blob_step(&revs, "staged", BlobStep::Next).is_none());
    }
}

/// MG.23f: the walk against a real repository — `file_revisions` must
/// agree with git about both *which* commits touched the file and what
/// order they come in, or `p`/`n` step somewhere plausible and wrong.
#[cfg(test)]
mod blob_navigation_round_trip {
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

    fn rev(dir: &Path, spec: &str) -> String {
        let out = Command::new("git")
            .args(["rev-parse", spec])
            .current_dir(dir)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Three commits, but only two touch `a.txt` — so a walk that
    /// listed every commit instead of the file's own would step into a
    /// revision where `a.txt` never changed.
    fn three_commit_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        std::fs::write(p.join("a.txt"), "one\n").unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "a: one"]);
        std::fs::write(p.join("b.txt"), "unrelated\n").unwrap();
        git_ok(p, &["add", "b.txt"]);
        git_ok(p, &["commit", "-m", "b: unrelated"]);
        std::fs::write(p.join("a.txt"), "two\n").unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "a: two"]);
        dir
    }

    #[test]
    fn file_revisions_lists_only_the_commits_that_touched_the_file() {
        let dir = three_commit_repo();
        let revs = file_revisions(dir.path(), Path::new("a.txt"));
        assert_eq!(
            revs.len(),
            2,
            "the unrelated middle commit must not be in a.txt's history: {revs:?}"
        );
        assert_eq!(revs[0], rev(dir.path(), "HEAD"), "newest first");
    }

    #[test]
    fn stepping_back_from_head_lands_on_the_files_earlier_revision() {
        let dir = three_commit_repo();
        let p = dir.path();
        let revs = file_revisions(p, Path::new("a.txt"));
        let head = rev(p, "HEAD");

        let earlier = blob_step(&revs, &head, BlobStep::Previous).expect("an earlier revision");
        assert_eq!(
            run_show_file(p, &earlier, Path::new("a.txt")).trim(),
            "one",
            "`p` must land on the content as it was, not on HEAD's"
        );

        // ...and back again, so the pair is genuinely an inverse.
        assert_eq!(
            blob_step(&revs, &earlier, BlobStep::Next).as_deref(),
            Some(head.as_str())
        );
        // The earlier revision is the file's first, so there is no
        // further step back.
        assert!(blob_step(&revs, &earlier, BlobStep::Previous).is_none());
    }

    // ── MG.54: the preview fetch and its guards ──────────────────

    /// The ordinary case: the content as it was, not as it is.
    #[test]
    fn preview_shows_the_blob_at_that_revision() {
        let dir = three_commit_repo();
        let p = dir.path();
        let revs = file_revisions(p, Path::new("a.txt"));
        let earlier = blob_step(&revs, &rev(p, "HEAD"), BlobStep::Previous).expect("an earlier");

        assert_eq!(
            preview_blob(p, &earlier, Path::new("a.txt"))
                .expect("the blob exists at that revision")
                .trim(),
            "one"
        );
        assert_eq!(
            preview_blob(p, "HEAD", Path::new("a.txt"))
                .expect("HEAD has it too")
                .trim(),
            "two"
        );
    }

    /// A file that did not exist at that revision is the ORDINARY case —
    /// it is often why you are looking. `None` (leave the previous
    /// preview up) rather than an error pane full of git's wording.
    #[test]
    fn a_path_absent_at_that_revision_previews_nothing() {
        let dir = three_commit_repo();
        let p = dir.path();
        assert!(
            preview_blob(p, "HEAD", Path::new("never-existed.txt")).is_none(),
            "no object ⇒ no preview, not an error pane"
        );
    }

    /// The size guard reads the object HEADER (`cat-file -s`), so
    /// refusing costs nothing — the point is that the big blob is never
    /// fetched, on a path that runs synchronously on the actor thread.
    #[test]
    fn an_oversized_blob_is_refused_with_a_note_instead_of_fetched() {
        let dir = three_commit_repo();
        let p = dir.path();
        let big = "x".repeat(PREVIEW_MAX_BYTES as usize + 1024);
        std::fs::write(p.join("big.txt"), &big).unwrap();
        git_ok(p, &["add", "big.txt"]);
        git_ok(p, &["commit", "-m", "big"]);

        let out = preview_blob(p, "HEAD", Path::new("big.txt")).expect("a note, not nothing");
        assert!(
            out.contains("too large to preview"),
            "the pane must say why it is empty; got {out:?}"
        );
        assert!(
            !out.contains("xxxx"),
            "the blob itself must not have been fetched"
        );
    }

    /// A blob at a revision can be a PNG. Its bytes would reach the
    /// terminal and corrupt the alternate screen, so binary gets a
    /// placeholder — the same answer the host's file preview gives.
    #[test]
    fn a_binary_blob_previews_as_a_placeholder() {
        let dir = three_commit_repo();
        let p = dir.path();
        std::fs::write(p.join("bin.dat"), [0x89u8, 0x50, 0x00, 0x1b, 0x5b, 0x41]).unwrap();
        git_ok(p, &["add", "bin.dat"]);
        git_ok(p, &["commit", "-m", "bin"]);

        let out = preview_blob(p, "HEAD", Path::new("bin.dat")).expect("a placeholder");
        assert!(out.contains("binary"), "got {out:?}");
        assert!(
            !out.contains('\u{1b}'),
            "an escape byte must never reach the pane"
        );
    }

    /// Control characters in a *text* blob are stripped too — a "text"
    /// file carrying a stray escape is the case that leaves garbage on
    /// screen precisely because it passes the binary check.
    #[test]
    fn escape_bytes_in_a_text_blob_are_stripped() {
        let dir = three_commit_repo();
        let p = dir.path();
        std::fs::write(p.join("sneaky.txt"), "before\u{1b}[31mafter\n\tkept\n").unwrap();
        git_ok(p, &["add", "sneaky.txt"]);
        git_ok(p, &["commit", "-m", "sneaky"]);

        let out = preview_blob(p, "HEAD", Path::new("sneaky.txt")).expect("text");
        // Only the ESC byte is removed; `[31m` is printable and stays,
        // which is the same trade the host's own bounded file preview
        // makes — the terminal is protected, the text is not rewritten.
        assert!(!out.contains('\u{1b}'), "escape stripped; got {out:?}");
        assert!(out.contains("before[31mafter"), "the text itself survives");
        assert!(out.contains('\t'), "tabs are kept — they are layout");
    }
}
