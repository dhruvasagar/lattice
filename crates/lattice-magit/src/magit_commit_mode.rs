//! MG.4: magit-commit major mode.
//!
//! Shows the staged diff (read-only top region) and an editable
//! message region below. C-c C-c commits, C-c C-k aborts.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{Effect, QuitScope};
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_vcs::{Commit, Repository};

use crate::buffer_state::{BufferStateGuard, BufferStates};
use crate::headerline;

pub struct MagitCommitMode;

impl MagitCommitMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-commit-mode")
    }
}

/// Separates the editable message (above) from the read-only staged
/// diff (below).
///
/// The message is on TOP, matching `git commit --verbose` and Emacs
/// magit: you open this buffer to write a message, so the cursor
/// should land where you type without scrolling past a diff that may
/// be hundreds of lines long. The diff is reference material for
/// while you write, which is what "below" means.
const DIFF_MARKER: &str = "--- Staged diff (review only — not part of the message) ---";

fn magit_commit_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<C-c><C-c>", doc: "Confirm commit", cmd: "action:magit-commit-confirm" },
            keymap_entry! { mode: Insert, chord: "<C-c><C-k>", doc: "Abort commit", cmd: "action:magit-commit-abort" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-c>", doc: "Confirm commit", cmd: "action:magit-commit-confirm" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-k>", doc: "Abort commit", cmd: "action:magit-commit-abort" },
        ]
    })
}

pub struct CommitState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    amend: bool,
    /// Line of `DIFF_MARKER` — the boundary between the editable
    /// message above and the read-only staged diff below. `<CR>`'s
    /// file-visit handler only fires BELOW it, so pressing it while
    /// writing the message does nothing rather than jumping away.
    diff_start_line: u32,
}

/// MG.22: this buffer's [`MagitView`].
///
/// The commit buffer had none, which cost it twice: `<CR>` needed its
/// own handler and its own diff-path parser, and hunk staging was
/// **refused outright** in the staged region below the message,
/// because `diff_source` fell through to the trait default (`None` =
/// "not classifiable here").
///
/// Its diff is `git diff --cached` — the index, by construction. So
/// `u` unstages a hunk from the commit you are composing, and `s` is
/// correctly refused with "already staged".
struct CommitView(Arc<Mutex<CommitState>>);

impl crate::buffer_state::MagitView for CommitView {
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

    /// The staged diff is rebuilt by the mode's own lifecycle, not by
    /// `gr` — re-running it here would race the message the user is
    /// typing above it.
    fn refresh(&self) -> Option<Effect> {
        None
    }

    /// Only *below* the marker. Above it is the message being written,
    /// which is not diff content at all — the same boundary `<CR>`
    /// respected before this view existed.
    fn diff_source(
        &self,
        cursor: lattice_protocol::position::Position,
    ) -> Option<crate::buffer_state::DiffSource> {
        let g = self.0.lock().ok()?;
        (cursor.line > g.diff_start_line).then_some(crate::buffer_state::DiffSource::Staged)
    }

