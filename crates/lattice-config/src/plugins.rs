//! Release plugins declared in `lattice.toml`: the `[plugins]` table.
//!
//! ```toml
//! [plugins.org]
//! release = "owner/lattice-org-plugin"
//! version = "v0.2.0"        # optional
//! sha256 = "…64 hex…"       # optional
//! enable-mode = "org-mode"  # optional
//!
//! [plugins]
//! extras = "someone/lattice-extras@v1.2.0"   # shorthand: repo[@tag]
//! ```
//!
//! This module checks **shape** only and produces neutral
//! [`PluginDeclaration`]s; the plugin loader lowers them onto the same install
//! queue `init.rs`'s `require-release` fills. What a declaration *means* is
//! checked where `init.rs`'s declarations are checked, so the two front-ends
//! share one validator per fact: the id's path safety at lowering, the repo and
//! tag syntax at install. See `docs/dev/architecture/plugin-toml.md` §4.
//!
//! Every entry stands alone. A malformed one becomes a [`RejectedDeclaration`]
//! carrying a user-facing reason and never stops the others.

/// The top-level `lattice.toml` key holding plugin declarations. Reserved: it
/// is not an option namespace.
pub const PLUGINS_TABLE: &str = "plugins";

/// The keys a `[plugins.<id>]` table may carry.
const RELEASE_KEYS: &[&str] = &["release", "version", "sha256", "enable-mode"];

/// Keys naming a source that builds from source, which `lattice.toml` cannot
/// declare: building needs the toolchain this front-end exists to avoid.
const SOURCE_KEYS: &[&str] = &["git", "path", "url", "build", "rev", "branch", "local"];

/// One release plugin declared in `lattice.toml`, shape-checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDeclaration {
    /// The table key: the plugin's id, its install directory and the
    /// namespace of its options.
    pub id: String,
    /// `owner/name` of the repository whose releases carry the plugin.
    pub repo: String,
    /// The release tag; `None` takes the newest release built for this
    /// editor's plugin API.
    pub version: Option<String>,
    /// A pin on the component's bytes, 64 hex characters.
    pub sha256: Option<String>,
    /// A mode to enable once the plugin loads; opaque here.
    pub enable_mode: Option<String>,
}

/// A `[plugins]` entry that could not be read as a declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedDeclaration {
    /// The entry's key, or [`PLUGINS_TABLE`] when the table itself is wrong.
    pub id: String,
    /// Why, written for the user: it is logged and shown in `:plugins`.
    pub reason: String,
}

/// Everything read from one `[plugins]` table.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PluginDeclarations {
    /// The entries that read as release declarations, in key order.
    pub declared: Vec<PluginDeclaration>,
    /// The entries that did not, each with its reason.
    pub rejected: Vec<RejectedDeclaration>,
}

/// Read the `[plugins]` table of a parsed configuration file.
///
/// A file with no `plugins` key yields nothing. Entries come back in the
/// table's key order, which for `toml::Table` is alphabetical; installs are
/// independent, so order carries no meaning.
pub fn parse_plugin_declarations(root: &toml::Table) -> PluginDeclarations {
    let mut out = PluginDeclarations::default();
    let Some(value) = root.get(PLUGINS_TABLE) else {
        return out;
    };
    let Some(table) = value.as_table() else {
        out.rejected.push(RejectedDeclaration {
            id: PLUGINS_TABLE.to_string(),
            reason: format!(
                "`{PLUGINS_TABLE}` must be a table of `<id> = \"owner/name[@tag]\"` \
                 or `[{PLUGINS_TABLE}.<id>]` entries, not a {}",
                value.type_str()
            ),
        });
        return out;
    };
    for (id, entry) in table {
        match parse_entry(id, entry) {
            Ok(declaration) => out.declared.push(declaration),
            Err(reason) => out.rejected.push(RejectedDeclaration {
                id: id.clone(),
                reason: format!("`{PLUGINS_TABLE}.{id}`: {reason}"),
            }),
        }
    }
    out
}

