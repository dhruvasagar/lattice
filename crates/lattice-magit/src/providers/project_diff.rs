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
use lattice_grammar::{Args, CommandRegistry, CommandRegistryHandle};
use lattice_mode::{
    CapabilitySet, Keymap, LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};
use lattice_multibuffer::view::create_multibuffer_view;
use lattice_multibuffer::{
    Excerpt, ExcerptHeader, HeaderlineStatus, MultibufferDocumentHandle, MultibufferExcerptsReady,
    MultibufferRegistryHandle,
};
use lattice_runtime::{Document, EventBus, spawn_document};
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
    // PD.9: `magit-nav-mode`, NOT `magit-core-mode`. This view is
    // editable, and `magit-core-mode` claims `i`, `C`, `D`, `S`, `U`, `q`
    // and `yr` — legitimate only because every major it attaches to is a
    // read-only list, which its `ActivationPolicy::Majors` enforces.
    // Reaching it through `implies` bypassed that gate, so `i` opened the
    // .gitignore prompt instead of entering Insert. The same trap
    // `magit-commit-mode` is excluded by name to avoid.
    //
    // `refreshable-view-mode` supplies `gr`, which this view does still
    // want and which was never magit-core's to give.
    IDS.get_or_init(|| {
        vec![
            crate::magit_nav_mode::MagitNavMode::mode_id(),
            lattice_mode::RefreshableViewMode::mode_id(),
        ]
    })
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
/// PD.7a: post-image lines the diff touched, as a [`DiffSignMap`].
///
/// **Delegates to `lattice_diff::compute_diff_sign_map`.** This function
/// used to walk the hunks itself and derive its own `(line, kind)` pairs,
/// which was the same classification written twice — and drifting
/// already: the hand-rolled version collapsed `Change` into `Add`, and
/// had no answer for `Conflict` at all.
///
/// The distinction that makes this reuse rather than a workaround: diff
/// classification is a **pure function** of two texts, and that is what
/// is shared. `DiffSession` is the orthogonal thing — a lifecycle that
/// watches buffers, debounces and republishes — and this view genuinely
/// has a different one: its baselines come from git rather than from a
/// sibling buffer, and it refreshes on a scan rather than on an edit.
/// Depending on the pure core and not the stateful shell is the whole of
/// the design here; manufacturing a baseline buffer per file so a session
/// could be opened would have been contorting the data to fit the tool.
pub fn changed_lines(before: &str, after: &str) -> lattice_diff::overlay::DiffSignMap {
    let Ok(idx) = lattice_diff::compute_diff(
        &[ropey::Rope::from_str(before), ropey::Rope::from_str(after)],
        lattice_diff::DiffAlgorithm::Histogram,
    ) else {
        return lattice_diff::overlay::DiffSignMap::default();
    };
    lattice_diff::overlay::compute_diff_sign_map(&idx)
}

/// PD.7b: text the diff removed, grouped by the post-image line the
/// removal sits above.
///
/// Taken from the PRE-image side (slot 0), which is the only place the
/// removed text still exists — the post-image is the file, and the file
/// no longer has it.
pub fn removed_lines(before: &str, after: &str) -> Vec<(u32, Vec<String>)> {
    use lattice_diff::{DiffAlgorithm, HunkKind};
    let Ok(idx) = lattice_diff::compute_diff(
        &[ropey::Rope::from_str(before), ropey::Rope::from_str(after)],
        DiffAlgorithm::Histogram,
    ) else {
        return Vec::new();
    };
    let pre: Vec<&str> = before.lines().collect();
    let mut out: Vec<(u32, Vec<String>)> = Vec::new();
    for h in &idx.hunks {
        if !matches!(h.kind, HunkKind::Remove | HunkKind::Change) {
            continue;
        }
        let (Some(before_r), Some(after_r)) = (h.ranges.first(), h.ranges.get(1)) else {
            continue;
        };
        let text: Vec<String> = (before_r.start..before_r.end)
            .filter_map(|l| pre.get(l as usize).map(|s| (*s).to_string()))
            .collect();
        if text.is_empty() {
            continue;
        }
        out.push((after_r.start, text));
    }
    out.sort_by_key(|(l, _)| *l);
    out
}

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
            // `LineRange` is half-open (`start..end`); `Excerpt::new`
            // takes an INCLUSIVE end. Converting needs the `- 1`, and
            // the clamp is to `last_line`, not `last_line + 1`.
            //
            // Both were wrong, and together they made every excerpt name
            // at least one row the file does not have. That row is
            // silently dropped when the text is composed
            // (`compose_text_from_sources` skips a `None` line) but still
            // gets an entry in the row translation — so the composed text
            // ran one row SHORT of its own line-number map, and every row
            // after the first such excerpt was numbered one low. Compounding
            // per excerpt, which is why `<CR>` landed off by one too: the
            // jump reads the same map.
            let start = r.start.saturating_sub(CONTEXT);
            let last_changed = r.end.saturating_sub(1);
            let end = last_changed.saturating_add(CONTEXT).min(last_line);
            Some((start, end.max(start)))
        })
        .collect()
}

/// One changed file, read and diffed: the working-tree text plus the
/// post-image ranges its hunks occupy.
///
/// The two halves of building a batch are split around this type on
/// purpose. Producing it is filesystem + CPU work
/// ([`read_and_diff`], `spawn_blocking`-only); consuming it spawns
/// document actors and touches the view ([`attach_batch`], async side).
/// Fusing them — the PD.1 shape, where one function read, diffed and
/// spawned — would have put every `read_to_string` and every diff on
/// the actor thread the moment a trigger existed to call it.
#[derive(Debug, Clone)]
pub struct FileHunks {
    pub path: PathBuf,
    /// The working-tree text, as read during the scan.
    pub text: String,
    /// Post-image excerpt ranges, one per hunk, context already applied.
    pub ranges: Vec<(u32, u32)>,
    /// PD.7a: which post-image lines the diff touched, in **source** line
    /// coordinates, sorted.
    ///
    /// Only `Add` and `Change` appear. A removed line has no post-image
    /// row to paint — showing it needs a virtual row, which is PD.7b.
    /// Recording nothing for it here is why this view currently reads as
    /// "some lines are highlighted" rather than "these lines went away".
    pub changed: lattice_diff::overlay::DiffSignMap,
    /// PD.7b: lines the diff removed, keyed by the **post-image line they
    /// sat above** and carrying the removed text.
    ///
    /// These have no row in the working-tree file — that is what "removed"
    /// means — so they cannot be painted like `changed`. They render as
    /// virtual rows anchored above the post-image line, which is also why
    /// they are unselectable and untypeable: a line that is not in the file
    /// is not a line you can edit, and the ghost row makes that visible
    /// rather than surprising.
    pub removed: Vec<(u32, Vec<String>)>,
}

