//! On-disk plugin discovery (PL8.B).
//!
//! A plugin lives in its own directory under `~/.config/lattice/plugins/`,
//! holding a `plugin.toml` manifest and a `.wasm` component. Discovery scans that
//! tree, parses each manifest, and reads the component bytes — every failure is a
//! logged skip, never fatal: one malformed plugin dir must not stop the others
//! or fail boot (the four-artefact graceful-degradation clause).

use std::path::{Path, PathBuf};

use lattice_plugin_host::PluginManifest;

/// The manifest filename inside a plugin directory.
const MANIFEST_FILE: &str = "plugin.toml";

/// The lattice config home — `~/.config` on **both Linux and macOS** (honoring
/// `$XDG_CONFIG_HOME`), `%APPDATA%` on Windows. Reuses the canonical
/// [`lattice_config::config_home`] the TOML config root already uses, so plugins
/// / `init.rs` live under the SAME `~/.config/lattice/` tree as `lattice.toml` —
/// NOT the macOS-native `~/Library/Application Support` that `dirs::config_dir`
/// returns (the convention Helix / Neovim / Zed / alacritty follow on macOS).
fn config_root() -> Option<PathBuf> {
    lattice_config::config_home()
}

/// A plugin found on disk, ready to load: its parsed manifest, the component
/// bytes, and the directory it came from (for diagnostics).
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub component_bytes: Vec<u8>,
    pub dir: PathBuf,
    /// PM.8a: where this plugin came from, read from its `.source` marker.
    /// [`SourceRecord::Unknown`] for a hand-installed plugin or one staged by
    /// a lattice predating the marker — the honest answer, rather than
    /// guessing `Local` and putting a wrong path in the view.
    pub source: crate::source_record::SourceRecord,
}

/// The default plugins directory: `~/.config/lattice/plugins/` on Linux AND
/// macOS (honoring `$XDG_CONFIG_HOME`), `%APPDATA%\lattice\plugins` on Windows.
/// `None` if the platform has no config dir (the editor then loads no on-disk
/// plugins).
pub fn default_plugins_dir() -> Option<PathBuf> {
    config_root().map(|d| d.join("lattice").join("plugins"))
}

/// The **core-plugins root** — prebuilt plugins that ship WITH lattice
/// (plugin-manager.md §7 / PM.1). Distinct from [`default_plugins_dir`] (the
/// user's `require`+build cache): core plugins are the batteries-included set,
/// discovered at boot at the `Bundled` tier. Resolved via a SEARCH PATH — the
/// first *existing* candidate wins, except an explicit `$LATTICE_RUNTIME` override
/// always wins (whether or not it exists yet):
///
/// 1. `$LATTICE_RUNTIME/plugins` — explicit override,
/// 2. `<LATTICE_INSTALL_PREFIX>/share/lattice/plugins` — the prefix a packager
///    bakes in at build time (`option_env!`),
/// 3. `<exe-dir>/../share/lattice/plugins` — a relocatable install / `.app`,
/// 4. `<exe-dir>/../../runtime/plugins` — dev, running from `target/<profile>/`.
///
/// `None` when no candidate exists — the editor then loads no core plugins (a
/// benign skip, like an absent user plugins dir).
pub fn default_core_plugins_dir() -> Option<PathBuf> {
    core_plugins_dir_from(
        std::env::var_os("LATTICE_RUNTIME"),
        option_env!("LATTICE_INSTALL_PREFIX"),
        std::env::current_exe().ok().as_deref(),
    )
}

