//! MG.1: magit-core shared minor mode.
//!
//! Activates on magit buffers. Provides shared keymap with real
//! navigation handlers: ]]/[[ (sections), ]f/[f (files/entries),
//! ]c/[c (hunks). Each returns Effect::SelectionChange — the same
//! cursor-move primitive diff-mode uses for hunk navigation.

use std::sync::{Arc, OnceLock};

use lattice_core::BufferId;
use lattice_grammar::{AppEffect, Effect};
use lattice_mode::{
    ActionContext, ActivationPolicy, BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry,
    LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet, keymap_entry,
};
use lattice_protocol::position::Position;

use crate::buffer_state::DiffSource;
use crate::magit_blame_mode::MagitBlameMode;
use crate::magit_branch_mode::MagitBranchMode;
use crate::magit_commit_mode::MagitCommitMode;
use crate::magit_diff_mode::MagitDiffMode;
use crate::magit_file_revision_mode::MagitFileRevisionMode;
use crate::magit_log_mode::MagitLogMode;
use crate::magit_rebase_mode::MagitRebaseMode;
use crate::magit_revision_mode::MagitRevisionMode;
use crate::magit_stash_mode::MagitStashMode;
use crate::magit_status_mode::MagitStatusMode;

/// Empty RAII guard — vestigial after MG.13.
///
/// It used to hold a `Vec<ActionHandlerRegistration>`, and its doc
/// comment already named the hazard that motivated MG.13: "two buffers
/// of the same major mode open at once silently let the second's
/// `on_activate` replace the first's handler (registry is
/// last-write-wins per `CommandId`), so firing the chord in buffer A
/// can execute buffer B's captured state against A's cursor."
///
/// Holding the tokens bounded the damage — the guard unregistered on
/// close — but could not prevent it, because the registry has no buffer
/// dimension: two live registrations of one `CommandId` cannot coexist
/// no matter who owns the tokens. MG.13 removes the hazard at the
/// source instead: every magit handler is registered **once** at boot
/// via `Mode::action_handlers()` and resolves per-buffer state from a
/// service at call time, so there is nothing per-activation left to
/// unwind. Kept only because `Mode` requires an associated `Guard`.
#[derive(Default)]
pub struct ActionRegsGuard;

pub struct MagitCoreMode;

impl MagitCoreMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-core-mode")
    }
}

fn magit_core_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "gr", doc: "Refresh current magit buffer", cmd: "action:magit-refresh" },
            keymap_entry! { mode: Normal, chord: "q", doc: "Close magit buffer", cmd: "action:magit-close" },
            keymap_entry! { mode: Normal, chord: "]]", doc: "Next section", cmd: "action:magit-next-section" },
            keymap_entry! { mode: Normal, chord: "[[", doc: "Previous section", cmd: "action:magit-prev-section" },
            keymap_entry! { mode: Normal, chord: "]f", doc: "Next file", cmd: "action:magit-next-file" },
            keymap_entry! { mode: Normal, chord: "[f", doc: "Previous file", cmd: "action:magit-prev-file" },
            keymap_entry! { mode: Normal, chord: "]c", doc: "Next hunk", cmd: "action:magit-next-hunk" },
            keymap_entry! { mode: Normal, chord: "[c", doc: "Previous hunk", cmd: "action:magit-prev-hunk" },
            keymap_entry! { mode: Normal, chord: "<Tab>", doc: "Toggle fold", cmd: "action:magit-toggle-fold" },
            keymap_entry! { mode: Normal, chord: "<S-Tab>", doc: "Cycle sections", cmd: "action:magit-cycle-sections" },
            // Operations on the commit under the cursor. Keys follow
            // **evil-collection-magit**, not raw magit — the reference
            // set for a modal editor, because it is the one that already
            // resolved magit-vs-vim collisions:
            //
            //   revert  magit `V` → evil `_`   ("subtracting a commit")
            //   reset   magit `X` → evil `O`
            //   discard magit `k` → evil `x`
            //   apply   `A` in both
            //
            // MG.20 originally took `V` for revert, citing "Emacs
            // magit's own keys". Magit does bind `V` — but magit is not
            // modal, so it costs magit nothing. Here it cost linewise
            // Visual in every magit buffer: the chord is consumed even
            // on a row with no commit, so `V` could not start a
            // selection at all, which MG.18e's region staging needs.
            // evil-magit frees `V` for `evil-visual-line` for exactly
            // that reason, and vim-fugitive likewise keeps `V` unbound
            // so its visual-mode staging works. `_` is free here (not
            // even a builtin motion yet), so this costs nothing.
            //
            // (The same commit also mis-attributed `O` to magit, which
            // uses `X`. `O` is evil-magit's remap — the binding was
            // right, the reason was not.)
            keymap_entry! { mode: Normal, chord: "A", doc: "Cherry-pick the commit at cursor", cmd: "action:magit-cherry-pick" },
            keymap_entry! { mode: Normal, chord: "_", doc: "Revert the commit at cursor", cmd: "action:magit-revert" },
            keymap_entry! { mode: Normal, chord: "Os", doc: "Reset --soft to the commit at cursor", cmd: "action:magit-reset-soft" },
            keymap_entry! { mode: Normal, chord: "Om", doc: "Reset --mixed to the commit at cursor", cmd: "action:magit-reset-mixed" },
            keymap_entry! { mode: Normal, chord: "Oh", doc: "Reset --hard to the commit at cursor (asks first)", cmd: "action:magit-reset-hard" },
        ]
    })
}

