//! MG.9: magit-rebase major mode.
//!
//! Editable interactive rebase todo buffer. C-c C-c runs rebase,
//! C-c C-k aborts.
//!
//! Fold audit fix: this used to populate the buffer with a
//! hardcoded fake todo and, on `C-c C-c`, write it straight to
//! `.git/rebase-merge/git-rebase-todo` and run `git rebase
//! --continue` — against a rebase that had never actually been
//! started, which always failed silently. The real flow: build the
//! todo from `git log` against a real upstream, and on `C-c C-c`
//! actually START the interactive rebase, injecting the buffer's
//! (possibly user-edited) todo via the standard
//! `GIT_SEQUENCE_EDITOR` trick — `git rebase -i` invokes the
//! sequence editor as `<editor> <path-to-generated-todo>`, so
//! setting it to `cp <our-file>` replaces git's todo with ours in
//! one step. `GIT_EDITOR=true` avoids hanging on a `reword` step's
//! commit-message prompt by accepting the original message unchanged.
//!
//! MG.43c lifted that limitation for the rebase `w` row, using the
//! same trick one level down: the message is collected in a compose
//! buffer FIRST, then `GIT_EDITOR` is pointed at `cp <message-file>`
//! so git's reword step writes it. `GIT_EDITOR=true` remains the
//! default for `edit` and `drop`, neither of which opens an editor.
//!
//! The todo buffer itself still keeps the original message on a
//! hand-typed `reword` line — it has no message-editing UI. That
//! remains a known limitation rather than a silent failure.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use lattice_protocol::position::Position;

use lattice_config;
use lattice_grammar::{Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_vcs::Repository;

use crate::buffer_state::{BufferStateGuard, BufferStates};
use crate::headerline;

pub struct MagitRebaseMode;

impl MagitRebaseMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-rebase-mode")
    }
}

fn magit_rebase_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<C-c><C-c>", doc: "Execute rebase", cmd: "action:magit-rebase-confirm" },
            keymap_entry! { mode: Insert, chord: "<C-c><C-k>", doc: "Abort rebase", cmd: "action:magit-rebase-abort" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-c>", doc: "Execute rebase", cmd: "action:magit-rebase-confirm" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-k>", doc: "Abort rebase", cmd: "action:magit-rebase-abort" },
            keymap_entry! { mode: Normal, chord: "<CR>", doc: "Show commit detail at cursor", cmd: "action:magit-rebase-show-commit" },
        ]
    })
}

pub struct RebaseState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    upstream: String,
    /// Resolved once at activation so the abort handler can decide
    /// *synchronously* whether a rebase is in progress without walking
    /// the filesystem to find the repo first (MG.12 — the confirm has
    /// to be part of the effect the chord returns, so the check cannot
    /// be deferred to `spawn_blocking` the way the abort itself is).
    gitdir: std::path::PathBuf,
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type RebaseStatesHandle = Arc<BufferStates<RebaseState>>;

/// MG.24c: this buffer's [`MagitView`], so `A` / `_` / `O` act on the
/// commit under the cursor.
///
/// `magit-core-mode.md` has claimed since MG.20 that those chords work
/// on "the rebase todo". They never have: they resolve through
/// `MagitView::commit_at_cursor`, and this mode published no view at
/// all, so the trait default returned `None` and every press was a
/// consumed dead key. The data was always here — `<CR>` reads the same
/// sha off the same line with the same parser.
struct RebaseView(Arc<Mutex<RebaseState>>);

impl crate::buffer_state::MagitView for RebaseView {
    /// **Deliberately nothing.** A rebase todo is a file the user is
    /// part-way through editing, and `gr` means "rebuild this view from
    /// git" everywhere else — here that would re-read the todo from
    /// disk and silently discard the reordering they were in the middle
    /// of. There is no refresh that is safe to offer.
    fn refresh(&self) -> Option<Effect> {
        None
    }

    fn commit_at_cursor(&self, cursor: Position) -> Option<String> {
        let g = self.0.lock().ok()?;
        let handle = g.store.handle_for(g.buffer_id)?;
        let snap = handle.snapshot();
        let line = snap.buffer.line(cursor.line)?;
        extract_sha(&line).map(str::to_string)
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        Some(self.0.lock().ok()?.workdir.clone())
    }
}

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<RebaseState>>> {
    crate::buffer_state::state_for::<RebaseState>(ctx)
}

