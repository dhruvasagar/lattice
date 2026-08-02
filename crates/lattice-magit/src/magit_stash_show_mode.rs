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

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::Effect;
use lattice_mode::{
    BufferStoreHandle, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, OptionOverrideSet,
};
use lattice_protocol::position::Position;

use crate::buffer_state::{BufferStateGuard, BufferStates};
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

/// MG.23g: what `a` / `-` need to know about this buffer — where the
/// repository is. The stash index is already in the buffer name; the
/// working directory is not, and a `git apply` needs one.
pub struct StashShowState {
    workdir: std::path::PathBuf,
    /// MG.22: which stash, so `<CR>` can name the ref it opens the
    /// file from. The buffer name carries it too, but a view that had
    /// to re-parse the name would make that format load-bearing in a
    /// second place — the drift that left every stash chord dead until
    /// MG.15.
    index: Option<usize>,
}

/// Service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type StashShowStatesHandle = Arc<BufferStates<StashShowState>>;

/// MG.23g: this buffer's [`MagitView`] — the stash peer of
/// `magit-revision-mode`'s, and identical for the same reason: a
/// stash's patch describes a change that is not sitting between two of
/// this checkout's trees, so `s` / `u` cannot move it and `a` / `-`
/// can put it into the working tree or take it back out.
///
/// This is hunk-level, which is what makes it different from the stash
/// list's own `a` (apply the *whole* stash): one hunk of a stash,
/// where the list's key takes all of it.
struct StashShowView(Arc<Mutex<StashShowState>>);

impl crate::buffer_state::MagitView for StashShowView {
    /// This buffer's content is a unified diff, so "a file" is a
    /// `diff --git` header — not the generic indented-row scan, which
    /// here matches every indented CONTEXT line and would walk `]f`
    /// through arbitrary code.
    fn file_lines(
        &self,
        store: &lattice_mode::BufferStoreHandle,
        buffer: lattice_core::BufferId,
    ) -> Option<Vec<u32>> {
        Some(crate::magit_core_mode::diff_file_lines(store, buffer))
    }

    /// `stash@{n}`'s patch does not change under a fixed index, so
    /// there is nothing for `gr` to rebuild — see this module's header
    /// for why the stash *list* is the thing that refreshes.
    fn refresh(&self) -> Option<Effect> {
        None
    }

    fn diff_source(&self, _cursor: Position) -> Option<crate::buffer_state::DiffSource> {
        Some(crate::buffer_state::DiffSource::Committed)
    }

    /// MG.22: the file as this stash left it. `git show
    /// stash@{n}:<path>` is a real revspec, so the existing
    /// `magit-file-revision-mode` opens it with no new machinery — the
    /// ref simply is not a sha.
    fn diff_target(&self, path: &std::path::Path) -> Option<Effect> {
        let idx = self.0.lock().ok()?.index?;
        Some(Effect::OpenSyntheticBuffer {
            name: format!("*magit:file:stash@{{{idx}}}:{}*", path.display()),
            mode_id: "magit-file-revision-mode".to_string(),
        })
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        self.0.lock().ok().map(|g| g.workdir.clone())
    }
}

impl Mode for MagitStashShowMode {
    /// MG.23g: was `Option<HeaderlineRegistration>` — the per-buffer
    /// state and the view registration join it now that `a` / `-` need
    /// somewhere to read this buffer's workdir from.
    type Guard = BufferStateGuard<StashShowState>;

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
            let orphan = || BufferStateGuard::new(Arc::new(BufferStates::default()), buffer_id);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(orphan());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(orphan());
            };
            let workdir = crate::workdir::magit_workdir().unwrap_or_default();

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

            // MG.13: publish BEFORE the first `.await` — see the note
            // in `magit_branch_mode::on_activate`.
            let Some(states) = ctx.service::<StashShowStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                StashShowState {
                    workdir: workdir.clone(),
                    index,
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            // MG.23g: without the view, `a` / `-` have nothing to ask
            // about this buffer and refuse in it.
            if let Some(views) = ctx.service::<crate::buffer_state::MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(StashShowView(state.clone())));
                guard = guard.with_views((*views).clone());
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
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ph) = ctx.service::<lattice_mode::PendingSyntheticHighlights>() {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
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
