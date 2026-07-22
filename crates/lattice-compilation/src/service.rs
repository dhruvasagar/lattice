//! CM.1: the compilation process-lifecycle service.
//!
//! Runs a shell command **pipe-captured** off the actor thread and
//! streams its stdout+stderr into the `*compilation*` buffer via
//! [`CompilationOutputPushed`] events. The editor actor runs a
//! `current_thread` runtime, so the blocking read loop must never
//! land on `tokio::spawn`: the process + coordinator run on
//! `spawn_blocking`, and the two pipe readers run on dedicated OS
//! threads (mirroring the terminal reader task). Nothing here
//! touches the UI/actor thread — paramount goal #1.
//!
//! Both stdout and stderr pipes are parsed for error locations —
//! compile diagnostics (rustc/cargo) go to stderr while test failure
//! output (thread panics) goes to stdout. A shared
//! `Arc<Mutex<Vec<ErrorEntry>>>` accumulator merges entries from both
//! streams into a single error list. Single-byte encoding errors in a
//! pipe are logged and skipped (the reader does not stop).
//!
//! Lifecycle: `:recompile` reuses the last cmdline and kills the
//! prior child before relaunching; the buffer is cleared via the
//! `Reset` chunk and re-streamed. On exit a one-shot `info!`
//! fires and a summary line is appended.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use lattice_mode::inbound::InboundBus;
use lattice_protocol::error_list::ErrorEntry;
use lattice_runtime::EventBus;

use crate::events::{CompilationOutputPushed, OutputChunk};
use crate::parser::ParserRegistry;

/// Process-lifecycle surface. Registered in the `ServiceRegistry`
/// at boot (as [`CompilationServiceHandle`]); the `AppEffect::CompileRun`
/// host arm looks it up and calls [`CompilationService::run`] (after
/// creating the `*compilation*` buffer host-side).
pub trait CompilationService: Send + Sync + std::fmt::Debug {
    /// Launch (or relaunch) a compilation.
    ///
    /// `cmdline`: `Some(cmd)` runs and records `cmd` as the last
    /// command; `None` reuses the last command (`:recompile` /
    /// `:make` with no argument). With no prior command, publishes
    /// a `Reset` explaining there is nothing to recompile and
    /// returns without spawning.
    ///
    /// `cwd`: working directory for the child process.
    fn run(&self, cmdline: Option<String>, cwd: Option<PathBuf>);
}

/// Per the `ServiceRegistry` Arc/TypeId convention: register and
/// look up under this exact alias.
pub type CompilationServiceHandle = Arc<dyn CompilationService>;

/// Coalesce this many captured lines before publishing an
/// `Append` chunk. Keeps a noisy build from publishing one event
/// per line while staying responsive on a slow trickle (a partial
/// batch flushes on EOF).
const READER_BATCH_LINES: usize = 8;

/// Mutable run state shared between `run` (which resolves the
/// cmdline + kills the prior child) and the coordinator task
/// (which stores the live child for kill-on-recompile and reaps
/// it on exit).
#[derive(Default)]
struct RunState {
    last_cmdline: Option<String>,
    child: Option<Child>,
}

/// Default [`CompilationService`]: `sh -c <cmd>`, pipe-captured,
/// streamed over the event bus.
pub struct DefaultCompilationService {
    events: Arc<EventBus>,
    runtime: tokio::runtime::Handle,
    state: Arc<Mutex<RunState>>,
    /// CM.3a: the off-thread → host-state seam for parsed error
    /// entries. The stderr reader accumulates parsed entries and sends
    /// the FULL accumulated list; the inbound handler (in `install`)
    /// maps it to `AppEffect::SetErrorList`. `send` wakes the editor so
    /// the list reaches the screen off-keystroke.
    qf_bus: InboundBus<Vec<ErrorEntry>>,
}

impl std::fmt::Debug for DefaultCompilationService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DefaultCompilationService")
            .finish_non_exhaustive()
    }
}