/// The pure search-path core of [`default_core_plugins_dir`] — takes the resolved
/// inputs so it's testable without touching the process environment.
fn core_plugins_dir_from(
    runtime_env: Option<std::ffi::OsString>,
    install_prefix: Option<&str>,
    exe: Option<&Path>,
) -> Option<PathBuf> {
    // Explicit override wins unconditionally (existence is discovery's concern).
    if let Some(root) = runtime_env {
        return Some(PathBuf::from(root).join("plugins"));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(prefix) = install_prefix {
        candidates.push(
            Path::new(prefix)
                .join("share")
                .join("lattice")
                .join("plugins"),
        );
    }
    if let Some(dir) = exe.and_then(Path::parent) {
        // Installed: `<prefix>/bin/lattice` → `<prefix>/share/lattice/plugins`.
        candidates.push(dir.join("..").join("share").join("lattice").join("plugins"));
        // Dev: `<workspace>/target/<profile>/lattice` → `<workspace>/runtime/plugins`.
        candidates.push(dir.join("..").join("..").join("runtime").join("plugins"));
    }
    candidates.into_iter().find(|p| p.exists())
}

/// The user's `init.rs` config plugin directory: `~/.config/lattice/init/` on
/// Linux AND macOS (honoring `$XDG_CONFIG_HOME`), `%APPDATA%\lattice\init` on
/// Windows. Holds the user's `init.rs`-compiled component + its `plugin.toml`
/// (`id = "init"`, `provides = [...]` for the seams it uses). Loaded at boot with
/// a boot-capability (`Bundled`) tier — it's the user's own trusted config, not
/// an external install. `None` if the platform has no config dir.
pub fn default_init_dir() -> Option<PathBuf> {
    config_root().map(|d| d.join("lattice").join("init"))
}

/// PM.6/PM.7b: the git source cache — `~/.cache/lattice/sources/`.
///
/// A *cache*, not config: a deleted checkout is re-cloned, so it belongs under
/// the cache root rather than beside the user's `plugin.toml`s. Falls back to
/// the config root when the platform has no cache dir, which keeps the
/// resolver working rather than failing on an unusual platform.
pub fn default_source_cache_dir() -> std::path::PathBuf {
    dirs::cache_dir()
        .or_else(config_root)
        .unwrap_or_else(std::env::temp_dir)
        .join("lattice")
        .join("sources")
}

/// Scan `dir` for plugin subdirectories, returning every one that parses. A
/// missing `dir` yields an empty list (no plugins installed — normal). Each
/// subdirectory needs a `plugin.toml` + exactly one `.wasm`; anything else is
/// logged at `warn`/`debug` and skipped.
pub fn discover(dir: &Path) -> Vec<DiscoveredPlugin> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            // A missing plugins dir is the common, benign case (no plugins
            // installed) — `debug`, not `warn`.
            tracing::debug!(
                path = %dir.display(),
                error = %err,
                "plugins dir not readable; loading no on-disk plugins"
            );
            return Vec::new();
        }
    };

    let mut found = Vec::new();
    for entry in entries.flatten() {
        let plugin_dir = entry.path();
        if !plugin_dir.is_dir() {
            continue;
        }
        match load_one(&plugin_dir) {
            Ok(Some(plugin)) => found.push(plugin),
            Ok(None) => {} // not a plugin dir (no manifest) — silently skip.
            Err(reason) => tracing::warn!(
                path = %plugin_dir.display(),
                reason,
                "skipping malformed plugin dir"
            ),
        }
    }
    found
}

/// Parse a single explicitly-named plugin directory — the `:plugin-load <path>`
/// entry point (PL8.C). Unlike [`discover`] (which scans a tree and silently
/// skips non-plugin subdirs), this is a direct request for *one* dir, so a
/// missing manifest is an error the user sees, not a silent skip.
pub fn discover_one(plugin_dir: &Path) -> Result<DiscoveredPlugin, String> {
    match load_one(plugin_dir) {
        Ok(Some(plugin)) => Ok(plugin),
        Ok(None) => Err(format!(
            "no `{MANIFEST_FILE}` in {} (not a plugin directory)",
            plugin_dir.display()
        )),
        Err(reason) => Err(reason),
    }
}

