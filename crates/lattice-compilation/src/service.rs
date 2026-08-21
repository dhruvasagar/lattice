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
//!
//! # Safety
//!
//! The two `unsafe` blocks below (`libc::setpgid` in the spawn path
//! and `libc::kill` in the kill path) are Unix-only and guarded by
//! `#[cfg(unix)]`. Both are the only way to atomically terminate an
//! entire process group (shell + all pipeline grandchildren). Without
//! process-group kill, pipe grandchildren outlive the parent and keep
//! the pipe readers blocking indefinitely.

#![allow(unsafe_code)]

use std::io::BufRead;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

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
    /// Kill the currently running compilation child process, if any.
    /// No-op when no child is running. The reader pipes will EOF on
    /// the closed child, and the drain will publish a Finished chunk
    /// with the termination summary.
    fn kill(&self);
}

/// Per the `ServiceRegistry` Arc/TypeId convention: register and
/// look up under this exact alias.
pub type CompilationServiceHandle = Arc<dyn CompilationService>;

/// Publish one `Append` chunk per line for real-time streaming.
/// The drain in `mode.rs` coalesces all events available per tick
/// into a single `apply_edit_batch` — batching is its concern, not
/// the pipe reader's. Publishing line-by-line gives the user
/// immediate feedback without measurable overhead (event bus push is
/// one `ArcSwap` store).
const READER_BATCH_LINES: usize = 1;

/// Mutable run state shared between `run` (which resolves the
/// cmdline + kills the prior child) and the coordinator task
/// (which stores the live child for kill-on-recompile and reaps
/// it on exit).
#[derive(Default)]
struct RunState {
    last_cmdline: Option<String>,
    /// PR.4: the directory the last run was launched in, so
    /// `:recompile` repeats it.
    ///
    /// Captured here rather than re-derived because by the time
    /// `:recompile` fires, the active buffer is `*compilation*` itself —
    /// which has no path, so re-resolving would silently fall back to
    /// the working directory and rebuild the wrong project. "Do that
    /// again" has to mean where, not just what.
    ///
    /// Lives beside `last_cmdline` for the same reason it does: this
    /// state's lifetime is the service's, not the editor's.
    last_cwd: Option<PathBuf>,
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
    /// CM.5: the interned `compilation.ansi.*` elements captured
    /// colour is painted with, filled by the mode during activation
    /// (see [`crate::CompilationAnsiSlot`] for why it is late-bound).
    ///
    /// An empty slot leaves stripping in place and skips the spans —
    /// the right degradation, because escape sequences must never
    /// reach the buffer whether or not anyone can colour them.
    ansi: Option<crate::CompilationAnsiSlot>,
    /// CM.6b: plugin-contributed parser factories, snapshotted once per
    /// run. `None` in a stripped harness that registered no handle;
    /// empty in the common case where no `error-parser` plugin is
    /// loaded. Either way every native parser still runs.
    parser_factories: Option<crate::CompilationParserFactoriesHandle>,
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
            ansi: None,
            parser_factories: None,
        }
    }

    /// CM.5: read the ANSI palette from `slot` when a run starts.
    ///
    /// Separate from [`Self::new`] because a stripped test harness
    /// stands up neither a theme registry nor the slot — and a service
    /// without one is still correct, just monochrome.
    pub fn with_ansi_slot(mut self, slot: crate::CompilationAnsiSlot) -> Self {
        self.ansi = Some(slot);
        self
    }

    /// CM.6b: read plugin-contributed parser factories from `handle`
    /// when a run starts.
    ///
    /// Separate from [`Self::new`] for the same reason the ANSI slot is:
    /// a stripped harness registers no handle, and a service without one
    /// is still correct — it just runs the built-in parsers only.
    pub fn with_parser_factories(
        mut self,
        handle: crate::CompilationParserFactoriesHandle,
    ) -> Self {
        self.parser_factories = Some(handle);
        self
    }

    fn publish(&self, chunk: OutputChunk) {
        self.events.publish_typed(CompilationOutputPushed { chunk });
    }
}