impl DefaultCompilationService {
    pub fn new(
        events: Arc<EventBus>,
        runtime: tokio::runtime::Handle,
        qf_bus: InboundBus<Vec<ErrorEntry>>,
    ) -> Self {
        Self {
            events,
            runtime,
            state: Arc::new(Mutex::new(RunState::default())),
            qf_bus,
        }
    }

    fn publish(&self, chunk: OutputChunk) {
        self.events.publish_typed(CompilationOutputPushed { chunk });
    }
}

impl CompilationService for DefaultCompilationService {
    fn run(&self, cmdline: Option<String>, cwd: Option<PathBuf>) {
        // Resolve the cmdline, record it for `:recompile`, and kill
        // any prior child — all under one lock so a rapid
        // recompile can't race two live children.
        let cmd = {
            let mut st = match self.state.lock() {
                Ok(guard) => guard,
                Err(_) => {
                    tracing::warn!("compilation: run-state lock poisoned; skipping run");
                    return;
                }
            };
            let resolved = match cmdline {
                Some(s) if !s.trim().is_empty() => {
                    st.last_cmdline = Some(s.clone());
                    s
                }
                _ => match st.last_cmdline.clone() {
                    Some(prev) => prev,
                    None => {
                        drop(st);
                        self.publish(OutputChunk::Reset {
                            header: "no previous compilation command\n\n".to_string(),
                        });
                        return;
                    }
                },
            };
            if let Some(mut prior) = st.child.take() {
                let _ = prior.kill();
                let _ = prior.wait();
            }
            resolved
        };

        // Clear + seed the buffer with the run header (the drain's
        // `Reset` path). Published before the spawn so it always
        // precedes the streamed `Append`s.
        self.publish(OutputChunk::Reset {
            header: format!("$ {cmd}\n\n"),
        });

            // CM.3a: a new run clears the stale error list. Send an
            // empty vec through the inbound seam so the host's
            // replace-semantics `set_error_list` drops the prior run's
            // entries before fresh ones stream in.
            let _ = self.qf_bus.send(Vec::new());

            let events = self.events.clone();
            let state = self.state.clone();
            let qf_bus = self.qf_bus.clone();
            self.runtime.spawn_blocking(move || {
                let mut command = Command::new("sh");
                command
                    .arg("-c")
                    .arg(&cmd)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                if let Some(dir) = cwd {
                    command.current_dir(dir);
                }

                let mut child = match command.spawn() {
                    Ok(child) => child,
                    Err(e) => {
                        publish(
                            &events,
                            OutputChunk::Finished {
                                summary: format!("\nCompilation failed to launch — {e}\n"),
                            },
                        );
                        return;
                    }
                };

                // Take the pipes out before parking the child in shared
                // state, so kill-on-recompile and this task's reap never
                // contend over the pipe fds.
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                if let Ok(mut st) = state.lock() {
                    st.child = Some(child);
                }

                // CM.3a+. Shared
                // `Arc<Mutex<Vec<ErrorEntry>>>` so both stdout and
                // stderr readers contribute to the same growing error
                // list. Each reader locks, extends, clones, and sends the
                // full state through the inbound seam — the host's
                // replace-semantics `set_error_list` then grows the
                // visible list regardless of which pipe delivered the
                // entry. Each reader has its own `ParserRegistry` (the
                // multi-line cargo parser is safe on both streams since
                // cargo only emits diagnostics on stderr and the
                // non-matching lines are no-ops).
                //
                // A shared list means the qf_bus always carries the
                // complete state: stdout entries can't overwrite stderr
                // entries (or vice versa) when the readers race.
                let shared: Arc<Mutex<Vec<ErrorEntry>>> =
                    Arc::new(Mutex::new(Vec::new()));

                // Two dedicated reader threads so a large pipe can't
                // deadlock the other. Both parse for error locations:
                // compile diagnostics (rustc/cargo) go to stderr; test
                // failure output (thread panics) goes to stdout.
                let out_events = events.clone();
                let out_qf = qf_bus.clone();
                let out_shared = shared.clone();
                let out_reader = std::thread::spawn(move || {
                    read_parsed_pipe(stdout, &out_events, &out_qf, &out_shared)
                });
                let err_events = events.clone();
                let err_qf = qf_bus.clone();
                let err_shared = shared.clone();
                let err_reader = std::thread::spawn(move || {
                    read_parsed_pipe(stderr, &err_events, &err_qf, &err_shared)
                });
                let _ = out_reader.join();
                let _ = err_reader.join();

            // Reap the child — unless a concurrent recompile already
            // took + killed it (then `child` is gone and the readers
            // EOF'd on the closed pipes).
            let waited = state.lock().ok().and_then(|mut st| st.child.take());
            let summary = match waited {
                Some(mut child) => match child.wait() {
                    Ok(status) => {
                        if status.success() {
                            tracing::info!("compilation finished");
                            format!("\nCompilation finished — {status}\n")
                        } else {
                            tracing::info!(%status, "compilation exited abnormally");
                            format!("\nCompilation exited abnormally — {status}\n")
                        }
                    }
                    Err(e) => format!("\nCompilation wait failed — {e}\n"),
                },
                None => "\nCompilation terminated\n".to_string(),
            };
            publish(&events, OutputChunk::Finished { summary });
        });
    }
}

