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
    /// Killed at [`FORMAT_TIMEOUT`].
    TimedOut { program: String },
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
            Self::TimedOut { program } => {
                format!("{program} timed out after {}s", FORMAT_TIMEOUT.as_secs())
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
    let program = spec.program.to_string();
    let mut command = Command::new(spec.program);
    command.args(spec.args);
    if let (Some(flag), Some(p)) = (spec.filename_flag, path) {
        command.arg(format!("{flag}={}", p.display()));
    }
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

    // Write the buffer and close stdin, or the formatter waits forever
    // for input that is already all there.
    if let Some(mut stdin) = child.stdin.take() {
        // A formatter that exits early (bad input) closes the pipe
        // while we are still writing, which surfaces as a broken pipe.
        // That is not an I/O failure worth reporting — the real error
        // is on stderr, and `wait_with_output` below will collect it.
        let _ = stdin.write_all(input.as_bytes());
    }

    // Poll rather than block so the timeout can actually fire.
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() >= FORMAT_TIMEOUT {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(FormatError::TimedOut { program });
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
    }

    let output = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            return Err(FormatError::Io {
                program,
                message: e.to_string(),
            });
        }
    };
    if !output.status.success() {
        return Err(FormatError::Failed {
            program,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    String::from_utf8(output.stdout).map_err(|_| FormatError::NotUtf8 { program })
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
    fn fake(name: &str, body: &str) -> (tempfile::TempDir, FormatterSpec) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let program: &'static str = Box::leak(path.to_string_lossy().into_owned().into_boxed_str());
        (
            dir,
            FormatterSpec {
                program,
                args: &[],
                filename_flag: None,
            },
        )
    }

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

    #[test]
    fn invalid_utf8_output_is_refused_rather_than_spliced() {
        let (_d, spec) = fake("garbage", "printf '\\xff\\xfe'");
        assert!(matches!(
            run(&spec, "x", None).unwrap_err(),
            FormatError::NotUtf8 { .. }
        ));
    }

    #[test]
    fn the_filename_flag_is_passed_when_the_spec_asks_for_it() {
        let (_d, mut spec) = fake("echoargs", "cat >/dev/null; echo \"$1\"");
        spec.filename_flag = Some("--name");
        let out = run(&spec, "x", Some(Path::new("/tmp/a.ts"))).unwrap();
        assert_eq!(out.trim(), "--name=/tmp/a.ts");
    }
}
