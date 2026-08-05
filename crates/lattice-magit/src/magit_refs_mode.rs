//! MG.35: `magit-refs-mode` — magit's `y` show-refs.
//!
//! Every local branch, remote-tracking branch and tag, grouped, with
//! each branch's ahead/behind against its upstream. The question it
//! answers is "what refs exist, and where does each point" — which
//! [`crate::magit_branch_mode`] does not: that buffer lists local
//! branches so you can act on them, and knows nothing about tags,
//! remote-tracking refs, or how far ahead you are.
//!
//! **`<CR>` shows the commit, `c` checks out.** Across magit-log,
//! magit-blame, magit-rebase and magit-status's recent commits, `<CR>`
//! already means "show the commit detail"; magit-branch-mode's
//! checkout-on-`<CR>` is the outlier. Reading is also the safe default
//! here in a way it is not there: `<CR>` on a tag or a remote-tracking
//! branch under a checkout reading would silently detach HEAD, a state
//! that is hard to recognise and hard to leave, from the most reflexive
//! key in a buffer whose purpose is looking things up.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_cells::{Style, StyledSpan};
use lattice_config;
use lattice_grammar::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::position::Position;
use lattice_vcs::{RefEntry, RefKind, Reference, Repository};

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline::{self, Field, MagitHeaderlineHandle};

pub struct MagitRefsMode;

impl MagitRefsMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-refs-mode")
    }
}

/// The buffer this mode owns. Fixed, not parameterised: "every ref in
/// this repository" has no argument to vary.
pub const REFS_BUFFER: &str = "*magit:refs*";

fn magit_refs_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show the commit this ref points at", cmd: "action:magit-refs-show" },
            keymap_entry! { mode: Normal, chord: "c", doc: "Check out the ref at cursor", cmd: "action:magit-refs-checkout" },
        ]
    })
}

pub struct RefsState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    headerline: Option<MagitHeaderlineHandle>,
    /// One slot per rendered line: the ref that line shows, or `None`
    /// for a heading or a blank.
    ///
    /// Indexed rather than re-parsed out of the rendered text. The
    /// renderer pads names into columns, so scraping a row back would
    /// mean the parser and the formatter having to agree about padding
    /// forever — and a ref whose name happens to contain the padding
    /// would read as a different ref. Building the index in the same
    /// pass that renders makes them agree by construction.
    rows: Vec<Option<RefEntry>>,
}

/// MG.13: service alias for this mode's per-buffer state — register and
/// look up through this exact type (`feedback_servicesregistry_arc_typeid`).
pub type RefsStatesHandle = Arc<BufferStates<RefsState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<RefsState>>> {
    crate::buffer_state::state_for::<RefsState>(ctx)
}

struct RefsView(Arc<Mutex<RefsState>>);

impl MagitView for RefsView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }

    /// MG.24c: `A` / `_` / `O` act on the commit a ref points at.
    ///
    /// Full id, never the abbreviation on screen: an abbreviation is
    /// ambiguous in principle and git resolves the ambiguity by
    /// refusing, which would surface as a cherry-pick that did nothing.
    fn commit_at_cursor(&self, cursor: Position) -> Option<String> {
        Some(ref_at_cursor(&*self.0.lock().ok()?, cursor)?.id)
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        Some(self.0.lock().ok()?.workdir.clone())
    }
}

