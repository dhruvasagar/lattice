//! CM.4 (2026-07-22): the **`*problems*` multibuffer view** — the
//! grouped, editable "problems" surface for the error list.
//!
//! Design: `docs/dev/architecture/compilation-mode.md` §4. Slice plan:
//! `docs/dev/operations/slice-plans/compilation-mode.md` (CM.4).
//!
//! `:copen` groups the current error entries as anchored source
//! excerpts (a few context lines around each error location), one
//! excerpt per entry, grouped by file. The view is a regular
//! `BufferKind::Multibuffer` — editable in place, edits propagate to
//! the source via the standard M.3 pipeline — with `ProblemsMinorMode`
//! as its identity marker. `:cclose` closes it.
//!
//! Like `providers::narrow` (and unlike feature-gated
//! `providers::search`), problems is a **first-class built-in**: no
//! cargo feature gate. It reuses the exact source-loading shape the
//! search provider uses — read each referenced file into a fresh
//! `RopeDocumentHandle` (`spawn_document`) and add it to the view's
//! source map — the only difference being that the entry list is
//! static (no async scan), so loading runs synchronously here rather
//! than in a forwarder task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_config::OptionOverrideSet;
use lattice_core::{BufferFlags, BufferId, DocumentBuilder};
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_mode::{
    CapabilitySet, Keymap, LifecycleFuture, Mode, ModeActivator, ModeContext, ModeId, ModeKind,
    ModeRegistry,
};
use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};
use lattice_runtime::{Document, spawn_document};
use lattice_syntax::LangRegistry;

use crate::registry::MultibufferRegistryHandle;
use crate::view::create_multibuffer_view;
use crate::{Excerpt, ExcerptHeader, HeaderlineStatus};

/// Context lines shown above and below each error location in the
/// `*problems*` view. A small fixed window (±2), clamped to the
/// file's line count; keeps each excerpt focused on the offending
/// site while showing enough surrounding code to orient. (Search's
/// `search.context_size` is a user option because search hits are
/// open-ended; a problems excerpt is anchored on a known error, so a
/// fixed window is the right default.)
const CONTEXT: u32 = 2;

// ─────────────────────────────────────────────────────────────────
// ProblemsMinorMode — identity marker for `*problems*` views
// ─────────────────────────────────────────────────────────────────

/// `problems-minor-mode` — the provider-minor activated on a
/// `*problems*` view. Pure identity marker (like [`NarrowMode`]):
/// a multibuffer with this minor active IS a problems view, which the
/// host's `:cclose` guard reads. Editable — no `ReadOnly` override, so
/// edits propagate to the source. `on_activate` is a no-op so the
/// marker is cheap; the `q`→close chord can land in a follow-up.
///
/// [`NarrowMode`]: crate::providers::narrow::NarrowMode
pub struct ProblemsMinorMode;

impl ProblemsMinorMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("problems-minor-mode")
    }
}

/// RAII guard for `ProblemsMinorMode`. Unit — no subscriptions /
/// action handlers yet (mirrors `NarrowModeGuard`).
pub struct ProblemsMinorModeGuard;

impl Mode for ProblemsMinorMode {
    type Guard = ProblemsMinorModeGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        // Problems views are EDITABLE — edits propagate to the source
        // via M.3. No ReadOnly override (matches narrow / search).
        OptionOverrideSet::new()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        // No contributed chords (a `q` → close binding can land in a
        // follow-up once `action:problems-close` is registered).
        //
        // RV.3: `gr` is NOT declared here either — it lives once on
        // `refreshable-view-mode`, reached via [`Self::refresh_action`].
        Keymap::default()
    }

    /// RV.3 (2026-08-10): rebuild the view from the current error list.
    ///
    /// Before RV.1 this view had no `gr` at all and the key was
    /// silently swallowed — one of the two gaps that motivated the
    /// shared chord. The list it renders is a snapshot taken at
    /// `:copen` time, so it goes stale the moment a compile re-runs or
    /// (post-EP.3) the language server republishes; refresh is how the
    /// user catches it up without closing and re-opening.
    fn refresh_action(&self) -> Option<&'static str> {
        Some("action:problems-refresh")
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(ProblemsMinorModeGuard) })
    }
}

