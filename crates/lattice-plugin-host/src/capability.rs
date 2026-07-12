//! Trust tiers, grant computation, and the per-plugin WASI view.
//!
//! Design fragment: `docs/dev/architecture/plugin-host.md` §6. Slice: PH7.2.
//!
//! The pipeline is: **manifest (request) + trust tier → grant (effective) →
//! WASI view (enforcement)**. The manifest ([`crate::manifest`]) is what a
//! plugin *asks for*; the [`CapabilityGrant`] is what it *gets* after the trust
//! tier filters the request; the [`wasmtime_wasi::WasiCtx`] is how the grant is
//! *enforced* — a plugin's `Store` is built with exactly its granted
//! filesystem preopens, so a path outside the grant is unreachable at the WASI
//! layer (WASI has no ambient authority: only preopened dirs exist). That is
//! the "denied at the WASI layer, not by discipline" property the PH7.2 exit
//! names.
//!
//! **Scope of WASI enforcement at PH7.2 = filesystem only.** `net:http` and
//! `proc:spawn` are carried on the grant as metadata but are *not* wired into
//! the WASI view here: raw WASI sockets/subprocess would be a *broader* grant
//! than intended. Network and process access are serviced by capability-gated
//! `host-services` calls (PH7.3+), which check the grant's allowlist — so the
//! grant is the single source of truth both layers read. Enabling raw TCP for
//! a `net:http` grant would leak authority the host-services check exists to
//! contain, so `build_wasi_ctx` deliberately leaves sockets disabled.

use std::path::{Path, PathBuf};

use lattice_mode::CapabilitySet;
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder};

use crate::manifest::{Capability, PluginManifest};

/// The guest path the per-plugin data dir is mounted at. A plugin always has a
/// private, writable scratch dir here regardless of any `fs:*` grant.
pub const DATA_DIR_GUEST_MOUNT: &str = "/data";

/// How much the editor trusts a plugin — decides which requested capabilities
/// become grants. A plugin cannot self-declare this (it is not a manifest
/// field); the host determines it from install provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustTier {
    /// Shipped with the editor. Capabilities are pre-granted at build time,
    /// no consent prompt. `proc:spawn` is bundled-only in v1.
    Bundled,
    /// Installed by the user from an external source. Prompts for consent on
    /// first install (the prompt itself is a host-UI concern, out of this
    /// crate). `proc:spawn` is never granted in v1.
    UserInstalled,
}

/// A single granted filesystem prefix and its write bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsGrant {
    /// The host path prefix the plugin may reach.
    pub prefix: PathBuf,
    /// Whether the plugin may mutate under the prefix (`fs:write` vs `fs:read`).
    pub write: bool,
}

/// The **effective** capabilities a plugin is granted — the request filtered by
/// its trust tier. This, not the manifest, is what the runtime enforces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CapabilityGrant {
    /// Granted filesystem prefixes (enforced via WASI preopens).
    pub fs: Vec<FsGrant>,
    /// Granted outbound-HTTP host allowlist (enforced at the host-services
    /// http seam, PH7.3+, not the WASI layer — see the module note).
    pub net_http: Vec<String>,
    /// Whether the plugin may spawn subprocesses (enforced at the
    /// host-services proc seam, PH7.3+).
    pub proc_spawn: bool,
    /// The editor capabilities a plugin-declared mode requires (enforced at
    /// mode activation, PH7.11).
    pub editor: CapabilitySet,
}

/// The result of computing a grant: the effective [`CapabilityGrant`] plus the
/// requested capabilities that were **denied** by the trust tier, so the host
/// can surface a "loaded with reduced function" notification (fragment §6 UX;
/// the four-artefact graceful-error clause).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantOutcome {
    /// What the plugin actually gets.
    pub grant: CapabilityGrant,
    /// Requested-but-not-granted capabilities (e.g. `proc:spawn` for a
    /// user-installed plugin). Never fatal — the plugin loads degraded.
    pub denied: Vec<Capability>,
}

