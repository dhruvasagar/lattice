//! MG.1: magit-core shared minor mode.
//!
//! Activates on magit buffers. Provides shared keymap with real
//! navigation handlers: ]]/[[ (sections), ]f/[f (files/entries),
//! ]c/[c (hunks). Each returns Effect::SelectionChange — the same
//! cursor-move primitive diff-mode uses for hunk navigation.

use std::sync::{Arc, OnceLock};

use lattice_core::BufferId;
use lattice_grammar::{AppEffect, CommandRegistryHandle, Effect};
use lattice_mode::{
    ActionContext, ActionHandlerRegistryHandle, ActivationPolicy, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
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

/// Shared RAII guard for magit modes whose lifecycle only needs to own
/// a set of `ActionHandlerRegistration` tokens. `ActionHandlerRegistration`
/// unregisters itself on `Drop` (see `lattice_mode::action_handler_registry`),
/// so simply holding the `Vec` here — instead of `mem::forget`-leaking it,
/// as every non-status magit mode did before this fix — is enough to clean
/// up on buffer close. Without it, two buffers of the same major mode open
/// at once silently let the second's `on_activate` replace the first's
/// handler (registry is last-write-wins per `CommandId`), so firing the
/// chord in buffer A can execute buffer B's captured state against A's
/// cursor.
#[derive(Default)]
pub struct ActionRegsGuard(pub Vec<lattice_mode::ActionHandlerRegistration>);

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

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(ActionRegsGuard::default());
            };

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else {
                return Ok(ActionRegsGuard::default());
            };
            let registry = cmd_arc.load();
            let handlers = (*ah_arc).clone();
            let mut regs = Vec::new();

            macro_rules! h {
                ($name:expr, $body:expr) => {
                    if let Some(cid) = registry.id_by_name($name) {
                        regs.push(handlers.register(cid, Arc::new($body)));
                    }
                };
            }

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
            // `Editor::prev_pane_for_popup` (stashed by
            // `Editor::open_synthetic_buffer` — see its doc comment)
            // without touching the editor's pane count at all. Help
            // buffers already use this exact mechanism for their own
            // `q`/`<Esc>`, which is how it's known to never quit.
            h!("action:magit-close", move |_ctx: &ActionContext<'_>| {
                Some(Effect::DismissPopup)
            });

            // ── next-section (]]) ──────────────────────────
            {
                let s = store.clone();
                h!("action:magit-next-section", move |ctx: &ActionContext<
                    '_,
                >| {
                    let headers = section_headers(&s, buffer_id);
                    Some(cursor_at(next_item(&headers, ctx.cursor.line)?))
                });
            }

            // ── prev-section ([[) ──────────────────────────
            {
                let s = store.clone();
                h!("action:magit-prev-section", move |ctx: &ActionContext<
                    '_,
                >| {
                    let headers = section_headers(&s, buffer_id);
                    Some(cursor_at(prev_item(&headers, ctx.cursor.line)?))
                });
            }

            // ── next-file (]f) ────────────────────────────
            {
                let s = store.clone();
                h!("action:magit-next-file", move |ctx: &ActionContext<'_>| {
                    let entries = entry_lines(&s, buffer_id);
                    Some(cursor_at(next_item(&entries, ctx.cursor.line)?))
                });
            }

            // ── prev-file ([f) ────────────────────────────
            {
                let s = store.clone();
                h!("action:magit-prev-file", move |ctx: &ActionContext<'_>| {
                    let entries = entry_lines(&s, buffer_id);
                    Some(cursor_at(prev_item(&entries, ctx.cursor.line)?))
                });
            }

            // ── next-hunk (]c) ────────────────────────────
            {
                let s = store.clone();
                h!("action:magit-next-hunk", move |ctx: &ActionContext<'_>| {
                    let hunks = hunk_lines(&s, buffer_id);
                    Some(cursor_at(next_item(&hunks, ctx.cursor.line)?))
                });
            }

            // ── prev-hunk ([c) ────────────────────────────
            {
                let s = store.clone();
                h!("action:magit-prev-hunk", move |ctx: &ActionContext<'_>| {
                    let hunks = hunk_lines(&s, buffer_id);
                    Some(cursor_at(prev_item(&hunks, ctx.cursor.line)?))
                });
            }

            // TAB — toggle the fold at cursor (per-entry/per-hunk,
            // per `MagitStatusFoldSource`'s nested ranges).
            h!("action:magit-toggle-fold", move |_ctx: &ActionContext<
                '_,
            >| {
                Some(Effect::AppAction(AppEffect::ToggleFoldAtCursor))
            });
            // S-TAB — cycle overview / all-headings / everything-shown,
            // matching magit's own section-cycling convention.
            h!(
                "action:magit-cycle-sections",
                move |_ctx: &ActionContext<'_>| {
                    Some(Effect::AppAction(AppEffect::CycleFoldsGlobal))
                }
            );

            Ok(ActionRegsGuard(regs))
        })
    }
}
