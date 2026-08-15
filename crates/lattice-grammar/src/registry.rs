//! The `CommandRegistry` holds every registered operator, motion, text
//! object, and ex-command. The dispatcher (`super::dispatcher::execute`)
//! looks up commands here.
//!
//! Built-in commands are registered at editor startup via `populate_builtins`
//! (see `super::builtins`). Plugins register their own through the same
//! `register_*` methods. v1 keeps these as native Rust closures; the WASM
//! plugin host (Phase 7) wraps the same shape.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use lattice_core::Buffer;
use lattice_core::BufferId;
use lattice_core::Document;
use lattice_protocol::ids::CommandId;
use lattice_protocol::position::{Position, Range as ProtoRange};

use crate::args::{ArgSpec, Args};
use crate::command::{CommandKind, CommandSpec, Count};
use crate::error::{CommandError, GrammarResult};
use crate::register::Register;
use crate::source::{SourceKind, SourceLayer, SourceLocation};

/// Strongly-typed handle to an operator command in the registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperatorId(pub CommandId);

/// Strongly-typed handle to a motion command in the registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MotionId(pub CommandId);

/// Strongly-typed handle to a text-object command in the registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TextObjectId(pub CommandId);

/// Strongly-typed handle to an ex-command in the registry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExCommandId(pub CommandId);

/// Strongly-typed handle to a custom range source (plugin-registered) used by
/// `Range::Custom(RangeId)`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RangeId(pub CommandId);

/// Context passed to a motion's evaluator.
///
/// **M.2.b.0.A (2026-05-31):** `buffer_id` carries the active
/// buffer's registry identity through to motion handlers. Built-
/// in motions (purely content-based: `w` / `e` / `]p` / etc.)
/// ignore it; kind-specific motions (multibuffer `]e` / `[e` /
/// `]E` / `[E`, future file-tree / oil structural motions, future
/// plugin-defined kinds) use it to look up the active major
/// mode's typed state through a service registry their handler
/// closure captures at registration time. Adding it here keeps
/// the grammar layer free of lattice-mode / ServiceRegistry
/// coupling — the handler decides what to look up.
pub struct MotionContext<'a> {
    pub buffer: &'a Buffer,
    /// Registry-level identity of the active buffer this motion
    /// is firing against. Distinct from `lattice_protocol::ids::
    /// DocumentId` (per-actor stable id); `BufferId` is the
    /// registry key that mode-state lookups use.
    pub buffer_id: BufferId,
    pub from: Position,
    pub count: Count,
    /// True when the invocation carried an explicit count (e.g.
    /// `5G`). False for bare invocations (`G` alone). Motions
    /// whose semantic changes with an explicit count (goto-last-
    /// line: last vs. specific line) use this to disambiguate.
    pub has_explicit_count: bool,
    pub args: Args,
    /// Cooperative cancellation handle (DESIGN.md §5.2.5). Hot
    /// loops should poll `cancel.check()?` on each iteration; on a
    /// flipped token the evaluator returns
    /// [`crate::CommandError::Cancelled`] and the dispatcher
    /// commits no effect.
    pub cancel: &'a crate::CancellationToken,
    /// N.1.4-motions: the active buffer's tree-sitter resolver for structural
    /// motions (`]f`/`[c`/…). `None` on Plain buffers with no parse — the
    /// motion then no-ops. Threaded by the host, identical to `TextObjectContext`.
    pub scope_resolver: Option<&'a dyn ScopeResolver>,
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
type MotionFn = Arc<dyn Fn(&MotionContext) -> GrammarResult<MotionResult> + Send + Sync>;

#[derive(Clone)]
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
    /// Cooperative cancellation handle (DESIGN.md §5.2.5). Operators
    /// that scan large ranges (`d_whole`, `gU` over a big visual
    /// block) should poll `cancel.check()?` between rows; on a
    /// flipped token return [`crate::CommandError::Cancelled`].
    pub cancel: &'a crate::CancellationToken,
    /// IN.0: one level of indentation, resolved by the host. Only the
    /// indent operators (`>` / `<`) read it.
    pub indent: lattice_core::IndentUnit,
}

