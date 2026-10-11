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
//! ## Cancelling kills the whole tree
//!
//! `npm` is a shell script that starts `node`, which starts more. Killing only
//! the direct child leaves the rest running and holding the output pipes open,
//! so the job would never finish. The child is therefore started in its own
//! process group and a cancel signals the group.

use std::io::{BufRead as _, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
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
) -> Result<PendingJob, String> {
    if !grant.proc_spawn {
        // info!: user-actionable (a plugin was denied a capability).
        tracing::info!(
            command,
            "host-services spawn-process denied: plugin has no proc:spawn grant"
        );
        return Err(format!(
            "spawn denied: '{command}' — the plugin has no `proc:spawn` grant \
             (bundled plugins only)"
        ));
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
    };
    Ok(PendingJob::new(plugin, bus, "process", move |job| {
        run(&request, job)
    }))
}

fn run(req: &Request, job: &mut Job) -> Result<(), String> {
    let mut command = Command::new(&req.command);
    command
        .args(&req.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = &req.cwd {
        command.current_dir(cwd);
    }
    own_process_group(&mut command);

    let mut child = command
        .spawn()
        .map_err(|e| format!("spawn failed: cannot run '{}': {e}", req.command))?;

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

    fn bundled() -> CapabilityGrant {
        CapabilityGrant {
            proc_spawn: true,
            ..Default::default()
        }
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
        )
        .expect_err("no grant");
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("proc:spawn"), "names the grant: {err}");
        assert!(!marker.exists(), "and nothing ran");
    }

    #[test]
    fn a_bad_working_directory_or_empty_command_is_refused() {
        let bus = || Arc::new(EventBus::new());
        let err = prepare(&bundled(), bus(), 1, "sh", Vec::new(), "/no/such/dir/xyz")
            .expect_err("no such directory");
        assert!(err.contains("not a directory"), "{err}");
        let err = prepare(&bundled(), bus(), 1, "", Vec::new(), "").expect_err("no command");
        assert!(err.contains("no command"), "{err}");
    }
}