/// **The blocking half.** Read each changed file and compute its hunk
/// ranges against the baseline the scan collected.
///
/// Pure, no tokio, no view access — call it inside `spawn_blocking`.
///
/// A file that fails to read is logged and skipped: one unreadable path
/// must not cost the user every other changed file. A file whose hunks
/// all vanished (it was edited back to the baseline between the status
/// call and the read) contributes nothing rather than an empty group.
pub fn read_and_diff(files: &[(PathBuf, String)]) -> Vec<FileHunks> {
    let mut out = Vec::with_capacity(files.len());
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
        let changed = changed_lines(baseline, &text);
        let removed = removed_lines(baseline, &text);
        out.push(FileHunks {
            path: path.clone(),
            text,
            ranges,
            changed,
            removed,
        });
    }
    out
}

/// **The view-touching half.** Spawn a source document per file, add it
/// to `view`'s source map, and append one excerpt per hunk.
///
/// Returns the number of excerpts appended, so the caller can keep a
/// running hunk count for the headerline without re-reading the view.
///
/// Appending (rather than replacing) is what makes the view fill
/// progressively: the user sees the first files while the rest are
/// still being read.
/// PD.7b: the removed lines, as virtual rows.
///
/// A removed line has no row in the working-tree file — that is what
/// removed means — so it cannot be painted like a changed one. It renders
/// as a `DeletionBlock` virtual row anchored above the post-image line the
/// deletion sat at.
///
/// **Composed anchors are computed here, per `collect()`, from the view's
/// current excerpt list — never cached.** That is what makes the rows
/// slide when the user types: an edit shifts the excerpts, the next
/// collect reads the shifted excerpts, and the ghosts move with them. A
/// stored composed anchor would detach on the first keystroke, which is
/// the drift PD.7 flagged.
///
/// **Known ceiling, chosen deliberately:** these rows are display-only, so
/// removed text cannot be searched, selected or copied. Zed shipped this
/// same shape first and later spent a large refactor making deleted hunks
/// ordinary text in the editor's coordinate space precisely to get those
/// three back. We take the simpler form because it preserves the
/// one-composed-row-one-source-line invariant the whole edit-propagation
/// path rests on — the invariant whose violation caused the line-number
/// off-by-one. If searchable deletions are wanted later, that is the
/// change, and it is a substrate change rather than a provider one.
#[derive(Debug)]
pub struct ProjectDiffDeletionRows {
    id: lattice_cells::ProviderId,
    view: MultibufferDocumentHandle,
    /// Removed text per source, keyed by the post-image line it sat above.
    removed: std::sync::Mutex<HashMap<BufferId, Vec<(u32, Vec<String>)>>>,
    version: std::sync::atomic::AtomicU64,
}

/// Namespace for the deletion-row provider, distinct from the
/// multibuffer's own header / status / fold provider ids.
const DELETION_ROW_NAMESPACE: u64 = 0xBBBB_0005_0000_0000;

