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
use lattice_core::Document;
use lattice_protocol::ids::CommandId;
use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::args::{ArgSpec, Args};
use crate::command::{CommandKind, CommandSpec, Count};
use crate::error::{CommandError, GrammarResult};
use crate::register::Register;
use crate::source::{SourceKind, SourceLayer, SourceLocation};

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
    /// Per-positional-argument metadata (DESIGN.md §B.1). Empty for
    /// motions without args (the common case).
    pub args_schema: Vec<ArgSpec>,
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
    /// Whether the range was produced by a linewise source (vim's
    /// `Range::CurrentLine` / `Range::Whole`, or a linewise visual
    /// selection). Yank uses this to tag the unnamed register so paste
    /// can do the right thing.
    pub linewise: bool,
    pub register: Register,
    pub count: Count,
    pub args: Args,
}

/// An operator's evaluator returns the full `Effect` it produced. Most
/// operators return `Effect::Edits(...)`; `change` adds a mode transition
/// via `Effect::Many(...)`; `yank` returns `Effect::Yank { ... }`. The
/// dispatcher passes the result through unchanged -- composition is
/// expressed in the Effect, not via flags on the spec.
type OperatorFn =
    Box<dyn Fn(&mut OperatorContext) -> GrammarResult<crate::effect::Effect> + Send + Sync>;

pub struct OperatorSpec {
    pub repeatable: bool,
    pub apply: OperatorFn,
    /// Per-positional-argument metadata (DESIGN.md §B.1). Empty for
    /// operators without args (the common case).
    pub args_schema: Vec<ArgSpec>,
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
    /// Per-positional-argument metadata (DESIGN.md §B.1). Empty for
    /// text objects without args (the common case).
    pub args_schema: Vec<ArgSpec>,
}

impl std::fmt::Debug for TextObjectSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextObjectSpec").finish_non_exhaustive()
    }
}

/// Context handed to an ex-command's evaluator. Mirrors the shape passed
/// to motion / operator / text-object specs but adds the `bang` bit and
/// drops direct document mutation: ex-commands describe their work by
/// returning an [`crate::effect::Effect`], which the host applies.
pub struct ExCommandContext {
    pub bang: bool,
    pub args: Args,
    pub range: Option<crate::range::Range>,
    pub register: Register,
    pub count: Count,
}

/// Parser callback for an ex-command. The host hands the rest of the
/// command line (everything after the command word and the optional `!`)
/// plus the `bang` bit; the callback returns typed [`Args`].
type ExParseFn = Box<dyn Fn(&str, bool) -> GrammarResult<Args> + Send + Sync>;

/// Evaluator callback. Returns the [`Effect`] the host should commit.
type ExApplyFn =
    Box<dyn Fn(&ExCommandContext) -> GrammarResult<crate::effect::Effect> + Send + Sync>;

/// How the user types this command on the `:` line. The default
/// (`Keyword`) covers most commands -- `:write`, `:quit`, `:set
/// number`. `Delimiter` is for the small family of commands whose
/// arguments are interleaved with delimiters: `:s/pat/repl/`,
/// `:g/pat/body`, `:v/pat/body`. The keyword form (`:ex:substitute`,
/// `:ex:global`) for these is intentionally a hard error -- the
/// front-end parser routes them via `try_parse_substitute` /
/// `try_parse_global`. UI surfaces (completion, command palette)
/// hide `Delimiter` commands because there's no useful keyword-form
/// completion for them; the user types the delimiter directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SurfaceForm {
    /// Type the command word, optionally with `!`, then args
    /// separated by whitespace. The default for most commands.
    #[default]
    Keyword,
    /// Type a delimiter prefix followed by a body. The keyword form
    /// errors with a redirect message; the embedded `hint` is the
    /// canonical syntax shown in that error (`:s/pat/repl/`,
    /// `:g/pat/body`).
    Delimiter { hint: &'static str },
}

