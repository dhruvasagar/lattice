//! The install state machine.
//!
//! An install is two host jobs and a few file operations between them:
//!
//! ```text
//! request ─► download ──ok──► extract ──ok──► mark executable ─► move into place ─► record ─► register
//!               │                │                  │                  │
//!               └──── err ───────┴──────────────────┴──────────────────┴─► clean up, report
//! ```
//!
//! "Register" is what makes an installed server *used*: the editor is told to
//! run the managed binary for that language. It is not a step of the install
//! so much as a consequence of the record — see [`Installer::reconcile`].
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

use crate::registry::{Archive, Registry, Server};

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

    /// Whether a file exists.
    fn exists(&self, path: &str) -> bool;

    fn put(&mut self, key: &str, value: &str) -> Result<(), String>;
    fn get(&self, key: &str) -> Option<String>;
    /// Forget a key. Absent is fine.
    fn delete(&mut self, key: &str);
    /// Every key carrying `prefix`, in full.
    fn keys(&self, prefix: &str) -> Vec<String>;

    /// Tell the editor to run `binary` (relative to the data directory) as
    /// `server`. Returns a token for [`unregister`](Self::unregister).
    fn register(&mut self, server: &Server, binary: &str) -> Result<u64, String>;
    /// Withdraw a registration.
    fn unregister(&mut self, token: u64);

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

    /// The executable, relative to the data directory.
    pub fn binary_path(&self, name: &str) -> String {
        format!("{}/{}", tree(name, &self.version), self.binary)
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

/// A server the editor has been told about.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Registered {
    name: String,
    version: String,
    token: u64,
}

/// What this plugin instance is doing and has done: the installs in flight,
/// keyed by the id of the host job each is waiting on, and the servers it has
/// registered with the editor.
///
/// Both are plain memory, and both are right to be: a host job does not
/// outlive the instance that started it, and neither does a registration.
#[derive(Debug, Default)]
pub struct Installer {
    jobs: Vec<(u64, Job)>,
    registered: Vec<Registered>,
}

