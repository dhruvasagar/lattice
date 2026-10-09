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
//! # What this crate owns
//!
//! - **The command registry.** [`CommandRegistry`] holds every motion,
//!   operator, text object, ex-command and action under a namespaced name
//!   (`motion:word-forward`, `operator:delete`, `ex:write`), each with a
//!   typed spec ([`MotionSpec`], [`OperatorSpec`], [`TextObjectSpec`],
//!   [`ExCommandSpec`], [`ActionSpec`]) and introspectable metadata
//!   ([`CommandSpec`]). Plugins register into the same registry through
//!   `register_plugin_*`; [`CommandRegistryHandle`] is how it is shared.
//! - **The call type and the dispatcher.** [`CommandInvocation`] is the one
//!   shape every front-end produces (chord, `:` line, palette, macro, plugin);
//!   [`execute`] / [`execute_with_env`] resolve it against a
//!   `lattice_core::Document` and return an [`Effect`]. Dispatch is
//!   synchronous and runs on the document's actor.
//! - **The grammar's typed values.** [`Count`], [`Register`], [`Range`],
//!   [`Target`], [`Args`], [`ModalState`], and the [`Effect`] /
//!   [`AppEffect`] vocabulary commands return to the host.
//! - **The native vim catalog.** [`builtins::populate`] and
//!   [`ex_commands::populate`] register the built-in motions, operators, text
//!   objects and ex-commands; [`reflow`] is the `gq` engine.
//! - **Introspection.** [`Introspectable`] and [`render_introspection`] give
//!   every `:describe-*` view one shape.
//!
//! # What it must not depend on
//!
//! Only `lattice-protocol` and `lattice-core` from the workspace. No
//! tree-sitter, no `lattice-mode` / `lattice-runtime` / host / UI crate, no
//! plugin runtime. The grammar runs on every keystroke and is linked by all
//! of those layers, so it must sit beneath them; anything it needs from
//! above arrives as data or as a small trait the host implements
//! ([`ScopeResolver`], [`IndentResolver`], [`FoldResolver`],
//! [`MarkResolver`], [`ViewportResolver`], [`DisplayResolver`]) bundled in
//! a per-dispatch [`GrammarEnv`]. That boundary is why this is a crate: it
//! is what keeps built-in and plugin commands, and every buffer kind, on
//! one dispatch path without dragging the syntax stack or the host into it.
//!
//! # Example
//!
//! Register a motion, then dispatch `2` of it through the one dispatcher:
//!
//! ```
//! use std::sync::Arc;
//! use lattice_core::{BufferId, Document};
//! use lattice_grammar::registry::MotionResult;
//! use lattice_grammar::{
//!     CancellationToken, CommandInvocation, CommandRegistry, Count, CurswantEffect, Effect,
//!     MotionSpec, execute,
//! };
//! use lattice_protocol::position::Position;
//!
//! let mut registry = CommandRegistry::new();
//! let down = registry.register_motion(
//!     "motion:my-line-down",
//!     "Move `count` lines down, to column 0.",
//!     MotionSpec {
//!         jump: false,
//!         exclusive: false,
//!         curswant: CurswantEffect::default(),
//!         args_schema: vec![],
//!         apply: Arc::new(|ctx| {
//!             let target = Position::new(ctx.from.line + ctx.count.get(), 0);
//!             Ok(MotionResult { target, ..Default::default() })
//!         }),
//!     },
//! );
//!
//! let mut doc = Document::from_text("a\nb\nc\n");
//! let effect = execute(
//!     &registry,
//!     &mut doc,
//!     BufferId(0),
//!     Position::ZERO,
//!     CommandInvocation::of(down.0).with_count(Count(2)),
//!     &CancellationToken::never(),
//! )
//! .unwrap();
//! assert!(matches!(effect, Effect::CursorMove(p) if p == Position::new(2, 0)));
//! ```
//!
//! # Design documents
//!
//! - `docs/dev/architecture/design.md` §5.2 (modal engine; §5.2.1 unified
//!   dispatch, §5.2.5 latency classes), §5.11 (introspection)
//! - `docs/dev/architecture/typed-motion-dispatch.md`
//! - `docs/dev/architecture/treesitter-motions.md`
//! - `docs/dev/architecture/select-mode.md`
//! - `docs/dev/architecture/text-reflow.md`
//! - `docs/dev/architecture/auto-indent.md`
#![warn(missing_docs)]

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
// RF.1: the text-reflow engine. Here rather than in a crate of its own
// (heuristic #6: it carves out no dependency surface) and here rather
// than in `lattice-format` (that crate is process spawning and diffing —
// a different mechanism that shares a word).
pub mod reflow;
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
pub use crate::dispatcher::{
    execute, execute_motion_only, execute_motion_only_reporting, execute_with_env, notice_text,
};
pub use crate::effect::{
    EchoLevel, Effect, FileAnchor, LspRequest, QuitScope, SubstituteScope, Utf16Pos, YankKind,
};
pub use crate::error::{CommandError, GrammarResult};
pub use crate::introspect::{
    HelpSection, Introspectable, RenderedAnchor, RenderedIntrospection, SourceEntry, SourceLabel,
    render_introspection, render_introspection_lines, render_introspection_with,
};
pub use crate::modal::{ModalState, SearchDirection, VisualKind};
pub use crate::range::{
    Range, RangeBound, RangeEnv, RangeError, parse_range_prefix, resolve_lines,
};
pub use crate::register::Register;
pub use crate::registry::{
    ActionContext, ActionSpec, CommandRegistration, CommandRegistry, CommentSyntax, Curswant,
    CurswantEffect, DisplayResolver, ExCommandContext, ExCommandSpec, FindKind, FoldResolver,
    GrammarEnv, IndentResolver, LastFind, LastSearch, MarkResolver, MotionNotice, MotionSpec,
    NavBoundary, NavDir, OperatorContext, OperatorSpec, ScopeResolver, ShownLines, SurfaceForm,
    TextObjectSpec, ViewportResolver,
};
pub use crate::registry::{ExCommandId, MotionId, OperatorId, TextObjectId};
pub use crate::source::{SourceKind, SourceLayer, SourceLocation};
pub use crate::target::Target;

/// Re-export the protocol's CommandId so callers don't need a second import.
pub use lattice_protocol::ids::CommandId;

/// The shared, hot-swappable command registry: the typed handle for
/// `ServiceRegistry` registration + lookup (M.10.3, 2026-06-03). Mode crates pull it via
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
