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
    TransientAction, TransientContext, TransientGroup, TransientItem, TransientItemKind,
    TransientSpec,
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
            })]
        }
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
        let verbs = [
            ("p", "project-switch"),
            ("f", "project-find-file"),
            ("d", "project-dired"),
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

    /// PC.5: declare the `projects` picker through the registry import — the
    /// OR.5b shape, where the host calls this once and the guest registers each
    /// source it provides.
    fn register_picker_sources() {
        lattice::plugin_host::picker_registry::register_picker_source(&picker::spec());
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
            CB_SWITCH_TO => cmd_switch_to(&ctx),
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
        _ctx: PickerContext,
        _args: Vec<String>,
    ) -> Result<Vec<CandidatePair>, String> {
        if source != picker::PROJECTS_PICKER {
            return Err(format!("project: no picker source `{source}`"));
        }
        Ok(picker::init(load())?
            .into_iter()
            .map(|(candidate, routing)| CandidatePair { candidate, routing })
            .collect())
    }

    fn accept(
        source: String,
        _ctx: PickerContext,
        routing: RoutingPayload,
    ) -> Result<PickerAcceptOutcome, String> {
        if source != picker::PROJECTS_PICKER {
            return Err(format!("project: no picker source `{source}`"));
        }
        picker::accept(routing)
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
