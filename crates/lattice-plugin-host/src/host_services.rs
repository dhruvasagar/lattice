//! The `host-services` guest→host seam (plugin-host.md §5) — PH7.4b.
//!
//! The first call direction *into* the host: a plugin asks the host to do
//! something on its behalf, capability-gated against the plugin's
//! [`CapabilityGrant`](crate::capability::CapabilityGrant) (PH7.2). This is
//! distinct from the guest's WASI filesystem view: that view is sandboxed by the
//! `Store`'s preopens, so a guest cannot reach outside its grant even if it tries.
//! A host-services call, by contrast, runs **host-side with full host authority**
//! — the host process is not sandboxed — so the grant check is mandatory *here*,
//! not delegated to WASI. Enforcing it is the whole point of the seam.
//!
//! PH7.4b lands one function, [`walk_within_grant`], the capability-gated
//! workspace enumeration the `fuzzy-finder` (PH7.4d) uses to replicate the native
//! `files` picker. It reuses the native walker's policy so a plugin source and a
//! first-party source enumerate identically. The `Host` trait impl + linker
//! wiring live in `lib.rs` (next to `PluginState`, which carries the grant); this
//! module holds the gate + walk logic so it is unit-testable without a `Store`.

use std::path::{Path, PathBuf};

use lattice_protocol::Event as NativeEvent;
use lattice_protocol::event_registry::register_runtime_event;
use lattice_runtime::EventBus;

use crate::PluginId;
use crate::capability::CapabilityGrant;

/// The `register-event` host-service body (PH7.8b) — declare a plugin-defined
/// event. Stamps the plugin's provenance (`plugin:<id>`) as the source so the
/// event is attributed to its owner in `:describe-event(s)`, then delegates to
/// the process-wide [`register_runtime_event`]. Returns `false` (registering
/// nothing) when `name` would shadow a BUILT-IN event — a plugin must not hijack
/// a native event's subscribers. Factored here (the [`walk_within_grant`]
/// precedent) so the provenance formatting is unit-testable without a `Store`.
pub(crate) fn register_plugin_event(plugin: PluginId, name: &str, doc: &str) -> bool {
    register_runtime_event(name, doc, format!("plugin:{}", plugin.0))
}

/// The `emit-event` host-service body (PH7.8b) — publish a plugin-defined event
/// on `bus`. The host is a thin router: `payload` is opaque MessagePack the
/// plugin owns; it crosses onto the bus as [`NativeEvent::Plugin`] verbatim and
/// the host NEVER interprets it. Fire-and-forget (the bus is observation-only,
/// §5.10): there is no reply. Subscribers filter by `name` in their handler.
pub(crate) fn emit_plugin_event(bus: &EventBus, name: String, payload: Vec<u8>) {
    bus.publish(NativeEvent::Plugin { name, payload });
}

/// The `local-utc-offset-seconds` host-service body (OC.4) — the host's offset
/// from UTC right now, east-positive.
///
/// **Resolved per call, not cached.** The offset is not a constant: it changes
/// at a DST boundary, and it changes if the user changes their system timezone
/// while the editor is running. Caching it would make an editor left open
/// overnight write clock lines an hour wrong for the rest of the session — the
/// exact bug class this seam exists to prevent, reintroduced as an optimisation
/// of a call org makes twice a minute.
///
/// Not capability-gated (see the WIT): it names no path and reaches no resource,
/// and gating it would mean a plugin with no filesystem grant renders every
/// timestamp in the wrong timezone.
pub(crate) fn local_utc_offset_seconds() -> i32 {
    chrono::Local::now().offset().local_minus_utc()
}

