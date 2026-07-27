//! Fold audit fix: nested fold ranges for magit-status inline
//! expansions.
//!
//! Mirrors [`lattice_diff::HunkFoldSource`]'s shape — an overlay
//! [`FoldSource`] that reads live state on every `compute_folds()`
//! call rather than caching stale line numbers, so it never
//! desyncs from concurrent edits. Emits two nested levels per
//! expanded entry (file/stash/commit): an outer fold spanning the
//! entry's header line through the end of its inserted patch (so
//! folding "the file" hides the whole diff, per-entry), and one
//! inner fold per `@@ ...@@` hunk within it (so folding a hunk
//! hides just its lines, and folding the outer range folds every
//! hunk with it — nesting is expressed the same way the generic
//! fold engine everywhere else expresses it: range containment).
//!
//! Registered by `MagitStatusMode::on_activate` via
//! `FoldOverlayServiceHandle`; `MagitStatusGuard::drop` removes it
//! (same Drop-based lifecycle `DiffModeGuard` and
//! `MultibufferModeGuard` use).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, Mutex};

use lattice_core::{BufferId, Fold, FoldSource, ProviderId};

use crate::actions::{StatusBufferState, classify_line, entry_key};

/// Namespace for per-buffer magit-status fold-source ids, OR'd with
/// the buffer's id so simultaneous magit-status buffers (unusual,
/// but not disallowed) register distinct overlay ids.
pub const MAGIT_STATUS_FOLD_NAMESPACE: u64 = 0x6A61_0001_0000_0000;

pub struct MagitStatusFoldSource {
    id: ProviderId,
    state: Arc<Mutex<StatusBufferState>>,
}

impl MagitStatusFoldSource {
    pub fn new(state: Arc<Mutex<StatusBufferState>>, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(MAGIT_STATUS_FOLD_NAMESPACE | buffer_id.0 as u64),
            state,
        }
    }
}

fn fold_identity(namespace: &str, start_line: u32) -> u64 {
    let mut h = DefaultHasher::new();
    namespace.hash(&mut h);
    start_line.hash(&mut h);
    h.finish()
}

impl FoldSource for MagitStatusFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }

    fn compute_folds(&self) -> Vec<Fold> {
        let Ok(g) = self.state.lock() else {
            return Vec::new();
        };
        if g.expanded.is_empty() {
            return Vec::new();
        }
        let Some(handle) = g.store.handle_for(g.buffer_id) else {
            return Vec::new();
        };
        let snap = handle.snapshot();
        let total = snap.buffer.line_count() as u32;
        let mut folds = Vec::new();
        let mut line = 0u32;
        while line < total {
            let Some(text) = snap.buffer.line(line) else {
                break;
            };
            if !text.starts_with("  ") {
                line += 1;
                continue;
            }
            // Every classified entry is exactly one line; if it's a
            // currently-expanded one, its patch occupies the next
            // `count` lines (an exact count from insertion time, not
            // a re-scanned guess) — fold that span, then jump past it
            // so the scan never has to disambiguate patch content
            // from real entries.
            if let Some(sl) = classify_line(&g, line) {
                let key = entry_key(&sl);
                if let Some(&count) = g.expanded.get(&key) {
                    if count > 0 {
                        let count = count as u32;
                        let body_start = line + 1;
                        let body_end = (line + count).min(total.saturating_sub(1));
                        folds.push(Fold {
                            start_line: line,
                            end_line: body_end,
                            closed: false,
                            identity: Some(fold_identity("magit:entry", line)),
                        });
                        folds.extend(hunk_folds(&snap, body_start, body_end));
                        line += 1 + count;
                        continue;
                    }
                }
            }
            line += 1;
        }
        folds
    }
}

/// One fold per `@@ ...@@` hunk within `[body_start, body_end]`
/// (inclusive), each spanning its header line through the line
/// before the next hunk header (or `body_end` for the last hunk).
/// Nested inside the caller's outer entry fold by construction —
/// `body_start`/`body_end` are that fold's interior.
fn hunk_folds(
    snap: &lattice_runtime::DocumentSnapshot,
    body_start: u32,
    body_end: u32,
) -> Vec<Fold> {
    let mut folds = Vec::new();
    let mut hunk_start: Option<u32> = None;
    for l in body_start..=body_end {
        let Some(text) = snap.buffer.line(l) else {
            break;
        };
        if text.starts_with("@@") {
            if let Some(start) = hunk_start {
                if l > start + 1 {
                    folds.push(Fold {
                        start_line: start,
                        end_line: l - 1,
                        closed: false,
                        identity: Some(fold_identity("magit:hunk", start)),
                    });
                }
            }
            hunk_start = Some(l);
        }
    }
    if let Some(start) = hunk_start {
        if body_end > start {
            folds.push(Fold {
                start_line: start,
                end_line: body_end,
                closed: false,
                identity: Some(fold_identity("magit:hunk", start)),
            });
        }
    }
    folds
}
