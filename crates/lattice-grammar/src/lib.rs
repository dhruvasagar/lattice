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

pub mod app_effect;
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

pub use crate::app_effect::{AppEffect, PaneDirection, ScrollPos, ViewportPos};
pub use crate::args::{ArgDefault, ArgKind, ArgSpec, ArgValue, Args};
pub use crate::cancel::{CancellationToken, CheckCancelled};
pub use crate::command::{CommandInvocation, CommandKind, CommandSpec, Count, LatencyClass};
pub use crate::dispatcher::{execute, execute_motion_only, execute_with_env};
pub use crate::effect::{EchoLevel, Effect, QuitScope, SubstituteScope, Utf16Pos, YankKind};
pub use crate::error::{CommandError, GrammarResult};
pub use crate::introspect::{
    HelpSection, Introspectable, RenderedAnchor, RenderedIntrospection, SourceEntry, SourceLabel,
    render_introspection, render_introspection_lines,
};
pub use crate::modal::{ModalState, SearchDirection, VisualKind};
pub use crate::range::{Range, RangeBound};
pub use crate::register::Register;
pub use crate::registry::{
    ActionContext, ActionSpec, CommandRegistration, CommandRegistry, CommentSyntax,
    ExCommandContext, ExCommandSpec, MotionSpec, OperatorContext, OperatorSpec, ScopeResolver,
    SurfaceForm, TextObjectEnv, TextObjectSpec,
};
pub use crate::registry::{ExCommandId, MotionId, OperatorId, TextObjectId};
pub use crate::source::{SourceKind, SourceLayer, SourceLocation};
pub use crate::target::Target;

/// Re-export the protocol's CommandId so callers don't need a second import.
pub use lattice_protocol::ids::CommandId;

/// M.10.3 (2026-06-03): typed handle for `ServiceRegistry`
/// registration + lookup. Boot wraps the `CommandRegistry` in
/// an `Arc<CommandRegistry>` and registers it under this alias;
/// mode crates pull it via
/// `ctx.service::<CommandRegistryHandle>()` to look up
/// CommandIds by action name (`id_by_name("action:...")`) at
/// `on_activate` time. Same shape as
/// `lattice_mode::ActionHandlerRegistryHandle` per
/// `feedback_servicesregistry_arc_typeid`.
pub type CommandRegistryHandle = std::sync::Arc<CommandRegistry>;
