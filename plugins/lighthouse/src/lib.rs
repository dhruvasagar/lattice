//! `lighthouse` — the language-server manager (LH.1).
//!
//! Design: `docs/dev/architecture/lighthouse.md`. Slice plan:
//! `docs/dev/operations/slice-plans/lighthouse.md`.
//!
//! `:lsp-install <server>` fetches a server the registry knows, checks it
//! against a pinned SHA-256, unpacks it into the plugin's own data directory,
//! registers it with the editor, and reports every step into
//! `*lsp-install:<server>*` as it happens. `:lsp-update` moves an installed
//! server to the registry's current pin; `:lsp-uninstall` removes one.
//!
//! ## Two instances, and which one does the work
//!
//! The host instantiates this component once per seam. The **grammar**
//! instance runs the ex-commands, on the keystroke path; the **events**
//! instance runs `on-event`, on its own task. They share a store and a data
//! directory and nothing else — not memory.
//!
//! All the work happens on the events instance. The command validates its
//! argument, publishes a [`REQUEST_EVENT`], and returns the effect that opens
//! the buffer; the events instance hears the request and runs the install.
//! Three things fall out of that:
//!
//! * The command returns at once, whatever the network is doing.
//! * A job is started and stepped by the same instance, so the in-flight
//!   table ([`install::Installer`]) is plain memory rather than a store both
//!   sides would have to race to update.
//! * A job's `job-finished` is queued behind the `on-event` call that started
//!   it, so the id is always recorded before its outcome can arrive.
//! * The file work (rename, remove, list) is this component's own filesystem
//!   calls, which only an async seam can make. The grammar instance touches
//!   no file except through `host-services`.
//!
//! ## Where the logic is
//!
//! Not here. [`registry`] reads and validates the server list; [`install`] is
//! the state machine, written against a [`install::Host`] trait and tested
//! with a fake. This file is the adapter between that trait and the editor.

wit_bindgen::generate!({
    world: "lighthouse-plugin",
    path: "../../crates/lattice-wit/wit",
});

use std::sync::Mutex;

use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::events::EventFilter;
use lattice::plugin_host::help;
use lattice::plugin_host::host_services::{self, ArchiveFormat, OutputState, ServerConfig};
use lattice::plugin_host::modes::{
    self, ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding,
    ModeKind,
};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, ArgDefault, ArgKind, ArgSpec, Args, EchoLevel, EchoPayload, Effect,
    EventKind, ExCommandContext, ExCommandSpec, LatencyClass, MotionContext, MotionResult,
    OpenSyntheticBufferPayload, OperatorContext, Range, SurfaceForm, TextObjectContext,
};
// `Event` is already in scope from the world's own `use types.{event}`.

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;

mod install;
mod list;
mod registry;

use install::{Installed, Installer, Phase};
use registry::{Archive, Registry, Server};

// Ex-command callback ids.
const CB_PARSE: u32 = 0;
const CB_INSTALL: u32 = 1;
const CB_UNINSTALL: u32 = 2;
const CB_UPDATE: u32 = 3;
const CB_UPDATE_ALL: u32 = 4;
const CB_SERVERS: u32 = 5;

// Action callback ids — the chords of `*lsp-servers*`. A third namespace.
const ROW_INSTALL: u32 = 1;
const ROW_UPDATE: u32 = 2;
const ROW_UNINSTALL: u32 = 3;
const ROW_LOG: u32 = 4;
const ROW_REFRESH: u32 = 5;

/// The minor mode that owns `*lsp-servers*`'s chords.
const SERVERS_MODE: &str = "lighthouse-servers-mode";

/// The row chords: `(chord, action, callback, doc)`. One table, read by both
/// the action registration and the mode's keymap, so a chord cannot be bound
/// to an action that was never registered.
const ROW_KEYS: [(&str, &str, u32, &str); 5] = [
    (
        "i",
        "lsp-servers-install",
        ROW_INSTALL,
        "Install the language server on the cursor's row of `*lsp-servers*` \
         (or reinstall it). The row updates as the install proceeds.",
    ),
    (
        "u",
        "lsp-servers-update",
        ROW_UPDATE,
        "Update the language server on the cursor's row of `*lsp-servers*` to \
         the version the registry pins, if it is not already there.",
    ),
    (
        "x",
        "lsp-servers-uninstall",
        ROW_UNINSTALL,
        "Uninstall the language server on the cursor's row of `*lsp-servers*`.",
    ),
    (
        "<CR>",
        "lsp-servers-log",
        ROW_LOG,
        "Open the install log, `*lsp-install:<server>*`, of the language \
         server on the cursor's row of `*lsp-servers*`.",
    ),
    (
        "gr",
        "lsp-servers-refresh",
        ROW_REFRESH,
        "Redraw `*lsp-servers*` from the registry and what is installed.",
    ),
];

