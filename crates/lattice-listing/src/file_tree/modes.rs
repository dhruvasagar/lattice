//! `file-tree-mode` -- major mode for the file-tree buffer + its
//! `BufferLocal`-owned per-buffer state (root, entries,
//! nerd-fonts toggle).
//!
//! Lives here (rather than in `lattice-mode`) per the
//! mode-architecture convention: "a mode lives with the crate
//! that owns its associated feature." `FileTreeBuffer` lives
//! in this crate, so the mode + the three pieces of
//! `file-tree-mode`-owned state live here too.
//!
//! ## What the `BufferLocal`s carry
//!
//! M.3.2.c.5 made `BufferLocal`s the **single** source of
//! truth for per-buffer mode-owned state. `FileTreeBuffer`
//! itself carries only the rendered rope + cursor / scroll;
//! every piece of "what does this buffer point at"
//! information lives in the three locals declared below:
//!
//! - [`FileTreeRoot`] -- the directory the tree is rooted at.
//! - [`FileTreeEntries`] -- the flat list of visible entries
//!   (root + every expanded subdir's children). Mutated
//!   through the App-side toggle chokepoint that re-renders
//!   the rope.
//! - [`FileTreeNerdFonts`] -- whether the rendered rope
//!   embeds nerd-font glyphs.
//!
//! The App reads through these directly; there is no struct
//! mirror to drift.

use std::path::PathBuf;

use std::sync::{Arc, OnceLock};

use lattice_config::OptionOverrideSet;
use lattice_core::BufferKind;
use lattice_core::ui::pane::OpenTarget;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferLocal, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry, keymap_entry,
};

use super::{FileTreeEntry, FileTreeEntryKind};

/// Major mode for file-tree buffers. Read-only contribution
/// (`ReadOnly = true`); any buffer whose major is
/// `file-tree-mode` rejects mutating operators.
pub struct FileTreeMode;

impl FileTreeMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("file-tree-mode")
    }
}

impl Mode for FileTreeMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    /// H.2: file-tree buffers (`BufferKind::FileTree`) dispatch to
    /// this major via the registry's kind index.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::FileTree)
    }
    /// `Number = false` is per-major, not per-listing (2026-08-16): a
    /// tree is a navigation surface and hides line numbers like every
    /// other tree UI, while oil is an ordinary editable buffer that
    /// keeps them. It used to sit on the shared
    /// `directory-listing-mode`, which forced the tree's answer onto
    /// oil. It lives here now, next to `ReadOnly`, which is per-major
    /// for the same reason.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::Number = false,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// 2026-05-26: claim invocation dispatch for file-tree panes
    /// via `Editor::run_file_tree_invocation`.
    fn invocation_runner(&self) -> Option<ModeId> {
        Some(Self::mode_id())
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(file_tree_mode_keymap_entries())
    }
    /// LM.4: the navigation/open chord bodies, mode-owned (they were the
    /// host's `do_file_tree_follow` + the `do_oil_navigate_up` file-tree
    /// branch, reached through the shared `Help | FileTree` input-gate).
    /// Each reads the tree's entries from the `ActionContext`'s buffer-locals
    /// (LM.1) and the row under the cursor, then hands the host an `Effect`
    /// (the diff/oil pattern). Bound globally at boot; each names its own
    /// `view` (`ctx.buffer_id`) so many trees stay independent (design §3.2).
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        // `<CR>`: a directory toggles expansion; a file opens in the current
        // pane.
        let follow: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let (path, is_dir, line) = file_tree_entry_at(ctx)?;
            let view = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            Some(if is_dir {
                Effect::FileTreeToggle {
                    view,
                    entry_index: line,
                }
            } else {
                Effect::OpenBufferAt {
                    path: Some(path),
                    position: lattice_protocol::Position::ZERO,
                    force: false,
                    content: None,
                    activate_minor: None,
                }
            })
        });
        vec![
            ActionHandlerContribution {
                action_name: "action:file-tree-follow",
                handler: follow,
            },
            // `<C-s>` / `<C-v>` / `<C-t>`: open the row in a split / vsplit /
            // tab. A file opens directly; a directory path resolves to
            // `DoEditOutcome::Directory` in the new pane, opening oil there.
            file_tree_open_in_target("action:file-tree-follow-split", OpenTarget::Split),
            file_tree_open_in_target("action:file-tree-follow-vsplit", OpenTarget::VSplit),
            file_tree_open_in_target("action:file-tree-follow-tab", OpenTarget::Tab),
        ]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn file_tree_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry!(
                mode: Normal,
                chord: "<CR>",
                doc: "Open the row under the cursor: toggle a directory's expansion, or open a file in the current pane.",
                cmd: "action:file-tree-follow"
            ),
            // No `-` here: `oil-global-mode` owns it, and as a minor mode
            // it would shadow one declared on this major.
            keymap_entry!(
                mode: Normal,
                chord: "<C-s>",
                doc: "Open the row under the cursor in a horizontal split.",
                cmd: "action:file-tree-follow-split"
            ),
            keymap_entry!(
                mode: Normal,
                chord: "<C-v>",
                doc: "Open the row under the cursor in a vertical split.",
                cmd: "action:file-tree-follow-vsplit"
            ),
            keymap_entry!(
                mode: Normal,
                chord: "<C-t>",
                doc: "Open the row under the cursor in a new tab.",
                cmd: "action:file-tree-follow-tab"
            ),
        ]
    })
}

