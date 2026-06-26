//! D-fix.5 (2026-06-26): diff-presentation options, owned by the diff
//! subsystem ([[feedback_mode_owns_its_surface]]) rather than host
//! core. Self-register via `linkme` like every other `options!` block;
//! the host's `init_from_linkme()` walks the global slice at boot, so
//! linking `lattice-diff` into the binary (it always is — `install`
//! runs in the Phase-B list) picks these up automatically.
//!
//! Both drive the universal `UnchangedFoldSource` (vimdiff
//! `foldmethod=diff` + `diffopt context:N`): in any diff session the
//! unchanged code between hunks is folded away so only the changes (±
//! `context` lines) show, on BOTH sides in lockstep.
//!
//! Bound to the `display` group (visual-presentation toggles, next to
//! line numbers / wrap / whitespace). User customizes via
//! `:set ui.diff.context=3` / `:set ui.diff.fold-unchanged` /
//! `:customize display`.

/// Validator for `ui.diff.context`: a non-negative line count. `0`
/// keeps zero context (only the changed lines show); the upper bound
/// guards against a fat-fingered value that would fold nothing
/// (`context >= file length` ⇒ the whole file is "kept").
fn validate_diff_context(n: &i64) -> Result<(), String> {
    if *n >= 0 && *n <= 1000 {
        Ok(())
    } else {
        Err(format!("ui.diff.context must be in range [0, 1000], got {n}"))
    }
}

lattice_config::options! {
    group = lattice_config::Display;

    /// Fold the unchanged regions of a diff so only the changes (±
    /// [`UiDiffContext`] lines) remain visible — vimdiff's
    /// `foldmethod=diff`, VS Code's "Collapse Unchanged Regions".
    ///
    /// `true` (default) — every diff session gets a closed
    /// `UnchangedFoldSource` on each side; `zR` / `zo` expand a region
    /// to read the surrounding context, `zM` re-collapses.
    ///
    /// `false` — diffs open fully expanded (the unchanged code stays
    /// visible). Hunk folds (`za` on a change) are unaffected — those
    /// are a separate, open-by-default source.
    #[name("ui.diff.fold-unchanged")]
    pub UiDiffFoldUnchanged: bool = true;

    /// Number of unchanged context lines to keep visible above and
    /// below each change when [`UiDiffFoldUnchanged`] folds a diff.
    /// Default `6` — vimdiff's `diffopt` `context:6`. Mirrors VS
    /// Code's `diffEditor.hideUnchangedRegions.contextLineCount`.
    ///
    /// `0` collapses right up to the change boundary; larger values
    /// leave more surrounding code visible. Unchanged gaps shorter than
    /// the per-fold floor (2 lines) are never folded regardless, so
    /// tiny inter-hunk gaps stay visible (VS Code's `minimumLineCount`).
    #[name("ui.diff.context")]
    #[validate(validate_diff_context)]
    pub UiDiffContext: i64 = 6;
}
