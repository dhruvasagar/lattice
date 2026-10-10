//! PH7.8c event fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `events-plugin` world,
//! driving the event-delivery actor (`event_task.rs`) through a real host→guest
//! `on-event` call:
//!   - `register-events` (the world export the host calls once) subscribes three
//!     handlers via the imported `events.subscribe`: handler 1 → `DocumentSaved`,
//!     handler 2 → `BeforeQuit`, handler 3 → `ModalModeChanged`.
//!   - `on-event(handler, ev)` appends `"<handler>:<kind>\n"` to
//!     `/data/received.log` (the writable data-dir mount, PH7.2) so the host test
//!     can observe that delivery reached the guest end to end.
//!   - Handler 3 is a **poison** handler: it traps (`unreachable!`) instead of
//!     writing, exercising graceful degradation — the host logs + skips the
//!     delivery and never crashes, and every other subscriber (a native bus
//!     channel in the test) is untouched (§8 isolation). A trap taints the
//!     instance, so this plugin's later deliveries also fail; that is fine (it is
//!     dead until re-instantiation, PH7.12).

wit_bindgen::generate!({
    world: "events-plugin",
    path: "../../../../lattice-wit/wit",
});

use lattice::plugin_host::events;
use lattice::plugin_host::host_services;
use lattice::plugin_host::types::{EventFilter, EventKind};
use lattice_plugin_sdk::PluginEvent;
use serde::{Deserialize, Serialize};

/// The plugin-defined event this fixture declares (`register-event`) and emits
/// (`emit-event`) when it observes a save. Authored via the PH7.8b.3 SDK derive:
/// `NAME` / `DOC` come from the derive (the `///` doc-comment IS the doc), and
/// `encode()` gives a type-safe MessagePack payload over the opaque wire. A
/// shared-crate copy of this exact type is the cross-plugin contract the host-side
/// consumer decodes in the e2e test.
#[derive(Serialize, Deserialize, PluginEvent)]
#[event(name = "events-fixture.saved-echo")]
struct SavedEcho {
    /// The path that was saved (echoed back on the bus).
    path: String,
}
// `Event` is world-`use`d, so wit-bindgen surfaces it at the crate root (in
// scope here without an import — importing it from `types` would collide).

struct Component;

/// Append one line to the log the host test reads. Factored out at OR.2, when a
/// second and third writer appeared; the behaviour is unchanged.
fn record(line: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/data/received.log")
    {
        let _ = f.write_all(format!("{line}\n").as_bytes());
    }
}

/// OC.2 wake state. A component is single-threaded, but `RefCell` says so
/// without `unsafe` and costs nothing at this call rate.
mod wake_state {
    use std::cell::Cell;

    thread_local! {
        /// The periodic wake armed from `register-events` — the "org's clock
        /// re-renders once a minute" shape.
        pub static TICKER: Cell<u32> = const { Cell::new(0) };
        /// How many times it has fired. The guest cancels itself at
        /// [`CANCEL_AFTER`] so a test can prove `cancel-wake` actually stops the
        /// timer rather than merely being callable.
        pub static FIRES: Cell<u32> = const { Cell::new(0) };
        /// A wake armed from *inside* `on-event` (org clocks in from a chord's
        /// event, not from registration) whose `on-wake` traps — the
        /// quarantine-without-wedging arm.
        pub static POISON: Cell<u32> = const { Cell::new(0) };
    }

    /// Fires after which the ticker cancels itself.
    pub const CANCEL_AFTER: u32 = 3;
}

/// A one-kind declarative filter (the common `:autocmd <kind>` shape).
fn kind_filter(kind: EventKind) -> EventFilter {
    EventFilter {
        kinds: Some(vec![kind]),
        path_globs: None,
        major_modes: None,
        minor_modes: None,
    }
}