/// LM.4: resolve the tree row under the cursor — `(path, is-dir, line)` —
/// from the `ActionContext`'s buffer-locals. `None` when the buffer carries
/// no tree state or the cursor is past the last row.
/// `-` on a file-tree row: open an oil browser at the row's directory —
/// the directory itself for a directory row, the file's parent for a file
/// row. `None` when the cursor is past the last row.
///
/// The body is the tree's; the chord is `oil-global-mode`'s, which calls
/// this for buffers carrying [`FileTreeEntries`].
pub(crate) fn file_tree_row_directory(ctx: &ActionContext<'_>) -> Option<Effect> {
    let (path, is_dir, _line) = file_tree_entry_at(ctx)?;
    let dir = if is_dir {
        path
    } else {
        path.parent().map(std::path::Path::to_path_buf)?
    };
    Some(Effect::OpenOil { dir: Some(dir) })
}

fn file_tree_entry_at(ctx: &ActionContext<'_>) -> Option<(std::path::PathBuf, bool, u32)> {
    let entries = ctx.buffer_local::<FileTreeEntries>()?;
    let line = ctx.cursor.line;
    let entry = entries.0.get(line as usize)?;
    let is_dir = matches!(entry.kind, FileTreeEntryKind::Directory { .. });
    Some((entry.path.clone(), is_dir, line))
}

/// LM.4: a `<C-s>`/`<C-v>`/`<C-t>` handler that opens the row under the
/// cursor in `target`. Shared body for the three chords.
fn file_tree_open_in_target(
    action_name: &'static str,
    target: OpenTarget,
) -> ActionHandlerContribution {
    let handler: ActionHandler = Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let (path, _is_dir, _line) = file_tree_entry_at(ctx)?;
        Some(Effect::OpenInTarget {
            path: Some(path),
            position: lattice_protocol::Position::ZERO,
            target,
        })
    });
    ActionHandlerContribution {
        action_name,
        handler,
    }
}

/// Filesystem path the file-tree buffer is rooted at.
/// Single source of truth (M.3.2.c.5).
#[derive(Debug, Clone)]
pub struct FileTreeRoot(pub PathBuf);

impl BufferLocal for FileTreeRoot {
    const NAME: &'static str = "file-tree-mode.root";
    const DOC: &'static str = "Directory the file-tree buffer is rooted at -- the path \
         the user passed to `:Tree` (or the workspace root for \
         the default tree).";
    const OWNER_MODE: &'static str = "file-tree-mode";
    fn describe(&self) -> String {
        self.0.display().to_string()
    }
}

/// Flat tree-of-entries backing the file-tree buffer.
/// Each entry carries its depth + expansion state. The rendered
/// rope is derived from this list -- App-side toggle handlers
/// mutate the entries through the
/// [`crate::toggle_entries_at`] helper and re-write this local
/// + the buffer's rope as one update.
#[derive(Debug, Clone)]
pub struct FileTreeEntries(pub Vec<FileTreeEntry>);

