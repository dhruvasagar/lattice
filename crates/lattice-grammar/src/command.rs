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

    pub fn icon(self) -> char {
        match self {
            CommandKind::ExCommand => ':',
            CommandKind::Motion => '→',
            CommandKind::Operator => '~',
            CommandKind::TextObject => '…',
            CommandKind::Action => '·',
        }
    }
}

/// Map a kind-label string to its display icon. Covers all labels
/// emitted by `KindLabelAnnotator` and `Introspectable::kind_label`.
/// Returns `·` for unknown labels.
pub fn kind_icon(label: &str) -> &'static str {
    match label {
        "ex-command" => ":",
        "motion" => "→",
        "operator" => "~",
        "text-object" => "…",
        "action" => "·",
        "command" => "·",
        "option" => "=",
        "file" => "f",
        "directory" => "d",
        "pattern" => "/",
        "buffer" => "b",
        "register" => "\"",
        "mark" => "'",
        "chord" => "@",
        "plugin" => "+",
        "plugin-api" => "+",
        "major" => "◆",
        "minor" => "◇",
        "stub" => "·",
        "doc" => "·",
        _ => "·",
    }
}

/// How the runtime should schedule a command, and what budget the CI
/// test harness will eventually enforce on it (DESIGN.md §5.2.5).
///
/// **v1 status: declarative only.** Every spec carries a class and
/// `:describe-command` surfaces it. The runtime infrastructure that
/// actually enforces these budgets (cancellation tokens; deadline
/// timers; bench-time per-class p99 assertions) lands together with
/// the §5.10 event-bus and the cancellation-token contract. Adding
/// the field now means hundreds of registrations don't have to be
/// retrofitted later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum LatencyClass {
    /// Single-stroke editing primitive: cursor motion, char insert,
    /// mode entry, simple delete, scroll. Sync `Effect` must commit
    /// within the keystroke budget (`<2ms p99`). The default for
    /// motions, operators, text objects, and small ex-commands.
    #[default]
    Reflex,
    /// UI affordance whose sync prelude must *appear* immediately
    /// (`<10ms p99`) but whose data may arrive later via events:
    /// completion popup, picker, hover, status segment. The
    /// `:describe-*` family of help views fits here.
    Display,
    /// No user-perceived sync budget. File-watcher tick, indexer
    /// pass, plugin housekeeping, LSP debounce. Throughput-only.
    Background,
}

impl LatencyClass {
    pub fn label(self) -> &'static str {
        match self {
            LatencyClass::Reflex => "reflex",
            LatencyClass::Display => "display",
            LatencyClass::Background => "background",
        }
    }

    /// Human-readable budget string for `:describe-command`
    /// rendering. Values come straight from DESIGN.md §5.2.5.
    pub fn budget_label(self) -> &'static str {
        match self {
            LatencyClass::Reflex => "<2ms p99",
            LatencyClass::Display => "<10ms p99 sync prelude",
            LatencyClass::Background => "throughput-only",
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
    /// Per-positional-argument metadata (DESIGN.md §B.1). Lifted from
    /// the per-kind spec (`MotionSpec.args_schema`,
    /// `ExCommandSpec.args_schema`, ...) at registration time so callers
    /// can introspect arg shape without knowing the registration kind.
    /// Used by `:describe-command`, palette form rendering, and missing-
    /// arg prompts.
    pub args_schema: Vec<crate::args::ArgSpec>,
    /// Where this command was registered (DESIGN.md §5.11). Captured
    /// via `#[track_caller]` for built-ins, by the plugin host for
    /// plugin-registered commands, by the config loader for
    /// user-registered commands. The field is `pub` for read access
    /// (introspection), but the only writers are the trusted
    /// `pub(crate) insert_*` registry methods -- there is no public
    /// API that takes a `SourceLocation` parameter.
    pub source: crate::source::SourceLocation,
    /// Latency class declaration (DESIGN.md §5.2.5). Surfaced by
    /// `:describe-command`; future cancellation / deadline
    /// machinery reads this to set per-call budgets. v1 is purely
    /// declarative -- no runtime enforcement yet.
    pub latency_class: LatencyClass,
}