/// The `new-uuid` host-service body (OR.3) — a random (v4) UUID, uppercase,
/// canonical `8-4-4-4-12` form.
///
/// **Host-side for [`read_within_grant`]'s exact reason.** `:org-roam-id-create`
/// is a grammar action: it runs on the grammar seam's *synchronous* linker,
/// where `wasmtime-wasi`'s sync shim blocks on a runtime internally and panics
/// on a thread already inside one. A guest minting its own id through
/// `wasi:random` would work on the async picker path and take the plugin down on
/// the grammar path — correct in every test that builds its own context, broken
/// in the editor.
///
/// **Hand-rolled rather than the `uuid` crate**, following the precedent the
/// workspace already set for `getrandom` (`lattice-ai`'s MCP session token: "a
/// minimal, vetted CSPRNG primitive, no heavier `rand`/`uuid` dep pulled for one
/// token"). v4 is sixteen random bytes with six bits pinned; the formatting is
/// one `write!`. A dependency would buy parsing, versions 1/3/5/7 and a `Uuid`
/// type, none of which crosses a WIT `string`.
///
/// **`Err`, not a degraded value**, and this is the one place on this seam where
/// that is the right shape. Its neighbours answer `0` when unwired
/// (`wake-every`, `local-utc-offset-seconds`) on the argument that a legible
/// wrong answer beats a fabricated one — but those values are *read*. An id is
/// **written**, into the user's own file, as an `:ID:` that outlives the session
/// and every other tool's view of that note. A guest handed an empty string on
/// entropy failure would write an empty drawer and nothing would ever say so.
/// One `match` at the two call sites buys that being impossible.
///
/// Not a panic either: a host function that unwinds through wasm frames aborts
/// the process, which is a worse answer than a plugin reporting that it could
/// not mint an id.
pub(crate) fn new_uuid() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    if let Err(error) = getrandom::getrandom(&mut bytes) {
        // error!: genuinely user-actionable and genuinely one-shot — the OS
        // entropy source being unavailable is not a per-keystroke condition.
        tracing::error!(%error, "new-uuid: the OS entropy source is unavailable");
        return Err(format!("cannot mint an id without entropy: {error}"));
    }
    // RFC 4122 §4.4: version 4 in the high nibble of byte 6, variant 10xx in
    // the two high bits of byte 8. Everything else stays random.
    bytes[6] = (bytes[6] & 0x0F) | 0x40;
    bytes[8] = (bytes[8] & 0x3F) | 0x80;

    let hex = |b: &[u8]| -> String { b.iter().map(|x| format!("{x:02X}")).collect() };
    Ok(format!(
        "{}-{}-{}-{}-{}",
        hex(&bytes[0..4]),
        hex(&bytes[4..6]),
        hex(&bytes[6..8]),
        hex(&bytes[8..10]),
        hex(&bytes[10..16]),
    ))
}

/// True if `root` lies within one of the grant's fs prefixes (read *or* write —
/// a walk only reads). Both sides are canonicalized first so a `..` segment
/// cannot escape a granted prefix. If canonicalization fails (e.g. the path does
/// not exist), the raw path is used: that still requires a literal prefix match,
/// so it can never *widen* the grant — at worst it denies a walk that a
/// resolvable path would have permitted, which fails safe.
pub(crate) fn grant_permits_walk(grant: &CapabilityGrant, root: &Path) -> bool {
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    grant.fs.iter().any(|g| {
        let canon_prefix = std::fs::canonicalize(&g.prefix).unwrap_or_else(|_| g.prefix.clone());
        canon_root.starts_with(&canon_prefix)
    })
}

/// The same check for a FILE — on the file itself when it exists, on its parent
/// only when it does not.
///
/// **The file itself first, and that ordering is a security property.**
/// Canonicalizing resolves symlinks, so `<granted>/innocent.org` pointing at
/// `/etc/passwd` resolves outside the prefix and is refused. Gating on the
/// parent alone would pass it — the parent *is* granted — and the read would
/// then follow the link straight out of the sandbox. Pinned by
/// `a_symlink_out_of_the_grant_is_denied`, which failed against exactly that
/// mistake before this ordering existed.
///
/// The parent fallback exists only for a path that does not resolve, and there
/// it fixes a different wrong answer. [`grant_permits_walk`] falls back to the
/// raw path, and wherever the granted prefix canonicalizes elsewhere (macOS
/// `/var` → `/private/var`) the comparison fails and the call is refused — so a
/// file that simply does not exist yet reports as a *permission* problem,
/// sending a plugin author to their manifest instead of their path. "Is there
/// anything in this file yet?" is the ordinary first-capture case and deserves
/// a truthful answer.
///
/// The fallback cannot be used to smuggle anything past the check: it only
/// applies when nothing is there to read, so the subsequent `read` fails
/// regardless. `<granted>/../../etc/passwd` resolves as a file and is denied on
/// the first branch.
pub fn grant_permits_read(grant: &CapabilityGrant, file: &Path) -> bool {
    if file.exists() {
        return grant_permits_walk(grant, file);
    }
    match file.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => grant_permits_walk(grant, parent),
        _ => grant_permits_walk(grant, file),
    }
}

