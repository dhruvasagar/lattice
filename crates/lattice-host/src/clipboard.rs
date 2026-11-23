//! The clipboard backends **shared by both renderer peers**
//! (`docs/dev/architecture/clipboard.md` §4):
//!
//! - `ArboardClipboard` (CB.4, behind the `system-clipboard` feature): the
//!   native OS clipboard via `arboard`.
//! - [`WslClipboard`] (CW.1, always compiled): the *Windows* clipboard,
//!   reached from inside WSL through a Windows helper (`win32yank.exe`, else
//!   `powershell.exe`). It needs nothing but `std::process`, so it is not
//!   feature-gated: the Linux TUI release build, which has no `arboard`,
//!   gets it too.
//!
//! The [`lattice_core::Clipboard`] trait requires `Send + Sync` and its
//! `try_read` is called synchronously on the editor actor thread (from
//! `Editor::read_register_checked`) so a paste lands before the next key.
//! `arboard` satisfies `Send + Sync` and reads synchronously, so a single
//! backend serves both the TUI and GUI peers — this module is the shared home
//! so the load-bearing bounded-read logic (paramount #1) isn't duplicated and
//! can't drift between peers.
//!
//! Why here (`lattice-host`) and not `lattice-core` (where the trait lives):
//! the bounded read needs `tokio` + `lattice_runtime::block_on`, which
//! `lattice-core` (a leaf crate) doesn't depend on. `lattice-host` has both
//! and is depended on by both peers. `ArboardClipboard` is compiled only
//! behind the `system-clipboard` feature so host's default build stays free
//! of the X11/Wayland link libs `arboard` pulls on Linux.
//!
//! The GPUI peer uses these directly. The TUI peer composes them with its own
//! OSC52 write-only fallback (`lattice-ui-tui`'s `clipboard.rs`) for headless
//! / SSH sessions where there's no display.

use lattice_core::{Clipboard, ClipboardRead};

/// Native OS clipboard via `arboard` (macOS / X11 / Wayland / Windows).
/// Full read + write.
#[cfg(feature = "system-clipboard")]
pub struct ArboardClipboard {
    inner: std::sync::Arc<std::sync::Mutex<arboard::Clipboard>>,
}

#[cfg(feature = "system-clipboard")]
impl ArboardClipboard {
    /// `None` when no display is reachable (headless, or the display
    /// server refused the connection) — the caller falls back (OSC52 for
    /// the TUI, the in-memory register otherwise).
    pub fn new() -> Option<Self> {
        arboard::Clipboard::new().ok().map(|inner| Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(inner)),
        })
    }

    /// Bounded wait so a hung display-server round-trip can't wedge the
    /// actor (paramount #1). This guards against a hang; it is not a latency
    /// budget. A local X11 / Wayland / macOS / Windows read takes well under
    /// 5 ms, and a read that outlasts the bound is reported as
    /// [`ClipboardRead::Failed`] rather than pasting the register in its
    /// place. Until CW.2 the bound was 30 ms and a miss pasted the register,
    /// which is how a cold read through WSLg (≈130 ms) silently pasted the
    /// wrong text.
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
}

#[cfg(feature = "system-clipboard")]
impl Clipboard for ArboardClipboard {
    fn try_read(&self) -> ClipboardRead {
        let clipboard = self.inner.clone();
        let fut = async move {
            tokio::time::timeout(
                Self::READ_TIMEOUT,
                tokio::task::spawn_blocking(move || match clipboard.lock() {
                    Ok(mut cb) => match cb.get_text() {
                        Ok(text) => ClipboardRead::Text(text),
                        // Empty, or an image / file list: nothing to paste.
                        Err(arboard::Error::ContentNotAvailable) => ClipboardRead::NoText,
                        Err(e) => ClipboardRead::Failed(e.to_string()),
                    },
                    Err(_) => ClipboardRead::Failed("the clipboard lock was poisoned".into()),
                }),
            )
            .await
        };
        match lattice_runtime::block_on(fut) {
            Ok(Ok(read)) => read,
            Ok(Err(_)) => ClipboardRead::Failed("the clipboard read panicked".into()),
            Err(_) => ClipboardRead::Failed(format!(
                "the clipboard did not answer within {} ms",
                Self::READ_TIMEOUT.as_millis()
            )),
        }
    }