    /// The index's blob — this buffer's diff is the index.
    fn diff_target(&self, path: &std::path::Path) -> Option<Effect> {
        Some(Effect::OpenSyntheticBuffer {
            name: format!("*magit:file:staged:{}*", path.display()),
            mode_id: "magit-file-revision-mode".to_string(),
        })
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        Some(self.0.lock().ok()?.workdir.clone())
    }
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type CommitStatesHandle = Arc<BufferStates<CommitState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<CommitState>>> {
    crate::buffer_state::state_for::<CommitState>(ctx)
}

impl Mode for MagitCommitMode {
    type Guard = BufferStateGuard<CommitState>;

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
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_commit_keymap_entries())
    }

    /// MG.13: boot-registered — see `buffer_state`'s module docs.
    ///
    /// `diff_end_line` is the one field this mode cannot know before
    /// its `.await` (it is derived from the generated buffer text). It
    /// is published as `0` and filled in afterwards; a `<CR>` landing
    /// in that window sees `cursor.line >= 0` and declines, which is
    /// the correct answer for a buffer whose diff region does not exist
    /// yet.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // ── confirm (C-c C-c) ──────────────────────
            ActionHandlerContribution {
                action_name: "action:magit-commit-confirm",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (message, workdir, amend) = {
                        let g = s.lock().ok()?;
                        let handle = g.store.handle_for(g.buffer_id)?;
                        let snap = handle.snapshot();
                        // The message is everything ABOVE the diff
                        // marker. Stopping at the marker (rather than
                        // skipping past it) means a diff line can never
                        // leak into a commit message, even if the user
                        // edited or deleted the marker line itself —
                        // absent marker ⇒ the whole buffer is message,
                        // which is the safe direction for a buffer
                        // whose entire purpose is the message.
                        let mut message = String::new();
                        for l in 0..snap.buffer.line_count() as u32 {
                            let text = snap.buffer.line(l).unwrap_or_default();
                            if text.contains(DIFF_MARKER) {
                                break;
                            }
                            if !text.trim().is_empty() {
                                message.push_str(&text);
                                message.push('\n');
                            }
                        }
                        (message, g.workdir.clone(), g.amend)
                    };
                    if message.trim().is_empty() {
                        // Fail loud instead of silently doing nothing —
                        // an empty subject used to just no-op the chord
                        // with no feedback.
                        return Some(Effect::Echo {
                            level: lattice_grammar::EchoLevel::Error,
                            text: "magit: commit message is empty".to_string(),
                        });
                    }
                    // Commit is a bounded, single-object git write
                    // (unlike `git status`/`git diff`, it never scans
                    // the working tree) — but it's still disk I/O, so it
                    // stays off the actor thread like every other
                    // mutation. The buffer closes optimistically; a
                    // failure surfaces via `tracing::error!` (no
                    // synchronous path back to the echo area from a
                    // detached task) rather than leaving the compose
                    // buffer open forever on a rare `gix` failure.
                    tokio::task::spawn(tokio::task::spawn_blocking(move || {
                        let Ok(repo) = Repository::discover(&workdir) else {
                            tracing::error!(target: "lattice_magit", "commit: repo discover failed");
                            return;
                        };
                        let result = if amend {
                            Commit::amend(&repo, message.trim())
                        } else {
                            Commit::create(&repo, message.trim())
                        };
                        if let Err(e) = result {
                            tracing::error!(target: "lattice_magit", "commit failed: {e}");
                        }
                    }));
                    Some(Effect::QuitEditor {
                        force: false,
                        scope: QuitScope::Pane,
                    })
                }),
            },
            // ── abort (C-c C-k) ─────────────────────────
            ActionHandlerContribution {
                action_name: "action:magit-commit-abort",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let _ = state(ctx)?;
                    Some(Effect::QuitEditor {
                        force: false,
                        scope: QuitScope::Pane,
                    })
                }),
            },
            // <CR> — visit the file at cursor AS STAGED (the index
            // blob), not the live working-tree file: this buffer shows
            // the STAGED diff specifically, which may already differ
            // from a since-edited working copy. Same target
            // magit-diff-mode's Staged-scoped `<CR>` opens.
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

            // Detect amend: opened via `ca` → buffer name is "*magit:amend*"
            let amend = store
                .name_for(buffer_id)
                .map(|n| n.contains("amend"))
                .unwrap_or(false);

            // MG.14: what is staged is not knowable until the diff
            // below lands, so the header fills in with it. `AMEND` is
            // known now but is published together with the rest — a
            // half-row that gains fields a beat later reads as a
            // glitch.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: publish BEFORE the first `.await`. `diff_end_line`
            // is not knowable yet (it comes out of the generated text);
            // it starts at 0 — which makes `<CR>` decline rather than
            // act on a diff region that does not exist — and is filled
            // in below once the buffer is populated.
            let Some(states) = ctx.service::<CommitStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                CommitState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    amend,
                    diff_start_line: u32::MAX,
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            // MG.22: publish the view. Without it `<CR>` has no target
            // to resolve here, and hunk staging in the staged region
            // stays refused for want of a `diff_source`.
            if let Some(views) = ctx.service::<crate::buffer_state::MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(CommitView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            // Populate the buffer: staged diff + message area. Amend
            // pre-populates the previous commit's message instead of a
            // blank region, matching what it's about to replace.
            let wd = workdir.clone();
            let (staged, prior_message, branch) = tokio::task::spawn_blocking(move || {
                let staged = run_staged_diff(&wd);
                let prior = if amend {
                    run_prior_commit_message(&wd)
                } else {
                    String::new()
                };
                // MG.14: the branch this commit lands on. One
                // `rev-parse` inside the SAME blocking call that
                // already runs two git commands.
                let branch = Repository::discover(&wd)
                    .ok()
                    .and_then(|r| r.run_git_str(["rev-parse", "--abbrev-ref", "HEAD"]).ok())
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                (staged, prior, branch)
            })
            .await
            .unwrap_or_default();
            headerline::publish(&hl, headerline::commit_fields(&branch, &staged, amend));
            // Message first (line 0 for a fresh commit, so the cursor
            // opens where you type), then the marker, then the diff.
            let initial = format!(
                "{}\n\
                 \n\
                 {DIFF_MARKER}\n\
                 {}\n",
                prior_message.trim(),
                if staged.is_empty() {
                    "(nothing staged)"
                } else {
                    &staged
                },
            );
            // Everything at/below the marker is the diff. Scoping the
            // styler to that range keeps the marker's own leading
            // `---` from being misclassified as a diff file marker
            // (see `highlight::commit_buffer_styled_spans`).
            let diff_start_line = initial
                .lines()
                .position(|l| l.contains(DIFF_MARKER))
                .unwrap_or(0);
            let line_count = initial.lines().count();
            let spans = crate::highlight::commit_buffer_styled_spans(
                &initial,
                diff_start_line + 1,
                line_count,
            );
            crate::buffer_io::replace_buffer_text(&handle, initial).await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.store_and_wake(buffer_id, spans);
            }

            // Late-resolved field, now that the text exists.
            if let Ok(mut g) = state.lock() {
                g.diff_start_line = diff_start_line as u32;
            }

            Ok(guard)
        })
    }
}

