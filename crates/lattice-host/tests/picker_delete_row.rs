//! PD.1 — `<C-d>` removes the selected row from whatever backs the list.
//!
//! Design: [`docs/dev/architecture/picker.md`](../../../docs/dev/architecture/picker.md)
//! §4.2quater. Slice plan: `project-commands.md` PD.1.
//!
//! ## Who owns what, which is the whole design
//!
//! The host owns the KEY and the refresh. The SOURCE owns the verb, by naming
//! an ex-command in its spec. `<C-s>` / `<C-v>` / `<C-t>` need no such thing —
//! the host knows how to open a candidate in a split without asking anyone —
//! but only the source knows that removing a row from a project list means
//! forgetting a root, and the `projects` source is a WASM guest, so the host
//! cannot work it out even in principle.
//!
//! ## What these tests pin that a spec-only test cannot
//!
//! Three claims, each of which can be false independently:
//!
//! 1. the declared command RUNS, and with the row's own argument — not the
//!    query, not the display text, not empty;
//! 2. the picker STAYS OPEN and re-lists, because deleting is a tidying action
//!    and one that closed the picker would make removing three stale entries
//!    into three round trips;
//! 3. a source that declares NO command gets silence, not an error and not a
//!    command run with an empty argument.
//!
//! The source here is native, standing in for the guest: the seam under test
//! is the host's, and a WASM round-trip would only add a way to be skipped.
//! `picker_actor.rs` covers the boundary half.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use lattice_core::Document as CoreDocument;
use lattice_grammar::{Args, Effect};
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{PickerAcceptOutcome, PickerContext, RoutingPayload, SourceResult};

const DELETABLE: &str = "pd1-deletable";
const UNDELETABLE: &str = "pd1-undeletable";
const FORGET: &str = "pd1-forget";
const SWITCH_TO: &str = "pd1-switch-to";

/// The store the rows come from — the stand-in for the plugin's project list.
type Store = Arc<Mutex<Vec<String>>>;

/// What `pd1-forget` was called with, in order. `None` never appears: an
/// un-run command leaves the vec empty, which is a different assertion from
/// "ran with the wrong thing" and the tests below distinguish them.
type Calls = Arc<Mutex<Vec<String>>>;

/// A source whose rows ARE the store, so a refresh that re-runs `init` shows
/// the deletion and one that splices locally would not.
struct ListSource {
    spec: PickerSourceSpec,
    store: Store,
}

impl PickerSourceGenerator for ListSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        let rows = self
            .store
            .lock()
            .unwrap()
            .iter()
            .map(|root| {
                let cand = lattice_completion::candidate::RawCandidate::plain(
                    root.clone(),
                    lattice_completion::candidate::CandidateKind::Plain,
                );
                // The routing carries the ROOT, exactly as the plugin's rows
                // do — `InvokeCommand` with the root as its argument. That is
                // where `<C-d>` reads the identity from, and it is why the
                // candidate's text can be anything at all.
                (
                    cand,
                    RoutingPayload::InvokeCommand {
                        id: SWITCH_TO.to_string(),
                        args: Args::String(root.clone()),
                    },
                )
            })
            .collect();
        Ok(PickerInitResult::Inline(rows))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        match routing {
            RoutingPayload::InvokeCommand { id, args } => Ok(PickerAcceptOutcome::InvokeCommand {
                id: id.clone(),
                args: args.clone(),
            }),
            other => Err(format!("unexpected routing {other:?}")),
        }
    }
}

fn register_ex(
    editor: &Editor,
    name: &'static str,
    apply: impl Fn(&str) -> Effect + Send + Sync + 'static,
) {
    let reg = editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .expect("the command registry is a boot service");
    let mut next = (**reg.load()).clone();
    next.register_ex_command(
        name,
        "PD.1 fixture",
        lattice_grammar::registry::ExCommandSpec {
            latency_class: lattice_grammar::command::LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|rest: &str, _bang: bool| Ok(Args::String(rest.to_string()))),
            apply: Arc::new(move |ctx| {
                let arg = match &ctx.args {
                    Args::String(s) => s.clone(),
                    _ => String::new(),
                };
                Ok(apply(&arg))
            }),
            args_schema: vec![],
            surface_form: lattice_grammar::registry::SurfaceForm::Keyword,
        },
    );
    reg.store(Arc::new(next));
}

fn boot(store: Store, calls: Calls) -> Editor {
    let editor = Editor::boot(CoreDocument::from_text("committed\n"));

    // The plugin's `:project-forget` stand-in: it records the argument AND
    // mutates the store, so the refresh has something real to show.
    let forget_store = store.clone();
    register_ex(&editor, FORGET, move |arg| {
        calls.lock().unwrap().push(arg.to_string());
        forget_store.lock().unwrap().retain(|r| r != arg);
        Effect::None
    });
    register_ex(&editor, SWITCH_TO, |_| Effect::None);

    let mut pickers = (**editor.picker_registry.load()).clone();
    pickers.register_generator(Arc::new(ListSource {
        spec: PickerSourceSpec::no_args(DELETABLE, "PD.1: a list whose rows can be forgotten.")
            .with_delete_command(FORGET),
        store: store.clone(),
    }));
    pickers.register_generator(Arc::new(ListSource {
        spec: PickerSourceSpec::no_args(UNDELETABLE, "PD.1: a list that declares no delete verb."),
        store,
    }));
    editor.picker_registry.store(Arc::new(pickers));
    editor
}

fn store_of(items: &[&str]) -> Store {
    Arc::new(Mutex::new(
        items.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
    ))
}

