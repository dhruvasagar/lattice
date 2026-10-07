//! On-disk plugin discovery (PL8.B).
//!
//! A plugin lives in its own directory under `~/.config/lattice/plugins/`,
//! holding a `plugin.toml` manifest and a `.wasm` component. Discovery scans that
//! tree, parses each manifest, and reads the component bytes — every failure is a
//! logged skip, never fatal: one malformed plugin dir must not stop the others
//! or fail boot (the four-artefact graceful-degradation clause).

use std::ffi::OsString;
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
/// `<exe-dir>` is tried twice for 3 and 4: first the directory of the path the
/// binary was **invoked through** ([`invoked_exe`]), then the directory of the
/// file it resolves to (`current_exe`). They differ when the binary is reached
/// through a symlink — `~/.local/bin/lattice -> ~/.cargo/bin/lattice` — and
/// the plugins were installed beside the link, not beside its target. On
/// Linux `current_exe` reads `/proc/self/exe`, which is always the target, so
/// looking there alone found nothing and loaded no core plugins, silently.
///
/// `None` when no candidate exists — the editor then loads no core plugins (a
/// benign skip, like an absent user plugins dir).
pub fn default_core_plugins_dir() -> Option<PathBuf> {
    let invoked = invoked_exe();
    let resolved = std::env::current_exe().ok();
    let exes: Vec<&Path> = [invoked.as_deref(), resolved.as_deref()]
        .into_iter()
        .flatten()
        .collect();
    core_plugins_dir_from(
        std::env::var_os("LATTICE_RUNTIME"),
        option_env!("LATTICE_INSTALL_PREFIX"),
        &exes,
    )
}

/// The path this process was started through, symlinks NOT followed: `argv[0]`
/// made absolute. `None` when it cannot be turned into a file that exists —
/// `argv[0]` is whatever the parent process chose to pass, so it is a hint to
/// check, never a fact to trust.
fn invoked_exe() -> Option<PathBuf> {
    invoked_exe_from(
        &PathBuf::from(std::env::args_os().next()?),
        std::env::var_os("PATH"),
        std::env::current_dir().ok().as_deref(),
    )
}

/// The pure core of [`invoked_exe`]. An `argv[0]` with a directory part is
/// taken as written (relative to `cwd`); a bare name was found by the shell on
/// `$PATH`, so it is looked up there the same way. A bare name on Windows
/// carries no `.exe` and so finds nothing; the resolved path covers it.
fn invoked_exe_from(
    arg0: &Path,
    path_env: Option<OsString>,
    cwd: Option<&Path>,
) -> Option<PathBuf> {
    let found = if arg0.is_absolute() {
        arg0.to_path_buf()
    } else if arg0.components().count() > 1 {
        cwd?.join(arg0)
    } else {
        std::env::split_paths(&path_env?)
            .map(|dir| dir.join(arg0))
            .find(|candidate| candidate.is_file())?
    };
    found.is_file().then_some(found)
}