/// An operator's evaluator returns the full `Effect` it produced. Most
/// operators return `Effect::Edits(...)`; `change` adds a mode transition
/// via `Effect::Many(...)`; `yank` returns `Effect::Yank { ... }`. The
/// dispatcher passes the result through unchanged -- composition is
/// expressed in the Effect, not via flags on the spec.
type OperatorFn =
    Arc<dyn Fn(&mut OperatorContext) -> GrammarResult<crate::effect::Effect> + Send + Sync>;

#[derive(Clone)]
pub struct OperatorSpec {
    pub repeatable: bool,
    pub apply: OperatorFn,
    /// Per-positional-argument metadata (DESIGN.md §B.1). Empty for
    /// operators without args (the common case).
    pub args_schema: Vec<ArgSpec>,
    /// Block-visual dispatch hint. `true` (the default for v1
    /// rectangle ops -- `d`, `y`, `c`) means a blockwise visual
    /// selection routes per-row: each row's column slice gets its
    /// own ProtoRange and `apply` runs once per row, with results
    /// merged into a single Blockwise yank + concatenated Edits.
    /// `false` (linewise-style ops -- `>`, `<`, `gU`, `gu`, `g~`)
    /// means a blockwise visual collapses to a single contiguous
    /// range covering anchor..head; `apply` runs once. This keeps
    /// the operator a single undo unit instead of N per-row units.
    pub blockwise_per_row: bool,
    /// When true (e.g. surround's `ys{motion}{char}`), the operator's
    /// keymap bindings append `ChordPattern::CharLiteral` after every
    /// motion path so a wrapping character is captured as `Args::Char`
    /// by the wildcard resolution. Default false.
    pub post_motion_char: bool,
}

impl std::fmt::Debug for OperatorSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OperatorSpec")
            .field("repeatable", &self.repeatable)
            .field("blockwise_per_row", &self.blockwise_per_row)
            .finish_non_exhaustive()
    }
}

/// N.1.4a (2026-06-10): resolves a tree-sitter scope at the cursor for
/// the structural text objects (`af` / `ac` / …). Defined here so
/// `lattice-grammar` stays free of any tree-sitter dependency — the
/// host implements it (backed by the buffer's `SyntaxSnapshot`) and
/// threads it into [`TextObjectContext`]. `scope_at` returns the
/// innermost matching node's byte-precise span as a half-open
/// `[start, end)` [`ProtoRange`] (N.1.4c: byte columns, not just rows,
/// so intra-line objects like `aa`/`ia` are charwise-accurate), or
/// `None` when there's no parse / no match. End is exclusive, matching
/// tree-sitter node ranges and the operator slice convention.
pub trait ScopeResolver {
    fn scope_at(&self, line: u32, col_byte: u32, suffix: &str) -> Option<ProtoRange>;

    /// The `count`-th node whose capture name ends with `suffix`, in `dir`,
    /// targeting the node's `boundary`. Respects the enclosing-object rule
    /// (see treesitter-motions.md §4.1): `(Forward, Start)` / `(Backward, End)`
    /// skip the object the cursor is inside; `(Backward, Start)` / `(Forward, End)`
    /// may land on the current object's own boundary. Returns the target
    /// position, or `None` (no tree / no match / fewer than `count` candidates).
    fn scope_toward(
        &self,
        line: u32,
        col_byte: u32,
        suffix: &str,
        dir: NavDir,
        boundary: NavBoundary,
        count: u32,
    ) -> Option<Position>;
}

/// Direction of travel for a structural motion. `Forward` scans toward EOF,
/// `Backward` toward BOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavDir {
    Forward,
    Backward,
}

/// Which boundary of the target node the motion lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavBoundary {
    Start,
    End,
}

/// N.1.6 (2026-06-10): per-buffer comment-leader descriptor for the
/// comment text objects (`aC` / `iC`). Commentstring-driven (NOT
/// tree-sitter) so it works for any language with a known leader, even
/// without a parse tree. The host populates it from the active buffer's
/// language (`Lang::comment_syntax`); `None` (or `line: None`) means the
/// comment objects resolve nothing (graceful operator no-op).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommentSyntax {
    /// Line-comment leader, e.g. `"//"` (rust / js) or `"#"` (python).
    pub line: Option<String>,
    /// Block-comment delimiters, e.g. `("/*", "*/")`. Reserved for a
    /// follow-up; v1 comment objects use `line` only.
    pub block: Option<(String, String)>,
}

