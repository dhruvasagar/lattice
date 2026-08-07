//! MG.3: magit-status action handlers.
//!
//! Each handler captures shared state from the mode's Guard so it
//! can read the cursor line, resolve the repo, and invoke git
//! operations. Async operations (diff expansion, refresh) use the
//! stored tokio handle — no `Runtime::new()`, no `block_on`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, PendingSyntheticHighlights,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_vcs::{Index, Repository};

use crate::buffer_state::DiffSource;
use crate::refresh;

pub struct StatusBufferState {
    pub buffer_id: BufferId,
    pub store: Arc<BufferStoreHandle>,
    pub workdir: PathBuf,
    pub runtime: tokio::runtime::Handle,
    /// MG.2: optional handle to store styled spans after async edit
    /// lands, so highlights appear without a keystroke.
    pub pending_highlights: Option<std::sync::Arc<PendingSyntheticHighlights>>,
    /// Entries currently inline-expanded (file diff / stash show /
    /// commit show), keyed by [`entry_key`], value = number of buffer
    /// lines the expansion occupies.
    ///
    /// MG.18d: a refresh no longer clears this. The rebuild *carries*
    /// the open entries' diffs (`refresh::build_and_format`), so the
    /// map is replaced with counts recomputed from the text that was
    /// actually written — staging a hunk makes a diff shorter, and a
    /// carried-over count would then collapse the wrong rows.
    pub expanded: HashMap<String, usize>,
    /// MG.14: the buffer's headerline — branch, ahead/behind, repo
    /// name, dirty counts. Re-set by every refresh from the same
    /// `SectionIndex` the body is built from.
    pub headerline: Option<crate::headerline::MagitHeaderlineHandle>,
    /// MG.22b: the config, not the value — read per refresh so a
    /// `:set magit.hunk.context-lines` takes effect on the next `gr`
    /// rather than only on reopen.
    pub config: Option<Arc<lattice_config::ConfigRegistry>>,
    /// MG.18d: where the cursor should land once the next refresh's
    /// text exists. Set by a mutation, consumed by the refresh it was
    /// queued for — a later `gr` must not re-apply a stale jump.
    pub pending_cursor: Option<crate::cursor_restore::HunkRestore>,
    /// MG.18d: the wake-baked bus the resolved position goes back on.
    pub cursor_bus: Option<crate::cursor_restore::CursorBusHandle>,
}

// ── line classification ─────────────────────────────────

/// What kind of entry occupies a status-buffer line. Derived directly
/// from the rendered line's fixed layout (see
/// `SectionIndex::format_buffer_styled`), not by guessing at word
/// boundaries — this is what lets `classify_line` tell a "new file"
/// (two-word label) entry apart from every other one-word label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusLine {
    File { path: PathBuf, staged: bool },
    Stash { index: usize },
    Commit { sha: String },
}

/// Status labels `SectionIndex::format_buffer_styled` renders via
/// `format!("  {:<12} {}", label, path)`. Checked as whole-word
/// prefixes (label followed by whitespace) so diff content inserted
/// by a toggled-open entry — which can start with an arbitrary
/// number of leading spaces when the underlying source line is
/// itself indented — never collides with these.
///
/// Must stay in sync with `sections::status_label`'s outputs (the
/// deduplicated set — `PathStatus::Modified` and `::Conflicted` both
/// render `"modified"`).
pub(crate) const FILE_LABELS: [&str; 7] = [
    "clean",
    "modified",
    "new file",
    "deleted",
    "untracked",
    "ignored",
    "unmerged",
];

/// Classify the entry at `line`, or `None` if it isn't a
/// stage/unstage/visit-able entry line (a header, blank line, or
/// content inside an inline-expanded diff).
pub(crate) fn classify_line(state: &StatusBufferState, line: u32) -> Option<StatusLine> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    let text = snap.buffer.line(line)?;
    // `section_header_above` needs the live buffer, so only call it
    // when `classify_line_text` actually needs to disambiguate
    // (File → staged?, or the Recent-commits fallback) — see there.
    classify_line_text(&text, || section_header_above(state, line))
}

/// The pure classification core of [`classify_line`], split out so it's
/// testable without a live buffer/store. `header_above` is called lazily
/// (only when a candidate match needs to know its enclosing section) so
/// callers with a real buffer don't pay for an unnecessary backward scan.
pub(crate) fn classify_line_text(
    text: &str,
    header_above: impl FnOnce() -> Option<String>,
) -> Option<StatusLine> {
    if !text.starts_with("  ") {
        return None;
    }
    let trimmed = &text[2..];
    if let Some(rest) = trimmed.strip_prefix("stash@{") {
        let idx_str = rest.split('}').next()?;
        return Some(StatusLine::Stash {
            index: idx_str.parse().ok()?,
        });
    }
    for label in FILE_LABELS {
        if let Some(rest) = trimmed.strip_prefix(label)
            && rest.starts_with(char::is_whitespace)
        {
            let path = PathBuf::from(rest.trim_start());
            let staged = header_above()
                .map(|h| h.starts_with("Staged"))
                .unwrap_or(false);
            return Some(StatusLine::File { path, staged });
        }
    }
    // Only "Recent commits" entries fall through to here: "<sha> <subject>".
    let header = header_above()?;
    if header.starts_with("Recent commits") {
        let sha = trimmed.split_whitespace().next()?;
        if !sha.is_empty() && sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(StatusLine::Commit {
                sha: sha.to_string(),
            });
        }
    }
    None
}

/// Stable identity for a `StatusLine`, used as the [`StatusBufferState::expanded`]
/// key. Includes `staged` for `File` — a `Conflicted` path appears in
/// BOTH the Staged and Unstaged sections simultaneously (see
/// `refresh::build_section_index`), as two distinct buffer rows that
/// can be independently expanded; collapsing that distinction would
/// let expanding one row's diff make `toggle_expand` treat the
/// *other* row as already-expanded too, and collapse the wrong line
/// range.
pub(crate) fn entry_key(sl: &StatusLine) -> String {
    match sl {
        StatusLine::File { path, staged } => format!("f:{staged}:{}", path.display()),
        StatusLine::Stash { index } => format!("s:{index}"),
        StatusLine::Commit { sha } => format!("c:{sha}"),
    }
}

fn section_header_above(state: &StatusBufferState, line: u32) -> Option<String> {
    let handle = state.store.handle_for(state.buffer_id)?;
    let snap = handle.snapshot();
    for l in (0..=line).rev() {
        let text = snap.buffer.line(l)?;
        let t = text.trim();
        if crate::sections::is_section_header(t) {
            return Some(t.to_string());
        }
    }
    None
}

/// MG.18c: map a status section header to the tree its entries' diffs
/// were produced against.
///
/// Untracked files count as Unstaged: a whole-file `s` there is
/// `git add`, and an untracked file has no diff to expand, so the
/// hunk path never reaches this with one — the row is here so the
/// mapping is total rather than silently defaulting.
///
/// Split from [`StatusView::diff_source`] so the classification is
/// testable without a live buffer, the same split `classify_line` /
/// `classify_line_text` already uses.
pub(crate) fn diff_source_for_header(header: &str) -> Option<DiffSource> {
    if header.starts_with("Staged") {
        Some(DiffSource::Staged)
    } else if header.starts_with("Unstaged") || header.starts_with("Untracked") {
        Some(DiffSource::Unstaged)
    } else {
        // "Recent commits", "Stashes", "Merge conflicts", …
        None
    }
}

/// Run the git command that shows `sl`'s content: a file's diff
/// (staged-aware), a stash's patch, or a commit's patch.
pub(crate) fn run_show(workdir: &Path, sl: &StatusLine, context: i64) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(workdir);
    // MG.22b: `magit.hunk.context-lines` applies to every patch magit
    // generates, not only the dedicated diff view — a value honoured in
    // `:magit-diff` but ignored by magit-status's inline `=` would be
    // the more confusing half of a half-migration.
    let unified = format!("--unified={context}");
    match sl {
        StatusLine::File { path, staged } => {
            cmd.arg("diff");
            if *staged {
                cmd.arg("--cached");
            }
            cmd.arg(&unified).arg("--").arg(path);
        }
        StatusLine::Stash { index } => {
            cmd.args([
                "stash",
                "show",
                "-p",
                &unified,
                &format!("stash@{{{index}}}"),
            ]);
        }
        StatusLine::Commit { sha } => {
            cmd.args(["show", &unified, sha]);
        }
    }
    let output = cmd.output().ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