// Event-handler ids — a different namespace from the callbacks above.
const ON_REQUEST: u32 = 1;
const ON_JOB: u32 = 2;

/// The plugin-defined event a command publishes to ask the events instance
/// for work. The payload is `<verb> <server>` as UTF-8.
const REQUEST_EVENT: &str = "lighthouse.request";

/// The host's generic mode for a plugin's output buffer.
const OUTPUT_MODE: &str = "plugin-output-mode";

/// Where the guest sees its data directory.
const GUEST_DATA: &str = "/data";

/// The user's own registry entries, laid over the bundled ones — a file of
/// this name in the data directory.
const OVERLAY: &str = "registry.toml";

/// The installs in flight. Lives in the EVENTS instance; the grammar instance
/// has its own copy of this static and never touches it.
static INSTALLER: Mutex<Installer> = Mutex::new(Installer::new());

struct Component;

/// The bundled registry with the user's overlay over it.
///
/// Read through the host's `read-file`, by host path — NOT through this
/// component's own filesystem. A command calls this, and a command runs on
/// the synchronous dispatch path, where a guest's WASI calls cannot be
/// driven. (The events instance could use either; one way of reading it is
/// one fewer thing to get out of step.)
fn registry() -> (Registry, Option<String>) {
    let overlay = host_services::data_dir()
        .and_then(|dir| host_services::read_file(&format!("{dir}/{OVERLAY}")).ok());
    Registry::load(overlay.as_deref())
}

fn platform() -> String {
    let p = host_services::host_platform();
    registry::platform_key(&p.os, &p.arch)
}

fn echo(level: EchoLevel, text: String) -> Vec<Effect> {
    vec![Effect::Echo(EchoPayload { level, text })]
}

/// [`install::Host`], against the real editor.
///
/// Every file has two names — its path on the host, which the host-side
/// seams take, and its path under `/data`, which this component's own
/// filesystem calls take. The state machine deals in the part they share.
struct Edge {
    /// The data directory as the host knows it.
    host_data: String,
}

impl Edge {
    fn open() -> Option<Self> {
        host_services::data_dir().map(|host_data| Self { host_data })
    }

    fn on_host(&self, path: &str) -> String {
        format!("{}/{path}", self.host_data)
    }

    fn in_guest(path: &str) -> String {
        format!("{GUEST_DATA}/{path}")
    }
}

impl install::Host for Edge {
    fn download(&mut self, url: &str, sha256: &str, dest: &str) -> Result<u64, String> {
        host_services::http_download(url, sha256, &self.on_host(dest))
    }

    fn extract(&mut self, src: &str, dest: &str, archive: Archive) -> Result<u64, String> {
        let format = match archive {
            Archive::Gz => ArchiveFormat::Gz,
            Archive::TarGz => ArchiveFormat::TarGz,
        };
        host_services::extract_archive(&self.on_host(src), &self.on_host(dest), format)
    }

    fn set_executable(&mut self, path: &str) -> Result<(), String> {
        host_services::set_executable(&self.on_host(path))
    }

    fn remove_file(&mut self, path: &str) {
        let _ = std::fs::remove_file(Self::in_guest(path));
    }

    fn remove_tree(&mut self, path: &str) {
        let _ = std::fs::remove_dir_all(Self::in_guest(path));
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        std::fs::rename(Self::in_guest(from), Self::in_guest(to))
            .map_err(|e| format!("could not move '{from}' to '{to}': {e}"))
    }

