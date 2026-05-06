//! Flat directory listing buffer — oil.nvim-style editable view.
//!
//! The rope contains one bare filename per line (no icons — icons are
//! added as renderer spans in `draw_oil_pane` so the rope stays pure
//! editable text). `apply()` diffs the rope against the open-time
//! snapshot and executes renames / deletes / creates on disk.

use std::path::{Path, PathBuf};

use lattice_core::Buffer;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

use crate::buffers::BufferId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OilEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug)]
pub struct OilBuffer {
    pub id: BufferId,
    pub dir: PathBuf,
    /// State at open / last successful `:w`.
    snapshot: Vec<OilEntry>,
    /// Editable rope — one bare filename per line, dirs-first alpha order.
    pub content: Buffer,
    pub cursor: Position,
    pub scroll: usize,
}

impl OilBuffer {
    pub fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        let snapshot = read_dir_entries(&dir)?;
        let content = render_to_buffer(&snapshot);
        Ok(Self {
            id: BufferId::next(),
            dir,
            snapshot,
            content,
            cursor: Position::ZERO,
            scroll: 0,
        })
    }

    /// Replace the listing with `subdir`'s contents in-place.
    pub fn navigate_into(&mut self, subdir: impl Into<PathBuf>) -> std::io::Result<()> {
        self.dir = subdir.into();
        self.snapshot = read_dir_entries(&self.dir)?;
        self.content = render_to_buffer(&self.snapshot);
        self.cursor = Position::ZERO;
        self.scroll = 0;
        Ok(())
    }

    /// Replace the listing with the parent directory's contents.
    pub fn navigate_up(&mut self) -> std::io::Result<()> {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            self.navigate_into(parent)?;
        }
        Ok(())
    }

    /// True when the current rope content differs from the snapshot render.
    pub fn is_dirty(&self) -> bool {
        self.content.as_string() != render_to_buffer(&self.snapshot).as_string()
    }

    /// Names from the snapshot (used by apply and tests).
    pub fn snapshot_names(&self) -> Vec<&str> {
        self.snapshot.iter().map(|e| e.name.as_str()).collect()
    }

    /// Snapshot entries (used by renderer).
    pub fn snapshot_entries(&self) -> &[OilEntry] {
        &self.snapshot
    }

    /// Entry at the cursor line (for `<CR>` dispatch).
    pub fn entry_at_cursor(&self) -> Option<&OilEntry> {
        self.snapshot.get(self.cursor.line as usize)
    }

    pub fn line_count(&self) -> u32 {
        self.content.line_count()
    }

    pub fn move_cursor(&mut self, dx: i32, dy: i32, viewport: usize) {
        let last_line = self.line_count().saturating_sub(1) as i32;
        let new_line = (self.cursor.line as i32 + dy).clamp(0, last_line) as u32;
        let line_len = line_byte_len(&self.content, new_line);
        let new_byte = (self.cursor.byte as i32 + dx).clamp(0, line_len as i32) as u32;
        self.cursor = Position::new(new_line, new_byte);
        self.adjust_scroll_to_cursor(viewport);
    }

    pub fn jump_cursor_to(&mut self, line: u32, viewport: usize) {
        let last_line = self.line_count().saturating_sub(1);
        let target = line.min(last_line);
        let line_len = line_byte_len(&self.content, target);
        self.cursor = Position::new(target, self.cursor.byte.min(line_len));
        self.adjust_scroll_to_cursor(viewport);
    }

    pub fn adjust_scroll_to_cursor(&mut self, viewport: usize) {
        if viewport == 0 { return; }
        let line = self.cursor.line as usize;
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + viewport {
            self.scroll = line + 1 - viewport;
        }
    }

    /// Diff rope against snapshot and execute filesystem operations.
    /// Order: renames → deletes → creates.
    /// On error: stops, returns the error (caller echoes it).
    /// On success: refreshes snapshot to new disk state.
    pub fn apply(&mut self) -> std::io::Result<()> {
        let current_names: Vec<String> = self
            .content
            .as_string()
            .lines()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();

        let snap_names: std::collections::HashSet<&str> =
            self.snapshot.iter().map(|e| e.name.as_str()).collect();
        let curr_set: std::collections::HashSet<&str> =
            current_names.iter().map(|s| s.as_str()).collect();

        let deleted: Vec<&OilEntry> = self
            .snapshot
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
            let from = self.dir.join(&deleted[0].name);
            let to   = self.dir.join(created[0]);
            std::fs::rename(&from, &to)?;
        } else {
            for entry in &deleted {
                let path = self.dir.join(&entry.name);
                if entry.is_dir {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
            for name in &created {
                let clean = name.trim_end_matches('/');
                let path = self.dir.join(clean);
                if name.ends_with('/') {
                    std::fs::create_dir_all(&path)?;
                } else {
                    std::fs::File::create(&path)?;
                }
            }
        }

        // Refresh snapshot from disk.
        self.snapshot = read_dir_entries(&self.dir)?;
        self.content  = render_to_buffer(&self.snapshot);
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
fn render_to_buffer(entries: &[OilEntry]) -> Buffer {
    let text = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            if i + 1 < entries.len() {
                format!("{}\n", e.name)
            } else {
                e.name.clone()
            }
        })
        .collect::<String>();
    let mut buf = Buffer::empty();
    if !text.is_empty() {
        let _ = buf.apply_edit(&Edit::insert(Position::ZERO, text));
    }
    buf
}

