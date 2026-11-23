//! LH.0.3 — the `host-services.spawn-process` seam.
//!
//! Design: `docs/dev/architecture/lighthouse.md` §3.3. Slice plan:
//! `docs/dev/operations/slice-plans/lighthouse.md` LH.0.3.
//!
//! ## What it is for, and who may use it
//!
//! Running a package manager (`npm install`, `pip install`, `go install`) for
//! a language server that ships no pre-built binary. Spawning an arbitrary
//! program is full trust — it is not sandboxed and it runs as the user — so
//! the seam is gated on `proc:spawn`, which `capability.rs` grants to bundled
//! plugins only. A user-installed plugin that asks is refused at the call.
//!
//! ## A job, with one thing of its own
//!
//! A subprocess is a host job ([`crate::job`]): an id now, `job-finished`
//! later. What it adds is **output** — `Event::JobOutput`, lines batched per
//! quiet interval, stdout and stderr interleaved as they arrive. The plugin
//! appends them to a buffer it owns; the host has no opinion about where they
//! go.
//!
//! A non-zero exit is an ordinary outcome and reports as an `err` naming the
//! status. It is not a host failure and is never a panic.
//!
//! ## Interactive jobs: stdin (OB.14)
//!
//! `spawn-interactive` is the same job with stdin left open. A guest writes
//! with `write-job-stdin` and ends the input with `close-job-stdin`. Babel's
//! `:session` is the first user: a REPL that outlives one block, fed a block
//! at a time.
//!
//! - **A writer thread per job owns the pipe.** A child that stops reading
//!   fills the pipe, and a write to a full pipe blocks. That must never be
//!   the guest's call, which may be on the grammar seam, a keystroke.
//! - **The table names who may write.** It is process-wide, like
//!   [`crate::job`]'s, because the instance that writes is not always the one
//!   that spawned. Each entry carries its owner, and another plugin's id
//!   reads as no job at all, the same `err` as an id that never existed, so
//!   ids cannot be probed.
//! - **The entry goes when the input can no longer be written.** When the
//!   job's work returns (exit, cancel, spawn failure) and when the guest
//!   closes it. A write after either is an `err`: input that cannot arrive
//!   is never reported as sent.
//!
//! ## Cancelling kills the whole tree
//!
//! `npm` is a shell script that starts `node`, which starts more. Killing only
//! the direct child leaves the rest running and holding the output pipes open,
//! so the job would never finish. The child is therefore started in its own
//! process group and a cancel signals the group.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read, Write as _};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use lattice_runtime::EventBus;

use crate::capability::CapabilityGrant;
use crate::job::{Job, PendingJob};

/// Quiet interval between output deliveries — the job's progress cadence.
const OUTPUT_INTERVAL: Duration = Duration::from_millis(100);

/// Most lines one delivery carries. A bound on **event size, not coverage**: a
/// chattier burst is delivered as several events, nothing is dropped.
const MAX_BATCH: usize = 256;

/// Longest line delivered, in bytes. A tool that prints a megabyte with no
/// newline (a progress bar redrawn with `\r`, a minified blob) would otherwise
/// cross the boundary as one string.
const MAX_LINE: usize = 4096;

#[derive(Debug)]
struct Request {
    command: String,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    /// `spawn-interactive`: stdin is a pipe the guest writes to.
    interactive: bool,
}

/// What a guest sends an interactive job's stdin.
enum Input {
    Data(Vec<u8>),
    Close,
}

/// Interactive jobs taking input, by id: the plugin that owns each and the
/// channel to its writer thread.
type InputTable = HashMap<u64, (u32, Sender<Input>)>;

static INPUT: LazyLock<Mutex<InputTable>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The table, surviving a poisoned lock: it holds ids and senders, nothing a
/// panicking holder could have left half-written.
fn input() -> MutexGuard<'static, InputTable> {
    INPUT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Send `msg` to `plugin`'s job `id`. Another plugin's job, a finished one, a