impl Mode for MagitRebaseMode {
    type Guard = BufferStateGuard<RebaseState>;

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
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_rebase_keymap_entries())
    }

    /// MG.13: boot-registered — see `buffer_state`'s module docs.
    ///
    /// `upstream` is the field this mode cannot resolve before its
    /// `.await`. It is published empty, and `confirm` already refuses
    /// to run against an empty upstream — so a `C-c C-c` in that window
    /// correctly does nothing rather than rebasing onto an unresolved
    /// ref.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // confirm (C-c C-c)
            ActionHandlerContribution {
                action_name: "action:magit-rebase-confirm",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (todo, workdir, upstream) = {
                        let g = s.lock().ok()?;
                        if g.upstream.is_empty() {
                            return None;
                        }
                        let handle = g.store.handle_for(g.buffer_id)?;
                        let snap = handle.snapshot();
                        let mut todo = String::new();
                        for l in 0..snap.buffer.line_count() as u32 {
                            let text = snap.buffer.line(l).unwrap_or_default();
                            if text.starts_with('#') || text.trim().is_empty() {
                                continue;
                            }
                            todo.push_str(&text);
                            todo.push('\n');
                        }
                        (todo, g.workdir.clone(), g.upstream.clone())
                    };
                    if todo.trim().is_empty() {
                        return None;
                    }
                    // Bounded, single-shot git invocation, off the actor
                    // thread — same optimistic-close shape as
                    // magit-commit's confirm.
                    tokio::task::spawn(tokio::task::spawn_blocking(move || {
                        if let Err(e) = run_rebase(&workdir, &upstream, &todo) {
                            tracing::error!(target: "lattice_magit", "rebase failed: {e}");
                        }
                    }));
                    Some(Effect::QuitEditor {
                        force: false,
                        scope: QuitScope::Pane,
                    })
                }),
            },
            // abort (C-c C-k) — MG.12. No rebase has necessarily
            // started yet (that only happens on confirm), and the two
            // cases deserve different answers:
            //
            //   nothing in progress → `C-c C-k` just closes a todo
            //     buffer nobody ran. Asking there would be pure noise,
            //     so it closes the pane outright.
            //   rebase in progress  → `--abort` throws away everything
            //     the rebase has replayed so far, which is the same
            //     class of act as discard / branch-delete, so it asks.
            //
            // The in-progress check is a single `stat` against the
            // gitdir resolved at activation — cheap enough to run on
            // the actor thread in response to an explicit chord, and it
            // *has* to run here because the confirm is the effect this
            // handler returns.
            ActionHandlerContribution {
                action_name: "action:magit-rebase-abort",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let gitdir = { s.lock().ok()?.gitdir.clone() };
                    if rebase_in_progress(&gitdir) {
                        Some(abort_rebase_confirm())
                    } else {
                        Some(Effect::QuitEditor {
                            force: false,
                            scope: QuitScope::Pane,
                        })
                    }
                }),
            },
            // abort, after confirmation.
            ActionHandlerContribution {
                action_name: "action:magit-rebase-abort-execute",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let workdir = { s.lock().ok()?.workdir.clone() };
                    tokio::task::spawn(tokio::task::spawn_blocking(move || {
                        if let Ok(repo) = Repository::discover(&workdir)
                            && rebase_in_progress(repo.gitdir())
                        {
                            let _ = repo.run_git(["rebase", "--abort"]);
                        }
                    }));
                    Some(Effect::QuitEditor {
                        force: false,
                        scope: QuitScope::Pane,
                    })
                }),
            },
            // <CR> — show commit detail for the todo line at cursor,
            // matching magit-log/magit-blame's convention.
            ActionHandlerContribution {
                action_name: "action:magit-rebase-show-commit",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let g = s.lock().ok()?;
                    let handle = g.store.handle_for(g.buffer_id)?;
                    let snap = handle.snapshot();
                    let line = snap.buffer.line(ctx.cursor.line)?;
                    let sha = extract_sha(&line)?;
                    Some(Effect::OpenSyntheticBuffer {
                        name: format!("*magit:commit:{sha}*"),
                        mode_id: "magit-revision-mode".to_string(),
                    })
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

            let discovered = Repository::discover(".").ok();
            let workdir = discovered
                .as_ref()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();
            let gitdir = discovered
                .as_ref()
                .map(|r| r.gitdir().to_path_buf())
                .unwrap_or_default();

            // Which rebase the buffer name asks for — see
            // [`RebaseTarget`]. Mirrors magit-blame's
            // target-in-buffer-name pattern; an unrecognised name falls
            // back to `@{upstream}`, which is what a bare
            // `*magit:rebase*` has always meant.
            let target = store
                .name_for(buffer_id)
                .as_deref()
                .and_then(parse_target)
                .unwrap_or(RebaseTarget::Onto(None));

            // MG.14: the upstream is resolved below (it may come from
            // `@{upstream}` rather than the buffer name), so the header
            // fills in with the todo text.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            let rebase_running = rebase_in_progress(&gitdir);

            // MG.13: publish BEFORE the first `.await`. `upstream` is
            // not resolvable yet; it starts empty, and `confirm`
            // already refuses on an empty upstream.
            let Some(states) = ctx.service::<RebaseStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                RebaseState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    upstream: String::new(),
                    gitdir,
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            // MG.24c: publish the view, or `A` / `_` / `O` resolve no
            // commit here and stay the dead keys they have been.
            if let Some(views) = ctx.service::<crate::buffer_state::MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(RebaseView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            let wd = workdir.clone();
            let (upstream, initial) =
                tokio::task::spawn_blocking(move || build_rebase_buffer(&wd, &target))
                    .await
                    .unwrap_or_else(|_| (String::new(), "Failed to prepare rebase.\n".to_string()));

            // Counted from the text just built, so no second
            // `rev-list`. Keyed on the leading verb rather than "has a
            // hex-looking token": the explanatory `#` footer is prose,
            // and an ordinary English word made only of `abcdef`
            // ("added", "faced") would otherwise count as a commit.
            let commits = initial.lines().filter(|l| is_todo_line(l)).count();
            headerline::publish(
                &hl,
                headerline::rebase_fields(&upstream, commits, rebase_running),
            );
            let spans = crate::highlight::rebase_styled_spans(&initial);
            crate::buffer_io::replace_buffer_text(&handle, initial).await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.store_and_wake(buffer_id, spans);
            }

            // Late-resolved field, now that the upstream is known.
            if let Ok(mut g) = state.lock() {
                g.upstream = upstream;
            }

            Ok(guard)
        })
    }
}

