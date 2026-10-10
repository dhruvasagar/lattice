//! `*lsp-servers*` — every server lighthouse knows about, and what state it
//! is in.
//!
//! One row per server, in a plugin output buffer, redrawn whenever anything
//! changes: an install starting, a job finishing, an uninstall. The buffer is
//! the manager surface — the row under the cursor is what `i` / `u` / `x`
//! act on — so a row has to be readable back: [`server_on_line`] is the
//! inverse of [`render`], and a test holds the two together.
//!
//! What a row says is worked out here, from the registry, the installed
//! records and what is in flight, with nothing cached: the list cannot
//! disagree with the state it is drawn from.

use crate::install::{self, Host, Installed, Installer, Phase};
use crate::registry::Registry;

/// The buffer the list is drawn into.
pub const BUFFER: &str = "*lsp-servers*";

/// What one server's row reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// In the registry, with a build for this machine, not installed.
    NotInstalled,
    /// In the registry, but with no build for this machine.
    Unavailable,
    /// An install is in flight.
    Installing,
    /// Installed at the registry's pin.
    Installed,
    /// Installed at `installed`; the registry pins something else.
    UpdateAvailable { installed: String },
    /// Recorded as installed, but the binary is not on disk.
    Missing,
    /// Installed, but the registry no longer lists it — so it cannot be
    /// registered with the editor or updated, only removed.
    Unlisted,
}

/// One row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub name: String,
    /// The version the row is about: the registry's pin, or for a server the
    /// registry no longer lists, what is installed.
    pub version: String,
    pub state: State,
}

impl Row {
    fn status(&self, platform: &str) -> String {
        match &self.state {
            State::NotInstalled => "not installed".to_string(),
            State::Unavailable => format!("no build for {platform}"),
            State::Installing => "installing\u{2026}".to_string(),
            State::Installed => "installed".to_string(),
            State::UpdateAvailable { installed } => {
                format!("installed {installed} \u{2014} update available")
            }
            State::Missing => "installed, but its files are missing \u{2014} reinstall".to_string(),
            State::Unlisted => "installed, no longer in the registry".to_string(),
        }
    }

    fn is_installed(&self) -> bool {
        matches!(
            self.state,
            State::Installed | State::UpdateAvailable { .. } | State::Missing | State::Unlisted
        )
    }
}

/// Every row, sorted by server name.
///
/// `installed` is each installed record by server name; `installing` and
/// `exists` are asked rather than passed as sets so the caller's own state
/// stays the single copy of the truth.
pub fn rows(
    registry: &Registry,
    platform: &str,
    installed: &[(String, Installed)],
    installing: impl Fn(&str) -> bool,
    exists: impl Fn(&str) -> bool,
) -> Vec<Row> {
    let record = |name: &str| installed.iter().find(|(n, _)| n == name).map(|(_, r)| r);
    let mut rows: Vec<Row> = registry
        .names()
        .into_iter()
        .filter_map(|name| registry.get(name))
        .map(|server| {
            let name = &server.name;
            let state = if installing(name) {
                State::Installing
            } else {
                match record(name) {
                    Some(r) if !exists(&r.binary_path(name)) => State::Missing,
                    Some(r) if r.version == server.version => State::Installed,
                    Some(r) => State::UpdateAvailable {
                        installed: r.version.clone(),
                    },
                    None if server.platform.contains_key(platform) => State::NotInstalled,
                    None => State::Unavailable,
                }
            };
            Row {
                name: name.clone(),
                // The pin is what an install or update would produce, and
                // for an up-to-date server it is also what is there.
                version: server.version.clone(),
                state,
            }
        })
        .collect();
    for (name, r) in installed {
        if registry.get(name).is_none() {
            rows.push(Row {
                name: name.clone(),
                version: r.version.clone(),
                state: State::Unlisted,
            });
        }
    }
    rows.sort_by(|a, b| a.name.cmp(&b.name));
    rows
}

const HEADINGS: [&str; 3] = ["Server", "Version", "Status"];

/// What the keys do — the last line of the buffer.
pub const KEYS: &str = "i install   u update   x uninstall   <CR> show log   gr refresh";

/// The widest of a column's cells and its heading.
fn column_width<'a>(cells: impl Iterator<Item = &'a str>, heading: &'a str) -> usize {
    cells
        .chain([heading])
        .map(|cell| cell.chars().count())
        .max()
        .unwrap_or(0)
}