/// The per-dispatch environment: everything the host knows that a
/// command's `apply` may need but the grammar layer cannot derive for
/// itself. Bundled into ONE value so the dispatch seam carries a single
/// env rather than a widening parameter list (the long-term-fit choice
/// over parallel params). `Copy`; `default()` is the no-input case, and
/// commands that read nothing from the env (`iw`, `ap`, `i{`) are
/// unaffected by what it carries.
///
/// It has widened twice, and the name followed on the second:
///
/// - **N.1.6 (2026-06-10)** introduced it as a text-object-only env —
///   the tree-sitter `scope_resolver` (`af`/`ac`, N.1.4) and the
///   `comment_syntax` (`aC`/`iC`).
/// - **TS.1** added the tree-sitter `syntax` snapshot for **actions**
///   (borrowed `Arc<dyn Any>` so `execute_action` can `Arc::clone` it
///   into the owned `ActionContext::syntax` — a `&dyn Any` alone can't
///   recover the `Arc`), which already made `TextObjectEnv` a misnomer.
/// - **IN.0** renamed it to `GrammarEnv` on adding a field read by
///   **operators** (`>` / `<`), which would have made the old name
///   actively misleading rather than merely stale.
#[derive(Clone, Copy, Default)]
pub struct GrammarEnv<'a> {
    pub scope_resolver: Option<&'a dyn ScopeResolver>,
    pub comment_syntax: Option<&'a CommentSyntax>,
    /// TS.1: the per-dispatch tree-sitter snapshot, type-erased and borrowed so
    /// it stays `Copy`. `execute_action` clones it into `ActionContext::syntax`;
    /// motions / text-objects / operators ignore it (they read
    /// `scope_resolver`). `None` = no parse.
    pub syntax: Option<&'a Arc<dyn std::any::Any + Send + Sync>>,
    /// IN.0: one level of indentation, resolved by the host from
    /// `shiftwidth` / `expandtab` / `tabstop` (including any
    /// `:setlocal` override). Read by the `>` / `<` operators.
    ///
    /// Not an `Option`: there is always a defensible answer, and
    /// `IndentUnit::default()` is the registered option defaults, so a
    /// caller that never resolved config indents like an unconfigured
    /// buffer instead of like nothing. That keeps the ~40 test and
    /// plugin call sites that build a `default()` env working without
    /// each having to care about indentation.
    pub indent: lattice_core::IndentUnit,
}

/// Context passed to a text-object's evaluator.
pub struct TextObjectContext<'a> {
    pub buffer: &'a Buffer,
    pub at: Position,
    pub count: Count,
    pub args: Args,
    /// Cooperative cancellation handle (DESIGN.md §5.2.5). Most
    /// text objects are O(line); polling rarely matters. Tag /
    /// paragraph / sentence objects that walk further can poll
    /// `cancel.check()?` on their inner loops.
    pub cancel: &'a crate::CancellationToken,
    /// N.1.4a: tree-sitter scope resolver for the structural text
    /// objects (`af` / `ac` / …), injected by the host (backed by the
    /// buffer's `SyntaxSnapshot`). `None` for buffers with no syntax —
    /// the structural objects then resolve nothing; the classic
    /// objects (`iw`, `ap`, `i{`) never read it.
    pub scope_resolver: Option<&'a dyn ScopeResolver>,
    /// N.1.6: comment-leader descriptor for the comment objects
    /// (`aC` / `iC`), injected by the host from the active buffer's
    /// language. `None` (or `line: None`) ⇒ the comment objects resolve
    /// nothing. Only `aC` / `iC` read it.
    pub comment_syntax: Option<&'a CommentSyntax>,
}

type TextObjectFn = Arc<dyn Fn(&TextObjectContext) -> GrammarResult<ProtoRange> + Send + Sync>;

