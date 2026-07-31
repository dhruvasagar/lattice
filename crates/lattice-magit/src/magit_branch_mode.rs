//! MG.9: magit-branch major mode.
//!
//! Lists local branches with checkout/create/delete/merge operations.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::position::Position;
use lattice_vcs::{Branch, Repository};

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline::{self, Field, MagitHeaderlineHandle};

pub struct MagitBranchMode;

impl MagitBranchMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-branch-mode")
    }
}

fn magit_branch_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Checkout branch", cmd: "action:magit-branch-checkout" },
            keymap_entry! { mode: Normal, chord: "c", doc: "Create branch", cmd: "action:magit-branch-create" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Delete branch", cmd: "action:magit-branch-delete" },
            keymap_entry! { mode: Normal, chord: "m", doc: "Merge branch", cmd: "action:magit-branch-merge" },
        ]
    })
}

pub struct BranchState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    /// MG.14: the buffer's headerline — current branch + total,
    /// re-set from the same `build_branch_list` call that produced
    /// the list itself.
    headerline: Option<MagitHeaderlineHandle>,
}

/// MG.13: service alias for this mode's per-buffer state. Register
/// and look up through this exact type — `ServiceRegistry` keys on
/// `TypeId`, so `Arc<BufferStates<BranchState>>` and
/// `BufferStates<BranchState>` are different slots
/// (`feedback_servicesregistry_arc_typeid`).
pub type BranchStatesHandle = Arc<BufferStates<BranchState>>;

/// Resolve this mode's state for the buffer an action fired in.
/// `None` means no magit-branch buffer is live there — the handler
/// no-ops, exactly as it did when it wasn't registered at all.
fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<BranchState>>> {
    crate::buffer_state::state_for::<BranchState>(ctx)
}

/// `gr` for a branch buffer. `magit-core-mode` owns the chord and the
/// single boot-registered handler; this supplies the body for buffers
/// this mode owns (see [`MagitView`] for why the shared action cannot
/// be registered per mode).
struct BranchView(Arc<Mutex<BranchState>>);

impl MagitView for BranchView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }
}

