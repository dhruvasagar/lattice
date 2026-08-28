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
    ActionContext, ActionSpec, ApplyEditPayload, Args, EchoLevel, EchoPayload, Edit, EditKind,
    Effect, ExCommandContext, MotionContext, MotionResult, MotionSpec, OperatorContext, Position,
    Range, TextObjectContext, TextObjectSpec,
};
use lattice::plugin_host::{config, grammar, host_services, modes, tree_sitter};

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
        // OT.2: parse a file that is NOT an open buffer. The path arrives via
        // `ctx.args` so one fixture proves the granted and every denied case.
        grammar::register_action(
            "multiseam-parse-file",
            "parse an off-buffer file via the tree-sitter seam (OT.2)",
            &spec(),
            22,
        );
        // OT.1: the tree reaches motions and text objects too, not only actions.
        // Both of these answer FROM THE TREE ALONE — a host that hands them
        // `none` (as every host did before OT.1) fails them loudly rather than
        // returning a plausible-looking position. That is the whole point:
        // org's `]]` / `[[` and `ir` / `ar` resolve `(section)` structure.
        grammar::register_motion(
            "multiseam-tree-motion",
            "jump to where the parse tree ends (OT.1)",
            &MotionSpec {
                jump: false,
                exclusive: false,
                args_schema: Vec::new(),
            },
            20,
        );
        grammar::register_text_object(
            "multiseam-tree-object",
            "the span of the whole parse tree (OT.1)",
            &TextObjectSpec {
                args_schema: Vec::new(),
            },
            21,
        );
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
        // AP.0.2 two-hop: an action that HANDLES the chord by editing, so a
        // test can tell "the second layer ran" from "the builtin ran".
        grammar::register_action(
            "multiseam-handles",
            "replace line 0 with HANDLED (two-hop fall-through probe)",
            &spec(),
            6,
        );
        grammar::register_action(
            "multiseam-cursor",
            "walk the tree with a cursor via the tree-sitter seam (TS.2)",
            &spec(),
            5,
        );
        // OM.11: an action that calls `host-services`. The point is not what
        // it returns — an ungranted plugin's `walk` is an `err` — but that the
        // import RESOLVES on the sync grammar linker. A component providing
        // both `grammar` and `picker-source` (org's refile) declares this
        // import, and instantiation must satisfy every import a world declares,
        // not only the ones the seam being drained uses.
        grammar::register_action(
            "multiseam-walk",
            "call host-services::walk from the sync grammar seam (OM.11)",
            &spec(),
            7,
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
            // OM.2: majors claim a language; this is a minor, so `none`.
            target_language: None,
            // MO.1: this mode sets no options for its buffers.
            options: vec![],
        });
        // AP.0.2 two-hop: a SECOND mode binding the same chord to an action
        // that handles it. Whichever of the two layers the host orders higher,
        // a correct peel ends at `multiseam-handles` — with the old
        // drop-every-layer fall-through it ends at the builtin `x` half the
        // time, which is exactly the bug.
        modes::register_mode(&ModeDeclaration {
            id: "multiseam-second-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Global,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![ModeKeymapBinding {
                binding_mode: BindingMode::Normal,
                chord: "x".to_string(),
                command: "multiseam-handles".to_string(),
            }],
            target_language: None,
            // MO.1: this mode sets no options for its buffers.
            options: vec![],
        });
    }

    /// config seam — register a typed option.
    fn register_options() {
        config::register_option(
            // Short name — the host auto-namespaces to `multiseam.style`.
            "style",
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
            // AP.0.2 two-hop: handle it, visibly.
            6 => Ok(vec![Effect::ApplyEdit(ApplyEditPayload {
                target: ctx.buffer_id,
                edit: Edit {
                    range: Range {
                        start: Position { line: 0, byte: 0 },
                        end: Position {
                            line: 0,
                            byte: doc.line(0).map(|l| l.len() as u32).unwrap_or(0),
                        },
                    },
                    kind: EditKind::Replace("HANDLED".to_string()),
                },
                cursor: None,
            })]),
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
            // OM.11: proof the `host-services` import is reachable here at
            // all. Ungranted, so `walk` answers `err`; echoing which way it
            // went distinguishes "the seam is wired" from "the seam is
            // missing", which an unresolved import would have turned into a
            // load failure long before this ran.
            7 => {
                let text = match host_services::walk("/") {
                    Ok(paths) => format!("walked:{}", paths.len()),
                    Err(_) => "refused".to_string(),
                };
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text,
                })])
            }
            // OT.2: parse an off-buffer file and report what came back, so the
            // test can tell "the tree crossed" from "the host said none".
            // Echoes `<root-kind>:<named-child-count>`.
            22 => {
                let path = match &ctx.args {
                    Args::String(s) => s.clone(),
                    other => return Err(format!("multiseam: parse-file wants a path, got {other:?}")),
                };
                let snapshot = tree_sitter::parse_file(&path)
                    .ok_or_else(|| format!("multiseam: parse-file returned none for {path}"))?;
                let root = snapshot.root();
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text: format!("{}:{}", root.kind(), root.named_child_count()),
                })])
            }
            other => Err(format!("multiseam: unknown action callback {other}")),
        }
    }

    fn apply_motion(
        c: u32,
        _ctx: MotionContext,
        _doc: &Document,
        tree: Option<&TreeSnapshot>,
    ) -> Result<MotionResult, String> {
        match c {
            // OT.1: target the end of the parse tree's own span. Unanswerable
            // without the tree, so `none` surfaces as a guest err rather than a
            // wrong-but-believable line.
            20 => {
                let tree = tree.ok_or_else(|| "multiseam: motion got no tree".to_string())?;
                Ok(MotionResult {
                    target: tree.root().byte_range().end,
                    linewise: true,
                })
            }
            other => Err(format!("multiseam: unknown motion callback {other}")),
        }
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("multiseam: no operators".into())
    }
    fn apply_text_object(
        c: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        match c {
            // OT.1: the structural peer — org's `ir` / `ar` resolve a subtree,
            // which IS a tree node rather than a star count.
            21 => {
                let tree = tree.ok_or_else(|| "multiseam: text object got no tree".to_string())?;
                Ok(tree.root().byte_range())
            }
            other => Err(format!("multiseam: unknown text-object callback {other}")),
        }
    }
    fn parse_ex_args(_c: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("multiseam: no ex-commands".into())
    }
    fn apply_ex_command(_c: u32, _ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        Err("multiseam: no ex-commands".into())
    }
}

export!(Component);