/// MG.20: build the handler for a commit operation.
///
/// Resolves the commit under the cursor through the buffer's
/// [`MagitView`], then either asks (destructive) or runs.
fn commit_op(
    action_name: &'static str,
    op: crate::magit_global_mode::CommitOp,
) -> lattice_mode::ActionHandlerContribution {
    lattice_mode::ActionHandlerContribution {
        action_name,
        handler: Arc::new(move |ctx: &ActionContext<'_>| {
            let view = crate::buffer_state::view_for(ctx)?;
            let commit = view.commit_at_cursor(ctx.cursor)?;
            match op.confirm_action {
                // Destructive: the ask half performs no git call at
                // all, so answering `n` cannot mutate — MG.12's rule.
                Some(yes) => Some(crate::confirm::ask(
                    format!("git {} {commit} — discard uncommitted changes?", op.what),
                    yes,
                )),
                None => {
                    let workdir = view.workdir()?;
                    Some(crate::magit_global_mode::spawn_commit_op(
                        op, workdir, &commit,
                    ))
                }
            }
        }),
    }
}

/// The post-confirmation half of a destructive [`commit_op`].
fn commit_op_execute(
    action_name: &'static str,
    op: crate::magit_global_mode::CommitOp,
) -> lattice_mode::ActionHandlerContribution {
    lattice_mode::ActionHandlerContribution {
        action_name,
        handler: Arc::new(move |ctx: &ActionContext<'_>| {
            let view = crate::buffer_state::view_for(ctx)?;
            let commit = view.commit_at_cursor(ctx.cursor)?;
            let workdir = view.workdir()?;
            Some(crate::magit_global_mode::spawn_commit_op(
                op, workdir, &commit,
            ))
        }),
    }
}

/// Move cursor to `target_row`. Returns `Effect::CursorMove` —
/// the canonical cursor-jump primitive.
fn cursor_at(target_row: u32) -> Effect {
    Effect::CursorMove(Position::new(target_row, 0))
}

/// Scan buffer for section header lines and return their row numbers.
fn section_headers(store: &BufferStoreHandle, buffer_id: BufferId) -> Vec<u32> {
    let Some(h) = store.handle_for(buffer_id) else {
        return vec![];
    };
    let snap = h.snapshot();
    let mut lines = Vec::new();
    for l in 0..snap.buffer.line_count() as u32 {
        if let Some(t) = snap.buffer.line(l) {
            if crate::sections::is_section_header(t.trim()) {
                lines.push(l);
            }
        }
    }
    lines
}

/// Scan buffer for file/entry lines (indented, non-header).
///
/// Fold audit fix: this used to check `starts_with("  ")` on the
/// line AFTER trimming it — `trim()` strips all leading whitespace,
/// so a trimmed string can never start with two spaces. The check
/// was unsatisfiable; `]f`/`[f` never navigated anywhere, on any
/// magit buffer, from the moment they were written. Now checks the
/// RAW (untrimmed) line, and trims only for the prefix comparisons
/// that follow it.
fn entry_lines(store: &BufferStoreHandle, buffer_id: BufferId) -> Vec<u32> {
    let Some(h) = store.handle_for(buffer_id) else {
        return vec![];
    };
    let snap = h.snapshot();
    let mut lines = Vec::new();
    for l in 0..snap.buffer.line_count() as u32 {
        if let Some(raw) = snap.buffer.line(l) {
            // Section headers and one-off status messages ("No
            // changes...") all render at column 0 — never indented —
            // so this guard alone already excludes them; no need to
            // separately re-check their text.
            if raw.starts_with("  ") && !raw.trim().is_empty() {
                lines.push(l);
            }
        }
    }
    lines
}

/// Scan for hunk-start lines (@@ or diff --git) and return their
/// row numbers.
fn hunk_lines(store: &BufferStoreHandle, buffer_id: BufferId) -> Vec<u32> {
    let Some(h) = store.handle_for(buffer_id) else {
        return vec![];
    };
    let snap = h.snapshot();
    let mut lines = Vec::new();
    for l in 0..snap.buffer.line_count() as u32 {
        if let Some(t) = snap.buffer.line(l) {
            let t = t.trim();
            if t.starts_with("@@") || t.starts_with("diff --git") {
                lines.push(l);
            }
        }
    }
    lines
}

// ── MG.18c: hunk-level staging ──────────────────────────
//
// The resolution lives here, not in each view, for the reason
// `]c` / `[c` above live here: a hunk is a property of diff *text*,
// identical in every magit buffer, so one implementation serves
// magit-status's inline diffs, magit-diff's buffer, and whatever
// binds `s` next. What genuinely differs per view — which file a
// non-hunk line names, and which tree the text was diffed against —
// stays behind `MagitView`.

/// The hunk under `cursor` in the buffer an action fired in, read
/// straight from the buffer text (`magit.md` §7.5's precedent: `]c`
/// and `[c` derive hunk boundaries the same way, so navigation and
/// staging cannot disagree about where a hunk begins).
///
/// Reads through `hunk_at_with`'s accessor rather than materialising
/// the buffer: a `*magit:diff*` against a large change is tens of
/// thousands of lines, and staging one hunk must not copy all of them.
pub(crate) fn hunk_at_cursor(
    store: &BufferStoreHandle,
    buffer_id: BufferId,
    cursor: u32,
) -> Option<crate::hunk::HunkPatch> {
    let handle = store.handle_for(buffer_id)?;
    let snap = handle.snapshot();
    crate::hunk::hunk_at_with(
        |i| u32::try_from(i).ok().and_then(|l| snap.buffer.line(l)),
        cursor as usize,
    )
}

/// What `s` / `u` / `x` do to a resolved hunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HunkOp {
    /// `s` — apply the unstaged hunk forward into the index.
    Stage,
    /// `u` — reverse the staged hunk back out of the index.
    Unstage,
    /// `x` — reverse the unstaged hunk out of the working tree.
    Discard,
}

impl HunkOp {
    /// MG.18e: which side of the patch the target already holds, which
    /// is what the region rewrite needs to know. The same fact
    /// [`Self::apply_flags`]' `reverse` encodes, named for the rewrite
    /// rather than for git's argv so the two cannot drift apart.
    fn direction(self) -> crate::hunk::ApplyDirection {
        match self {
            HunkOp::Stage => crate::hunk::ApplyDirection::Forward,
            HunkOp::Unstage | HunkOp::Discard => crate::hunk::ApplyDirection::Reverse,
        }
    }