    fn list(&self, dir: &str) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(Self::in_guest(dir)) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect()
    }

    fn exists(&self, path: &str) -> bool {
        std::path::Path::new(&Self::in_guest(path)).exists()
    }

    fn put(&mut self, key: &str, value: &str) -> Result<(), String> {
        host_services::store_put(key, value.as_bytes())
    }

    fn delete(&mut self, key: &str) {
        let _ = host_services::store_delete(key);
    }

    fn keys(&self, prefix: &str) -> Vec<String> {
        host_services::store_keys(prefix)
    }

    fn register(&mut self, server: &Server, binary: &str) -> Result<u64, String> {
        host_services::register_server(&ServerConfig {
            id: server.lsp_id.clone(),
            // The editor runs this, so it is the HOST's name for the file.
            command: self.on_host(binary),
            args: server.args.clone(),
            env: Vec::new(),
            root_markers: server.root_markers.clone(),
            file_patterns: server.file_patterns.clone(),
            language_id: server.language_id.clone(),
            initialization_options: None,
        })
    }

    fn unregister(&mut self, token: u64) {
        host_services::unregister_server(token);
    }

    fn get(&self, key: &str) -> Option<String> {
        host_services::store_get(key).and_then(|bytes| String::from_utf8(bytes).ok())
    }

    // The output calls can only fail on a malformed buffer name or a host
    // with no output store, and there is nowhere better to report either.
    fn say(&mut self, buffer: &str, line: &str) {
        let _ = host_services::output_append(buffer, &[line.to_string()]);
    }

    fn status(&mut self, buffer: &str, phase: Phase, text: &str) {
        let state = match phase {
            Phase::Running => OutputState::Running,
            Phase::Succeeded => OutputState::Succeeded,
            Phase::Failed => OutputState::Failed,
        };
        let _ = host_services::output_status(buffer, state, text);
    }

    fn reset(&mut self, buffer: &str) {
        let _ = host_services::output_reset(buffer);
    }
}

// ── The command side (grammar instance) ─────────────────────────────────────

/// The single server-name argument.
fn arg_server(args: &Args) -> Option<String> {
    match args {
        Args::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    }
}

/// `:lsp-install <server>`. Validates, asks the events instance to do the
/// work, and opens the buffer it will report into.
fn cmd_install(ctx: &ExCommandContext) -> Vec<Effect> {
    let (registry, problem) = registry();
    let available = registry.names().join(", ");
    let Some(name) = arg_server(&ctx.args) else {
        return echo(
            EchoLevel::Warn,
            format!("lsp-install: which server? (available: {available})"),
        );
    };
    if registry.get(&name).is_none() {
        // A broken overlay is the likeliest reason a server someone just
        // added is "unknown", so say that first if it is the case.
        let why = problem.map(|p| format!(" — {p}")).unwrap_or_default();
        return echo(
            EchoLevel::Warn,
            format!("lsp-install: no server named '{name}' (available: {available}){why}"),
        );
    }
    ask("install", &name)
}

/// Publish a request for the events instance, and open the buffer it will
/// report into.
fn ask(verb: &str, name: &str) -> Vec<Effect> {
    host_services::emit_event(REQUEST_EVENT, format!("{verb} {name}").as_bytes());
    vec![Effect::OpenSyntheticBuffer(OpenSyntheticBufferPayload {
        name: install::buffer_name(name),
        mode_id: OUTPUT_MODE.to_string(),
        content: None,
        cursor: None,
        activate_minor: None,
    })]
}

/// What the store says is installed, read through `host-services` — the one
/// way the command side may read anything.
fn installed_record(name: &str) -> Option<Installed> {
    host_services::store_get(&format!("{}{name}", install::INSTALLED_PREFIX))
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .and_then(|text| Installed::decode(&text))
}

/// Every installed server's name.
fn installed_names() -> Vec<String> {
    host_services::store_keys(install::INSTALLED_PREFIX)
        .iter()
        .filter_map(|key| key.strip_prefix(install::INSTALLED_PREFIX))
        .map(str::to_string)
        .collect()
}

/// `:lsp-uninstall <server>`.
fn cmd_uninstall(ctx: &ExCommandContext) -> Vec<Effect> {
    let Some(name) = arg_server(&ctx.args) else {
        let installed = installed_names();
        let which = if installed.is_empty() {
            "nothing is installed".to_string()
        } else {
            format!("installed: {}", installed.join(", "))
        };
        return echo(
            EchoLevel::Warn,
            format!("lsp-uninstall: which server? ({which})"),
        );
    };
    if installed_record(&name).is_none() {
        return echo(
            EchoLevel::Warn,
            format!("lsp-uninstall: '{name}' is not installed"),
        );
    }
    ask("uninstall", &name)
}

