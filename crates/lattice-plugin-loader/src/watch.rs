//! PL8.D.4 — auto-reload the user's `init.rs` config when its compiled artifact
//! changes on disk.
//!
//! The edit→rebuild→reload loop shouldn't need a manual `:reload-config`. This
//! watches `<config>/lattice/init/` with the `notify` crate and calls
//! [`PluginLoader::sync_init`] (load-or-reload) whenever the directory changes —
//! so after you `cargo build` a new `init.wasm`, the editor picks it up.
//!
//! **Why the artifact, not the source.** `init.rs` is Rust *source*; the editor
//! loads the compiled `init.wasm`. Saving the source doesn't change the artifact,
//! so the watch is on the directory holding the `.wasm` (a `:plugin-build`
//! command that compiles source → artifact is future work — until then you build
//! externally and this fires on the resulting write).
//!
//! **Debounce.** A build rewrites the file in a burst (truncate, write, rename);
//! the watcher coalesces events, waiting for [`SETTLE`] of quiet before it
//! reloads once, so a single rebuild triggers a single reload.
//!
//! **Broken build heals.** `sync_init` reloads when `init` is loaded and loads
//! when it isn't; a rebuild that produces a bad component leaves `init` unloaded
//! (logged), and the next good build's write fires this again and loads it. No
//! restart needed either way.

use std::path::PathBuf;
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::mpsc;

use lattice_plugin_host::TrustTier;

use crate::PluginLoaderHandle;

/// Quiet window after the last fs event before reloading — coalesces a build's
/// event burst into one reload.
const SETTLE: Duration = Duration::from_millis(300);

/// Spawn the init-artifact auto-reload watcher on `runtime`. Watches `init_dir`
/// (non-recursively) and `sync_init`s the loader on every settled change. A
/// watcher that can't be created / can't watch the dir is a logged warning that
/// disables auto-reload (manual `:reload-config` still works), never a failure.
/// No-op if `init_dir` doesn't exist (nothing to watch until the user creates a
/// config; a restart picks it up).
pub fn spawn_init_watcher(loader: PluginLoaderHandle, init_dir: PathBuf, runtime: &Handle) {
    if !init_dir.is_dir() {
        return;
    }
    runtime.spawn(async move {
        // `notify` invokes its callback on its own OS thread; forward a unit
        // "something changed" tick to this async task over an mpsc. Content /
        // ordering don't matter — the task debounces + reloads from disk.
        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                // Create / modify / rename land a new artifact; access events
                // (reads) don't and are ignored.
                if matches!(
                    event.kind,
                    notify::EventKind::Create(_)
                        | notify::EventKind::Modify(_)
                        | notify::EventKind::Remove(_)
                ) {
                    let _ = tx.send(());
                }
            }
        }) {
            Ok(w) => w,
            Err(err) => {
                tracing::warn!(error = %err, "init auto-reload disabled: cannot create fs watcher");
                return;
            }
        };
        if let Err(err) = notify::Watcher::watch(
            &mut watcher,
            &init_dir,
            notify::RecursiveMode::NonRecursive,
        ) {
            tracing::warn!(
                dir = %init_dir.display(),
                error = %err,
                "init auto-reload disabled: cannot watch the init dir"
            );
            return;
        }

        // The watcher must outlive the loop (dropping it stops watching); it is
        // owned here for the task's lifetime.
        tracing::debug!(dir = %init_dir.display(), "watching init config for changes");
        while rx.recv().await.is_some() {
            // Settle: keep draining while more events arrive within `SETTLE`, so
            // one rebuild's event burst collapses to one reload. Breaks on quiet
            // (`Err(Elapsed)`) or channel close (`Ok(None)`).
            while let Ok(Some(())) = tokio::time::timeout(SETTLE, rx.recv()).await {
                // more events arrived within the window — keep waiting
            }
            tracing::info!(dir = %init_dir.display(), "init config changed; reloading");
            match loader.sync_init(&init_dir, TrustTier::Bundled).await {
                Ok(id) => tracing::info!(id = id.0, "init config auto-reloaded"),
                Err(err) => tracing::warn!(
                    error = %err,
                    "init config auto-reload failed (fix the build and rebuild — the next good build reloads it)"
                ),
            }
        }
    });
}