impl crate::introspect::Introspectable for CommandSpec {
    fn kind_label(&self) -> &'static str {
        self.kind.label()
    }

    fn identifier(&self) -> String {
        self.name.clone()
    }

    fn doc(&self) -> &str {
        &self.doc
    }

    fn sources(&self) -> Vec<crate::introspect::SourceEntry<'_>> {
        vec![crate::introspect::SourceEntry {
            label: crate::introspect::SourceLabel::DefinedAt,
            source: &self.source,
        }]
    }

    fn extra_sections(&self) -> Vec<crate::introspect::HelpSection> {
        let mut sections = Vec::new();
        // Latency class declaration (DESIGN.md §5.2.5). Surfaced
        // in describe-command so users can see the budget the
        // runtime treats this command under.
        sections.push(crate::introspect::HelpSection {
            heading: "Latency:".to_string(),
            lines: vec![format!(
                "       {}  ({})",
                self.latency_class.label(),
                self.latency_class.budget_label()
            )],
            anchor: Some("latency".to_string()),
        });
        if !self.args_schema.is_empty() {
            // Two-tiered render: a parent "Arguments:" section
            // anchored as "args", then one subsection per arg
            // anchored as "arg:<name>". `<C-h>` on the cmdline
            // jumps directly to the relevant `arg:<name>` (DESIGN.md
            // §5.11.1 + §5.11.3 arg-aware help).
            sections.push(crate::introspect::HelpSection {
                heading: "Arguments:".to_string(),
                lines: Vec::new(),
                anchor: Some("args".to_string()),
            });
            for (i, arg) in self.args_schema.iter().enumerate() {
                let default = match &arg.default {
                    crate::args::ArgDefault::Required => "required".to_string(),
                    crate::args::ArgDefault::None => "optional".to_string(),
                    crate::args::ArgDefault::Literal(_) => "default".to_string(),
                    crate::args::ArgDefault::UseSelection => "default: selection".to_string(),
                    crate::args::ArgDefault::UseCursorWord => "default: cursor word".to_string(),
                    crate::args::ArgDefault::UseLastResponse => "default: last value".to_string(),
                };
                let mut lines = Vec::with_capacity(2);
                if !arg.doc.is_empty() {
                    lines.push(format!("       {}", arg.doc));
                }
                sections.push(crate::introspect::HelpSection {
                    heading: format!("  {}. {}: {:?}  ({})", i + 1, arg.name, arg.kind, default),
                    lines,
                    anchor: Some(format!("arg:{}", arg.name)),
                });
            }
        }
        sections
    }
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

    #[test]
    fn command_kind_icons() {
        assert_eq!(CommandKind::ExCommand.icon(), ':');
        assert_eq!(CommandKind::Motion.icon(), '→');
        assert_eq!(CommandKind::Operator.icon(), '~');
        assert_eq!(CommandKind::TextObject.icon(), '…');
        assert_eq!(CommandKind::Action.icon(), '·');
    }

    #[test]
    fn kind_icon_maps_all_labels() {
        assert_eq!(kind_icon("ex-command"), ":");
        assert_eq!(kind_icon("motion"), "→");
        assert_eq!(kind_icon("operator"), "~");
        assert_eq!(kind_icon("text-object"), "…");
        assert_eq!(kind_icon("action"), "·");
        assert_eq!(kind_icon("command"), "·");
        assert_eq!(kind_icon("option"), "=");
        assert_eq!(kind_icon("file"), "f");
        assert_eq!(kind_icon("directory"), "d");
        assert_eq!(kind_icon("pattern"), "/");
        assert_eq!(kind_icon("buffer"), "b");
        assert_eq!(kind_icon("register"), "\"");
        assert_eq!(kind_icon("mark"), "'");
        assert_eq!(kind_icon("chord"), "@");
        assert_eq!(kind_icon("plugin"), "+");
        assert_eq!(kind_icon("plugin-api"), "+");
        assert_eq!(kind_icon("major"), "◆");
        assert_eq!(kind_icon("minor"), "◇");
        assert_eq!(kind_icon("stub"), "·");
        assert_eq!(kind_icon("doc"), "·");
        assert_eq!(kind_icon("unknown"), "·");
    }
}
