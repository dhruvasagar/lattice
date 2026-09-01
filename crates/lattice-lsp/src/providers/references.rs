//! LR.1 (2026-08-11): the **references multibuffer** — every reference
//! site as an editable excerpt.
//!
//! Design: `docs/dev/architecture/lsp-architecture.md` §17. Slice plan:
//! `docs/dev/operations/slice-plans/lsp-references-view.md`.
//!
//! ## Why a second surface rather than a replacement
//!
//! `gr` opens a **picker**, and keeps doing so. That is the right shape
//! for *go to one of these*, and the muscle memory should not move. It
//! is the wrong shape for *rename this argument at all fifteen call
//! sites*, which is what references are mostly for — and that workflow
//! has no surface at all today.
//!
//! So this is a peer, opened by `:lsp-references`, not a change to what
//! `gr` does.
//!
//! ## Shape
//!
//! Follows `lattice_multibuffer::providers::problems` rather than
//! `providers::search`: the LSP layer delivers the location list over
//! an existing channel, so this composes a *delivered* result and owns
//! no scan of its own. Each unique file is read into a fresh
//! `RopeDocumentHandle` and added to the view's source map — the same
//! source-loading shape both existing providers use, deliberately not a
//! novel file-reading path.
//!
//! ## `gr` in the view means refresh
//!
//! [`LspReferencesMode`] declares `refresh_action` and so inherits `gr`
//! from `refreshable-view-mode` (RV.1). It binds no chord itself: the
//! chord lives in exactly one place, and a copy here would be the sixth
//! (`mode-architecture.md` §5.5).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use lattice_config::OptionOverrideSet;
use lattice_core::{BufferFlags, BufferId, DocumentBuilder};
use lattice_grammar::Effect;
use lattice_grammar::LspRequest;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, CapabilitySet, Keymap,
    LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind, ModeRegistry,
};
use lattice_multibuffer::view::create_multibuffer_view;
use lattice_multibuffer::{Excerpt, ExcerptHeader, HeaderlineStatus, MultibufferRegistryHandle};
use lattice_runtime::{Document, spawn_document};
use lattice_syntax::LangRegistry;
use lsp_types::Location;

use crate::actor::uri_to_path;

/// Context lines shown above and below each reference site.
///
/// A fixed ±2 window, like `problems`, and for the same reason: a
/// reference is anchored on a known location, so the excerpt wants
/// enough surrounding code to orient and no more. (Search's
/// `search.context_size` is a user option because search hits are
/// open-ended.)
const CONTEXT: u32 = 2;

// ─────────────────────────────────────────────────────────────────
// The origin — what a refresh re-queries
// ─────────────────────────────────────────────────────────────────

/// Where the references query was issued from.
///
/// Kept per-view so `gr` can re-run the *same* query. It must not be
/// re-derived from the cursor at refresh time: by then the cursor is
/// inside the multibuffer, and re-querying there would ask about
/// whatever symbol happens to sit under it — a different question, with
/// a plausible-looking wrong answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReferencesOrigin {
    /// The document the query was issued against.
    pub uri: String,
    /// 0-based line within that document.
    pub line: u32,
    /// 0-based UTF-16 character offset (LSP's own convention).
    pub character: u32,
    /// The symbol text, for the headerline. May be empty when the
    /// word under the cursor could not be determined.
    pub symbol: String,
}

/// Per-view state: the origin to re-query, keyed by the view's
/// `BufferId`, plus a `DocumentId` → `BufferId` index for cleanup.
///
/// The second map is not redundant. `Event::DocumentClosed` carries a
/// `DocumentId`, and the two ids are NOT interchangeable — the
/// multibuffer registry keeps a separate `remove_by_document_id` for
/// the same reason. Recording the pair at creation is cheaper than
/// walking every view on each close.
#[derive(Debug, Default)]
pub struct LspReferencesService {
    views: std::sync::RwLock<HashMap<BufferId, ReferencesOrigin>>,
    by_document: std::sync::RwLock<HashMap<lattice_protocol::ids::DocumentId, BufferId>>,
}

