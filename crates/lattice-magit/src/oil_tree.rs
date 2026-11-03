//! OR.4b: oil at a revision — fugitive's `:Gedit <rev>:<dir>/`.
//!
//! `-` in `*magit:file:<repo>:<rev>:<path>*` lists `<path>`'s directory
//! *as `<rev>` has it*, on `<path>`. Inside that listing `<CR>` on a
//! directory descends and `-` climbs, both still at `<rev>`, and `<CR>`
//! on a file opens that file at `<rev>` — the same buffer `<CR>` on a
//! file in a commit opens. Exploring a revision is a walk, not a lookup.
//!
//! Oil owns the chord, the listing and the walk (`lattice-listing`'s
//! `OilSource` seam); this module owns the two things only magit knows:
//! which buffers are at a revision ([`MagitOilOrigins`]), and what a
//! revision's tree holds ([`GitTreeSource`], over `lattice_vcs::Tree`).
//!
//! The listing is read-only: a revision has no working copy to rename
//! files in, and oil's `:w` writes to the disk.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_core::ui::pane::OpenTarget;
use lattice_grammar::Effect;
use lattice_listing::oil::{OilEntry, OilOrigin, OilSource, OilSourceProvider};

use crate::magit_file_revision_mode::{
    ConflictStage, MagitFileRevisionMode, blob_buffer_name, parse_buffer_name,
};
use crate::repo_scope::RepoScopesHandle;

/// `id` prefix of every [`GitTreeSource`].
const ID_PREFIX: &str = "magit-tree";
/// Separates the id's fields. A unit separator cannot appear in a
/// revision, a repository label or (in practice) a path.
const SEP: char = '\u{1f}';

/// A repository's tree at one revision, or its index (`staged`).
#[derive(Debug)]
pub(crate) struct GitTreeSource {
    id: String,
    /// The repository label magit names its buffers under, so a file
    /// opened from the listing belongs to the same repository.
    label: String,
    rev: String,
    workdir: PathBuf,
    scopes: RepoScopesHandle,
}

impl GitTreeSource {
    pub(crate) fn new(label: &str, rev: &str, workdir: &Path, scopes: RepoScopesHandle) -> Self {
        Self {
            id: format!(
                "{ID_PREFIX}{SEP}{label}{SEP}{rev}{SEP}{}",
                workdir.display()
            ),
            label: label.to_string(),
            rev: rev.to_string(),
            workdir: workdir.to_path_buf(),
            scopes,
        }
    }

    /// The source an id from [`Self::new`] names.
    fn parse(id: &str, scopes: RepoScopesHandle) -> Option<Self> {
        let mut parts = id.splitn(4, SEP);
        (parts.next()? == ID_PREFIX).then_some(())?;
        let label = parts.next()?;
        let rev = parts.next()?;
        let workdir = Path::new(parts.next()?);
        Some(Self::new(label, rev, workdir, scopes))
    }

    /// `path` relative to the repository root, or an error naming why
    /// it is not in this tree.
    fn relative<'a>(&self, path: &'a Path) -> std::io::Result<&'a Path> {
        path.strip_prefix(&self.workdir).map_err(|_| {
            std::io::Error::other(format!(
                "{} is outside the repository at {}",
                path.display(),
                self.workdir.display()
            ))
        })
    }

    /// The revision as the modeline shows it: a full sha shortened the
    /// way `git log --oneline` does, anything else as written.
    fn short_rev(&self) -> &str {
        let full_sha = self.rev.len() == 40 && self.rev.bytes().all(|b| b.is_ascii_hexdigit());
        if full_sha { &self.rev[..7] } else { &self.rev }
    }

    /// The blob buffer for the file at `rel` at this revision. Recorded
    /// against the repository before it exists, so it resolves even if
    /// every buffer that led here has been closed.
    fn open_file(&self, rel: &Path) -> Effect {
        let name = blob_buffer_name(&self.label, &self.rev, rel);
        self.scopes.record(name.clone(), self.workdir.clone());
        Effect::OpenSyntheticBuffer {
            name,
            mode_id: MagitFileRevisionMode::mode_id().to_string(),
            content: None,
            cursor: None,
            activate_minor: None,
        }
    }
}

