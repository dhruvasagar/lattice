//! PD.1 (2026-08-12): the **project-diff view** — every changed file in
//! the working tree as one editable multibuffer.
//!
//! Design: `docs/dev/architecture/magit-project-diff.md`. Slice plan:
//! `docs/dev/operations/slice-plans/magit-project-diff.md`.
//! Catalogue entry: A.1 in `slice-plans/multibuffer-providers.md`.
//!
//! ## The gap this fills
//!
//! Magit already diffs in two shapes and neither is this one.
//! `magit-status`'s sections are patch text built for staging;
//! `*magit:diff:<path>*` reads well but is one file at a time and still
//! patch text. Missing is *every changed file at once, as real source
//! you can edit* — you spot a typo in file 19 of a 30-file review and
//! today you must leave the diff, open the file, fix it, come back.
//!
//! ## Excerpts anchor in the working-tree file
//!
//! An excerpt is a hunk's post-image range in the file on disk, so
//! edits propagate through the ordinary M.3 pipeline with no patch
//! application and no write-back path of its own.
//!
//! That anchoring is also the constraint: **only the working tree is a
//! file.** A staged-vs-HEAD or `rev..rev` comparison has an index blob
//! as its post-image, with nothing for an edit to land in, so those
//! open read-only rather than getting an invented index-write-back
//! path. Read-only there is the correct rendering of a comparison
//! between two things that are not the file on disk — not a degraded
//! mode.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use lattice_config::OptionOverrideSet;
use lattice_core::{BufferFlags, BufferId, DocumentBuilder};
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_mode::{
    CapabilitySet, Keymap, LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};
use lattice_multibuffer::view::create_multibuffer_view;
use lattice_multibuffer::{Excerpt, ExcerptHeader, HeaderlineStatus, MultibufferRegistryHandle};
use lattice_runtime::{Document, spawn_document};
use lattice_syntax::LangRegistry;

/// Context lines above and below each hunk. Wider than the ±2 the
/// reference views use: this surface is for *reading a change in
/// place*, where a little more surrounding code is what makes an edit
/// safe to make without opening the file.
const CONTEXT: u32 = 3;

// ─────────────────────────────────────────────────────────────────
// What is being compared
// ─────────────────────────────────────────────────────────────────

/// Which comparison a project-diff view shows.
///
/// Editability follows the post-image, and only the working tree is a
/// file — see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProjectDiffComparison {
    /// Working tree vs `HEAD` — the daily driver, and the only
    /// **editable** one.
    #[default]
    WorkingTree,
    /// Index vs `HEAD`. Read-only: the post-image is an index blob.
    Staged,
}

impl ProjectDiffComparison {
    /// Does this comparison's post-image exist as a file an edit can
    /// propagate into?
    pub fn is_editable(self) -> bool {
        matches!(self, Self::WorkingTree)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::WorkingTree => "working tree",
            Self::Staged => "staged",
        }
    }
}

/// Per-view state: what it compares, and where.
#[derive(Debug, Clone)]
pub struct ProjectDiffState {
    pub workdir: PathBuf,
    pub comparison: ProjectDiffComparison,
}

/// Per-view state keyed by the view's `BufferId`, plus a
/// `DocumentId → BufferId` index for cleanup.
///
/// The second map is not redundant: `Event::DocumentClosed` carries a
/// `DocumentId` and the two ids are NOT interchangeable — the
/// multibuffer registry keeps a separate `remove_by_document_id` for
/// the same reason.
#[derive(Debug, Default)]
pub struct ProjectDiffService {
    views: std::sync::RwLock<HashMap<BufferId, ProjectDiffState>>,
    by_document: std::sync::RwLock<HashMap<lattice_protocol::ids::DocumentId, BufferId>>,
}

