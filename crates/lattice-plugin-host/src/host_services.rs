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

use crate::capability::CapabilityGrant;

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
            !out.iter().any(|p| p.contains(".git") || p.contains("target")),
            "ignore dirs are skipped host-side: {out:?}"
        );
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
