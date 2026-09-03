//! MG.40: `magit-cherry-mode` — magit's `Y` cherries.
//!
//! "Which of my commits are not upstream yet, and which already are —
//! possibly under a different SHA?" `git cherry` answers exactly that,
//! and the second half is what makes it more than `git log
//! upstream..HEAD`: a commit that was cherry-picked or rebased upstream
//! has a *different* SHA there, so a range walk still lists it as
//! missing. `git cherry` compares patch-ids instead, and marks it `-`.
//!
//! ```text
//! Cherries  main vs origin/main
//!
//! + a1b2c3d  not upstream yet
//! - e4f5g6h  already upstream, under another sha
//! ```
//!
//! `<CR>` opens the commit, and `magit-core-mode`'s `A` / `_` / `O` act
//! on it — a `+` row is exactly the thing you reach for cherry-pick
//! with, which is where the command's name comes from.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_cells::{Style, StyledSpan};
use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::position::Position;
use lattice_vcs::Repository;

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline::{self, Field, MagitHeaderlineHandle};

pub struct MagitCherryMode;

impl MagitCherryMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-cherry-mode")
    }
}

/// `*magit:cherry:<upstream>..<head>*`.
///
/// Both ends are in the name because "cherries" is meaningless without
/// them — the same buffer text describes a completely different question
/// against a different upstream.
pub(crate) const CHERRY_VIEW: &str = "cherry";

/// MR.3b: this view's `rest` — `<upstream>..<head>`, git's own range
/// spelling, which is why `..` rather than a further `:` separates them.
pub(crate) fn cherry_view_rest(upstream: &str, head: &str) -> String {
    format!("{upstream}..{head}")
}

/// The `(upstream, head)` a cherry buffer's name carries.
fn parse_name(name: &str) -> Option<(String, String)> {
    let parsed = crate::workdir::parse_magit_name(name)?;
    (parsed.view == CHERRY_VIEW).then_some(())?;
    let (upstream, head) = parsed.rest?.split_once("..")?;
    (!upstream.is_empty() && !head.is_empty()).then(|| (upstream.to_string(), head.to_string()))
}

fn magit_cherry_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show the commit at cursor", cmd: "action:magit-cherry-show" },
        ]
    })
}

pub struct CherryState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    headerline: Option<MagitHeaderlineHandle>,
    upstream: String,
    head: String,
    /// One slot per rendered line: the full sha that line names, or
    /// `None` for a heading or blank. Built in the same pass that
    /// renders, for the reason `magit-refs-mode` does it — the row is
    /// padded, so scraping it back would tie the parser to the
    /// formatter forever.
    rows: Vec<Option<String>>,
}

/// MG.13: service alias — register and look up through this exact type.
pub type CherryStatesHandle = Arc<BufferStates<CherryState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<CherryState>>> {
    crate::buffer_state::state_for::<CherryState>(ctx)
}

struct CherryView(Arc<Mutex<CherryState>>);

impl MagitView for CherryView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }

    /// MG.24c: `A` / `_` / `O` act on the commit under the cursor. This
    /// is the buffer those chords matter most in — a `+` row is a commit
    /// that is not upstream, which is the thing you cherry-pick.
    fn commit_at_cursor(&self, cursor: Position) -> Option<String> {
        let g = self.0.lock().ok()?;
        g.rows.get(cursor.line as usize).cloned().flatten()
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        Some(self.0.lock().ok()?.workdir.clone())
    }
}

impl Mode for MagitCherryMode {
    type Guard = BufferStateGuard<CherryState>;

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

