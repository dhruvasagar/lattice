//! MG.21c: magit-remote major mode.
//!
//! Lists the configured remotes with their fetch and push URLs, and
//! manages them from the row under the cursor: `a` add, `r` rename,
//! `d` remove, `u` set-url, `p` prune.
//!
//! **Why a buffer and not a transient.** Magit puts remote management
//! on the `M` transient, rendering `remote.<name>.url` as *variable
//! rows* inside the menu. Lattice's transient has no variable-row
//! concept, so a straight port would leave the URLs — the thing you
//! actually open this for — invisible, and every operation would be a
//! name typed blind. A buffer is what makes the list readable, `/`
//! searchable, `y`-yankable and `gr`-refreshable, which is paramount
//! goal #3's everything-is-a-buffer claim rather than a convenience.
//! `M` on the root dispatch opens this buffer (MG.21d).
//!
//! Fetch / pull / push are NOT here. Those are `magit_global_mode`'s
//! `RemoteOp` — long-running network operations with their own flag
//! transients. Everything on this mode is a local config edit, except
//! `p` prune, which is the one row that talks to the network.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::position::Position;
use lattice_vcs::{Remote, RemoteEntry, Repository};

use crate::buffer_state::{BufferStateGuard, BufferStates, MagitView, MagitViewsHandle};
use crate::headerline::{self, Field, MagitHeaderlineHandle};

pub struct MagitRemoteMode;

impl MagitRemoteMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-remote-mode")
    }
}

/// The chords, and why they are these chords.
///
/// Magit's own `magit-remote` transient uses `a` add, `r` rename, `k`
/// remove, `p` prune, and `u` for the url variable row. Four of the
/// five carry over unchanged. **`k` does not**: it is the up-motion,
/// and evil-collection-magit moves every magit `k` off it for exactly
/// that reason (`feedback_magit_keys_follow_evil_magit`). Removal lands
/// on `d` instead — which is also what magit-branch's `d` (delete) and
/// magit-stash's `d` (drop) already mean, so "d removes the row under
/// the cursor" is one rule across all three list buffers rather than a
/// third variant.
///
/// `<CR>` is deliberately unbound. There is no obvious thing to *open*
/// a remote into, and a chord that does nothing is worse than an
/// absent one.
fn magit_remote_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "a", doc: "Add a remote", cmd: "action:magit-remote-add" },
            keymap_entry! { mode: Normal, chord: "r", doc: "Rename the remote at cursor", cmd: "action:magit-remote-rename" },
            keymap_entry! { mode: Normal, chord: "d", doc: "Remove the remote at cursor", cmd: "action:magit-remote-remove" },
            keymap_entry! { mode: Normal, chord: "u", doc: "Set the URL of the remote at cursor", cmd: "action:magit-remote-set-url" },
            keymap_entry! { mode: Normal, chord: "p", doc: "Prune stale branches of the remote at cursor", cmd: "action:magit-remote-prune" },
        ]
    })
}

pub struct RemoteState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    pending_highlights: Option<lattice_mode::PendingSyntheticHighlightsHandle>,
    headerline: Option<MagitHeaderlineHandle>,
    /// The remotes as last rendered, in render order.
    ///
    /// The cursor→remote mapping reads THIS rather than re-parsing the
    /// line under the cursor. Re-parsing would make the header row
    /// ("Remotes (2)") decode to a remote named `Remotes`, which is the
    /// class of bug that reaches git before anything notices.
    entries: Vec<RemoteEntry>,
}

/// MG.13: service alias for this mode's per-buffer state. Register and
/// look up through this exact type (`feedback_servicesregistry_arc_typeid`).
pub type RemoteStatesHandle = Arc<BufferStates<RemoteState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<RemoteState>>> {
    crate::buffer_state::state_for::<RemoteState>(ctx)
}

/// `gr` for a remote buffer — see [`MagitView`].
struct RemoteView(Arc<Mutex<RemoteState>>);

impl MagitView for RemoteView {
    fn refresh(&self) -> Option<Effect> {
        refresh(self.0.clone())
    }
}