    /// `(cached, reverse)` for `Index::apply_patch`.
    fn apply_flags(self) -> (bool, bool) {
        match self {
            HunkOp::Stage => (true, false),
            HunkOp::Unstage => (true, true),
            // The worktree, not the index — matching file-level `x`,
            // which is `git checkout -- <path>` and likewise leaves
            // the index alone.
            HunkOp::Discard => (false, true),
        }
    }

    /// The only [`DiffSource`] this operation can act on. A hunk from
    /// the other side is refused rather than handed to git, whose
    /// "patch does not apply" says nothing about which key to press.
    fn requires(self) -> DiffSource {
        match self {
            HunkOp::Stage | HunkOp::Discard => DiffSource::Unstaged,
            HunkOp::Unstage => DiffSource::Staged,
        }
    }

    fn present(self) -> &'static str {
        match self {
            HunkOp::Stage => "stage",
            HunkOp::Unstage => "unstage",
            HunkOp::Discard => "discard",
        }
    }

    fn past(self) -> &'static str {
        match self {
            HunkOp::Stage => "staged",
            HunkOp::Unstage => "unstaged",
            HunkOp::Discard => "discarded",
        }
    }

    /// Why a hunk from the wrong side cannot be acted on, phrased as
    /// what to do instead.
    fn wrong_source_hint(self) -> &'static str {
        match self {
            HunkOp::Stage => "that hunk is already staged",
            HunkOp::Unstage => "that hunk isn't staged",
            HunkOp::Discard => "that hunk is staged — unstage it with `u` first",
        }
    }
}

/// What the cursor resolved to for a hunk-level operation.
pub(crate) enum HunkResolution {
    /// Not inside a hunk. The caller runs its file-level path
    /// unchanged — this is what keeps every pre-MG.18c behaviour.
    FileLevel,
    /// Inside a hunk this operation cannot act on. Carries the
    /// explanation; the file-level path must NOT run, or `s` would
    /// silently stage the whole file the user was inspecting a hunk of.
    Refused(Effect),
    Ready {
        view: Arc<dyn crate::buffer_state::MagitView>,
        workdir: std::path::PathBuf,
        patch: crate::hunk::HunkPatch,
        /// MG.18d: where to put the cursor once the rebuild lands.
        /// `None` when the hunk's own header names no file — nothing to
        /// find again, so the refresh leaves the cursor alone.
        site: Option<crate::cursor_restore::HunkSite>,
        /// MG.18e: how many changed lines a Visual-mode region selected,
        /// or `None` for a whole hunk. Named in the echo and the discard
        /// prompt so a selection that reached past this hunk reads as
        /// what it did, not as what the user drew.
        region_lines: Option<usize>,
    },
}

/// MG.18e: what the active region did to the hunk under the cursor.
enum RegionOutcome {
    /// No region, or one that covers the whole hunk — the unrestricted
    /// patch, byte-identical to what a Normal-mode press produces.
    Whole,
    /// The region selected some of the hunk's changes.
    Restricted(crate::hunk::HunkPatch),
    /// The region is inside the hunk but holds no `+`/`-` line, so
    /// there is nothing to move.
    Empty,
}

/// Narrow `whole` to the active region, if there is one.
///
/// The region is intersected with the hunk under the cursor: rows
/// outside it belong to other hunks or other entries, and a selection
/// that reaches past this hunk acts on the part inside it. That is a
/// deliberate one-hunk-at-a-time limit — magit's own region can span
/// hunks, which needs a multi-hunk patch builder, so the echo names the
/// hunk it acted on rather than implying it did more.
fn region_of(whole: &crate::hunk::HunkPatch, ctx: &ActionContext<'_>, op: HunkOp) -> RegionOutcome {
    let Some(region) = ctx.selection else {
        return RegionOutcome::Whole;
    };
    let rows = region.start.line as usize..=region.end.line as usize;
    // A region covering the hunk end-to-end is not a special case: the
    // rewrite reproduces the whole patch. Short-circuiting it keeps the
    // verbatim header (and its round-trip proof) on the common path.
    if *rows.start() <= whole.header_line + 1 && *rows.end() >= whole.end_line.saturating_sub(1) {
        return RegionOutcome::Whole;
    }
    match whole.restrict_to_rows(rows, op.direction()) {
        Some(patch) => RegionOutcome::Restricted(patch),
        None => RegionOutcome::Empty,
    }
}

fn echo(text: String) -> Effect {
    Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text,
    }
}

/// MG.18d: name the work this hunk is, so the rebuilt buffer can be
/// searched for it. File + side + ordinal — deliberately not a row,
/// which the rebuild invalidates.
fn hunk_site(
    store: &BufferStoreHandle,
    buffer_id: BufferId,
    patch: &crate::hunk::HunkPatch,
    source: DiffSource,
) -> Option<crate::cursor_restore::HunkSite> {
    let path = std::path::PathBuf::from(patch.file_path()?);
    let handle = store.handle_for(buffer_id)?;
    let snap = handle.snapshot();
    let ordinal = crate::hunk::hunk_ordinal_at(
        |i| u32::try_from(i).ok().and_then(|l| snap.buffer.line(l)),
        patch.header_line,
    );
    Some(crate::cursor_restore::HunkSite {
        path,
        staged: source == DiffSource::Staged,
        ordinal,
    })
}

