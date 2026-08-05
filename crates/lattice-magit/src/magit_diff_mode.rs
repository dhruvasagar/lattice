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
    ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry,
    LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
};
use lattice_protocol::position::Position;
use lattice_vcs::{Index, Repository};

use crate::buffer_state::{
    BufferStateGuard, BufferStates, DiffSource, MagitView, MagitViewsHandle,
};
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
            // MG.18e: region staging, same chords on the selection.
        ]
    })
}

/// Which baseline a diff buffer compares against — encoded in the
/// buffer name (see [`parse_buffer_name`]) so the SAME mode serves
/// `:magit-diff` (against HEAD, combining staged+unstaged), and the
/// status buffer's per-section `d` binding (against the index, for
/// exactly one side of the working tree).
#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffScope {
    /// `git diff HEAD` — staged + unstaged changes combined.
    Head,
    /// `git diff --cached` — index vs HEAD (the Staged section).
    Staged,
    /// `git diff` — working tree vs index (the Unstaged section).
    Unstaged,
    /// MG.43e: magit's merge `p` — what merging `branch` would bring
    /// in, without merging it.
    ///
    /// `git diff HEAD...<branch>` (THREE dots) is the right question:
    /// it shows what `branch` added since the two diverged. The
    /// two-dot form would also report everything HEAD gained in the
    /// meantime as though the merge were removing it, which is the
    /// opposite of what a preview is for.
    MergePreview(String),
}

