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

/// The stash a chord acts on, or the picker to ask with.
///
/// Resolution order is the whole fix. Every magit buffer that shows
/// stashes answers `stash_at_cursor` for its own rows, so the cursor
/// wins wherever it lands one — the stash list AND magit-status's
/// Stashes section. With nothing under the cursor the row *asks*
/// rather than dying silently, which is where MG.23j landed `A` / `_`
/// / `O` for exactly the same reason: the dispatch menu can be opened
/// from a buffer with no stash in it at all.
enum StashTarget {
    At(usize),
    Ask(Effect),
}

fn stash_target(ctx: &ActionContext<'_>, ex_command: &str) -> StashTarget {
    match crate::buffer_state::view_for(ctx).and_then(|v| v.stash_at_cursor(ctx.cursor)) {
        Some(idx) => StashTarget::At(idx),
        None => StashTarget::Ask(Effect::OpenPicker {
            source: crate::picker_sources::STASH_PICK_SOURCE.to_string(),
            args: vec![ex_command.to_string()],
        }),
    }
}

/// A `lattice_vcs::Stash` mutation, as data — so the three that share
/// [`run_on_stash`]'s body differ by one function pointer rather than
/// by a copy of it.
type StashOp = fn(&Repository, usize) -> lattice_vcs::Result<()>;

/// Run `op` on `stash@{idx}`, refreshing in place when we own the
/// buffer.
///
/// Two paths, deliberately: inside the stash list there is a
/// `StashState` to rebuild, so the list updates itself. From anywhere
/// else — magit-status, or a dispatch menu over an ordinary file —
/// there is nothing of ours to rebuild, so the operation reports by
/// notification and `gr` refreshes, which is exactly what
/// `spawn_commit_op` does for the commit ops fired from any buffer.
fn run_on_stash(
    ctx: &ActionContext<'_>,
    idx: usize,
    verb: &'static str,
    op: StashOp,
) -> Option<Effect> {
    if let Some(s) = state(ctx) {
        let workdir = { s.lock().ok()?.workdir.clone() };
        return spawn_mutation_and_refresh(s, format!("{verb} stash@{{{idx}}}"), move || {
            let repo =
                Repository::discover(&workdir).map_err(|e| format!("not a git repository: {e}"))?;
            op(&repo, idx)
                .map(|_| String::new())
                .map_err(|e| e.to_string())
        });
    }
    Some(crate::magit_global_mode::spawn_git(
        vec![
            "stash".to_string(),
            verb.to_string(),
            format!("stash@{{{idx}}}"),
        ],
        verb,
    ))
}

/// `gr` for a stash buffer — see [`MagitView`].
struct StashView(Arc<Mutex<StashState>>);

