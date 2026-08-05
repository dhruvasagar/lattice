//! MG.18d: putting the cursor back on the user's work after a rebuild.
//!
//! Every magit mutation ends in a refresh that replaces the whole
//! buffer, so the row the cursor was on means nothing afterwards —
//! files move between sections, counts change, the staged hunk is gone.
//! At file granularity losing your place was tolerable. At hunk
//! granularity it defeats the feature: staging four of a file's six
//! hunks means finding your place four times.
//!
//! So the cursor is restored by **identity, not by row**: the entry (or
//! file header) the work belonged to, plus the ordinal of the hunk
//! within it. Staging hunk *k* removes it, so ordinal *k* now names the
//! next remaining hunk — the restore rule and magit's behaviour fall
//! out of the same arithmetic. Clamping to the last hunk covers staging
//! the final one.
//!
//! Pure functions over the rebuilt text, resolved BEFORE it is applied:
//! the refresh already holds the string it is about to write, so no
//! second read of the buffer is needed and there is no window in which
//! the text could change under the lookup.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::Effect;
use lattice_mode::SubsystemBoot;
use lattice_mode::inbound::InboundBus;
use lattice_protocol::position::Position;

/// A resolved cursor placement, on its way back to the editor thread.
pub struct CursorRequest {
    pub buffer: BufferId,
    pub position: Position,
}

/// Service alias — register and look up through this exact type
/// (`feedback_servicesregistry_arc_typeid`).
pub type CursorBusHandle = Arc<InboundBus<CursorRequest>>;

/// Install the bus a refresh sends its resolved cursor on.
///
/// **`inbound`, not `tick_callback`.** The wake is baked into
/// `InboundBus::send`, so the editor is woken the moment the position
/// is known and the drain runs off-keystroke. A bare tick callback has
/// no wake of its own: the cursor would sit until the user pressed
/// something else, which reads as "staging works but the cursor only
/// catches up when I touch a key" — see `boot-composition.md` §3.
///
/// The handler is the whole mapping: one request, one
/// [`Effect::CursorMoveIn`]. Targeted rather than a bare `CursorMove`
/// because by the time this lands the user may have moved to another
/// buffer, and the position means nothing there.
pub(crate) fn install_cursor_bus(boot: &mut impl SubsystemBoot) {
    let bus = boot.inbound::<CursorRequest, _>(|req| {
        vec![Effect::CursorMoveIn {
            target: req.buffer,
            position: req.position,
        }]
    });
    boot.register_service::<CursorBusHandle>(Arc::new(bus));
}

/// Hand a resolved position back to the editor thread.
///
/// `None` bus means a harness without the service — the refresh still
/// works, it just does not move the cursor.
pub(crate) fn send_cursor(bus: &Option<CursorBusHandle>, buffer: BufferId, position: Position) {
    let Some(bus) = bus else { return };
    // A failed send means the drain was dropped (the editor is going
    // away); there is nothing useful to do about a cursor at that point.
    let _ = bus.send(CursorRequest { buffer, position });
}

/// Where the work lived, in whichever buffer shape the view uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAnchor {
    /// A magit-status entry row (`  modified    src/main.rs`) under the
    /// section matching `staged`. Its diff, when expanded, follows it.
    StatusEntry { path: PathBuf, staged: bool },
    /// A `diff --git a/<path> b/<path>` line. magit-diff's buffers have
    /// no entry rows — the file header is the only anchor there.
    DiffHeader { path: PathBuf },
}

/// The work a mutation interrupted: which file, and which hunk of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkRestore {
    pub anchor: RestoreAnchor,
    /// 0-based index of the hunk among that file's hunks, as the buffer
    /// showed them *before* the mutation.
    pub ordinal: usize,
}

/// The same fact in view-independent terms, as the shared staging path
/// can state it: which file, which side of the index, which hunk.
///
/// The *anchor* is deliberately not decided here. Turning this into a
/// row is a question about buffer shape — an entry row in magit-status,
/// a `diff --git` header in a diff buffer — and only the view knows
/// which it has. So the staging path names the work and each view names
/// the landmark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HunkSite {
    pub path: PathBuf,
    pub staged: bool,
    pub ordinal: usize,
}

impl HunkSite {
    /// Read as a magit-status entry row.
    pub fn as_status_entry(self) -> HunkRestore {
        HunkRestore {
            anchor: RestoreAnchor::StatusEntry {
                path: self.path,
                staged: self.staged,
            },
            ordinal: self.ordinal,
        }
    }

    /// Read as a `diff --git` header in a raw diff buffer, where the
    /// staged/unstaged split is a property of the whole buffer rather
    /// than of a section within it.
    pub fn as_diff_header(self) -> HunkRestore {
        HunkRestore {
            anchor: RestoreAnchor::DiffHeader { path: self.path },
            ordinal: self.ordinal,
        }
    }
}

