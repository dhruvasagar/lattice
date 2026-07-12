//! PH7.7c grammar fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `grammar-plugin` world,
//! driving the sync trampoline (`grammar_trampoline.rs`) through a real
//! guest↔host sync call:
//!   - `register-grammar` (the world export the host calls once) contributes a
//!     motion (`down-n`, callback 1) and a text object (`to-cursor`, callback 2)
//!     via the imported `grammar` register API.
//!   - `grammar-callbacks.apply-motion(1, ctx)` returns a target `count` lines
//!     below the cursor (pure arithmetic over the projected context — no document
//!     handle needed), proving the context crosses in and the `motion-result`
//!     crosses back.
//!   - `apply-text-object(2, ctx)` returns the range from line-start to the
//!     cursor, proving the text-object path.
//!   - An unknown callback id, and every other callback kind, return the WIT
//!     typed `err` (distinct from a host trap — exercises the graceful path).

wit_bindgen::generate!({
    world: "grammar-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::grammar_callbacks::Guest as Callbacks;
use lattice::plugin_host::grammar;
use lattice::plugin_host::types::{
    ActionContext, Args, Effect, ExCommandContext, MotionContext, MotionResult, MotionSpec,
    OperatorContext, Position, Range, TextObjectContext, TextObjectSpec,
};

struct Component;

impl Guest for Component {
    /// The host calls this once; the guest contributes its grammar through the
    /// imported `register-*` host functions.
    fn register_grammar() {
        grammar::register_motion(
            "down-n",
            "jump count lines down (fixture)",
            &MotionSpec {
                jump: false,
                exclusive: false,
                args_schema: Vec::new(),
            },
            1,
        );
        grammar::register_text_object(
            "to-cursor",
            "line start to cursor (fixture)",
            &TextObjectSpec {
                args_schema: Vec::new(),
            },
            2,
        );
        // A motion whose callback id has no `apply-motion` arm → the guest
        // returns a WIT `err`, exercising the graceful no-op path (§8).
        grammar::register_motion(
            "fails",
            "always returns a guest err (fixture)",
            &MotionSpec {
                jump: false,
                exclusive: false,
                args_schema: Vec::new(),
            },
            99,
        );
    }
}

impl Callbacks for Component {
    fn apply_motion(callback: u32, ctx: MotionContext) -> Result<MotionResult, String> {
        match callback {
            1 => Ok(MotionResult {
                target: Position {
                    line: ctx.from.line + ctx.count,
                    byte: 0,
                },
                linewise: true,
            }),
            other => Err(format!("fixture: unknown motion callback {other}")),
        }
    }

    fn apply_text_object(callback: u32, ctx: TextObjectContext) -> Result<Range, String> {
        match callback {
            2 => Ok(Range {
                start: Position {
                    line: ctx.at.line,
                    byte: 0,
                },
                end: ctx.at,
            }),
            other => Err(format!("fixture: unknown text-object callback {other}")),
        }
    }

    fn apply_operator(_callback: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("fixture: no operators".to_string())
    }

    fn apply_action(_callback: u32, _ctx: ActionContext) -> Result<Vec<Effect>, String> {
        Err("fixture: no actions".to_string())
    }

    fn parse_ex_args(_callback: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("fixture: no ex-commands".to_string())
    }

    fn apply_ex_command(_callback: u32, _ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        Err("fixture: no ex-commands".to_string())
    }
}

export!(Component);
