//! MG.15: `*magit:stash:<n>*` — one stash's patch.
//!
//! magit-stash listed stashes but had no `<CR>`: the only way to see
//! what a stash actually contained was magit-status, where `<CR>`
//! toggles the patch inline among the other sections. That was the
//! last hole in MG.11's `<CR>` uniformity rule — every other list view
//! navigates to a detail buffer, and "apply this to my working tree?"
//! is exactly the question you want answered before pressing `a`.
//!
//! magit-status's inline toggle is deliberately left alone: there a
//! stash is one row among many and expanding in place keeps the
//! surrounding context. Here the stash IS the subject.
//!
//! Fixed-content view, like `magit-revision-mode`: `gr` is a no-op
//! because `stash@{n}`'s patch does not change under a fixed index.
//! (Dropping or popping the stash renumbers the *other* entries —
//! which is why the buffer name carries the index it was opened at,
//! and why the stash list is the place that refreshes, not this.)

use std::sync::OnceLock;

use lattice_config;
use lattice_mode::{
    BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_vcs::Repository;

use crate::headerline;

pub struct MagitStashShowMode;

impl MagitStashShowMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-stash-show-mode")
    }
}

fn magit_stash_show_keymap_entries() -> &'static [KeymapEntry] {
    // No mode-specific chords — `q`/`gr`/nav come from magit-core
    // (this mode is in its `ActivationPolicy::Majors` list). Same
    // shape as `magit-file-revision-mode`.
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(Vec::new)
}

impl Mode for MagitStashShowMode {
    /// MG.14: the headerline registration — this mode's only
    /// per-activation resource, like `magit-file-revision-mode`.
    type Guard = Option<crate::headerline::HeaderlineRegistration>;

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
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_stash_show_keymap_entries())
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let workdir = Repository::discover(".")
                .ok()
                .and_then(|r| r.workdir().map(|p| p.to_path_buf()))
                .unwrap_or_default();

            let index = store
                .name_for(buffer_id)
                .and_then(|name| parse_buffer_name(&name));

            // MG.14: the index is in the buffer name, so the header is
            // complete before `git stash show` runs. The message needs
            // the git call and is filled in below.
            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };
            if let Some(idx) = index {
                headerline::publish(&hl, headerline::stash_show_fields(idx, ""));
            }

            let wd = workdir.clone();
            let (text, message) = tokio::task::spawn_blocking(move || match index {
                Some(idx) => (run_stash_show(&wd, idx), run_stash_message(&wd, idx)),
                None => ("No stash given.\n".to_string(), String::new()),
            })
            .await
            .unwrap_or_default();
            if let Some(idx) = index {
                headerline::publish(&hl, headerline::stash_show_fields(idx, &message));
            }

            // `git stash show -p` is a plain unified diff, so the
            // whole-buffer styler applies directly — the same reuse
            // `magit-diff-mode` and `magit-revision-mode` make.
            let spans = crate::highlight::diff_styled_spans(&text);
            let snap = handle.snapshot();
            let last = snap.buffer.line_count().saturating_sub(1);
            let last_line = snap.buffer.line(last).unwrap_or_default();
            let end = Position::new(last, last_line.len() as u32);
            let _ = handle
                .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
                .await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(hl_registration)
        })
    }
}

/// `"*magit:stash:<n>*"` → `n`. Rejects a non-numeric or empty index
/// rather than defaulting to 0 — showing stash@{0}'s patch under a
/// name claiming some other stash is worse than saying nothing.
fn parse_buffer_name(name: &str) -> Option<usize> {
    let rest = name.strip_prefix("*magit:stash:")?;
    let rest = rest.strip_suffix('*')?;
    rest.parse().ok()
}

/// The buffer name for stash `index` — the single place the
/// `<CR>` handler and this mode's parser agree on the format.
pub fn buffer_name(index: usize) -> String {
    format!("*magit:stash:{index}*")
}

fn run_stash_show(workdir: &std::path::Path, index: usize) -> String {
    let spec = format!("stash@{{{index}}}");
    std::process::Command::new("git")
        .args(["stash", "show", "-p", &spec])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .filter(|s| !s.trim().is_empty())
        // A stash whose patch is empty is a real outcome (a stash of
        // untracked files only, shown without `-u`), and so is a bad
        // index after a concurrent drop. Both read better as a
        // sentence than as a blank buffer.
        .unwrap_or_else(|| format!("No patch to show for {spec}.\n"))
}

/// The stash's own subject line, for the headerline. Separate from
/// the patch so a failure in one does not blank the other.
fn run_stash_message(workdir: &std::path::Path, index: usize) -> String {
    let spec = format!("stash@{{{index}}}");
    std::process::Command::new("git")
        .args(["log", "-1", "--format=%s", &spec])
        .current_dir(workdir)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_buffer_name_extracts_the_index() {
        assert_eq!(parse_buffer_name("*magit:stash:0*"), Some(0));
        assert_eq!(parse_buffer_name("*magit:stash:12*"), Some(12));
    }

    #[test]
    fn parse_buffer_name_rejects_malformed_names() {
        assert_eq!(parse_buffer_name("*magit:stash:*"), None);
        assert_eq!(parse_buffer_name("*magit:stash:abc*"), None);
        assert_eq!(parse_buffer_name("*magit:stash*"), None);
        assert_eq!(parse_buffer_name("*magit:commit:a1b2c3d*"), None);
    }

    /// The name the `<CR>` handler builds must be the name this mode
    /// parses. They live in different files; this is the seam that
    /// would otherwise drift into a buffer that opens empty.
    #[test]
    fn the_name_builder_and_the_parser_agree() {
        for idx in [0usize, 3, 42] {
            assert_eq!(parse_buffer_name(&buffer_name(idx)), Some(idx));
        }
    }
}
