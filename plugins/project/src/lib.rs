//! `project` — a `project.el`-style command layer (PC.4).
//!
//! Design: `docs/dev/architecture/project-commands.md`. Slice plan:
//! `docs/dev/operations/slice-plans/project-commands.md`.
//!
//! ## What this plugin is for
//!
//! Every project-aware surface in lattice resolves its root **implicitly, from
//! the buffer you are standing in** — `:files`, `:terminal`, `:search`, magit.
//! That is right until it is exactly wrong: a file open in project A, wanting to
//! open one in project B. The missing verb is *choose the project first*.
//!
//! ## What it deliberately does NOT do
//!
//! It never RESOLVES a project — it only reads `project.root-for-*`.
//! `wit/project.wit` draws that line and gives the reason: resolution is core
//! (terminal, compilation, search, the file picker and magit all root from it),
//! so it can never depend on a plugin being alive. The same header hands this
//! plugin its job: *"No file listing, no project list, no switching — those are
//! the plugin's job."*
//!
//! It also never touches the filesystem. `plugin.toml` grants `state:write` and
//! nothing else — the host resolves roots, the native pickers do every walk, and
//! the store holds the list. That absence is load-bearing rather than
//! minimalism: it is why a remembered project whose directory has been deleted
//! is not auto-pruned (the plugin cannot tell a deleted tree from an unmounted
//! volume), and why `:project-forget` exists instead.
//!
//! ## PC.4's scope
//!
//! The remembered list and the two commands that seed and unseed it. The picker,
//! the verbs and the switch-commands menu are PC.5–PC.7; this slice is the state
//! they all read.

wit_bindgen::generate!({
    world: "project-plugin",
    path: "../../wit",
});

use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::events::EventFilter;
use lattice::plugin_host::host_services;
use lattice::plugin_host::project::{self, ProjectKind};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ArgDefault, ArgKind, ArgSpec, Args, EchoLevel, EchoPayload, Effect, EventKind,
    ExCommandContext, ExCommandSpec, LatencyClass, MotionContext, MotionResult, OperatorContext,
    Range, SurfaceForm, TextObjectContext,
};
// `Event` is already in scope from the world's own `use types.{event}` — the
// world-level `use` and an explicit import of the same name collide.

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use exports::lattice::plugin_host::picker_source::{CandidatePair, Guest as PickerSource};
use exports::lattice::plugin_host::transient_source::Guest as TransientSource;
use lattice::plugin_host::modes::{
    self, ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding,
    ModeKind,
};
use lattice::plugin_host::types::{
    OpenPickerPayload, OpenTransientPayload, PickerAcceptOutcome, PickerContext, RoutingPayload,
    SpawnTerminalPayload, TransientAction, TransientContext, TransientGroup, TransientItem,
    TransientItemKind, TransientSpec,
};

mod picker;
mod projects;
mod switch;

/// The store key holding the whole list.
///
/// ONE key, not a key per project. The list is read whole (the picker shows all
/// of it) and written whole on change, so a key per project would buy nothing
/// and cost a `store-keys` scan on every document open.
const STORE_KEY: &str = "projects";

// Ex-command callback ids. `parse` is shared: every command here takes the same
// optional single path argument, so one parser serves all of them and there is
// no per-command parsing that could disagree.
const CB_PARSE: u32 = 0;
const CB_REMEMBER: u32 = 1;
const CB_FORGET: u32 = 2;
const CB_SWITCH: u32 = 3;
const CB_FIND_FILE: u32 = 4;
const CB_DIRED: u32 = 5;
const CB_SWITCH_TO: u32 = 6;
const CB_GREP: u32 = 7;
const CB_SHELL: u32 = 8;
/// PC.12. `9` was free; `10` is `ON_DOCUMENT_OPENED`, a handler id in a
/// different namespace that happens to sit next door — kept apart on purpose
/// rather than renumbered, since the event id crosses a different seam.
const CB_CHOOSE_DIR: u32 = 9;
const CB_REMEMBER_AND_SWITCH: u32 = 11;
/// PB.1. `10` stays skipped for the reason above — `ON_DOCUMENT_OPENED` lives
/// at that number in the event-handler namespace, and reusing the digit here
/// would make the two tables read as if they collided.
const CB_BUFFERS: u32 = 12;

/// The native live-grep picker. Rooted through the OPEN's root (PC.1), not
/// through an argument: `grep` re-queries on every keystroke via
/// `on-query-changed`, which sees the context and not the open's args.
const GREP_PICKER: &str = "grep";

/// The mode owning both prefixes' chords.
const MODE_ID: &str = "project-mode";

/// The transient this plugin registers. One source per component (the seam is
/// shaped that way), so it dispatches on `transient-context.args`.
const SWITCH_TRANSIENT: &str = "project-switch";