/// Whether `name` is installed at something other than the registry's pin.
/// `Err` is the message for when there is nothing to do.
fn needs_update(registry: &Registry, name: &str) -> Result<(), String> {
    let Some(record) = installed_record(name) else {
        return Err(format!(
            "'{name}' is not installed — :lsp-install {name} installs it"
        ));
    };
    let Some(server) = registry.get(name) else {
        return Err(format!(
            "'{name}' is installed but no longer in the registry, so there is \
             nothing to update it to"
        ));
    };
    if server.version == record.version {
        return Err(format!("{name} {} is up to date", record.version));
    }
    Ok(())
}

/// `:lsp-update <server>`. An update is an install of the registry's pin: the
/// new version is fetched and verified beside the old one, the editor is
/// moved onto it, and only then is the old one removed.
fn cmd_update(ctx: &ExCommandContext) -> Vec<Effect> {
    let Some(name) = arg_server(&ctx.args) else {
        return echo(
            EchoLevel::Warn,
            "lsp-update: which server? (:lsp-update-all updates every one)".to_string(),
        );
    };
    let (registry, _) = registry();
    match needs_update(&registry, &name) {
        Ok(()) => ask("install", &name),
        Err(nothing_to_do) => echo(EchoLevel::Info, format!("lsp-update: {nothing_to_do}")),
    }
}

/// `:lsp-update-all`. Several installs at once have no single buffer to
/// open, so this one reports in the echo area and each server reports in its
/// own buffer.
fn cmd_update_all() -> Vec<Effect> {
    let (registry, _) = registry();
    let stale: Vec<String> = installed_names()
        .into_iter()
        .filter(|name| needs_update(&registry, name).is_ok())
        .collect();
    if stale.is_empty() {
        return echo(
            EchoLevel::Info,
            "lsp-update-all: everything installed is up to date".to_string(),
        );
    }
    for name in &stale {
        host_services::emit_event(REQUEST_EVENT, format!("install {name}").as_bytes());
    }
    echo(
        EchoLevel::Info,
        format!(
            "lsp-update-all: updating {} — progress is in each *lsp-install:<server>* buffer",
            stale.join(", ")
        ),
    )
}

/// `:lsp-servers`. The list is drawn by the events instance — it is the one
/// that knows what is in flight — so this asks for a redraw and opens the
/// buffer, with the mode that owns its chords riding the output mode.
fn cmd_servers() -> Vec<Effect> {
    host_services::emit_event(REQUEST_EVENT, b"list -");
    vec![Effect::OpenSyntheticBuffer(OpenSyntheticBufferPayload {
        name: list::BUFFER.to_string(),
        mode_id: OUTPUT_MODE.to_string(),
        content: None,
        cursor: None,
        activate_minor: Some(SERVERS_MODE.to_string()),
    })]
}

/// A chord in `*lsp-servers*`. Unlike the ex-commands these stay in the
/// list: the row itself is where the result shows, and jumping to the log on
/// every keypress would take the user out of the view they are working in.
fn row_action(action: u32, ctx: &ActionContext, doc: &Document) -> Vec<Effect> {
    if action == ROW_REFRESH {
        host_services::emit_event(REQUEST_EVENT, b"list -");
        return Vec::new();
    }
    let line = doc.line(ctx.cursor.line).unwrap_or_default();
    let Some(name) = list::server_on_line(&line) else {
        return echo(
            EchoLevel::Info,
            "lsp-servers: no server on this line".to_string(),
        );
    };
    let (registry, _) = registry();
    let request = |verb: &str| {
        host_services::emit_event(REQUEST_EVENT, format!("{verb} {name}").as_bytes());
    };
    match action {
        ROW_INSTALL => {
            if registry.get(name).is_none() {
                return echo(
                    EchoLevel::Warn,
                    format!(
                        "lsp-servers: '{name}' is not in the registry, so it cannot be installed"
                    ),
                );
            }
            request("install");
            echo(
                EchoLevel::Info,
                format!("lsp-servers: installing {name} \u{2014} <CR> shows its log"),
            )
        }
        ROW_UPDATE => match needs_update(&registry, name) {
            Ok(()) => {
                request("install");
                echo(
                    EchoLevel::Info,
                    format!("lsp-servers: updating {name} \u{2014} <CR> shows its log"),
                )
            }
            Err(nothing_to_do) => echo(EchoLevel::Info, format!("lsp-servers: {nothing_to_do}")),
        },
        ROW_UNINSTALL => {
            if installed_record(name).is_none() {
                return echo(
                    EchoLevel::Info,
                    format!("lsp-servers: '{name}' is not installed"),
                );
            }
            request("uninstall");
            echo(EchoLevel::Info, format!("lsp-servers: uninstalling {name}"))
        }
        ROW_LOG => vec![Effect::OpenSyntheticBuffer(OpenSyntheticBufferPayload {
            name: install::buffer_name(name),
            mode_id: OUTPUT_MODE.to_string(),
            content: None,
            cursor: None,
            activate_minor: None,
        })],
        _ => Vec::new(),
    }
}

