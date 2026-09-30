//! `CommandInvocation` -- the unified call type that flows through the
//! dispatcher (DESIGN.md §5.2.1).
//!
//! Vim ex-syntax (the `:` parser front-end), keymap chord resolution, command
//! palette selection, and plugin-to-plugin calls all produce values of this
//! shape. The dispatcher's `execute()` consumes them.
//!
//! An invocation is plain data: `Clone`, serializable, and free of
//! borrowed state, so it can be recorded (macros record invocations, not
//! keystrokes), replayed (`.`), sent across the core protocol, or built by
//! a plugin. It names its command by [`CommandId`], which is only
//! meaningful against the [`CommandRegistry`](crate::CommandRegistry) that
//! minted it.
//!
//! # Examples
//!
//! The slots of `"a3dd` (register `a`, count 3, the delete operator over
//! the current line), filled by hand:
//!
//! ```
//! use lattice_grammar::{CommandInvocation, CommandRegistry, Count, Range, Register, builtins};
//!
//! let mut registry = CommandRegistry::new();
//! let b = builtins::populate(&mut registry);
//!
//! let inv = CommandInvocation::of(b.delete.0)
//!     .with_count(Count(3))
//!     .with_register(Register::Named('a'))
//!     .with_range(Range::CurrentLine);
//!
//! assert_eq!(inv.count_or_default().get(), 3);
//! assert_eq!(inv.register_or_default(), Register::Named('a'));
//!
//! // Unset slots fall back to vim's defaults.
//! let bare = CommandInvocation::of(b.delete.0);
//! assert_eq!(bare.count, None);
//! assert_eq!(bare.count_or_default(), Count::ONE);
//! assert_eq!(bare.register_or_default(), Register::Unnamed);
//! ```

use serde::{Deserialize, Serialize};

use lattice_protocol::ids::CommandId;

use crate::args::Args;
use crate::range::Range;
use crate::register::Register;
use crate::target::Target;

/// A vim count prefix: the `3` in `3dw` or `3j`. Each command decides what
/// it multiplies; an absent count is [`Count::ONE`] (the `Default`), but
/// [`CommandInvocation::count`] keeps `None` distinct so a command that
/// cares (`G` vs `5G`) can tell "no count" from "count 1".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Count(pub u32);

impl Count {
    /// The implicit count of a bare command.
    pub const ONE: Count = Count(1);

    /// The raw value. Evaluators typically use `get().max(1)` to treat a
    /// stray `0` as 1.
    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for Count {
    fn default() -> Self {
        Count::ONE
    }
}

/// One call of one command, with every grammar slot vim can fill: the
/// unified call type every front-end produces and
/// [`execute`](crate::execute) consumes (DESIGN.md §5.2.1).
///
/// A chord (`"a3dw`), a `:` line (`:%s/a/b/g`), a palette pick, a macro
/// replay and a plugin call all become one of these. The dispatcher looks
/// `command` up in the registry and routes by its [`CommandKind`]; each
/// kind reads the slots it understands and ignores the rest (a motion
/// ignores `register`, an action ignores `range` and `target`).
///
/// Build with [`Self::of`] plus the `with_*` builders. The value owns
/// everything it holds and borrows nothing, so it can outlive the keystroke
/// that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandInvocation {
    /// The command to run. Must have been minted by the registry the
    /// invocation is dispatched against, or dispatch fails with
    /// [`CommandError::UnknownCommand`](crate::CommandError::UnknownCommand).
    pub command: CommandId,
    /// The count prefix, if one was typed. `None` and `Some(Count(1))`
    /// differ: see [`Count`].
    pub count: Option<Count>,
    /// The `"x` register prefix, if one was typed; `None` means the
    /// unnamed register.
    pub register: Option<Register>,
    /// An explicit grammar range (`:%`, `:1,5`, the Visual selection).
    /// When an operator has both, the range wins over [`Self::target`].
    pub range: Option<Range>,
    /// What an operator acts on: a motion, text object or range. Unused by
    /// other kinds.
    pub target: Option<Target>,
    /// Command-specific arguments: the char of `f{char}`, the path of
    /// `:w path`, the parsed fields of `:s/…/…/`.
    pub args: Args,
    /// Trailing `!` on the ex-syntax form (`:q!`, `:w!`, `:e!`). Carried
    /// out of the parser into the dispatcher; meaningless for non-ex
    /// invocations and ignored by motion / operator / text-object
    /// dispatch.
    #[serde(default)]
    pub bang: bool,
}

