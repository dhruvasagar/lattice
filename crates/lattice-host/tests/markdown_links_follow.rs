//! MDL.2 — `<CR>` on a markdown link follows it, through the real keymap.
//!
//! The unit tests in `lattice_syntax::markdown_mode` pin the decisions
//! (path resolution, missing file, `file#heading`). These pin the wiring the
//! units cannot see: that markdown-mode's `<CR>` binds at all in a `.md`
//! buffer, that its handler reads the buffer it fired in, that a decline off
//! a link leaves the buffer alone, and that help buffers — whose major is
//! also markdown-mode — keep their own `<CR>`.
//!
//! Every assertion is on what the user would see (cursor, jump ring, the
//! file the editor is asked to open, the echo), not on the handler's return.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_grammar::Effect;
use lattice_host::chord::{KeyChord, SpecialKey};
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_mode::ModeId;
use lattice_protocol::position::Position;

fn press_enter(editor: &mut Editor) -> DispatchOutcome {
    let mut partial = Vec::new();
    editor
        .dispatch_chord_with_outcome(KeyChord::special(SpecialKey::Enter), &mut partial)
        .1
}

/// Boot, then open `name` (written with `text`) in `dir` as a real file, so
/// language detection activates markdown-mode the way it does for a user.
fn open_md(dir: &std::path::Path, name: &str, text: &str) -> Editor {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    let mut editor = Editor::boot(CoreDocument::from_text(""));
    editor.do_edit(Some(path), false);
    let id = editor.active_pane_buffer_id();
    assert!(
        editor
            .active_modes
            .get(&id)
            .is_some_and(|m| m.is_active(ModeId::new("markdown-mode"))),
        "test premise: a .md file activates markdown-mode"
    );
    editor
}

/// The file `<CR>` asked the peer to open, if any. `OpenBufferAt` is
/// peer-applied, so the host hands it back in the outcome rather than
/// opening the file itself.
fn opened(out: &DispatchOutcome) -> Option<(std::path::PathBuf, Position)> {
    fn walk(e: &Effect) -> Option<(std::path::PathBuf, Position)> {
        match e {
            Effect::OpenBufferAt {
                path: Some(p),
                position,
                ..
            } => Some((p.clone(), *position)),
            Effect::Many(v) => v.iter().find_map(walk),
            _ => None,
        }
    }
    out.effects.iter().find_map(walk)
}

#[test]
fn an_anchor_link_jumps_to_its_heading_and_records_the_jump() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = open_md(
        dir.path(),
        "doc.md",
        "# Doc\n\nSee [the setup](#setup-steps).\n\ntext\n\n## Setup Steps\n\nbody\n",
    );
    editor.cursor = Position::new(2, 6); // inside `[the setup]`
    let before = editor.position_history.len();

    press_enter(&mut editor);

    assert_eq!(editor.cursor, Position::new(6, 0), "lands on the heading");
    assert_eq!(
        editor.position_history.len(),
        before + 1,
        "the follow records a jump so <C-o> can return"
    );
    assert_eq!(
        editor.position_history.last().unwrap().position.line,
        2,
        "the recorded entry is the link, not the heading"
    );
}

#[test]
fn a_file_link_opens_the_file_relative_to_the_document() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/guide.md"), "# Guide\n\n## Install\n").unwrap();
    let mut editor = open_md(
        dir.path(),
        "index.md",
        "[guide](sub/guide.md) and [install](sub/guide.md#install)\n",
    );

    editor.cursor = Position::new(0, 1);
    let out = press_enter(&mut editor);
    let (path, pos) = opened(&out).expect("<CR> on a file link opens it");
    assert_eq!(path, dir.path().join("sub/guide.md"));
    assert_eq!(pos, Position::new(0, 0));

    let col = "[guide](sub/guide.md) and [".len() as u32;
    editor.cursor = Position::new(0, col);
    let out = press_enter(&mut editor);
    let (_, pos) = opened(&out).expect("file#heading opens the file");
    assert_eq!(pos, Position::new(2, 0), "at the named heading");
}

#[test]
fn off_a_link_enter_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let text = "plain prose here\n[a](#b)\n";
    let mut editor = open_md(dir.path(), "doc.md", text);
    editor.cursor = Position::new(0, 3);

    let out = press_enter(&mut editor);

    assert_eq!(editor.cursor, Position::new(0, 3));
    assert_eq!(editor.active_text().as_string(), text, "no edit");
    assert!(opened(&out).is_none());
}

#[test]
fn a_missing_file_says_so_and_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = open_md(dir.path(), "doc.md", "[gone](nope.md)\n");
    editor.cursor = Position::new(0, 1);

    let out = press_enter(&mut editor);

    assert!(opened(&out).is_none());
    let msg = editor.last_message.as_ref().map(|m| m.text.clone());
    assert!(
        msg.as_deref().is_some_and(|m| m.contains("no such file")),
        "{msg:?}"
    );
}

/// Help buffers are markdown-mode buffers too. Their links are stripped to
/// labels when built, so markdown-mode's handler finds no `[…](…)` and
/// declines; help's own `<CR>` must still follow the link.
#[test]
fn help_buffers_keep_their_own_enter() {
    let mut editor = Editor::boot(CoreDocument::from_text(""));
    let content = lattice_help::HelpContent::from_lines(
        "help",
        vec!["See [the languages page](help:languages).".into()],
    );
    let _ = editor.open_help_in_pane(content);
    let link_pos = (0..60u32)
        .map(|c| Position::new(0, c))
        .find(|p| editor.help_link_at(*p).is_some())
        .expect("test premise: the help buffer carries the link");
    editor.cursor = link_pos;

    let out = press_enter(&mut editor);

    assert!(
        out.renderer_signals
            .iter()
            .any(|s| matches!(s, lattice_host::dispatch::RendererSignal::DisplayBuffer(_))),
        "help's <CR> opened the linked page; signals: {:?}",
        out.renderer_signals.len()
    );
}
