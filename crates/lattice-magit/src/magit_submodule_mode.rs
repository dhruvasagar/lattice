//! MG.21i: magit-submodule major mode.
//!
//! Lists the configured submodules with git's own status marker, the
//! recorded commit, and the path: `a` add, `u` update, `s` sync,
//! `d` remove (asks).
//!
//! **A buffer, and here magit agrees.** Unlike remote management
//! (§4.6b, where we diverge), magit itself renders submodules as a
//! buffer — `magit-list-submodules` opens `*Modules*`. Its `o`
//! transient carries the *operations*; the list is a buffer in both.
//! So this is the UX-convention rule and paramount goal #3 pointing
//! the same way, and the shape is `magit-remote-mode`'s.
//!
//! The one genuinely destructive row is `d`: removing a submodule
//! deletes its whole working tree, including anything uncommitted
//! inside it, and git keeps no copy. That is §12.13's ask/execute
//! contract, unlike `magit-remote-mode`'s `d`, which is recoverable
//! and therefore does not ask.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::position::Position;
use lattice_vcs::{Repository, Submodule, SubmoduleEntry};

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline::{self, Field, MagitHeaderlineHandle};

pub struct MagitSubmoduleMode;

impl MagitSubmoduleMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-submodule-mode")
    }
}

/// The chords.
///
/// Magit's `o` transient uses `a` add, `u` update, `s` synchronize,
/// `d` unpopulate and `k` remove, plus `p` populate and `r` register.
/// Four carry over unchanged. Two changes, both for reasons already
/// settled elsewhere:
///
/// - **`k` does not carry over** — it is the up-motion
///   (`feedback_magit_keys_follow_evil_magit`). Removal lands on `d`,
///   which is what magit-branch, magit-stash and magit-remote already
///   mean by "remove the row under the cursor". Magit's own `d`
///   (unpopulate) is not offered: it is `update`'s inverse and nobody
///   reaches for it daily, and offering it *and* remove on adjacent
///   keys would make the destructive one easy to hit by accident.
/// - **`p` populate and `r` register are folded into `u`**, which runs
///   `submodule update --init --recursive` — the command that subsumes
///   both. Three keys for one intent is three chances to pick the
///   wrong one.
fn magit_submodule_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "a", doc: "Add a submodule", cmd: "action:magit-submodule-add" },
            keymap_entry! { mode: Normal, chord: "u", doc: "Update (init + checkout) the submodule at cursor", cmd: "action:magit-submodule-update" },
            keymap_entry! { mode: Normal, chord: "s", doc: "Sync the submodule at cursor's URL", cmd: "action:magit-submodule-sync" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Remove the submodule at cursor (asks first)", cmd: "action:magit-submodule-remove" },
        ]
    })
}

pub struct SubmoduleState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    headerline: Option<MagitHeaderlineHandle>,
    /// The submodules as last rendered, in render order — the cursor
    /// mapping reads this rather than re-parsing the line. Same
    /// reasoning as `magit_remote_mode`: the heading row would
    /// otherwise decode as a submodule named `Submodules`.
    entries: Vec<SubmoduleEntry>,
}

pub type SubmoduleStatesHandle = Arc<BufferStates<SubmoduleState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<SubmoduleState>>> {
    crate::buffer_state::state_for::<SubmoduleState>(ctx)
}

struct SubmoduleView(Arc<Mutex<SubmoduleState>>);

impl MagitView for SubmoduleView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }

    fn workdir(&self) -> Option<std::path::PathBuf> {
        self.0.lock().ok().map(|g| g.workdir.clone())
    }
}