    /// MG.RO: `read-only-mode` is where the gate actually is.
    ///
    /// `ReadOnly = true` above stops TYPING and nothing else. It is read by
    /// `read_only_edit_rejected`, which guards the insert-mode char path;
    /// operators never reach it, because a `Document`'s grammar dispatch
    /// applies its own edits and hands the host an already-applied
    /// `Effect::Edits`. `x` deleted a character out of `*magit:status*` while
    /// the buffer reported itself read-only — worse than not gating at all,
    /// because it looks protected.
    ///
    /// `read-only-mode` carries the option AND the `invocation_runner`
    /// (`Editor::run_read_only_motion`) that refuses mutating operators while
    /// letting motions, `:` and `/` through.
    ///
    /// Declared per MAJOR rather than once on `magit-core-mode`: an implied
    /// mode is followed from the mode being ACTIVATED, and the majors are what
    /// the host activates. Putting it on the shared minor looked right and was
    /// verified not to fire.
    fn implies(&self) -> &[lattice_mode::ModeId] {
        static IMPLIED: std::sync::OnceLock<Vec<lattice_mode::ModeId>> = std::sync::OnceLock::new();
        IMPLIED.get_or_init(|| vec![lattice_mode::modes::ReadOnlyMode::mode_id()])
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_cherry_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![ActionHandlerContribution {
            action_name: "action:magit-cherry-show",
            handler: Arc::new(|ctx: &ActionContext<'_>| {
                let s = state(ctx)?;
                let sha = {
                    let g = s.lock().ok()?;
                    g.rows.get(ctx.cursor.line as usize).cloned().flatten()?
                };
                Some(crate::magit_global_mode::open_repo_view_from_action_with(
                    ctx,
                    crate::magit_revision_mode::SHOW_VIEW,
                    "magit-revision-mode",
                    Some(&sha),
                ))
            }),
        }]
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
            let (upstream, head) = store
                .name_for(buffer_id)
                .as_deref()
                .and_then(parse_name)
                .unwrap_or_default();

            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: publish BEFORE the first `.await`. `rows` starts
            // empty, so `<CR>` and `A` / `_` / `O` decline in the window
            // before the walk lands rather than acting on a stale row.
            let Some(states) = ctx.service::<CherryStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                CherryState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    pending_highlights: pending_highlights.clone(),
                    headerline: hl.clone(),
                    upstream: upstream.clone(),
                    head: head.clone(),
                    rows: Vec::new(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(CherryView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            let wd = workdir.clone();
            let (u, h) = (upstream.clone(), head.clone());
            let built = tokio::task::spawn_blocking(move || build_cherry_buffer(&wd, &u, &h))
                .await
                .unwrap_or_default();
            headerline::publish(&hl, built.header.clone());
            if let Ok(mut g) = state.lock() {
                g.rows = built.rows;
            }
            crate::buffer_io::replace_buffer_text(&handle, built.text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, built.spans);
            }

            Ok(guard)
        })
    }
}

/// `gr` — re-run the walk.
fn refresh(s: Arc<Mutex<CherryState>>) -> Option<Effect> {
    let (handle, wd, pending, buffer_id, hl, upstream, head) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
            g.headerline.clone(),
            g.upstream.clone(),
            g.head.clone(),
        )
    };
    let busy = headerline::busy(&hl);
    tokio::task::spawn(async move {
        let _busy = busy;
        let built = tokio::task::spawn_blocking(move || build_cherry_buffer(&wd, &upstream, &head))
            .await
            .unwrap_or_default();
        headerline::publish(&hl, built.header.clone());
        if let Ok(mut g) = s.lock() {
            g.rows = built.rows;
        }
        crate::buffer_io::replace_buffer_text(&handle, built.text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, built.spans);
        }
    });
    None
}

/// The rendered buffer, its header, its line→sha index and its spans —
/// one walk, because they must agree.
#[derive(Default)]
pub(crate) struct CherryBuffer {
    pub(crate) text: String,
    pub(crate) header: Vec<Field>,
    pub(crate) rows: Vec<Option<String>>,
    pub(crate) spans: Vec<Vec<StyledSpan>>,
}

fn build_cherry_buffer(workdir: &std::path::Path, upstream: &str, head: &str) -> CherryBuffer {
    if upstream.is_empty() {
        return CherryBuffer {
            text: "magit: cherries needs an upstream to compare against.\n".to_string(),
            rows: vec![None],
            spans: vec![Vec::new()],
            ..Default::default()
        };
    }
    let Ok(repo) = Repository::discover(workdir) else {
        return CherryBuffer {
            text: "Not a git repository.\n".to_string(),
            rows: vec![None],
            spans: vec![Vec::new()],
            ..Default::default()
        };
    };
    // `-v` adds the subject; without it the rows are bare shas and the
    // buffer is unreadable. `%H`-style full shas are not available from
    // `git cherry`, so `rev-parse` is not needed: it prints full shas
    // already, which is what `<CR>` and `A` hand on.
    let raw = repo
        .run_git_str(["cherry", "-v", upstream, head])
        .unwrap_or_default();
    render_cherries(&raw, upstream, head)
}

