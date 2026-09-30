//! AD.6: a manifest shown in the plugin guides is one the loader accepts.
//!
//! The authoring guide documents `plugin.toml` by example — keys, capability
//! forms, editor capabilities. Hand-written TOML in a guide goes stale the
//! same way hand-written code does (the guide named the file `manifest.toml`
//! for months after the loader stopped reading that name), and a reader
//! copies it. Every fenced block directly after a `<!-- manifest -->` marker
//! in `docs/dev/guides/*.md` is parsed here with the real parser, so a
//! renamed key or a dropped capability form fails this test instead of a
//! plugin author's first load.

use std::path::PathBuf;

use lattice_plugin_host::PluginManifest;

fn guides_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/dev/guides")
}

/// Every `<!-- manifest -->` block, as `(file:line, toml)`.
fn documented_manifests() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut files: Vec<PathBuf> = std::fs::read_dir(guides_dir())
        .expect("read docs/dev/guides")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    for file in files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_string();
        let text = std::fs::read_to_string(&file).expect("read a guide");
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.trim() != "<!-- manifest -->" {
                continue;
            }
            assert!(
                lines.get(i + 1).is_some_and(|l| l.starts_with("```toml")),
                "{name}:{}: `<!-- manifest -->` must be followed by a ```toml block",
                i + 1
            );
            let body: Vec<&str> = lines[i + 2..]
                .iter()
                .take_while(|l| **l != "```")
                .copied()
                .collect();
            out.push((format!("{name}:{}", i + 1), body.join("\n")));
        }
    }
    out
}

#[test]
fn every_documented_manifest_parses() {
    let manifests = documented_manifests();
    assert!(
        !manifests.is_empty(),
        "no `<!-- manifest -->` blocks found in docs/dev/guides — the authoring \
         guide's manifest example lost its marker"
    );
    for (at, toml) in manifests {
        if let Err(e) = PluginManifest::from_toml_str(&toml) {
            panic!("{at}: the documented manifest does not parse: {e:#}\n---\n{toml}");
        }
        // The parser ignores keys it does not know (no `deny_unknown_fields`
        // on `RawManifest`), so a misspelt key in a guide would parse. Pin the
        // documented keys to the ones `RawManifest` reads. When a key is added
        // there and documented, add it here; a key removed there must be
        // removed from the guides too.
        const KNOWN: &[&str] = &[
            "id",
            "capabilities",
            "editor_capabilities",
            "provides",
            "doc",
            "default_mode",
            "default_modes",
        ];
        for line in toml.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('"') {
                continue;
            }
            if let Some((key, _)) = line.split_once('=') {
                let key = key.trim();
                assert!(
                    KNOWN.contains(&key),
                    "{at}: `{key}` is not a key the manifest parser reads"
                );
            }
        }
    }
}
