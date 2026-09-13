//! PC.10 — `<C-l>` goes into the selected candidate, `<C-h>` comes back out.
//!
//! Design:
//! [`docs/dev/architecture/project-commands.md`](../../../docs/dev/architecture/project-commands.md)
//! §9 H5. Slice plan: PC.10.
//!
//! ## What is actually at risk here
//!
//! Not "does `dir-pick` descend" — that is one line over a candidate's text,
//! and PC.9 pins the listing it depends on. The risk is the OTHER pickers.
//! `descend` / `ascend` are trait hooks with `None` defaults, and a wiring
//! that ignored the default would give every picker in the editor a `<C-l>`
//! that silently rewrites its query. That failure is invisible in a
//! `dir-pick` test and obvious in a `buffers` one, so both are here.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_core::Document as CoreDocument;
use lattice_host::action::Action;
use lattice_host::editor::Editor;
use lattice_protocol::KeyChord;

/// The REAL path — chord → `dispatch_chord` → `input::translate` → the
/// dispatch arm. Dispatching `Action::PickerDescend` directly would pass
/// against a `<C-l>` that was never bound, which is the half of this slice
/// that can actually go missing.
fn press(editor: &mut Editor, ch: char) -> Action {
    let mut partial: Vec<KeyChord> = Vec::new();
    editor.dispatch_chord(KeyChord::ctrl(ch), &mut partial)
}

/// A tree two levels deep, so descending has somewhere to go and ascending
/// has somewhere to come back to.
fn tree() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(dir.path().join("alpha").join("inner")).unwrap();
    std::fs::create_dir_all(dir.path().join("beta")).unwrap();
    dir
}

fn open_dir_pick(root: &std::path::Path) -> Editor {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let _ = editor.open_picker(
        lattice_picker::DIR_PICK_SOURCE.to_string(),
        vec![root.to_string_lossy().to_string()],
    );
    assert!(
        editor.picker.is_some(),
        "precondition: `dir-pick` seated a picker at {}",
        root.display()
    );
    editor
}

fn query(editor: &Editor) -> String {
    editor
        .picker
        .as_ref()
        .map(|p| p.query.clone())
        .unwrap_or_default()
}

fn rows(editor: &Editor) -> Vec<String> {
    editor
        .picker
        .as_ref()
        .map(|p| p.candidates.iter().map(|c| c.raw.text.clone()).collect())
        .unwrap_or_default()
}

/// Move the selection onto the first row that is not PP.1's `../`.
///
/// `dir-pick` lists `../` first and opens selected on it, so a test about
/// descending into a CHILD has to say which row it means. Done by moving the
/// selection rather than by indexing the list, because what `<C-l>` acts on is
/// the selection and that is the thing under test.
fn select_first_child(editor: &mut Editor) -> String {
    while editor
        .picker
        .as_ref()
        .and_then(|p| p.selected_candidate())
        .is_some_and(|c| c.raw.display == "../")
    {
        if let Some(p) = editor.picker.as_mut() {
            p.select_next();
        }
    }
    editor
        .picker
        .as_ref()
        .and_then(|p| p.selected_candidate())
        .map(|c| c.raw.text.clone())
        .expect("the tree has a child row")
}

/// `<C-l>` replaces the query with the selected directory, so the next
/// listing is of its children rather than its siblings.
#[test]
fn descending_makes_the_selected_directory_the_query() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);

    let selected = select_first_child(&mut editor);
    assert!(
        selected.ends_with('/'),
        "precondition: rows are directories: {selected}"
    );

    press(&mut editor, 'l');

    assert_eq!(
        query(&editor),
        selected,
        "the selected row's own path becomes the query — trailing slash and \
         all, because that is the prefix that lists its CONTENTS rather than \
         its siblings"
    );
}

/// `<C-h>` drops the last component. Round-tripping is the assertion, because
/// an ascend that merely *changed* the query would pass a one-sided test.
#[test]
fn ascending_undoes_a_descend() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);

    let before = query(&editor);
    select_first_child(&mut editor);
    press(&mut editor, 'l');
    assert_ne!(query(&editor), before, "precondition: the descend moved");

    press(&mut editor, 'h');

    assert_eq!(
        query(&editor),
        format!("{}/", root.to_string_lossy()),
        "back to the directory we were listing"
    );
}

