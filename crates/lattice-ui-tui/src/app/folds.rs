//! Code folding -- the App-side surface above
//! `crate::folds`. Vim's `z*` commands (zo / zc / za / zR /
//! zM / zd / zf), the `recompute_folds` provider dispatch
//! (manual / indent / markdown / syntax), and the
//! cursor-snap glue that keeps `j` / `k` / search hits from
//! landing inside hidden fold bodies.
//!
//! Methods that live here:
//! - `recompute_folds` (dispatcher across foldmethods),
//!   `recompute_syntax_folds` (private syntax-provider
//!   helper with markdown / indent fallback).
//! - `do_create_fold_from_visual` (`zf`), the
//!   `do_set_fold_state_at_cursor` family (`zo` / `zc` /
//!   `za`), `do_set_all_folds` (`zR` / `zM`),
//!   `do_goto_fold` (`zj` / `zk`),
//!   `do_delete_fold_at_cursor` (`zd`).
//! - Renderer-side queries: `line_inside_closed_fold`,
//!   `fold_start_at`, `fold_start_at_any`.
//! - Cursor maintenance: `snap_cursor_past_closed_folds`,
//!   `auto_open_folds_at_cursor`.
//! - Free fns: `compute_fold_hash` (cache key for
//!   refresh_highlights), `innermost_fold_idx`,
//!   `fold_to_close_at`, `outermost_fold_idx` (the
//!   selection rules behind zo / zc / za).
//!
//! What does NOT live here: the fold algorithm itself
//! (compute_indent_folds / compute_markdown_folds /
//! compute_syntax_folds in `crate::folds`), the `Fold`
//! struct (lives in `app.rs`), tree-sitter query plumbing
//! (`crate::syntax`).

use lattice_grammar::ModalState;
use lattice_protocol::position::Position;

use super::{
    App, EchoLevel, Fold, FoldMethod, is_blank_line, last_addressable_line, line_byte_len,
};

impl App {
    /// Refresh [`Self::folds`] from the active [`FoldMethod`].
    /// `manual` -- no-op (preserves user `zf` folds). The other
    /// providers (`indent` / `markdown` / `syntax`) replace `folds`
    /// with the recomputed set, preserving the closed/open state of
    /// any existing fold whose identity matches a recomputed one
    /// (so `zc` survives a reparse).
    ///
    /// `Syntax` runs the language's tree-sitter `folds.scm` query
    /// against the live parse tree and emits one fold per `@fold`
    /// capture spanning more than one line. When the buffer's
    /// language doesn't ship a `folds.scm` (or the parse tree
    /// hasn't been built yet), the syntax provider cascades to the
    /// markdown / indent providers based on the file extension --
    /// so `:set foldmethod=syntax` is useful even on a plain-text
    /// buffer.
    pub fn recompute_folds(&mut self) {
        let fm = self.foldmethod();
        if matches!(fm, FoldMethod::Manual) {
            return;
        }
        let snapshot = self.document.snapshot();
        let mut next = match fm {
            FoldMethod::Manual => return,
            FoldMethod::Indent => crate::folds::compute_indent_folds(&snapshot.buffer),
            FoldMethod::Markdown => crate::folds::compute_markdown_folds(&snapshot.buffer),
            FoldMethod::Syntax => self.recompute_syntax_folds(&snapshot.buffer),
        };
        // Carry over closed-state. Identity hash (heading text +
        // depth) is the primary key so that adding a line to one
        // section doesn't reopen the closed section above. Falls
        // back to (start_line, end_line) when identity is missing.
        for nf in next.iter_mut() {
            let prev = nf
                .identity
                .and_then(|id| self.folds.iter().find(|f| f.identity == Some(id)))
                .or_else(|| {
                    self.folds
                        .iter()
                        .find(|f| f.start_line == nf.start_line && f.end_line == nf.end_line)
                });
            if let Some(prev) = prev {
                nf.closed = prev.closed;
            }
        }
        // Manual folds (identity = None) coexist with computed
        // folds; recomputed providers don't produce them, so carry
        // them over verbatim.
        for prev in &self.folds {
            if prev.identity.is_none() {
                next.push(*prev);
            }
        }
        next.sort_by(|a, b| {
            a.start_line
                .cmp(&b.start_line)
                .then_with(|| b.end_line.cmp(&a.end_line))
        });
        self.folds = next;
    }

    /// Run the tree-sitter folds.scm provider against the live
    /// `Syntax`, falling back to markdown / indent when the syntax
    /// provider returns `None` (no `folds.scm` for this language,
    /// or no parse tree yet). Reads the latest snapshot from the
    /// async syntax handle (wait-free).
    fn recompute_syntax_folds(&self, buffer: &lattice_core::Buffer) -> Vec<Fold> {
        if let Some(syntax) = self.syntax.as_ref() {
            let snap = syntax.snapshot();
            if let Some(folds) = crate::folds::compute_syntax_folds(&snap) {
                return folds;
            }
        }
        // Cascade: markdown for `.md`, indent otherwise.
        let is_md = self
            .document
            .path()
            .map(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("md"))
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if is_md {
            crate::folds::compute_markdown_folds(buffer)
        } else {
            crate::folds::compute_indent_folds(buffer)
        }
    }