impl Mode for MagitRefsMode {
    type Guard = BufferStateGuard<RefsState>;

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
        Keymap::from_entries(magit_refs_keymap_entries())
    }

    /// MG.13: registered once at boot, resolving per-buffer state at
    /// call time — see `buffer_state`'s module docs.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: "action:magit-refs-show",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let entry = {
                        let g = s.lock().ok()?;
                        ref_at_cursor(&g, ctx.cursor)?
                    };
                    Some(Effect::OpenSyntheticBuffer {
                        name: format!("*magit:commit:{}*", entry.id),
                        mode_id: "magit-revision-mode".to_string(),
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-refs-checkout",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (entry, workdir) = {
                        let g = s.lock().ok()?;
                        (ref_at_cursor(&g, ctx.cursor)?, g.workdir.clone())
                    };
                    // A tag or a remote-tracking branch has no local
                    // branch to move, so checking it out detaches HEAD.
                    // Git says so in a paragraph the user never sees
                    // here, so the refusal says it instead — and points
                    // at the operation they almost certainly wanted.
                    if entry.kind != RefKind::Branch {
                        return Some(Effect::Echo {
                            level: EchoLevel::Error,
                            text: format!(
                                "magit: checking out {} would detach HEAD — \
                                 make a branch from it instead \
                                 (:magit-branch-create <name>, or `b` `c` in the dispatch)",
                                entry.name
                            ),
                        });
                    }
                    let name = entry.name.clone();
                    spawn_mutation_and_refresh(s, move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = lattice_vcs::Branch::checkout(&repo, &name);
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

            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: publish BEFORE the first `.await`, or the chords
            // above resolve to handlers that cannot find their state.
            // `rows` starts empty; `ref_at_cursor` returns `None` for
            // every line until the walk lands, so a `<CR>` in that
            // window declines rather than acting on a stale row.
            let Some(states) = ctx.service::<RefsStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                RefsState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    pending_highlights: pending_highlights.clone(),
                    headerline: hl.clone(),
                    rows: Vec::new(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(RefsView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            let wd = workdir.clone();
            let built = tokio::task::spawn_blocking(move || build_refs_buffer(&wd))
                .await
                .unwrap_or_default();
            headerline::publish(&hl, built.header.clone());
            let spans = built.spans;
            if let Ok(mut g) = state.lock() {
                g.rows = built.rows;
            }
            crate::buffer_io::replace_buffer_text(&handle, built.text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

/// `gr` — re-walk the refs.
fn refresh(s: Arc<Mutex<RefsState>>) -> Option<Effect> {
    respawn(s, || {})
}

/// Run `mutate` (a blocking git call) off the actor thread, then
/// re-walk — the shape every mutating handler uses instead of calling
/// git inline. Paramount goal #1: no `git` on the actor thread, ever.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<RefsState>>,
    mutate: impl FnOnce() + Send + 'static,
) -> Option<Effect> {
    respawn(s, mutate)
}

fn respawn(s: Arc<Mutex<RefsState>>, mutate: impl FnOnce() + Send + 'static) -> Option<Effect> {
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
    // MG.27: the headerline says "refreshing" until the guard drops,
    // including on every early exit inside the task.
    let busy = headerline::busy(&hl);
    tokio::task::spawn(async move {
        let _busy = busy;
        let _ = tokio::task::spawn_blocking(mutate).await;
        let built = tokio::task::spawn_blocking(move || build_refs_buffer(&wd))
            .await
            .unwrap_or_default();
        headerline::publish(&hl, built.header.clone());
        let spans = built.spans;
        // The row index is replaced BEFORE the text, so there is no
        // window in which a cursor lands on a new row and resolves
        // against the old index — which would act on a ref the user is
        // not looking at.
        if let Ok(mut g) = s.lock() {
            g.rows = built.rows;
        }
        crate::buffer_io::replace_buffer_text(&handle, built.text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

fn ref_at_cursor(state: &RefsState, cursor: Position) -> Option<RefEntry> {
    state.rows.get(cursor.line as usize).cloned().flatten()
}

/// The rendered buffer, its headerline fields, the line→ref index and
/// the highlight spans — all from one walk, because they must agree.
///
/// The spans are built here rather than in `highlight.rs` (where every
/// other magit buffer's are) because a refs row cannot be scanned back
/// from its text: the tracking summary contains spaces, and a row
/// without one runs straight from the id into the subject, so "summary
/// or subject?" is unanswerable after the fact. See the note in
/// `highlight.rs`.
#[derive(Default)]
pub(crate) struct RefsBuffer {
    pub(crate) text: String,
    pub(crate) header: Vec<Field>,
    pub(crate) rows: Vec<Option<RefEntry>>,
    pub(crate) spans: Vec<Vec<StyledSpan>>,
}

fn build_refs_buffer(workdir: &std::path::Path) -> RefsBuffer {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => {
            return RefsBuffer {
                text: "Not a git repository.\n".to_string(),
                ..Default::default()
            };
        }
    };
    render_refs(&Reference::list(&repo).unwrap_or_default())
}

/// Column the object id starts at. Long ref names push past it rather
/// than being truncated — a truncated ref name is not a ref name, and
/// this buffer's whole job is naming things.
const NAME_WIDTH: usize = 28;
/// Column the tracking summary starts at, measured from the id.
const ID_WIDTH: usize = 10;
/// Column the subject starts at, measured from the tracking summary.
const TRACK_WIDTH: usize = 20;

/// Render the grouped buffer.
///
/// Split from [`build_refs_buffer`] so the layout is testable without a
/// repository — the same reason `log_merged_argv` is a separate pure
/// function from `resolve_merge_commit`.
pub(crate) fn render_refs(refs: &[RefEntry]) -> RefsBuffer {
    if refs.is_empty() {
        return RefsBuffer {
            text: "No refs.\n".to_string(),
            header: headerline::refs_fields(0, 0, 0),
            rows: vec![None],
            spans: vec![Vec::new()],
        };
    }
    let count = |k: RefKind| refs.iter().filter(|r| r.kind == k).count();
    let (branches, remotes, tags) = (
        count(RefKind::Branch),
        count(RefKind::Remote),
        count(RefKind::Tag),
    );

    let mut text = String::new();
    let mut rows: Vec<Option<RefEntry>> = Vec::new();
    let mut spans: Vec<Vec<StyledSpan>> = Vec::new();
    for (kind, label, n) in [
        (RefKind::Branch, "Branches", branches),
        (RefKind::Remote, "Remotes", remotes),
        (RefKind::Tag, "Tags", tags),
    ] {
        if n == 0 {
            // An empty group is omitted rather than shown as a heading
            // with nothing under it: a repository with no remotes is the
            // ordinary case, not a state worth a row.
            continue;
        }
        text.push_str(&format!("{label} ({n})\n"));
        rows.push(None);
        spans.push(Vec::new());
        for entry in refs.iter().filter(|r| r.kind == kind) {
            let (row, row_spans) = render_row(entry);
            text.push_str(&row);
            text.push('\n');
            rows.push(Some(entry.clone()));
            spans.push(row_spans);
        }
        text.push('\n');
        rows.push(None);
        spans.push(Vec::new());
    }
    RefsBuffer {
        text,
        header: headerline::refs_fields(branches, remotes, tags),
        rows,
        spans,
    }
}

/// One ref row: `<marker><name> <short-id> <track> <subject>`, padded
/// into columns — and the spans naming each field, whose byte offsets
/// are recorded as the row is built rather than recovered from it.
fn render_row(entry: &RefEntry) -> (String, Vec<StyledSpan>) {
    let marker = if entry.head { "* " } else { "  " };
    // Char counts, not byte lengths: a ref name or subject may be
    // non-ASCII, and padding by bytes would misalign every column after
    // it. (Width in cells is a further step this buffer does not take —
    // the columns are a reading aid, not a table the cursor indexes.)
    let pad = |s: &str, w: usize| {
        let used = s.chars().count();
        " ".repeat(w.saturating_sub(used).max(1))
    };
    let mut row = String::from(marker);
    let mut spans = Vec::new();

    row.push_str(&entry.name);
    if entry.head {
        // Marker and name are one visual unit — the same span
        // `branch_styled_spans` gives the checked-out branch.
        spans.push(StyledSpan {
            start: 0,
            end: row.len(),
            style: Style::MagitBranchCurrent,
        });
    }
    row.push_str(&pad(&entry.name, NAME_WIDTH));

    let id_start = row.len();
    row.push_str(&entry.short_id);
    spans.push(StyledSpan {
        start: id_start,
        end: row.len(),
        style: Style::MagitSha,
    });
    row.push_str(&pad(&entry.short_id, ID_WIDTH));

    if !entry.track.is_empty() {
        let track_start = row.len();
        row.push_str(&entry.track);
        spans.push(StyledSpan {
            start: track_start,
            end: row.len(),
            // It is exactly that: a decoration saying where this ref
            // sits relative to another.
            style: Style::MagitRefDecoration,
        });
    }

    if entry.subject.is_empty() {
        // Nothing follows, so the padding after the tracking summary
        // would be trailing whitespace. Truncating cannot invalidate a
        // span: every span above ends at or before the id/track text,
        // and only padding is removed.
        return (row.trim_end().to_string(), spans);
    }
    row.push_str(&pad(&entry.track, TRACK_WIDTH));
    row.push_str(&entry.subject);
    (row, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: RefKind, name: &str) -> RefEntry {
        RefEntry {
            kind,
            name: name.to_string(),
            id: format!("{name}-full-id"),
            short_id: "a1b2c3d".to_string(),
            upstream: String::new(),
            track: String::new(),
            head: false,
            subject: "subject".to_string(),
        }
    }

    /// The index must name the ref on the line it was written to. This
    /// is the whole safety argument for indexing rather than scraping:
    /// every action reads through it, so an off-by-one acts on the wrong
    /// ref while looking correct.
    #[test]
    fn every_row_index_points_at_the_ref_rendered_on_that_line() {
        let refs = vec![
            entry(RefKind::Branch, "main"),
            entry(RefKind::Branch, "feature/x"),
            entry(RefKind::Remote, "origin/main"),
            entry(RefKind::Tag, "v1.0.0"),
        ];
        let built = render_refs(&refs);
        let lines: Vec<&str> = built.text.lines().collect();
        assert_eq!(
            built.rows.len(),
            lines.len(),
            "one index slot per rendered line"
        );
        for (i, slot) in built.rows.iter().enumerate() {
            match slot {
                Some(e) => assert!(
                    lines[i].contains(&e.name),
                    "line {i} ({:?}) must show {}",
                    lines[i],
                    e.name
                ),
                None => assert!(
                    !lines[i].starts_with("  ") && !lines[i].starts_with("* "),
                    "line {i} ({:?}) is indexed as a non-row but looks like one",
                    lines[i]
                ),
            }
        }
    }

    /// Headings and the blank separators must resolve to no ref, or
    /// `<CR>` on the word "Branches" would act on whatever the index
    /// happened to carry.
    #[test]
    fn headings_and_blanks_carry_no_ref() {
        let built = render_refs(&[entry(RefKind::Branch, "main")]);
        assert_eq!(built.rows[0], None, "the heading");
        assert!(built.rows[1].is_some(), "the one branch");
        assert_eq!(built.rows[2], None, "the trailing blank");
    }

    /// A group with nothing in it is omitted. A repository with no
    /// remotes is ordinary, and a `Remotes (0)` heading is a row that
    /// says nothing.
    #[test]
    fn empty_groups_are_omitted() {
        let built = render_refs(&[entry(RefKind::Branch, "main")]);
        assert!(built.text.starts_with("Branches (1)"));
        assert!(!built.text.contains("Remotes"), "got: {}", built.text);
        assert!(!built.text.contains("Tags"), "got: {}", built.text);
    }

    /// The checked-out branch is the only row marked, and the marker is
    /// what `refs_styled_spans` colours.
    #[test]
    fn only_the_checked_out_branch_is_marked() {
        let mut head = entry(RefKind::Branch, "main");
        head.head = true;
        let built = render_refs(&[head, entry(RefKind::Branch, "other")]);
        let marked: Vec<&str> = built.text.lines().filter(|l| l.starts_with("* ")).collect();
        assert_eq!(marked.len(), 1, "got: {}", built.text);
        assert!(marked[0].contains("main"));
    }

    /// A ref name longer than the column pushes the following fields
    /// right rather than being cut. A truncated ref name is not a ref
    /// name, and this buffer exists to name things.
    #[test]
    fn a_long_name_is_never_truncated() {
        let long = "feature/a-really-quite-long-branch-name-here";
        let built = render_refs(&[entry(RefKind::Branch, long)]);
        assert!(
            built.text.contains(long),
            "the name survives whole: {}",
            built.text
        );
        assert!(
            built.text.contains("a1b2c3d"),
            "and the id still follows it: {}",
            built.text
        );
    }

    /// Padding counts characters, not bytes — otherwise a non-ASCII ref
    /// name silently misaligns every column after it, and the misalignment
    /// scales with how non-ASCII the name is.
    #[test]
    fn a_non_ascii_name_pads_by_characters() {
        let built = render_refs(&[entry(RefKind::Branch, "función/ünïcode")]);
        let row = built.text.lines().nth(1).expect("the row");
        let id_at = row.find("a1b2c3d").expect("the id is on the row");
        // "  " + 15 chars + at least one pad space. Byte-based padding
        // would have produced a shorter run of spaces here, because the
        // name is 15 chars but 18 bytes.
        assert_eq!(
            row[..id_at].chars().count(),
            2 + NAME_WIDTH,
            "columns align by character: {row:?}"
        );
    }

    /// A ref with no subject must not leave the tracking column's
    /// padding behind as trailing whitespace.
    #[test]
    fn a_row_with_no_subject_has_no_trailing_whitespace() {
        let mut e = entry(RefKind::Tag, "v1.0.0");
        e.subject = String::new();
        let built = render_refs(&[e]);
        let row = built.text.lines().nth(1).expect("the row");
        assert_eq!(row, row.trim_end(), "trailing whitespace in {row:?}");
    }

    /// Every span must cover the text it claims to. This is the whole
    /// safety argument for emitting spans here rather than scanning the
    /// rendered line back: a byte offset that drifts colours the wrong
    /// characters, and the result still looks like a coloured buffer.
    #[test]
    fn every_span_covers_the_field_it_names() {
        let mut head = entry(RefKind::Branch, "main");
        head.head = true;
        head.track = "ahead 3, behind 1".to_string();
        let built = render_refs(&[head.clone(), entry(RefKind::Tag, "v1.0.0")]);
        let lines: Vec<&str> = built.text.lines().collect();
        assert_eq!(built.spans.len(), lines.len(), "one span slot per line");

        let row = lines[1];
        let spans = &built.spans[1];
        let at = |s: &StyledSpan| row[s.start..s.end].to_string();
        let find = |style: Style| spans.iter().find(|s| s.style == style).map(&at);

        assert_eq!(find(Style::MagitBranchCurrent).as_deref(), Some("* main"));
        assert_eq!(find(Style::MagitSha).as_deref(), Some("a1b2c3d"));
        assert_eq!(
            find(Style::MagitRefDecoration).as_deref(),
            Some("ahead 3, behind 1"),
            "the summary contains spaces — the reason it cannot be \
             recovered by scanning the row back"
        );
    }

    /// A ref that is level with its upstream has no summary, so there
    /// must be no decoration span — the subject follows the id directly,
    /// and colouring it as a decoration is the exact mistake a
    /// text-scanning highlighter would make.
    #[test]
    fn a_ref_with_no_tracking_summary_gets_no_decoration_span() {
        let built = render_refs(&[entry(RefKind::Branch, "main")]);
        assert!(
            !built.spans[1]
                .iter()
                .any(|s| s.style == Style::MagitRefDecoration),
            "no summary, so no decoration: {:?}",
            built.spans[1]
        );
        let row = built.text.lines().nth(1).expect("the row");
        assert!(
            row.ends_with("subject"),
            "the subject is still there: {row:?}"
        );
    }

    /// An empty repository says so, and still indexes its one line as
    /// carrying no ref.
    #[test]
    fn no_refs_says_so_and_indexes_nothing() {
        let built = render_refs(&[]);
        assert_eq!(built.text, "No refs.\n");
        assert_eq!(built.rows, vec![None]);
    }
}