/// PP.1: `../` descends OUT, which is the whole reason it is a row rather than
/// a legend. `<C-l>` on it must land exactly where `<C-h>` would — they share
/// `parent_of` so they cannot drift, and this is what says so through the real
/// keystroke path.
#[test]
fn descending_into_the_parent_row_goes_up() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let parent = format!("{}/", root.parent().unwrap().to_string_lossy());

    let mut up_by_row = open_dir_pick(&root);
    assert_eq!(
        up_by_row
            .picker
            .as_ref()
            .and_then(|p| p.selected_candidate())
            .map(|c| c.raw.display.clone()),
        Some("../".to_string()),
        "precondition: `../` is first and is what opens selected"
    );
    press(&mut up_by_row, 'l');

    let mut up_by_key = open_dir_pick(&root);
    press(&mut up_by_key, 'h');

    assert_eq!(query(&up_by_row), parent, "`<C-l>` on `../` goes up");
    assert_eq!(
        query(&up_by_key),
        parent,
        "and `<C-h>` goes to the same place"
    );
}

/// **PP.3: `<CR>` on `../` GOES UP — it does not choose the parent.**
///
/// PP.1 shipped the other reading (`../` is an ordinary row, so `<CR>`
/// supplies its path) and it was wrong in the way that only shows up in use:
/// at `~/`, `<CR>` on `../` supplied `/Users`, which the project flow then
/// refused with `` `/Users/` is not inside a project `` — an error message
/// where the user had asked to go up a level.
///
/// Asserted three ways, because "the query moved" alone would pass on a
/// version that accepted AND moved: the picker must still be open, and nothing
/// must have been resolved.
#[test]
fn accepting_the_parent_row_navigates_rather_than_choosing_it() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let parent = format!("{}/", root.parent().unwrap().to_string_lossy());
    let mut editor = open_dir_pick(&root);

    assert_eq!(
        editor
            .picker
            .as_ref()
            .and_then(|p| p.selected_candidate())
            .map(|c| c.raw.display.clone()),
        Some("../".to_string()),
        "precondition: `../` is what opens selected"
    );

    let out = editor.do_picker_accept();

    assert!(
        editor.picker.is_some(),
        "the picker stays OPEN — `<CR>` on `../` is navigation, and an accept \
         that closed the picker would have resolved something"
    );
    assert_eq!(query(&editor), parent, "and the query moved up one level");
    assert!(
        out.effects.is_empty(),
        "nothing was resolved: no effect, no value supplied, no command run — \
         got {:?}",
        out.effects
    );
}

/// …and `<CR>` on a CHILD still chooses it. `../` is the one row whose accept
/// means something different; a fix that made every row navigate would turn
/// `dir-pick` into a browser that can never answer the question it was opened
/// to answer.
#[test]
fn accepting_a_child_row_still_chooses_it() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);
    let child = select_first_child(&mut editor);

    let _ = editor.do_picker_accept();

    assert!(
        editor.picker.is_none(),
        "choosing closes the picker, where navigating left it open"
    );
    assert_ne!(
        query(&editor),
        child,
        "and the query did not become the row — that would be a descend"
    );
}

/// The query opens ON the start directory, which is what puts the current
/// directory in the prompt — the one line meant to orient you used to be the
/// only one carrying no path at all.
#[test]
fn the_picker_opens_on_the_directory_it_is_listing() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let editor = open_dir_pick(&root);

    assert_eq!(
        query(&editor),
        format!("{}/", root.to_string_lossy()),
        "the prompt reads the directory being listed, trailing slash and all"
    );
}

/// The root is a fixed point. Emptying the query there would silently
/// relocate the user somewhere they never asked to be.
#[test]
fn ascending_stops_at_the_filesystem_root() {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let _ = editor.open_picker(
        lattice_picker::DIR_PICK_SOURCE.to_string(),
        vec!["/".to_string()],
    );
    assert!(editor.picker.is_some(), "precondition: `/` seats a picker");

    for _ in 0..5 {
        press(&mut editor, 'h');
    }

    assert_eq!(
        query(&editor),
        "/",
        "five presses at the root leave the query at the root"
    );
}

