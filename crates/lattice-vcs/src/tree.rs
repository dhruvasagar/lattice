//! One directory of a repository as a revision, or the index, has it.
//!
//! The listing a browser needs — names and which of them are
//! directories — and nothing else: no object ids, no modes beyond
//! "is a tree". Read in-process through gix rather than by spawning
//! `git ls-tree`, because a listing is opened by a keystroke (`-`) and
//! re-read on every `<CR>` into a directory; a process spawn per step is
//! the cost worth not paying there. `benches/tree.rs` measures it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::{Repository, Result, VcsError};

/// One entry of a listed directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// The entry's name within its directory.
    pub name: String,
    /// Whether it is a directory (a tree). A submodule is not: it is a
    /// commit, and its contents are not in this repository.
    pub is_dir: bool,
}

/// Directory listings at a revision, and in the index.
pub struct Tree;

impl Tree {
    /// The entries of `dir` (relative to the repository root; empty for
    /// the root) at `rev`, which is anything `git rev-parse` accepts that
    /// names a commit or a tree — a sha, a branch, `HEAD~2`, `stash@{0}`.
    ///
    /// Directories first, then by name: the order the filesystem listing
    /// beside it uses, so the same directory reads the same either way.
    pub fn list(repo: &Repository, rev: &str, dir: &Path) -> Result<Vec<TreeEntry>> {
        let missing = |what: String| VcsError::ObjectNotFound(what);
        let at = || format!("{rev}:{}", dir.display());
        let inner = repo.inner();
        let root = inner
            .rev_parse_single(rev)
            .map_err(|e| missing(format!("{rev}: {e}")))?
            .object()
            .map_err(|e| missing(format!("{rev}: {e}")))?
            .peel_to_tree()
            .map_err(|e| missing(format!("{rev}: {e}")))?;
        let tree = if dir.as_os_str().is_empty() {
            root
        } else {
            let entry = root
                .lookup_entry_by_path(dir)
                .map_err(|e| missing(format!("{}: {e}", at())))?
                .ok_or_else(|| missing(at()))?;
            if !entry.mode().is_tree() {
                return Err(missing(format!("{} is not a directory", at())));
            }
            entry
                .object()
                .map_err(|e| missing(format!("{}: {e}", at())))?
                .peel_to_tree()
                .map_err(|e| missing(format!("{}: {e}", at())))?
        };
        let mut out = Vec::new();
        for entry in tree.iter() {
            let entry = entry.map_err(|e| missing(format!("{}: {e}", at())))?;
            out.push(TreeEntry {
                name: entry.filename().to_string(),
                is_dir: entry.mode().is_tree(),
            });
        }
        sort_dirs_first(&mut out);
        Ok(out)
    }

    /// The entries of `dir` as the index holds it — what a commit made
    /// now would contain. The index is a flat list of paths, so a
    /// directory is any first path component with more after it. A
    /// conflicted path is listed once, whichever stages it has.
    pub fn list_index(repo: &Repository, dir: &Path) -> Result<Vec<TreeEntry>> {
        let index = repo
            .inner()
            .index_or_empty()
            .map_err(|e| VcsError::Index(e.to_string()))?;
        // Index paths are `/`-separated whatever the platform.
        let prefix = if dir.as_os_str().is_empty() {
            String::new()
        } else {
            let mut p = dir.to_string_lossy().replace('\\', "/");
            p.push('/');
            p
        };
        let mut names: BTreeMap<String, bool> = BTreeMap::new();
        for entry in index.entries() {
            let path = entry.path(&index).to_string();
            let Some(rest) = path.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((first, _)) => {
                    names.insert(first.to_string(), true);
                }
                None => {
                    names.entry(rest.to_string()).or_insert(false);
                }
            }
        }
        if names.is_empty() && !prefix.is_empty() {
            return Err(VcsError::ObjectNotFound(format!(
                "{} is not a directory in the index",
                dir.display()
            )));
        }
        let mut out: Vec<TreeEntry> = names
            .into_iter()
            .map(|(name, is_dir)| TreeEntry { name, is_dir })
            .collect();
        sort_dirs_first(&mut out);
        Ok(out)
    }
}

fn sort_dirs_first(entries: &mut [TreeEntry]) {
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
}