impl LspReferencesService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_origin(&self, view: BufferId, origin: ReferencesOrigin) {
        if let Ok(mut w) = self.views.write() {
            w.insert(view, origin);
        }
    }

    /// Record the view's document id so a `DocumentClosed` event can
    /// find it.
    pub fn index_document(&self, document: lattice_protocol::ids::DocumentId, view: BufferId) {
        if let Ok(mut w) = self.by_document.write() {
            w.insert(document, view);
        }
    }

    /// Cleanup entry point for the `DocumentClosed` subscriber.
    /// Returns `true` when a view was forgotten.
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

    pub fn origin(&self, view: BufferId) -> Option<ReferencesOrigin> {
        self.views.read().ok()?.get(&view).cloned()
    }

    /// Drop a view's state.
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
/// `Arc<LspReferencesService>` and asking for `LspReferencesService`
/// silently returns `None`.
pub type LspReferencesServiceHandle = Arc<LspReferencesService>;

// ─────────────────────────────────────────────────────────────────
// LspReferencesMode — identity marker for references views
// ─────────────────────────────────────────────────────────────────

/// `lsp-references-mode` — the provider-minor activated on a references
/// view. An identity marker, like `ProblemsMinorMode`: a multibuffer
/// with this minor active IS a references view.
///
/// Editable — no `ReadOnly` override, so edits propagate to the sources
/// through the standard M.3 pipeline. That is the entire point of the
/// surface.
pub struct LspReferencesMode;

impl LspReferencesMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-references-mode")
    }
}

pub struct LspReferencesModeGuard;

/// The refresh action this mode declares and handles.
pub const REFRESH_ACTION: &str = "action:lsp-references-refresh";

/// Returns the host-owned effect; the substrate reads the view's stored
/// origin rather than the live cursor (see [`ReferencesOrigin`]).
fn refresh_handler() -> ActionHandler {
    Arc::new(|_ctx: &ActionContext<'_>| Some(Effect::Lsp(LspRequest::ReferencesViewRefresh)))
}

impl Mode for LspReferencesMode {
    type Guard = LspReferencesModeGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::new()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    /// No chords of its own. `gr` arrives from `refreshable-view-mode`
    /// via [`Self::refresh_action`] — see the module docs.
    fn keymap(&self) -> Keymap {
        Keymap::default()
    }

    /// Declaring the target is what pulls in the shared `gr` minor
    /// through the implies cascade.
    fn refresh_action(&self) -> Option<&'static str> {
        Some(REFRESH_ACTION)
    }

    /// OA.4b: this view folds by blocks, so `<Tab>` / `<S-Tab>` come from the
    /// shared `foldable-view-mode`. Nothing special to do on a block, so it
    /// names the generic body.
    fn fold_toggle_action(&self) -> Option<&'static str> {
        Some(lattice_mode::FOLD_TOGGLE_DEFAULT_ACTION)
    }

    /// LR.3: the refresh handler. Mode owns the *decision*; the host
    /// owns the generic async execution — §16's split, unchanged. The
    /// handler holds only `&ActionContext` and so could not drive the
    /// request even if it wanted to.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![ActionHandlerContribution {
            action_name: REFRESH_ACTION,
            handler: refresh_handler(),
        }]
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(LspReferencesModeGuard) })
    }
}

// ─────────────────────────────────────────────────────────────────
// View construction
// ─────────────────────────────────────────────────────────────────

/// Header for one reference excerpt: the path plus the 1-based line,
/// with the path attached so the rich header renderer shows the
/// file-type icon and basename/dir split.
fn reference_excerpt_header(path: &std::path::Path, line0: u32) -> ExcerptHeader {
    let mut header = ExcerptHeader::new(format!("{}", line0.saturating_add(1)));
    header.path = Some(path.to_path_buf());
    header
}

