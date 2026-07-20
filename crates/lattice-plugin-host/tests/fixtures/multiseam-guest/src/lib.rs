//! AP.1 spike guest — ONE `wasm32-wasip2` component providing three seams at
//! once (the `auto-pair` shape), built against the combined `multiseam-fixture`
//! world:
//!   - **grammar** — a `multiseam-read` action that reads the char at the cursor
//!     through the AP.0.1 `borrow<document>` handle (the SYNC seam),
//!   - **modes** — an `multiseam-mode` minor mode owning an insert-mode keymap
//!     binding to the plugin's own action (the async modes seam),
//!   - **config** — a `multiseam.style` option (the async config seam).
//!
//! The host instantiates this SAME artifact once per seam through each per-seam
//! world's bindings (grammar sync / modes+config async); the test asserts all
//! three register from the one component — proving a multi-seam plugin loads.

wit_bindgen::generate!({
    world: "multiseam-fixture",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::config::OptionType;
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, Args, EchoLevel, EchoPayload, Effect, ExCommandContext,
    MotionContext, MotionResult, OperatorContext, Position, Range, TextObjectContext,
};
use lattice::plugin_host::{config, grammar, modes};

struct Component;

impl Guest for Component {
    /// grammar seam — the action the mode's keymap binds to, plus (AP.0.2) a
    /// `multiseam-declines` action that returns `Effect::Declined` to exercise
    /// fall-through.
    fn register_grammar() {
        let spec = || ActionSpec {
            args_schema: Vec::new(),
        };
        grammar::register_action("multiseam-read", "echo the char at the cursor", &spec(), 1);
        grammar::register_action("multiseam-declines", "always declines (AP.0.2)", &spec(), 2);
        // TS.1: echo the enclosing `block` scope through the tree-snapshot handle.
        grammar::register_action(
            "multiseam-enclosing",
            "echo the enclosing block via the tree-sitter seam (TS.1)",
            &spec(),
            3,
        );
        // TS.2: run a query, and walk with a cursor.
        grammar::register_action(
            "multiseam-query",
            "compile+run a query via the tree-sitter seam (TS.2)",
            &spec(),
            4,
        );
        grammar::register_action(
            "multiseam-cursor",
            "walk the tree with a cursor via the tree-sitter seam (TS.2)",
            &spec(),
            5,
        );
    }

    /// modes seam — a minor mode binding `x` (Normal) to the declining action, so
    /// a test can prove the chord falls through to the builtin `x` (delete char).
    fn register_modes() {
        modes::register_mode(&ModeDeclaration {
            id: "multiseam-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Global,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![ModeKeymapBinding {
                binding_mode: BindingMode::Normal,
                chord: "x".to_string(),
                command: "multiseam-declines".to_string(),
            }],
        });
    }

    /// config seam — register a typed option.
    fn register_options() {
        config::register_option(
            "multiseam.style",
            OptionType::String,
            "auto",
            "spike option proving the config seam co-registers from one component",
        );
    }
}

impl GrammarCallbacks for Component {
    fn apply_action(
        callback: u32,
        ctx: ActionContext,
        doc: &Document,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            1 => {
                let start = ctx.cursor;
                let end = Position {
                    line: start.line,
                    byte: start.byte + 1,
                };
                let text = doc.get_text_range(Range { start, end })?;
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text,
                })])
            }
            // AP.0.2: decline the chord — the dispatcher falls through.
            2 => Ok(vec![Effect::Declined]),
            // TS.1: resolve the enclosing `block` scope through the tree-snapshot
            // handle and echo `<language>:<kind>:<named-child-count>` — proof the
            // seam crossed (handle received, `enclosing` walked host-side, node
            // projection returned). `err` when there's no tree (no grant / no
            // parse) or no enclosing block (graceful degradation).
            3 => {
                let tree = tree.ok_or("multiseam: no tree snapshot")?;
                let node = tree
                    .enclosing(ctx.cursor, &["block".to_string()])
                    .ok_or("multiseam: no enclosing block")?;
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text: format!(
                        "{}:{}:{}",
                        tree.language(),
                        node.kind(),
                        node.named_child_count()
                    ),
                })])
            }
            // TS.2: compile + run a query through the seam; echo
            // `<count>:<first-capture-name>:<first-node-kind>` — proof the
            // compiled query crossed, ran host-side (predicates included), and the
            // capture nodes came back.
            4 => {
                let tree = tree.ok_or("multiseam: no tree snapshot")?;
                let query = tree.compile_query("(function_item name: (identifier) @fname)")?;
                let caps = tree.run_query(&query, None);
                let first = caps
                    .first()
                    .map(|c| format!("{}:{}", c.name, c.node.kind()))
                    .unwrap_or_default();
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text: format!("{}:{}", caps.len(), first),
                })])
            }
            // TS.2: walk with a tree-cursor; echo `<moved>:<kind-after-descent>` —
            // proof the cursor crossed and its `goto-*` mutated host-side state.
            5 => {
                let tree = tree.ok_or("multiseam: no tree snapshot")?;
                let cursor = tree.root().walk();
                let moved = cursor.goto_first_named_child();
                let kind = cursor.current_node().kind();
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text: format!("{moved}:{kind}"),
                })])
            }
            other => Err(format!("multiseam: unknown action callback {other}")),
        }
    }

    fn apply_motion(_c: u32, _ctx: MotionContext) -> Result<MotionResult, String> {
        Err("multiseam: no motions".into())
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("multiseam: no operators".into())
    }
    fn apply_text_object(_c: u32, _ctx: TextObjectContext) -> Result<Range, String> {
        Err("multiseam: no text objects".into())
    }
    fn parse_ex_args(_c: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("multiseam: no ex-commands".into())
    }
    fn apply_ex_command(_c: u32, _ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        Err("multiseam: no ex-commands".into())
    }
}

export!(Component);