    /// Vim's `zf`: create a fold over the current Visual selection's
    /// line range. No-op outside Visual mode.
    pub(super) fn do_create_fold_from_visual(&mut self) {
        if !matches!(self.modal, ModalState::Visual(_)) {
            self.set_message(
                EchoLevel::Error,
                "zf requires a Visual selection".to_string(),
            );
            return;
        }
        let sels = self.document.selections();
        let sel = sels.primary();
        let start_line = sel.anchor.line.min(sel.head.line);
        let end_line = sel.anchor.line.max(sel.head.line);
        if start_line == end_line {
            return;
        }
        self.folds.push(Fold {
            start_line,
            end_line,
            closed: true,
            identity: None,
        });
        self.cursor = Position::new(start_line, 0);
        self.do_exit_visual();
    }

    /// Toggle / open / close the fold containing the cursor.
    /// `Some(true)` = `zc` close, `Some(false)` = `zo` open,
    /// `None` = `za` toggle. Selection rules:
    /// - `zc`: outermost open at start_line if any, else
    ///   innermost open containing cursor.
    /// - `zo`: outermost closed containing cursor.
    /// - `za`: zo if any closed contains cursor, else zc.
    pub(super) fn do_set_fold_state_at_cursor(&mut self, state: Option<bool>) {
        let line = self.cursor.line;
        let target = match state {
            Some(true) => fold_to_close_at(&self.folds, line),
            Some(false) => outermost_fold_idx(&self.folds, line, |f| f.closed),
            None => {
                let any_closed = self
                    .folds
                    .iter()
                    .any(|f| f.closed && line >= f.start_line && line <= f.end_line);
                if any_closed {
                    outermost_fold_idx(&self.folds, line, |f| f.closed)
                } else {
                    fold_to_close_at(&self.folds, line)
                }
            }
        };
        let Some(idx) = target else {
            self.set_message(EchoLevel::Error, "E490: No fold found".to_string());
            return;
        };
        self.folds[idx].closed = match state {
            None => !self.folds[idx].closed,
            Some(s) => s,
        };
    }

    pub(super) fn do_set_all_folds(&mut self, closed: bool) {
        for fold in self.folds.iter_mut() {
            fold.closed = closed;
        }
    }

    pub(super) fn do_goto_fold(&mut self, forward: bool) {
        let line = self.cursor.line;
        let target = if forward {
            self.folds
                .iter()
                .filter(|f| f.start_line > line)
                .map(|f| f.start_line)
                .min()
        } else {
            self.folds
                .iter()
                .filter(|f| f.end_line < line)
                .map(|f| f.end_line)
                .max()
        };
        if let Some(t) = target {
            self.cursor = Position::new(t, 0);
        } else {
            self.set_message(EchoLevel::Error, "no more folds".to_string());
        }
    }

    pub(super) fn do_delete_fold_at_cursor(&mut self) {
        let line = self.cursor.line;
        if let Some(idx) = innermost_fold_idx(&self.folds, line, |_| true) {
            self.folds.remove(idx);
        } else {
            self.set_message(EchoLevel::Error, "E490: No fold found".to_string());
        }
    }

    /// Returns true if `line` is inside a closed fold (and not the fold
    /// start). The renderer uses this to skip lines. When `foldenable`
    /// is false, returns `false` regardless of fold state.
    pub fn line_inside_closed_fold(&self, line: u32) -> bool {
        if !self.foldenable() {
            return false;
        }
        self.folds
            .iter()
            .any(|f| f.closed && line > f.start_line && line <= f.end_line)
    }

    /// Returns Some(fold) if `line` is the start of a closed fold; the
    /// renderer renders the summary header instead of the line content.
    pub fn fold_start_at(&self, line: u32) -> Option<&Fold> {
        if !self.foldenable() {
            return None;
        }
        self.folds.iter().find(|f| f.closed && f.start_line == line)
    }

    /// Returns Some(fold) if `line` is the start of any fold (open or
    /// closed). Used by the renderer for the gutter glyph.
    pub fn fold_start_at_any(&self, line: u32) -> Option<&Fold> {
        if !self.foldenable() {
            return None;
        }
        self.folds.iter().find(|f| f.start_line == line)
    }

