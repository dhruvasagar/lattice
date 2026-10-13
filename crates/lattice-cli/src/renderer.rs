//! GD.9: which renderer a bare `lattice` opens.
//!
//! The window, unless something says otherwise. Three things can:
//!
//! - `--tui` asks for the terminal, and gets it.
//! - This binary may have been built without the `gui` feature, in
//!   which case the terminal is all it has.
//! - There may be no display to open a window on — an SSH session, a
//!   server, a Linux console. Then the terminal is the editor that can
//!   actually start, so that is what opens, without a flag. `--gui`
//!   still means "a window or an error".
//!
//! A pure function of its inputs so every row of that table is a test;
//! `main` supplies the real environment.

/// The two renderer peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renderer {
    Gui,
    Tui,
}

/// What `main` knows when it has to choose.
#[derive(Debug, Clone, Copy)]
pub struct Launch {
    /// `--gui` was passed.
    pub gui_flag: bool,
    /// `--tui` was passed.
    pub tui_flag: bool,
    /// The binary was built with the `gui` feature.
    pub gui_compiled: bool,
    /// Running from inside a macOS `.app` bundle: launched by the
    /// window system, with no terminal to fall back to.
    pub in_app_bundle: bool,
    /// A window could be opened here; see [`display_reachable`].
    pub display_reachable: bool,
}

/// The renderer for this launch.
///
/// `--gui` answers `Gui` even when this binary has none compiled in or
/// no display is reachable: the caller reports that as an error, which
/// is what an explicit request for a window should get.
pub fn choose(launch: Launch) -> Renderer {
    if launch.tui_flag {
        return Renderer::Tui;
    }
    if launch.gui_flag {
        return Renderer::Gui;
    }
    if !launch.gui_compiled {
        return Renderer::Tui;
    }
    if launch.in_app_bundle || launch.display_reachable {
        Renderer::Gui
    } else {
        Renderer::Tui
    }
}

/// The operating-system families that differ in how a display is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS and Windows: the window system is always there for a local
    /// session, and never there for a remote shell.
    Desktop,
    /// Linux and the BSDs: a display is named by an environment
    /// variable, or there is none.
    Unix,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        if cfg!(any(target_os = "macos", target_os = "windows")) {
            Platform::Desktop
        } else {
            Platform::Unix
        }
    }
}

/// Whether a window could be opened, judged from the environment.
///
/// `env` looks a variable up; a variable set to the empty string counts
/// as unset, which is how `DISPLAY=` is used to switch X off for one
/// command.
///
/// - **Unix:** `WAYLAND_DISPLAY` or `DISPLAY` names a display. That
///   includes an SSH session with X forwarding, which is a display the
///   user asked for.
/// - **Desktop:** yes, unless this is an SSH session (`SSH_CONNECTION`
///   or `SSH_TTY`). A remote shell on a Mac has no window server to
///   talk to, and a window opening on a screen nobody is sitting at is
///   not an editor.
///
/// This is a guess made before anything is opened, and it errs toward
/// the terminal: a wrong "no" costs a `--gui`, a wrong "yes" costs an
/// editor that fails to start.
pub fn display_reachable(platform: Platform, env: impl Fn(&str) -> Option<String>) -> bool {
    let set = |name: &str| env(name).is_some_and(|v| !v.is_empty());
    match platform {
        Platform::Unix => set("WAYLAND_DISPLAY") || set("DISPLAY"),
        Platform::Desktop => !(set("SSH_CONNECTION") || set("SSH_TTY")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch() -> Launch {
        Launch {
            gui_flag: false,
            tui_flag: false,
            gui_compiled: true,
            in_app_bundle: false,
            display_reachable: true,
        }
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn a_bare_launch_opens_the_window() {
        assert_eq!(choose(launch()), Renderer::Gui);
    }

    #[test]
    fn tui_flag_opens_the_terminal_even_with_a_display() {
        assert_eq!(
            choose(Launch {
                tui_flag: true,
                ..launch()
            }),
            Renderer::Tui
        );
    }

    /// The fallback: no flag, no display, so the editor that can start.
    #[test]
    fn with_no_display_a_bare_launch_opens_the_terminal() {
        assert_eq!(
            choose(Launch {
                display_reachable: false,
                ..launch()
            }),
            Renderer::Tui
        );
    }

    /// `--gui` is a request, not a preference: it is not quietly
    /// downgraded. The caller turns an impossible one into an error.
    #[test]
    fn gui_flag_is_never_downgraded() {
        assert_eq!(
            choose(Launch {
                gui_flag: true,
                display_reachable: false,
                ..launch()
            }),
            Renderer::Gui
        );
        assert_eq!(
            choose(Launch {
                gui_flag: true,
                gui_compiled: false,
                ..launch()
            }),
            Renderer::Gui
        );
    }

    /// A terminal-only build stays a working terminal editor.
    #[test]
    fn a_build_without_the_gui_opens_the_terminal() {
        assert_eq!(
            choose(Launch {
                gui_compiled: false,
                ..launch()
            }),
            Renderer::Tui
        );
    }

    /// Double-clicked from Finder there is no terminal to fall back to,
    /// whatever the environment looks like.
    #[test]
    fn an_app_bundle_opens_the_window_regardless_of_the_environment() {
        assert_eq!(
            choose(Launch {
                in_app_bundle: true,
                display_reachable: false,
                ..launch()
            }),
            Renderer::Gui
        );
    }

    #[test]
    fn unix_needs_a_named_display() {
        assert!(!display_reachable(Platform::Unix, env_of(&[])));
        assert!(display_reachable(
            Platform::Unix,
            env_of(&[("DISPLAY", ":0")])
        ));
        assert!(display_reachable(
            Platform::Unix,
            env_of(&[("WAYLAND_DISPLAY", "wayland-0")])
        ));
        // `DISPLAY=` switches X off for one command.
        assert!(!display_reachable(
            Platform::Unix,
            env_of(&[("DISPLAY", "")])
        ));
        // SSH with X forwarding is a display the user asked for.
        assert!(display_reachable(
            Platform::Unix,
            env_of(&[("SSH_CONNECTION", "1 2 3 4"), ("DISPLAY", "localhost:10.0")])
        ));
        // SSH without it is not.
        assert!(!display_reachable(
            Platform::Unix,
            env_of(&[("SSH_CONNECTION", "1 2 3 4")])
        ));
    }

    #[test]
    fn a_desktop_has_a_display_unless_this_is_a_remote_shell() {
        assert!(display_reachable(Platform::Desktop, env_of(&[])));
        assert!(!display_reachable(
            Platform::Desktop,
            env_of(&[("SSH_CONNECTION", "1 2 3 4")])
        ));
        assert!(!display_reachable(
            Platform::Desktop,
            env_of(&[("SSH_TTY", "/dev/ttys004")])
        ));
        // A stray DISPLAY (XQuartz) does not make an SSH session local.
        assert!(!display_reachable(
            Platform::Desktop,
            env_of(&[("SSH_TTY", "/dev/ttys004"), ("DISPLAY", ":0")])
        ));
    }
}
