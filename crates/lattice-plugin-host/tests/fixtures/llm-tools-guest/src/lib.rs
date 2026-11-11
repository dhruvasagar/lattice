//! AT.5 llm-tools fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `llm-tools-plugin`
//! world, driving the actor (`llm_tools_task.rs`) through real host→guest
//! calls:
//!
//!   - `tools` offers `outline` (read) and `change` (propose), their input
//!     schema written by the SDK from the `Outline` struct;
//!   - `call("outline", args)` parses `args` through the SDK, so a wrong input
//!     is the call's error naming the field, then echoes `args`, so the host
//!     can assert the model's input crossed rather than trusting the signature;
//!   - `call("change", _)` returns text and one proposed edit;
//!   - `call("read", path)` / `call("delete", path)` go through
//!     `host-services`, so the host can prove a tool call reads under its
//!     grant and cannot delete under it, whatever the grant says;
//!   - `call("fail", _)` is the WIT typed `err`, and `call("trap", _)` panics,
//!     which traps the guest and quarantines it;
//!   - `profiles` contributes one workflow.

wit_bindgen::generate!({
    world: "llm-tools-plugin",
    path: "../../../../lattice-wit/wit",
});

use exports::lattice::plugin_host::llm_tools::Guest;
use lattice::plugin_host::host_services;
use lattice::plugin_host::types::{
    LlmProfile, LlmTool, LlmToolKind, LlmToolResult, ProposedEdit,
};

use lattice_plugin_sdk::llm::{input_schema, parse_args};
use lattice_plugin_sdk::ConfigShape;

struct Component;

/// What `outline` and `change` take.
#[derive(ConfigShape)]
struct Outline {
    /// Only headlines matching this.
    q: Option<String>,
}

fn text(text: String) -> LlmToolResult {
    LlmToolResult {
        text,
        proposals: Vec::new(),
    }
}

impl Guest for Component {
    // @example llm-tools.tools: Offer one read tool and one propose tool, each with an object input schema
    fn tools() -> Vec<LlmTool> {
        // The schema comes from the struct the call parses into, so the two
        // cannot drift. It is an error only for a struct without named fields.
        let schema = input_schema::<Outline>().unwrap_or_default();
        vec![
            LlmTool {
                name: "outline".to_string(),
                description: "The outline of the notes.".to_string(),
                input_schema: schema.clone(),
                kind: LlmToolKind::Read,
            },
            LlmTool {
                name: "change".to_string(),
                description: "Mark the first TODO done.".to_string(),
                input_schema: schema,
                kind: LlmToolKind::Propose,
            },
        ]
    }
    // @end-example

    fn call(name: String, args: String) -> Result<LlmToolResult, String> {
        match name.as_str() {
            // @example llm-tools.call: Answer a read tool with text, and a propose tool with an edit the host reviews and applies
            "outline" => {
                // A wrong input goes back to the model naming the field.
                let _query: Outline = parse_args(&args).map_err(|e| e.to_string())?;
                Ok(text(format!("outline for {args}")))
            }
            "change" => Ok(LlmToolResult {
                text: "marking it done".to_string(),
                proposals: vec![ProposedEdit {
                    path: "notes.org".to_string(),
                    old_string: "* TODO ship".to_string(),
                    new_string: "* DONE ship".to_string(),
                }],
            }),
            // @end-example
            // @example host-services.read-file: Read a file through the host under the plugin's fs grant, reporting a refusal as text
            "read" => Ok(text(match host_services::read_file(&args) {
                Ok(body) => body,
                Err(e) => format!("error: {e}"),
            })),
            // @end-example
            "delete" => Ok(text(match host_services::delete_file(&args) {
                Ok(()) => "deleted".to_string(),
                Err(e) => format!("error: {e}"),
            })),
            "fail" => Err("fixture: nothing matched".to_string()),
            "trap" => panic!("fixture: trapped on purpose"),
            other => Err(format!("fixture: no tool `{other}`")),
        }
    }

    // @example llm-tools.profiles: Contribute a workflow: a system prompt and a tool selection, as profile-file keys
    fn profiles() -> Vec<LlmProfile> {
        vec![LlmProfile {
            name: "fixture-plan".to_string(),
            description: "Plan from the fixture's notes.".to_string(),
            settings: vec![
                ("system".to_string(), "Plan the day.".to_string()),
                ("tools".to_string(), "fixture.outline".to_string()),
            ],
        }]
    }
    // @end-example
}

export!(Component);
