//! MG.1: magit-core shared minor mode.
//!
//! Activates on magit buffers. Provides shared keymap with real
//! navigation handlers: ]]/[[ (sections), ]f/[f (files/entries),
//! ]c/[c (hunks). Each returns Effect::SelectionChange — the same
//! cursor-move primitive diff-mode uses for hunk navigation.

use std::sync::{Arc, OnceLock};

use lattice_core::BufferId;
use lattice_grammar::{CommandRegistryHandle, Effect, QuitScope};
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
use crate::magit_log_mode::MagitLogMode;
use crate::magit_rebase_mode::MagitRebaseMode;
use crate::magit_stash_mode::MagitStashMode;
use crate::magit_status_mode::MagitStatusMode;

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
            let t = t.trim();
            if t.starts_with("Staged changes")
                || t.starts_with("Unstaged changes")
                || t.starts_with("Untracked files")
                || t.starts_with("Stashes")
                || t.starts_with("Recent commits")
            {
                lines.push(l);
            }
        }
    }
    lines
}

/// Scan buffer for file/entry lines (indented, non-header).
fn entry_lines(store: &BufferStoreHandle, buffer_id: BufferId) -> Vec<u32> {
    let Some(h) = store.handle_for(buffer_id) else {
        return vec![];
    };
    let snap = h.snapshot();
    let mut lines = Vec::new();
    for l in 0..snap.buffer.line_count() as u32 {
        if let Some(t) = snap.buffer.line(l) {
            let t = t.trim();
            if t.starts_with("  ")
                && !t.is_empty()
                && !t.starts_with("Staged")
                && !t.starts_with("Unstaged")
                && !t.starts_with("Untracked")
                && !t.starts_with("Stashes")
                && !t.starts_with("Recent")
                && !t.starts_with("No changes")
            {
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
    type Guard = ();

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
                return Ok(());
            };

            let Some(cmd_arc) = ctx.service::<CommandRegistryHandle>() else {
                return Ok(());
            };
            let Some(ah_arc) = ctx.service::<ActionHandlerRegistryHandle>() else {
                return Ok(());
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
            h!("action:magit-close", move |_ctx: &ActionContext<'_>| {
                Some(Effect::QuitEditor {
                    force: false,
                    scope: QuitScope::Pane,
                })
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

            // TAB / S-TAB — fold engine handles these via Effect
            h!("action:magit-toggle-fold", move |_ctx: &ActionContext<
                '_,
            >| { None });
            h!(
                "action:magit-cycle-sections",
                move |_ctx: &ActionContext<'_>| { None }
            );

            std::mem::forget(regs);
            Ok(())
        })
    }
}
