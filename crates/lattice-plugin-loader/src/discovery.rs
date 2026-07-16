//! On-disk plugin discovery (PL8.B).
//!
//! A plugin lives in its own directory under `<data>/lattice/plugins/`, holding
//! a `plugin.toml` manifest and a `.wasm` component. Discovery scans that tree,
//! parses each manifest, and reads the component bytes — every failure is a
//! logged skip, never fatal: one malformed plugin dir must not stop the others
//! or fail boot (the four-artefact graceful-degradation clause).

use std::path::{Path, PathBuf};

use lattice_plugin_host::PluginManifest;

/// The manifest filename inside a plugin directory.
const MANIFEST_FILE: &str = "plugin.toml";

/// A plugin found on disk, ready to load: its parsed manifest, the component
/// bytes, and the directory it came from (for diagnostics).
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub component_bytes: Vec<u8>,
    pub dir: PathBuf,
}

/// The default plugins directory: `<data>/lattice/plugins/` (XDG data on Linux,
/// Application Support on macOS, LocalAppData on Windows). `None` if the
/// platform has no data dir (the editor then loads no on-disk plugins).
pub fn default_plugins_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("lattice").join("plugins"))
}

/// The user's `init.rs` config plugin directory: `<config>/lattice/init/` (XDG
/// config on Linux, Application Support on macOS, RoamingAppData on Windows).
/// Holds the user's `init.rs`-compiled component + its `plugin.toml` (`id =
/// "init"`, `provides = [...]` for the seams it uses). Loaded at boot with a
/// boot-capability (`Bundled`) tier — it's the user's own trusted config, not an
/// external install. `None` if the platform has no config dir.
pub fn default_init_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("lattice").join("init"))
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