impl Mode for MagitRemoteMode {
    type Guard = BufferStateGuard<RemoteState>;

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
        Keymap::from_entries(magit_remote_keymap_entries())
    }

    /// MG.13: registered once at boot, not per activation.
    ///
    /// The `-finish` halves are deliberately NOT gated on [`state`]:
    /// they fire with the prompt buffer's id, where no remote state
    /// exists. They reach their buffers through the service registry
    /// instead ([`BufferStates::all`]), which is context-free.
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            // add (a) — two prompts, magit's own order: name, then URL.
            // The name rides in the second prompt's buffer name because
            // by submit time the prompt buffer is the active one and
            // nothing else still knows it.
            ActionHandlerContribution {
                action_name: "action:magit-remote-add",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let _ = state(ctx)?;
                    Some(Effect::OpenPrompt {
                        prompt: "Remote name: ".to_string(),
                        initial: String::new(),
                        on_submit_action: "action:magit-remote-add-url".to_string(),
                        buffer_name: None,
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-remote-add-url",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let name = ctx.prompt_value?.trim().to_string();
                    // An empty name is a cancel, not a request to add a
                    // remote called "".
                    if name.is_empty() {
                        return None;
                    }
                    Some(Effect::OpenPrompt {
                        prompt: format!("URL for {name}: "),
                        initial: String::new(),
                        on_submit_action: "action:magit-remote-add-finish".to_string(),
                        buffer_name: Some(add_prompt_buffer_name(&name)),
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-remote-add-finish",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let url = ctx.prompt_value?.trim().to_string();
                    let name = carried_name(ctx, ADD_PREFIX)?;
                    if url.is_empty() {
                        return None;
                    }
                    let what = format!("add {name}");
                    spawn_remote_mutation(ctx, what, move |repo| Remote::add(repo, &name, &url))?;
                    None
                }),
            },
            // rename (r) — seeded with the current name so a typo fix
            // is an edit rather than a retype.
            ActionHandlerContribution {
                action_name: "action:magit-remote-rename",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let entry = entry_at_cursor(ctx)?;
                    Some(Effect::OpenPrompt {
                        prompt: format!("Rename {} to: ", entry.name),
                        initial: entry.name.clone(),
                        on_submit_action: "action:magit-remote-rename-finish".to_string(),
                        buffer_name: Some(rename_prompt_buffer_name(&entry.name)),
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-remote-rename-finish",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let to = ctx.prompt_value?.trim().to_string();
                    let from = carried_name(ctx, RENAME_PREFIX)?;
                    // Submitting the seeded value unchanged is a
                    // cancel; git would fail with "remote <x> already
                    // exists", which reads like a bug rather than a
                    // no-op.
                    if to.is_empty() || to == from {
                        return None;
                    }
                    let what = format!("rename {from} to {to}");
                    spawn_remote_mutation(ctx, what, move |repo| Remote::rename(repo, &from, &to))?;
                    None
                }),
            },
            // set-url (u) — seeded with the current fetch URL.
            ActionHandlerContribution {
                action_name: "action:magit-remote-set-url",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let entry = entry_at_cursor(ctx)?;
                    Some(Effect::OpenPrompt {
                        prompt: format!("URL for {}: ", entry.name),
                        initial: entry.fetch_url.clone(),
                        on_submit_action: "action:magit-remote-set-url-finish".to_string(),
                        buffer_name: Some(set_url_prompt_buffer_name(&entry.name)),
                    })
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-remote-set-url-finish",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let url = ctx.prompt_value?.trim().to_string();
                    let name = carried_name(ctx, SET_URL_PREFIX)?;
                    if url.is_empty() {
                        return None;
                    }
                    let what = format!("set-url {name}");
                    spawn_remote_mutation(ctx, what, move |repo| {
                        Remote::set_url(repo, &name, &url)
                    })?;
                    None
                }),
            },
            // remove (d) — no confirmation, deliberately. §12.13 routes
            // *irrecoverable* operations through ask/execute; removing
            // a remote drops config and tracking refs that `a` puts
            // back in two prompts, and the URL it needs is on the row
            // in front of you when you press it. Asking here would
            // train the user to dismiss the prompt that does matter
            // (`Oh`, reset --hard). Magit does not confirm it either.
            ActionHandlerContribution {
                action_name: "action:magit-remote-remove",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let entry = entry_at_cursor(ctx)?;
                    let name = entry.name.clone();
                    let what = format!("remove {name}");
                    spawn_remote_mutation(ctx, what, move |repo| Remote::remove(repo, &name))?;
                    None
                }),
            },
            // prune (p) — the one row here that talks to the network,
            // so it echoes: unlike the others there is nothing on
            // screen to change until it returns, and it can take as
            // long as a fetch.
            ActionHandlerContribution {
                action_name: "action:magit-remote-prune",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let entry = entry_at_cursor(ctx)?;
                    let name = entry.name.clone();
                    let echo = format!("pruning {name}\u{2026}");
                    let what = format!("prune {name}");
                    spawn_remote_mutation(ctx, what, move |repo| Remote::prune(repo, &name))?;
                    Some(Effect::Echo {
                        level: EchoLevel::Info,
                        text: echo,
                    })
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

            // MG.13: publish BEFORE the first `.await` — see
            // magit_branch_mode's note on the dead-chord window.
            let Some(states) = ctx.service::<RemoteStatesHandle>() else {
                return Ok(orphan());
            };
            let state = states.publish(
                buffer_id,
                RemoteState {
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
                views.publish(buffer_id, Arc::new(RemoteView(state.clone())));
                guard = guard.with_views((*views).clone());
            }

            let wd = workdir.clone();
            let (text, header, entries) =
                tokio::task::spawn_blocking(move || build_remote_list(&wd))
                    .await
                    .unwrap_or_else(|_| (String::new(), Vec::new(), Vec::new()));
            if let Ok(mut g) = state.lock() {
                g.entries = entries;
            }
            headerline::publish(&hl, header);
            let spans = crate::highlight::remote_styled_spans(&text);
            crate::buffer_io::replace_buffer_text(&handle, text).await;
            if let Some(ref ph) = pending_highlights {
                ph.store_and_wake(buffer_id, spans);
            }

            Ok(guard)
        })
    }
}

