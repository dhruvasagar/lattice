use thiserror::Error;

/// Error type for all `lattice-vcs` operations.
#[derive(Error, Debug)]
pub enum VcsError {
    /// An error from the underlying `gix` library.
    /// Boxed to avoid large-`Err` on the hot path.
    #[error("git error: {0}")]
    Git(Box<gix::discover::Error>),

    /// A git object could not be resolved.
    #[error("object not found: {0}")]
    ObjectNotFound(String),

    /// A reference could not be resolved.
    #[error("reference not found: {0}")]
    ReferenceNotFound(String),

    /// Git command execution failed to start.
    #[error("git command failed to start ({context}): {source}")]
    GitCommand {
        context: String,
        #[source]
        source: std::io::Error,
    },

    /// Git command exited with non-zero status.
    #[error("git command failed: {stderr}")]
    GitCommandFailed { stderr: String },

    /// UTF-8 conversion failed for git output.
    #[error("utf-8 conversion failed ({context}): {source}")]
    Utf8 {
        context: String,
        #[source]
        source: std::string::FromUtf8Error,
    },

    /// Operation attempted on a bare repository where a working tree
    /// is required.
    #[error("operation requires a working tree, but this is a bare repository: {0}")]
    BareRepo(String),

    /// Index (staging) operation failed.
    #[error("index operation failed: {0}")]
    Index(String),

    /// Stash operation failed.
    #[error("stash operation failed: {0}")]
    Stash(String),

    /// Remote management operation failed.
    #[error("remote operation failed: {0}")]
    Remote(String),

    /// Bisect operation failed.
    #[error("bisect operation failed: {0}")]
    Bisect(String),

    /// Submodule operation failed.
    #[error("submodule operation failed: {0}")]
    Submodule(String),

    /// A working-tree status entry could not be parsed.
    #[error("status parse error: {0}")]
    StatusParse(String),
}

impl From<gix::discover::Error> for VcsError {
    fn from(e: gix::discover::Error) -> Self {
        VcsError::Git(Box::new(e))
    }
}

/// Convenience result type alias.
pub type Result<T, E = VcsError> = std::result::Result<T, E>;
