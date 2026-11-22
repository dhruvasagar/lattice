//! The clipboard backends **shared by both renderer peers**
//! (`docs/dev/architecture/clipboard.md` §4):
//!
//! - [`ArboardClipboard`] (CB.4, behind the `system-clipboard` feature): the
//!   native OS clipboard via `arboard`.
//! - [`WslClipboard`] (CW.1, always compiled): the *Windows* clipboard,
//!   reached from inside WSL through a Windows helper (`win32yank.exe`, else
//!   `powershell.exe`). It needs nothing but `std::process`, so it is not
//!   feature-gated: the Linux TUI release build, which has no `arboard`,
//!   gets it too.
//!
//! The [`lattice_core::Clipboard`] trait requires `Send + Sync` and its
//! `read` is called synchronously on the editor actor thread (from
//! `Editor::read_register`). `arboard` satisfies `Send + Sync` and reads
//! synchronously, so a single backend serves both the TUI and GUI peers —
//! this module is the shared home so the load-bearing bounded-read logic
//! (paramount #1) isn't duplicated and can't drift between peers.
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

use lattice_core::Clipboard;

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

    /// Bounded wait so a hung display-server round-trip can't stall
    /// dispatch (paramount #1). `Editor::read_register`'s call sits on the
    /// blocking render→actor RPC (`input-pipeline.md`); this bound is what
    /// keeps the synchronous read contract honest for a real backend. 30ms
    /// is generous headroom over the common case (a local
    /// X11/Wayland/macOS clipboard round-trip is sub-5ms) while still
    /// bounding the worst case.
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(30);
}

#[cfg(feature = "system-clipboard")]
impl Clipboard for ArboardClipboard {
    fn read(&self) -> Option<String> {
        let clipboard = self.inner.clone();
        let fut = async move {
            tokio::time::timeout(
                Self::READ_TIMEOUT,
                tokio::task::spawn_blocking(move || clipboard.lock().ok()?.get_text().ok()),
            )
            .await
        };
        match lattice_runtime::block_on(fut) {
            Ok(Ok(text)) => text,
            // Timed out, the blocking task panicked, or the mutex was
            // poisoned -- any of these degrade to "no clipboard value",
            // never a panic on the hot path.
            _ => None,
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

/// The Windows helper a [`WslClipboard`] runs. `win32yank.exe` is preferred:
/// it is the fast one (≈40–150 ms through WSL interop, against ≈350 ms for
/// PowerShell's start-up). `clip.exe` is deliberately not a candidate: it
/// decodes its input in the console's code page, so anything outside ASCII
/// arrives garbled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WslTool {
    /// `win32yank.exe -i --crlf` (Neovim ships one; it is often on `PATH`).
    Win32Yank(std::path::PathBuf),
    /// `powershell.exe … Set-Clipboard`, which every Windows has.
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
            Self::Win32Yank(program) => {
                let mut cmd = std::process::Command::new(program);
                // `--crlf`: Windows applications expect CRLF line ends.
                cmd.args(["-i", "--crlf"]);
                cmd
            }
            Self::PowerShell(program) => {
                let mut cmd = std::process::Command::new(program);
                cmd.args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    POWERSHELL_WRITE,
                ]);
                cmd
            }
        }
    }
}

/// The Windows clipboard from inside WSL (CW.1).
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
/// **Reads** go to `reader` when one is given (the GUI's and a
/// `system-clipboard` TUI's `arboard`, which reads the Windows clipboard
/// through WSLg), and are `None` otherwise: the register layer then uses its
/// in-memory entry. A helper read takes far longer than the 30 ms the
/// synchronous read allows, so reading through one waits for CW.2's
/// asynchronous paste.
pub struct WslClipboard {
    writes: std::sync::mpsc::Sender<String>,
    reader: Option<lattice_core::ClipboardHandle>,
}

