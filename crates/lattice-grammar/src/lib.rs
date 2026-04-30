//! Vim modal editing engine and the unified command/grammar dispatch.
//!
//! Per DESIGN.md §5.2:
//!
//! - The vim grammar is the public command API.
//! - There is one `CommandRegistry` and one `execute(...)` dispatcher.
//! - Operators, motions, text objects, ex-commands, and plugin contributions
//!   are all peers of the dispatcher.
//! - Built-in motions / operators / text objects live here in native Rust
//!   (off the WASM hot path).
//!
//! Phase 1 scope (this revision):
//! - Modal state enum.
//! - Typed primitives: Operator / Motion / TextObject / Register / Count /
//!   Target / Range / Args / Effect.
//! - `CommandRegistry` + `execute()`.
//! - First built-in motion (`word_forward`) and operator (`delete`).
//!
//! Out of scope for this revision (later in Phase 1):
//! - The full vim catalog (most operators, motions, text objects).
//! - The keystroke-to-CommandInvocation parser (state machine).
//! - Macros, marks, dot-repeat, registers (the storage; the type exists).

pub mod args;
pub mod builtins;
pub mod command;
pub mod dispatcher;
pub mod effect;
pub mod error;
pub mod modal;
pub mod range;
pub mod register;
pub mod registry;
pub mod target;

pub use crate::args::Args;
pub use crate::command::{CommandInvocation, CommandKind, CommandSpec, Count};
pub use crate::dispatcher::execute;
pub use crate::effect::{Effect, YankKind};
pub use crate::error::{CommandError, GrammarResult};
pub use crate::modal::{ModalState, SearchDirection, VisualKind};
pub use crate::range::{Range, RangeBound};
pub use crate::register::Register;
pub use crate::registry::{
    CommandRegistration, CommandRegistry, MotionSpec, OperatorContext, OperatorSpec,
    TextObjectSpec,
};
pub use crate::target::Target;

/// Re-export the protocol's CommandId so callers don't need a second import.
pub use lattice_protocol::ids::CommandId;
