//! Demo config for the L.4b capture harness: a PROGRAMMATIC custom command,
//! not a keybinding. `:hello <name>` is registered into the same
//! `CommandRegistry` the builtin `:write` / `:quit` live in — indistinguishable
//! to the dispatcher from a native command. See docs/user/init.md's "Custom
//! grammar" section for the full worked example this is trimmed from.

wit_bindgen::generate!({ world: "grammar-plugin", path: "wit" });

use exports::lattice::plugin_host::grammar_callbacks::Guest as Callbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::grammar;
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, Args, EchoLevel, EchoPayload, Effect, ExCommandContext, ExCommandSpec,
    LatencyClass, MotionContext, MotionResult, OperatorContext, Range, SurfaceForm,
    TextObjectContext,
};

struct Component;

// Callback ids — this component only contributes one thing, so it only needs
// one pair (parse, apply).
const EXC_HELLO_PARSE: u32 = 1;
const EXC_HELLO_APPLY: u32 = 2;

impl Guest for Component {
    fn register_grammar() {
        // A real ex-command, registered from Rust compiled to WASM -- this is
        // what "config is programmable" means: a compiled function, not a
        // remapped key.
        grammar::register_ex_command(
            "hello",
            "Greet someone: :hello <name>",
            &ExCommandSpec {
                latency_class: LatencyClass::Reflex,
                accepts_bang: false,
                accepts_range: false,
                args_schema: Vec::new(),
                surface_form: SurfaceForm::Keyword,
            },
            EXC_HELLO_PARSE,
            EXC_HELLO_APPLY,
        );
    }
}

impl Callbacks for Component {
    // Unused by this component -- `grammar-plugin` exports the whole
    // grammar-callbacks interface regardless of which register_* calls a
    // given guest makes, so every callback needs a body. None of these ids
    // are ever dispatched because register_grammar never hands them out.
    fn apply_motion(
        callback: u32,
        _ctx: MotionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<MotionResult, String> {
        Err(format!("no motion {callback}"))
    }

    fn apply_operator(callback: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err(format!("no operator {callback}"))
    }

    fn apply_text_object(
        callback: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        Err(format!("no text object {callback}"))
    }

    fn apply_action(
        callback: u32,
        _ctx: ActionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Err(format!("no action {callback}"))
    }

    // Parse `:hello <rest>` -- the raw string after the command word.
    fn parse_ex_args(callback: u32, rest: String, _bang: bool) -> Result<Args, String> {
        match callback {
            EXC_HELLO_PARSE => Ok(Args::String(rest.trim().to_string())),
            other => Err(format!("no parser {other}")),
        }
    }

    // Apply `:hello` -- read the parsed Args, return an Effect.
    fn apply_ex_command(
        callback: u32,
        ctx: ExCommandContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            EXC_HELLO_APPLY => {
                let who = match ctx.args {
                    Args::String(s) if !s.is_empty() => s,
                    _ => "world".to_string(),
                };
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text: format!("Hello, {who}! (from init.rs, compiled to WASM)"),
                })])
            }
            other => Err(format!("no ex-command {other}")),
        }
    }
}

export!(Component);
