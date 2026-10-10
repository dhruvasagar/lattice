//! The install state machine.
//!
//! An install is two host jobs and a few file operations between them:
//!
//! ```text
//! request ─► download ──ok──► extract ──ok──► mark executable ─► move into place ─► record
//!               │                │                  │                  │
//!               └──── err ───────┴──────────────────┴──────────────────┴─► clean up, report
//! ```
//!
//! Each job ends in exactly one `job-finished` event, and [`Installer`] is
//! stepped from it. Nothing here waits.
//!
//! ## Everything goes through [`Host`]
//!
//! The machine never calls the editor directly. It asks a `Host` to download,
//! unpack, rename, remember and report, which is what lets the whole thing —
//! including every failure branch, where the interesting behaviour is — run
//! under `cargo test` with a fake that records what it was asked. The real
//! `Host` is a thin adapter in `lib.rs`.
//!
//! ## Paths are relative to the data directory
//!
//! The same file has two names: the host-side seams know it by its real path,
//! and the guest's own filesystem calls know it under `/data`. The machine
//! deals only in the part they share, and the adapter prefixes each side.
//!
//! ## The layout, and why a failed install leaves nothing
//!
//! ```text
//! lsp/<name>/<version>/            an installed server — complete, or absent
//! lsp/<name>/<version>.partial/    being unpacked
//! lsp/<name>/<version>.download    being downloaded
//! ```
//!
//! Work happens in the two scratch names and the tree is moved into place by
//! one rename at the very end. So `lsp/<name>/<version>/` existing means the
//! install finished; there is no half-installed state to detect. A failure
//! removes the scratch names, and [`sweep`] removes any an editor exit left
//! behind.

use crate::registry::{Archive, Server};

/// Store-key prefix of the installed-server records.
pub const INSTALLED_PREFIX: &str = "installed/";

/// The output buffer an install of `server` reports into.
pub fn buffer_name(server: &str) -> String {
    format!("*lsp-install:{server}*")
}

/// What the output buffer's headerline says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Running,
    Succeeded,
    Failed,
}

/// Everything the machine needs done for it. Paths are relative to the
/// plugin's data directory.
pub trait Host {
    /// Start a download; `Ok(id)` once it is under way.
    fn download(&mut self, url: &str, sha256: &str, dest: &str) -> Result<u64, String>;
    /// Start unpacking `src` to `dest`; `Ok(id)` once it is under way.
    fn extract(&mut self, src: &str, dest: &str, archive: Archive) -> Result<u64, String>;
    fn set_executable(&mut self, path: &str) -> Result<(), String>;
    /// Remove a file. Absent is fine.
    fn remove_file(&mut self, path: &str);
    /// Remove a directory and everything in it. Absent is fine.
    fn remove_tree(&mut self, path: &str);
    fn rename(&mut self, from: &str, to: &str) -> Result<(), String>;
    /// The names in a directory. Absent is empty.
    fn list(&self, dir: &str) -> Vec<String>;

    fn put(&mut self, key: &str, value: &str) -> Result<(), String>;
    fn get(&self, key: &str) -> Option<String>;

    /// Append one line to an output buffer.
    fn say(&mut self, buffer: &str, line: &str);
    /// Set an output buffer's headerline.
    fn status(&mut self, buffer: &str, phase: Phase, text: &str);
    /// Empty an output buffer.
    fn reset(&mut self, buffer: &str);
}

/// A server that is installed: what the editor needs to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    /// The executable, relative to the version's tree.
    pub binary: String,
}

impl Installed {
    pub fn encode(&self) -> String {
        format!("{}\n{}", self.version, self.binary)
    }

    pub fn decode(text: &str) -> Option<Self> {
        let mut lines = text.lines();
        let version = lines.next()?.to_string();
        let binary = lines.next()?.to_string();
        (!version.is_empty() && !binary.is_empty()).then_some(Self { version, binary })
    }
}

fn server_dir(name: &str) -> String {
    format!("lsp/{name}")
}

fn tree(name: &str, version: &str) -> String {
    format!("lsp/{name}/{version}")
}

