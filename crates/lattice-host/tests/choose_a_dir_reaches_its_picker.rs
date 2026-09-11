//! PC.12 — `… (choose a dir)` must actually open the directory picker, and the
//! directory it picks must actually reach the menu.
//!
//! Design:
//! [`docs/dev/architecture/project-commands.md`](../../../docs/dev/architecture/project-commands.md)
//! §5. Slice plan: PC.12.
//!
//! ## The two silent drops this pins
//!
//! The flow is three hops, and each one crosses a seam that has nowhere to hand
//! a renderer-owned effect back to:
//!
//! 1. the projects picker accepts the row → `project-choose-dir` →
//!    `Effect::OpenPicker { dir-pick, fill_action }`. A plugin source's accept
//!    is always async (`accept_async`, PH7.4c.2), so this commits through
//!    `drain_pending_picker_accept`, whose allowlist forwarded exactly three
//!    variants and dropped the rest with a `warn!`. `OpenPicker` was not one of
//!    the three, so the row did nothing at all — which is precisely what a
//!    fourth feature dying on that allowlist looks like from the outside.
//! 2. `dir-pick` answers → `FillTarget::Action` → `project-remember-and-switch`
//!    → `Effect::OpenTransient`. That arm dropped `out.effects` on a comment
//!    claiming they were "applied by `apply_effect_host` inside
//!    `dispatch_invocation`" — true of most effects and false of exactly the
//!    ones a plugin reaches for here: `OpenTransient`'s body is
//!    `Editor::open_named_transient`, hoisted for the renderer peers and called
//!    by neither of these paths.
//!
//! Both are reproduced WITHOUT the plugin: a native async picker source stands
//! in for `WasmPickerSource`, and native ex-commands stand in for the guest's,
//! returning the same effects `plugins/project` returns. The seams are the real
//! ones.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_core::Document as CoreDocument;
use lattice_grammar::{Args, Effect};
use lattice_host::editor::Editor;
use lattice_picker::source::{PickerInitResult, PickerSourceGenerator, PickerSourceSpec};
use lattice_picker::{
    AcceptFuture, FillTarget, PickerAcceptOutcome, PickerContext, RoutingPayload, SourceResult,
    TransientGroup, TransientItem, TransientItemKind, TransientSourceRegistryHandle, TransientSpec,
};

const SOURCE_ID: &str = "pc12-projects-fixture";
const CHOOSE_DIR: &str = "test-project-choose-dir";
const REMEMBER_AND_SWITCH: &str = "test-project-remember-and-switch";
const SWITCH_MENU: &str = "pc12-switch-menu";

/// Stand-in for `WasmPickerSource`: `accept_async` returns `Some`, which is
/// what forces the commit through `drain_pending_picker_accept` rather than
/// `do_picker_accept`'s synchronous return. A test that took the sync path
/// would pass on the broken code — the renderer applies the effects there.
struct AsyncProjectsSource {
    spec: PickerSourceSpec,
}

impl PickerSourceGenerator for AsyncProjectsSource {
    fn spec(&self) -> &PickerSourceSpec {
        &self.spec
    }

    fn init(&self, _ctx: &PickerContext<'_>, _args: &[String]) -> SourceResult<PickerInitResult> {
        Ok(PickerInitResult::Inline(Vec::new()))
    }

    fn accept(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome> {
        Err("must resolve via accept_async".to_string())
    }

    fn accept_async(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> Option<AcceptFuture> {
        Some(Box::pin(async move {
            tokio::task::yield_now().await;
            Ok(PickerAcceptOutcome::InvokeCommand {
                id: CHOOSE_DIR.to_string(),
                args: Args::None,
            })
        }))
    }
}

/// Register an ex-command returning a fixed effect, the way the project
/// plugin's two hops do. Native rather than guest-backed: the seam under test
/// is the host's, and a WASM round-trip would only add a way for the test to
/// be skipped.
fn register_ex(
    editor: &Editor,
    name: &'static str,
    effect: impl Fn(&str) -> Effect + Send + Sync + 'static,
) {
    let reg = editor
        .services
        .get::<lattice_grammar::CommandRegistryHandle>()
        .expect("the command registry is a boot service");
    let mut next = (**reg.load()).clone();
    next.register_ex_command(
        name,
        "PC.12 fixture",
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
                Ok(effect(&arg))
            }),
            args_schema: vec![],
            surface_form: lattice_grammar::registry::SurfaceForm::Keyword,
        },
    );
    reg.store(Arc::new(next));
}