#[derive(Clone)]
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
///
/// Owns a [`crate::CancellationToken`] (not a borrow) so apply
/// closures can hold it across `Box::new(move |ctx| ...)`
/// boundaries without lifetime gymnastics. The actor flips a clone
/// when cancellation arrives.
pub struct ExCommandContext {
    pub bang: bool,
    pub args: Args,
    pub range: Option<crate::range::Range>,
    pub register: Register,
    pub count: Count,
    pub cancel: crate::CancellationToken,
}

/// Parser callback for an ex-command. The host hands the rest of the
/// command line (everything after the command word and the optional `!`)
/// plus the `bang` bit; the callback returns typed [`Args`].
type ExParseFn = Arc<dyn Fn(&str, bool) -> GrammarResult<Args> + Send + Sync>;

/// Evaluator callback. Returns the [`Effect`] the host should commit.
type ExApplyFn =
    Arc<dyn Fn(&ExCommandContext) -> GrammarResult<crate::effect::Effect> + Send + Sync>;

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
// PL8.F: no longer `Copy` — the `hint` became `Cow<'static, str>` (a plugin
// delimiter command's hint crosses WIT as an owned string and must free on
// unregister). Consumers that bound it by value now bind by reference.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SurfaceForm {
    /// Type the command word, optionally with `!`, then args
    /// separated by whitespace. The default for most commands.
    #[default]
    Keyword,
    /// Type a delimiter prefix followed by a body. The keyword form
    /// errors with a redirect message; the embedded `hint` is the
    /// canonical syntax shown in that error (`:s/pat/repl/`,
    /// `:g/pat/body`).
    Delimiter {
        hint: std::borrow::Cow<'static, str>,
    },
}

