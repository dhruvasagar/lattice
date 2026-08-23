//! The plugin manifest — a plugin's *declared* capability request.
//!
//! Design fragment: `docs/dev/architecture/plugin-host.md` §6. Slice: PH7.2.
//!
//! A manifest declares **what a plugin asks for**, not what it gets. The
//! *grant* — what a plugin is actually granted — is computed from the manifest
//! plus its [`crate::TrustTier`] (and, for user-installed plugins, consent);
//! see [`crate::capability`]. The manifest is untrusted input: a plugin cannot
//! declare its own trust tier (that would defeat the point), so the tier is a
//! host-supplied argument to grant computation, never a manifest field.
//!
//! The manifest is a committed **TOML** format so the Phase-8 plugin manager
//! can discover + parse it off disk; the host itself consumes an
//! already-parsed [`PluginManifest`] (there is no on-disk plugin discovery at
//! PH7.2 — that is the plugin manager, deferred to Phase 8).
//!
//! ```toml
//! id = "fuzzy-finder"
//! capabilities = ["fs:read:/home/alice/project", "net:http:crates.io"]
//! editor_capabilities = ["tree-sitter"]
//! ```
//!
//! Two capability namespaces meet here and stay distinct:
//! - **OS capabilities** ([`Capability`]) gate the plugin's WASI view
//!   (`fs:*` / `net:*` / `proc:*`). These are enforced by the runtime.
//! - **Editor capabilities** ([`CapabilitySet`]) are the *same* set
//!   [`lattice_mode::Mode::required_capabilities`] returns; a plugin that
//!   declares a mode carries its capability requirements here. Enforcement
//!   stays the mode-activation path (PH7.11) — this slice only sizes the
//!   manifest honestly per fragment §6.

use std::path::PathBuf;
use std::str::FromStr;

use lattice_mode::CapabilitySet;
use serde::{Deserialize, Serialize};

/// An OS-level capability a plugin requests in its manifest. Distinct from
/// [`CapabilitySet`] (editor/buffer capabilities); these gate the plugin's
/// WASI view (filesystem / network / process).
///
/// Wire form (manifest + [`Display`]): `fs:read:<prefix>`, `fs:write:<prefix>`,
/// `net:http:<host>`, `proc:spawn`. The `<prefix>` may itself contain `:`
/// (paths, `host:port`); only the first two segments are the discriminator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Capability {
    /// Read access to a host path prefix (`fs:read:<prefix>`).
    FsRead(PathBuf),
    /// Read + write access to a host path prefix (`fs:write:<prefix>`).
    FsWrite(PathBuf),
    /// Outbound HTTP to one host-allowlist entry (`net:http:<host>`).
    NetHttp(String),
    /// Permission to spawn subprocesses (`proc:spawn`). Bundled-only in v1
    /// (dropped from a user-installed plugin's grant — fragment §6).
    ProcSpawn,
}

/// The string `s` was not a recognised capability form.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "unrecognised capability `{0}` (expected `fs:read:<p>` / `fs:write:<p>` / `net:http:<host>` / `proc:spawn`)"
)]
pub struct CapabilityParseError(pub String);

impl FromStr for Capability {
    type Err = CapabilityParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // `splitn(3, ':')` keeps everything after the second colon intact, so
        // a path or `host:port` prefix survives whole.
        let mut parts = s.splitn(3, ':');
        match (parts.next(), parts.next(), parts.next()) {
            (Some("fs"), Some("read"), Some(p)) if !p.is_empty() => {
                Ok(Capability::FsRead(PathBuf::from(p)))
            }
            (Some("fs"), Some("write"), Some(p)) if !p.is_empty() => {
                Ok(Capability::FsWrite(PathBuf::from(p)))
            }
            (Some("net"), Some("http"), Some(h)) if !h.is_empty() => {
                Ok(Capability::NetHttp(h.to_string()))
            }
            (Some("proc"), Some("spawn"), None) => Ok(Capability::ProcSpawn),
            _ => Err(CapabilityParseError(s.to_string())),
        }
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Capability::FsRead(p) => write!(f, "fs:read:{}", p.display()),
            Capability::FsWrite(p) => write!(f, "fs:write:{}", p.display()),
            Capability::NetHttp(h) => write!(f, "net:http:{h}"),
            Capability::ProcSpawn => f.write_str("proc:spawn"),
        }
    }
}