/// Toggle the inline expansion of `sl` at `cursor_line`: collapse it
/// if already expanded (removing exactly the number of lines recorded
/// in `StatusBufferState::expanded` — not a re-scanned guess), or
/// insert its `git show`/`git diff` output and record the inserted
/// line count if collapsed. Shared by `=` (files) and `<CR>`
/// (stashes/commits).
/// MG.44: what a press on an already-classified file line should do.
///
/// Pulled out as a pure decision because it is the whole behavioural
/// change: before, an expanded entry was DELETED from the buffer and
/// the next press re-ran `git diff`. Now it folds, so the fetched rows
/// survive. Keeping the decision separate from the effect is what
/// makes that assertable without a `BufferStoreHandle`.
#[derive(Debug, PartialEq, Eq)]
enum DiffToggle {
    /// Nothing fetched yet — run `git diff` and insert it.
    Fetch,
    /// Rows are present — hide/show them, keeping the text.
    Fold,
    /// Recorded as expanded but occupying no rows, so there is
    /// nothing to fold; forget it instead.
    Drop,
}

impl DiffToggle {
    fn for_state(existing_count: Option<usize>) -> Self {
        match existing_count {
            None => Self::Fetch,
            Some(0) => Self::Drop,
            Some(_) => Self::Fold,
        }
    }
}

/// MG.44: the body BOTH `=` and `<Tab>` run on a status file line.
///
/// One operation with three states — not fetched, shown, hidden:
///
/// - not fetched -> run `git diff` and insert it
/// - shown       -> fold it shut (the rows stay)
/// - hidden      -> unfold
///
/// `=` and `<Tab>` were previously different operations on the same
/// line: one spliced text in and out, the other folded whatever was
/// already there. Sharing the body is what makes them agree, and it
/// has to be shared rather than duplicated because `<Tab>` is owned by
/// `magit-core-mode` (a MINOR mode, which outranks the status major in
/// the layer order) while `=` is the status major's own chord — two
/// copies would drift and only one of them would ever be reachable.
///
/// Off a file line there is nothing magit-specific to do, so the
/// generic fold toggle stands: `<Tab>` keeps working on section
/// headers and hunks exactly as before.
pub(crate) fn toggle_diff_or_fold(ctx: &ActionContext<'_>) -> Option<Effect> {
    let fold = || {
        Some(Effect::AppAction(
            lattice_grammar::AppEffect::ToggleFoldAtCursor,
        ))
    };
    let Some(s) = status_state(ctx) else {
        return fold();
    };
    let sl = {
        let Ok(g) = s.lock() else { return fold() };
        classify_line(&g, ctx.cursor.line)
    };
    let Some(sl @ StatusLine::File { .. }) = sl else {
        return fold();
    };
    toggle_expand(&s, sl, ctx.cursor.line)
}

fn toggle_expand(
    s: &Arc<Mutex<StatusBufferState>>,
    sl: StatusLine,
    cursor_line: u32,
) -> Option<Effect> {
    let key = entry_key(&sl);
    let (handle, wd, rt, existing_count, pending, bid, context, hl) = {
        let g = s.lock().ok()?;
        let h = g.store.handle_for(g.buffer_id)?;
        let context = context_lines(&g.config);
        (
            h,
            g.workdir.clone(),
            g.runtime.clone(),
            g.expanded.get(&key).copied(),
            g.pending_highlights.clone(),
            g.buffer_id,
            context,
            g.headerline.clone(),
        )
    };

    match DiffToggle::for_state(existing_count) {
        DiffToggle::Fold => {
            // MG.44: **hide it, do not delete it.**
            //
            // This branch used to splice the diff out of the buffer, so
            // re-showing it re-ran `git diff` — throwing away work
            // already done and paying I/O for a keystroke that shows
            // text the buffer had a moment ago. A fold hides the rows
            // and keeps them, which is what emacs magit does and what
            // the buffer model already provides.
            //
            // Nothing is removed from `expanded` either: the entry IS
            // still expanded, it is merely folded shut. Clearing it
            // would make the next press re-fetch, which is the very
            // thing this removed — and would desync the fold ranges
            // `MagitStatusFoldSource` derives from that map.
            return Some(Effect::AppAction(
                lattice_grammar::AppEffect::ToggleFoldAtCursor,
            ));
        }
        DiffToggle::Drop => {
            // A zero-line expansion has no rows to fold, so there is
            // nothing to hide and the entry is dropped as before.
            if let Ok(mut g) = s.lock() {
                g.expanded.remove(&key);
            }
        }
        DiffToggle::Fetch => {
            let pos = Position::new(cursor_line + 1, 0);
            let start_line = cursor_line + 1;
            let s = s.clone();
            let path = match &sl {
                StatusLine::File { path, .. } => path.display().to_string(),
                _ => String::new(),
            };
            rt.spawn(async move {
                // MG.31: the git call happens HERE, inside the spawned
                // task, not above on the actor thread. See
                // [`expand_payload`].
                let (text, line_count, spans) = match expand_payload(wd, sl, context).await {
                    Ok(payload) => payload,
                    // MG.56: say something. Returning quietly here is
                    // what made `=` look like an unbound key on a row
                    // whose changes had been committed elsewhere — the
                    // press did fire, git did answer, and the answer
                    // was an empty patch.
                    Err(miss) => {
                        crate::headerline::publish_notice(
                            &hl,
                            Some(match miss {
                                ExpandMiss::NoChanges => {
                                    format!("no changes in {path} — press gr to refresh")
                                }
                                ExpandMiss::Failed(e) => {
                                    format!("could not diff {path}: {e}")
                                }
                            }),
                        );
                        return;
                    }
                };
                let _ = handle
                    .apply_edit_batch(vec![Edit::insert(pos, format!("{}\n", text))])
                    .await;
                // Recorded only after the insert lands — see the
                // collapse-branch comment above. Recording it
                // beforehand let a rapid second `=`/`<CR>` press see
                // "already expanded" and race the collapse branch
                // against rows the insert hadn't populated yet.
                if let Ok(mut g) = s.lock() {
                    g.expanded.insert(key, line_count);
                }
                if let Some(ref ph) = pending {
                    ph.insert_at_and_wake(bid, start_line, spans);
                }
            });
        }
    }
    None
}

/// MG.31: the blocking half of an inline expansion — the `git` call and
/// the styling of its output — on the blocking pool.
///
/// **Why this is a function and not three lines in `toggle_expand`.**
/// `toggle_expand` is an action handler, so its body runs on the editor
/// actor's `current_thread` runtime (`editor_actor.rs`: one task,
/// `run_actor` processes commands one at a time). [`run_show`] ends in
/// `Command::output()` — a fork/exec plus wait — so calling it from the
/// handler stalled the loop that services keystrokes for the whole
/// duration of a `git diff`, which alone exceeds paramount-goal-1's
/// one-frame ceiling. Every other magit view already ran its git call
/// inside `spawn_blocking`, including [`run_show`]'s two other callers;
/// this path was the one that did not.
///
/// The styling moves with it deliberately: it is `O(lines)` over the
/// diff and belongs on the same side of the boundary as the call that
/// produced it.
///
/// `None` when the entry has no diff to show (git failed, or the output
/// was blank) — the caller then inserts nothing, exactly as before.
/// Why an expansion produced nothing.
///
/// The two used to collapse into one `None`, and the caller returned
/// silently on either — so pressing `=` on a file whose changes had
/// since been committed elsewhere did *nothing*, repeatedly, and looked
/// exactly like an unbound key. They are different problems with
/// different fixes and have to be told apart to say anything useful.
#[derive(Debug)]
pub(crate) enum ExpandMiss {
    /// git answered, and the answer was an empty patch. Almost always
    /// a stale buffer: the row is still listed because the status scan
    /// that produced it has been overtaken by a commit, a stage, or an
    /// edit made outside this view.
    NoChanges,
    /// The git call itself failed.
    Failed(String),
}

async fn expand_payload(
    workdir: PathBuf,
    sl: StatusLine,
    context: i64,
) -> Result<(String, usize, Vec<Vec<lattice_cells::style::StyledSpan>>), ExpandMiss> {
    tokio::task::spawn_blocking(move || {
        let raw = run_show(&workdir, &sl, context)
            .ok_or_else(|| ExpandMiss::Failed("git could not read the diff".to_string()))?;
        if raw.trim().is_empty() {
            return Err(ExpandMiss::NoChanges);
        }
        // MG.46: only the trailing newline goes. A patch is not free
        // text — each hunk's `@@` header declares how many body lines
        // follow, and `hunk_fold_source` bounds the fold by that count.
        // `.trim()` also ate a trailing blank context line (git emits
        // one as a lone space), leaving the text one line shorter than
        // its own header claimed, and the fold then ran past the end of
        // the diff into the status rows below.
        let text = raw.trim_end_matches('\n').to_string();
        let line_count = text.lines().count();
        let spans = crate::highlight::diff_styled_spans(&text);
        Ok((text, line_count, spans))
    })
    .await
    .unwrap_or_else(|e| Err(ExpandMiss::Failed(e.to_string())))
}