/// The buffer's lines: a heading, a row per server in aligned columns, the
/// keys — and, when the user's own registry file could not be used, why.
///
/// That last line is the only place such a file's mistake is guaranteed to
/// be seen. It is skipped whole when it is wrong, so the servers it meant to
/// add are simply absent and the ones it meant to replace are silently the
/// bundled ones; without this the list would look complete and be wrong.
pub fn render(rows: &[Row], platform: &str, problem: Option<&str>) -> Vec<String> {
    let name_w = column_width(rows.iter().map(|r| r.name.as_str()), HEADINGS[0]);
    let version_w = column_width(rows.iter().map(|r| r.version.as_str()), HEADINGS[1]);
    let line = |name: &str, version: &str, status: &str| {
        format!("  {name:<name_w$}  {version:<version_w$}  {status}")
            .trim_end()
            .to_string()
    };
    let mut lines = vec![line(HEADINGS[0], HEADINGS[1], HEADINGS[2])];
    lines.extend(
        rows.iter()
            .map(|r| line(&r.name, &r.version, &r.status(platform))),
    );
    if rows.is_empty() {
        lines.push("  (the registry is empty)".to_string());
    }
    lines.push(String::new());
    lines.push(KEYS.to_string());
    if let Some(problem) = problem {
        lines.push(String::new());
        // Not indented, so it can never read back as a server's row.
        lines.push(format!("! ignored: {problem}"));
    }
    lines
}

/// `3 servers · 1 installed · 1 installing`.
pub fn summary(rows: &[Row]) -> String {
    let count = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut parts = vec![count(rows.len(), "server", "servers")];
    parts.push(format!(
        "{} installed",
        rows.iter().filter(|r| r.is_installed()).count()
    ));
    let installing = rows.iter().filter(|r| r.state == State::Installing).count();
    if installing > 0 {
        parts.push(format!("{installing} installing"));
    }
    let updates = rows
        .iter()
        .filter(|r| matches!(r.state, State::UpdateAvailable { .. }))
        .count();
    if updates > 0 {
        parts.push(count(updates, "update available", "updates available"));
    }
    parts.join(" \u{b7} ")
}

/// The server a line of the buffer is about: the inverse of [`render`].
///
/// A row is indented by two spaces and starts with the server's name; the
/// heading has the same shape and is told apart by its text, and the keys
/// line and blank lines are not indented.
pub fn server_on_line(line: &str) -> Option<&str> {
    let row = line.strip_prefix("  ")?;
    let name = row.split_whitespace().next()?;
    (name != HEADINGS[0] && !name.starts_with('(')).then_some(name)
}

/// Every installed record, by server name.
pub fn installed_records(host: &impl Host) -> Vec<(String, Installed)> {
    host.keys(install::INSTALLED_PREFIX)
        .iter()
        .filter_map(|key| key.strip_prefix(install::INSTALLED_PREFIX))
        .filter_map(|name| install::installed(host, name).map(|r| (name.to_string(), r)))
        .collect()
}

