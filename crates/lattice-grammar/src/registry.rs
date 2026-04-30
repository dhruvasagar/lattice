//! The `CommandRegistry` holds every registered operator, motion, text
//! object, and ex-command. The dispatcher (`super::dispatcher::execute`)
//! looks up commands here.
//!
//! Built-in commands are registered at editor startup via `populate_builtins`
//! (see `super::builtins`). Plugins register their own through the same
//! `register_*` methods. v1 keeps these as native Rust closures; the WASM
//! plugin host (Phase 7) wraps the same shape.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use lattice_core::Buffer;
use lattice_core::buffer::AppliedEdit;
use lattice_core::Document;
use lattice_protocol::ids::CommandId;
use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::args::Args;
use crate::command::{CommandKind, CommandSpec, Count};
use crate::error::{CommandError, GrammarResult};
use crate::register::Register;

/// Strongly-typed handle to an operator command in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperatorId(pub CommandId);

/// Strongly-typed handle to a motion command in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MotionId(pub CommandId);

/// Strongly-typed handle to a text-object command in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TextObjectId(pub CommandId);

/// Strongly-typed handle to an ex-command in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExCommandId(pub CommandId);

/// Strongly-typed handle to a custom range source (plugin-registered) used by
/// `Range::Custom(RangeId)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RangeId(pub CommandId);

/// Context passed to a motion's evaluator.
pub struct MotionContext<'a> {
    pub buffer: &'a Buffer,
    pub from: Position,
    pub count: Count,
    pub args: Args,
}

/// What a motion's evaluator returned.
#[derive(Debug, Clone, Copy)]
pub struct MotionResult {
    pub target: Position,
    /// `true` if the motion is linewise (ranges expand to whole lines on
    /// resolution).
    pub linewise: bool,
}

/// Implementation of a motion. Boxed because evaluator closures capture
/// configuration; cheap to call.
type MotionFn = Box<dyn Fn(&MotionContext) -> GrammarResult<MotionResult> + Send + Sync>;

pub struct MotionSpec {
    pub jump: bool,
    pub exclusive: bool,
    pub apply: MotionFn,
}

impl std::fmt::Debug for MotionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MotionSpec")
            .field("jump", &self.jump)
            .field("exclusive", &self.exclusive)
            .finish_non_exhaustive()
    }
}

/// Context passed to an operator's evaluator.
pub struct OperatorContext<'a> {
    pub document: &'a mut Document,
    pub range: ProtoRange,
    pub register: Register,
    pub count: Count,
    pub args: Args,
}

type OperatorFn =
    Box<dyn Fn(&mut OperatorContext) -> GrammarResult<Vec<AppliedEdit>> + Send + Sync>;

pub struct OperatorSpec {
    pub repeatable: bool,
    pub apply: OperatorFn,
}

impl std::fmt::Debug for OperatorSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorSpec")
            .field("repeatable", &self.repeatable)
            .finish_non_exhaustive()
    }
}

/// Context passed to a text-object's evaluator.
pub struct TextObjectContext<'a> {
    pub buffer: &'a Buffer,
    pub at: Position,
    pub count: Count,
    pub args: Args,
}

type TextObjectFn = Box<dyn Fn(&TextObjectContext) -> GrammarResult<ProtoRange> + Send + Sync>;

pub struct TextObjectSpec {
    pub apply: TextObjectFn,
}

impl std::fmt::Debug for TextObjectSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextObjectSpec").finish_non_exhaustive()
    }
}

/// What a registered command holds in the registry, beyond its metadata.
pub enum CommandRegistration {
    Motion(MotionSpec),
    Operator(OperatorSpec),
    TextObject(TextObjectSpec),
    /// Phase 1 stub for ex-commands and free-form actions; populated later.
    Stub,
}

impl CommandRegistration {
    pub fn kind(&self) -> CommandKind {
        match self {
            CommandRegistration::Motion(_) => CommandKind::Motion,
            CommandRegistration::Operator(_) => CommandKind::Operator,
            CommandRegistration::TextObject(_) => CommandKind::TextObject,
            CommandRegistration::Stub => CommandKind::Action,
        }
    }
}

#[derive(Debug, Default)]
pub struct CommandRegistry {
    by_id: HashMap<CommandId, CommandEntry>,
    by_name: HashMap<String, CommandId>,
}

pub(crate) struct CommandEntry {
    pub spec: CommandSpec,
    pub registration: CommandRegistration,
}

