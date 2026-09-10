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

mod projects;

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
        Ok(()) => match save(&list) {
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
        // An explicit path: resolve it so `:project-remember .` or a path to a
        // file inside the tree both name the project rather than the argument.
        Some(path) => match project_of_path(&path) {
            Some(root) => root,
            None => {
                return warn(format!(
                    "project: `{path}` is not inside a project — no root marker above it"
                ));
            }
        },
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
            completion: None,
            picker: None,
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

export!(Component);