// ── registration ────────────────────────────────────────

/// MG.13: service alias for magit-status's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type StatusStatesHandle = Arc<crate::buffer_state::BufferStates<StatusBufferState>>;

/// Resolve the status buffer's state for the buffer an action fired
/// in. `None` means this is not a live magit-status buffer, so the
/// handler declines — the same outcome as before, minus the race.
pub(crate) fn status_state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<StatusBufferState>>> {
    crate::buffer_state::state_for::<StatusBufferState>(ctx)
}

/// MG.13: magit-status's action handlers, registered once at boot by
/// `MagitStatusMode::action_handlers()`.
///
/// Each body opens with `let s = status_state(ctx)?;` — resolving this
/// buffer's state from the `BufferStates<StatusBufferState>` service
/// rather than closing over it at activation. That removes the window
/// in which `x` / `=` / `<CR>` resolved but had no handler yet. `s`,
/// `u` and `gr` are NOT here: they are shared with `magit-diff-mode`,
/// so `magit-core-mode` owns their single handler and reaches this
/// buffer through [`StatusView`] (see `buffer_state::MagitView`).
pub fn status_action_handlers() -> Vec<ActionHandlerContribution> {
    let mut contributions: Vec<ActionHandlerContribution> = Vec::new();

    macro_rules! handler {
        ($name:expr, $body:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $name,
                handler: Arc::new($body),
            });
        };
    }

    // Run `mutate` (a blocking git call) on `spawn_blocking`, off the
    // actor thread entirely, then refresh — the shape every mutating
    // handler below uses instead of calling git synchronously inline.
    // Handlers read whatever cursor/path state they need up front
    // (fast, in-memory) and hand this a self-contained closure; the
    // handler itself returns `None` immediately, before the git call
    // has even started.

    // ── stage (s) ──────────────────────────────────────
    // MG.13: registered once at boot by `magit-core-mode` (a shared
    // action — `magit-diff-mode` binds `s` too) and dispatched
    // through `StatusView`'s `MagitView` impl below.

    // ── unstage (u) ───────────────────────────────────
    // MG.13: registered once at boot by `magit-core-mode` (a shared
    // action — `magit-diff-mode` binds `u` too) and dispatched
    // through `StatusView`'s `MagitView` impl below.

    // ── discard (x) ───────────────────────────────────
    // PU.6: prompt for confirmation before destructive discard.
    //
    // MG.18c: hunk-at-cursor first, exactly as `s` / `u` resolve —
    // but through the ask/execute pair, because §12.13 requires a
    // destructive action's chord to perform no git call at all. The
    // prompt names the hunk's file position so the question is
    // answerable without dismissing it.
    {
        handler!("action:magit-discard", move |ctx: &ActionContext<'_>| {
            match crate::magit_core_mode::resolve_hunk(ctx, crate::magit_core_mode::HunkOp::Discard)
            {
                crate::magit_core_mode::HunkResolution::Ready {
                    patch,
                    region_lines,
                    workdir,
                    ..
                } => {
                    // MG.18e: the prompt names what will actually go.
                    // "Discard hunk" over a 2-line selection would be a
                    // question about something the user did not ask for
                    // — and §12.13 requires the question to be
                    // answerable without dismissing it.
                    let target = match region_lines {
                        Some(1) => format!("1 line of {}", patch.display_location()),
                        Some(n) => format!("{n} lines of {}", patch.display_location()),
                        None => format!("hunk at {}", patch.display_location()),
                    };
                    // IX.2: carry the PATCH, not the rows it came
                    // from. A row span is a coordinate a rebuild
                    // invalidates — a refresh landing while the dialog
                    // is open would make the same span mean different
                    // lines. The patch is content, so it still means
                    // what it meant; and if the tree moved under it,
                    // `git apply`'s context check refuses it loudly
                    // rather than discarding somewhere plausible.
                    Some(crate::confirm::ask_with(
                        format!("Discard {target}?"),
                        "action:magit-discard-execute",
                        lattice_grammar::Args::List(vec![
                            lattice_grammar::ArgValue::String(String::new()),
                            lattice_grammar::ArgValue::String(patch.to_patch()),
                            lattice_grammar::ArgValue::String(
                                workdir.to_string_lossy().into_owned(),
                            ),
                        ]),
                    ))
                }
                crate::magit_core_mode::HunkResolution::Refused(effect) => Some(effect),
                crate::magit_core_mode::HunkResolution::FileLevel => {
                    let s = status_state(ctx)?;
                    let g = s.lock().ok()?;
                    let StatusLine::File { path, .. } = classify_line(&g, ctx.cursor.line)? else {
                        return None;
                    };
                    drop(g);
                    Some(crate::confirm::ask_target(
                        format!("Discard changes to {}?", path.display()),
                        "action:magit-discard-execute",
                        path.to_string_lossy().into_owned(),
                    ))
                }
            }
        });
    }
    // PU.6: actual discard, dispatched by Confirm's yes-action.
    //
    // IX.2: acts on what the prompt named. The ask half carries either
    // the synthesized patch (hunk / region) or the path (file), and
    // this half prefers that over anything it could re-derive — a
    // refresh landing while the dialog is open rebuilds the buffer and
    // moves the cursor, so re-derivation is how you discard a file you
    // never confirmed.
    //
    // A patch is content, not coordinates, so it still means what it
    // meant; and if the working tree moved under it, `git apply`'s
    // exact-context check refuses it loudly instead of applying it at a
    // plausible-looking offset.
    {
        handler!(
            "action:magit-discard-execute",
            move |ctx: &ActionContext<'_>| {
                // Slot 1 is the carried patch, slot 2 its workdir.
                if let (Some(patch), Some(workdir)) = (ctx.arg_str(1), ctx.arg_str(2))
                    && !patch.is_empty()
                {
                    return Some(crate::magit_core_mode::spawn_patch_discard(
                        std::path::PathBuf::from(workdir),
                        patch.to_string(),
                        crate::buffer_state::view_for(ctx),
                    ));
                }
                if let Some(path) = crate::confirm::carried_target(ctx)
                    && !path.is_empty()
                {
                    let s = status_state(ctx)?;
                    let workdir = s.lock().ok()?.workdir.clone();
                    return spawn_mutation_and_refresh(
                        s.clone(),
                        format!("discard {path}"),
                        move || {
                            let repo = Repository::discover(&workdir)
                                .map_err(|e| format!("not a git repository: {e}"))?;
                            repo.run_git(["checkout", "--", &path])
                                .map(|out| String::from_utf8_lossy(&out).into_owned())
                                .map_err(|e| e.to_string())
                        },
                    );
                }
                match crate::magit_core_mode::resolve_hunk(
                    ctx,
                    crate::magit_core_mode::HunkOp::Discard,
                ) {
                    crate::magit_core_mode::HunkResolution::Ready {
                        view,
                        workdir,
                        patch,
                        site,
                        region_lines,
                    } => Some(crate::magit_core_mode::spawn_hunk_apply(
                        view,
                        workdir,
                        patch,
                        crate::magit_core_mode::HunkOp::Discard,
                        site,
                        region_lines,
                    )),
                    crate::magit_core_mode::HunkResolution::Refused(effect) => Some(effect),
                    crate::magit_core_mode::HunkResolution::FileLevel => {
                        let s = status_state(ctx)?;
                        let (path, workdir) = {
                            let g = s.lock().ok()?;
                            let StatusLine::File { path, .. } = classify_line(&g, ctx.cursor.line)?
                            else {
                                return None;
                            };
                            (path, g.workdir.clone())
                        };
                        spawn_mutation_and_refresh(
                            s.clone(),
                            format!("discard {}", path.display()),
                            move || {
                                let repo = Repository::discover(&workdir)
                                    .map_err(|e| format!("not a git repository: {e}"))?;
                                repo.run_git(["checkout", "--", &path.to_string_lossy()])
                                    .map(|out| String::from_utf8_lossy(&out).into_owned())
                                    .map_err(|e| e.to_string())
                            },
                        )
                    }
                }
            }
        );
    }

    // ── visit (<CR>) ───────────────────────────────────
    // File entries open the file — the INDEX blob for a Staged
    // entry (`*magit:file:staged:<path>*`, read-only: this section
    // describes what's staged, which may already differ from a
    // since-edited working copy), the live editable working-tree
    // file for Unstaged (uniform with magit-diff-mode's own
    // Staged-vs-Unstaged `<CR>` split — see magit.md §6.3). Stash
    // entries toggle their inline patch, same mechanism `=` uses
    // for files (there's no dedicated "stash detail" buffer to open
    // instead).
    {
        handler!("action:magit-visit", move |ctx: &ActionContext<'_>| {
            let s = status_state(ctx)?;
            visit_status_line(&s, ctx.cursor.line)
        });
    }

    status_action_handlers_rest(&mut contributions);
    contributions
}