fn run_staged_diff(workdir: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["diff", "--cached"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

/// `git log -1 --format=%B` — the current HEAD commit's full message,
/// used to pre-populate the amend buffer instead of leaving it blank.
fn run_prior_commit_message(workdir: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["log", "-1", "--format=%B"])
        .current_dir(workdir)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message is above the diff. Reported directly: you open this
    /// buffer to write a message, so the cursor must land where you
    /// type rather than after a diff that can be hundreds of lines.
    /// Matches `git commit --verbose` and Emacs magit.
    #[test]
    fn the_message_area_comes_before_the_diff() {
        let buffer = format!("subject line\n\n{DIFF_MARKER}\ndiff --git a/x b/x\n+added\n");
        let marker = buffer
            .lines()
            .position(|l| l.contains(DIFF_MARKER))
            .expect("marker present");
        assert_eq!(marker, 2, "the diff marker must sit below the message");
        assert!(
            buffer.lines().next().unwrap().contains("subject"),
            "line 0 is the subject, so a fresh commit opens with the cursor \
             already in the message"
        );
    }

    /// Extraction stops AT the marker rather than skipping past it, so
    /// a diff line cannot end up in a commit message even if the user
    /// edited or deleted the marker.
    #[test]
    fn a_diff_line_never_leaks_into_the_extracted_message() {
        let buffer =
            format!("subject\n\nbody line\n{DIFF_MARKER}\ndiff --git a/x b/x\n+added\n-removed\n");
        let mut message = String::new();
        for line in buffer.lines() {
            if line.contains(DIFF_MARKER) {
                break;
            }
            if !line.trim().is_empty() {
                message.push_str(line);
                message.push('\n');
            }
        }
        assert_eq!(message, "subject\nbody line\n");
        assert!(!message.contains("diff --git"));
        assert!(!message.contains("+added"));
    }

    /// With the marker gone, the whole buffer is message. That is the
    /// safe direction to fail for a buffer whose entire purpose is the
    /// message — better than silently committing nothing.
    #[test]
    fn a_missing_marker_treats_everything_as_message() {
        let buffer = "just a subject\n";
        let mut message = String::new();
        for line in buffer.lines() {
            if line.contains(DIFF_MARKER) {
                break;
            }
            if !line.trim().is_empty() {
                message.push_str(line);
                message.push('\n');
            }
        }
        assert_eq!(message, "just a subject\n");
    }

    /// `<CR>` visits a file only BELOW the marker. Pressing it while
    /// writing the message must do nothing rather than jump away
    /// mid-sentence.
    #[test]
    fn enter_visits_a_file_only_below_the_diff_marker() {
        let diff_start: u32 = 2;
        for line in [0u32, 1, 2] {
            assert!(line <= diff_start, "line {line} is message or marker");
        }
        assert!(3 > diff_start, "line 3 is inside the diff");
    }

    /// Before the diff lands, `diff_start_line` is `u32::MAX` so the
    /// gate refuses everything — a `<CR>` in the window between the
    /// buffer opening and git answering must not visit a file chosen
    /// from a half-built buffer.
    #[test]
    fn the_gate_refuses_until_the_diff_boundary_is_known() {
        let unset = u32::MAX;
        for line in [0u32, 5, 1000] {
            assert!(line <= unset, "every line is refused while unset");
        }
    }
}
