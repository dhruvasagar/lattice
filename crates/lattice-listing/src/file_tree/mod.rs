//! File-tree buffer (DESIGN.md §5.9 buffer-as-content).
//!
//! v1 status (B.1.d): a flattened, lazily-expanded tree rendered
//! one entry per line. `<CR>` on a directory toggles its expansion;
//! on a file opens that file as a new Document buffer through the
//! standard `:e FILE` path. Standard motions (`j`/`k`/`0`/`$`/`G`/`gg`/`<C-d>`/...)
//! navigate via the active-buffer routing in `lattice_core::ui::pane` -- the
//! file tree carries no special motion bindings of its own.
//!
//! The buffer is read-only (mutations error with the same
//! "read-only" echo as the help buffer). The rendered text is a
//! [`lattice_core::Buffer`] derived from the entry list; the same
//! buffer feeds syntax highlighting and motions that need the line
//! text.
//!
//! ## Where does "the root", "the entry list", "nerd-fonts" live?
//!
//! Not on `FileTreeBuffer`. M.3.2.c.5 retired the struct fields;
//! the canonical answer for each is a `BufferLocal` owned by
//! `file-tree-mode`:
//!
//! - [`modes::FileTreeRoot`] -- the rooted directory.
//! - [`modes::FileTreeEntries`] -- the flat list of visible
//!   entries.
//! - [`modes::FileTreeNerdFonts`] -- the nerd-font toggle.
//!
//! The `FileTreeBuffer` carries only the rendered rope + cursor +
//! scroll. Operations that need the entry list / root / nerd-fonts
//! flag take them as explicit parameters or read them from
//! buffer-locals through the App's chokepoint helpers. Mutations
//! to the entry list flow through [`toggle_entries_at`] (a pure
//! function that takes `&mut Vec<FileTreeEntry>`), so the App can
//! orchestrate "update locals + re-render rope" as one atomic step
//! and no struct mirror exists to drift.

pub mod modes;

pub use modes::{
    FileTreeEntries, FileTreeMode, FileTreeNerdFonts, FileTreeRoot, register_file_tree_modes,
};

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

/// One open file-tree buffer. Carries only the rendered rope and
/// motion state (cursor + scroll). All "where does this buffer
/// point" data -- root, entry list, nerd-fonts toggle -- lives in
/// the App's `BufferLocals` map under the
/// [`modes::FileTreeRoot`] / [`modes::FileTreeEntries`] /
/// [`modes::FileTreeNerdFonts`] keys.
#[derive(Debug)]
pub struct FileTreeBuffer {
    pub id: BufferId,
    pub content: Buffer,
    pub cursor: Position,
    pub scroll: usize,
}

impl FileTreeBuffer {
    /// Build a fresh file-tree rooted at `root`. Returns the
    /// buffer plus the initial entry list -- the caller is
    /// responsible for seeding the [`modes::FileTreeRoot`] /
    /// [`modes::FileTreeEntries`] / [`modes::FileTreeNerdFonts`]
    /// buffer-locals from these values.
    pub fn open(root: &Path, nerd_fonts: bool) -> std::io::Result<(Self, Vec<FileTreeEntry>)> {
        let entries = initial_entries(root)?;
        let content = render_to_buffer(&entries, nerd_fonts);
        let buf = Self {
            id: BufferId::next(),
            content,
            cursor: Position::ZERO,
            scroll: 0,
        };
        Ok((buf, entries))
    }