/// Compute the effective grant for `manifest` under `tier`.
///
/// The only tier-dependent filter in v1 is `proc:spawn` (bundled-only); every
/// other requested capability is granted as declared. User-consent narrowing
/// (a user-installed plugin's grant reduced to what the user approved) plugs in
/// here in a later slice — the [`GrantOutcome::denied`] channel already carries
/// the "requested but withheld" set that a consent step would populate.
pub fn grant(manifest: &PluginManifest, tier: TrustTier) -> GrantOutcome {
    let mut g = CapabilityGrant {
        editor: manifest.editor_capabilities,
        ..CapabilityGrant::default()
    };
    let mut denied = Vec::new();

    for cap in &manifest.requested {
        match cap {
            Capability::FsRead(p) => g.fs.push(FsGrant {
                prefix: p.clone(),
                write: false,
            }),
            Capability::FsWrite(p) => g.fs.push(FsGrant {
                prefix: p.clone(),
                write: true,
            }),
            Capability::NetHttp(h) => g.net_http.push(h.clone()),
            Capability::ProcSpawn => match tier {
                TrustTier::Bundled => g.proc_spawn = true,
                // proc:spawn is bundled-only in v1 — a user plugin's request
                // is withheld (fragment §6). Surfaced, never fatal.
                TrustTier::UserInstalled => denied.push(Capability::ProcSpawn),
            },
        }
    }

    GrantOutcome { grant: g, denied }
}

/// A resolved directory preopen: which host dir maps to which guest path, and
/// whether it is writable. The mapping between a [`CapabilityGrant`] and the
/// WASI view, exposed as data so it is unit-testable without a live guest
/// (PH7.2 proves enforcement at this layer; the guest-level end-to-end proof
/// lands at PH7.4 with the real `wasm32-wasip2` `fuzzy-finder`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreopenSpec {
    /// The host directory made visible to the guest.
    pub host_path: PathBuf,
    /// The path the guest sees it at.
    pub guest_path: String,
    /// Whether the guest may mutate it.
    pub writable: bool,
}

impl CapabilityGrant {
    /// The full set of directory preopens for a plugin whose private data dir
    /// is `data_dir`. The data dir is always mounted writable at
    /// [`DATA_DIR_GUEST_MOUNT`]; each `fs` grant adds a preopen at a guest path
    /// equal to its host path (so the guest opens the same absolute path it was
    /// granted). A plugin with no `fs` grant reaches *only* its data dir.
    ///
    /// The guest-path convention (`fs` prefix mounted at its own host path) is
    /// provisional until `fuzzy-finder` exercises it (PH7.4, open question in
    /// fragment §13).
    pub fn preopens(&self, data_dir: &Path) -> Vec<PreopenSpec> {
        let mut specs = vec![PreopenSpec {
            host_path: data_dir.to_path_buf(),
            guest_path: DATA_DIR_GUEST_MOUNT.to_string(),
            writable: true,
        }];
        for fs in &self.fs {
            specs.push(PreopenSpec {
                host_path: fs.prefix.clone(),
                guest_path: fs.prefix.to_string_lossy().into_owned(),
                writable: fs.write,
            });
        }
        specs
    }
}