fn no_arg_spec() -> ExCommandSpec {
    ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    }
}

fn server_arg_spec(prompt: &str) -> ExCommandSpec {
    ExCommandSpec {
        // The command itself only publishes a request; the download is a host
        // job reported through events.
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        args_schema: vec![ArgSpec {
            name: "server".to_string(),
            kind: ArgKind::String,
            doc: "a server from the registry, e.g. `rust-analyzer`".to_string(),
            prompt: prompt.to_string(),
            default: ArgDefault::None,
            completion: None,
            picker: None,
        }],
        surface_form: SurfaceForm::Keyword,
    }
}

// ── The work side (events instance) ─────────────────────────────────────────

/// A request published by a command: `<verb> <server>`.
fn on_request(payload: &[u8]) {
    let Ok(text) = std::str::from_utf8(payload) else {
        return;
    };
    let Some((verb, name)) = text.split_once(' ') else {
        return;
    };
    if !matches!(verb, "install" | "uninstall" | "list") {
        return;
    }
    let buffer = install::buffer_name(name);
    let (registry, problem) = registry();
    let Some(mut edge) = Edge::open() else {
        let _ = host_services::output_append(
            &buffer,
            &["error: this plugin has no data directory to install into".to_string()],
        );
        let _ = host_services::output_status(
            &buffer,
            OutputState::Failed,
            &format!("{name}: install failed"),
        );
        return;
    };
    let mut installer = INSTALLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let platform = platform();
    match verb {
        "uninstall" => installer.uninstall(&mut edge, &registry, name),
        // The command checked the name; it is checked again because this is
        // a bus event and anything may have published it.
        "install" => {
            if let Some(server) = registry.get(name) {
                installer.request(&mut edge, server, &platform);
            }
        }
        // "list": nothing to do but the redraw below.
        _ => {}
    }
    // Whatever just happened, the list now says something else.
    list::show(
        &mut edge,
        &registry,
        problem.as_deref(),
        &platform,
        &installer,
    );
}

fn on_job(ev: &Event) {
    let Some(mut edge) = Edge::open() else {
        return;
    };
    let mut installer = INSTALLER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match ev {
        Event::JobProgress(p) => installer.progress(&mut edge, p.id, p.done, p.total),
        Event::JobFinished(f) => {
            let (registry, problem) = registry();
            installer.finished(&mut edge, &registry, f.id, f.outcome.clone());
            // A job ending is a row changing: "installing…" becomes
            // "installed", or goes back to what it was.
            list::show(
                &mut edge,
                &registry,
                problem.as_deref(),
                &platform(),
                &installer,
            );
        }
        _ => {}
    }
}

fn kinds(kinds: &[EventKind]) -> EventFilter {
    EventFilter {
        kinds: Some(kinds.to_vec()),
        path_globs: None,
        major_modes: None,
        minor_modes: None,
    }
}

impl Guest for Component {
    fn register_grammar() {
        lattice::plugin_host::grammar::register_ex_command(
            "lsp-install",
            "Install a language server from lighthouse's registry into the \
             editor's own managed directory — no `PATH` entry needed. Returns \
             at once; the download, its SHA-256 check and the unpack are \
             reported live in `*lsp-install:<server>*`, and so is any failure. \
             Running it again for an installed server reinstalls it.",
            &server_arg_spec("Install server: "),
            CB_PARSE,
            CB_INSTALL,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "lsp-uninstall",
            "Remove a language server lighthouse installed: its files, and its \
             registration with the editor, so the language goes back to \
             whatever server is on `PATH`. A server that is already running \
             keeps running until the editor restarts.",
            &server_arg_spec("Uninstall server: "),
            CB_PARSE,
            CB_UNINSTALL,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "lsp-update",
            "Update an installed language server to the version lighthouse's \
             registry pins. The new version is downloaded and verified beside \
             the old one, which stays in use until the new one is ready and \
             is removed only after. Says so if the server is already current.",
            &server_arg_spec("Update server: "),
            CB_PARSE,
            CB_UPDATE,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "lsp-update-all",
            "Update every installed language server whose version differs \
             from the one lighthouse's registry pins. Each reports in its own \
             `*lsp-install:<server>*` buffer.",
            &no_arg_spec(),
            CB_PARSE,
            CB_UPDATE_ALL,
        );
        lattice::plugin_host::grammar::register_ex_command(
            "lsp-servers",
            "List every language server lighthouse can install, with its \
             version and whether it is installed, in `*lsp-servers*`. The list \
             redraws by itself as installs proceed. On a server's row: `i` \
             installs it, `u` updates it, `x` uninstalls it, `<CR>` opens its \
             install log, `gr` redraws.",
            &no_arg_spec(),
            CB_PARSE,
            CB_SERVERS,
        );
        for (_chord, action, callback, doc) in ROW_KEYS {
            lattice::plugin_host::grammar::register_action(
                action,
                doc,
                &ActionSpec {
                    args_schema: Vec::new(),
                },
                callback,
            );
        }
    }