impl ProjectDiffService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_state(&self, view: BufferId, state: ProjectDiffState) {
        if let Ok(mut w) = self.views.write() {
            w.insert(view, state);
        }
    }

    pub fn state(&self, view: BufferId) -> Option<ProjectDiffState> {
        self.views.read().ok()?.get(&view).cloned()
    }

    pub fn index_document(&self, document: lattice_protocol::ids::DocumentId, view: BufferId) {
        if let Ok(mut w) = self.by_document.write() {
            w.insert(document, view);
        }
    }

    /// Cleanup entry point for the `DocumentClosed` subscriber.
    pub fn forget_by_document_id(&self, document: lattice_protocol::ids::DocumentId) -> bool {
        let view = match self.by_document.write() {
            Ok(mut w) => w.remove(&document),
            Err(_) => None,
        };
        match view {
            Some(v) => {
                self.forget(v);
                true
            }
            None => false,
        }
    }

    pub fn forget(&self, view: BufferId) {
        if let Ok(mut w) = self.views.write() {
            w.remove(&view);
        }
        if let Ok(mut w) = self.by_document.write() {
            w.retain(|_, v| *v != view);
        }
    }

    pub fn tracked_views(&self) -> usize {
        self.views.read().map(|r| r.len()).unwrap_or(0)
    }
}

/// Register and look up under THIS alias, never the inner type — the
/// `ServiceRegistry` keys on `TypeId`, so registering an
/// `Arc<ProjectDiffService>` and asking for `ProjectDiffService`
/// silently returns `None`.
pub type ProjectDiffServiceHandle = Arc<ProjectDiffService>;

// ─────────────────────────────────────────────────────────────────
// MagitProjectDiffMode — identity marker
// ─────────────────────────────────────────────────────────────────

/// `magit-project-diff-mode` — the provider-minor activated on a
/// project-diff view.
///
/// **Declares no keymap of its own.** It activates `magit-core-mode`
/// alongside, which is where magit's cross-buffer chords already live
/// (`gr`, `q`, `]]` / `[[`, …) — so this view inherits the family's
/// chords by joining the family rather than by copying them. That is
/// the "shared behaviour is a minor mode, never a copied keymap" rule
/// paying out in the crate where the missing-`x` gap happened.
pub struct MagitProjectDiffMode;

impl MagitProjectDiffMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-project-diff-mode")
    }
}

pub struct MagitProjectDiffModeGuard;

impl Mode for MagitProjectDiffMode {
    type Guard = MagitProjectDiffModeGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// Editable by default — no `ReadOnly` override. A read-only
    /// comparison (§2.1) sets the per-buffer read-only property at
    /// creation instead, so read-only is a *property of the view*, not
    /// a second mode or a renderer kind-branch.
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::new()
    }

    /// No chords. `gr` / `q` arrive from `magit-core-mode`, which this
    /// mode implies.
    fn keymap(&self) -> Keymap {
        Keymap::default()
    }

    /// Joining the magit family is what supplies the chords; declaring
    /// them here would be the duplication the standing rule forbids.
    fn implies(&self) -> &[ModeId] {
        magit_core_implies()
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(MagitProjectDiffModeGuard) })
    }
}

fn magit_core_implies() -> &'static [ModeId] {
    static IDS: std::sync::OnceLock<Vec<ModeId>> = std::sync::OnceLock::new();
    IDS.get_or_init(|| vec![crate::magit_core_mode::MagitCoreMode::mode_id()])
}

// ─────────────────────────────────────────────────────────────────
// View construction
// ─────────────────────────────────────────────────────────────────

/// Header for one hunk excerpt: the path plus the 1-based start line.
fn hunk_excerpt_header(path: &std::path::Path, line0: u32) -> ExcerptHeader {
    let mut header = ExcerptHeader::new(format!("{}", line0.saturating_add(1)));
    header.path = Some(path.to_path_buf());
    header
}