/// Read the configuration file at `path` and parse its `[plugins]` table.
///
/// `None` when the file is absent, unreadable or does not parse. None of those
/// is reported here: this is a second reader of the user's `lattice.toml`, and
/// the editor's own load of the same file already tells the user what is wrong
/// with it. Saying it twice is noise.
pub fn read_plugin_declarations(path: &std::path::Path) -> Option<PluginDeclarations> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::debug!(path = %path.display(), error = %e, "plugins: config unreadable");
            }
        })
        .ok()?;
    let root: toml::Table = text
        .parse()
        .map_err(|_| {
            tracing::debug!(
                path = %path.display(),
                "plugins: config does not parse; the config load reports why"
            );
        })
        .ok()?;
    Some(parse_plugin_declarations(&root))
}

fn parse_entry(id: &str, entry: &toml::Value) -> Result<PluginDeclaration, String> {
    match entry {
        toml::Value::String(spec) => parse_shorthand(id, spec),
        toml::Value::Table(table) => parse_table(id, table),
        other => Err(format!(
            "expected `\"owner/name[@tag]\"` or a table with `release`, not a {}",
            other.type_str()
        )),
    }
}

/// `owner/name` or `owner/name@tag`. A repository name cannot contain `@`, so
/// the first one separates the tag.
fn parse_shorthand(id: &str, spec: &str) -> Result<PluginDeclaration, String> {
    let (repo, version) = match spec.split_once('@') {
        Some((repo, tag)) => (repo, Some(tag)),
        None => (spec, None),
    };
    let repo = non_empty(repo, "the repository before `@`")?;
    let version = version
        .map(|tag| non_empty(tag, "the tag after `@` (omit `@` for the newest release)"))
        .transpose()?;
    Ok(PluginDeclaration {
        id: id.to_string(),
        repo,
        version,
        sha256: None,
        enable_mode: None,
    })
}

fn parse_table(id: &str, table: &toml::Table) -> Result<PluginDeclaration, String> {
    // Every key is checked before any value, so the reason names the real
    // problem: a `git` key is "declare it in init.rs" even when `release` is
    // missing too.
    for key in table.keys() {
        if RELEASE_KEYS.contains(&key.as_str()) {
            continue;
        }
        if SOURCE_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "`{key}` builds from source, which lattice.toml cannot declare; \
                 declare it in init.rs, or use `release = \"owner/name\"`"
            ));
        }
        // Refusing the whole entry, rather than dropping the key, is what keeps
        // a misspelt `sha256` from installing unpinned.
        let hint = if key == "tag" {
            " (the tag is `version`)"
        } else {
            ""
        };
        return Err(format!(
            "unknown key `{key}`{hint}; expected one of {}",
            RELEASE_KEYS.join(", ")
        ));
    }
    let repo = string_key(table, "release")?.ok_or_else(|| {
        "missing `release = \"owner/name\"`, the repository that publishes the plugin".to_string()
    })?;
    let sha256 = string_key(table, "sha256")?;
    if let Some(pin) = &sha256
        && !(pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(format!(
            "`sha256` must be 64 hex characters (a SHA-256 digest), got `{pin}`"
        ));
    }
    Ok(PluginDeclaration {
        id: id.to_string(),
        repo,
        version: string_key(table, "version")?,
        sha256,
        enable_mode: string_key(table, "enable-mode")?,
    })
}

/// `table[key]` as a non-empty string, `None` when absent.
fn string_key(table: &toml::Table, key: &str) -> Result<Option<String>, String> {
    match table.get(key) {
        None => Ok(None),
        Some(toml::Value::String(s)) => non_empty(s, &format!("`{key}`")).map(Some),
        Some(other) => Err(format!(
            "`{key}` must be a string, not a {}",
            other.type_str()
        )),
    }
}

