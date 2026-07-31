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
use lattice_protocol::position::Position;
use lattice_vcs::{Repository, Stash};

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline::{self, Field, MagitHeaderlineHandle};

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
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show this stash's patch", cmd: "action:magit-stash-show" },
        ]
    })
}

pub struct StashState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    /// MG.14: the buffer's headerline — the stash count, re-set from
    /// the same `build_stash_list` call that produced the list.
    headerline: Option<MagitHeaderlineHandle>,
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
                    // IX.2: drop the stash the prompt named. Stash
                    // indices RENUMBER — dropping or creating one
                    // shifts every later index — so re-reading the row
                    // after a refresh is how you drop the wrong stash.
                    let (idx, workdir) = {
                        let g = s.lock().ok()?;
                        let idx = match crate::confirm::carried_target(ctx)
                            .and_then(|t| t.parse::<usize>().ok())
                        {
                            Some(carried) => carried,
                            None => stash_index_at_cursor(&g, ctx.cursor)?,
                        };
                        (idx, g.workdir.clone())
                    };
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = Stash::drop(&repo, idx);
                        }
                    })
                }),
            },
            // MG.15: <CR> — open this stash's patch in its own buffer.
            // The one list view that had no `<CR>`, which made it the
            // last exception to MG.11's uniformity rule. Read-only and
            // non-mutating, so no confirm.
            ActionHandlerContribution {
                action_name: "action:magit-stash-show",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let idx = {
                        let g = s.lock().ok()?;
                        stash_index_at_cursor(&g, ctx.cursor)?
                    };
                    Some(Effect::OpenSyntheticBuffer {
                        name: crate::magit_stash_show_mode::buffer_name(idx),
                        mode_id: "magit-stash-show-mode".to_string(),
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
            let workdir = crate::workdir::magit_workdir().unwrap_or_default();
            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.14: install the headerline in the same synchronous
            // prefix as the state publish.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

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
                    headerline: hl.clone(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(StashView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            // Populate stash list: blocking I/O on spawn_blocking, then
            // apply edit on the current task (no Runtime::new()).
            let wd = workdir.clone();
            let (text, header) = tokio::task::spawn_blocking(move || build_stash_list(&wd))
                .await
                .unwrap();
            headerline::publish(&hl, header);
            let spans = crate::highlight::stash_styled_spans(&text);
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

/// `gr` — re-list stashes without a prior mutation.
fn refresh(s: Arc<Mutex<StashState>>) -> Option<Effect> {
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
        let (text, header) = tokio::task::spawn_blocking(move || build_stash_list(&wd))
            .await
            .unwrap_or_default();
        headerline::publish(&hl, header);
        let spans = crate::highlight::stash_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
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
        let (text, header) = tokio::task::spawn_blocking(move || build_stash_list(&wd))
            .await
            .unwrap_or_default();
        headerline::publish(&hl, header);
        let spans = crate::highlight::stash_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
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
    crate::confirm::ask_target(
        format!("Drop stash@{{{index}}}?"),
        "action:magit-stash-drop-execute",
        index.to_string(),
    )
}

/// One row of the stash list. The single writer of this format —
/// [`stash_index_at_cursor`] is its only reader, and
/// `highlight::stash_styled_spans` colours it by the same offsets.
pub fn list_row(index: usize, message: &str) -> String {
    format!("  stash@{{{index}}} {message}")
}

fn stash_index_at_cursor(state: &StashState, cursor: Position) -> Option<usize> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let line = snap.buffer.line(cursor.line)?;
    parse_index(&line)
}

/// `"  stash@{N} message"` → `N`. The reader half of [`list_row`],
/// split out as a free function so the round-trip between the two is
/// testable without a live buffer — the seam where they drifted apart
/// is what left every chord in this buffer dead.
pub fn parse_index(line: &str) -> Option<usize> {
    line.trim()
        .strip_prefix("stash@{")
        .and_then(|s| s.split('}').next())
        .and_then(|idx| idx.parse().ok())
}

/// Build the stash list AND its MG.14 header fields — the count comes
/// from the same `Stash::list` the body is formatted from.
fn build_stash_list(workdir: &std::path::Path) -> (String, Vec<Field>) {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return ("Not a git repository.\n".to_string(), Vec::new()),
    };
    let stashes = Stash::list(&repo).unwrap_or_default();
    let header = headerline::stash_fields(stashes.len());
    if stashes.is_empty() {
        return ("No stashes.\n".to_string(), header);
    }
    // MG.15: rows carry the `stash@{N}` label, matching the stash
    // entries magit-status already renders (`sections.rs`). This is a
    // BUG FIX, not cosmetics: `stash_index_at_cursor` has always
    // parsed `stash@{N}` out of the row, so while the list rendered a
    // bare message EVERY chord in this buffer — `a`, `p`, `d` — read
    // `None` and silently did nothing. Same failure class as MG.6's
    // dead `<CR>`: the reader and the writer of a line format drifted
    // apart with no test spanning them. `list_row` is now the one
    // writer and `stash_index_at_cursor` reads it back; a round-trip
    // test spans the pair.
    let mut out = format!("Stashes ({})\n", stashes.len());
    for s in &stashes {
        out.push_str(&list_row(s.index, &s.message));
        out.push('\n');
    }
    out.push('\n');
    (out, header)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.15 — the regression guard for a bug that made EVERY chord in
    /// this buffer dead.
    ///
    /// `stash_index_at_cursor` has always parsed `stash@{N}` out of the
    /// row under the cursor, but `build_stash_list` rendered a bare
    /// `  <message>`. So `a` (apply), `p` (pop) and `d` (drop) all
    /// resolved `None` and silently did nothing — no error, no effect,
    /// indistinguishable from an unbound key. Nothing caught it because
    /// the writer and the reader were only ever tested apart.
    ///
    /// This spans them: whatever `list_row` writes, `parse_index` must
    /// read back.
    #[test]
    fn every_row_the_list_writes_parses_back_to_its_own_index() {
        for (index, message) in [
            (0usize, "WIP on main: 1234abc a message"),
            (7, "On feature/x: another"),
            (12, ""),
        ] {
            let row = list_row(index, message);
            assert_eq!(
                parse_index(&row),
                Some(index),
                "the chord handlers read the row the list writes; a \
                 format the parser cannot read leaves a/p/d/<CR> dead: {row:?}"
            );
        }
    }

    /// The inverse, so the guard above cannot pass vacuously: the
    /// pre-MG.15 format (bare message, no label) is exactly what the
    /// parser cannot read.
    #[test]
    fn the_old_unlabelled_row_format_is_unparseable() {
        assert_eq!(parse_index("  WIP on main: 1234abc a message"), None);
    }

    /// The list header and blank separator are not stash rows — a
    /// chord fired on either must decline rather than act on stash 0.
    #[test]
    fn non_row_lines_carry_no_index() {
        assert_eq!(parse_index("Stashes (3)"), None);
        assert_eq!(parse_index(""), None);
        assert_eq!(parse_index("No stashes."), None);
    }

    /// MG.15: `<CR>` targets the detail buffer for the stash at the
    /// cursor. Asserted through the same name builder the mode's own
    /// parser round-trips (see `magit_stash_show_mode`), so the two
    /// halves cannot drift the way the list format did.
    #[test]
    fn enter_opens_the_detail_buffer_for_the_row_under_the_cursor() {
        let row = list_row(3, "WIP on main: deadbee something");
        let index = parse_index(&row).expect("a list row carries its index");
        assert_eq!(
            crate::magit_stash_show_mode::buffer_name(index),
            "*magit:stash:3*"
        );
    }

    /// MG.12: `d` used to call `Stash::drop` straight from the chord,
    /// while magit-status's `x` on the same class of act asked first.
    #[test]
    fn drop_asks_before_dropping_and_names_the_stash() {
        match drop_stash_confirm(2) {
            Effect::Confirm {
                prompt,
                yes_action,
                args,
            } => {
                assert_eq!(prompt, "Drop stash@{2}?");
                assert_eq!(yes_action, "action:magit-stash-drop-execute");
            }
            other => panic!("expected a confirm before dropping a stash, got {other:?}"),
        }
    }

    /// The prompt names the stash with the same `stash@{N}` ref the
    /// list row shows, so the question matches what is on screen
    /// behind the transient.
    ///
    /// MG.15 note: this test used to build `row` as a hand-written
    /// string literal — which is precisely how the bug below survived.
    /// It asserted against the format the author *believed* the list
    /// rendered, and the list rendered something else. It now calls
    /// [`list_row`], the real writer.
    #[test]
    fn drop_prompt_uses_the_same_ref_form_the_list_row_shows() {
        let index = 0;
        let row = list_row(index, "WIP on main: 1234abc msg");
        match drop_stash_confirm(index) {
            Effect::Confirm { prompt, .. } => {
                let stash_ref = format!("stash@{{{index}}}");
                assert!(row.contains(&stash_ref) && prompt.contains(&stash_ref));
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }
}