    fn write(&self, text: String) {
        // True fire-and-forget: schedule onto the shared runtime and
        // return immediately, no bound needed (nothing awaits the result).
        let clipboard = self.inner.clone();
        lattice_runtime::spawn_task(async move {
            let _ = tokio::task::spawn_blocking(move || {
                if let Ok(mut cb) = clipboard.lock() {
                    let _ = cb.set_text(text);
                }
            })
            .await;
        });
    }
}

/// The Windows helper a [`WslClipboard`] runs, for reads and writes alike.
/// `win32yank.exe` is preferred: it is the fast one (≈40–150 ms through WSL
/// interop, against ≈350 ms for PowerShell's start-up). `clip.exe` is deliberately not a candidate: it
/// decodes its input in the console's code page, so anything outside ASCII
/// arrives garbled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WslTool {
    /// `win32yank.exe -i --crlf` / `-o --lf` (Neovim ships one; it is often on
    /// `PATH`).
    Win32Yank(std::path::PathBuf),
    /// `powershell.exe … Set-Clipboard` / `Get-Clipboard`, which every Windows
    /// has.
    PowerShell(std::path::PathBuf),
}

/// Where Windows keeps PowerShell, for a WSL whose `PATH` leaves out the
/// Windows directories (`appendWindowsPath = false` in `wsl.conf`).
const POWERSHELL_FALLBACK: &str = "/mnt/c/Windows/System32/WindowsPowerShell/v1.0/powershell.exe";

/// The PowerShell side of a write. PowerShell decodes piped input in the
/// console's code page unless told otherwise; UTF-8 without a BOM is what a
/// Linux process sends.
const POWERSHELL_WRITE: &str = "[Console]::InputEncoding = New-Object System.Text.UTF8Encoding $false; \
     Set-Clipboard -Value ([Console]::In.ReadToEnd())";

/// The PowerShell side of a read: UTF-8 out, and `Write` rather than the
/// pipeline, which would append a newline the clipboard does not hold.
const POWERSHELL_READ: &str = "[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding $false; \
     $t = Get-Clipboard -Raw; if ($t) { [Console]::Out.Write($t) }";

impl WslTool {
    /// The first helper found: `win32yank.exe` on `path`, then
    /// `powershell.exe` on `path`, then PowerShell where Windows installs it.
    /// `exists` is the filesystem probe, a parameter so the choice is
    /// testable without a Windows install.
    pub fn find(
        path: Option<&std::ffi::OsStr>,
        exists: impl Fn(&std::path::Path) -> bool,
    ) -> Option<Self> {
        let on_path = |name: &str| {
            path.into_iter()
                .flat_map(std::env::split_paths)
                .map(|dir| dir.join(name))
                .find(|candidate| exists(candidate))
        };
        on_path("win32yank.exe")
            .map(Self::Win32Yank)
            .or_else(|| on_path("powershell.exe").map(Self::PowerShell))
            .or_else(|| {
                let fixed = std::path::PathBuf::from(POWERSHELL_FALLBACK);
                exists(&fixed).then_some(Self::PowerShell(fixed))
            })
    }

    /// The command that puts its standard input on the Windows clipboard.
    fn write_command(&self) -> std::process::Command {
        match self {
            // `--crlf`: Windows applications expect CRLF line ends.
            Self::Win32Yank(program) => command(program, &["-i", "--crlf"]),
            Self::PowerShell(program) => command(
                program,
                &[
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    POWERSHELL_WRITE,
                ],
            ),
        }
    }

    /// The command that prints the Windows clipboard's text.
    fn read_command(&self) -> std::process::Command {
        match self {
            Self::Win32Yank(program) => command(program, &["-o", "--lf"]),
            Self::PowerShell(program) => command(
                program,
                &["-NoProfile", "-NonInteractive", "-Command", POWERSHELL_READ],
            ),
        }
    }

    /// The helper's file name, for messages the user reads.
    fn name(&self) -> String {
        let (Self::Win32Yank(program) | Self::PowerShell(program)) = self;
        program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.display().to_string())
    }
}

fn command(program: &std::path::Path, args: &[&str]) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    cmd.args(args);
    cmd
}

