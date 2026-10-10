//! LH.0.4 — registering a language server at runtime, as a service.
//!
//! Design: `docs/dev/architecture/lighthouse.md` §3.4.
//!
//! The LSP subsystem decides which server handles a buffer from a list of
//! configs fixed at boot. A server installed *while the editor is running* —
//! which is what a server manager does — has to join that list afterwards, and
//! the thing that installed it is a plugin, which cannot name an LSP type.
//!
//! So the seam is a trait here, in the crate both sides already depend on:
//! the LSP subsystem implements it over its supervisor and registers the
//! handle as a service; the plugin host looks the service up and never learns
//! what is behind it. Neither crate depends on the other.
//!
//! [`LanguageServerSpec`] is plain data for the same reason — strings and
//! paths, with initialization options as a JSON *string* so this crate stays
//! free of a JSON dependency it has no other use for.

use std::path::PathBuf;
use std::sync::Arc;

/// One language server, as something outside the LSP subsystem describes it.
///
/// Mirrors the LSP subsystem's own config field for field; kept separate so a
/// caller can build one without depending on that crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageServerSpec {
    /// Stable identifier — by convention the language id (`"rust"`). A
    /// registered spec **shadows** every boot-time config with the same id for
    /// as long as it is registered.
    pub id: String,
    /// The program to run. Relative paths resolve via `PATH`.
    pub command: PathBuf,
    /// Arguments, passed verbatim.
    pub args: Vec<String>,
    /// Extra environment variables for the server process.
    pub env: Vec<(String, String)>,
    /// Workspace-root markers (`Cargo.toml`, `.git`), searched upwards from
    /// the buffer's path.
    pub root_markers: Vec<String>,
    /// Globs for the files this server handles (`*.rs`).
    pub file_patterns: Vec<String>,
    /// The LSP `languageId` sent on `didOpen`.
    pub language_id: String,
    /// `initializationOptions`, as JSON text. `None` sends none.
    pub initialization_options: Option<String>,
}

/// Adds and removes language servers while the editor runs.
///
/// Both calls are synchronous and must not block: they are reached from a
/// plugin's host call, which can be on the editor's dispatch thread. An
/// implementation hands the change to whatever owns the config list and
/// returns.
pub trait LanguageServerRegistrar: Send + Sync {
    /// Register `spec`. Returns a token that [`unregister`](Self::unregister)
    /// takes.
    ///
    /// The server is not started here. It is spawned the next time a buffer
    /// its patterns match is opened; a server already running for those
    /// buffers keeps running until it is restarted.
    ///
    /// `Err` when the spec cannot be used (malformed initialization options)
    /// or the subsystem is gone — in words a user can act on.
    fn register(&self, spec: LanguageServerSpec) -> Result<u64, String>;

    /// Remove the registration `token` names. Whatever boot-time config it
    /// shadowed applies again. An unknown token is nothing.
    fn unregister(&self, token: u64);
}

/// The registrar as a service — registered and looked up under this exact
/// type (the `ServiceRegistry` keys by `TypeId`, so the alias is the contract).
pub type LanguageServerRegistrarHandle = Arc<dyn LanguageServerRegistrar>;
