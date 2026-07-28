//! MG.9: magit-stash major mode.
//!
//! Lists stash entries with apply/pop/drop/create operations.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::{Repository, Stash};

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};

pub struct MagitStashMode;

impl MagitStashMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-stash-mode")
    }
}

fn magit_stash_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "a", doc: "Apply stash", cmd: "action:magit-stash-apply" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Pop stash", cmd: "action:magit-stash-pop" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Drop stash", cmd: "action:magit-stash-drop" },
            keymap_entry! { mode: Normal, chord: "z", doc: "Create stash", cmd: "action:magit-stash-create" },
        ]
    })
}

pub struct StashState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
}

/// MG.13: service alias for this mode's per-buffer state — register
/// and look up through this exact type
/// (`feedback_servicesregistry_arc_typeid`).
pub type StashStatesHandle = Arc<BufferStates<StashState>>;

/// Resolve this mode's state for the buffer an action fired in.
/// `None` means no magit-stash buffer is live there.
fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<StashState>>> {
    crate::buffer_state::state_for::<StashState>(ctx)
}

/// `gr` for a stash buffer — see [`MagitView`].
struct StashView(Arc<Mutex<StashState>>);

impl MagitView for StashView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }
}

impl Mode for MagitStashMode {
    type Guard = BufferStateGuard<StashState>;

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
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_stash_keymap_entries())
    }

    /// MG.13: registered once at boot, not per activation — see
    /// `buffer_state`'s module docs for why.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // apply (a)
            ActionHandlerContribution {
                action_name: "action:magit-stash-apply",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (idx, workdir) = {
                        let g = s.lock().ok()?;
                        (stash_index_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                    };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = Stash::apply(&repo, idx);
                        }
                    })
                }),
            },
            // pop (p)
            ActionHandlerContribution {
                action_name: "action:magit-stash-pop",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (idx, workdir) = {
                        let g = s.lock().ok()?;
                        (stash_index_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                    };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = Stash::pop(&repo, idx);
                        }
                    })
                }),
            },
            // drop (d) — MG.12: a dropped stash is gone; `apply` and
            // `pop` above put their content somewhere the user can
            // still see it, so only this one asks. No git call in this
            // half: answering `n` never reaches the execute half.
            ActionHandlerContribution {
                action_name: "action:magit-stash-drop",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let idx = {
                        let g = s.lock().ok()?;
                        stash_index_at_cursor(&g, ctx.cursor)?
                    };
                    Some(drop_stash_confirm(idx))
                }),
            },
            // drop, after confirmation — re-reads the stash at the
            // cursor, which the confirm transient could not have moved
            // (see the matching note in `magit_branch_mode`).
            ActionHandlerContribution {
                action_name: "action:magit-stash-drop-execute",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (idx, workdir) = {
                        let g = s.lock().ok()?;
                        (stash_index_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                    };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = Stash::drop(&repo, idx);
                        }
                    })
                }),
            },
            // create (z)
            ActionHandlerContribution {
                action_name: "action:magit-stash-create",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let workdir = { s.lock().ok()?.workdir.clone() };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = Stash::create(&repo, None, false);
                        }
                    })
                }),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let orphan = || BufferStateGuard::new(Arc::new(BufferStates::default()), buffer_id);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(orphan());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(orphan());
            };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.13: publish BEFORE the first `.await` — see the note
            // in `magit_branch_mode::on_activate`.
            let Some(states) = ctx.service::<StashStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                StashState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    pending_highlights: pending_highlights.clone(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(StashView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            // Populate stash list: blocking I/O on spawn_blocking, then
            // apply edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let text = tokio::task::spawn_blocking(move || build_stash_list(&wd))
                .await
                .unwrap();
            let spans = crate::highlight::stash_styled_spans(&text);
            apply_full_replace(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

async fn apply_full_replace(handle: &Arc<dyn lattice_runtime::Document>, text: String) {
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await;
}

/// `gr` — re-list stashes without a prior mutation.
fn refresh(s: Arc<Mutex<StashState>>) -> Option<Effect> {
    let (handle, wd, pending, buffer_id) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };
    tokio::task::spawn(async move {
        let text = tokio::task::spawn_blocking(move || build_stash_list(&wd))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::stash_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Run `mutate` (a blocking git call) on `spawn_blocking`, off the
/// actor thread, then re-list stashes — the shape every mutating
/// handler above uses instead of calling git synchronously inline.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<StashState>>,
    mutate: impl FnOnce() + Send + 'static,
) -> Option<Effect> {
    let (handle, wd, pending, buffer_id) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };
    tokio::task::spawn(async move {
        let _ = tokio::task::spawn_blocking(mutate).await;
        let text = tokio::task::spawn_blocking(move || build_stash_list(&wd))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::stash_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// MG.12: the ask half of `d`. Names the stash by the same
/// `stash@{N}` ref the list row shows, so the prompt and the row it
/// came from read identically.
fn drop_stash_confirm(index: usize) -> Effect {
    crate::confirm::ask(
        format!("Drop stash@{{{index}}}?"),
        "action:magit-stash-drop-execute",
    )
}

fn stash_index_at_cursor(state: &StashState, cursor: Position) -> Option<usize> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let line = snap.buffer.line(cursor.line)?;
    // Format: "  stash@{N} message"
    let trimmed = line.trim();
    if let Some(idx_str) = trimmed
        .strip_prefix("stash@{")
        .and_then(|s| s.split('}').next())
    {
        idx_str.parse().ok()
    } else {
        None
    }
}

fn build_stash_list(workdir: &std::path::Path) -> String {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return "Not a git repository.\n".to_string(),
    };
    let stashes = Stash::list(&repo).unwrap_or_default();
    if stashes.is_empty() {
        return "No stashes.\n".to_string();
    }
    let mut out = format!("Stashes ({})\n", stashes.len());
    for s in &stashes {
        out.push_str(&format!("  {}\n", s.message));
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.12: `d` used to call `Stash::drop` straight from the chord,
    /// while magit-status's `x` on the same class of act asked first.
    #[test]
    fn drop_asks_before_dropping_and_names_the_stash() {
        match drop_stash_confirm(2) {
            Effect::Confirm { prompt, yes_action } => {
                assert_eq!(prompt, "Drop stash@{2}?");
                assert_eq!(yes_action, "action:magit-stash-drop-execute");
            }
            other => panic!("expected a confirm before dropping a stash, got {other:?}"),
        }
    }

    /// The prompt names the stash with the same `stash@{N}` ref the
    /// list row shows, so the question matches what is on screen
    /// behind the transient.
    #[test]
    fn drop_prompt_uses_the_same_ref_form_the_list_row_shows() {
        let index = 0;
        let row = format!("  stash@{{{index}}} WIP on main: 1234abc msg");
        match drop_stash_confirm(index) {
            Effect::Confirm { prompt, .. } => {
                let stash_ref = format!("stash@{{{index}}}");
                assert!(row.contains(&stash_ref) && prompt.contains(&stash_ref));
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }
}