// ─────────────────────────────────────────────────────────────────
// create_problems_view — group error entries into excerpts
// ─────────────────────────────────────────────────────────────────

/// Human-readable severity label for an excerpt header.
fn severity_label(severity: ErrorSeverity) -> &'static str {
    match severity {
        ErrorSeverity::Error => "error",
        ErrorSeverity::Warning => "warning",
        ErrorSeverity::Info => "info",
        ErrorSeverity::Note => "note",
    }
}

/// Build the [`ExcerptHeader`] for one error entry: `"<severity>:
/// <message>"` as the title, with the source path attached so the rich
/// header renderer shows the leading file-type icon + basename/dir
/// split (the same shape the search provider's header uses).
fn problems_excerpt_header(path: &Path, entry: &ErrorEntry) -> ExcerptHeader {
    let mut header = ExcerptHeader::new(format!(
        "{}: {}",
        severity_label(entry.severity),
        entry.message
    ));
    header.path = Some(path.to_path_buf());
    header
}

/// Open a `*problems*` multibuffer view grouping `entries` as anchored
/// source excerpts. Returns the new view's `BufferId`, or `None` when
/// `entries` is empty (nothing to show) or every referenced file is
/// unreadable (no excerpt could be built).
///
/// Grouping is stable: files appear in first-seen order; within a
/// file, entries are ordered by line. Each entry becomes one excerpt
/// of ±[`CONTEXT`] lines around its location, clamped to the file's
/// line count.
///
/// Source loading mirrors `providers::search`: each unique file is
/// read into a fresh `RopeDocumentHandle` via [`spawn_document`] and
/// added to the view's source map. A file that fails to read is logged
/// and skipped (its entries drop out); the view still opens for the
/// readable ones. Consistency with search is deliberate — no novel
/// file-reading path.
pub fn create_problems_view(
    activator: &mut dyn ModeActivator,
    entries: &[ErrorEntry],
    registry: CommandRegistryHandle,
    lang_registry: Option<Arc<LangRegistry>>,
) -> Option<BufferId> {
    let (sources, excerpts, n_files) = build_problems_excerpts(entries)?;

    let n_entries = excerpts.len();
    let view_id = create_multibuffer_view(
        activator,
        sources,
        excerpts,
        Some("*problems*".to_string()),
        BufferFlags::default(),
        registry,
        lang_registry,
    );

    set_problems_headerline(activator, view_id, n_entries, n_files);
    activator.activate_minor_by_id(view_id, ProblemsMinorMode::mode_id());
    Some(view_id)
}

