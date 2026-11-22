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
    /// host-services proc seam, PH7.3+). The UNBOUND grant: any program.
    pub proc_spawn: bool,
    /// OB.2: the programs an option-bound `proc:spawn:{…}` grant permits,
    /// exactly as the user wrote them — `sh` does not permit `/bin/sh`.
    pub spawn_commands: std::collections::BTreeSet<String>,
    /// OB.2: the options those programs came from, so a refusal can name where
    /// the user would add one.
    pub spawn_options: Vec<String>,
    /// OR.1: whether the plugin may persist bytes in its own key/value store
    /// (enforced at the `host-services` `store-*` seam — the store lives in the
    /// plugin's private data dir, which WASI already mounts, so this gate is
    /// host-side like `net:http` rather than a preopen).
    pub state_write: bool,
    /// CM.2: whether the plugin may bind an operator chord into the universal
    /// operator-pending grammar. Enforced at the loader's grammar drain — a
    /// declared chord is skipped (and logged) without this, while the operator
    /// itself still registers and stays reachable by name.
    pub grammar_chord: bool,
    /// The editor capabilities a plugin-declared mode requires (enforced at
    /// mode activation, PH7.11).
    pub editor: CapabilitySet,
    /// LH.0.6: the plugin's private data dir, as a HOST path — set by the
    /// host when it mounts one, never by a manifest.
    ///
    /// WASI already mounts this directory writable at `/data`, so the guest
    /// can do anything to it. What it could not do was name it to a host-side
    /// seam: `http-download`, `extract-archive` and `register-server` take
    /// host paths and check them against `fs`, where the data dir never
    /// appeared. Carrying it here lets those checks pass for the one directory
    /// the plugin owns outright, and grants no reach it did not have.
    ///
    /// Deliberately not an entry in `fs`: that list is also the preopen list,
    /// and the directory is already mounted.
    pub data_dir: Option<PathBuf>,
}

impl CapabilityGrant {
    /// OB.2: may this plugin run `command`? The unbound grant permits any
    /// program; an option-bound one exactly the strings it lists.
    pub fn may_spawn(&self, command: &str) -> bool {
        self.proc_spawn || self.spawn_commands.contains(command)
    }
}

/// OB.2: one option-bound process grant, as resolved — the option and the
/// programs it gave. Compared across a reload decision like
/// [`OptionBoundGrant`], and guarded against the plugin setting it (PMF.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnBoundGrant {
    /// The option the grant follows (`org.babel.languages`).
    pub option: String,
    /// The programs it permits, in the option's order.
    pub commands: Vec<String>,
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
    /// PMF.3: how each option-bound filesystem grant resolved — the option,
    /// the path it gave (or `None` for an explicit empty value), and whether
    /// that path was the manifest's default. What `:plugins` shows, and what a
    /// refusal names, so a user can see which setting decides a plugin's reach.
    pub option_bound: Vec<OptionBoundGrant>,
    /// OB.2: how each option-bound process grant resolved.
    pub spawn_bound: Vec<SpawnBoundGrant>,
}

/// PMF.3: one option-bound grant, as resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionBoundGrant {
    /// The option the grant follows (`org.directory`).
    pub option: String,
    /// The granted path, `~`-expanded; `None` when the option is set to empty
    /// or has no value and no default.
    pub path: Option<PathBuf>,
    /// `true` when `path` is the manifest's default rather than the user's.
    pub from_default: bool,
    /// `fs:write` (`true`) or `fs:read`.
    pub write: bool,
}

/// Compute the effective grant for `manifest` under `tier`.
///
/// The only tier-dependent filter in v1 is `proc:spawn` (bundled-only); every
/// other requested capability is granted as declared. User-consent narrowing
/// (a user-installed plugin's grant reduced to what the user approved) plugs in
/// here in a later slice — the [`GrantOutcome::denied`] channel already carries
/// the "requested but withheld" set that a consent step would populate.
pub fn grant(manifest: &PluginManifest, tier: TrustTier) -> GrantOutcome {
    grant_with(manifest, tier, &|_, _| None)
}

