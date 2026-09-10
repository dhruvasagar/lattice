//! PC.1 — a picker can be opened rooted somewhere other than the active
//! buffer's project.
//!
//! Design: `docs/dev/architecture/project-commands.md` §9 H1.
//!
//! ## Why the root is in the CONTEXT and not an argument
//!
//! `files` would happily take a root as `args[0]` — it already does, for
//! `:picker files <path>`. `grep` would not survive it. A `live` source
//! re-queries through `on_query_changed`, which receives the query and the
//! context and **not** the open's args, and a source is a shared `&self`
//! generator with no per-open state. So an argument-borne root would apply to
//! the first grep and silently revert to the workspace root on the next
//! keystroke — a feature that works until you type, which is worse than one
//! that is absent.
//!
//! Putting it in the context makes it survive every re-query and gives every
//! root-sensitive source the same answer without a per-source convention. The
//! `grep` source needed **no change at all** as a result: it already reads
//! `ctx.workspace_root` in both `init` and `on_query_changed`.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn boot() -> Editor {
    Editor::boot(CoreDocument::from_text("fn main() {}\n"))
}

/// The override wins over every resolution beneath it.
#[test]
fn an_explicit_root_overrides_the_buffers_project() {
    let mut editor = boot();
    let elsewhere = std::path::PathBuf::from("/somewhere/else");
    editor.picker_root = Some(elsewhere.clone());

    let snap = editor.document.snapshot();
    assert_eq!(
        editor.picker_workspace_root_path(&snap),
        elsewhere,
        "an explicit root is returned verbatim — it is a root the caller \
         already resolved, so re-resolving could only move the answer"
    );
}

/// And with no override, nothing changes: every picker before PC.1 resolved
/// from the active buffer and still does.
#[test]
fn no_override_resolves_from_the_buffer_as_before() {
    let editor = boot();
    let snap = editor.document.snapshot();
    let resolved = editor.picker_workspace_root_path(&snap);
    assert!(
        !resolved.as_os_str().is_empty(),
        "the ordinary path still answers something"
    );
    assert_ne!(resolved, std::path::PathBuf::from("/somewhere/else"));
}

/// **The override is set unconditionally at open, `None` included.**
///
/// That is what makes a stale root impossible without a close hook: opening a
/// picker is the one moment guaranteed to run, whereas every close path would
/// have to remember to clear it. This asserts the clearing half — a picker
/// opened with no root must not inherit the last one's.
#[test]
fn opening_without_a_root_clears_a_previous_override() {
    let mut editor = boot();
    editor.picker_root = Some(std::path::PathBuf::from("/stale"));

    // What the applier does for `Effect::OpenPicker { root: None, .. }`.
    editor.picker_root = None;

    let snap = editor.document.snapshot();
    assert_ne!(
        editor.picker_workspace_root_path(&snap),
        std::path::PathBuf::from("/stale"),
        "a picker opened with no root resolves from the buffer, not from \
         whatever the previous picker was rooted at"
    );
}