/// **The one that could silently break every other picker.** `descend` and
/// `ascend` default to `None`, and a wiring that ignored the default would
/// give `<C-l>` a meaning in pickers that have none — rewriting a `buffers`
/// query to a candidate's text, which is not remotely what the key says.
///
/// `buffers` also is not live, which is the second half of the gate: a static
/// source's rows come from `init` and are fuzzy-refiltered, so rewriting its
/// query would filter the rows it already has rather than fetch new ones.
#[test]
fn a_picker_with_no_notion_of_depth_ignores_both_keys() {
    let mut editor = Editor::boot(CoreDocument::from_text("committed\n"));
    let _ = editor.open_picker("buffers".to_string(), Vec::new());
    assert!(editor.picker.is_some(), "precondition: `buffers` seats");

    let before_query = query(&editor);
    let before_rows = rows(&editor);

    press(&mut editor, 'l');
    press(&mut editor, 'h');

    assert_eq!(
        query(&editor),
        before_query,
        "`<C-l>` / `<C-h>` must not touch the query of a picker whose source \
         takes the `None` default"
    );
    assert_eq!(
        rows(&editor),
        before_rows,
        "and must not disturb its candidates either"
    );
}

/// Neither key may close the picker or accept anything. They are navigation,
/// and a `<C-l>` that fell through to accept would open whatever was
/// selected — the worst possible reading of "go deeper".
#[test]
fn neither_key_accepts_or_dismisses() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);

    press(&mut editor, 'l');
    assert!(editor.picker.is_some(), "`<C-l>` leaves the picker open");

    press(&mut editor, 'h');
    assert!(editor.picker.is_some(), "`<C-h>` leaves the picker open");
}

/// **The listing must follow the descend with NO further keystroke.**
///
/// Every test above asserts the QUERY moved, which is only half the gesture —
/// what the user is looking at is the list of rows, and the re-query that
/// produces it is debounced. A build where the debounce deadline is reached
/// but nothing wakes the loop passes all of them and still shows the parent's
/// children until the user types, which is the shape this exists to catch.
///
/// Waiting on `async_landed` rather than dispatching another action is the
/// whole point: dispatching would run `run_tick_pending` through the keystroke
/// tail and pass against a build with no wake scheduled at all (the hole
/// CLAUDE.md names).
#[tokio::test]
async fn descending_relists_without_a_keypress() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick(&root);

    // `alpha/` is the first child and the only one with a child of its own,
    // so "did the listing follow" has an unambiguous witness: `inner/`.
    let selected = select_first_child(&mut editor);
    assert!(
        selected.ends_with("alpha/"),
        "precondition: the first child row is `alpha/`; got {selected}"
    );
    assert!(
        !rows(&editor).iter().any(|r| r.ends_with("inner/")),
        "precondition: `inner/` is not listed before the descend"
    );

    press(&mut editor, 'l');

    let woke = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        editor.async_landed.notified(),
    )
    .await;
    assert!(
        woke.is_ok(),
        "the descend's re-query must wake the actor on its own"
    );

    let mut ticks = 0;
    while !rows(&editor).iter().any(|r| r.ends_with("inner/")) && ticks < 50 {
        editor.run_tick_pending();
        ticks += 1;
        if !rows(&editor).iter().any(|r| r.ends_with("inner/")) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    assert!(
        rows(&editor).iter().any(|r| r.ends_with("inner/")),
        "the descended directory's contents must be listed without the user \
         typing; rows were {:?}",
        rows(&editor)
    );
}

// ── The `… (choose a dir)` open path ──────────────────────────────────────
//
// Everything above opens `dir-pick` directly. The way a user actually reaches
// it is the project flow: the projects picker's row accepts, that accept is
// deferred (`accept_async`, as every plugin source's is), it invokes
// `project-choose-dir`, and THAT returns `Effect::OpenPicker { dir-pick }`,
// applied later by `drain_pending_picker_accept`. Same source, different
// opener — and the opener is the half that was never covered here.