/// OB.2: which values an option-bound grant may follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Authorship {
    /// The option's current value, whoever set it — a directory grant, whose
    /// default the manifest states in its own template (`{org.directory|~/org}`).
    Any,
    /// Only a value the USER wrote (config file, `init.rs`, `:set`), never the
    /// default the plugin registered for the option. A process grant: a
    /// plugin's own default must not be able to grant it a program.
    UserOnly,
}

/// [`grant`], resolving option-bound grants (PMF.3) through `option_value`:
/// the named option's current value, or `None` when it has none. With no
/// value the manifest's default applies; an explicitly empty value grants
/// nothing. [`crate::PluginHost::grant_for`] supplies the editor's config, so
/// every caller resolves a plugin's grant the same way.
pub fn grant_with(
    manifest: &PluginManifest,
    tier: TrustTier,
    option_value: &dyn Fn(&str, Authorship) -> Option<lattice_config::ConfigValue>,
) -> GrantOutcome {
    let mut option_bound = Vec::new();
    let mut spawn_bound = Vec::new();
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
            Capability::FsReadOption(op) | Capability::FsWriteOption(op) => {
                let write = matches!(cap, Capability::FsWriteOption(_));
                let (paths, from_default) = match option_value(&op.option, Authorship::Any) {
                    Some(v) => (option_paths(&op.option, &v), false),
                    None => (op.default.iter().cloned().collect(), true),
                };
                for prefix in &paths {
                    g.fs.push(FsGrant {
                        prefix: prefix.clone(),
                        write,
                    });
                }
                // One resolved grant per path, each naming the option, so
                // `:plugins` and a refusal list them all. An option that
                // grants nothing is still reported, as granting nothing.
                let resolved: Vec<Option<PathBuf>> = if paths.is_empty() {
                    vec![None]
                } else {
                    paths.into_iter().map(Some).collect()
                };
                option_bound.extend(resolved.into_iter().map(|path| OptionBoundGrant {
                    option: op.option.clone(),
                    path,
                    from_default,
                    write,
                }));
            }
            Capability::NetHttp(h) => g.net_http.push(h.clone()),
            // OR.1: granted at either tier. The store reaches only the
            // plugin's own data dir, which every plugin already has mounted
            // writable, so withholding it from a user-installed plugin would
            // deny nothing it could not already do through WASI — it would
            // only deny the crash-safe, versioned, cross-instance shape.
            Capability::StateWrite => g.state_write = true,
            // CM.2: both tiers. See the variant's doc — the chord is scoped to
            // the plugin's own minor mode, shown in `:plugins`, and reversed on
            // unload, so withholding it would deny the feature without
            // narrowing the risk that matters.
            Capability::GrammarChord => g.grammar_chord = true,
            // OB.2: both tiers — it permits only what the user configured.
            Capability::ProcSpawnOption(op) => {
                let commands = match option_value(&op.option, Authorship::UserOnly) {
                    Some(v) => option_commands(&op.option, op.field.as_deref(), &v),
                    None => Vec::new(),
                };
                g.spawn_commands.extend(commands.iter().cloned());
                g.spawn_options.push(op.option.clone());
                spawn_bound.push(SpawnBoundGrant {
                    option: op.option.clone(),
                    commands,
                });
            }
            Capability::ProcSpawn => match tier {
                TrustTier::Bundled => g.proc_spawn = true,
                // proc:spawn is bundled-only in v1 — a user plugin's request
                // is withheld (fragment §6). Surfaced, never fatal.
                TrustTier::UserInstalled => denied.push(Capability::ProcSpawn),
            },
        }
    }

    GrantOutcome {
        grant: g,
        denied,
        option_bound,
        spawn_bound,
    }
}

