<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `picker-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `picker-source-plugin` (exports), `project-plugin` (exports)

Mirrors `PickerSourceGenerator` (`lattice_picker::source`). A WASM picker
plugin *exports* this interface to serve the sources it declared through
`picker-registry`; the host wraps the exports as an
`Arc<dyn PickerSourceGenerator>` (PH7.4c.2) and registers it through the
`SubsystemBoot` install seam → `PickerRegistry::register_generator`, so a
plugin source is indistinguishable from a first-party one at the registry.
The ⭐ Phase-7-exit interface; exercised by the `picker-guest` fixture and
used by `plugins/project`.

## Uses

- [`raw-candidate`](types.md#record-raw-candidate) from [`types`](types.md)
- [`routing-payload`](types.md#variant-routing-payload) from [`types`](types.md)
- [`picker-context`](types.md#record-picker-context) from [`types`](types.md)
- [`picker-accept-outcome`](types.md#variant-picker-accept-outcome) from [`types`](types.md)

## Functions (2)

### `accept`

```wit
accept: func(source: string, ctx: picker-context, routing: routing-payload) -> result<picker-accept-outcome, string>
```

Translate the user's chosen `routing` token into a typed
`PickerAcceptOutcome` the host applies. A mismatch is an `err` (echoed).

**Example — Map the routing token a row carried to the outcome the host performs** · [`crates/lattice-plugin-host/tests/fixtures/picker-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/picker-guest/src/lib.rs)

```rust
fn accept(
    source: String,
    _ctx: PickerContext,
    routing: RoutingPayload,
) -> Result<PickerAcceptOutcome, String> {
    // OR.5b: the second source's accept is distinguishable too — otherwise a
    // test could not tell "routed to the right source" from "there is only
    // one body".
    if source == SECOND {
        return Ok(PickerAcceptOutcome::OpenFile("/second/accepted".to_string()));
    }
    match routing {
        RoutingPayload::OpenFile(p) => Ok(PickerAcceptOutcome::OpenFile(p)),
        RoutingPayload::Buffer(id) => Ok(PickerAcceptOutcome::SwitchBuffer(id)),
        // OR.5: the create row. The query crosses VERBATIM — the host must
        // not have trimmed, lowercased or otherwise had an opinion about a
        // namespace it does not own — so the fixture echoes it back inside
        // a path the test can compare exactly.
        RoutingPayload::Create(query) => {
            Ok(PickerAcceptOutcome::OpenFile(format!("/created/{query}")))
        }
        _ => Err("fixture: unexpected routing token".to_string()),
    }
}
```

### `init`

```wit
init: func(source: string, ctx: picker-context, args: list<string>) -> result<list<candidate-pair>, string>
```

Build the candidate set for `:picker <id> <args>`. `ctx` is the owned
`PickerContext` projection (§4.2). Returns the `(candidate, routing)`
pairs; an `err` string is echoed and the picker stays closed. (One-shot
list; the incremental `Stream` shape — the deferred §15 streaming
question — lands with a live source.)

NB: the active buffer's bulk **text** rides a `borrow<document>` handle
(PH7.3c `DocumentResource`) that a text-reading source (`:picker lines`)
needs — deferred here (the `fuzzy-finder`/`files` exit reads no buffer
text, only walks the fs via `host-services`). Passing a host-owned
resource into a guest *export* has a bindgen-modeling subtlety to resolve;
tracked as a focused follow-up (see the slice plan).

`source` names WHICH of this plugin's registered sources is being built
— one component may register several (see `picker-registry`), and they
share one actor and one guest instance.

**Example — Build the candidate rows for each picker source this component registered** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// `source` is checked rather than assumed: one component may register
/// several sources and they share one actor, so a source id this plugin
/// never registered is untrusted input, not a case to fall through.
fn init(
    source: String,
    ctx: PickerContext,
    _args: Vec<String>,
) -> Result<Vec<CandidatePair>, String> {
    let pairs = match source.as_str() {
        picker::PROJECTS_PICKER => picker::init(load())?,
        // PB.1: the root rides the CONTEXT, not the args — PC.1's rule,
        // and the same reason: `:project-buffers` opened from the
        // switch-commands menu names a project other than the one the
        // buffer is in, and `Effect::OpenPicker { root }` is the seam that
        // carries it. Reading `args[0]` would work for this source and
        // then be a second convention for the next one.
        picker::PROJECT_BUFFERS_PICKER => picker::buffers_init(
            &ctx.workspace_root,
            ctx.buffers,
            ctx.active_buffer.buffer_id,
        ),
        other => return Err(format!("project: no picker source `{other}`")),
    };
    Ok(pairs
        .into_iter()
        .map(|(candidate, routing)| CandidatePair { candidate, routing })
        .collect())
}
```

## Types (1)

### record `candidate-pair`

```wit
record candidate-pair {
    candidate: raw-candidate,
    routing: routing-payload,
}
```

One `(candidate, routing)` pair — the WIT form of the native
`CandidateBatch` element (`Vec<(RawCandidate, RoutingPayload)>`). The
`routing` token is opaque to the picker; the source emits it here and
consumes it in `accept`.

**Fields**

- `candidate`: [`raw-candidate`](types.md#record-raw-candidate)
- `routing`: [`routing-payload`](types.md#variant-routing-payload)