/// One changed file's hunks, as excerpt line ranges over the
/// working-tree file.
///
/// Returns the post-image ranges — the lines as they exist on disk
/// *now* — because that is what an excerpt anchors to and what an edit
/// propagates into.
pub fn file_hunk_ranges(before: &str, after: &str) -> Vec<(u32, u32)> {
    use lattice_diff::{DiffAlgorithm, HunkKind};
    let idx = lattice_diff::compute_diff(
        &[ropey::Rope::from_str(before), ropey::Rope::from_str(after)],
        DiffAlgorithm::Histogram,
    );
    let Ok(idx) = idx else {
        return Vec::new();
    };
    let last_line = (after.lines().count() as u32).saturating_sub(1);
    idx.hunks
        .iter()
        .filter_map(|h| {
            // The post-image side. A pure Remove has an empty range
            // there — anchor it at the deletion point so the excerpt
            // still shows where the lines went.
            let r = match h.kind {
                HunkKind::Add | HunkKind::Change => *h.ranges.get(1)?,
                HunkKind::Remove => {
                    let at = h.ranges.get(1).map(|r| r.start).unwrap_or(0);
                    lattice_diff::LineRange::new(at, at.saturating_add(1))
                }
                _ => return None,
            };
            let start = r.start.saturating_sub(CONTEXT);
            let end = (r.end.saturating_add(CONTEXT)).min(last_line.saturating_add(1));
            Some((start, end.max(start)))
        })
        .collect()
}

/// Build the sources + excerpts for a set of changed files.
///
/// `files` is `(path, baseline_text)`; the working-tree text is read
/// from disk. Returns `None` when there is nothing to show — no
/// changed files, or every one unreadable.
///
/// A file that fails to read is logged and skipped: one unreadable
/// path must not cost the user every other changed file.
#[allow(clippy::type_complexity)]
pub fn build_project_diff_excerpts(
    files: &[(PathBuf, String)],
) -> Option<(HashMap<BufferId, Arc<dyn Document>>, Vec<Excerpt>, usize)> {
    if files.is_empty() {
        return None;
    }
    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    let mut excerpts: Vec<Excerpt> = Vec::new();
    let mut n_files = 0usize;

    for (path, baseline) in files {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "project-diff: working-tree file unreadable; skipping it",
                );
                continue;
            }
        };
        let ranges = file_hunk_ranges(baseline, &text);
        if ranges.is_empty() {
            continue;
        }

        let source_id = BufferId::next();
        let document = DocumentBuilder::default()
            .with_text(&text)
            .with_path(path.clone())
            .build();
        let source_registry = Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
        let handle = spawn_document(source_id, document, source_registry);
        sources.insert(source_id, Arc::new(handle) as Arc<dyn Document>);
        n_files += 1;

        for (start, end) in ranges {
            let header = hunk_excerpt_header(path, start);
            excerpts.push(Excerpt::new(source_id, start, end).with_header(header));
        }
    }

    if excerpts.is_empty() {
        return None;
    }
    Some((sources, excerpts, n_files))
}

/// Set the sticky headerline: what is being compared, and whether it
/// is editable.
///
/// Stating read-only here is deliberate — the design's §2.2: a
/// read-only view must be *explained*, not merely enforced, or it
/// reads as a bug.
fn set_project_diff_headerline(
    activator: &mut dyn ModeActivator,
    view: BufferId,
    comparison: ProjectDiffComparison,
    n_hunks: usize,
    n_files: usize,
) {
    if let Some(reg) = activator.services().get::<MultibufferRegistryHandle>()
        && let Some(handle) = reg.handle(view)
    {
        let editable = if comparison.is_editable() {
            ""
        } else {
            " (read-only)"
        };
        handle.set_headerline(HeaderlineStatus::Complete {
            summary: format!(
                "[project-diff: {}{editable}] {n_hunks} hunks in {n_files} files",
                comparison.label()
            ),
            emphasis: None,
        });
    }
}

