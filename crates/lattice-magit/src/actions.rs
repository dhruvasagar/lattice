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
use lattice_protocol::position::{Position, Range};
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
        if let Some(rest) = trimmed.strip_prefix(label) {
            if rest.starts_with(char::is_whitespace) {
                let path = PathBuf::from(rest.trim_start());
                let staged = header_above()
                    .map(|h| h.starts_with("Staged"))
                    .unwrap_or(false);
                return Some(StatusLine::File { path, staged });
            }
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
pub(crate) fn run_show(workdir: &Path, sl: &StatusLine) -> Option<String> {
    let mut cmd = std::process::Command::new("git");
    cmd.current_dir(workdir);
    match sl {
        StatusLine::File { path, staged } => {
            cmd.arg("diff");
            if *staged {
                cmd.arg("--cached");
            }
            cmd.arg("--").arg(path);
        }
        StatusLine::Stash { index } => {
            cmd.args(["stash", "show", "-p", &format!("stash@{{{index}}}")]);
        }
        StatusLine::Commit { sha } => {
            cmd.args(["show", sha]);
        }
    }
    let output = cmd.output().ok()?;
    if output.status.success() {
        String::from_utf8(output.stdout).ok()
    } else {
        None
    }
}

/// The delete range that collapses an inline expansion inserted at
/// `cursor_line + 1` occupying exactly `count` rows.
///
/// The end position must land at COLUMN 0 of the row following the
/// diff, not at the end of that row's own text — the diff occupies
/// exactly `count` rows (`cursor_line+1 ..= cursor_line+count`);
/// anything at or past `cursor_line+1+count` belongs to the next
/// real entry and must survive the collapse untouched. Anchoring the
/// end column on that row's text length instead (the bug this
/// replaces) deletes the next entry's content too, leaving a blank
/// line in its place and shifting every subsequent row — which
/// desyncs previously-applied syntax-highlight spans from their
/// rows, producing exactly the "corrupts subsequent files'
/// highlighting" symptom this was reported as.
///
/// `total_lines`/`last_line_len` describe the buffer's current
/// shape: when the diff is the last content in the buffer (no
/// following row to anchor on), the range instead ends at the end of
/// the diff's own last line.
fn collapse_range(
    cursor_line: u32,
    count: u32,
    total_lines: u32,
    last_line_len: u32,
) -> (Position, Position) {
    let start = Position::new(cursor_line + 1, 0);
    let target_end_line = cursor_line + 1 + count;
    let end = if target_end_line < total_lines {
        Position::new(target_end_line, 0)
    } else {
        let last = total_lines.saturating_sub(1);
        Position::new(last, last_line_len)
    };
    (start, end)
}

/// Toggle the inline expansion of `sl` at `cursor_line`: collapse it
/// if already expanded (removing exactly the number of lines recorded
/// in `StatusBufferState::expanded` — not a re-scanned guess), or
/// insert its `git show`/`git diff` output and record the inserted
/// line count if collapsed. Shared by `=` (files) and `<CR>`
/// (stashes/commits).
fn toggle_expand(
    s: &Arc<Mutex<StatusBufferState>>,
    sl: StatusLine,
    cursor_line: u32,
) -> Option<Effect> {
    let key = entry_key(&sl);
    let (handle, wd, rt, existing_count, pending, bid) = {
        let g = s.lock().ok()?;
        let h = g.store.handle_for(g.buffer_id)?;
        (
            h,
            g.workdir.clone(),
            g.runtime.clone(),
            g.expanded.get(&key).copied(),
            g.pending_highlights.clone(),
            g.buffer_id,
        )
    };

    if let Some(count) = existing_count {
        if count > 0 {
            let snap = handle.snapshot();
            let total = snap.buffer.line_count() as u32;
            let last_line_len = snap
                .buffer
                .line(total.saturating_sub(1))
                .map(|t| t.len() as u32)
                .unwrap_or(0);
            let (start, end) = collapse_range(cursor_line, count as u32, total, last_line_len);
            let start_line = cursor_line + 1;
            let s = s.clone();
            rt.spawn(async move {
                let _ = handle
                    .apply_edit_batch(vec![Edit::replace(Range::new(start, end), String::new())])
                    .await;
                // Only now that the collapse has actually landed is it
                // safe to forget this entry's expansion — clearing it
                // earlier let a rapid second toggle race ahead of the
                // edit and compute a delete range against rows that
                // didn't contain the diff yet (see the insert-branch
                // comment below for the mirrored insert-side hazard).
                if let Ok(mut g) = s.lock() {
                    g.expanded.remove(&key);
                }
                if let Some(ref ph) = pending {
                    // Mirror the insert branch's `insert_at_and_wake`:
                    // the diff's `count` highlight-span rows must be
                    // spliced OUT (not just left in place) or every
                    // line after them stays shifted-and-mispainted
                    // forever, surviving even a full collapse.
                    ph.remove_at_and_wake(bid, start_line, count);
                }
            });
        } else if let Ok(mut g) = s.lock() {
            g.expanded.remove(&key);
        }
    } else {
        let diff = run_show(&wd, &sl).unwrap_or_default();
        if !diff.trim().is_empty() {
            let text = diff.trim().to_string();
            let line_count = text.lines().count();
            let spans = crate::highlight::diff_styled_spans(&text);
            let pos = Position::new(cursor_line + 1, 0);
            let start_line = cursor_line + 1;
            let s = s.clone();
            rt.spawn(async move {
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

// ── registration ────────────────────────────────────────

/// MG.13: service alias for magit-status's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type StatusStatesHandle = Arc<crate::buffer_state::BufferStates<StatusBufferState>>;

/// Resolve the status buffer's state for the buffer an action fired
/// in. `None` means this is not a live magit-status buffer, so the
/// handler declines — the same outcome as before, minus the race.
fn status_state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<StatusBufferState>>> {
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
                    return spawn_mutation_and_refresh(s.clone(), move || {
                        if let Ok(repo) = Repository::discover(&workdir) {
                            let _ = repo.run_git(["checkout", "--", &path]);
                        }
                    });
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
                        spawn_mutation_and_refresh(s.clone(), move || {
                            if let Ok(repo) = Repository::discover(&workdir) {
                                let _ = repo.run_git(["checkout", "--", &path.to_string_lossy()]);
                            }
                        })
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
            let sl = {
                let g = s.lock().ok()?;
                classify_line(&g, ctx.cursor.line)?
            };
            match sl {
                StatusLine::File { path, staged: true } => Some(Effect::OpenSyntheticBuffer {
                    name: format!("*magit:file:staged:{}*", path.display()),
                    mode_id: "magit-file-revision-mode".to_string(),
                }),
                StatusLine::File {
                    path,
                    staged: false,
                } => {
                    let g = s.lock().ok()?;
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
                StatusLine::Stash { .. } => toggle_expand(&s, sl, ctx.cursor.line),
                // Bug fix: `<CR>` on a commit SHA used to toggle the
                // inline diff (same as `=`) — but every other magit
                // view that shows a SHA (log, blame, rebase) treats
                // `<CR>` as "open the dedicated commit buffer", so
                // status was the one inconsistent surface. `=` still
                // does the inline toggle for a quick look without
                // leaving the status buffer; `<CR>` now matches log/
                // blame/rebase's convention.
                StatusLine::Commit { sha } => Some(Effect::OpenSyntheticBuffer {
                    name: format!("*magit:commit:{sha}*"),
                    mode_id: "magit-revision-mode".to_string(),
                }),
            }
        });
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
        handler!("action:magit-toggle-diff", move |ctx: &ActionContext<
            '_,
        >| {
            let s = status_state(ctx)?;
            let sl = {
                let g = s.lock().ok()?;
                classify_line(&g, ctx.cursor.line)?
            };
            if !matches!(sl, StatusLine::File { .. }) {
                return None;
            }
            toggle_expand(&s, sl, ctx.cursor.line)
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

    contributions
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
            (0..snap.buffer.line_count() as u32).find(|l| {
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

/// Run `mutate` (a blocking git call) on `spawn_blocking`, off the
/// actor thread entirely, then refresh.
///
/// MG.13: lifted to module scope (was nested in
/// `register_action_handlers`) so [`StatusView`]'s `stage`/`unstage`
/// can reach it from the boot-registered path.
fn spawn_mutation_and_refresh(
    s: Arc<Mutex<StatusBufferState>>,
    mutate: impl FnOnce() + Send + 'static,
) -> Option<Effect> {
    let ctx = refresh_context(&s)?;
    tokio::task::spawn(async move {
        let _ = tokio::task::spawn_blocking(mutate).await;
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
    state: Arc<Mutex<StatusBufferState>>,
}

/// Snapshot the refresh inputs. Takes `pending_cursor` — a restore is
/// consumed by the refresh it was queued for, so a later `gr` does not
/// re-apply a stale jump.
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
    state: Arc<Mutex<StatusBufferState>>,
) {
    let (text, spans, header, reopened) =
        tokio::task::spawn_blocking(move || refresh::build_and_format(&wd, &open))
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
        let tok = line.trim().split_whitespace().next()?;
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
            let StatusLine::File { path, .. } = classify_line(&g, cursor.line)? else {
                return None;
            };
            (path, g.workdir.clone())
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

    fn header(s: &str) -> impl FnOnce() -> Option<String> + '_ {
        move || Some(s.to_string())
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

    #[test]
    fn collapse_range_ends_at_next_row_column_zero_not_its_text_length() {
        // File header at line 5; a 3-line diff was inserted at 6..=8;
        // line 9 is the next real entry ("  modified  other.rs", 21
        // bytes). Collapsing must end at (9, 0), not (9, 21) — ending
        // at the row's text length is exactly the bug that deleted
        // the next entry's content and left a blank line behind.
        let (start, end) = collapse_range(5, 3, 20, 21);
        assert_eq!(start, Position::new(6, 0));
        assert_eq!(
            end,
            Position::new(9, 0),
            "must not consume the next row's text"
        );
    }

    #[test]
    fn collapse_range_at_buffer_end_falls_back_to_the_diffs_own_last_line() {
        // Diff inserted at 6..=8 with nothing after it (total_lines
        // == 9, so target_end_line == 9 == total_lines, out of
        // range) — there's no following row to anchor on, so the end
        // must land at the end of the diff's own last line (8, 12).
        let (start, end) = collapse_range(5, 3, 9, 12);
        assert_eq!(start, Position::new(6, 0));
        assert_eq!(end, Position::new(8, 12));
    }

    #[test]
    fn collapse_range_single_line_diff() {
        let (start, end) = collapse_range(0, 1, 10, 0);
        assert_eq!(start, Position::new(1, 0));
        assert_eq!(end, Position::new(2, 0));
    }

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
