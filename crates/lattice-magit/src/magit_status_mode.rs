//! MG.2-3: magit-status major mode.
//!
//! Creates the `*magit:status*` buffer, spawns a refresh task on
//! `spawn_blocking`, registers per-buffer action handlers.

use std::sync::OnceLock;

use lattice_config;
use lattice_core::FoldOverlayServiceHandle;
use lattice_mode::{
    BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};
use lattice_vcs::Repository;

use crate::actions;
use crate::fold_source::MagitStatusFoldSource;
use crate::refresh;

pub struct MagitStatusMode;

impl MagitStatusMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-status-mode")
    }
}

fn magit_status_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "s", doc: "Stage hunk or file at cursor", cmd: "action:magit-stage" },
            keymap_entry! { mode: Normal, chord: "u", doc: "Unstage hunk or file at cursor", cmd: "action:magit-unstage" },
            keymap_entry! { mode: Normal, chord: "x", doc: "Discard hunk or file at cursor", cmd: "action:magit-discard" },
            keymap_entry! { mode: Normal, chord: "cc", doc: "Open commit buffer", cmd: "action:magit-commit" },
            keymap_entry! { mode: Normal, chord: "ca", doc: "Amend previous commit", cmd: "action:magit-commit-amend" },
            keymap_entry! { mode: Normal, chord: "=", doc: "Toggle inline diff at cursor", cmd: "action:magit-toggle-diff" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Open file diff in a dedicated buffer", cmd: "action:magit-diff-file" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Stage hunk interactively", cmd: "action:magit-stage-patch" },
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Context-aware open/visit at cursor", cmd: "action:magit-visit" },
        ]
    })
}

#[derive(Default)]
pub struct MagitStatusGuard {
    /// MG.13: unpublishes this buffer's state on deactivation.
    states: Option<(actions::StatusStatesHandle, lattice_core::BufferId)>,
    _state: Option<std::sync::Arc<std::sync::Mutex<actions::StatusBufferState>>>,
    /// Fold-audit fix: deregisters the buffer's `MagitStatusFoldSource`
    /// on deactivation — same Drop-based lifecycle
    /// `DiffModeGuard`/`MultibufferModeGuard` use.
    fold_registration: Option<(FoldOverlayServiceHandle, lattice_core::ProviderId)>,
    /// MG.13: unpublishes this buffer's `MagitView` on deactivation so
    /// `gr` cannot resolve against a dead status buffer.
    views: Option<(
        crate::buffer_state::MagitViewsHandle,
        lattice_core::BufferId,
    )>,
    /// MG.14: the headerline provider registration. Its own `Drop`
    /// unregisters the sticky row when the mode deactivates.
    _headerline: Option<crate::headerline::HeaderlineRegistration>,
}

impl Drop for MagitStatusGuard {
    fn drop(&mut self) {
        if let Some((states, buffer)) = self.states.take() {
            states.remove(buffer);
        }
        if let Some((views, buffer)) = self.views.take() {
            views.remove(buffer);
        }
        if let Some((svc, id)) = self.fold_registration.take() {
            svc.remove_source(id);
        }
    }
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
        None
    }

    fn options(&self) -> OptionOverrideSet {
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

    /// MG.13: registered once at boot; see `actions::status_action_handlers`.
    fn action_handlers(&self) -> Vec<lattice_mode::ActionHandlerContribution> {
        actions::status_action_handlers()
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

            let workdir = match Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
            {
                Some(w) => w,
                None => {
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

            let pending = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.14: the header this buffer never had — branch,
            // ahead/behind, repo, dirty counts. Installed in the same
            // synchronous prefix as the state publish; it renders
            // nothing until the first refresh below lands.
            let (hl, hl_registration) =
                match crate::headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: build and publish BEFORE the initial refresh's
            // `.await`. Every field here is synchronous, so this lands
            // during `spawn_cascade`'s first synchronous poll — before
            // `activate_major` returns — which is what makes the
            // boot-registered handlers reachable on the very next
            // keystroke. Publishing after the refresh below would leave
            // `x` / `=` / `<CR>` dead for the width of a `git status`,
            // which is the longest window of any magit view.
            let shared_state =
                std::sync::Arc::new(std::sync::Mutex::new(actions::StatusBufferState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    runtime: runtime.clone(),
                    pending_highlights: pending.clone(),
                    expanded: std::collections::HashMap::new(),
                    headerline: hl.clone(),
                    pending_cursor: None,
                    // MG.18d: the wake-baked bus a post-mutation cursor
                    // goes back on. `None` in a harness without the
                    // service — the refresh still works, it just does
                    // not move the cursor.
                    cursor_bus: ctx
                        .service::<crate::cursor_restore::CursorBusHandle>()
                        .map(|outer| (*outer).clone()),
                }));
            if let Some(states) = ctx.service::<actions::StatusStatesHandle>() {
                states.publish_shared(buffer_id, shared_state.clone());
            }

            // Initial refresh: blocking I/O on spawn_blocking, then async
            // edit apply + highlights on the current task.
            {
                let wd = workdir.clone();
                // Nothing is expanded on a fresh buffer, so the
                // open-entry set is empty and the rebuilt bookkeeping
                // it returns is too.
                let (text, spans, header, _) = tokio::task::spawn_blocking(move || {
                    refresh::build_and_format(&wd, &std::collections::HashSet::new())
                })
                .await
                .expect("spawn_blocking");
                crate::headerline::publish(&hl, header);
                refresh::apply_and_highlight(
                    handle.clone(),
                    text,
                    spans,
                    pending.clone(),
                    buffer_id,
                )
                .await;
            }

            // Fold-audit fix: register the nested file>hunk fold
            // source for this buffer's inline expansions.
            let fold_registration = ctx
                .service::<FoldOverlayServiceHandle>()
                .map(|outer| (*outer).clone())
                .map(|svc| {
                    let source = std::sync::Arc::new(MagitStatusFoldSource::new(
                        shared_state.clone(),
                        buffer_id,
                    ));
                    let id = svc.add_source(source, buffer_id);
                    (svc, id)
                });

            // MG.13: publish this buffer's `MagitView` so `gr` — now
            // registered once at boot by `magit-core-mode` — reaches
            // the status buffer's own refresh body. See
            // `buffer_state::MagitView`.
            let views = ctx
                .service::<crate::buffer_state::MagitViewsHandle>()
                .map(|v| (*v).clone());
            if let Some(ref v) = views {
                v.publish(
                    buffer_id,
                    std::sync::Arc::new(actions::StatusView(shared_state.clone())),
                );
            }

            Ok(MagitStatusGuard {
                states: ctx
                    .service::<actions::StatusStatesHandle>()
                    .map(|st| ((*st).clone(), buffer_id)),
                _state: Some(shared_state),
                fold_registration,
                views: views.map(|v| (v, buffer_id)),
                _headerline: hl_registration,
            })
        })
    }
}
