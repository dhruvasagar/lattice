//! MG.2: magit-status major mode definition.
//!
//! Creates the `*magit:status*` buffer, spawns a refresh task on
//! `spawn_blocking` that runs `git status` / `git stash list` /
//! `git log`, formats the section index, and applies it as buffer
//! content. Diffs load on demand via `=` (MG.3).

use std::sync::OnceLock;

use lattice_config;
use lattice_grammar::CommandRegistryHandle;
use lattice_mode::{
    ActionHandlerRegistryHandle, BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry,
    LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};
use lattice_vcs::Repository;

use crate::actions;
use crate::refresh;

/// Major mode for the `*magit:status*` buffer.
pub struct MagitStatusMode;

impl MagitStatusMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-status-mode")
    }
}

/// Static keymap for `magit-status-mode`.
///
/// Stubs only — real handler bodies land in MG.3. The host's
/// `translate_mode_keymaps` pass auto-pushes these as a
/// `MajorMode(magit-status-mode)` layer.
fn magit_status_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "s",
                doc: "Stage hunk or file at cursor",
                cmd: "action:magit-stage"
            },
            keymap_entry! {
                mode: Normal, chord: "u",
                doc: "Unstage hunk or file at cursor",
                cmd: "action:magit-unstage"
            },
            keymap_entry! {
                mode: Normal, chord: "x",
                doc: "Discard hunk or file at cursor",
                cmd: "action:magit-discard"
            },
            keymap_entry! {
                mode: Normal, chord: "cc",
                doc: "Open commit buffer",
                cmd: "action:magit-commit"
            },
            keymap_entry! {
                mode: Normal, chord: "ca",
                doc: "Amend previous commit",
                cmd: "action:magit-commit-amend"
            },
            keymap_entry! {
                mode: Normal, chord: "=",
                doc: "Toggle inline diff at cursor",
                cmd: "action:magit-toggle-diff"
            },
            keymap_entry! {
                mode: Normal, chord: "p",
                doc: "Stage hunk interactively (git add -p)",
                cmd: "action:magit-stage-patch"
            },
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "Context-aware open/visit at cursor",
                cmd: "action:magit-visit"
            },
        ]
    })
}

/// MG.3: RAII guard holding per-buffer action handler registrations
/// and shared state the handlers read at chord-press time.
#[derive(Default)]
pub struct MagitStatusGuard {
    _action_handler_registrations: Vec<lattice_mode::ActionHandlerRegistration>,
    _state: Option<std::sync::Arc<std::sync::Mutex<actions::StatusBufferState>>>,
}

impl Mode for MagitStatusMode {
    type Guard = MagitStatusGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None // plain Document — no kind-specific logic
    }

    fn options(&self) -> OptionOverrideSet {
        // Read-only synthetic buffer. Owner writes (mgit-status
        // refresh) go through apply_edit_batch which bypasses the
        // read-only gate at the dispatcher's Insert/operator path.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
            lattice_config::SignColumnOption = lattice_config::SignColumn::Yes,
            lattice_config::CursorLine = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_status_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(MagitStatusGuard::default());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(MagitStatusGuard::default());
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(MagitStatusGuard::default());
            };

            // Discover git repo from current directory
            let workdir = match Repository::discover(".").ok().and_then(|r| r.workdir().map(|p| p.to_path_buf())) {
                Some(w) => w,
                None => {
                    // Write "not a git repository" message into buffer
                    let _ = handle
                        .apply_edit_batch(vec![lattice_protocol::edit::Edit::replace(
                            lattice_protocol::position::Range::new(
                                lattice_protocol::position::Position::ZERO,
                                lattice_protocol::position::Position::new(0, 0),
                            ),
                            "Not a git repository.\n".to_string(),
                        )])
                        .await;
                    return Ok(MagitStatusGuard::default());
                }
            };

            tracing::debug!(
                target: "lattice_magit",
                "magit-status-mode activated on buffer {buffer_id:?}, workdir={workdir:?}",
            );

            // Spawn the refresh on spawn_blocking
            let handle_clone = handle.clone();
            let wd = workdir.clone();
            runtime.spawn_blocking(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(refresh::refresh_status(buffer_id, handle_clone, wd));
            });

            // MG.3: register per-buffer action handlers
            let shared_state = std::sync::Arc::new(std::sync::Mutex::new(
                actions::StatusBufferState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                },
            ));

            let mut action_registrations = Vec::new();
            if let (Some(cmd_registry_arc), Some(action_handlers_arc)) = (
                ctx.service::<CommandRegistryHandle>(),
                ctx.service::<ActionHandlerRegistryHandle>(),
            ) {
                action_registrations = actions::register_action_handlers(
                    shared_state.clone(),
                    &cmd_registry_arc,
                    &action_handlers_arc,
                );
            }

            Ok(MagitStatusGuard {
                _action_handler_registrations: action_registrations,
                _state: Some(shared_state),
            })
        })
    }
}