/// OC.5a: capability-gated file read (host-side, §5).
///
/// The reason this exists rather than the guest using WASI is structural, not a
/// convenience: the grammar seam runs on a **synchronous** linker so an action
/// can be called on the dispatch thread, and `wasmtime-wasi`'s sync filesystem
/// shim blocks on a runtime internally — which panics on a thread already inside
/// one. A grammar action reading through WASI therefore takes the plugin down
/// instead of returning bytes. Async seams (pickers, completion) are unaffected;
/// this is the read that works everywhere.
///
/// Same grant check as [`walk_within_grant`], for the same reason: the host has
/// ambient authority the guest's sandbox would otherwise have bounded.
///
/// Three distinct failures rather than one, because a plugin author debugging
/// "the read failed" needs to know whether to fix their manifest, their path, or
/// their expectations about the file's encoding.
pub(crate) fn read_within_grant(grant: &CapabilityGrant, path: &str) -> Result<String, String> {
    let file = PathBuf::from(path);
    if !grant_permits_read(grant, &file) {
        // info!: user-actionable (a plugin was denied fs access), not per-frame
        // noise — the log-levels rule (CLAUDE.md). Matches `walk`'s level.
        tracing::info!(
            path = %file.display(),
            "host-services read denied: outside the plugin's fs grant"
        );
        return Err(format!(
            "fs read denied: '{path}' is outside the plugin's granted paths"
        ));
    }
    match std::fs::read(&file) {
        Ok(bytes) => String::from_utf8(bytes)
            .map_err(|_| format!("fs read failed: '{path}' is not valid UTF-8")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Not logged. A caller asking whether a file exists yet — a first
            // capture into a new file — would otherwise fill the log with
            // events that are the ordinary case, not a problem.
            Err(format!("fs read failed: '{path}' does not exist"))
        }
        Err(e) => Err(format!("fs read failed: '{path}': {e}")),
    }
}