/// Parse a single plugin directory. `Ok(None)` if it has no manifest (not a
/// plugin dir); `Err(reason)` if it has a manifest but is otherwise malformed
/// (bad TOML, missing/ambiguous component) — the caller logs the reason.
fn load_one(plugin_dir: &Path) -> Result<Option<DiscoveredPlugin>, String> {
    let manifest_path = plugin_dir.join(MANIFEST_FILE);
    if !manifest_path.exists() {
        return Ok(None);
    }
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("cannot read {MANIFEST_FILE}: {e}"))?;
    let manifest = PluginManifest::from_toml_str(&manifest_text)
        .map_err(|e| format!("invalid manifest: {e}"))?;

    let component_path = sole_wasm(plugin_dir)?;
    let component_bytes = std::fs::read(&component_path)
        .map_err(|e| format!("cannot read component {}: {e}", component_path.display()))?;

    Ok(Some(DiscoveredPlugin {
        manifest,
        component_bytes,
        dir: plugin_dir.to_path_buf(),
        source: crate::source_record::read(plugin_dir),
    }))
}

/// The single `.wasm` file in `plugin_dir`. An error if there is none or more
/// than one — the manifest does not name the component, so exactly one is the
/// unambiguous contract.
fn sole_wasm(plugin_dir: &Path) -> Result<PathBuf, String> {
    let mut wasm: Vec<PathBuf> = Vec::new();
    let entries =
        std::fs::read_dir(plugin_dir).map_err(|e| format!("cannot read plugin dir: {e}"))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "wasm") {
            wasm.push(path);
        }
    }
    match wasm.len() {
        1 => Ok(wasm.into_iter().next().expect("len checked == 1")),
        0 => Err("no `.wasm` component found".to_string()),
        n => Err(format!("{n} `.wasm` files found; expected exactly one")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::core_plugins_dir_from;
    use std::path::{Path, PathBuf};

    #[test]
    fn runtime_env_override_wins_unconditionally() {
        // The override is used even when it doesn't exist (discovery skips a
        // missing dir); no other candidate is consulted.
        let got = core_plugins_dir_from(
            Some("/opt/lattice-runtime".into()),
            Some("/usr"),
            Some(Path::new("/usr/bin/lattice")),
        );
        assert_eq!(got, Some(PathBuf::from("/opt/lattice-runtime/plugins")));
    }

    #[test]
    fn install_prefix_beats_exe_relative_when_it_exists() {
        // A real dir for the prefix candidate; exe-relative candidates don't
        // exist, so the prefix wins.
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let plugins = prefix.join("share").join("lattice").join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let got = core_plugins_dir_from(
            None,
            Some(prefix.to_str().unwrap()),
            Some(Path::new("/nowhere/bin/lattice")),
        );
        assert_eq!(got, Some(plugins));
    }

    #[test]
    fn falls_through_to_the_dev_runtime_dir() {
        // No override, no prefix; the exe-relative dev candidate
        // (`<exe>/../../runtime/plugins`) exists.
        let tmp = tempfile::tempdir().unwrap();
        // Simulate `<workspace>/target/debug/lattice`.
        let exe = tmp.path().join("target").join("debug").join("lattice");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        let dev_plugins = tmp.path().join("runtime").join("plugins");
        std::fs::create_dir_all(&dev_plugins).unwrap();
        let got = core_plugins_dir_from(None, None, Some(&exe));
        // `<exe>/../../runtime/plugins` normalises to the created dir.
        assert_eq!(got.map(|p| p.exists()), Some(true));
        assert!(got_matches(&exe, &dev_plugins));
    }

    #[test]
    fn none_when_no_candidate_exists() {
        assert_eq!(
            core_plugins_dir_from(None, None, Some(Path::new("/nowhere/bin/lattice"))),
            None
        );
        // No exe at all (current_exe failed) + no prefix → None.
        assert_eq!(core_plugins_dir_from(None, None, None), None);
    }

    // The dev candidate path contains `..` segments; compare by canonicalized
    // existence rather than literal equality.
    fn got_matches(exe: &Path, expected_existing: &Path) -> bool {
        let got = core_plugins_dir_from(None, None, Some(exe)).unwrap();
        got.canonicalize().ok() == expected_existing.canonicalize().ok()
    }
}