/// Is a rebase actually mid-flight? `git` records one as a
/// `rebase-merge` directory in the gitdir (`rebase-apply` for the
/// legacy `--apply` backend and for `git am`). Both are checked
/// because either means `--abort` has work to throw away.
fn rebase_in_progress(gitdir: &Path) -> bool {
    gitdir.join("rebase-merge").exists() || gitdir.join("rebase-apply").exists()
}

/// MG.12: the ask half of `C-c C-k`, reached only when a rebase is
/// genuinely in progress.
fn abort_rebase_confirm() -> Effect {
    crate::confirm::ask(
        "Abort this rebase?".to_string(),
        "action:magit-rebase-abort-execute",
    )
}

/// The verbs a rebase-todo line may lead with. Shared by the commit
/// counter below and mirrored by `highlight::rebase_styled_spans`,
/// which colours the same set.
const TODO_VERBS: [&str; 6] = ["pick", "reword", "edit", "squash", "fixup", "drop"];

/// MG.14: is this todo line a real commit row? `<verb> <sha> ...` —
/// not a `#` comment and not the trailing blank.
fn is_todo_line(line: &str) -> bool {
    TODO_VERBS
        .iter()
        .any(|v| line.strip_prefix(v).is_some_and(|r| r.starts_with(' ')))
}

/// A rebase-todo line is `<verb> <sha> <subject>` (or a `#`-comment) —
/// the sha is the first hex-looking whitespace-delimited token,
/// mirroring `magit_log_mode::extract_sha`'s same "first hex token"
/// scan (duplicated rather than shared: each mode's line format
/// differs enough that a shared parser would need its own
/// verb/graph-char skip logic anyway).
fn extract_sha(line: &str) -> Option<&str> {
    line.split_whitespace()
        .find(|tok| tok.len() >= 4 && tok.chars().all(|c| c.is_ascii_hexdigit()))
}

/// MG.34: what a rebase buffer's name asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RebaseTarget {
    /// `*magit:rebase*` / `*magit:rebase:<upstream>*` — rebase onto the
    /// named ref, or onto `@{upstream}` when none is named.
    Onto(Option<String>),
    /// `*magit:rebase-edit:<line>:<path>*` — magit's
    /// `magit-edit-line-commit`. Find the commit that last wrote line
    /// `<line>` of `<path>`, and mark **that** commit `edit` so the
    /// rebase stops on it.
    ///
    /// The blame is the reason this is a buffer-name form rather than a
    /// resolved sha handed over by the action: finding the commit costs
    /// a `git blame`, and the handler that fires the row is synchronous
    /// and must not run `git` on the actor thread (MG.31). Same shape
    /// `magit-revision-mode` uses for `*magit:merged:*`.
    EditLine { line: u32, path: String },
}

/// MG.34: the buffer name that asks "amend whatever wrote this line".
///
/// Line first so the split is unambiguous — a path may contain `:`, a
/// line number may not.
pub(crate) fn edit_line_buffer_name(line: u32, path: &str) -> String {
    format!("*magit:rebase-edit:{line}:{path}*")
}

/// Which rebase a buffer name asks for. `None` for a name this mode does
/// not own; the caller treats that as the bare `@{upstream}` form, which
/// is what it has always meant.
fn parse_target(name: &str) -> Option<RebaseTarget> {
    let body = name.strip_suffix('*')?;
    if let Some(rest) = body.strip_prefix("*magit:rebase-edit:") {
        let (line, path) = rest.split_once(':')?;
        let line: u32 = line.parse().ok()?;
        return (!path.is_empty()).then(|| RebaseTarget::EditLine {
            line,
            path: path.to_string(),
        });
    }
    let upstream = body.strip_prefix("*magit:rebase:")?;
    Some(RebaseTarget::Onto(
        (!upstream.is_empty()).then(|| upstream.to_string()),
    ))
}