/// Redraw `*lsp-servers*` from the current state.
pub fn show(
    host: &mut impl Host,
    registry: &Registry,
    problem: Option<&str>,
    platform: &str,
    installer: &Installer,
) {
    let installed = installed_records(host);
    let rows = rows(
        registry,
        platform,
        &installed,
        |name| installer.is_installing(name),
        |path| host.exists(path),
    );
    let phase = if rows.iter().any(|r| r.state == State::Installing) {
        Phase::Running
    } else {
        Phase::Succeeded
    };
    host.reset(BUFFER);
    for line in render(&rows, platform, problem) {
        host.say(BUFFER, &line);
    }
    host.status(BUFFER, phase, &summary(&rows));
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORM: &str = "linux-x86_64";

    const EXTRA: &str = r#"
[[server]]
name = "zls"
lsp-id = "zig"
language-id = "zig"
version = "0.13.0"
file-patterns = ["*.zig"]
[server.platform.linux-x86_64]
url = "https://example.org/zls.tar.gz"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
archive = "tar-gz"
binary = "bin/zls"

[[server]]
name = "mac-only"
lsp-id = "m"
language-id = "m"
version = "1"
file-patterns = ["*.m"]
[server.platform.macos-aarch64]
url = "https://example.org/m.gz"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
archive = "gz"
binary = "m"
"#;

    fn registry() -> Registry {
        let (registry, problem) = Registry::load(Some(EXTRA));
        assert_eq!(problem, None);
        registry
    }

    fn record(version: &str, binary: &str) -> Installed {
        Installed {
            version: version.into(),
            binary: binary.into(),
        }
    }

    fn state_of<'a>(rows: &'a [Row], name: &str) -> &'a State {
        &rows.iter().find(|r| r.name == name).unwrap().state
    }

    #[test]
    fn nothing_installed_lists_the_registry_and_says_what_cannot_run_here() {
        let rows = rows(&registry(), PLATFORM, &[], |_| false, |_| true);
        assert_eq!(
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["mac-only", "rust-analyzer", "zls"],
            "sorted by name"
        );
        assert_eq!(state_of(&rows, "zls"), &State::NotInstalled);
        assert_eq!(state_of(&rows, "mac-only"), &State::Unavailable);
        assert_eq!(summary(&rows), "3 servers \u{b7} 0 installed");
    }

    #[test]
    fn each_installed_state_is_told_apart() {
        let installed = vec![
            ("zls".to_string(), record("0.13.0", "bin/zls")),
            (
                "rust-analyzer".to_string(),
                record("2020-01-01", "rust-analyzer"),
            ),
            ("gone".to_string(), record("3", "gone")),
        ];
        let rows = rows(&registry(), PLATFORM, &installed, |_| false, |_| true);
        assert_eq!(state_of(&rows, "zls"), &State::Installed);
        assert_eq!(
            state_of(&rows, "rust-analyzer"),
            &State::UpdateAvailable {
                installed: "2020-01-01".into()
            }
        );
        // Installed, and the registry no longer lists it.
        assert_eq!(state_of(&rows, "gone"), &State::Unlisted);
        assert_eq!(
            summary(&rows),
            "4 servers \u{b7} 3 installed \u{b7} 1 update available"
        );
    }

    #[test]
    fn a_record_whose_binary_is_not_on_disk_is_reported_as_missing() {
        let installed = vec![("zls".to_string(), record("0.13.0", "bin/zls"))];
        let rows = rows(
            &registry(),
            PLATFORM,
            &installed,
            |_| false,
            |path| {
                assert_eq!(path, "lsp/zls/0.13.0/bin/zls", "it asks about the binary");
                false
            },
        );
        assert_eq!(state_of(&rows, "zls"), &State::Missing);
    }

    /// An install in flight wins over whatever is on disk: during an update
    /// the old version is still installed, and "installed" would hide that
    /// anything is happening.
    #[test]
    fn installing_wins_over_what_is_installed() {
        let installed = vec![("zls".to_string(), record("0.12.0", "bin/zls"))];
        let rows = rows(
            &registry(),
            PLATFORM,
            &installed,
            |name| name == "zls",
            |_| true,
        );
        assert_eq!(state_of(&rows, "zls"), &State::Installing);
        assert_eq!(
            summary(&rows),
            "3 servers \u{b7} 0 installed \u{b7} 1 installing"
        );
    }

    #[test]
    fn the_columns_line_up_and_the_keys_are_the_last_line() {
        let installed = vec![("zls".to_string(), record("0.13.0", "bin/zls"))];
        let rows = rows(&registry(), PLATFORM, &installed, |_| false, |_| true);
        let lines = render(&rows, PLATFORM, None);
        assert_eq!(
            lines,
            vec![
                "  Server         Version     Status",
                "  mac-only       1           no build for linux-x86_64",
                "  rust-analyzer  2026-10-05  not installed",
                "  zls            0.13.0      installed",
                "",
                KEYS,
            ]
        );
        assert!(
            lines.iter().all(|l| l == l.trim_end()),
            "no trailing whitespace"
        );
    }

    /// The row under the cursor is what the keys act on, so every row must
    /// read back as its server — and nothing else must read back as one.
    #[test]
    fn every_row_reads_back_as_its_server_and_no_other_line_does() {
        let installed = vec![
            ("zls".to_string(), record("0.1", "bin/zls")),
            ("gone".to_string(), record("3", "gone")),
        ];
        let rows = rows(
            &registry(),
            PLATFORM,
            &installed,
            |name| name == "mac-only",
            |_| true,
        );
        let lines = render(&rows, PLATFORM, None);
        let read_back: Vec<&str> = lines.iter().filter_map(|l| server_on_line(l)).collect();
        assert_eq!(
            read_back,
            rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>()
        );

        let empty = render(&[], PLATFORM, None);
        assert!(
            empty.iter().all(|l| server_on_line(l).is_none()),
            "{empty:?}"
        );
        assert!(empty.iter().any(|l| l.contains("registry is empty")));
    }

    #[test]
    fn a_registry_file_that_was_ignored_is_reported_under_the_list() {
        let (registry, problem) = Registry::load(Some("[[server]]\nname = \"../x\"\n"));
        let problem = problem.expect("the overlay is rejected");
        let rows = rows(&registry, PLATFORM, &[], |_| false, |_| true);
        let lines = render(&rows, PLATFORM, Some(&problem));
        let last = lines.last().unwrap();
        assert!(
            last.starts_with("! ignored: registry.toml in the data directory"),
            "{last}"
        );
        assert_eq!(
            lines.iter().filter_map(|l| server_on_line(l)).count(),
            rows.len(),
            "the note is not mistaken for a server"
        );
    }

    #[test]
    fn a_singular_count_reads_as_one() {
        let (only, _) = Registry::load(None);
        let rows = rows(&only, PLATFORM, &[], |_| false, |_| true);
        assert_eq!(summary(&rows), "1 server \u{b7} 0 installed");
    }
}
