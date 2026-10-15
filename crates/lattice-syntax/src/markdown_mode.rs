//! `markdown-mode` — the markdown major, and the link following it owns.
//!
//! `<CR>` on a link follows it: `[t](#heading)` jumps within the buffer,
//! `[t](other.md)` / `[t](other.md#heading)` opens the file (at the
//! heading), `[t](https://…)` and `<https://…>` go to the OS handler.
//! Anywhere else `<CR>` declines, so it composes with whatever the
//! chord means beneath this major — today nothing, for a Document.
//!
//! The chord and the handler body both live here: the host contributes the
//! effect vocabulary and nothing markdown-specific
//! (`feedback_mode_owns_its_surface`). See
//! `docs/dev/architecture/markdown-links.md`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use lattice_core::BufferKind;
use lattice_grammar::{ActionSpec, CommandRegistry, EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, keymap_entry,
};
use lattice_protocol::position::Position;

use crate::markdown::{LinkDestination, find_heading, link_at};

/// The command `<CR>` resolves to in a markdown buffer.
pub const FOLLOW_LINK: &str = "action:markdown-follow-link";

/// Major mode for markdown documents.
///
/// H.2 (2026-05-31): also the default major for `BufferKind::Help` (via
/// the registry's kind index), so help buffers and markdown files share
/// one mode. Help buffers keep their own `<CR>`: their links are stripped
/// to labels when the buffer is built, so there is no `[…](…)` under the
/// cursor for this handler to claim, and it declines.
pub struct MarkdownMode;

impl MarkdownMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("markdown-mode")
    }
}

fn markdown_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry!(
            mode: Normal,
            chord: "<CR>",
            doc: "Follow the markdown link under the cursor: a #heading in this buffer, a file (optionally #heading), or a URL.",
            cmd: "action:markdown-follow-link"
        )]
    })
}

impl Mode for MarkdownMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Help)
    }
    fn options(&self) -> lattice_config::OptionOverrideSet {
        lattice_config::overrides! {
            // RF.4: markdown IS prose, so wrapping while typing applies to
            // every line rather than only to comments (there are none). The
            // global default is the conservative `comments` rung because a
            // code line broken mid-expression is destructive in a way a
            // broken sentence is not; a prose major has no such hazard and
            // opts up.
            lattice_config::AutoWrapOption = lattice_core::AutoWrap::All,
        }
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(markdown_keymap_entries())
    }
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        let follow: ActionHandler = Arc::new(follow_link);
        vec![ActionHandlerContribution {
            action_name: FOLLOW_LINK,
            handler: follow,
        }]
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Declare markdown-mode's `action:*` commands so the keymap's `cmd:`
/// resolves to a `CommandId` and [`MarkdownMode::action_handlers`] binds.
/// The `apply` fallback declines: with no handler registered (a harness)
/// `<CR>` falls through rather than swallowing the key.
pub fn register_markdown_commands(registry: &mut CommandRegistry) {
    registry.register_action(
        FOLLOW_LINK,
        "markdown `<CR>`: follow the link under the cursor — a #heading in this buffer, a file (optionally at #heading), or a URL in the OS handler.",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::Declined)),
            args_schema: vec![],
        },
    );
}

/// The `<CR>` body. Declines off a link so the chord stays free for
/// whatever lies beneath the major.
fn follow_link(ctx: &ActionContext<'_>) -> Option<Effect> {
    let Some(store) = ctx.services.get::<BufferStoreHandle>() else {
        return Some(Effect::Declined);
    };
    let id = lattice_core::BufferId(ctx.buffer_id.0 as u32);
    let Some(doc) = store.handle_for(id) else {
        return Some(Effect::Declined);
    };
    let snap = doc.snapshot();
    let Some(line) = snap.buffer.line(ctx.cursor.line) else {
        return Some(Effect::Declined);
    };
    let Some(dest) = link_at(&line, ctx.cursor.byte as usize) else {
        return Some(Effect::Declined);
    };
    Some(match dest {
        LinkDestination::External(uri) => Effect::OpenExternalUri { uri },
        LinkDestination::Anchor(slug) => {
            let text = snap.buffer.as_string();
            match find_heading(text.lines(), &slug) {
                Some(line) => Effect::Many(vec![
                    Effect::RecordJump,
                    Effect::CursorMove(Position::new(line, 0)),
                ]),
                None => no_heading(&slug, None),
            }
        }
        LinkDestination::File { path, anchor } => {
            let base = store
                .path_for(id)
                .and_then(|p| p.parent().map(Path::to_path_buf));
            open_file(&resolve(&path, base.as_deref()), anchor.as_deref())
        }
    })
}

