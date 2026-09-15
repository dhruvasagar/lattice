use thiserror::Error;

use lattice_core::CoreError;
use lattice_protocol::ProtocolError;

#[derive(Debug, Error)]
pub enum CommandError {
    #[error("unknown command id")]
    UnknownCommand,

    #[error("command kind mismatch: expected {expected}, got {actual}")]
    KindMismatch {
        expected: &'static str,
        actual: &'static str,
    },

    #[error("missing target for operator")]
    MissingTarget,

    /// VM.3L: a motion couldn't move (vim beeps): `j` on the last line, `k`
    /// on the first. The dispatcher commits no effect, so an operator it was
    /// feeding is cancelled — vim deletes nothing for `dj` on the last line,
    /// where returning the cursor would have deleted that line.
    #[error("motion failed")]
    MotionFailed,

    /// VM.3d-2: a command failed with a message the user should see — vim's
    /// `E486: Pattern not found`, `E35: no previous regular expression`. Like
    /// every error, no effect is committed (an operator fed by a failing `n`
    /// deletes nothing, as in vim); unlike the others, the host echoes it.
    #[error("{0}")]
    User(String),

    #[error("invalid args for command: {0}")]
    InvalidArgs(&'static str),

    /// An ex-command's `parse_args` callback rejected the input. Carries
    /// the human-readable reason; the parser front-end surfaces it through
    /// `ExCommandError::BadArgs`.
    #[error("invalid ex-command args: {0}")]
    BadArgs(String),

    /// A WASM-plugin grammar contribution failed at `apply` / `parse_args`
    /// (PH7.7c): a guest-returned `err`, a fuel/epoch trap (the Reflex-budget
    /// runaway guard), a boundary-conversion failure, or a dead plugin. The
    /// dispatcher treats it like any evaluator error — **no `Effect` is
    /// committed**, the contribution is a no-op (graceful degradation,
    /// plugin-host.md §8), and the reason is logged. Built-in grammar never
    /// produces this.
    #[error("plugin grammar failed: {0}")]
    Plugin(String),

    /// The evaluator observed a cancelled [`crate::CancellationToken`]
    /// and returned early. By DESIGN.md §5.2.5, no `Effect` is
    /// committed; the document is left at the version the keystroke
    /// arrived at, exactly as if the user had not pressed the key.
    #[error("operation cancelled")]
    Cancelled,

    #[error(transparent)]
    Core(#[from] CoreError),

    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

pub type GrammarResult<T> = Result<T, CommandError>;