/// Group locations into per-file sources + excerpts.
///
/// Returns `(sources, excerpts, n_files)`, or `None` when there is
/// nothing to show — no locations, or every referenced file unreadable.
/// Shared by open and refresh so the two cannot drift.
///
/// Files appear in first-seen order and locations within a file are
/// ordered by line, which gives a stable layout across refreshes.
/// A file that fails to read is logged and skipped: one unreadable
/// path must not cost the user every other reference.
#[allow(clippy::type_complexity)]
pub fn build_reference_excerpts(
    locations: &[Location],
) -> Option<(HashMap<BufferId, Arc<dyn Document>>, Vec<Excerpt>, usize)> {
    if locations.is_empty() {
        return None;
    }

    let mut file_order: Vec<PathBuf> = Vec::new();
    let mut by_file: HashMap<PathBuf, Vec<u32>> = HashMap::new();
    for loc in locations {
        let Some(path) = uri_to_path(&loc.uri) else {
            tracing::debug!(
                uri = %loc.uri.as_str(),
                "references: location has no file path; skipping"
            );
            continue;
        };
        if !by_file.contains_key(&path) {
            file_order.push(path.clone());
        }
        by_file.entry(path).or_default().push(loc.range.start.line);
    }

    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    let mut excerpts: Vec<Excerpt> = Vec::new();
    let mut n_files = 0usize;

    for path in &file_order {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "references: source file unreadable; skipping its sites",
                );
                continue;
            }
        };
        let last_line = (text.lines().count() as u32).saturating_sub(1);

        let source_id = BufferId::next();
        let document = DocumentBuilder::default()
            .with_text(&text)
            .with_path(path.clone())
            .build();
        let source_registry = Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
        let handle = spawn_document(source_id, document, source_registry);
        let dyn_handle: Arc<dyn Document> = Arc::new(handle);
        sources.insert(source_id, dyn_handle);
        n_files += 1;

        let mut lines = by_file.remove(path).unwrap_or_default();
        lines.sort_unstable();
        // The same line can host several references (`foo(foo)`); one
        // excerpt per line is what the user wants to see and edit.
        lines.dedup();
        for line0 in lines {
            let line = line0.min(last_line);
            let start = line.saturating_sub(CONTEXT);
            let end = (line + CONTEXT).min(last_line);
            let header = reference_excerpt_header(path, line);
            excerpts.push(Excerpt::new(source_id, start, end).with_header(header));
        }
    }

    if excerpts.is_empty() {
        return None;
    }
    Some((sources, excerpts, n_files))
}

/// Set the sticky headerline: the site/file count and the symbol.
fn set_references_headerline(
    activator: &mut dyn ModeActivator,
    view: BufferId,
    symbol: &str,
    n_sites: usize,
    n_files: usize,
) {
    if let Some(reg) = activator.services().get::<MultibufferRegistryHandle>()
        && let Some(handle) = reg.handle(view)
    {
        let label = if symbol.is_empty() {
            String::new()
        } else {
            format!(" for `{symbol}`")
        };
        handle.set_headerline(HeaderlineStatus::Complete {
            summary: format!("[references{label}] {n_sites} in {n_files} files"),
            emphasis: None,
        });
    }
}