fn boot() -> Editor {
    let editor = Editor::boot(CoreDocument::from_text("committed\n"));

    // Hop 1's command: open the directory sub-picker, naming the plugin's own
    // command as where the answer goes (PC.11's `fill-action`).
    register_ex(&editor, CHOOSE_DIR, |_| {
        Effect::OpenPicker {
            // `buffers` rather than `dir-pick`: any registered native source
            // proves the effect was applied, and this one needs no filesystem.
            source: "buffers".to_string(),
            args: Vec::new(),
            root: None,
            fill_action: Some(REMEMBER_AND_SWITCH.to_string()),
        }
    });
    // Hop 2's command: a path becomes a project, and the switch-commands menu
    // opens in the same breath.
    register_ex(&editor, REMEMBER_AND_SWITCH, |_arg| Effect::OpenTransient {
        source: SWITCH_MENU.to_string(),
        args: Args::None,
    });

    let mut pickers = (**editor.picker_registry.load()).clone();
    pickers.register_generator(Arc::new(AsyncProjectsSource {
        spec: PickerSourceSpec::no_args(SOURCE_ID, "PC.12 regression: async projects source."),
    }));
    editor.picker_registry.store(Arc::new(pickers));

    let transients = editor
        .services
        .get::<TransientSourceRegistryHandle>()
        .expect("the transient registry is a boot service");
    transients.register(SWITCH_MENU, |_ctx| TransientSpec {
        title: "Switch to project".to_string(),
        groups: vec![TransientGroup {
            label: "Actions".into(),
            items: vec![TransientItem {
                key: vec!["f".into()],
                label: "find file".into(),
                description: String::new(),
                kind: TransientItemKind::Dismiss,
            }],
        }],
        preview: None,
        footer: None,
    });

    editor
}

fn seat_the_choose_row(editor: &mut Editor) {
    let cand = lattice_completion::candidate::RawCandidate::plain(
        "\u{2026} (choose a dir)".to_string(),
        lattice_completion::candidate::CandidateKind::Plain,
    );
    editor.seat_picker_from_pairs(
        SOURCE_ID.to_string(),
        vec![(cand, RoutingPayload::Buffer { id: 0 })],
    );
}

/// Pump the drain the actor reaches via `run_tick_pending` on the
/// `async_landed` wake, until the deferred accept commits.
fn settle_accept(editor: &mut Editor) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && editor.pending_picker_accept.is_some() {
        let _ = editor.drain_pending_picker_accept();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        editor.pending_picker_accept.is_none(),
        "the drain committed and cleared the pending accept"
    );
}

/// **Hop 1, the reported bug.** Selecting `… (choose a dir)` opens the
/// directory picker. On the broken code the accept resolved, the projects
/// picker closed, and nothing replaced it — a row that is indistinguishable
/// from a dead key.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_choose_row_opens_the_directory_picker() {
    let mut editor = boot();
    seat_the_choose_row(&mut editor);

    let _ = editor.do_picker_accept();
    assert!(editor.picker.is_none(), "the projects picker closes");
    assert!(
        editor.pending_picker_accept.is_some(),
        "a plugin source's accept is deferred — this is the path under test"
    );

    settle_accept(&mut editor);

    assert!(
        editor.picker.is_some(),
        "the sub-picker is open (PC.12: `Effect::OpenPicker` must not be \
         dropped by the async picker-accept drain)"
    );
}

/// …and it opens KNOWING where its answer goes. A sub-picker that opened
/// without the capture would fill nothing on `<CR>` and echo "nothing was
/// waiting for a value" — the same dead end one hop later.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_directory_picker_opens_with_its_fill_target_captured() {
    let mut editor = boot();
    seat_the_choose_row(&mut editor);
    let _ = editor.do_picker_accept();
    settle_accept(&mut editor);

    assert_eq!(
        editor.picker_fill_target,
        Some(FillTarget::Action {
            command: REMEMBER_AND_SWITCH.to_string()
        }),
        "the fill action rides the effect and is captured at open"
    );
}

/// **Hop 2.** The picked directory reaches the command, and the menu the
/// command opens actually appears. `FillTarget::Action` dropped `out.effects`
/// on the claim that `dispatch_invocation` had already applied them — true of
/// most effects, false of `OpenTransient`, whose body is an `Editor` method the
/// renderer peers call and this path did not.
#[test]
fn the_chosen_directory_opens_the_switch_menu() {
    let mut editor = boot();

    let _ = editor.open_picker_for_effect(
        "buffers".to_string(),
        Vec::new(),
        None,
        Some(REMEMBER_AND_SWITCH.to_string()),
    );
    assert!(editor.picker.is_some(), "precondition: the picker opened");
    // `do_picker_accept` takes the picker before it applies the outcome, so
    // production reaches the fill with nothing seated. Mirrored here, or the
    // assertion below could pass on a picker the accept was supposed to close.
    editor.picker = None;

    let _ = editor.apply_picker_outcome(PickerAcceptOutcome::FillCaller {
        text: "/srv/chosen".to_string(),
    });

    let picker = editor
        .picker
        .as_ref()
        .expect("the switch-commands menu is seated");
    let transient = picker
        .transient
        .as_ref()
        .expect("and it is a transient, not a leftover picker");
    assert_eq!(transient.title, "Switch to project");
}