/// Resolve `restore` against the rebuilt buffer `text`.
///
/// `None` when the anchor is gone — the file was fully staged and left
/// its section, or the buffer no longer shows it. The caller then sends
/// no cursor at all, which leaves the user wherever the refresh put
/// them rather than guessing at a row.
pub(crate) fn restore_position(text: &str, restore: &HunkRestore) -> Option<Position> {
    let lines: Vec<&str> = text.lines().collect();
    let anchor_row = anchor_row(&lines, &restore.anchor)?;

    // The file's own `diff --git` header — present only while its diff
    // is expanded. Without one there are no hunks to land on and the
    // entry row itself is the honest answer.
    let Some(header_row) = file_header_row(&lines, &restore.anchor, anchor_row) else {
        return Some(Position::new(anchor_row as u32, 0));
    };

    let hunks = hunk_rows_of_file(&lines, header_row);
    if hunks.is_empty() {
        return Some(Position::new(anchor_row as u32, 0));
    }
    // Ordinal `k` now names the hunk that took the staged one's place.
    // Clamp: staging the last hunk lands on the new last.
    let row = hunks[restore.ordinal.min(hunks.len() - 1)];
    Some(Position::new(row as u32, 0))
}

/// The row the anchor names, or `None` if the buffer no longer has it.
fn anchor_row(lines: &[&str], anchor: &RestoreAnchor) -> Option<usize> {
    match anchor {
        RestoreAnchor::DiffHeader { path } => {
            lines.iter().position(|l| is_file_header_for(l, path))
        }
        RestoreAnchor::StatusEntry { path, staged } => {
            let want_staged = *staged;
            let mut in_staged_section = false;
            for (row, line) in lines.iter().enumerate() {
                if crate::sections::is_section_header(line.trim()) {
                    in_staged_section = line.starts_with("Staged");
                    continue;
                }
                if in_staged_section != want_staged {
                    continue;
                }
                if entry_path(line).is_some_and(|p| p == path.as_path()) {
                    return Some(row);
                }
            }
            None
        }
    }
}

/// The `diff --git` row belonging to the anchor.
///
/// For a status entry that is the first one below it, and only while it
/// really belongs to this entry — a collapsed entry is followed by the
/// *next* entry, whose own expansion must not be adopted. For a
/// magit-diff anchor the anchor row already IS the header.
fn file_header_row(lines: &[&str], anchor: &RestoreAnchor, anchor_row: usize) -> Option<usize> {
    match anchor {
        RestoreAnchor::DiffHeader { .. } => Some(anchor_row),
        RestoreAnchor::StatusEntry { .. } => {
            let next = lines.get(anchor_row + 1)?;
            next.starts_with("diff --git").then_some(anchor_row + 1)
        }
    }
}

/// Rows of the `@@` headers between `header_row` and the next file's
/// `diff --git` (or a section header, which ends an inline expansion in
/// magit-status).
fn hunk_rows_of_file(lines: &[&str], header_row: usize) -> Vec<usize> {
    let mut rows = Vec::new();
    for (offset, line) in lines.iter().enumerate().skip(header_row + 1) {
        if line.starts_with("diff --git") || crate::sections::is_section_header(line.trim()) {
            break;
        }
        if line.starts_with("@@") {
            rows.push(offset);
        }
    }
    rows
}

fn is_file_header_for(line: &str, path: &Path) -> bool {
    let Some(rest) = line.strip_prefix("diff --git a/") else {
        return false;
    };
    rest.split(" b/")
        .next()
        .is_some_and(|p| Path::new(p) == path)
}

