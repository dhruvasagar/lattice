<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `llm-tools`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `llm-tools-plugin` (exports)

AT.5: tools a plugin offers to the LLM assistant.

The assistant's tools come from sources: lattice's own, `web`, each MCP
server, and with this seam each plugin that exports `llm-tools`. A tool's
descriptor is MCP's shape (a name, a description, a JSON Schema for the
input), so a plugin gets MCP's data model without its transport.

### A call cannot write

`call` returns data and nothing else: text for the model, and edits the
host applies through `lattice.edit_file`, with its project fence, staleness
check and review. There is no effect list, so a plugin that wants to change
a file can only propose it. That is structural, not trusted.

### The host decides whether a tool asks

From the plugin's capability grant, never from what the plugin says about
its tools: a plugin with no network and no process capability runs its
`read` tools unasked and sends its proposals to review; one that holds
either asks before every call. `llm-tool-kind` says what a tool does with
its result, not how much it is trusted.

### Where it runs

On the plugin's own actor, off the editor actor, once per model tool call.
A call is never on the keystroke path: the assistant awaits it like any
other tool, and a slow plugin delays its own reply and nothing else.

## Uses

- [`llm-tool`](types.md#record-llm-tool) from [`types`](types.md)
- [`llm-tool-result`](types.md#record-llm-tool-result) from [`types`](types.md)
- [`llm-profile`](types.md#record-llm-profile) from [`types`](types.md)

## Functions (3)

### `call`

```wit
call: func(name: string, args: string) -> result<llm-tool-result, string>
```

Run the tool `name`. `args` is the model's JSON input, as text.

An `err` is the call's result as the model sees it: the conversation
continues, and the model is told the tool failed and why.

**Example — Answer a read tool with text, and a propose tool with an edit the host reviews and applies** · [`crates/lattice-plugin-host/tests/fixtures/llm-tools-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/llm-tools-guest/src/lib.rs)

```rust
"outline" => Ok(text(format!("outline for {args}"))),
"change" => Ok(LlmToolResult {
    text: "marking it done".to_string(),
    proposals: vec![ProposedEdit {
        path: "notes.org".to_string(),
        old_string: "* TODO ship".to_string(),
        new_string: "* DONE ship".to_string(),
    }],
}),
```

### `profiles`

```wit
profiles: func() -> list<llm-profile>
```

Profiles this plugin contributes: a workflow is a system prompt and a
tool selection. Called once, at registration. The host trusts a plugin
profile as it trusts a project's file, so a key it may not set is
dropped and named.

**Example — Contribute a workflow: a system prompt and a tool selection, as profile-file keys** · [`crates/lattice-plugin-host/tests/fixtures/llm-tools-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/llm-tools-guest/src/lib.rs)

```rust
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
```

### `tools`

```wit
tools: func() -> list<llm-tool>
```

The tools this plugin offers. Called once, at registration.

A tool whose name is not letters, digits, `_` and `-`, or whose schema
is not a JSON object, is not offered, and the listing says why.

**Example — Offer one read tool and one propose tool, each with an object input schema** · [`crates/lattice-plugin-host/tests/fixtures/llm-tools-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/llm-tools-guest/src/lib.rs)

```rust
fn tools() -> Vec<LlmTool> {
    vec![
        LlmTool {
            name: "outline".to_string(),
            description: "The outline of the notes.".to_string(),
            input_schema: SCHEMA.to_string(),
            kind: LlmToolKind::Read,
        },
        LlmTool {
            name: "change".to_string(),
            description: "Mark the first TODO done.".to_string(),
            input_schema: SCHEMA.to_string(),
            kind: LlmToolKind::Propose,
        },
    ]
}
```