/// Resolve the hunk at the cursor for `op`, per magit-hunk-staging.md
/// §"Resolution order: hunk, then file".
pub(crate) fn resolve_hunk(ctx: &ActionContext<'_>, op: HunkOp) -> HunkResolution {
    let (Some(store), Some(view)) = (
        ctx.services.get::<BufferStoreHandle>(),
        crate::buffer_state::view_for(ctx),
    ) else {
        return HunkResolution::FileLevel;
    };
    let buffer_id = BufferId(ctx.buffer_id.0 as u32);
    let Some(whole) = hunk_at_cursor(&store, buffer_id, ctx.cursor.line) else {
        return HunkResolution::FileLevel;
    };
    // MG.18e: a Visual-mode selection narrows the hunk to the lines it
    // covers. Resolved BEFORE the source gate so "nothing selectable
    // there" is answered ahead of "wrong side" — the user picked those
    // rows deliberately, and telling them the selection was empty is
    // more useful than a staged/unstaged lecture.
    let (patch, region_lines) = match region_of(&whole, ctx, op) {
        RegionOutcome::Whole => (whole, None),
        RegionOutcome::Restricted(patch) => {
            // Every `+`/`-` still carrying its marker is a selected
            // change: the rewrite contextualised or dropped the rest.
            let lines = patch
                .hunk
                .iter()
                .skip(1)
                .filter(|l| l.starts_with('+') || l.starts_with('-'))
                .count();
            (patch, Some(lines))
        }
        RegionOutcome::Empty => {
            return HunkResolution::Refused(echo(format!(
                "magit: nothing to {} in the selection — it holds no added or removed lines",
                op.present()
            )));
        }
    };
    let hint = match view.diff_source(ctx.cursor) {
        Some(source) if source == op.requires() => {
            return match view.workdir() {
                Some(workdir) => HunkResolution::Ready {
                    site: hunk_site(&store, buffer_id, &patch, source),
                    view,
                    workdir,
                    patch,
                    region_lines,
                },
                // A view that stages but cannot name its repository is
                // a wiring bug, not a user error; decline rather than
                // guess at a working directory.
                None => HunkResolution::FileLevel,
            };
        }
        Some(_) => op.wrong_source_hint().to_string(),
        None => format!(
            "hunk-level staging isn't available in this view — move to the file header to {} the whole file",
            op.present()
        ),
    };
    HunkResolution::Refused(Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: format!("magit: {hint}"),
    })
}

/// Apply `patch` off the actor thread, then rebuild the view.
///
/// Returns immediately with the echo naming what is being done; the
/// git call has not started yet. Failure is reported the way every
/// other async magit mutation reports it — `tracing::error!`, which
/// the `MessagesLayer` fans into `*messages*` — and the refresh runs
/// either way, so a refused patch leaves the buffer showing the truth
/// rather than a state the user may believe they changed.
pub(crate) fn spawn_hunk_apply(
    view: Arc<dyn crate::buffer_state::MagitView>,
    workdir: std::path::PathBuf,
    patch: crate::hunk::HunkPatch,
    op: HunkOp,
    site: Option<crate::cursor_restore::HunkSite>,
    region_lines: Option<usize>,
) -> Effect {
    let location = patch.display_location();
    let text = patch.to_patch();
    let (cached, reverse) = op.apply_flags();
    let logged = location.clone();
    tokio::task::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let repo = lattice_vcs::Repository::discover(&workdir)
                .map_err(|e| format!("not a git repository: {e}"))?;
            lattice_vcs::Index::apply_patch(&repo, &text, cached, reverse)
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap_or_else(|e| Err(e.to_string()));
        if let Err(err) = result {
            // `git apply` refuses a patch whose context does not match
            // the target exactly — which is the safeguard, not a
            // malfunction: the buffer had drifted from the tree.
            tracing::error!(
                target: "lattice_magit",
                "magit: could not {} hunk at {logged}: {err}", op.present()
            );
        }
        // Both views drive their own async rebuild and return `None`;
        // there is no effect to propagate from inside a spawned task.
        //
        // MG.18d: the rebuild is also what puts the cursor back — it is
        // the only thing that knows the new text, so the restore rides
        // with it rather than racing it from here.
        let _ = match site {
            Some(site) => view.refresh_restoring(site),
            None => view.refresh(),
        };
    });
    announce(op, &location, region_lines)
}

/// What the user is told, and whether Visual mode ends.
///
/// Split out so both are testable without spawning the git call the
/// caller has already started.
fn announce(op: HunkOp, location: &str, region_lines: Option<usize>) -> Effect {
    let echo = Effect::Echo {
        level: lattice_grammar::EchoLevel::Info,
        text: match region_lines {
            // Name the count, not "the selection": a region that reached
            // past this hunk acted on the part inside it, and "3 lines"
            // says so where "the selection" would not.
            Some(1) => format!("magit: {} 1 line of {location}", op.past()),
            Some(n) => format!("magit: {} {n} lines of {location}", op.past()),
            None => format!("magit: {} hunk at {location}", op.past()),
        },
    };
    match region_lines {
        // Acting on a region consumes it, the way a Visual-mode operator
        // does in vim — staying selected would invite a second `s` over
        // rows whose meaning just changed under the refresh.
        Some(_) => Effect::Many(vec![
            Effect::EnterMode(lattice_grammar::ModalState::Normal),
            echo,
        ]),
        None => echo,
    }
}

/// The `s` / `u` handler body: hunk first, then the view's file-level
/// path. `x` runs the same resolution through its confirm pair in
/// `actions.rs`.
fn stage_or_unstage(ctx: &ActionContext<'_>, op: HunkOp) -> Option<Effect> {
    match resolve_hunk(ctx, op) {
        HunkResolution::Ready {
            view,
            workdir,
            patch,
            site,
            region_lines,
        } => Some(spawn_hunk_apply(
            view,
            workdir,
            patch,
            op,
            site,
            region_lines,
        )),
        HunkResolution::Refused(effect) => Some(effect),
        HunkResolution::FileLevel => {
            let view = crate::buffer_state::view_for(ctx)?;
            match op {
                HunkOp::Stage => view.stage(ctx.cursor),
                HunkOp::Unstage => view.unstage(ctx.cursor),
                HunkOp::Discard => None,
            }
        }
    }
}

/// Walk `items` forward from `cursor_row` and return the first
/// item strictly greater. Wraps to the first item if none found.
fn next_item(items: &[u32], cursor_row: u32) -> Option<u32> {
    items
        .iter()
        .copied()
        .find(|&r| r > cursor_row)
        .or_else(|| items.first().copied())
}

