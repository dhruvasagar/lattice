//! Per-language-server configuration.
//!
//! The editor maintains a registry of [`ServerConfig`] entries
//! keyed by language identifier. When a buffer of that language
//! is opened, the supervisor checks for an existing actor at the
//! resolved workspace root; if absent, it constructs one from the
//! config.
//!
//! v1 ships hardcoded defaults for rust-analyzer, pyright, gopls,
//! tsserver, clangd, and lua-language-server; users override via
//! a future `lsp.toml` (queued behind §5.12). For now,
//! [`builtin_servers`] returns the curated list and the editor
//! reads from it directly.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;

use serde_json::Value;

/// Configuration for one language server. Captured at registry
/// time; immutable after spawn.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Stable identifier used in logs / telemetry / reference
    /// counting. Convention: language id (e.g. `"rust"`,
    /// `"python"`); a workspace can host multiple actors with
    /// the same id if they target different roots.
    pub id: String,
    /// Path to the server binary. Relative paths resolve via
    /// `PATH`. Tilde expansion and env-var substitution are NOT
    /// done here -- the registry layer applies them when
    /// loading from `lsp.toml` so the runtime sees a literal
    /// path.
    pub binary: PathBuf,
    /// Arguments passed verbatim. rust-analyzer takes none;
    /// pyright wants `--stdio`; gopls is configured via init
    /// options.
    pub args: Vec<OsString>,
    /// Extra environment variables for the spawned process.
    /// Useful for `RUST_LOG=info` or `PYTHONPATH=...`.
    pub env: HashMap<String, String>,
    /// Workspace-root markers. The supervisor walks up from the
    /// opened buffer's path looking for any of these; the first
    /// hit becomes the server's `rootUri`. Empty list falls back
    /// to the buffer's directory.
    pub root_markers: Vec<String>,
    /// Server-specific initialization options. Sent verbatim as
    /// `initialize.params.initializationOptions`. rust-analyzer
    /// uses this to toggle `cargo check` on save, pyright for
    /// venv discovery, etc.
    pub initialization_options: Option<Value>,
    /// File-pattern globs this server handles. The supervisor
    /// matches a buffer's path against these to decide which
    /// server (if any) to attach. Multiple patterns are OR'd.
    /// Example: `["*.rs", "*.toml"]` for rust-analyzer to also
    /// see Cargo manifests.
    pub file_patterns: Vec<String>,
    /// LSP language identifier sent in `didOpen.textDocument.languageId`.
    /// Distinct from `id` because some servers (denols, vtsls)
    /// handle multiple language ids; the registry can have one
    /// `ServerConfig` per pattern with distinct `language_id`
    /// values and the same `id` (so the actor is shared).
    pub language_id: String,
}

impl ServerConfig {
    /// Build a minimal config -- intended for tests and as a
    /// starting point for the curated registry below.
    pub fn new(
        id: impl Into<String>,
        binary: impl Into<PathBuf>,
        language_id: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            binary: binary.into(),
            args: Vec::new(),
            env: HashMap::new(),
            root_markers: Vec::new(),
            initialization_options: None,
            file_patterns: Vec::new(),
            language_id: language_id.into(),
        }
    }

    /// Builder helper -- one-line `.with_args(["--stdio"])`.
    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Builder helper for env vars.
    pub fn with_env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Builder helper for root markers (e.g. `["Cargo.toml", ".git"]`).
    pub fn with_root_markers<I, S>(mut self, markers: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.root_markers = markers.into_iter().map(Into::into).collect();
        self
    }

    /// Builder helper for file-pattern globs.
    pub fn with_file_patterns<I, S>(mut self, patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.file_patterns = patterns.into_iter().map(Into::into).collect();
        self
    }

    /// Builder helper for `initializationOptions`.
    pub fn with_initialization_options(mut self, opts: Value) -> Self {
        self.initialization_options = Some(opts);
        self
    }
}

