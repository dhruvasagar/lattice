//! MG.14 — the sticky headerline every magit buffer carries.
//!
//! **What it answers.** Each magit view is a slab of git output whose
//! identity lives outside the text: `*magit:diff*` does not say which
//! scope it diffed, `*magit:blame:x.rs*` does not say which revision
//! it walked back to, the status buffer never showed the branch at
//! all (`SectionIndex::branch_status_line` was written for it and
//! never called). The headerline is the one row that answers "what am
//! I looking at?" without re-deriving it from the body.
//!
//! **One provider, every view.** There is exactly one [`Headerline`]
//! impl here, and the per-view difference is *data*: a `Vec<Field>`,
//! each field a string plus a [`FieldStyle`] naming its git role. No
//! `match buffer_kind`, no per-kind impl — adding a view means adding
//! a field-builder function, not a branch.
//!
//! **No work per tick.** The cells worker calls [`Headerline::version`]
//! every tick and [`Headerline::render`] only when it advanced.
//! [`MagitHeaderline::set`] compares before it bumps, so a refresh that
//! finds the same branch and the same counts costs one comparison and
//! no repaint (paramount goal #1). Fields are produced by the SAME
//! blocking builder that produces the buffer's text — activation and
//! `gr` alike — so the header never costs a git round-trip of its own.
//!
//! **Theme-live.** The row resolves its colours inside `render()` and
//! folds the theme's resolved version into its own, so `:colorscheme`
//! repaints the header instead of leaving it on the previous palette's
//! colours. The two headerlines that shipped before this one
//! (compilation, ai-conversation) capture `u32`s at activation and go
//! stale; this is the better shape and the cost is one uncontended
//! read-lock per tick.
//!
//! Design anchor: `docs/dev/architecture/headerline.md`, slice
//! `docs/dev/operations/slice-plans/magit.md` §MG.14.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lattice_cells::{Cell, Headerline, HeaderlineProvider, HeaderlineRow, ProviderId};
use lattice_core::BufferId;
use lattice_mode::{ModeContext, VirtualRowRegistrar};
use lattice_theme::{
    ColorRef, ElementId, ElementName, ElementOwner, StyleSpec, ThemeRegistryHandle,
};

/// Provider id tag for magit's headerline. One per buffer scope, so a
/// single constant covers every view — a magit buffer has exactly one
/// major mode and therefore exactly one header.
pub const MAGIT_HEADERLINE_PROVIDER_ID: ProviderId = 0x6d61_6769_745f_686c; // "magit_hl"

/// Separator between fields. Two spaces rather than a glyph: the
/// fields are already colour-separated, and a `·`/`|` chain reads as
/// noise at this density.
const SEP: &str = "  ";

/// MG.27: what the row says while a refresh is running.
///
/// A word rather than a `⟳` glyph, which the slice title proposed.
/// The icon-degradation rule wants a BMP fallback for every glyph
/// surface, and `⟳` (U+27F3, Miscellaneous Symbols and Arrows) is not
/// in the fallback set — while the toggle that would select between
/// them, `ui.nerd-fonts`, is buffer-local to the file tree today, not a
/// global option this row could read. A word costs three more columns
/// on a row already full of words (`clean`, `3 staged`, `AMEND`) and
/// works in every terminal font. The glyph is the natural upgrade once
/// that toggle is global.
const BUSY_TEXT: &str = "refreshing";

// ── Fields ───────────────────────────────────────────────────────────

/// The git role a header field plays. Maps to a theme element, which
/// is what gives the row its identity-by-colour (the compact format
/// carries no `Head:`-style labels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldStyle {
    /// A commit SHA (`magit.sha`).
    Sha,
    /// The checked-out / subject branch (`magit.branch.current`).
    Branch,
    /// Any other ref — upstream, rebase target, blamed revision
    /// (`magit.ref.decoration`).
    Ref,
    /// A commit author (`magit.author`).
    Author,
    /// A state the user must not miss: `AMEND`, `REBASE IN PROGRESS`
    /// (`magit.headerline.alert`).
    Alert,
    /// Counts, paths, scopes, dates — the supporting detail
    /// (`magit.headerline.label`).
    Label,
}

/// One coloured run in the header row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub text: String,
    pub style: FieldStyle,
}

impl Field {
    pub fn new(text: impl Into<String>, style: FieldStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }
    pub fn sha(text: impl Into<String>) -> Self {
        Self::new(text, FieldStyle::Sha)
    }
    pub fn branch(text: impl Into<String>) -> Self {
        Self::new(text, FieldStyle::Branch)
    }
    pub fn git_ref(text: impl Into<String>) -> Self {
        Self::new(text, FieldStyle::Ref)
    }
    pub fn author(text: impl Into<String>) -> Self {
        Self::new(text, FieldStyle::Author)
    }
    pub fn alert(text: impl Into<String>) -> Self {
        Self::new(text, FieldStyle::Alert)
    }
    pub fn label(text: impl Into<String>) -> Self {
        Self::new(text, FieldStyle::Label)
    }
}

/// Resolved theme handle + the element id per [`FieldStyle`]. Ids are
/// interned once at install; the concrete colours are read per render
/// so a live `:colorscheme` lands.
struct FieldElements {
    theme: ThemeRegistryHandle,
    sha: ElementId,
    branch: ElementId,
    reference: ElementId,
    author: ElementId,
    alert: ElementId,
    label: ElementId,
}

impl FieldElements {
    fn id_for(&self, style: FieldStyle) -> ElementId {
        match style {
            FieldStyle::Sha => self.sha,
            FieldStyle::Branch => self.branch,
            FieldStyle::Ref => self.reference,
            FieldStyle::Author => self.author,
            FieldStyle::Alert => self.alert,
            FieldStyle::Label => self.label,
        }
    }
}

