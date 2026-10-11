//! LH.0.2 — the `host-services.extract-archive` and `set-executable` seams.
//!
//! Design: `docs/dev/architecture/lighthouse.md` §3.2. Slice plan:
//! `docs/dev/operations/slice-plans/lighthouse.md` LH.0.2.
//!
//! ## Why the host unpacks
//!
//! Inflating a release archive is CPU-bound work, and a guest does its work
//! inside a fuel-metered, wall-clock-bounded call. It also cannot write the
//! result from every seam (the sync WASI filesystem shim panics on the grammar
//! seam — `read-file`'s doc comment). So unpacking is a host job like a
//! download is: see [`crate::job`].
//!
//! ## An archive is untrusted input
//!
//! It arrived over the network, and a SHA pinned in a registry says the bytes
//! are the ones somebody reviewed — not that they are benign. Every entry is
//! confined to the destination, three ways, because each alone has a known way
//! around it:
//!
//! - **The path** must be relative with no `..`. (`../../.ssh/authorized_keys`.)
//! - **Nothing is written through a symlink.** An archive can ship `a -> /etc`
//!   and then `a/passwd`; the second path is lexically innocent.
//! - **A symlink's target** must itself be relative and stay inside. (So the
//!   tree left behind holds no pointer out of itself for a later step to
//!   follow.)
//!
//! Hard links, devices and FIFOs are refused by name rather than skipped: a
//! release archive has no use for them, and a silently-skipped entry is a
//! half-installed server.
//!
//! ## All or nothing
//!
//! Work happens in a sibling `<dest>.part` and is renamed into place only when
//! the whole archive has been read. Every failure — a bad entry, a truncated
//! stream, a cancel, a size limit — removes the part and leaves `dest` absent,
//! so "the directory exists" is a sound test for "the install is complete".

use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Write as _};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use flate2::read::GzDecoder;
use lattice_runtime::EventBus;

use crate::capability::CapabilityGrant;
use crate::job::{Job, PendingJob};

/// The archive kinds the seam unpacks — the ones the first registry needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Format {
    /// One gzip-compressed file. `dest` is the file to write.
    Gz,
    /// A gzip-compressed tar. `dest` is the directory to create.
    TarGz,
}

/// The bounds an extraction runs under. Host policy: a guest cannot widen them.
#[derive(Debug, Clone, Copy)]
struct Limits {
    /// Most bytes written, in total. The compressed size says nothing about
    /// this — a few kilobytes of gzip can describe gigabytes of zeroes.
    max_bytes: u64,
    /// Most entries in a tar. Bounds an archive of a million empty files.
    max_entries: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // 4 GiB unpacked: several times the largest language server.
            max_bytes: 4 << 30,
            max_entries: 100_000,
        }
    }
}

#[derive(Debug)]
struct Request {
    src: PathBuf,
    dest: PathBuf,
    format: Format,
    limits: Limits,
}

/// Validate an extraction against `grant` and make it a job.
///
/// Refused here, by name: a source outside the plugin's readable paths or not
/// a file, a destination outside its writable paths, and a destination that
/// already exists as something the format cannot replace.
pub(crate) fn prepare(
    grant: &CapabilityGrant,
    bus: Arc<EventBus>,
    plugin: u32,
    src: &str,
    dest: &str,
    format: Format,
) -> Result<PendingJob, String> {
    prepare_with(grant, bus, plugin, src, dest, format, Limits::default())
}