/// `gr` — re-list remotes without a prior mutation.
fn refresh(s: Arc<Mutex<RemoteState>>) -> Option<Effect> {
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
        let (text, header, entries) = tokio::task::spawn_blocking(move || build_remote_list(&wd))
            .await
            .unwrap_or_else(|_| (String::new(), Vec::new(), Vec::new()));
        if let Ok(mut g) = s.lock() {
            g.entries = entries;
        }
        headerline::publish(&hl, header);
        let spans = crate::highlight::remote_styled_spans(&text);
        crate::buffer_io::replace_buffer_text(&handle, text).await;
        if let Some(ph) = pending {
            ph.store_and_wake(buffer_id, spans);
        }
    });
    None
}

/// Run one remote-management git call off the actor thread, then
/// re-list every live remote buffer.
///
/// Reaching the buffers through the service rather than through
/// `ctx.buffer_id` is what lets the `-finish` handlers — which fire in
/// the *prompt* buffer — put their result on screen. The chord-fired
/// handlers go through the same path so there is one shape, not two.
///
/// Returns `Some(())` when the work was scheduled, so callers can `?`
/// it and then decide what to echo.
fn spawn_remote_mutation(
    ctx: &ActionContext<'_>,
    what: String,
    mutate: impl FnOnce(&Repository) -> lattice_vcs::Result<()> + Send + 'static,
) -> Option<()> {
    let states = ctx.services.get::<RemoteStatesHandle>()?;
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
            Err(e) => Err(lattice_vcs::VcsError::Remote(format!(
                "no repository at {}: {e}",
                wd.display()
            ))),
        })
        .await;
        // There is no synchronous path back to the echo area from a
        // detached task (§4.6) — the log is the report, and the
        // refresh below is what the user actually sees.
        match outcome {
            Ok(Ok(())) => tracing::info!("magit-remote: {what}"),
            Ok(Err(e)) => tracing::error!("magit-remote: {what} failed: {e}"),
            Err(e) => tracing::error!("magit-remote: {what} panicked: {e}"),
        }
        for target in targets {
            refresh(target);
        }
    });
    Some(())
}