/// Fallback colours for a harness with no theme registry — the header
/// still renders, just in fixed tones rather than the active palette.
fn fallback_fg(style: FieldStyle) -> u32 {
    match style {
        FieldStyle::Sha => 0x89b4fa,
        FieldStyle::Branch => 0xa6e3a1,
        FieldStyle::Ref => 0xf5c2e7,
        FieldStyle::Author => 0x9399b2,
        FieldStyle::Alert => 0xf38ba8,
        FieldStyle::Label => 0x888888,
    }
}

// ── The provider ─────────────────────────────────────────────────────

/// The one [`Headerline`] impl behind every magit buffer's header row.
pub struct MagitHeaderline {
    fields: RwLock<Vec<Field>>,
    /// Bumped by [`Self::set`] only when the fields actually changed.
    version: AtomicU64,
    /// MG.27: a refresh is running. Orthogonal to `fields` — see
    /// [`Self::set_busy`].
    busy: std::sync::atomic::AtomicBool,
    /// `None` in a harness without a theme registry.
    elements: Option<FieldElements>,
}

/// Cheap-clone handle. The mode keeps one in its per-buffer state so
/// every refresh path can re-`set` the row; the registered provider
/// holds another reference to the same allocation.
pub type MagitHeaderlineHandle = Arc<MagitHeaderline>;

impl MagitHeaderline {
    /// Build a headerline that resolves its colours through `theme`
    /// (`None` — a harness with no theme registry — falls back to
    /// fixed tones). Starts empty, so it renders nothing until the
    /// owning mode publishes its first fields.
    ///
    /// [`install`] wraps this and registers the result as a
    /// virtual-row provider; benches construct one directly to measure
    /// the row in isolation.
    pub fn new(theme: Option<ThemeRegistryHandle>, mode_id: &str) -> MagitHeaderlineHandle {
        Arc::new(Self {
            fields: RwLock::new(Vec::new()),
            version: AtomicU64::new(0),
            busy: std::sync::atomic::AtomicBool::new(false),
            elements: theme.map(|t| resolve_elements(t, mode_id)),
        })
    }

    /// Replace the row's fields. Returns `true` when something changed
    /// (and the version was bumped); `false` is the no-work path a
    /// refresh that found identical data takes.
    pub fn set(&self, fields: Vec<Field>) -> bool {
        let Ok(mut slot) = self.fields.write() else {
            return false;
        };
        if *slot == fields {
            return false;
        }
        *slot = fields;
        self.version.fetch_add(1, Ordering::Release);
        true
    }

    /// MG.27: mark a refresh as in flight (or finished).
    ///
    /// Kept OUT of `fields` deliberately. Every refresh path ends by
    /// calling [`Self::set`] with freshly-computed fields, so a busy
    /// marker living in that vector would be wiped by the very
    /// completion it is supposed to survive until — and worse, would
    /// have to be re-added by each builder, which is a rule every new
    /// view could forget. A separate flag composes instead: builders
    /// stay ignorant of it and cannot drop it.
    ///
    /// Returns `true` when the state actually changed, matching
    /// [`Self::set`]'s contract — the version is only bumped then, so
    /// a refresh that starts and finishes between two ticks costs no
    /// repaint.
    pub fn set_busy(&self, busy: bool) -> bool {
        if self.busy.swap(busy, Ordering::AcqRel) == busy {
            return false;
        }
        self.version.fetch_add(1, Ordering::Release);
        true
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// The row's own content version, ignoring the theme. Test seam —
    /// [`Headerline::version`] folds the theme's version in, which
    /// would make "did the content change?" unobservable on its own.
    pub fn content_version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// The plain text of the current row, separators included. Used by
    /// tests and `:describe-buffer`-style introspection; `render()` is
    /// what the worker paints.
    pub fn text(&self) -> String {
        self.fields
            .read()
            .map(|f| {
                f.iter()
                    .map(|f| f.text.as_str())
                    .chain(self.is_busy().then_some(BUSY_TEXT))
                    .collect::<Vec<_>>()
                    .join(SEP)
            })
            .unwrap_or_default()
    }
}

impl std::fmt::Debug for MagitHeaderline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MagitHeaderline")
            .field("version", &self.content_version())
            .field("text", &self.text())
            .finish()
    }
}

impl Headerline for MagitHeaderline {
    fn version(&self) -> u64 {
        // Fold the theme's resolved version in so a palette swap
        // repaints the row. `resolved()` is a read-lock plus an
        // ArcSwap load — no rebuild unless the theme is dirty — and
        // this runs on the cells worker, never the UI thread.
        let theme_version = self
            .elements
            .as_ref()
            .map(|e| e.theme.resolved().version())
            .unwrap_or(0);
        self.content_version().wrapping_add(theme_version)
    }

    fn render(&self) -> Option<HeaderlineRow> {
        let fields = self.fields.read().ok()?;
        if fields.is_empty() {
            // Nothing known yet (the buffer opened, git has not
            // answered). Hide the row rather than paint an empty bar
            // that shifts the content down and back up again.
            return None;
        }
        let resolved = self.elements.as_ref().map(|e| (e, e.theme.resolved()));
        let fg = |style: FieldStyle| -> u32 {
            resolved
                .as_ref()
                .and_then(|(e, table)| table.get(e.id_for(style)).fg)
                .map(|c| c.to_rgb_u32(0))
                .unwrap_or_else(|| fallback_fg(style))
        };

        let mut cells: Vec<Cell> = Vec::new();
        let label_fg = fg(FieldStyle::Label);
        cells.push(Cell::new(' ' as u32, label_fg, 0, 0));
        // MG.27: appended, not woven in, so it never displaces a field
        // and a view that gains fields later needs no change here.
        let busy = self.is_busy().then_some(Field::label(BUSY_TEXT));
        for (i, field) in fields.iter().chain(busy.iter()).enumerate() {
            if i > 0 {
                cells.extend(SEP.chars().map(|c| Cell::new(c as u32, label_fg, 0, 0)));
            }
            let colour = fg(field.style);
            cells.extend(
                field
                    .text
                    .chars()
                    .map(|c| Cell::new(c as u32, colour, 0, 0)),
            );
        }
        cells.push(Cell::new(' ' as u32, label_fg, 0, 0));

        Some(HeaderlineRow {
            cells: cells.into(),
            bg: None,
        })
    }
}

