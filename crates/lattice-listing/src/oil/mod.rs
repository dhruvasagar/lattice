//! Flat directory listing buffer — oil.nvim-style editable view.
//!
//! The buffer's text is one bare filename per line. Bare is
//! load-bearing: `apply()` diffs that text against the open-time
//! snapshot to derive the renames / deletes / creates to execute, so
//! anything decorative in it would be read as a filename. Icons are
//! leading virtual text published by `directory-listing-mode` (DL.3b)
//! for exactly this reason.
//!
//! DL.5: the buffer itself is an ordinary actor-backed Document — oil
//! is writable, so its edits take the normal document path with no
//! bespoke rope-mutation detour, and it paints through the shared
//! compose path like every other kind.
//!
//! ## Where does "the dir this oil buffer represents" live?
//!
//! Not on the snapshot. M.3.2.c.5 retired the struct-stored `dir`
//! field; the canonical answer is the [`OilDir`] `BufferLocal`
//! owned by `oil-mode`. The App reads it from
//! `buffer_locals[id].get::<OilDir>()` and passes it into the
//! `OilBuffer` methods that need it ([`OilBuffer::open`],
//! [`OilBuffer::reload`], [`OilBuffer::apply`]) as an explicit
//! `&Path` parameter.
//!
//! That design forces a single source of truth (the buffer-local)
//! and makes the per-buffer mode-owned state uniform across every
//! buffer kind: a reader does `buffer_locals[id].get::<T>()` and
//! never branches on `BufferKind`. `:describe-buffer` enumerates
//! every contributed local through the same path. Forgetting to
//! re-mirror state after a mutation -- the class of bug that
//! produced "navigate-up corrupts paths" -- is impossible by
//! construction because the buffer-local IS the state. There's
//! nothing to mirror.

pub mod modes;

pub use modes::{OilDir, OilMode, register_oil_modes};

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OilEntry {
    pub name: String,
    pub is_dir: bool,
}

/// The directory state an oil buffer diffs against: the entries as
/// they were at open, or at the last successful `:w`.
///
/// DL.5: this used to be `OilBuffer`, carrying the rope, a cursor and
/// a scroll alongside the snapshot. The rope moved into an
/// actor-backed Document (the buffer *is* one now), and the cursor /
/// scroll were archival duplicates of the pane's that the hot path
/// never read — their own doc comment said reading them was unsafe.
/// What is left is the only thing oil genuinely owns: what the
/// directory looked like, so `:w` can tell what the user changed.
///
/// The current text is passed in rather than held, because the
/// Document owns it. That is also what keeps the rope bare filenames:
/// icons are virtual text, so the diff sees exactly what the user
/// typed.
///
/// Does NOT hold the directory path — that lives in the [`OilDir`]
/// `BufferLocal`.
#[derive(Debug, Clone, Default)]
pub struct OilSnapshot {
    entries: Vec<OilEntry>,
}

impl OilSnapshot {
    /// Read `dir` into a fresh snapshot.
    pub fn open(dir: &Path) -> std::io::Result<Self> {
        Ok(Self {
            entries: read_dir_entries(dir)?,
        })
    }

    /// Re-read `dir`, replacing the snapshot.
    pub fn reload(&mut self, dir: &Path) -> std::io::Result<()> {
        self.entries = read_dir_entries(dir)?;
        Ok(())
    }

    /// True when `current_text` differs from what the snapshot renders
    /// to — i.e. the user edited the listing.
    pub fn is_dirty(&self, current_text: &str) -> bool {
        current_text != render_to_text(&self.entries)
    }

    /// Names from the snapshot (used by apply and tests).
    pub fn snapshot_names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Snapshot entries (used by renderer).
    pub fn snapshot_entries(&self) -> &[OilEntry] {
        &self.entries
    }

    /// Entry at `line` in the snapshot.
    pub fn entry_at_line(&self, line: u32) -> Option<&OilEntry> {
        self.entries.get(line as usize)
    }

    /// Diff rope against snapshot and execute filesystem operations
    /// relative to `dir`. Order: renames → deletes → creates.
    /// On error: stops, returns the error (caller echoes it).
    /// On success: refreshes snapshot to new disk state.
    ///
    /// `dir` is supplied by the caller from
    /// `buffer_locals[id].get::<OilDir>()`. Passing it explicitly
    /// instead of storing on the buffer eliminates the class of
    /// sync bug where post-navigate state could drift (the
    /// buffer-local IS the state).
    pub fn apply(&mut self, dir: &Path, current_text: &str) -> std::io::Result<()> {
        let current_names: Vec<String> = current_text
            .lines()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();

        let snap_names: std::collections::HashSet<&str> =
            self.entries.iter().map(|e| e.name.as_str()).collect();
        let curr_set: std::collections::HashSet<&str> =
            current_names.iter().map(|s| s.as_str()).collect();

        let deleted: Vec<&OilEntry> = self
            .entries
            .iter()
            .filter(|e| !curr_set.contains(e.name.as_str()))
            .collect();

        let created: Vec<&str> = current_names
            .iter()
            .map(|s| s.as_str())
            .filter(|name| !snap_names.contains(*name))
            .collect();

        // Rename heuristic: exactly 1 delete + 1 create → use fs::rename
        if deleted.len() == 1 && created.len() == 1 {
            let from = dir.join(&deleted[0].name);
            let to = dir.join(created[0]);
            std::fs::rename(&from, &to)?;
        } else {
            for entry in &deleted {
                let path = dir.join(&entry.name);
                if entry.is_dir {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
            for name in &created {
                let clean = name.trim_end_matches('/');
                let path = dir.join(clean);
                if name.ends_with('/') {
                    std::fs::create_dir_all(&path)?;
                } else {
                    std::fs::File::create(&path)?;
                }
            }
        }

        // Refresh snapshot from disk.
        // Refresh from disk so the next diff is against reality.
        self.entries = read_dir_entries(dir)?;
        Ok(())
    }
}

/// Read a directory's entries sorted dirs-first then alpha.
fn read_dir_entries(dir: &Path) -> std::io::Result<Vec<OilEntry>> {
    let mut entries: Vec<OilEntry> = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let is_dir = entry.file_type()?.is_dir();
        let name = entry.file_name().to_string_lossy().into_owned();
        entries.push(OilEntry { name, is_dir });
    }
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.cmp(&b.name),
    });
    Ok(entries)
}

