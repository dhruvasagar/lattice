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

pub use crate::app_effect::{
    AppEffect, ErrorTarget, HScroll, InsertLineEdit, PaneDirection, ScrollPos, ViewportPos,
};
pub use crate::args::{ArgDefault, ArgKind, ArgSpec, ArgValue, Args};
pub use crate::cancel::{CancellationToken, CheckCancelled};
pub use crate::command::{
    CommandInvocation, CommandKind, CommandSpec, Count, LatencyClass, kind_icon,
};
pub use crate::dispatcher::{execute, execute_motion_only, execute_with_env};
pub use crate::effect::{
    EchoLevel, Effect, LspRequest, QuitScope, SubstituteScope, Utf16Pos, YankKind,
};
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
    ExCommandContext, ExCommandSpec, GrammarEnv, IndentResolver, MotionSpec, NavBoundary, NavDir,
    OperatorContext, OperatorSpec, ScopeResolver, SurfaceForm, TextObjectSpec,
};
pub use crate::registry::{ExCommandId, MotionId, OperatorId, TextObjectId};
pub use crate::source::{SourceKind, SourceLayer, SourceLocation};
pub use crate::target::Target;

/// Re-export the protocol's CommandId so callers don't need a second import.
pub use lattice_protocol::ids::CommandId;

/// M.10.3 (2026-06-03): typed handle for `ServiceRegistry`
/// registration + lookup. Mode crates pull it via
/// `ctx.service::<CommandRegistryHandle>()` to look up
/// CommandIds by action name (`id_by_name("action:...")`) at
/// `on_activate` time. Same shape as
/// `lattice_mode::ActionHandlerRegistryHandle` per
/// `feedback_servicesregistry_arc_typeid`.
///
/// PL8.B / B3b (2026-07-15): held behind `ArcSwap` (was a bare
/// `Arc<CommandRegistry>`) so the plugin loader can RCU-register a
/// runtime grammar contribution (`register_plugin_motion` /
/// `_operator` / `_ex_command` / …) into a cloned registry and
/// `store` it, while the dispatch path — the per-buffer actor and
/// every host-side ex-command / completion read — snapshots it
/// wait-free via `.load()` (`.load_full()` where an owned `Arc`
/// snapshot must outlive a `&mut self` borrow). Mirrors
/// `lattice_mode::ModeRegistryHandle` / `lattice_picker::PickerRegistryHandle`
/// (decision B: ArcSwap all plugin-contributed registries).
pub type CommandRegistryHandle = std::sync::Arc<arc_swap::ArcSwap<CommandRegistry>>;