/// RV.3 (2026-08-10): the source-loading + excerpt-grouping step,
/// shared by [`create_problems_view`] and [`refresh_problems_view`].
///
/// Returns `(sources, excerpts, n_files)`, or `None` when `entries` is
/// empty or every referenced file proved unreadable — the two cases
/// where there is nothing to show. Extracted rather than duplicated so
/// a refresh cannot drift from the grouping an open produces.
fn build_problems_excerpts(
    entries: &[ErrorEntry],
) -> Option<(HashMap<BufferId, Arc<dyn Document>>, Vec<Excerpt>, usize)> {
    if entries.is_empty() {
        return None;
    }

    // Group entries by file, preserving first-seen file order.
    let mut file_order: Vec<PathBuf> = Vec::new();
    let mut by_file: HashMap<PathBuf, Vec<ErrorEntry>> = HashMap::new();
    for entry in entries {
        if !by_file.contains_key(&entry.path) {
            file_order.push(entry.path.clone());
        }
        by_file
            .entry(entry.path.clone())
            .or_default()
            .push(entry.clone());
    }

    let mut sources: HashMap<BufferId, Arc<dyn Document>> = HashMap::new();
    let mut excerpts: Vec<Excerpt> = Vec::new();
    let mut n_files: usize = 0;

    for path in &file_order {
        // Mirror search's per-file loader: read the file, spawn a
        // fresh RopeDocumentHandle, add it to the source map. Skip
        // (log + continue) on a read error — never abort the whole
        // view, never panic.
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "problems: source file unreadable; skipping its entries",
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
        // Source docs get a fresh empty registry behind the `ArcSwap`
        // handle `spawn_document` expects (search's exact shape).
        let source_registry = Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
        let handle = spawn_document(source_id, document, source_registry);
        let dyn_handle: Arc<dyn Document> = Arc::new(handle);
        sources.insert(source_id, dyn_handle);
        n_files += 1;

        // Entries for this file, ordered by line.
        let mut file_entries = by_file.remove(path).unwrap_or_default();
        file_entries.sort_by_key(|e| e.line);
        for entry in &file_entries {
            let line = entry.line.min(last_line);
            let start = line.saturating_sub(CONTEXT);
            let end = (line + CONTEXT).min(last_line);
            let header = problems_excerpt_header(path, entry);
            excerpts.push(Excerpt::new(source_id, start, end).with_header(header));
        }
    }

    // Every referenced file was unreadable — nothing to show.
    if excerpts.is_empty() {
        return None;
    }

    Some((sources, excerpts, n_files))
}

/// Sticky headerline — the entry/file count. Problems composition is
/// synchronous (no scan), so straight to Complete.
fn set_problems_headerline(
    activator: &mut dyn ModeActivator,
    view_id: BufferId,
    n_entries: usize,
    n_files: usize,
) {
    if let Some(mb_reg) = activator.services().get::<MultibufferRegistryHandle>()
        && let Some(view) = mb_reg.handle(view_id)
    {
        view.set_headerline(HeaderlineStatus::Complete {
            summary: format!("[problems] {n_entries} in {n_files} files"),
            emphasis: None,
        });
    }
}

/// RV.3 (2026-08-10): rebuild an existing `*problems*` view from the
/// current error list, in place. Returns the new entry count, or `None`
/// when the view is unknown to the multibuffer registry or the fresh
/// entry set yields nothing to show (in which case the view is left
/// exactly as it was — a refresh must never blank the buffer the user
/// is reading).
///
/// In place is the whole point: [`create_problems_view`] mints a new
/// `BufferId` every call, so "refresh" cannot be a re-open without
/// stranding the old view and opening a second `*problems*`.
/// [`crate::MultibufferDocumentHandle::replace_excerpts`] swaps the
/// source map and excerpt list atomically and republishes, so the
/// buffer the user is looking at simply becomes current.
///
/// Sources are re-read from disk, which is the point of a refresh here:
/// the view's sources are freshly-spawned handles taken at open time,
/// not the live editor buffers, so both the error list *and* the file
/// contents may have moved on.
pub fn refresh_problems_view(
    activator: &mut dyn ModeActivator,
    view_id: BufferId,
    entries: &[ErrorEntry],
) -> Option<usize> {
    let (sources, excerpts, n_files) = build_problems_excerpts(entries)?;
    let n_entries = excerpts.len();

    let mb_reg = activator.services().get::<MultibufferRegistryHandle>()?;
    let view = mb_reg.handle(view_id)?;
    view.replace_excerpts(sources, excerpts);
    drop(view);

    set_problems_headerline(activator, view_id, n_entries, n_files);
    Some(n_entries)
}

// ─────────────────────────────────────────────────────────────────
// Boot integration
// ─────────────────────────────────────────────────────────────────

/// Boot helper — register the problems provider-minor mode. Called
/// from [`crate::install`] alongside the other multibuffer modes.
pub fn register_problems_mode(mode_registry: &mut ModeRegistry) {
    mode_registry
        .register(ProblemsMinorMode)
        .expect("problems-minor-mode registers without conflict at boot");
}