/// Map an editor-capability name (dashed, lowercase) to its [`CapabilitySet`]
/// bit. Names mirror the `capability.rs` documentation in `lattice-mode`.
fn parse_editor_capability(name: &str) -> Option<CapabilitySet> {
    Some(match name {
        "buffer-uri" => CapabilitySet::BUFFER_URI,
        "lsp" => CapabilitySet::LSP,
        "tree-sitter" => CapabilitySet::TREE_SITTER,
        "folds" => CapabilitySet::FOLDS,
        "writable" => CapabilitySet::WRITABLE,
        "diagnostics" => CapabilitySet::DIAGNOSTICS,
        _ => return None,
    })
}

/// Which extension seam a plugin's component implements — the WIT world it
/// exports. Each seam is its own world (`picker-source`, `events-plugin`, …); a
/// component implements one, and the manifest declares which so the plugin
/// loader (`lattice-plugin-loader`) knows which `spawn_*` path to drive. An
/// empty `provides` list is a **lifecycle-only** plugin (the base `plugin`
/// world — `init.rs`, the no-op fixture), driven through `instantiate_plugin` +
/// `activate` rather than a seam actor.
///
/// Wire form (manifest `provides = [...]` + [`Display`]) is the dashed WIT
/// interface name: `picker-source`, `completion-source`, `grammar`, `events`,
/// `modes`, `config`, `decorations`, `context`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSeam {
    PickerSource,
    CompletionSource,
    Grammar,
    Events,
    Modes,
    Config,
    Decorations,
    /// TC.2 — the sticky-context producer (`context-plugin` world). Async,
    /// host-cached per parse version; see `treesitter-context.md`.
    Context,
    /// TC.4 — theme-element declaration (`theme-plugin` world). The plugin's
    /// elements land in the SAME registry builtins use.
    Theme,
    Keymap,
    /// The `wasi:logging`-shaped guest→host logging import (PO.5, Layer 2). Not a
    /// native trait seam — the guest's own narrative, host-captured into the same
    /// tracer as the boundary trace.
    Logging,
    /// PM.7 — the `require` seam (`plugin-manager-plugin` world). A config
    /// guest (the user's `init.rs`) declares the plugins it wants; the host
    /// records the specs and the loader resolves/builds/loads them off-thread.
    /// Deliberately its own seam rather than folded into `config`: declaring
    /// software to install is a strictly larger authority than setting an
    /// option, and `provides` is where that difference is visible to a user
    /// reading the manifest.
    PluginManager,
    /// CM.6 — a compilation-output parser (`error-parser-plugin` world). Sync,
    /// fed one captured line at a time by the compilation reader.
    ErrorParser,
    /// CR.3 — plugin-contributed `:help` pages (`help-plugin` world). The
    /// markdown ships inside the component and crosses once, at load; the
    /// topic then lives in the same registry the builtin docs do.
    Help,
    /// CR.4 — plugin-contributed launch-page sections (`dashboard-plugin`
    /// world). Sync, and unlike `help` the guest stays instantiated: a
    /// section is a function of a live `DashboardCtx`, not data.
    Dashboard,
    /// LG.3c — a plugin-contributed language (`language-plugin` world). Data,
    /// like `help`: the grammar bytes and query sources cross once at load and
    /// the guest is dropped. The host compiles the grammar (~100 ms) and owns
    /// the parse loop forever after, so there is no guest call on any hot
    /// path.
    Language,
}