fn partial(name: &str, version: &str) -> String {
    format!("lsp/{name}/{version}.partial")
}

fn download_file(name: &str, version: &str) -> String {
    format!("lsp/{name}/{version}.download")
}

/// Where a job is in the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Download,
    Extract,
}

/// One install in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Job {
    name: String,
    version: String,
    archive: Archive,
    binary: String,
    step: Step,
}

impl Job {
    fn buffer(&self) -> String {
        buffer_name(&self.name)
    }
}

/// The installs in flight, keyed by the id of the host job each is waiting on.
#[derive(Debug, Default)]
pub struct Installer {
    jobs: Vec<(u64, Job)>,
}

/// `12.3 MB`, to one decimal — a download's size is read at a glance, not
/// counted.
fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

/// Report a failure and remove everything the attempt created. An
/// already-installed tree is never touched: a failed reinstall or update
/// leaves the working server working.
fn fail(host: &mut impl Host, job: &Job, error: &str) {
    host.remove_file(&download_file(&job.name, &job.version));
    host.remove_tree(&partial(&job.name, &job.version));
    let buffer = job.buffer();
    host.say(&buffer, &format!("error: {error}"));
    host.status(
        &buffer,
        Phase::Failed,
        &format!("{} {}: install failed", job.name, job.version),
    );
}

/// The last steps, none of which is a job: mark the binary executable, move
/// the tree to its final name, and record it.
fn put_in_place(host: &mut impl Host, job: &Job) -> Result<(), String> {
    let staging = partial(&job.name, &job.version);
    let dest = tree(&job.name, &job.version);
    // Also the check that the archive held the binary the registry promised:
    // marking a file that is not there fails, by name.
    host.set_executable(&format!("{staging}/{}", job.binary))?;
    // A reinstall of the same version replaces it. The old tree goes only
    // now, when its replacement is complete and one rename away.
    host.remove_tree(&dest);
    host.rename(&staging, &dest)?;
    host.remove_file(&download_file(&job.name, &job.version));
    host.put(
        &format!("{INSTALLED_PREFIX}{}", job.name),
        &Installed {
            version: job.version.clone(),
            binary: job.binary.clone(),
        }
        .encode(),
    )
}

impl Installer {
    pub const fn new() -> Self {
        Self { jobs: Vec::new() }
    }

    /// Whether an install of `name` is in flight.
    pub fn is_installing(&self, name: &str) -> bool {
        self.jobs.iter().any(|(_, job)| job.name == name)
    }

    /// Begin installing `server` for `platform` (`<os>-<arch>`).
    ///
    /// Everything that happens — including every way this can fail to start —
    /// is written to the server's output buffer, so the caller has nothing to
    /// report and nothing to check.
    pub fn request(&mut self, host: &mut impl Host, server: &Server, platform: &str) {
        let name = &server.name;
        let buffer = buffer_name(name);
        if self.is_installing(name) {
            // The buffer is showing that install; leave it alone.
            host.say(&buffer, &format!("{name} is already being installed"));
            return;
        }
        host.reset(&buffer);
        let version = &server.version;
        let Some(build) = server.platform.get(platform) else {
            let available: Vec<&str> = server.platform.keys().map(String::as_str).collect();
            host.say(
                &buffer,
                &format!(
                    "{name} has no build for {platform} (available: {})",
                    available.join(", ")
                ),
            );
            host.status(
                &buffer,
                Phase::Failed,
                &format!("{name}: no build for {platform}"),
            );
            return;
        };
        // Say so when this replaces something: a reinstall and a first
        // install look the same from here on, and the difference matters to
        // someone wondering whether their server just changed under them.
        match installed(host, name) {
            Some(prev) if prev.version == *version => {
                host.say(
                    &buffer,
                    &format!("Reinstalling {name} {version} for {platform}"),
                );
            }
            Some(prev) => host.say(
                &buffer,
                &format!(
                    "Installing {name} {version} for {platform} (replacing {})",
                    prev.version
                ),
            ),
            None => host.say(
                &buffer,
                &format!("Installing {name} {version} for {platform}"),
            ),
        }
        host.say(&buffer, &format!("Downloading {}", build.url));
        host.status(
            &buffer,
            Phase::Running,
            &format!("{name} {version}: downloading\u{2026}"),
        );

        // Whatever an interrupted attempt left under these names is not
        // trusted: the tarball unpack refuses an existing destination, and a
        // stale download would be a file of unknown provenance.
        host.remove_file(&download_file(name, version));
        host.remove_tree(&partial(name, version));

        let job = Job {
            name: name.clone(),
            version: version.clone(),
            archive: build.archive,
            binary: build.binary.clone(),
            step: Step::Download,
        };
        match host.download(&build.url, &build.sha256, &download_file(name, version)) {
            Ok(id) => self.jobs.push((id, job)),
            Err(e) => fail(host, &job, &e),
        }
    }