impl Mode for MagitBranchMode {
    type Guard = BufferStateGuard<BranchState>;

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
        Keymap::from_entries(magit_branch_keymap_entries())
    }

    /// MG.13: registered once at boot, not per activation. Each body
    /// resolves its per-buffer state through [`state`] at call time,
    /// so there is no window in which the chord resolves but no
    /// handler exists. See `buffer_state`'s module docs.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // checkout (<CR>)
            ActionHandlerContribution {
                action_name: "action:magit-branch-checkout",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (name, workdir) = {
                        let g = s.lock().ok()?;
                        (branch_name_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                    };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = Branch::checkout(&repo, &name);
                        }
                    })
                }),
            },
            // delete (d) — MG.12: `Branch::delete` is a force delete
            // (`-D`), which silently discards unmerged commits, so it
            // asks first. This half does no git call at all; answering
            // `n` simply never reaches the execute half below.
            ActionHandlerContribution {
                action_name: "action:magit-branch-delete",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let name = {
                        let g = s.lock().ok()?;
                        branch_name_at_cursor(&g, ctx.cursor)?
                    };
                    Some(delete_branch_confirm(&name))
                }),
            },
            // delete, after confirmation. Re-reads the branch at the
            // cursor rather than carrying it through the prompt: the
            // confirm transient owns every keystroke while it is open,
            // so the cursor cannot have moved (`do_transient_trigger`
            // hands the yes-action the *document* cursor). Same shape
            // as magit-status's `magit-discard-execute`.
            ActionHandlerContribution {
                action_name: "action:magit-branch-delete-execute",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    // IX.2: act on the branch the prompt named. The
                    // cursor is only consulted when nothing was carried
                    // — a refresh can rebuild the list while the dialog
                    // is open, and then the row means a different branch.
                    let (name, workdir) = {
                        let g = s.lock().ok()?;
                        let name = match crate::confirm::carried_target(ctx) {
                            Some(carried) => carried,
                            None => branch_name_at_cursor(&g, ctx.cursor)?,
                        };
                        (name, g.workdir.clone())
                    };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = Branch::delete(&repo, &name);
                        }
                    })
                }),
            },
            // merge (m)
            ActionHandlerContribution {
                action_name: "action:magit-branch-merge",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (name, workdir) = {
                        let g = s.lock().ok()?;
                        (branch_name_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                    };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = repo.run_git(["merge", &name]);
                        }
                    })
                }),
            },
            // create (c) — Emacs-magit-style two-step wizard: pick an
            // existing branch as the base via the picker, then a
            // follow-up prompt asks for the new branch's name (see
            // `picker_sources::BranchPickBaseSource` +
            // `action:magit-branch-create-finish` in
            // `magit_global_mode`). The direct `:magit-branch-create
            // <name>` ex-command (creates from HEAD, no base choice)
            // stays available for the scriptable/quick path.
            //
            // State-free, but still gated: `state(ctx)?` keeps it from
            // firing in a buffer that is not a magit-branch buffer.
            ActionHandlerContribution {
                action_name: "action:magit-branch-create",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let _ = state(ctx)?;
                    Some(Effect::OpenPicker {
                        source: "magit-branch-pick-base".to_string(),
                        args: Vec::new(),
                    })
                }),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            // Nothing to publish without a store or a live handle —
            // hand back a guard over an orphan registry so the
            // lifecycle contract (always a fresh Guard) still holds.
            let orphan = || BufferStateGuard::new(Arc::new(BufferStates::default()), buffer_id);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(orphan());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(orphan());
            };
            let workdir = crate::workdir::magit_workdir().unwrap_or_default();
            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.14: install the headerline in the same synchronous
            // prefix as the state publish; it stays hidden until the
            // branch list below lands.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: publish BEFORE the first `.await`. `spawn_cascade`
            // polls this future once synchronously on the App thread
            // before spawning it, so everything above the first await
            // has run by the time `activate_major` returns — which is
            // what makes the boot-registered handlers above able to
            // find their state on the very next keystroke. Moving any
            // `.await` above this line reopens the dead-chord window.
            let Some(states) = ctx.service::<BranchStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                BranchState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    pending_highlights: pending_highlights.clone(),
                    headerline: hl.clone(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(BranchView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            // Populate branch list: blocking I/O on spawn_blocking, then
            // apply edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let (text, header) = tokio::task::spawn_blocking(move || build_branch_list(&wd))
                .await
                .unwrap();
            headerline::publish(&hl, header);
            let spans = crate::highlight::branch_styled_spans(&text);
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

/// `gr` — re-list branches without a prior mutation.
fn refresh(s: Arc<Mutex<BranchState>>) -> Option<Effect> {
    let (handle, wd, pending, buffer_id, hl) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
            g.headerline.clone(),
        )
    };
    tokio::task::spawn(async move {
        let (text, header) = tokio::task::spawn_blocking(move || build_branch_list(&wd))
            .await
            .unwrap_or_default();
        headerline::publish(&hl, header);
        let spans = crate::highlight::branch_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Run `mutate` (a blocking git call) on `spawn_blocking`, off the
/// actor thread, then re-list branches — the shape every mutating
/// handler above uses instead of calling git synchronously inline.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<BranchState>>,
    mutate: impl FnOnce() + Send + 'static,
) -> Option<Effect> {
    let (handle, wd, pending, buffer_id, hl) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
            g.headerline.clone(),
        )
    };
    tokio::task::spawn(async move {
        let _ = tokio::task::spawn_blocking(mutate).await;
        let (text, header) = tokio::task::spawn_blocking(move || build_branch_list(&wd))
            .await
            .unwrap_or_default();
        headerline::publish(&hl, header);
        let spans = crate::highlight::branch_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// MG.12: the ask half of `d`. Names the branch so the question is
/// answerable while the confirm transient covers the branch list.
fn delete_branch_confirm(name: &str) -> Effect {
    crate::confirm::ask_target(
        format!("Delete branch {name}?"),
        "action:magit-branch-delete-execute",
        name,
    )
}

fn branch_name_at_cursor(state: &BranchState, cursor: Position) -> Option<String> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let line = snap.buffer.line(cursor.line)?;
    // Format: "  branch-name" or "* branch-name (current)"
    let name = line
        .trim()
        .trim_start_matches("* ")
        .split_whitespace()
        .next()?;
    Some(name.to_string())
}

/// Build the branch list AND its MG.14 header fields. One pass: the
/// header's "current branch, N branches" comes from the same
/// `Branch::list` + `rev-parse` this call already made, so the row
/// costs no git of its own.
fn build_branch_list(workdir: &std::path::Path) -> (String, Vec<Field>) {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return ("Not a git repository.\n".to_string(), Vec::new()),
    };
    let branches = Branch::list(&repo).unwrap_or_default();

    // Determine current branch
    let current = repo
        .run_git_str(["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let header = headerline::branch_fields(&current, branches.len());

    if branches.is_empty() {
        return ("No branches.\n".to_string(), header);
    }

    let mut out = format!("Branches ({})\n", branches.len());
    for b in &branches {
        let marker = if *b == current { "* " } else { "  " };
        out.push_str(&format!("{}{}\n", marker, b));
    }
    out.push('\n');
    (out, header)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.12: `d` used to call `Branch::delete` — a force delete —
    /// straight from the chord. It must now produce nothing but a
    /// question; the git call moved behind the yes-action.
    #[test]
    fn delete_asks_before_deleting_and_names_the_branch() {
        match delete_branch_confirm("feature/foo") {
            Effect::Confirm {
                prompt,
                yes_action,
                args,
            } => {
                assert_eq!(prompt, "Delete branch feature/foo?");
                assert_eq!(yes_action, "action:magit-branch-delete-execute");
            }
            other => panic!("expected a confirm before a force delete, got {other:?}"),
        }
    }

    /// The prompt has to survive branch names with slashes and dots
    /// intact — a truncated name makes the question unanswerable.
    #[test]
    fn delete_prompt_preserves_the_full_branch_name() {
        match delete_branch_confirm("release/v1.2.3-rc.1") {
            Effect::Confirm { prompt, .. } => {
                assert!(
                    prompt.contains("release/v1.2.3-rc.1"),
                    "prompt lost the branch name: {prompt}"
                );
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }
}