/// MG.22: magit-status's `<CR>` body, lifted out of the handler so the
/// `MagitView` can answer with it too.
///
/// `magit-hunk-mode` owns the chord; this stays the status buffer's
/// answer for rows that are not diff content, reached through
/// `MagitView::visit_at_cursor`. The `action:magit-visit` id is kept
/// so an ex-command or a user keymap can still reach it directly.
fn visit_status_line(s: &Arc<Mutex<StatusBufferState>>, line: u32) -> Option<Effect> {
    let sl = {
        let g = s.lock().ok()?;
        classify_line(&g, line)?
    };
    match sl {
        StatusLine::File { path, staged: true } => Some(Effect::OpenSyntheticBuffer {
            name: crate::magit_file_revision_mode::blob_buffer_name("staged", &path),
            mode_id: "magit-file-revision-mode".to_string(),
        }),
        StatusLine::File {
            path,
            staged: false,
        } => {
            let g = s.lock().ok()?;
            let full = g.workdir.join(&path);
            full.exists().then_some(Effect::OpenBuffer {
                path: Some(full),
                force: false,
            })
        }
        StatusLine::Stash { .. } => toggle_expand(s, sl, line),
        // Bug fix: `<CR>` on a commit SHA used to toggle the inline
        // diff (same as `=`) — but every other magit view that shows a
        // SHA (log, blame, rebase) treats `<CR>` as "open the dedicated
        // commit buffer", so status was the one inconsistent surface.
        // `=` still does the inline toggle for a quick look without
        // leaving the status buffer.
        StatusLine::Commit { sha } => Some(Effect::OpenSyntheticBuffer {
            name: format!("*magit:commit:{sha}*"),
            mode_id: "magit-revision-mode".to_string(),
        }),
    }
}

/// The remaining status handlers, split from
/// [`status_action_handlers`] only because `visit_status_line` had to
/// be lifted to module scope between them.
fn status_action_handlers_rest(contributions: &mut Vec<ActionHandlerContribution>) {
    macro_rules! handler {
        ($name:expr, $body:expr) => {
            contributions.push(ActionHandlerContribution {
                action_name: $name,
                handler: Arc::new($body),
            });
        };
    }

    // ── commit (cc) ───────────────────────────────────
    {
        handler!("action:magit-commit", move |ctx: &ActionContext<'_>| {
            let _ = status_state(ctx)?;
            Some(Effect::OpenSyntheticBuffer {
                name: "*magit:commit*".to_string(),
                mode_id: "magit-commit-mode".to_string(),
            })
        });
    }

    // ── commit amend (ca) ─────────────────────────────
    {
        handler!("action:magit-commit-amend", move |ctx: &ActionContext<
            '_,
        >| {
            let _ = status_state(ctx)?;
            Some(Effect::OpenSyntheticBuffer {
                name: "*magit:amend*".to_string(),
                mode_id: "magit-commit-mode".to_string(),
            })
        });
    }

    // ── stage patch (p) ───────────────────────────────
    // `git add -p` is genuinely interactive — it reads its own
    // prompts from stdin, which the TUI's raw-mode input loop already
    // owns. Running it via `Command::output()` (as this handler used
    // to) blocks the single-threaded actor waiting for a child that's
    // also waiting on stdin neither process routes to the other —
    // an indefinite hang, not just a slow blocking call. Until there's
    // a terminal-suspend mechanism (`:!`-style handoff) to route through,
    // fail loudly instead of hanging: stage via `s` (file-level) or
    // expand the diff with `=` and review before staging.
    {
        handler!("action:magit-stage-patch", move |ctx: &ActionContext<
            '_,
        >| {
            let _ = status_state(ctx)?;
            Some(Effect::Echo {
                level: lattice_grammar::EchoLevel::Error,
                text: "magit: interactive `git add -p` isn't supported yet — stage the whole \
                       file with `s`, or expand the diff with `=` to review first"
                    .to_string(),
            })
        });
    }

    // ── refresh (gr) ──────────────────────────────────
    // MG.13: registered once at boot by `magit-core-mode` and
    // dispatched through the status buffer's `MagitView`; see
    // `buffer_state::MagitView` for why it cannot be per-mode.

    // ── toggle diff (=) ───────────────────────────────
    {
        // MG.44: `=` and `<Tab>` are the same operation now — see
        // `toggle_diff_or_fold`.
        handler!("action:magit-toggle-diff", move |ctx: &ActionContext<
            '_,
        >| {
            toggle_diff_or_fold(ctx)
        });
    }

    // ── diff-file (d) — open a dedicated diff buffer scoped to
    // the file at cursor AND its section's baseline (index for
    // Staged, working-tree-vs-index for Unstaged), instead of
    // expanding inline like `=`. See `magit_diff_mode`'s `DiffScope`.
    {
        handler!("action:magit-diff-file", move |ctx: &ActionContext<'_>| {
            let s = status_state(ctx)?;
            let g = s.lock().ok()?;
            let StatusLine::File { path, staged } = classify_line(&g, ctx.cursor.line)? else {
                return None;
            };
            let scope = if staged { "staged" } else { "unstaged" };
            Some(Effect::OpenSyntheticBuffer {
                name: format!("*magit:diff:{scope}:{}*", path.display()),
                mode_id: "magit-diff-mode".to_string(),
            })
        });
    }

    // ── close (q) ─────────────────────────────────────
    // MG.13: removed from here. `action:magit-close` was registered by
    // BOTH this mode (`Effect::BufferDelete`) and `magit-core-mode`
    // (`Effect::DismissPopup`). Same action id ⇒ last registrant won,
    // decided by cascade ordering, so `q` in the status buffer was
    // nondeterministic between "delete the buffer" and "bury it".
    //
    // This is not a behaviour *choice* — `DismissPopup` is the already
    // documented and already tested intent. `magit-core-mode`'s handler
    // records the live-reported bug it fixed (`q` quitting the whole
    // editor), and
    // `lattice-ui-tui`'s `q_on_magit_status_buries_it_and_never_quits_the_editor`
    // asserts that `q` restores the buffer that was active before
    // magit-status opened. The registration here contradicted that
    // test; whenever it won the race the guarantee was simply not in
    // force. Removing it makes the tested behaviour deterministic.

    // ── MG.23h: jump to a section (the `s` row's submenu) ──
    //
    // Fired from the dispatch menu, which by then owns the keystrokes —
    // so the handler reads the buffer that was active when it opened
    // (`ActionContext::buffer_id`), the same seam every other menu row
    // resolves through.
    //
    // The section is found by scanning for its header text rather than
    // by consulting the `SectionIndex`: `]]` / `[[` already locate
    // sections that way, and two mechanisms for "where does this
    // section start" is one more than can stay in agreement. The
    // prefixes come from `sections::SECTION_HEADER_PREFIXES`, which is
    // also what renders them.
    for (action_name, prefix) in [
        ("action:magit-jump-staged", "Staged changes"),
        ("action:magit-jump-unstaged", "Unstaged changes"),
        ("action:magit-jump-untracked", "Untracked files"),
        ("action:magit-jump-stashes", "Stashes"),
        ("action:magit-jump-commits", "Recent commits"),
    ] {
        contributions.push(ActionHandlerContribution {
            action_name,
            handler: Arc::new(move |ctx: &ActionContext<'_>| Some(jump_to_section(ctx, prefix))),
        });
    }
}

/// MG.23h: move the cursor to the section whose header starts with
/// `prefix`, or say it isn't there.
///
/// A section with no entries is not rendered at all, so "jump to
/// Stashes" in a repo with no stashes has nothing to land on. Echoing
/// beats leaving the cursor where it was with no explanation — from
/// inside a menu, a row that appears to do nothing reads as broken.
fn jump_to_section(ctx: &ActionContext<'_>, prefix: &str) -> Effect {
    let found = ctx
        .services
        .get::<lattice_mode::BufferStoreHandle>()
        .and_then(|store| store.handle_for(lattice_core::BufferId(ctx.buffer_id.0 as u32)))
        .and_then(|handle| {
            let snap = handle.snapshot();
            (0..snap.buffer.line_count()).find(|l| {
                snap.buffer
                    .line(*l)
                    .is_some_and(|t| t.trim_start().starts_with(prefix))
            })
        });
    match found {
        Some(row) => Effect::CursorMove(lattice_protocol::position::Position::new(row, 0)),
        None => Effect::Echo {
            level: lattice_grammar::EchoLevel::Info,
            text: format!("magit: no {prefix} section here"),
        },
    }
}