pub struct ExCommandSpec {
    /// Whether the parser should accept a trailing `!` after the command
    /// word. If `false`, `:cmd!` parses as an unknown command.
    pub accepts_bang: bool,
    /// Whether the parser should accept an ex-style line range (`1,5cmd`,
    /// `'a,'bcmd`, ...). v1 only honours `Whole` and `CurrentLine`; this
    /// flag is the migration knob for richer range parsing.
    pub accepts_range: bool,
    pub parse_args: ExParseFn,
    pub apply: ExApplyFn,
    /// Per-positional-argument metadata (DESIGN.md §B.1). Drives palette
    /// forms, missing-arg prompts, completion, validation, and
    /// `:describe-command` output. Empty for commands taking no
    /// structured args (`:q`, `:noh`).
    pub args_schema: Vec<ArgSpec>,
    /// User-facing surface form. Drives whether completion / palette
    /// list this command. Defaults to [`SurfaceForm::Keyword`].
    pub surface_form: SurfaceForm,
}

impl std::fmt::Debug for ExCommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExCommandSpec")
            .field("accepts_bang", &self.accepts_bang)
            .field("accepts_range", &self.accepts_range)
            .finish_non_exhaustive()
    }
}

/// What a registered command holds in the registry, beyond its metadata.
pub enum CommandRegistration {
    Motion(MotionSpec),
    Operator(OperatorSpec),
    TextObject(TextObjectSpec),
    ExCommand(ExCommandSpec),
    /// Phase 1 stub for free-form actions populated later.
    Stub,
}