impl DiffScope {
    /// MG.14: how this scope reads in the headerline. The same three
    /// words `parse_buffer_name` accepts, so the header echoes the
    /// buffer name rather than inventing a second vocabulary.
    fn header_label(&self) -> &'static str {
        match self {
            DiffScope::Head => "HEAD",
            DiffScope::Staged => "staged",
            DiffScope::Unstaged => "unstaged",
            DiffScope::MergePreview(_) => "merge preview",
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
    /// MG.18d: the wake-baked bus a post-mutation cursor goes back on.
    cursor_bus: Option<crate::cursor_restore::CursorBusHandle>,
    /// MG.23k: extra git arguments the `D` menu set, replayed on every
    /// subsequent refresh so `gr` does not silently revert to the
    /// default diff.
    extra_args: Vec<String>,
    /// MG.22b: the config, not the value. Read per refresh so a
    /// `:set magit.hunk.context-lines` takes effect on the next `gr`
    /// rather than only on reopen.
    config: Option<std::sync::Arc<lattice_config::ConfigRegistry>>,
}

/// `magit.hunk.context-lines`, or git's own default when there is no
/// config registry (a stripped harness).
fn context_lines(config: &Option<std::sync::Arc<lattice_config::ConfigRegistry>>) -> i64 {
    config
        .as_ref()
        .and_then(|c| c.get_typed::<crate::options::MagitHunkContextLines>())
        .map(|v| *v)
        .unwrap_or(3)
}

/// Parse `"*magit:diff[:staged|:unstaged]:<path>*"` (or the bare
/// unscoped `"*magit:diff*"`) into a `(scope, path)` pair. The
/// `staged`/`unstaged` infix must be checked before the bare
/// `"*magit:diff:"` prefix, since that prefix is itself a substring
/// of both scoped forms.
fn parse_buffer_name(name: &str) -> (DiffScope, Option<PathBuf>) {
    // MG.43e: checked FIRST. The generic `*magit:diff:` arm below
    // would otherwise match `merge-preview:<branch>` and read the
    // whole thing as a PATH, silently diffing a file that does not
    // exist instead of previewing a merge.
    if let Some(s) = name
        .strip_prefix("*magit:diff:merge-preview:")
        .and_then(|s| s.strip_suffix('*'))
        && !s.is_empty()
    {
        return (DiffScope::MergePreview(s.to_string()), None);
    }
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
                    scope: scope.clone(),
                    path: path.clone(),
                    pending_highlights: pending_highlights.clone(),
                    cursor_bus: ctx
                        .service::<crate::cursor_restore::CursorBusHandle>()
                        .map(|outer| (*outer).clone()),
                    extra_args: Vec::new(),
                    config: ctx
                        .service::<std::sync::Arc<lattice_config::ConfigRegistry>>()
                        .map(|outer| (*outer).clone()),
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
            let context = context_lines(
                &ctx.service::<std::sync::Arc<lattice_config::ConfigRegistry>>()
                    .map(|outer| (*outer).clone()),
            );
            let text = tokio::task::spawn_blocking(move || {
                run_diff(&wd, &scope, path_for_task.as_deref(), &[], context)
            })
            .await
            .unwrap_or_default();
            let spans = crate::highlight::diff_styled_spans(&text);
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

fn refresh(s: Arc<Mutex<DiffState>>) -> Option<Effect> {
    refresh_with(s, None)
}

/// Rebuild the diff, and — when a mutation supplied one — put the
/// cursor back on the hunk that took the staged one's place.
///
/// MG.18d: the position is resolved against the text this rebuild is
/// about to write and sent afterwards, so it can neither race the
/// replace nor be clamped against the outgoing content. The send wakes
/// the editor, so it lands without the user pressing anything
/// (`boot-composition.md` §3).
fn refresh_with(
    s: Arc<Mutex<DiffState>>,
    restore: Option<crate::cursor_restore::HunkRestore>,
) -> Option<Effect> {
    let (handle, wd, scope, path, pending, buffer_id, cursor_bus, extra, context) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.scope.clone(),
            g.path.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
            g.cursor_bus.clone(),
            g.extra_args.clone(),
            context_lines(&g.config),
        )
    };
    tokio::task::spawn(async move {
        let text = tokio::task::spawn_blocking(move || {
            run_diff(&wd, &scope, path.as_deref(), &extra, context)
        })
        .await
        .unwrap_or_default();
        let spans = crate::highlight::diff_styled_spans(&text);
        let position = restore.and_then(|r| crate::cursor_restore::restore_position(&text, &r));
        crate::buffer_io::replace_buffer_text(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
        if let Some(position) = position {
            crate::cursor_restore::send_cursor(&cursor_bus, buffer_id, position);
        }
    });
    None
}

fn spawn_mutation_and_refresh(
    s: Arc<Mutex<DiffState>>,
    mutate: impl FnOnce() + Send + 'static,
) -> Option<Effect> {
    let (handle, wd, scope, path, pending, buffer_id, extra, context) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.scope.clone(),
            g.path.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
            g.extra_args.clone(),
            context_lines(&g.config),
        )
    };
    tokio::task::spawn(async move {
        let _ = tokio::task::spawn_blocking(mutate).await;
        let text = tokio::task::spawn_blocking(move || {
            run_diff(&wd, &scope, path.as_deref(), &extra, context)
        })
        .await
        .unwrap_or_default();
        let spans = crate::highlight::diff_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Walk upward from `line` to the nearest `diff --git a/<path> b/<path>`
/// header and extract `<path>` (the `b/` side — the current-tree path).
/// MG.22: the shared diff-path parser, read through this view's own
/// buffer. The scan itself lives in `hunk` — three modes had a copy of
/// it and one of them had a bug the other two did not.
fn file_at_cursor(state: &DiffState, line: u32) -> Option<PathBuf> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    crate::hunk::path_at_cursor(
        |i| u32::try_from(i).ok().and_then(|l| snap.buffer.line(l)),
        line as usize,
    )
}

/// MG.23k: the arguments `D` offers in a diff buffer.
///
/// magit's `magit-diff` transient carries many more; these are the
/// three that change how the SAME diff reads. Arguments that change
/// *which* diff it is (`--cached`, a revision range) are deliberately
/// absent — the buffer's scope is in its name, so a menu row that
/// silently made `*magit:diff:staged:x*` show unstaged content would
/// leave the headerline and the buffer name both lying.
pub(crate) const DIFF_ARGS: &[crate::magit_global_mode::RemoteFlag] = &[
    crate::magit_global_mode::RemoteFlag {
        name: "ignore-space",
        arg: "-w",
        key: "-w",
        doc: "Ignore whitespace-only changes",
        kind: crate::magit_global_mode::RemoteArgKind::Flag,
    },
    crate::magit_global_mode::RemoteFlag {
        name: "stat",
        arg: "--stat",
        key: "-s",
        doc: "Show a summary of changed files instead of the patch",
        kind: crate::magit_global_mode::RemoteArgKind::Flag,
    },
    crate::magit_global_mode::RemoteFlag {
        // Joined, not separated: `git diff -U 3` and `--unified 3` are
        // both errors. See `RemoteArgKind::ValueJoined`.
        name: "unified",
        arg: "--unified=",
        key: "-U",
        doc: "Lines of context around each hunk",
        kind: crate::magit_global_mode::RemoteArgKind::ValueJoined {
            prompt: "Context lines",
        },
    },
];

/// The `git` argv for one diff run.
///
/// Pure and separate from the spawning path, for the reason
/// `blame_argv` and `tag_argv` already are: the flags and their ORDER
/// are the part worth testing, and reaching them through the runner
/// would mean every test needed a repository.
fn run_diff_argv(
    scope: &DiffScope,
    path: Option<&Path>,
    extra: &[String],
    context: i64,
) -> Vec<String> {
    let mut args = vec!["diff".to_string()];
    match scope {
        DiffScope::Head => args.push("HEAD".to_string()),
        DiffScope::Staged => args.push("--cached".to_string()),
        // `git diff` with no ref compares the working tree against
        // the index — exactly the Unstaged section's semantics.
        DiffScope::Unstaged => {}
        DiffScope::MergePreview(branch) => args.push(format!("HEAD...{branch}")),
    }
    // MG.22b: `magit.hunk.context-lines` is the DEFAULT, so it only
    // applies when `D` did not set one — the same precedence
    // `run_log` gives its `-n`. Appending both and letting git take
    // the last would work, but it puts two contradictory `-U`s in the
    // argv and the next reader cannot tell which wins.
    if !extra.iter().any(|a| a.starts_with("--unified")) {
        args.push(format!("--unified={context}"));
    }
    // MG.23k: the `D` menu's arguments go before the `--` separator,
    // or git reads them as pathspecs.
    args.extend(extra.iter().cloned());
    if let Some(p) = path {
        args.push("--".to_string());
        args.push(p.to_string_lossy().into_owned());
    }
    args
}

/// MG.43e: the merge-preview argv, for the assertion that its range
/// uses three dots. Exposed rather than reconstructed in the test so
/// the test cannot drift from what actually runs.
#[cfg(test)]
pub(crate) fn merge_preview_argv_for_test(branch: &str) -> Vec<String> {
    run_diff_argv(&DiffScope::MergePreview(branch.to_string()), None, &[], 3)
}

fn run_diff(
    workdir: &Path,
    scope: &DiffScope,
    path: Option<&Path>,
    extra: &[String],
    context: i64,
) -> String {
    let args = run_diff_argv(&scope, path, extra, context);
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
                    DiffScope::MergePreview(b) => {
                        format!("Merging {b} would bring in no changes.\n")
                    }
                }
            } else {
                text
            }
        }
        _ => "Not a git repository, or no commits yet.\n".to_string(),
    }
}

