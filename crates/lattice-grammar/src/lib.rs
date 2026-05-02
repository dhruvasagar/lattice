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
pub mod cancel;
pub mod command;
pub mod dispatcher;
pub mod effect;
pub mod error;
pub mod ex_commands;
pub mod introspect;
pub mod modal;
pub mod range;
pub mod register;
pub mod registry;
pub mod source;
pub mod target;

pub use crate::args::{ArgDefault, ArgKind, ArgSpec, ArgValue, Args};
pub use crate::cancel::CancellationToken;
pub use crate::introspect::{
    HelpSection, Introspectable, RenderedAnchor, RenderedIntrospection, SourceEntry, SourceLabel,
    render_introspection, render_introspection_lines,
};
pub use crate::source::{SourceKind, SourceLayer, SourceLocation};
pub use crate::command::{CommandInvocation, CommandKind, CommandSpec, Count, LatencyClass};
pub use crate::dispatcher::execute;
pub use crate::effect::{EchoLevel, Effect, SubstituteScope, YankKind};
pub use crate::error::{CommandError, GrammarResult};
pub use crate::modal::{ModalState, SearchDirection, VisualKind};
pub use crate::range::{Range, RangeBound};
pub use crate::register::Register;
pub use crate::registry::{
    CommandRegistration, CommandRegistry, ExCommandContext, ExCommandSpec, MotionSpec,
    OperatorContext, OperatorSpec, SurfaceForm, TextObjectSpec,
};
pub use crate::registry::{ExCommandId, MotionId, OperatorId, TextObjectId};
pub use crate::target::Target;

/// Re-export the protocol's CommandId so callers don't need a second import.
pub use lattice_protocol::ids::CommandId;
