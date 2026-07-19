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
use lattice::plugin_host::modes::{
    ActivationPolicy, ModeCapabilities, ModeDeclaration, ModeKind,
};
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, Args, EchoLevel, EchoPayload, Effect, ExCommandContext,
    MotionContext, MotionResult, OperatorContext, Position, Range, TextObjectContext,
};
use lattice::plugin_host::{config, grammar, modes};

struct Component;

impl Guest for Component {
    /// grammar seam — contribute the action the mode's keymap binds to.
    fn register_grammar() {
        grammar::register_action(
            "multiseam-read",
            "echo the char at the cursor (multiseam fixture)",
            &ActionSpec {
                args_schema: Vec::new(),
            },
            1,
        );
    }

    /// modes seam — declare a minor mode. Empty keymap: the spike proves the
    /// mode *registers* from the combined component; keymap-binding-to-own-action
    /// (with the `provides` grammar-before-modes ordering + action naming) is
    /// AP.1 proper, and the bind mechanism itself is already covered by the
    /// single-seam `emacs-keys-guest` test.
    fn register_modes() {
        modes::register_mode(&ModeDeclaration {
            id: "multiseam-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Global,
            capabilities: ModeCapabilities::empty(),
            keymap: Vec::new(),
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