/// MG.18c: which tree this buffer's hunks can be moved between.
///
/// Split from [`MagitView::diff_source`] so the mapping is testable
/// without a live buffer and a spawned document actor.
fn source_for_scope(scope: &DiffScope) -> Option<DiffSource> {
    match scope {
        DiffScope::Staged => Some(DiffSource::Staged),
        DiffScope::Unstaged => Some(DiffSource::Unstaged),
        // MG.43e: a merge preview describes a merge that has NOT
        // happened, so there is no tree to stage a hunk into. Applying
        // one would write changes the branch has not been merged for —
        // `s` / `u` / `x` correctly decline here.
        DiffScope::Head | DiffScope::MergePreview(_) => None,
    }
}

/// `gr` for this view — `magit-core-mode` owns the chord and the one
/// boot-registered handler; see [`MagitView`].
struct DiffView(Arc<Mutex<DiffState>>);

impl MagitView for DiffView {
    /// This buffer's content is a unified diff, so "a file" is a
    /// `diff --git` header — not the generic indented-row scan, which
    /// here matches every indented CONTEXT line and would walk `]f`
    /// through arbitrary code.
    fn file_lines(
        &self,
        store: &lattice_mode::BufferStoreHandle,
        buffer: lattice_core::BufferId,
    ) -> Option<Vec<u32>> {
        Some(crate::magit_core_mode::diff_file_lines(store, buffer))
    }