/// Curated registry of well-known language servers. The editor
/// loads these as defaults; user overrides (from `lsp.toml` in a
/// future iteration) merge on top.
///
/// Each entry uses the canonical binary name -- on PATH for any
/// developer who installed the server through their language's
/// usual channel. Servers that need extra args (pyright wants
/// `--stdio`) get them here.
pub fn builtin_servers() -> Vec<ServerConfig> {
    vec![
        // rust-analyzer is the canonical Rust LSP. Speaks the
        // full LSP 3.17 spec; honours initializationOptions for
        // check-on-save toggles. Workspace root is the Cargo
        // workspace top (Cargo.toml with [workspace] preferred,
        // any Cargo.toml as fallback).
        ServerConfig::new("rust", "rust-analyzer", "rust")
            .with_root_markers(["Cargo.toml", "rust-project.json", ".git"])
            .with_file_patterns(["*.rs"]),
        // pyright: Microsoft's Python type checker; --stdio is
        // mandatory for LSP mode (default is HTTP).
        ServerConfig::new("python", "pyright-langserver", "python")
            .with_args(["--stdio"])
            .with_root_markers([
                "pyproject.toml",
                "setup.py",
                "setup.cfg",
                "requirements.txt",
                ".git",
            ])
            .with_file_patterns(["*.py", "*.pyi"]),
        // gopls: official Go server. No args; configured via
        // initializationOptions (we leave it on defaults).
        ServerConfig::new("go", "gopls", "go")
            .with_root_markers(["go.mod", "go.work", ".git"])
            .with_file_patterns(["*.go"]),
        // typescript-language-server: the community npm-installed
        // server. `--stdio` for the LSP-over-stdio mode.
        ServerConfig::new("typescript", "typescript-language-server", "typescript")
            .with_args(["--stdio"])
            .with_root_markers(["tsconfig.json", "jsconfig.json", "package.json", ".git"])
            .with_file_patterns(["*.ts", "*.tsx", "*.js", "*.jsx", "*.mts", "*.cts"]),
        // clangd: the upstream LLVM C/C++ server. Default args
        // are fine; serious users supply `compile_commands.json`
        // at the root.
        ServerConfig::new("c-cpp", "clangd", "cpp")
            .with_root_markers([
                "compile_commands.json",
                "compile_flags.txt",
                "CMakeLists.txt",
                ".git",
            ])
            .with_file_patterns(["*.c", "*.h", "*.cc", "*.cpp", "*.cxx", "*.hpp", "*.hh"]),
        // lua-language-server: sumneko's Lua server.
        ServerConfig::new("lua", "lua-language-server", "lua")
            .with_root_markers([".luarc.json", ".luarc.jsonc", "stylua.toml", ".git"])
            .with_file_patterns(["*.lua"]),
    ]
}

/// Walk up from `start_dir` looking for any of `markers`. Returns
/// the resolved workspace root, applying language-specific
/// "outermost wins" semantics where appropriate. Falls back to
/// `start_dir` when no marker matches -- the LSP spec allows
/// passing the buffer's directory as a degenerate workspace.
///
/// **Cargo-workspace awareness.** Rust's Cargo allows nested
/// `Cargo.toml` files (member crate's `[package]`-only Cargo.toml
/// inside the workspace's `[workspace]` Cargo.toml). rust-analyzer
/// must be anchored at the *workspace* root for cross-crate
/// goto-definition + external-dependency indexing to work. This
/// resolver therefore walks the entire ancestor chain when
/// `Cargo.toml` is among the markers and prefers the *outermost*
/// `Cargo.toml` that declares `[workspace]`. For all other markers
/// (and for standalone-crate Rust projects with no enclosing
/// workspace) the *nearest* match still wins -- the historical
/// behaviour and what every other language wants.
pub fn resolve_workspace_root(start_dir: &std::path::Path, markers: &[String]) -> PathBuf {
    let mut nearest_marker_dir: Option<PathBuf> = None;
    let mut outermost_workspace_dir: Option<PathBuf> = None;
    let cargo_marker_present = markers.iter().any(|m| m == "Cargo.toml");

    // Absolutize `start_dir` before walking up. With a relative
    // `start_dir` (e.g. `crates/foo/src/`) `Path::parent()` stops
    // at an empty component long before reaching any workspace
    // ancestor that lives above the cwd -- the resolver would
    // then either fall back to the buffer's own directory or
    // (worse, post-fix) return the nearest Cargo.toml without
    // the outer `[workspace]` ever being seen. Canonicalize when
    // the path exists; absolutize via `current_dir().join` when
    // it doesn't (e.g. `:e new-file.txt` against an unsaved
    // path).
    let absolute_start: PathBuf = std::fs::canonicalize(start_dir)
        .or_else(|_| std::env::current_dir().map(|cwd| cwd.join(start_dir)))
        .unwrap_or_else(|_| start_dir.to_path_buf());

    let mut cursor = Some(absolute_start.as_path());
    while let Some(dir) = cursor {
        for marker in markers {
            let marker_path = dir.join(marker);
            if marker_path.exists() {
                if nearest_marker_dir.is_none() {
                    nearest_marker_dir = Some(dir.to_path_buf());
                }
                // Cargo workspace upgrade path: peek inside any
                // Cargo.toml we encounter; the outermost one with
                // `[workspace]` is the true root. Cheap line scan
                // -- no TOML parser dep needed.
                if cargo_marker_present
                    && marker == "Cargo.toml"
                    && cargo_toml_declares_workspace(&marker_path)
                {
                    outermost_workspace_dir = Some(dir.to_path_buf());
                }
            }
        }
        cursor = dir.parent();
    }
    outermost_workspace_dir
        .or(nearest_marker_dir)
        .unwrap_or(absolute_start)
}