/// The distinct files the buffer rows `rows` cover, plus the workdir.
///
/// `None` when the selection holds no file entry at all — a range over
/// section headers or commit rows, where staging means nothing. The
/// caller then falls through to the cursor's own entry, which declines
/// the same way it always did.
fn files_in_rows(
    s: &Arc<Mutex<StatusBufferState>>,
    rows: std::ops::RangeInclusive<u32>,
) -> Option<(Vec<PathBuf>, PathBuf)> {
    let g = s.lock().ok()?;
    let paths = distinct_files(rows.map(|line| classify_line(&g, line)));
    if paths.is_empty() {
        return None;
    }
    Some((paths, g.workdir.clone()))
}

/// The distinct file paths in a run of classified rows, in buffer
/// order.
///
/// Pure, because the decisions are here rather than in the lookup
/// around it. **Distinct** matters twice: a file entry and its expanded
/// inline diff are separate rows of the same file, so a selection
/// covering both must not stage it twice; and the same path can appear
/// in the staged *and* unstaged sections at once. **Buffer order**
/// matters because a batch reported in a different order than it is
/// shown is harder to check.
pub(crate) fn distinct_files(lines: impl Iterator<Item = Option<StatusLine>>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for line in lines.flatten() {
        if let StatusLine::File { path, .. } = line
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    }
    paths
}

/// Run `mutate` (a blocking git call) on `spawn_blocking`, off the
/// actor thread entirely, then refresh.
///
/// MG.13: lifted to module scope (was nested in
/// `register_action_handlers`) so [`StatusView`]'s `stage`/`unstage`
/// can reach it from the boot-registered path.
/// Run `op` over every item and fold the outcomes into one report.
///
/// A batch keeps going after a failure — stopping halfway would leave
/// the user to work out which half ran — but "keeps going" is not the
/// same as "says nothing". The previous code logged each error and
/// returned `()`, so a selection where four of five files staged looked
/// identical to one where all five did.
///
/// Success is `Ok("")` when everything worked, because
/// [`finish_task`](crate::magit_global_mode::finish_task) renders an
/// empty summary as a plain "<label> finished" — there is nothing to
/// add. A partial batch is an `Err` naming the count and the first
/// failure: it is the case worth interrupting for, and the count is
/// what tells the user to go and look.
fn batch_result<T>(
    items: impl Iterator<Item = T>,
    mut op: impl FnMut(T) -> lattice_vcs::Result<()>,
) -> Result<String, String> {
    let mut failed = 0usize;
    let mut total = 0usize;
    let mut first: Option<String> = None;
    for item in items {
        total += 1;
        if let Err(e) = op(item) {
            failed += 1;
            first.get_or_insert_with(|| e.to_string());
        }
    }
    match first {
        None => Ok(String::new()),
        Some(err) => Err(format!("{failed} of {total} failed — first: {err}")),
    }
}

/// Run a repository mutation off-thread, report it, then refresh.
///
/// **`mutate` returns a `Result` and that is not incidental.** It used
/// to be `impl FnOnce()`, so every caller wrote
/// `let _ = repo.run_git(...)` and threw the outcome away. Staging,
/// unstaging and discarding therefore finished in total silence — and
/// worse, a *failed* one did too: the buffer refreshed as though it had
/// worked, so the only symptom was a file that stayed where it was.
///
/// Making the closure return `Result<String, String>` moves that from a
/// discipline nobody kept to something the compiler asks for, and
/// [`finish_task`] then logs and publishes in one call. `label` names
/// the operation in the notification, so it is what the user reads —
/// "stage src/main.rs", not an argv.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<StatusBufferState>>,
    label: String,
    mutate: impl FnOnce() -> Result<String, String> + Send + 'static,
) -> Option<Effect> {
    let ctx = refresh_context(&s)?;
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(mutate)
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        crate::magit_global_mode::finish_task(&label, result);
        run_refresh(ctx).await;
    });
    None
}

/// Everything a refresh needs, read out of the state in one lock.
///
/// MG.18d: gathered into a struct because the list stopped fitting a
/// tuple once the refresh had to carry the open entries, the cursor
/// restore and the bus to answer on.
struct RefreshContext {
    handle: Arc<dyn lattice_runtime::Document>,
    wd: PathBuf,
    pending: Option<Arc<PendingSyntheticHighlights>>,
    bid: BufferId,
    headerline: Option<crate::headerline::MagitHeaderlineHandle>,
    /// Entry keys whose diffs must come back expanded.
    open: std::collections::HashSet<String>,
    restore: Option<crate::cursor_restore::HunkRestore>,
    cursor_bus: Option<crate::cursor_restore::CursorBusHandle>,
    /// MG.22b: `magit.hunk.context-lines`, snapshotted with the rest of
    /// the refresh inputs.
    context: i64,
    state: Arc<Mutex<StatusBufferState>>,
}

/// Snapshot the refresh inputs. Takes `pending_cursor` — a restore is
/// consumed by the refresh it was queued for, so a later `gr` does not
/// re-apply a stale jump.
/// `magit.hunk.context-lines`, or git's own default when there is no
/// config registry (a stripped harness).
pub(crate) fn context_lines(config: &Option<Arc<lattice_config::ConfigRegistry>>) -> i64 {
    config
        .as_ref()
        .and_then(|c| c.get_typed::<crate::options::MagitHunkContextLines>())
        .map(|v| *v)
        .unwrap_or(3)
}

fn refresh_context(s: &Arc<Mutex<StatusBufferState>>) -> Option<RefreshContext> {
    let mut g = s.lock().ok()?;
    let handle = g.store.handle_for(g.buffer_id)?;
    let restore = g.pending_cursor.take();
    Some(RefreshContext {
        handle,
        wd: g.workdir.clone(),
        pending: g.pending_highlights.clone(),
        bid: g.buffer_id,
        headerline: g.headerline.clone(),
        // MG.18d: the keys survive the rebuild — `build_and_format`
        // re-runs their diffs and inlines them, rather than the buffer
        // coming back collapsed and the map being cleared to match.
        open: g.expanded.keys().cloned().collect(),
        restore,
        cursor_bus: g.cursor_bus.clone(),
        context: context_lines(&g.config),
        state: Arc::clone(s),
    })
}

async fn run_refresh(ctx: RefreshContext) {
    do_refresh(
        ctx.handle,
        ctx.wd,
        ctx.pending,
        ctx.bid,
        ctx.headerline,
        ctx.open,
        ctx.restore,
        ctx.cursor_bus,
        ctx.context,
        ctx.state,
    )
    .await;
}

/// Refresh the status buffer: blocking `git status`/`stash
/// list`/`log` on `spawn_blocking`, then apply the formatted text +
/// highlights on the current task.
///
/// MG.13: lifted to module scope (was nested in
/// `register_action_handlers`) so [`trigger_refresh`] can reach it
/// from the boot-registered `gr` path.
///
/// MG.18d: `open` names the entries whose diffs must come back
/// expanded, and the rebuilt text carries them — a refresh no longer
/// throws away what you had open. `restore` (set only by a mutation)
/// then resolves the cursor against that same text, so the entry and
/// the position agree by construction rather than by two lookups
/// against a buffer in motion.
#[allow(clippy::too_many_arguments)]
async fn do_refresh(
    handle: Arc<dyn lattice_runtime::Document>,
    wd: PathBuf,
    pending: Option<Arc<PendingSyntheticHighlights>>,
    bid: BufferId,
    headerline: Option<crate::headerline::MagitHeaderlineHandle>,
    open: std::collections::HashSet<String>,
    restore: Option<crate::cursor_restore::HunkRestore>,
    cursor_bus: Option<crate::cursor_restore::CursorBusHandle>,
    context: i64,
    state: Arc<Mutex<StatusBufferState>>,
) {
    // MG.27: the row says "refreshing" for the whole of this function,
    // cleared by the guard's drop — including if the `spawn_blocking`
    // below panics or the task is cancelled when the buffer closes.
    let _busy = crate::headerline::busy(&headerline);
    let (text, spans, header, reopened) =
        tokio::task::spawn_blocking(move || refresh::build_and_format(&wd, &open, context))
            .await
            .expect("spawn_blocking");
    // MG.14: publish before the edit — the header describes the state
    // the body is about to show, and `set` is a comparison plus (at
    // most) one atomic, nowhere near the edit's cost.
    crate::headerline::publish(&headerline, header);
    // The expansion bookkeeping describes the text about to be written,
    // so it is replaced (not merged): an entry that vanished from the
    // status output has no rows to collapse later.
    if let Ok(mut g) = state.lock() {
        g.expanded = reopened;
    }
    // Resolved against the text rather than the buffer — the buffer is
    // about to become this text, and reading it back would race the
    // very edit being applied.
    let position = restore.and_then(|r| crate::cursor_restore::restore_position(&text, &r));
    refresh::apply_and_highlight(handle, text, spans, pending, bid).await;
    // Sent AFTER the replace lands: a cursor delivered first would be
    // clamped against the outgoing content. The send wakes the editor,
    // so the cursor arrives without the user touching a key
    // (`boot-composition.md` §3).
    if let Some(position) = position {
        crate::cursor_restore::send_cursor(&cursor_bus, bid, position);
    }
}

