//! `oil-mode` -- major mode for the oil.nvim-style editable
//! directory listing buffer.
//!
//! Lives here (rather than in `lattice-mode`) per the
//! mode-architecture convention: "a mode lives with the crate
//! that owns its associated feature." `OilBuffer` lives in
//! this crate, so the mode + the oil-mode-owned
//! [`BufferLocal`] state ([`OilDir`]) live here too.
//!
//! The mode itself is metadata only: kind = Major, no
//! contributed options (oil is writable so it
//! contributes no `ReadOnly` override), no capability
//! requirements, no-op lifecycle hooks. Behaviour --
//! navigation, the rope-vs-snapshot diff, the
//! filesystem-op planner -- lives on `OilBuffer` and the
//! consumer's App surface.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use lattice_core::BufferKind;
use lattice_core::ui::pane::OpenTarget;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferLocal, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry, keymap_entry,
};

/// Major mode for oil-style directory-listing buffers. Any
/// buffer whose major is `oil-mode` is an `OilBuffer`; the
/// renderer dispatches accordingly.
pub struct OilMode;

impl OilMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("oil-mode")
    }
}

fn oil_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry!(
                mode: Normal,
                chord: "-",
                doc: "Navigate to the parent directory in the oil buffer.",
                cmd: "action:oil-navigate-up"
            ),
            keymap_entry!(
                mode: Normal,
                chord: "<CR>",
                doc: "Open the entry under the cursor: descend into a directory, or open a file in the current pane.",
                cmd: "action:oil-follow"
            ),
            keymap_entry!(
                mode: Normal,
                chord: "<C-s>",
                doc: "Open the entry under the cursor in a horizontal split.",
                cmd: "action:oil-follow-split"
            ),
            keymap_entry!(
                mode: Normal,
                chord: "<C-v>",
                doc: "Open the entry under the cursor in a vertical split.",
                cmd: "action:oil-follow-vsplit"
            ),
            keymap_entry!(
                mode: Normal,
                chord: "<C-t>",
                doc: "Open the entry under the cursor in a new tab.",
                cmd: "action:oil-follow-tab"
            ),
        ]
    })
}

impl Mode for OilMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    /// H.2: oil buffers (`BufferKind::Oil`) dispatch to this major
    /// via the registry's kind index.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Oil)
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// 2026-05-26: claim invocation dispatch for oil panes via
    /// `Editor::run_oil_invocation`.
    fn invocation_runner(&self) -> Option<ModeId> {
        Some(Self::mode_id())
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(oil_mode_keymap_entries())
    }
    /// LM.3: the navigation/open chord bodies, mode-owned (they were the
    /// host's `do_oil_follow` / `do_oil_navigate_up` + the `BufferKind::Oil`
    /// input-gate). Each reads the oil buffer's dir + snapshot from the
    /// `ActionContext`'s buffer-locals (LM.1) and the entry under the cursor,
    /// then hands the host an `Effect` (the diff-mode pattern): the mode owns
    /// the *decision*, the host owns the *apply*. Bound globally at boot by
    /// `register_mode_action_handlers` — oil-mode can be active on many
    /// buffers at once, and each handler names its own `view`
    /// (`ctx.buffer_id`), so many oil buffers stay independent (design §3.2).
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        // `<CR>`: a directory re-lists in place; a file opens in the current
        // pane.
        let follow: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let (dir, name, is_dir) = oil_entry_at(ctx)?;
            let view = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let target = dir.join(&name);
            Some(if is_dir {
                Effect::OilNavigate {
                    view,
                    dir: target,
                    focus: None,
                }
            } else {
                Effect::OpenBufferAt {
                    path: Some(target),
                    position: lattice_protocol::Position::ZERO,
                    force: false,
                    content: None,
                    activate_minor: None,
                }
            })
        });
        // `-`: re-list to the parent, landing the cursor on the directory we
        // stepped out of (oil.nvim's round-trip).
        let up: ActionHandler = Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
            let dir = ctx.buffer_local::<OilDir>()?.0.clone();
            let parent = dir.parent()?.to_path_buf();
            let view = lattice_core::BufferId(ctx.buffer_id.0 as u32);
            let focus = dir.file_name().map(|n| n.to_string_lossy().into_owned());
            Some(Effect::OilNavigate {
                view,
                dir: parent,
                focus,
            })
        });
        vec![
            ActionHandlerContribution {
                action_name: "action:oil-follow",
                handler: follow,
            },
            ActionHandlerContribution {
                action_name: "action:oil-navigate-up",
                handler: up,
            },
            // `<C-s>` / `<C-v>` / `<C-t>`: open the entry in a split / vsplit /
            // tab. A file opens directly; a directory path resolves to
            // `DoEditOutcome::Directory` in the new pane, which opens oil there
            // (`handle_do_edit_outcome`), so the same effect serves both.
            oil_open_in_target("action:oil-follow-split", OpenTarget::Split),
            oil_open_in_target("action:oil-follow-vsplit", OpenTarget::VSplit),
            oil_open_in_target("action:oil-follow-tab", OpenTarget::Tab),
        ]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// LM.3: resolve the oil entry under the cursor — `(oil dir, entry name,