/// closed one and one that never existed are one `err`, by design (module
/// doc).
fn send_input(plugin: u32, id: u64, msg: Input) -> Result<(), String> {
    let close = matches!(msg, Input::Close);
    let mut table = input();
    let sent = match table.get(&id) {
        Some((owner, tx)) if *owner == plugin => tx.send(msg).is_ok(),
        _ => false,
    };
    if !sent {
        return Err(format!(
            "job {id} takes no input: it is not running, its input is closed, or it was not \
             started with spawn-interactive by this plugin"
        ));
    }
    if close {
        table.remove(&id);
    }
    Ok(())
}

/// `write-job-stdin`: queue `data` for job `id`'s stdin. Returns at once; the
/// writer thread delivers it.
pub(crate) fn write_stdin(plugin: u32, id: u64, data: Vec<u8>) -> Result<(), String> {
    send_input(plugin, id, Input::Data(data))
}

/// `close-job-stdin`: end job `id`'s input, after anything already written.
pub(crate) fn close_stdin(plugin: u32, id: u64) -> Result<(), String> {
    send_input(plugin, id, Input::Close)
}

/// An interactive job's place in [`INPUT`]. Registered when the job is
/// prepared, so a write that arrives before the process exists is queued
/// rather than refused, and removed when it drops: with the job's work,
/// whether that work ran, failed to spawn, or was never started.
struct InputEntry {
    id: u64,
    /// Taken by the writer thread once there is a pipe to write to.
    rx: Option<Receiver<Input>>,
}

impl InputEntry {
    fn register(id: u64, plugin: u32) -> Self {
        let (tx, rx) = channel::<Input>();
        input().insert(id, (plugin, tx));
        Self { id, rx: Some(rx) }
    }

    /// Start the thread that writes what was queued, and what comes, to
    /// `pipe`.
    fn start_writer(&mut self, mut pipe: ChildStdin) {
        let Some(rx) = self.rx.take() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("lattice-plugin-process-in".to_string())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        // A child that exited breaks the pipe: stop. The
                        // entry goes when the work returns.
                        Input::Data(d) => {
                            if pipe.write_all(&d).and_then(|()| pipe.flush()).is_err() {
                                break;
                            }
                        }
                        Input::Close => break,
                    }
                }
                // Dropping `pipe` is the child's end of file.
            });
        if let Err(e) = spawned {
            // No writer, so refuse every write from now on rather than
            // accept input that goes nowhere.
            tracing::warn!(
                id = self.id,
                ?e,
                "spawn-interactive: no stdin writer thread"
            );
            input().remove(&self.id);
        }
    }
}

impl Drop for InputEntry {
    fn drop(&mut self) {
        input().remove(&self.id);
    }
}

/// Validate a spawn against `grant` and make it a job.
///
/// Refused here, by name: a plugin without `proc:spawn`, an empty command, and
/// a working directory that is not a directory. Whether the program *exists*
/// is not checked here — that is the spawn's to report, as the job's outcome.
pub(crate) fn prepare(
    grant: &CapabilityGrant,
    bus: Arc<EventBus>,
    plugin: u32,
    command: &str,
    args: Vec<String>,
    cwd: &str,
    interactive: bool,
) -> Result<PendingJob, String> {
    if !grant.may_spawn(command) {
        // info!: user-actionable (a plugin was denied a capability).
        tracing::info!(
            command,
            "host-services spawn-process denied: not covered by the plugin's proc:spawn grant"
        );
        return Err(if grant.spawn_options.is_empty() {
            format!(
                "spawn denied: '{command}' — the plugin has no `proc:spawn` grant \
                 (bundled plugins only)"
            )
        } else {
            // OB.2: the user decides what runs; say where.
            format!(
                "spawn denied: '{command}' is not in {}; add it there to allow it",
                grant.spawn_options.join(" or ")
            )
        });
    }
    if command.is_empty() {
        return Err("spawn failed: no command given".to_string());
    }
    let cwd = if cwd.is_empty() {
        None
    } else {
        let dir = PathBuf::from(cwd);
        if !dir.is_dir() {
            return Err(format!("spawn failed: '{cwd}' is not a directory"));
        }
        Some(dir)
    };
    let request = Request {
        command: command.to_string(),
        args,
        cwd,
        interactive,
    };
    Ok(PendingJob::new_with_id(plugin, bus, "process", move |id| {
        let mut input = request
            .interactive
            .then(|| InputEntry::register(id, plugin));
        move |job| run(&request, job, input.as_mut())
    }))
}