impl CompilationService for DefaultCompilationService {
    fn run(&self, cmdline: Option<String>, cwd: Option<PathBuf>) {
        // Resolve the cmdline AND the directory, record both for
        // `:recompile`, and kill any prior child — all under one lock so
        // a rapid recompile can't race two live children.
        let (cmd, cwd) = {
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
                    // A fresh `:compile` re-binds the directory too: the
                    // caller resolved it from the buffer the command
                    // fired in, which is the answer we want to repeat.
                    st.last_cwd = cwd.clone();
                    (s, cwd)
                }
                // `:recompile` / bare `:make`. The caller's `cwd` is
                // discarded on purpose — it was resolved from whatever
                // is active NOW, and after the first run that is the
                // pathless `*compilation*` buffer.
                _ => match st.last_cmdline.clone() {
                    Some(prev) => (prev, st.last_cwd.clone()),
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
        // Resolve the palette once per run rather than per line. An
        // unfilled slot means the mode had no theme registry to intern
        // against; stripping still happens, colouring does not.
        let ansi: Option<crate::ansi::AnsiPalette> =
            self.ansi.as_ref().and_then(|slot| slot.get().copied());
        // CM.6b: snapshot the plugin factories once per run, not per
        // reader and certainly not per line. A plugin loaded mid-build
        // therefore joins the NEXT build — which is the honest
        // behaviour, since a parser that starts halfway through a
        // stream has no pending state for what it missed.
        let factories: Option<Arc<crate::CompilationParserFactories>> = self
            .parser_factories
            .as_ref()
            .map(|h| h.load_full())
            .filter(|set| !set.is_empty());
        self.runtime.spawn_blocking(move || {
            let mut command = Command::new("sh");
            command
                .arg("-c")
                .arg(&cmd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            // Unix: put the shell (and all its pipeline children) in
            // their own process group so kill() terminates the entire
            // tree, not just the shell PID. Without this, pipe
            // grandchildren outlive the parent and keep the pipe
            // readers blocking indefinitely.
            #[cfg(unix)]
            unsafe {
                command.pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                });
            }
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
            let shared: Arc<Mutex<Vec<ErrorEntry>>> = Arc::new(Mutex::new(Vec::new()));

            // Two dedicated reader threads so a large pipe can't
            // deadlock the other. Both parse for error locations:
            // compile diagnostics (rustc/cargo) go to stderr; test
            // failure output (thread panics) goes to stdout.
            let out_events = events.clone();
            let out_qf = qf_bus.clone();
            let out_shared = shared.clone();
            let out_factories = factories.clone();
            let out_reader = std::thread::spawn(move || {
                read_parsed_pipe(
                    stdout,
                    &out_events,
                    &out_qf,
                    &out_shared,
                    ansi.as_ref(),
                    out_factories.as_deref(),
                )
            });
            let err_events = events.clone();
            let err_qf = qf_bus.clone();
            let err_shared = shared.clone();
            let err_factories = factories.clone();
            let err_reader = std::thread::spawn(move || {
                read_parsed_pipe(
                    stderr,
                    &err_events,
                    &err_qf,
                    &err_shared,
                    ansi.as_ref(),
                    err_factories.as_deref(),
                )
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

    fn kill(&self) {
        if let Ok(mut st) = self.state.lock()
            && let Some(mut child) = st.child.take()
        {
            // Unix: kill the entire process group, not just the
            // shell PID. The shell was put in its own process
            // group via pre_exec(setpgid(0,0)), so killpg()
            // terminates the shell AND every pipeline grandchild.
            // Without this, pipe grandchildren (seq, while, ...)
            // survive the shell kill and keep stdout/stderr open.
            #[cfg(unix)]
            {
                let pgid = child.id();
                unsafe { libc::kill(-(pgid as i32), libc::SIGKILL) };
            }
            #[cfg(not(unix))]
            {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
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
/// CM.5: every line is passed through [`crate::ansi::clean_line`]
/// **before** anything else sees it, so the escape sequences are gone
/// from both the text that reaches the buffer and the text the
/// parsers match against. That ordering is the point: a coloured
/// `error[E0308]` carries `ESC[1m` in front of `error`, and matching
/// the raw line would silently miss it.
///
/// `sgr` carries the active attributes across lines within this pipe
/// (a producer may open a colour on one line and close it on the
/// next). It is per-pipe, never shared — stdout and stderr are
/// independent streams.
///
/// CM.6b: `factories` mints this reader's **own** plugin parsers. Each
/// reader gets fresh instances for exactly the reason the `sgr` state
/// above is per-pipe: the two streams carry independent pending state,
/// and a WASM-backed parser could not be shared regardless (it owns a
/// `Store`). They register ahead of the catch-all — see
/// [`ParserRegistry::register_before_catch_all`].
fn read_parsed_pipe<R: std::io::Read>(
    pipe: Option<R>,
    events: &Arc<EventBus>,
    qf_bus: &InboundBus<Vec<ErrorEntry>>,
    shared: &Mutex<Vec<ErrorEntry>>,
    ansi: Option<&crate::ansi::AnsiPalette>,
    factories: Option<&crate::CompilationParserFactories>,
) {
    let Some(pipe) = pipe else {
        return;
    };
    let reader = std::io::BufReader::new(pipe);
    let mut batch = String::new();
    let mut batch_spans: Vec<Vec<lattice_cells::StyledSpan>> = Vec::new();
    let mut lines_in_batch = 0usize;
    let mut registry = ParserRegistry::with_builtins();
    if let Some(factories) = factories {
        for parser in factories.create_all() {
            registry.register_before_catch_all(parser);
        }
    }
    let mut sgr = crate::ansi::SgrState::default();
    for line in reader.lines() {
        let raw = match line {
            Ok(l) => l,
            Err(e) => {
                tracing::debug!(error = %e, "compilation: pipe read error; skipping line");
                continue;
            }
        };
        let clean = crate::ansi::clean_line(&raw, &mut sgr, ansi);
        let new_entries = registry.feed(&clean.text);
        if !new_entries.is_empty() {
            let mut guard = match shared.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            guard.extend(new_entries);
            let _ = qf_bus.send(guard.clone());
        }
        batch.push_str(&clean.text);
        batch.push('\n');
        batch_spans.push(clean.spans);
        lines_in_batch += 1;
        if lines_in_batch >= READER_BATCH_LINES {
            publish(
                events,
                OutputChunk::Append {
                    text: std::mem::take(&mut batch),
                    spans: std::mem::take(&mut batch_spans),
                },
            );
            lines_in_batch = 0;
        }
    }
    if !batch.is_empty() {
        publish(
            events,
            OutputChunk::Append {
                text: batch,
                spans: batch_spans,
            },
        );
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
        collect_run_with_factories(cmdline, dur, None)
    }

    /// Like [`collect_run_with_qf`] but with CM.6b plugin parser
    /// factories registered on the service.
    fn collect_run_with_factories(
        cmdline: Option<String>,
        dur: Duration,
        factories: Option<crate::CompilationParserFactoriesHandle>,
    ) -> (Vec<OutputChunk>, Vec<ErrorEntry>) {
        let bus = Arc::new(EventBus::new());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<CompilationOutputPushed>();
        bus.subscribe_typed::<CompilationOutputPushed>(tx);

        let (qf_bus, mut qf_drain, latest) = qf_capture();
        let mut svc =
            DefaultCompilationService::new(bus.clone(), tokio::runtime::Handle::current(), qf_bus);
        if let Some(handle) = factories {
            svc = svc.with_parser_factories(handle);
        }
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
                OutputChunk::Append { text, .. } => text.clone(),
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

    /// CM.5, and the reason CM.5 is a correctness fix rather than a
    /// cosmetic one: a colourised diagnostic carries `ESC[…m` in front
    /// of `error`, so a parser fed the raw line matches nothing and the
    /// entry silently never reaches the error list. This is the same
    /// diagnostic as `stderr_diagnostics_populate_the_error_list`,
    /// wearing the escapes a `--color=always` build would put on it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn colourised_diagnostics_still_populate_the_error_list() {
        let (chunks, entries) = tokio::task::spawn_blocking(|| {
            collect_run_with_qf(
                Some(
                    "printf '\\033[1m\\033[31mmain.c:10:5: error:\\033[0m bad thing\\n' 1>&2"
                        .to_string(),
                ),
                Duration::from_secs(3),
            )
        })
        .await
        .unwrap();

        assert_eq!(
            entries.len(),
            1,
            "expected one parsed entry from colourised output, got {entries:?}"
        );
        let e = &entries[0];
        assert_eq!(e.path, PathBuf::from("main.c"));
        assert_eq!(e.line, 9);
        assert_eq!(e.col, 4);
        assert_eq!(e.severity, ErrorSeverity::Error);

        // And the text that reached the buffer carries no escapes.
        let appended: String = chunks
            .iter()
            .filter_map(|c| match c {
                OutputChunk::Append { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert!(
            !appended.contains('\u{1b}'),
            "escape sequences must not reach the buffer, got {appended:?}"
        );
        assert!(appended.contains("main.c:10:5: error: bad thing"));
    }

    /// The palette slot is unfilled in this harness (no theme
    /// registry), so colouring is off — but stripping is not
    /// conditional on it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stripping_happens_without_a_palette() {
        let chunks = tokio::task::spawn_blocking(|| {
            collect_run(
                Some("printf '\\033[32mgreen\\033[0m\\n'".to_string()),
                Duration::from_secs(3),
            )
        })
        .await
        .unwrap();

        let appended: String = chunks
            .iter()
            .filter_map(|c| match c {
                OutputChunk::Append { text, spans } => {
                    assert!(
                        spans.iter().all(|l| l.is_empty()),
                        "no palette was interned, so no spans should be produced"
                    );
                    Some(text.clone())
                }
                _ => None,
            })
            .collect();
        assert!(appended.contains("green"));
        assert!(!appended.contains('\u{1b}'));
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

    /// CM.6b: a factory registered on the service reaches the error
    /// list for a line no built-in parser understands.
    #[derive(Debug)]
    struct QqFactory {
        created: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Debug)]
    struct QqParser;

    impl crate::CompilationParser for QqParser {
        fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
            line.strip_prefix("QQ ")
                .map(|rest| {
                    vec![ErrorEntry {
                        path: std::path::PathBuf::from(rest),
                        line: 7,
                        col: 3,
                        severity: ErrorSeverity::Warning,
                        message: "from a plugin".to_string(),
                    }]
                })
                .unwrap_or_default()
        }
    }

    impl crate::CompilationParserFactory for QqFactory {
        fn plugin_id(&self) -> u64 {
            42
        }
        fn create(&self) -> Option<Box<dyn crate::CompilationParser>> {
            self.created
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Box::new(QqParser))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_registered_factory_contributes_entries() {
        let created = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut set = crate::CompilationParserFactories::new();
        set.register(Arc::new(QqFactory {
            created: created.clone(),
        }));
        let handle: crate::CompilationParserFactoriesHandle =
            Arc::new(arc_swap::ArcSwap::from_pointee(set));

        let created_probe = created.clone();
        let (_chunks, entries) = tokio::task::spawn_blocking(move || {
            collect_run_with_factories(
                Some("echo 'QQ src/plugin.rs'".to_string()),
                Duration::from_secs(3),
                Some(handle),
            )
        })
        .await
        .unwrap();

        assert!(
            entries
                .iter()
                .any(|e| e.path == std::path::PathBuf::from("src/plugin.rs")
                    && e.severity == ErrorSeverity::Warning
                    && e.message == "from a plugin"),
            "the plugin parser's entry should reach the error list: {entries:?}"
        );
        // One instance per reader — the property the factory exists for.
        assert_eq!(
            created_probe.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "stdout and stderr each mint their own parser"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_registered_factory_leaves_the_builtins_alone() {
        // The common case: no `error-parser` plugin loaded. The
        // built-in parsers must behave exactly as before — a
        // gnu-style line still lands.
        let (_chunks, entries) = tokio::task::spawn_blocking(|| {
            collect_run_with_factories(
                Some("echo 'src/a.rs:3:5: error: boom'".to_string()),
                Duration::from_secs(3),
                None,
            )
        })
        .await
        .unwrap();

        assert!(
            entries
                .iter()
                .any(|e| e.path == std::path::PathBuf::from("src/a.rs")),
            "built-in parsing is unaffected by the factory seam: {entries:?}"
        );
    }

    /// PR.4: `:recompile` repeats WHERE, not just what.
    ///
    /// By the time it fires, the active buffer is `*compilation*`,
    /// which has no path — so the host resolves a project root from it
    /// and gets the working directory. If the service took that value,
    /// a recompile would rebuild the wrong project while looking like
    /// it worked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn recompile_reuses_the_directory_of_the_last_real_run() {
        let dir = std::env::temp_dir().join(format!(
            "lattice-recompile-cwd-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let canonical = std::fs::canonicalize(&dir).unwrap();

        let bus = Arc::new(EventBus::new());
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<CompilationOutputPushed>();
        bus.subscribe_typed::<CompilationOutputPushed>(tx);
        let (qf_bus, _drain, _latest) = qf_capture();
        let svc =
            DefaultCompilationService::new(bus.clone(), tokio::runtime::Handle::current(), qf_bus);

        let collect = |rx: &mut tokio::sync::mpsc::UnboundedReceiver<CompilationOutputPushed>| {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut text = String::new();
            let mut saw_finish = false;
            while std::time::Instant::now() < deadline && !saw_finish {
                match rx.try_recv() {
                    Ok(ev) => match ev.chunk {
                        OutputChunk::Append { text: t, .. } => text.push_str(&t),
                        OutputChunk::Finished { .. } => saw_finish = true,
                        OutputChunk::Reset { .. } => {}
                    },
                    Err(_) => std::thread::sleep(Duration::from_millis(10)),
                }
            }
            text
        };

        // A real run, in `dir`.
        svc.run(Some("pwd".to_string()), Some(canonical.clone()));
        let first = tokio::task::spawn_blocking({
            let mut rx = rx;
            move || {
                let t = collect(&mut rx);
                (t, rx)
            }
        })
        .await
        .unwrap();
        let (first_text, mut rx) = first;
        assert!(
            first_text.contains(&canonical.display().to_string()),
            "the first run should be in {canonical:?}, got {first_text:?}"
        );

        // `:recompile` — no cmdline, and a DIFFERENT cwd, standing in for
        // the pathless `*compilation*` buffer resolving to somewhere else.
        svc.run(None, Some(std::env::temp_dir()));
        let second = tokio::task::spawn_blocking(move || collect(&mut rx))
            .await
            .unwrap();
        assert!(
            second.contains(&canonical.display().to_string()),
            "recompile must reuse the first run's directory, got {second:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn service_is_debug() {
        // Trait bound sanity: the handle stays object-safe + Debug.
        fn assert_debug<T: std::fmt::Debug + Send + Sync>() {}
        assert_debug::<DefaultCompilationService>();
    }
}
