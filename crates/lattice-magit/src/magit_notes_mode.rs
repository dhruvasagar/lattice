//! MG.37: `magit-notes-mode` — the note attached to one commit, as an
//! editable buffer.
//!
//! `*magit:note:<sha>*`, seeded with `git notes show <sha>` (empty when
//! there is none). `C-c C-c` writes it, `C-c C-k` closes without
//! writing — the same pair [`crate::magit_commit_mode`] uses, and for
//! the same reason: this is a compose buffer, not a view.
//!
//! **This buffer exists because `git notes edit` opens `$EDITOR`.**
//! Inside an editor that means a child waiting on a terminal that is
//! not there — it would hang holding a blocking-pool thread and never
//! report. `lattice_vcs::Note::set` pipes the text to `-F -` instead,
//! which makes this buffer the editor.
//!
//! Notes need no separate viewer: `git show` prints them by default, so
//! [`crate::magit_revision_mode`] already displays a commit's note under
//! its message. Pinned by a test in `lattice-vcs`, because a future
//! `--no-notes` would silently remove the only place they surface.

use std::sync::{Arc, Mutex, OnceLock};

use lattice_config;
use lattice_grammar::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandlerContribution, BufferStoreHandle, CapabilitySet, Keymap,
    KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_vcs::{Note, Repository};

use crate::buffer_state::{BufferStateGuard, BufferStates};
use crate::headerline;

pub struct MagitNotesMode;

impl MagitNotesMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-notes-mode")
    }
}

/// MR.3b: this view's word. The commit rides in `rest`, behind the
/// repository.
pub(crate) const NOTE_VIEW: &str = "note";

/// The commit a note buffer's name names. `None` for any other name,
/// and for an empty sha — `git notes add` on an empty rev would act on
/// HEAD, which is not what a malformed name asked for.
fn sha_from_name(name: &str) -> Option<String> {
    let parsed = crate::workdir::parse_magit_name(name)?;
    (parsed.view == NOTE_VIEW).then_some(())?;
    parsed.rest.map(str::to_string)
}

fn magit_notes_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Insert, chord: "<C-c><C-c>", doc: "Save this note", cmd: "action:magit-note-confirm" },
            keymap_entry! { mode: Insert, chord: "<C-c><C-k>", doc: "Close without saving", cmd: "action:magit-note-abort" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-c>", doc: "Save this note", cmd: "action:magit-note-confirm" },
            keymap_entry! { mode: Normal, chord: "<C-c><C-k>", doc: "Close without saving", cmd: "action:magit-note-abort" },
        ]
    })
}

pub struct NoteState {
    buffer_id: lattice_core::BufferId,
    store: Arc<BufferStoreHandle>,
    workdir: std::path::PathBuf,
    /// The commit this note belongs to, read from the buffer name.
    sha: String,
}

/// MG.13: service alias for this mode's per-buffer state
/// (`feedback_servicesregistry_arc_typeid`).
pub type NoteStatesHandle = Arc<BufferStates<NoteState>>;