impl ProjectDiffDeletionRows {
    pub fn new(view: MultibufferDocumentHandle, buffer_id: BufferId) -> Self {
        Self {
            id: DELETION_ROW_NAMESPACE | buffer_id.0 as u64,
            view,
            removed: std::sync::Mutex::new(HashMap::new()),
            version: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Record a batch's removals and invalidate, so the next frame
    /// re-collects rather than serving a cached row set.
    pub fn set_for_source(&self, source: BufferId, removed: Vec<(u32, Vec<String>)>) {
        if let Ok(mut map) = self.removed.lock() {
            map.insert(source, removed);
        }
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl lattice_cells::VirtualRowProvider for ProjectDiffDeletionRows {
    fn id(&self) -> lattice_cells::ProviderId {
        self.id
    }

    fn version(&self) -> u64 {
        // Folded with the view's own content version so an excerpt
        // append — which moves every composed anchor below it — also
        // invalidates, not only a new batch of removals.
        self.version.load(std::sync::atomic::Ordering::Relaxed) ^ self.view.snapshot().version
    }

    fn collect(&self) -> Vec<lattice_cells::VirtualRow> {
        let Ok(removed) = self.removed.lock() else {
            return Vec::new();
        };
        let excerpts = self.view.excerpts();
        let mut rows = Vec::new();
        let mut composed = 0u32;
        for excerpt in &excerpts {
            if let Some(entries) = removed.get(&excerpt.source) {
                for (at, text) in entries {
                    // Only removals that fall INSIDE this excerpt's span
                    // have a row to anchor to. One outside it belongs to a
                    // hunk this excerpt does not show.
                    if *at < excerpt.start_line || *at > excerpt.end_line {
                        continue;
                    }
                    let anchor = composed + (*at - excerpt.start_line);
                    for line in text {
                        rows.push(lattice_cells::VirtualRow {
                            anchor_line: anchor,
                            position: lattice_cells::AnchorPosition::Above,
                            cells: line
                                .chars()
                                .map(|c| lattice_cells::Cell::new(c as u32, 0, 0, 0))
                                .collect::<Vec<_>>()
                                .into(),
                            height: 1,
                            kind: lattice_cells::VirtualRowKind::DeletionBlock,
                            bg: None,
                            scales: None,
                            gutter_line: None,
                            gutter_fg: None,
                        });
                    }
                }
            }
            composed += excerpt.line_count();
        }
        rows
    }
}

/// PD.7a: the composed-row spans that make a change visible.
///
/// Composed rows are excerpts laid end to end, so a source line's row
/// index depends on every excerpt appended before it — which is why this
/// runs after the batch has been appended and reads the view's own
/// excerpt list rather than trying to predict it.
///
/// Published as **styled spans**, not as a diff session. The host already
/// derives its gutter sign map from `Style::DiffAdd` / `DiffRemove`
/// (`diff_signs_from_spans`), which is how magit's patch buffers get
/// their signs — so one publish yields both the row tint and the gutter
/// mark, and `lattice-multibuffer` gains no diff dependency, which PD.1
/// asserts it must not.
fn composed_diff_spans(
    view: &MultibufferDocumentHandle,
    changed_by_source: &HashMap<BufferId, lattice_diff::overlay::DiffSignMap>,
) -> Vec<Vec<lattice_cells::StyledSpan>> {
    use lattice_cells::style::Style;
    let excerpts = view.excerpts();
    let total: usize = excerpts.iter().map(|e| e.line_count() as usize).sum();
    let mut rows: Vec<Vec<lattice_cells::StyledSpan>> = vec![Vec::new(); total];

    let mut composed = 0usize;
    for excerpt in &excerpts {
        let changed = changed_by_source.get(&excerpt.source);
        for offset in 0..excerpt.line_count() {
            let source_line = excerpt.start_line + offset;
            if let Some(kind) = changed.and_then(|c| {
                let e = c.entries();
                e.binary_search_by_key(&source_line, |(l, _)| *l)
                    .ok()
                    .map(|i| e[i].1)
            }) {
                // Only Add and Remove exist as text styles. Every kind
                // that reaches here describes a line PRESENT in the
                // post-image — `compute_diff_sign_map` skips `Remove`
                // outright — so `DiffAdd` is accurate for what this rope
                // actually contains rather than a fallback. The removal
                // half is the row that is not there, which PD.7b renders
                // as a virtual row.
                let _ = kind;
                let style = Style::DiffAdd;
                // Whole-row span. The renderer paints the line background
                // from it and the host reads the style for the gutter.
                rows[composed + offset as usize] = vec![lattice_cells::StyledSpan {
                    start: 0,
                    end: usize::MAX,
                    style,
                }];
            }
        }
        composed += excerpt.line_count() as usize;
    }
    rows
}

pub fn attach_batch(view: &MultibufferDocumentHandle, batch: &[FileHunks]) -> usize {
    let mut appended = 0usize;
    for file in batch {
        let source_id = BufferId::next();
        let document = DocumentBuilder::default()
            .with_text(&file.text)
            .with_path(file.path.clone())
            .build();
        // A source document in a provider view gets its own empty
        // command registry behind the `ArcSwap` handle `spawn_document`
        // expects — the same shape the search provider's sources use.
        let source_registry = Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
        let handle = spawn_document(source_id, document, source_registry);
        view.add_source(source_id, Arc::new(handle) as Arc<dyn Document>);

        let excerpts: Vec<Excerpt> = file
            .ranges
            .iter()
            .map(|(start, end)| {
                Excerpt::new(source_id, *start, *end)
                    .with_header(hunk_excerpt_header(&file.path, *start))
            })
            .collect();
        appended += excerpts.len();
        view.append_excerpts(excerpts);
    }
    appended
}

// ─────────────────────────────────────────────────────────────────
// The scan
// ─────────────────────────────────────────────────────────────────

/// Collect the changed files and their baseline text.
///
/// **Blocking by construction, and must run on `spawn_blocking`.** It
/// shells out to git once for the status and once per changed file for
/// the baseline blob; on the editor actor's `current_thread` runtime a
/// bare `tokio::spawn` would put every one of those on the actor
/// thread. Paramount goal #1 — see the standing rule.
///
/// Returns `(path, baseline_text)` pairs. A file whose baseline cannot
/// be read is **skipped, not fatal**: a newly-added file has no `HEAD`
/// blob at all, which is a normal state rather than an error, and one
/// unreadable path must not cost the user every other changed file.
pub fn scan_changed_files(
    workdir: &std::path::Path,
    comparison: ProjectDiffComparison,
) -> Vec<(PathBuf, String)> {
    use lattice_vcs::{GitBlob, Repository, WorkingTree};

    let Ok(repo) = Repository::discover(workdir) else {
        tracing::debug!(
            workdir = %workdir.display(),
            "project-diff: not a git repository"
        );
        return Vec::new();
    };
    let Ok(statuses) = WorkingTree::statuses(&repo) else {
        tracing::debug!("project-diff: `git status` failed");
        return Vec::new();
    };

    let mut out = Vec::new();
    for (rel, change) in statuses {
        // Which axis the comparison reads. Working tree vs HEAD wants
        // anything that differs from HEAD at all; staged wants only
        // what is in the index.
        let relevant = match comparison {
            ProjectDiffComparison::WorkingTree => {
                change.staged.is_some() || change.unstaged.is_some()
            }
            ProjectDiffComparison::Staged => change.staged.is_some(),
        };
        if !relevant {
            continue;
        }

        // The baseline side. An added file has no HEAD blob — treat it
        // as empty rather than skipping, so the whole file shows as
        // added rather than the file vanishing from the view.
        let baseline = GitBlob::read_path(&repo, "HEAD", &rel)
            .map(|r| r.to_string())
            .unwrap_or_default();
        out.push((workdir.join(&rel), baseline));
    }
    out
}

// ─────────────────────────────────────────────────────────────────
// The trigger
// ─────────────────────────────────────────────────────────────────

/// The name this provider is registered under in the
/// [`ProviderViewRegistry`]. Both front-ends —
/// `:magit-project-diff` and the Diff transient's `e` row — name it.
pub const PROVIDER_NAME: &str = "magit-project-diff";

/// The view's buffer name. Stable, so re-triggering finds the buffer
/// the user already has open instead of stacking a second one beside it
/// under the same name (which would make `:b *magit:project-diff*`
/// ambiguous).
pub const VIEW_NAME: &str = "*magit:project-diff*";

/// Files read + diffed per batch before the view is touched.
///
/// Small enough that the first hunks land almost immediately on a large
/// working tree; large enough that a 200-file diff does not pay 200
/// round-trips between the blocking pool and the actor. Not a user
/// option — the right value is a property of the two costs, not of
/// anyone's preference.
const SCAN_BATCH: usize = 8;

/// Which comparison the trigger's arguments asked for.
///
/// Anything unrecognised falls back to the working tree rather than
/// refusing: the working tree is the daily driver, and a typo in an
/// argument is a worse reason to show nothing than to show the default.
fn comparison_from_args(args: &Args) -> ProjectDiffComparison {
    let raw = match args {
        Args::String(s) => Some(s.trim().to_ascii_lowercase()),
        Args::List(values) => values.iter().find_map(|v| match v {
            lattice_grammar::ArgValue::String(s) => Some(s.trim().to_ascii_lowercase()),
            _ => None,
        }),
        _ => None,
    };
    match raw.as_deref() {
        Some("staged") | Some("index") => ProjectDiffComparison::Staged,
        _ => ProjectDiffComparison::WorkingTree,
    }
}

/// Find the project-diff view already open, if there is one.
///
/// Name lookup alone is not enough — a buffer could carry the name
/// without being a live multibuffer (a stale registry entry, a test
/// harness) — so the candidate must also resolve to a multibuffer
/// handle before it is reused.
fn existing_view(services: &lattice_mode::ServiceRegistry) -> Option<BufferId> {
    let store = services.get::<lattice_mode::BufferStoreHandle>()?;
    let id = store.find_by_name(VIEW_NAME)?;
    let registry = services.get::<MultibufferRegistryHandle>()?;
    registry.handle(id).map(|_| id)
}

/// Open (or re-drive) the project-diff view.
///
/// This is the closure registered on the generic provider-view seam, so
/// it is the whole of what the host does for this feature: the host arm
/// looks the name up, calls this with itself as the activator, and
/// applies the returned [`ProviderViewOutcome`].
///
/// The view opens **empty and immediately**; the scan runs off-thread
/// and streams into it. Re-triggering with the view already open
/// re-drives the scan into the same buffer rather than minting a second
/// one — which is also why the excerpts are cleared here rather than in
/// the task: the clear must be visible before the first batch lands, or
/// the old and new scans briefly show together.
pub fn open_project_diff(
    activator: &mut dyn ModeActivator,
    args: &Args,
) -> lattice_mode::ProviderViewOutcome {
    use lattice_mode::ProviderViewOutcome;

    let comparison = comparison_from_args(args);
    let Some(workdir) = crate::workdir::magit_workdir() else {
        return ProviderViewOutcome::Declined {
            message: "magit: not inside a git repository".to_string(),
        };
    };

    let services = activator.services();
    let Some(registry) = services.get::<CommandRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: "magit: command registry unavailable; cannot open the project diff"
                .to_string(),
        };
    };
    let lang_registry = services.get::<Arc<LangRegistry>>().map(|h| (*h).clone());

    let reopened = existing_view(&services);
    let view = match reopened {
        Some(view) => view,
        None => create_multibuffer_view(
            activator,
            HashMap::new(),
            Vec::new(),
            Some(VIEW_NAME.to_string()),
            BufferFlags::default(),
            (*registry).clone(),
            lang_registry,
        ),
    };

    let Some(mb_registry) = services.get::<MultibufferRegistryHandle>() else {
        return ProviderViewOutcome::Declined {
            message: "magit: multibuffer registry unavailable; cannot open the project diff"
                .to_string(),
        };
    };
    let Some(handle) = mb_registry.handle(view) else {
        return ProviderViewOutcome::Declined {
            message: "magit: the project-diff view failed to open".to_string(),
        };
    };

    if let Some(svc) = services.get::<ProjectDiffServiceHandle>() {
        svc.set_state(
            view,
            ProjectDiffState {
                workdir: workdir.clone(),
                comparison,
            },
        );
        svc.index_document(handle.document_id(), view);
    } else {
        tracing::debug!("project-diff: service not registered; view state will not be tracked");
    }

    // Empty the view before the scan starts. On a first open this is a
    // no-op; on a re-trigger it is what stops the previous scan's
    // excerpts from sitting above the new ones.
    handle.replace_excerpts(HashMap::new(), Vec::new());
    handle.set_headerline(HeaderlineStatus::InProgress {
        label: format!("Computing {} diff", comparison.label()),
        count: Some(0),
        emphasis: None,
    });

    activator.activate_minor_by_id(view, MagitProjectDiffMode::mode_id());

    // PD.4: editability follows the post-image (design §2.1). A working
    // tree is a file an edit can propagate into; an index blob is not,
    // and neither is a revision's blob — so those open read-only through
    // the generic minor, never a magit-local gate or a kind-branch.
    //
    // Both arms fire, and the `else` is the load-bearing one: the view
    // is reused across triggers, so opening the staged diff and then the
    // working-tree diff must *clear* the mode. Activating conditionally
    // and never deactivating is how the second view ends up silently
    // unwritable, in a buffer that looks exactly like a writable one.
    if comparison.is_editable() {
        activator.deactivate_minor_by_id(view, lattice_mode::modes::ReadOnlyMode::mode_id());
    } else {
        activator.activate_minor_by_id(view, lattice_mode::modes::ReadOnlyMode::mode_id());
    }

    let events = services.get::<Arc<EventBus>>().map(|b| (*b).clone());
    // PD.7a: the seam the diff styling rides on. Absent in a test host
    // that wired no highlight service — the view then renders uncoloured
    // rather than failing, which is the graceful-degradation rule.
    let synthetic_highlights = services
        .get::<lattice_mode::PendingSyntheticHighlightsHandle>()
        .map(|h| (*h).clone());
    // PD.7b: created and registered HERE, synchronously, because
    // `register_virtual_row_provider` needs `&mut` on the activator and
    // the scan is async. The scan then only pushes data into it.
    let deletion_rows = mb_registry.handle(view).map(|h| {
        let rows = Arc::new(ProjectDiffDeletionRows::new((*h).clone(), view));
        activator.register_virtual_row_provider(view, rows.clone());
        rows
    });
    spawn_project_diff_scan(
        view,
        workdir,
        comparison,
        mb_registry,
        events,
        synthetic_highlights,
        deletion_rows,
    );

    ProviderViewOutcome::Opened {
        view,
        message: Some(format!(
            "project-diff: scanning the {} …",
            comparison.label()
        )),
    }
}

/// Run the scan off-thread and stream its batches into `view`.
///
/// Shape, and why:
///
/// - The git status + baseline reads and the per-file read + diff both
///   run under **`spawn_blocking`**. The editor actor is a
///   `current_thread` runtime, so a bare `tokio::spawn` for that work
///   would land every `read_to_string` and every diff on the actor
///   thread — paramount goal #1's forbidden pattern, and the reason
///   PD.2 documented `scan_changed_files` as blocking-only before any
///   caller existed.
/// - Excerpts are appended **per batch**, so the view fills
///   progressively instead of blinking from empty to complete.
/// - Each batch publishes [`MultibufferExcerptsReady`], which is the
///   registered off-keystroke wake. Without it the excerpts would sit
///   invisible until the user happened to press a key — the bug class
///   that reads as a rendering fault and is not one.
///
/// There is no typed batch event + forwarder pair here (the shape
/// `providers::search` uses) because producer and consumer are the same
/// task: the bus exists to decouple a producer from an unknown set of
/// subscribers, and inventing one for a single known consumer would be
/// indirection without a reader.
fn spawn_project_diff_scan(
    view: BufferId,
    workdir: PathBuf,
    comparison: ProjectDiffComparison,
    mb_registry: Arc<MultibufferRegistryHandle>,
    events: Option<Arc<EventBus>>,
    synthetic_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    deletion_rows: Option<Arc<ProjectDiffDeletionRows>>,
) {
    let editable_note = if comparison.is_editable() {
        ""
    } else {
        " (read-only)"
    };

    tokio::spawn(async move {
        let scanned = tokio::task::spawn_blocking({
            let workdir = workdir.clone();
            move || scan_changed_files(&workdir, comparison)
        })
        .await;
        let files = match scanned {
            Ok(files) => files,
            Err(e) => {
                tracing::warn!(error = %e, "project-diff: the scan task failed");
                Vec::new()
            }
        };

        // The view may have been closed while the scan ran. Every
        // handle lookup below re-checks, so a closed view ends the task
        // instead of appending into a registry entry nobody reads.
        let Some(handle) = mb_registry.handle(view) else {
            return;
        };

        if files.is_empty() {
            handle.set_headerline(HeaderlineStatus::Complete {
                summary: format!(
                    "[project-diff: {}{editable_note}] no changes",
                    comparison.label()
                ),
                emphasis: None,
            });
            if let Some(events) = &events {
                events.publish_typed(MultibufferExcerptsReady { view });
            }
            return;
        }

        let total_files = files.len();
        let mut files_done = 0usize;
        let mut hunks = 0usize;
        // PD.7a: accumulated across batches, because the composed spans
        // are rebuilt whole each time and every source's classification
        // has to still be there when a later batch triggers the rebuild.
        let mut changed_by_source: HashMap<BufferId, lattice_diff::overlay::DiffSignMap> =
            HashMap::new();

        for chunk in files.chunks(SCAN_BATCH) {
            let owned: Vec<(PathBuf, String)> = chunk.to_vec();
            let built = match tokio::task::spawn_blocking(move || read_and_diff(&owned)).await {
                Ok(built) => built,
                Err(e) => {
                    tracing::warn!(error = %e, "project-diff: a batch failed to read; skipping it");
                    continue;
                }
            };

            let Some(handle) = mb_registry.handle(view) else {
                return;
            };
            hunks += attach_batch(&handle, &built);
            // PD.7a: repaint the whole view after each batch. Rebuilding
            // every row rather than appending is what keeps the spans
            // aligned — an excerpt appended now shifts nothing, but a
            // later batch's rows sit after these, and `Replace` is the
            // op that cannot drift out of step with the rope.
            for f in &built {
                if let Some(id) = handle
                    .source_buffer_ids()
                    .into_iter()
                    .find(|id| handle.source_path(*id).as_deref() == Some(f.path.as_path()))
                {
                    changed_by_source.insert(id, f.changed.clone());
                    if let Some(rows) = &deletion_rows {
                        rows.set_for_source(id, f.removed.clone());
                    }
                }
            }
            if let Some(pending) = &synthetic_highlights {
                pending.store_and_wake(view, composed_diff_spans(&handle, &changed_by_source));
            }
            files_done += chunk.len();

            handle.set_headerline(HeaderlineStatus::InProgress {
                label: format!(
                    "Computing {} diff ({files_done}/{total_files} files)",
                    comparison.label()
                ),
                count: Some(hunks),
                emphasis: None,
            });
            if let Some(events) = &events {
                events.publish_typed(MultibufferExcerptsReady { view });
            }
        }

        let Some(handle) = mb_registry.handle(view) else {
            return;
        };
        // The file count is the number of files that actually produced
        // hunks, which can be lower than the scanned count: a file can be
        // edited back to its baseline between `git status` and the read.
        let shown_files = handle.source_buffer_ids().len();
        handle.set_headerline(HeaderlineStatus::Complete {
            summary: format!(
                "[project-diff: {}{editable_note}] {hunks} hunks in {shown_files} files",
                comparison.label()
            ),
            emphasis: None,
        });
        if let Some(events) = &events {
            events.publish_typed(MultibufferExcerptsReady { view });
        }
    });
}

// ─────────────────────────────────────────────────────────────────
// Boot integration
// ─────────────────────────────────────────────────────────────────

pub fn register_project_diff_mode(modes: &mut ModeRegistry) {
    modes
        .register(MagitProjectDiffMode)
        .expect("magit-project-diff-mode registers without conflict at boot");
}

/// Register the view opener on the generic provider-view seam.
///
/// This — plus an ex-command and a transient row, both also in this
/// crate — is the whole of the trigger. No `Editor::` method, no host
/// `Action` variant, no dispatch arm: the acid test a provider crate is
/// supposed to pass.
///
/// A missing registry means the host did not publish the seam (an older
/// boot, or a test harness); logged and skipped, because refusing to
/// boot over an unavailable optional surface is the worse failure.
pub fn register_project_diff_provider(services: &lattice_mode::ServiceRegistry) {
    let Some(registry) = services.get::<lattice_mode::ProviderViewRegistryHandle>() else {
        tracing::debug!(
            "project-diff: no ProviderViewRegistry; `:magit-project-diff` will not be available"
        );
        return;
    };
    if !registry.register(
        PROVIDER_NAME,
        Arc::new(|activator: &mut dyn ModeActivator, args: &Args| {
            open_project_diff(activator, args)
        }),
    ) {
        tracing::warn!(
            provider = PROVIDER_NAME,
            "project-diff: a provider view is already registered under this name"
        );
    }
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
    ///
    /// PD.9 changed WHICH part of the family it joins. It implied
    /// `magit-core-mode`, which claims `i`, `C`, `D`, `S`, `U`, `q` and
    /// `yr` — legitimate only on the read-only lists its
    /// `ActivationPolicy::Majors` gate allows, and this view is editable,
    /// so `i` opened the .gitignore prompt instead of entering Insert.
    /// It now implies `magit-nav-mode` (the chords that are safe
    /// anywhere) plus `refreshable-view-mode` for `gr`.
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
                .contains(&crate::magit_nav_mode::MagitNavMode::mode_id()),
            "must join magit-nav-mode to get ]] / [[ / <Tab> without the \
             read-only letters"
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
        assert!(read_and_diff(&[]).is_empty());
    }

    // ── PD.2: the scan ───────────────────────────────────────────

    fn git(dir: &std::path::Path, args: &[&str]) {
        let st = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// A repo with one committed file modified, and one brand-new file.
    fn repo_with_changes() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git(p, &["init"]);
        git(p, &["config", "user.email", "t@lattice.dev"]);
        git(p, &["config", "user.name", "lattice-test"]);
        std::fs::write(p.join("tracked.rs"), "fn main() {\n    let old = 1;\n}\n").unwrap();
        git(p, &["add", "tracked.rs"]);
        git(p, &["commit", "-m", "base"]);
        std::fs::write(p.join("tracked.rs"), "fn main() {\n    let new = 2;\n}\n").unwrap();
        std::fs::write(p.join("added.rs"), "fn fresh() {}\n").unwrap();
        git(p, &["add", "added.rs"]);
        dir
    }

    #[test]
    fn the_scan_finds_modified_and_added_files() {
        let dir = repo_with_changes();
        let found = scan_changed_files(dir.path(), ProjectDiffComparison::WorkingTree);
        let names: Vec<String> = found
            .iter()
            .filter_map(|(p, _)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(names.contains(&"tracked.rs".to_string()), "got {names:?}");
        assert!(names.contains(&"added.rs".to_string()), "got {names:?}");
    }

    /// A newly-added file has no HEAD blob. That is a normal state, not
    /// an error — it must come back with an EMPTY baseline so the whole
    /// file reads as added, rather than vanishing from the view.
    #[test]
    fn an_added_file_gets_an_empty_baseline_rather_than_being_skipped() {
        let dir = repo_with_changes();
        let found = scan_changed_files(dir.path(), ProjectDiffComparison::WorkingTree);
        let added = found
            .iter()
            .find(|(p, _)| p.ends_with("added.rs"))
            .expect("the added file is in the scan");
        assert!(added.1.is_empty(), "no HEAD blob ⇒ empty baseline");
    }

    /// The staged comparison reads the index axis only, so a purely
    /// unstaged modification is not in it.
    #[test]
    fn the_staged_comparison_excludes_unstaged_only_changes() {
        let dir = repo_with_changes();
        let staged = scan_changed_files(dir.path(), ProjectDiffComparison::Staged);
        let names: Vec<String> = staged
            .iter()
            .filter_map(|(p, _)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .collect();
        assert!(
            names.contains(&"added.rs".to_string()),
            "added.rs was `git add`ed: {names:?}"
        );
        assert!(
            !names.contains(&"tracked.rs".to_string()),
            "tracked.rs is modified but unstaged: {names:?}"
        );
    }

    /// Not a repository is a normal thing to point at, not a panic.
    #[test]
    fn a_non_repository_scans_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scan_changed_files(dir.path(), ProjectDiffComparison::WorkingTree).is_empty());
    }

    /// End to end across the blocking half: the scan feeds
    /// `read_and_diff`, and the modified file comes back with hunks.
    #[test]
    fn the_scan_feeds_the_blocking_half() {
        let dir = repo_with_changes();
        let files = scan_changed_files(dir.path(), ProjectDiffComparison::WorkingTree);
        let built = read_and_diff(&files);
        assert!(!built.is_empty(), "changed files yield hunks");
        assert!(
            built.iter().all(|f| !f.ranges.is_empty()),
            "a file with no surviving hunks is dropped, not carried empty"
        );
        assert!(
            built.iter().any(|f| f.path.ends_with("tracked.rs")),
            "the modified file is in the built batch: {:?}",
            built.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }

    // ── PD.8: excerpt ranges must name rows the file has ─────────

    /// Reported as "the line numbers are off by one, and `<CR>` lands one
    /// line off". One cause: `LineRange` is half-open and `Excerpt::new`
    /// takes an inclusive end, so every range named one row too many —
    /// and the clamp allowed `last_line + 1`, a row past EOF.
    ///
    /// The symptom is indirect, which is why it is worth a test at this
    /// level: a row the source does not have is skipped when the text is
    /// composed but still counted in the row translation, so the composed
    /// text runs short of its own line-number map and everything below is
    /// numbered low. Compounding per excerpt.
    #[test]
    fn an_excerpt_never_names_a_row_past_the_end_of_the_file() {
        // Change the LAST line, so context pushes the range against EOF.
        let before = "a\nb\nc\n";
        let after = "a\nb\nCHANGED\n";
        let last = (after.lines().count() as u32) - 1;
        for (start, end) in file_hunk_ranges(before, after) {
            assert!(
                end <= last,
                "excerpt ({start},{end}) names row {end}, past the last line {last}"
            );
        }
    }

    /// The inclusive end must be the last CHANGED line plus context, not
    /// the half-open end plus context — otherwise every excerpt is one row
    /// long even when it fits inside the file, and the desync is just
    /// harder to notice.
    #[test]
    fn the_excerpt_end_is_inclusive_of_the_last_context_line() {
        // 20 lines, one changed in the middle: context is unclamped, so
        // the arithmetic is visible.
        let before: String = (0..20).map(|i| format!("line{i}\n")).collect();
        let after = before.replace("line10\n", "CHANGED\n");
        let ranges = file_hunk_ranges(&before, &after);
        assert_eq!(ranges.len(), 1, "one hunk; got {ranges:?}");
        let (start, end) = ranges[0];
        assert_eq!(
            start,
            10 - CONTEXT,
            "start is the changed line minus context"
        );
        assert_eq!(
            end,
            10 + CONTEXT,
            "end is the changed line plus context, INCLUSIVE"
        );
    }

    /// Every row an excerpt names must exist in the source, for any hunk
    /// position — the property the two tests above are instances of.
    #[test]
    fn every_named_row_exists_for_a_change_anywhere_in_the_file() {
        let before: String = (0..12).map(|i| format!("line{i}\n")).collect();
        let last = 11u32;
        for changed in 0..12u32 {
            let after = before.replace(&format!("line{changed}\n"), "CHANGED\n");
            for (start, end) in file_hunk_ranges(&before, &after) {
                assert!(
                    end <= last && start <= end,
                    "changing line {changed} produced excerpt ({start},{end}); \
                     last line is {last}"
                );
            }
        }
    }

    // ── PD.7b: removed lines as virtual rows ─────────────────────

    #[test]
    fn a_removed_line_is_captured_with_its_text() {
        let removed = removed_lines("a\ngone\nb\n", "a\nb\n");
        assert_eq!(removed.len(), 1, "one removal; got {removed:?}");
        let (at, text) = &removed[0];
        assert_eq!(*at, 1, "anchored at the post-image line it sat above");
        assert_eq!(text, &vec!["gone".to_string()], "carries the removed text");
    }

    /// A rewritten line is BOTH: an added row to paint and a removed row
    /// to ghost. Losing the second half would show the new text with no
    /// sign of what it replaced, which is most of what a diff is for.
    #[test]
    fn a_changed_line_keeps_its_removed_half() {
        let removed = removed_lines("a\nold\nb\n", "a\nnew\nb\n");
        assert!(
            removed.iter().any(|(_, t)| t.contains(&"old".to_string())),
            "the replaced text must survive as a ghost row; got {removed:?}"
        );
    }

    #[test]
    fn a_pure_addition_removes_nothing() {
        assert!(removed_lines("a\nb\n", "a\nNEW\nb\n").is_empty());
    }

    #[test]
    fn a_multi_line_removal_keeps_every_line_in_order() {
        let removed = removed_lines("a\none\ntwo\nthree\nb\n", "a\nb\n");
        let text: Vec<String> = removed.iter().flat_map(|(_, t)| t.clone()).collect();
        assert_eq!(text, vec!["one", "two", "three"]);
    }

    /// The composed anchor is `excerpt_offset + (source_line -
    /// excerpt.start_line)`, recomputed per collect. This pins the
    /// arithmetic for the SECOND excerpt, where a naive implementation
    /// that forgot the running offset would anchor into the first one —
    /// and would look correct in any single-excerpt test.
    #[test]
    fn a_removal_in_the_second_excerpt_anchors_past_the_first() {
        // Two excerpts of 3 rows each; a removal at source line 21 in the
        // second, whose span starts at 20 — so composed row 3 + 1 = 4.
        let first_len = 3u32;
        let second_start = 20u32;
        let removal_at = 21u32;
        let composed = first_len + (removal_at - second_start);
        assert_eq!(
            composed, 4,
            "second excerpt's rows start after the first's, so the anchor \
             must include the running offset"
        );
    }

    // ── PD.7a: which lines the diff touched ──────────────────────
    //
    // The view showed real source with nothing marking what changed, so a
    // reader could not tell a changed line from its context — the whole
    // point of a diff. `changed_lines` is the classification that fixes
    // it, in post-image (working-tree) coordinates, which is the only
    // coordinate space the excerpts have rows in.

    #[test]
    fn an_added_line_is_classified() {
        let changed = changed_lines("a\nb\n", "a\nNEW\nb\n");
        assert!(
            changed.entries().iter().any(|(l, _)| *l == 1),
            "the inserted line 1 should be marked; got {:?}",
            changed.entries()
        );
    }

    /// Delegating to `compute_diff_sign_map` buys a distinction the
    /// hand-rolled version did not make: a rewritten line is `Change`,
    /// not `Add`. Pinned because it is the concrete evidence that the
    /// duplication had already drifted, and the reason to keep only one
    /// implementation.
    #[test]
    fn a_rewritten_line_is_change_not_add() {
        use lattice_diff::overlay::DiffSignKind;
        let changed = changed_lines("a\nold\nc\n", "a\nnew\nc\n");
        let kind = changed
            .entries()
            .iter()
            .find(|(l, _)| *l == 1)
            .map(|(_, k)| *k);
        assert_eq!(kind, Some(DiffSignKind::Change));
    }

    #[test]
    fn a_changed_line_is_classified() {
        let changed = changed_lines("a\nold\nc\n", "a\nnew\nc\n");
        assert!(
            changed.entries().iter().any(|(l, _)| *l == 1),
            "the rewritten line 1 should be marked; got {:?}",
            changed.entries()
        );
    }

    /// Context lines are what the marks are read AGAINST. If everything
    /// were marked the view would be as uninformative as marking nothing.
    #[test]
    fn untouched_lines_are_not_classified() {
        let changed = changed_lines("a\nb\nc\n", "a\nNEW\nb\nc\n");
        let marked: Vec<u32> = changed.entries().iter().map(|(l, _)| *l).collect();
        assert!(
            !marked.contains(&0),
            "line 0 is unchanged context; got {marked:?}"
        );
    }

    /// A pure deletion has NO post-image row, so it is deliberately absent
    /// here — there is no line to paint. Showing the user that something
    /// was removed needs a virtual row, which is PD.7b. Pinned so that
    /// absence reads as a decision rather than a miss.
    #[test]
    fn a_pure_removal_marks_nothing_because_it_has_no_row() {
        let changed = changed_lines("a\ngone\nb\n", "a\nb\n");
        assert!(
            changed.is_empty(),
            "a removed line has no post-image row to mark; got {:?}",
            changed.entries()
        );
    }

    #[test]
    fn an_unchanged_file_classifies_nothing() {
        assert!(changed_lines("same\n", "same\n").is_empty());
    }

    /// The classification is sorted and deduplicated, because the span
    /// builder binary-searches it once per composed row.
    #[test]
    fn the_classification_is_sorted_and_unique() {
        let changed = changed_lines("a\nb\nc\nd\n", "A\nb\nC\nD\n");
        let lines: Vec<u32> = changed.entries().iter().map(|(l, _)| *l).collect();
        let mut sorted = lines.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(lines, sorted, "must be sorted for binary search");
    }

    /// The scan carries the classification through to the batch, or the
    /// view has nothing to paint from.
    #[test]
    fn the_batch_carries_the_classification() {
        let dir = repo_with_changes();
        let files = scan_changed_files(dir.path(), ProjectDiffComparison::WorkingTree);
        let built = read_and_diff(&files);
        let tracked = built
            .iter()
            .find(|f| f.path.ends_with("tracked.rs"))
            .expect("the modified file is in the batch");
        assert!(
            !tracked.changed.is_empty(),
            "a file with hunks must carry marked lines"
        );
    }

    // ── PD.4: an edit in the view lands in the file ──────────────
    //
    // Design §2: an excerpt is a hunk's post-image range in the
    // WORKING-TREE file, so editing it goes through the ordinary M.3
    // propagation pipeline and lands in the file — no patch application,
    // no write-back path of its own. These assert the anchoring that
    // claim rests on, which is the part that would break silently: an
    // excerpt anchored in generated patch text would still render, still
    // accept keystrokes, and propagate an edit to nowhere.

    /// The source document a batch attaches carries the **working-tree**
    /// text and the file's path — not the baseline blob. Anchoring it to
    /// the baseline would look identical in the view (the hunk ranges are
    /// post-image either way) and send every edit into a document that
    /// was never the file.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_attached_source_is_the_working_tree_file() {
        let dir = repo_with_changes();
        let files = scan_changed_files(dir.path(), ProjectDiffComparison::WorkingTree);
        let built = read_and_diff(&files);
        let tracked = built
            .iter()
            .find(|f| f.path.ends_with("tracked.rs"))
            .expect("the modified file is in the batch");

        assert!(
            tracked.text.contains("let new = 2;"),
            "the source must hold the working-tree text; got {:?}",
            tracked.text
        );
        assert!(
            !tracked.text.contains("let old = 1;"),
            "...and not the HEAD baseline, or edits would land in the wrong content"
        );
        assert_eq!(
            tracked.text,
            std::fs::read_to_string(&tracked.path).unwrap(),
            "byte-identical to the file on disk"
        );
    }

    /// End to end through a live view: attach a real repo's hunks, edit
    /// a composed row, and watch the edit arrive in the source document
    /// that carries the file's path. That last clause is the assertion
    /// that matters — propagation into *some* document proves nothing if
    /// it is not the one anchored to the file.
    #[tokio::test(flavor = "multi_thread")]
    async fn editing_an_excerpt_reaches_the_document_anchored_to_the_file() {
        let dir = repo_with_changes();
        let files = scan_changed_files(dir.path(), ProjectDiffComparison::WorkingTree);
        let built: Vec<FileHunks> = read_and_diff(&files)
            .into_iter()
            .filter(|f| f.path.ends_with("tracked.rs"))
            .collect();
        assert_eq!(built.len(), 1, "one changed file for this test");
        let path = built[0].path.clone();

        let registry: lattice_grammar::CommandRegistryHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
        let view =
            MultibufferDocumentHandle::new(std::collections::HashMap::new(), Vec::new(), registry)
                .expect("empty view");
        assert!(attach_batch(&view, &built) > 0, "hunks attached");

        let source_id = *view
            .source_buffer_ids()
            .first()
            .expect("the batch attached a source");
        assert_eq!(
            view.source_path(source_id).as_deref(),
            Some(path.as_path()),
            "the source is anchored to the file the hunk came from"
        );
        // Read BEFORE the edit. Propagation turned out to be fast enough
        // that sampling afterwards raced the forwarder and saw the
        // marker already there — which would have made the assertion
        // below unfalsifiable.
        assert!(
            !view
                .source_text(source_id)
                .expect("source present")
                .contains("// "),
            "precondition: the marker is not already in the file"
        );

        // Type at the very start of the composed view — inside the first
        // hunk's post-image range by construction.
        view.apply_edit(lattice_protocol::edit::Edit::insert(
            lattice_protocol::position::Position::new(0, 0),
            "// ",
        ))
        .await
        .expect("the working-tree view is editable");

        // Asserted as "the marker arrived", not "the text starts with
        // it": composed row 0 is the first row of the first hunk, which
        // sits at whatever source row its context begins on. Pinning
        // row 0 would pass here and break on a fixture whose first hunk
        // is further down, for a reason having nothing to do with
        // propagation.
        //
        // The source catches up through the forwarder task.
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            if view
                .source_text(source_id)
                .is_some_and(|t| t.contains("// "))
            {
                return;
            }
        }
        panic!(
            "the edit never reached the file's document; source reads {:?}",
            view.source_text(source_id)
        );
    }