fn is_scratch(entry: &str) -> bool {
    [".partial", ".partial.part", ".download", ".download.part"]
        .iter()
        .any(|suffix| entry.ends_with(suffix))
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
        Self {
            jobs: Vec::new(),
            registered: Vec::new(),
        }
    }

    /// Make the editor's view match the installed records: register every
    /// installed server it has not been told about, and withdraw every
    /// registration whose server is no longer installed.
    ///
    /// Run at startup (nothing is registered yet, so this is what makes an
    /// install survive a restart), after an install, and after an uninstall.
    /// One routine for all three, so there is no path on which the records
    /// and the registrations can be left disagreeing.
    ///
    /// A server whose version changed is registered at the new version
    /// BEFORE the old registration is withdrawn. The editor takes the newest
    /// registration for an id, so there is no moment with neither.
    pub fn reconcile(&mut self, host: &mut impl Host, registry: &Registry) {
        let installed: Vec<(String, Installed)> = host
            .keys(INSTALLED_PREFIX)
            .iter()
            .filter_map(|key| key.strip_prefix(INSTALLED_PREFIX))
            .filter_map(|name| installed(host, name).map(|record| (name.to_string(), record)))
            .collect();

        for (name, record) in &installed {
            let current = self
                .registered
                .iter()
                .any(|r| r.name == *name && r.version == record.version);
            if current {
                continue;
            }
            // The registry is where the editor-facing half of a server lives
            // (its language, its file patterns). An installed server the
            // registry no longer lists cannot be described, so it is left
            // installed and unregistered rather than guessed at.
            let Some(server) = registry.get(name) else {
                continue;
            };
            let binary = record.binary_path(name);
            let buffer = buffer_name(name);
            if !host.exists(&binary) {
                // Registering a command that is not there would shadow a
                // working server on `PATH` with one that cannot start.
                host.say(
                    &buffer,
                    &format!(
                        "error: {name} {} is recorded as installed but its files are missing; run :lsp-install {name}",
                        record.version
                    ),
                );
                continue;
            }
            match host.register(server, &binary) {
                Ok(token) => {
                    let mut superseded = Vec::new();
                    self.registered.retain(|r| {
                        if r.name == *name {
                            superseded.push(r.token);
                            false
                        } else {
                            true
                        }
                    });
                    self.registered.push(Registered {
                        name: name.clone(),
                        version: record.version.clone(),
                        token,
                    });
                    for token in superseded {
                        host.unregister(token);
                    }
                }
                Err(e) => host.say(
                    &buffer,
                    &format!("error: could not register {name} with the editor: {e}"),
                ),
            }
        }

        let mut withdrawn = Vec::new();
        self.registered.retain(|r| {
            if installed.iter().any(|(name, _)| *name == r.name) {
                true
            } else {
                withdrawn.push(r.token);
                false
            }
        });
        for token in withdrawn {
            host.unregister(token);
        }
    }

    /// Whether the editor has been told to run `name` at `version`.
    fn is_registered(&self, name: &str, version: &str) -> bool {
        self.registered
            .iter()
            .any(|r| r.name == name && r.version == version)
    }

    /// Remove `name`: its registration, its record and its files.
    pub fn uninstall(&mut self, host: &mut impl Host, registry: &Registry, name: &str) {
        let buffer = buffer_name(name);
        if self.is_installing(name) {
            host.say(
                &buffer,
                &format!("{name} is being installed; uninstall it once that finishes"),
            );
            return;
        }
        host.reset(&buffer);
        let Some(record) = installed(host, name) else {
            host.say(&buffer, &format!("{name} is not installed"));
            host.status(&buffer, Phase::Failed, &format!("{name}: not installed"));
            return;
        };
        // Record first, then the registration that follows from it, then the
        // files: the editor is never pointed at a tree that is being deleted.
        host.delete(&format!("{INSTALLED_PREFIX}{name}"));
        self.reconcile(host, registry);
        host.remove_tree(&server_dir(name));
        host.say(&buffer, &format!("Removed {name} {}", record.version));
        host.say(
            &buffer,
            "A server that is already running keeps running until the editor restarts.",
        );
        host.status(&buffer, Phase::Succeeded, &format!("{name} uninstalled"));
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
    pub fn finished(
        &mut self,
        host: &mut impl Host,
        registry: &Registry,
        id: u64,
        outcome: Result<(), String>,
    ) {
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
                    self.reconcile(host, registry);
                    if !self.is_registered(&job.name, &job.version) {
                        // `reconcile` said why. The files are in place and
                        // the record is written, so the next startup tries
                        // again; this session just does not have the server.
                        host.status(
                            &buffer,
                            Phase::Failed,
                            &format!("{} {} installed, but not registered", job.name, job.version),
                        );
                        return;
                    }
                    host.say(
                        &buffer,
                        "Registered with the editor: files opened from now on use it.",
                    );
                    // The version just replaced, if any — only now, after
                    // the editor has been moved off it.
                    let dir = server_dir(&job.name);
                    for entry in host.list(&dir) {
                        if entry != job.version && !is_scratch(&entry) {
                            host.remove_tree(&format!("{dir}/{entry}"));
                            host.say(&buffer, &format!("Removed the previous version, {entry}"));
                        }
                    }
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
        fn exists(&self, path: &str) -> bool {
            self.has(path)
        }
        fn delete(&mut self, key: &str) {
            self.store.remove(key);
        }
        fn keys(&self, prefix: &str) -> Vec<String> {
            self.store
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect()
        }
        fn register(&mut self, server: &Server, binary: &str) -> Result<u64, String> {
            self.calls.push(format!(
                "register {} as {} -> {binary}",
                server.name, server.lsp_id
            ));
            self.refuses("register")?;
            self.next_id += 1;
            Ok(self.next_id)
        }
        fn unregister(&mut self, token: u64) {
            self.calls.push(format!("unregister {token}"));
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

    const ZLS: &str = r#"
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
"#;

    /// The bundled servers plus `zls`, a tarball.
    fn registry() -> Registry {
        let (registry, problem) = Registry::load(Some(ZLS));
        assert_eq!(problem, None);
        registry
    }

    fn tarball_server() -> Server {
        registry().get("zls").unwrap().clone()
    }

    /// The calls that told the editor something, in order.
    fn editor_calls(host: &Fake) -> Vec<&str> {
        host.calls
            .iter()
            .filter(|c| c.starts_with("register") || c.starts_with("unregister"))
            .map(String::as_str)
            .collect()
    }

    /// Install `server` to completion.
    fn install(installer: &mut Installer, host: &mut Fake, server: &Server) {
        let extract = through_download(installer, host, server);
        let build = &server.platform[PLATFORM];
        host.files.insert(format!(
            "lsp/{}/{}.partial/{}",
            server.name, server.version, build.binary
        ));
        installer.finished(host, &registry(), extract, Ok(()));
        assert_eq!(host.last_status().0, Phase::Succeeded, "{:?}", host.said);
    }

    fn at_version(server: &Server, version: &str) -> Server {
        let mut newer = server.clone();
        newer.version = version.to_string();
        newer
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
        installer.finished(host, &registry(), download, Ok(()));
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
        installer.finished(&mut host, &registry(), extract, Ok(()));

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
        installer.finished(&mut host, &registry(), extract, Ok(()));

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
            &registry(),
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
        installer.finished(
            &mut host,
            &registry(),
            extract,
            Err("extract failed: truncated".into()),
        );

        assert!(host.files.is_empty(), "left behind: {:?}", host.files);
        assert_eq!(host.last_status().0, Phase::Failed);
    }

    /// The registry said `bin/zls`; the archive had no such file.
    #[test]
    fn an_archive_without_the_promised_binary_is_a_failure_not_an_install() {
        let (mut installer, mut host, server) = setup(tarball_server());
        let extract = through_download(&mut installer, &mut host, &server);
        host.files.insert("lsp/zls/0.13.0.partial/README".into());
        installer.finished(&mut host, &registry(), extract, Ok(()));

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
                    installer.finished(&mut host, &registry(), extract, Ok(()));
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
            assert!(
                editor_calls(&host).is_empty(),
                "{op}: nothing was registered"
            );
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
        installer.finished(&mut host, &registry(), extract, Ok(()));
        let record = installed(&host, "rust-analyzer");
        assert!(record.is_some());

        installer.request(&mut host, &server, PLATFORM);
        let download = host.next_id;
        installer.finished(
            &mut host,
            &registry(),
            download,
            Err("download failed: offline".into()),
        );

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
        installer.finished(&mut host, &registry(), download, Err("offline".into()));

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
        installer.finished(&mut host, &registry(), download, Err("offline".into()));

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
    fn a_finished_install_is_registered_with_the_editor_by_its_managed_path() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        install(&mut installer, &mut host, &server);
        assert_eq!(
            editor_calls(&host),
            vec![format!(
                "register rust-analyzer as rust -> lsp/rust-analyzer/{}/rust-analyzer",
                server.version
            )]
        );
        assert!(host
            .said
            .iter()
            .any(|l| l.starts_with("Registered with the editor")));
    }

    /// An install survives a restart because startup reconciles: nothing is
    /// registered in a fresh instance, and the records say what should be.
    #[test]
    fn startup_registers_every_installed_server_that_is_really_there() {
        let (mut installer, mut host) = (Installer::new(), Fake::default());
        host.store.insert(
            "installed/rust-analyzer".into(),
            "2026-10-05\nrust-analyzer".into(),
        );
        host.files
            .insert("lsp/rust-analyzer/2026-10-05/rust-analyzer".into());
        // Recorded, but someone deleted the files.
        host.store
            .insert("installed/zls".into(), "0.13.0\nbin/zls".into());
        // Recorded, files present, but no registry entry describes it.
        host.store
            .insert("installed/orphan".into(), "1\norphan".into());
        host.files.insert("lsp/orphan/1/orphan".into());

        installer.reconcile(&mut host, &registry());

        assert_eq!(
            editor_calls(&host),
            vec!["register rust-analyzer as rust -> lsp/rust-analyzer/2026-10-05/rust-analyzer"]
        );
        assert!(
            host.said
                .iter()
                .any(|l| l.contains("zls") && l.contains("files are missing")),
            "{:?}",
            host.said
        );

        // And it is idempotent: a second pass has nothing left to do.
        let before = host.calls.len();
        installer.reconcile(&mut host, &registry());
        assert_eq!(host.calls.len(), before);
    }

    /// An update: the new version is registered BEFORE the old registration
    /// is withdrawn, and the old tree goes only after both.
    #[test]
    fn an_update_flips_the_registration_and_then_removes_the_old_version() {
        let (mut installer, mut host, server) = setup(tarball_server());
        install(&mut installer, &mut host, &server);
        host.calls.clear();

        let newer = at_version(&server, "0.14.0");
        installer.request(&mut host, &newer, PLATFORM);
        assert!(
            host.said[0].ends_with("(replacing 0.13.0)"),
            "{:?}",
            host.said
        );
        let extract = finish_download(&mut installer, &mut host, &newer).unwrap();
        host.files.insert("lsp/zls/0.14.0.partial/bin/zls".into());
        assert!(
            host.has("lsp/zls/0.13.0/bin/zls"),
            "the old version is untouched while the new one is fetched"
        );
        installer.finished(&mut host, &registry(), extract, Ok(()));

        let calls = editor_calls(&host);
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert_eq!(calls[0], "register zls as zig -> lsp/zls/0.14.0/bin/zls");
        assert!(calls[1].starts_with("unregister "), "{calls:?}");
        assert!(host.has("lsp/zls/0.14.0/bin/zls"));
        assert!(!host.has("lsp/zls/0.13.0/bin/zls"), "the old tree is gone");
        assert_eq!(installed(&host, "zls").unwrap().version, "0.14.0");
        assert!(host
            .said
            .iter()
            .any(|l| l == "Removed the previous version, 0.13.0"));
    }

    /// A failed update costs nothing: the old version stays installed AND
    /// registered.
    #[test]
    fn a_failed_update_leaves_the_old_version_registered() {
        let (mut installer, mut host, server) = setup(tarball_server());
        install(&mut installer, &mut host, &server);
        host.calls.clear();

        let newer = at_version(&server, "0.14.0");
        installer.request(&mut host, &newer, PLATFORM);
        let download = host.next_id;
        installer.finished(&mut host, &registry(), download, Err("offline".into()));

        assert!(editor_calls(&host).is_empty(), "{:?}", host.calls);
        assert!(host.has("lsp/zls/0.13.0/bin/zls"));
        assert_eq!(installed(&host, "zls").unwrap().version, "0.13.0");
    }

    /// The editor refuses the registration: the server is installed, and the
    /// buffer does not claim more than that.
    #[test]
    fn an_install_the_editor_will_not_register_says_so() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        host.refuse = Some("register");
        let extract = through_download(&mut installer, &mut host, &server);
        host.files.insert(format!(
            "lsp/rust-analyzer/{}.partial/rust-analyzer",
            server.version
        ));
        installer.finished(&mut host, &registry(), extract, Ok(()));

        let (phase, text) = host.last_status();
        assert_eq!(*phase, Phase::Failed);
        assert!(text.ends_with("installed, but not registered"), "{text}");
        assert!(host
            .said
            .iter()
            .any(|l| l.starts_with("error: could not register")));
        assert!(
            installed(&host, "rust-analyzer").is_some(),
            "the record stays, so the next startup tries again"
        );
    }

    #[test]
    fn uninstall_withdraws_the_registration_then_removes_the_record_and_files() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        install(&mut installer, &mut host, &server);
        host.calls.clear();

        installer.uninstall(&mut host, &registry(), "rust-analyzer");

        let calls = editor_calls(&host);
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].starts_with("unregister "));
        assert_eq!(installed(&host, "rust-analyzer"), None);
        assert!(host.files.is_empty(), "left behind: {:?}", host.files);
        assert_eq!(host.last_status().0, Phase::Succeeded);

        // Nothing is left for a later reconcile to resurrect.
        host.calls.clear();
        installer.reconcile(&mut host, &registry());
        assert!(host.calls.is_empty());
    }

    #[test]
    fn uninstalling_what_is_not_installed_says_so_and_touches_nothing() {
        let (mut installer, mut host) = (Installer::new(), Fake::default());
        host.files.insert("lsp/other/1/other".into());
        installer.uninstall(&mut host, &registry(), "rust-analyzer");

        assert_eq!(host.last_status().0, Phase::Failed);
        assert_eq!(host.said, vec!["rust-analyzer is not installed"]);
        assert!(host.has("lsp/other/1/other"));
        assert!(host.calls.is_empty());
    }

    #[test]
    fn a_server_being_installed_cannot_be_uninstalled_out_from_under_it() {
        let (mut installer, mut host, server) = setup(rust_analyzer());
        installer.request(&mut host, &server, PLATFORM);
        let resets = host.resets;
        installer.uninstall(&mut host, &registry(), "rust-analyzer");

        assert!(installer.is_installing("rust-analyzer"));
        assert_eq!(host.resets, resets, "the install's log is not wiped");
        assert!(host.said.last().unwrap().contains("being installed"));
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
        installer.finished(&mut host, &registry(), 99, Ok(()));
        installer.finished(&mut host, &registry(), 99, Err("x".into()));
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