/// Does the given `Cargo.toml` declare a `[workspace]` section?
/// Returns false on any read / parse failure -- the caller treats
/// the file as a non-workspace `Cargo.toml` and the resolver falls
/// back to the nearest-marker path.
///
/// We avoid pulling in a TOML parser dep for this single check;
/// a line-by-line scan for `[workspace]` (or `[workspace.something]`)
/// is precise enough. False positives require a `[workspace]`
/// substring at the start of a non-comment line that *also*
/// satisfies TOML's section-header grammar -- vanishingly rare in
/// real Cargo manifests.
fn cargo_toml_declares_workspace(path: &std::path::Path) -> bool {
    let Ok(content) = std::fs::read_to_string(path) else {
        return false;
    };
    content.lines().any(|line| {
        let trimmed = line.trim();
        trimmed == "[workspace]" || trimmed.starts_with("[workspace.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile_lite as tempdir; // avoid extra dep; see helper

    /// Tiny in-test temp dir helper -- avoids pulling in `tempfile`.
    /// Creates `target/lsp-tests/<name>` and returns the path; the
    /// caller deletes it. We don't need atomicity here; tests are
    /// serialised within a single Cargo invocation.
    mod tempfile_lite {
        use std::path::PathBuf;
        pub fn new_dir(name: &str) -> PathBuf {
            let mut p = std::env::temp_dir();
            p.push(format!("lattice-lsp-test-{}-{}", name, std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            // Canonicalize the created dir so equality checks against the
            // resolver's output are stable on hosts that put the temp dir
            // behind a symlink (macOS: `/var` -> `/private/var`).
            // `resolve_workspace_root` canonicalizes `start_dir`, so its
            // result is already the canonical form; the expected value must
            // match.
            std::fs::canonicalize(&p).unwrap()
        }
    }

    #[test]
    fn builtin_servers_have_distinct_ids() {
        let servers = builtin_servers();
        let mut ids: Vec<&str> = servers.iter().map(|c| c.id.as_str()).collect();
        ids.sort();
        let n_before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n_before, "duplicate server id in builtin set");
    }

    #[test]
    fn builtin_servers_have_file_patterns() {
        for cfg in builtin_servers() {
            assert!(
                !cfg.file_patterns.is_empty(),
                "{} has no file patterns; supervisor cannot match it to a buffer",
                cfg.id
            );
        }
    }

    #[test]
    fn builtin_servers_have_root_markers() {
        // Every server config must list at least one root marker
        // beyond `.git`; otherwise we'd anchor every workspace at
        // the user's home dir.
        for cfg in builtin_servers() {
            let non_git: Vec<&String> = cfg
                .root_markers
                .iter()
                .filter(|m| m.as_str() != ".git")
                .collect();
            assert!(
                !non_git.is_empty(),
                "{} only has .git as a root marker -- needs a language-specific anchor",
                cfg.id
            );
        }
    }

    #[test]
    fn resolve_workspace_root_finds_marker_in_parent() {
        let dir = tempdir::new_dir("ws-root-parent");
        let inner = dir.join("a/b/c");
        fs::create_dir_all(&inner).unwrap();
        fs::write(dir.join("Cargo.toml"), "[package]\nname=\"x\"").unwrap();

        let resolved = resolve_workspace_root(&inner, &["Cargo.toml".into()]);
        assert_eq!(resolved, dir);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_workspace_root_falls_back_to_start_dir() {
        let dir = tempdir::new_dir("ws-root-no-marker");
        fs::create_dir_all(&dir).unwrap();
        let resolved = resolve_workspace_root(&dir, &["never-exists.toml".into()]);
        assert_eq!(resolved, dir);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_workspace_root_prefers_outermost_cargo_workspace() {
        // Layout:
        //   tmp/Cargo.toml         -- [workspace] members = [...]
        //   tmp/crates/foo/Cargo.toml  -- [package]
        //   tmp/crates/foo/src/lib.rs
        // Walking up from `src/`, we should land on `tmp/`, not
        // `tmp/crates/foo/`. This is the bug that broke cross-
        // crate goto-definition + external-dep hover for nested
        // member crates.
        let dir = tempdir::new_dir("ws-root-cargo-outer");
        let inner = dir.join("crates/foo/src");
        fs::create_dir_all(&inner).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        fs::write(
            dir.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let resolved = resolve_workspace_root(&inner, &["Cargo.toml".into()]);
        assert_eq!(
            resolved, dir,
            "outermost Cargo.toml with [workspace] should win"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_workspace_root_standalone_crate_uses_nearest() {
        // No enclosing workspace: nearest Cargo.toml wins (the
        // crate's own).
        let dir = tempdir::new_dir("ws-root-standalone");
        let inner = dir.join("src");
        fs::create_dir_all(&inner).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"solo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let resolved = resolve_workspace_root(&inner, &["Cargo.toml".into()]);
        assert_eq!(resolved, dir);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_workspace_root_two_unrelated_cargo_dirs_keeps_nearest() {
        // Two nested Cargo.toml files, neither declares
        // [workspace]. This is unusual (broken-by-cargo) but the
        // fallback must still pick the nearest -- the inner crate
        // is the right anchor for tooling.
        let dir = tempdir::new_dir("ws-root-unrelated-nested");
        let inner_crate = dir.join("inner");
        let inner_src = inner_crate.join("src");
        fs::create_dir_all(&inner_src).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"outer\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(
            inner_crate.join("Cargo.toml"),
            "[package]\nname = \"inner\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let resolved = resolve_workspace_root(&inner_src, &["Cargo.toml".into()]);
        assert_eq!(
            resolved, inner_crate,
            "nearest Cargo.toml should win when no workspace is declared"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_workspace_root_workspace_table_with_subkey_detected() {
        // `[workspace.dependencies]` is a sub-table; the parser
        // should still recognise it as workspace-bearing.
        let dir = tempdir::new_dir("ws-root-workspace-subkey");
        let inner = dir.join("crates/foo/src");
        fs::create_dir_all(&inner).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[workspace.dependencies]\nserde = \"1\"\n",
        )
        .unwrap();
        fs::write(
            dir.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let resolved = resolve_workspace_root(&inner, &["Cargo.toml".into()]);
        assert_eq!(resolved, dir);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_workspace_root_resolves_relative_start_dir() {
        // Regression for the post-resolver-fix break: when
        // `Document::open` keeps the path relative (e.g.
        // `lattice src/lib.rs` from inside a member crate), the
        // resolver's start_dir is relative. `Path::parent()`
        // stops at an empty component long before the outer
        // workspace's Cargo.toml is reached, so a member crate
        // anchored at the inner `crates/foo/Cargo.toml` would
        // (post-fix) have no Cargo.toml found anywhere, and
        // `:lsp-status` reported "no servers attached". Fix:
        // absolutize start_dir via canonicalize / cwd.join
        // before walking.
        let dir = tempdir::new_dir("ws-root-relative-start");
        let inner = dir.join("crates/foo/src");
        fs::create_dir_all(&inner).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\n",
        )
        .unwrap();
        fs::write(
            dir.join("crates/foo/Cargo.toml"),
            "[package]\nname = \"foo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        // cd into the workspace root so a relative start_dir
        // matches the path-shape the App passes through.
        let saved_cwd = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let relative = std::path::Path::new("crates/foo/src");
        let resolved = resolve_workspace_root(relative, &["Cargo.toml".into()]);
        std::env::set_current_dir(saved_cwd).unwrap();

        // canonicalize() may resolve symlinks; compare against
        // the canonical workspace dir for stability across hosts
        // that put `/tmp` behind a symlink (macOS does).
        let expected = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(
            resolved, expected,
            "relative start_dir must still find the outer workspace"
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_workspace_root_non_cargo_marker_keeps_nearest() {
        // Markers other than Cargo.toml don't get the
        // outermost-wins semantics -- a `package.json` deep in a
        // monorepo should anchor at the deepest match (the
        // sub-package), not at the monorepo root.
        let dir = tempdir::new_dir("ws-root-non-cargo");
        let inner = dir.join("apps/web/src");
        fs::create_dir_all(&inner).unwrap();
        fs::write(dir.join("package.json"), "{}").unwrap();
        fs::write(dir.join("apps/web/package.json"), "{}").unwrap();
        let resolved = resolve_workspace_root(&inner, &["package.json".into()]);
        assert_eq!(
            resolved,
            dir.join("apps/web"),
            "nearest non-Cargo marker wins"
        );
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn builder_chains() {
        let cfg = ServerConfig::new("rust", "rust-analyzer", "rust")
            .with_args(["--no-cache"])
            .with_env("RUST_LOG", "info")
            .with_root_markers(["Cargo.toml"])
            .with_file_patterns(["*.rs"]);
        assert_eq!(cfg.id, "rust");
        assert_eq!(cfg.args, vec![OsString::from("--no-cache")]);
        assert_eq!(cfg.env.get("RUST_LOG").unwrap(), "info");
    }
}
