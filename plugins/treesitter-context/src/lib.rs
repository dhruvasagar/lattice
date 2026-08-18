//! `treesitter-context` — sticky scope headers, as a bundled plugin.
//!
//! Three seams from one component:
//!
//!   - **context** — the scope producer. Runs a per-language `@context` query
//!     against the call-scoped tree snapshot the host hands it and returns
//!     structural scopes. Never resolves *which* scopes a pane shows: that is
//!     a function of the cursor and viewport, and the host does it natively so
//!     no WASM call sits on the scroll path.
//!   - **config** — the ten `context.*` options.
//!   NOT theme: the strip is host chrome, styled by the host's
//!   `sticky.context.*` elements. The plugin owns the scopes, not their look.
//!
//! ## Why the header span comes from a `@context.end` capture
//!
//! A scope's header is everything before its body: `fn f(\n  a: u32,\n) {` is
//! three lines of header, not one. The body's position therefore has to cross
//! the boundary somehow.
//!
//! It was originally read guest-side via `node.child_by_field("body")`, which
//! needs a NODE — and every node the host hands back is a resource-table entry
//! with its own snapshot bump and its own drop call. `run-query` minted one per
//! capture, so a whole-file structural query paid tens of thousands of boundary
//! round trips and went superlinear; that cost, not the query, is what forced
//! the `max-file-lines` guard down to 5 000 lines.
//!
//! TC.10 replaced it with `run-query-ranges` (extents, no resources) plus a
//! `@context.end` capture in each query, paired to its `@context` by match
//! index. The header derivation is unchanged; only where the body position
//! comes from changed. The cost of the per-language bookkeeping this
//! reintroduces — a field name to get right per construct — is paid by
//! `treesitter_context_queries.rs`, which compiles every bundled query against
//! its real grammar.

wit_bindgen::generate!({
    world: "treesitter-context-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::context::Guest as ContextGuest;
use exports::lattice::plugin_host::grammar_callbacks::Guest as CallbacksGuest;
use lattice::plugin_host::grammar::{register_action, register_ex_command};
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
    register_mode,
};
use lattice::plugin_host::config::{OptionType, get_option, register_option, set_option};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, Args, ContextRequest, ContextScope, Effect, ExCommandContext,
    ExCommandSpec, LatencyClass, Position, SurfaceForm,
};

struct Component;

// ── Queries ──────────────────────────────────────────────────────────────────

/// The `@context` query for a grammar id, or `None` when the language has none.
///
/// A missing query is a NORMAL state, not a defect: most languages will not
/// have one, and the honest response is an empty scope set (no strip) rather
/// than an error that would blank a strip the user was reading.
fn query_for(language: &str) -> Option<&'static str> {
    Some(match language {
        "rust" => include_str!("../queries/rust.scm"),
        "python" => include_str!("../queries/python.scm"),
        "go" => include_str!("../queries/go.scm"),
        "javascript" => include_str!("../queries/javascript.scm"),
        "typescript" | "tsx" => include_str!("../queries/typescript.scm"),
        "c" | "cpp" => include_str!("../queries/c.scm"),
        "markdown" => include_str!("../queries/markdown.scm"),
        _ => return None,
    })
}

/// Build a scope from one pattern match's captures.
///
/// `scope` is the `@context` extent (the construct's own line span);
/// `body_start` is the line the match's `@context.end` begins on, absent for
/// constructs whose header IS their first line (`} else {`, a `case` label).
fn scope_from(scope: (u32, u32), body_start: Option<u32>) -> ContextScope {
    let (scope_start, scope_end) = scope;
    let header_end = body_start
        .unwrap_or(scope_start)
        // A body starting before the construct does is impossible from a real
        // tree, but clamping costs nothing and keeps a malformed grammar from
        // producing an inverted span the host would have to defend against.
        .max(scope_start);
    ContextScope {
        scope_start,
        scope_end,
        header_start: scope_start,
        header_end,
    }
}

