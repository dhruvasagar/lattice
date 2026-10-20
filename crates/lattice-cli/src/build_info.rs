//! The `:version` page, assembled here because this is the one place that
//! knows all of it: the build stamp is compiled into this binary (`build.rs`),
//! the renderer is chosen in `main`, and the paths come from crates the mode
//! that shows the page does not depend on.
//!
//! [`report`] is handed to `version-mode` before the editor boots, and printed
//! by `lattice --version --verbose` — the same `VersionReport::render` either
//! way, so the terminal and the buffer cannot disagree.

use std::path::PathBuf;

use lattice_mode::{VersionReport, VersionSection};

/// The editor's version, as plain `--version` prints it.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The page. `renderer` says what this process is running in, or would run
/// in — `--version --verbose` describes a launch that is not happening.
pub fn report(renderer: &str) -> VersionReport {
    VersionReport {
        version: VERSION.to_string(),
        sections: vec![build(), runtime(renderer), plugin_api(), paths()],
    }
}

fn row(label: &str, value: impl Into<String>) -> (String, String) {
    (label.to_string(), value.into())
}

fn build() -> VersionSection {
    let sha = env!("LATTICE_BUILD_GIT_SHA");
    let dirty = env!("LATTICE_BUILD_GIT_DIRTY");
    let date = env!("LATTICE_BUILD_GIT_COMMIT_DATE");
    let commit = if sha == "unknown" {
        "unknown (built outside a git checkout, with no commit supplied)".to_string()
    } else {
        format!("{sha} ({dirty}), {date}")
    };
    let features = match env!("LATTICE_BUILD_FEATURES") {
        "gui" => "window + terminal",
        _ => "terminal only",
    };
    VersionSection {
        heading: "Build".into(),
        rows: vec![
            row("Version", VERSION),
            row("Commit", commit),
            row("Profile", env!("LATTICE_BUILD_PROFILE")),
            row("Renderers", features),
            row("Rust", env!("LATTICE_BUILD_RUSTC")),
            row("Target", env!("LATTICE_BUILD_TARGET")),
        ],
    }
}

fn runtime(renderer: &str) -> VersionSection {
    let mut rows = vec![
        row("Renderer", renderer),
        row(
            "OS",
            format!("{} {}", std::env::consts::OS, std::env::consts::ARCH),
        ),
    ];
    // The terminal's own account of itself: what colour depth and key
    // reporting the TUI can expect. Absent variables are left out rather than
    // shown empty.
    for var in ["TERM", "COLORTERM", "TERM_PROGRAM"] {
        if let Ok(value) = std::env::var(var)
            && !value.is_empty()
        {
            rows.push(row(var, value));
        }
    }
    VersionSection {
        heading: "Runtime".into(),
        rows,
    }
}

fn plugin_api() -> VersionSection {
    let wit = env!("LATTICE_BUILD_LATTICE_WIT");
    // `lattice-wit`'s major.minor IS the WIT package's (releasing.md), so the
    // crate version names the generation a plugin must be built for.
    let generation = match wit.split('.').collect::<Vec<_>>().as_slice() {
        [major, minor, ..] => {
            format!("lattice:plugin-host@{major}.{minor}.x (lattice-wit {wit})")
        }
        _ => "unknown".to_string(),
    };
    VersionSection {
        heading: "Plugin API".into(),
        rows: vec![
            row("API", generation),
            row("Fingerprint", lattice_wit::ABI_FINGERPRINT),
            row("wasmtime", env!("LATTICE_BUILD_WASMTIME")),
        ],
    }
}

fn paths() -> VersionSection {
    let shown = |p: Option<PathBuf>| match p {
        Some(p) => lattice_core::home::contract_tilde(&p),
        None => "not found".to_string(),
    };
    let config_dir = lattice_config::config_home().map(|c| c.join("lattice"));
    VersionSection {
        heading: "Paths".into(),
        rows: vec![
            row("Config", shown(config_dir)),
            row(
                "Options file",
                shown(lattice_config::default_user_config_path()),
            ),
            row("init.rs", shown(lattice_plugin_loader::default_init_dir())),
            row(
                "Plugins",
                shown(lattice_plugin_loader::default_plugins_dir()),
            ),
            row(
                "Core plugins",
                shown(lattice_plugin_loader::default_core_plugins_dir()),
            ),
            row("Cache", shown(lattice_config::cache_home())),
            // There is no log file: say where logs do go, so nobody hunts for one.
            row(
                "Logs",
                "the `*messages*` buffer (`:messages`); stderr with `--stderr-logs`",
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_report_has_every_section_and_names_its_renderer() {
        let page = report("terminal (TUI)").render();
        for heading in ["## Build", "## Runtime", "## Plugin API", "## Paths"] {
            assert!(page.contains(heading), "missing {heading} in:\n{page}");
        }
        assert!(page.starts_with(&format!("# Lattice {VERSION}\n")));
        assert!(page.contains("- **Renderer:** terminal (TUI)\n"));
        assert!(page.contains(&format!(
            "- **Fingerprint:** {}\n",
            lattice_wit::ABI_FINGERPRINT
        )));
    }

    #[test]
    fn the_build_stamp_is_filled_in_a_checkout() {
        // This test runs from the workspace, which IS a checkout with a
        // lockfile, so none of these may have fallen back to `unknown`.
        for (name, value) in [
            ("commit", env!("LATTICE_BUILD_GIT_SHA")),
            ("commit date", env!("LATTICE_BUILD_GIT_COMMIT_DATE")),
            ("rustc", env!("LATTICE_BUILD_RUSTC")),
            ("target", env!("LATTICE_BUILD_TARGET")),
            ("lattice-wit", env!("LATTICE_BUILD_LATTICE_WIT")),
            ("wasmtime", env!("LATTICE_BUILD_WASMTIME")),
        ] {
            assert_ne!(value, "unknown", "{name} was not stamped");
        }
    }
}