#[derive(Clone)]
pub struct ExCommandSpec {
    /// Latency class declaration (DESIGN.md §5.2.5). Most ex-commands
    /// stay [`crate::command::LatencyClass::Reflex`] (the default) --
    /// they're cheap state mutations. File I/O (`:write`, `:edit`)
    /// and help-buffer builders (`:describe-*`, `:apropos`,
    /// `:keymap`) declare [`crate::command::LatencyClass::Display`]
    /// so future cancellation / deadline machinery treats them with
    /// the right budget.
    pub latency_class: crate::command::LatencyClass,
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

/// Doc string for an auto-generated `:<mode-name>` mode-toggle ex-command —
/// shared so native and plugin modes read identically in `:describe-command`.
pub const MODE_TOGGLE_COMMAND_DOC: &str = "Toggle the mode on the active buffer (auto-generated; see `:help modes` for \
     the full mode-system overview).";

/// Build the auto-generated `:<mode-name>` mode-toggle ex-command spec. The
/// `apply` returns [`crate::effect::Effect::ToggleMode`]; the host routes that
/// to `toggle_mode_by_name`, flipping the mode on the active buffer. Shared by
/// boot (native modes, `register_mode_toggle_commands`) AND the plugin
/// modes-seam drain, so a plugin-registered mode gets an IDENTICAL `:<mode>`
/// toggle surface. The caller registers it under the right provenance: `Builtin`
/// for native modes; `SourceLayer::Plugin(id)` for plugin modes, so unload
/// reverses it. Takes no arguments.
pub fn mode_toggle_ex_command_spec(mode_name: &str) -> ExCommandSpec {
    let mode = mode_name.to_string();
    ExCommandSpec {
        latency_class: crate::command::LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        parse_args: std::sync::Arc::new(|s: &str, _bang: bool| {
            if s.trim().is_empty() {
                Ok(crate::args::Args::None)
            } else {
                Err(crate::error::CommandError::BadArgs(
                    "mode toggle takes no arguments".into(),
                ))
            }
        }),
        apply: std::sync::Arc::new(move |_ctx| {
            Ok(crate::effect::Effect::ToggleMode {
                mode_name: mode.clone(),
            })
        }),
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    }
}

/// Context passed to a free-form action's evaluator. Mirrors
/// [`ExCommandContext`]'s shape (no document mutation; the App
/// applies the returned [`crate::effect::Effect`]) but omits the
/// `bang` bit -- chord-bound actions never carry one. The
/// `register` / `count` slots flow the count and register prefixes
/// the user typed before the chord (vim's `3"+yy`-style); most
/// actions ignore them.
pub struct ActionContext {
    pub args: Args,
    pub register: Register,
    pub count: Count,
    /// Where the caret sits when the action fires (AP.0.1) — the
    /// action's equivalent of `MotionContext::from`. Native actions
    /// ignore it; a plugin action pairs it with `buffer` (below) to
    /// read the text around the cursor.
    pub cursor: Position,
    /// The active buffer's id (AP.2) — the `target` a plugin action
    /// names in an `Effect::ApplyEdit`. Mirrors `MotionContext::buffer_id`.
    pub buffer_id: BufferId,
    /// A point-in-time view of the buffer the action fired in
    /// (AP.0.1). An owned `Buffer` — a `ropey::Rope` clone is O(1)
    /// (Arc-shared nodes), so carrying it costs nothing on the
    /// dispatch path and avoids a context lifetime. Native actions
    /// ignore it; the plugin-host trampoline mints a `document`
    /// resource from it so a grammar plugin can read buffer text.
    /// Layering: a `lattice-core` type, so `lattice-grammar` needs no
    /// `lattice-runtime` dependency (the snapshot is built host-side).
    pub buffer: Buffer,
    /// TS.1: a point-in-time tree-sitter snapshot for the buffer the action
    /// fired in, **type-erased** as `Arc<dyn Any>` so `lattice-grammar` keeps
    /// its `protocol`+`core`-only dep set (the same reason `buffer` is a
    /// `lattice-core` type — a concrete `SyntaxSnapshot` would drag the whole
    /// syntax stack under the lean grammar crate). The host upcasts the
    /// buffer's `Arc<SyntaxSnapshot>` here **at the same instant** it clones
    /// `buffer` (so tree + text versions agree); native actions ignore it; the
    /// plugin-host trampoline downcasts it to mint a `tree-snapshot` resource
    /// so a grammar plugin can query structure (auto-pair's `enclosing` scope).
    /// `None` when the buffer has no parse (plain text / parse pending).
    pub syntax: Option<Arc<dyn std::any::Any + Send + Sync>>,
    /// Cooperative cancellation handle (DESIGN.md §5.2.5). Most
    /// actions are O(1) state mutations and ignore this; long-
    /// running ones (a hypothetical "rebuild fold tree" action)
    /// poll `cancel.check()?` between iterations.
    pub cancel: crate::CancellationToken,
}

/// Evaluator callback for a free-form action. Returns the
/// [`crate::effect::Effect`] the host should apply -- typically
/// `Effect::AppAction(AppEffect::Foo)` for a chord-bound action,
/// occasionally a richer `Effect::Many([...])` if the action also
/// emits an edit / mode transition / yank.
type ActionFn = Arc<dyn Fn(&ActionContext) -> GrammarResult<crate::effect::Effect> + Send + Sync>;

#[derive(Clone)]
pub struct ActionSpec {
    pub apply: ActionFn,
    /// Per-positional-argument metadata (DESIGN.md §B.1). Empty
    /// for actions without args (the common case for chord
    /// bindings -- the args slot is reserved for future
    /// captured-char / numeric-param variants in slice 8.i.2-3).
    pub args_schema: Vec<ArgSpec>,
}

impl std::fmt::Debug for ActionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionSpec").finish_non_exhaustive()
    }
}

/// What a registered command holds in the registry, beyond its metadata.
#[derive(Clone)]
pub enum CommandRegistration {
    Motion(MotionSpec),
    Operator(OperatorSpec),
    TextObject(TextObjectSpec),
    ExCommand(ExCommandSpec),
    /// Free-form App-side action. The dispatcher's
    /// `CommandKind::Action` branch invokes the spec's `apply`
    /// closure and returns its [`crate::effect::Effect`] -- almost
    /// always an [`crate::effect::Effect::AppAction`] carrying a
    /// typed [`crate::app_effect::AppEffect`]. Wired in slice 8.i.0;
    /// populated from the per-mode keymap modules during 8.i.1-3 as
    /// the legacy `Action` bridge retires. See
    /// `docs/dev/notes/8i-approach.md`.
    Action(ActionSpec),
}