const PROJECTS_FIXTURE: &str = "descend-projects-fixture";
const CHOOSE_DIR: &str = "descend-test-project-choose-dir";

/// Stand-in for `WasmPickerSource`: `accept_async` returns `Some`, so the
/// commit goes through `drain_pending_picker_accept` rather than
/// `do_picker_accept`'s synchronous return — the path the real flow takes.
struct AsyncProjectsSource {
    spec: lattice_picker::source::PickerSourceSpec,
}

impl lattice_picker::source::PickerSourceGenerator for AsyncProjectsSource {
    fn spec(&self) -> &lattice_picker::source::PickerSourceSpec {
        &self.spec
    }

    fn init(
        &self,
        _ctx: &lattice_picker::PickerContext<'_>,
        _args: &[String],
    ) -> lattice_picker::SourceResult<lattice_picker::source::PickerInitResult> {
        Ok(lattice_picker::source::PickerInitResult::Inline(Vec::new()))
    }

    fn accept(
        &self,
        _ctx: &lattice_picker::PickerContext<'_>,
        _routing: &lattice_picker::RoutingPayload,
    ) -> lattice_picker::SourceResult<lattice_picker::PickerAcceptOutcome> {
        Err("must resolve via accept_async".to_string())
    }

    fn accept_async(
        &self,
        _ctx: &lattice_picker::PickerContext<'_>,
        _routing: &lattice_picker::RoutingPayload,
    ) -> Option<lattice_picker::AcceptFuture> {
        Some(Box::pin(async move {
            tokio::task::yield_now().await;
            Ok(lattice_picker::PickerAcceptOutcome::InvokeCommand {
                id: CHOOSE_DIR.to_string(),
                args: lattice_grammar::Args::None,
            })
        }))
    }
}

/// Drive the flow up to "the directory picker is open", exactly as
/// `… (choose a dir)` does. Rooted at `root` rather than the plugin's
/// `args: []` (which browses `$HOME`) so the rows are the test's own tree.
fn open_dir_pick_via_choose_a_dir(root: &std::path::Path) -> Editor {
    use lattice_picker::source::{PickerSourceGenerator, PickerSourceSpec};

    let editor = Editor::boot(CoreDocument::from_text("committed\n"));

    // Hop 1's command, as `plugins/project` writes it: open the directory
    // sub-picker, naming a fill action so the capture path is live too.
    let open_at = root.to_string_lossy().to_string();
    {
        let reg = editor
            .services
            .get::<lattice_grammar::CommandRegistryHandle>()
            .expect("the command registry is a boot service");
        let mut next = (**reg.load()).clone();
        next.register_ex_command(
            CHOOSE_DIR,
            "descend regression fixture",
            lattice_grammar::registry::ExCommandSpec {
                latency_class: lattice_grammar::command::LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                parse_args: std::sync::Arc::new(|rest: &str, _bang: bool| {
                    Ok(lattice_grammar::Args::String(rest.to_string()))
                }),
                apply: std::sync::Arc::new(move |_ctx| {
                    Ok(lattice_grammar::Effect::OpenPicker {
                        source: lattice_picker::DIR_PICK_SOURCE.to_string(),
                        args: vec![open_at.clone()],
                        root: None,
                        fill_action: Some("descend-remember-and-switch".to_string()),
                    })
                }),
                args_schema: vec![],
                surface_form: lattice_grammar::registry::SurfaceForm::Keyword,
            },
        );
        reg.store(std::sync::Arc::new(next));
    }

    let mut pickers = (**editor.picker_registry.load()).clone();
    pickers.register_generator(std::sync::Arc::new(AsyncProjectsSource {
        spec: PickerSourceSpec::no_args(PROJECTS_FIXTURE, "descend regression: async projects."),
    }) as std::sync::Arc<dyn PickerSourceGenerator>);
    editor.picker_registry.store(std::sync::Arc::new(pickers));

    let mut editor = editor;
    let cand = lattice_completion::candidate::RawCandidate::plain(
        "\u{2026} (choose a dir)".to_string(),
        lattice_completion::candidate::CandidateKind::Plain,
    );
    editor.seat_picker_from_pairs(
        PROJECTS_FIXTURE.to_string(),
        vec![(cand, lattice_picker::RoutingPayload::Buffer { id: 0 })],
    );

    let _ = editor.do_picker_accept();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline && editor.pending_picker_accept.is_some() {
        let _ = editor.drain_pending_picker_accept();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        editor.picker.is_some(),
        "precondition: `… (choose a dir)` seated the directory picker"
    );
    editor
}

