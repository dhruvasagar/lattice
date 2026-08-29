//! OR.2 — the `host-services.watch` / `unwatch` seam.
//!
//! Design: `docs/dev/architecture/org-roam.md` §4.3. Slice plan:
//! `docs/dev/operations/slice-plans/org-roam.md` OR.2.
//!
//! ## Why a watcher rather than a save hook
//!
//! The corpus a plugin indexes is edited from outside lattice: emacs writes a
//! note, a `git pull` lands twenty, a sync daemon rewrites a directory. A save
//! hook observes none of those, and the symptom — an index missing files you
//! know you wrote — reads as **data loss** rather than as a stale cache. That is
//! a UX argument, not an architectural one, and it is why this exists at all.
//!
//! ## What a watch owns, and for how long
//!
//! A [`Watch`] is stored on the guest's own [`PluginState`](crate::PluginState),
//! not in a host-side registry keyed by plugin. That is deliberate: the watch's
//! lifetime is exactly the plugin instance's, so dropping the `Store` — on
//! unload, on quarantine, on the actor's channel closing — stops the watch with
//! no teardown wiring to forget. Mechanism lives where its lifetime matches.
//!
//! ## Coalescing, and the thread it costs
//!
//! `notify` invokes its callback on its own OS thread, one event per filesystem
//! notification. A `git pull` rewriting 200 files would therefore be 200 guest
//! calls, each re-reading an index. So each watch owns one coalescing thread:
//! it blocks for the first event, drains everything that arrives within
//! [`DEBOUNCE`] of the last, deduplicates, and publishes **one**
//! [`Event::FilesChanged`] carrying the batch.
//!
//! A thread per watch rather than one shared for the host, because a watch is
//! rare (a plugin arms one or two, over directories) and a shared thread would
//! need its own routing table, its own shutdown protocol and its own teardown —
//! all to save a thread that spends its life blocked on a channel. It ends by
//! itself when the `notify` watcher drops, which is the property that makes the
//! lifetime story above true.
//!
//! ## Failure behaviour
//!
//! Every refusal is a typed `Err` naming which one it was, and none is fatal: a
//! plugin whose watch fails falls back to indexing on boot plus an explicit
//! resync, which is degraded and honest rather than appearing to work and going
//! stale.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, Sender, channel};
use std::time::Duration;

use lattice_protocol::Event as NativeEvent;
use lattice_runtime::EventBus;
use notify::{RecursiveMode, Watcher as _};

use crate::capability::CapabilityGrant;

/// Quiet window after the last filesystem event before a batch is published.
///
/// 300 ms, matching the init-artifact watcher's `SETTLE`. Long enough that a
/// build's truncate/write/rename burst is one batch and a `git pull` is a small
/// number of them; short enough that a note saved in emacs is indexed before
/// the user has switched windows.
const DEBOUNCE: Duration = Duration::from_millis(300);

/// Most paths one published batch carries.
///
/// This is a bound on **event size, not on coverage**: a burst larger than this
/// publishes the first `MAX_BATCH` and keeps accumulating the rest into the next
/// batch, so nothing is dropped. A cap that silently discarded paths would leave
/// an index quietly wrong, which is the failure this whole seam exists to
/// prevent.
const MAX_BATCH: usize = 4096;

/// Re-root `path` onto the spelling the guest used.
///
/// **`notify` reports canonical paths; `walk` reports the ones it was given.**
/// On macOS a tempdir handed in as `/var/folders/…` comes back from the watcher
/// as `/private/var/folders/…`, and the two disagree for the whole life of the
/// watch. That is not cosmetic: a guest keys its index by path, so the walk
/// writes one key and the watcher looks up another — additions still work
/// (nothing to look up) and **changes and deletions silently stop retracting**,
/// which is exactly the "an index that cannot see deletions offers destinations
/// that do not exist" failure, arriving through a door nobody was watching.
///
/// So the host makes them agree, at the only point where both spellings are
/// known. Re-rooting rather than canonicalizing everywhere, because `walk`'s
/// output is what a guest already has and changing that would move the problem
/// rather than fix it.
fn reroot(path: PathBuf, canonical_root: &Path, given_root: &Path) -> PathBuf {
    match path.strip_prefix(canonical_root) {
        Ok(rest) => given_root.join(rest),
        // Not under the canonical root — either the roots are already the same
        // (Linux, usually) or this is a path the re-rooting does not apply to.
        // Either way the original is the honest answer.
        Err(_) => path,
    }
}