/// Open a references multibuffer over `locations`.
///
/// Returns the view's `BufferId`, or `None` when there is nothing to
/// show — the caller echoes rather than opening an empty view.
///
/// `origin` is stored per-view so `gr` re-queries the symbol the search
/// started from, not whatever the multibuffer cursor later lands on.
pub fn create_references_view(
    activator: &mut dyn ModeActivator,
    locations: &[Location],
    origin: ReferencesOrigin,
    registry: CommandRegistryHandle,
    lang_registry: Option<Arc<LangRegistry>>,
) -> Option<BufferId> {
    let (sources, excerpts, n_files) = build_reference_excerpts(locations)?;
    let n_sites = excerpts.len();

    let view = create_multibuffer_view(
        activator,
        sources,
        excerpts,
        Some("*references*".to_string()),
        BufferFlags::default(),
        registry,
        lang_registry,
        // AF.1: references arrive grouped by file and each carries its path as
        // a header, so a file is a contiguous run — the default.
        lattice_multibuffer::FoldGrouping::SourceFile,
    );

    if let Some(svc) = activator.services().get::<LspReferencesServiceHandle>() {
        svc.set_origin(view, origin.clone());
        // Index by document id so `DocumentClosed` can find this view.
        if let Some(reg) = activator.services().get::<MultibufferRegistryHandle>()
            && let Some(handle) = reg.handle(view)
        {
            svc.index_document(handle.document_id(), view);
        }
    } else {
        // Not fatal: the view is still usable, `gr` simply has nothing
        // to re-query. Loud at debug because it means boot wiring is
        // missing, not that the user did anything.
        tracing::debug!("references: service not registered; refresh will be unavailable");
    }

    set_references_headerline(activator, view, &origin.symbol, n_sites, n_files);
    activator.activate_minor_by_id(view, LspReferencesMode::mode_id());
    Some(view)
}

/// LR.3 (2026-08-11): rebuild an existing references view from a fresh
/// result set, in place.
///
/// In place is the point: [`create_references_view`] mints a new
/// `BufferId` every call, so a refresh that re-opened would strand the
/// view the user pressed `gr` in and add a second `*references*`
/// beside it — the mistake `*problems*` refresh had to avoid too.
///
/// Returns the new site count, or `None` when the view is unknown or
/// the fresh results yield nothing to show — in which case the view is
/// left exactly as it was. A refresh must never blank the buffer the
/// user is reading.
pub fn refresh_references_view(
    activator: &mut dyn ModeActivator,
    view: BufferId,
    locations: &[Location],
) -> Option<usize> {
    let (sources, excerpts, n_files) = build_reference_excerpts(locations)?;
    let n_sites = excerpts.len();

    let reg = activator.services().get::<MultibufferRegistryHandle>()?;
    let handle = reg.handle(view)?;
    handle.replace_excerpts(sources, excerpts);
    drop(handle);

    let symbol = activator
        .services()
        .get::<LspReferencesServiceHandle>()
        .and_then(|svc| svc.origin(view))
        .map(|o| o.symbol)
        .unwrap_or_default();
    set_references_headerline(activator, view, &symbol, n_sites, n_files);
    Some(n_sites)
}

// ─────────────────────────────────────────────────────────────────
// Boot integration
// ─────────────────────────────────────────────────────────────────