impl PluginSeam {
    /// Where this seam sits in the loader's drain order (OM.0). Lower drains
    /// first; the loader stable-sorts a plugin's `provides` by this before
    /// draining, so **the order a manifest happens to list its seams in cannot
    /// change what registers**.
    ///
    /// The ordering exists because seams have real registration dependencies,
    /// and the one that bites is `modes` on `grammar`: a
    /// `mode-keymap-binding` resolves its `command` against the
    /// `CommandRegistry` *at registration*, so a mode binding a chord to the
    /// plugin's OWN grammar action needs that action already registered. Drain
    /// the other way round and the binding is skipped with a log — the plugin
    /// loads "successfully" and the user's chord silently does nothing.
    ///
    /// This used to be the plugin author's problem. Both bundled multi-seam
    /// manifests carried a hand-written comment telling the next author to put
    /// `grammar` before `modes`, which made a load-bearing invariant depend on
    /// prose inside a **guest-authored** file. Sorting here makes it a property
    /// of the seam set instead.
    ///
    /// The ranks, and why each is where it is:
    ///
    /// | rank | seams | reason |
    /// |---|---|---|
    /// | 0 | `config`, `theme`, `logging` | declarations later seams may reference — options and theme elements exist before any producer runs |
    /// | 1 | `language` | a major mode may declare `target-language`; the language should exist first |
    /// | 2 | `grammar` | registers commands into the `CommandRegistry` |
    /// | 3 | `modes` | keymap bindings resolve command *names* — needs rank 2 |
    /// | 4 | `keymap` | user-layer bindings, same name resolution |
    /// | 5 | everything else | producers and data seams with no registration dependency |
    ///
    /// Ties keep manifest order (the sort is stable), so an author's intent is
    /// respected everywhere it cannot break anything.
    pub fn drain_rank(self) -> u8 {
        match self {
            PluginSeam::Config | PluginSeam::Theme | PluginSeam::Logging => 0,
            PluginSeam::Language => 1,
            PluginSeam::Grammar => 2,
            PluginSeam::Modes => 3,
            PluginSeam::Keymap => 4,
            PluginSeam::PickerSource
            | PluginSeam::CompletionSource
            | PluginSeam::Events
            | PluginSeam::Decorations
            | PluginSeam::Context
            | PluginSeam::PluginManager
            | PluginSeam::ErrorParser
            | PluginSeam::Help
            | PluginSeam::Dashboard => 5,
        }
    }

    /// The dashed wire / display name (matches the WIT interface).
    pub fn as_str(self) -> &'static str {
        match self {
            PluginSeam::PickerSource => "picker-source",
            PluginSeam::CompletionSource => "completion-source",
            PluginSeam::Grammar => "grammar",
            PluginSeam::Events => "events",
            PluginSeam::Modes => "modes",
            PluginSeam::Config => "config",
            PluginSeam::Decorations => "decorations",
            PluginSeam::Context => "context",
            PluginSeam::Theme => "theme",
            PluginSeam::Keymap => "keymap",
            PluginSeam::Logging => "logging",
            PluginSeam::PluginManager => "plugin-manager",
            PluginSeam::ErrorParser => "error-parser",
            PluginSeam::Help => "help",
            PluginSeam::Dashboard => "dashboard",
            PluginSeam::Language => "language",
        }
    }
}

impl std::fmt::Display for PluginSeam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PluginSeam {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "picker-source" => PluginSeam::PickerSource,
            "completion-source" => PluginSeam::CompletionSource,
            "grammar" => PluginSeam::Grammar,
            "events" => PluginSeam::Events,
            "modes" => PluginSeam::Modes,
            "config" => PluginSeam::Config,
            "decorations" => PluginSeam::Decorations,
            "context" => PluginSeam::Context,
            "theme" => PluginSeam::Theme,
            "keymap" => PluginSeam::Keymap,
            "logging" => PluginSeam::Logging,
            "plugin-manager" => PluginSeam::PluginManager,
            "error-parser" => PluginSeam::ErrorParser,
            "help" => PluginSeam::Help,
            "dashboard" => PluginSeam::Dashboard,
            "language" => PluginSeam::Language,
            _ => return Err(()),
        })
    }
}