// ── Installation ─────────────────────────────────────────────────────

/// Removes the buffer's headerline provider when the mode deactivates.
///
/// The mode owns its full surface: nothing else in the host knows this
/// provider exists, so nothing else can clean it up.
pub struct HeaderlineRegistration {
    registrar: Arc<dyn VirtualRowRegistrar>,
    buffer: BufferId,
}

impl Drop for HeaderlineRegistration {
    fn drop(&mut self) {
        self.registrar
            .unregister(self.buffer, MAGIT_HEADERLINE_PROVIDER_ID);
    }
}

/// Build a headerline for `buffer` and register it as a virtual-row
/// provider. Returns the handle the mode keeps (to `set` fields as its
/// data lands) and the registration whose drop tears the row down.
///
/// Synchronous and cheap — call it above the first `.await` in
/// `on_activate`, alongside the state publish, so the row exists (and
/// stays hidden, rendering `None`) from the moment the buffer opens.
/// Returns `None` only in a harness with no virtual-row registrar.
pub fn install(
    ctx: &ModeContext,
    buffer: BufferId,
    mode_id: &str,
) -> Option<(MagitHeaderlineHandle, HeaderlineRegistration)> {
    let registrar: Arc<dyn VirtualRowRegistrar> = ctx
        .service::<Arc<dyn VirtualRowRegistrar>>()
        .map(|outer| (*outer).clone())?;

    let theme = ctx
        .service::<ThemeRegistryHandle>()
        .map(|outer| (*outer).clone());
    let headerline = MagitHeaderline::new(theme, mode_id);

    let provider = Arc::new(HeaderlineProvider::new(
        MAGIT_HEADERLINE_PROVIDER_ID,
        headerline.clone() as Arc<dyn Headerline>,
    ));
    // `register` refuses to replace a live id, so clear whatever a
    // previous activation on this buffer left behind — a reopened
    // magit buffer must bind its own header, not keep the stale one.
    registrar.unregister(buffer, MAGIT_HEADERLINE_PROVIDER_ID);
    registrar.register(buffer, provider);

    Some((headerline, HeaderlineRegistration { registrar, buffer }))
}

/// Intern the element ids the row paints with.
///
/// The four git-role colours are the MG.11 palette, already registered
/// as builtins because `lattice-syntax`'s styled-span table resolves
/// them by builtin id. The two header-only roles are registered HERE,
/// owned by the mode — the host has no business naming a magit
/// element that only magit paints. `register` is idempotent by name,
/// so repeat activations re-intern rather than duplicate.
fn resolve_elements(theme: ThemeRegistryHandle, mode_id: &str) -> FieldElements {
    let owner = ElementOwner::Mode(mode_id.to_string().into());
    let alert = theme.register(
        ElementName::from_static("magit.headerline.alert"),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("red".into())).bold(),
        "Magit headerline: a state the user must not miss (`AMEND`, `REBASE IN PROGRESS`).",
    );
    let label = theme.register(
        ElementName::from_static("magit.headerline.label"),
        owner,
        StyleSpec::new().fg(ColorRef::Palette("muted".into())),
        "Magit headerline: supporting detail — counts, paths, scopes, dates, separators.",
    );
    let by_name = |name: &'static str| {
        theme
            .id(&ElementName::from_static(name))
            .unwrap_or(ElementId::INVALID)
    };
    let (sha, branch, reference, author) = (
        by_name("magit.sha"),
        by_name("magit.branch.current"),
        by_name("magit.ref.decoration"),
        by_name("magit.author"),
    );
    FieldElements {
        theme,
        sha,
        branch,
        reference,
        author,
        alert,
        label,
    }
}

/// MG.26b: the two element ids a blame chunk-heading paints with.
///
/// Reuses the same interning `resolve_elements` does — `register` is
/// idempotent by name — so a heading's sha is the same colour as a
/// sha anywhere else in magit, and a `:colorscheme` moves both.
pub(crate) fn intern_blame_heading_elements(
    theme: &ThemeRegistryHandle,
    mode_id: &str,
) -> (ElementId, ElementId) {
    let e = resolve_elements(theme.clone(), mode_id);
    (e.sha, e.label)
}

/// MG.26b: the fallback colours for those two, for a harness with no
/// theme registry. Same table the headerline falls back to.
pub(crate) fn blame_heading_fallback() -> (u32, u32) {
    (fallback_fg(FieldStyle::Sha), fallback_fg(FieldStyle::Label))
}

/// Push `fields` into an optional handle — the shape every mode's
/// refresh path uses, since `install` yields `None` in a stripped
/// harness.
pub(crate) fn publish(handle: &Option<MagitHeaderlineHandle>, fields: Vec<Field>) {
    if let Some(h) = handle {
        h.set(fields);
    }
}

/// MG.27: mark this view busy until the returned guard drops.
///
/// **A guard rather than a matching pair of calls**, because a refresh
/// has several ways out — an early `return` when the buffer handle is
/// gone, a `spawn_blocking` that panics, a task cancelled when the
/// buffer closes — and every one of them would leave the row saying
/// "refreshing" forever. `Drop` runs on all of them. The one thing it
/// cannot survive is the whole process going away, which takes the row
/// with it.
///
/// Cheap when there is no headerline (a harness without a registry):
/// the guard holds `None` and does nothing.
#[must_use = "the row stays busy until this guard drops"]
pub(crate) fn busy(handle: &Option<MagitHeaderlineHandle>) -> BusyGuard {
    if let Some(h) = handle {
        h.set_busy(true);
    }
    BusyGuard(handle.clone())
}

pub(crate) struct BusyGuard(Option<MagitHeaderlineHandle>);

impl Drop for BusyGuard {
    fn drop(&mut self) {
        if let Some(h) = &self.0 {
            h.set_busy(false);
        }
    }
}

