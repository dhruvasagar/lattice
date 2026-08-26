//! TR.2b transient fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the
//! `transient-source-plugin` world, driving the menu actor
//! (`transient_task.rs`) through real host→guest calls:
//!
//!   - `id` names the menu `fixture-capture`, which is what the host registers
//!     it under and what `Effect::OpenTransient` addresses;
//!   - `build` returns three rows: two `action`s naming the SAME command with
//!     DIFFERENT args (the per-row-args property the whole seam exists for),
//!     plus a `dismiss` — and a fourth row naming a command that was never
//!     registered, so the drop-the-row-keep-the-menu rule is proven against a
//!     real guest;
//!   - the menu's title ECHOES the projected context's major mode, so the host
//!     can assert the projection crossed rather than trusting the signature;
//!   - a context whose major mode is `broken-mode` returns the WIT typed
//!     `err`, exercising the menu-does-not-open path with a real guest error
//!     rather than a host trap.

wit_bindgen::generate!({
    world: "transient-source-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::transient_source::Guest;
use lattice::plugin_host::types::{
    Args, TransientAction, TransientArgument, TransientContext, TransientGroup, TransientItem,
    TransientItemKind, TransientSpec,
};

struct Component;

/// One action row firing `command` with a single string argument.
fn action(key: &str, label: &str, command: &str, arg: &str) -> TransientItem {
    TransientItem {
        key: vec![key.to_string()],
        label: label.to_string(),
        description: format!("capture a {label}"),
        kind: TransientItemKind::Action(TransientAction {
            command: command.to_string(),
            args: Args::String(arg.to_string()),
        }),
    }
}

impl Guest for Component {
    fn id() -> String {
        "fixture-capture".to_string()
    }

    fn build(ctx: TransientContext) -> Result<TransientSpec, String> {
        if ctx.major_mode.as_deref() == Some("broken-mode") {
            return Err("fixture: no templates configured".to_string());
        }
        Ok(TransientSpec {
            // The title echoes the projection so a host assertion is testing
            // that the context crossed, not merely that a menu appeared. The
            // minor count rides along for the second axis.
            title: format!(
                "{} ({} minors) for {}",
                ctx.major_mode.as_deref().unwrap_or("no-major"),
                ctx.minor_modes.len(),
                // TR.3a: what the open was FOR. Echoed so a host assertion
                // tests that the args crossed, not merely that the field
                // exists — a menu that drills down reads its subject here.
                match &ctx.args {
                    Args::String(s) => s.clone(),
                    Args::None => "nothing".to_string(),
                    other => format!("{other:?}"),
                }
            ),
            groups: vec![TransientGroup {
                label: "Templates".to_string(),
                items: vec![
                    action("t", "todo", "fixture-capture-key", "todo"),
                    action("n", "note", "fixture-capture-key", "note"),
                    // Names a command nobody registered: the host must drop
                    // THIS row and keep the rest.
                    action("z", "ghost", "fixture-command-never-registered", "z"),
                    // TR.3b: a FIELD. Pressing its key parks the menu, prompts,
                    // and puts the menu back with the answer in its state.
                    TransientItem {
                        key: vec!["w".to_string()],
                        label: "word".to_string(),
                        description: "a field the menu collects".to_string(),
                        kind: TransientItemKind::Argument(TransientArgument {
                            name: "word".to_string(),
                            default: Some("chat".to_string()),
                            prompt: "Word".to_string(),
                        }),
                    },
                    TransientItem {
                        key: vec!["q".to_string()],
                        label: "quit".to_string(),
                        description: String::new(),
                        kind: TransientItemKind::Dismiss,
                    },
                ],
            }],
            footer: Some("q to dismiss".to_string()),
        })
    }
}

export!(Component);