fn scopes_from_tree(tree: &TreeSnapshot) -> Result<Vec<ContextScope>, String> {
    let language = tree.language();
    let Some(source) = query_for(&language) else {
        // No query for this grammar. Not an error — the strip simply has
        // nothing to show, and the host caches that as "no scopes".
        return Ok(Vec::new());
    };
    // Compiled per call rather than cached: the guest has no per-language
    // cache slot that survives a call, and this runs once per REPARSE (not per
    // keystroke, scroll, or frame), so the cost sits far off every hot path.
    // A cache would be the right move only if the producer were re-driven more
    // often, and the whole scopes-not-rows split exists to ensure it is not.
    let query = tree.compile_query(source)?;
    // `run_query_ranges`, not `run_query`: this is a WHOLE-FILE structural
    // query, and the node-returning form pays a resource handle per capture.
    // See the module doc — that difference is the file-size ceiling.
    let captures = tree.run_query_ranges(&query, None);
    let mut scopes: Vec<ContextScope> = Vec::new();
    // Captures arrive grouped by match (the host pushes each match's captures
    // together and stamps them with one index), so one linear scan pairs each
    // `@context` with its `@context.end` — no containment test, which would be
    // ambiguous for a construct nested directly inside another.
    let mut i = 0;
    while i < captures.len() {
        let match_index = captures[i].match_index;
        let mut extent: Option<(u32, u32)> = None;
        let mut body_start: Option<u32> = None;
        while i < captures.len() && captures[i].match_index == match_index {
            let c = &captures[i];
            match c.name.as_str() {
                "context" => extent = Some((c.range.start.line, c.range.end.line)),
                "context.end" => body_start = Some(c.range.start.line),
                // A query may carry captures for its own predicates; anything
                // unrecognised is ignored rather than treated as a scope.
                _ => {}
            }
            i += 1;
        }
        if let Some(extent) = extent {
            scopes.push(scope_from(extent, body_start));
        }
    }
    // A scope spanning a single line can never be a context: its header cannot
    // scroll away while the cursor is still inside it. Dropping them here keeps
    // the host's cache (and the resolver's scan) free of entries that can never
    // resolve to anything.
    scopes.retain(|s| s.scope_end > s.scope_start);
    Ok(scopes)
}

/// Fallback when `max-file-lines` cannot be read (no config wired).
///
/// Measured cost of the whole-buffer query, Rust, release wasm, before and
/// after TC.10 moved the producer onto `run-query-ranges`:
///
/// |   lines | `run-query` | `run-query-ranges` |
/// |--------:|------------:|-------------------:|
/// |   1 000 |       25 ms |             4.6 ms |
/// |   2 500 |       83 ms |             6.0 ms |
/// |   5 000 |      287 ms |             9.0 ms |
/// |  10 000 |      1.18 s |              15 ms |
/// |  20 000 |        TRAP |              28 ms |
/// | 100 000 |           — |             135 ms |
/// | 400 000 |           — |             534 ms |
///
/// The old shape was superlinear — roughly 4x the time for 2x the lines —
/// because `run-query` mints one node RESOURCE per capture, and a structural
/// query over a large file has tens of thousands. Past ~20k lines it exceeded
/// the producer's epoch deadline and TRAPPED, which quarantines the plugin:
/// after one oversized file it stopped producing for every buffer until
/// reload. That cliff is what the guard existed to keep users away from.
///
/// The ranges form is linear (~1.4 us/line) and measured to 400k lines without
/// trapping, so the guard no longer protects against a cliff — it just bounds
/// how long a background task may run per reparse. 100k lines is far above any
/// realistic source file (this repo's largest is 36k) while keeping the worst
/// case near a tenth of a second; `0` disables it, which is now a defensible
/// setting rather than a foot-gun.
const DEFAULT_MAX_FILE_LINES: u32 = 100_000;