impl OilSource for GitTreeSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn list(&self, dir: &Path) -> std::io::Result<Vec<OilEntry>> {
        let rel = self.relative(dir)?;
        let repo = lattice_vcs::Repository::discover(&self.workdir)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let entries = if self.rev == "staged" {
            lattice_vcs::Tree::list_index(&repo, rel)
        } else {
            lattice_vcs::Tree::list(&repo, &self.rev, rel)
        }
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(entries
            .into_iter()
            .map(|e| OilEntry {
                name: e.name,
                is_dir: e.is_dir,
            })
            .collect())
    }

    /// `a1b2c3d:/` for the root, `a1b2c3d:src/lib/` below it — the
    /// `<rev>:<path>` spelling git itself uses for an object at a
    /// revision.
    fn display(&self, dir: &Path) -> String {
        let rel = dir.strip_prefix(&self.workdir).unwrap_or(dir);
        if rel.as_os_str().is_empty() {
            format!("{}:/", self.short_rev())
        } else {
            format!("{}:{}/", self.short_rev(), rel.display())
        }
    }

    fn read_only(&self) -> Option<&str> {
        Some("a revision's tree is read-only")
    }

    /// The repository root is the top: above it is not in the revision.
    fn parent(&self, dir: &Path) -> Option<PathBuf> {
        if dir == self.workdir {
            return None;
        }
        dir.parent().map(Path::to_path_buf)
    }

    fn open_entry(&self, path: &Path, is_dir: bool, target: Option<OpenTarget>) -> Effect {
        let rel = match self.relative(path) {
            Ok(rel) => rel,
            Err(e) => {
                return Effect::Echo {
                    level: lattice_grammar::EchoLevel::Error,
                    text: format!("magit: {e}"),
                };
            }
        };
        let open = if is_dir {
            // Only reached with a target: in place, oil re-lists itself.
            Effect::OpenOilSource {
                source: self.id.clone(),
                dir: path.to_path_buf(),
                focus: None,
            }
        } else {
            self.open_file(rel)
        };
        match target {
            None => open,
            Some(target) => Effect::InTarget {
                target,
                effect: Box::new(open),
            },
        }
    }
}

/// Answers `-` for magit's file-at-a-revision buffers.
#[derive(Debug)]
pub(crate) struct MagitOilOrigins {
    scopes: RepoScopesHandle,
}

impl MagitOilOrigins {
    pub(crate) fn new(scopes: RepoScopesHandle) -> Self {
        Self { scopes }
    }
}

impl OilSourceProvider for MagitOilOrigins {
    fn origin_for(&self, buffer_name: &str) -> Option<OilOrigin> {
        let (rev, path) = parse_buffer_name(buffer_name)?;
        let label = crate::workdir::parse_magit_name(buffer_name)?
            .repo
            .unwrap_or_default();
        let workdir = self
            .scopes
            .workdir_for(buffer_name)
            .or_else(|| self.scopes.workdir_for_label(label))?;
        let dir = match path.parent() {
            Some(parent) => workdir.join(parent),
            None => workdir.clone(),
        };
        let focus = path.file_name().map(|n| n.to_string_lossy().into_owned());
        // A conflict's stage is a blob in the index with no tree behind
        // it: ours and theirs are whole commits, but the stage names only
        // this file. The working tree is the directory the conflict is
        // being resolved in, so `-` lists that.
        let source = if ConflictStage::from_token(&rev).is_some() {
            lattice_listing::oil::FsSource::ID.to_string()
        } else {
            GitTreeSource::new(label, &rev, &workdir, self.scopes.clone())
                .id
                .clone()
        };
        Some(OilOrigin { source, dir, focus })
    }