/// The Windows clipboard from inside WSL (CW.1, CW.2).
///
/// A Linux process in WSL has no direct way onto the Windows clipboard. OSC52
/// depends on the terminal honouring it and on every multiplexer between
/// passing it on, and WSLg's X11 bridge is absent on older Windows. A Windows
/// helper reached through WSL interop works everywhere WSL does, which is why
/// the TUI and GUI both prefer this backend whenever they run in WSL.
///
/// **Writes** go to one worker thread, in order, so two quick yanks cannot
/// land in the wrong order; a burst coalesces to its newest text, since only
/// the last write would survive anyway. The helper takes ≈40–350 ms, so the
/// yank returns at once and the clipboard catches up.
///
/// **Reads** run the same helper, synchronously and bounded by
/// `WSL_READ_TIMEOUT` (1 s), so a paste lands before the next key does. While a
/// write of ours is still on its way to Windows, a read returns that text
/// without running the helper: it is what the clipboard is about to hold,
/// and asking Windows would return the text it replaces (`yy` then `p`
/// would paste the old clipboard). Reads and writes going through one helper
/// is the point: reading through WSLg instead lags a helper's write by up
/// to ≈0.3 s, the same stale paste by another route.
pub struct WslClipboard {
    tool: WslTool,
    writes: std::sync::mpsc::Sender<String>,
    /// The newest write not yet confirmed by the worker (see the type's doc).
    pending: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

/// A helper that has not finished after this long is killed, so a hung
/// Windows process cannot wedge every later write behind it.
const WSL_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a paste waits for the helper. win32yank answers in ≈40–150 ms and
/// PowerShell in ≈320 ms; the bound is for a helper that hangs, and a read
/// that outlasts it is reported, not replaced by the register.
const WSL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

impl WslClipboard {
    /// The backend for this session when it runs in WSL and a helper is
    /// found; `None` elsewhere (macOS, Windows, plain Linux) or when the
    /// worker thread cannot start, and the caller picks its usual backend.
    pub fn detect() -> Option<Self> {
        if !is_wsl() {
            return None;
        }
        let path = std::env::var_os("PATH");
        let Some(tool) = WslTool::find(path.as_deref(), |p| p.is_file()) else {
            tracing::debug!(
                "clipboard: WSL detected, but neither win32yank.exe nor powershell.exe \
                 was found; using the usual backend"
            );
            return None;
        };
        tracing::debug!("clipboard: WSL; reading and writing through {tool:?}");
        Self::with_tool(tool)
    }