/// The native file picker, driven at an explicit root.
///
/// **Not re-implemented**, deliberately: its source already reads `args[0]` as
/// its root, with the comment "an explicit `:picker files <path>` still wins —
/// that is the user saying 'not that project, this one'." That sentence is this
/// whole feature, already built; the plugin's job is only to decide WHICH root.
const FILES_PICKER: &str = "files";

/// PC.9's native directory picker — `file-pick`'s peer, browsing one level at
/// a time. Native and not plugin-local because this plugin holds `state:write`
/// and no `fs:` grant: it cannot list a directory at all, which is the same
/// constraint that makes §4's no-auto-pruning honest.
const DIR_PICKER: &str = "dir-pick";

/// The `document-opened` subscription's handler id.
const ON_DOCUMENT_OPENED: u32 = 10;

struct Component;

// ── The list, through the store ─────────────────────────────────────────────

/// Read the remembered list.
///
/// A `none` from `store-get` covers every degraded case — no grant, no data
/// dir, a store discarded as corrupt — and the seam's own doc says a reader for
/// whom absence is ordinary cannot distinguish them and does not need to. Here
/// absence genuinely is ordinary: it is a fresh install.
fn load() -> Vec<String> {
    host_services::store_get(STORE_KEY)
        .map(|bytes| projects::decode(&bytes))
        .unwrap_or_default()
}

/// Persist the list. The `Err` is returned rather than swallowed so a command
/// can echo it — a `:project-remember` that reports success and stored nothing
/// is precisely the silent failure this plugin must not have.
fn save(list: &[String]) -> Result<(), String> {
    host_services::store_put(STORE_KEY, &projects::encode(list))
}

/// The project a buffer belongs to, or `None` when there is not one.
///
/// `kind = pwd` means the editor's working directory standing in — the seam
/// documents that a guest wanting to say "not in a project" checks for this
/// rather than for an absent root. A list of *projects* that accumulated the cwd
/// would put `~` in front of the user forever, so this is where that is refused.
fn project_of_buffer(buffer: u64) -> Option<String> {
    let info = project::root_for_buffer(buffer)?;
    (info.kind != ProjectKind::Pwd).then_some(info.root)
}

/// The project containing a path the user typed.
fn project_of_path(path: &str) -> Option<String> {
    let info = project::root_for_path(path)?;
    (info.kind != ProjectKind::Pwd).then_some(info.root)
}

/// PP.4: the project for a path the user NAMED — and **a directory with no
/// root marker above it is a project, because the user said so.**
///
/// This reverses design §5's one real refusal: *"A directory with no root
/// marker above it is the one real refusal, and it is reported by the existing
/// `project: `…` is not inside a project` path."* The argument for it was that
/// the plugin cannot know what a project is without asking the host, which is
/// true and is not the same claim — the host answers "is there a marker above
/// this", and that question was standing in for "is this a project" without
/// ever being it.
///
/// A directory of notes, a scratch tree, a vendored drop, anything not yet
/// `git init`-ed: all are projects if you want to work in them, and every verb
/// this plugin has (`find-file`, `grep`, `dired`, `shell`, `buffers`) works
/// perfectly well rooted at a plain directory. The refusal bought nothing and
/// cost the whole flow — browsing to a folder and being told it does not count
/// is the picker declining to do the one thing it was opened to do.
///
/// **Resolution stays as a preference, not a gate.** A marker above the path
/// still wins, so `:project-remember .` inside a checkout still names the
/// checkout rather than the subdirectory you happen to be standing in, and a
/// path to a FILE still names its project rather than storing a file as a
/// project. Only the `None` case changed: it used to refuse and now takes the
/// path.
fn project_root_or_path(path: &str) -> String {
    project_of_path(path).unwrap_or_else(|| path.to_string())
}

fn echo(level: EchoLevel, text: String) -> Vec<Effect> {
    vec![Effect::Echo(EchoPayload { level, text })]
}

fn warn(text: String) -> Vec<Effect> {
    echo(EchoLevel::Warn, text)
}

// ── Remembering ─────────────────────────────────────────────────────────────

/// Record a visit. Called from the `document-opened` handler and from
/// `:project-remember`, so the two cannot drift on what "remembered" means.
///
/// Returns the message to echo, or `None` when there is nothing worth saying —
/// which is the document-opened case: every file you open would otherwise
/// announce its project.
fn remember_root(root: &str) -> Option<String> {
    let mut list = load();
    match projects::remember(&mut list, root) {
        // Nothing moved — skip the write. This is the common case on the
        // `document-opened` path, and writing bytes the store already holds
        // once per file opened is the one place this plugin could have been
        // needlessly hot.
        Ok(false) => None,
        Ok(true) => match save(&list) {
            Ok(()) => None,
            Err(e) => Some(format!("project: could not save the project list: {e}")),
        },
        Err(projects::Refused::Unstorable) => Some(format!(
            "project: cannot remember `{root}` — a project path may not contain a newline"
        )),
    }
}