impl Mode for MagitSubmoduleMode {
    type Guard = BufferStateGuard<SubmoduleState>;

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
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_submodule_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // NO `<CR>`, and the reason is a real limitation rather
            // than an oversight. The obvious binding is "open this
            // submodule's own magit-status" — but `workdir::magit_workdir`
            // discovers from the PROCESS's current directory, so every
            // magit buffer in the editor is bound to one repository.
            // There is no way to point a status buffer at a
            // subdirectory today, and inventing a per-buffer workdir
            // belongs in its own slice (it is the same thing magit's
            // `Z` worktree rows will need). A chord that opened the
            // superproject's status while claiming to open the
            // submodule's would be worse than no chord.
            //
            // add (a) — two prompts, git's own argument order: URL,
            // then the path to put it at.
            ActionHandlerContribution {
                action_name: "action:magit-submodule-add",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let _ = state(ctx)?;
                    Some(Effect::OpenPrompt {
                        prompt: "Submodule URL: ".to_string(),
                        initial: String::new(),
                        on_submit_action: "action:magit-submodule-add-path".to_string(),
                        buffer_name: None,
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-submodule-add-path",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let url = ctx.prompt_value?.trim().to_string();
                    if url.is_empty() {
                        return None;
                    }
                    Some(Effect::OpenPrompt {
                        prompt: format!("Path for {url}: "),
                        // The last path segment, minus `.git`, is what
                        // git itself would default to.
                        initial: default_path_for(&url),
                        on_submit_action: "action:magit-submodule-add-finish".to_string(),
                        buffer_name: Some(add_prompt_buffer_name(&url)),
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-submodule-add-finish",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let path = ctx.prompt_value?.trim().to_string();
                    let url = carried_url(ctx)?;
                    if path.is_empty() {
                        return None;
                    }
                    let what = format!("add {path}");
                    spawn_submodule_mutation(ctx, what, move |repo| {
                        Submodule::add(repo, &url, &path)
                    })?;
                    Some(Effect::Echo {
                        level: EchoLevel::Info,
                        text: "cloning submodule\u{2026}".to_string(),
                    })
                }),
            },
            // update (u) — reaches the network for a submodule that has
            // never been cloned, so it echoes.
            ActionHandlerContribution {
                action_name: "action:magit-submodule-update",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let entry = entry_at_cursor(ctx)?;
                    let path = entry.path.clone();
                    let echo = format!("updating {path}\u{2026}");
                    let what = format!("update {path}");
                    spawn_submodule_mutation(ctx, what, move |repo| {
                        Submodule::update(repo, Some(&path))
                    })?;
                    Some(Effect::Echo {
                        level: EchoLevel::Info,
                        text: echo,
                    })
                }),
            },
            // sync (s) — local, so no echo; the list is the feedback.
            ActionHandlerContribution {
                action_name: "action:magit-submodule-sync",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let entry = entry_at_cursor(ctx)?;
                    let path = entry.path.clone();
                    let what = format!("sync {path}");
                    spawn_submodule_mutation(ctx, what, move |repo| {
                        Submodule::sync(repo, Some(&path))
                    })?;
                    None
                }),
            },
            // remove (d) — asks, and the ask half performs no git call
            // at all. Unlike magit-remote's `d`, this one deletes a
            // whole working tree that git keeps no copy of.
            ActionHandlerContribution {
                action_name: "action:magit-submodule-remove",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let entry = entry_at_cursor(ctx)?;
                    Some(remove_confirm(&entry.path))
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-submodule-remove-execute",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    // IX.2: act on the submodule the prompt named. The
                    // cursor is consulted only when nothing was
                    // carried — a refresh can rebuild the list while
                    // the dialog is open, and then the row means a
                    // different submodule.
                    let path = match crate::confirm::carried_target(ctx) {
                        Some(carried) => carried,
                        None => entry_at_cursor(ctx)?.path,
                    };
                    let what = format!("remove {path}");
                    spawn_submodule_mutation(ctx, what, move |repo| {
                        Submodule::remove(repo, &path)
                    })?;
                    None
                }),
            },
        ]
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
            let pending_highlights = ctx.service::<lattice_mode::PendingSyntheticHighlights>();

            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: publish BEFORE the first `.await`.
            let Some(states) = ctx.service::<SubmoduleStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                SubmoduleState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    pending_highlights: pending_highlights.clone(),
                    headerline: hl.clone(),
                    entries: Vec::new(),
                },
            );
            let mut guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);
            if let Some(views) = ctx.service::<MagitViewsHandle>() {
                views.publish(buffer_id, Arc::new(SubmoduleView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            let wd = workdir.clone();
            let (text, header, entries) =
                tokio::task::spawn_blocking(move || build_submodule_list(&wd))
                    .await
                    .unwrap_or_else(|_| (String::new(), Vec::new(), Vec::new()));
            if let Ok(mut g) = state.lock() {
                g.entries = entries;
            }
            headerline::publish(&hl, header);
            let spans = crate::highlight::submodule_styled_spans(&text);
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

fn refresh(s: Arc<Mutex<SubmoduleState>>) -> Option<Effect> {
    let (handle, wd, pending, buffer_id, hl) = {
        let g = s.lock().ok()?;
        (
            g.store.handle_for(g.buffer_id)?,
            g.workdir.clone(),
            g.pending_highlights.clone(),
            g.buffer_id,
            g.headerline.clone(),
        )
    };
    // MG.27: the row says "refreshing" from here until the
    // guard drops — including on every early exit inside the
    // task, which is why it is a guard and not a matching pair.
    let busy = headerline::busy(&hl);
    tokio::task::spawn(async move {
        let _busy = busy;
        let (text, header, entries) =
            tokio::task::spawn_blocking(move || build_submodule_list(&wd))
                .await
                .unwrap_or_else(|_| (String::new(), Vec::new(), Vec::new()));
        if let Ok(mut g) = s.lock() {
            g.entries = entries;
        }
        headerline::publish(&hl, header);
        let spans = crate::highlight::submodule_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Run one submodule git call off the actor thread, then re-list every
/// live submodule buffer.
///
/// Reached through the service rather than `ctx.buffer_id` for the same
/// reason `magit_remote_mode` does it: the `-finish` halves fire in the
/// PROMPT buffer, and the confirm's execute half fires wherever the
/// confirm transient left focus.
fn spawn_submodule_mutation(
    ctx: &ActionContext<'_>,
    what: String,
    mutate: impl FnOnce(&Repository) -> lattice_vcs::Result<()> + Send + 'static,
) -> Option<()> {
    let states = ctx.services.get::<SubmoduleStatesHandle>()?;
    let targets = states.all();
    let workdir = crate::workdir::magit_workdir().unwrap_or_default();
    // No guard here: this task's own refreshes are what the
    // user sees, and each `refresh(target)` below raises and
    // clears the busy flag on its OWN row. Marking busy here
    // too would need a second guard per target and would clear
    // on a different schedule.
    tokio::task::spawn(async move {
        let wd = workdir.clone();
        let outcome = tokio::task::spawn_blocking(move || match Repository::discover(&wd) {
            Ok(repo) => mutate(&repo),
            Err(e) => Err(lattice_vcs::VcsError::Submodule(format!(
                "no repository at {}: {e}",
                wd.display()
            ))),
        })
        .await;
        match outcome {
            Ok(Ok(())) => tracing::info!("magit-submodule: {what}"),
            Ok(Err(e)) => tracing::error!("magit-submodule: {what} failed: {e}"),
            Err(e) => tracing::error!("magit-submodule: {what} panicked: {e}"),
        }
        for target in targets {
            refresh(target);
        }
    });
    Some(())
}

/// MG.21i: the ask half of `d`. Names the submodule and says what is
/// lost, because the working tree it deletes may hold uncommitted work
/// git has no copy of.
pub(crate) fn remove_confirm(path: &str) -> Effect {
    crate::confirm::ask_target(
        format!("Remove submodule {path}? Its working tree is deleted."),
        "action:magit-submodule-remove-execute",
        path,
    )
}

const ADD_PREFIX: &str = "*magit:submodule-add:";

pub(crate) fn add_prompt_buffer_name(url: &str) -> String {
    format!("{ADD_PREFIX}{url}*")
}

pub(crate) fn url_from_prompt_buffer_name(buffer_name: &str) -> Option<String> {
    let s = buffer_name.strip_prefix(ADD_PREFIX)?;
    let s = s.strip_suffix('*')?;
    (!s.is_empty()).then(|| s.to_string())
}

fn carried_url(ctx: &ActionContext<'_>) -> Option<String> {
    let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
    ctx.services
        .get::<BufferStoreHandle>()?
        .name_for(buffer_id)
        .and_then(|n| url_from_prompt_buffer_name(&n))
}

/// The path `git submodule add` would choose for `url` — its last
/// segment with any `.git` suffix and trailing slash removed.
pub(crate) fn default_path_for(url: &str) -> String {
    url.trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".git")
        .to_string()
}

fn entry_at_cursor(ctx: &ActionContext<'_>) -> Option<SubmoduleEntry> {
    let s = state(ctx)?;
    let g = s.lock().ok()?;
    submodule_at_line(&g.entries, ctx.cursor)
}

/// Map a cursor line onto a rendered submodule. Line 0 is the heading,
/// so row `i` is entry `i - 1`.
pub(crate) fn submodule_at_line(
    entries: &[SubmoduleEntry],
    cursor: Position,
) -> Option<SubmoduleEntry> {
    let index = (cursor.line as usize).checked_sub(1)?;
    entries.get(index).cloned()
}

fn build_submodule_list(workdir: &std::path::Path) -> (String, Vec<Field>, Vec<SubmoduleEntry>) {
    let repo = match Repository::discover(workdir) {
        Ok(r) => r,
        Err(_) => {
            return (
                "Not a git repository.\n".to_string(),
                Vec::new(),
                Vec::new(),
            );
        }
    };
    let entries = match Submodule::list(&repo) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("magit-submodule: listing submodules failed: {e}");
            Vec::new()
        }
    };
    let header = headerline::submodule_fields(&entries);
    (render_submodule_list(&entries), header, entries)
}

/// The buffer text for `entries`.
///
/// `  <marker> <short-sha> <path><describe>` — git's own marker in
/// git's own column, so a row reads the same here as in `git submodule
/// status`. Pure, so the layout the cursor mapping and the styler both
/// depend on is testable without a repository.
pub(crate) fn render_submodule_list(entries: &[SubmoduleEntry]) -> String {
    if entries.is_empty() {
        return "No submodules.\n".to_string();
    }
    let width = entries
        .iter()
        .map(|e| e.path.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!("Submodules ({})\n", entries.len());
    for e in entries {
        let short: String = e.sha.chars().take(7).collect();
        let pad = width.saturating_sub(e.path.chars().count());
        out.push_str(&format!(
            "  {} {}  {}{}",
            e.state.marker(),
            short,
            e.path,
            " ".repeat(pad)
        ));
        if !e.describe.is_empty() {
            out.push_str(&format!("  ({})", e.describe));
        }
        // Trailing padding with no describe would be invisible
        // whitespace; trim it back off.
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_vcs::SubmoduleState as St;

    fn entry(state: St, sha: &str, path: &str, describe: &str) -> SubmoduleEntry {
        SubmoduleEntry {
            state,
            sha: sha.into(),
            path: path.into(),
            describe: describe.into(),
        }
    }

    fn at(line: u32) -> Position {
        Position { line, byte: 0 }
    }

    #[test]
    fn the_heading_row_maps_to_no_submodule() {
        let entries = vec![entry(St::InSync, "abc1234567", "vendor/x", "")];
        assert!(submodule_at_line(&entries, at(0)).is_none());
    }

    #[test]
    fn each_row_maps_to_its_own_submodule() {
        let entries = vec![
            entry(St::InSync, "aaa1111111", "vendor/a", ""),
            entry(St::Modified, "bbb2222222", "vendor/b", ""),
        ];
        assert_eq!(submodule_at_line(&entries, at(1)).unwrap().path, "vendor/a");
        assert_eq!(submodule_at_line(&entries, at(2)).unwrap().path, "vendor/b");
    }

    #[test]
    fn out_of_range_rows_map_to_nothing() {
        let entries = vec![entry(St::InSync, "abc1234567", "vendor/x", "")];
        assert!(submodule_at_line(&entries, at(2)).is_none());
        assert!(submodule_at_line(&[], at(1)).is_none());
    }

    #[test]
    fn a_row_carries_gits_marker_the_short_sha_and_the_path() {
        let text = render_submodule_list(&[
            entry(St::Uninitialised, "abc1234567890", "vendor/a", ""),
            entry(St::InSync, "def1234567890", "vendor/bb", "v1.2.3"),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Submodules (2)");
        assert_eq!(lines[1], "  - abc1234  vendor/a");
        assert_eq!(lines[2], "    def1234  vendor/bb  (v1.2.3)");
    }

    #[test]
    fn no_submodules_renders_a_sentence_not_an_empty_buffer() {
        assert_eq!(render_submodule_list(&[]), "No submodules.\n");
    }

    /// The rendered rows and the cursor mapping must agree, or a chord
    /// acts on a submodule other than the one under the cursor — and
    /// one of these chords deletes a working tree.
    #[test]
    fn the_rendered_row_index_matches_the_cursor_mapping() {
        let entries = vec![
            entry(St::InSync, "aaa1111111", "a", ""),
            entry(St::Modified, "bbb2222222", "bb", "v1"),
            entry(St::Conflicted, "ccc3333333", "ccc", ""),
        ];
        let text = render_submodule_list(&entries);
        for (i, line) in text.lines().enumerate().skip(1).take(entries.len()) {
            let mapped = submodule_at_line(&entries, at(i as u32)).expect("row maps");
            assert!(
                line.contains(&mapped.path),
                "line {i} ({line:?}) does not name the mapped submodule {:?}",
                mapped.path
            );
        }
    }

    #[test]
    fn remove_asks_before_deleting_and_names_the_submodule() {
        match remove_confirm("vendor/child") {
            Effect::Confirm {
                prompt, yes_action, ..
            } => {
                assert!(prompt.contains("vendor/child"), "{prompt}");
                assert!(
                    prompt.contains("working tree is deleted"),
                    "the question must say what is lost: {prompt}"
                );
                assert_eq!(yes_action, "action:magit-submodule-remove-execute");
            }
            other => panic!("expected a confirm before a destructive remove, got {other:?}"),
        }
    }

    #[test]
    fn the_add_prompt_seeds_the_path_git_would_have_chosen() {
        assert_eq!(default_path_for("https://example.com/foo/bar.git"), "bar");
        assert_eq!(default_path_for("git@example.com:foo/bar.git"), "bar");
        assert_eq!(default_path_for("https://example.com/foo/bar/"), "bar");
        assert_eq!(default_path_for("../sibling"), "sibling");
    }

    #[test]
    fn the_add_prompt_carries_its_url_and_nothing_elses() {
        let name = add_prompt_buffer_name("https://example.com/x.git");
        assert_eq!(
            url_from_prompt_buffer_name(&name).as_deref(),
            Some("https://example.com/x.git")
        );
        assert!(url_from_prompt_buffer_name("*magit:status*").is_none());
        assert!(url_from_prompt_buffer_name(ADD_PREFIX).is_none());
    }
}