fn line_byte_len(buf: &Buffer, line: u32) -> u32 {
    buf.as_string()
        .split('\n')
        .nth(line as usize)
        .map(|l| l.len() as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn temp_dir() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir()
            .join(format!("lattice-oil-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn open_lists_dirs_first_then_files_alpha() {
        let dir = temp_dir();
        std::fs::write(dir.join("z.txt"), "").unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let oil = OilBuffer::open(&dir).unwrap();
        assert_eq!(oil.snapshot_names(), vec!["sub", "a.txt", "z.txt"]);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rope_contains_bare_names_only() {
        let dir = temp_dir();
        std::fs::write(dir.join("main.rs"), "").unwrap();
        let oil = OilBuffer::open(&dir).unwrap();
        let text = oil.content.as_string();
        assert_eq!(text.trim(), "main.rs");
        assert!(!text.contains("󱘗"), "icon should not be in rope");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn navigate_into_replaces_listing() {
        let dir = temp_dir();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("inner.rs"), "").unwrap();
        let mut oil = OilBuffer::open(&dir).unwrap();
        oil.navigate_into(&sub).unwrap();
        assert_eq!(oil.snapshot_names(), vec!["inner.rs"]);
        assert_eq!(oil.cursor, Position::ZERO);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn navigate_up_goes_to_parent() {
        let dir = temp_dir();
        let sub = dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let mut oil = OilBuffer::open(&sub).unwrap();
        oil.navigate_up().unwrap();
        assert_eq!(oil.dir, dir);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn is_dirty_false_when_unchanged() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        let oil = OilBuffer::open(&dir).unwrap();
        assert!(!oil.is_dirty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_renames_single_file() {
        let dir = temp_dir();
        std::fs::write(dir.join("old.txt"), "content").unwrap();
        let mut oil = OilBuffer::open(&dir).unwrap();
        // Edit rope: replace "old.txt" with "new.txt"
        oil.content = Buffer::empty();
        oil.content.apply_edit(&Edit::insert(Position::ZERO, "new.txt".to_string())).unwrap();
        oil.apply().unwrap();
        assert!(dir.join("new.txt").exists(), "new.txt should exist after rename");
        assert!(!dir.join("old.txt").exists(), "old.txt should not exist after rename");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_deletes_removed_lines() {
        let dir = temp_dir();
        std::fs::write(dir.join("keep.txt"), "").unwrap();
        std::fs::write(dir.join("gone.txt"), "").unwrap();
        let mut oil = OilBuffer::open(&dir).unwrap();
        let text = oil.content.as_string();
        let new_text = text.lines()
            .filter(|l| *l != "gone.txt")
            .collect::<Vec<_>>()
            .join("\n");
        oil.content = Buffer::empty();
        if !new_text.is_empty() {
            oil.content.apply_edit(&Edit::insert(Position::ZERO, new_text)).unwrap();
        }
        oil.apply().unwrap();
        assert!(!dir.join("gone.txt").exists());
        assert!(dir.join("keep.txt").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_creates_new_file_lines() {
        let dir = temp_dir();
        std::fs::write(dir.join("existing.txt"), "").unwrap();
        let mut oil = OilBuffer::open(&dir).unwrap();
        let mut text = oil.content.as_string();
        text.push_str("\nnewfile.txt");
        oil.content = Buffer::empty();
        oil.content.apply_edit(&Edit::insert(Position::ZERO, text)).unwrap();
        oil.apply().unwrap();
        assert!(dir.join("newfile.txt").exists());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn apply_handles_multiple_deletes_and_creates() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();
        let mut oil = OilBuffer::open(&dir).unwrap();
        oil.content = Buffer::empty();
        oil.content.apply_edit(&Edit::insert(Position::ZERO, "c.txt\nd.txt".to_string())).unwrap();
        oil.apply().unwrap();
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
        let mut oil = OilBuffer::open(&dir).unwrap();
        oil.content = Buffer::empty();
        oil.content.apply_edit(&Edit::insert(Position::ZERO, "b.txt".to_string())).unwrap();
        oil.apply().unwrap();
        assert_eq!(oil.snapshot_names(), vec!["b.txt"]);
        assert!(!oil.is_dirty());
        std::fs::remove_dir_all(dir).ok();
    }
}