/// Render snapshot entries to a rope — one bare name per line, no icons.
/// The listing text: one bare filename per line, dirs-first alpha.
///
/// Bare is load-bearing — `:w` diffs this against the snapshot to
/// derive renames, so anything decorative in here would be read as a
/// filename. Icons are virtual text for exactly this reason.
pub fn render_to_text(entries: &[OilEntry]) -> String {
    entries
        .iter()
        .map(|e| e.name.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Project the snapshot into the shape `directory-listing-mode` reads.
/// `icon_byte` is 0 — oil rows are bare names, so the icon leads.
pub fn listing_entries(
    dir: &std::path::Path,
    entries: &[OilEntry],
) -> Vec<crate::listing_mode::ListingEntry> {
    entries
        .iter()
        .map(|e| crate::listing_mode::ListingEntry {
            path: dir.join(&e.name),
            is_dir: e.is_dir,
            icon_byte: 0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::path::PathBuf;

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "lattice-oil-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn open_lists_dirs_first_then_files_alpha() {
        let dir = temp_dir();
        std::fs::write(dir.join("z.txt"), "").unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let oil = OilSnapshot::open(&dir).unwrap();
        assert_eq!(oil.snapshot_names(), vec!["sub", "a.txt", "z.txt"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rope_contains_bare_names_only() {
        let dir = temp_dir();
        std::fs::write(dir.join("main.rs"), "").unwrap();
        let oil = OilSnapshot::open(&dir).unwrap();
        let text = render_to_text(oil.snapshot_entries());
        assert_eq!(text.trim(), "main.rs");
        assert!(!text.contains("󱘗"), "icon should not be in rope");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn reload_replaces_listing() {
        let dir = temp_dir();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inner.rs"), "").unwrap();
        let mut oil = OilSnapshot::open(&dir).unwrap();
        oil.reload(&sub).unwrap();
        assert_eq!(oil.snapshot_names(), vec!["inner.rs"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn navigate_up_via_caller_computed_parent() {
        // The `navigate_up` method was retired in M.3.2.c.5
        // -- the caller computes the parent from the OilDir
        // buffer-local and calls reload. This test pins the
        // shape: starting in `sub`, reload at `dir` (its
        // parent) yields `dir`'s entries.
        let dir = temp_dir();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let mut oil = OilSnapshot::open(&sub).unwrap();
        // Caller does: parent = sub.parent(); oil.reload(parent).
        let parent = sub.parent().expect("sub has parent").to_path_buf();
        oil.reload(&parent).unwrap();
        assert!(oil.snapshot_names().contains(&"a.txt"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn is_dirty_false_when_unchanged() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let oil = OilSnapshot::open(&dir).unwrap();
        assert!(!oil.is_dirty(&render_to_text(oil.snapshot_entries())));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_renames_single_file() {
        let dir = temp_dir();
        std::fs::write(dir.join("old.txt"), "content").unwrap();
        let mut oil = OilSnapshot::open(&dir).unwrap();
        // The user's edited listing text.
        let text = "new.txt".to_string();
        oil.apply(&dir, &text).unwrap();
        assert!(
            dir.join("new.txt").exists(),
            "new.txt should exist after rename"
        );
        assert!(
            !dir.join("old.txt").exists(),
            "old.txt should not exist after rename"
        );
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_deletes_removed_lines() {
        let dir = temp_dir();
        std::fs::write(dir.join("keep.txt"), "").unwrap();
        std::fs::write(dir.join("gone.txt"), "").unwrap();
        let mut oil = OilSnapshot::open(&dir).unwrap();
        let text = render_to_text(oil.snapshot_entries());
        let new_text = text
            .lines()
            .filter(|l| *l != "gone.txt")
            .collect::<Vec<_>>()
            .join("\n");
        oil.apply(&dir, &new_text).unwrap();
        assert!(!dir.join("gone.txt").exists());
        assert!(dir.join("keep.txt").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_creates_new_file_lines() {
        let dir = temp_dir();
        std::fs::write(dir.join("existing.txt"), "").unwrap();
        let mut oil = OilSnapshot::open(&dir).unwrap();
        let mut text = render_to_text(oil.snapshot_entries());
        text.push_str("\nnewfile.txt");
        let text = text;
        oil.apply(&dir, &text).unwrap();
        assert!(dir.join("newfile.txt").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_handles_multiple_deletes_and_creates() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();
        let mut oil = OilSnapshot::open(&dir).unwrap();
        let text = "c.txt\nd.txt".to_string();
        oil.apply(&dir, &text).unwrap();
        assert!(!dir.join("a.txt").exists());
        assert!(!dir.join("b.txt").exists());
        assert!(dir.join("c.txt").exists());
        assert!(dir.join("d.txt").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_refreshes_snapshot_on_success() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let mut oil = OilSnapshot::open(&dir).unwrap();
        let text = "b.txt".to_string();
        oil.apply(&dir, &text).unwrap();
        assert_eq!(oil.snapshot_names(), vec!["b.txt"]);
        assert!(!oil.is_dirty(&render_to_text(oil.snapshot_entries())));
        std::fs::remove_dir_all(dir).ok();
    }
}