/// An empty string is never a value the user meant: an empty `version` is
/// not "newest", it is a key left half-written.
fn non_empty(s: &str, what: &str) -> Result<String, String> {
    if s.trim().is_empty() {
        Err(format!("{what} is empty"))
    } else {
        Ok(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn parse(toml_text: &str) -> PluginDeclarations {
        parse_plugin_declarations(&toml_text.parse::<toml::Table>().unwrap())
    }

    fn only_rejection(toml_text: &str) -> RejectedDeclaration {
        let out = parse(toml_text);
        assert!(out.declared.is_empty(), "declared: {:?}", out.declared);
        assert_eq!(out.rejected.len(), 1, "rejected: {:?}", out.rejected);
        out.rejected.into_iter().next().unwrap()
    }

    const PIN: &str = "3f9a000000000000000000000000000000000000000000000000000000000abc";

    #[test]
    fn a_full_table_reads_every_field() {
        let out = parse(&format!(
            r#"
            [plugins.org]
            release = "owner/lattice-org-plugin"
            version = "v0.2.0"
            sha256 = "{PIN}"
            enable-mode = "org-mode"
            "#
        ));
        assert!(out.rejected.is_empty(), "{:?}", out.rejected);
        assert_eq!(
            out.declared,
            vec![PluginDeclaration {
                id: "org".into(),
                repo: "owner/lattice-org-plugin".into(),
                version: Some("v0.2.0".into()),
                sha256: Some(PIN.into()),
                enable_mode: Some("org-mode".into()),
            }]
        );
    }

    #[test]
    fn a_table_with_only_release_takes_the_newest() {
        let out = parse("[plugins.org]\nrelease = \"owner/org\"\n");
        assert_eq!(out.declared.len(), 1);
        let d = &out.declared[0];
        assert_eq!(d.repo, "owner/org");
        assert_eq!(
            (&d.version, &d.sha256, &d.enable_mode),
            (&None, &None, &None)
        );
    }

    #[test]
    fn the_shorthand_takes_a_repo_and_an_optional_tag() {
        let out = parse(
            r#"
            [plugins]
            newest = "owner/newest"
            pinned = "owner/pinned@v1.2.0"
            "#,
        );
        assert!(out.rejected.is_empty(), "{:?}", out.rejected);
        let by_id = |id: &str| out.declared.iter().find(|d| d.id == id).unwrap();
        assert_eq!(by_id("newest").repo, "owner/newest");
        assert_eq!(by_id("newest").version, None);
        assert_eq!(by_id("pinned").repo, "owner/pinned");
        assert_eq!(by_id("pinned").version.as_deref(), Some("v1.2.0"));
    }

    #[test]
    fn the_shorthand_and_tables_mix_in_one_file() {
        let out = parse(
            r#"
            [plugins]
            short = "owner/short"

            [plugins.long]
            release = "owner/long"
            "#,
        );
        let ids: Vec<_> = out.declared.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["long", "short"]);
    }

    #[test]
    fn no_plugins_table_and_an_empty_one_declare_nothing() {
        assert_eq!(parse("tabstop = 4\n"), PluginDeclarations::default());
        assert_eq!(parse("[plugins]\n"), PluginDeclarations::default());
    }

    #[test]
    fn a_plugins_key_that_is_not_a_table_is_one_rejection() {
        let r = only_rejection("plugins = [\"owner/org\"]\n");
        assert_eq!(r.id, PLUGINS_TABLE);
        assert!(r.reason.contains("must be a table"), "{}", r.reason);
        assert!(r.reason.contains("array"), "{}", r.reason);
    }

    #[test]
    fn an_entry_that_is_neither_string_nor_table_is_rejected() {
        let r = only_rejection("[plugins]\norg = 3\n");
        assert_eq!(r.id, "org");
        assert!(r.reason.starts_with("`plugins.org`:"), "{}", r.reason);
        assert!(r.reason.contains("integer"), "{}", r.reason);
    }

    #[test]
    fn a_table_without_release_is_rejected() {
        let r = only_rejection("[plugins.org]\nversion = \"v1\"\n");
        assert!(r.reason.contains("missing `release"), "{}", r.reason);
    }

    #[test]
    fn a_non_string_value_is_rejected() {
        let r = only_rejection("[plugins.org]\nrelease = \"owner/org\"\nversion = 2\n");
        assert!(
            r.reason.contains("`version` must be a string"),
            "{}",
            r.reason
        );
    }

    #[test]
    fn every_build_from_source_key_points_at_init_rs() {
        for key in SOURCE_KEYS {
            let r = only_rejection(&format!(
                "[plugins.org]\nrelease = \"owner/org\"\n{key} = \"x\"\n"
            ));
            assert!(
                r.reason.contains("declare it in init.rs"),
                "{key}: {}",
                r.reason
            );
            assert!(
                r.reason.contains(&format!("`{key}`")),
                "{key}: {}",
                r.reason
            );
        }
    }

    #[test]
    fn a_source_key_is_named_even_without_release() {
        let r = only_rejection("[plugins.org]\ngit = \"https://example.com/org\"\n");
        assert!(r.reason.contains("declare it in init.rs"), "{}", r.reason);
    }

    /// A misspelt pin dropped quietly would install the plugin unpinned, which
    /// is the one outcome the user wrote the pin to prevent.
    #[test]
    fn an_unknown_key_rejects_the_whole_entry() {
        let r = only_rejection(&format!(
            "[plugins.org]\nrelease = \"owner/org\"\nsha265 = \"{PIN}\"\n"
        ));
        assert!(r.reason.contains("unknown key `sha265`"), "{}", r.reason);
    }

    #[test]
    fn tag_is_pointed_at_version() {
        let r = only_rejection("[plugins.org]\nrelease = \"owner/org\"\ntag = \"v1\"\n");
        assert!(r.reason.contains("the tag is `version`"), "{}", r.reason);
    }

    #[test]
    fn a_malformed_sha256_is_rejected_not_ignored() {
        for bad in ["abc", &"g".repeat(64), &"a".repeat(65)] {
            let r = only_rejection(&format!(
                "[plugins.org]\nrelease = \"owner/org\"\nsha256 = \"{bad}\"\n"
            ));
            assert!(
                r.reason.contains("64 hex characters"),
                "{bad}: {}",
                r.reason
            );
        }
    }

    #[test]
    fn an_uppercase_sha256_is_accepted() {
        let out = parse(&format!(
            "[plugins.org]\nrelease = \"owner/org\"\nsha256 = \"{}\"\n",
            PIN.to_ascii_uppercase()
        ));
        assert_eq!(out.declared.len(), 1, "{:?}", out.rejected);
    }

    #[test]
    fn empty_strings_are_rejected() {
        for entry in ["org = \"\"", "org = \"@v1\"", "org = \"owner/org@\""] {
            let r = only_rejection(&format!("[plugins]\n{entry}\n"));
            assert!(r.reason.contains("is empty"), "{entry}: {}", r.reason);
        }
        let r = only_rejection("[plugins.org]\nrelease = \"owner/org\"\nversion = \" \"\n");
        assert!(r.reason.contains("`version` is empty"), "{}", r.reason);
    }

    #[test]
    fn one_bad_entry_does_not_stop_the_others() {
        let out = parse(
            r#"
            [plugins]
            alpha = "owner/alpha"
            broken = 7
            omega = "owner/omega@v2"

            [plugins.source]
            git = "https://example.com/x"
            "#,
        );
        let ok: Vec<_> = out.declared.iter().map(|d| d.id.as_str()).collect();
        let bad: Vec<_> = out.rejected.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ok, ["alpha", "omega"]);
        assert_eq!(bad, ["broken", "source"]);
    }

    #[test]
    fn reading_a_missing_or_broken_file_declares_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lattice.toml");
        assert_eq!(read_plugin_declarations(&path), None);
        std::fs::write(&path, "[plugins\norg = ").unwrap();
        assert_eq!(read_plugin_declarations(&path), None);
    }

    #[test]
    fn reading_a_file_parses_its_plugins_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lattice.toml");
        std::fs::write(&path, "tabstop = 4\n[plugins]\norg = \"owner/org@v1\"\n").unwrap();
        let out = read_plugin_declarations(&path).unwrap();
        assert_eq!(out.declared.len(), 1);
        assert_eq!(out.declared[0].version.as_deref(), Some("v1"));
    }

    /// Repo and tag syntax belong to the install step's validator, shared with
    /// `init.rs`; checking them here too would be a second validator to drift.
    #[test]
    fn repo_syntax_is_left_to_the_install_step() {
        let out = parse("[plugins]\norg = \"not a repo\"\n");
        assert_eq!(out.declared.len(), 1);
        assert_eq!(out.declared[0].repo, "not a repo");
    }
}