/// A relative link is relative to the document that contains it, the
/// way every renderer of markdown reads it — not to the editor's cwd. A
/// buffer with no path (a scratch) has no directory, so the cwd is the
/// only base there is.
fn resolve(path: &str, base: Option<&Path>) -> PathBuf {
    let p = Path::new(path);
    match base {
        Some(dir) if p.is_relative() => dir.join(p),
        _ => p.to_path_buf(),
    }
}

/// Open `target`, at `anchor`'s heading when one is named.
///
/// A missing file is an error, not a new buffer — `gf` in vim does the
/// same. Opening a blank buffer would make a mistyped link look like it
/// worked, and a later `:w` would create the file.
///
/// The heading is looked up in the file on disk. If the target is open
/// with unsaved edits that moved the heading, the jump lands on the saved
/// line; reading through the open buffer needs a find-by-path the store
/// does not offer, and a few lines' drift in a dirty buffer is the
/// smaller cost.
fn open_file(target: &Path, anchor: Option<&str>) -> Effect {
    if !target.exists() {
        return Effect::Echo {
            level: EchoLevel::Error,
            text: format!("no such file: {}", target.display()),
        };
    }
    let line = match anchor {
        None => 0,
        Some(slug) => {
            let text = std::fs::read_to_string(target).unwrap_or_default();
            match find_heading(text.lines(), slug) {
                Some(line) => line,
                // Open the file anyway: the user asked for it, and the
                // missing heading is worth a word rather than a refusal.
                None => {
                    return Effect::Many(vec![
                        Effect::RecordJump,
                        open_at(target, 0),
                        no_heading(slug, Some(target)),
                    ]);
                }
            }
        }
    };
    Effect::Many(vec![Effect::RecordJump, open_at(target, line)])
}

fn open_at(target: &Path, line: u32) -> Effect {
    Effect::OpenBufferAt {
        path: Some(target.to_path_buf()),
        position: Position::new(line, 0),
        force: false,
        content: None,
        activate_minor: None,
    }
}

fn no_heading(slug: &str, file: Option<&Path>) -> Effect {
    let text = match file {
        Some(f) => format!("no heading #{slug} in {}", f.display()),
        None => format!("no heading #{slug}"),
    };
    Effect::Echo {
        level: EchoLevel::Warn,
        text,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn relative_links_resolve_against_the_document_directory() {
        let base = Path::new("/notes/project");
        assert_eq!(
            resolve("../todo.md", Some(base)),
            PathBuf::from("/notes/project/../todo.md")
        );
        assert_eq!(resolve("/abs.md", Some(base)), PathBuf::from("/abs.md"));
        assert_eq!(resolve("a.md", None), PathBuf::from("a.md"));
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_new_buffer() {
        let eff = open_file(Path::new("/definitely/not/here.md"), None);
        assert!(
            matches!(eff, Effect::Echo { level: EchoLevel::Error, ref text } if text.contains("no such file")),
            "{eff:?}"
        );
    }

    #[test]
    fn a_file_anchor_lands_on_its_heading_and_records_the_jump() {
        let dir = std::env::temp_dir().join(format!("lattice-mdl-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("guide.md");
        std::fs::write(&f, "# Guide\n\ntext\n\n## Setup Steps\n").unwrap();
        let eff = open_file(&f, Some("setup-steps"));
        let Effect::Many(effs) = eff else {
            panic!("expected Many, got {eff:?}")
        };
        assert!(matches!(effs[0], Effect::RecordJump));
        assert!(
            matches!(effs[1], Effect::OpenBufferAt { position, .. } if position == Position::new(4, 0))
        );
        // An unknown heading still opens the file, and says why it is at the top.
        let Effect::Many(effs) = open_file(&f, Some("nope")) else {
            panic!()
        };
        assert!(matches!(
            effs[2],
            Effect::Echo {
                level: EchoLevel::Warn,
                ..
            }
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