/// `:project-remember [dir]` — seed a project without opening a file in it.
///
/// **The cold-start hole this fills is the motivating example verbatim.**
/// Remembering-on-visit cannot reach a project you have never opened, so
/// without this the feature's answer to "open a file in project B" would be
/// "open a file in project B the hard way first". `project.el` carries the same
/// escape hatch (`project-remember-projects-under`) for the same reason.
fn cmd_remember(ctx: &ExCommandContext) -> Vec<Effect> {
    let root = match arg_path(&ctx.args) {
        // An explicit path: a marker above it still wins, so `:project-remember .`
        // and a path to a file inside the tree both name the project rather
        // than the argument. PP.4: a path with NO marker above it is taken as
        // the project itself rather than refused — see `project_root_or_path`.
        Some(path) => project_root_or_path(&path),
        None => match project_of_buffer(ctx.buffer_id as u64) {
            Some(root) => root,
            None => return warn("project: this buffer is not inside a project".to_string()),
        },
    };
    if let Some(message) = remember_root(&root) {
        return warn(message);
    }
    echo(EchoLevel::Info, format!("project: remembered {root}"))
}

/// `:project-forget [dir]` — drop a project from the list.
///
/// Nothing is deleted from disk and nothing is verified: the plugin holds no
/// `fs:` grant, so this is purely a retraction of "I care about this one".
fn cmd_forget(ctx: &ExCommandContext) -> Vec<Effect> {
    let root = match arg_path(&ctx.args) {
        // NOT resolved through `root-for-path` — a project whose directory has
        // been deleted is exactly the one you want to forget, and resolving
        // would refuse it. The stored spelling is what a user reads out of the
        // picker, so the stored spelling is what this matches.
        Some(path) => projects::normalize(&path),
        None => match project_of_buffer(ctx.buffer_id as u64) {
            Some(root) => root,
            None => return warn("project: this buffer is not inside a project".to_string()),
        },
    };
    let mut list = load();
    if !projects::forget(&mut list, &root) {
        return warn(format!("project: `{root}` was not remembered"));
    }
    match save(&list) {
        Ok(()) => echo(EchoLevel::Info, format!("project: forgot {root}")),
        Err(e) => warn(format!("project: could not save the project list: {e}")),
    }
}

/// The project a command should act on: the argument if given, else the one
/// this buffer is in.
///
/// Shared by every verb so they cannot disagree about what "no argument" means.
/// The bare form is `project.el`'s `C-x p f` — act on the project I am already
/// in — and the argument form is what the picker's accept supplies.
fn target_root(ctx: &ExCommandContext) -> Result<String, Vec<Effect>> {
    match arg_path(&ctx.args) {
        Some(path) => Ok(projects::normalize(&path)),
        None => project_of_buffer(ctx.buffer_id as u64)
            .ok_or_else(|| warn("project: this buffer is not inside a project".to_string())),
    }
}

/// `:project-switch` — open the projects picker.
fn cmd_switch() -> Vec<Effect> {
    vec![Effect::OpenPicker(OpenPickerPayload {
        source: picker::PROJECTS_PICKER.to_string(),
        args: Vec::new(),
        root: None,
        fill_action: None,
    })]
}

/// PC.12: `:project-choose-dir` — the second surface of `project.el`'s
/// `… (choose a dir)`.
///
/// Opens the native `dir-pick` sub-picker over the projects picker and names
/// this plugin's own command as where the answer goes (PC.11's
/// `fill-action`). A guest cannot receive a picked value any other way: every
/// other fill target is a host surface — the document, the `:` line, a prompt,
/// a transient argument — and a plugin owns none of them.
///
/// No `root`: `dir-pick` browses from the HOME directory by default, which is
/// the right start for "find a project I have not opened". Rooting it at the
/// current project would begin the search in the one place it is not.
fn cmd_choose_dir() -> Vec<Effect> {
    vec![Effect::OpenPicker(OpenPickerPayload {
        source: DIR_PICKER.to_string(),
        args: Vec::new(),
        root: None,
        fill_action: Some(picker::REMEMBER_AND_SWITCH_COMMAND.to_string()),
    })]
}