/// `gr` — bare refresh of the status buffer, no prior mutation.
///
/// MG.13: a free function (not a closure inside
/// `register_action_handlers`) because the `gr` handler is now
/// registered once at boot by `magit-core-mode` and reaches this
/// through [`StatusView`]; see `buffer_state::MagitView`.
pub fn trigger_refresh(s: Arc<Mutex<StatusBufferState>>) -> Option<Effect> {
    let ctx = refresh_context(&s)?;
    tokio::task::spawn(run_refresh(ctx));
    None::<Effect>
}

/// The status buffer's `MagitView` — supplies `gr`'s body for buffers
/// `magit-status-mode` owns.
pub struct StatusView(pub Arc<Mutex<StatusBufferState>>);

impl crate::buffer_state::MagitView for StatusView {
    /// MG.22: magit-status's `<CR>` — the reason `visit_at_cursor`
    /// exists on the trait at all.
    ///
    /// `magit-hunk-mode` owns the chord now, but here it must keep
    /// resolving rows that are not diff content: a staged file opens
    /// its index blob, an unstaged one the live file, a stash toggles
    /// its inline patch, a commit opens its buffer. Returning `None`
    /// for anything `classify_line` does not recognise is what lets
    /// the caller fall through to diff-path resolution — and what
    /// keeps it from resolving a row against a *previous* entry's
    /// expanded diff.
    fn visit_at_cursor(&self, cursor: lattice_protocol::position::Position) -> Option<Effect> {
        visit_status_line(&self.0, cursor.line)
    }

    /// MG.20: the commit on the Recent-commits row under the cursor.
    /// File and stash rows correctly yield `None`, so `V` on a staged
    /// file does nothing rather than reverting an unrelated commit.
    fn commit_at_cursor(&self, cursor: lattice_protocol::position::Position) -> Option<String> {
        let g = self.0.lock().ok()?;
        let handle = g.store.handle_for(g.buffer_id)?;
        let snap = handle.snapshot();
        let line = snap.buffer.line(cursor.line)?;
        // A Recent-commits row is `"  <sha> <subject>"`; every other
        // row kind (file entries carry a status label, stashes carry
        // `stash@{`) fails the hex test.
        let tok = line.split_whitespace().next()?;
        (tok.len() >= 4 && tok.chars().all(|c| c.is_ascii_hexdigit())).then(|| tok.to_string())
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        Some(self.0.lock().ok()?.workdir.clone())
    }

    /// MG.18c: the section an inline diff was expanded under says
    /// which tree it was diffed against — `run_show` passes
    /// `--cached` for a Staged entry and nothing for an Unstaged one,
    /// so the header the diff sits below is the same fact, already on
    /// screen.
    ///
    /// Stashes and commits expand patches too, and those belong to
    /// neither the index nor the worktree; `None` refuses hunk staging
    /// there rather than applying a commit's diff to the index.
    /// MG.50: `<CR>` inside an inline diff.
    ///
    /// This was the one view with no answer — `<CR>` in a magit-status
    /// hunk fell to the trait default and did nothing at all, while the
    /// same key in magit-diff or a revision opened the file.
    ///
    /// Which version to open is the SECTION's question, not the
    /// buffer's: a status buffer holds staged and unstaged diffs at
    /// once, and they describe different content. `diff_source` already
    /// answers it from the header above the cursor — the same seam
    /// `s` / `u` / `x` use to decide which tree a hunk applies to, so
    /// the version `<CR>` shows and the tree a hunk stages to can never
    /// disagree.
    fn diff_target(&self, path: &std::path::Path, cursor: Position) -> Option<Effect> {
        match self.diff_source(cursor)? {
            // Staged: the index blob, which is what the diff describes.
            // The working-tree file may have moved on since.
            DiffSource::Staged => Some(Effect::OpenSyntheticBuffer {
                name: crate::magit_file_revision_mode::blob_buffer_name("staged", path),
                mode_id: "magit-file-revision-mode".to_string(),
            }),
            // Unstaged (and untracked): the diff IS against the working
            // tree, so that file is the thing being described.
            DiffSource::Unstaged => {
                let full = self.0.lock().ok()?.workdir.join(path);
                full.exists().then_some(Effect::OpenBuffer {
                    path: Some(full),
                    force: false,
                })
            }
            // A commit's patch, expanded inline under its Recent-commits
            // row. THAT row names the revision, and it is the only place
            // the sha exists — the patch text below it does not repeat
            // it. So walk up to the entry this content was expanded
            // under, exactly as the fold source does to find an
            // expansion's extent.
            //
            // A stash's patch resolves to no sha and declines rather
            // than guessing a revision.
            DiffSource::Committed => {
                let g = self.0.lock().ok()?;
                let sha = (0..=cursor.line).rev().find_map(|l| {
                    match classify_line(&g, l) {
                        Some(StatusLine::Commit { sha }) => Some(sha),
                        // Any other classified entry means the walk left
                        // this patch without finding a commit.
                        Some(_) => None,
                        None => None,
                    }
                })?;
                Some(Effect::OpenSyntheticBuffer {
                    name: crate::magit_file_revision_mode::blob_buffer_name(&sha, path),
                    mode_id: "magit-file-revision-mode".to_string(),
                })
            }
        }
    }

    fn diff_source(&self, cursor: Position) -> Option<DiffSource> {
        let g = self.0.lock().ok()?;
        diff_source_for_header(&section_header_above(&g, cursor.line)?)
    }

    fn refresh(&self) -> Option<Effect> {
        trigger_refresh(self.0.clone())
    }

    /// MG.18d: queue the restore, then refresh. The refresh consumes it
    /// once it holds the rebuilt text — see [`refresh_context`].
    fn refresh_restoring(&self, site: crate::cursor_restore::HunkSite) -> Option<Effect> {
        if let Ok(mut g) = self.0.lock() {
            // A status buffer's landmark is the entry row; its section
            // is what `staged` selects between, since one path can be
            // listed under both.
            g.pending_cursor = Some(site.as_status_entry());
        }
        trigger_refresh(self.0.clone())
    }

    /// `s` — stage the file on the status entry line at `cursor`.
    fn stage(&self, cursor: Position) -> Option<Effect> {
        let s = self.0.clone();
        let (path, workdir) = {
            let g = s.lock().ok()?;
            let StatusLine::File { path, .. } = classify_line(&g, cursor.line)? else {
                return None;
            };
            (path, g.workdir.clone())
        };
        spawn_mutation_and_refresh(s, format!("stage {}", path.display()), move || {
            let repo =
                Repository::discover(&workdir).map_err(|e| format!("not a git repository: {e}"))?;
            Index::stage_path(&repo, &path)
                .map(|()| String::new())
                .map_err(|e| e.to_string())
        })
    }

    /// Every distinct file the selected rows cover, staged in ONE task
    /// with ONE refresh.
    ///
    /// Distinct because a file entry and its expanded inline diff are
    /// separate rows of the same file — a selection over both must not
    /// stage it twice — and because the same path can appear in both
    /// the staged and unstaged sections.
    fn stage_rows(&self, rows: std::ops::RangeInclusive<u32>) -> Option<Effect> {
        let s = self.0.clone();
        let (paths, workdir) = files_in_rows(&s, rows)?;
        spawn_mutation_and_refresh(s, format!("stage {} files", paths.len()), move || {
            let repo =
                Repository::discover(&workdir).map_err(|e| format!("not a git repository: {e}"))?;
            // One failure does not abandon the rest: a batch that
            // stopped halfway would leave the user to work out which
            // half. It is still REPORTED though — the batch's result is
            // "3 of 5 staged", not silence, because a partial batch is
            // exactly the outcome a user needs to know about.
            batch_result(paths.iter(), |path| Index::stage_path(&repo, path))
        })
    }

