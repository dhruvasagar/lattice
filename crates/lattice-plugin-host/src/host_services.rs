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

/// True if `root` lies within one of the grant's fs prefixes (read *or* write —
/// a walk only reads). Both sides are canonicalized first so a `..` segment
/// cannot escape a granted prefix. If canonicalization fails (e.g. the path does
/// not exist), the raw path is used: that still requires a literal prefix match,
/// so it can never *widen* the grant — at worst it denies a walk that a
/// resolvable path would have permitted, which fails safe.
fn grant_permits_walk(grant: &CapabilityGrant, root: &Path) -> bool {
    let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    grant.fs.iter().any(|g| {
        let canon_prefix = std::fs::canonicalize(&g.prefix).unwrap_or_else(|_| g.prefix.clone());
        canon_root.starts_with(&canon_prefix)
    })
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