const ADD_PREFIX: &str = "*magit:remote-add:";
const RENAME_PREFIX: &str = "*magit:remote-rename:";
const SET_URL_PREFIX: &str = "*magit:remote-set-url:";

pub(crate) fn add_prompt_buffer_name(name: &str) -> String {
    format!("{ADD_PREFIX}{name}*")
}
pub(crate) fn rename_prompt_buffer_name(name: &str) -> String {
    format!("{RENAME_PREFIX}{name}*")
}
pub(crate) fn set_url_prompt_buffer_name(name: &str) -> String {
    format!("{SET_URL_PREFIX}{name}*")
}

/// Decode the remote name a prompt was opened for out of the prompt
/// buffer's own name — the carrier `magit_global_mode`'s rename and
/// checkout prompts already use.
pub(crate) fn name_from_prompt_buffer_name(buffer_name: &str, prefix: &str) -> Option<String> {
    let s = buffer_name.strip_prefix(prefix)?;
    let s = s.strip_suffix('*')?;
    (!s.is_empty()).then(|| s.to_string())
}

fn carried_name(ctx: &ActionContext<'_>, prefix: &str) -> Option<String> {
    let buffer_id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
    ctx.services
        .get::<BufferStoreHandle>()?
        .name_for(buffer_id)
        .and_then(|n| name_from_prompt_buffer_name(&n, prefix))
}

fn entry_at_cursor(ctx: &ActionContext<'_>) -> Option<RemoteEntry> {
    let s = state(ctx)?;
    let g = s.lock().ok()?;
    remote_at_line(&g.entries, ctx.cursor)
}

/// Map a cursor line onto a rendered remote.
///
/// Line 0 is the `Remotes (N)` heading, so row `i` is entry `i - 1`.
/// Out of range — the heading, the trailing blank, an empty list — is
/// `None`, and the handler no-ops.
pub(crate) fn remote_at_line(entries: &[RemoteEntry], cursor: Position) -> Option<RemoteEntry> {
    let index = (cursor.line as usize).checked_sub(1)?;
    entries.get(index).cloned()
}

/// Render the remote list, its headerline fields, and the entries the
/// cursor mapping reads — all from one `git remote -v`.
fn build_remote_list(workdir: &std::path::Path) -> (String, Vec<Field>, Vec<RemoteEntry>) {
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
    let entries = match Remote::list(&repo) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("magit-remote: listing remotes failed: {e}");
            Vec::new()
        }
    };
    let header = headerline::remote_fields(entries.len());
    (render_remote_list(&entries), header, entries)
}

