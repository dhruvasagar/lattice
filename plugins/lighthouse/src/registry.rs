//! The server registry: which servers lighthouse can install, and from where.
//!
//! Data, not code. `registry.toml` is compiled in; a user may add or replace
//! entries with a file of the same shape in the plugin's data directory
//! (`registry.toml` there — an entry with a bundled server's name replaces
//! it). The host never sees any of this: it is asked to download a URL and
//! check a digest, and has no idea a registry exists.
//!
//! ## Why everything is validated on parse
//!
//! Every field here ends up somewhere it could do harm if it were wrong. The
//! `name` and `version` become directory names; `binary` becomes a path that
//! is marked executable and handed to the editor to run; `sha256` is the only
//! thing standing between the user and whatever the URL serves today. A
//! registry is edited by hand, so each of those is checked once, here, with
//! an error that names the server and the field — rather than discovered as a
//! confusing failure three steps into an install.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The bundled registry's text.
pub const BUNDLED: &str = include_str!("../registry.toml");

/// How a download is packed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Archive {
    /// One gzip-compressed file — the server binary itself.
    Gz,
    /// A gzip-compressed tarball.
    TarGz,
}

/// One server's download for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Build {
    pub url: String,
    pub sha256: String,
    pub archive: Archive,
    /// The executable, relative to the unpacked tree.
    pub binary: String,
}

/// One installable server.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Server {
    pub name: String,
    pub lsp_id: String,
    pub language_id: String,
    pub version: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub file_patterns: Vec<String>,
    #[serde(default)]
    pub root_markers: Vec<String>,
    /// Keyed `<os>-<arch>`.
    pub platform: BTreeMap<String, Build>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    server: Vec<Server>,
}

/// The registry key for a platform, as `host-platform` reports it.
pub fn platform_key(os: &str, arch: &str) -> String {
    format!("{os}-{arch}")
}

/// The parsed, validated registry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Registry {
    servers: Vec<Server>,
}

/// A string that is safe as ONE path component: it names a directory under
/// the install tree, so it must not be able to name anything else.
fn is_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+'))
}

