//! MG.5: magit-diff major mode.
//!
//! Fold audit fix: this used to be a full stub — `on_activate`
//! registered only a close handler and the buffer opened empty; `s`/
//! `u` were declared in the keymap with no handler of their own, so
//! pressing them silently hijacked whatever `magit-status` handler
//! happened to be registered (operating on magit-status's captured
//! buffer state, not this buffer's cursor). The design's full
//! side-by-side `DiffSession` + hunk-level staging (reusing D.4's
//! pane-group machinery) remains a larger follow-up; this is a
//! real, scoped middle ground: `git diff HEAD` content (staged +
//! unstaged changes combined, matching the module's original
//! "against HEAD" framing) with its own file-level `s`/`u`/`x`
//! handlers, scoped to this buffer's own state.
//!
//! `d` on a file in magit-status's Staged/Unstaged sections
//! (`action:magit-diff-file` in `actions.rs`) opens one of these
//! buffers scoped to BOTH a file and a baseline
//! (`*magit:diff:staged:<path>*` / `*magit:diff:unstaged:<path>*`),
//! instead of the status buffer's own inline `=` toggle — large
//! diffs get a real scrollable buffer instead of ballooning the
//! status buffer's line count (and re-triggering its splice-based
//! inline-highlight bookkeeping) for a file the user just wants to
//! read in full.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::{Index, Repository};

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline;

pub struct MagitDiffMode;

impl MagitDiffMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-diff-mode")
    }
}

fn magit_diff_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "s", doc: "Stage file at cursor", cmd: "action:magit-stage" },
            keymap_entry! { mode: Normal, chord: "u", doc: "Unstage file at cursor", cmd: "action:magit-unstage" },
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Visit file at cursor", cmd: "action:magit-diff-visit-file" },
        ]
    })
}

/// Which baseline a diff buffer compares against — encoded in the
/// buffer name (see [`parse_buffer_name`]) so the SAME mode serves
/// `:magit-diff` (against HEAD, combining staged+unstaged), and the
/// status buffer's per-section `d` binding (against the index, for
/// exactly one side of the working tree).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffScope {
    /// `git diff HEAD` — staged + unstaged changes combined.
    Head,
    /// `git diff --cached` — index vs HEAD (the Staged section).
    Staged,
    /// `git diff` — working tree vs index (the Unstaged section).
    Unstaged,
}

impl DiffScope {
    /// MG.14: how this scope reads in the headerline. The same three
    /// words `parse_buffer_name` accepts, so the header echoes the
    /// buffer name rather than inventing a second vocabulary.
    fn header_label(self) -> &'static str {
        match self {
            DiffScope::Head => "HEAD",
            DiffScope::Staged => "staged",
            DiffScope::Unstaged => "unstaged",
        }
    }
}

pub struct DiffState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: PathBuf,
    scope: DiffScope,
    /// `Some` when this buffer is scoped to one file (opened as
    /// `*magit:diff:<path>*` / `*magit:diff:staged:<path>*` /
    /// `*magit:diff:unstaged:<path>*`); `None` for the unscoped
    /// `*magit:diff*` (`:magit-diff`) view.
    path: Option<PathBuf>,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
}