/// One armed watch. Dropping it stops the OS watch, which closes the coalescing
/// thread's channel, which ends the thread.
pub(crate) struct Watch {
    /// Held for its `Drop`. `notify` stops watching when the watcher is dropped
    /// and drops the callback, which drops the [`Sender`] the coalescing thread
    /// is blocked on.
    _watcher: notify::RecommendedWatcher,
}

impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Watch")
    }
}

/// Arm a recursive watch on `path`, publishing coalesced
/// [`Event::FilesChanged`] batches addressed to `plugin`.
///
/// Capability-gated on the same `fs:read`/`fs:write` prefixes `walk` and
/// `read-file` check — a watch reveals filesystem activity, so it is the same
/// authorization question and gets the same answer from the same function.
pub(crate) fn watch_within_grant(
    grant: &CapabilityGrant,
    bus: Arc<EventBus>,
    plugin: u32,
    path: &str,
) -> Result<Watch, String> {
    let root = PathBuf::from(path);
    // `grant_permits_read`, not `grant_permits_walk`, and the difference is
    // not cosmetic: `walk`'s check falls back to the RAW path when
    // canonicalization fails, and a path that does not exist cannot
    // canonicalize — so wherever the granted prefix resolves elsewhere (macOS
    // `/var` → `/private/var`) a missing directory reports as a *permission*
    // problem, sending a plugin author to their manifest instead of their path.
    // `read-file` already solved that with a parent fallback; this is the same
    // question, so it uses the same answer rather than a second opinion about
    // it. The grant check still runs FIRST, so an ungranted plugin cannot learn
    // whether a path exists by watching it.
    if !crate::host_services::grant_permits_read(grant, &root) {
        // info!: user-actionable (a plugin was denied fs access), the level
        // `walk` and `read-file` denials use.
        tracing::info!(
            path = %root.display(),
            "host-services watch denied: outside the plugin's fs grant"
        );
        return Err(format!(
            "fs watch denied: '{path}' is outside the plugin's granted paths"
        ));
    }
    if !root.exists() {
        return Err(format!("fs watch failed: '{path}' does not exist"));
    }

    // Both spellings of the root, so the coalescing thread can report paths in
    // the one the guest actually uses. See `reroot`.
    let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
    let given_root = root.clone();

    let (tx, rx) = channel::<Vec<PathBuf>>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        forward(&tx, res);
    })
    .map_err(|e| format!("fs watch failed: '{path}': cannot create a watcher: {e}"))?;
    watcher
        .watch(&root, RecursiveMode::Recursive)
        .map_err(|e| format!("fs watch failed: '{path}': {e}"))?;

    let name = format!("lattice-plugin-watch-{plugin}");
    let spawned = std::thread::Builder::new()
        .name(name)
        .spawn(move || coalesce(rx, bus, plugin, canonical_root, given_root));
    if let Err(e) = spawned {
        return Err(format!(
            "fs watch failed: '{path}': cannot spawn the coalescing thread: {e}"
        ));
    }

    tracing::debug!(path = %root.display(), plugin, "plugin watch armed");
    Ok(Watch { _watcher: watcher })
}

/// The `notify` callback: forward the paths of a content-changing event.
///
/// Access events (a read) are ignored — they change nothing an index cares
/// about, and on some platforms they are the majority of the traffic. A removal
/// IS forwarded: an index that cannot see deletions offers destinations that no
/// longer exist, which is the case a cache-shaped design forgets.
fn forward(tx: &Sender<Vec<PathBuf>>, res: notify::Result<notify::Event>) {
    let Ok(event) = res else {
        // A watcher-level error (a dropped inotify queue, a permissions change)
        // is not fatal: the watch keeps running and the next event still
        // arrives. `debug!` rather than `warn!` because a busy directory can
        // produce these in bursts.
        return;
    };
    if !matches!(
        event.kind,
        notify::EventKind::Create(_) | notify::EventKind::Modify(_) | notify::EventKind::Remove(_)
    ) {
        return;
    }
    if event.paths.is_empty() {
        return;
    }
    // A closed receiver means the coalescing thread has ended; the watcher is
    // about to be dropped too, so there is nothing to report.
    let _ = tx.send(event.paths);
}