impl CommandInvocation {
    /// A bare invocation of `command`: no count, register, range or
    /// target; [`Args::None`]; no bang.
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

    /// Set the count prefix.
    pub fn with_count(mut self, count: Count) -> Self {
        self.count = Some(count);
        self
    }

    /// Set the register prefix.
    pub fn with_register(mut self, register: Register) -> Self {
        self.register = Some(register);
        self
    }

    /// Set an explicit range.
    pub fn with_range(mut self, range: Range) -> Self {
        self.range = Some(range);
        self
    }

    /// Set the operator target.
    pub fn with_target(mut self, target: Target) -> Self {
        self.target = Some(target);
        self
    }

    /// Replace the arguments.
    pub fn with_args(mut self, args: Args) -> Self {
        self.args = args;
        self
    }

    /// Set the trailing-`!` bit (ex-commands only).
    pub fn with_bang(mut self, bang: bool) -> Self {
        self.bang = bang;
        self
    }

    /// The count, or [`Count::ONE`] when none was typed.
    pub fn count_or_default(&self) -> Count {
        self.count.unwrap_or_default()
    }

    /// The register, or the unnamed register when none was typed.
    pub fn register_or_default(&self) -> Register {
        self.register.unwrap_or_default()
    }
}

/// What kind of command an entry in the registry is. Determines how the
/// dispatcher resolves the invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandKind {
    /// Acts on a span (`d`, `c`, `y`, `gU`): resolves the invocation's
    /// target or range, then runs over it. See
    /// [`OperatorSpec`](crate::OperatorSpec).
    Operator,
    /// Computes a new cursor position (`w`, `j`, `G`); also usable as an
    /// operator target. See [`MotionSpec`](crate::MotionSpec).
    Motion,
    /// Selects a span around the cursor (`iw`, `a(`); an operator target or
    /// a Visual selection. See [`TextObjectSpec`](crate::TextObjectSpec).
    TextObject,
    /// Reached from the `:` line; parses its own argument string. See
    /// [`ExCommandSpec`](crate::ExCommandSpec).
    ExCommand,
    /// A free-form command with no grammar role — most chord bindings that
    /// are not motions or operators (`K` for LSP hover, fold cycling). See
    /// [`ActionSpec`](crate::ActionSpec).
    Action,
}

impl CommandKind {
    /// Kebab-case name used in help views and completion annotations:
    /// `"operator"`, `"motion"`, `"text-object"`, `"ex-command"`, `"action"`.
    pub fn label(self) -> &'static str {
        match self {
            CommandKind::Operator => "operator",
            CommandKind::Motion => "motion",
            CommandKind::TextObject => "text-object",
            CommandKind::ExCommand => "ex-command",
            CommandKind::Action => "action",
        }
    }

    /// Single-glyph marker for completion menus and help headings; agrees
    /// with [`kind_icon`] on [`Self::label`].
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
    /// Lower-case name: `"reflex"`, `"display"`, `"background"`.
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
#[derive(Debug, Clone)]
pub struct CommandSpec {
    /// The id the registry minted at registration; unique per process.
    pub id: CommandId,
    /// The canonical, namespaced name (`motion:word-forward`,
    /// `operator:delete`, `ex:write`, `action:…`). The key for
    /// [`CommandRegistry::id_by_name`](crate::CommandRegistry::id_by_name);
    /// user-typed aliases are resolved to it by the front-end.
    pub name: String,
    /// Which dispatcher path handles it.
    pub kind: CommandKind,
    /// The help text shown by `:describe-command`, `:apropos` and
    /// completion.
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