/// PC.12: `:project-remember-and-switch <path>` — a path becomes a project,
/// and you carry straight on to the switch-commands menu.
///
/// **One hop, which is `project.el`'s shape.** `project-switch-project` does
/// not hand you back to the project list to confirm a directory you just
/// chose; the choice IS the answer. So this remembers and opens the menu in
/// the same breath.
///
/// Resolved through `project_of_path` rather than stored verbatim, which is
/// the difference between this and [`cmd_switch_to`]. That one is fed by the
/// picker with a root the list already holds; this one is fed a path a human
/// (or a directory walk) named, so `~/src/lattice/crates` has to become
/// `~/src/lattice`. Storing what was typed would put a subdirectory in the
/// project list and every later switch would root one level too deep.
fn cmd_remember_and_switch(ctx: &ExCommandContext) -> Vec<Effect> {
    let Some(path) = arg_path(&ctx.args) else {
        return warn("project: choose a directory first".to_string());
    };
    let root = project_root_or_path(&path);
    if let Some(message) = remember_root(&root) {
        return warn(message);
    }
    vec![Effect::OpenTransient(OpenTransientPayload {
        source: SWITCH_TRANSIENT.to_string(),
        args: Args::String(root),
    })]
}

/// `:project-find-file [root]` — the native file picker, rooted at a project.
fn cmd_find_file(ctx: &ExCommandContext) -> Vec<Effect> {
    match target_root(ctx) {
        Ok(root) => {
            // Remembered on the way through, so switching to a project through
            // the picker refreshes its recency even when you open nothing. The
            // `document-opened` subscription would only fire if you went on to
            // pick a file.
            let _ = remember_root(&root);
            vec![Effect::OpenPicker(OpenPickerPayload {
                source: FILES_PICKER.to_string(),
                // PC.1: the root rides the CONTEXT, not the args. `files` would
                // also accept `args[0]`, but `grep` would not survive its own
                // first keystroke that way — `on-query-changed` sees the
                // context and not the open's args. One mechanism for every
                // root-sensitive source beats a per-source convention.
                args: Vec::new(),
                root: Some(root),
                fill_action: None,
            })]
        }
        Err(effects) => effects,
    }
}

/// PB.1: `:project-buffers [root]` — the open buffers inside a project.
///
/// `project.el`'s `project-switch-to-buffer`. The everyday half of the project
/// verbs rather than the switching half: `:b` lists every buffer across every
/// checkout, which is right for `:b` and wrong when you are inside one project
/// and want the handful of files that belong to it.
///
/// **Not remembered on the way through**, unlike `find-file` / `dired` /
/// `grep` / `shell`. Those four can land you in a project you have not
/// recorded; this one can only list buffers that are already open, and opening
/// them is what remembered the project in the first place (`document-opened`).
/// Recording it again here would touch the store on a keystroke to write bytes
/// it already holds.
fn cmd_buffers(ctx: &ExCommandContext) -> Vec<Effect> {
    match target_root(ctx) {
        Ok(root) => vec![Effect::OpenPicker(OpenPickerPayload {
            source: picker::PROJECT_BUFFERS_PICKER.to_string(),
            args: Vec::new(),
            root: Some(root),
            fill_action: None,
        })],
        Err(effects) => effects,
    }
}

/// `:project-dired [root]` — the directory browser, rooted at a project.
fn cmd_dired(ctx: &ExCommandContext) -> Vec<Effect> {
    match target_root(ctx) {
        Ok(root) => {
            let _ = remember_root(&root);
            vec![Effect::OpenOil(Some(root))]
        }
        Err(effects) => effects,
    }
}

/// `:project-switch-to <root>` — the second hop of the project picker.
///
/// Two hops because `picker-accept-outcome` has no "open a transient" arm and
/// should not grow one: an accept resolves to a typed outcome, and opening a
/// menu is an effect. `invoke-command` is the arm that bridges them, which is
/// the route `roam_insert`'s create row already takes.
fn cmd_switch_to(ctx: &ExCommandContext) -> Vec<Effect> {
    match target_root(ctx) {
        Ok(root) => {
            let _ = remember_root(&root);
            vec![Effect::OpenTransient(OpenTransientPayload {
                source: SWITCH_TRANSIENT.to_string(),
                args: Args::String(root),
            })]
        }
        Err(effects) => effects,
    }
}

/// The configured rows, or the defaults.
fn switch_commands() -> Vec<switch::SwitchCommand> {
    match lattice::plugin_host::config::get_option_value(switch::OPTION) {
        Some(value) => switch::from_value(&value),
        // Unregistered or unreadable — the same answer either way, and it is
        // the useful one: a menu with no rows looks exactly like a broken chord.
        None => switch::defaults(),
    }
}