impl BufferLocal for FileTreeEntries {
    const NAME: &'static str = "file-tree-mode.entries";
    const DOC: &'static str = "Flat list of tree entries (directories + files), each \
         carrying its depth + expansion state. The file-tree \
         renderer iterates this in order; the rope content is \
         derived from it.";
    const OWNER_MODE: &'static str = "file-tree-mode";
    fn describe(&self) -> String {
        format!("{} entries", self.0.len())
    }
}

/// Whether the file-tree renders nerd-font icon glyphs inline.
#[derive(Debug, Clone, Copy)]
pub struct FileTreeNerdFonts(pub bool);

impl BufferLocal for FileTreeNerdFonts {
    const NAME: &'static str = "file-tree-mode.nerd-fonts";
    const DOC: &'static str = "Whether the file-tree buffer's rendered rope embeds \
         nerd-font icon glyphs alongside file names.";
    const OWNER_MODE: &'static str = "file-tree-mode";
    fn describe(&self) -> String {
        if self.0 { "enabled" } else { "disabled" }.to_string()
    }
}

/// Register every `lattice-file-tree`-owned mode against
/// `registry`. Called from the App's boot path alongside
/// `lattice_mode::register_foundation_modes` etc. Mirrors
/// `lattice_oil::register_oil_modes`.
pub fn register_file_tree_modes(registry: &mut ModeRegistry) {
    registry
        .register(FileTreeMode)
        .expect("file-tree-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_tree_mode_id_kind_and_options() {
        assert_eq!(FileTreeMode.id(), FileTreeMode::mode_id());
        assert_eq!(FileTreeMode::mode_id().as_str(), "file-tree-mode");
        assert_eq!(FileTreeMode.kind(), ModeKind::Major);
        // Assert by identity, not by count: a bare count says nothing
        // about WHICH options are contributed and breaks every time the
        // set legitimately grows (it did, when `Number` moved here off
        // the shared minor).
        let ids: Vec<std::any::TypeId> = FileTreeMode
            .options()
            .iter()
            .map(|o| o.option_type_id)
            .collect();
        assert!(
            ids.contains(&std::any::TypeId::of::<lattice_config::ReadOnly>()),
            "the tree is read-only"
        );
        assert!(
            ids.contains(&std::any::TypeId::of::<lattice_config::Number>()),
            "the tree hides line numbers"
        );
    }

    #[test]
    fn buffer_local_metadata_owner_mode() {
        assert_eq!(<FileTreeRoot as BufferLocal>::OWNER_MODE, "file-tree-mode");
        assert_eq!(
            <FileTreeEntries as BufferLocal>::OWNER_MODE,
            "file-tree-mode"
        );
        assert_eq!(
            <FileTreeNerdFonts as BufferLocal>::OWNER_MODE,
            "file-tree-mode"
        );
        assert_eq!(FileTreeRoot(PathBuf::from("/x")).describe(), "/x");
        assert_eq!(FileTreeNerdFonts(true).describe(), "enabled");
        assert_eq!(FileTreeNerdFonts(false).describe(), "disabled");
    }

    #[test]
    fn register_file_tree_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_file_tree_modes(&mut registry);
        assert!(registry.is_registered(FileTreeMode::mode_id()));
    }

    #[test]
    fn file_tree_mode_binds_navigation_and_open_chords() {
        use lattice_mode::Mode as _;
        let km = FileTreeMode.keymap();
        // LM.4: assert by identity, not count — the set will keep growing.
        let bound: Vec<(&str, Option<&str>)> =
            km.entries.iter().map(|e| (e.chord, e.command)).collect();
        for (chord, cmd) in [
            ("<CR>", "action:file-tree-follow"),
            ("<C-s>", "action:file-tree-follow-split"),
            ("<C-v>", "action:file-tree-follow-vsplit"),
            ("<C-t>", "action:file-tree-follow-tab"),
        ] {
            assert!(
                bound.contains(&(chord, Some(cmd))),
                "file-tree-mode must bind {chord} → {cmd}; got {bound:?}",
            );
        }
    }
}