    fn unstage_rows(&self, rows: std::ops::RangeInclusive<u32>) -> Option<Effect> {
        let s = self.0.clone();
        let (paths, workdir) = files_in_rows(&s, rows)?;
        spawn_mutation_and_refresh(s, format!("unstage {} files", paths.len()), move || {
            let repo =
                Repository::discover(&workdir).map_err(|e| format!("not a git repository: {e}"))?;
            batch_result(paths.iter(), |path| Index::unstage_path(&repo, path))
        })
    }

    fn unstage(&self, cursor: Position) -> Option<Effect> {
        let s = self.0.clone();
        let (path, workdir) = {
            let g = s.lock().ok()?;
            let StatusLine::File { path, .. } = classify_line(&g, cursor.line)? else {
                return None;
            };
            (path, g.workdir.clone())
        };
        spawn_mutation_and_refresh(s, format!("unstage {}", path.display()), move || {
            let repo =
                Repository::discover(&workdir).map_err(|e| format!("not a git repository: {e}"))?;
            Index::unstage_path(&repo, &path)
                .map(|()| String::new())
                .map_err(|e| e.to_string())
        })
    }
}

#[cfg(test)]
mod diff_toggle_tests {
    use super::DiffToggle;

    /// **An entry that has rows folds; it never re-fetches.**
    ///
    /// This is the regression the slice exists to prevent. The old
    /// behaviour spliced the diff out of the buffer, so pressing `=`
    /// twice meant two `git diff` runs and threw away text the buffer
    /// already had. Any future change that maps a present expansion
    /// back to `Fetch` reintroduces exactly that.
    #[test]
    fn an_expanded_entry_folds_rather_than_refetching() {
        assert_eq!(DiffToggle::for_state(Some(12)), DiffToggle::Fold);
        assert_eq!(DiffToggle::for_state(Some(1)), DiffToggle::Fold);
    }

    /// Nothing fetched yet is the only state that runs git.
    #[test]
    fn only_an_unfetched_entry_runs_git() {
        assert_eq!(DiffToggle::for_state(None), DiffToggle::Fetch);
    }

    /// A zero-row expansion has nothing to hide, so folding it would
    /// be a no-op the user reads as a dead key. It is forgotten
    /// instead, which lets the next press fetch again.
    #[test]
    fn a_zero_row_expansion_is_dropped_not_folded() {
        assert_eq!(DiffToggle::for_state(Some(0)), DiffToggle::Drop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(s: &str) -> impl FnOnce() -> Option<String> + '_ {
        move || Some(s.to_string())
    }

    fn file(path: &str, staged: bool) -> Option<StatusLine> {
        Some(StatusLine::File {
            path: PathBuf::from(path),
            staged,
        })
    }

    /// A Visual selection over several entries stages all of them, in
    /// the order the buffer shows.
    #[test]
    fn a_selection_collects_every_file_it_covers_in_buffer_order() {
        let rows = [
            file("src/a.rs", false),
            file("src/b.rs", false),
            file("src/c.rs", false),
        ];
        assert_eq!(
            distinct_files(rows.into_iter()),
            vec![
                PathBuf::from("src/a.rs"),
                PathBuf::from("src/b.rs"),
                PathBuf::from("src/c.rs")
            ]
        );
    }

    /// A file entry and its expanded inline diff are separate rows of
    /// the SAME file. A selection over both must stage it once — twice
    /// is not harmless when the second call runs against a tree the
    /// first already changed.
    #[test]
    fn an_expanded_entry_is_not_staged_twice() {
        let rows = [
            file("src/a.rs", false),
            file("src/a.rs", false),
            file("src/b.rs", false),
        ];
        assert_eq!(
            distinct_files(rows.into_iter()),
            vec![PathBuf::from("src/a.rs"), PathBuf::from("src/b.rs")]
        );
    }

    /// The same path can sit in the staged AND unstaged sections at
    /// once — a partially-staged file. A selection spanning both is
    /// still one path.
    #[test]
    fn a_partially_staged_file_appearing_twice_is_still_one_path() {
        let rows = [file("src/a.rs", true), file("src/a.rs", false)];
        assert_eq!(
            distinct_files(rows.into_iter()),
            vec![PathBuf::from("src/a.rs")]
        );
    }

    /// Rows that are not files — section headers, commit rows, blanks —
    /// contribute nothing, so a selection over them declines and the
    /// caller falls back to the cursor's own entry.
    #[test]
    fn non_file_rows_contribute_nothing() {
        let rows = [
            None,
            Some(StatusLine::Commit {
                sha: "a1b2c3d".into(),
            }),
            Some(StatusLine::Stash { index: 0 }),
        ];
        assert!(distinct_files(rows.into_iter()).is_empty());
    }

    /// MG.18c — the header a hunk sits under decides which tree its
    /// patch applies to. Getting this backwards would send an
    /// unstaged hunk through `u` (git refuses, harmless) or a staged
    /// one through `x` (reverses it out of the worktree while leaving
    /// it staged — the half-state the gate exists to prevent).
    #[test]
    fn section_headers_map_to_the_tree_their_diffs_came_from() {
        assert_eq!(
            diff_source_for_header("Staged changes (2)"),
            Some(DiffSource::Staged)
        );
        assert_eq!(
            diff_source_for_header("Unstaged changes (3)"),
            Some(DiffSource::Unstaged)
        );
        assert_eq!(
            diff_source_for_header("Untracked files (1)"),
            Some(DiffSource::Unstaged),
            "`s` on an untracked file is `git add` — the worktree side"
        );
    }

    /// A commit's or stash's inline patch belongs to neither the index
    /// nor the worktree. `None` refuses hunk staging there rather than
    /// applying a commit's diff to the index.
    #[test]
    fn commit_and_stash_sections_have_no_stageable_source() {
        assert_eq!(diff_source_for_header("Recent commits"), None);
        assert_eq!(diff_source_for_header("Stashes (2)"), None);
    }

    fn no_header() -> impl FnOnce() -> Option<String> {
        || None
    }

    // ── audit fix: collapse deleted the following entry's text ──
    // ── audit fix: the "new file" (two-word) label bug ──────────

    #[test]
    fn staged_new_file_entry_classifies_with_full_path() {
        // Root cause of the u / =-on-staged bugs: the old
        // `parse_file_path` split on the first space and got the
        // "file" half of the "new file" label instead of the path.
        let line = format!("  {:<12} {}", "new file", "src/lib.rs");
        let sl = classify_line_text(&line, header("Staged changes (1)"));
        assert_eq!(
            sl,
            Some(StatusLine::File {
                path: PathBuf::from("src/lib.rs"),
                staged: true,
            })
        );
    }

    #[test]
    fn unstaged_modified_entry_classifies_as_not_staged() {
        let line = format!("  {:<12} {}", "modified", "src/main.rs");
        let sl = classify_line_text(&line, header("Unstaged changes (1)"));
        assert_eq!(
            sl,
            Some(StatusLine::File {
                path: PathBuf::from("src/main.rs"),
                staged: false,
            })
        );
    }

    #[test]
    fn untracked_file_entry_classifies_as_not_staged() {
        let line = format!("  {:<12} {}", "untracked", "notes.txt");
        let sl = classify_line_text(&line, header("Untracked files (1)"));
        assert_eq!(
            sl,
            Some(StatusLine::File {
                path: PathBuf::from("notes.txt"),
                staged: false,
            })
        );
    }

    #[test]
    fn deleted_entry_classifies_correctly() {
        let line = format!("  {:<12} {}", "deleted", "old.rs");
        let sl = classify_line_text(&line, header("Unstaged changes (1)"));
        assert_eq!(
            sl,
            Some(StatusLine::File {
                path: PathBuf::from("old.rs"),
                staged: false,
            })
        );
    }

    // ── stash / commit entries — <CR> previously no-op'd on both ──

    #[test]
    fn stash_entry_classifies_by_index() {
        let sl = classify_line_text("  stash@{2} WIP on main: 1234abc msg", no_header());
        assert_eq!(sl, Some(StatusLine::Stash { index: 2 }));
    }

    #[test]
    fn commit_entry_classifies_sha_under_recent_commits_header() {
        let sl = classify_line_text("  a1b2c3d Fix the thing", header("Recent commits (20)"));
        assert_eq!(
            sl,
            Some(StatusLine::Commit {
                sha: "a1b2c3d".to_string(),
            })
        );
    }

    #[test]
    fn commit_like_line_outside_recent_commits_header_is_not_a_commit() {
        // Guards against misclassifying arbitrary indented text as a
        // commit entry when it isn't actually under that section.
        let sl = classify_line_text("  a1b2c3d Fix the thing", header("Stashes (1)"));
        assert_eq!(sl, None);
    }

    // ── non-entry lines ──────────────────────────────────────────

    #[test]
    fn section_header_line_is_not_an_entry() {
        assert_eq!(classify_line_text("Staged changes (2)", no_header()), None);
    }

    #[test]
    fn blank_line_is_not_an_entry() {
        assert_eq!(classify_line_text("", no_header()), None);
    }

    #[test]
    fn no_changes_message_is_not_an_entry() {
        assert_eq!(
            classify_line_text("No changes (working tree clean)", no_header()),
            None
        );
    }

    // ── entry_key: Conflicted-file staged/unstaged collision fix ──

    #[test]
    fn entry_key_distinguishes_staged_and_unstaged_rows_for_the_same_path() {
        // A Conflicted file appears in BOTH sections at once
        // (refresh::build_section_index); the two rows must map to
        // distinct expansion-tracking keys or expanding one would
        // make `toggle_expand` treat the other as already-expanded.
        let staged = StatusLine::File {
            path: PathBuf::from("conflict.rs"),
            staged: true,
        };
        let unstaged = StatusLine::File {
            path: PathBuf::from("conflict.rs"),
            staged: false,
        };
        assert_ne!(entry_key(&staged), entry_key(&unstaged));
    }

    #[test]
    fn entry_key_stable_for_same_status_line() {
        let a = StatusLine::Stash { index: 3 };
        let b = StatusLine::Stash { index: 3 };
        assert_eq!(entry_key(&a), entry_key(&b));
    }
}

/// MG.31: the inline `=` expansion's git call belongs on the blocking
/// pool, not the actor thread.
#[cfg(test)]
mod expand_payload_tests {
    use super::*;
    use lattice_cells::style::Style;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn git_ok(dir: &Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    /// A repo with one tracked file whose working tree differs from
    /// HEAD in `changed` lines. `changed` drives how long `git diff`
    /// takes, which is what the responsiveness probe below needs.
    fn repo_with_modified_file(lines: usize) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        let base: String = (1..=lines).map(|i| format!("line {i}\n")).collect();
        std::fs::write(p.join("a.txt"), &base).expect("write base");
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);
        // Every line differs, so the diff is proportional to `lines`.
        let modified: String = (1..=lines).map(|i| format!("line {i} CHANGED\n")).collect();
        std::fs::write(p.join("a.txt"), &modified).expect("write modified");
        dir
    }

