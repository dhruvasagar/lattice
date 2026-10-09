//! Running a formatter, with the failure modes spelled out.
//!
//! Blocking by design. The caller decides where this runs — the host
//! puts it on `spawn_blocking`, never the actor or UI thread — and
//! keeping the spawn itself synchronous makes it testable without a
//! runtime.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::spec::FormatterSpec;

/// Wall-clock ceiling for one formatter run.
///
/// A formatter is a batch tool on a file-sized input; anything past
/// this is hung, not slow. The number matters because IN.9 runs this
/// on the save path, where the rule is that a formatter must never
/// cost the user their write.
pub const FORMAT_TIMEOUT: Duration = Duration::from_secs(2);

/// Why a formatter run produced no edits.
///
/// Every variant is a case the caller must handle differently, which
/// is why this is not a `String`: "not installed" is routine and
/// silent on save, "non-zero exit" carries diagnostics the user needs
/// to see, and "timed out" means a process was killed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The program is not on `PATH`. Routine — a user without
    /// `prettier` installed should not be nagged on every save.
    NotFound { program: String },
    /// The formatter ran and rejected the input. `stderr` is the
    /// compiler-style diagnostic and belongs in front of the user.
    Failed { program: String, stderr: String },
    /// Killed at its time limit ([`FORMAT_TIMEOUT`] for a formatter).
    TimedOut { program: String, after: Duration },
    /// The formatter wrote bytes that are not UTF-8. Refusing is the
    /// only safe answer: splicing them into the rope would corrupt the
    /// buffer.
    NotUtf8 { program: String },
    /// Spawning or piping failed for a reason other than the program
    /// being absent.
    Io { program: String, message: String },
}

impl FormatError {
    /// One-line form for the echo area.
    pub fn message(&self) -> String {
        match self {
            Self::NotFound { program } => format!("formatter not found: {program}"),
            Self::Failed { program, stderr } => {
                let first = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                if first.is_empty() {
                    format!("{program} failed")
                } else {
                    format!("{program}: {first}")
                }
            }
            Self::TimedOut { program, after } => {
                format!("{program} timed out after {}s", after.as_secs())
            }
            Self::NotUtf8 { program } => format!("{program} produced invalid UTF-8"),
            Self::Io { program, message } => format!("{program}: {message}"),
        }
    }

    /// Whether this is worth interrupting the user for.
    ///
    /// `NotFound` is not: it means the tool simply is not installed,
    /// which is a configuration state rather than an event. The others
    /// describe something that went wrong during work the user asked
    /// for.
    pub fn is_noteworthy(&self) -> bool {
        !matches!(self, Self::NotFound { .. })
    }
}

/// Run `spec` over `input`, returning the formatted text.
///
/// Blocking, bounded by [`FORMAT_TIMEOUT`]. On timeout the child is
/// killed rather than left to leak.
pub fn run(spec: &FormatterSpec, input: &str, path: Option<&Path>) -> Result<String, FormatError> {
    let mut command = Command::new(spec.program);
    command.args(spec.args);
    if let (Some(flag), Some(p)) = (spec.filename_flag, path) {
        command.arg(format!("{flag}={}", p.display()));
    }
    run_command(command, spec.program, input, FORMAT_TIMEOUT)
}

/// Wall-clock ceiling for a `:{range}!cmd` filter.
///
/// Far longer than a formatter's: the user typed this command and may well
/// mean something slow. It exists so a command that waits on a terminal it
/// does not have is eventually killed rather than leaked.
pub const FILTER_TIMEOUT: Duration = Duration::from_secs(60);

/// Run a shell command line over `input` — vim's `:{range}!cmd`.
///
/// The line goes to the platform shell (`sh -c`, or `cmd /C` on Windows),
/// so pipes, quoting and globs mean what they mean in a terminal. `cwd` is
/// where it runs. Blocking, bounded by `timeout`.
pub fn run_shell(
    command_line: &str,
    input: &str,
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<String, FormatError> {
    let mut command = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(command_line);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(command_line);
        c
    };
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    run_command(command, command_line, input, timeout)
}