/// Capability-gated workspace walk (host-side, §5). Returns absolute UTF-8 paths
/// under `root`, applying the native file-picker policy (bounded entry count;
/// skips `.git`/`target`/`node_modules`/`dist`/`.cache` and dotfiles) so a plugin
/// source enumerates identically to the first-party `files` source.
///
/// `root` must lie within one of the plugin's granted `fs:read`/`fs:write`
/// prefixes; otherwise the call is a typed `Err` (echoed to the user, §4) and the
/// denial is logged. A plugin with no fs grant reaches nothing. A non-UTF-8 path
/// is skipped (it cannot cross as a WIT `string`), never an error — one
/// oddly-named file must not fail the whole walk.
pub(crate) fn walk_within_grant(
    grant: &CapabilityGrant,
    root: &str,
) -> Result<Vec<String>, String> {
    let root_path = PathBuf::from(root);
    if !grant_permits_walk(grant, &root_path) {
        // info!: user-actionable (a plugin was denied fs access), not per-frame
        // noise — the log-levels rule (CLAUDE.md).
        tracing::info!(
            path = %root_path.display(),
            "host-services walk denied: outside the plugin's fs grant"
        );
        return Err(format!(
            "fs walk denied: '{root}' is outside the plugin's granted paths"
        ));
    }
    let paths = lattice_picker::picker_sources::walk_files_for_picker(&root_path);
    Ok(paths
        .into_iter()
        .filter_map(|p| p.to_str().map(str::to_string))
        .collect())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::capability::FsGrant;

    /// Build a grant that reads exactly `prefix`.
    fn read_grant(prefix: PathBuf) -> CapabilityGrant {
        CapabilityGrant {
            fs: vec![FsGrant {
                prefix,
                write: false,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn walk_returns_files_within_grant() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        std::fs::write(dir.path().join("b.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/c.rs"), "").unwrap();

        let grant = read_grant(dir.path().to_path_buf());
        let out = walk_within_grant(&grant, dir.path().to_str().unwrap()).unwrap();

        assert_eq!(out.len(), 3, "walks recursively: {out:?}");
        assert!(out.iter().all(|p| p.ends_with(".rs")));
        assert!(out.iter().any(|p| p.ends_with("sub/c.rs")));
    }

    #[test]
    fn read_file_returns_the_contents_within_grant() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.org");
        std::fs::write(&file, "* Tasks\nbody\n").unwrap();

        let grant = read_grant(dir.path().to_path_buf());
        let out = read_within_grant(&grant, file.to_str().unwrap()).unwrap();
        assert_eq!(out, "* Tasks\nbody\n");
    }

    /// Same gate as `walk`, and it matters more here: `read-file` returns file
    /// CONTENTS, so a missing check leaks the contents of any file the editor
    /// process can reach.
    #[test]
    fn read_file_outside_the_grant_is_a_typed_error() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let secret = other.path().join("secret");
        std::fs::write(&secret, "private").unwrap();

        let grant = read_grant(granted.path().to_path_buf());
        let err = read_within_grant(&grant, secret.to_str().unwrap())
            .expect_err("a path outside the grant must be denied");
        assert!(err.contains("denied"), "error explains the denial: {err}");
        assert!(
            !err.contains("private"),
            "and the denial does not leak what it refused to read: {err}"
        );
    }

    /// **A symlink inside the granted directory must not read outside it.**
    ///
    /// The gate canonicalizes so that a path resolving out of the grant is
    /// refused; gating on the parent directory alone would pass this — the
    /// parent IS granted — and then `read` would follow the link. That is a
    /// capability bypass, not a cosmetic bug: the whole point of the grant is
    /// that a plugin reaches only what it was given.
    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_grant_is_denied() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let secret = other.path().join("secret");
        std::fs::write(&secret, "private").unwrap();

        let link = granted.path().join("innocent.org");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let grant = read_grant(granted.path().to_path_buf());
        let err = read_within_grant(&grant, link.to_str().unwrap())
            .expect_err("a symlink escaping the grant must be denied");
        assert!(err.contains("denied"), "{err}");
        assert!(!err.contains("private"), "and leaks nothing: {err}");
    }

    /// A symlink that stays INSIDE the grant is fine — the check is about where
    /// the target lands, not about symlinks being suspicious.
    #[cfg(unix)]
    #[test]
    fn a_symlink_within_the_grant_is_permitted() {
        let granted = tempfile::tempdir().unwrap();
        let real = granted.path().join("real.org");
        std::fs::write(&real, "* Tasks\n").unwrap();
        let link = granted.path().join("alias.org");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let grant = read_grant(granted.path().to_path_buf());
        assert_eq!(
            read_within_grant(&grant, link.to_str().unwrap()).unwrap(),
            "* Tasks\n"
        );
    }

    /// A plugin with no fs grant reads nothing — the default posture.
    #[test]
    fn read_file_with_an_empty_grant_reaches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.org");
        std::fs::write(&file, "x").unwrap();

        let grant = CapabilityGrant::default();
        assert!(read_within_grant(&grant, file.to_str().unwrap()).is_err());
    }

    /// Absence is distinguishable from denial. A caller for whom "not there
    /// yet" is an ordinary case — a first capture into a new file — must be
    /// able to tell it apart from a manifest it needs to fix.
    #[test]
    fn a_missing_file_says_so_rather_than_reading_as_denied() {
        let dir = tempfile::tempdir().unwrap();
        let grant = read_grant(dir.path().to_path_buf());
        let err = read_within_grant(&grant, dir.path().join("nope.org").to_str().unwrap())
            .expect_err("a missing file is an error");
        assert!(err.contains("does not exist"), "{err}");
        assert!(
            !err.contains("denied"),
            "and is NOT reported as a denial: {err}"
        );
    }

    /// A write grant implies read — it is the same directory, opened with
    /// `READ | MUTATE`. Org relies on this: capture already needs `fs:write`
    /// for the file it is about to append to, and OC.5a reads that same file to
    /// find the headline. Requiring a second grant for it would be ceremony.
    #[test]
    fn a_write_grant_also_permits_reading() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.org");
        std::fs::write(&file, "* Tasks\n").unwrap();

        let grant = CapabilityGrant {
            fs: vec![FsGrant {
                prefix: dir.path().to_path_buf(),
                write: true,
            }],
            ..Default::default()
        };
        assert_eq!(
            read_within_grant(&grant, file.to_str().unwrap()).unwrap(),
            "* Tasks\n"
        );
    }

    #[test]
    fn walk_outside_the_grant_is_a_typed_error() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("secret"), "").unwrap();

        let grant = read_grant(granted.path().to_path_buf());
        let err = walk_within_grant(&grant, other.path().to_str().unwrap())
            .expect_err("a path outside the grant must be denied");
        assert!(err.contains("denied"), "error explains the denial: {err}");
    }

    #[test]
    fn empty_grant_reaches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "").unwrap();
        // A plugin with no fs grant (the default) can walk nothing.
        let err = walk_within_grant(&CapabilityGrant::default(), dir.path().to_str().unwrap())
            .expect_err("no grant reaches nothing");
        assert!(err.contains("denied"));
    }

    #[test]
    fn walk_applies_the_native_ignore_policy() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.rs"), "").unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/HEAD"), "").unwrap();
        std::fs::create_dir(dir.path().join("target")).unwrap();
        std::fs::write(dir.path().join("target/out"), "").unwrap();

        let grant = read_grant(dir.path().to_path_buf());
        let out = walk_within_grant(&grant, dir.path().to_str().unwrap()).unwrap();

        assert!(out.iter().any(|p| p.ends_with("keep.rs")));
        assert!(
            !out.iter()
                .any(|p| p.contains(".git") || p.contains("target")),
            "ignore dirs are skipped host-side: {out:?}"
        );
    }

    #[test]
    fn register_plugin_event_stamps_the_plugin_provenance() {
        use lattice_protocol::event_registry::{event_info_by_name, unregister_runtime_event};

        // A fresh, uniquely-named event registers and carries the plugin's
        // provenance (`plugin:<id>`) as its source — the piece host_services adds
        // on top of the registry (built-in-shadow rejection is `register_runtime_event`'s
        // own contract, covered in `event_registry`).
        let name = "host-services-test.custom-event";
        assert!(register_plugin_event(PluginId(42), name, "a test event"));
        let info = event_info_by_name(name).expect("registered");
        assert_eq!(info.source, "plugin:42");
        assert!(!info.builtin, "a plugin event is not a built-in");
        assert_eq!(info.doc, "a test event");
        unregister_runtime_event(name);
    }

    #[test]
    fn emit_plugin_event_publishes_to_a_native_subscriber() {
        use lattice_protocol::EventKind;
        use lattice_runtime::{EventFilter, SubscriptionTarget};

        let bus = EventBus::new();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::Plugin),
            SubscriptionTarget::Channel(tx),
        );

        emit_plugin_event(&bus, "my-plugin.indexed".into(), vec![1, 2, 3]);

        match rx.try_recv() {
            Ok(NativeEvent::Plugin { name, payload }) => {
                assert_eq!(name, "my-plugin.indexed");
                assert_eq!(payload, vec![1, 2, 3], "opaque bytes cross verbatim");
            }
            other => panic!("expected a Plugin event, got {other:?}"),
        }
    }

    #[test]
    fn a_subdirectory_of_a_granted_prefix_is_permitted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "").unwrap();
        // Grant the parent; walk a child — starts_with permits it.
        let grant = read_grant(dir.path().to_path_buf());
        let out = walk_within_grant(&grant, dir.path().join("src").to_str().unwrap()).unwrap();
        assert!(out.iter().any(|p| p.ends_with("src/main.rs")));
    }
}