/// A helper that has not finished after this long is killed, so a hung
/// Windows process cannot wedge every later write behind it.
const WSL_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
        tracing::debug!("clipboard: WSL; writing through {tool:?}");
        Self::with_tool(tool)
    }

    /// A backend writing through `tool`. `None` when the worker thread cannot
    /// be spawned (logged); the caller falls back to its usual backend.
    pub fn with_tool(tool: WslTool) -> Option<Self> {
        let (writes, queue) = std::sync::mpsc::channel::<String>();
        let spawned = std::thread::Builder::new()
            .name("clipboard-wsl".into())
            .spawn(move || {
                let mut warned = false;
                while let Ok(mut text) = queue.recv() {
                    while let Ok(newer) = queue.try_recv() {
                        text = newer;
                    }
                    if let Err(reason) = run_write(&tool, &text) {
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
                writes,
                reader: None,
            }),
            Err(e) => {
                tracing::warn!("clipboard: cannot start the WSL clipboard worker: {e}");
                None
            }
        }
    }

    /// Read through `reader` (see the type's doc).
    pub fn with_reader(mut self, reader: lattice_core::ClipboardHandle) -> Self {
        self.reader = Some(reader);
        self
    }
}

impl Clipboard for WslClipboard {
    fn read(&self) -> Option<String> {
        self.reader.as_ref().and_then(|r| r.read())
    }

    fn write(&self, text: String) {
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
    use std::io::Write;
    use std::process::Stdio;
    let mut child = tool
        .write_command()
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {tool:?}: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("cannot write to {tool:?}: {e}"))?;
        // Dropping `stdin` closes it, which is what ends the helper's read.
    }
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let mut stderr = String::new();
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read;
                    let _ = pipe.read_to_string(&mut stderr);
                }
                return Err(format!("{tool:?} exited with {status}: {}", stderr.trim()));
            }
            Ok(None) if started.elapsed() > WSL_WRITE_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{tool:?} did not finish in {WSL_WRITE_TIMEOUT:?}"));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(5)),
            Err(e) => return Err(format!("waiting for {tool:?}: {e}")),
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
        let tool = WslTool::find(
            Some(OsStr::new("/a:/b")),
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
    fn without_a_reader_a_read_is_none() {
        let tool = WslTool::Win32Yank(PathBuf::from("/nonexistent/win32yank.exe"));
        let cb = WslClipboard::with_tool(tool).unwrap();
        assert_eq!(cb.read(), None);
    }

    #[test]
    fn reads_go_to_the_reader() {
        let tool = WslTool::Win32Yank(PathBuf::from("/nonexistent/win32yank.exe"));
        let reader = std::sync::Arc::new(lattice_core::FakeClipboard::new());
        reader.write("from windows".into());
        let cb = WslClipboard::with_tool(tool).unwrap().with_reader(reader);
        assert_eq!(cb.read().as_deref(), Some("from windows"));
    }

    #[test]
    fn a_missing_helper_is_an_error_not_a_panic() {
        let tool = WslTool::PowerShell(PathBuf::from("/nonexistent/powershell.exe"));
        assert!(run_write(&tool, "x").is_err());
    }

    /// The write path end to end, against a stand-in helper: a shell script
    /// called `win32yank.exe` that appends its input to a file. Never the
    /// real Windows clipboard (hermeticity, `clipboard.md` §4).
    #[cfg(unix)]
    #[test]
    fn writes_reach_the_helper_in_order() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "lattice-cw1-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("clipboard");
        let helper = dir.join("win32yank.exe");
        std::fs::write(
            &helper,
            format!(
                "#!/bin/sh\n[ \"$1 $2\" = \"-i --crlf\" ] || exit 3\ncat >> '{}'\nprintf '|' >> '{}'\n",
                out.display(),
                out.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();

        let cb = WslClipboard::with_tool(WslTool::Win32Yank(helper)).unwrap();
        cb.write("héllo\nwörld".into());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let wrote = loop {
            let got = std::fs::read_to_string(&out).unwrap_or_default();
            if got.ends_with('|') || std::time::Instant::now() > deadline {
                break got;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        cb.write("second".into());
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let wrote_both = loop {
            let got = std::fs::read_to_string(&out).unwrap_or_default();
            if got.matches('|').count() >= 2 || std::time::Instant::now() > deadline {
                break got;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            wrote, "héllo\nwörld|",
            "the text reaches the helper unchanged"
        );
        assert_eq!(wrote_both, "héllo\nwörld|second|", "and in order");
    }
}
