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
/// the first ancestor that contains one. Falls back to `start_dir`
/// when no marker matches -- the LSP spec allows passing the
/// buffer's directory as a degenerate workspace.
pub fn resolve_workspace_root(
    start_dir: &std::path::Path,
    markers: &[String],
) -> PathBuf {
    let mut cursor = Some(start_dir);
    while let Some(dir) = cursor {
        for marker in markers {
            if dir.join(marker).exists() {
                return dir.to_path_buf();
            }
        }
        cursor = dir.parent();
    }
    start_dir.to_path_buf()
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
            p
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
            let non_git: Vec<&String> =
                cfg.root_markers.iter().filter(|m| m.as_str() != ".git").collect();
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