/// Register `action:lsp-references-refresh` so the mode's declared
/// refresh target resolves at boot.
///
/// The `apply` is a dead `Effect::None`: the mode's `action_handlers`
/// closure intercepts before the grammar Action gate. It exists so the
/// `CommandId` resolves — the `repl-mode` shape.
pub fn register_references_actions(registry: &mut CommandRegistry) {
    registry.register_action(
        REFRESH_ACTION,
        "lsp-references-mode `gr`: re-run the query at the view's origin and rebuild in place.",
        lattice_grammar::registry::ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
}

/// Register the provider-minor mode.
pub fn register_references_mode(modes: &mut ModeRegistry) {
    modes
        .register(LspReferencesMode)
        .expect("lsp-references-mode registers without conflict at boot");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lsp_types::{Position, Range, Uri};

    fn loc(uri: &str, line: u32) -> Location {
        Location {
            uri: uri.parse::<Uri>().unwrap(),
            range: Range {
                start: Position { line, character: 0 },
                end: Position { line, character: 3 },
            },
        }
    }

    struct TempTree {
        dir: PathBuf,
    }

    impl TempTree {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("lattice-refs-{tag}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
        fn write(&self, name: &str, body: &str) -> PathBuf {
            let p = self.dir.join(name);
            std::fs::write(&p, body).unwrap();
            p
        }
        fn uri(&self, name: &str) -> String {
            format!("file://{}", self.dir.join(name).display())
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    const EIGHT: &str = "l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7\n";

    #[test]
    fn mode_declares_a_refresh_and_binds_no_chord() {
        let m = LspReferencesMode;
        assert_eq!(m.kind(), ModeKind::Minor);
        assert_eq!(m.refresh_action(), Some("action:lsp-references-refresh"));
        // `gr` comes from the shared minor. A chord here would be the
        // sixth copy — the thing RV.2 removed.
        assert!(m.keymap().entries.is_empty() && m.keymap().bindings.is_empty());
    }

    #[test]
    fn excerpts_group_by_file_and_sort_by_line() {
        let t = TempTree::new("group");
        t.write("a.rs", EIGHT);
        t.write("b.rs", EIGHT);
        // Interleaved, out of order, to prove grouping + sorting.
        let locs = vec![
            loc(&t.uri("a.rs"), 5),
            loc(&t.uri("b.rs"), 1),
            loc(&t.uri("a.rs"), 2),
        ];
        let (sources, excerpts, n_files) = build_reference_excerpts(&locs).unwrap();
        assert_eq!(n_files, 2);
        assert_eq!(sources.len(), 2);
        assert_eq!(excerpts.len(), 3);
        // a.rs's two sites lead, ordered by line: ±2 clamped → [0,4], [3,7].
        assert_eq!((excerpts[0].start_line, excerpts[0].end_line), (0, 4));
        assert_eq!((excerpts[1].start_line, excerpts[1].end_line), (3, 7));
        assert_eq!(excerpts[0].source, excerpts[1].source);
        assert_ne!(excerpts[0].source, excerpts[2].source);
    }

    /// `foo(foo)` yields two locations on one line; the user wants one
    /// excerpt, not a duplicate.
    #[test]
    fn several_sites_on_one_line_collapse_to_one_excerpt() {
        let t = TempTree::new("dedup");
        t.write("a.rs", EIGHT);
        let locs = vec![loc(&t.uri("a.rs"), 3), loc(&t.uri("a.rs"), 3)];
        let (_, excerpts, _) = build_reference_excerpts(&locs).unwrap();
        assert_eq!(excerpts.len(), 1);
    }

    #[test]
    fn no_locations_yields_nothing_to_show() {
        assert!(build_reference_excerpts(&[]).is_none());
    }

    #[test]
    fn all_unreadable_files_yield_nothing_to_show() {
        let locs = vec![loc("file:///nonexistent/zzz.rs", 0)];
        assert!(build_reference_excerpts(&locs).is_none());
    }

    /// One bad path must not cost the user the readable references.
    #[test]
    fn an_unreadable_file_is_skipped_not_fatal() {
        let t = TempTree::new("partial");
        t.write("good.rs", EIGHT);
        let locs = vec![
            loc("file:///nonexistent/gone.rs", 0),
            loc(&t.uri("good.rs"), 1),
        ];
        let (_, excerpts, n_files) = build_reference_excerpts(&locs).unwrap();
        assert_eq!(n_files, 1);
        assert_eq!(excerpts.len(), 1);
    }

    /// A location past EOF (a stale result raced against an edit) must
    /// clamp rather than produce an out-of-range excerpt.
    #[test]
    fn a_line_past_eof_clamps_to_the_last_line() {
        let t = TempTree::new("clamp");
        t.write("a.rs", EIGHT);
        let locs = vec![loc(&t.uri("a.rs"), 9_999)];
        let (_, excerpts, _) = build_reference_excerpts(&locs).unwrap();
        assert_eq!(excerpts[0].end_line, 7, "clamped to the file's last line");
    }

    #[test]
    fn service_tracks_and_forgets_view_origins() {
        let svc = LspReferencesService::new();
        let view = BufferId::next();
        let origin = ReferencesOrigin {
            uri: "file:///tmp/a.rs".to_string(),
            line: 3,
            character: 7,
            symbol: "foo".to_string(),
        };
        svc.set_origin(view, origin.clone());
        assert_eq!(svc.origin(view), Some(origin));
        assert_eq!(svc.tracked_views(), 1);
        svc.forget(view);
        assert_eq!(svc.origin(view), None);
        assert_eq!(svc.tracked_views(), 0);
    }
}