/// Parse `"*magit:diff[:staged|:unstaged]:<path>*"` (or the bare
/// unscoped `"*magit:diff*"`) into a `(scope, path)` pair. The
/// `staged`/`unstaged` infix must be checked before the bare
/// `"*magit:diff:"` prefix, since that prefix is itself a substring
/// of both scoped forms.
fn parse_buffer_name(name: &str) -> (DiffScope, Option<PathBuf>) {
    if let Some(s) = name
        .strip_prefix("*magit:diff:staged:")
        .and_then(|s| s.strip_suffix('*'))
    {
        return (DiffScope::Staged, (!s.is_empty()).then(|| PathBuf::from(s)));
    }
    if let Some(s) = name
        .strip_prefix("*magit:diff:unstaged:")
        .and_then(|s| s.strip_suffix('*'))
    {
        return (
            DiffScope::Unstaged,
            (!s.is_empty()).then(|| PathBuf::from(s)),
        );
    }
    if let Some(s) = name
        .strip_prefix("*magit:diff:")
        .and_then(|s| s.strip_suffix('*'))
    {
        return (DiffScope::Head, (!s.is_empty()).then(|| PathBuf::from(s)));
    }
    (DiffScope::Head, None)
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type DiffStatesHandle = Arc<BufferStates<DiffState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<DiffState>>> {
    crate::buffer_state::state_for::<DiffState>(ctx)
}

impl Mode for MagitDiffMode {
    type Guard = BufferStateGuard<DiffState>;

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
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_diff_keymap_entries())
    }

    /// MG.13: boot-registered — see `buffer_state`'s module docs. `gr`,
    /// `s` and `u` are NOT here: they are shared actions owned by
    /// `magit-core-mode` and reached through this mode's `MagitView`.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // <CR> — visit the file at cursor. Staged scope shows the
            // INDEX blob (`*magit:file:staged:<path>*`, read-only) —
            // this diff describes staged content, which may already
            // differ from the live working-tree file. Unstaged IS the
            // working tree, and Head combines both (no single frozen
            // blob to show), so both open the real editable file — same
            // target magit-status's Unstaged section opens.
            ActionHandlerContribution {
                action_name: "action:magit-diff-visit-file",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let g = s.lock().ok()?;
                    let path = file_at_cursor(&g, ctx.cursor.line)?;
                    match g.scope {
                        DiffScope::Staged => Some(Effect::OpenSyntheticBuffer {
                            name: format!("*magit:file:staged:{}*", path.display()),
                            mode_id: "magit-file-revision-mode".to_string(),
                        }),
                        DiffScope::Head | DiffScope::Unstaged => {
                            let full = g.workdir.join(&path);
                            if full.exists() {
                                Some(Effect::OpenBuffer {
                                    path: Some(full),
                                    force: false,
                                })
                            } else {
                                None
                            }
                        }
                    }
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
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            // "*magit:diff[:staged|:unstaged]:<path>*" scopes the view
            // to one file and (optionally) one baseline (mirrors
            // magit-blame's file-in-buffer-name pattern); bare
            // "*magit:diff*" (from `:magit-diff`) stays unscoped
            // against HEAD.
            let (scope, path) = store
                .name_for(buffer_id)
                .map(|name| parse_buffer_name(&name))
                .unwrap_or((DiffScope::Head, None));

            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            // MG.14: this view's header is fully known from the buffer
            // name — scope, plus the path when file-scoped — so it is
            // set here rather than after the diff lands. Neither field
            // changes under `gr`: re-diffing the same scope of the
            // same path still describes the same view.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            headerline::publish(
                &hl,
                headerline::diff_fields(scope.header_label(), path.as_deref()),
            );

            // MG.13: publish BEFORE the first `.await` — see the note
            // in `magit_branch_mode::on_activate`.
            let Some(states) = ctx.service::<DiffStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                DiffState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    scope,
                    path: path.clone(),
                    pending_highlights: pending_highlights.clone(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(DiffView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            let wd = workdir.clone();
            let path_for_task = path.clone();
            let text =
                tokio::task::spawn_blocking(move || run_diff(&wd, scope, path_for_task.as_deref()))
                    .await
                    .unwrap_or_default();
            let spans = crate::highlight::diff_styled_spans(&text);
            apply_full_replace(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

async fn apply_full_replace(handle: &Arc<dyn lattice_runtime::Document>, text: String) {
    let snap = handle.snapshot();
    let last = snap.buffer.line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await;
}

fn refresh(s: Arc<Mutex<DiffState>>) -> Option<Effect> {
    let (handle, wd, scope, path, pending, buffer_id) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.scope,
            g.path.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };
    tokio::task::spawn(async move {
        let text = tokio::task::spawn_blocking(move || run_diff(&wd, scope, path.as_deref()))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::diff_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

fn spawn_mutation_and_refresh(
    s: Arc<Mutex<DiffState>>,
    mutate: impl FnOnce() + Send + 'static,
) -> Option<Effect> {
    let (handle, wd, scope, path, pending, buffer_id) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.scope,
            g.path.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };
    tokio::task::spawn(async move {
        let _ = tokio::task::spawn_blocking(mutate).await;
        let text = tokio::task::spawn_blocking(move || run_diff(&wd, scope, path.as_deref()))
            .await
            .unwrap_or_default();
        let spans = crate::highlight::diff_styled_spans(&text);
        apply_full_replace(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Walk upward from `line` to the nearest `diff --git a/<path> b/<path>`
/// header and extract `<path>` (the `b/` side — the current-tree path).
fn file_at_cursor(state: &DiffState, line: u32) -> Option<PathBuf> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    for l in (0..=line).rev() {
        let text = snap.buffer.line(l)?;
        if let Some(rest) = text.strip_prefix("diff --git a/") {
            // "a/<path> b/<path>" — split on " b/" to isolate the
            // first path (identical to the second except for renames,
            // which this coarse file-level staging doesn't special-case).
            let path = rest.split(" b/").next()?;
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn run_diff(workdir: &Path, scope: DiffScope, path: Option<&Path>) -> String {
    let mut args = vec!["diff".to_string()];
    match scope {
        DiffScope::Head => args.push("HEAD".to_string()),
        DiffScope::Staged => args.push("--cached".to_string()),
        // `git diff` with no ref compares the working tree against
        // the index — exactly the Unstaged section's semantics.
        DiffScope::Unstaged => {}
    }
    if let Some(p) = path {
        args.push("--".to_string());
        args.push(p.to_string_lossy().into_owned());
    }
    let output = std::process::Command::new("git")
        .args(&args)
        .current_dir(workdir)
        .output();
    match output {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8(o.stdout).unwrap_or_default();
            if text.trim().is_empty() {
                match scope {
                    DiffScope::Head => "No changes against HEAD.\n".to_string(),
                    DiffScope::Staged => "No staged changes.\n".to_string(),
                    DiffScope::Unstaged => "No unstaged changes.\n".to_string(),
                }
            } else {
                text
            }
        }
        _ => "Not a git repository, or no commits yet.\n".to_string(),
    }
}

/// `gr` for this view — `magit-core-mode` owns the chord and the one
/// boot-registered handler; see [`MagitView`].
struct DiffView(Arc<Mutex<DiffState>>);

impl MagitView for DiffView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }

    /// `s` — file-level: finds the nearest `diff --git a/X b/X` header
    /// above the cursor.
    fn stage(&self, cursor: Position) -> Option<Effect> {
        let s = self.0.clone();
        let (path, workdir) = {
            let g = s.lock().ok()?;
            (file_at_cursor(&g, cursor.line)?, g.workdir.clone())
        };
        spawn_mutation_and_refresh(s, move || {
            if let Ok(repo) = Repository::discover(&workdir) {
                let _ = Index::stage_path(&repo, &path);
            }
        })
    }

    fn unstage(&self, cursor: Position) -> Option<Effect> {
        let s = self.0.clone();
        let (path, workdir) = {
            let g = s.lock().ok()?;
            (file_at_cursor(&g, cursor.line)?, g.workdir.clone())
        };
        spawn_mutation_and_refresh(s, move || {
            if let Ok(repo) = Repository::discover(&workdir) {
                let _ = Index::unstage_path(&repo, &path);
            }
        })
    }
}