/// A relative path that stays inside the tree it is joined to.
fn is_relative_inside(path: &str) -> bool {
    !path.is_empty() && path.split('/').all(is_component)
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `https://…`, or plain `http://` to this machine only — the same rule the
/// host's download seam enforces, checked here so a bad entry is reported
/// when the registry is read rather than when someone tries to install it.
fn is_allowed_url(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty() && !rest.starts_with('/');
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        // A bracketed IPv6 literal: the host is everything up to the `]`.
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority
            .rsplit_once(':')
            .map_or(authority, |(host, _port)| host),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

fn validate(server: &Server) -> Result<(), String> {
    let name = &server.name;
    let bad = |field: &str, why: &str| Err(format!("server '{name}': `{field}` {why}"));
    if !is_component(name) {
        return Err(format!(
            "server name '{name}' must be one path component \
             (letters, digits, `-`, `_`, `.`, `+`)"
        ));
    }
    if !is_component(&server.version) {
        return bad(
            "version",
            "must be one path component — it names the install directory",
        );
    }
    if server.lsp_id.trim().is_empty() {
        return bad("lsp-id", "is empty");
    }
    if server.language_id.trim().is_empty() {
        return bad("language-id", "is empty");
    }
    if server.file_patterns.is_empty() {
        return bad(
            "file-patterns",
            "is empty, so no buffer would ever start it",
        );
    }
    if server.platform.is_empty() {
        return bad(
            "platform",
            "has no entries, so it cannot be installed anywhere",
        );
    }
    for (platform, build) in &server.platform {
        let bad = |field: &str, why: &str| {
            Err(format!(
                "server '{name}', platform '{platform}': `{field}` {why}"
            ))
        };
        if !is_allowed_url(&build.url) {
            return bad("url", "must be https (plain http only to this machine)");
        }
        if !is_sha256(&build.sha256) {
            return bad("sha256", "must be 64 hexadecimal characters");
        }
        if !is_relative_inside(&build.binary) {
            return bad(
                "binary",
                "must be a relative path inside the unpacked tree, with no `..`",
            );
        }
        // A `.gz` is one file; it has no directories to put the binary in.
        if build.archive == Archive::Gz && build.binary.contains('/') {
            return bad("binary", "must be a bare file name for a `gz` archive");
        }
    }
    Ok(())
}

impl Registry {
    /// Parse and validate registry text. The first problem found is the error.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = toml::from_str(text).map_err(|e| e.message().to_string())?;
        let mut servers: Vec<Server> = Vec::with_capacity(file.server.len());
        for server in file.server {
            validate(&server)?;
            if servers.iter().any(|s| s.name == server.name) {
                return Err(format!("server '{}' is listed twice", server.name));
            }
            servers.push(server);
        }
        Ok(Self { servers })
    }

    /// The bundled registry, with `overlay` — the user's own file, if there
    /// is one — laid over it.
    ///
    /// A broken overlay does not cost the bundled servers: it is skipped, and
    /// the reason is returned beside the registry so the caller can show it.
    pub fn load(overlay: Option<&str>) -> (Self, Option<String>) {
        // The bundled text is pinned by a test, so this cannot fail in a
        // build that passed; an empty registry is the honest fallback.
        let mut registry = Self::parse(BUNDLED).unwrap_or_default();
        let mut problem = None;
        if let Some(text) = overlay {
            match Self::parse(text) {
                Ok(user) => {
                    for server in user.servers {
                        registry.servers.retain(|s| s.name != server.name);
                        registry.servers.push(server);
                    }
                }
                Err(e) => problem = Some(format!("registry.toml in the data directory: {e}")),
            }
        }
        registry.servers.sort_by(|a, b| a.name.cmp(&b.name));
        (registry, problem)
    }

    pub fn get(&self, name: &str) -> Option<&Server> {
        self.servers.iter().find(|s| s.name == name)
    }

    /// Every server name, sorted — for an error that says what *is* there.
    pub fn names(&self) -> Vec<&str> {
        self.servers.iter().map(|s| s.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = r#"
[[server]]
name = "zls"
lsp-id = "zig"
language-id = "zig"
version = "0.13.0"
args = ["--stdio"]
file-patterns = ["*.zig"]

[server.platform.linux-x86_64]
url = "https://example.org/zls.tar.gz"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
archive = "tar-gz"
binary = "bin/zls"
"#;

    fn with(replace: &str, by: &str) -> Result<Registry, String> {
        assert!(ONE.contains(replace), "fixture has no `{replace}`");
        Registry::parse(&ONE.replace(replace, by))
    }

    /// The registry that ships must parse — `load` falls back to an EMPTY
    /// registry otherwise, and the plugin would install nothing with no error
    /// anyone would see.
    #[test]
    fn the_bundled_registry_is_valid() {
        let registry = Registry::parse(BUNDLED).expect("registry.toml parses and validates");
        let ra = registry.get("rust-analyzer").expect("rust-analyzer ships");
        // Shadows the editor's own rust config, which is keyed `rust`.
        assert_eq!(ra.lsp_id, "rust");
        for platform in [
            "linux-x86_64",
            "linux-aarch64",
            "macos-x86_64",
            "macos-aarch64",
        ] {
            let build = ra
                .platform
                .get(platform)
                .unwrap_or_else(|| panic!("no {platform} build"));
            assert!(
                build.url.contains(&ra.version),
                "{platform}: url is for the pinned version"
            );
        }
    }

    /// Every host the bundled registry downloads from must be granted, or the
    /// install is refused at the first byte. Redirect targets cannot be read
    /// off the registry, so this pins the hosts that CAN be.
    #[test]
    fn every_bundled_download_host_is_in_the_manifest() {
        let manifest = include_str!("../plugin.toml");
        let registry = Registry::parse(BUNDLED).unwrap();
        for server in &registry.servers {
            for build in server.platform.values() {
                let host = build
                    .url
                    .strip_prefix("https://")
                    .and_then(|rest| rest.split('/').next())
                    .unwrap();
                assert!(
                    manifest.contains(&format!("\"net:http:{host}\"")),
                    "{}: plugin.toml has no `net:http:{host}`",
                    server.name
                );
            }
        }
    }

    #[test]
    fn a_well_formed_entry_parses() {
        let registry = Registry::parse(ONE).unwrap();
        let zls = registry.get("zls").unwrap();
        assert_eq!(zls.args, vec!["--stdio"]);
        assert!(zls.root_markers.is_empty(), "optional, defaults to none");
        let build = &zls.platform["linux-x86_64"];
        assert_eq!(build.archive, Archive::TarGz);
        assert_eq!(build.binary, "bin/zls");
        assert_eq!(registry.names(), vec!["zls"]);
        assert!(registry.get("gopls").is_none());
    }

    #[test]
    fn a_name_or_version_that_is_not_one_path_component_is_refused() {
        for bad in ["../evil", "a/b", "", ".hidden", "has space"] {
            let err = with("name = \"zls\"", &format!("name = \"{bad}\"")).unwrap_err();
            assert!(err.contains("one path component"), "{bad:?}: {err}");
        }
        let err = with("version = \"0.13.0\"", "version = \"../../etc\"").unwrap_err();
        assert!(err.contains("`version`"), "{err}");
    }

    #[test]
    fn a_binary_path_cannot_leave_the_tree() {
        for bad in ["/usr/bin/zls", "../zls", "bin/../../zls", "bin\\\\zls", ""] {
            let err = with("binary = \"bin/zls\"", &format!("binary = \"{bad}\"")).unwrap_err();
            assert!(err.contains("`binary`"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_gz_archive_holds_one_file_so_its_binary_is_a_bare_name() {
        let err = with("archive = \"tar-gz\"", "archive = \"gz\"").unwrap_err();
        assert!(err.contains("bare file name"), "{err}");
    }

    #[test]
    fn a_digest_is_mandatory_and_must_look_like_one() {
        let zeros = "0".repeat(64);
        for bad in ["", "abc", &"g".repeat(64), &"0".repeat(63)] {
            let err = with(&zeros, bad).unwrap_err();
            assert!(err.contains("`sha256`"), "{bad:?}: {err}");
        }
        let without = ONE
            .lines()
            .filter(|l| !l.starts_with("sha256"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            Registry::parse(&without).is_err(),
            "a missing digest is an error"
        );
    }

    #[test]
    fn only_https_or_this_machine() {
        let url = "https://example.org/zls.tar.gz";
        for bad in [
            "http://example.org/zls.tar.gz",
            "ftp://example.org/zls",
            "file:///etc/passwd",
            "https://",
            "http://127.0.0.1.evil.example/x",
            "http://localhost.evil.example:80/x",
        ] {
            let err = with(url, bad).unwrap_err();
            assert!(err.contains("`url`"), "{bad:?}: {err}");
        }
        for good in [
            "http://127.0.0.1:8080/zls.tar.gz",
            "http://localhost/zls.tar.gz",
            "http://[::1]:9/zls",
            "http://[::1]/zls",
        ] {
            assert!(with(url, good).is_ok(), "{good}");
        }
    }

    #[test]
    fn a_misspelt_field_is_an_error_not_a_silent_default() {
        let err = with("file-patterns", "file-pattern").unwrap_err();
        assert!(err.contains("file-pattern"), "{err}");
    }

    #[test]
    fn a_server_listed_twice_is_refused() {
        let err = Registry::parse(&format!("{ONE}\n{ONE}")).unwrap_err();
        assert!(err.contains("listed twice"), "{err}");
    }

    #[test]
    fn a_server_with_no_platform_or_no_patterns_is_refused() {
        let no_platform: String = ONE.split("[server.platform").next().unwrap().to_string();
        assert!(Registry::parse(&no_platform).is_err());
        let err = with("file-patterns = [\"*.zig\"]", "file-patterns = []").unwrap_err();
        assert!(err.contains("`file-patterns`"), "{err}");
    }

    #[test]
    fn an_overlay_adds_servers_and_replaces_a_bundled_one_by_name() {
        let (registry, problem) = Registry::load(Some(ONE));
        assert_eq!(problem, None);
        assert_eq!(registry.names(), vec!["rust-analyzer", "zls"]);

        let mine = ONE
            .replace("name = \"zls\"", "name = \"rust-analyzer\"")
            .replace("0.13.0", "my-build");
        let (registry, problem) = Registry::load(Some(&mine));
        assert_eq!(problem, None);
        assert_eq!(registry.names(), vec!["rust-analyzer"]);
        assert_eq!(registry.get("rust-analyzer").unwrap().version, "my-build");
    }

    #[test]
    fn a_broken_overlay_costs_nothing_but_itself_and_says_why() {
        let (registry, problem) = Registry::load(Some("this is not toml ["));
        assert!(
            registry.get("rust-analyzer").is_some(),
            "bundled servers survive"
        );
        let problem = problem.expect("the reason is returned");
        assert!(problem.contains("data directory"), "{problem}");
    }

    #[test]
    fn the_platform_key_joins_os_and_arch() {
        assert_eq!(platform_key("linux", "x86_64"), "linux-x86_64");
    }
}