/// `:project-grep [root]` — live grep, rooted at a project.
///
/// The pattern is NOT passed: `grep` opens empty and greps as you type, which
/// is the surface the source was built for. Seeding a pattern would mean asking
/// for one before showing the picker, and the picker is where you refine it.
fn cmd_grep(ctx: &ExCommandContext) -> Vec<Effect> {
    match target_root(ctx) {
        Ok(root) => {
            let _ = remember_root(&root);
            vec![Effect::OpenPicker(OpenPickerPayload {
                source: GREP_PICKER.to_string(),
                // `args[0]` is grep's PATTERN, not its root — the asymmetry
                // with `files` is why PC.1 put the root in the context instead
                // of inventing a second argument convention.
                args: Vec::new(),
                root: Some(root),
                fill_action: None,
            })]
        }
        Err(effects) => effects,
    }
}

/// `:project-shell [root]` — a terminal in a project.
fn cmd_shell(ctx: &ExCommandContext) -> Vec<Effect> {
    match target_root(ctx) {
        Ok(root) => {
            let _ = remember_root(&root);
            vec![Effect::SpawnTerminal(SpawnTerminalPayload {
                // PC.2. Binding the cwd at SPAWN is what lets several projects
                // coexist: `Command::cwd` applies once, so a shell already
                // running is the OS's business and nothing resolved later can
                // move it.
                cwd: Some(root),
                // `None` spawns `$SHELL`, which is what `project.el`'s
                // `project-shell` means.
                cmd_line: None,
                env: Vec::new(),
                activate_minor: None,
            })]
        }
        Err(effects) => effects,
    }
}

/// The single optional path argument every command here takes.
fn arg_path(args: &Args) -> Option<String> {
    match args {
        Args::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// The shared spec: one optional path, no bang, no range.
///
/// `Reflex` because none of these touches the filesystem or waits on anything —
/// they read and write one small store value.
/// PC.12: no arguments at all. `:project-choose-dir` takes none — the picker
/// it opens is the argument.
fn no_arg_spec() -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    }
}

fn path_arg_spec(doc: &str, prompt: &str) -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        args_schema: vec![ArgSpec {
            name: "dir".to_string(),
            kind: ArgKind::String,
            doc: doc.to_string(),
            prompt: prompt.to_string(),
            // Optional: the bare form means "this buffer's project", which is
            // the common call and must not prompt for a path the editor can
            // already work out.
            default: ArgDefault::None,
            // PC.13. These were both `None` from PC.4, so `:project-remember
            // <Tab>` has never completed a path — a command whose whole
            // argument is a directory, offering nothing when you ask it for
            // one. Nothing justified it; the generator and the picker simply
            // were not wired.
            //
            // `gen:directories` for `<Tab>` (inline, one component at a time)
            // and `dir-pick` for `<C-x><C-o>` (the richer surface, PC.9). An
            // argument may legitimately declare both — they answer the same
            // question at different weights, which is what `ArgSpec`'s own
            // doc says the split is for.
            completion: Some("gen:directories".to_string()),
            picker: Some(DIR_PICKER.to_string()),
        }],
        surface_form: SurfaceForm::Keyword,
    }
}

