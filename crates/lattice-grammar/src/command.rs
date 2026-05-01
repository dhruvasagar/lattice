//! `CommandInvocation` -- the unified call type that flows through the
//! dispatcher (DESIGN.md §5.2.1).
//!
//! Vim ex-syntax (the `:` parser front-end), keymap chord resolution, command
//! palette selection, and plugin-to-plugin calls all produce values of this
//! shape. The dispatcher's `execute()` consumes them.

use serde::{Deserialize, Serialize};

use lattice_protocol::ids::CommandId;

use crate::args::Args;
use crate::range::Range;
use crate::register::Register;
use crate::target::Target;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Count(pub u32);

impl Count {
    pub const ONE: Count = Count(1);

    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for Count {
    fn default() -> Self {
        Count::ONE
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandInvocation {
    pub command: CommandId,
    pub count: Option<Count>,
    pub register: Option<Register>,
    pub range: Option<Range>,
    pub target: Option<Target>,
    pub args: Args,
    /// Trailing `!` on the ex-syntax form (`:q!`, `:w!`, `:e!`). Carried
    /// out of the parser into the dispatcher; meaningless for non-ex
    /// invocations and ignored by motion / operator / text-object
    /// dispatch.
    #[serde(default)]
    pub bang: bool,
}

impl CommandInvocation {
    pub fn of(command: CommandId) -> Self {
        Self {
            command,
            count: None,
            register: None,
            range: None,
            target: None,
            args: Args::None,
            bang: false,
        }
    }

    pub fn with_count(mut self, count: Count) -> Self {
        self.count = Some(count);
        self
    }

    pub fn with_register(mut self, register: Register) -> Self {
        self.register = Some(register);
        self
    }

    pub fn with_range(mut self, range: Range) -> Self {
        self.range = Some(range);
        self
    }

    pub fn with_target(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }

    pub fn with_args(mut self, args: Args) -> Self {
        self.args = args;
        self
    }

    pub fn with_bang(mut self, bang: bool) -> Self {
        self.bang = bang;
        self
    }

    pub fn count_or_default(&self) -> Count {
        self.count.unwrap_or_default()
    }

    pub fn register_or_default(&self) -> Register {
        self.register.unwrap_or_default()
    }
}

/// What kind of command an entry in the registry is. Determines how the
/// dispatcher resolves the invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
    Operator,
    Motion,
    TextObject,
    ExCommand,
    Action,
}

impl CommandKind {
    pub fn label(self) -> &'static str {
        match self {
            CommandKind::Operator => "operator",
            CommandKind::Motion => "motion",
            CommandKind::TextObject => "text-object",
            CommandKind::ExCommand => "ex-command",
            CommandKind::Action => "action",
        }
    }
}

/// Metadata + the actual implementation of a registered command. Stored in
/// the `CommandRegistry`.
#[derive(Debug)]
pub struct CommandSpec {
    pub id: CommandId,
    pub name: String,
    pub kind: CommandKind,
    pub doc: String,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn count_default_is_one() {
        assert_eq!(Count::default(), Count::ONE);
        assert_eq!(Count::default().get(), 1);
    }

    #[test]
    fn invocation_builder_sets_each_field() {
        let id = CommandId::new(1);
        let inv = CommandInvocation::of(id)
            .with_count(Count(3))
            .with_register(Register::Named('a'))
            .with_range(Range::Whole)
            .with_args(Args::Char('q'));
        assert_eq!(inv.command, id);
        assert_eq!(inv.count, Some(Count(3)));
        assert_eq!(inv.register, Some(Register::Named('a')));
        assert_eq!(inv.range, Some(Range::Whole));
        assert_eq!(inv.args, Args::Char('q'));
    }

    #[test]
    fn count_or_default_returns_one_when_unset() {
        let inv = CommandInvocation::of(CommandId::new(1));
        assert_eq!(inv.count_or_default(), Count::ONE);
    }

    #[test]
    fn register_or_default_returns_unnamed_when_unset() {
        let inv = CommandInvocation::of(CommandId::new(1));
        assert_eq!(inv.register_or_default(), Register::Unnamed);
    }

    #[test]
    fn command_kind_labels() {
        assert_eq!(CommandKind::Operator.label(), "operator");
        assert_eq!(CommandKind::Motion.label(), "motion");
        assert_eq!(CommandKind::TextObject.label(), "text-object");
        assert_eq!(CommandKind::ExCommand.label(), "ex-command");
        assert_eq!(CommandKind::Action.label(), "action");
    }
}
