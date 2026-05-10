//! File-tree buffer (DESIGN.md §5.9 buffer-as-content).
//!
//! v1 status (B.1.d): a flattened, lazily-expanded tree rendered
//! one entry per line. `<CR>` on a directory toggles its expansion;
//! on a file opens that file as a new Document buffer through the
//! standard `:e FILE` path. Standard motions (j/k/0/$/G/gg/<C-d>/...)
//! navigate via the active-buffer routing in [`crate::pane`] -- the
//! file tree carries no special motion bindings of its own.
//!
//! The buffer is read-only (mutations error with the same
//! "read-only" echo as the help buffer). To make the rendered text
//! consume the standard rope path, we serialise the visible tree to
//! a [`lattice_core::Buffer`] each time the structure changes; the
//! same buffer feeds syntax highlighting and any future motions
//! that need the line text.

use std::path::{Path, PathBuf};

use lattice_core::Buffer;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;

use lattice_core::BufferId;

/// One row in the rendered file tree. `depth` controls the
/// indentation prefix; `kind` decides whether `<CR>` toggles
/// expansion or opens a file.
#[derive(Debug, Clone)]
pub struct FileTreeEntry {
    pub path: PathBuf,
    pub depth: u32,
    pub kind: FileTreeEntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileTreeEntryKind {
    /// A directory. `expanded = true` means the renderer recurses
    /// into its children.
    Directory { expanded: bool },
    /// A regular file.
    File,
}

/// One open file-tree buffer. Composes with the rest of the
/// editor by exposing the same `Buffer` + cursor + scroll surface
/// as [`crate::help::HelpBuffer`]; motions resolve through
/// `lattice_grammar::execute_motion_only` against [`Self::content`].
#[derive(Debug)]
pub struct FileTreeBuffer {
    pub id: BufferId,
    pub root: PathBuf,
    pub entries: Vec<FileTreeEntry>,
    pub content: Buffer,
    pub cursor: Position,
    pub scroll: usize,
    pub nerd_fonts: bool,
}

impl FileTreeBuffer {
    /// Build a fresh file-tree rooted at `root`. Reads the root
    /// directory eagerly (one level deep); subdirectories are
    /// lazy-loaded on expansion.
    ///
    /// `nerd_fonts` controls whether nerd-font icon glyphs are
    /// embedded inline in the rendered rope lines.
    pub fn open(root: impl Into<PathBuf>, nerd_fonts: bool) -> std::io::Result<Self> {
        let root = root.into();
        let mut entries = Vec::new();
        // The root itself appears as the first row so the user has
        // a context line + can collapse the whole view by toggling
        // it.
        entries.push(FileTreeEntry {
            path: root.clone(),
            depth: 0,
            kind: FileTreeEntryKind::Directory { expanded: true },
        });
        let children = read_dir_sorted(&root)?;
        for (path, is_dir) in children {
            entries.push(FileTreeEntry {
                path,
                depth: 1,
                kind: if is_dir {
                    FileTreeEntryKind::Directory { expanded: false }
                } else {
                    FileTreeEntryKind::File
                },
            });
        }
        let content = render_to_buffer(&entries, nerd_fonts);
        Ok(Self {
            id: BufferId::next(),
            root,
            entries,
            content,
            cursor: Position::ZERO,
            scroll: 0,
            nerd_fonts,
        })
    }

    /// Toggle expansion of the directory entry at `index` (1-based
    /// within `entries`). Children of newly-expanded directories
    /// are inserted right after the directory's row at depth + 1;
    /// closing a directory removes every entry whose depth is
    /// strictly greater than the closing one until depth drops
    /// back. No-op for non-directory rows.
    pub fn toggle_at(&mut self, index: usize) -> std::io::Result<()> {
        let Some(entry) = self.entries.get(index) else {
            return Ok(());
        };
        if let FileTreeEntryKind::Directory { expanded } = entry.kind {
            if expanded {
                self.collapse(index);
            } else {
                self.expand(index)?;
            }
            self.content = render_to_buffer(&self.entries, self.nerd_fonts);
        }
        Ok(())
    }