fn rows(editor: &Editor) -> Vec<String> {
    editor
        .picker
        .as_ref()
        .map(|p| p.candidates.iter().map(|c| c.raw.display.clone()).collect())
        .unwrap_or_default()
}

/// Press `<C-d>` the way a terminal does — through `translate`, so the picker
/// branch is what decides what the key means. Dispatching the action directly
/// would pass on a build where the key is not bound at all.
fn press_ctrl_d(editor: &mut Editor) {
    let mut partial: Vec<lattice_protocol::KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(lattice_protocol::KeyChord::ctrl('d'), &mut partial);
}

fn press_char(editor: &mut Editor, ch: char) {
    let mut partial: Vec<lattice_protocol::KeyChord> = Vec::new();
    let _ = editor.dispatch_chord(lattice_protocol::KeyChord::char(ch), &mut partial);
}

/// The headline: the declared command runs with the ROW's argument, the store
/// shrinks, and the list re-lists to match — without the picker closing.
#[test]
fn ctrl_d_runs_the_sources_own_delete_verb() {
    let store = store_of(&["/src/alpha", "/src/beta", "/src/gamma"]);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut editor = boot(store.clone(), calls.clone());

    let _ = editor.open_picker(DELETABLE.to_string(), Vec::new());
    assert_eq!(rows(&editor).len(), 3, "precondition: three rows");

    press_ctrl_d(&mut editor);

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["/src/alpha"],
        "the command ran ONCE, with the selected row's own argument — not the \
         query, not the display text, not empty"
    );
    assert!(
        editor.picker.is_some(),
        "the picker stays open: deleting is a tidying action, and one that \
         closed it would make removing three stale entries three round trips"
    );
    assert_eq!(
        rows(&editor),
        vec!["/src/beta".to_string(), "/src/gamma".to_string()],
        "and the list re-listed from the store rather than splicing a row out"
    );
}

/// Delete several in a row without touching the selection. The clamp is what
/// makes this work: the index that named the deleted row now names its
/// successor, which is what you want when clearing out stale entries.
#[test]
fn deleting_repeatedly_walks_down_the_list() {
    let store = store_of(&["/src/alpha", "/src/beta", "/src/gamma"]);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut editor = boot(store.clone(), calls.clone());
    let _ = editor.open_picker(DELETABLE.to_string(), Vec::new());

    press_ctrl_d(&mut editor);
    press_ctrl_d(&mut editor);
    press_ctrl_d(&mut editor);

    assert_eq!(
        calls.lock().unwrap().as_slice(),
        ["/src/alpha", "/src/beta", "/src/gamma"],
        "each press took the next row, rather than re-running on a stale index"
    );
    assert!(store.lock().unwrap().is_empty(), "the store is empty");
    assert!(
        editor.picker.is_some(),
        "and the picker is STILL open on an empty list — there is nothing left \
         to select, which is not the same as nothing left to do"
    );

    // A fourth press on an empty list must not panic or run the command with
    // an empty argument. `:project-forget` with none falls back to the current
    // buffer's project, which would forget something never selected.
    press_ctrl_d(&mut editor);
    assert_eq!(
        calls.lock().unwrap().len(),
        3,
        "no row, no call — an empty list has nothing to delete"
    );
}

/// A source that declares no delete verb gets SILENCE.
///
/// Not an error: `<C-d>` is unbound as far as that picker is concerned, and a
/// message on every stray press would be noise about a thing the user did not
/// ask for. The same silence `<C-l>` keeps in a picker with no depth.
#[test]
fn a_source_with_no_delete_verb_is_silent() {
    let store = store_of(&["/src/alpha", "/src/beta"]);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut editor = boot(store.clone(), calls.clone());

    let _ = editor.open_picker(UNDELETABLE.to_string(), Vec::new());
    press_ctrl_d(&mut editor);

    assert!(
        calls.lock().unwrap().is_empty(),
        "nothing ran — the source named no verb"
    );
    assert_eq!(
        store.lock().unwrap().len(),
        2,
        "and the backing list is untouched"
    );
    assert!(editor.picker.is_some(), "the picker is undisturbed");
    assert_eq!(
        rows(&editor),
        vec!["/src/alpha".to_string(), "/src/beta".to_string()],
        "and the rows are exactly as they were — no refresh, no reorder"
    );
}

/// The query survives the refresh.
///
/// Deleting is something you do WHILE narrowing — typing `old`, removing the
/// three stale entries it turned up. A refresh that cleared the filter would
/// put you back at the top of the full list after every single one.
#[test]
fn the_query_survives_a_delete() {
    let store = store_of(&["/src/alpha-old", "/src/beta", "/src/gamma-old"]);
    let calls: Calls = Arc::new(Mutex::new(Vec::new()));
    let mut editor = boot(store.clone(), calls.clone());
    let _ = editor.open_picker(DELETABLE.to_string(), Vec::new());

    for ch in "old".chars() {
        press_char(&mut editor, ch);
    }
    assert_eq!(
        rows(&editor),
        vec!["/src/alpha-old".to_string(), "/src/gamma-old".to_string()],
        "precondition: the query narrowed to the two stale entries"
    );

    press_ctrl_d(&mut editor);

    assert_eq!(
        editor.picker.as_ref().map(|p| p.query.clone()),
        Some("old".to_string()),
        "the filter is still applied"
    );
    assert_eq!(
        rows(&editor),
        vec!["/src/gamma-old".to_string()],
        "and it still narrows — the other match is gone, the unmatched rows \
         stayed out"
    );
}