    /// A download reported how far along it is.
    pub fn progress(&mut self, host: &mut impl Host, id: u64, done: u64, total: Option<u64>) {
        let Some((_, job)) = self.jobs.iter().find(|(job_id, _)| *job_id == id) else {
            return;
        };
        if job.step != Step::Download {
            return;
        }
        let amount = match total {
            Some(total) if total > 0 => format!(
                "{}% of {}",
                (done.saturating_mul(100) / total).min(100),
                megabytes(total)
            ),
            _ => megabytes(done),
        };
        host.status(
            &job.buffer(),
            Phase::Running,
            &format!("{} {}: downloading\u{2026} {amount}", job.name, job.version),
        );
    }

    /// A host job ended. Steps the install it belongs to; an id this
    /// installer is not waiting on is ignored.
    pub fn finished(&mut self, host: &mut impl Host, id: u64, outcome: Result<(), String>) {
        let Some(at) = self.jobs.iter().position(|(job_id, _)| *job_id == id) else {
            return;
        };
        let (_, mut job) = self.jobs.swap_remove(at);
        if let Err(e) = outcome {
            fail(host, &job, &e);
            return;
        }
        let buffer = job.buffer();
        match job.step {
            Step::Download => {
                host.say(&buffer, "Downloaded; SHA-256 verified");
                host.say(&buffer, "Unpacking");
                host.status(
                    &buffer,
                    Phase::Running,
                    &format!("{} {}: unpacking\u{2026}", job.name, job.version),
                );
                let staging = partial(&job.name, &job.version);
                // A `.gz` is the binary itself and unpacks to a file; a
                // tarball unpacks to a directory.
                let dest = match job.archive {
                    Archive::Gz => format!("{staging}/{}", job.binary),
                    Archive::TarGz => staging,
                };
                let src = download_file(&job.name, &job.version);
                match host.extract(&src, &dest, job.archive) {
                    Ok(next) => {
                        job.step = Step::Extract;
                        self.jobs.push((next, job));
                    }
                    Err(e) => fail(host, &job, &e),
                }
            }
            Step::Extract => match put_in_place(host, &job) {
                Ok(()) => {
                    host.say(&buffer, &format!("Installed {} {}", job.name, job.version));
                    host.status(
                        &buffer,
                        Phase::Succeeded,
                        &format!("{} {} installed", job.name, job.version),
                    );
                }
                Err(e) => fail(host, &job, &e),
            },
        }
    }
}

/// The installed record for `name`, if it has one.
pub fn installed(host: &impl Host, name: &str) -> Option<Installed> {
    host.get(&format!("{INSTALLED_PREFIX}{name}"))
        .and_then(|text| Installed::decode(&text))
}