    /// Entries in the listing. CV.3: content space — this bounds
    /// cursor motion, and the empty line ropey reports after the
    /// listing's terminating newline is not an entry.
    pub fn line_count(&self) -> u32 {
        self.content.content_line_count()
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

/// Look up the entry at `line` in an entry list. The App reads
/// `FileTreeEntries` from buffer-locals and calls this with
/// `app.cursor.line` for `<CR>` dispatch.
pub fn entry_at_line(entries: &[FileTreeEntry], line: u32) -> Option<&FileTreeEntry> {
    entries.get(line as usize)
}

/// Build the initial entry list for `root`: the root itself as a
/// depth-0 expanded directory, plus its immediate children at
/// depth 1.
pub fn initial_entries(root: &Path) -> std::io::Result<Vec<FileTreeEntry>> {
    let mut entries = Vec::new();
    entries.push(FileTreeEntry {
        path: root.to_path_buf(),
        depth: 0,
        kind: FileTreeEntryKind::Directory { expanded: true },
    });
    let children = read_dir_sorted(root)?;
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
    Ok(entries)
}

/// Toggle expansion of the directory entry at `index` (1-based
/// within `entries`). Children of newly-expanded directories
/// are inserted right after the directory's row at depth + 1;
/// closing a directory removes every entry whose depth is
/// strictly greater than the closing one until depth drops
/// back. No-op for non-directory rows.
///
/// Pure function over the entry list -- no struct state, no
/// rope re-render. The App-side caller composes this with
/// [`render_to_buffer`] and a `FileTreeEntries` buffer-local
/// write to atomically refresh both halves of the file-tree
/// representation.
pub fn toggle_entries_at(entries: &mut Vec<FileTreeEntry>, index: usize) -> std::io::Result<()> {
    let Some(entry) = entries.get(index) else {
        return Ok(());
    };
    if let FileTreeEntryKind::Directory { expanded } = entry.kind {
        if expanded {
            collapse(entries, index);
        } else {
            expand(entries, index)?;
        }
    }
    Ok(())
}

fn expand(entries: &mut Vec<FileTreeEntry>, index: usize) -> std::io::Result<()> {
    let parent_path = entries[index].path.clone();
    let parent_depth = entries[index].depth;
    let children = read_dir_sorted(&parent_path)?;
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
    if let FileTreeEntryKind::Directory { ref mut expanded } = entries[index].kind {
        *expanded = true;
    }
    entries.splice(insert_at..insert_at, new_entries.drain(..));
    Ok(())
}

fn collapse(entries: &mut Vec<FileTreeEntry>, index: usize) {
    let parent_depth = entries[index].depth;
    if let FileTreeEntryKind::Directory { ref mut expanded } = entries[index].kind {
        *expanded = false;
    }
    let mut end = index + 1;
    while end < entries.len() && entries[end].depth > parent_depth {
        end += 1;
    }
    entries.drain(index + 1..end);
}

/// Read a directory's children, partitioned (directories first)
/// and sorted by name. Hidden files (those starting with `.`) are
/// included so the user can navigate config dirs.
fn read_dir_sorted(root: &Path) -> std::io::Result<Vec<(PathBuf, bool)>> {
    let mut out: Vec<(PathBuf, bool)> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        let is_dir = entry.file_type()?.is_dir();
        out.push((path, is_dir));
    }
    out.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.cmp(&b.0),
    });
    Ok(out)
}

/// Serialise the entry list to a [`Buffer`]. Each row is
/// `<indent><marker><icon><name>` where `<marker>` is `▾ ` for an
/// expanded dir, `▸ ` for a collapsed dir, and `  ` for a file.
/// Indent is two spaces per `depth`. The `<icon>` glyph comes from
/// `lattice_core::ui::icons::glyph_for_entry` -- nerd-fonts when
/// `nerd_fonts` is true, the BMP-block fallback palette otherwise.
/// Both palettes occupy two cells, so column geometry is the same
/// either way.
pub fn render_to_buffer(entries: &[FileTreeEntry], _nerd_fonts: bool) -> Buffer {
    let mut buffer = Buffer::empty();
    let text = render_to_text(entries);
    if !text.is_empty() {
        let _ = buffer.apply_edit(&Edit::insert(Position::ZERO, text));
    }
    buffer
}