/// `input` is an interactive job's registered stdin, written to once the
/// child exists, and held (so writable) until this returns.
fn run(req: &Request, job: &mut Job, input: Option<&mut InputEntry>) -> Result<(), String> {
    let mut command = Command::new(&req.command);
    command
        .args(&req.args)
        .stdin(if req.interactive {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &req.cwd {
        command.current_dir(cwd);
    }
    own_process_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn failed: cannot run '{}': {e}", req.command))?;

    if let (Some(input), Some(pipe)) = (input, child.stdin.take()) {
        input.start_writer(pipe);
    }

    let (tx, rx) = channel::<String>();
    if let Some(out) = child.stdout.take() {
        read_lines(out, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        read_lines(err, tx.clone());
    }
    // The readers hold the only senders now, so the channel closes when both
    // pipes do — which is when every process holding them has exited.
    drop(tx);

    if let Err(e) = pump(&rx, job) {
        kill_tree(&mut child);
        let _ = child.wait();
        return Err(e);
    }

    let status = child
        .wait()
        .map_err(|e| format!("spawn failed: waiting for '{}': {e}", req.command))?;
    if status.success() {
        Ok(())
    } else {
        Err(match status.code() {
            Some(code) => format!("'{}' exited with status {code}", req.command),
            None => format!("'{}' was terminated by a signal", req.command),
        })
    }
}

/// Forward output to the guest in batches until both pipes close, or the job
/// is cancelled.
fn pump(rx: &Receiver<String>, job: &mut Job) -> Result<(), String> {
    let mut batch: Vec<String> = Vec::new();
    let mut last_flush = Instant::now();
    loop {
        job.check_cancelled()?;
        let closed = match rx.recv_timeout(OUTPUT_INTERVAL) {
            Ok(line) => {
                batch.push(line);
                false
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => true,
        };
        let due = last_flush.elapsed() >= OUTPUT_INTERVAL || batch.len() >= MAX_BATCH;
        if !batch.is_empty() && (closed || due) {
            job.output(std::mem::take(&mut batch));
            last_flush = Instant::now();
        }
        if closed {
            return Ok(());
        }
    }
}

/// Read `pipe` to its end on a thread of its own, one line per send.
///
/// A thread per pipe because a blocking read on one would starve the other,
/// and a full pipe blocks the child. Bytes that are not UTF-8 are replaced
/// rather than failing the line — a tool's output is not ours to reject.
fn read_lines(pipe: impl Read + Send + 'static, tx: Sender<String>) {
    let spawned = std::thread::Builder::new()
        .name("lattice-plugin-process-out".to_string())
        .spawn(move || {
            let mut reader = BufReader::new(pipe);
            let mut raw = Vec::new();
            loop {
                raw.clear();
                match reader.read_until(b'\n', &mut raw) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                while raw.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                    raw.pop();
                }
                let mut line = String::from_utf8_lossy(&raw).into_owned();
                if line.len() > MAX_LINE {
                    let mut cut = MAX_LINE;
                    while !line.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    line.truncate(cut);
                    line.push('…');
                }
                if tx.send(line).is_err() {
                    return;
                }
            }
        });
    if let Err(error) = spawned {
        // The pipe is dropped with the closure, so the child sees it closed
        // and the job still ends; it just ends without this stream's output.
        tracing::warn!(%error, "plugin process output reader could not start");
    }
}

/// Start the child as the leader of its own process group, so the whole tree
/// can be signalled at once.
#[cfg(unix)]
fn own_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;
    command.process_group(0);
}

#[cfg(not(unix))]
fn own_process_group(_command: &mut Command) {}

/// Kill the child and everything it started.
///
/// The group is signalled through `kill(1)` rather than `libc::kill`: this
/// crate denies `unsafe`, and one short-lived process on a cancel is a fair
/// price for keeping it that way. The direct `Child::kill` afterwards covers a
/// platform or a sandbox with no `kill` on `PATH`.
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let group = format!("-{}", child.id());
        let _ = Command::new("kill")
            .args(["-KILL", "--", &group])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    let _ = child.kill();
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::job::CANCELLED;
    use crate::job::test_support::{Outcome, job_bus, outcome_of};
    use lattice_protocol::Event as NativeEvent;
    use tokio::sync::mpsc::UnboundedReceiver;

    fn bundled() -> CapabilityGrant {
        CapabilityGrant {
            proc_spawn: true,
            ..Default::default()
        }
    }

    /// OB.2: a grant that follows an option of commands.
    fn option_bound(commands: &[&str]) -> CapabilityGrant {
        CapabilityGrant {
            spawn_commands: commands.iter().map(|c| c.to_string()).collect(),
            spawn_options: vec!["org.babel.languages".to_string()],
            ..Default::default()
        }
    }

    #[test]
    fn an_option_bound_grant_permits_a_listed_command() {
        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare(
            &option_bound(&["sh"]),
            bus,
            7,
            "sh",
            vec!["-c".to_string(), "echo ok".to_string()],
            "",
            false,
        )
        .expect("a listed command may spawn");
        let id = pending.id();
        let _guard = pending.start();
        assert_eq!(outcome_of(&mut rx, id).output, vec!["ok"]);
    }

    #[test]
    fn an_option_bound_grant_refuses_an_unlisted_command_naming_the_option() {
        let bus = Arc::new(EventBus::new());
        let Err(err) = prepare(&option_bound(&["python3"]), bus, 7, "sh", vec![], "", false) else {
            panic!("sh is not listed");
        };
        assert!(err.contains("'sh'"), "{err}");
        assert!(
            err.contains("org.babel.languages"),
            "names the option: {err}"
        );
    }

    /// One process from request to outcome.
    fn sh(script: &str, cwd: &str) -> Outcome {
        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare(
            &bundled(),
            bus,
            7,
            "sh",
            vec!["-c".to_string(), script.to_string()],
            cwd,
            false,
        )
        .expect("a bundled plugin may spawn");
        let id = pending.id();
        let _guard = pending.start();
        outcome_of(&mut rx, id)
    }

    #[test]
    fn output_and_a_clean_exit_are_reported() {
        let o = sh("echo one; echo two", "");
        assert_eq!(o.result, Ok(()));
        assert_eq!(o.plugin, 7, "addressed to the plugin that started it");
        assert_eq!(o.output, vec!["one", "two"]);
    }

    #[test]
    fn stderr_is_delivered_too() {
        let o = sh("echo to-err >&2", "");
        assert_eq!(o.result, Ok(()));
        assert_eq!(o.output, vec!["to-err"]);
    }

    /// An ordinary outcome: reported, with the status, and with the output
    /// that explains it.
    #[test]
    fn a_non_zero_exit_is_an_outcome_not_a_crash() {
        let o = sh("echo why; exit 3", "");
        let err = o.result.expect_err("non-zero is a failure");
        assert!(err.contains("status 3"), "{err}");
        assert_eq!(o.output, vec!["why"], "the output still arrived");
    }

    #[test]
    fn the_working_directory_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let o = sh("pwd -P", dir.path().to_str().unwrap());
        assert_eq!(o.result, Ok(()));
        assert_eq!(o.output, vec![canonical.display().to_string()]);
    }

    #[test]
    fn a_program_that_does_not_exist_is_a_reported_failure() {
        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare(
            &bundled(),
            bus,
            1,
            "lattice-no-such-program-xyz",
            Vec::new(),
            "",
            false,
        )
        .unwrap();
        let id = pending.id();
        let _guard = pending.start();
        let err = outcome_of(&mut rx, id).result.expect_err("nothing to run");
        assert!(err.contains("cannot run"), "{err}");
        assert!(err.contains("lattice-no-such-program-xyz"), "{err}");
    }

    /// A burst is batched: many lines, few deliveries, none lost.
    #[test]
    fn a_chatty_process_is_batched_without_losing_lines() {
        let o = sh(
            "i=0; while [ $i -lt 2000 ]; do echo line-$i; i=$((i+1)); done",
            "",
        );
        assert_eq!(o.result, Ok(()));
        assert_eq!(o.output.len(), 2000);
        assert_eq!(o.output[0], "line-0");
        assert_eq!(o.output[1999], "line-1999");
        assert!(
            o.output_batches < 200,
            "2000 lines arrived in {} deliveries",
            o.output_batches
        );
    }

    #[test]
    fn an_over_long_line_is_truncated() {
        let o = sh("head -c 20000 /dev/zero | tr '\\0' 'x'; echo", "");
        assert_eq!(o.result, Ok(()));
        assert_eq!(o.output.len(), 1);
        assert!(o.output[0].len() <= MAX_LINE + '…'.len_utf8());
        assert!(o.output[0].ends_with('…'));
    }

    /// Is process `pid` still there? (`kill -0` signals nothing; it only asks.)
    fn alive(pid: &str) -> bool {
        Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// **The `npm` case.** The shell starts a grandchild and waits on it. A
    /// cancel must take the grandchild down too — killing only the shell ends
    /// the job just as promptly and leaves a `node` running that nobody owns.
    #[test]
    fn cancelling_kills_the_whole_process_tree() {
        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare(
            &bundled(),
            bus,
            3,
            "sh",
            vec!["-c".to_string(), "sleep 60 & echo $!; wait".to_string()],
            "",
            false,
        )
        .unwrap();
        let id = pending.id();
        let _guard = pending.start();
        // Long enough for the shell to print the grandchild's pid.
        std::thread::sleep(Duration::from_millis(500));
        crate::job::cancel(3, id);
        let o = outcome_of(&mut rx, id);
        assert_eq!(o.result, Err(CANCELLED.to_string()));

        let pid = o.output.first().expect("the grandchild's pid was printed");
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(pid), "the grandchild ({pid}) died with the job");
    }

    // ---- the synchronous refusals -------------------------------------

    /// **The trust boundary.** Without the grant nothing is spawned at all.
    #[test]
    fn a_plugin_without_proc_spawn_is_refused_at_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("ran");
        let err = prepare(
            &CapabilityGrant::default(),
            Arc::new(EventBus::new()),
            1,
            "touch",
            vec![marker.display().to_string()],
            "",
            false,
        )
        .expect_err("no grant");
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("proc:spawn"), "names the grant: {err}");
        assert!(!marker.exists(), "and nothing ran");
    }

    #[test]
    fn a_bad_working_directory_or_empty_command_is_refused() {
        let bus = || Arc::new(EventBus::new());
        let err = prepare(
            &bundled(),
            bus(),
            1,
            "sh",
            Vec::new(),
            "/no/such/dir/xyz",
            false,
        )
        .expect_err("no such directory");
        assert!(err.contains("not a directory"), "{err}");
        let err = prepare(&bundled(), bus(), 1, "", Vec::new(), "", false).expect_err("no command");
        assert!(err.contains("no command"), "{err}");
    }

    // ---- OB.14: interactive jobs ------------------------------------------

    fn interactive(plugin: u32, script: &str) -> (PendingJob, UnboundedReceiver<NativeEvent>) {
        let bus = Arc::new(EventBus::new());
        let rx = job_bus(&bus);
        let pending = prepare(
            &bundled(),
            bus,
            plugin,
            "sh",
            vec!["-c".to_string(), script.to_string()],
            "",
            true,
        )
        .expect("a bundled plugin may spawn");
        (pending, rx)
    }

    /// Input written, then closed, comes back. Written BEFORE the job starts
    /// too: the guest holds the id from the call, and a write it makes at once
    /// is queued, not refused.
    #[test]
    fn a_round_trip_through_cat() {
        let (pending, mut rx) = interactive(11, "cat");
        let id = pending.id();
        write_stdin(11, id, b"early\n".to_vec()).expect("queued before the process exists");
        let _guard = pending.start();
        write_stdin(11, id, b"one\ntwo\n".to_vec()).unwrap();
        close_stdin(11, id).unwrap();
        let o = outcome_of(&mut rx, id);
        assert_eq!(o.result, Ok(()), "cat exits at end of input");
        assert_eq!(o.output, vec!["early", "one", "two"]);
        assert!(
            write_stdin(11, id, b"x".to_vec()).is_err(),
            "closed is closed"
        );
    }

    #[test]
    fn write_after_exit_is_an_err() {
        let (pending, mut rx) = interactive(12, "echo done");
        let id = pending.id();
        let _guard = pending.start();
        assert_eq!(outcome_of(&mut rx, id).result, Ok(()));
        let err = write_stdin(12, id, b"late\n".to_vec()).expect_err("the job is over");
        assert!(err.contains("takes no input"), "{err}");
        assert!(close_stdin(12, id).is_err());
    }

    /// Another plugin's id is no job at all: the same `err` as one that
    /// never existed, so ids cannot be probed. A `spawn-process` job is not
    /// writable either.
    #[test]
    fn another_plugins_job_is_not_writable() {
        let (pending, mut rx) = interactive(13, "cat");
        let id = pending.id();
        let _guard = pending.start();
        let theirs = write_stdin(99, id, b"x\n".to_vec()).expect_err("not plugin 99's job");
        let none = write_stdin(99, u64::MAX, b"x\n".to_vec()).expect_err("no such job");
        assert_eq!(
            theirs.replace(&id.to_string(), "N"),
            none.replace(&u64::MAX.to_string(), "N"),
            "indistinguishable from a missing job"
        );
        assert!(close_stdin(99, id).is_err());
        close_stdin(13, id).unwrap();
        assert_eq!(outcome_of(&mut rx, id).result, Ok(()));

        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let plain = prepare(
            &bundled(),
            bus,
            13,
            "sh",
            vec!["-c".to_string(), "sleep 1".to_string()],
            "",
            false,
        )
        .unwrap();
        let id = plain.id();
        let _guard = plain.start();
        assert!(
            write_stdin(13, id, b"x".to_vec()).is_err(),
            "no stdin to write"
        );
        let _ = outcome_of(&mut rx, id);
    }

    /// A REPL is a process waiting on input forever; cancelling it must end
    /// it, and what it started, as for any job.
    #[test]
    fn cancelling_kills_an_interactive_jobs_tree() {
        let (pending, mut rx) = interactive(14, "sleep 60 & echo $!; cat");
        let id = pending.id();
        let _guard = pending.start();
        std::thread::sleep(Duration::from_millis(500));
        crate::job::cancel(14, id);
        let o = outcome_of(&mut rx, id);
        assert_eq!(o.result, Err(CANCELLED.to_string()));
        assert!(write_stdin(14, id, b"x".to_vec()).is_err(), "gone with it");
        let pid = o.output.first().expect("the grandchild's pid was printed");
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(pid), "the grandchild ({pid}) died with the job");
    }
}