impl ContextGuest for Component {
    fn context_scopes(
        req: ContextRequest,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<ContextScope>, String> {
        if req.line_count == 0 {
            return Ok(Vec::new());
        }
        // Bound the work BEFORE running the query. Returning empty (not `err`)
        // is deliberate: the host caches an empty set, which is the truth —
        // this file has no context — rather than keeping a stale set from
        // whatever was open before.
        let max_lines = get_option("max-file-lines")
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(DEFAULT_MAX_FILE_LINES);
        if max_lines > 0 && req.line_count > max_lines {
            return Ok(Vec::new());
        }
        // No parse (plain text, or one still pending) is a normal state the
        // host caches as "no scopes" — never an error, which would make it keep
        // the previous buffer's structure.
        let Some(tree) = tree else {
            return Ok(Vec::new());
        };
        scopes_from_tree(tree)
    }
}


// ── The jump: `[u` / `:context-up` ───────────────────────────────────────────

/// Callback id for the `context-up` action. The guest picks these; the host
/// hands the id back on dispatch (the trampoline pattern).
const CB_CONTEXT_UP: u32 = 1;
/// Callback ids for `:context-toggle` (arg parse + apply).
const CB_PARSE_NOARGS: u32 = 2;
const CB_EX_CONTEXT_TOGGLE: u32 = 4;

/// Walk `count` levels up the context stack from `line`, returning the header
/// line to land on.
///
/// The predicate is "innermost scope containing `line` whose header is STRICTLY
/// above it". That strictness is what makes repeated `[u` terminate rather than
/// stick: landing on a scope's header leaves the cursor inside that scope, but
/// its header is no longer above the cursor, so the next press finds the parent.
/// It is also why there is no `]u` — the inverse of walking up is `<C-o>`.
fn context_up_target(scopes: &[ContextScope], line: u32, count: u32) -> Option<u32> {
    let mut at = line;
    let mut landed = None;
    for _ in 0..count.max(1) {
        let next = scopes
            .iter()
            .filter(|s| s.scope_start <= at && at <= s.scope_end && s.header_end < at)
            .max_by_key(|s| s.header_start)?;
        at = next.header_start;
        landed = Some(at);
    }
    landed
}

fn jump_effects(tree: Option<&TreeSnapshot>, cursor: Position, count: u32) -> Vec<Effect> {
    let Some(tree) = tree else {
        return vec![Effect::None];
    };
    let Ok(scopes) = scopes_from_tree(tree) else {
        return vec![Effect::None];
    };
    match context_up_target(&scopes, cursor.line, count) {
        // `record-jump` FIRST: the position ring must capture where the cursor
        // was before the move, which is what makes `<C-o>` walk back.
        Some(line) => vec![
            Effect::RecordJump,
            Effect::CursorMove(Position { line, byte: 0 }),
        ],
        // Nothing enclosing with a header above — already at top level. A
        // no-op that CONSUMES the chord rather than `declined`: falling
        // through to another binding would be surprising, since the user did
        // ask for this action and it simply had nowhere to go.
        None => vec![Effect::None],
    }
}

// ── Config ───────────────────────────────────────────────────────────────────

/// Names are registered SHORT and the host namespaces them by plugin id, so
/// `max-lines` becomes `treesitter-context.max-lines`.

// ── Registration: the action, the ex-commands, the mode ──────────────────────

impl Guest for Component {
    fn register_grammar() {
        register_action(
            "context-up",
            "Jump to the header of the enclosing scope. Repeat to walk further \
             out; `<C-o>` returns.",
            &ActionSpec {
                args_schema: Vec::new(),
            },
            CB_CONTEXT_UP,
        );
        // NO `:context-up` ex-command. The design called for one sharing the
        // chord's handler, and the seam cannot deliver it: `apply-ex-command`
        // receives no `borrow<tree-snapshot>` (only `apply-action` does), so an
        // ex-command cannot compute a jump target, and no `Effect` re-dispatches
        // a command to borrow the action's tree. Shipping a `:context-up` that
        // silently does nothing would be worse than not having it; the chord is
        // the real surface. If a second consumer ever needs structure from an
        // ex-command, the fix is a tree parameter on `apply-ex-command` — a
        // seam change worth making for two consumers and not for one.
        //
        // Dashed + namespaced per the naming rule, and no one- or two-letter
        // short: those slots are scarce and reserved for vim-canonical commands.
        register_ex_command(
            "context-toggle",
            "Toggle the sticky context strip for this buffer.",
            &ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                args_schema: Vec::new(),
                surface_form: SurfaceForm::Keyword,
            },
            CB_PARSE_NOARGS,
            CB_EX_CONTEXT_TOGGLE,
        );
    }

    fn register_modes() {
        // A MINOR mode, activated on document buffers. `[u` is not universal
        // vim grammar — binding it at the builtin layer would fire it in every
        // buffer including ones with no tree — so it lives at
        // `KeymapLayer::MinorMode(treesitter-context-mode)`, which the host
        // scopes to buffers where this mode is active.
        register_mode(&ModeDeclaration {
            id: "treesitter-context-mode".to_string(),
            kind: ModeKind::Minor,
            // `global` = document buffers. A listing, help or terminal buffer
            // has no tree and no use for the chord.
            activation_policy: ActivationPolicy::Global,
            // NO capability requirement, deliberately.
            //
            // Declaring `TREE_SITTER` here made the mode fail to activate on
            // every buffer, including Rust files: the mode-capability gate's
            // enforcement half exists, but nothing ever POPULATES a buffer's
            // capability set — every activation site passes
            // `CapabilitySet::empty()`, and no native mode had declared a
            // requirement before, so the gate had never been exercised.
            //
            // It would be the wrong requirement even if the buffer side were
            // built. A buffer gains its tree when the first parse lands, so
            // gating on it would make `[u` unavailable until then and need a
            // re-activation afterwards — activation churn for a chord that
            // already no-ops gracefully with no tree. The capability that
            // genuinely matters is the MANIFEST's `editor_capabilities =
            // ["tree-sitter"]`, which gates the tree handle itself; without
            // the grant the guest gets `none` and returns no scopes.
            capabilities: ModeCapabilities::empty(),
            keymap: vec![ModeKeymapBinding {
                binding_mode: BindingMode::Normal,
                chord: "[u".to_string(),
                // Binds to this plugin's OWN action, which is why `grammar`
                // precedes `modes` in the manifest's `provides`.
                command: "context-up".to_string(),
            }],
        });
    }

