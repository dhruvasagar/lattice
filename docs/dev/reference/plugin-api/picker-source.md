<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `picker-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `picker-source-plugin` (exports), `project-plugin` (exports)

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