    /// A backend reading and writing through `tool`. `None` when the worker
    /// thread cannot be spawned (logged); the caller falls back to its usual
    /// backend.
    pub fn with_tool(tool: WslTool) -> Option<Self> {
        let (writes, queue) = std::sync::mpsc::channel::<String>();
        let pending = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let worker_tool = tool.clone();
        let worker_pending = pending.clone();
        let spawned = std::thread::Builder::new()
            .name("clipboard-wsl".into())
            .spawn(move || {
                let mut warned = false;
                while let Ok(mut text) = queue.recv() {
                    while let Ok(newer) = queue.try_recv() {
                        text = newer;
                    }
                    let result = run_write(&worker_tool, &text);
                    // Confirmed (or given up on): reads ask Windows again —
                    // unless a newer write arrived meanwhile, which is still
                    // on its way.
                    if let Ok(mut pending) = worker_pending.lock()
                        && pending.as_deref() == Some(text.as_str())
                    {
                        *pending = None;
                    }
                    if let Err(reason) = result {
                        // Once at `warn` (a copy that never arrived is the
                        // user's to know about), then quietly.
                        if warned {
                            tracing::debug!("clipboard: copying to Windows failed: {reason}");
                        } else {
                            warned = true;
                            tracing::warn!("clipboard: copying to Windows failed: {reason}");
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => Some(Self {
                tool,
                writes,
                pending,
            }),
            Err(e) => {
                tracing::warn!("clipboard: cannot start the WSL clipboard worker: {e}");
                None
            }
        }
    }
}

impl Clipboard for WslClipboard {
    fn try_read(&self) -> ClipboardRead {
        if let Ok(pending) = self.pending.lock()
            && let Some(text) = pending.as_ref()
        {
            return ClipboardRead::Text(text.clone());
        }
        match run_helper(&self.tool, self.tool.read_command(), None, WSL_READ_TIMEOUT) {
            // PowerShell hands back CRLF; win32yank's `--lf` already did this.
            Ok(out) => match String::from_utf8_lossy(&out).replace("\r\n", "\n") {
                text if text.is_empty() => ClipboardRead::NoText,
                text => ClipboardRead::Text(text),
            },
            Err(reason) => ClipboardRead::Failed(reason),
        }
    }

    fn write(&self, text: String) {
        if let Ok(mut pending) = self.pending.lock() {
            *pending = Some(text.clone());
        }
        // Fails only once the worker has exited, which it does only when
        // this sender is dropped: never while `self` is alive.
        let _ = self.writes.send(text);
    }
}

/// Whether this process runs inside WSL. `WSLInterop` is the binfmt entry
/// that lets Linux run Windows executables, which is exactly what this
/// backend needs; the environment variables cover a WSL with that entry
/// registered under another name.
fn is_wsl() -> bool {
    std::path::Path::new("/proc/sys/fs/binfmt_misc/WSLInterop").exists()
        || std::env::var_os("WSL_INTEROP").is_some()
        || std::env::var_os("WSL_DISTRO_NAME").is_some()
}

/// Run `tool` with `text` on its standard input, bounded by
/// [`WSL_WRITE_TIMEOUT`].
fn run_write(tool: &WslTool, text: &str) -> Result<(), String> {
    run_helper(tool, tool.write_command(), Some(text), WSL_WRITE_TIMEOUT).map(|_| ())
}

/// Run `cmd` (one of `tool`'s), feed it `input`, and return what it printed.
/// A helper still running after `timeout` is killed and the run is an error;
/// so is a non-zero exit, with the helper's stderr. Input and output move on
/// threads of their own, so a large yank or paste cannot fill a pipe and
/// stall the helper until the timeout.
fn run_helper(
    tool: &WslTool,
    mut cmd: std::process::Command,
    input: Option<&str>,
    timeout: std::time::Duration,
) -> Result<Vec<u8>, String> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let name = tool.name();
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {name}: {e}"))?;
    if let (Some(mut stdin), Some(text)) = (child.stdin.take(), input) {
        let text = text.to_owned();
        // Dropping `stdin` at the end closes it, which ends the helper's read.
        std::thread::spawn(move || {
            let _ = stdin.write_all(text.as_bytes());
        });
    }
    let stdout = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let _ = pipe.read_to_end(&mut out);
            out
        })
    });
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return Ok(stdout.and_then(|t| t.join().ok()).unwrap_or_default());
            }
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return Err(format!("{name} exited with {status}: {}", stderr.trim()));
            }
            Ok(None) if started.elapsed() > timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "{name} did not answer within {} ms",
                    timeout.as_millis()
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(2)),
            Err(e) => return Err(format!("waiting for {name}: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    fn present(files: &'static [&'static str]) -> impl Fn(&Path) -> bool {
        move |p| files.iter().any(|f| p == Path::new(f))
    }

    #[test]
    fn win32yank_is_preferred_over_powershell() {
        // Joined, not spelled `/a:/b`: Windows separates `PATH` with `;`.
        let path = std::env::join_paths(["/a", "/b"]).unwrap();
        let tool = WslTool::find(
            Some(&path),
            present(&["/a/powershell.exe", "/b/win32yank.exe"]),
        );
        assert_eq!(
            tool,
            Some(WslTool::Win32Yank(PathBuf::from("/b/win32yank.exe")))
        );
    }

    #[test]
    fn powershell_on_path_is_the_second_choice() {
        let tool = WslTool::find(Some(OsStr::new("/a")), present(&["/a/powershell.exe"]));
        assert_eq!(
            tool,
            Some(WslTool::PowerShell(PathBuf::from("/a/powershell.exe")))
        );
    }

    #[test]
    fn powershell_is_found_where_windows_installs_it_when_path_has_no_windows_dirs() {
        let tool = WslTool::find(
            Some(OsStr::new("/usr/bin")),
            present(&[POWERSHELL_FALLBACK]),
        );
        assert_eq!(
            tool,
            Some(WslTool::PowerShell(PathBuf::from(POWERSHELL_FALLBACK)))
        );
    }

    #[test]
    fn clip_exe_is_never_chosen() {
        // clip.exe garbles anything outside ASCII; with no other helper the
        // backend is not used at all.
        let tool = WslTool::find(Some(OsStr::new("/w")), present(&["/w/clip.exe"]));
        assert_eq!(tool, None);
    }

    #[test]
    fn a_missing_helper_is_an_error_not_a_panic() {
        let tool = WslTool::PowerShell(PathBuf::from("/nonexistent/powershell.exe"));
        assert!(run_write(&tool, "x").is_err());
    }

    #[test]
    fn a_missing_helper_fails_the_read_rather_than_reading_as_empty() {
        let tool = WslTool::Win32Yank(PathBuf::from("/nonexistent/win32yank.exe"));
        let cb = WslClipboard::with_tool(tool).unwrap();
        assert!(matches!(cb.try_read(), ClipboardRead::Failed(_)));
    }

    /// A stand-in for `win32yank.exe` in a fresh directory: a shell script
    /// with `body` that is only ever given `-i --crlf` (write) or `-o --lf`
    /// (read), and fails on anything else. `$CLIP` in `body` is a file in the
    /// same directory. Never the real Windows clipboard (hermeticity,
    /// `clipboard.md` §4).
    #[cfg(unix)]
    fn stand_in(name: &str, body: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "lattice-cw-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("clipboard");
        let helper = dir.join("win32yank.exe");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\nCLIP='{}'\ncase \"$1 $2\" in\n\"-i --crlf\"|\"-o --lf\") ;;\n*) exit 3 ;;\nesac\n{body}\n",
                clip.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        (helper, dir)
    }

    #[cfg(unix)]
    fn wait_for(path: &Path, done: impl Fn(&str) -> bool) -> String {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let got = std::fs::read_to_string(path).unwrap_or_default();
            if done(&got) || std::time::Instant::now() > deadline {
                return got;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The write path end to end: the stand-in appends each write and a `|`.
    #[cfg(unix)]
    #[test]
    fn writes_reach_the_helper_in_order() {
        let (helper, dir) = stand_in(
            "write",
            r#"[ "$1" = "-i" ] && { cat >> "$CLIP"; printf '|' >> "$CLIP"; }"#,
        );
        let out = dir.join("clipboard");
        let cb = WslClipboard::with_tool(WslTool::Win32Yank(helper)).unwrap();
        cb.write("héllo\nwörld".into());
        let wrote = wait_for(&out, |got| got.ends_with('|'));
        cb.write("second".into());
        let wrote_both = wait_for(&out, |got| got.matches('|').count() >= 2);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            wrote, "héllo\nwörld|",
            "the text reaches the helper unchanged"
        );
        assert_eq!(wrote_both, "héllo\nwörld|second|", "and in order");
    }

    /// CW.2: a read runs the helper and returns what Windows holds, with
    /// Windows line ends made Unix ones.
    #[cfg(unix)]
    #[test]
    fn a_read_returns_the_windows_clipboard() {
        let (helper, dir) = stand_in("read", r#"printf 'fr\303\266m\r\nwindows'"#);
        let cb = WslClipboard::with_tool(WslTool::Win32Yank(helper)).unwrap();
        let read = cb.try_read();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read, ClipboardRead::Text("fröm\nwindows".into()));
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_windows_clipboard_reads_as_no_text() {
        let (helper, dir) = stand_in("empty", "true");
        let cb = WslClipboard::with_tool(WslTool::Win32Yank(helper)).unwrap();
        let read = cb.try_read();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read, ClipboardRead::NoText, "so paste uses the register");
    }

    /// CW.2: `yy` then `p` before the copy has reached Windows pastes the
    /// yank, not the text it is replacing. The stand-in's write takes a
    /// second; its read would say "old".
    #[cfg(unix)]
    #[test]
    fn a_read_while_our_write_is_on_its_way_returns_that_write() {
        let (helper, dir) = stand_in(
            "pending",
            r#"if [ "$1" = "-i" ]; then sleep 1; cat > "$CLIP"; else printf old; fi"#,
        );
        let cb = WslClipboard::with_tool(WslTool::Win32Yank(helper)).unwrap();
        cb.write("mine".into());
        let during = cb.try_read();
        let landed = wait_for(&dir.join("clipboard"), |got| got == "mine");
        // Confirmed: the next read asks Windows again.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let after = loop {
            let read = cb.try_read();
            if read != ClipboardRead::Text("mine".into()) || std::time::Instant::now() > deadline {
                break read;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(during, ClipboardRead::Text("mine".into()));
        assert_eq!(landed, "mine");
        assert_eq!(after, ClipboardRead::Text("old".into()));
    }

    /// A helper that hangs fails the read once the bound passes; it neither
    /// blocks the paste for longer nor reads as an empty clipboard.
    #[cfg(unix)]
    #[test]
    fn a_hung_helper_fails_the_read_within_the_bound() {
        let (helper, dir) = stand_in("hung", "sleep 30");
        let cb = WslClipboard::with_tool(WslTool::Win32Yank(helper)).unwrap();
        let started = std::time::Instant::now();
        let read = cb.try_read();
        let took = started.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            matches!(&read, ClipboardRead::Failed(why) if why.contains("did not answer")),
            "got {read:?}"
        );
        assert!(
            took < WSL_READ_TIMEOUT + std::time::Duration::from_secs(2),
            "took {took:?}"
        );
    }
}