/// **The reported bug.** `<C-l>` (and `<Tab>`) in the directory picker reached
/// through `… (choose a dir)` moves the query but leaves the previous rows on
/// screen — the descended directory's contents only appear once the user
/// starts typing to filter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn descending_relists_when_opened_via_choose_a_dir() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick_via_choose_a_dir(&root);

    let selected = select_first_child(&mut editor);
    assert!(
        selected.ends_with("alpha/"),
        "precondition: the first child row is `alpha/`; got {selected}"
    );

    press(&mut editor, 'l');
    assert_eq!(query(&editor), selected, "precondition: the descend moved");

    let woke = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        editor.async_landed.notified(),
    )
    .await;
    assert!(
        woke.is_ok(),
        "the descend's re-query must wake the actor on its own"
    );

    let mut ticks = 0;
    while !rows(&editor).iter().any(|r| r.ends_with("inner/")) && ticks < 50 {
        editor.run_tick_pending();
        ticks += 1;
        if !rows(&editor).iter().any(|r| r.ends_with("inner/")) {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    assert!(
        rows(&editor).iter().any(|r| r.ends_with("inner/")),
        "the descended directory's contents must be listed without the user \
         typing; rows were {:?}",
        rows(&editor)
    );
}

/// **The reported bug, with the step that actually triggers it: a FILTER is
/// typed before the descend.**
///
/// `descending_relists_when_opened_via_choose_a_dir` descends from a freshly
/// opened picker, where nothing is in flight. The real gesture is
/// `src<Tab>` — type to narrow, then drill in — and the typing leaves a live
/// re-query mid-flight that the descend's own re-query has to supersede. What
/// the user sees is the path updating to `~/src/` while the rows stay the ones
/// that matched `src` in the parent, until another keystroke shakes them loose.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn descending_after_typing_a_filter_relists() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick_via_choose_a_dir(&root);

    // `src<Tab>`, in this tree's vocabulary: narrow to `alpha`, then drill in.
    for c in "alpha".chars() {
        editor.dispatch(Action::PickerAppend(c));
    }
    // Wait for the TYPED query to land, not merely for `alpha/` to be present
    // — it already is, unfiltered, so a looser predicate returns before the
    // re-query and leaves the selection on `../`, which `<Tab>` then walks UP.
    settle_live_query(&mut editor, |e| {
        let r = rows(e);
        r.len() == 1 && r[0].ends_with("alpha/")
    })
    .await;
    assert_eq!(
        rows(&editor).len(),
        1,
        "precondition: typing `alpha` narrowed the listing to one row; got {:?}",
        rows(&editor)
    );

    // `<Tab>`, not `<C-l>`: the user reaches this with both, and `<Tab>` has a
    // fall-through to `select_next` that `<C-l>` does not — a descend that
    // silently declined would move the SELECTION here and look like this bug.
    editor.dispatch(Action::PickerDescendOrSelectNext);
    assert!(
        query(&editor).ends_with("alpha/"),
        "precondition: the descend moved; query is {:?}",
        query(&editor)
    );

    settle_live_query(&mut editor, |e| {
        rows(e).iter().any(|r| r.ends_with("inner/"))
    })
    .await;

    assert!(
        rows(&editor).iter().any(|r| r.ends_with("inner/")),
        "the descended directory's contents must be listed without the user \
         typing again; rows were {:?}",
        rows(&editor)
    );
}

