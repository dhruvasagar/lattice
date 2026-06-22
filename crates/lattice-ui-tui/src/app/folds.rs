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

use super::{App, Fold};

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
    /// 5.5.D: full fold recompute moved to
    /// [`lattice_host::editor::Editor::recompute_folds`] alongside
    /// `recompute_syntax_folds` / `recompute_lsp_folds`. Renderer
    /// call sites keep this thin wrapper until 5.5.G collapses
    /// App's match entirely.
    pub fn recompute_folds(&mut self) {
        // Slice 3c.final.E.2: route through `mutate_editor`.
        self.mutate_editor(|e| e.recompute_folds());
    }

    // 5.5.G.16: `do_create_fold_from_visual` migrated to
    // [`lattice_host::dispatch::Editor`].

    // 5.5.G.1: `do_set_fold_state_at_cursor` / `do_set_all_folds`
    // / `do_goto_fold` / `do_delete_fold_at_cursor` all migrated
    // to [`lattice_host::dispatch::Editor`] (the `Action::*Fold*`
    // arms in `Editor::dispatch` call them directly). The private
    // selection-rule helpers `innermost_fold_idx`, `fold_to_close_at`,
    // and `outermost_fold_idx` co-moved.

    /// 5.8.U: body migrated to
    /// [`lattice_host::dispatch::Editor::line_inside_closed_fold`]
    /// so the GPUI peer can reach the same check.
    pub fn line_inside_closed_fold(&self, line: u32) -> bool {
        // Slice 3c.final.X.cleanup: read folds from published
        // `ad()` instead of via the actor seam. Editor::line_inside_closed_fold
        // gates on `option_cache.foldenable` then walks `self.folds` —
        // both are mirrored on `ActiveDocumentRenderState`.
        // cfg(test) escape hatch — see `App::cursor()`.
        #[cfg(test)]
        {
            self.editor.line_inside_closed_fold(line)
        }
        #[cfg(not(test))]
        {
            let ad = self.ad();
            if !ad.option_cache.foldenable {
                return false;
            }
            ad.folds
                .iter()
                .any(|f| f.closed && line > f.start_line && line <= f.end_line)
        }
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::fold_start_at`]. Retained
    /// as a delegate because the renderer's per-frame gutter pass
    /// (search.rs, motions.rs, render.rs) and the host-side
    /// `run_document_invocation` both still call it before the
    /// helper deletion sweep.
    pub fn fold_start_at(&self, line: u32) -> Option<Fold> {
        // Slice 3c.final.X.cleanup: read via published `ad().folds`.
        // cfg(test) escape hatch — see `App::cursor()`.
        #[cfg(test)]
        {
            self.editor.fold_start_at(line).copied()
        }
        #[cfg(not(test))]
        {
            let ad = self.ad();
            if !ad.option_cache.foldenable {
                return None;
            }
            ad.folds
                .iter()
                .find(|f| f.closed && f.start_line == line)
                .copied()
        }
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::fold_start_at_any`]. Retained
    /// as a 1-line delegate; renderer call sites retire on the same
    /// sweep as `fold_start_at`.
    pub fn fold_start_at_any(&self, line: u32) -> Option<Fold> {
        // Slice 3c.final.X.cleanup: read via published `ad().folds`.
        // cfg(test) escape hatch — see `App::cursor()`.
        #[cfg(test)]
        {
            self.editor.fold_start_at_any(line).copied()
        }
        #[cfg(not(test))]
        {
            let ad = self.ad();
            if !ad.option_cache.foldenable {
                return None;
            }
            ad.folds.iter().find(|f| f.start_line == line).copied()
        }
    }

    /// 5.5.G.23: body migrated to
    /// [`lattice_host::dispatch::Editor::snap_cursor_past_closed_folds`].
    /// Retained as a 1-line delegate because the renderer's
    /// search-result + motions paths still drive it; deletion
    /// follows once those callers go host-side.
    pub(super) fn snap_cursor_past_closed_folds(&mut self, prev_line: u32) {
        // Slice 3c.final.E.2: route through `mutate_editor`.
        self.mutate_editor(move |e| e.snap_cursor_past_closed_folds(prev_line));
    }

    /// 5.5.G.4: body migrated to
    /// [`lattice_host::dispatch::Editor::auto_open_folds_at_cursor`].
    /// Retained as a delegate because 7 ui-tui call sites
    /// (search.rs, motions.rs, dispatch.rs) still invoke it; the
    /// delegate retires when those call sites migrate.
    pub fn auto_open_folds_at_cursor(&mut self) {
        // Slice 3c.final.E.2: route through `mutate_editor`.
        self.mutate_editor(|e| e.auto_open_folds_at_cursor());
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

// 5.5.G.1: private selection-rule helpers (`innermost_fold_idx`,
// `fold_to_close_at`, `outermost_fold_idx`) migrated to
// `lattice_host::dispatch` alongside the `do_*` arms that called
// them.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::compute_fold_hash;
    use crate::app::test_helpers::{app_with, attach_test_syntax, invoke_motion};
    use crate::app::*;
    use lattice_grammar::{ModalState, VisualKind};
    use lattice_protocol::edit::Edit;
    use lattice_protocol::position::Position;

    // Phase 5.8.AF.5 / Slice X2.6: three `refresh_highlights_cache_
    // invalidates_*` tests were retired with the
    // `Editor::visible_highlights_key` field they pinned. Equivalent
    // coverage for the worker's cache-invalidation contract
    // (fold change, fold toggle, edit -> new snapshot) lives in
    // `lattice_host::overlay_worker::recompute` tests (display-line
    // B4.2: the span-cache worker was gutted to the overlay bucket).

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
        a.apply(invoke_motion(a.editor.builtins.line_down));
        a.apply(invoke_motion(a.editor.builtins.line_down));
        // Selection now spans lines 0..2.
        a.apply(Action::CreateFoldFromVisual);
        assert_eq!(a.editor.folds.len(), 1);
        let fold = &a.editor.folds[0];
        assert_eq!(fold.start_line, 0);
        assert_eq!(fold.end_line, 2);
        assert!(fold.closed);
        // Visual exited.
        assert_eq!(a.editor.modal, ModalState::Normal);
    }

    #[test]
    fn zf_outside_visual_emits_error() {
        let mut a = app_with("hello", 10);
        a.apply(Action::CreateFoldFromVisual);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn zo_opens_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.apply(Action::OpenFoldAtCursor);
        assert!(!a.editor.folds[0].closed);
    }

    #[test]
    fn zc_closes_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.apply(Action::CloseFoldAtCursor);
        assert!(a.editor.folds[0].closed);
    }

    #[test]
    fn za_toggles_fold_at_cursor() {
        let mut a = app_with("a\nb\nc", 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.apply(Action::ToggleFoldAtCursor);
        assert!(a.editor.folds[0].closed);
        a.apply(Action::ToggleFoldAtCursor);
        assert!(!a.editor.folds[0].closed);
    }

    #[test]
    fn capital_zr_opens_all_folds() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 1,
            closed: true,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: true,
            identity: None,
        });
        a.apply(Action::OpenAllFolds);
        assert!(a.editor.folds.iter().all(|f| !f.closed));
    }

    #[test]
    fn capital_zm_closes_all_folds() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 1,
            closed: false,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
            identity: None,
        });
        a.apply(Action::CloseAllFolds);
        assert!(a.editor.folds.iter().all(|f| f.closed));
    }

    #[test]
    fn zd_deletes_fold_at_cursor() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::DeleteFoldAtCursor);
        assert!(a.editor.folds.is_empty());
    }

    // --- Nested-fold semantics (`zc` / `zo` / `za` / `zd`) -----

    fn nested_folds_app() -> App {
        // Two nested open folds: outer covers lines 0..=10, inner
        // covers 2..=8. Cursor sits inside both at line 4.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 10,
            closed: false,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 2,
            end_line: 8,
            closed: false,
            identity: None,
        });
        a.editor.cursor = Position::new(4, 0);
        a
    }

    #[test]
    fn zc_closes_innermost_open_fold_first() {
        let mut a = nested_folds_app();
        a.apply(Action::CloseFoldAtCursor);
        let inner = a.editor.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.editor.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed, "inner should close first");
        assert!(!outer.closed, "outer should remain open until next zc");
    }

    #[test]
    fn second_zc_closes_outer_fold() {
        let mut a = nested_folds_app();
        a.apply(Action::CloseFoldAtCursor); // closes inner
        a.apply(Action::CloseFoldAtCursor); // should close outer
        let inner = a.editor.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.editor.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed);
        assert!(outer.closed);
    }

    #[test]
    fn zo_opens_outermost_closed_fold_first() {
        let mut a = nested_folds_app();
        // Both folds closed.
        for f in a.editor.folds.iter_mut() {
            f.closed = true;
        }
        a.apply(Action::OpenFoldAtCursor);
        let outer = a.editor.folds.iter().find(|f| f.start_line == 0).unwrap();
        let inner = a.editor.folds.iter().find(|f| f.start_line == 2).unwrap();
        assert!(!outer.closed, "outer should open first");
        assert!(inner.closed, "inner should remain closed until next zo");
    }

    #[test]
    fn za_toggles_to_open_when_any_fold_closed_then_close_when_all_open() {
        let mut a = nested_folds_app();
        // Close outer only.
        a.editor.folds[0].closed = true;
        // za with the outer closed => open the outermost closed (the outer).
        a.apply(Action::ToggleFoldAtCursor);
        let outer = a.editor.folds.iter().find(|f| f.start_line == 0).unwrap();
        let inner = a.editor.folds.iter().find(|f| f.start_line == 2).unwrap();
        assert!(!outer.closed);
        assert!(!inner.closed);
        // Now both open: za should close the innermost.
        a.apply(Action::ToggleFoldAtCursor);
        let inner = a.editor.folds.iter().find(|f| f.start_line == 2).unwrap();
        let outer = a.editor.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(inner.closed);
        assert!(!outer.closed);
    }

    #[test]
    fn zc_with_all_folds_closed_emits_e490() {
        let mut a = nested_folds_app();
        for f in a.editor.folds.iter_mut() {
            f.closed = true;
        }
        a.apply(Action::CloseFoldAtCursor);
        // No state change; both still closed.
        assert!(a.editor.folds.iter().all(|f| f.closed));
        // E490 echoed.
        let msg = a
            .editor
            .last_message
            .as_ref()
            .expect("message")
            .text
            .clone();
        assert!(msg.contains("E490"), "expected E490, got {msg:?}");
    }

    #[test]
    fn zd_removes_innermost_only() {
        let mut a = nested_folds_app();
        a.apply(Action::DeleteFoldAtCursor);
        // The inner (start=2) fold is gone; outer remains.
        assert!(a.editor.folds.iter().any(|f| f.start_line == 0));
        assert!(!a.editor.folds.iter().any(|f| f.start_line == 2));
    }

    // --- Linear j/k skip closed folds (`docs/user/folding.md`) ---

    #[test]
    fn line_down_from_closed_fold_heading_skips_to_after_fold() {
        // 12-line buffer with a closed fold spanning lines 1..=4.
        // From line 1 (heading), `j` should land on line 5, not 2.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.editor.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(
            a.editor.cursor.line, 5,
            "j from closed-fold heading must skip to fold.end_line + 1"
        );
    }

    #[test]
    fn line_up_into_closed_fold_snaps_to_heading() {
        // From line 5, `k` lands on 4 -- inside a closed fold (1..=4).
        // Snap to fold.start_line (1).
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.editor.cursor = Position::new(5, 0);
        a.apply(invoke_motion(a.editor.builtins.line_up));
        assert_eq!(
            a.editor.cursor.line, 1,
            "k into a closed fold must snap to its heading line"
        );
    }

    #[test]
    fn linear_j_into_open_fold_does_not_skip() {
        // Open folds don't hide content; j moves one line as usual.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: false,
            identity: None,
        });
        a.editor.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(a.editor.cursor.line, 2);
    }

    #[test]
    fn linear_motion_with_nofoldenable_does_not_skip() {
        // `:set nofoldenable` / `zi` should make every line visible
        // for navigation, including closed-fold interiors.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.set_foldenable_for_test(false);
        a.editor.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(a.editor.cursor.line, 2);
    }

    fn line_down_lands_on_next_fold_heading_when_consecutive() {
        // Three closed folds back-to-back: 1..=3, 4..=6, 7..=9.
        // Each `j` moves one visible line; a closed fold's heading
        // IS a visible line. So:
        //   line 1 (fold A heading) --j--> line 4 (fold B heading)
        //   line 4 (fold B heading) --j--> line 7 (fold C heading)
        //   line 7 (fold C heading) --j--> line 10 (after fold C)
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 3,
            closed: true,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 4,
            end_line: 6,
            closed: true,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 7,
            end_line: 9,
            closed: true,
            identity: None,
        });
        a.editor.cursor = Position::new(1, 0);
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(a.editor.cursor.line, 4, "first j → fold B heading");
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(a.editor.cursor.line, 7, "second j → fold C heading");
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(a.editor.cursor.line, 10, "third j → past fold C");
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
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.editor.cursor = Position::new(0, 0);
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        // Without snap the cursor would land on "bravo" (line 0,
        // byte 6) -- still visible. Press w again: would go into
        // hidden `charlie`. The snap kicks in there.
        a.apply(invoke_motion(a.editor.builtins.word_forward));
        assert!(
            !a.line_inside_closed_fold(a.editor.cursor.line),
            "w must not leave cursor inside a hidden fold body \
             (cursor.line = {})",
            a.editor.cursor.line
        );
    }

    // display-line B4.2: `refresh_highlights_covers_buffer_lines_below_a_closed_fold`
    // was deleted here. It asserted through the now-removed
    // `App::refresh_highlights` + `highlights_for_buffer_line` span
    // readers (the dead span/row cache). The fold-aware window
    // stretch it guarded (`Editor::fold_aware_highlight_end_line` →
    // `SyntaxRenderState::end_line_override`) still exists and is
    // exercised by the overlay worker's `recompute_honours_*` path;
    // syntax styling under folds now flows through the cells /
    // `DisplayMatrix` substrate, covered by the cells worker tests.

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
        for f in &a.editor.folds {
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
        a.editor.cursor = Position::new(1, 0);
        a.apply(Action::CloseFoldAtCursor);
        let fold = a
            .fold_start_at(1)
            .expect("a closed fold should start at line 1");
        let count = fold.end_line - fold.start_line + 1;
        assert_eq!(
            count, 5,
            "indented if/else fold must span 5 lines (got {count}; fold = {fold:?}; all = {:?})",
            a.editor.folds
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
        a.editor.cursor = Position::new(0, 0);
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
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.editor.cursor = Position::new(0, 0);
        a.apply(invoke_motion(a.editor.builtins.paragraph_forward));
        assert!(
            !a.line_inside_closed_fold(a.editor.cursor.line),
            "}} must not leave cursor inside a hidden fold body \
             (cursor.line = {})",
            a.editor.cursor.line
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
        let src =
            "impl B {\n    fn a() {\n    }\n\n    fn b() {\n    }\n\n    fn c() {\n    }\n}\n";
        let mut a = app_with(src, 20);
        // Outer impl + three function folds (skip blank-line 3 / 6).
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 9,
            closed: false,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 2,
            closed: true,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 4,
            end_line: 5,
            closed: false,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 7,
            end_line: 8,
            closed: false,
            identity: None,
        });
        a.editor.cursor = Position::new(1, 0);
        // j from fn a's heading: snap over fn a's body, swallow the
        // blank, land on fn b's heading (line 4).
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(
            a.editor.cursor.line, 4,
            "j after fold A must skip the blank and land on fn b"
        );
        // Close fn b, j again, land on fn c's heading (line 7).
        a.apply(Action::CloseFoldAtCursor);
        let fnb = a.editor.folds.iter().find(|f| f.start_line == 4).unwrap();
        assert!(fnb.closed, "zc on fn b heading closes fn b");
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(
            a.editor.cursor.line, 7,
            "j after fold B must skip the blank and land on fn c"
        );
        // Close fn c. The outer impl must remain open.
        a.apply(Action::CloseFoldAtCursor);
        let fnc = a.editor.folds.iter().find(|f| f.start_line == 7).unwrap();
        let outer = a.editor.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(fnc.closed, "zc on fn c heading closes fn c, not outer");
        assert!(
            !outer.closed,
            "outer impl must remain open through the sequence"
        );
    }

    #[test]
    fn zc_on_sibling_fold_after_navigating_with_j_closes_sibling_not_parent() {
        // Regression: with one inner fold already closed, `j` from
        // its heading must put the cursor on the sibling's heading
        // (line 5), not inside the closed fold's body. Then `zc`
        // on the sibling closes the sibling -- not the outer.
        let mut a = app_with(&"x\n".repeat(12), 10);
        a.editor.folds.push(Fold {
            start_line: 0,
            end_line: 10,
            closed: false,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 4,
            closed: true,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 5,
            end_line: 9,
            closed: false,
            identity: None,
        });
        a.editor.cursor = Position::new(1, 0);
        // Move to the sibling's heading.
        a.apply(invoke_motion(a.editor.builtins.line_down));
        assert_eq!(
            a.editor.cursor.line, 5,
            "cursor should land on sibling, not interior"
        );
        // Close the sibling.
        a.apply(Action::CloseFoldAtCursor);
        let sibling = a.editor.folds.iter().find(|f| f.start_line == 5).unwrap();
        let outer = a.editor.folds.iter().find(|f| f.start_line == 0).unwrap();
        assert!(sibling.closed, "sibling should close, not the outer");
        assert!(!outer.closed, "outer must remain open");
    }

    #[test]
    fn zj_jumps_to_next_fold_start() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        a.editor.folds.push(Fold {
            start_line: 2,
            end_line: 3,
            closed: false,
            identity: None,
        });
        a.editor.folds.push(Fold {
            start_line: 5,
            end_line: 5,
            closed: false,
            identity: None,
        });
        a.editor.cursor = Position::ZERO;
        a.apply(Action::GotoNextFold);
        assert_eq!(a.editor.cursor.line, 2);
    }

    #[test]
    fn zk_jumps_to_previous_fold_end() {
        let mut a = app_with("a\nb\nc\nd\ne\nf", 10);
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 2,
            closed: false,
            identity: None,
        });
        a.editor.cursor = Position::new(5, 0);
        a.apply(Action::GotoPrevFold);
        assert_eq!(a.editor.cursor.line, 2);
    }

    #[test]
    fn zj_with_no_more_folds_emits_error() {
        let mut a = app_with("a\nb\nc", 10);
        a.apply(Action::GotoNextFold);
        let msg = a.editor.last_message.as_ref().unwrap();
        assert_eq!(msg.level, EchoLevel::Error);
    }

    #[test]
    fn line_inside_closed_fold_returns_true_for_interior() {
        let mut a = app_with("a\nb\nc\nd", 10);
        a.editor.folds.push(Fold {
            start_line: 1,
            end_line: 3,
            closed: true,
            identity: None,
        });
        a.editor.publish_render_state();
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
        a.editor.command_line = "set foldmethod=indent".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Indent);
        assert!(!a.editor.folds.is_empty());
        let f = a
            .editor
            .folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("fold");
        assert_eq!(f.end_line, 2);
    }

    #[test]
    fn foldmethod_indent_preserves_closed_state_across_reparse() {
        let mut a = app_with("a:\n    b\n    c\n", 10);
        a.set_foldmethod_for_test(FoldMethod::Indent);
        a.recompute_folds();
        assert_eq!(a.editor.folds.len(), 1);
        // Close the fold.
        a.editor.folds[0].closed = true;
        // Recompute should preserve closed state (same range).
        a.recompute_folds();
        assert!(a.editor.folds[0].closed);
    }

    #[test]
    fn foldmethod_manual_default_does_not_recompute() {
        let mut a = app_with("def f():\n    pass\n", 10);
        a.recompute_folds();
        assert!(a.editor.folds.is_empty());
    }

    #[test]
    fn foldmethod_markdown_populates_folds_from_atx_headings() {
        let mut a = app_with("# H1\nbody\nmore body\n", 10);
        a.editor.command_line = "set foldmethod=markdown".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Markdown);
        assert!(!a.editor.folds.is_empty());
        let f = a
            .editor
            .folds
            .iter()
            .find(|f| f.start_line == 0)
            .expect("fold");
        assert!(f.end_line >= 2);
    }

    #[test]
    fn foldmethod_syntax_cascades_to_indent_when_no_md_extension() {
        // Plain-text buffer (no `Syntax`): syntax provider returns
        // None and we cascade to indent.
        let mut a = app_with("def f():\n    pass\n    pass\n", 10);
        a.editor.command_line = "set foldmethod=syntax".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Syntax);
        assert!(
            a.editor
                .folds
                .iter()
                .any(|f| f.start_line == 0 && f.end_line == 2)
        );
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
        a.editor.command_line = "set foldmethod=syntax".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Syntax);
        // Tree-sitter fold for the struct (lines 0..=2).
        assert!(
            a.editor
                .folds
                .iter()
                .any(|f| f.start_line == 0 && f.end_line >= 2),
            "expected struct fold from tree-sitter: {:?}",
            a.editor.folds
        );
        // Tree-sitter fold for the impl (starts at line 4).
        assert!(
            a.editor.folds.iter().any(|f| f.start_line == 4),
            "expected impl fold from tree-sitter: {:?}",
            a.editor.folds
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
            .editor
            .folds
            .iter()
            .position(|f| f.start_line == 3)
            .expect("second: fold exists");
        a.editor.folds[second_idx].closed = true;
        // Insert a new line inside the first function (between `a` and `b`).
        a.apply_edit_blocking(Edit::insert(Position::new(2, 0), "    extra\n"))
            .unwrap();
        a.recompute_folds();
        // The recomputed `second:` fold has start_line = 4 now, but
        // its identity (heading text "second:" + indent 0) matches.
        let new_second = a
            .editor
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
        a.editor.command_line = "set foldmethod=bogus".into();
        a.editor.modal = ModalState::Command;
        a.apply(Action::CommandLineSubmit);
        assert_eq!(a.foldmethod(), FoldMethod::Manual);
        assert!(a.editor.last_message.is_some());
    }
}