impl CommandRegistration {
    pub fn kind(&self) -> CommandKind {
        match self {
            CommandRegistration::Motion(_) => CommandKind::Motion,
            CommandRegistration::Operator(_) => CommandKind::Operator,
            CommandRegistration::TextObject(_) => CommandKind::TextObject,
            CommandRegistration::ExCommand(_) => CommandKind::ExCommand,
            CommandRegistration::Action(_) => CommandKind::Action,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct CommandRegistry {
    by_id: HashMap<CommandId, CommandEntry>,
    by_name: HashMap<String, CommandId>,
    /// Motions in the vim "word-forward" class (`w` / `W`). Under an
    /// operator these get the word-motion special case: `dw` / `cw` /
    /// `yw` on the last word of a line stop at the line end rather than
    /// reaching over the newline into the next line's first word
    /// (`:help word-motions`). Populated by `builtins::populate` via
    /// [`Self::tag_word_forward_motion`]; read by the operator range
    /// resolver.
    word_forward_motions: std::collections::HashSet<CommandId>,
    /// Monotonic mutation counter, bumped on every `insert` and every
    /// non-empty `unregister_plugin`. A cheap version stamp for cache
    /// invalidation: the `:`-command-name completion generator keys its cache on
    /// this (via [`Self::generation`]) so a plugin's *runtime* command
    /// registration or unload — which RCUs a fresh registry with a bumped
    /// counter — shows up in `<Tab>` completion without any manual cache flush.
    /// Clones with the registry (RCU snapshot), so the stored copy carries the
    /// post-mutation value the dispatcher + completion then read.
    generation: u64,
}

#[derive(Clone)]
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

    /// Tag a motion as "word-forward class" (vim `w` / `W`) so the
    /// operator range resolver applies the word-motion line-stop special
    /// case to it. Called by `builtins::populate` right after the two
    /// word-forward motions are registered. Idempotent.
    pub fn tag_word_forward_motion(&mut self, id: MotionId) {
        self.word_forward_motions.insert(id.0);
    }

    /// Whether `id` is a word-forward-class motion (see
    /// [`Self::tag_word_forward_motion`]).
    pub fn is_word_forward_motion(&self, id: CommandId) -> bool {
        self.word_forward_motions.contains(&id)
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
                latency_class: crate::command::LatencyClass::Reflex,
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
                latency_class: crate::command::LatencyClass::Reflex,
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
                latency_class: crate::command::LatencyClass::Reflex,
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
                latency_class: spec.latency_class,
            },
            registration: CommandRegistration::ExCommand(spec),
        });
        ExCommandId(id)
    }

    /// Register a free-form action (DESIGN.md §5.2.1; see
    /// `docs/dev/notes/8i-approach.md`). Used by chord bindings whose
    /// historical `Action` enum payload had no grammar concept
    /// attached. The spec's `apply` returns an
    /// [`crate::effect::Effect`] -- typically
    /// `Effect::AppAction(AppEffect::...)`.
    #[track_caller]
    pub fn register_action(&mut self, name: &str, doc: &str, spec: ActionSpec) -> CommandId {
        let source = capture_builtin_source();
        self.insert_action(name, doc, spec, source)
    }

    pub(crate) fn insert_action(
        &mut self,
        name: &str,
        doc: &str,
        spec: ActionSpec,
        source: SourceLocation,
    ) -> CommandId {
        let id = next_command_id();
        let args_schema = spec.args_schema.clone();
        self.insert(CommandEntry {
            spec: CommandSpec {
                id,
                name: name.to_string(),
                kind: CommandKind::Action,
                doc: doc.to_string(),
                args_schema,
                source,
                latency_class: crate::command::LatencyClass::Reflex,
            },
            registration: CommandRegistration::Action(spec),
        });
        id
    }

    // ---- Plugin-contribution registration (PH7.7c, §6) ----
    // The forgery-safe cross-crate seam for a WASM plugin's grammar
    // contributions. Each takes the **host-issued** `plugin_id` (a `u32`, never a
    // `SourceLocation`) and always stamps `SourceLayer::Plugin(plugin_id)` via
    // [`SourceLocation::plugin`] — so the "no public fn takes a `SourceLocation`"
    // forgery invariant (`source.rs`) holds, and a plugin cannot forge builtin /
    // user provenance. The `spec.apply` / `parse_args` are the plugin host's sync
    // trampoline closures into the guest (`lattice-plugin-host::grammar_trampoline`);
    // the registry, dispatcher, and every `:describe-*` view treat the entry
    // exactly like a builtin (paramount #3 — a plugin motion is first-class
    // grammar). These are the public counterpart to the `pub(crate) insert_*`
    // path builtins use through `register_*`.

    /// Register a plugin-contributed motion under `SourceLayer::Plugin(plugin_id)`.
    pub fn register_plugin_motion(
        &mut self,
        plugin_id: u32,
        name: &str,
        doc: &str,
        spec: MotionSpec,
    ) -> MotionId {
        self.insert_motion(name, doc, spec, SourceLocation::plugin(plugin_id))
    }

    /// Register a plugin-contributed operator under `SourceLayer::Plugin(plugin_id)`.
    pub fn register_plugin_operator(
        &mut self,
        plugin_id: u32,
        name: &str,
        doc: &str,
        spec: OperatorSpec,
    ) -> OperatorId {
        self.insert_operator(name, doc, spec, SourceLocation::plugin(plugin_id))
    }

    /// Register a plugin-contributed text object under `SourceLayer::Plugin(plugin_id)`.
    pub fn register_plugin_text_object(
        &mut self,
        plugin_id: u32,
        name: &str,
        doc: &str,
        spec: TextObjectSpec,
    ) -> TextObjectId {
        self.insert_text_object(name, doc, spec, SourceLocation::plugin(plugin_id))
    }

    /// Register a plugin-contributed ex-command under `SourceLayer::Plugin(plugin_id)`.
    pub fn register_plugin_ex_command(
        &mut self,
        plugin_id: u32,
        name: &str,
        doc: &str,
        spec: ExCommandSpec,
    ) -> ExCommandId {
        self.insert_ex_command(name, doc, spec, SourceLocation::plugin(plugin_id))
    }

    /// Register a plugin-contributed action under `SourceLayer::Plugin(plugin_id)`.
    pub fn register_plugin_action(
        &mut self,
        plugin_id: u32,
        name: &str,
        doc: &str,
        spec: ActionSpec,
    ) -> CommandId {
        self.insert_action(name, doc, spec, SourceLocation::plugin(plugin_id))
    }

    /// Remove every command a plugin contributed, keyed by its host-issued
    /// `plugin_id` (the `u32` inside `SourceLayer::Plugin`). The teardown seam
    /// for a plugin reload / unload (PH7.12b): the registry is otherwise
    /// append-only, so without this a reload would re-register on top of the
    /// old entries and the `by_id`/`by_name` maps would grow unbounded across
    /// reloads (audit F6). Provenance-driven — only `Plugin(plugin_id)` entries
    /// go; built-in / config / runtime commands are never touched, mirroring
    /// the forgery invariant (a caller supplies only a `u32`, never a
    /// `SourceLayer`). Returns the number of commands removed (0 if the plugin
    /// contributed none — an idempotent no-op on a second unload). Every
    /// index (`by_id`, `by_name`, the word-forward tag set) is kept consistent.
    pub fn unregister_plugin(&mut self, plugin_id: u32) -> usize {
        let doomed: Vec<CommandId> = self
            .by_id
            .iter()
            .filter(|(_, entry)| entry.spec.source.layer == SourceLayer::Plugin(plugin_id))
            .map(|(id, _)| *id)
            .collect();
        for id in &doomed {
            if let Some(entry) = self.by_id.remove(id) {
                self.by_name.remove(&entry.spec.name);
            }
            self.word_forward_motions.remove(id);
        }
        if !doomed.is_empty() {
            self.generation = self.generation.wrapping_add(1);
        }
        doomed.len()
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
        self.generation = self.generation.wrapping_add(1);
    }

    /// Monotonic mutation counter (see the `generation` field). Read by the
    /// completion layer's command-name generator to key its cache: a change
    /// means the command set moved (a plugin loaded or unloaded), so the cached
    /// candidate list must be regenerated rather than served stale.
    pub fn generation(&self) -> u64 {
        self.generation
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

pub(crate) fn require_action(entry: &CommandEntry) -> GrammarResult<&ActionSpec> {
    match &entry.registration {
        CommandRegistration::Action(s) => Ok(s),
        other => Err(CommandError::KindMismatch {
            expected: "action",
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
            apply: Arc::new(|ctx| {
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
    fn unregister_plugin_removes_only_that_plugins_commands() {
        let mut r = CommandRegistry::new();
        // A built-in and two plugins, one of which registers a word-forward motion.
        r.register_motion("builtin:w", "builtin", dummy_motion());
        let p7 = r.register_plugin_motion(7, "p7:down", "", dummy_motion());
        r.tag_word_forward_motion(p7);
        r.register_plugin_motion(7, "p7:up", "", dummy_motion());
        r.register_plugin_motion(9, "p9:left", "", dummy_motion());
        assert_eq!(r.len(), 4);

        // Unregister plugin 7: both its commands go, the built-in and plugin 9 stay.
        let removed = r.unregister_plugin(7);
        assert_eq!(removed, 2);
        assert_eq!(r.len(), 2);
        assert!(r.lookup_by_name("p7:down").is_none());
        assert!(r.lookup_by_name("p7:up").is_none());
        assert!(r.lookup_by_name("builtin:w").is_some());
        assert!(r.lookup_by_name("p9:left").is_some());
        // The word-forward tag for the removed motion is gone too.
        assert!(!r.is_word_forward_motion(p7.0));

        // Idempotent: a second unload of the same plugin removes nothing.
        assert_eq!(r.unregister_plugin(7), 0);
    }

    #[test]
    fn generation_bumps_on_register_and_nonempty_unregister() {
        // The completion layer keys its `:`-command cache on this counter, so it
        // MUST move whenever the command set changes (a plugin drain / unload) —
        // otherwise plugin commands never appear in `<Tab>` completion.
        let mut r = CommandRegistry::new();
        let g0 = r.generation();
        r.register_motion("builtin:w", "builtin", dummy_motion());
        let g1 = r.generation();
        assert!(g1 > g0, "registering a command must bump generation");
        r.register_plugin_motion(7, "p7:down", "", dummy_motion());
        let g2 = r.generation();
        assert!(g2 > g1, "a plugin registration must bump generation");
        assert_eq!(r.unregister_plugin(7), 1);
        let g3 = r.generation();
        assert!(
            g3 > g2,
            "unregistering a plugin's commands must bump generation"
        );
        // A no-op unregister (nothing removed) must NOT bump — no cache churn.
        assert_eq!(r.unregister_plugin(7), 0);
        assert_eq!(
            r.generation(),
            g3,
            "an empty unregister must not bump generation"
        );
    }

    #[test]
    fn mode_toggle_spec_toggles_named_mode_and_rejects_args() {
        let spec = mode_toggle_ex_command_spec("auto-pair-mode");
        // No args → Ok(None); any args → BadArgs.
        assert!(matches!(
            (spec.parse_args)("", false),
            Ok(crate::args::Args::None)
        ));
        assert!((spec.parse_args)("nope", false).is_err());
        // apply → ToggleMode for the named mode (the spec ignores ctx).
        let ctx = ExCommandContext {
            bang: false,
            args: crate::args::Args::None,
            range: None,
            register: Register::default(),
            count: Count::default(),
            cancel: crate::CancellationToken::new(),
        };
        match (spec.apply)(&ctx) {
            Ok(crate::effect::Effect::ToggleMode { mode_name }) => {
                assert_eq!(mode_name, "auto-pair-mode");
            }
            other => panic!("expected ToggleMode, got {other:?}"),
        }
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
            SourceKind::File {
                path,
                line: Some(line),
            } => {
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
                apply: Arc::new(|_| Ok(crate::effect::Effect::None)),
                args_schema: vec![],
                blockwise_per_row: false,
                post_motion_char: false,
            },
        );
        let spec = r.lookup(id.0).unwrap();
        if let SourceKind::File {
            line: Some(line), ..
        } = &spec.source.kind
        {
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
                apply: Arc::new(|_| Ok(ProtoRange::new(Position::ZERO, Position::ZERO))),
                args_schema: vec![],
            },
        );
        let spec = r.lookup(id.0).unwrap();
        if let SourceKind::File {
            line: Some(line), ..
        } = &spec.source.kind
        {
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
        assert!(
            line_b > line_a,
            "second call's line should follow the first"
        );
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
