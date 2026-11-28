//! [`ConfigFilesReloaded`]: the config files were read again.
//!
//! The registry holds the files' options, and the host re-derives its own
//! caches from them. Several subsystems instead own a whole table of the file
//! — `[llm]`, say — which they read themselves at boot. This event tells each of
//! those owners to read its table again, so no subsystem has to know about
//! another's table, and the one running the reload does not have to know who
//! owns what.

use std::path::PathBuf;

/// Published by the host once a reload has re-applied the config files to the
/// registry and to its own caches, and before `init.rs` is reloaded.
#[derive(Debug, Clone, Default)]
pub struct ConfigFilesReloaded {
    /// The project root whose `.lattice/` files were read, if any.
    pub workspace_root: Option<PathBuf>,
}

lattice_protocol::register_event!(
    ConfigFilesReloaded,
    "config.files-reloaded",
    "The config files were read again (`:reload-config`); each owner of a table in them \
     re-reads its own.",
    "lattice-config",
);