impl Guest for Component {
    fn register_grammar() {
        lattice::plugin_host::grammar::register_ex_command(
            "project-remember",
            "Remember a project so it appears in the project picker. With no \
             argument, remembers this buffer's project; with a path, the project \
             containing it — which is how you reach a project you have never \
             opened a file in.",
            &path_arg_spec(
                "a directory or file inside the project; defaults to this buffer's project",
                "Remember project: ",
            ),
            CB_PARSE,
            CB_REMEMBER,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-forget",
            "Drop a project from the project picker. Nothing on disk is touched \
             — this only retracts the entry, which is what you want for a \
             project that has been deleted or moved.",
            &path_arg_spec(
                "the project path as the picker shows it; defaults to this buffer's project",
                "Forget project: ",
            ),
            CB_PARSE,
            CB_FORGET,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-switch",
            "Choose a project, then act on it. The verb this whole plugin \
             exists for: every other project-aware surface roots itself at the \
             buffer you are standing in, which is right until you want the one \
             you are not.",
            &ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                args_schema: Vec::new(),
                surface_form: SurfaceForm::Keyword,
            },
            CB_PARSE,
            CB_SWITCH,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-find-file",
            "Open a file in a project. With no argument, this buffer's project; \
             with a path, that one — which is what the project picker passes.",
            &path_arg_spec(
                "the project to search; defaults to this buffer's project",
                "Find file in project: ",
            ),
            CB_PARSE,
            CB_FIND_FILE,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-dired",
            "Browse a project's directory tree. With no argument, this buffer's \
             project; with a path, that one.",
            &path_arg_spec(
                "the project to browse; defaults to this buffer's project",
                "Browse project: ",
            ),
            CB_PARSE,
            CB_DIRED,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-buffers",
            "Switch to an open buffer inside a project. `:b` lists every buffer \
             across every checkout; this lists one project's. With no argument, \
             this buffer's project; with a path, that one.",
            &path_arg_spec(
                "the project whose buffers to list; defaults to this buffer's project",
                "Buffers in project: ",
            ),
            CB_PARSE,
            CB_BUFFERS,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-switch-to",
            "Open the project-switch menu for a project. The second hop of the \
             project picker — `picker-accept-outcome` has no arm for opening a \
             menu, so the accept routes here and this returns the effect.",
            &path_arg_spec(
                "the project the menu acts on; defaults to this buffer's project",
                "Switch to project: ",
            ),
            CB_PARSE,
            CB_SWITCH_TO,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-grep",
            "Search a project with live grep. With no argument, this buffer's \
             project; with a path, that one.",
            &path_arg_spec(
                "the project to search; defaults to this buffer's project",
                "Grep project: ",
            ),
            CB_PARSE,
            CB_GREP,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "project-shell",
            "Open a shell in a project. With no argument, this buffer's \
             project; with a path, that one.",
            &path_arg_spec(
                "the project to open a shell in; defaults to this buffer's project",
                "Shell in project: ",
            ),
            CB_PARSE,
            CB_SHELL,
        );
        // PC.12: the two hops of `… (choose a dir)`. Registered rather than
        // kept private because a plugin's picker rows route through the
        // command registry — `PickerAcceptOutcome::InvokeCommand` names a
        // command, and an unregistered name is a row that does nothing.
        //
        // Both take no argument from the USER — the first takes none at all,
        // the second is handed a path by the picker — so neither declares a
        // prompt. A `:project-choose-dir` typed by hand is a perfectly good
        // way in, which is why it is documented rather than hidden.
        lattice::plugin_host::grammar::register_ex_command(
            picker::CHOOSE_DIR_COMMAND,
            "Browse the filesystem for a project directory — any folder will do, \
             it need not be a git repo. `<C-l>` descends into the selected \
             directory, `<C-h>` goes back up, `<CR>` chooses the one you are on \
             (or goes up, on the `../` row) — and the chosen one is remembered \
             and opened.",
            &no_arg_spec(),
            CB_PARSE,
            CB_CHOOSE_DIR,
        );
        lattice::plugin_host::grammar::register_ex_command(
            picker::REMEMBER_AND_SWITCH_COMMAND,
            "Remember the project containing a path and open its \
             switch-commands menu. The second hop of `project-choose-dir`; \
             takes a path rather than a project root, and resolves it.",
            &path_arg_spec(
                "a directory or file inside the project to remember and switch to",
                "Project directory: ",
            ),
            CB_PARSE,
            CB_REMEMBER_AND_SWITCH,
        );
    }

    /// PC.6: `project-mode`, a `universal` minor owning BOTH prefixes.
    ///
    /// Universal — the `org-global-mode` / `magit-global-mode` precedent, and
    /// for their reason: the verbs are global, and a project picker that only
    /// worked inside a project would be useless for the case it exists for.
    ///
    /// The chords live at `MinorMode(project-mode)`, never the builtin layer,
    /// which is reserved for universal vim grammar.
    ///
    /// **`<C-x>p` is bound unconditionally, and the cost is real:**
    /// `:set noemacs-keys` no longer fully reclaims `<C-x>` — it stays
    /// half-alive with this one sub-chord. The design wanted it gated on the
    /// option, and that is not buildable: a plugin registers keymaps at LOAD
    /// (`register-modes` / `keymap.register-binding`) and there is no
    /// unregister and no runtime push/pop. See `project-commands.md` §8, which
    /// records the three rejected alternatives.
    fn register_modes() {
        let bind = |chord: &str, command: &str| ModeKeymapBinding {
            binding_mode: BindingMode::Normal,
            chord: chord.to_string(),
            command: command.to_string(),
        };
        // `project.el`'s own letters, under both prefixes, so the muscle
        // memory transfers whichever one a user reaches for.
        //
        // **PK.1: every menu row has a chord, and that is the invariant.**
        // `g`, `s` and `v` were menu-only — the switch menu offered them on a
        // chosen project while the keymap offered nothing for the project you
        // were already in, so the everyday half of §6's two entry points was
        // missing for exactly half the verbs. `project.el` binds all of them
        // (`C-x p g` / `C-x p s` / `C-x p v`), and `switch.rs`'s defaults and
        // this list are now the same set of letters on purpose;
        // `every_menu_row_has_a_chord` holds them together.
        let verbs = [
            ("p", "project-switch"),
            ("f", "project-find-file"),
            // PB.1: `b`, `project.el`'s own letter for
            // `project-switch-to-buffer`.
            ("b", "project-buffers"),
            ("d", "project-dired"),
            ("g", "project-grep"),
            ("s", "project-shell"),
            // Magit's OWN command, not a wrapper — PC.3 made `:magit-status
            // <path>` satisfy the extension contract, so there is nothing for
            // this plugin to add, and the switch menu's `v` row already names
            // it directly. Safe as a chord because `lattice_magit::install`
            // runs unconditionally at boot; the row has a greyed-with-reason
            // fallback for a missing command and a chord has none, so this
            // binding is only correct while that stays true.
            ("v", "magit-status"),
        ];
        let mut keymap = Vec::with_capacity(verbs.len() * 2);
        for (suffix, command) in verbs {
            keymap.push(bind(&format!("<leader>p{suffix}"), command));
            keymap.push(bind(&format!("<C-x>p{suffix}"), command));
        }
        modes::register_mode(&ModeDeclaration {
            id: MODE_ID.to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Universal,
            capabilities: ModeCapabilities::empty(),
            keymap,
            // A minor claims no language.
            target_language: None,
            options: vec![],
        });
    }

    /// PC.6: `project.switch-commands`, a real `list<record>`.
    ///
    /// `register-structured-option`, not a string carrying TOML.
    /// `org-capture.md` §2's "no option can hold a record" was true when it was
    /// written and stopped being true at TC.4/TC.5; `:describe-option` shows a
    /// schema here rather than a blob.
    fn register_options() {
        let rows = switch::defaults();
        let _ = lattice::plugin_host::config::register_structured_option(
            switch::OPTION,
            &switch::schema(),
            &switch::to_value(&rows),
            "Rows of the project-switch menu. Each names an ex-command that \
             takes a project root as its first argument — which is the whole \
             contract for adding your own.",
        );
    }

    /// PC.8: this plugin's own `:help project` page.
    ///
    /// The markdown is `include_str!`'d from this plugin's `doc/`, so the manual
    /// travels with the plugin, is removed when the plugin is, and never enters
    /// lattice's own embedded-doc budget. An empty topic name registers at the
    /// bare plugin id, so the page answers to `:help project`.
    fn register_help_topics() {
        let _ = lattice::plugin_host::help::register_topic(
            "",
            "Choose a project, then the verb — find a file, grep, or open a \
             shell somewhere other than the buffer you are standing in.",
            include_str!("../doc/project.md"),
            &["project".to_string()],
        );
    }

    /// PC.5: declare the `projects` picker through the registry import — the
    /// OR.5b shape, where the host calls this once and the guest registers each
    /// source it provides.
    fn register_picker_sources() {
        lattice::plugin_host::picker_registry::register_picker_source(&picker::spec());
        // PB.1: the second source from this component — the OR.5b shape this
        // seam was built for.
        lattice::plugin_host::picker_registry::register_picker_source(&picker::buffers_spec());
    }

    /// Subscribe to `document-opened` — how a project comes to be remembered at
    /// all, and `project.el`'s `project-remember-project` in one line.
    ///
    /// Filtered to the one kind rather than taking everything and branching: the
    /// filter is the host's, so an unfiltered subscription would wake this
    /// plugin's task for every modal-mode change and every option write in the
    /// editor, to do nothing.
    fn register_events() {
        lattice::plugin_host::events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::DocumentOpened]),
                path_globs: None,
                major_modes: None,
            },
            ON_DOCUMENT_OPENED,
        );
    }

    /// Runs on the event actor's own task, never a keystroke — which is the
    /// property that lets it do a store read+write at all.
    ///
    /// Silent by construction: a handler that echoed would announce a project on
    /// every file you open. A store failure is dropped here rather than shown,
    /// because there is no user action that provoked it and nothing they could
    /// do about it mid-open; `:project-remember` is the path that reports.
    fn on_event(handler: u32, ev: Event) {
        if handler != ON_DOCUMENT_OPENED {
            return;
        }
        let Event::DocumentOpened(opened) = ev else {
            return;
        };
        // A buffer with no path on disk resolves to `pwd`, which
        // `project_of_buffer` already refuses — but checking here avoids a host
        // call per scratch buffer, and the field is right there.
        if opened.path.is_none() {
            return;
        }
        // `opened.id` is a `DocumentId` by TYPE and a buffer id by VALUE:
        // `publish_document_opened_for_active` builds it as
        // `DocumentId::new(buffer_id.0 as u64)`. `root-for-buffer` wants the
        // buffer id, so passing this straight through is correct — verified
        // rather than assumed, because the two type names disagree and a wrong
        // id here would resolve to `none` and silently remember nothing.
        if let Some(root) = project_of_buffer(opened.id) {
            let _ = remember_root(&root);
        }
    }

    /// No wakes are armed; the export exists because the world declares it.
    fn on_wake(_id: u32) {}
}

