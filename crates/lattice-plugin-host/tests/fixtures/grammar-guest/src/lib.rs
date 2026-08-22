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
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::grammar;
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, Args, Effect, EchoLevel, EchoPayload, ExCommandContext,
    OpenPickerPayload,
    MotionContext, MotionResult, MotionSpec, OperatorContext, Position, Range, TextObjectContext,
    TextObjectSpec,
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
        // A motion whose callback TRAPS (a guest panic → wasm `unreachable`),
        // distinct from a guest err: exercises the trampoline's trap branch
        // (quarantine trip + one PluginCrashed + Error/Trap trace) and the
        // re-trip short-circuit on the next dispatch (§8, PH7.12).
        grammar::register_motion(
            "traps",
            "always traps the guest (fixture)",
            &MotionSpec {
                jump: false,
                exclusive: false,
                args_schema: Vec::new(),
            },
            3,
        );
        // AP.0.1: an action that reads buffer text through the `borrow<document>`
        // handle. `apply-action(5)` slices the byte at `ctx.cursor` and echoes it
        // with the cursor coords — proving both the document borrow and the
        // cursor cross the sync grammar boundary.
        grammar::register_action(
            "read-at-cursor",
            "echo the char at the cursor (fixture)",
            &ActionSpec {
                args_schema: Vec::new(),
            },
            5,
        );
        // PH7.4e: "utilize an existing picker" — the guest asks the host to
        // open a picker source it did not define. `apply-action(6)` returns
        // `Effect::OpenPicker`, which is how a plugin reuses a native
        // picker (or another plugin's) rather than shipping its own.
        grammar::register_action(
            "open-files-picker",
            "open the host's `files` picker (fixture)",
            &ActionSpec {
                args_schema: Vec::new(),
            },
            6,
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
            // A host TRAP (not a guest err): panic → wasm `unreachable`. The host
            // classifies it, trips the quarantine, and short-circuits later calls.
            3 => panic!("fixture: deliberate trap"),
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

    fn apply_action(
        callback: u32,
        ctx: ActionContext,
        doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            // Read the single byte at the cursor via the borrowed document, then
            // echo it back with the cursor coords — observable proof that the
            // `document` handle AND `ctx.cursor` crossed correctly (AP.0.1). A
            // range past EOF returns the host's typed `err` (graceful path).
            5 => {
                let start = ctx.cursor;
                let end = Position {
                    line: start.line,
                    byte: start.byte + 1,
                };
                let text = doc.get_text_range(Range { start, end })?;
                Ok(vec![Effect::Echo(EchoPayload {
                    level: EchoLevel::Info,
                    text: format!("{text}@{}:{}", start.line, start.byte),
                })])
            }
            // PH7.4e: reuse a picker the guest does not own. The args are
            // non-empty and ordered so the test can prove the payload
            // crossed intact rather than just that *a* picker opened.
            6 => Ok(vec![Effect::OpenPicker(OpenPickerPayload {
                source: "files".to_string(),
                args: vec!["src".to_string(), "*.rs".to_string()],
            })]),
            other => Err(format!("fixture: unknown action callback {other}")),
        }
    }

    fn parse_ex_args(_callback: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("fixture: no ex-commands".to_string())
    }

    fn apply_ex_command(_callback: u32, _ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        Err("fixture: no ex-commands".to_string())
    }
}

export!(Component);