/// The path on a magit-status entry row, or `None` for anything else.
///
/// Reuses the crate's one entry classifier rather than re-deriving the
/// row layout — the section is already known by the caller, so the
/// staged flag it would compute is discarded here.
fn entry_path(line: &str) -> Option<PathBuf> {
    match crate::actions::classify_line_text(line, || None)? {
        crate::actions::StatusLine::File { path, .. } => Some(path),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A status buffer with one expanded file holding three hunks.
    const STATUS: &str = "\
Unstaged changes (2)
  modified     src/main.rs
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,2 @@
 keep
-a
@@ -10,2 +10,2 @@
 keep
-b
@@ -20,2 +20,2 @@
 keep
-c
  modified     src/other.rs

Recent commits (1)
  abc1234 something
";

    fn entry(path: &str, staged: bool) -> RestoreAnchor {
        RestoreAnchor::StatusEntry {
            path: PathBuf::from(path),
            staged,
        }
    }

    /// The core rule: staging hunk `k` removes it, so ordinal `k` now
    /// names the next one — which is where magit leaves you.
    #[test]
    fn the_same_ordinal_lands_on_the_hunk_that_took_the_staged_ones_place() {
        let r = HunkRestore {
            anchor: entry("src/main.rs", false),
            ordinal: 1,
        };
        assert_eq!(
            restore_position(STATUS, &r),
            Some(Position::new(8, 0)),
            "ordinal 1 is the second `@@`"
        );
    }

    /// Staging the LAST hunk has no successor; magit lands on the new
    /// last rather than falling off the entry.
    #[test]
    fn an_ordinal_past_the_end_clamps_to_the_last_hunk() {
        let r = HunkRestore {
            anchor: entry("src/main.rs", false),
            ordinal: 9,
        };
        assert_eq!(restore_position(STATUS, &r), Some(Position::new(11, 0)));
    }

    /// The entry is still listed but its diff is collapsed (nothing was
    /// re-expanded, e.g. after `gr`): the entry row is the honest
    /// answer, not a hunk row from someone else's diff.
    #[test]
    fn a_collapsed_entry_restores_to_its_own_row() {
        let text = "\
Unstaged changes (2)
  modified     src/main.rs
  modified     src/other.rs
";
        let r = HunkRestore {
            anchor: entry("src/main.rs", false),
            ordinal: 2,
        };
        assert_eq!(restore_position(text, &r), Some(Position::new(1, 0)));
    }

    /// The entry below a collapsed one carries its own expansion. The
    /// collapsed entry must not adopt it — that would jump the cursor
    /// into a different file's diff.
    #[test]
    fn a_collapsed_entry_does_not_adopt_the_next_entrys_diff() {
        let text = "\
Unstaged changes (2)
  modified     src/main.rs
  modified     src/other.rs
diff --git a/src/other.rs b/src/other.rs
@@ -1,2 +1,2 @@
 keep
-x
";
        let r = HunkRestore {
            anchor: entry("src/main.rs", false),
            ordinal: 0,
        };
        assert_eq!(
            restore_position(text, &r),
            Some(Position::new(1, 0)),
            "src/main.rs is collapsed — its own row, not src/other.rs's hunk"
        );
    }

    /// Staging a file's last remaining hunk moves it out of Unstaged
    /// entirely. There is nothing to restore to; leaving the cursor
    /// where the refresh put it beats guessing at a row.
    #[test]
    fn a_vanished_entry_yields_no_position() {
        let text = "\
Staged changes (1)
  modified     src/main.rs
";
        let r = HunkRestore {
            anchor: entry("src/main.rs", false),
            ordinal: 0,
        };
        assert_eq!(
            restore_position(text, &r),
            None,
            "the entry moved to Staged — the unstaged anchor is gone"
        );
    }

    /// The same path appears in BOTH sections when a file has staged
    /// and unstaged changes. The anchor's side decides which row.
    #[test]
    fn the_staged_flag_picks_between_two_rows_for_one_path() {
        let text = "\
Staged changes (1)
  modified     src/main.rs

Unstaged changes (1)
  modified     src/main.rs
";
        assert_eq!(
            restore_position(
                text,
                &HunkRestore {
                    anchor: entry("src/main.rs", true),
                    ordinal: 0
                }
            ),
            Some(Position::new(1, 0))
        );
        assert_eq!(
            restore_position(
                text,
                &HunkRestore {
                    anchor: entry("src/main.rs", false),
                    ordinal: 0
                }
            ),
            Some(Position::new(4, 0))
        );
    }

    /// magit-diff's buffers are raw diffs: no entries, no sections, and
    /// several files in one buffer.
    #[test]
    fn a_diff_buffer_anchors_on_the_file_header() {
        let text = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
 keep
-a
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,2 +1,2 @@
 keep
-b
@@ -9,2 +9,2 @@
 keep
-c
";
        let r = HunkRestore {
            anchor: RestoreAnchor::DiffHeader {
                path: PathBuf::from("b.rs"),
            },
            ordinal: 1,
        };
        assert_eq!(
            restore_position(text, &r),
            Some(Position::new(12, 0)),
            "b.rs's second hunk, not a.rs's"
        );
    }

    /// A file's hunk list must stop at the next file's header, or
    /// ordinal-clamping would walk into the neighbour's hunks.
    #[test]
    fn a_files_hunks_do_not_run_into_the_next_files() {
        let text = "\
diff --git a/a.rs b/a.rs
@@ -1,2 +1,2 @@
 keep
-a
diff --git a/b.rs b/b.rs
@@ -1,2 +1,2 @@
 keep
-b
";
        let r = HunkRestore {
            anchor: RestoreAnchor::DiffHeader {
                path: PathBuf::from("a.rs"),
            },
            ordinal: 5,
        };
        assert_eq!(
            restore_position(text, &r),
            Some(Position::new(1, 0)),
            "clamped to a.rs's only hunk"
        );
    }
}