    fn source(&self, id: &str) -> Option<Arc<dyn OilSource>> {
        GitTreeSource::parse(id, self.scopes.clone()).map(|s| Arc::new(s) as Arc<dyn OilSource>)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// Two commits that list differently: `old.txt` is only in the first,
    /// `src/new.rs` only in the second.
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("old.txt"), "old\n").unwrap();
        std::fs::write(root.join("src/a.rs"), "a\n").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-qm", "one"]);
        std::fs::remove_file(root.join("old.txt")).unwrap();
        std::fs::write(root.join("src/new.rs"), "\n").unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-qm", "two"]);
        dir
    }

    fn names(entries: Vec<OilEntry>) -> Vec<String> {
        entries.into_iter().map(|e| e.name).collect()
    }

    fn scopes() -> RepoScopesHandle {
        Arc::new(crate::repo_scope::RepoScopes::default())
    }

    #[test]
    fn a_revision_lists_its_own_tree_not_the_working_trees() {
        let dir = repo();
        let root = dir.path().canonicalize().unwrap();
        let old = GitTreeSource::new("r", "HEAD~1", &root, scopes());
        assert_eq!(names(old.list(&root).unwrap()), vec!["src", "old.txt"]);
        assert_eq!(names(old.list(&root.join("src")).unwrap()), vec!["a.rs"]);
        let new = GitTreeSource::new("r", "HEAD", &root, scopes());
        assert_eq!(names(new.list(&root).unwrap()), vec!["src"]);
    }

    #[test]
    fn staged_lists_the_index() {
        let dir = repo();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("staged.txt"), "\n").unwrap();
        git(&root, &["add", "staged.txt"]);
        let staged = GitTreeSource::new("r", "staged", &root, scopes());
        assert_eq!(
            names(staged.list(&root).unwrap()),
            vec!["src", "staged.txt"]
        );
    }

    #[test]
    fn the_listing_is_read_only_and_tops_out_at_the_repository_root() {
        let root = PathBuf::from("/r");
        let s = GitTreeSource::new("r", "HEAD", &root, scopes());
        assert!(s.read_only().is_some());
        assert_eq!(s.parent(&root), None);
        assert_eq!(s.parent(&root.join("src/lib")), Some(root.join("src")));
        assert!(s.list(Path::new("/elsewhere")).is_err());
    }

    #[test]
    fn it_names_directories_the_way_git_names_objects() {
        let root = PathBuf::from("/r");
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            GitTreeSource::new("r", sha, &root, scopes()).display(&root),
            "0123456:/"
        );
        assert_eq!(
            GitTreeSource::new("r", "HEAD~2", &root, scopes()).display(&root.join("src/lib")),
            "HEAD~2:src/lib/"
        );
    }

    #[test]
    fn a_file_opens_at_the_revision_in_place_or_in_a_target() {
        let root = PathBuf::from("/r");
        let scopes = scopes();
        let s = GitTreeSource::new("r", "HEAD~1", &root, scopes.clone());
        let want = blob_buffer_name("r", "HEAD~1", Path::new("src/a.rs"));
        assert!(matches!(
            s.open_entry(&root.join("src/a.rs"), false, None),
            Effect::OpenSyntheticBuffer { ref name, .. } if *name == want
        ));
        assert_eq!(
            scopes.workdir_for(&want),
            Some(root.clone()),
            "the buffer finds its repository without the one it came from"
        );
        let Effect::InTarget { target, effect } =
            s.open_entry(&root.join("src"), true, Some(OpenTarget::Split))
        else {
            panic!("a target wraps the open");
        };
        assert_eq!(target, OpenTarget::Split);
        assert!(matches!(
            *effect,
            Effect::OpenOilSource { ref source, ref dir, .. } if source == s.id() && *dir == root.join("src")
        ));
    }

    #[test]
    fn dash_from_a_file_at_a_revision_lists_its_directory_there() {
        let scopes = scopes();
        let root = PathBuf::from("/r");
        let name = blob_buffer_name("r", "HEAD~1", Path::new("src/a.rs"));
        scopes.record(name.clone(), root.clone());
        let origins = MagitOilOrigins::new(scopes);

        let origin = origins.origin_for(&name).unwrap();
        assert_eq!(origin.dir, root.join("src"));
        assert_eq!(origin.focus.as_deref(), Some("a.rs"));
        let source = origins.source(&origin.source).unwrap();
        assert_eq!(source.display(&origin.dir), "HEAD~1:src/");

        // A conflict stage has no tree: the working directory, on disk.
        let ours = blob_buffer_name("r", ConflictStage::Ours.token(), Path::new("src/a.rs"));
        let origin = origins.origin_for(&ours).unwrap();
        assert_eq!(origin.source, lattice_listing::oil::FsSource::ID);
        assert_eq!(origin.dir, root.join("src"));

        // Not a file at a revision: not magit's to answer.
        assert!(origins.origin_for("*magit:status:r*").is_none());
        assert!(origins.origin_for("scratch").is_none());
    }
}