/// Turn `git cherry -v` output into the buffer.
///
/// Split out pure so the layout and the index are testable without a
/// repository — the same shape `render_refs` has.
pub(crate) fn render_cherries(raw: &str, upstream: &str, head: &str) -> CherryBuffer {
    let mut text = format!("Cherries  {head} vs {upstream}\n\n");
    let mut rows: Vec<Option<String>> = vec![None, None];
    let mut spans: Vec<Vec<StyledSpan>> = vec![Vec::new(), Vec::new()];
    let (mut ahead, mut equivalent) = (0usize, 0usize);

    for line in raw.lines() {
        // `<+|-> <sha> <subject>`.
        let Some((mark, rest)) = line.split_once(' ') else {
            continue;
        };
        let (sha, subject) = match rest.split_once(' ') {
            Some((s, subj)) => (s, subj),
            None => (rest, ""),
        };
        if sha.is_empty() || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        match mark {
            "+" => ahead += 1,
            "-" => equivalent += 1,
            _ => continue,
        }
        let short: String = sha.chars().take(7).collect();
        let row = if subject.is_empty() {
            format!("{mark} {short}")
        } else {
            format!("{mark} {short}  {subject}")
        };
        // The sha's span, measured from the row just built rather than
        // assumed — the mark is one char plus a space, but saying so
        // twice is how the two drift.
        let sha_start = mark.len() + 1;
        spans.push(vec![StyledSpan {
            start: sha_start,
            end: sha_start + short.len(),
            style: Style::MagitSha,
        }]);
        text.push_str(&row);
        text.push('\n');
        rows.push(Some(sha.to_string()));
    }

    if ahead == 0 && equivalent == 0 {
        text.push_str("Nothing to compare — no commits on either side.\n");
        rows.push(None);
        spans.push(Vec::new());
    }
    CherryBuffer {
        text,
        header: headerline::cherry_fields(upstream, head, ahead, equivalent),
        rows,
        spans,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "+ a1b2c3d4e5f6 not upstream yet\n\
                          - e4f5a6b7c8d9 already upstream under another sha\n\
                          + 0123456789ab another one\n";

    #[test]
    fn the_name_carries_both_ends_and_round_trips() {
        let name = crate::workdir::magit_buffer_name_with(
            CHERRY_VIEW,
            "lattice",
            &cherry_view_rest("origin/main", "main"),
        );
        assert_eq!(
            parse_name(&name),
            Some(("origin/main".to_string(), "main".to_string()))
        );
    }

    /// Both ends are required. A cherry buffer with half a comparison
    /// describes a different question than the one asked.
    #[test]
    fn names_missing_an_end_resolve_to_nothing() {
        for name in [
            "*magit:cherry:..main*",
            "*magit:cherry:origin/main..*",
            "*magit:cherry:origin/main*",
            "*magit:commit:abc*",
        ] {
            assert_eq!(parse_name(name), None, "{name:?}");
        }
    }

    /// The index must name the commit on the line it was written to —
    /// `<CR>`, `A`, `_` and `O` all read through it, so an off-by-one
    /// acts on the wrong commit while looking right.
    #[test]
    fn every_row_index_points_at_the_commit_on_that_line() {
        let built = render_cherries(SAMPLE, "origin/main", "main");
        let lines: Vec<&str> = built.text.lines().collect();
        assert_eq!(built.rows.len(), lines.len(), "one slot per line");
        assert_eq!(built.spans.len(), lines.len(), "one span slot per line");
        for (i, slot) in built.rows.iter().enumerate() {
            if let Some(sha) = slot {
                assert!(
                    lines[i].contains(&sha[..7]),
                    "line {i} ({:?}) must show {sha}",
                    lines[i]
                );
            }
        }
    }

    /// The FULL sha is what the index carries, not the seven characters
    /// on screen: an abbreviation is ambiguous in principle and git
    /// resolves the ambiguity by refusing, which would surface as a
    /// `<CR>` that opened nothing.
    #[test]
    fn the_index_carries_full_shas_even_though_rows_show_short_ones() {
        let built = render_cherries(SAMPLE, "origin/main", "main");
        let shas: Vec<&String> = built.rows.iter().flatten().collect();
        assert_eq!(shas.len(), 3);
        assert_eq!(shas[0], "a1b2c3d4e5f6");
        assert!(
            built.text.contains("+ a1b2c3d"),
            "but the row shows the short one: {}",
            built.text
        );
    }

    /// `-` rows are the whole point of using `git cherry` over a range
    /// walk: a commit already upstream under a different sha. Both marks
    /// must survive into the buffer and be counted separately.
    #[test]
    fn both_marks_survive_and_are_counted_apart() {
        let built = render_cherries(SAMPLE, "origin/main", "main");
        assert!(built.text.starts_with("Cherries"));
        assert_eq!(
            built.text.lines().filter(|l| l.starts_with("+ ")).count(),
            2,
            "two not-upstream commits"
        );
        assert_eq!(
            built.text.lines().filter(|l| l.starts_with("- ")).count(),
            1,
            "one already-upstream commit"
        );
    }

    /// Garbage lines are skipped rather than rendered as a row with no
    /// commit behind it — the same judgement `parse_for_each_ref` makes.
    #[test]
    fn unparseable_lines_are_skipped_not_rendered() {
        let built = render_cherries("garbage\n* wrongmark abc subject\n+ \n", "u", "h");
        assert!(
            built.rows.iter().flatten().next().is_none(),
            "no row survived: {:?}",
            built.text
        );
        assert!(built.text.contains("Nothing to compare"));
    }

    /// Every span must cover the sha it claims to.
    #[test]
    fn the_sha_span_covers_the_sha() {
        let built = render_cherries(SAMPLE, "origin/main", "main");
        let lines: Vec<&str> = built.text.lines().collect();
        for (i, spans) in built.spans.iter().enumerate() {
            for s in spans {
                assert_eq!(
                    &lines[i][s.start..s.end],
                    &built.rows[i].as_ref().expect("a row")[..7],
                    "span on line {i} must cover the short sha"
                );
            }
        }
    }
}