fn prepare_with(
    grant: &CapabilityGrant,
    bus: Arc<EventBus>,
    plugin: u32,
    src: &str,
    dest: &str,
    format: Format,
    limits: Limits,
) -> Result<PendingJob, String> {
    let src_path = PathBuf::from(src);
    if !crate::host_services::grant_permits_read(grant, &src_path) {
        tracing::info!(
            src,
            "host-services extract-archive denied: source outside the plugin's fs grant"
        );
        return Err(format!(
            "extract denied: '{src}' is outside the plugin's granted paths"
        ));
    }
    if !src_path.is_file() {
        return Err(format!("extract failed: '{src}' is not a file"));
    }
    let dest_path = PathBuf::from(dest);
    if !crate::host_services::grant_permits_write(grant, &dest_path) {
        tracing::info!(
            dest,
            "host-services extract-archive denied: destination outside the plugin's fs:write grant"
        );
        return Err(format!(
            "extract denied: '{dest}' is outside the plugin's writable paths"
        ));
    }
    match format {
        // A file is replaced by the rename, as a download's is.
        Format::Gz if dest_path.is_dir() => {
            return Err(format!("extract failed: '{dest}' is a directory"));
        }
        // A directory is not: merging an archive into an existing tree is how
        // an update ends up running half of the old version. The caller
        // unpacks beside it and switches.
        Format::TarGz if dest_path.exists() => {
            return Err(format!("extract failed: '{dest}' already exists"));
        }
        _ => {}
    }
    let request = Request {
        src: src_path,
        dest: dest_path,
        format,
        limits,
    };
    Ok(PendingJob::new(plugin, bus, "extract", move |job| {
        extract(&request, job)
    }))
}

/// Make `path` executable — what a guest cannot do for a binary it downloaded
/// or unpacked from a bare `.gz`, since WASI has no `chmod`.
///
/// Gated on `fs:write`. Regular files only. A no-op where the platform has no
/// executable bit.
pub(crate) fn set_executable(grant: &CapabilityGrant, path: &str) -> Result<(), String> {
    let file = PathBuf::from(path);
    if !crate::host_services::grant_permits_write(grant, &file) {
        tracing::info!(
            path,
            "host-services set-executable denied: outside the plugin's fs:write grant"
        );
        return Err(format!(
            "set-executable denied: '{path}' is outside the plugin's writable paths"
        ));
    }
    let meta =
        std::fs::metadata(&file).map_err(|e| format!("set-executable failed: '{path}': {e}"))?;
    if !meta.is_file() {
        return Err(format!("set-executable failed: '{path}' is not a file"));
    }
    set_mode(&file, true).map_err(|e| format!("set-executable failed: '{path}': {e}"))
}

/// `0o755` or `0o644` — never the archive's own mode bits, which may carry
/// setuid or world-writable.
#[cfg(unix)]
fn set_mode(path: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if executable { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

/// `<dest>.part` — a sibling, so the final rename never crosses a filesystem.
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Remove whatever is at `part`, file or tree. Absent is fine.
fn remove_part(part: &Path) {
    if part.is_dir() && !part.is_symlink() {
        let _ = std::fs::remove_dir_all(part);
    } else {
        let _ = std::fs::remove_file(part);
    }
}

/// Run one extraction to its end. `Ok` only with the result at `dest`; on
/// every `Err` the part is gone.
fn extract(req: &Request, job: &mut Job) -> Result<(), String> {
    let part = part_path(&req.dest);
    // A part left by a host that died mid-extraction would otherwise be
    // merged into.
    remove_part(&part);
    let result = extract_into(req, job, &part).and_then(|()| {
        std::fs::rename(&part, &req.dest).map_err(|e| {
            format!(
                "extract failed: cannot move the result to '{}': {e}",
                req.dest.display()
            )
        })
    });
    if result.is_err() {
        remove_part(&part);
    }
    result
}

/// Counts the bytes read through it, for progress: how much of the
/// *compressed* source has been consumed is the only total known up front.
struct Counting<R> {
    inner: R,
    read: Rc<Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.set(self.read.get() + n as u64);
        Ok(n)
    }
}

/// What every copy loop shares: the output budget and the progress source.
struct Budget<'a> {
    job: &'a mut Job,
    written: u64,
    max_bytes: u64,
    consumed: Rc<Cell<u64>>,
    total: u64,
}

impl Budget<'_> {
    /// Copy `from` to `to`, polling for a cancel and charging the budget per
    /// chunk.
    fn copy(&mut self, from: &mut impl Read, to: &mut File, what: &Path) -> Result<(), String> {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            self.job.check_cancelled()?;
            let n = from
                .read(&mut buf)
                .map_err(|e| format!("extract failed: reading the archive: {e}"))?;
            if n == 0 {
                return Ok(());
            }
            self.written += n as u64;
            if self.written > self.max_bytes {
                return Err(format!(
                    "extract failed: the archive unpacks to more than the {} byte limit",
                    self.max_bytes
                ));
            }
            to.write_all(&buf[..n])
                .map_err(|e| format!("extract failed: writing '{}': {e}", what.display()))?;
            self.job.progress(self.consumed.get(), Some(self.total));
        }
    }
}

