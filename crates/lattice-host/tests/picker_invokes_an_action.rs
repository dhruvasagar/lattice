//! OM.11 — a picker source can call back into an ACTION, with its args intact.
//!
//! `PickerAcceptOutcome::InvokeCommand` used to render the chosen id + args
//! into a `:` line and run that. Two things fell out of it, and the second is
//! the one that blocked a feature:
//!
//! 1. The typed args made a round trip through text and a re-split, so a value
//!    containing a space arrived as two arguments. (The arm's own comment
//!    records an earlier version that dropped `args` entirely, which is how
//!    the branch pickers ended up asking for the branch you had just picked.)
//! 2. **An action is not reachable from the `:` line at all** —
//!    `excommand::parse` answers `Unknown` for `CommandKind::Action`. So a
//!    picker could only call an ex-command, and an ex-command gets no
//!    `borrow<document>`: it cannot read the buffer the user is sitting in.
//!    Org's refile is exactly that shape — pick a target, then take the
//!    subtree at the cursor.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::Mutex;

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{PickerAcceptOutcome, PickerContext, RoutingPayload, SourceResult};

const SOURCE_ID: &str = "invoke-action-test";
const ACTION: &str = "record-the-args";

/// One candidate, routed at an action with a value the source already
/// resolved — a path with a space in it, because that is the case the ex-line
/// round trip could not carry.
const VALUE: &str = "/tmp/My Notes/today.org\t7";

struct RoutingSource(PickerSourceSpec);

impl PickerSourceGenerator for RoutingSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.0
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        Ok(PickerInitResult::Inline(vec![(
            lattice_completion::RawCandidate::plain(
                "the target".to_string(),
                lattice_completion::CandidateKind::Plain,
            ),
            RoutingPayload::InvokeCommand {
                id: ACTION.to_string(),
                args: lattice_grammar::Args::String(VALUE.to_string()),
            },
        )]))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        let RoutingPayload::InvokeCommand { id, args } = routing else {
            return Ok(PickerAcceptOutcome::NoOp);
        };
        Ok(PickerAcceptOutcome::InvokeCommand {
            id: id.clone(),
            args: args.clone(),
        })
    }
}

/// Boot an editor with the source above and an action that records whatever
/// args it is dispatched with.
fn boot() -> (Editor, Arc<Mutex<Option<lattice_grammar::Args>>>) {
    let editor = Editor::boot(CoreDocument::from_text("* One\n"));

    let seen: Arc<Mutex<Option<lattice_grammar::Args>>> = Arc::new(Mutex::new(None));
    let sink = Arc::clone(&seen);
    let mut registry = (**editor.registry.load()).clone();
    registry.register_action(
        ACTION,
        "records the args it was dispatched with (test)",
        lattice_grammar::registry::ActionSpec {
            args_schema: Vec::new(),
            apply: Arc::new(move |ctx: &lattice_grammar::registry::ActionContext| {
                *sink.lock().unwrap() = Some(ctx.args.clone());
                Ok(lattice_grammar::Effect::None)
            }),
        },
    );
    editor.registry.store(Arc::new(registry));

    let mut pickers = (**editor.picker_registry.load()).clone();
    pickers.register_generator(Arc::new(RoutingSource(PickerSourceSpec::no_args(
        SOURCE_ID,
        "OM.11 action-routing test source",
    ))));
    editor.picker_registry.store(Arc::new(pickers));

    (editor, seen)
}

/// The action runs, and its args are the ones the source resolved — byte for
/// byte, tab and embedded space included.
#[test]
fn an_action_is_dispatched_with_the_args_the_source_resolved() {
    let (mut editor, seen) = boot();
    let _ = editor.open_picker(SOURCE_ID.to_string(), Vec::new());
    let _ = editor.do_picker_accept();

    let got = seen.lock().unwrap().clone();
    assert_eq!(
        got,
        Some(lattice_grammar::Args::String(VALUE.to_string())),
        "the action ran with the typed args, not with whatever a `:` line \
         re-split them into"
    );
}

/// The ex-line fallback is still there for everything that is not an action —
/// the command palette and the magit pickers all route at ex-commands, and
/// they must keep working.
#[test]
fn a_command_that_is_not_an_action_still_goes_through_the_ex_line() {
    let (mut editor, seen) = boot();
    // `nohlsearch` is a real ex-command and a harmless one.
    editor.apply_picker_outcome(PickerAcceptOutcome::InvokeCommand {
        id: "nohlsearch".to_string(),
        args: lattice_grammar::Args::None,
    });
    assert!(
        seen.lock().unwrap().is_none(),
        "sanity: the test action was not what ran"
    );
    assert!(
        !editor
            .last_message
            .as_ref()
            .is_some_and(|m| m.text.contains("Unknown")),
        "the ex-command resolved: {:?}",
        editor.last_message
    );
}