fn state(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<NoteState>>> {
    crate::buffer_state::state_for::<NoteState>(ctx)
}

impl Mode for MagitNotesMode {
    type Guard = BufferStateGuard<NoteState>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn target_buffer_kind(&self) -> Option<lattice_core::BufferKind> {
        None
    }

    /// Editable — unlike every other magit buffer. `ReadOnly` is
    /// deliberately absent: the whole point is typing in it.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(magit_notes_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: "action:magit-note-confirm",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let s = state(ctx)?;
                    let (text, workdir, sha) = {
                        let g = s.lock().ok()?;
                        let handle = g.store.handle_for(g.buffer_id)?;
                        (
                            handle.snapshot().buffer.as_string(),
                            g.workdir.clone(),
                            g.sha.clone(),
                        )
                    };
                    if sha.is_empty() {
                        return Some(Effect::Echo {
                            level: EchoLevel::Error,
                            text: "magit: this note buffer names no commit".to_string(),
                        });
                    }
                    // An empty buffer is not refused the way an empty
                    // commit message is: clearing the buffer is how you
                    // say "remove this note", and `Note::set` translates
                    // it to exactly that.
                    //
                    // Off the actor thread, and the buffer closes
                    // optimistically — the same shape magit-commit's
                    // confirm uses, including the `error!` for a failure
                    // that has no synchronous path back to a buffer
                    // which is already gone.
                    tokio::task::spawn(tokio::task::spawn_blocking(move || {
                        let Ok(repo) = Repository::discover(&workdir) else {
                            tracing::error!(target: "lattice_magit", "note: repo discover failed");
                            return;
                        };
                        if let Err(e) = Note::set(&repo, &sha, &text) {
                            tracing::error!(target: "lattice_magit", "note save {sha}: {e}");
                        }
                    }));
                    Some(Effect::BuryBuffer)
                }),
            },
            ActionHandlerContribution {
                action_name: "action:magit-note-abort",
                handler: Arc::new(|ctx: &ActionContext<'_>| {
                    let _ = state(ctx)?;
                    // `force`: the buffer is dirty by construction (it
                    // was seeded, then typed in), and "close without
                    // saving" is precisely what the chord promises. A
                    // dirty-buffer prompt here would ask a question the
                    // user just answered.
                    Some(Effect::BuryBuffer)
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
            // MR.3: the repository the trigger resolved for THIS
            // buffer, not the one the editor was started in.
            let workdir =
                crate::repo_scope::view_workdir(&ctx, buffer_id, &handle).unwrap_or_default();
            let sha = store
                .name_for(buffer_id)
                .as_deref()
                .and_then(sha_from_name)
                .unwrap_or_default();

            let (hl, hl_registration) =
                match headerline::install(&ctx, buffer_id, Self::mode_id().as_str()) {
                    Some((h, reg)) => (Some(h), Some(reg)),
                    None => (None, None),
                };

            // MG.13: publish BEFORE the first `.await`, or `C-c C-c`
            // resolves to a handler with no state on the very next
            // keystroke. `sha` is known synchronously here (it is in the
            // buffer name), so unlike magit-rebase there is no
            // late-resolved field to guard against.
            let Some(states) = ctx.service::<NoteStatesHandle>() else {
                return Ok(orphan());
            };
            states.publish(
                buffer_id,
                NoteState {
                    buffer_id,
                    store: store.clone(),
                    workdir: workdir.clone(),
                    sha: sha.clone(),
                },
            );
            let guard = BufferStateGuard::new((*states).clone(), buffer_id)
                .with_headerline(hl_registration);

            let wd = workdir.clone();
            let sha_for_task = sha.clone();
            let (text, meta) = tokio::task::spawn_blocking(move || {
                let existing = Repository::discover(&wd)
                    .ok()
                    .and_then(|repo| Note::show(&repo, &sha_for_task))
                    .unwrap_or_default();
                let meta = crate::magit_revision_mode::commit_meta(&wd, &sha_for_task);
                (existing, meta)
            })
            .await
            .unwrap_or_default();
            headerline::publish(&hl, headerline::note_fields(&meta, !text.trim().is_empty()));
            crate::buffer_io::replace_buffer_text(&handle, text).await;

            Ok(guard)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The buffer name is the only place the commit is recorded, so a
    /// name that does not round-trip means a note saved against the
    /// wrong commit — or, with an empty sha, against HEAD, which is a
    /// commit the user never named.
    #[test]
    fn the_name_carries_the_commit_and_round_trips() {
        let sha = "a1b2c3d4e5f6";
        let name = crate::workdir::magit_buffer_name_with(NOTE_VIEW, "lattice", sha);
        assert_eq!(sha_from_name(&name).as_deref(), Some(sha));
    }

    #[test]
    fn names_this_mode_does_not_own_resolve_to_no_commit() {
        for name in [
            "*magit:note:*",
            "*magit:notes:abc*",
            "*magit:commit:abc*",
            "*magit:note:abc",
            "a.txt",
        ] {
            assert_eq!(
                sha_from_name(name),
                None,
                "{name:?} must not resolve to a commit"
            );
        }
    }
}