fn extract_into(req: &Request, job: &mut Job, part: &Path) -> Result<(), String> {
    let src = File::open(&req.src)
        .map_err(|e| format!("extract failed: cannot open '{}': {e}", req.src.display()))?;
    let total = src.metadata().map(|m| m.len()).unwrap_or(0);
    let consumed = Rc::new(Cell::new(0));
    let decoder = GzDecoder::new(Counting {
        inner: src,
        read: Rc::clone(&consumed),
    });
    let mut budget = Budget {
        job,
        written: 0,
        max_bytes: req.limits.max_bytes,
        consumed,
        total,
    };

    if let Some(parent) = req.dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("extract failed: cannot create '{}': {e}", parent.display()))?;
    }

    match req.format {
        Format::Gz => {
            let mut decoder = decoder;
            let mut file = create(part)?;
            budget.copy(&mut decoder, &mut file, part)?;
            file.sync_all()
                .map_err(|e| format!("extract failed: syncing '{}': {e}", part.display()))
        }
        Format::TarGz => untar(decoder, part, &mut budget, req.limits.max_entries),
    }
}

fn create(path: &Path) -> Result<File, String> {
    File::create(path)
        .map_err(|e| format!("extract failed: cannot create '{}': {e}", path.display()))
}

fn untar(
    decoder: impl Read,
    root: &Path,
    budget: &mut Budget<'_>,
    max_entries: u64,
) -> Result<(), String> {
    std::fs::create_dir(root)
        .map_err(|e| format!("extract failed: cannot create '{}': {e}", root.display()))?;
    let mut archive = tar::Archive::new(decoder);
    let entries = archive
        .entries()
        .map_err(|e| format!("extract failed: not a tar archive: {e}"))?;
    let mut count = 0u64;
    for entry in entries {
        budget.job.check_cancelled()?;
        let mut entry = entry.map_err(|e| format!("extract failed: reading the archive: {e}"))?;
        count += 1;
        if count > max_entries {
            return Err(format!(
                "extract failed: the archive holds more than {max_entries} entries"
            ));
        }
        let name = entry
            .path()
            .map_err(|e| format!("extract failed: an entry has an unreadable path: {e}"))?
            .into_owned();
        let Some(relative) = confined(&name) else {
            return Err(format!(
                "extract failed: entry '{}' would land outside the destination",
                name.display()
            ));
        };
        if relative.as_os_str().is_empty() {
            // `./` — the archive's own root.
            continue;
        }
        let target = root.join(&relative);
        refuse_symlinked_ancestors(root, &relative, &name)?;

        let kind = entry.header().entry_type();
        if kind.is_dir() {
            std::fs::create_dir_all(&target).map_err(|e| {
                format!("extract failed: cannot create '{}': {e}", target.display())
            })?;
        } else if kind.is_file() {
            make_parent(&target)?;
            let executable = entry.header().mode().is_ok_and(|m| m & 0o111 != 0);
            let mut file = create(&target)?;
            budget.copy(&mut entry, &mut file, &target)?;
            drop(file);
            set_mode(&target, executable).map_err(|e| {
                format!(
                    "extract failed: setting the mode of '{}': {e}",
                    target.display()
                )
            })?;
        } else if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(|e| format!("extract failed: an entry has an unreadable link: {e}"))?
                .map(std::borrow::Cow::into_owned)
                .unwrap_or_default();
            if !link_stays_inside(&relative, &link) {
                return Err(format!(
                    "extract failed: symlink '{}' points outside the destination ('{}')",
                    name.display(),
                    link.display()
                ));
            }
            make_parent(&target)?;
            symlink(&link, &target)
                .map_err(|e| format!("extract failed: cannot link '{}': {e}", target.display()))?;
        } else {
            return Err(format!(
                "extract failed: entry '{}' is a {kind:?}, which is not unpacked \
                 (files, directories and symlinks only)",
                name.display()
            ));
        }
    }
    Ok(())
}

