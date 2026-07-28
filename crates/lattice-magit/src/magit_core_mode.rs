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
        ]
    })
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
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-stage",
                handler: Arc::new(|ctx: &ActionContext<'_>| view_for(ctx)?.stage(ctx.cursor)),
            },
            lattice_mode::ActionHandlerContribution {
                action_name: "action:magit-unstage",
                handler: Arc::new(|ctx: &ActionContext<'_>| view_for(ctx)?.unstage(ctx.cursor)),
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
                handler: Arc::new(|_ctx: &ActionContext<'_>| Some(Effect::DismissPopup)),
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