/// The pure search-path core of [`default_core_plugins_dir`] — takes the resolved
/// inputs so it's testable without touching the process environment. `exes` is
/// in priority order; each contributes the installed and the dev candidate.
fn core_plugins_dir_from(
    runtime_env: Option<OsString>,
    install_prefix: Option<&str>,
    exes: &[&Path],
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
    for dir in exes.iter().filter_map(|exe| exe.parent()) {
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

/// PM.6/PM.7b: the git source cache — `~/.config/lattice/cache/sources/`.
///
/// A *cache*, not config: a deleted checkout is re-cloned, so it belongs in
/// `<config-home>/lattice/cache/sources/` rather than beside the user's
/// `plugin.toml`s. Falls back to the temp dir when no config home resolves,
/// which keeps the resolver working rather than failing.
pub fn default_source_cache_dir() -> std::path::PathBuf {
    let dir = lattice_config::cache_home()
        .unwrap_or_else(std::env::temp_dir)
        .join("sources");
    // Once: carry checkouts from the pre-0.9.2 `dirs::cache_dir()` location
    // rather than re-cloning (and rebuilding) every `require`d plugin.
    if let Some(legacy) = dirs::cache_dir().map(|d| d.join("lattice").join("sources")) {
        lattice_config::migrate_path(&legacy, &dir);
    }
    dir
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
    use super::{core_plugins_dir_from, invoked_exe_from};
    use std::path::{Path, PathBuf};

    #[test]
    fn runtime_env_override_wins_unconditionally() {
        // The override is used even when it doesn't exist (discovery skips a
        // missing dir); no other candidate is consulted.
        let got = core_plugins_dir_from(
            Some("/opt/lattice-runtime".into()),
            Some("/usr"),
            &[Path::new("/usr/bin/lattice")],
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
            &[Path::new("/nowhere/bin/lattice")],
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
        let got = core_plugins_dir_from(None, None, &[&exe]);
        // `<exe>/../../runtime/plugins` normalises to the created dir.
        assert_eq!(got.map(|p| p.exists()), Some(true));
        assert!(got_matches(&exe, &dev_plugins));
    }

    #[test]
    fn resolves_a_relocatable_install_from_the_bin_dir() {
        // The release-archive layout (launch-0.9.md §3): the user extracts
        // `lattice-<ver>-<platform>/` anywhere and runs `bin/lattice`, which
        // must find `../share/lattice/plugins` beside it. No baked prefix —
        // the archive is relocatable, so the prefix isn't known at build time.
        //
        // Compared by canonical path rather than literal `PathBuf` equality:
        // the candidate is built by appending a `..` component instead of
        // popping the parent, so it comes back as
        // `<prefix>/bin/../share/lattice/plugins`. That names the right
        // directory and `.exists()` selects it correctly — only the spelling
        // differs, and nothing compares this path. The sibling dev-fallback
        // test compares the same way for the same reason. Both sides are
        // canonicalized because macOS resolves the tempdir's `/var` to
        // `/private/var`.
        let tmp = tempfile::tempdir().unwrap();
        let prefix = tmp.path();
        let plugins = prefix.join("share").join("lattice").join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();

        let got = core_plugins_dir_from(None, None, &[&prefix.join("bin").join("lattice")])
            .expect("an extracted archive must find the plugins shipped beside its binary");

        assert_eq!(
            got.canonicalize().unwrap(),
            plugins.canonicalize().unwrap(),
            "resolved {} but expected the plugins dir beside the binary",
            got.display()
        );
    }

    #[test]
    fn none_when_no_candidate_exists() {
        assert_eq!(
            core_plugins_dir_from(None, None, &[Path::new("/nowhere/bin/lattice")]),
            None
        );
        // No exe at all (current_exe failed) + no prefix → None.
        assert_eq!(core_plugins_dir_from(None, None, &[]), None);
    }

    #[test]
    fn a_symlinked_binary_finds_the_plugins_beside_the_link() {
        // Issue #3: `~/.local/bin/lattice -> ~/.cargo/bin/lattice`, plugins in
        // `~/.local/share/lattice/plugins`. The resolved path's prefix has no
        // `share/`; the invoked path's does, and is asked first.
        let tmp = tempfile::tempdir().unwrap();
        let link_prefix = tmp.path().join("local");
        let plugins = link_prefix.join("share").join("lattice").join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let invoked = link_prefix.join("bin").join("lattice");
        let resolved = tmp.path().join("cargo").join("bin").join("lattice");
        // `<bin>/../share` only exists if `<bin>` does.
        std::fs::create_dir_all(invoked.parent().unwrap()).unwrap();
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();

        // The resolved path alone is the reported bug: nothing found.
        assert_eq!(core_plugins_dir_from(None, None, &[&resolved]), None);

        let got = core_plugins_dir_from(None, None, &[&invoked, &resolved])
            .expect("the plugins beside the symlink must be found");
        assert_eq!(got.canonicalize().unwrap(), plugins.canonicalize().unwrap());
    }

    #[test]
    fn the_invoked_prefix_wins_when_both_prefixes_carry_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        let share = |prefix: &str| {
            let dir = tmp.path().join(prefix).join("share/lattice/plugins");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::create_dir_all(tmp.path().join(prefix).join("bin")).unwrap();
            dir
        };
        let (beside_link, _beside_target) = (share("local"), share("cargo"));
        let got = core_plugins_dir_from(
            None,
            None,
            &[
                &tmp.path().join("local/bin/lattice"),
                &tmp.path().join("cargo/bin/lattice"),
            ],
        )
        .unwrap();
        assert_eq!(
            got.canonicalize().unwrap(),
            beside_link.canonicalize().unwrap()
        );
    }

    #[test]
    fn the_invoked_path_is_argv0_made_absolute() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("lattice");
        std::fs::write(&exe, b"").unwrap();
        let path_env = std::env::join_paths([tmp.path().join("empty"), bin.clone()]).unwrap();

        // Absolute: as written.
        assert_eq!(invoked_exe_from(&exe, None, None), Some(exe.clone()));
        // With a directory part: against the working directory.
        assert_eq!(
            invoked_exe_from(Path::new("bin/lattice"), None, Some(tmp.path())),
            Some(tmp.path().join("bin/lattice")),
        );
        // A bare name: found on `$PATH`, as the shell found it.
        assert_eq!(
            invoked_exe_from(Path::new("lattice"), Some(path_env.clone()), None),
            Some(exe),
        );
    }

    #[test]
    fn an_argv0_that_names_no_file_is_ignored() {
        // `argv[0]` is the parent's to set; an `exec -a` or a login shell's
        // `-lattice` must fall through to the resolved path, not be believed.
        let tmp = tempfile::tempdir().unwrap();
        let path_env = std::env::join_paths([tmp.path()]).unwrap();
        assert_eq!(
            invoked_exe_from(Path::new("lattice"), Some(path_env), None),
            None
        );
        assert_eq!(
            invoked_exe_from(&tmp.path().join("gone/lattice"), None, None),
            None
        );
        assert_eq!(
            invoked_exe_from(Path::new("bin/lattice"), None, Some(tmp.path())),
            None
        );
        // No `$PATH`, no working directory: nothing to resolve against.
        assert_eq!(invoked_exe_from(Path::new("lattice"), None, None), None);
        assert_eq!(invoked_exe_from(Path::new("bin/lattice"), None, None), None);
    }

    // The dev candidate path contains `..` segments; compare by canonicalized
    // existence rather than literal equality.
    fn got_matches(exe: &Path, expected_existing: &Path) -> bool {
        let got = core_plugins_dir_from(None, None, &[exe]).unwrap();
        got.canonicalize().ok() == expected_existing.canonicalize().ok()
    }
}