    /// `lighthouse-servers-mode`: the chords of `*lsp-servers*`, in the
    /// mode's own keymap layer. Manual — it is activated on that one buffer,
    /// by the effect that opens it, and nowhere else.
    fn register_modes() {
        modes::register_mode(&ModeDeclaration {
            id: SERVERS_MODE.to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Manual,
            capabilities: ModeCapabilities::empty(),
            keymap: ROW_KEYS
                .iter()
                .map(|(chord, action, _, _)| ModeKeymapBinding {
                    binding_mode: BindingMode::Normal,
                    chord: (*chord).to_string(),
                    command: (*action).to_string(),
                })
                .collect(),
            target_language: None,
            options: vec![],
        });
    }

    fn register_help_topics() {
        let _ = help::register_topic(
            "",
            "Install, update and remove language servers from inside the editor \
             — `:lsp-install`, `:lsp-servers`.",
            include_str!("../doc/lighthouse.md"),
            &["lsp-install".to_string(), "lsp-servers".to_string()],
        );
    }

    fn register_events() {
        host_services::register_event(
            REQUEST_EVENT,
            "A lighthouse command asking its events instance to install, update \
             or remove a language server. Payload: `<verb> <server>`, UTF-8.",
        );
        lattice::plugin_host::events::subscribe(&kinds(&[EventKind::Plugin]), ON_REQUEST);
        lattice::plugin_host::events::subscribe(
            &kinds(&[EventKind::JobProgress, EventKind::JobFinished]),
            ON_JOB,
        );
        // Nothing can be in flight yet, so any scratch file is from an
        // install the last editor session did not finish.
        if let Some(mut edge) = Edge::open() {
            install::sweep(&mut edge);
            // And nothing is registered yet: a registration lasts as long as
            // the instance that made it. This is what makes an install
            // outlive the session it was made in.
            let (registry, _) = registry();
            INSTALLER
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reconcile(&mut edge, &registry);
        }
    }

    fn on_event(handler: u32, ev: Event) {
        match handler {
            ON_REQUEST => {
                // Every plugin-defined event shares one kind; the name is
                // matched here.
                if let Event::Plugin(p) = &ev {
                    if p.name == REQUEST_EVENT {
                        on_request(&p.payload);
                    }
                }
            }
            ON_JOB => on_job(&ev),
            _ => {}
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
            CB_INSTALL => cmd_install(&ctx),
            CB_UNINSTALL => cmd_uninstall(&ctx),
            CB_UPDATE => cmd_update(&ctx),
            CB_UPDATE_ALL => cmd_update_all(),
            CB_SERVERS => cmd_servers(),
            other => return Err(format!("lighthouse: unknown ex-command callback {other}")),
        })
    }

    fn apply_action(
        c: u32,
        ctx: ActionContext,
        doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        if ROW_KEYS.iter().any(|(_, _, callback, _)| *callback == c) {
            Ok(row_action(c, &ctx, doc))
        } else {
            Err(format!("lighthouse: unknown action callback {c}"))
        }
    }
    fn apply_motion(
        _c: u32,
        _ctx: MotionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<MotionResult, String> {
        Err("lighthouse: no motions".into())
    }
    fn apply_operator(
        _c: u32,
        _ctx: OperatorContext,
        _doc: &Document,
    ) -> Result<Vec<Effect>, String> {
        Err("lighthouse: no operators".into())
    }
    fn apply_text_object(
        _c: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        Err("lighthouse: no text objects".into())
    }
}

export!(Component);