    fn unstaged(path: &str) -> StatusLine {
        StatusLine::File {
            path: PathBuf::from(path),
            staged: false,
        }
    }

    /// The relocation must not change what the caller receives: the
    /// trimmed diff text, its line count, and one span row per line.
    #[test]
    fn returns_the_diff_its_line_count_and_a_span_row_per_line() {
        let dir = repo_with_modified_file(20);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (text, line_count, spans) = rt
            .block_on(expand_payload(
                dir.path().to_path_buf(),
                unstaged("a.txt"),
                3,
            ))
            .expect("a modified tracked file has a diff");

        assert!(text.starts_with("diff --git"), "got: {text:?}");
        assert_eq!(line_count, text.lines().count());
        assert_eq!(
            spans.len(),
            line_count,
            "one span row per line, or the highlight splice misaligns"
        );
        assert!(
            spans
                .iter()
                .any(|row| row.iter().any(|s| s.style == Style::DiffAdd)),
            "a changed file's diff must carry added lines"
        );
    }

    /// MG.46: **the patch must be inlined verbatim**, because a hunk's
    /// `@@` header declares how many body lines it has and the fold
    /// source bounds the hunk by that count.
    ///
    /// A blank trailing context line is a single space, and `.trim()`
    /// on the whole patch removed it — leaving the text one line
    /// shorter than its own header claimed. The hunk fold then ran past
    /// the end of the diff into the status rows below it, which is the
    /// same symptom `hunk_fold_source` was fixed for and the reason
    /// only trailing newlines may be stripped.
    #[test]
    fn a_trailing_blank_context_line_survives_into_the_expansion() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        // The file ends with a blank line, so the diff's last context
        // line is a lone space.
        std::fs::write(p.join("a.txt"), "one\ntwo\n\n").expect("write base");
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);
        std::fs::write(p.join("a.txt"), "one\ntwo CHANGED\n\n").expect("write modified");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let (text, line_count, spans) = rt
            .block_on(expand_payload(p.to_path_buf(), unstaged("a.txt"), 3))
            .expect("a modified tracked file has a diff");

        let body: Vec<&str> = text.lines().collect();
        let at = body
            .iter()
            .position(|l| l.starts_with("@@"))
            .expect("the patch has a hunk header");
        // Every row after the header is hunk body, including the blank
        // context line git emits as a lone space.
        let declared = body[at]
            .split_whitespace()
            .find_map(|t| {
                t.strip_prefix('+')?
                    .split_once(',')
                    .map(|(_, c)| c.to_string())
            })
            .and_then(|c| c.parse::<usize>().ok())
            .expect("the header declares a new-side count");
        let present = body[at + 1..]
            .iter()
            .filter(|l| l.is_empty() || l.starts_with([' ', '+']))
            .count();
        assert_eq!(
            present, declared,
            "the inlined body must supply every line its header declares; \
             got {body:?}",
        );
        assert_eq!(line_count, text.lines().count());
        assert_eq!(spans.len(), line_count, "one span row per line");
    }

    /// An entry with nothing to show declines rather than inserting a
    /// blank expansion — the behaviour the old `!diff.trim().is_empty()`
    /// guard had.
    #[test]
    fn declines_when_there_is_no_diff() {
        let dir = repo_with_modified_file(5);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        // `b.txt` is not in the repo at all, so `git diff -- b.txt` is
        // empty.
        let out = rt.block_on(expand_payload(
            dir.path().to_path_buf(),
            unstaged("b.txt"),
            3,
        ));
        // MG.56: not merely "nothing was inserted" — WHY. This used to
        // assert only the silence, which is exactly the behaviour that
        // made `=` look like an unbound key: a file whose changes had
        // been committed elsewhere produced an empty patch and no word
        // about it. The distinction between "git had nothing to show"
        // and "git failed" is what lets the caller say something
        // useful, so the test pins it rather than the emptiness.
        assert!(
            matches!(out, Err(ExpandMiss::NoChanges)),
            "an empty diff must report NoChanges, not a bare failure — \
             the row is stale, and `gr` is the fix worth naming"
        );
    }

    /// **The MG.31 regression guard.** Mirrors
    /// `lattice-multibuffer/tests/ui_responsive_during_scan.rs`: run on
    /// a `current_thread` runtime (the editor actor's configuration,
    /// `editor_actor.rs:562`) and assert a concurrent probe keeps its
    /// sleep budget while the expansion runs.
    ///
    /// **Verified non-vacuous**, not assumed: dropping the
    /// `spawn_blocking` from `expand_payload` (the pre-MG.31 shape) puts
    /// the git call and the styling on this runtime and the measured gap
    /// goes to **263 ms** against the 50 ms threshold — a 5× margin, so
    /// neither CI jitter nor a fast machine can flip the verdict. The
    /// file is sized (200k lines, every one changed) to buy exactly that
    /// margin; at 40k it was only 74 ms, which was too close to call.
    #[test]
    fn the_expansion_does_not_starve_the_actor_runtime() {
        let dir = repo_with_modified_file(200_000);
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let (max_gap, ticks, produced) = rt.block_on(async {
            let task = tokio::task::spawn(expand_payload(
                dir.path().to_path_buf(),
                unstaged("a.txt"),
                3,
            ));

            let mut max_gap = Duration::ZERO;
            let mut ticks = 0usize;
            let mut last = Instant::now();
            for _ in 0..50 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                let now = Instant::now();
                max_gap = max_gap.max(now.duration_since(last));
                ticks += 1;
                last = now;
            }
            let produced = task.await.expect("join").is_ok();
            (max_gap, ticks, produced)
        });

        assert_eq!(ticks, 50, "all probe iterations ran");
        assert!(produced, "the expansion still produced its diff");
        assert!(
            max_gap < Duration::from_millis(50),
            "max probe gap was {max_gap:?}; expected < 50 ms — the actor's \
             current_thread runtime is being starved by the `=` expansion \
             (paramount-goal-1 regression, MG.31). The git call and the \
             styling belong inside `spawn_blocking`."
        );
    }
}