/// Build the [`WasiCtx`] enforcing `grant` for a plugin whose data dir is
/// `data_dir`. Only filesystem preopens are wired (see the module note);
/// sockets and subprocess spawning stay disabled at the WASI layer.
///
/// **Graceful degradation:** a granted prefix that cannot be opened (missing
/// dir, permission error) is skipped with a `warn!`, never a panic or a failed
/// load — the plugin runs with that one capability degraded (fragment §6). The
/// caller must have created `data_dir` before this runs (the host does).
pub fn build_wasi_ctx(grant: &CapabilityGrant, data_dir: &Path) -> WasiCtx {
    let mut builder = WasiCtxBuilder::new();
    // Deliberately import-free otherwise: no stdio, no env, no ambient
    // clocks/random beyond WASI defaults, no sockets, no subprocess.
    for spec in grant.preopens(data_dir) {
        let (dir_perms, file_perms) = if spec.writable {
            (
                DirPerms::READ | DirPerms::MUTATE,
                FilePerms::READ | FilePerms::WRITE,
            )
        } else {
            (DirPerms::READ, FilePerms::READ)
        };
        if let Err(err) =
            builder.preopened_dir(&spec.host_path, &spec.guest_path, dir_perms, file_perms)
        {
            // A denied/inaccessible granted prefix degrades to "not mounted".
            tracing::warn!(
                host_path = %spec.host_path.display(),
                guest_path = %spec.guest_path,
                error = %err,
                "plugin filesystem preopen skipped (capability degraded)"
            );
        }
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::manifest::Capability;

    fn manifest(caps: Vec<Capability>) -> PluginManifest {
        PluginManifest::new("test-plugin", caps, CapabilitySet::empty())
    }

    #[test]
    fn fs_read_and_write_map_to_grants() {
        let m = manifest(vec![
            Capability::FsRead(PathBuf::from("/ro")),
            Capability::FsWrite(PathBuf::from("/rw")),
        ]);
        let out = grant(&m, TrustTier::UserInstalled);
        assert_eq!(
            out.grant.fs,
            vec![
                FsGrant {
                    prefix: PathBuf::from("/ro"),
                    write: false
                },
                FsGrant {
                    prefix: PathBuf::from("/rw"),
                    write: true
                },
            ]
        );
        assert!(out.denied.is_empty());
    }

    #[test]
    fn proc_spawn_is_bundled_only() {
        let m = manifest(vec![Capability::ProcSpawn]);

        let bundled = grant(&m, TrustTier::Bundled);
        assert!(bundled.grant.proc_spawn);
        assert!(bundled.denied.is_empty());

        let user = grant(&m, TrustTier::UserInstalled);
        assert!(!user.grant.proc_spawn);
        assert_eq!(user.denied, vec![Capability::ProcSpawn]);
    }

    #[test]
    fn net_http_allowlist_is_carried_on_the_grant() {
        let m = manifest(vec![
            Capability::NetHttp("crates.io".into()),
            Capability::NetHttp("docs.rs".into()),
        ]);
        let out = grant(&m, TrustTier::UserInstalled);
        assert_eq!(out.grant.net_http, vec!["crates.io", "docs.rs"]);
    }

    #[test]
    fn editor_capabilities_flow_through_unchanged() {
        let m = PluginManifest::new("x", vec![], CapabilitySet::TREE_SITTER | CapabilitySet::LSP);
        let out = grant(&m, TrustTier::Bundled);
        assert_eq!(
            out.grant.editor,
            CapabilitySet::TREE_SITTER | CapabilitySet::LSP
        );
    }

    #[test]
    fn empty_grant_preopens_only_the_data_dir() {
        let g = CapabilityGrant::default();
        let specs = g.preopens(Path::new("/data/plugins/x/data"));
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].guest_path, DATA_DIR_GUEST_MOUNT);
        assert_eq!(specs[0].host_path, PathBuf::from("/data/plugins/x/data"));
        assert!(specs[0].writable);
    }

    #[test]
    fn fs_grants_become_preopens_with_correct_perms() {
        let out = grant(
            &manifest(vec![
                Capability::FsRead(PathBuf::from("/ro")),
                Capability::FsWrite(PathBuf::from("/rw")),
            ]),
            TrustTier::Bundled,
        );
        let specs = out.grant.preopens(Path::new("/data"));
        // data dir + two fs grants.
        assert_eq!(specs.len(), 3);
        let ro = specs
            .iter()
            .find(|s| s.host_path == Path::new("/ro"))
            .unwrap();
        assert!(!ro.writable);
        assert_eq!(ro.guest_path, "/ro");
        let rw = specs
            .iter()
            .find(|s| s.host_path == Path::new("/rw"))
            .unwrap();
        assert!(rw.writable);
    }

    #[test]
    fn build_wasi_ctx_skips_missing_prefix_without_panic() {
        // A granted prefix that does not exist must degrade gracefully: the
        // context still builds (the data dir mounts), the bad prefix is
        // skipped. `data_dir` must exist, so use the current dir.
        let cwd = std::env::current_dir().unwrap();
        let g = CapabilityGrant {
            fs: vec![FsGrant {
                prefix: PathBuf::from("/nonexistent-lattice-plugin-prefix-xyz"),
                write: false,
            }],
            ..CapabilityGrant::default()
        };
        // Must not panic; returns a usable WasiCtx.
        let _ctx = build_wasi_ctx(&g, &cwd);
    }
}