/// Walk `items` backward from `cursor_row` and return the first
/// item strictly less. Wraps to the last item if none found.
fn prev_item(items: &[u32], cursor_row: u32) -> Option<u32> {
    items
        .iter()
        .rev()
        .copied()
        .find(|&r| r < cursor_row)
        .or_else(|| items.last().copied())
}

impl Mode for MagitCoreMode {
    type Guard = ActionRegsGuard;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Majors(vec![
            MagitStatusMode::mode_id(),
            MagitCommitMode::mode_id(),
            MagitDiffMode::mode_id(),
            MagitLogMode::mode_id(),
            MagitBlameMode::mode_id(),
            MagitStashMode::mode_id(),
            MagitBranchMode::mode_id(),
            MagitRebaseMode::mode_id(),
            MagitRevisionMode::mode_id(),
            MagitFileRevisionMode::mode_id(),
            crate::magit_stash_show_mode::MagitStashShowMode::mode_id(),
        ])
    }

    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::new()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_core_keymap_entries())
    }

    /// MG.13: every magit-core chord, registered once at boot.
    ///
    /// None of these need per-buffer state — they read the buffer
    /// through `BufferStoreHandle` using `ctx.buffer_id`, so they are
    /// pure functions of the `ActionContext`. That matters twice over:
    /// this mode is a *minor* active on **every** magit buffer, so
    /// per-activation registration meant N registrations of the same
    /// action id with two magit buffers open — last-wins, and the first
    /// deactivation unregistering the chord for both.
    ///
    /// `gr`, `s` and `u` are the shared actions: `gr` is bound here,
    /// while `s`/`u` are bound by `magit-status-mode` and
    /// `magit-diff-mode`. Either way the *handler* must exist exactly
    /// once, so all three live here and dispatch per-buffer through
    /// `MagitView`. The binding still belongs to whichever mode offers
    /// the chord — a buffer whose mode does not bind `s` never routes
    /// one here.
    fn action_handlers(&self) -> Vec<lattice_mode::ActionHandlerContribution> {
        use crate::buffer_state::view_for;

        /// Read the buffer this action fired in. No per-buffer state
        /// needed — the store is a service and the buffer comes from
        /// the `ActionContext`.
        fn store_and_buffer(ctx: &ActionContext<'_>) -> Option<(Arc<BufferStoreHandle>, BufferId)> {
            let store = ctx.services.get::<BufferStoreHandle>()?;
            Some((store, BufferId(ctx.buffer_id.0 as u32)))
        }

        macro_rules! nav {
            ($name:literal, $lines:ident, $step:ident) => {
                lattice_mode::ActionHandlerContribution {
                    action_name: $name,
                    handler: Arc::new(|ctx: &ActionContext<'_>| {
                        let (store, buffer_id) = store_and_buffer(ctx)?;
                        let items = $lines(&store, buffer_id);
                        Some(cursor_at($step(&items, ctx.cursor.line)?))
                    }),
                }
            };
        }

        vec![
            // ── shared actions: one handler, per-view body ──────
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-refresh",
                handler: Arc::new(|ctx: &ActionContext<'_>| view_for(ctx)?.refresh()),
            },
            // MG.20: one handler per operation, each resolving its
            // target through the view — the same shape `gr` / `s` / `u`
            // use. A view with no commit under the cursor declines, so
            // pressing `V` in a branch list does nothing rather than
            // acting on something arbitrary.
            commit_op(
                "action:magit-cherry-pick",
                crate::magit_global_mode::CommitOp::CHERRY_PICK,
            ),
            commit_op(
                "action:magit-revert",
                crate::magit_global_mode::CommitOp::REVERT,
            ),
            commit_op(
                "action:magit-reset-soft",
                crate::magit_global_mode::CommitOp::RESET_SOFT,
            ),
            commit_op(
                "action:magit-reset-mixed",
                crate::magit_global_mode::CommitOp::RESET_MIXED,
            ),
            commit_op(
                "action:magit-reset-hard",
                crate::magit_global_mode::CommitOp::RESET_HARD,
            ),
            // The execute half of reset --hard, reached only through
            // its confirm. Re-resolves the commit at the cursor rather
            // than carrying it through the prompt: the confirm
            // transient owns every keystroke while open, so the cursor
            // cannot have moved (same argument as branch-delete).
            commit_op_execute(
                "action:magit-reset-hard-execute",
                crate::magit_global_mode::CommitOp::RESET_HARD,
            ),
            // MG.18c: hunk-at-cursor first, the view's file-level path
            // second. The hunk half is identical in every magit
            // buffer, so it resolves here; only the fallback is
            // per-view.
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-stage",
                handler: Arc::new(|ctx: &ActionContext<'_>| stage_or_unstage(ctx, HunkOp::Stage)),
            },
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-unstage",
                handler: Arc::new(|ctx: &ActionContext<'_>| stage_or_unstage(ctx, HunkOp::Unstage)),
            },
            // ── close (q) ─────────────────────────────────
            // Bug fix: this used to return `Effect::QuitEditor { scope:
            // Pane, .. }` — vim's `:q` semantics ("close the pane; if
            // it's the last one, quit the editor"). With magit buffers
            // opened IN PLACE in the current pane (not a split), `:q`
            // semantics on the only pane open QUIT THE WHOLE EDITOR —
            // the exact live-reported bug. magit's `q` means "bury this
            // buffer" (Emacs `bury-buffer` / vim alternate-buffer), not
            // "close a window" — it must never risk quitting. Fixed by
            // returning `Effect::DismissPopup`, which restores the
            // pane's pre-open buffer/cursor/scroll from
            // `Editor::prev_pane_for_popup` without touching the
            // editor's pane count at all.
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-close",
                // `Effect::BuryBuffer`, not `DismissPopup`: a magit view
                // is a full-pane buffer, not a popup. Opening one swaps
                // the pane AND the editor's active-document handle;
                // dismissing a popup only drops an overlay, so it left
                // the document pointing at magit while the pane pointed
                // at the file — the pane named one buffer and the screen
                // painted another, and no redraw could fix it because
                // the data was stale, not the paint.
                handler: Arc::new(|_ctx: &ActionContext<'_>| Some(Effect::BuryBuffer)),
            },
            // ── navigation: ]] [[ ]f [f ]c [c ────────────
            nav!("action:magit-next-section", section_headers, next_item),
            nav!("action:magit-prev-section", section_headers, prev_item),
            nav!("action:magit-next-file", entry_lines, next_item),
            nav!("action:magit-prev-file", entry_lines, prev_item),
            nav!("action:magit-next-hunk", hunk_lines, next_item),
            nav!("action:magit-prev-hunk", hunk_lines, prev_item),
            // TAB — toggle the fold at cursor (per-entry/per-hunk,
            // per `MagitStatusFoldSource`'s nested ranges).
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-toggle-fold",
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(Effect::AppAction(AppEffect::ToggleFoldAtCursor))
                }),
            },
            // S-TAB — cycle overview / all-headings / everything-shown,
            // matching magit's own section-cycling convention.
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-cycle-sections",
                handler: Arc::new(|_ctx: &ActionContext<'_>| {
                    Some(Effect::AppAction(AppEffect::CycleFoldsGlobal))
                }),
            },
        ]
    }

    /// MG.13: nothing to do per activation — every chord this mode
    /// contributes is registered at boot by `action_handlers()`. The
    /// Guard is empty; it exists only to satisfy the lifecycle
    /// contract (a fresh Guard per activation).
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move { Ok(ActionRegsGuard::default()) })
    }
}