/// Pump the off-keystroke path — wait for the wake, then run the tick
/// aggregator — until `done`, or give up. Deliberately never dispatches an
/// action: doing so would run `run_tick_pending` through the keystroke tail
/// and pass against the very build this is trying to catch.
async fn settle_live_query(editor: &mut Editor, done: impl Fn(&Editor) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !done(editor) && std::time::Instant::now() < deadline {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            editor.async_landed.notified(),
        )
        .await;
        editor.run_tick_pending();
    }
}

/// **The bug itself: the new rows must reach a FRAME, not just `Editor`.**
///
/// Every other test here reads `editor.picker.candidates`, and the host state
/// was never the broken half — the re-query landed, the rows were correct, and
/// the screen showed the previous listing anyway. The actor's `async_landed`
/// arm repaints only when `publish_render_state()` reports that
/// `paint_revision` moved, and that hash folded in `picker.is_some()` and
/// nothing about its contents. So the publish carrying the descended
/// directory's rows said "nothing moved", `paint_request` never fired, and the
/// user saw the old listing until the next keystroke — which is precisely the
/// report: the path updates, the folders do not.
///
/// This mirrors the actor arm's sequence (`run_tick_pending` → publish → paint
/// if it moved) and asserts on the publish that actually carried the change.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_relisting_moves_the_paint_gate() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick_via_choose_a_dir(&root);

    select_first_child(&mut editor);
    // Seed `last_paint_revision`, so what follows is measured against a
    // published baseline rather than against the boot state.
    let _ = editor.publish_render_state();

    editor.dispatch(Action::PickerDescendOrSelectNext);
    // The keystroke's own publish: the query moved, so this one paints. It is
    // also what made the bug survivable-looking — the path visibly updated.
    let _ = editor.publish_render_state();

    let mut painted_when_rows_landed: Option<bool> = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while painted_when_rows_landed.is_none() && std::time::Instant::now() < deadline {
        let before = rows(&editor);
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            editor.async_landed.notified(),
        )
        .await;
        editor.run_tick_pending();
        let painted = editor.publish_render_state();
        if rows(&editor) != before {
            painted_when_rows_landed = Some(painted);
        }
    }

    assert_eq!(
        painted_when_rows_landed,
        Some(true),
        "the publish that carried the descended listing must report a moved \
         paint revision, or the actor never fires `paint_request` and the rows \
         wait for a keystroke"
    );
}

/// The gate must not go the other way either: the stamp moves on a re-filter
/// and on nothing else.
///
/// A stamp re-taken on every publish (rather than on every `refilter`) would
/// fix the bug above and then report a repaint forever, spinning the GPUI
/// paint bridge — a worse trade than the bug. This asserts the stamp is stable
/// across idle ticks once the listing has settled.
///
/// It asserts on the STAMP rather than on `publish_render_state()` returning
/// false, deliberately: an idle tick over an open live picker does report a
/// repaint today for reasons that predate this fix (verified by reverting the
/// hash change — the same assertion fails either way), so a publish-level
/// guard here would be pinning someone else's behaviour and would go red on
/// the day they fix it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_stamp_moves_only_when_the_rows_do() {
    let dir = tree();
    let root = dir.path().canonicalize().unwrap();
    let mut editor = open_dir_pick_via_choose_a_dir(&root);

    let stamp = |e: &Editor| e.picker.as_ref().map(|p| p.revision);
    let before_descend = stamp(&editor);

    select_first_child(&mut editor);
    assert_eq!(
        stamp(&editor),
        before_descend,
        "moving the selection re-filters nothing, so the stamp holds"
    );

    editor.dispatch(Action::PickerDescendOrSelectNext);
    settle_live_query(&mut editor, |e| {
        rows(e).iter().any(|r| r.ends_with("inner/"))
    })
    .await;
    let after_relist = stamp(&editor);
    assert_ne!(
        after_relist, before_descend,
        "the re-listing re-filtered, so the stamp moved"
    );

    // …and then settles. Three idle ticks with no query change must not mint
    // three more stamps.
    for _ in 0..3 {
        editor.run_tick_pending();
    }
    assert_eq!(
        stamp(&editor),
        after_relist,
        "idle ticks must not re-stamp; a stamp that moved every tick would \
         report a repaint forever"
    );
}