impl CommandRegistration {
    pub fn kind(&self) -> CommandKind {
        match self {
            CommandRegistration::Motion(_) => CommandKind::Motion,
            CommandRegistration::Operator(_) => CommandKind::Operator,
            CommandRegistration::TextObject(_) => CommandKind::TextObject,
            CommandRegistration::ExCommand(_) => CommandKind::ExCommand,
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

    /// Register a motion. The caller's source location is captured
    /// via `#[track_caller]` -- the caller cannot supply or override
    /// it. Trusted subsystems (config loader, plugin host bridge,
    /// runtime dispatcher) reach the `pub(crate) insert_motion`
    /// companion directly with a layer-appropriate source.
    #[track_caller]
    pub fn register_motion(&mut self, name: &str, doc: &str, spec: MotionSpec) -> MotionId {
        let source = capture_builtin_source();
        self.insert_motion(name, doc, spec, source)
    }

    /// Internal entry point: store the spec with a caller-supplied
    /// source. Visible only inside `lattice-grammar`; trusted
    /// subsystems in sibling crates reach this via a sealed-trait
    /// re-export when they exist (deferred until first cross-crate
    /// trusted subsystem lands -- see DESIGN.md §5.11).
    pub(crate) fn insert_motion(
        &mut self,
        name: &str,
        doc: &str,
        spec: MotionSpec,
        source: SourceLocation,
    ) -> MotionId {
        let id = next_command_id();
        let args_schema = spec.args_schema.clone();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::Motion,
                doc: doc.to_string(),
                args_schema,
                source,
            },
            registration: CommandRegistration::Motion(spec),
        });
        MotionId(id)
    }

    #[track_caller]
    pub fn register_operator(&mut self, name: &str, doc: &str, spec: OperatorSpec) -> OperatorId {
        let source = capture_builtin_source();
        self.insert_operator(name, doc, spec, source)
    }

    pub(crate) fn insert_operator(
        &mut self,
        name: &str,
        doc: &str,
        spec: OperatorSpec,
        source: SourceLocation,
    ) -> OperatorId {
        let id = next_command_id();
        let args_schema = spec.args_schema.clone();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::Operator,
                doc: doc.to_string(),
                args_schema,
                source,
            },
            registration: CommandRegistration::Operator(spec),
        });
        OperatorId(id)
    }

    #[track_caller]
    pub fn register_text_object(
        &mut self,
        name: &str,
        doc: &str,
        spec: TextObjectSpec,
    ) -> TextObjectId {
        let source = capture_builtin_source();
        self.insert_text_object(name, doc, spec, source)
    }

    pub(crate) fn insert_text_object(
        &mut self,
        name: &str,
        doc: &str,
        spec: TextObjectSpec,
        source: SourceLocation,
    ) -> TextObjectId {
        let id = next_command_id();
        let args_schema = spec.args_schema.clone();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::TextObject,
                doc: doc.to_string(),
                args_schema,
                source,
            },
            registration: CommandRegistration::TextObject(spec),
        });
        TextObjectId(id)
    }

    #[track_caller]
    pub fn register_ex_command(
        &mut self,
        name: &str,
        doc: &str,
        spec: ExCommandSpec,
    ) -> ExCommandId {
        let source = capture_builtin_source();
        self.insert_ex_command(name, doc, spec, source)
    }

    pub(crate) fn insert_ex_command(
        &mut self,
        name: &str,
        doc: &str,
        spec: ExCommandSpec,
        source: SourceLocation,
    ) -> ExCommandId {
        let id = next_command_id();
        let args_schema = spec.args_schema.clone();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::ExCommand,
                doc: doc.to_string(),
                args_schema,
                source,
            },
            registration: CommandRegistration::ExCommand(spec),
        });
        ExCommandId(id)
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

    /// Borrow the [`ExCommandSpec`] body for an ex-command id. Returns
    /// `None` for ids that aren't ex-commands or aren't registered.
    /// Used by the `:`-line parser front-end so it can call the
    /// command's `parse_args` callback before building the
    /// `CommandInvocation`.
    pub fn ex_command_spec(&self, id: CommandId) -> Option<&ExCommandSpec> {
        match self.by_id.get(&id)?.registration {
            CommandRegistration::ExCommand(ref spec) => Some(spec),
            _ => None,
        }
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

/// Read the immediate caller's source location through the
/// `#[track_caller]` mechanism. Must be called from inside a
/// `#[track_caller]`-marked function whose caller is the
/// registration site we want to record. Marked
/// `#[track_caller]` itself so the location propagates through:
/// `register_motion` -> `capture_builtin_source` -> caller's site.
#[track_caller]
fn capture_builtin_source() -> SourceLocation {
    let loc = std::panic::Location::caller();
    SourceLocation {
        layer: SourceLayer::Builtin,
        kind: SourceKind::File {
            path: std::path::PathBuf::from(loc.file()),
            line: Some(loc.line()),
        },
    }
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

pub(crate) fn require_ex_command(entry: &CommandEntry) -> GrammarResult<&ExCommandSpec> {
    match &entry.registration {
        CommandRegistration::ExCommand(s) => Ok(s),
        other => Err(CommandError::KindMismatch {
            expected: "ex-command",
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
            args_schema: vec![],
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

    // ---- Source-capture determinism (DESIGN.md §5.11) ----
    //
    // `#[track_caller]` is the load-bearing piece -- it captures the
    // caller's `(file, line)` automatically so registration sites
    // never need to spell their location explicitly. These tests
    // pin its behaviour: any future refactor that breaks call-site
    // capture (e.g. wrapping `register_motion` in a `dyn Fn`
    // dispatcher, which resets the location) fails CI.

    #[test]
    fn track_caller_captures_register_motion_call_site() {
        let mut r = CommandRegistry::new();
        // Sentinel: capture the line of the next call.
        let expected_line = line!() + 1;
        let id = r.register_motion("test:sentinel", "", dummy_motion());
        let spec = r.lookup(id.0).unwrap();
        match &spec.source.kind {
            SourceKind::File { path, line: Some(line) } => {
                assert!(
                    path.to_string_lossy().contains("registry.rs"),
                    "expected path to contain `registry.rs`, got `{}`",
                    path.display()
                );
                assert_eq!(
                    *line, expected_line,
                    "track_caller line drift: expected {expected_line}, got {line}"
                );
            }
            other => panic!("expected Builtin/File source, got {other:?}"),
        }
        assert_eq!(spec.source.layer, SourceLayer::Builtin);
    }

    #[test]
    fn track_caller_captures_register_operator_call_site() {
        let mut r = CommandRegistry::new();
        let expected_line = line!() + 1;
        let id = r.register_operator(
            "test:sentinel-op",
            "",
            OperatorSpec {
                repeatable: false,
                apply: Box::new(|_| Ok(crate::effect::Effect::None)),
                args_schema: vec![],
            },
        );
        let spec = r.lookup(id.0).unwrap();
        if let SourceKind::File { line: Some(line), .. } = &spec.source.kind {
            // The literal ends 6 lines after the `expected_line`
            // assignment because of formatting -- track_caller records
            // the line of the call expression's *first* token.
            assert_eq!(*line, expected_line);
        } else {
            panic!("expected File source, got {:?}", spec.source.kind);
        }
    }

    #[test]
    fn track_caller_captures_register_text_object_call_site() {
        let mut r = CommandRegistry::new();
        let expected_line = line!() + 1;
        let id = r.register_text_object(
            "test:sentinel-tobj",
            "",
            TextObjectSpec {
                apply: Box::new(|_| {
                    Ok(ProtoRange::new(
                        Position::ZERO,
                        Position::ZERO,
                    ))
                }),
                args_schema: vec![],
            },
        );
        let spec = r.lookup(id.0).unwrap();
        if let SourceKind::File { line: Some(line), .. } = &spec.source.kind {
            assert_eq!(*line, expected_line);
        } else {
            panic!("expected File source");
        }
    }

    #[test]
    fn each_registration_records_its_own_line() {
        // Two adjacent registrations should record different lines,
        // proving that `#[track_caller]` distinguishes call sites
        // and not just call origins.
        let mut r = CommandRegistry::new();
        let id_a = r.register_motion("test:a", "", dummy_motion());
        let id_b = r.register_motion("test:b", "", dummy_motion());
        let line_a = line_of(&r, id_a.0).expect("id_a has File source");
        let line_b = line_of(&r, id_b.0).expect("id_b has File source");
        assert_ne!(line_a, line_b);
        assert!(line_b > line_a, "second call's line should follow the first");
    }

    #[test]
    fn track_caller_propagates_through_helper_marked_track_caller() {
        // A helper that wraps `register_motion` and is itself marked
        // `#[track_caller]` should pass the caller's location through.
        // Without `#[track_caller]` on the helper, the location would
        // be that of the inner call inside the helper.
        #[track_caller]
        fn helper(r: &mut CommandRegistry, name: &str) -> MotionId {
            r.register_motion(name, "", dummy_motion())
        }
        let mut r = CommandRegistry::new();
        let expected_line = line!() + 1;
        let id = helper(&mut r, "test:via-helper");
        let line = line_of(&r, id.0).expect("File source");
        assert_eq!(
            line, expected_line,
            "helper marked #[track_caller] should propagate the OUTER caller's line"
        );
    }

    #[test]
    fn unmarked_helper_records_inner_line_not_outer() {
        // Counterexample: a helper that is NOT `#[track_caller]`
        // captures its own internal line, not the outer caller.
        // This documents the propagation contract -- any helper that
        // wraps `register_*` must opt in.
        fn unmarked_helper(r: &mut CommandRegistry, name: &str) -> (MotionId, u32) {
            let inner_line = line!() + 1;
            let id = r.register_motion(name, "", dummy_motion());
            (id, inner_line)
        }
        let mut r = CommandRegistry::new();
        let (id, inner_line) = unmarked_helper(&mut r, "test:unmarked");
        let captured = line_of(&r, id.0).expect("File source");
        assert_eq!(
            captured, inner_line,
            "unmarked helper should record the INNER call line"
        );
    }

    fn line_of(r: &CommandRegistry, id: CommandId) -> Option<u32> {
        match &r.lookup(id)?.source.kind {
            SourceKind::File { line, .. } => *line,
            _ => None,
        }
    }
}