/// A stable label per event kind — what the guest records so the host test can
/// assert which events were delivered.
fn label(ev: &Event) -> &'static str {
    match ev {
        Event::DocumentOpened(_) => "opened",
        Event::DocumentClosed(_) => "closed",
        Event::BeforeSave(_) => "before-save",
        Event::DocumentSaved(_) => "saved",
        Event::DocumentChanged(_) => "changed",
        Event::SelectionsChanged(_) => "selections",
        Event::ModalModeChanged(_) => "modal",
        Event::BeforeQuit => "quit",
        Event::OptionChanged(_) => "option",
        Event::MajorEntered(_) => "major-entered",
        Event::MajorExiting(_) => "major-exiting",
        Event::MinorActivated(_) => "minor-activated",
        Event::MinorDeactivated(_) => "minor-deactivated",
        Event::Plugin(_) => "plugin",
        Event::PrePluginLoaded(_) => "pre-plugin-loaded",
        Event::PluginLoaded(_) => "plugin-loaded",
        Event::PluginUnloaded(_) => "plugin-unloaded",
        Event::FilesChanged(_) => "files-changed",
        Event::JobProgress(_) => "job-progress",
        Event::JobOutput(_) => "job-output",
        Event::JobFinished(_) => "job-finished",
    }
}

/// OR.2. The host path the guest watches, handed in through the data-dir mount
/// because a guest cannot know where its own `/data` lives on the host — and,
/// more to the point, because a real plugin learns its corpus root from
/// configuration too (`org.roam-directory`). Absent → no watch is armed, which
/// is what every pre-OR.2 test gets.
const WATCH_TARGET: &str = "/data/watch-target";

/// LH.0.1. Present ⇒ download from `register-events`. Three lines: the URL,
/// the expected SHA-256, and the host path to write to — handed in the same
/// way `WATCH_TARGET` is, and for its reason.
const DOWNLOAD_REQUEST: &str = "/data/download-request";

/// LH.0.2. Present ⇒ unpack from `register-events`. Three lines: the archive,
/// the destination, and `gz` or `tar-gz`.
const EXTRACT_REQUEST: &str = "/data/extract-request";

/// LH.0.3. Present ⇒ run a process from `register-events`. First line the
/// program, each further line one argument.
const SPAWN_REQUEST: &str = "/data/spawn-request";

/// LH.0.4. Present ⇒ register a language server from `register-events`. Three
/// lines: the server id, the command, and one file pattern.
const SERVER_REQUEST: &str = "/data/server-request";

/// LH.0.4. Present as well ⇒ withdraw that registration straight away.
const SERVER_WITHDRAW: &str = "/data/server-withdraw";

/// PH7.8c: present ⇒ ring our own doorbell from `register-events`. A marker
/// file rather than an unconditional emit, so only the test that is about this
/// behaviour pays for it.
const EMIT_AT_REGISTER: &str = "/data/emit-at-register";

