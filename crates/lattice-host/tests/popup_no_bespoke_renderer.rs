//! PU.6 grep-gate (regression guard): no popup path may reintroduce a
//! bespoke content renderer.
//!
//! PU.1 / PU.1b / PU.2 unified every popup surface (centered help popup,
//! floating hover/signature popup, in-pane help) onto the shared
//! `compose_pane_lines` / per-pane `DisplayMatrix` seam and DELETED the
//! hand-rolled layout: `draw_help_in_pane`, `draw_inactive_help`,
//! `manually_wrap_lines` (TUI), plus the `with_markdown_syntax` precompute
//! and `popup_help_highlights` read path. A popup is now pixel-equivalent
//! to a `:set nonu signcolumn=no wrap` document in a box (K.4,
//! `feedback_buffers_no_special_case`, `feedback_tui_gpui_parity`).
//!
//! This test is the CI grep-gate that keeps those deletions deleted —
//! analogous to the `Effect::*` / `DiffSignKind::*` GPUI-parity grep in the
//! TUI/GPUI-parity rule. It scans every `.rs` under `crates/` for a
//! re-introduced DEFINITION of a deleted bespoke renderer (`fn <name>`),
//! across BOTH renderers, so a kind-specific popup paint can't sneak back
//! in on either the TUI or GPUI side.

use std::fs;
use std::path::{Path, PathBuf};

/// Deleted bespoke popup renderers / highlight precomputes. None may
/// reappear as a function definition anywhere in the workspace.
const DELETED_BESPOKE_RENDERERS: &[&str] = &[
    "draw_help_in_pane",
    "draw_inactive_help",
    "manually_wrap_lines",
    "with_markdown_syntax",
    "popup_help_highlights",
];

/// Collect every `.rs` file under `dir` (skipping any `target/` build dir).
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_bespoke_popup_content_renderer_in_workspace() {
    // CARGO_MANIFEST_DIR = crates/lattice-host → parent = crates/.
    let crates_dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/ dir");
    let mut files = Vec::new();
    collect_rs_files(crates_dir, &mut files);
    assert!(
        files.len() > 50,
        "sanity: expected to scan the whole workspace, found only {} .rs files",
        files.len()
    );

    for file in &files {
        // The guard names the deleted renderers as data — don't gate itself.
        if file.ends_with("popup_no_bespoke_renderer.rs") {
            continue;
        }
        let src = fs::read_to_string(file).unwrap_or_default();
        for name in DELETED_BESPOKE_RENDERERS {
            let definition = format!("fn {name}");
            assert!(
                !src.contains(&definition),
                "PU.6 grep-gate: bespoke popup renderer `{definition}` reappeared in \
                 {} — popup content must compose through the shared `compose_pane_lines` / \
                 per-pane `DisplayMatrix` seam, not a hand-rolled renderer (K.4 / PU.1b).",
                file.display()
            );
        }
    }
}
