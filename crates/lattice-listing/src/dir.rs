//! One directory read, shared by both listing majors.
//!
//! DL.7: oil and the file tree each carried their own `read_dir` +
//! dirs-first-alpha sort (`read_dir_entries` / `read_dir_sorted`),
//! differing only in whether they yielded a `String` name or a
//! `PathBuf`. Same syscall, same ordering rule, same error handling,
//! written twice — and the ordering rule is user-visible, so a change
//! to one would have silently disagreed with the other.
//!
//! This is the *whole* of what those two modules genuinely shared.
//! Their entry models and their rope renderers deliberately did not
//! converge — see the module docs on each for why.

use std::path::{Path, PathBuf};

/// One filesystem entry, in the shape both listings need.
///
/// Carries the name and the full path because the two majors want
/// different halves: oil's rope is bare names (its `:w` diff reads
/// them as filenames), while the tree keys rows and icons off paths.
/// Computing both once here is cheaper than either caller
/// re-deriving the other from a `read_dir` result it has already
/// dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
}

/// Read `dir`, sorted **directories first, then alphabetically** —
/// the ordering both listings present and the one a user navigates by
/// muscle memory.
///
/// Ordering is by `name`, not by `path`. Within a single directory
/// the two agree, so this is not a behaviour change; naming the
/// weaker key would just invite a future caller to pass entries from
/// more than one directory and get a surprising order.
///
/// I/O errors propagate: a listing that silently dropped unreadable
/// entries would show a directory that does not match the disk, and
/// oil would then diff `:w` against that wrong picture.
pub fn read_dir_sorted(dir: &Path) -> std::io::Result<Vec<DirEntry>> {
    let mut out: Vec<DirEntry> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let is_dir = entry.file_type()?.is_dir();
        out.push(DirEntry {
            path: entry.path(),
            name: entry.file_name().to_string_lossy().into_owned(),
            is_dir,
        });
    }
    out.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn tempdir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let d = std::env::temp_dir().join(format!("lattice-dir-{tag}-{nanos}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn directories_sort_before_files_then_alphabetically() {
        let dir = tempdir("sort");
        std::fs::write(dir.join("b.txt"), "").unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::create_dir(dir.join("zeta")).unwrap();
        std::fs::create_dir(dir.join("alpha")).unwrap();

        let names: Vec<String> = read_dir_sorted(&dir)
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, vec!["alpha", "zeta", "a.txt", "b.txt"]);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn each_entry_carries_both_halves_the_majors_need() {
        let dir = tempdir("halves");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("f.rs"), "").unwrap();

        let entries = read_dir_sorted(&dir).unwrap();
        let sub = &entries[0];
        assert_eq!(sub.name, "sub", "oil's rope reads the bare name");
        assert_eq!(
            sub.path,
            dir.join("sub"),
            "the tree keys rows and icons off the path"
        );
        assert!(sub.is_dir);
        assert!(!entries[1].is_dir);

        std::fs::remove_dir_all(dir).ok();
    }

    /// An unreadable directory is an error, not an empty listing.
    /// Oil diffs `:w` against what this returns, so a silently empty
    /// read would look like "the user deleted everything".
    #[test]
    fn a_missing_directory_is_an_error_not_an_empty_listing() {
        let missing = std::env::temp_dir().join("lattice-dir-does-not-exist-please");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(read_dir_sorted(&missing).is_err());
    }
}