impl std::fmt::Debug for CommandEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandEntry")
            .field("spec", &self.spec)
            .field("registration", &self.registration.kind().label())
            .finish()
    }
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_motion(&mut self, name: &str, doc: &str, spec: MotionSpec) -> MotionId {
        let id = next_command_id();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::Motion,
                doc: doc.to_string(),
            },
            registration: CommandRegistration::Motion(spec),
        });
        MotionId(id)
    }

    pub fn register_operator(&mut self, name: &str, doc: &str, spec: OperatorSpec) -> OperatorId {
        let id = next_command_id();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::Operator,
                doc: doc.to_string(),
            },
            registration: CommandRegistration::Operator(spec),
        });
        OperatorId(id)
    }

    pub fn register_text_object(
        &mut self,
        name: &str,
        doc: &str,
        spec: TextObjectSpec,
    ) -> TextObjectId {
        let id = next_command_id();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::TextObject,
                doc: doc.to_string(),
            },
            registration: CommandRegistration::TextObject(spec),
        });
        TextObjectId(id)
    }

    pub fn lookup(&self, id: CommandId) -> Option<&CommandSpec> {
        self.by_id.get(&id).map(|e| &e.spec)
    }

    pub fn lookup_by_name(&self, name: &str) -> Option<&CommandSpec> {
        self.by_name.get(name).and_then(|id| self.lookup(*id))
    }

    pub fn id_by_name(&self, name: &str) -> Option<CommandId> {
        self.by_name.get(name).copied()
    }

    pub(crate) fn entry(&self, id: CommandId) -> Option<&CommandEntry> {
        self.by_id.get(&id)
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.by_name.keys().map(String::as_str)
    }

    fn insert(&mut self, entry: CommandEntry) {
        let id = entry.spec.id;
        let name = entry.spec.name.clone();
        self.by_id.insert(id, entry);
        self.by_name.insert(name, id);
    }
}

fn next_command_id() -> CommandId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    CommandId::new(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// Helper used by the dispatcher to extract the typed body from a registry
/// entry.
pub(crate) fn require_motion(entry: &CommandEntry) -> GrammarResult<&MotionSpec> {
    match &entry.registration {
        CommandRegistration::Motion(s) => Ok(s),
        other => Err(CommandError::KindMismatch {
            expected: "motion",
            actual: other.kind().label(),
        }),
    }
}

pub(crate) fn require_operator(entry: &CommandEntry) -> GrammarResult<&OperatorSpec> {
    match &entry.registration {
        CommandRegistration::Operator(s) => Ok(s),
        other => Err(CommandError::KindMismatch {
            expected: "operator",
            actual: other.kind().label(),
        }),
    }
}

pub(crate) fn require_text_object(entry: &CommandEntry) -> GrammarResult<&TextObjectSpec> {
    match &entry.registration {
        CommandRegistration::TextObject(s) => Ok(s),
        other => Err(CommandError::KindMismatch {
            expected: "text-object",
            actual: other.kind().label(),
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn dummy_motion() -> MotionSpec {
        MotionSpec {
            jump: false,
            exclusive: false,
            apply: Box::new(|ctx| {
                Ok(MotionResult {
                    target: ctx.from,
                    linewise: false,
                })
            }),
        }
    }

    #[test]
    fn empty_registry() {
        let r = CommandRegistry::new();
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert!(r.lookup_by_name("nope").is_none());
    }

    #[test]
    fn register_motion_returns_id_and_finds_by_name() {
        let mut r = CommandRegistry::new();
        let id = r.register_motion("test:noop", "no-op motion", dummy_motion());
        assert_eq!(r.len(), 1);
        let spec = r.lookup_by_name("test:noop").unwrap();
        assert_eq!(spec.id, id.0);
        assert_eq!(spec.kind, CommandKind::Motion);
    }

    #[test]
    fn distinct_ids_for_distinct_registrations() {
        let mut r = CommandRegistry::new();
        let a = r.register_motion("a", "", dummy_motion());
        let b = r.register_motion("b", "", dummy_motion());
        assert_ne!(a.0, b.0);
    }

    #[test]
    fn lookup_by_id_returns_metadata() {
        let mut r = CommandRegistry::new();
        let id = r.register_motion("test:m", "doc", dummy_motion());
        let spec = r.lookup(id.0).unwrap();
        assert_eq!(spec.name, "test:m");
        assert_eq!(spec.doc, "doc");
    }
}