/// Feed `input` to `command`'s stdin and collect its stdout.
///
/// The three pipes are serviced concurrently. Writing all of stdin before
/// reading any of stdout deadlocks as soon as the child answers with more
/// than a pipe holds (64 KiB on Linux) before it has finished reading —
/// which is any streaming filter, `cat` or `sed`, over a large buffer — and
/// reading stdout only after the child exits has the same shape from the
/// other side: a child blocked on a full stdout never exits. The first
/// would hang where no timeout could reach it; the second surfaced as a
/// spurious timeout on any file whose formatted text ran past the pipe.
fn run_command(
    mut command: Command,
    label: &str,
    input: &str,
    timeout: Duration,
) -> Result<String, FormatError> {
    let program = label.to_string();
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(FormatError::NotFound { program });
        }
        Err(e) => {
            return Err(FormatError::Io {
                program,
                message: e.to_string(),
            });
        }
    };

    // Write the buffer and close stdin, or the child waits forever for
    // input that is already all there. On its own thread, so a child that
    // answers as it reads cannot block us mid-write.
    let writer = child.stdin.take().map(|mut stdin| {
        let input = input.as_bytes().to_vec();
        std::thread::spawn(move || {
            // A child that exits early (bad input) closes the pipe while
            // we are still writing, which surfaces as a broken pipe. That
            // is not an I/O failure worth reporting — the real error is on
            // stderr.
            let _ = stdin.write_all(&input);
        })
    });
    fn drain<R: std::io::Read + Send + 'static>(
        pipe: Option<R>,
    ) -> Option<std::thread::JoinHandle<Vec<u8>>> {
        pipe.map(|mut pipe| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = pipe.read_to_end(&mut bytes);
                bytes
            })
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    // Poll rather than block so the timeout can actually fire.
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(FormatError::TimedOut {
                        program,
                        after: timeout,
                    });
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                return Err(FormatError::Io {
                    program,
                    message: e.to_string(),
                });
            }
        }
    };
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let collect = |handle: Option<std::thread::JoinHandle<Vec<u8>>>| {
        handle.and_then(|h| h.join().ok()).unwrap_or_default()
    };
    let (stdout, stderr) = (collect(stdout), collect(stderr));
    if !status.success() {
        return Err(FormatError::Failed {
            program,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        });
    }
    String::from_utf8(stdout).map_err(|_| FormatError::NotUtf8 { program })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an executable script into a temp dir and return a spec
    /// pointing at it.
    ///
    /// Every formatter test uses one of these rather than a real tool:
    /// CI must not depend on `rustfmt` or `prettier` being installed,
    /// and a fake lets the failure modes (non-zero exit, hang, garbage
    /// output) be produced on demand instead of hoped for.
    ///
    /// Unix only, and so is every test that uses it: the fake is a `#!/bin/sh`
    /// script, which Windows cannot execute ("not a valid Win32 application").
    /// What these tests pin — exit status, timeout, stderr, UTF-8 — is the
    /// runner's handling of a child process, which is the same code on every
    /// platform; the not-found path below needs no fake and runs everywhere.
    #[cfg(unix)]
    fn fake(name: &str, body: &str) -> (tempfile::TempDir, FormatterSpec) {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        // Run as `/bin/sh <script>`, not by exec'ing the script. A file that
        // was just written cannot be exec'd while ANY process still holds it
        // open for writing (ETXTBSY), and a concurrent test's `fork` inherits
        // this one's write descriptor for the instant before it closes — so
        // under the parallel test runner the fake occasionally failed to
        // start, and the hang test saw an `Io` error where it wanted a
        // timeout. A shell only READS the script, which is never refused.
        let script: &'static str = Box::leak(path.to_string_lossy().into_owned().into_boxed_str());
        let args: &'static [&'static str] = Box::leak(vec![script].into_boxed_slice());
        (
            dir,
            FormatterSpec {
                program: "/bin/sh",
                args,
                filename_flag: None,
            },
        )
    }

    #[cfg(unix)]
    #[test]
    fn a_successful_run_returns_stdout() {
        let (_d, spec) = fake("ok", "sed 's/a/b/g'");
        assert_eq!(run(&spec, "aaa\n", None).unwrap(), "bbb\n");
    }

    #[test]
    fn a_missing_program_is_reported_as_not_found_and_is_not_noteworthy() {
        let spec = FormatterSpec {
            program: "definitely-not-a-real-formatter-xyz",
            args: &[],
            filename_flag: None,
        };
        let err = run(&spec, "x", None).unwrap_err();
        assert!(matches!(err, FormatError::NotFound { .. }));
        assert!(
            !err.is_noteworthy(),
            "an uninstalled tool must not nag on every save"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_non_zero_exit_carries_stderr_to_the_user() {
        let (_d, spec) = fake("bad", "echo 'syntax error on line 3' >&2; exit 1");
        let err = run(&spec, "x", None).unwrap_err();
        match &err {
            FormatError::Failed { stderr, .. } => {
                assert!(stderr.contains("syntax error on line 3"))
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(err.is_noteworthy());
        assert!(err.message().contains("syntax error on line 3"));
    }

    #[cfg(unix)]
    #[test]
    fn a_hanging_formatter_is_killed_at_the_timeout() {
        let (_d, spec) = fake("hang", "sleep 30");
        let started = Instant::now();
        let err = run(&spec, "x", None).unwrap_err();
        assert!(matches!(err, FormatError::TimedOut { .. }));
        assert!(
            started.elapsed() < FORMAT_TIMEOUT + Duration::from_secs(2),
            "must not wait for the child to finish on its own"
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_output_is_refused_rather_than_spliced() {
        // Octal, not `\\xff`: the fake runs under `/bin/sh`, and dash (Ubuntu's)
        // has no hex escapes in `printf` — it emits the text `\xff\xfe`, which
        // is valid UTF-8, so the formatter "succeeded" and the test failed on CI.
        let (_d, spec) = fake("garbage", "printf '\\377\\376'");
        assert!(matches!(
            run(&spec, "x", None).unwrap_err(),
            FormatError::NotUtf8 { .. }
        ));
    }

    #[cfg(unix)]
    #[test]
    fn the_filename_flag_is_passed_when_the_spec_asks_for_it() {
        let (_d, mut spec) = fake("echoargs", "cat >/dev/null; echo \"$1\"");
        spec.filename_flag = Some("--name");
        let out = run(&spec, "x", Some(Path::new("/tmp/a.ts"))).unwrap();
        assert_eq!(out.trim(), "--name=/tmp/a.ts");
    }

    /// The reason the pipes are serviced concurrently. A megabyte through
    /// `cat` is sixteen pipefuls: written-then-read it never returns, and
    /// no timeout is watching the write.
    #[cfg(unix)]
    #[test]
    fn a_streaming_filter_over_more_than_a_pipeful_does_not_deadlock() {
        let input = "a line of text that is fairly ordinary\n".repeat(30_000);
        assert!(input.len() > 1_000_000);
        let output = run_shell("cat", &input, None, Duration::from_secs(20)).expect("cat runs");
        assert_eq!(output.len(), input.len());
        assert_eq!(output, input);
    }

    /// The command line goes to a shell: a pipe is a pipe.
    #[cfg(unix)]
    #[test]
    fn a_shell_line_is_run_by_a_shell() {
        let out = run_shell("sort | uniq", "b\na\nb\n", None, Duration::from_secs(20));
        assert_eq!(out.as_deref(), Ok("a\nb\n"));
    }

    /// A filter that fails reports its stderr and yields no text, so the
    /// caller has nothing to put in the buffer.
    #[cfg(unix)]
    #[test]
    fn a_failing_shell_line_carries_its_stderr() {
        let err = run_shell(
            "echo nope >&2; exit 3",
            "x\n",
            None,
            Duration::from_secs(20),
        )
        .expect_err("non-zero exit");
        assert_eq!(err.message(), "echo nope >&2; exit 3: nope");
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_line_runs_in_the_directory_it_is_given() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("here.txt"), "").expect("write");
        let out = run_shell("ls", "", Some(dir.path()), Duration::from_secs(20));
        assert_eq!(out.as_deref(), Ok("here.txt\n"));
    }
}