// ── Per-view field builders ──────────────────────────────────────────
//
// The views differ HERE and nowhere else: same provider, same
// render path, different data. Each builder is pure, so what a view
// claims to show is testable without opening a buffer or touching git.
// Every builder runs inside the same `spawn_blocking` that produced
// the buffer's text, from the primitives that call already had.

use std::path::Path;

use crate::sections::{SectionIndex, SectionKind};

/// The repository's own name — the workdir's last path component.
/// Present on the status header because a second lattice window on a
/// second checkout is otherwise indistinguishable.
pub(crate) fn repo_name(workdir: &Path) -> String {
    workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// magit-status: repo, branch, ahead/behind, dirty counts.
pub(crate) fn status_fields(index: &SectionIndex, workdir: &Path) -> Vec<Field> {
    let mut fields = Vec::new();
    let repo = repo_name(workdir);
    if !repo.is_empty() {
        fields.push(Field::label(repo));
    }
    if !index.branch.is_empty() {
        let mut branch = index.branch.clone();
        if index.ahead > 0 {
            branch.push_str(&format!(" \u{2191}{}", index.ahead));
        }
        if index.behind > 0 {
            branch.push_str(&format!(" \u{2193}{}", index.behind));
        }
        fields.push(Field::branch(branch));
    }
    let count = |kind: SectionKind| {
        index
            .sections
            .iter()
            .find(|s| s.kind == kind)
            .map(|s| s.entries.len())
            .unwrap_or(0)
    };
    // MG.21f: the bisect alert goes in BEFORE the clean-tree early
    // return. Bisecting a clean tree is the normal case — git checks
    // out each candidate for you — so putting it after would hide the
    // alert exactly when it is always true, and the user would be
    // testing a detached HEAD with nothing on screen saying why.
    if let Some(bisect) = &index.bisect {
        fields.push(Field::alert(bisect_label(bisect)));
    }
    let (staged, unstaged, untracked) = (
        count(SectionKind::Staged),
        count(SectionKind::Unstaged),
        count(SectionKind::Untracked),
    );
    if staged == 0 && unstaged == 0 && untracked == 0 {
        fields.push(Field::label("clean"));
        return fields;
    }
    if staged > 0 {
        fields.push(Field::label(format!("{staged} staged")));
    }
    if unstaged > 0 {
        fields.push(Field::label(format!("{unstaged} unstaged")));
    }
    if untracked > 0 {
        fields.push(Field::label(format!("{untracked} untracked")));
    }
    fields
}

/// Files touched / lines added / lines removed in a unified diff.
/// Counts `diff --git` headers rather than `+++`/`---` pairs so a
/// pure-rename or mode-change entry still counts as a file.
pub(crate) fn diff_counts(diff: &str) -> (usize, usize, usize) {
    let mut files = 0;
    let mut added = 0;
    let mut removed = 0;
    for line in diff.lines() {
        if line.starts_with("diff --git") {
            files += 1;
        } else if line.starts_with("+++") || line.starts_with("---") {
            continue;
        } else if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (files, added, removed)
}

/// magit-commit: the branch being committed to, what is staged, and
/// whether this rewrites the previous commit.
pub(crate) fn commit_fields(branch: &str, staged_diff: &str, amend: bool) -> Vec<Field> {
    let mut fields = Vec::new();
    if !branch.is_empty() {
        fields.push(Field::branch(branch.to_string()));
    }
    let (files, added, removed) = diff_counts(staged_diff);
    if files == 0 {
        fields.push(Field::label("nothing staged"));
    } else {
        let plural = if files == 1 { "file" } else { "files" };
        fields.push(Field::label(format!(
            "{files} {plural} +{added} \u{2212}{removed}"
        )));
    }
    if amend {
        fields.push(Field::alert("AMEND"));
    }
    fields
}

/// One commit's identity, as `git show -s` reports it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct RevisionMeta {
    pub sha: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// Parse the NUL-separated `%h%x00%an%x00%ar%x00%s` format the
/// revision view asks for. Chosen over scraping `git show`'s human
/// header because that output is locale- and config-dependent
/// (`log.date`, `i18n.logOutputEncoding`), while `--format` is not.
pub(crate) fn parse_revision_meta(raw: &str) -> RevisionMeta {
    let mut parts = raw.trim_end_matches('\n').split('\0');
    RevisionMeta {
        sha: parts.next().unwrap_or_default().to_string(),
        author: parts.next().unwrap_or_default().to_string(),
        date: parts.next().unwrap_or_default().to_string(),
        subject: parts.next().unwrap_or_default().to_string(),
    }
}

/// magit-revision: short SHA, author, relative date, subject.
pub(crate) fn revision_fields(meta: &RevisionMeta) -> Vec<Field> {
    let mut fields = Vec::new();
    if !meta.sha.is_empty() {
        fields.push(Field::sha(meta.sha.clone()));
    }
    if !meta.author.is_empty() {
        fields.push(Field::author(meta.author.clone()));
    }
    if !meta.date.is_empty() {
        fields.push(Field::label(meta.date.clone()));
    }
    if !meta.subject.is_empty() {
        fields.push(Field::label(meta.subject.clone()));
    }
    fields
}

/// magit-file-revision: `<path> @ <ref>`. The `staged` pseudo-ref
/// reads as `@ index` — "staged" names how it got there, `index`
/// names where it is, and the latter is what the user is looking at.
pub(crate) fn file_revision_fields(git_ref: &str, path: &Path) -> Vec<Field> {
    let mut fields = vec![Field::label(path.display().to_string()), Field::label("@")];
    if git_ref == "staged" {
        fields.push(Field::git_ref("index"));
    } else {
        fields.push(Field::sha(git_ref.to_string()));
    }
    fields
}

/// magit-diff: which scope was diffed, and the path when file-scoped.
pub(crate) fn diff_fields(scope: &str, path: Option<&Path>) -> Vec<Field> {
    let mut fields = vec![Field::git_ref(scope.to_string())];
    if let Some(p) = path {
        fields.push(Field::label(p.display().to_string()));
    }
    fields
}

/// magit-log: the ref being logged, how many commits are shown, and
/// the path filter when file-scoped. `commits` counts rendered commit
/// rows, not `--graph` connector lines.
pub(crate) fn log_fields(git_ref: &str, commits: usize, path: Option<&Path>) -> Vec<Field> {
    let mut fields = vec![Field::git_ref(git_ref.to_string())];
    let plural = if commits == 1 { "commit" } else { "commits" };
    fields.push(Field::label(format!("{commits} {plural}")));
    if let Some(p) = path {
        fields.push(Field::label(p.display().to_string()));
    }
    fields
}

/// magit-blame: the path and the revision currently blamed — `p`
/// walks the revision back, and without this the buffer gives no clue

/// magit-branch: the checked-out branch and how many exist.
pub(crate) fn branch_fields(current: &str, total: usize) -> Vec<Field> {
    let mut fields = Vec::new();
    if !current.is_empty() {
        fields.push(Field::branch(current.to_string()));
    }
    let plural = if total == 1 { "branch" } else { "branches" };
    fields.push(Field::label(format!("{total} {plural}")));
    fields
}

/// MG.40 — magit-cherry: what is being compared, and the two counts.
///
/// The counts are the answer the buffer exists to give, and they are
/// kept apart rather than summed: "3 ahead" and "3 already upstream"
/// call for opposite actions, and a single total would hide which you
/// are looking at.
pub(crate) fn cherry_fields(
    upstream: &str,
    head: &str,
    ahead: usize,
    equivalent: usize,
) -> Vec<Field> {
    let mut fields = Vec::new();
    if !head.is_empty() {
        fields.push(Field::branch(head.to_string()));
    }
    if !upstream.is_empty() {
        fields.push(Field::git_ref(format!("vs {upstream}")));
    }
    fields.push(Field::label(format!("{ahead} ahead")));
    if equivalent > 0 {
        fields.push(Field::label(format!("{equivalent} already upstream")));
    }
    fields
}

/// MG.37 — magit-notes: which commit the note belongs to, and whether
/// there already is one.
///
/// The commit identity matters more here than in most magit buffers:
/// this buffer is a bare text area with nothing in its *content* naming
/// what it is attached to, so without the row a note written against
/// the wrong commit looks identical to one written against the right
/// one. "new" vs "editing" is the second half of that — it says whether
/// `C-c C-c` will create or overwrite.
pub(crate) fn note_fields(meta: &RevisionMeta, has_existing: bool) -> Vec<Field> {
    let mut fields = vec![Field::label(
        if has_existing { "editing note" } else { "new note" }.to_string(),
    )];
    fields.extend(revision_fields(meta));
    fields
}

/// MG.35 — magit-refs: how many of each kind.
///
/// Three counts rather than one total, because "47 refs" answers
/// nothing: the question this buffer is opened with is usually about
/// one of the three groups, and the row says which are worth scrolling
/// to. Empty groups are omitted here for the same reason the buffer
/// omits their headings.
pub(crate) fn refs_fields(branches: usize, remotes: usize, tags: usize) -> Vec<Field> {
    let mut fields = Vec::new();
    for (n, singular, plural) in [
        (branches, "branch", "branches"),
        (remotes, "remote", "remotes"),
        (tags, "tag", "tags"),
    ] {
        if n > 0 {
            let word = if n == 1 { singular } else { plural };
            fields.push(Field::label(format!("{n} {word}")));
        }
    }
    if fields.is_empty() {
        fields.push(Field::label("no refs".to_string()));
    }
    fields
}

/// MG.21f — the bisect alert's text.
///
/// Mirrors git's own "Bisecting: N revisions left to test after this
/// (roughly M steps)" rather than inventing a phrasing, because the
/// user is reading git's line in a terminal at the same time. Degrades
/// to the bare word when git has no numbers to give (a bad end marked
/// but no good one yet) — an alert with a missing count still says the
/// thing that matters, which is that HEAD is not where you left it.
pub(crate) fn bisect_label(state: &lattice_vcs::BisectState) -> String {
    match (state.revisions_left, state.steps) {
        (Some(left), Some(steps)) => format!("BISECTING {left} left, ~{steps} steps"),
        (Some(left), None) => format!("BISECTING {left} left"),
        _ => "BISECTING".to_string(),
    }
}

/// MG.21i — magit-submodule: how many, and how many need attention.
///
/// The second half is the reason this is not just a count: an
/// uninitialised or modified submodule is the thing you opened the
/// buffer to deal with, and a bare "4 submodules" would not say that
/// any of them need anything.
pub(crate) fn submodule_fields(entries: &[lattice_vcs::SubmoduleEntry]) -> Vec<Field> {
    use lattice_vcs::SubmoduleState;
    let plural = if entries.len() == 1 {
        "submodule"
    } else {
        "submodules"
    };
    let mut fields = vec![Field::label(format!("{} {plural}", entries.len()))];
    let uninit = entries
        .iter()
        .filter(|e| e.state == SubmoduleState::Uninitialised)
        .count();
    if uninit > 0 {
        fields.push(Field::alert(format!("{uninit} uninitialised")));
    }
    let modified = entries
        .iter()
        .filter(|e| e.state == SubmoduleState::Modified)
        .count();
    if modified > 0 {
        fields.push(Field::label(format!("{modified} modified")));
    }
    let conflicted = entries
        .iter()
        .filter(|e| e.state == SubmoduleState::Conflicted)
        .count();
    if conflicted > 0 {
        fields.push(Field::alert(format!("{conflicted} conflicted")));
    }
    fields
}

/// MG.21c — magit-remote: how many remotes are configured.
pub(crate) fn remote_fields(total: usize) -> Vec<Field> {
    let plural = if total == 1 { "remote" } else { "remotes" };
    vec![Field::label(format!("{total} {plural}"))]
}

/// magit-stash: how many stashes are held.
pub(crate) fn stash_fields(total: usize) -> Vec<Field> {
    let plural = if total == 1 { "stash" } else { "stashes" };
    vec![Field::label(format!("{total} {plural}"))]
}

/// MG.15 — magit-stash-show: which stash, and its subject. The ref is
/// styled as a ref rather than a SHA: `stash@{2}` is a name that
/// renumbers when its neighbours are dropped, not a fixed commit.
pub(crate) fn stash_show_fields(index: usize, message: &str) -> Vec<Field> {
    let mut fields = vec![Field::git_ref(format!("stash@{{{index}}}"))];
    if !message.is_empty() {
        fields.push(Field::label(message.to_string()));
    }
    fields
}

/// magit-rebase: the upstream being rebased onto, how many commits
/// the todo carries, and whether a rebase is already running (in
/// which case `C-c C-c` would compound it, so the state is an alert).
pub(crate) fn rebase_fields(upstream: &str, commits: usize, in_progress: bool) -> Vec<Field> {
    let mut fields = Vec::new();
    if !upstream.is_empty() {
        fields.push(Field::label("onto"));
        fields.push(Field::git_ref(upstream.to_string()));
    }
    let plural = if commits == 1 { "commit" } else { "commits" };
    fields.push(Field::label(format!("{commits} {plural}")));
    if in_progress {
        fields.push(Field::alert("REBASE IN PROGRESS"));
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(fields: Vec<Field>) -> MagitHeaderlineHandle {
        let hl = MagitHeaderline::new(None, "test");
        hl.set(fields);
        hl
    }

    #[test]
    fn empty_fields_hide_the_row() {
        assert!(bare(Vec::new()).render().is_none());
    }

    #[test]
    fn render_emits_one_padded_row_of_every_field() {
        let hl = bare(vec![Field::branch("main"), Field::label("3 staged")]);
        let row = hl.render().expect("non-empty fields render");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or(' '))
            .collect();
        assert_eq!(text, " main  3 staged ");
    }

    #[test]
    fn each_field_paints_in_its_own_colour() {
        let hl = bare(vec![Field::sha("a1b2c3d"), Field::label("today")]);
        let row = hl.render().unwrap();
        let sha_fg = row.cells[1].fg;
        let label_fg = row.cells[row.cells.len() - 2].fg;
        assert_eq!(sha_fg, fallback_fg(FieldStyle::Sha));
        assert_eq!(label_fg, fallback_fg(FieldStyle::Label));
        assert_ne!(sha_fg, label_fg, "roles must be distinguishable by colour");
    }

    /// The no-work-per-tick guarantee. A refresh that finds the same
    /// branch and the same counts must not bump the version — the
    /// worker would otherwise rebuild and repaint the row on every
    /// `gr` and every auto-refresh (paramount goal #1).
    #[test]
    fn setting_identical_fields_does_not_advance_the_version() {
        let hl = bare(Vec::new());
        assert!(hl.set(vec![Field::branch("main")]), "first set changes");
        let v = hl.content_version();
        assert!(!hl.set(vec![Field::branch("main")]), "identical is no-work");
        assert_eq!(hl.content_version(), v);
    }

    /// The other half: a background refresh that DID find new data
    /// bumps exactly once, not once per field.
    #[test]
    fn a_changed_refresh_bumps_the_version_exactly_once() {
        let hl = bare(Vec::new());
        hl.set(vec![Field::branch("main"), Field::label("3 staged")]);
        let v = hl.content_version();
        assert!(hl.set(vec![Field::branch("main"), Field::label("4 staged")]));
        assert_eq!(hl.content_version(), v + 1);
    }

    /// The teardown contract: dropping the registration unregisters
    /// the provider, so the sticky row cannot outlive the mode.
    ///
    /// Tested here rather than through `:bd` in the host harness
    /// because `Editor::do_buffer_delete` currently removes the buffer
    /// from the registry without removing its `active_modes` entry —
    /// so no mode's Drop runs on buffer delete, and a host-level test
    /// would be asserting on a host gap rather than on this code. See
    /// the note in `lattice-ui-tui/src/app/magit_bindings.rs`.
    #[test]
    fn dropping_the_registration_unregisters_the_provider() {
        #[derive(Default)]
        struct FakeRegistrar {
            unregistered: std::sync::Mutex<Vec<(BufferId, ProviderId)>>,
        }
        impl VirtualRowRegistrar for FakeRegistrar {
            fn register(
                &self,
                _buffer: BufferId,
                _provider: Arc<dyn lattice_cells::VirtualRowProvider>,
            ) -> bool {
                true
            }
            fn unregister(&self, buffer: BufferId, id: ProviderId) -> bool {
                self.unregistered.lock().unwrap().push((buffer, id));
                true
            }
        }

        let registrar = Arc::new(FakeRegistrar::default());
        let registration = HeaderlineRegistration {
            registrar: registrar.clone(),
            buffer: BufferId(7),
        };
        assert!(registrar.unregistered.lock().unwrap().is_empty());
        drop(registration);
        assert_eq!(
            *registrar.unregistered.lock().unwrap(),
            vec![(BufferId(7), MAGIT_HEADERLINE_PROVIDER_ID)],
            "the mode owns its full surface — teardown included"
        );
    }

    #[test]
    fn text_joins_fields_with_the_separator() {
        let hl = bare(vec![
            Field::git_ref("origin/main"),
            Field::label("4 commits"),
        ]);
        assert_eq!(hl.text(), "origin/main  4 commits");
    }

    // ── Per-view builders ────────────────────────────────────────────
    //
    // Every view must answer "what am I looking at?" — so each test
    // below asserts the row is non-empty AND carries that view's
    // identifying field. A builder that silently returned an empty vec
    // would hide the row, which is the failure this slice exists to
    // remove.

    fn rendered(fields: Vec<Field>) -> String {
        assert!(!fields.is_empty(), "a view must publish a non-empty row");
        bare(fields).text()
    }

    fn status_index(branch: &str, ahead: usize, behind: usize) -> SectionIndex {
        SectionIndex {
            sections: Vec::new(),
            branch: branch.to_string(),
            ahead,
            behind,
            bisect: None,
        }
    }

    fn bisecting(
        mut index: SectionIndex,
        revisions_left: Option<usize>,
        steps: Option<usize>,
    ) -> SectionIndex {
        index.bisect = Some(lattice_vcs::BisectState {
            revisions_left,
            steps,
            start_ref: "main".into(),
        });
        index
    }

    /// MG.21f: bisecting a CLEAN tree is the normal case — git checks
    /// out each candidate for you — so the alert must survive the
    /// clean-tree early return. It did not, in the first draft.
    #[test]
    fn the_bisect_alert_shows_on_a_clean_tree() {
        let row = rendered(status_fields(
            &bisecting(status_index("main", 0, 0), Some(3), Some(2)),
            Path::new("/src/lattice"),
        ));
        assert!(
            row.contains("BISECTING 3 left, ~2 steps"),
            "clean-tree row lost the alert: {row}"
        );
        assert!(
            row.contains("clean"),
            "and still says the tree is clean: {row}"
        );
    }

    #[test]
    fn the_bisect_alert_shows_alongside_dirty_counts() {
        let index = with_section(
            bisecting(status_index("main", 0, 0), Some(1), Some(1)),
            SectionKind::Unstaged,
            2,
        );
        let row = rendered(status_fields(&index, Path::new("/src/lattice")));
        assert!(row.contains("BISECTING"), "{row}");
        assert!(row.contains("2 unstaged"), "{row}");
    }

    /// The numbers are git's, so when git has none the label degrades
    /// rather than inventing a zero — "0 left" would read as finished.
    #[test]
    fn the_bisect_alert_degrades_when_git_has_no_numbers() {
        let state = lattice_vcs::BisectState {
            revisions_left: None,
            steps: None,
            start_ref: "main".into(),
        };
        assert_eq!(bisect_label(&state), "BISECTING");
        assert_eq!(
            bisect_label(&lattice_vcs::BisectState {
                revisions_left: Some(4),
                steps: None,
                start_ref: "main".into(),
            }),
            "BISECTING 4 left"
        );
    }

    #[test]
    fn no_bisect_means_no_alert() {
        let row = rendered(status_fields(
            &status_index("main", 0, 0),
            Path::new("/src/lattice"),
        ));
        assert!(!row.contains("BISECT"), "{row}");
    }

    fn with_section(mut index: SectionIndex, kind: SectionKind, entries: usize) -> SectionIndex {
        use crate::sections::{Section, SectionEntry};
        index.sections.push(Section {
            kind,
            header_line: 0,
            body_start: 1,
            body_end: 1 + entries,
            entries: (0..entries)
                .map(|i| SectionEntry::File {
                    path: std::path::PathBuf::from(format!("f{i}.rs")),
                    status: lattice_vcs::PathStatus::Modified,
                })
                .collect(),
        });
        index
    }

    #[test]
    fn status_row_carries_branch_ahead_behind_and_counts() {
        let index = with_section(
            with_section(status_index("main", 2, 1), SectionKind::Staged, 3),
            SectionKind::Unstaged,
            5,
        );
        let row = rendered(status_fields(&index, Path::new("/src/lattice")));
        assert_eq!(
            row,
            "lattice  main \u{2191}2 \u{2193}1  3 staged  5 unstaged"
        );
    }

    #[test]
    fn status_row_says_clean_rather_than_listing_three_zeroes() {
        let row = rendered(status_fields(&status_index("main", 0, 0), Path::new("/x")));
        assert_eq!(row, "x  main  clean");
    }

    #[test]
    fn diff_counts_counts_files_adds_and_removes_ignoring_file_markers() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,2 @@\n+one\n+two\n-gone\n ctx\n";
        assert_eq!(diff_counts(diff), (1, 2, 1));
    }

    #[test]
    fn commit_row_carries_branch_staged_counts_and_the_amend_marker() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n+one\n-two\n";
        assert_eq!(
            rendered(commit_fields("main", diff, false)),
            "main  1 file +1 \u{2212}1"
        );
        assert!(
            rendered(commit_fields("main", diff, true)).contains("AMEND"),
            "amend must be visible — it rewrites history"
        );
    }

    #[test]
    fn commit_row_names_an_empty_index_rather_than_showing_zeroes() {
        assert_eq!(
            rendered(commit_fields("main", "", false)),
            "main  nothing staged"
        );
    }

    #[test]
    fn revision_row_carries_sha_author_date_and_subject() {
        let meta = parse_revision_meta("a1b2c3d\0Jane Doe\03 days ago\0Fix the thing\n");
        assert_eq!(
            meta,
            RevisionMeta {
                sha: "a1b2c3d".into(),
                author: "Jane Doe".into(),
                date: "3 days ago".into(),
                subject: "Fix the thing".into(),
            }
        );
        assert_eq!(
            rendered(revision_fields(&meta)),
            "a1b2c3d  Jane Doe  3 days ago  Fix the thing"
        );
    }

    /// A subject containing NUL is impossible, but a truncated read is
    /// not — the parser must degrade to fewer fields, never panic.
    #[test]
    fn revision_meta_tolerates_a_short_read() {
        let meta = parse_revision_meta("a1b2c3d\0Jane Doe");
        assert_eq!(meta.date, "");
        assert_eq!(rendered(revision_fields(&meta)), "a1b2c3d  Jane Doe");
    }

    #[test]
    fn file_revision_row_reads_path_at_ref() {
        assert_eq!(
            rendered(file_revision_fields("a1b2c3d", Path::new("src/main.rs"))),
            "src/main.rs  @  a1b2c3d"
        );
    }

    #[test]
    fn file_revision_row_names_the_staged_pseudo_ref_index() {
        assert_eq!(
            rendered(file_revision_fields("staged", Path::new("src/main.rs"))),
            "src/main.rs  @  index"
        );
    }

    #[test]
    fn diff_row_carries_scope_and_optional_path() {
        assert_eq!(rendered(diff_fields("HEAD", None)), "HEAD");
        assert_eq!(
            rendered(diff_fields("staged", Some(Path::new("src/main.rs")))),
            "staged  src/main.rs"
        );
    }

    #[test]
    fn log_row_carries_ref_commit_count_and_path_filter() {
        assert_eq!(rendered(log_fields("HEAD", 50, None)), "HEAD  50 commits");
        assert_eq!(
            rendered(log_fields("HEAD", 1, Some(Path::new("src/main.rs")))),
            "HEAD  1 commit  src/main.rs"
        );
    }

    #[test]
    fn branch_row_carries_current_branch_and_total() {
        assert_eq!(rendered(branch_fields("main", 12)), "main  12 branches");
        assert_eq!(rendered(branch_fields("main", 1)), "main  1 branch");
    }

    #[test]
    fn submodule_row_counts_and_flags_the_ones_needing_attention() {
        use lattice_vcs::{SubmoduleEntry, SubmoduleState as St};
        let e = |state| SubmoduleEntry {
            state,
            sha: "abc".into(),
            path: "vendor/x".into(),
            describe: String::new(),
        };
        assert_eq!(rendered(submodule_fields(&[e(St::InSync)])), "1 submodule");
        let row = rendered(submodule_fields(&[
            e(St::InSync),
            e(St::Uninitialised),
            e(St::Modified),
            e(St::Conflicted),
        ]));
        assert!(row.contains("4 submodules"), "{row}");
        assert!(row.contains("1 uninitialised"), "{row}");
        assert!(row.contains("1 modified"), "{row}");
        assert!(row.contains("1 conflicted"), "{row}");
    }

    #[test]
    fn an_all_clean_submodule_row_says_nothing_more_than_the_count() {
        use lattice_vcs::{SubmoduleEntry, SubmoduleState as St};
        let e = SubmoduleEntry {
            state: St::InSync,
            sha: "abc".into(),
            path: "vendor/x".into(),
            describe: String::new(),
        };
        let row = rendered(submodule_fields(&[e.clone(), e]));
        assert_eq!(row, "2 submodules", "no zero-counts padding the row: {row}");
    }

    // ── MG.27: the in-flight indicator ───────────────────────────

    #[test]
    fn a_busy_row_says_so_after_its_fields() {
        let hl = bare(vec![Field::branch("main"), Field::label("clean")]);
        assert_eq!(hl.text(), "main  clean");
        assert!(hl.set_busy(true));
        assert_eq!(hl.text(), "main  clean  refreshing");
        assert!(hl.set_busy(false));
        assert_eq!(hl.text(), "main  clean");
    }

    /// The whole reason the flag is not a `Field`: every refresh ends
    /// by REPLACING the field vector, so a marker living in it would be
    /// wiped by the very completion it must outlive.
    #[test]
    fn publishing_fields_does_not_clear_the_busy_marker() {
        let hl = bare(vec![Field::label("clean")]);
        hl.set_busy(true);
        hl.set(vec![Field::branch("main"), Field::label("2 staged")]);
        assert!(hl.is_busy(), "a field publish must not clear busy");
        assert!(hl.text().ends_with("refreshing"), "{}", hl.text());
    }

    /// Only a real change repaints — a refresh that starts and finishes
    /// between two ticks must cost nothing.
    #[test]
    fn setting_busy_to_what_it_already_is_bumps_no_version() {
        let hl = bare(vec![Field::label("clean")]);
        let before = hl.content_version();
        assert!(hl.set_busy(true));
        let after_set = hl.content_version();
        assert!(after_set > before);
        assert!(!hl.set_busy(true), "no change, no bump");
        assert_eq!(hl.content_version(), after_set);
    }

    /// The guard is what survives an early return or a panic inside a
    /// refresh — the failure mode being a row stuck on "refreshing".
    #[test]
    fn the_busy_guard_clears_on_drop() {
        let hl = Some(bare(vec![Field::label("clean")]));
        {
            let _guard = busy(&hl);
            assert!(hl.as_ref().unwrap().is_busy());
        }
        assert!(
            !hl.as_ref().unwrap().is_busy(),
            "dropping the guard must clear it, however the scope was left"
        );
    }

    #[test]
    fn the_busy_guard_is_harmless_without_a_headerline() {
        // A harness with no theme/registry has `None` here; taking the
        // guard must not panic or require a handle.
        let none: Option<MagitHeaderlineHandle> = None;
        drop(busy(&none));
    }

    /// A busy row still renders its fields — the marker is appended,
    /// not substituted, so nothing the user was reading disappears
    /// while a refresh runs.
    #[test]
    fn a_busy_row_still_renders_every_field() {
        let hl = bare(vec![Field::branch("main"), Field::label("3 staged")]);
        hl.set_busy(true);
        let row = hl.render().expect("non-empty fields render");
        let text: String = row
            .cells
            .iter()
            .map(|c| char::from_u32(c.codepoint).unwrap_or(' '))
            .collect();
        assert_eq!(text, " main  3 staged  refreshing ");
    }

    #[test]
    fn remote_row_carries_the_count() {
        assert_eq!(rendered(remote_fields(2)), "2 remotes");
        assert_eq!(rendered(remote_fields(1)), "1 remote");
        assert_eq!(rendered(remote_fields(0)), "0 remotes");
    }

    #[test]
    fn stash_row_carries_the_count() {
        assert_eq!(rendered(stash_fields(3)), "3 stashes");
        assert_eq!(rendered(stash_fields(0)), "0 stashes");
    }

    #[test]
    fn rebase_row_carries_upstream_count_and_in_progress_alert() {
        assert_eq!(
            rendered(rebase_fields("origin/main", 4, false)),
            "onto  origin/main  4 commits"
        );
        assert!(
            rendered(rebase_fields("origin/main", 4, true)).contains("REBASE IN PROGRESS"),
            "an already-running rebase must be visible before C-c C-c compounds it"
        );
    }
}