    /// Move the cursor out of any closed fold's hidden body to the
    /// nearest visible line. Called after every non-jump motion so
    /// the cursor's logical position never lands in a hidden region.
    /// `foldenable = false` suppresses entirely.
    pub(super) fn snap_cursor_past_closed_folds(&mut self, prev_line: u32) {
        if !self.foldenable() {
            return;
        }
        let new_line = self.cursor.line;
        if new_line == prev_line {
            return;
        }
        let going_down = new_line > prev_line;
        let snap = self.document.snapshot();
        let last = last_addressable_line(&snap.buffer);
        let mut snapped = new_line;
        let mut exited_a_fold = false;
        loop {
            let in_closed = self
                .folds
                .iter()
                .find(|f| f.closed && snapped > f.start_line && snapped <= f.end_line)
                .copied();
            if let Some(fold) = in_closed {
                snapped = if going_down {
                    (fold.end_line + 1).min(last)
                } else {
                    fold.start_line
                };
                exited_a_fold = true;
                continue;
            }
            if exited_a_fold
                && going_down
                && snapped < last
                && is_blank_line(&snap.buffer, snapped)
            {
                snapped += 1;
                continue;
            }
            break;
        }
        if snapped == new_line {
            return;
        }
        let len = line_byte_len(&snap.buffer, snapped);
        let byte = self.cursor.byte.min(len);
        self.cursor = Position::new(snapped, byte);
    }

    /// Open every closed fold whose range contains the current cursor
    /// line. Called by jump-class motions (search hits, gg / G,
    /// marks, Ctrl-O / Ctrl-I, `%`) so the cursor never lands inside
    /// a hidden region.
    pub fn auto_open_folds_at_cursor(&mut self) {
        if !self.foldenable() {
            return;
        }
        let line = self.cursor.line;
        for fold in self.folds.iter_mut() {
            if fold.closed && line >= fold.start_line && line <= fold.end_line {
                fold.closed = false;
            }
        }
    }
}

/// Hash a slice of folds into a single u64 for the highlight
/// cache key. `start_line` / `end_line` / `closed` is the
/// minimum that affects the visible window; `identity` is
/// excluded -- two folds with the same range and state but
/// different identities don't change which bytes are visible.
pub(super) fn compute_fold_hash(folds: &[Fold]) -> u64 {
    use std::hash::{DefaultHasher, Hash, Hasher};
    let mut h = DefaultHasher::new();
    folds.len().hash(&mut h);
    for f in folds {
        f.start_line.hash(&mut h);
        f.end_line.hash(&mut h);
        f.closed.hash(&mut h);
    }
    h.finish()
}

/// Index of the *innermost* fold containing `line` that satisfies
/// `pred`. Innermost = max start_line, then min end_line on ties.
/// Used by `zc` (close innermost open) and `za`'s close branch.
fn innermost_fold_idx<F: Fn(&Fold) -> bool>(
    folds: &[Fold],
    line: u32,
    pred: F,
) -> Option<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| pred(f) && line >= f.start_line && line <= f.end_line)
        .max_by_key(|(_, f)| (f.start_line, std::cmp::Reverse(f.end_line)))
        .map(|(i, _)| i)
}

/// Pick the fold that `zc` (or `za`'s close branch) should target
/// when the cursor is on `line`.
///
/// If any open fold *starts* at `line`, the user is positioned on
/// the line that opens one or more folds. Their natural intent is
/// to fold the *largest* of those constructs in one step (the
/// "fold the entire form" reading of `zc`). Pick the outermost
/// (largest end_line) among the open folds whose start_line equals
/// the cursor.
///
/// Otherwise the cursor is in a fold's body and the inverse rule
/// applies: pick the innermost open fold containing the cursor.
fn fold_to_close_at(folds: &[Fold], line: u32) -> Option<usize> {
    let starts_here = folds
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.closed && f.start_line == line)
        .max_by_key(|(_, f)| f.end_line)
        .map(|(i, _)| i);
    if starts_here.is_some() {
        return starts_here;
    }
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| !f.closed && line > f.start_line && line <= f.end_line)
        .max_by_key(|(_, f)| (f.start_line, std::cmp::Reverse(f.end_line)))
        .map(|(i, _)| i)
}