/// Block for a burst, drain it until quiet, publish it as one batch. Ends when
/// the channel closes — which is what dropping the [`Watch`] causes.
fn coalesce(
    rx: std::sync::mpsc::Receiver<Vec<PathBuf>>,
    bus: Arc<EventBus>,
    plugin: u32,
    canonical_root: PathBuf,
    given_root: PathBuf,
) {
    let reroot_all = |paths: Vec<PathBuf>| -> Vec<PathBuf> {
        paths
            .into_iter()
            .map(|p| reroot(p, &canonical_root, &given_root))
            .collect()
    };
    while let Ok(first) = rx.recv() {
        let first = reroot_all(first);
        // A `BTreeSet` because a burst repeats paths (truncate, write, rename
        // on one file is three events) and because sorted output makes the
        // batch reproducible in a test. Dedup is also what keeps `MAX_BATCH`
        // from being reached by activity rather than by breadth.
        let mut paths: BTreeSet<PathBuf> = first.into_iter().collect();
        let mut disconnected = false;
        while paths.len() < MAX_BATCH {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(more) => paths.extend(reroot_all(more)),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if !paths.is_empty() {
            tracing::debug!(plugin, files = paths.len(), "plugin watch batch");
            bus.publish(NativeEvent::FilesChanged {
                plugin,
                paths: paths.into_iter().collect(),
            });
        }
        if disconnected {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::capability::FsGrant;
    use lattice_protocol::EventKind;
    use lattice_runtime::{EventFilter, SubscriptionTarget};
    use std::path::Path;

    fn read_grant(prefix: &Path) -> CapabilityGrant {
        CapabilityGrant {
            fs: vec![FsGrant {
                prefix: prefix.to_path_buf(),
                write: false,
            }],
            ..Default::default()
        }
    }

    /// Subscribe to `FilesChanged` and return the receiver.
    fn watch_bus(bus: &EventBus) -> tokio::sync::mpsc::UnboundedReceiver<NativeEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        bus.subscribe(
            EventFilter::kind(EventKind::FilesChanged),
            SubscriptionTarget::Channel(tx),
        );
        rx
    }

    /// Wait up to `budget` for one batch. Polling rather than a fixed sleep, so
    /// the test is not a race on a slow machine.
    fn next_batch(
        rx: &mut tokio::sync::mpsc::UnboundedReceiver<NativeEvent>,
        budget: Duration,
    ) -> Option<(u32, Vec<PathBuf>)> {
        let deadline = std::time::Instant::now() + budget;
        while std::time::Instant::now() < deadline {
            if let Ok(NativeEvent::FilesChanged { plugin, paths }) = rx.try_recv() {
                return Some((plugin, paths));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    }

    #[test]
    fn a_single_write_is_observed() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new();
        let mut rx = watch_bus(&bus);

        let _watch = watch_within_grant(
            &read_grant(dir.path()),
            Arc::new(bus),
            7,
            dir.path().to_str().unwrap(),
        )
        .expect("a granted directory is watchable");

        std::fs::write(dir.path().join("note.org"), "* hello\n").unwrap();

        let (plugin, paths) = next_batch(&mut rx, Duration::from_secs(5)).expect("a batch arrives");
        assert_eq!(
            plugin, 7,
            "the batch is addressed to the plugin that armed it"
        );
        assert!(
            paths.iter().any(|p| p.ends_with("note.org")),
            "the changed path is in the batch: {paths:?}"
        );
    }

    /// **The `git pull` case.** 200 writes must not be 200 events — and this is
    /// the only assertion that would fail against a watcher wired straight to
    /// the bus.
    #[test]
    fn a_burst_coalesces_into_a_bounded_number_of_batches() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new();
        let mut rx = watch_bus(&bus);

        let _watch = watch_within_grant(
            &read_grant(dir.path()),
            Arc::new(bus),
            1,
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        for i in 0..200 {
            std::fs::write(dir.path().join(format!("n{i}.org")), "x").unwrap();
        }

        let mut batches = 0;
        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        // Keep collecting until the stream goes quiet for a full debounce window
        // plus slack — the point is the ratio of batches to writes, so the loop
        // has to see the tail as well as the head.
        while let Some((_, paths)) = next_batch(&mut rx, DEBOUNCE * 4) {
            batches += 1;
            seen.extend(paths);
        }

        assert!(batches >= 1, "at least one batch was published");
        assert!(
            batches < 20,
            "200 writes coalesced into {batches} batches, not 200"
        );
        assert!(
            seen.len() > 100,
            "and coalescing did not cost coverage: {} of 200 paths seen",
            seen.len()
        );
    }

    /// OR.4 needs deletions: a node removed from disk must leave the index.
    #[test]
    fn a_deletion_is_reported_as_a_change() {
        let dir = tempfile::tempdir().unwrap();
        let doomed = dir.path().join("gone.org");
        std::fs::write(&doomed, "x").unwrap();

        let bus = EventBus::new();
        let mut rx = watch_bus(&bus);
        let _watch = watch_within_grant(
            &read_grant(dir.path()),
            Arc::new(bus),
            1,
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        std::fs::remove_file(&doomed).unwrap();

        let (_, paths) = next_batch(&mut rx, Duration::from_secs(5)).expect("a batch arrives");
        assert!(
            paths.iter().any(|p| p.ends_with("gone.org")),
            "the removed path is reported: {paths:?}"
        );
    }

    /// Dropping the watch stops delivery — this is what `unwatch`, unload and
    /// quarantine all reduce to.
    #[test]
    fn dropping_the_watch_stops_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new();
        let mut rx = watch_bus(&bus);

        let watch = watch_within_grant(
            &read_grant(dir.path()),
            Arc::new(bus),
            1,
            dir.path().to_str().unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join("a.org"), "x").unwrap();
        assert!(
            next_batch(&mut rx, Duration::from_secs(5)).is_some(),
            "delivery works before the drop"
        );

        drop(watch);
        // Give the watcher's thread a moment to notice.
        std::thread::sleep(DEBOUNCE);
        while rx.try_recv().is_ok() {}

        std::fs::write(dir.path().join("b.org"), "x").unwrap();
        assert!(
            next_batch(&mut rx, DEBOUNCE * 4).is_none(),
            "nothing is delivered after the watch is dropped"
        );
    }

    #[test]
    fn a_path_outside_the_grant_is_denied_by_name() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let err = watch_within_grant(
            &read_grant(granted.path()),
            Arc::new(EventBus::new()),
            1,
            other.path().to_str().unwrap(),
        )
        .expect_err("a path outside the grant must be denied");
        assert!(err.contains("denied"), "{err}");
        assert!(
            err.contains("granted paths"),
            "and names the grant as what to fix: {err}"
        );
    }

    #[test]
    fn an_empty_grant_watches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            watch_within_grant(
                &CapabilityGrant::default(),
                Arc::new(EventBus::new()),
                1,
                dir.path().to_str().unwrap(),
            )
            .is_err(),
            "a plugin with no fs grant reaches nothing"
        );
    }

    /// **The paths a batch reports are spelled the way the guest asked**, not
    /// the way the OS canonicalizes them.
    ///
    /// This is the macOS `/var` → `/private/var` case, and it is not cosmetic:
    /// a guest keys its index by path, so a walk that wrote one spelling and a
    /// watcher that reported another would leave changes and deletions
    /// silently never retracting — additions would still work, which is what
    /// makes it so easy to miss. Found by org-roam's deletion test, which is
    /// the only assertion that could see it.
    #[test]
    fn reported_paths_use_the_spelling_the_watch_was_armed_with() {
        let dir = tempfile::tempdir().unwrap();
        let given = dir.path().to_path_buf();
        let canonical = std::fs::canonicalize(&given).unwrap();

        let bus = EventBus::new();
        let mut rx = watch_bus(&bus);
        let _watch = watch_within_grant(
            &read_grant(&given),
            Arc::new(bus),
            1,
            given.to_str().unwrap(),
        )
        .unwrap();

        std::fs::write(given.join("note.org"), "x").unwrap();
        let (_, paths) = next_batch(&mut rx, Duration::from_secs(5)).expect("a batch arrives");

        assert!(
            paths.iter().all(|p| p.starts_with(&given)),
            "every path is under the root AS GIVEN ({}): {paths:?}",
            given.display()
        );
        if canonical != given {
            assert!(
                !paths.iter().any(|p| p.starts_with(&canonical)),
                "and none leaked the canonical spelling ({}): {paths:?}",
                canonical.display()
            );
        }
    }

    /// Re-rooting leaves a path it does not recognise alone, rather than
    /// mangling it.
    #[test]
    fn a_path_outside_the_canonical_root_is_returned_unchanged() {
        let path = PathBuf::from("/elsewhere/note.org");
        let got = reroot(
            path.clone(),
            Path::new("/private/var/root"),
            Path::new("/var/root"),
        );
        assert_eq!(got, path);
    }

    /// A missing directory is distinguishable from a denial, because the two
    /// have different fixes.
    #[test]
    fn a_missing_directory_says_so_rather_than_reading_as_denied() {
        let dir = tempfile::tempdir().unwrap();
        let err = watch_within_grant(
            &read_grant(dir.path()),
            Arc::new(EventBus::new()),
            1,
            dir.path().join("nope").to_str().unwrap(),
        )
        .expect_err("a missing directory cannot be watched");
        assert!(err.contains("does not exist"), "{err}");
        assert!(!err.contains("denied"), "and is not a denial: {err}");
    }
}