/// is-dir)` — from the `ActionContext`'s buffer-locals. `None` when the
/// buffer carries no oil state or the cursor is past the last row, which a
/// handler treats as "nothing to open".
fn oil_entry_at(ctx: &ActionContext<'_>) -> Option<(PathBuf, String, bool)> {
    let dir = ctx.buffer_local::<OilDir>()?.0.clone();
    let snapshot = ctx.buffer_local::<OilSnapshotLocal>()?;
    let entry = snapshot
        .0
        .snapshot_entries()
        .get(ctx.cursor.line as usize)?;
    Some((dir, entry.name.clone(), entry.is_dir))
}

/// LM.3: a `<C-s>`/`<C-v>`/`<C-t>` handler that opens the entry under the
/// cursor in `target`. Shared body for the three chords.
fn oil_open_in_target(action_name: &'static str, target: OpenTarget) -> ActionHandlerContribution {
    let handler: ActionHandler = Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let (dir, name, _is_dir) = oil_entry_at(ctx)?;
        Some(Effect::OpenInTarget {
            path: Some(dir.join(&name)),
            position: lattice_protocol::Position::ZERO,
            target,
        })
    });
    ActionHandlerContribution {
        action_name,
        handler,
    }
}

/// `BufferLocal` carrying the filesystem path the oil
/// buffer's listing represents (M.3.2.c.3 mirror of
/// `OilBuffer::dir`). Renderers / writers read through this
/// rather than poking `OilBuffer::dir` directly so the canonical
/// "what directory does this oil buffer represent" lookup is
/// uniform with the rest of the mode-owned per-buffer state.
#[derive(Debug, Clone)]
pub struct OilDir(pub PathBuf);

/// DL.5: the directory state `:w` diffs against.
///
/// It used to live on `OilBuffer` alongside the rope. The rope is an
/// actor-backed Document now, so the snapshot moves here — the same
/// place `OilDir` already lived, and the same shape the file tree has
/// used for its entries all along.
#[derive(Debug, Clone, Default)]
pub struct OilSnapshotLocal(pub super::OilSnapshot);

impl BufferLocal for OilSnapshotLocal {
    const NAME: &'static str = "oil-mode.snapshot";
    const DOC: &'static str = "Directory entries as of open or the last successful `:w`. \
         `:w` diffs the buffer's text against this to derive the renames, \
         deletes and creates to execute.";
    const OWNER_MODE: &'static str = "oil-mode";
    fn describe(&self) -> String {
        format!("{} entries", self.0.snapshot_entries().len())
    }
}

impl BufferLocal for OilDir {
    const NAME: &'static str = "oil-mode.dir";
    const DOC: &'static str = "Directory the oil buffer's editable listing represents. \
         Diff-on-:write applies filesystem ops relative to this \
         path; status line shows it.";
    const OWNER_MODE: &'static str = "oil-mode";
    fn describe(&self) -> String {
        self.0.display().to_string()
    }
}

/// Register every `lattice-oil`-owned mode against `registry`.
/// Called from the App's boot path alongside
/// `lattice_mode::register_foundation_modes`,
/// `lattice_syntax::register_language_modes`, and
/// `lattice_lsp::register_lsp_log_modes`. Mirrors the same
/// per-feature-crate registration pattern.
pub fn register_oil_modes(registry: &mut ModeRegistry) {
    registry.register(OilMode).expect("oil-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oil_mode_id_and_kind() {
        assert_eq!(OilMode.id(), OilMode::mode_id());
        assert_eq!(OilMode::mode_id().as_str(), "oil-mode");
        assert_eq!(OilMode.kind(), ModeKind::Major);
    }

    #[test]
    fn oil_dir_buffer_local_owner_mode_is_oil_mode() {
        assert_eq!(<OilDir as BufferLocal>::OWNER_MODE, "oil-mode");
        assert_eq!(<OilDir as BufferLocal>::NAME, "oil-mode.dir");
        let d = OilDir(PathBuf::from("/tmp/x"));
        assert_eq!(d.describe(), "/tmp/x");
    }

    #[test]
    fn register_oil_modes_populates_registry() {
        let mut registry = ModeRegistry::new();
        register_oil_modes(&mut registry);
        assert!(registry.is_registered(OilMode::mode_id()));
    }

    #[test]
    fn oil_mode_keymap_binds_navigation_and_open_chords() {
        use lattice_mode::Mode as _;
        let km = OilMode.keymap();
        // Assert by identity, not count: LM.3 added `<CR>` + the three
        // open-in-target chords beside `-`, and the set will keep growing.
        let bound: Vec<(&str, Option<&str>)> =
            km.entries.iter().map(|e| (e.chord, e.command)).collect();
        for (chord, cmd) in [
            ("-", "action:oil-navigate-up"),
            ("<CR>", "action:oil-follow"),
            ("<C-s>", "action:oil-follow-split"),
            ("<C-v>", "action:oil-follow-vsplit"),
            ("<C-t>", "action:oil-follow-tab"),
        ] {
            assert!(
                bound.contains(&(chord, Some(cmd))),
                "oil-mode must bind {chord} → {cmd}; got {bound:?}",
            );
        }
    }

    #[test]
    fn oil_mode_keymap_entry_chord_and_command() {
        use lattice_mode::Mode as _;
        let km = OilMode.keymap();
        let e = &km.entries[0];
        assert_eq!(e.chord, "-");
        assert_eq!(e.command, Some("action:oil-navigate-up"));
    }
}