/// Open a project-diff multibuffer over `files`.
///
/// Returns the view's `BufferId`, or `None` when there is nothing to
/// show — the caller echoes rather than opening an empty view.
pub fn create_project_diff_view(
    activator: &mut dyn ModeActivator,
    files: &[(PathBuf, String)],
    state: ProjectDiffState,
    registry: CommandRegistryHandle,
    lang_registry: Option<Arc<LangRegistry>>,
) -> Option<BufferId> {
    let (sources, excerpts, n_files) = build_project_diff_excerpts(files)?;
    let n_hunks = excerpts.len();

    let view = create_multibuffer_view(
        activator,
        sources,
        excerpts,
        Some("*magit:project-diff*".to_string()),
        BufferFlags::default(),
        registry,
        lang_registry,
    );

    if let Some(svc) = activator.services().get::<ProjectDiffServiceHandle>() {
        svc.set_state(view, state.clone());
        if let Some(reg) = activator.services().get::<MultibufferRegistryHandle>()
            && let Some(handle) = reg.handle(view)
        {
            svc.index_document(handle.document_id(), view);
        }
    } else {
        tracing::debug!("project-diff: service not registered; refresh will be unavailable");
    }

    set_project_diff_headerline(activator, view, state.comparison, n_hunks, n_files);
    activator.activate_minor_by_id(view, MagitProjectDiffMode::mode_id());
    Some(view)
}

// ─────────────────────────────────────────────────────────────────
// Boot integration
// ─────────────────────────────────────────────────────────────────

pub fn register_project_diff_mode(modes: &mut ModeRegistry) {
    modes
        .register(MagitProjectDiffMode)
        .expect("magit-project-diff-mode registers without conflict at boot");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn only_the_working_tree_is_editable() {
        assert!(ProjectDiffComparison::WorkingTree.is_editable());
        assert!(
            !ProjectDiffComparison::Staged.is_editable(),
            "an index blob is not a file; an edit has nowhere to land"
        );
    }

    /// The mode joins the magit family rather than copying its chords —
    /// the thing RV.2 spent a slice undoing elsewhere.
    #[test]
    fn the_mode_inherits_magit_chords_and_declares_none() {
        let m = MagitProjectDiffMode;
        assert_eq!(m.kind(), ModeKind::Minor);
        assert!(
            m.keymap().entries.is_empty() && m.keymap().bindings.is_empty(),
            "no chords of its own"
        );
        assert!(
            m.implies()
                .contains(&crate::magit_core_mode::MagitCoreMode::mode_id()),
            "must join magit-core-mode to get gr / q / ]] / [["
        );
    }

    #[test]
    fn a_changed_file_yields_one_excerpt_range_per_hunk() {
        let before = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let after = "a\nB\nc\nd\ne\nf\nG\nh\n";
        let ranges = file_hunk_ranges(before, after);
        assert_eq!(ranges.len(), 2, "two separated changes → two hunks");
        for (s, e) in &ranges {
            assert!(s <= e, "range is ordered: {s}..{e}");
        }
    }

    #[test]
    fn an_unchanged_file_yields_no_ranges() {
        assert!(file_hunk_ranges("same\n", "same\n").is_empty());
    }

    /// Context must not run past the end of the file — a hunk at the
    /// last line would otherwise produce an out-of-range excerpt.
    #[test]
    fn context_clamps_at_the_end_of_the_file() {
        let before = "a\nb\nc\n";
        let after = "a\nb\nZ\n";
        let last = (after.lines().count() as u32) - 1;
        for (_, end) in file_hunk_ranges(before, after) {
            assert!(end <= last + 1, "end {end} past last line {last}");
        }
    }

    #[test]
    fn no_files_yields_nothing_to_show() {
        assert!(build_project_diff_excerpts(&[]).is_none());
    }

    #[test]
    fn service_tracks_and_forgets_view_state() {
        let svc = ProjectDiffService::new();
        let view = BufferId::next();
        let state = ProjectDiffState {
            workdir: PathBuf::from("/tmp/repo"),
            comparison: ProjectDiffComparison::WorkingTree,
        };
        svc.set_state(view, state);
        assert_eq!(svc.tracked_views(), 1);
        svc.forget(view);
        assert!(svc.state(view).is_none());
        assert_eq!(svc.tracked_views(), 0);
    }
}