/// Remove the scratch files of installs that never finished — run once when
/// the plugin starts, when by definition nothing is in flight.
///
/// A host job does not outlive the editor, so anything under a scratch name
/// at startup belongs to an install that was interrupted and will never be
/// resumed. Installed trees are not looked at.
pub fn sweep(host: &mut impl Host) {
    for server in host.list("lsp") {
        let dir = server_dir(&server);
        for entry in host.list(&dir) {
            let path = format!("{dir}/{entry}");
            if entry.ends_with(".partial") || entry.ends_with(".partial.part") {
                host.remove_tree(&path);
            } else if entry.ends_with(".download") || entry.ends_with(".download.part") {
                host.remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::Registry;
    use std::collections::{BTreeMap, BTreeSet};

    /// A host that does nothing and remembers everything.
    #[derive(Default)]
    struct Fake {
        next_id: u64,
        /// Paths that exist. A directory is implied by the paths under it.
        files: BTreeSet<String>,
        store: BTreeMap<String, String>,
        calls: Vec<String>,
        said: Vec<String>,
        statuses: Vec<(Phase, String)>,
        resets: usize,
        /// Make the named operation fail.
        refuse: Option<&'static str>,
    }

    impl Fake {
        fn refuses(&self, op: &str) -> Result<(), String> {
            match self.refuse {
                Some(refused) if refused == op => Err(format!("{op} refused")),
                _ => Ok(()),
            }
        }

        fn last_status(&self) -> &(Phase, String) {
            self.statuses.last().expect("a status was set")
        }

        fn has(&self, path: &str) -> bool {
            self.files.contains(path)
        }
    }

    impl Host for Fake {
        fn download(&mut self, url: &str, sha256: &str, dest: &str) -> Result<u64, String> {
            self.calls.push(format!("download {url} {sha256} {dest}"));
            self.refuses("download")?;
            self.next_id += 1;
            Ok(self.next_id)
        }
        fn extract(&mut self, src: &str, dest: &str, archive: Archive) -> Result<u64, String> {
            self.calls.push(format!("extract {src} {dest} {archive:?}"));
            self.refuses("extract")?;
            self.next_id += 1;
            Ok(self.next_id)
        }
        fn set_executable(&mut self, path: &str) -> Result<(), String> {
            self.calls.push(format!("chmod {path}"));
            self.refuses("chmod")?;
            if self.has(path) {
                Ok(())
            } else {
                Err(format!("set executable failed: '{path}': no such file"))
            }
        }
        fn remove_file(&mut self, path: &str) {
            self.files.remove(path);
        }
        fn remove_tree(&mut self, path: &str) {
            let under = format!("{path}/");
            self.files.retain(|f| f != path && !f.starts_with(&under));
        }
        fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
            self.calls.push(format!("rename {from} {to}"));
            self.refuses("rename")?;
            let under = format!("{from}/");
            let moved: Vec<String> = self
                .files
                .iter()
                .filter(|f| *f == from || f.starts_with(&under))
                .cloned()
                .collect();
            for old in moved {
                self.files.remove(&old);
                self.files.insert(old.replacen(from, to, 1));
            }
            Ok(())
        }
        fn list(&self, dir: &str) -> Vec<String> {
            let under = format!("{dir}/");
            let mut names = BTreeSet::new();
            for f in &self.files {
                if let Some(rest) = f.strip_prefix(&under) {
                    names.insert(rest.split('/').next().unwrap_or_default().to_string());
                }
            }
            names.into_iter().collect()
        }
        fn put(&mut self, key: &str, value: &str) -> Result<(), String> {
            self.refuses("put")?;
            self.store.insert(key.to_string(), value.to_string());
            Ok(())
        }
        fn get(&self, key: &str) -> Option<String> {
            self.store.get(key).cloned()
        }
        fn say(&mut self, _buffer: &str, line: &str) {
            self.said.push(line.to_string());
        }
        fn status(&mut self, _buffer: &str, phase: Phase, text: &str) {
            self.statuses.push((phase, text.to_string()));
        }
        fn reset(&mut self, _buffer: &str) {
            self.resets += 1;
            self.said.clear();
        }
    }

    const PLATFORM: &str = "linux-x86_64";

    fn rust_analyzer() -> Server {
        Registry::parse(crate::registry::BUNDLED)
            .unwrap()
            .get("rust-analyzer")
            .unwrap()
            .clone()
    }

    fn tarball_server() -> Server {
        Registry::parse(
            r#"
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
"#,
        )
        .unwrap()
        .get("zls")
        .unwrap()
        .clone()
    }

    fn setup(server: Server) -> (Installer, Fake, Server) {
        (Installer::new(), Fake::default(), server)
    }

    /// The download the last `request` started succeeds on disk. Returns the
    /// id of the extract job, or `None` if the unpack refused to start.
    fn finish_download(installer: &mut Installer, host: &mut Fake, server: &Server) -> Option<u64> {
        let download = host.next_id;
        host.files
            .insert(download_file(&server.name, &server.version));
        installer.finished(host, download, Ok(()));
        (host.next_id != download).then_some(host.next_id)
    }

    /// Request an install and carry it to a started unpack.
    fn through_download(installer: &mut Installer, host: &mut Fake, server: &Server) -> u64 {
        installer.request(host, server, PLATFORM);
        finish_download(installer, host, server).expect("the unpack started")
    }

    #[test]
    fn a_gz_server_installs_to_a_versioned_tree_and_is_recorded() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        let extract = through_download(&mut installer, &mut host, &server);
        let v = &server.version;
        assert_eq!(
            host.calls,
            vec![
                format!(
                    "download {} {} lsp/rust-analyzer/{v}.download",
                    server.platform[PLATFORM].url, server.platform[PLATFORM].sha256
                ),
                // A `.gz` unpacks to the binary itself, inside the staging dir.
                format!(
                    "extract lsp/rust-analyzer/{v}.download \
                     lsp/rust-analyzer/{v}.partial/rust-analyzer Gz"
                ),
            ]
        );
        assert!(installer.is_installing("rust-analyzer"));

        host.files
            .insert(format!("lsp/rust-analyzer/{v}.partial/rust-analyzer"));
        installer.finished(&mut host, extract, Ok(()));

        assert!(host.has(&format!("lsp/rust-analyzer/{v}/rust-analyzer")));
        assert!(
            !host.has(&format!("lsp/rust-analyzer/{v}.download")),
            "the archive is not kept"
        );
        let record = installed(&host, "rust-analyzer").expect("recorded");
        assert_eq!(
            record,
            Installed {
                version: v.clone(),
                binary: "rust-analyzer".into()
            }
        );
        assert_eq!(host.last_status().0, Phase::Succeeded);
        assert!(!installer.is_installing("rust-analyzer"));
    }

    #[test]
    fn a_tarball_unpacks_to_the_staging_directory_itself() {
        let (mut installer, mut host, server) = setup(tarball_server());
        let extract = through_download(&mut installer, &mut host, &server);
        assert_eq!(
            host.calls.last().unwrap(),
            "extract lsp/zls/0.13.0.download lsp/zls/0.13.0.partial TarGz"
        );
        host.files.insert("lsp/zls/0.13.0.partial/bin/zls".into());
        host.files.insert("lsp/zls/0.13.0.partial/README".into());
        installer.finished(&mut host, extract, Ok(()));

        assert!(host.has("lsp/zls/0.13.0/bin/zls"));
        assert!(host.has("lsp/zls/0.13.0/README"), "the whole tree moves");
        assert_eq!(installed(&host, "zls").unwrap().binary, "bin/zls");
    }

    /// The exit criterion: a download that fails verification installs
    /// nothing and leaves nothing.
    #[test]
    fn a_failed_download_reports_and_leaves_no_partial_tree() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        installer.request(&mut host, &server, PLATFORM);
        let download = host.next_id;
        installer.finished(
            &mut host,
            download,
            Err("download failed: sha256 mismatch".into()),
        );

        assert!(host.files.is_empty(), "left behind: {:?}", host.files);
        assert_eq!(installed(&host, "rust-analyzer"), None);
        assert_eq!(host.last_status().0, Phase::Failed);
        assert!(host
            .said
            .iter()
            .any(|l| l == "error: download failed: sha256 mismatch"));
        assert!(
            !installer.is_installing("rust-analyzer"),
            "it can be tried again"
        );
        assert!(
            !host.calls.iter().any(|c| c.starts_with("extract")),
            "an unverified download is never unpacked"
        );
    }

    #[test]
    fn a_failed_unpack_removes_the_download_and_the_staging_tree() {
        let (mut installer, mut host, server) = setup(tarball_server());
        let extract = through_download(&mut installer, &mut host, &server);
        host.files.insert("lsp/zls/0.13.0.partial/half".into());
        installer.finished(&mut host, extract, Err("extract failed: truncated".into()));

        assert!(host.files.is_empty(), "left behind: {:?}", host.files);
        assert_eq!(host.last_status().0, Phase::Failed);
    }

    /// The registry said `bin/zls`; the archive had no such file.
    #[test]
    fn an_archive_without_the_promised_binary_is_a_failure_not_an_install() {
        let (mut installer, mut host, server) = setup(tarball_server());
        let extract = through_download(&mut installer, &mut host, &server);
        host.files.insert("lsp/zls/0.13.0.partial/README".into());
        installer.finished(&mut host, extract, Ok(()));

        assert_eq!(installed(&host, "zls"), None);
        assert!(host.files.is_empty(), "left behind: {:?}", host.files);
        assert!(
            host.said.iter().any(|l| l.contains("bin/zls")),
            "{:?}",
            host.said
        );
    }

    /// Every step that can refuse, refusing: the result is always a reported
    /// failure and no scratch files.
    #[test]
    fn a_refusal_at_any_step_is_reported_and_cleaned_up() {
        for op in ["download", "extract", "chmod", "rename", "put"] {
            let (mut installer, mut host, server) = setup(rust_analyzer());
            host.refuse = Some(op);
            installer.request(&mut host, &server, PLATFORM);
            if op != "download" {
                if let Some(extract) = finish_download(&mut installer, &mut host, &server) {
                    host.files.insert(format!(
                        "lsp/rust-analyzer/{}.partial/rust-analyzer",
                        server.version
                    ));
                    installer.finished(&mut host, extract, Ok(()));
                }
            }
            assert_eq!(host.last_status().0, Phase::Failed, "{op}");
            assert!(
                host.said.iter().any(|l| l.starts_with("error: ")),
                "{op}: {:?}",
                host.said
            );
            assert!(!installer.is_installing("rust-analyzer"), "{op}");
            let scratch: Vec<&String> = host
                .files
                .iter()
                .filter(|f| f.contains(".partial") || f.contains(".download"))
                .collect();
            assert!(scratch.is_empty(), "{op} left {scratch:?}");
            // `put` is the last step: the tree is in place and only the
            // record failed. Every earlier refusal installs nothing at all.
            if op != "put" {
                assert!(host.files.is_empty(), "{op} left {:?}", host.files);
            }
            assert_eq!(installed(&host, "rust-analyzer"), None, "{op}");
        }
    }

    /// A failed reinstall must not cost the user the server they had.
    #[test]
    fn a_failed_reinstall_leaves_the_working_install_alone() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        let v = server.version.clone();
        let extract = through_download(&mut installer, &mut host, &server);
        host.files
            .insert(format!("lsp/rust-analyzer/{v}.partial/rust-analyzer"));
        installer.finished(&mut host, extract, Ok(()));
        let record = installed(&host, "rust-analyzer");
        assert!(record.is_some());

        installer.request(&mut host, &server, PLATFORM);
        let download = host.next_id;
        installer.finished(&mut host, download, Err("download failed: offline".into()));

        assert!(host.has(&format!("lsp/rust-analyzer/{v}/rust-analyzer")));
        assert_eq!(installed(&host, "rust-analyzer"), record);
    }

    #[test]
    fn the_first_line_says_whether_this_replaces_an_install() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        let v = server.version.clone();
        installer.request(&mut host, &server, PLATFORM);
        assert_eq!(
            host.said[0],
            format!("Installing rust-analyzer {v} for {PLATFORM}")
        );
        let download = host.next_id;
        installer.finished(&mut host, download, Err("offline".into()));

        host.store.insert(
            "installed/rust-analyzer".into(),
            format!("{v}\nrust-analyzer"),
        );
        installer.request(&mut host, &server, PLATFORM);
        assert_eq!(
            host.said[0],
            format!("Reinstalling rust-analyzer {v} for {PLATFORM}")
        );
        let download = host.next_id;
        installer.finished(&mut host, download, Err("offline".into()));

        host.store.insert(
            "installed/rust-analyzer".into(),
            "2025-01-01\nrust-analyzer".into(),
        );
        installer.request(&mut host, &server, PLATFORM);
        assert_eq!(
            host.said[0],
            format!("Installing rust-analyzer {v} for {PLATFORM} (replacing 2025-01-01)")
        );
    }

    #[test]
    fn a_second_request_while_one_is_in_flight_does_not_start_another() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        installer.request(&mut host, &server, PLATFORM);
        installer.request(&mut host, &server, PLATFORM);

        assert_eq!(
            host.calls
                .iter()
                .filter(|c| c.starts_with("download"))
                .count(),
            1
        );
        assert_eq!(host.resets, 1, "the running install's log is not wiped");
        assert!(host
            .said
            .last()
            .unwrap()
            .contains("already being installed"));
    }

    #[test]
    fn a_platform_with_no_build_says_which_ones_there_are() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        installer.request(&mut host, &server, "plan9-mips");

        assert!(host.calls.is_empty(), "nothing was downloaded");
        assert_eq!(host.last_status().0, Phase::Failed);
        let line = host.said.last().unwrap();
        assert!(
            line.contains("plan9-mips") && line.contains("linux-x86_64"),
            "{line}"
        );
        assert!(!installer.is_installing("rust-analyzer"));
    }

    #[test]
    fn a_request_clears_what_an_interrupted_attempt_left_under_its_names() {
        let (mut installer, mut host, server) = setup(tarball_server());
        host.files.insert("lsp/zls/0.13.0.download".into());
        host.files.insert("lsp/zls/0.13.0.partial/stale".into());
        installer.request(&mut host, &server, PLATFORM);
        assert!(host.files.is_empty(), "{:?}", host.files);
    }

    #[test]
    fn progress_shows_a_percentage_when_the_size_is_known() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        installer.request(&mut host, &server, PLATFORM);
        let id = host.next_id;

        installer.progress(&mut host, id, 5 * 1_048_576, Some(10 * 1_048_576));
        assert!(
            host.last_status().1.ends_with("50% of 10.0 MB"),
            "{:?}",
            host.last_status()
        );

        installer.progress(&mut host, id, 3 * 1_048_576, None);
        assert!(
            host.last_status().1.ends_with("3.0 MB"),
            "{:?}",
            host.last_status()
        );

        // A server that lies about its length must not show 140%.
        installer.progress(&mut host, id, 14, Some(10));
        assert!(
            host.last_status().1.contains("100%"),
            "{:?}",
            host.last_status()
        );
    }

    #[test]
    fn events_for_a_job_that_is_not_ours_are_ignored() {
        let (mut installer, mut host) = (Installer::new(), Fake::default());
        installer.progress(&mut host, 99, 1, Some(2));
        installer.finished(&mut host, 99, Ok(()));
        installer.finished(&mut host, 99, Err("x".into()));
        assert!(host.statuses.is_empty() && host.said.is_empty() && host.calls.is_empty());
    }

    #[test]
    fn sweep_removes_scratch_names_and_nothing_else() {
        let mut host = Fake::default();
        for path in [
            "lsp/rust-analyzer/2026-10-05/rust-analyzer",
            "lsp/rust-analyzer/2026-11-01.partial/rust-analyzer",
            "lsp/rust-analyzer/2026-11-01.download",
            "lsp/rust-analyzer/2026-11-01.download.part",
            "lsp/zls/0.13.0.partial.part/x",
            "registry.toml",
        ] {
            host.files.insert(path.into());
        }
        sweep(&mut host);
        assert_eq!(
            host.files.into_iter().collect::<Vec<_>>(),
            vec![
                "lsp/rust-analyzer/2026-10-05/rust-analyzer".to_string(),
                "registry.toml".to_string()
            ]
        );
    }

    #[test]
    fn an_installed_record_round_trips_and_rejects_garbage() {
        let record = Installed {
            version: "2026-10-05".into(),
            binary: "bin/ra".into(),
        };
        assert_eq!(Installed::decode(&record.encode()), Some(record));
        for bad in ["", "only-a-version", "\nbinary", "version\n"] {
            assert_eq!(Installed::decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_buffer_is_named_for_the_server() {
        assert_eq!(buffer_name("rust-analyzer"), "*lsp-install:rust-analyzer*");
    }
}