/// Resolve the upstream and build the todo-buffer text. Returns
/// `(upstream, buffer_text)`; `upstream` is empty when resolution failed
/// — `buffer_text` explains why, and the confirm handler refuses to run
/// against an empty upstream.
///
/// Blocking; call on `spawn_blocking`.
fn build_rebase_buffer(workdir: &Path, target: &RebaseTarget) -> (String, String) {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => return (String::new(), "Not a git repository.\n".to_string()),
    };
    // MG.34: the edit-line form resolves to an ordinary upstream plus
    // "which commit to stop on", so everything below is shared.
    let (upstream, stop_at) = match target {
        RebaseTarget::Onto(Some(u)) => (u.clone(), None),
        RebaseTarget::Onto(None) => {
            match repo.run_git_str(["rev-parse", "--abbrev-ref", "@{upstream}"]) {
                Ok(s) => (s.trim().to_string(), None),
                Err(_) => {
                    return (
                        String::new(),
                        "No upstream configured for this branch.\n\
                         Use `:magit-rebase <ref>` to rebase onto a specific ref.\n"
                            .to_string(),
                    );
                }
            }
        }
        RebaseTarget::EditLine { line, path } => match blame_line_commit(&repo, *line, path) {
            Ok(sha) => (parent_or_root(&repo, &sha), Some(sha)),
            Err(msg) => return (String::new(), msg),
        },
    };
    let range = if upstream == ROOT {
        // `--root` rebases from the first commit, so the log is the
        // whole history rather than a range.
        "HEAD".to_string()
    } else {
        format!("{upstream}..HEAD")
    };
    let log = repo
        .run_git_str(["log", "--reverse", "--format=pick %h %s", &range])
        .unwrap_or_default();
    if log.trim().is_empty() {
        return (
            String::new(),
            format!("Nothing to rebase — already up to date with {upstream}.\n"),
        );
    }
    // MG.34: mark the blamed commit `edit` so the rebase stops there.
    //
    // Matched by sha rather than "the first row", because the first row
    // is only the blamed commit when history is linear: with a merge in
    // range, `--reverse` can put a side branch's older commits ahead of
    // it. Marking the wrong row would stop the rebase on a commit the
    // user never named — shape-identical to the right answer, which is
    // the failure class this slice avoids elsewhere too.
    let (log, note) = match &stop_at {
        None => (log, String::new()),
        Some(sha) => {
            let short = repo
                .run_git_str(["log", "-1", "--format=%h", sha])
                .unwrap_or_default()
                .trim()
                .to_string();
            match mark_edit(&log, &short) {
                Some(marked) => (
                    marked,
                    format!(
                        "# {short} is marked `edit` — it is the commit that wrote that line.\n\
                         # The rebase will stop there; amend, then `:magit-rebase-continue`.\n"
                    ),
                ),
                // Unreachable in practice (the blamed commit is by
                // construction in `<sha>^..HEAD`), but silently shipping
                // an all-`pick` todo would replay history for no reason.
                None => {
                    return (
                        String::new(),
                        format!(
                            "magit: {short} is not in the range being rebased — \
                             nothing to edit.\n"
                        ),
                    );
                }
            }
        }
    };
    let text = format!(
        "{log}\n\
         {note}# Rebase onto {upstream} — edit the list above, then C-c C-c to run,\n\
         # or C-c C-k to abort.\n\
         # Commands: pick, reword, edit, squash, fixup, drop\n\
         # (reword keeps the original message — no message-edit UI yet)\n"
    );
    (upstream, text)
}

/// The upstream that rebases a root commit. `git rebase -i --root`
/// takes it in the same argument position an upstream ref would, so it
/// travels through `RebaseState::upstream` and `run_rebase` unchanged.
const ROOT: &str = "--root";

/// Makes each rebase's scratch files unique. See
/// `run_rebase_with_message` for the collision this prevents.
static REBASE_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `<sha>^`, or [`ROOT`] when `sha` is a root commit and has no parent.
///
/// Asked as "how many parents does it have" rather than "does `<sha>^`
/// resolve", because the obvious spelling of the latter is a trap:
/// `<sha>^{commit}` is peel-to-commit syntax, not first-parent, so it
/// succeeds for *every* commit and quietly reports a root commit as
/// having a parent. `rev-list --parents` prints `<sha> <parent>…`, so a
/// single field means no parents and there is nothing to misread.
fn parent_or_root(repo: &Repository, sha: &str) -> String {
    let parents = repo
        .run_git_str(["rev-list", "--parents", "-n", "1", sha])
        .unwrap_or_default();
    if parents.split_whitespace().count() > 1 {
        format!("{sha}^")
    } else {
        ROOT.to_string()
    }
}

/// The commit that last wrote `line` (1-based) of `path`.
///
/// `Err` carries the buffer text explaining why there is none — an
/// uncommitted line is the case worth naming, since it is the one a user
/// hits by asking about code they just typed.
fn blame_line_commit(repo: &Repository, line: u32, path: &str) -> Result<String, String> {
    let spec = format!("{line},{line}");
    let out = repo
        .run_git_str(["blame", "-L", &spec, "--porcelain", "--", path])
        .map_err(|_| {
            format!("magit: could not blame line {line} of {path} — is it tracked?\n")
        })?;
    // Porcelain's first line is `<sha> <orig-line> <final-line> [<n>]`.
    let sha = out
        .split_whitespace()
        .next()
        .filter(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| format!("magit: no blame for line {line} of {path}.\n"))?;
    if sha.chars().all(|c| c == '0') {
        return Err(format!(
            "magit: line {line} of {path} is not committed yet.\n\
             \n\
             There is no commit to amend — commit it first.\n"
        ));
    }
    Ok(sha.to_string())
}