/// MG.18c — the `s` / `u` / `x` resolution ladder, exercised through a
/// real buffer and a published view.
///
/// The unit tests in `hunk.rs` prove the parser; these prove the
/// *wiring*, which is where this crate's history says the bugs live
/// (MG.13's handler race, MG.15's dead stash chords). Each case builds
/// the same `ActionContext` shape production dispatch builds.
#[cfg(test)]
mod hunk_staging {
    use super::*;
    use crate::buffer_state::{MagitView, MagitViews, MagitViewsHandle};
    use lattice_mode::{BufferStore, ServiceRegistry};

    const DIFF: &str = "\
diff --git a/a.txt b/a.txt
index 111..222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 keep
-old
+new
  modified src/other.rs
";
    /// Row 6 is `+new` — inside the hunk. Row 8 is the status entry
    /// below it, where staging must stay file-level.
    const IN_HUNK: u32 = 6;
    const BELOW_HUNK: u32 = 8;

    struct OneBufferStore {
        id: lattice_core::BufferId,
        doc: Arc<dyn lattice_runtime::Document>,
    }

    impl BufferStore for OneBufferStore {
        fn find_by_name(&self, _name: &str) -> Option<lattice_core::BufferId> {
            None
        }
        fn name_for(&self, _id: lattice_core::BufferId) -> Option<String> {
            None
        }
        fn handle_for(
            &self,
            id: lattice_core::BufferId,
        ) -> Option<Arc<dyn lattice_runtime::Document>> {
            (id == self.id).then(|| self.doc.clone())
        }
        fn insert_document_buffer(
            &self,
            _id: lattice_core::BufferId,
            _kind: lattice_core::BufferKind,
            _handle: Arc<dyn lattice_runtime::Document>,
            _flags: lattice_core::BufferFlags,
            _name: Option<String>,
        ) {
        }
    }

    /// A view that answers only what the ladder asks it.
    struct StubView(Option<DiffSource>);

    impl MagitView for StubView {
        fn refresh(&self) -> Option<Effect> {
            None
        }
        fn diff_source(&self, _cursor: Position) -> Option<DiffSource> {
            self.0
        }
        fn workdir(&self) -> Option<std::path::PathBuf> {
            Some(std::path::PathBuf::from("/tmp/repo"))
        }
    }

    fn services_for(
        text: &str,
        source: Option<DiffSource>,
    ) -> (ServiceRegistry, lattice_core::BufferId) {
        let id = lattice_core::BufferId::next();
        let registry: lattice_grammar::CommandRegistryHandle = Arc::new(
            arc_swap::ArcSwap::from_pointee(lattice_grammar::CommandRegistry::new()),
        );
        let doc: Arc<dyn lattice_runtime::Document> = Arc::new(lattice_runtime::spawn_document(
            id,
            lattice_core::Document::from_text(text),
            registry,
        ));
        let store: Arc<dyn BufferStore> = Arc::new(OneBufferStore { id, doc });
        let views: MagitViewsHandle = Arc::new(MagitViews::default());
        views.publish(id, Arc::new(StubView(source)));
        let mut services = ServiceRegistry::new();
        services.register(BufferStoreHandle::new(store));
        services.register(views);
        (services, id)
    }

    /// Run the ladder the way a chord press does.
    fn resolve(cursor_line: u32, source: Option<DiffSource>, op: HunkOp) -> HunkResolution {
        resolve_with_region(cursor_line, None, source, op)
    }

    /// MG.18e: the same, with a Visual-mode region live — `rows` is the
    /// inclusive buffer-row span the selection covers.
    fn resolve_with_region(
        cursor_line: u32,
        rows: Option<(u32, u32)>,
        source: Option<DiffSource>,
        op: HunkOp,
    ) -> HunkResolution {
        let (services, id) = services_for(DIFF, source);
        let events = lattice_runtime::EventBus::new();
        let ctx = ActionContext {
            buffer_id: lattice_protocol::ids::BufferId::new(id.0 as u64),
            cursor: Position::new(cursor_line, 0),
            selection: rows.map(|(a, b)| {
                lattice_protocol::position::Range::new(Position::new(a, 0), Position::new(b, 0))
            }),
            services: &services,
            events: &events,
            prompt_value: None,
            args: lattice_grammar::Args::None,
        };
        resolve_hunk(&ctx, op)
    }

    fn refusal_text(r: HunkResolution) -> String {
        match r {
            HunkResolution::Refused(Effect::Echo { text, .. }) => text,
            HunkResolution::Refused(other) => panic!("expected an Echo, got {other:?}"),
            HunkResolution::Ready { .. } => panic!("expected a refusal, got Ready"),
            HunkResolution::FileLevel => panic!("expected a refusal, got FileLevel"),
        }
    }