/// Index of the *outermost* fold containing `line` that satisfies
/// `pred`. Outermost = min start_line, then max end_line on ties.
/// Used by `zo` (open outermost closed) and `za`'s open branch.
fn outermost_fold_idx<F: Fn(&Fold) -> bool>(
    folds: &[Fold],
    line: u32,
    pred: F,
) -> Option<usize> {
    folds
        .iter()
        .enumerate()
        .filter(|(_, f)| pred(f) && line >= f.start_line && line <= f.end_line)
        .min_by_key(|(_, f)| (f.start_line, std::cmp::Reverse(f.end_line)))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use crate::app::*;
    use crate::app::test_helpers::{app_with, attach_test_syntax, invoke_motion};
    use lattice_grammar::ModalState;
    use lattice_protocol::edit::Edit;
    use lattice_protocol::position::Position;
    use super::compute_fold_hash;

    // ---- refresh_highlights cache invalidation on fold change ----


    #[test]
    fn refresh_highlights_cache_invalidates_on_fold_change() {
        let mut a = app_with("fn a() {\n    1;\n}\nfn b() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let key1 = a.visible_highlights_key;
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.refresh_highlights();
        let key2 = a.visible_highlights_key;
        assert_ne!(key1, key2, "fold push must invalidate cache");
    }

    #[test]
    fn refresh_highlights_cache_invalidates_on_fold_toggle() {
        let mut a = app_with("fn a() {\n    1;\n}\nfn b() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.refresh_highlights();
        let key1 = a.visible_highlights_key;
        a.folds[0].closed = true;
        a.refresh_highlights();
        let key2 = a.visible_highlights_key;
        assert_ne!(key1, key2, "fold open->closed must invalidate cache");
    }

    #[test]
    fn refresh_highlights_cache_invalidates_on_edit() {
        // Apply edit -> document text_version bumps ->
        // maybe_reparse_syntax publishes a new snapshot ->
        // refresh_highlights sees a new snapshot pointer +
        // text_version, so the cache key is fresh.
        let mut a = app_with("fn main() {}", 5);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.refresh_highlights();
        let key1 = a.visible_highlights_key;
        // Edit + reparse seam (mirrors what App::apply does at
        // the end of an Action).
        a.apply_edit_blocking(Edit::insert(Position::new(0, 11), "\nfn b() {}"))
            .unwrap();
        a.maybe_reparse_syntax();
        // The seeded syntax handle's worker runs synchronously
        // in the seeded path (no tokio runtime in lib tests
        // means the worker doesn't run, so the snapshot stays
        // at the prior version). Drive the parse explicitly so
        // the cache key reflects the new snapshot.
        if let Some(syntax) = a.syntax.as_ref() {
            // Re-seed via the test helper: parses the current
            // text synchronously, replaces the handle. Mirrors
            // the worker's effect.
            let new_text = a.document.text();
            let new_tv = a.document.text_version();
            let mut s = lattice_syntax::Syntax::for_language(syntax.lang())
                .unwrap()
                .expect("syntax registered for lang");
            s.parse_at(&new_text, new_tv);
            a.syntax = Some(lattice_syntax::SyntaxHandle::seeded(s));
            a.last_synced_syntax_version = new_tv;
        }
        a.refresh_highlights();
        let key2 = a.visible_highlights_key;
        assert_ne!(key1, key2, "edit must invalidate cache");
    }


    // ---- compute_fold_hash ----

    #[test]
    fn fold_hash_distinguishes_closed_vs_open() {
        let f_open = Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        };
        let f_closed = Fold {
            closed: true,
            ..f_open
        };
        assert_ne!(compute_fold_hash(&[f_open]), compute_fold_hash(&[f_closed]));
    }

    #[test]
    fn fold_hash_distinguishes_different_ranges() {
        let f1 = Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        };
        let f2 = Fold {
            start_line: 0,
            end_line: 3,
            closed: false,
            identity: None,
        };
        assert_ne!(compute_fold_hash(&[f1]), compute_fold_hash(&[f2]));
    }

    #[test]
    fn fold_hash_ignores_identity() {
        // identity is metadata for closed-state preservation
        // across recomputes; doesn't affect which bytes are
        // visible. Two folds with same range/state but
        // different identities should hash equal.
        let f1 = Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: Some(42),
        };
        let f2 = Fold {
            identity: Some(99),
            ..f1
        };
        assert_eq!(compute_fold_hash(&[f1]), compute_fold_hash(&[f2]));
    }


    // ---- z* fold operations ----

    // ---- Folds (zf, zo, zc, za, zR, zM, zd) ----

    #[test]
    fn zf_from_visual_creates_a_closed_fold() {
        let mut a = app_with("a\nb\nc\nd\ne", 10);
        a.apply(Action::EnterVisual(VisualKind::Linewise));
        a.apply(invoke_motion(a.builtins.line_down));
        a.apply(invoke_motion(a.builtins.line_down));
        // Selection now spans lines 0..2.
        a.apply(Action::CreateFoldFromVisual);
        assert_eq!(a.folds.len(), 1);
        let fold = &a.folds[0];
        assert_eq!(fold.start_line, 0);
        assert_eq!(fold.end_line, 2);
        assert!(fold.closed);
        // Visual exited.
        assert_eq!(a.modal, ModalState::Normal);
    }

    #[test]
    fn zf_outside_visual_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::CreateFoldFromVisual);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn zo_opens_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.apply(Action::OpenFoldAtCursor);
        assert!(!a.folds[0].closed);
    }

    #[test]
    fn zc_closes_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.apply(Action::CloseFoldAtCursor);
        assert!(a.folds[0].closed);
    }

    #[test]
    fn za_toggles_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.apply(Action::ToggleFoldAtCursor);
        assert!(a.folds[0].closed);
        a.apply(Action::ToggleFoldAtCursor);
        assert!(!a.folds[0].closed);
    }

    #[test]
    fn capital_zr_opens_all_folds() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 1,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: true,
            identity: None,
        });
        a.apply(Action::OpenAllFolds);
        assert!(a.folds.iter().all(|f| !f.closed));
    }

    #[test]
    fn capital_zm_closes_all_folds() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 1,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
            identity: None,
        });
        a.apply(Action::CloseAllFolds);
        assert!(a.folds.iter().all(|f| f.closed));
    }

    #[test]
    fn zd_deletes_fold_at_cursor() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(Action::DeleteFoldAtCursor);
        assert!(a.folds.is_empty());
    }

    // --- Nested-fold semantics (`zc` / `zo` / `za` / `zd`) -----

    fn nested_folds_app() -> App {
        // Two nested open folds: outer covers lines 0..=10, inner
        // covers 2..=8. Cursor sits inside both at line 4.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 10,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 2,
            end_line: 8,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(4, 0);
        a
    }

    #[test]
    fn zc_closes_innermost_open_fold_first() {
        let mut a = nested_folds_app();
        a.apply(Action::CloseFoldAtCursor);
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed, "inner should close first");
        assert!(!outer.closed, "outer should remain open until next zc");
    }

    #[test]
    fn second_zc_closes_outer_fold() {
        let mut a = nested_folds_app();
        a.apply(Action::CloseFoldAtCursor); // closes inner
        a.apply(Action::CloseFoldAtCursor); // should close outer
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed);
        assert!(outer.closed);
    }

    #[test]
    fn zo_opens_outermost_closed_fold_first() {
        let mut a = nested_folds_app();
        // Both folds closed.
        for f in a.folds.iter_mut() {
            f.closed = true;
        }
        a.apply(Action::OpenFoldAtCursor);
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        assert!(!outer.closed, "outer should open first");
        assert!(inner.closed, "inner should remain closed until next zo");
    }

    #[test]
    fn za_toggles_to_open_when_any_fold_closed_then_close_when_all_open() {
        let mut a = nested_folds_app();
        // Close outer only.
        a.folds[0].closed = true;
        // za with the outer closed => open the outermost closed (the outer).
        a.apply(Action::ToggleFoldAtCursor);
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        assert!(!outer.closed);
        assert!(!inner.closed);
        // Now both open: za should close the innermost.
        a.apply(Action::ToggleFoldAtCursor);
        let inner = a.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed);
        assert!(!outer.closed);
    }

    #[test]
    fn zc_with_all_folds_closed_emits_e490() {
        let mut a = nested_folds_app();
        for f in a.folds.iter_mut() {
            f.closed = true;
        }
        a.apply(Action::CloseFoldAtCursor);
        // No state change; both still closed.
        assert!(a.folds.iter().all(|f| f.closed));
        // E490 echoed.
        let msg = a.last_message.as_ref().expect("message").text.clone();
        assert!(msg.contains("E490"), "expected E490, got {msg:?}");
    }

    #[test]
    fn zd_removes_innermost_only() {
        let mut a = nested_folds_app();
        a.apply(Action::DeleteFoldAtCursor);
        // The inner (start=2) fold is gone; outer remains.
        assert!(a.folds.iter().any(|f| f.start_line == 0));
        assert!(!a.folds.iter().any(|f| f.start_line == 2));
    }

    // --- Linear j/k skip closed folds (`docs/help/folding.md`) ---

    #[test]
    fn line_down_from_closed_fold_heading_skips_to_after_fold() {
        // 12-line buffer with a closed fold spanning lines 1..=4.
        // From line 1 (heading), `j` should land on line 5, not 2.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(
            a.cursor.line, 5,
            "j from closed-fold heading must skip to fold.end_line + 1"
        );
    }

    #[test]
    fn line_up_into_closed_fold_snaps_to_heading() {
        // From line 5, `k` lands on 4 -- inside a closed fold (1..=4).
        // Snap to fold.start_line (1).
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(5, 0);
        a.apply(invoke_motion(a.builtins.line_up));
        assert_eq!(
            a.cursor.line, 1,
            "k into a closed fold must snap to its heading line"
        );
    }

    #[test]
    fn linear_j_into_open_fold_does_not_skip() {
        // Open folds don't hide content; j moves one line as usual.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn linear_motion_with_nofoldenable_does_not_skip() {
        // `:set nofoldenable` / `zi` should make every line visible
        // for navigation, including closed-fold interiors.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.set_foldenable_for_test(false);
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 2);
    }

    fn line_down_lands_on_next_fold_heading_when_consecutive() {
        // Three closed folds back-to-back: 1..=3, 4..=6, 7..=9.
        // Each `j` moves one visible line; a closed fold's heading
        // IS a visible line. So:
        //   line 1 (fold A heading) --j--> line 4 (fold B heading)
        //   line 4 (fold B heading) --j--> line 7 (fold C heading)
        //   line 7 (fold C heading) --j--> line 10 (after fold C)
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 3,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 4,
            end_line: 6,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 7,
            end_line: 9,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 4, "first j → fold B heading");
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 7, "second j → fold C heading");
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 10, "third j → past fold C");
    }

    #[test]
    fn line_down_skips_consecutive_closed_folds_in_one_keypress() {
        // Wrapper / dummy: superseded by
        // `line_down_lands_on_next_fold_heading_when_consecutive`.
        // The historical name preserved so anyone re-running an
        // older test list spots the rename.
        line_down_lands_on_next_fold_heading_when_consecutive();
    }

    // --- Generalised snap covers all non-jump motions --------

    #[test]
    fn word_forward_snaps_out_of_closed_fold_body() {
        // `w` from a closed fold's heading lands on the next word.
        // Pre-snap, that next word might be inside the fold body
        // (cursor at hidden line). The snap projects cursor onto a
        // visible line so subsequent `zc` resolves correctly.
        let src = "alpha bravo\n    charlie delta\n    echo foxtrot\nafter golf hotel\n";
        let mut a = app_with(src, 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(0, 0);
        a.apply(invoke_motion(a.builtins.word_forward));
        // Without snap the cursor would land on "bravo" (line 0,
        // byte 6) -- still visible. Press w again: would go into
        // hidden `charlie`. The snap kicks in there.
        a.apply(invoke_motion(a.builtins.word_forward));
        assert!(
            !a.line_inside_closed_fold(a.cursor.line),
            "w must not leave cursor inside a hidden fold body \
             (cursor.line = {})",
            a.cursor.line
        );
    }

    #[test]
    fn refresh_highlights_covers_buffer_lines_below_a_closed_fold() {
        // Regression: with a closed fold inside the viewport, the
        // highlight window must stretch to include lines that
        // appear *below* the fold's collapsed row but are still in
        // the visible region. Otherwise spans drop to empty and
        // syntax styling visibly disappears for content under
        // every fold.
        let mut a = app_with(
            "fn a() {\n    1;\n    2;\n    3;\n    4;\n}\nfn b() {\n    5;\n}\n",
            5, // viewport = 5 rows
        );
        // Wire up a real syntax instance so highlight_lines runs.
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        // Close the first fn (lines 0..=5, 6 buffer lines collapsed
        // onto one row). With a 5-row viewport that means `fn b`
        // (line 6) and its body (lines 7, 8) all sit in the visible
        // region.
        a.folds.push(Fold {
            start_line: 0,
            end_line: 5,
            closed: true,
            identity: None,
        });
        a.refresh_highlights();
        // Without the fix: visible_highlights is sized 5 (height),
        // so line 6 (offset 6) returns &[] -> no syntax. Now: the
        // highlight window stretches to cover line 8, so line 6's
        // spans are populated.
        assert!(
            !a.highlights_for_buffer_line(6).is_empty(),
            "fn b heading must be highlighted under a closed fold"
        );
        assert!(
            !a.highlights_for_buffer_line(7).is_empty(),
            "fn b body must be highlighted under a closed fold"
        );
    }

    #[test]
    fn syntax_fold_zc_on_indented_let_with_if_else_reports_five_lines() {
        // The user's actual scenario: the `let` form is INDENTED
        // (inside a function body). Verify the outer-pick rule on
        // the if/let line still resolves to the full if_expression
        // fold even with leading whitespace, so the rendered count
        // is 5 lines (not 3 -- the inner then-block size).
        let src = "fn outer() -> u32 {\n    let len = if has_trailing_newline {\n        bytes - 1\n    } else {\n        bytes\n    };\n    len\n}\n";
        let mut a = app_with(src, 20);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.set_foldmethod_for_test(FoldMethod::Syntax);
        a.recompute_folds();
        // Dump for diagnosis -- show the fold ranges at the live
        // tree's current state.
        eprintln!("FOLDS (indented let-if-else inside fn):");
        for f in &a.folds {
            eprintln!(
                "  ({}, {}) span={} lines",
                f.start_line,
                f.end_line,
                f.end_line - f.start_line + 1
            );
        }
        // Cursor on line 1 -- the indented `let len = if ...`
        // line. zc should pick the outermost fold whose start_line
        // is 1 (the if_expression / let_declaration), not the
        // inner then-block.
        a.cursor = Position::new(1, 0);
        a.apply(Action::CloseFoldAtCursor);
        let fold = a
            .fold_start_at(1)
            .expect("a closed fold should start at line 1");
        let count = fold.end_line - fold.start_line + 1;
        assert_eq!(
            count, 5,
            "indented if/else fold must span 5 lines (got {count}; fold = {fold:?}; all = {:?})",
            a.folds
        );
    }

    #[test]
    fn syntax_fold_zc_on_let_with_if_else_reports_five_lines() {
        // Full-pipeline regression for the user's scenario: a top-
        // level `let` binding wrapping an if/else expression. With
        // foldmethod=syntax, the cursor on the `let` line gets
        // `zc` to close the entire 5-line form and the rendered
        // summary shows "5 lines folded".
        let src = "let len = if cond {\n    bytes - 1\n} else {\n    bytes\n}\n";
        let mut a = app_with(src, 10);
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.set_foldmethod_for_test(FoldMethod::Syntax);
        a.recompute_folds();
        // Cursor on line 0 (the `let` line). zc must pick the
        // outermost fold starting at line 0 -- the if_expression /
        // let_declaration spanning (0, 4) -- and close it.
        a.cursor = Position::new(0, 0);
        a.apply(Action::CloseFoldAtCursor);
        let fold = a
            .fold_start_at(0)
            .expect("a closed fold should start at line 0 after zc");
        let count = fold.end_line - fold.start_line + 1;
        assert_eq!(
            count, 5,
            "fold at line 0 must span 5 lines (got {count}; fold = {fold:?})"
        );
    }

    #[test]
    fn paragraph_motion_snaps_out_of_closed_fold_body() {
        // `}` (paragraph forward) from inside a fold can land
        // cursor on a hidden paragraph break. Snap must apply.
        let src = "alpha\n\n    body line one\n    body line two\n\nafter\n";
        let mut a = app_with(src, 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.cursor = Position::new(0, 0);
        a.apply(invoke_motion(a.builtins.paragraph_forward));
        assert!(
            !a.line_inside_closed_fold(a.cursor.line),
            "}} must not leave cursor inside a hidden fold body \
             (cursor.line = {})",
            a.cursor.line
        );
    }

    #[test]
    fn line_down_swallows_blanks_between_sibling_folds_for_zc_targeting() {
        // Reproduces the user's "third form" regression: with a
        // blank line between two closed folds, j from the first
        // fold's heading must land on the *next sibling's heading*,
        // not on the blank between them. Otherwise zc on the blank
        // resolves to "innermost open fold containing this line" =
        // the parent.
        //
        // Buffer (impl with three fns separated by blank lines):
        //   line 0: impl B {
        //   line 1:   fn a() {
        //   line 2:   }
        //   line 3:   <blank>
        //   line 4:   fn b() {
        //   line 5:   }
        //   line 6:   <blank>
        //   line 7:   fn c() {
        //   line 8:   }
        //   line 9: }
        let src = "impl B {\n    fn a() {\n    }\n\n    fn b() {\n    }\n\n    fn c() {\n    }\n}\n";
        let mut a = app_with(src, 20);
        // Outer impl + three function folds (skip blank-line 3 / 6).
        a.folds.push(Fold {
            start_line: 0,
            end_line: 9,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 1,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 4,
            end_line: 5,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 7,
            end_line: 8,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        // j from fn a's heading: snap over fn a's body, swallow the
        // blank, land on fn b's heading (line 4).
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(
            a.cursor.line, 4,
            "j after fold A must skip the blank and land on fn b"
        );
        // Close fn b, j again, land on fn c's heading (line 7).
        a.apply(Action::CloseFoldAtCursor);
        let fnb = a.folds.iter().find(|f| f.start_line == 4).unwrap();
        assert!(fnb.closed, "zc on fn b heading closes fn b");
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(
            a.cursor.line, 7,
            "j after fold B must skip the blank and land on fn c"
        );
        // Close fn c. The outer impl must remain open.
        a.apply(Action::CloseFoldAtCursor);
        let fnc = a.folds.iter().find(|f| f.start_line == 7).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(fnc.closed, "zc on fn c heading closes fn c, not outer");
        assert!(!outer.closed, "outer impl must remain open through the sequence");
    }

    #[test]
    fn zc_on_sibling_fold_after_navigating_with_j_closes_sibling_not_parent() {
        // Regression: with one inner fold already closed, `j` from
        // its heading must put the cursor on the sibling's heading
        // (line 5), not inside the closed fold's body. Then `zc`
        // on the sibling closes the sibling -- not the outer.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.folds.push(Fold {
            start_line: 0,
            end_line: 10,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 5,
            end_line: 9,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(1, 0);
        // Move to the sibling's heading.
        a.apply(invoke_motion(a.builtins.line_down));
        assert_eq!(a.cursor.line, 5, "cursor should land on sibling, not interior");
        // Close the sibling.
        a.apply(Action::CloseFoldAtCursor);
        let sibling = a.folds.iter().find(|f| f.start_line == 5).unwrap();
        let outer = a.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(sibling.closed, "sibling should close, not the outer");
        assert!(!outer.closed, "outer must remain open");
    }

    #[test]
    fn zj_jumps_to_next_fold_start() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        a.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
            identity: None,
        });
        a.folds.push(Fold {
            start_line: 5,
            end_line: 5,
            closed: false,
            identity: None,
        });
        a.cursor = Position::ZERO;
        a.apply(Action::GotoNextFold);
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn zk_jumps_to_previous_fold_end() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.cursor = Position::new(5, 0);
        a.apply(Action::GotoPrevFold);
        assert_eq!(a.cursor.line, 2);
    }

    #[test]
    fn zj_with_no_more_folds_emits_error() {
        let mut a = app_with("a\nb\nc", 10);
        a.apply(Action::GotoNextFold);
        let msg = a.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn line_inside_closed_fold_returns_true_for_interior() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.folds.push(Fold {
            start_line: 1,
            end_line: 3,
            closed: true,
            identity: None,
        });
        assert!(!a.line_inside_closed_fold(0));
        // Start line is the summary, NOT inside.
        assert!(!a.line_inside_closed_fold(1));
        assert!(a.line_inside_closed_fold(2));
        assert!(a.line_inside_closed_fold(3));
    }


    // ---- foldmethod ----

    // ---- Computed folds (DESIGN.md §15:18, C.2) ----

    #[test]
    fn foldmethod_indent_populates_folds_from_indentation() {
        let mut a = app_with("def f():\n    pass\n    pass\n", 10);
        a.command_line = "set foldmethod=indent".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Indent);
        assert!(!a.folds.is_empty());
        let f = a.folds.iter().find(|f| f.start_line == 0).expect("fold");
        assert_eq!(f.end_line, 2);
    }

    #[test]
    fn foldmethod_indent_preserves_closed_state_across_reparse() {
        let mut a = app_with("a:\n    b\n    c\n", 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        assert_eq!(a.folds.len(), 1);
        // Close the fold.
        a.folds[0].closed = true;
        // Recompute should preserve closed state (same range).
        a.recompute_folds();
        assert!(a.folds[0].closed);
    }

    #[test]
    fn foldmethod_manual_default_does_not_recompute() {
        let mut a = app_with("def f():\n    pass\n", 10);
        a.recompute_folds();
        assert!(a.folds.is_empty());
    }

    #[test]
    fn foldmethod_markdown_populates_folds_from_atx_headings() {
        let mut a = app_with("# H1\nbody\nmore body\n", 10);
        a.command_line = "set foldmethod=markdown".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Markdown);
        assert!(!a.folds.is_empty());
        let f = a.folds.iter().find(|f| f.start_line == 0).expect("fold");
        assert!(f.end_line >= 2);
    }

    #[test]
    fn foldmethod_syntax_cascades_to_indent_when_no_md_extension() {
        // Plain-text buffer (no `Syntax`): syntax provider returns
        // None and we cascade to indent.
        let mut a = app_with("def f():\n    pass\n    pass\n", 10);
        a.command_line = "set foldmethod=syntax".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Syntax);
        assert!(a.folds.iter().any(|f| f.start_line == 0 && f.end_line == 2));
    }

    #[test]
    fn foldmethod_syntax_uses_tree_sitter_for_rust_buffer() {
        // With Syntax set up for Rust, `:set foldmethod=syntax`
        // should produce tree-sitter folds (struct, fn, impl) rather
        // than indent folds.
        let mut a = app_with(
            "struct B {\n    x: u8,\n}\n\nimpl B {\n    fn n() -> Self {\n        Self { x: 0 }\n    }\n}\n",
            10,
        );
        // Wire up Rust syntax + parse the document so the fold
        // provider has a tree to query.
        attach_test_syntax(&mut a, lattice_syntax::Lang::Rust);
        a.command_line = "set foldmethod=syntax".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Syntax);
        // Tree-sitter fold for the struct (lines 0..=2).
        assert!(
            a.folds.iter().any(|f| f.start_line == 0 && f.end_line >= 2),
            "expected struct fold from tree-sitter: {:?}",
            a.folds
        );
        // Tree-sitter fold for the impl (starts at line 4).
        assert!(
            a.folds.iter().any(|f| f.start_line == 4),
            "expected impl fold from tree-sitter: {:?}",
            a.folds
        );
    }

    #[test]
    fn foldmethod_indent_identity_preserves_closed_state_after_unrelated_insert() {
        // Two sibling functions; close the *second* fold; insert a
        // new line into the *first* function (shifting line numbers
        // for the second fold). Identity-based matching should keep
        // the second fold closed despite its (start_line, end_line)
        // having shifted.
        let initial = "first:\n    a\n    b\nsecond:\n    x\n    y\n";
        let mut a = app_with(initial, 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        // Find and close the `second:` fold.
        let second_idx = a
            .folds
            .iter()
            .position(|f| f.start_line == 3)
            .expect("second: fold exists");
        a.folds[second_idx].closed = true;
        // Insert a new line inside the first function (between `a` and `b`).
        a.apply_edit_blocking(Edit::insert(Position::new(2, 0), "    extra\n"))
            .unwrap();
        a.recompute_folds();
        // The recomputed `second:` fold has start_line = 4 now, but
        // its identity (heading text "second:" + indent 0) matches.
        let new_second = a
            .folds
            .iter()
            .find(|f| f.start_line == 4)
            .expect("second: fold survived insertion");
        assert!(
            new_second.closed,
            "closed-state should survive line shift via identity match"
        );
    }

    #[test]
    fn foldmethod_rejects_unknown_value() {
        let mut a = app_with("a\n", 10);
        a.command_line = "set foldmethod=bogus".into();
        a.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Manual);
        assert!(a.last_message.is_some());
    }

}