/// Free helper so the coordinator closure (which owns `events` by
/// move) can publish without borrowing `&self`.
fn publish(events: &Arc<EventBus>, chunk: OutputChunk) {
    events.publish_typed(CompilationOutputPushed { chunk });
}

/// CM.3a+. Blocking line-reader for one captured pipe.
///
/// Streams text into the `*compilation*` buffer (coalescing up to
/// [`READER_BATCH_LINES`] lines per published `Append`) AND parses
/// every line through a [`ParserRegistry`] for error locations. New
/// [`ErrorEntry`]s are merged into the shared
/// `Arc<Mutex<Vec<ErrorEntry>>>` accumulator and the full accumulated
/// list is sent through the error inbound seam (replace-semantics
/// `set_error_list`).
///
/// A per-line encoding error is logged at `debug!` and skipped — the
/// reader does NOT stop on a single bad byte (prior behaviour lost the
/// rest of the pipe). Flushes a partial batch on EOF.
///
fn read_parsed_pipe<R: std::io::Read>(
    pipe: Option<R>,
    events: &Arc<EventBus>,
    qf_bus: &InboundBus<Vec<ErrorEntry>>,
    shared: &Mutex<Vec<ErrorEntry>>,
) {
    let Some(pipe) = pipe else {
        return;
    };
    let reader = std::io::BufReader::new(pipe);
    let mut batch = String::new();
    let mut lines_in_batch = 0usize;
    let mut registry = ParserRegistry::with_builtins();
    for line in reader.lines() {
        let l = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "compilation: pipe read error; skipping line");
                continue;
            }
        };
        let new_entries = registry.feed(&l);
        if !new_entries.is_empty() {
            let mut guard = shared.lock().unwrap();
            guard.extend(new_entries);
            let _ = qf_bus.send(guard.clone());
        }
        batch.push_str(&l);
        batch.push('\n');
        lines_in_batch += 1;
        if lines_in_batch >= READER_BATCH_LINES {
            publish(
                events,
                OutputChunk::Append {
                    text: std::mem::take(&mut batch),
                },
            );
            lines_in_batch = 0;
        }
    }
    if !batch.is_empty() {
        publish(events, OutputChunk::Append { text: batch });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_protocol::error_list::ErrorSeverity;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Build a throwaway error inbound bus. The handler stashes the
    /// LATEST full accumulated list into `latest` (the reader sends the
    /// full list each time, so the last send is the complete set); the
    /// returned drain must be run to flush queued sends through it.
    fn qf_capture() -> (
        InboundBus<Vec<ErrorEntry>>,
        lattice_mode::tick_callback::TickCallback,
        Arc<Mutex<Vec<ErrorEntry>>>,
    ) {
        let latest = Arc::new(Mutex::new(Vec::<ErrorEntry>::new()));
        let latest_in = latest.clone();
        let wake = Arc::new(tokio::sync::Notify::new());
        let (bus, drain) = lattice_mode::inbound::make_inbound::<Vec<ErrorEntry>, _>(
            wake,
            move |entries: Vec<ErrorEntry>| {
                *latest_in.lock().unwrap() = entries;
                Vec::new()
            },
        );
        (bus, drain, latest)
    }

    /// Collect chunks published to the bus for `dur`, running the
    /// service on the ambient multi-thread test runtime.
    fn collect_run(cmdline: Option<String>, dur: Duration) -> Vec<OutputChunk> {
        collect_run_with_qf(cmdline, dur).0
    }

    /// Like [`collect_run`] but also returns the final parsed error
    /// list captured off the inbound bus.
    fn collect_run_with_qf(
        cmdline: Option<String>,
        dur: Duration,
    ) -> (Vec<OutputChunk>, Vec<ErrorEntry>) {
        let bus = Arc::new(EventBus::new());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CompilationOutputPushed>();
        bus.subscribe_typed::<CompilationOutputPushed>(tx);

        let (qf_bus, mut qf_drain, latest) = qf_capture();
        let svc =
            DefaultCompilationService::new(bus.clone(), tokio::runtime::Handle::current(), qf_bus);
        svc.run(cmdline, None);

        // Drain until quiescent (no new chunk within a short window)
        // or the overall deadline elapses.
        let mut chunks = Vec::new();
        let deadline = std::time::Instant::now() + dur;
        loop {
            match rx.try_recv() {
                Ok(ev) => chunks.push(ev.chunk),
                Err(_) => {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        // Flush any queued error sends through the capture handler.
        let _ = qf_drain();
        let entries = latest.lock().unwrap().clone();
        (chunks, entries)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_streams_echo_output() {
        let chunks = tokio::task::spawn_blocking(|| {
            collect_run(Some("echo hello".to_string()), Duration::from_secs(3))
        })
        .await
        .unwrap();

        assert!(
            matches!(chunks.first(), Some(OutputChunk::Reset { .. })),
            "first chunk should be the run-header Reset, got {chunks:?}"
        );
        let joined: String = chunks
            .iter()
            .map(|c| match c {
                OutputChunk::Reset { header } => header.clone(),
                OutputChunk::Append { text } => text.clone(),
                OutputChunk::Finished { summary } => summary.clone(),
            })
            .collect();
        assert!(
            joined.contains("hello"),
            "output should contain 'hello': {joined:?}"
        );
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, OutputChunk::Finished { .. })),
            "a Finished chunk should arrive, got {chunks:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stderr_diagnostics_populate_the_error_list() {
        // A command whose stderr carries a gnu-style diagnostic must
        // flow through the parser → inbound seam and land as a parsed
        // error entry (0-based line/col).
        let (_chunks, entries) = tokio::task::spawn_blocking(|| {
            collect_run_with_qf(
                Some("printf 'main.c:10:5: error: bad thing\\n' 1>&2".to_string()),
                Duration::from_secs(3),
            )
        })
        .await
        .unwrap();

        assert_eq!(
            entries.len(),
            1,
            "expected one parsed entry, got {entries:?}"
        );
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("main.c"));
        assert_eq!(e.line, 9, "1-based 10 → 0-based 9");
        assert_eq!(e.col, 4, "1-based 5 → 0-based 4");
        assert_eq!(e.severity, ErrorSeverity::Error);
        assert_eq!(e.message, "bad thing");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recompile_with_no_prior_command_is_graceful() {
        // `None` cmdline with no recorded last command must not
        // panic; it publishes a single explanatory Reset.
        let chunks = tokio::task::spawn_blocking(|| collect_run(None, Duration::from_millis(300)))
            .await
            .unwrap();

        assert_eq!(
            chunks.len(),
            1,
            "expected exactly one Reset, got {chunks:?}"
        );
        match &chunks[0] {
            OutputChunk::Reset { header } => {
                assert!(header.contains("no previous compilation command"));
            }
            other => panic!("expected Reset, got {other:?}"),
        }
    }

    #[test]
    fn service_is_debug() {
        // Trait bound sanity: the handle stays object-safe + Debug.
        fn assert_debug<T: std::fmt::Debug + Send + Sync>() {}
        assert_debug::<DefaultCompilationService>();
    }
}