    /// MG.22: the scope this buffer was opened at decides which
    /// version `<CR>` opens — the index blob for a staged diff, the
    /// live file otherwise. The `Head` scope combines both sides, so
    /// the working-tree copy is the only version that is definitely
    /// what the user is looking at.
    fn diff_target(&self, path: &std::path::Path) -> Option<Effect> {
        let g = self.0.lock().ok()?;
        match g.scope {
            DiffScope::Staged => Some(Effect::OpenSyntheticBuffer {
                name: crate::magit_file_revision_mode::blob_buffer_name("staged", path),
                mode_id: "magit-file-revision-mode".to_string(),
            }),
            DiffScope::Head | DiffScope::Unstaged | DiffScope::MergePreview(_) => {
                let full = g.workdir.join(path);
                full.exists().then_some(Effect::OpenBuffer {
                    path: Some(full),
                    force: false,
                })
            }
        }
    }

    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }

    /// MG.23k: `D`'s rows for a diff buffer — magit's own diff
    /// arguments, minus the ones that would change what this buffer
    /// *is* rather than how it renders.
    fn argument_flags(&self) -> &'static [crate::magit_global_mode::RemoteFlag] {
        DIFF_ARGS
    }

    fn refresh_with_args(&self, extra: Vec<String>) -> Option<Effect> {
        if let Ok(mut g) = self.0.lock() {
            g.extra_args = extra;
        }
        refresh(self.0.clone())
    }

    /// MG.18c: the buffer's scope answers this for its whole content —
    /// every line came from one `git diff` invocation.
    ///
    /// `DiffScope::Head` deliberately yields `None`. `git diff HEAD`
    /// combines staged and unstaged changes into single hunks, so a
    /// hunk from it is not a patch against either tree, and staging it
    /// would be guesswork. `d s` / `d u` from magit-status open the
    /// scoped views where the question has an answer.
    fn diff_source(&self, _cursor: Position) -> Option<DiffSource> {
        source_for_scope(&self.0.lock().ok()?.scope)
    }

    /// MG.18d: a diff buffer's landmark is the `diff --git` header —
    /// it has no entry rows, and its staged/unstaged identity belongs
    /// to the whole buffer rather than to a section within it.
    fn refresh_restoring(&self, site: crate::cursor_restore::HunkSite) -> Option<Effect> {
        refresh_with(self.0.clone(), Some(site.as_diff_header()))
    }

    fn workdir(&self) -> Option<PathBuf> {
        Some(self.0.lock().ok()?.workdir.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.22b: the option is the DEFAULT, so `D`'s `--unified` wins.
    /// Emitting both and letting git take the last would work, but it
    /// puts two contradictory `-U`s in the argv and the next reader
    /// cannot tell which one applies.
    #[test]
    fn the_context_option_yields_to_the_menus_override() {
        let with_default = run_diff_argv(&DiffScope::Unstaged, None, &[], 7);
        assert!(
            with_default.contains(&"--unified=7".to_string()),
            "the option supplies the context when the menu did not: {with_default:?}"
        );

        let overridden = run_diff_argv(&DiffScope::Unstaged, None, &["--unified=1".to_string()], 7);
        assert!(
            overridden.contains(&"--unified=1".to_string()),
            "the menu's value must be there: {overridden:?}"
        );
        assert_eq!(
            overridden
                .iter()
                .filter(|a| a.starts_with("--unified"))
                .count(),
            1,
            "exactly one `--unified` reaches git: {overridden:?}"
        );
    }

    /// The arguments must land before `--`, or git reads them as
    /// pathspecs and the diff silently comes back empty.
    #[test]
    fn every_argument_precedes_the_path_separator() {
        let argv = run_diff_argv(
            &DiffScope::Staged,
            Some(std::path::Path::new("src/main.rs")),
            &["-w".to_string()],
            3,
        );
        let sep = argv.iter().position(|a| a == "--").expect("a separator");
        for flag in ["--cached", "-w", "--unified=3"] {
            let at = argv
                .iter()
                .position(|a| a == flag)
                .unwrap_or_else(|| panic!("`{flag}` missing from {argv:?}"));
            assert!(at < sep, "`{flag}` must precede `--`: {argv:?}");
        }
    }

    /// MG.18c — `*magit:diff*` compares against HEAD, so one hunk can
    /// contain both staged and unstaged lines. It is not a patch
    /// against either tree, and `git apply` would either refuse it or
    /// (worse) accept a partially-correct one. `d s` / `d u` from
    /// magit-status open the scoped views where the question has an
    /// answer; here staging stays file-level.
    #[test]
    fn only_the_scoped_views_can_stage_a_hunk() {
        assert_eq!(
            source_for_scope(&DiffScope::Staged),
            Some(DiffSource::Staged)
        );
        assert_eq!(
            source_for_scope(&DiffScope::Unstaged),
            Some(DiffSource::Unstaged)
        );
        assert_eq!(
            source_for_scope(&DiffScope::Head),
            None,
            "a HEAD diff mixes both sides in one hunk"
        );
    }

    /// The buffer name is the only carrier of scope, so a parse that
    /// drifted would silently reclassify every hunk in the view.
    #[test]
    fn the_scope_a_buffer_name_encodes_survives_the_round_trip() {
        for (name, scope) in [
            ("*magit:diff*", DiffScope::Head),
            ("*magit:diff:staged:src/a.rs*", DiffScope::Staged),
            ("*magit:diff:unstaged:src/a.rs*", DiffScope::Unstaged),
        ] {
            assert_eq!(parse_buffer_name(name).0, scope, "{name}");
        }
    }
}