/// The tree's rope text: indent, expand marker, name — and **no
/// icon**.
///
/// DL.4: the glyph used to be baked in here. It is virtual text now
/// (`directory-listing-mode` publishes it as a leading inlay), which
/// makes the rope the entry NAMES: searchable, yankable, and not a
/// rendering artefact. It also makes the tree symmetric with oil,
/// whose rope never contained glyphs because `:w` diffs it.
pub fn render_to_text(entries: &[FileTreeEntry]) -> String {
    let mut text = String::new();
    for (i, entry) in entries.iter().enumerate() {
        text.push_str(&row_prefix(entry));
        text.push_str(&row_name(entry));
        if i + 1 < entries.len() {
            text.push('\n');
        }
    }
    text
}

/// Indent + expand marker — everything before the icon anchor.
fn row_prefix(entry: &FileTreeEntry) -> String {
    let indent = "  ".repeat(entry.depth as usize);
    let marker = match entry.kind {
        FileTreeEntryKind::Directory { expanded: true } => "▾ ",
        FileTreeEntryKind::Directory { expanded: false } => "▸ ",
        FileTreeEntryKind::File => "  ",
    };
    format!("{indent}{marker}")
}

fn row_name(entry: &FileTreeEntry) -> String {
    if entry.depth == 0 {
        entry.path.display().to_string()
    } else {
        entry
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// Project the tree's entries into the shape
/// `directory-listing-mode` reads (DL.2's shared local).
///
/// `icon_byte` is the byte length of the indent + marker, so the icon
/// splices between the tree structure and the name rather than at the
/// far left.
pub fn listing_entries(entries: &[FileTreeEntry]) -> Vec<crate::listing_mode::ListingEntry> {
    entries
        .iter()
        .map(|e| crate::listing_mode::ListingEntry {
            path: e.path.clone(),
            is_dir: matches!(e.kind, FileTreeEntryKind::Directory { .. }),
            icon_byte: row_prefix(e).len() as u32,
        })
        .collect()
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
    fn open_returns_buffer_and_initial_entries() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let (_buf, entries) = FileTreeBuffer::open(&dir, false).unwrap();
        // Root + 2 children = 3 entries.
        assert_eq!(entries.len(), 3);
        assert!(matches!(
            entries[1].kind,
            FileTreeEntryKind::Directory { .. }
        ));
        assert!(matches!(entries[2].kind, FileTreeEntryKind::File));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn toggle_entries_at_expands_and_collapses() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("inner.txt"), "x").unwrap();
        let (_buf, mut entries) = FileTreeBuffer::open(&dir, false).unwrap();
        // "sub" is at index 1 (after root row).
        toggle_entries_at(&mut entries, 1).unwrap();
        // After expand: root, sub, sub/inner.txt = 3 rows.
        assert_eq!(entries.len(), 3);
        // Collapse again.
        toggle_entries_at(&mut entries, 1).unwrap();
        assert_eq!(entries.len(), 2);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn render_uses_indentation_and_marker() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let (buf, _) = FileTreeBuffer::open(&dir, false).unwrap();
        let body = buf.content.as_string();
        assert!(body.contains("a.txt"));
        // File row is the second; depth 1 = 2 spaces of indent + 2
        // spaces for the file marker + the 2-cell BMP icon.
        assert!(body.lines().nth(1).unwrap().starts_with("    "));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn render_bmp_mode_does_not_double_arrow_on_directories() {
        // Regression: the BMP fallback used to return `▸ ` as the
        // dir glyph -- identical to the collapsed-marker, so a
        // row would render as `▸ ▸ name`. Post-fix the glyph is
        // 2-space padding, eliminating the visual collision while
        // keeping the name column aligned with files.
        let dir = temp_dir();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let entries = vec![
            FileTreeEntry {
                path: dir.clone(),
                depth: 0,
                kind: FileTreeEntryKind::Directory { expanded: true },
            },
            FileTreeEntry {
                path: dir.join("sub"),
                depth: 1,
                kind: FileTreeEntryKind::Directory { expanded: false },
            },
        ];
        let buf = render_to_buffer(&entries, false);
        let body = buf.as_string();
        assert!(
            !body.contains("▸ ▸"),
            "BMP-mode collapsed dir row should not have double arrow; got:\n{body}",
        );
        assert!(
            !body.contains("▾ ▸"),
            "BMP-mode expanded dir + collapsed child should not pair the two markers as a double glyph; got:\n{body}",
        );
        // Sanity: the row still contains the marker and the dir
        // name, just not a redundant glyph between them.
        let row = body.lines().nth(1).unwrap();
        assert!(
            row.contains("▸ "),
            "collapsed marker should be present: {row:?}"
        );
        assert!(row.contains("sub"), "dir name should be present: {row:?}");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn move_cursor_clamps_to_last_line() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let (mut buf, _) = FileTreeBuffer::open(&dir, false).unwrap();
        buf.move_cursor(0, 1000, 10);
        assert_eq!(buf.cursor.line, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn entry_at_line_returns_currently_targeted() {
        let dir = temp_dir();
        std::fs::write(dir.join("a.txt"), "x").unwrap();
        let (_buf, entries) = FileTreeBuffer::open(&dir, false).unwrap();
        let e = entry_at_line(&entries, 1).unwrap();
        assert!(matches!(e.kind, FileTreeEntryKind::File));
        std::fs::remove_dir_all(dir).ok();
    }

    /// DL.4: the rope is entry NAMES. The glyph used to be baked in;
    /// it is virtual text now, published by `directory-listing-mode`
    /// as a leading inlay.
    ///
    /// This test is the inverse of the two it replaces (which asserted
    /// the nerd glyph and the BMP fallback were *in* the body). Text a
    /// user can search and yank should not contain rendering artefacts,
    /// and keeping it out is what makes the tree symmetric with oil,
    /// whose rope never held glyphs because `:w` diffs it.
    #[test]
    fn rope_holds_names_not_glyphs() {
        let dir = temp_dir();
        std::fs::write(dir.join("main.rs"), "x").unwrap();
        let (_buf, entries) = FileTreeBuffer::open(&dir, true).unwrap();
        let body = render_to_text(&entries);
        assert!(
            body.contains("main.rs"),
            "the name must be in the rope: {body}"
        );
        for glyph in ["\u{f1617} ", "· ", "◆ "] {
            assert!(
                !body.contains(glyph),
                "glyph {glyph:?} leaked into the rope: {body}"
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    /// The icon anchors between the tree structure and the name — at
    /// byte 0 it would render left of the indent and the tree's shape
    /// would collapse.
    #[test]
    fn listing_entries_anchor_icons_after_indent_and_marker() {
        let dir = temp_dir();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("main.rs"), "x").unwrap();
        let (_buf, mut entries) = FileTreeBuffer::open(&dir, false).unwrap();
        // Expand `sub` so there is a depth-1 row to check.
        let sub = entries
            .iter()
            .position(|e| matches!(e.kind, FileTreeEntryKind::Directory { .. }) && e.depth == 1)
            .expect("a child directory row");
        toggle_entries_at(&mut entries, sub).unwrap();

        let listing = listing_entries(&entries);
        assert_eq!(listing.len(), entries.len(), "one per row");

        let text = render_to_text(&entries);
        for (row, (le, line)) in listing.iter().zip(text.split('\n')).enumerate() {
            let anchor = le.icon_byte as usize;
            assert!(
                line.is_char_boundary(anchor),
                "row {row}: icon_byte {anchor} must be a char boundary in {line:?}"
            );
            // Everything before the anchor is structure (indent +
            // marker); nothing of the name may precede it.
            let prefix = &line[..anchor];
            assert!(
                prefix.chars().all(|c| c == ' ' || c == '▾' || c == '▸'),
                "row {row}: prefix {prefix:?} must be indent + marker only"
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }
}