/// Rewrite the `pick` on the row naming `short` to `edit`. `None` when
/// no row names it.
fn mark_edit(log: &str, short: &str) -> Option<String> {
    mark_verb(log, short, "edit")
}

/// MG.43c: rewrite the `pick` on the row naming `short` to `verb`.
///
/// Generalises [`mark_edit`], which MG.34 needed only for `edit`.
/// Magit's rebase `m` / `w` / `k` are the same operation with `edit`,
/// `reword` and `drop` — the verb is the only thing that differs, so
/// it is a parameter rather than three near-identical walks.
///
/// Matched by sha rather than "the first row", for the reason MG.34
/// recorded: with a merge in range, `--reverse` can put a side
/// branch's older commits ahead of the named one, and marking the
/// wrong row is shape-identical to marking the right one.
pub(crate) fn mark_verb(log: &str, short: &str, verb: &str) -> Option<String> {
    let mut found = false;
    let marked = log
        .lines()
        .map(|l| match l.strip_prefix("pick ") {
            Some(rest) if !found && rest.split_whitespace().next() == Some(short) => {
                found = true;
                format!("{verb} {rest}")
            }
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    found.then_some(marked)
}

/// MG.43c: run an interactive rebase that acts on ONE commit.
///
/// Builds the todo for `<commit>^..HEAD`, rewrites that commit's row
/// to `verb`, and runs it. `message`, when given, is what git's
/// `reword` step writes — see [`run_rebase_with_message`] for why that
/// is what makes `w` work at all.
///
/// Returns the label-worthy error text on failure.
pub(crate) fn rebase_one_commit(
    workdir: &Path,
    commit: &str,
    verb: &str,
    message: Option<&str>,
) -> Result<(), String> {
    // A commit that begins with `-` would be parsed as an OPTION by
    // every `git` call below, not as a revision — `git log -1
    // --format=%h --output=/tmp/x` writes a file rather than reporting
    // a sha. The picker only ever supplies real shas, but this is also
    // reachable from `:magit-rebase-edit-commit <arg>`, where the value
    // is whatever was typed or pasted.
    //
    // Refused rather than escaped: no revision legitimately starts with
    // `-`, so there is nothing to lose by declining, and `--` does not
    // help for the calls that take the revision in option position.
    if commit.starts_with('-') {
        return Err(format!("`{commit}` is not a revision"));
    }
    let repo = Repository::discover(workdir).map_err(|e| e.to_string())?;
    let upstream = parent_or_root(&repo, commit);
    let range = if upstream == ROOT {
        "HEAD".to_string()
    } else {
        format!("{upstream}..HEAD")
    };
    let log = repo
        .run_git_str(["log", "--reverse", "--format=pick %h %s", &range])
        .map_err(|e| e.to_string())?;
    let short = repo
        .run_git_str(["log", "-1", "--format=%h", commit])
        .map_err(|e| e.to_string())?
        .trim()
        .to_string();
    // A commit outside the range would otherwise produce an all-`pick`
    // todo: a rebase that replays history and changes nothing, which
    // looks like success and is not what the row promised.
    let todo = mark_verb(&log, &short, verb)
        .ok_or_else(|| format!("{short} is not in the range being rebased"))?;
    run_rebase_with_message(workdir, &upstream, &todo, message)
}

fn run_rebase(workdir: &Path, upstream: &str, todo: &str) -> Result<(), String> {
    run_rebase_with_message(workdir, upstream, todo, None)
}

/// MG.43c: `run_rebase`, plus the message a `reword` step will take.
///
/// **This is what makes rebase `w` possible.** `GIT_EDITOR=true`
/// accepts a reword's message unchanged, which turns the operation
/// into a no-op that reports success — the limitation this module's
/// header records. Pointing `GIT_EDITOR` at `cp <file>` instead hands
/// git a message we collected up front, exactly the way
/// `GIT_SEQUENCE_EDITOR` already hands it a todo list.
///
/// With no message the old behaviour is unchanged: `true` accepts
/// whatever git generated, which is correct for `edit` and `drop`
/// because neither opens an editor.
fn run_rebase_with_message(
    workdir: &Path,
    upstream: &str,
    todo: &str,
    message: Option<&str>,
) -> Result<(), String> {
    // Process id + upstream is NOT unique: two rebases can be in
    // flight at once, and when they share an upstream they share the
    // path — one overwrites the other's todo and git replays the wrong
    // list. A monotonic counter makes each call's file its own.
    //
    // Found by two tests colliding: identical fixture repos built in
    // the same second produce identical shas, so both named the same
    // upstream. The tests exposed it; the race is real without them.
    let seq = REBASE_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "lattice-rebase-todo-{}-{seq}-{}",
        std::process::id(),
        upstream.replace(['/', ' '], "_")
    ));
    std::fs::write(&tmp, todo).map_err(|e| e.to_string())?;
    let editor_cmd = format!("cp {}", tmp.display());
    // Kept alive for the whole call: dropping it would remove the file
    // before git's reword step reads it.
    let msg_tmp = match message {
        Some(m) => {
            let path = std::env::temp_dir().join(format!(
                "lattice-rebase-msg-{}-{seq}-{}",
                std::process::id(),
                upstream.replace(['/', ' '], "_")
            ));
            std::fs::write(&path, m).map_err(|e| e.to_string())?;
            Some(path)
        }
        None => None,
    };
    let git_editor = match &msg_tmp {
        Some(path) => format!("cp {}", path.display()),
        None => "true".to_string(),
    };
    let result = std::process::Command::new("git")
        .args(["rebase", "-i", upstream])
        .env("GIT_SEQUENCE_EDITOR", &editor_cmd)
        .env("GIT_EDITOR", &git_editor)
        .current_dir(workdir)
        .output();
    let _ = std::fs::remove_file(&tmp);
    if let Some(path) = &msg_tmp {
        let _ = std::fs::remove_file(path);
    }
    match result {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MG.12: `C-c C-k` on a todo buffer that was never executed is
    /// just "close this buffer" — there is nothing to throw away, so
    /// it must not ask. This is why the confirm is gated rather than
    /// unconditional.
    #[test]
    fn a_gitdir_with_no_rebase_state_is_not_in_progress() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(!rebase_in_progress(dir.path()));
    }

    /// Both backends count: `rebase-merge` is the modern one,
    /// `rebase-apply` the legacy `--apply` / `git am` one. Missing
    /// either would abort real in-flight work without asking.
    #[test]
    fn either_rebase_state_directory_counts_as_in_progress() {
        for marker in ["rebase-merge", "rebase-apply"] {
            let dir = tempfile::tempdir().expect("temp dir");
            std::fs::create_dir(dir.path().join(marker)).expect("create marker dir");
            assert!(
                rebase_in_progress(dir.path()),
                "`{marker}` must count as a rebase in progress"
            );
        }
    }

    #[test]
    fn abort_confirm_points_at_the_execute_action() {
        match abort_rebase_confirm() {
            Effect::Confirm {
                prompt,
                yes_action,
                args,
            } => {
                assert_eq!(prompt, "Abort this rebase?");
                assert_eq!(yes_action, "action:magit-rebase-abort-execute");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    // ── MG.34: `e` edit-line-commit ─────────────────────────────────

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A repository with three commits, each of which wrote one line of
    /// `a.txt` — so blaming a line picks out a *specific* commit rather
    /// than whichever one happens to be HEAD.
    fn repo_with_a_line_per_commit() -> (tempfile::TempDir, [String; 3]) {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        git(p, &["config", "user.email", "t@lattice.dev"]);
        git(p, &["config", "user.name", "lattice-test"]);
        let mut shas = Vec::new();
        for (n, body) in [("one", "first\n"), ("two", "second\n"), ("three", "third\n")] {
            let mut text = std::fs::read_to_string(p.join("a.txt")).unwrap_or_default();
            text.push_str(body);
            std::fs::write(p.join("a.txt"), text).expect("write");
            git(p, &["add", "a.txt"]);
            git(p, &["commit", "-m", n]);
            shas.push(git(p, &["rev-parse", "HEAD"]));
        }
        let shas: [String; 3] = shas.try_into().expect("three commits");
        (dir, shas)
    }

    /// The three name forms, and that they do not bleed into each
    /// other. `rebase-edit:` shares a prefix with `rebase:` up to the
    /// colon, so a naive `strip_prefix("*magit:rebase:")` ordering would
    /// mis-parse one as the other and rebase onto a ref named
    /// `-edit:12:src/a.rs`.
    #[test]
    fn the_three_buffer_name_forms_stay_distinct() {
        assert_eq!(parse_target("*magit:rebase*"), None, "bare name is not ours");
        assert_eq!(
            parse_target("*magit:rebase:origin/main*"),
            Some(RebaseTarget::Onto(Some("origin/main".into())))
        );
        assert_eq!(
            parse_target("*magit:rebase:*"),
            Some(RebaseTarget::Onto(None))
        );
        assert_eq!(
            parse_target("*magit:rebase-edit:12:src/a.rs*"),
            Some(RebaseTarget::EditLine {
                line: 12,
                path: "src/a.rs".into()
            })
        );
    }

    /// Line first, path second — because a path may contain a colon and
    /// a line number may not. Splitting the other way round would break
    /// on any such path, which is the reason for the ordering.
    #[test]
    fn a_path_containing_a_colon_still_parses() {
        let name = edit_line_buffer_name(7, "weird:name.txt");
        assert_eq!(
            parse_target(&name),
            Some(RebaseTarget::EditLine {
                line: 7,
                path: "weird:name.txt".into()
            })
        );
    }

    /// The load-bearing reason `mark_edit` matches by sha instead of
    /// taking row one: `--reverse` orders by commit date, so a merge in
    /// range can put a side branch's older commits ahead of the one that
    /// was blamed. Marking row one would stop the rebase on a commit the
    /// user never named — and the resulting todo looks perfectly
    /// plausible, which is what makes it worth pinning.
    #[test]
    fn the_marked_row_is_the_named_commit_not_the_first_one() {
        let log = "pick aaaaaaa older side commit\n\
                   pick bbbbbbb the one that wrote the line\n\
                   pick ccccccc later";
        let marked = mark_edit(log, "bbbbbbb").expect("bbbbbbb is in range");
        assert_eq!(
            marked,
            "pick aaaaaaa older side commit\n\
             edit bbbbbbb the one that wrote the line\n\
             pick ccccccc later"
        );
    }

    /// A commit outside the range is refused rather than silently
    /// yielding an all-`pick` todo, which would replay history and
    /// change nothing — a rebase the user did not ask for.
    #[test]
    fn a_commit_not_in_range_is_refused() {
        assert_eq!(mark_edit("pick aaaaaaa only", "bbbbbbb"), None);
    }

    /// MG.43c: a value that would be read as an option is refused.
    ///
    /// The commit reaches `git log -1 --format=%h <commit>` in option
    /// position, so `--output=/tmp/x` would write a file instead of
    /// reporting a sha. The picker only supplies real shas, but
    /// `:magit-rebase-edit-commit <arg>` takes whatever was typed.
    #[test]
    fn an_option_looking_commit_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        for bad in ["--output=/tmp/lattice-should-not-exist", "-n", "--help"] {
            assert!(
                rebase_one_commit(dir.path(), bad, "edit", None).is_err(),
                "`{bad}` must be refused rather than passed to git",
            );
        }
        assert!(
            !std::path::Path::new("/tmp/lattice-should-not-exist").exists(),
            "the refused value must not have reached git",
        );
    }

    /// MG.43c: the verb is the operation, and only the named row's
    /// verb changes.
    #[test]
    fn mark_verb_rewrites_only_the_named_row() {
        let log = "pick aaaaaaa one\npick bbbbbbb two\npick ccccccc three";
        for verb in ["edit", "reword", "drop"] {
            let marked = mark_verb(log, "bbbbbbb", verb).expect("in range");
            assert_eq!(
                marked,
                format!("pick aaaaaaa one\n{verb} bbbbbbb two\npick ccccccc three"),
            );
        }
    }

    /// MG.43c: **`m` really does stop the rebase at the named commit.**
    ///
    /// The failure this guards is the quiet one: a todo whose verb
    /// never took would replay history unchanged and report success,
    /// so the row would look like it worked and do nothing.
    #[test]
    fn editing_a_commit_stops_the_rebase_there() {
        let (dir, shas) = repo_with_a_line_per_commit();
        let p = dir.path();
        rebase_one_commit(p, &shas[1], "edit", None).expect("rebase runs");
        assert!(
            rebase_in_progress(&p.join(".git")),
            "an `edit` verb must leave the rebase stopped",
        );
        assert_eq!(
            git(p, &["rev-parse", "HEAD"]),
            shas[1],
            "it must stop ON the named commit, not before or after it",
        );
        git(p, &["rebase", "--abort"]);
    }

    /// MG.43c: `k` removes the named commit and keeps the rest.
    ///
    /// Each commit touches its OWN file. The shared-file fixture the
    /// other tests use would conflict here, and legitimately so —
    /// dropping a commit a later one builds on is a real conflict git
    /// stops on, not something this row should paper over.
    #[test]
    fn removing_a_commit_drops_only_that_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git(p, &["init", "-b", "main"]);
        git(p, &["config", "user.email", "t@lattice.dev"]);
        git(p, &["config", "user.name", "lattice-test"]);
        for (n, file) in [("one", "a.txt"), ("two", "b.txt"), ("three", "c.txt")] {
            std::fs::write(p.join(file), format!("{n}\n")).expect("write");
            git(p, &["add", file]);
            git(p, &["commit", "-m", n]);
        }
        let middle = git(p, &["rev-parse", "HEAD~1"]);
        rebase_one_commit(p, &middle, "drop", None).expect("rebase runs");
        let subjects = git(p, &["log", "--format=%s"]);
        assert!(!subjects.contains("two"), "`two` must be gone: {subjects:?}");
        assert!(subjects.contains("one"), "`one` must survive: {subjects:?}");
        assert!(
            subjects.contains("three"),
            "`three` must survive: {subjects:?}"
        );
    }

    /// MG.43c: **`w` actually applies the message — the whole reason
    /// `GIT_EDITOR` is pointed at `cp <file>` instead of `true`.**
    ///
    /// With `GIT_EDITOR=true` git accepts a reword's message
    /// unchanged, so the operation succeeds and changes nothing. That
    /// is precisely the limitation this module's header used to
    /// record, and it is invisible from the outside: the command exits
    /// 0 either way. Asserting on the resulting message is the only
    /// thing that tells the two apart.
    #[test]
    fn rewording_a_commit_applies_the_new_message() {
        let (dir, _) = repo_with_a_line_per_commit();
        let p = dir.path();
        let middle = git(p, &["rev-parse", "HEAD~1"]);
        rebase_one_commit(p, &middle, "reword", Some("a better subject")).expect("rebase runs");

        let subjects = git(p, &["log", "--format=%s"]);
        assert!(
            subjects.contains("a better subject"),
            "the new message must reach the commit: {subjects:?}",
        );
        assert!(
            !subjects.contains("two"),
            "the old message must be gone: {subjects:?}",
        );
        // The other commits keep theirs — a reword rewrites one
        // message, not the branch's.
        assert!(subjects.contains("one") && subjects.contains("three"), "{subjects:?}");
    }

    /// Blame resolves the commit that wrote *that* line, not HEAD.
    #[test]
    fn blame_names_the_commit_that_wrote_the_line() {
        let (dir, shas) = repo_with_a_line_per_commit();
        let repo = Repository::discover(dir.path()).expect("discover");
        for (line, expected) in [(1, &shas[0]), (2, &shas[1]), (3, &shas[2])] {
            assert_eq!(
                blame_line_commit(&repo, line, "a.txt").as_deref(),
                Ok(expected.as_str()),
                "line {line} must blame to its own commit"
            );
        }
    }

    /// An uncommitted line has no commit to amend. The message says so
    /// rather than the buffer being empty, because "I just typed this"
    /// is the common way to reach it.
    #[test]
    fn an_uncommitted_line_says_so_instead_of_blaming_zeros() {
        let (dir, _) = repo_with_a_line_per_commit();
        let p = dir.path();
        let mut text = std::fs::read_to_string(p.join("a.txt")).expect("read");
        text.push_str("fresh\n");
        std::fs::write(p.join("a.txt"), text).expect("write");
        let repo = Repository::discover(p).expect("discover");
        let err = blame_line_commit(&repo, 4, "a.txt").expect_err("line 4 is uncommitted");
        assert!(
            err.contains("not committed yet"),
            "must name the real reason, got: {err}"
        );
    }

    /// `<sha>^` for a commit with a parent, `--root` for the first
    /// commit in the repository — which has none, so `git rebase -i
    /// <sha>^` would fail outright.
    #[test]
    fn the_root_commit_rebases_with_root_not_with_a_missing_parent() {
        let (dir, shas) = repo_with_a_line_per_commit();
        let repo = Repository::discover(dir.path()).expect("discover");
        assert_eq!(parent_or_root(&repo, &shas[0]), ROOT);
        assert_eq!(parent_or_root(&repo, &shas[1]), format!("{}^", shas[1]));
    }

    /// End to end: asking about line 2 produces a todo whose `edit` row
    /// is the second commit, rebasing onto its parent.
    #[test]
    fn edit_line_builds_a_todo_that_stops_on_that_lines_commit() {
        let (dir, shas) = repo_with_a_line_per_commit();
        let p = dir.path();
        let (upstream, text) = build_rebase_buffer(
            p,
            &RebaseTarget::EditLine {
                line: 2,
                path: "a.txt".into(),
            },
        );
        assert_eq!(upstream, format!("{}^", shas[1]), "rebase onto its parent");

        let short = git(p, &["log", "-1", "--format=%h", &shas[1]]);
        let edits: Vec<&str> = text.lines().filter(|l| l.starts_with("edit ")).collect();
        assert_eq!(edits.len(), 1, "exactly one commit is marked, got: {text}");
        assert!(
            edits[0].starts_with(&format!("edit {short} ")),
            "the marked commit must be the one that wrote line 2; got {:?}",
            edits[0]
        );
        // The third commit is still replayed after it, or the rebase
        // would silently drop it.
        let short3 = git(p, &["log", "-1", "--format=%h", &shas[2]]);
        assert!(
            text.contains(&format!("pick {short3} ")),
            "later commits must still be picked; got: {text}"
        );
    }

    /// The whole point of the `edit` row is that the rebase stops and
    /// waits — so the buffer must say how to resume, or the user is left
    /// in a state with no visible exit.
    #[test]
    fn the_todo_names_the_command_that_resumes_the_rebase() {
        let (dir, _) = repo_with_a_line_per_commit();
        let (_, text) = build_rebase_buffer(
            dir.path(),
            &RebaseTarget::EditLine {
                line: 2,
                path: "a.txt".into(),
            },
        );
        assert!(
            text.contains(":magit-rebase-continue"),
            "the way out must be named in the buffer; got: {text}"
        );
    }

    /// The pre-MG.34 path is unchanged: a named upstream still produces
    /// an all-`pick` todo with nothing marked.
    #[test]
    fn an_ordinary_rebase_marks_nothing() {
        let (dir, shas) = repo_with_a_line_per_commit();
        let (upstream, text) =
            build_rebase_buffer(dir.path(), &RebaseTarget::Onto(Some(shas[0].clone())));
        assert_eq!(upstream, shas[0]);
        assert!(
            !text.lines().any(|l| l.starts_with("edit ")),
            "a plain rebase must not mark any commit; got: {text}"
        );
    }
}