    #[test]
    fn s_on_an_unstaged_hunk_builds_that_hunks_patch() {
        match resolve(IN_HUNK, Some(DiffSource::Unstaged), HunkOp::Stage) {
            HunkResolution::Ready { patch, workdir, .. } => {
                assert_eq!(workdir, std::path::PathBuf::from("/tmp/repo"));
                let text = patch.to_patch();
                assert!(text.starts_with("diff --git a/a.txt b/a.txt\n"), "{text}");
                assert!(text.contains("+new"), "{text}");
                assert!(
                    !text.contains("modified src/other.rs"),
                    "the status entry below the diff must not reach the patch:\n{text}"
                );
            }
            other => panic!("expected Ready, got {}", label(&other)),
        }
    }

    /// The file-level path is what every pre-MG.18c press did, and it
    /// must survive: a cursor on an entry line is not in a hunk.
    #[test]
    fn a_cursor_below_the_diff_falls_through_to_file_level() {
        assert!(matches!(
            resolve(BELOW_HUNK, Some(DiffSource::Unstaged), HunkOp::Stage),
            HunkResolution::FileLevel
        ));
    }

    /// Pressing `u` on an unstaged hunk would hand git a patch it
    /// refuses. Saying so beats `error: patch does not apply`.
    #[test]
    fn u_on_an_unstaged_hunk_is_refused_with_a_reason() {
        let text = refusal_text(resolve(IN_HUNK, Some(DiffSource::Staged), HunkOp::Stage));
        assert!(text.contains("already staged"), "{text}");
        let text = refusal_text(resolve(
            IN_HUNK,
            Some(DiffSource::Unstaged),
            HunkOp::Unstage,
        ));
        assert!(text.contains("isn't staged"), "{text}");
    }

    /// The destructive one. `x` on a staged hunk must not reverse it
    /// out of the worktree while leaving it in the index — the change
    /// would vanish from the file and still be committed by `cc`.
    #[test]
    fn x_on_a_staged_hunk_refuses_rather_than_half_discarding() {
        let text = refusal_text(resolve(IN_HUNK, Some(DiffSource::Staged), HunkOp::Discard));
        assert!(
            text.contains("unstage it with `u` first"),
            "the refusal must say what to do instead: {text}"
        );
    }

    /// `*magit:diff*` (against HEAD) mixes both sides into one hunk,
    /// and a commit's inline patch in magit-status belongs to neither
    /// tree. Refusing beats falling through — falling through would
    /// stage the WHOLE FILE from a keypress aimed at one hunk.
    #[test]
    fn an_unclassifiable_diff_refuses_hunk_staging_instead_of_staging_the_file() {
        let text = refusal_text(resolve(IN_HUNK, None, HunkOp::Stage));
        assert!(text.contains("isn't available in this view"), "{text}");
        assert!(
            text.contains("file header"),
            "and must point at the way to stage the file deliberately: {text}"
        );
    }

    // ── MG.18e: the region path through a real buffer ──
    //
    // `DIFF`'s body is row 5 ` keep`, row 6 `-old`, row 7 `+new`.

    /// A region over one changed line narrows the patch to it and
    /// reports the count, so the echo cannot imply more than happened.
    #[test]
    fn a_region_over_one_line_narrows_the_patch_and_counts_it() {
        match resolve_with_region(7, Some((7, 7)), Some(DiffSource::Unstaged), HunkOp::Stage) {
            HunkResolution::Ready {
                patch,
                region_lines,
                ..
            } => {
                assert_eq!(region_lines, Some(1), "one changed line selected");
                let text = patch.to_patch();
                assert!(text.contains("+new"), "{text}");
                assert!(
                    text.contains(" old"),
                    "the unselected removal became context, not a deletion:\n{text}"
                );
                assert!(
                    !text.contains("-old"),
                    "and must NOT still be a removal:\n{text}"
                );
            }
            other => panic!("expected Ready, got {}", label(&other)),
        }
    }

    /// A region covering the whole body is not a special case — it must
    /// produce the identical whole-hunk patch, with no region reported,
    /// so `V` over a hunk and a bare `s` on it cannot diverge.
    #[test]
    fn a_region_covering_the_whole_hunk_is_the_whole_hunk() {
        let whole = match resolve(6, Some(DiffSource::Unstaged), HunkOp::Stage) {
            HunkResolution::Ready { patch, .. } => patch.to_patch(),
            other => panic!("expected Ready, got {}", label(&other)),
        };
        match resolve_with_region(6, Some((5, 7)), Some(DiffSource::Unstaged), HunkOp::Stage) {
            HunkResolution::Ready {
                patch,
                region_lines,
                ..
            } => {
                assert_eq!(patch.to_patch(), whole);
                assert_eq!(
                    region_lines, None,
                    "no region to announce — this IS the hunk"
                );
            }
            other => panic!("expected Ready, got {}", label(&other)),
        }
    }

    /// Selecting only context is refused with a reason, not handed to
    /// git as a patch that does nothing.
    #[test]
    fn a_region_holding_only_context_is_refused() {
        let text = refusal_text(resolve_with_region(
            5,
            Some((5, 5)),
            Some(DiffSource::Unstaged),
            HunkOp::Stage,
        ));
        assert!(text.contains("nothing to stage in the selection"), "{text}");
    }

    /// The refusal for an empty selection comes BEFORE the staged/unstaged
    /// gate: the user picked those rows deliberately, and "there is
    /// nothing there" is more useful than a lecture about which side of
    /// the index they are on.
    #[test]
    fn an_empty_region_is_answered_before_the_source_gate() {
        let text = refusal_text(resolve_with_region(
            5,
            Some((5, 5)),
            // Wrong side for `s` — which would normally refuse first.
            Some(DiffSource::Staged),
            HunkOp::Stage,
        ));
        assert!(
            text.contains("nothing to stage in the selection"),
            "the selection is answered first: {text}"
        );
    }