/// The buffer text for `entries`.
///
/// Pure, so the layout the cursor mapping and the styler both depend on
/// is testable without a repository.
pub(crate) fn render_remote_list(entries: &[RemoteEntry]) -> String {
    if entries.is_empty() {
        return "No remotes.\n".to_string();
    }
    let width = entries
        .iter()
        .map(|e| e.name.chars().count())
        .max()
        .unwrap_or(0);
    let mut out = format!("Remotes ({})\n", entries.len());
    for e in entries {
        let pad = width.saturating_sub(e.name.chars().count());
        out.push_str(&format!("  {}{}  {}", e.name, " ".repeat(pad), e.fetch_url));
        // The push URL is shown only when it differs — printing an
        // identical URL twice on every row would bury the one case
        // that matters.
        if e.push_url != e.fetch_url {
            out.push_str(&format!("  (push: {})", e.push_url));
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, fetch: &str, push: &str) -> RemoteEntry {
        RemoteEntry {
            name: name.into(),
            fetch_url: fetch.into(),
            push_url: push.into(),
        }
    }

    fn at(line: u32) -> Position {
        Position { line, byte: 0 }
    }

    #[test]
    fn the_heading_row_maps_to_no_remote() {
        // The bug this exists to prevent: parsing the line under the
        // cursor would decode "Remotes (1)" as a remote called
        // `Remotes` and hand it to `git remote remove`.
        let entries = vec![entry("origin", "u", "u")];
        assert!(remote_at_line(&entries, at(0)).is_none());
    }

    #[test]
    fn each_row_maps_to_its_own_remote() {
        let entries = vec![entry("origin", "u1", "u1"), entry("upstream", "u2", "u2")];
        assert_eq!(remote_at_line(&entries, at(1)).unwrap().name, "origin");
        assert_eq!(remote_at_line(&entries, at(2)).unwrap().name, "upstream");
    }

    #[test]
    fn the_trailing_blank_line_maps_to_no_remote() {
        let entries = vec![entry("origin", "u", "u")];
        assert!(remote_at_line(&entries, at(2)).is_none());
        assert!(remote_at_line(&entries, at(99)).is_none());
    }

    #[test]
    fn an_empty_list_maps_nothing() {
        assert!(remote_at_line(&[], at(1)).is_none());
    }

    #[test]
    fn rows_line_up_and_only_a_differing_push_url_is_printed() {
        let text = render_remote_list(&[
            entry("origin", "https://a.git", "https://a.git"),
            entry("up", "https://b.git", "git@b.git"),
        ]);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Remotes (2)");
        assert_eq!(lines[1], "  origin  https://a.git");
        assert_eq!(lines[2], "  up      https://b.git  (push: git@b.git)");
        assert!(
            !lines[1].contains("push:"),
            "an identical push URL is not printed twice"
        );
    }

    #[test]
    fn no_remotes_renders_a_sentence_not_an_empty_buffer() {
        assert_eq!(render_remote_list(&[]), "No remotes.\n");
    }

    /// The rendered rows and the cursor mapping have to agree, or a
    /// chord acts on a remote other than the one under the cursor.
    #[test]
    fn the_rendered_row_index_matches_the_cursor_mapping() {
        let entries = vec![
            entry("a", "ua", "ua"),
            entry("bb", "ub", "ub"),
            entry("ccc", "uc", "uc"),
        ];
        let text = render_remote_list(&entries);
        for (i, line) in text.lines().enumerate().skip(1).take(entries.len()) {
            let mapped = remote_at_line(&entries, at(i as u32)).expect("row maps");
            assert!(
                line.trim_start().starts_with(&mapped.name),
                "line {i} ({line:?}) does not start with the mapped remote {:?}",
                mapped.name
            );
        }
    }

    #[test]
    fn prompt_buffer_names_round_trip_the_remote_they_carry() {
        for (built, prefix) in [
            (add_prompt_buffer_name("origin"), ADD_PREFIX),
            (rename_prompt_buffer_name("origin"), RENAME_PREFIX),
            (set_url_prompt_buffer_name("origin"), SET_URL_PREFIX),
        ] {
            assert_eq!(
                name_from_prompt_buffer_name(&built, prefix).as_deref(),
                Some("origin"),
                "{built} did not round-trip"
            );
        }
    }

    /// The three prompts must not decode each other's carriers — a
    /// set-url finish reading a rename prompt would repoint the wrong
    /// remote silently.
    #[test]
    fn a_prompt_carrier_does_not_decode_under_another_prefix() {
        let rename = rename_prompt_buffer_name("origin");
        assert!(name_from_prompt_buffer_name(&rename, SET_URL_PREFIX).is_none());
        assert!(name_from_prompt_buffer_name(&rename, ADD_PREFIX).is_none());
    }

    #[test]
    fn an_unrelated_buffer_name_carries_nothing() {
        assert!(name_from_prompt_buffer_name("*magit:status*", RENAME_PREFIX).is_none());
        assert!(name_from_prompt_buffer_name(RENAME_PREFIX, RENAME_PREFIX).is_none());
    }
}