/// Everything a plugin declares about itself and what it needs. Untrusted
/// input; the trust tier is supplied separately at grant time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginManifest {
    /// Stable, human-legible plugin id (`fuzzy-finder`). Keys the per-plugin
    /// data dir (`<data>/lattice/plugins/<id>/data/`). Distinct from the
    /// host-issued numeric [`crate::PluginId`] used for compact provenance.
    pub id: String,
    /// The OS capabilities the plugin requests (its WASI view).
    pub requested: Vec<Capability>,
    /// The editor capabilities a plugin-declared mode requires (fragment §6).
    pub editor_capabilities: CapabilitySet,
    /// The extension seam(s) this plugin's component implements — which
    /// `spawn_*` path(s) the loader drives. Empty ⇒ lifecycle-only (base
    /// `plugin` world). Programmatic [`new`](Self::new) defaults this empty;
    /// disk discovery reads it from the manifest `provides` list.
    pub provides: Vec<PluginSeam>,
    /// PI.4: the plugin's own documentation, shown by `:describe-plugin`. A
    /// static, author-written string (immutable at editor runtime). The
    /// preferred doc source is the plugin's embedded WIT world doc-comment;
    /// this manifest field is the fallback when a component ships no WIT docs.
    pub doc: Option<String>,
    /// PM.3: the minor mode this plugin enables by DEFAULT. When set, the loader
    /// auto-registers a `<id>.enabled` bool option (default true) that gates it —
    /// on load (and on any change to the option) the manager enables / disables
    /// this mode via the `ModeEnablementRequested` path. `None` ⇒ the plugin
    /// contributes a mode the user must enable explicitly (the CI.3
    /// available-but-off default). The host learns the mode-id ONLY from here —
    /// the plugin owns it (mode-ownership).
    pub default_mode: Option<String>,
}

/// The on-disk manifest shape. Deserialised first, then validated into
/// [`PluginManifest`] so parse errors carry the offending string.
#[derive(Debug, Deserialize, Serialize)]
struct RawManifest {
    id: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    editor_capabilities: Vec<String>,
    /// PL8.B: the extension seam(s) the component implements (`picker-source`,
    /// `events`, …). Empty / absent ⇒ lifecycle-only.
    #[serde(default)]
    provides: Vec<String>,
    /// PI.4: the plugin's documentation (`:describe-plugin`).
    #[serde(default)]
    doc: Option<String>,
    /// PM.3: the minor mode this plugin enables by default (gated by
    /// `<id>.enabled`). Absent ⇒ no default mode.
    #[serde(default)]
    default_mode: Option<String>,
}

/// Why a manifest failed to parse. Every failure is a value — the host logs +
/// skips a bad manifest (graceful degradation), never panics.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The TOML itself was malformed or missing a required field.
    #[error("failed to parse plugin manifest TOML")]
    Toml(#[from] toml::de::Error),

    /// A `capabilities` entry was not a recognised capability form.
    #[error(transparent)]
    Capability(#[from] CapabilityParseError),

    /// An `editor_capabilities` entry was not a recognised editor-capability
    /// name.
    #[error(
        "unrecognised editor capability `{0}` (expected buffer-uri / lsp / tree-sitter / folds / writable / diagnostics)"
    )]
    EditorCapability(String),

    /// The `id` field was empty — a plugin must have a non-empty id (it keys
    /// the data dir path).
    #[error("plugin manifest `id` must not be empty")]
    EmptyId,

    /// The `id` field was not a single, safe path component. The id keys the
    /// per-plugin on-disk data dir (joined into a path + mounted WRITABLE into
    /// the guest), so a `/`, `\`, `.`, `..`, or absolute id would let a crafted
    /// manifest escape its sandbox and write outside the data dir. Rejected.
    #[error(
        "plugin manifest `id` `{0}` must be a single path component (no `/`, `\\`, `.`, `..`, or absolute path)"
    )]
    InvalidId(String),

    /// A `provides` entry was not a recognised seam name.
    #[error(
        "unrecognised plugin seam `{0}` (expected picker-source / completion-source / grammar / events / modes / config / decorations / keymap)"
    )]
    Seam(String),
}

/// Is `id` a single, safe path component — usable as a directory name that
/// cannot escape its parent? The plugin id keys the per-plugin data dir, which
/// is joined into a host path and mounted writable into the guest; a crafted id
/// (`/etc/cron.d`, `../../.ssh`, `.`) would otherwise relocate that writable
/// mount outside the sandbox with no fs grant (the CRITICAL isolation contract,
/// lib.rs `build_plugin_wasi`). Accepts exactly one `Component::Normal`; rejects
/// empty, absolute, separators, and `.` / `..`.
pub fn is_safe_plugin_id(id: &str) -> bool {
    use std::path::Component;
    if id.trim().is_empty() {
        return false;
    }
    let path = std::path::Path::new(id);
    if path.is_absolute() {
        return false;
    }
    let mut comps = path.components();
    matches!(
        (comps.next(), comps.next()),
        (Some(Component::Normal(_)), None)
    )
}