    /// A region outside the hunk entirely leaves nothing selected inside
    /// it, so the operation declines rather than silently acting on the
    /// whole hunk.
    #[test]
    fn a_region_that_misses_the_hunk_body_is_refused() {
        let text = refusal_text(resolve_with_region(
            6,
            // Rows 0..=2 are the `diff --git` / `index` / `---` header.
            Some((0, 2)),
            Some(DiffSource::Unstaged),
            HunkOp::Stage,
        ));
        assert!(text.contains("nothing to stage in the selection"), "{text}");
    }

    /// A region action ends Visual mode, like any vim operator on a
    /// selection — and the echo says how many lines moved.
    #[test]
    fn acting_on_a_region_leaves_visual_mode_and_names_the_count() {
        match announce(HunkOp::Stage, "a.txt:1", Some(2)) {
            Effect::Many(parts) => {
                assert!(
                    matches!(
                        parts.first(),
                        Some(Effect::EnterMode(lattice_grammar::ModalState::Normal))
                    ),
                    "Visual ends first, so the echo is what the user is left looking at"
                );
                match parts.get(1) {
                    Some(Effect::Echo { text, .. }) => {
                        assert!(text.contains("staged 2 lines of a.txt:1"), "{text}")
                    }
                    other => panic!("expected an Echo, got {other:?}"),
                }
            }
            other => panic!("expected Many, got {other:?}"),
        }
    }

    /// A whole-hunk press was never in Visual mode, so it must not emit
    /// a mode change — that would exit Visual for an unrelated reason if
    /// the user happened to be in it.
    #[test]
    fn a_whole_hunk_action_only_echoes() {
        match announce(HunkOp::Unstage, "a.txt:1", None) {
            Effect::Echo { text, .. } => {
                assert!(text.contains("unstaged hunk at a.txt:1"), "{text}")
            }
            other => panic!("expected a bare Echo, got {other:?}"),
        }
    }

    /// One line reads as "1 line", not "1 lines".
    #[test]
    fn a_single_line_region_is_announced_in_the_singular() {
        match announce(HunkOp::Discard, "a.txt:9", Some(1)) {
            Effect::Many(parts) => match parts.get(1) {
                Some(Effect::Echo { text, .. }) => {
                    assert!(text.contains("discarded 1 line of"), "{text}")
                }
                other => panic!("expected an Echo, got {other:?}"),
            },
            other => panic!("expected Many, got {other:?}"),
        }
    }

    fn label(r: &HunkResolution) -> &'static str {
        match r {
            HunkResolution::Ready { .. } => "Ready",
            HunkResolution::Refused(_) => "Refused",
            HunkResolution::FileLevel => "FileLevel",
        }
    }

    /// The flag table is the whole safety contract of the three
    /// operations: a wrong pair silently mutates the wrong tree.
    #[test]
    fn the_apply_flag_table_is_the_documented_one() {
        assert_eq!(HunkOp::Stage.apply_flags(), (true, false), "index, forward");
        assert_eq!(
            HunkOp::Unstage.apply_flags(),
            (true, true),
            "index, reversed"
        );
        assert_eq!(
            HunkOp::Discard.apply_flags(),
            (false, true),
            "the WORKTREE reversed — `--cached` here would discard from the index instead, \
             leaving the worktree edit in place and staging its removal"
        );
    }
}

/// MG.18c — the discard flags against real git.
///
/// `hunk.rs`'s round-trips prove the *patch* is one git accepts for
/// stage and unstage. This proves the third pairing, which is the one
/// with no second chance: `x` must reverse the hunk out of the
/// **working tree** and leave the index alone. `--cached` here would
/// stage the removal instead, which reads on screen as the discard
/// having worked while the change is still queued for the next commit.
#[cfg(test)]
mod discard_round_trip {
    use super::*;
    use std::process::Command;

    fn git_ok(dir: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(st.success(), "git {args:?} failed");
    }

    fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn discard_reverses_the_hunk_out_of_the_worktree_and_leaves_the_index_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path();
        git_ok(p, &["init"]);
        git_ok(p, &["config", "user.email", "t@lattice.dev"]);
        git_ok(p, &["config", "user.name", "lattice-test"]);
        let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        std::fs::write(p.join("a.txt"), &base).unwrap();
        git_ok(p, &["add", "a.txt"]);
        git_ok(p, &["commit", "-m", "base"]);
        // Two changes far enough apart that git reports two hunks.
        let edited: String = (1..=20)
            .map(|i| match i {
                2 => "line 2 EDITED\n".to_string(),
                19 => "line 19 EDITED\n".to_string(),
                _ => format!("line {i}\n"),
            })
            .collect();
        std::fs::write(p.join("a.txt"), &edited).unwrap();

        let diff = git_out(p, &["diff", "--", "a.txt"]);
        let lines: Vec<&str> = diff.lines().collect();
        let first_hunk = lines
            .iter()
            .position(|l| l.starts_with("@@ "))
            .expect("a hunk header");
        let patch =
            crate::hunk::hunk_at_with(|i| lines.get(i).map(|l| (*l).to_string()), first_hunk + 1)
                .expect("cursor inside hunk 1")
                .to_patch();

        let (cached, reverse) = HunkOp::Discard.apply_flags();
        let repo = lattice_vcs::Repository::discover(p).expect("discover");
        lattice_vcs::Index::apply_patch(&repo, &patch, cached, reverse).expect("discard applies");

        let worktree = std::fs::read_to_string(p.join("a.txt")).unwrap();
        assert!(
            !worktree.contains("line 2 EDITED"),
            "the discarded hunk is gone from the file:\n{worktree}"
        );
        assert!(
            worktree.contains("line 19 EDITED"),
            "the neighbouring hunk survives:\n{worktree}"
        );
        assert_eq!(
            git_out(p, &["diff", "--cached", "--name-only"]).trim(),
            "",
            "discard must not touch the index"
        );
    }
}