fn make_parent(target: &Path) -> Result<(), String> {
    match target.parent() {
        Some(parent) => std::fs::create_dir_all(parent)
            .map_err(|e| format!("extract failed: cannot create '{}': {e}", parent.display())),
        None => Ok(()),
    }
}

#[cfg(unix)]
fn symlink(link: &Path, at: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(link, at)
}

#[cfg(not(unix))]
fn symlink(_link: &Path, _at: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks are not unpacked on this platform",
    ))
}

/// An entry's path, normalised, if it stays inside the destination: relative,
/// no `..`, no root or drive prefix. `./` components are dropped.
fn confined(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out)
}

/// Would a symlink at `at` (relative to the root) with target `link` resolve
/// inside the root? Lexical: `..` may climb, but never above the root.
fn link_stays_inside(at: &Path, link: &Path) -> bool {
    // Resolution starts from the directory holding the link.
    let mut depth = at.components().count().saturating_sub(1);
    for component in link.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return false,
        }
    }
    true
}

/// Refuse an entry any of whose parent directories — inside the root — is a
/// symlink. The lexical checks cannot see this one: `a/passwd` is a perfectly
/// confined path until `a` turns out to be a link an earlier entry made.
fn refuse_symlinked_ancestors(root: &Path, relative: &Path, name: &Path) -> Result<(), String> {
    let mut at = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        if components.peek().is_none() {
            break;
        }
        at.push(component);
        if at.is_symlink() {
            return Err(format!(
                "extract failed: entry '{}' would be written through a symlink",
                name.display()
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::capability::FsGrant;
    use crate::job::test_support::{job_bus, outcome_of};
    use flate2::Compression;
    use flate2::write::GzEncoder;

    fn gz(bytes: &[u8]) -> Vec<u8> {
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    }

    /// One tar entry, as a test describes it.
    enum Entry<'a> {
        File(&'a str, &'a [u8], u32),
        Dir(&'a str),
        Symlink(&'a str, &'a str),
        /// A regular file whose name is written into the header verbatim —
        /// the builder's own path setter refuses `..`, which is exactly what a
        /// hostile archive does not use.
        RawFile(&'a str, &'a [u8]),
        Hardlink(&'a str, &'a str),
    }

    fn tar_gz(entries: &[Entry<'_>]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for entry in entries {
            let mut header = tar::Header::new_gnu();
            match entry {
                Entry::File(path, data, mode) => {
                    header.set_size(data.len() as u64);
                    header.set_mode(*mode);
                    header.set_entry_type(tar::EntryType::Regular);
                    builder.append_data(&mut header, path, *data).unwrap();
                }
                Entry::Dir(path) => {
                    header.set_size(0);
                    header.set_mode(0o755);
                    header.set_entry_type(tar::EntryType::Directory);
                    builder.append_data(&mut header, path, &[][..]).unwrap();
                }
                Entry::Symlink(path, target) => {
                    header.set_size(0);
                    header.set_mode(0o777);
                    header.set_entry_type(tar::EntryType::Symlink);
                    builder.append_link(&mut header, path, target).unwrap();
                }
                Entry::Hardlink(path, target) => {
                    header.set_size(0);
                    header.set_mode(0o644);
                    header.set_entry_type(tar::EntryType::Link);
                    builder.append_link(&mut header, path, target).unwrap();
                }
                Entry::RawFile(name, data) => {
                    header.set_size(data.len() as u64);
                    header.set_mode(0o644);
                    header.set_entry_type(tar::EntryType::Regular);
                    let field = &mut header.as_old_mut().name;
                    field[..name.len()].copy_from_slice(name.as_bytes());
                    header.set_cksum();
                    builder.append(&header, *data).unwrap();
                }
            }
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn grant(dir: &Path) -> CapabilityGrant {
        CapabilityGrant {
            fs: vec![FsGrant {
                prefix: dir.to_path_buf(),
                write: true,
            }],
            ..Default::default()
        }
    }

    /// One extraction from request to outcome.
    struct Run {
        dir: tempfile::TempDir,
        dest: PathBuf,
        result: Result<(), String>,
    }

    impl Run {
        /// Everything under the granted directory, relative, sorted — so a
        /// test can assert on exactly what was left behind.
        fn left_behind(&self) -> Vec<String> {
            fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
                for entry in std::fs::read_dir(dir).unwrap().flatten() {
                    let path = entry.path();
                    out.push(path.strip_prefix(root).unwrap().display().to_string());
                    if path.is_dir() && !path.is_symlink() {
                        walk(&path, root, out);
                    }
                }
            }
            let mut out = Vec::new();
            walk(self.dir.path(), self.dir.path(), &mut out);
            out.sort();
            out
        }
    }

    fn run(archive: &[u8], format: Format, limits: Limits) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("archive");
        std::fs::write(&src, archive).unwrap();
        let dest = dir.path().join("out");
        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare_with(
            &grant(dir.path()),
            bus,
            7,
            src.to_str().unwrap(),
            dest.to_str().unwrap(),
            format,
            limits,
        )
        .expect("a granted source and destination are accepted");
        let id = pending.id();
        let _guard = pending.start();
        let result = outcome_of(&mut rx, id).result;
        Run { dir, dest, result }
    }

    fn run_tar(entries: &[Entry<'_>]) -> Run {
        run(&tar_gz(entries), Format::TarGz, Limits::default())
    }

    /// The failure contract: an error naming `needle`, and nothing on disk but
    /// the archive that was handed in.
    fn assert_refused(r: &Run, needle: &str) {
        let err = r.result.as_ref().expect_err("refused");
        assert!(err.contains(needle), "{err}");
        assert_eq!(r.left_behind(), vec!["archive"], "nothing was left behind");
    }

    #[test]
    fn a_gz_unpacks_to_one_file() {
        let r = run(&gz(b"#!/bin/sh\necho hi\n"), Format::Gz, Limits::default());
        assert_eq!(r.result, Ok(()));
        assert_eq!(std::fs::read(&r.dest).unwrap(), b"#!/bin/sh\necho hi\n");
        assert_eq!(r.left_behind(), vec!["archive", "out"]);
    }

    #[test]
    fn a_corrupt_gz_leaves_nothing() {
        let mut bytes = gz(&[b'x'; 4096]);
        let mid = bytes.len() / 2;
        bytes.truncate(mid);
        assert_refused(&run(&bytes, Format::Gz, Limits::default()), "reading");
    }

    /// The decompression-bomb case: the limit is on what comes OUT.
    #[test]
    fn a_gz_that_unpacks_past_the_limit_is_refused() {
        let limits = Limits {
            max_bytes: 1024,
            ..Limits::default()
        };
        let bomb = gz(&vec![0u8; 1024 * 1024]);
        assert!(bomb.len() < 4096, "small going in: {} bytes", bomb.len());
        assert_refused(&run(&bomb, Format::Gz, limits), "limit");
    }

    #[test]
    fn a_tar_gz_unpacks_its_tree() {
        let mut entries = vec![
            Entry::Dir("server/"),
            Entry::File("server/bin/ls", b"binary", 0o755),
            Entry::File("server/README", b"read me", 0o644),
        ];
        // Symlinks are unpacked on unix only; elsewhere an archive holding
        // one is refused by name (the test below this one). This test first
        // ran on Windows in CI with the links in, and failed on that refusal
        // rather than on anything it was written to check.
        if cfg!(unix) {
            entries.push(Entry::Symlink("server/bin/current", "ls"));
            entries.push(Entry::Symlink("server/docs", "../server/README"));
        }
        let r = run_tar(&entries);
        assert_eq!(r.result, Ok(()));
        assert_eq!(
            std::fs::read(r.dest.join("server/bin/ls")).unwrap(),
            b"binary"
        );
        assert_eq!(
            std::fs::read(r.dest.join("server/README")).unwrap(),
            b"read me"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &str| {
                std::fs::metadata(r.dest.join(p))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!(mode("server/bin/ls"), 0o755, "the executable bit survives");
            assert_eq!(mode("server/README"), 0o644);
            assert_eq!(
                std::fs::read(r.dest.join("server/bin/current")).unwrap(),
                b"binary",
                "an in-tree symlink resolves"
            );
        }
        assert!(!part_path(&r.dest).exists(), "the part was renamed away");
    }

    /// Where a symlink cannot be made, an archive holding one is refused
    /// whole — by name, and leaving nothing — rather than unpacked without it.
    /// A tree missing the link its launcher resolves through is a broken
    /// install that looks like a finished one.
    #[cfg(not(unix))]
    #[test]
    fn an_archive_with_a_symlink_is_refused_where_links_cannot_be_made() {
        let r = run_tar(&[
            Entry::File("server/bin/ls", b"binary", 0o755),
            Entry::Symlink("server/bin/current", "ls"),
        ]);
        assert_refused(&r, "symlinks are not unpacked on this platform");
        assert!(!r.dest.exists(), "nothing was left at the destination");
    }

    /// Mode bits beyond "executable or not" do not come out of an archive.
    #[cfg(unix)]
    #[test]
    fn setuid_and_world_writable_bits_are_not_honoured() {
        use std::os::unix::fs::PermissionsExt as _;
        let r = run_tar(&[Entry::File("tool", b"x", 0o4777)]);
        assert_eq!(r.result, Ok(()));
        let mode = std::fs::metadata(r.dest.join("tool"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o7777, 0o755);
    }

    /// **The classic.** Nothing may land outside the destination, and nothing
    /// partial may be left inside it.
    #[test]
    fn an_entry_climbing_out_of_the_destination_is_refused() {
        let r = run_tar(&[
            Entry::File("ok", b"fine", 0o644),
            Entry::RawFile("../escaped", b"gotcha"),
        ]);
        assert_refused(&r, "outside the destination");
        assert!(!r.dir.path().join("escaped").exists());
    }

    #[test]
    fn an_absolute_entry_is_refused() {
        let r = run_tar(&[Entry::RawFile("/tmp/lattice-extract-test-absolute", b"x")]);
        assert_refused(&r, "outside the destination");
        assert!(!Path::new("/tmp/lattice-extract-test-absolute").exists());
    }

    #[test]
    fn a_symlink_pointing_out_is_refused() {
        assert_refused(
            &run_tar(&[Entry::Symlink("link", "../../somewhere")]),
            "points outside",
        );
        assert_refused(
            &run_tar(&[Entry::Symlink("link", "/etc")]),
            "points outside",
        );
    }

    /// The one the lexical checks cannot see. Every path here is confined and
    /// the link stays inside — but writing `a/file` goes through `a`.
    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_symlink() {
        let r = run_tar(&[
            Entry::Dir("real/"),
            Entry::Symlink("a", "real"),
            Entry::File("a/file", b"x", 0o644),
        ]);
        assert_refused(&r, "through a symlink");
    }

    #[test]
    fn a_hard_link_is_refused_by_name_not_skipped() {
        let r = run_tar(&[Entry::File("a", b"x", 0o644), Entry::Hardlink("b", "a")]);
        assert_refused(&r, "not unpacked");
    }

    #[test]
    fn too_many_entries_is_refused() {
        let limits = Limits {
            max_entries: 2,
            ..Limits::default()
        };
        let archive = tar_gz(&[
            Entry::File("a", b"", 0o644),
            Entry::File("b", b"", 0o644),
            Entry::File("c", b"", 0o644),
        ]);
        assert_refused(&run(&archive, Format::TarGz, limits), "entries");
    }

    #[test]
    fn a_tar_that_unpacks_past_the_limit_is_refused() {
        let limits = Limits {
            max_bytes: 10,
            ..Limits::default()
        };
        let archive = tar_gz(&[
            Entry::File("a", b"12345678", 0o644),
            Entry::File("b", b"12345678", 0o644),
        ]);
        assert_refused(&run(&archive, Format::TarGz, limits), "limit");
    }

    #[test]
    fn a_gz_that_is_not_a_tar_is_refused() {
        let r = run(
            &gz(b"just some text, not a tar"),
            Format::TarGz,
            Limits::default(),
        );
        assert!(r.result.is_err(), "{:?}", r.result);
        assert_eq!(r.left_behind(), vec!["archive"]);
    }

    /// A part left by a host that died mid-extraction is not merged into.
    #[test]
    fn a_stale_part_is_replaced_not_merged() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("archive");
        std::fs::write(&src, tar_gz(&[Entry::File("new", b"n", 0o644)])).unwrap();
        let dest = dir.path().join("out");
        std::fs::create_dir(part_path(&dest)).unwrap();
        std::fs::write(part_path(&dest).join("stale"), b"old").unwrap();

        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare(
            &grant(dir.path()),
            bus,
            1,
            src.to_str().unwrap(),
            dest.to_str().unwrap(),
            Format::TarGz,
        )
        .unwrap();
        let id = pending.id();
        let _guard = pending.start();
        assert_eq!(outcome_of(&mut rx, id).result, Ok(()));
        assert!(dest.join("new").exists());
        assert!(!dest.join("stale").exists());
    }

    // ---- the synchronous refusals -------------------------------------

    fn refused(grant: &CapabilityGrant, src: &Path, dest: &Path, format: Format) -> String {
        prepare(
            grant,
            Arc::new(EventBus::new()),
            1,
            src.to_str().unwrap(),
            dest.to_str().unwrap(),
            format,
        )
        .expect_err("refused")
    }

    #[test]
    fn a_source_outside_the_grant_is_refused() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let src = other.path().join("archive");
        std::fs::write(&src, gz(b"x")).unwrap();
        let err = refused(
            &grant(granted.path()),
            &src,
            &granted.path().join("out"),
            Format::Gz,
        );
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("granted paths"), "{err}");
    }

    #[test]
    fn a_destination_outside_the_write_grant_is_refused() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let src = granted.path().join("archive");
        std::fs::write(&src, gz(b"x")).unwrap();
        let err = refused(
            &grant(granted.path()),
            &src,
            &other.path().join("out"),
            Format::Gz,
        );
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("writable paths"), "{err}");

        // …and a read-only grant over the destination is not enough.
        let mut read_only = grant(granted.path());
        read_only.fs[0].write = false;
        let err = refused(&read_only, &src, &granted.path().join("out"), Format::Gz);
        assert!(err.contains("writable paths"), "{err}");
    }

    #[test]
    fn a_missing_source_says_so_rather_than_reading_as_denied() {
        let dir = tempfile::tempdir().unwrap();
        let err = refused(
            &grant(dir.path()),
            &dir.path().join("nope"),
            &dir.path().join("out"),
            Format::Gz,
        );
        assert!(err.contains("not a file"), "{err}");
        assert!(!err.contains("denied"), "{err}");
    }

    /// An update unpacks beside the old version; it never merges into it.
    #[test]
    fn a_tar_destination_that_exists_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("archive");
        std::fs::write(&src, tar_gz(&[])).unwrap();
        let dest = dir.path().join("out");
        std::fs::create_dir(&dest).unwrap();
        let err = refused(&grant(dir.path()), &src, &dest, Format::TarGz);
        assert!(err.contains("already exists"), "{err}");
    }

    // ---- set-executable ------------------------------------------------

    #[test]
    fn set_executable_marks_a_granted_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("server");
        std::fs::write(&file, b"x").unwrap();
        set_executable(&grant(dir.path()), file.to_str().unwrap()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
        }
    }

    #[test]
    fn set_executable_names_each_refusal() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let outside = other.path().join("server");
        std::fs::write(&outside, b"x").unwrap();
        let g = grant(granted.path());

        let err = set_executable(&g, outside.to_str().unwrap()).expect_err("outside the grant");
        assert!(err.contains("denied"), "{err}");

        let err = set_executable(&g, granted.path().join("nope").to_str().unwrap())
            .expect_err("nothing there");
        assert!(
            !err.contains("denied"),
            "a missing file is not a denial: {err}"
        );

        let sub = granted.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let err = set_executable(&g, sub.to_str().unwrap()).expect_err("a directory");
        assert!(err.contains("not a file"), "{err}");
    }

    #[test]
    fn links_are_confined_lexically() {
        assert!(link_stays_inside(Path::new("a/b/link"), Path::new("../c")));
        assert!(link_stays_inside(Path::new("link"), Path::new("x/y")));
        assert!(!link_stays_inside(Path::new("link"), Path::new("../x")));
        assert!(!link_stays_inside(
            Path::new("a/link"),
            Path::new("../../x")
        ));
        assert!(!link_stays_inside(Path::new("a/link"), Path::new("/x")));
        // Climbing out and back in is still out.
        assert!(!link_stays_inside(
            Path::new("link"),
            Path::new("../out/in")
        ));
    }
}