impl GrammarCallbacks for Component {
    fn parse_ex_args(_c: u32, rest: String, _bang: bool) -> Result<Args, String> {
        let rest = rest.trim();
        Ok(if rest.is_empty() {
            Args::None
        } else {
            Args::String(rest.to_string())
        })
    }

    fn apply_ex_command(
        c: u32,
        ctx: ExCommandContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Ok(match c {
            CB_REMEMBER => cmd_remember(&ctx),
            CB_FORGET => cmd_forget(&ctx),
            CB_SWITCH => cmd_switch(),
            CB_FIND_FILE => cmd_find_file(&ctx),
            CB_DIRED => cmd_dired(&ctx),
            CB_BUFFERS => cmd_buffers(&ctx),
            CB_SWITCH_TO => cmd_switch_to(&ctx),
            CB_GREP => cmd_grep(&ctx),
            CB_SHELL => cmd_shell(&ctx),
            CB_CHOOSE_DIR => cmd_choose_dir(),
            CB_REMEMBER_AND_SWITCH => cmd_remember_and_switch(&ctx),
            other => return Err(format!("project: unknown ex-command callback {other}")),
        })
    }

    fn apply_action(
        _c: u32,
        _ctx: lattice::plugin_host::types::ActionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Err("project: no actions".into())
    }
    fn apply_motion(
        _c: u32,
        _ctx: MotionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<MotionResult, String> {
        Err("project: no motions".into())
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("project: no operators".into())
    }
    fn apply_text_object(
        _c: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        Err("project: no text objects".into())
    }
}

impl PickerSource for Component {
    /// `source` is checked rather than assumed: one component may register
    /// several sources and they share one actor, so a source id this plugin
    /// never registered is untrusted input, not a case to fall through.
    fn init(
        source: String,
        ctx: PickerContext,
        _args: Vec<String>,
    ) -> Result<Vec<CandidatePair>, String> {
        let pairs = match source.as_str() {
            picker::PROJECTS_PICKER => picker::init(load())?,
            // PB.1: the root rides the CONTEXT, not the args — PC.1's rule,
            // and the same reason: `:project-buffers` opened from the
            // switch-commands menu names a project other than the one the
            // buffer is in, and `Effect::OpenPicker { root }` is the seam that
            // carries it. Reading `args[0]` would work for this source and
            // then be a second convention for the next one.
            picker::PROJECT_BUFFERS_PICKER => picker::buffers_init(
                &ctx.workspace_root,
                ctx.buffers,
                ctx.active_buffer.buffer_id,
            ),
            other => return Err(format!("project: no picker source `{other}`")),
        };
        Ok(pairs
            .into_iter()
            .map(|(candidate, routing)| CandidatePair { candidate, routing })
            .collect())
    }

