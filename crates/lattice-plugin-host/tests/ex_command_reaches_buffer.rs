//! OC.10 — a plugin ex-command can read the buffer it was invoked from.
//!
//! It could not. `apply-ex-command` handed a guest `bang` / `args` / `register`
//! / `count` and nothing else — no cursor, no buffer id, no document handle —
//! while still returning `list<effect>`, which includes `apply-edit`. That
//! effect names a `target` buffer id, and the guest had no way to obtain one. So
//! the seam offered an effect vocabulary a plugin structurally could not use:
//! the `plugin-gates-hand-guests-throwaway-contexts` shape, in a second place
//! after OC.1 found it on the event bus.
//!
//! The argument for closing it is older than this slice. MR.2 added `buffer_id`
//! to the NATIVE `ExCommandContext` reasoning that "a command reached that way
//! was seeing strictly less than the same command reached by a chord" —
//! `:magit-status` found it natively. The WIT mirror simply never followed, and
//! `:org-clock-in` is what found that.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::any::Any;
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_grammar::{CommandInvocation, CommandRegistry, GrammarEnv};
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginHost, PluginManifest, TrustTier};
use lattice_protocol::CancellationToken;
use lattice_protocol::position::Position;
use lattice_syntax::{Lang, Syntax};

const SRC: &str = "fn m() { let x = 1; }\nsecond line\n";

fn guest_wasm() -> Option<&'static str> {
    let path = env!("MULTISEAM_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

fn rust_snapshot(src: &str) -> Arc<dyn Any + Send + Sync> {
    let mut syntax = Syntax::for_language(Lang::Rust).unwrap().unwrap();
    syntax.parse(src);
    Arc::new(syntax.snapshot_owned())
}

/// Dispatch the fixture's `multiseam-ex-edit` and return the resulting effect.
fn run_ex(editor_caps: CapabilitySet, with_tree: bool) -> lattice_grammar::effect::Effect {
    let path = guest_wasm().expect("caller checked");
    let dirs = tempfile::tempdir().unwrap();
    let host = PluginHost::with_dirs(dirs.path().join("cache"), dirs.path().join("data")).unwrap();
    let component = host.compile(&std::fs::read(path).unwrap()).unwrap();
    let manifest = PluginManifest::new("multiseam", Vec::new(), editor_caps);
    let bus = Arc::new(lattice_runtime::EventBus::new());
    let set = host
        .instantiate_grammar_plugin(&component, &manifest, TrustTier::Bundled, &bus, None, None)
        .expect("grammar drain instantiates");
    let mut commands = CommandRegistry::new();
    set.register_all(&mut commands);
    let id = commands.id_by_name("multiseam-ex-edit").unwrap();

    let snapshot = rust_snapshot(SRC);
    let mut document = lattice_core::Document::from_text(SRC);
    let cancel = CancellationToken::never();
    let env = GrammarEnv {
        syntax: with_tree.then_some(&snapshot),
        ..Default::default()
    };
    // Cursor on line 1 — NOT line 0. A host that lost the cursor and defaulted
    // to the top of the buffer would still produce a plausible edit, so the
    // assertion below checks which line was replaced.
    lattice_grammar::execute_with_env(
        &commands,
        &mut document,
        BufferId(7),
        Position { line: 1, byte: 0 },
        CommandInvocation::of(id),
        &cancel,
        env,
    )
    .expect("the ex-command dispatches through the sync trampoline")
}

#[test]
fn an_ex_command_edits_the_line_the_cursor_is_on() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    let effect = run_ex(CapabilitySet::TREE_SITTER, true);
    match effect {
        lattice_grammar::effect::Effect::ApplyEdit { target, edit, .. } => {
            assert_eq!(
                target,
                BufferId(7),
                "the guest named the buffer it was invoked from — before OC.10 it \
                 had no id to name at all"
            );
            assert_eq!(
                edit.range.start.line, 1,
                "the edit landed on the CURSOR's line, not the top of the buffer"
            );
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;

            // `true` = the document handle read a line; `source_file` = the tree
            // crossed as well. Both were unreachable from this seam.
            assert_eq!(
                text, "EX:true:source_file",
                "the document handle and the tree both reached the guest"
            );
        }
        other => panic!("expected an ApplyEdit, got {other:?}"),
    }
}

/// The tree is capability-gated here exactly as it is on `apply-action`: an
/// ungranted plugin gets `none` and the document handle still works, rather than
/// the whole call failing.
#[test]
fn without_the_grant_the_document_still_crosses_and_the_tree_does_not() {
    if guest_wasm().is_none() {
        eprintln!("SKIP: multiseam fixture not built");
        return;
    }
    let effect = run_ex(CapabilitySet::empty(), true);
    match effect {
        lattice_grammar::effect::Effect::ApplyEdit { edit, .. } => {
            let lattice_protocol::edit::EditKind::Replace { text } = &edit.kind;

            assert_eq!(
                text, "EX:true:none",
                "no tree-sitter grant means no tree — and the buffer read still works"
            );
        }
        other => panic!("expected an ApplyEdit, got {other:?}"),
    }
}