    fn register_options() {
        let opts: &[(&str, OptionType, &str, &str)] = &[
            (
                "anchor",
                OptionType::String,
                "cursor",
                "Which line drives the context: `cursor` (where you are) or \
                 `topline` (what you are looking at).",
            ),
            (
                "max-lines",
                OptionType::Integer,
                "0",
                "Maximum context rows; 0 (the default) is unlimited. Counts \
                 ROWS, so a wrapped signature spends more than one. The strip \
                 is bounded by `max-viewport-fraction` regardless — that bound \
                 scales with the pane where a row count cannot.",
            ),
            (
                "trim-scope",
                OptionType::String,
                "outer",
                "Which end to drop when over budget: `outer` keeps the scopes \
                 you are innermost in, `inner` keeps the outermost.",
            ),
            (
                "multiline-threshold",
                OptionType::Integer,
                "1",
                "Maximum rows one scope's header may use. Raise it to see a \
                 whole wrapped signature.",
            ),
            (
                "max-viewport-fraction",
                OptionType::Integer,
                "33",
                "Percent of the pane the whole sticky strip may occupy, \
                 headerline included.",
            ),
            (
                "separator",
                OptionType::String,
                "",
                "Glyph repeated as a rule under the context block. Empty \
                 disables it.",
            ),
            (
                "line-numbers",
                OptionType::Boolean,
                "true",
                "Show each context row's source line number in the gutter.",
            ),
            (
                "disabled-languages",
                OptionType::String,
                "",
                "Comma-separated grammar ids to skip (e.g. `markdown,yaml`).",
            ),
            (
                "max-file-lines",
                OptionType::Integer,
                "100000",
                "Skip the structural query above this line count; 0 disables \
                 the guard. The query is linear (~1.4 us/line: ~9 ms at 5k \
                 lines, ~135 ms at 100k) and runs off-thread once per reparse, \
                 so this bounds background work rather than protecting against \
                 a stall.",
            ),
        ];
        for (name, ty, default, doc) in opts {
            register_option(name, *ty, default, doc);
        }
    }

}


impl CallbacksGuest for Component {
    fn apply_action(
        callback: u32,
        ctx: ActionContext,
        _doc: &lattice::plugin_host::buffer::Document,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            CB_CONTEXT_UP => Ok(jump_effects(tree, ctx.cursor, ctx.count)),
            other => Err(format!("unknown action callback {other}")),
        }
    }

    fn apply_ex_command(
        callback: u32,
        ctx: ExCommandContext,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            // Flip the loader-registered enablement switch. This one needs no
            // tree — it only reads and writes an option — which is exactly why
            // it survives where `:context-up` could not.
            CB_EX_CONTEXT_TOGGLE => {
                let _ = ctx;
                let on = get_option("enabled").map(|v| v == "true").unwrap_or(true);
                set_option("enabled", if on { "false" } else { "true" });
                Ok(vec![Effect::None])
            }
            other => Err(format!("unknown ex-command callback {other}")),
        }
    }

    fn parse_ex_args(_callback: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Ok(Args::None)
    }

    fn apply_motion(
        _callback: u32,
        _ctx: lattice::plugin_host::types::MotionContext,
    ) -> Result<lattice::plugin_host::types::MotionResult, String> {
        Err("treesitter-context contributes no motions".to_string())
    }

    fn apply_operator(
        _callback: u32,
        _ctx: lattice::plugin_host::types::OperatorContext,
    ) -> Result<Vec<Effect>, String> {
        Err("treesitter-context contributes no operators".to_string())
    }

    fn apply_text_object(
        _callback: u32,
        _ctx: lattice::plugin_host::types::TextObjectContext,
    ) -> Result<lattice::plugin_host::types::Range, String> {
        Err("treesitter-context contributes no text objects".to_string())
    }
}

export!(Component);
