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
#[error("unrecognised capability `{0}` (expected `fs:read:<p>` / `fs:write:<p>` / `net:http:<host>` / `proc:spawn`)")]
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
    #[error("unrecognised editor capability `{0}` (expected buffer-uri / lsp / tree-sitter / folds / writable / diagnostics)")]
    EditorCapability(String),

    /// The `id` field was empty — a plugin must have a non-empty id (it keys
    /// the data dir path).
    #[error("plugin manifest `id` must not be empty")]
    EmptyId,
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
        Ok(Self {
            id: raw.id,
            requested,
            editor_capabilities: editor,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

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
}