impl MagitView for StashView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }

    /// Every row in this buffer is a stash, so the cursor line is the
    /// whole answer.
    fn stash_at_cursor(&self, cursor: Position) -> Option<usize> {
        let g = self.0.lock().ok()?;
        stash_index_at_cursor(&g, cursor)
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
                    match stash_target(ctx, "magit-stash-apply") {
                        StashTarget::At(idx) => run_on_stash(ctx, idx, "apply", Stash::apply),
                        StashTarget::Ask(effect) => Some(effect),
                    }
                }),
            },
            // pop (p)
            ActionHandlerContribution {
                action_name: "action:magit-stash-pop",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    match stash_target(ctx, "magit-stash-pop") {
                        StashTarget::At(idx) => run_on_stash(ctx, idx, "pop", Stash::pop),
                        StashTarget::Ask(effect) => Some(effect),
                    }
                }),
            },
            // drop (d) — MG.12: a dropped stash is gone; `apply` and
            // `pop` above put their content somewhere the user can
            // still see it, so only this one asks. No git call in this
            // half: answering `n` never reaches the execute half.
            ActionHandlerContribution {
                action_name: "action:magit-stash-drop",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    match stash_target(ctx, "magit-stash-drop") {
                        StashTarget::At(idx) => Some(drop_stash_confirm(idx)),
                        StashTarget::Ask(effect) => Some(effect),
                    }
                }),
            },
            // drop, after confirmation — re-reads the stash at the
            // cursor, which the confirm transient could not have moved
            // (see the matching note in `magit_branch_mode`).
            ActionHandlerContribution {
                action_name: "action:magit-stash-drop-execute",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    // IX.2: drop the stash the prompt named. Stash
                    // indices RENUMBER — dropping or creating one
                    // shifts every later index — so re-reading the row
                    // after a refresh is how you drop the wrong stash.
                    //
                    // The carried target is read BEFORE any view
                    // lookup, and that ordering is load-bearing here in
                    // a way it was not before: the picker path fires
                    // this half from a buffer with no stash under the
                    // cursor at all, so a cursor re-read would find
                    // nothing and silently drop none.
                    let idx = match crate::confirm::carried_target(ctx)
                        .and_then(|t| t.parse::<usize>().ok())
                    {
                        Some(carried) => carried,
                        None => match stash_target(ctx, "magit-stash-drop") {
                            StashTarget::At(idx) => idx,
                            StashTarget::Ask(effect) => return Some(effect),
                        },
                    };
                    run_on_stash(ctx, idx, "drop", Stash::drop)
                }),
            },
            // MG.15: <CR> — open this stash's patch in its own buffer.
            // The one list view that had no `<CR>`, which made it the
            // last exception to MG.11's uniformity rule. Read-only and
            // non-mutating, so no confirm.
            ActionHandlerContribution {
                action_name: "action:magit-stash-show",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let idx = match stash_target(ctx, "magit-stash-show") {
                        StashTarget::At(idx) => idx,
                        StashTarget::Ask(effect) => return Some(effect),
                    };
                    Some(crate::magit_global_mode::open_repo_view_from_action_with(
                        ctx,
                        "stash",
                        "magit-stash-show-mode",
                        Some(&crate::magit_stash_show_mode::stash_view_rest(idx)),
                    ))
                }),
            },
            // create (z)
            ActionHandlerContribution {
                action_name: "action:magit-stash-create",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let workdir = { s.lock().ok()?.workdir.clone() };
                    spawn_mutation_and_refresh(s, "stash".to_string(), move || {
                        let repo = Repository::discover(&workdir)
                            .map_err(|e| format!("not a git repository: {e}"))?;
                        Stash::create(&repo, None, false)
                            .map(|_| String::new())
                            .map_err(|e| e.to_string())
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
            // MR.3: the repository the trigger resolved for THIS
            // buffer, not the one the editor was started in.
            let workdir =
                crate::repo_scope::view_workdir(&ctx, buffer_id, &handle).unwrap_or_default();
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
    // MG.27: the row says "refreshing" from here until the
    // guard drops — including on every early exit inside the
    // task, which is why it is a guard and not a matching pair.
    let busy = headerline::busy(&hl);
    tokio::task::spawn(async move {
        let _busy = busy;
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
/// Run a repository mutation off-thread, report it, then refresh.
///
/// MG.54: `mutate` returns a `Result` so the outcome can be published.
/// It used to be `impl FnOnce()`, which meant every caller discarded
/// its git result — the operation finished in silence, and a FAILED
/// one finished in the same silence with the buffer refreshing as
/// though it had worked.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<StashState>>,
    label: String,
    mutate: impl FnOnce() -> Result<String, String> + Send + 'static,
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
    // MG.27: the row says "refreshing" from here until the
    // guard drops — including on every early exit inside the
    // task, which is why it is a guard and not a matching pair.
    let busy = headerline::busy(&hl);
    tokio::task::spawn(async move {
        let _busy = busy;
        let result = tokio::task::spawn_blocking(mutate)
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        crate::magit_global_mode::finish_task(&label, result);
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

    /// magit-status renders stash rows too, and now *resolves* them —
    /// so the round-trip above has to hold for the status buffer's
    /// writer as well.
    ///
    /// It nearly did not: `sections.rs` had its own inline copy of the
    /// format rather than calling [`list_row`]. That is the identical
    /// writer/reader split MG.15 was, one buffer over, and it would
    /// have surfaced the identical way — `p` on a Stashes row doing
    /// nothing, indistinguishable from an unbound key. This asserts
    /// they are the same string rather than trusting that they are.
    #[test]
    fn the_status_buffer_writes_the_same_stash_row_the_list_does() {
        use crate::sections::{Section, SectionEntry, SectionIndex, SectionKind};

        let entries: Vec<SectionEntry> = [(0usize, "WIP on main: abc123 x"), (3, "On main: y")]
            .into_iter()
            .map(|(index, message)| SectionEntry::Stash {
                index,
                message: message.to_string(),
            })
            .collect();
        let index = SectionIndex {
            sections: vec![Section {
                kind: SectionKind::Stashes,
                header_line: 0,
                body_start: 1,
                body_end: 1 + entries.len(),
                entries,
            }],
            branch: "main".to_string(),
            ahead: 0,
            behind: 0,
            bisect: None,
            in_flight: None,
            upstream: None,
        };

        // Render the real status buffer and read its stash rows back
        // through the parser the chords use.
        let rendered = index.format_buffer();
        let found: Vec<usize> = rendered.lines().filter_map(parse_index).collect();
        assert_eq!(
            found,
            vec![0, 3],
            "magit-status's stash rows must parse back to their indices — \
             the chords resolve the stash under the cursor this way, and \
             a row the parser cannot read is a dead key.\n{rendered}"
        );
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
            crate::workdir::magit_buffer_name_with(
                "stash",
                "lattice",
                &crate::magit_stash_show_mode::stash_view_rest(index),
            ),
            "*magit:stash:lattice:3*"
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
                args: _,
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