impl PluginManifest {
    /// Construct a manifest programmatically (the host's typed entry point).
    pub fn new(
        id: impl Into<String>,
        requested: Vec<Capability>,
        editor_capabilities: CapabilitySet,
    ) -> Self {
        Self {
            id: id.into(),
            requested,
            editor_capabilities,
            provides: Vec::new(),
            doc: None,
            default_mode: None,
        }
    }

    /// Parse a manifest from its TOML text, validating every capability form.
    /// A malformed capability / editor-capability / empty id is a typed
    /// [`ManifestError`], never a panic.
    pub fn from_toml_str(text: &str) -> Result<Self, ManifestError> {
        let raw: RawManifest = toml::from_str(text)?;
        if raw.id.trim().is_empty() {
            return Err(ManifestError::EmptyId);
        }
        // SECURITY (isolation): the id keys the writable per-plugin data mount, so
        // an untrusted on-disk manifest MUST NOT carry a path-escaping id.
        if !is_safe_plugin_id(&raw.id) {
            return Err(ManifestError::InvalidId(raw.id));
        }
        let requested = raw
            .capabilities
            .iter()
            .map(|c| Capability::from_str(c))
            .collect::<Result<Vec<_>, _>>()?;
        let mut editor = CapabilitySet::empty();
        for name in &raw.editor_capabilities {
            editor |= parse_editor_capability(name)
                .ok_or_else(|| ManifestError::EditorCapability(name.clone()))?;
        }
        let provides = raw
            .provides
            .iter()
            .map(|s| PluginSeam::from_str(s).map_err(|()| ManifestError::Seam(s.clone())))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            id: raw.id,
            requested,
            editor_capabilities: editor,
            provides,
            doc: raw.doc.filter(|d| !d.trim().is_empty()),
            default_mode: raw.default_mode.filter(|m| !m.trim().is_empty()),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// PI.4: the optional `doc` field parses (the `:describe-plugin` fallback
    /// doc source); absent or blank → `None`.
    #[test]
    fn parses_optional_doc_field() {
        let m = PluginManifest::from_toml_str(
            "id = \"git-gutter\"\ndoc = \"Shows git diff signs in the gutter.\"\n",
        )
        .unwrap();
        assert_eq!(
            m.doc.as_deref(),
            Some("Shows git diff signs in the gutter.")
        );

        let none = PluginManifest::from_toml_str("id = \"x\"\n").unwrap();
        assert!(none.doc.is_none());

        let blank = PluginManifest::from_toml_str("id = \"x\"\ndoc = \"   \"\n").unwrap();
        assert!(blank.doc.is_none(), "blank doc is normalised to None");
    }

    /// PM.3: `default_mode` parses (the `<id>.enabled` gate trigger); absent or
    /// blank → `None`.
    #[test]
    fn parses_optional_default_mode() {
        let m = PluginManifest::from_toml_str(
            "id = \"auto-pair\"\ndefault_mode = \"auto-pair-mode\"\n",
        )
        .unwrap();
        assert_eq!(m.default_mode.as_deref(), Some("auto-pair-mode"));
        assert!(
            PluginManifest::from_toml_str("id = \"x\"\n")
                .unwrap()
                .default_mode
                .is_none()
        );
        assert!(
            PluginManifest::from_toml_str("id = \"x\"\ndefault_mode = \" \"\n")
                .unwrap()
                .default_mode
                .is_none(),
            "blank default_mode normalises to None"
        );
    }

    /// PM.4: the SHIPPED `auto-pair` manifest declares its `default_mode` (+ the
    /// `tree-sitter` capability), so it enables out of the box via the gate. Reads
    /// the real manifest from the repo — a regression guard on the shipped file.
    #[test]
    fn shipped_auto_pair_manifest_declares_the_gate() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../plugins/auto-pair/plugin.toml"
        );
        let text = std::fs::read_to_string(path).expect("the auto-pair manifest exists");
        let m = PluginManifest::from_toml_str(&text).expect("it parses");
        assert_eq!(m.id, "auto-pair");
        assert_eq!(
            m.default_mode.as_deref(),
            Some("auto-pair-mode"),
            "the shipped manifest enables auto-pair-mode by default"
        );
        assert!(
            m.editor_capabilities
                .contains(lattice_mode::CapabilitySet::TREE_SITTER),
            "the shipped manifest declares the tree-sitter capability (manual style)"
        );
    }

    #[test]
    fn parses_each_capability_form() {
        assert_eq!(
            "fs:read:/home/alice/project".parse::<Capability>().unwrap(),
            Capability::FsRead(PathBuf::from("/home/alice/project"))
        );
        assert_eq!(
            "fs:write:/tmp/out".parse::<Capability>().unwrap(),
            Capability::FsWrite(PathBuf::from("/tmp/out"))
        );
        assert_eq!(
            "net:http:crates.io".parse::<Capability>().unwrap(),
            Capability::NetHttp("crates.io".to_string())
        );
        assert_eq!(
            "proc:spawn".parse::<Capability>().unwrap(),
            Capability::ProcSpawn
        );
    }

    #[test]
    fn capability_prefix_may_contain_colons() {
        // A `host:port` net entry and a path with a `:` both survive whole.
        assert_eq!(
            "net:http:localhost:8080".parse::<Capability>().unwrap(),
            Capability::NetHttp("localhost:8080".to_string())
        );
        assert_eq!(
            "fs:read:/weird:path".parse::<Capability>().unwrap(),
            Capability::FsRead(PathBuf::from("/weird:path"))
        );
    }

    #[test]
    fn display_round_trips_parse() {
        for s in [
            "fs:read:/a/b",
            "fs:write:/c",
            "net:http:example.com",
            "proc:spawn",
        ] {
            let cap: Capability = s.parse().unwrap();
            assert_eq!(cap.to_string(), s);
        }
    }

    #[test]
    fn a_safe_id_is_a_single_normal_component() {
        for ok in [
            "git-gutter",
            "fuzzy_finder",
            "a.b.c",
            "plugin123",
            "with space",
        ] {
            assert!(is_safe_plugin_id(ok), "`{ok}` should be accepted");
        }
    }

    #[test]
    fn a_path_escaping_id_is_rejected() {
        // SECURITY: each of these, joined into the writable data-mount path, would
        // escape the sandbox — they MUST be rejected (the CRITICAL isolation fix).
        for bad in [
            "",
            "   ",
            "/etc/cron.d",      // absolute → base discarded
            "../../../.ssh",    // traversal
            "..",               // parent
            ".",                // current
            "a/b",              // separator (nested)
            "sub/../../escape", // mixed
            "/",                // root
        ] {
            assert!(!is_safe_plugin_id(bad), "`{bad}` must be rejected");
        }
    }

    #[test]
    fn from_toml_rejects_a_path_escaping_id() {
        // The untrusted on-disk parse path refuses a crafted id with a typed error.
        for bad in ["/etc/cron.d", "../../victim", ".."] {
            let toml = format!("id = \"{bad}\"\n");
            assert!(
                matches!(
                    PluginManifest::from_toml_str(&toml),
                    Err(ManifestError::InvalidId(_))
                ),
                "`{bad}` must parse to InvalidId, not load"
            );
        }
        // A normal id still parses.
        assert!(PluginManifest::from_toml_str("id = \"git-gutter\"\n").is_ok());
    }

    #[test]
    fn plugin_seam_as_str_round_trips_from_str_for_every_variant() {
        // Every variant's wire word parses back to itself — pins the `as_str` /
        // `from_str` symmetry the trace-record + manifest paths both rely on.
        for seam in [
            PluginSeam::PickerSource,
            PluginSeam::CompletionSource,
            PluginSeam::Grammar,
            PluginSeam::Events,
            PluginSeam::Modes,
            PluginSeam::Config,
            PluginSeam::Decorations,
            PluginSeam::Keymap,
            PluginSeam::Logging,
        ] {
            assert_eq!(PluginSeam::from_str(seam.as_str()), Ok(seam));
        }
        assert_eq!(PluginSeam::from_str("logging"), Ok(PluginSeam::Logging));
        assert!(PluginSeam::from_str("nonsense").is_err());
    }

    #[test]
    fn malformed_capabilities_are_rejected() {
        for s in [
            "",
            "fs",
            "fs:read",      // no prefix
            "fs:read:",     // empty prefix
            "fs:exec:/a",   // unknown verb
            "net:http:",    // empty host
            "proc:kill",    // unknown proc verb
            "proc:spawn:x", // trailing junk
            "garbage",
        ] {
            assert!(
                s.parse::<Capability>().is_err(),
                "expected `{s}` to be rejected"
            );
        }
    }

    #[test]
    fn manifest_toml_round_trips() {
        let text = r#"
            id = "fuzzy-finder"
            capabilities = ["fs:read:/home/alice/project", "net:http:crates.io"]
            editor_capabilities = ["tree-sitter", "lsp"]
        "#;
        let m = PluginManifest::from_toml_str(text).unwrap();
        assert_eq!(m.id, "fuzzy-finder");
        assert_eq!(
            m.requested,
            vec![
                Capability::FsRead(PathBuf::from("/home/alice/project")),
                Capability::NetHttp("crates.io".to_string()),
            ]
        );
        assert_eq!(
            m.editor_capabilities,
            CapabilitySet::TREE_SITTER | CapabilitySet::LSP
        );
    }

    #[test]
    fn manifest_defaults_to_no_capabilities() {
        let m = PluginManifest::from_toml_str(r#"id = "bare""#).unwrap();
        assert!(m.requested.is_empty());
        assert_eq!(m.editor_capabilities, CapabilitySet::empty());
    }

    #[test]
    fn manifest_rejects_empty_id() {
        assert!(matches!(
            PluginManifest::from_toml_str(r#"id = "  ""#),
            Err(ManifestError::EmptyId)
        ));
    }

    #[test]
    fn manifest_rejects_bad_capability() {
        let text = r#"
            id = "x"
            capabilities = ["fs:teleport:/a"]
        "#;
        assert!(matches!(
            PluginManifest::from_toml_str(text),
            Err(ManifestError::Capability(_))
        ));
    }

    #[test]
    fn manifest_rejects_bad_editor_capability() {
        let text = r#"
            id = "x"
            editor_capabilities = ["telepathy"]
        "#;
        assert!(matches!(
            PluginManifest::from_toml_str(text),
            Err(ManifestError::EditorCapability(_))
        ));
    }

    /// OM.0 — the dependency the whole sort exists for. A mode's keymap
    /// binding resolves its command name at registration, so `grammar` must
    /// drain before `modes`.
    #[test]
    fn grammar_drains_before_modes() {
        assert!(PluginSeam::Grammar.drain_rank() < PluginSeam::Modes.drain_rank());
    }

    /// Declarations others reference come first; name-resolving seams come
    /// after the seam that registers the names.
    #[test]
    fn drain_ranks_order_declarations_before_consumers() {
        for declaring in [PluginSeam::Config, PluginSeam::Theme, PluginSeam::Language] {
            assert!(
                declaring.drain_rank() < PluginSeam::Grammar.drain_rank(),
                "{declaring} declares things later seams read"
            );
        }
        // `keymap` binds command names too, so it cannot precede `grammar`.
        assert!(PluginSeam::Grammar.drain_rank() < PluginSeam::Keymap.drain_rank());
    }

    /// Sorting must be total and stable — every seam has a rank, and equal
    /// ranks keep the author's ordering. Guards against a new `PluginSeam`
    /// variant being added without a thought about where it drains.
    #[test]
    fn sorting_is_stable_and_order_independent() {
        let ranked = |mut v: Vec<PluginSeam>| {
            v.sort_by_key(|s| s.drain_rank());
            v
        };
        let a = ranked(vec![
            PluginSeam::Modes,
            PluginSeam::Config,
            PluginSeam::Grammar,
        ]);
        let b = ranked(vec![
            PluginSeam::Grammar,
            PluginSeam::Modes,
            PluginSeam::Config,
        ]);
        assert_eq!(a, b, "two permutations sort to the same drain order");
        assert_eq!(
            a,
            vec![PluginSeam::Config, PluginSeam::Grammar, PluginSeam::Modes]
        );

        // Equal ranks preserve input order (stability), so an author's
        // intent survives wherever it cannot break anything.
        let same_rank = ranked(vec![PluginSeam::Help, PluginSeam::Events]);
        assert_eq!(same_rank, vec![PluginSeam::Help, PluginSeam::Events]);
    }
}
