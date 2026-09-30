//! [`CommandError`]: why a grammar invocation produced no effect.
//!
//! Every evaluator (motion, operator, text object, ex-command, action) and
//! the dispatcher itself return [`GrammarResult`]. The contract shared by
//! all variants: **an `Err` commits nothing.** The dispatcher returns before
//! any [`crate::Effect`] reaches the host, so the document, cursor, undo
//! stack and registers are exactly as they were when the keystroke arrived.
//! Variants differ only in what the host does *besides* dropping the
//! invocation -- most are logged, [`CommandError::User`] is echoed to the
//! user, [`CommandError::MotionFailed`] is vim's silent beep.

use thiserror::Error;

use lattice_core::CoreError;
use lattice_protocol::ProtocolError;

/// Why a grammar invocation failed. See the [module docs](self) for the
/// no-effect-on-error contract every variant shares.
#[derive(Debug, Error)]
pub enum CommandError {
    /// The invocation named a [`crate::CommandId`] (or a motion / operator /
    /// text-object id) that is not in the [`crate::CommandRegistry`] -- e.g.
    /// a plugin contribution that has since been unloaded, or a stale id
    /// carried in a recorded macro.
    #[error("unknown command id")]
    UnknownCommand,

    /// The id resolved, but to a registration of a different kind than the
    /// call site needs (an operator id passed where a motion was expected).
    /// A programming error in the caller, not a user error.
    #[error("command kind mismatch: expected {expected}, got {actual}")]
    KindMismatch {
        /// The kind the call site required, as its registry label
        /// (`"motion"`, `"operator"`, `"text-object"`, `"ex-command"`,
        /// `"action"`).
        expected: &'static str,
        /// The kind the id actually names, same label vocabulary.
        actual: &'static str,
    },

    /// An operator invocation carried neither a [`crate::Target`] nor a
    /// [`crate::Range`], so there is nothing to operate on. The keystroke
    /// parser never builds such an invocation; this guards programmatic
    /// callers (plugins, replayed macros).
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

    /// The evaluator received [`crate::Args`] of the wrong shape, or args
    /// that do not fit the document (the common case is a position computed
    /// past the end of the buffer). The static string names what was wrong;
    /// it is for logs, not the user.
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

    /// A buffer-model failure from [`lattice_core`] (I/O, nothing to
    /// undo/redo, a core-level cancellation) surfaced through `?`.
    #[error(transparent)]
    Core(#[from] CoreError),

    /// A protocol-layer failure from [`lattice_protocol`] (unknown
    /// document, out-of-bounds position, stale version) surfaced through
    /// `?`.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
}

/// `Result` alias used by every evaluator and by the dispatcher.
pub type GrammarResult<T> = Result<T, CommandError>;