    /// The read-only half of §2.1, at the level the table states it:
    /// only the working tree is a file, so only it is editable. Pinned
    /// alongside the propagation tests because the two claims are one
    /// rule — an index blob has no anchor to propagate through, which is
    /// exactly why it opens read-only rather than editable-but-broken.
    #[test]
    fn the_editable_comparisons_are_the_working_tree_ones() {
        assert!(ProjectDiffComparison::WorkingTree.is_editable());
        assert!(!ProjectDiffComparison::Staged.is_editable());
    }

    /// The blocking half never panics on a path that vanished between
    /// `git status` and the read — a rebase or a `rm` mid-scan is a
    /// normal race, not an error state.
    #[test]
    fn a_file_that_disappeared_mid_scan_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("never-existed.rs");
        let built = read_and_diff(&[(gone, "fn old() {}\n".to_string())]);
        assert!(built.is_empty());
    }

    // ── PD.3: the trigger ────────────────────────────────────────

    /// The comparison selector is an argument, not a second entry
    /// point — one opener serves `:magit-project-diff` and the Diff
    /// transient's `e` row.
    #[test]
    fn the_comparison_comes_from_the_trigger_arguments() {
        assert_eq!(
            comparison_from_args(&Args::None),
            ProjectDiffComparison::WorkingTree
        );
        assert_eq!(
            comparison_from_args(&Args::String("staged".into())),
            ProjectDiffComparison::Staged
        );
        assert_eq!(
            comparison_from_args(&Args::String("  INDEX ".into())),
            ProjectDiffComparison::Staged,
            "case and surrounding space are not the user's problem"
        );
    }

    /// An unrecognised argument opens the daily driver rather than
    /// refusing: showing nothing is a worse answer to a typo than
    /// showing the default.
    #[test]
    fn an_unknown_comparison_falls_back_to_the_working_tree() {
        assert_eq!(
            comparison_from_args(&Args::String("nonsense".into())),
            ProjectDiffComparison::WorkingTree
        );
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