/// OB.1: the directories an option-bound grant's value names. A string is one
/// path (blank = none); a list is one per non-blank string entry. Anything
/// else names no directory, and says so — a grant must never widen on a
/// value it could not read.
fn option_paths(option: &str, value: &lattice_config::ConfigValue) -> Vec<PathBuf> {
    use lattice_config::ConfigValue;
    let path = |s: &str| {
        let s = s.trim();
        (!s.is_empty()).then(|| PathBuf::from(lattice_core::home::expand_tilde(s)))
    };
    match value {
        ConfigValue::Str(s) => path(s).into_iter().collect(),
        ConfigValue::List(items) => items
            .iter()
            .filter_map(|item| match item {
                ConfigValue::Str(s) => path(s),
                other => {
                    tracing::warn!(
                        option,
                        kind = other.kind_label(),
                        "option-bound grant: a non-string entry names no directory"
                    );
                    None
                }
            })
            .collect(),
        other => {
            tracing::warn!(
                option,
                kind = other.kind_label(),
                "option-bound grant: this value names no directory"
            );
            Vec::new()
        }
    }
}

/// OB.2: the programs an option-bound process grant's value names. With a
/// `field`, the value is a list of records and each record's `field` string is
/// one program; without, a list of strings (or one string). Blank and
/// misshapen entries name nothing, and say so — never a wider grant.
fn option_commands(
    option: &str,
    field: Option<&str>,
    value: &lattice_config::ConfigValue,
) -> Vec<String> {
    use lattice_config::ConfigValue;
    let named = |v: Option<&ConfigValue>| match v {
        Some(ConfigValue::Str(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        _ => None,
    };
    let entry = |item: &ConfigValue| match (field, item) {
        (Some(f), ConfigValue::Record(fields)) => named(fields.get(f)),
        (None, ConfigValue::Str(_)) => named(Some(item)),
        _ => None,
    };
    let items: Vec<&ConfigValue> = match value {
        ConfigValue::List(items) => items.iter().collect(),
        ConfigValue::Str(_) if field.is_none() => vec![value],
        _ => Vec::new(),
    };
    let mut commands = Vec::new();
    for item in items {
        match entry(item) {
            Some(command) if !commands.contains(&command) => commands.push(command),
            Some(_) => {}
            None => tracing::warn!(
                option,
                field,
                kind = item.kind_label(),
                "option-bound spawn grant: an entry names no program"
            ),
        }
    }
    commands
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
/// **Graceful degradation:** a granted prefix that cannot be opened is skipped,
/// never a panic or a failed load — the plugin runs with that one capability
/// degraded (fragment §6). A directory that does not exist is logged at
/// `debug!` (an option-bound grant's default often does not); one that exists
/// but will not open (permissions) at `warn!`. The
/// caller must have created `data_dir` before this runs (the host does).
pub fn build_wasi_ctx(grant: &CapabilityGrant, data_dir: &Path) -> WasiCtx {
    let mut builder = WasiCtxBuilder::new();
    // Deliberately import-free otherwise: no stdio, no ambient clocks/random
    // beyond WASI defaults, no sockets, no subprocess.
    //
    // **`HOME` is the one exception, and it is deliberate rather than a
    // loosening that crept in.** A guest reads path options the user wrote —
    // `org.roam-directory = "~/notes"` — and with no environment it cannot
    // expand the tilde, so the path reaches the host verbatim, resolves to
    // nothing, and the feature reports an empty corpus rather than a bad path.
    // The user's config is then simply not portable: it must name
    // `/Users/dhruva/...` or `/home/dhruva/...` and stops working on their
    // other machine.
    //
    // What this does NOT concede: the guest still has no `PATH`, no `USER`, no
    // shell, no ambient credentials, and no ability to reach anything it was
    // not granted. `HOME` is a *string* here — knowing the path does not mount
    // it, and every filesystem access still goes through the preopens below.
    // A plugin that learns the home directory and asks to read it is refused
    // exactly as before.
    //
    // Resolved through `dirs::home_dir` rather than `std::env::var_os("HOME")`
    // so a Windows host supplies `%USERPROFILE%` — otherwise this would hand
    // POSIX guests a working tilde and Windows guests nothing, which is the
    // asymmetry the shared helper exists to end.
    if let Some(home) = dirs::home_dir() {
        builder.env("HOME", home.display().to_string());
    }
    for spec in grant.preopens(data_dir) {
        let (dir_perms, file_perms) = if spec.writable {
            (
                DirPerms::READ | DirPerms::MUTATE,
                FilePerms::READ | FilePerms::WRITE,
            )
        } else {
            (DirPerms::READ, FilePerms::READ)
        };
        // PMF.3: a grant that follows an option starts on the plugin's default
        // (org's `~/org-roam`), which most users never create. A directory that
        // does not exist is therefore the normal case, not a fault: it is simply
        // not mounted, and the plugin treats the feature as off. Only a path
        // that exists and still cannot be opened is worth a warning.
        if !spec.host_path.exists() {
            tracing::debug!(
                host_path = %spec.host_path.display(),
                "granted directory does not exist; not mounted"
            );
            continue;
        }
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
    use crate::manifest::{Capability, OptionPath};

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

    // ── PMF.3: option-bound grants ───────────────────────────────────────

    use lattice_config::ConfigValue;

    fn org_manifest() -> PluginManifest {
        PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"fs:write:{org.directory|~/org}\", \"state:write\"]\n",
        )
        .unwrap()
    }

    fn home() -> PathBuf {
        PathBuf::from(lattice_core::home::expand_tilde("~"))
    }

    #[test]
    fn an_option_bound_grant_parses_and_round_trips() {
        let m = org_manifest();
        let Capability::FsWriteOption(op) = &m.requested[0] else {
            panic!("an option-bound write grant: {:?}", m.requested[0]);
        };
        assert_eq!(op.option, "org.directory");
        assert_eq!(op.default.as_deref(), Some(home().join("org").as_path()));
        let reparsed: Capability = m.requested[0].to_string().parse().unwrap();
        assert_eq!(reparsed, m.requested[0], "Display is the wire form");

        let no_default: Capability = "fs:read:{org.archive}".parse().unwrap();
        assert!(matches!(
            no_default,
            Capability::FsReadOption(OptionPath { default: None, .. })
        ));
    }

    #[test]
    fn a_malformed_option_template_is_an_error_not_a_path() {
        for bad in [
            "fs:write:{org.directory",
            "fs:write:{}",
            "fs:write:{ |~/x}",
            "fs:write:{a b}",
        ] {
            assert!(bad.parse::<Capability>().is_err(), "`{bad}` must not parse");
        }
    }

    #[test]
    fn a_grant_may_only_follow_the_plugins_own_options() {
        let err = PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"fs:write:{roam.directory|~/x}\"]\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, crate::manifest::ManifestError::ForeignOption { ref option, .. } if option == "roam.directory"),
            "{err}"
        );
    }

    #[test]
    fn an_unset_option_grants_the_manifest_default() {
        let out = grant_with(&org_manifest(), TrustTier::UserInstalled, &|_, _| None);
        assert_eq!(
            out.grant.fs,
            vec![FsGrant {
                prefix: home().join("org"),
                write: true
            }]
        );
        assert_eq!(out.option_bound.len(), 1);
        assert!(out.option_bound[0].from_default);
        assert!(out.option_bound[0].write);
    }

    #[test]
    fn the_users_value_replaces_the_default() {
        let out = grant_with(&org_manifest(), TrustTier::UserInstalled, &|name, _| {
            (name == "org.directory").then(|| ConfigValue::Str("~/notes".into()))
        });
        assert_eq!(out.grant.fs[0].prefix, home().join("notes"));
        assert!(!out.option_bound[0].from_default);
    }

    #[test]
    fn an_empty_value_grants_nothing() {
        let out = grant_with(&org_manifest(), TrustTier::UserInstalled, &|_, _| {
            Some(ConfigValue::Str("  ".into()))
        });
        assert!(out.grant.fs.is_empty(), "explicit empty = no access");
        assert_eq!(out.option_bound[0].path, None);
        assert!(
            out.grant.state_write,
            "the other capabilities are unaffected"
        );
    }

    #[test]
    fn no_value_and_no_default_grants_nothing() {
        let m = PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"fs:write:{org.directory}\"]\n",
        )
        .unwrap();
        let out = grant_with(&m, TrustTier::UserInstalled, &|_, _| None);
        assert!(out.grant.fs.is_empty());
        assert_eq!(out.option_bound[0].path, None);
    }

    fn tangle_manifest() -> PluginManifest {
        PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"fs:write:{org.babel.tangle-directories}\"]\n",
        )
        .unwrap()
    }

    fn strs(items: &[&str]) -> ConfigValue {
        ConfigValue::List(
            items
                .iter()
                .map(|s| ConfigValue::Str((*s).into()))
                .collect(),
        )
    }

    #[test]
    fn a_list_option_grants_each_entry() {
        let out = grant_with(&tangle_manifest(), TrustTier::UserInstalled, &|_, _| {
            Some(strs(&["~/dotfiles", "/etc/lattice"]))
        });
        assert_eq!(
            out.grant.fs,
            vec![
                FsGrant {
                    prefix: home().join("dotfiles"),
                    write: true
                },
                FsGrant {
                    prefix: PathBuf::from("/etc/lattice"),
                    write: true
                },
            ]
        );
        let paths: Vec<_> = out.option_bound.iter().map(|g| g.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                Some(home().join("dotfiles")),
                Some(PathBuf::from("/etc/lattice"))
            ],
            "one resolved grant per entry, each naming the option"
        );
        assert!(
            out.option_bound
                .iter()
                .all(|g| g.option == "org.babel.tangle-directories" && !g.from_default)
        );
    }

    #[test]
    fn an_empty_list_grants_nothing_and_does_not_fall_back() {
        let m = PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"fs:write:{org.babel.tangle-directories|~/x}\"]\n",
        )
        .unwrap();
        let out = grant_with(&m, TrustTier::UserInstalled, &|_, _| Some(strs(&[])));
        assert!(out.grant.fs.is_empty(), "an empty list is an explicit no");
        assert_eq!(
            out.option_bound.len(),
            1,
            "still reported, as granting nothing"
        );
        assert_eq!(out.option_bound[0].path, None);
        assert!(!out.option_bound[0].from_default);
    }

    #[test]
    fn blank_and_non_string_list_entries_grant_nothing() {
        let value = ConfigValue::List(vec![
            ConfigValue::Str("  ".into()),
            ConfigValue::Int(3),
            ConfigValue::Str("/ok".into()),
        ]);
        let out = grant_with(&tangle_manifest(), TrustTier::UserInstalled, &|_, _| {
            Some(value.clone())
        });
        assert_eq!(
            out.grant.fs,
            vec![FsGrant {
                prefix: PathBuf::from("/ok"),
                write: true
            }]
        );
    }

    #[test]
    fn a_record_value_grants_no_path() {
        let out = grant_with(&org_manifest(), TrustTier::UserInstalled, &|_, _| {
            Some(ConfigValue::record([(
                "a".to_string(),
                ConfigValue::Str("/x".into()),
            )]))
        });
        assert!(out.grant.fs.is_empty(), "a table names no directory");
    }

    // ── OB.2: `proc:spawn:{option:field}` ─────────────────────────────────

    fn babel_manifest() -> PluginManifest {
        PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"proc:spawn:{org.babel.languages:command}\"]\n",
        )
        .unwrap()
    }

    fn language(name: &str, command: &str) -> ConfigValue {
        ConfigValue::record([
            ("name".to_string(), ConfigValue::Str(name.into())),
            ("command".to_string(), ConfigValue::Str(command.into())),
        ])
    }

    #[test]
    fn a_proc_spawn_option_parses_and_round_trips() {
        let m = babel_manifest();
        let Capability::ProcSpawnOption(op) = &m.requested[0] else {
            panic!("an option-bound spawn grant: {:?}", m.requested[0]);
        };
        assert_eq!(op.option, "org.babel.languages");
        assert_eq!(op.field.as_deref(), Some("command"));
        let reparsed: Capability = m.requested[0].to_string().parse().unwrap();
        assert_eq!(reparsed, m.requested[0], "Display is the wire form");

        let bare: Capability = "proc:spawn:{org.commands}".parse().unwrap();
        let Capability::ProcSpawnOption(op) = bare else {
            panic!("a field-less spawn grant");
        };
        assert_eq!((op.option.as_str(), op.field), ("org.commands", None));
    }

    #[test]
    fn a_malformed_spawn_template_is_an_error() {
        for bad in [
            "proc:spawn:{}",
            "proc:spawn:{org.x:}",
            "proc:spawn:{:command}",
            "proc:spawn:{org.x|sh}",
            "proc:spawn:sh",
            "proc:spawn:{org.x",
        ] {
            assert!(bad.parse::<Capability>().is_err(), "`{bad}` must not parse");
        }
    }

    #[test]
    fn a_spawn_grant_may_only_follow_the_plugins_own_options() {
        let err = PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"proc:spawn:{lighthouse.servers:command}\"]\n",
        )
        .unwrap_err();
        assert!(
            matches!(err, crate::manifest::ManifestError::ForeignOption { ref option, .. } if option == "lighthouse.servers"),
            "{err}"
        );
    }

    #[test]
    fn a_user_plugin_is_granted_exactly_the_configured_commands() {
        let value = ConfigValue::List(vec![language("sh", "sh"), language("python", "python3")]);
        let out = grant_with(&babel_manifest(), TrustTier::UserInstalled, &|name, _| {
            (name == "org.babel.languages").then(|| value.clone())
        });
        assert!(
            out.denied.is_empty(),
            "an option-bound spawn grant is not tier-gated"
        );
        assert!(
            !out.grant.proc_spawn,
            "never the unbound grant — register-server stays off"
        );
        assert_eq!(
            out.grant.spawn_commands,
            ["python3", "sh"].iter().map(|s| s.to_string()).collect()
        );
        assert!(out.grant.may_spawn("sh"));
        assert!(
            !out.grant.may_spawn("/bin/sh"),
            "exact string, no PATH resolution"
        );
        assert!(!out.grant.may_spawn("bash"));
    }

    #[test]
    fn no_configured_languages_permits_nothing() {
        for value in [None, Some(ConfigValue::List(vec![]))] {
            let out = grant_with(&babel_manifest(), TrustTier::UserInstalled, &|_, _| {
                value.clone()
            });
            assert!(out.grant.spawn_commands.is_empty(), "{value:?}");
            assert!(!out.grant.may_spawn("sh"));
        }
    }

    #[test]
    fn records_without_the_field_or_with_a_blank_one_permit_nothing() {
        let value = ConfigValue::List(vec![
            ConfigValue::record([("name".to_string(), ConfigValue::Str("x".into()))]),
            language("y", "  "),
            language("z", "ruby"),
            ConfigValue::Str("perl".into()),
        ]);
        let out = grant_with(&babel_manifest(), TrustTier::UserInstalled, &|_, _| {
            Some(value.clone())
        });
        assert_eq!(
            out.grant.spawn_commands,
            ["ruby"].iter().map(|s| s.to_string()).collect(),
            "a bare string is not a record when a field is named"
        );
    }

    #[test]
    fn a_field_less_spawn_grant_reads_a_list_of_strings() {
        let m = PluginManifest::from_toml_str(
            "id = \"org\"\ncapabilities = [\"proc:spawn:{org.commands}\"]\n",
        )
        .unwrap();
        let out = grant_with(&m, TrustTier::UserInstalled, &|_, _| {
            Some(strs(&["make", " "]))
        });
        assert_eq!(
            out.grant.spawn_commands,
            ["make"].iter().map(|s| s.to_string()).collect()
        );
    }

    #[test]
    fn the_spawn_option_is_reported_for_reload_and_guarding() {
        let value = ConfigValue::List(vec![language("sh", "sh")]);
        let out = grant_with(&babel_manifest(), TrustTier::UserInstalled, &|_, _| {
            Some(value.clone())
        });
        assert_eq!(
            out.spawn_bound,
            vec![SpawnBoundGrant {
                option: "org.babel.languages".into(),
                commands: vec!["sh".into()],
            }]
        );
    }

    #[test]
    fn unbound_proc_spawn_still_permits_any_command_for_a_bundled_plugin() {
        let m = manifest(vec![Capability::ProcSpawn]);
        let out = grant(&m, TrustTier::Bundled);
        assert!(out.grant.may_spawn("anything"));
        let out = grant(&m, TrustTier::UserInstalled);
        assert!(!out.grant.may_spawn("anything"));
    }
}