impl Guest for Component {
    /// The host calls this once; the guest subscribes through the imported
    /// `events.subscribe` host function.
    fn register_events() {
        events::subscribe(&kind_filter(EventKind::DocumentSaved), 1);
        events::subscribe(&kind_filter(EventKind::BeforeQuit), 2);
        // Poison handler: traps on delivery (graceful-skip exercise).
        events::subscribe(&kind_filter(EventKind::ModalModeChanged), 3);
        // No-op handler: returns immediately (no fs) — the clean per-delivery
        // dispatch path the perf ratchet (PH7.8d) measures.
        events::subscribe(&kind_filter(EventKind::DocumentChanged), 4);
        // OC.2: arms the poison wake when it fires (see `on_event`). A separate
        // kind so the existing delivery assertions are untouched.
        events::subscribe(&kind_filter(EventKind::DocumentOpened), 5);
        // @example host-services.register-event: Declare a plugin-defined event, taking its name and doc from the SDK's `PluginEvent` derive
        // PH7.8b.2/3: declare a plugin-defined event via the `register-event`
        // host-service, using the SDK-derived `NAME` + `DOC` (the doc-comment).
        // It self-registers into the host's runtime event registry under this
        // plugin's provenance; `on-event` handler 1 emits it on save.
        host_services::register_event(SavedEcho::NAME, SavedEcho::DOC);
        // @end-example
        // PH7.8c: ring our OWN doorbell from inside `register-events`.
        //
        // The shape a guest reaches for when registration has to kick off its
        // own work — org's roam index queues a corpus walk and emits the first
        // batch step exactly here. The subscription above is recorded but not
        // yet on the bus, so without the host holding this it is published to
        // everyone except us and the chain never starts. Handler 7 records the
        // delivery, so a test can tell "arrived" from "dropped".
        //
        // Opt-in via a marker file, the `WATCH_TARGET` idiom two arms down: an
        // unconditional emit here would add a line to every other test's
        // expected log, coupling all of them to this one behaviour.
        if std::fs::metadata(EMIT_AT_REGISTER).is_ok() {
            events::subscribe(&kind_filter(EventKind::Plugin), 7);
            host_services::emit_event("fixture/registered", b"1");
        }
        // @example events.wake-every: Arm a periodic wake at registration and keep its id for `cancel-wake`
        // OC.2: arm a periodic wake from registration. 50 ms is the seam's
        // floor — fast enough that a test does not sit on a real clock, and the
        // guest cancels itself after a few fires so it cannot run away.
        wake_state::TICKER.with(|t| t.set(events::wake_every(50)));
        // @end-example
        // OR.2: arm a directory watch, if the test handed us one. Handler 6
        // records each batch — the point being that it records it with NO
        // action dispatched afterwards, which is the failure mode this seam is
        // most likely to have.
        if let Ok(target) = std::fs::read_to_string(WATCH_TARGET) {
            let target = target.trim();
            // @example host-services.watch: Subscribe to `files-changed`, then watch a directory and record whether the grant allowed it
            events::subscribe(&kind_filter(EventKind::FilesChanged), 6);
            let outcome = match host_services::watch(target) {
                Ok(()) => "watch:ok".to_string(),
                Err(e) => format!("watch:err({e})"),
            };
            record(&outcome);
            // @end-example
            // …and a path the plugin was NOT granted. Recording the refusal
            // beside the success is what makes the grant check observable
            // rather than assumed: a seam that permitted everything would
            // produce the same first line.
            let denied = match host_services::watch("/") {
                Ok(()) => "denied:ok".to_string(),
                Err(e) => format!("denied:err({e})"),
            };
            record(&denied);
        }
        // LH.0.1: download a file, if the test asked for one. Started from
        // INSIDE `register-events` on purpose: the subscription two lines up is
        // recorded but not on the bus yet, and a loopback transfer finishes
        // faster than the wiring does — so this is also the test that the host
        // holds the start until there is something to hear the outcome.
        if let Ok(request) = std::fs::read_to_string(DOWNLOAD_REQUEST) {
            let mut lines = request.lines();
            let (url, sha256, dest) = (
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
            );
            // @example host-services.http-download: Subscribe to the job events, then fetch a pinned file into a granted directory
            events::subscribe(&kind_filter(EventKind::JobProgress), 8);
            events::subscribe(&kind_filter(EventKind::JobFinished), 8);
            let outcome = match host_services::http_download(url, sha256, dest) {
                // The id is what `job-finished` will carry; a plugin running
                // several jobs keys its state by it.
                Ok(_id) => "download:started".to_string(),
                Err(e) => format!("download:err({e})"),
            };
            record(&outcome);
            // @end-example
            // …and a host the plugin was NOT granted, recorded beside it so
            // the grant check is observed rather than assumed.
            let denied = match host_services::http_download(
                "https://not-granted.invalid/x",
                sha256,
                dest,
            ) {
                Ok(_) => "download-denied:started".to_string(),
                Err(e) => format!("download-denied:err({e})"),
            };
            record(&denied);
            // @example host-services.cancel-job: Cancel a job by the id the function that started it returned
            // A cancel of an id that is not ours to cancel — or not anyone's —
            // is nothing, so a plugin need not track which are still running.
            host_services::cancel_job(u64::MAX);
            // @end-example
            // Stay inside `register-events` for longer than a loopback
            // transfer takes. Without this the request above is followed by
            // the subscription wiring within microseconds, the race is never
            // lost, and a host that did NOT hold the start would pass — which
            // is what this test did the first time it was written. A real
            // guest lingers here whenever registration has other work to do.
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        // LH.0.2: unpack an archive, if the test asked for one. The same two
        // job events as a download — that is the point of their being generic.
        if let Ok(request) = std::fs::read_to_string(EXTRACT_REQUEST) {
            let mut lines = request.lines();
            let (src, dest, format) = (
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
            );
            let format = if format == "tar-gz" {
                host_services::ArchiveFormat::TarGz
            } else {
                host_services::ArchiveFormat::Gz
            };
            // @example host-services.extract-archive: Unpack a downloaded archive into a granted directory and wait for `job-finished`
            events::subscribe(&kind_filter(EventKind::JobFinished), 9);
            let outcome = match host_services::extract_archive(src, dest, format) {
                Ok(_id) => "extract:started".to_string(),
                Err(e) => format!("extract:err({e})"),
            };
            record(&outcome);
            // @end-example
        }
        // LH.0.3: run a process, if the test asked for one.
        if let Ok(request) = std::fs::read_to_string(SPAWN_REQUEST) {
            let mut lines = request.lines();
            let command = lines.next().unwrap_or_default();
            let args: Vec<String> = lines.map(str::to_string).collect();
            // @example host-services.spawn-process: Run a program with explicit arguments and subscribe to its output and exit
            events::subscribe(&kind_filter(EventKind::JobOutput), 10);
            events::subscribe(&kind_filter(EventKind::JobFinished), 10);
            // No shell: each element of `args` is one argument, whatever it
            // contains. `""` runs it in the editor's working directory.
            let outcome = match host_services::spawn_process(command, &args, "") {
                Ok(_id) => "spawn:started".to_string(),
                Err(e) => format!("spawn:err({e})"),
            };
            record(&outcome);
            // @end-example
        }
        // LH.0.4: register a language server, if the test asked for one.
        if let Ok(request) = std::fs::read_to_string(SERVER_REQUEST) {
            let mut lines = request.lines();
            let (id, command, pattern) = (
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
                lines.next().unwrap_or_default(),
            );
            // @example host-services.register-server: Register an installed language server so matching buffers start it
            let config = host_services::ServerConfig {
                id: id.to_string(),
                // An absolute path into the install tree — no `PATH` entry
                // needed, which is the point of managing the install.
                command: command.to_string(),
                args: vec!["--stdio".to_string()],
                env: Vec::new(),
                root_markers: vec![".git".to_string()],
                file_patterns: vec![pattern.to_string()],
                language_id: id.to_string(),
                initialization_options: None,
            };
            let registered = host_services::register_server(&config);
            // @end-example
            match &registered {
                Ok(_token) => record("register:ok"),
                Err(e) => record(&format!("register:err({e})")),
            }
            if let (Ok(token), true) = (registered, std::fs::metadata(SERVER_WITHDRAW).is_ok()) {
                // @example host-services.unregister-server: Withdraw a server registration by its token, restoring what it shadowed
                host_services::unregister_server(token);
                // @end-example
                record("unregister:done");
            }
        }
    }

    /// Deliver one matching event. Handler 3 traps, handler 4 is a no-op (the
    /// perf-ratchet dispatch path); the rest append their kind to the data-dir
    /// log so the test can observe end-to-end delivery.
    fn on_event(handler: u32, ev: Event) {
        // OC.2: a wake armed from inside a handler — the shape org's clock-in
        // uses (a chord fires, the mode's actor arms the minute tick). This one's
        // `on-wake` traps, so a test can prove a trapping wake quarantines the
        // plugin without wedging the actor for everyone else.
        if handler == 5 {
            wake_state::POISON.with(|p| p.set(events::wake_every(50)));
            return;
        }
        if handler == 3 {
            // Deliberate trap: the host catches it, logs, and skips this
            // delivery — the plugin stays subscribed (§8).
            unreachable!("fixture poison handler traps on delivery");
        }
        if handler == 4 {
            // No-op: pure dispatch, no side effect (perf measurement).
            return;
        }
        // PH7.8c: the doorbell rung from `register-events`. Recording it is the
        // whole proof — a dropped event leaves this line absent, which is what
        // the symptom looked like from the outside: nothing, forever.
        if handler == 7 {
            if let Event::Plugin(p) = &ev {
                if p.name == "fixture/registered" {
                    record("7:registered-event-delivered");
                }
            }
            return;
        }
        // LH.0: a host job moved, or ended. The outcome line is the proof —
        // it is written with no action dispatched after the request.
        if handler == 8 {
            match &ev {
                Event::JobProgress(p) => {
                    record(&format!("8:job-progress:{}", p.done));
                }
                Event::JobFinished(f) => match &f.outcome {
                    Ok(()) => record("8:job-finished:ok"),
                    Err(e) => record(&format!("8:job-finished:err({e})")),
                },
                _ => record("8:not-a-job-event"),
            }
            return;
        }
        // LH.0.3: a process wrote something, or exited.
        if handler == 10 {
            match &ev {
                Event::JobOutput(o) => {
                    for line in &o.lines {
                        record(&format!("10:out:{line}"));
                    }
                }
                Event::JobFinished(f) => match &f.outcome {
                    Ok(()) => record("10:exit:ok"),
                    Err(e) => record(&format!("10:exit:err({e})")),
                },
                _ => record("10:not-a-job-event"),
            }
            return;
        }
        // LH.0.2: the unpack ended. A bare `.gz` carries no mode, so the file
        // it produced is not runnable until the host is asked to make it so.
        if handler == 9 {
            let Event::JobFinished(f) = &ev else {
                record("9:not-a-job-event");
                return;
            };
            match &f.outcome {
                Ok(()) => record("9:extract-finished:ok"),
                Err(e) => record(&format!("9:extract-finished:err({e})")),
            }
            if let (Ok(()), Ok(request)) = (&f.outcome, std::fs::read_to_string(EXTRACT_REQUEST)) {
                let dest = request.lines().nth(1).unwrap_or_default();
                // @example host-services.set-executable: Make an unpacked binary runnable once its job reports success
                let outcome = match host_services::set_executable(dest) {
                    Ok(()) => "set-executable:ok".to_string(),
                    Err(e) => format!("set-executable:err({e})"),
                };
                record(&outcome);
                // @end-example
            }
            return;
        }
        // OR.2: a watch batch. Record how many paths arrived and their
        // basenames, so the host test can assert BOTH that the change crossed
        // and that a burst arrived as one batch rather than as N.
        if handler == 6 {
            let Event::FilesChanged(paths) = &ev else {
                record("6:not-files-changed");
                return;
            };
            let names: Vec<&str> = paths
                .iter()
                // Native paths: `\` separates on Windows.
                .filter_map(|p| p.rsplit(['/', '\\']).next())
                .filter(|n| !n.is_empty())
                .collect();
            record(&format!("6:files-changed:{}:{}", names.len(), names.join(",")));
            // A batch naming `stop.org` disarms the watch, so a test can prove
            // `unwatch` reaches a live watcher rather than merely being
            // callable.
            if names.contains(&"stop.org") {
                // @example host-services.unwatch: Disarm a directory watch from inside the batch handler that decided to stop
                if let Ok(target) = std::fs::read_to_string(WATCH_TARGET) {
                    let outcome = match host_services::unwatch(target.trim()) {
                        Ok(()) => "unwatch:ok".to_string(),
                        Err(e) => format!("unwatch:err({e})"),
                    };
                    record(&outcome);
                }
                // @end-example
            }
            return;
        }
        record(&format!("{handler}:{}", label(&ev)));
        // @example host-services.emit-event: Emit a typed plugin event on save, its payload MessagePack-encoded by the SDK derive
        // PH7.8b.2/3: on a save, EMIT a plugin-defined event. The SDK derive
        // MessagePack-encodes a typed struct (`SavedEcho`) into the opaque
        // payload; it crosses to the bus verbatim and a consumer sharing the type
        // decodes it (the e2e test). The host never parses the bytes.
        if handler == 1 {
            let echo = SavedEcho {
                path: match &ev {
                    Event::DocumentSaved(p) => p.path.clone(),
                    _ => String::new(),
                },
            };
            host_services::emit_event(SavedEcho::NAME, &echo.encode());
        }
        // @end-example
    }

    /// OC.2: an armed wake came due.
    ///
    /// Two arms. The **ticker** appends `wake:<n>` to the same log the event
    /// deliveries write, so a test can see it advance with no event published at
    /// all — the whole point of the seam — and then cancels itself, so the log
    /// stops growing and a test can prove `cancel-wake` reached a live timer.
    /// The **poison** wake traps, exercising the same graceful-degradation
    /// contract `on-event`'s handler 3 does.
    fn on_wake(id: u32) {
        if id != 0 && wake_state::POISON.with(|p| p.get()) == id {
            unreachable!("fixture poison wake traps on delivery");
        }
        // @example events.cancel-wake: Count a periodic wake's fires in `on-wake` and cancel it after the last one
        let n = wake_state::FIRES.with(|f| {
            let n = f.get() + 1;
            f.set(n);
            n
        });
        record(&format!("wake:{n}"));
        if n >= wake_state::CANCEL_AFTER {
            events::cancel_wake(id);
        }
        // @end-example
    }
}

export!(Component);