    fn accept(
        source: String,
        _ctx: PickerContext,
        routing: RoutingPayload,
    ) -> Result<PickerAcceptOutcome, String> {
        match source.as_str() {
            picker::PROJECTS_PICKER => picker::accept(routing),
            picker::PROJECT_BUFFERS_PICKER => picker::buffers_accept(routing),
            other => Err(format!("project: no picker source `{other}`")),
        }
    }
}

impl TransientSource for Component {
    fn id() -> String {
        SWITCH_TRANSIENT.to_string()
    }

    /// One row per configured command, each carrying the chosen root.
    ///
    /// The root rides `ctx.args` (TR.3a) rather than guest memory, and that is
    /// the whole reason TR.3a exists: guest state is never cleared by `<Esc>`,
    /// so a remembered subject would leak into the next open — the menu would
    /// act on the project you looked at last rather than the one in front of
    /// you.
    fn build(ctx: TransientContext) -> Result<TransientSpec, String> {
        let Args::String(root) = &ctx.args else {
            return Err("project: the switch menu was opened without a project".to_string());
        };
        let root = root.trim();
        if root.is_empty() {
            return Err("project: the switch menu was opened without a project".to_string());
        }
        let mut items: Vec<TransientItem> = switch_commands()
            .into_iter()
            .map(|row| TransientItem {
                key: vec![row.key],
                label: row.label,
                description: String::new(),
                kind: TransientItemKind::Action(TransientAction {
                    command: row.command,
                    args: Args::String(root.to_string()),
                }),
            })
            .collect();
        // A menu with no way out is a trap.
        items.push(TransientItem {
            key: vec!["q".to_string()],
            label: "quit".to_string(),
            description: String::new(),
            kind: TransientItemKind::Dismiss,
        });
        Ok(TransientSpec {
            // The project is NAMED in the title. The whole point of this menu
            // is that you are acting on somewhere you are not standing, so a
            // title that did not say which project would be the one piece of
            // information the user most needs.
            title: format!("Project: {}", projects::basename(root)),
            groups: vec![TransientGroup {
                label: String::new(),
                items,
            }],
            footer: Some(root.to_string()),
        })
    }
}

export!(Component);