    fn expand(&mut self, index: usize) -> std::io::Result<()> {
        let parent_path = self.entries[index].path.clone();
        let parent_depth = self.entries[index].depth;
        let children = read_dir_sorted(&parent_path)?;
        // Splice children in one position after the parent.
        let insert_at = index + 1;
        let mut new_entries: Vec<FileTreeEntry> = children
            .into_iter()
            .map(|(path, is_dir)| FileTreeEntry {
                path,
                depth: parent_depth + 1,
                kind: if is_dir {
                    FileTreeEntryKind::Directory { expanded: false }
                } else {
                    FileTreeEntryKind::File
                },
            })
            .collect();
        // Mark parent as expanded.
        if let FileTreeEntryKind::Directory { ref mut expanded } = self.entries[index].kind {
            *expanded = true;
        }
        self.entries
            .splice(insert_at..insert_at, new_entries.drain(..));
        Ok(())
    }

    fn collapse(&mut self, index: usize) {
        let parent_depth = self.entries[index].depth;
        // Mark parent as collapsed.
        if let FileTreeEntryKind::Directory { ref mut expanded } = self.entries[index].kind {
            *expanded = false;
        }
        // Drop every entry after `index` whose depth is greater
        // than parent_depth, until we hit one that isn't.
        let mut end = index + 1;
        while end < self.entries.len() && self.entries[end].depth > parent_depth {
            end += 1;
        }
        self.entries.drain(index + 1..end);
    }

    /// Path under the cursor -- caller decides whether to follow it
    /// (`<CR>` on a file) or toggle (`<CR>` on a directory).
    pub fn entry_at_cursor(&self) -> Option<&FileTreeEntry> {
        self.entries.get(self.cursor.line as usize)
    }

    pub fn line_count(&self) -> u32 {
        self.content.line_count()
    }

    /// Same shape as `HelpBuffer::move_cursor` so the active-buffer
    /// routing in App can drive both kinds through the same motion
    /// path.
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
        if viewport == 0 {
            return;
        }
        let line = self.cursor.line as usize;
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + viewport {
            self.scroll = line + 1 - viewport;
        }
    }
}

/// Read a directory's children, partitioned (directories first)
/// and sorted by name. Hidden files (those starting with `.`) are
/// included so the user can navigate config dirs; future polish
/// can add a toggle.
fn read_dir_sorted(root: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
    let mut out: Vec<(PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let is_dir = entry.file_type()?.is_dir();
        out.push((path, is_dir));
    }
    out.sort_by(|a, b| {
        // Directories before files; then alpha.
        match (a.1, b.1) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.0.cmp(&b.0),
        }
    });
    Ok(out)
}