/// Boot helper — register the `:copen` + `:cclose` ex-commands.
///
/// `:copen` emits [`AppEffect::ProblemsOpen`]; the host arm reads the
/// core error list and calls [`create_problems_view`]. `:cclose`
/// emits [`AppEffect::ProblemsClose`], which the host guards to the
/// active problems view before closing.
///
/// [`AppEffect::ProblemsOpen`]: lattice_grammar::app_effect::AppEffect::ProblemsOpen
/// [`AppEffect::ProblemsClose`]: lattice_grammar::app_effect::AppEffect::ProblemsClose
pub fn register_problems_ex_commands(registry: &mut CommandRegistry) {
    use lattice_grammar::app_effect::AppEffect;
    use lattice_grammar::args::Args;
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::effect::Effect;
    use lattice_grammar::registry::{ExCommandSpec, SurfaceForm};

    // naming-2026-07-22: readable canonical `:problems` leads; the vim
    // `:copen`/`:cclose` spellings are aliases in `lattice-host::excommand`.
    registry.register_ex_command(
        "problems",
        "Open the `*problems*` view: the current error list grouped as \
         editable source excerpts by file. Edits propagate to the source. \
         `:problems-close` (vim `:cclose`) closes it. Vim alias: `:copen`.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|_s: &str, _bang: bool| Ok(Args::None)),
            apply: Arc::new(|_ctx| Ok(Effect::AppAction(AppEffect::ProblemsOpen))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    registry.register_ex_command(
        "problems-close",
        "Close the active `*problems*` view, leaving the source buffers open (vim `:cclose`).",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|_s: &str, _bang: bool| Ok(Args::None)),
            apply: Arc::new(|_ctx| Ok(Effect::AppAction(AppEffect::ProblemsClose))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );

    // RV.3: the refresh target `ProblemsMinorMode::refresh_action`
    // names. Registered as a plain action rather than an ex-command —
    // it is reached through the shared `gr`, not typed at the `:` line,
    // and `:problems` already re-opens for anyone who wants that.
    //
    // Its `apply` is LIVE (not the dead `Effect::None` of a
    // handler-intercepted action): this provider registers no
    // `ActionHandler`, so the Action gate is what satisfies it. That is
    // exactly the shape the RV.2 dispatch fix exists to support.
    registry.register_action(
        "action:problems-refresh",
        "problems-mode `gr`: rebuild the `*problems*` view from the current error list.",
        lattice_grammar::registry::ActionSpec {
            apply: Arc::new(|_ctx| Ok(Effect::AppAction(AppEffect::ProblemsRefresh))),
            args_schema: vec![],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_labels_are_stable() {
        assert_eq!(severity_label(ErrorSeverity::Error), "error");
        assert_eq!(severity_label(ErrorSeverity::Warning), "warning");
        assert_eq!(severity_label(ErrorSeverity::Info), "info");
        assert_eq!(severity_label(ErrorSeverity::Note), "note");
    }

    #[test]
    fn header_carries_severity_message_and_path() {
        let entry = ErrorEntry {
            path: PathBuf::from("/tmp/a.rs"),
            line: 3,
            col: 0,
            severity: ErrorSeverity::Warning,
            message: "unused variable".to_string(),
        };
        let header = problems_excerpt_header(&entry.path, &entry);
        assert_eq!(header.title, "warning: unused variable");
        assert_eq!(
            header.path.as_deref(),
            Some(std::path::Path::new("/tmp/a.rs"))
        );
    }

    #[test]
    fn register_problems_ex_commands_registers_problems_open_and_close() {
        let mut registry = CommandRegistry::new();
        register_problems_ex_commands(&mut registry);
        assert!(
            registry.id_by_name("problems").is_some(),
            "`:problems` ex-command must register (vim alias `:copen`)"
        );
        assert!(
            registry.id_by_name("problems-close").is_some(),
            "`:problems-close` ex-command must register (vim alias `:cclose`)"
        );
    }
}