/// Serialise the entry list to a [`Buffer`]. Each row is
/// `<indent><marker><icon><name>` where `<marker>` is `▾ ` for an
/// expanded dir, `▸ ` for a collapsed dir, and `  ` for a file.
/// Indent is two spaces per `depth`. When `nerd_fonts` is true the
/// icon glyph from `lattice_core::ui::icons::glyph_for_entry` is
/// inserted between the marker and the name; when false the icon
/// is `""`. Colour is applied by the renderer at draw time, not
/// embedded in the rope. Re-rendered every time the tree structure
/// changes; cheap on real-sized trees (~hundreds of rows).
fn render_to_buffer(entries: &[FileTreeEntry], nerd_fonts: bool) -> Buffer {
    use lattice_core::ui::icons::glyph_for_entry;
    let mut text = String::new();
    for (i, entry) in entries.iter().enumerate() {
        let indent = "  ".repeat(entry.depth as usize);
        let marker = match entry.kind {
            FileTreeEntryKind::Directory { expanded: true } => "▾ ",
            FileTreeEntryKind::Directory { expanded: false } => "▸ ",
            FileTreeEntryKind::File => "  ",
        };
        let is_dir = matches!(entry.kind, FileTreeEntryKind::Directory { .. });
        let icon = glyph_for_entry(&entry.path, is_dir, nerd_fonts);
        let name = if entry.depth == 0 {
            entry.path.display().to_string()
        } else {
            entry
                .path
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        text.push_str(&indent);
        text.push_str(marker);
        text.push_str(icon);
        text.push_str(&name);
        if i + 1 < entries.len() {
            text.push('\n');
        }
    }
    let mut buffer = Buffer::empty();
    if !text.is_empty() {
        let _ = buffer.apply_edit(&Edit::insert(Position::ZERO, text));
    }
    buffer
}

fn line_byte_len(buf: &Buffer, line: u32) -> u32 {
    let s = buf.as_string();
    s.split('\n')
        .nth(line as usize)
        .map(|l| l.len() as u32)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn temp_dir() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lattice-tree-{}-{}", std::process::id(), uniq()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn uniq() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    #[test]
    fn open_lists_root_then_children() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let tree = FileTreeBuffer::open(&dir, false).unwrap();
        // Root + 2 children = 3 entries.
        assert_eq!(tree.entries.len(), 3);
        // Directory sorts before file.
        assert!(matches!(
            tree.entries[1].kind,
            FileTreeEntryKind::Directory { .. }
        ));
        assert!(matches!(tree.entries[2].kind, FileTreeEntryKind::File));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn toggle_directory_expands_and_collapses() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("inner.txt"), "x").unwrap();
        let mut tree = FileTreeBuffer::open(&dir, false).unwrap();
        // Find the index of the "sub" directory entry. It's at
        // index 1 (root, then sub).
        tree.toggle_at(1).unwrap();
        // After expand: root, sub, sub/inner.txt = 3 rows.
        assert_eq!(tree.entries.len(), 3);
        // Collapse again.
        tree.toggle_at(1).unwrap();
        assert_eq!(tree.entries.len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn render_uses_indentation_and_marker() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let tree = FileTreeBuffer::open(&dir, false).unwrap();
        let body = tree.content.as_string();
        assert!(body.contains("a.txt"));
        // File row is the second; depth 1 = 2 spaces of indent + 2
        // spaces for the file marker (nerd_fonts=false means icon="").
        assert!(body.lines().nth(1).unwrap().starts_with("    "));
    }

    #[test]
    fn move_cursor_clamps_to_last_line() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let mut tree = FileTreeBuffer::open(&dir, false).unwrap();
        tree.move_cursor(0, 1000, 10);
        // 2 entries -> 2 rendered lines, cursor pinned at last.
        assert_eq!(tree.cursor.line, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn entry_at_cursor_returns_currently_targeted() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let mut tree = FileTreeBuffer::open(&dir, false).unwrap();
        tree.cursor = Position::new(1, 0);
        let e = tree.entry_at_cursor().unwrap();
        assert!(matches!(e.kind, FileTreeEntryKind::File));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn render_embeds_nerd_icon_in_rope() {
        let dir = temp_dir();
        std::fs::write(dir.join("main.rs"), "x").unwrap();
        let tree = FileTreeBuffer::open(&dir, true).unwrap();
        let body = tree.content.as_string();
        assert!(body.contains("󱘗 "), "expected rust glyph in rope, got: {body}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn render_no_icon_when_nerd_fonts_disabled() {
        let dir = temp_dir();
        std::fs::write(dir.join("main.rs"), "x").unwrap();
        let tree = FileTreeBuffer::open(&dir, false).unwrap();
        let body = tree.content.as_string();
        assert!(!body.contains("󱘗 "), "unexpected glyph when nerd_fonts=false");
        std::fs::remove_dir_all(dir).ok();
    }
}
