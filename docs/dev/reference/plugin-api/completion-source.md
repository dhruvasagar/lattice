<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `completion-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `completion-source-plugin` (exports)

Mirrors `lattice_completion` completion sources (PH7.6). A WASM completion
source *exports* this interface; the host drives its async `generate` off the
keystroke path (the LSP-async-completion precedent, `pipeline.rs`
`match_and_rank` "pre-supplies rows from async LSP responses") and feeds the
produced candidates through the **native** matcher / ranker / annotator.

**Generator only, by design (option A, locked with Dhruva).** The four native
traits — `Candidate{Generator,Matcher,Ranker,Annotator}` — are NOT four guest
exports: `matches` + `annotate` run *per candidate* on the synchronous
keystroke pipeline, so crossing them to an async, actor-bound guest per item
would fire hundreds of boundary calls per keystroke (paramount #1). The
plugin's value-add is the GENERATOR (async produce, like LSP); matching /
ranking / annotation stay native (they have good defaults a plugin rarely
overrides — "the API grows from real plugins", design §5.5). The matcher /
ranker / annotator data types are still mirrored in `types.wit` so the WIT is
sized against the whole trait set before the ABI freeze.

## Uses

- [`completion-source-spec`](types.md#record-completion-source-spec) from [`types`](types.md)
- [`generate-context`](types.md#record-generate-context) from [`types`](types.md)
- [`raw-candidate`](types.md#record-raw-candidate) from [`types`](types.md)

## Functions (2)

### `generate`

```wit
generate: func(ctx: generate-context) -> result<list<raw-candidate>, string>
```

Produce raw candidates for the current slot. `ctx` carries the query
prefix + case flag (§4.2 owned projection); the host then runs the
native `match_and_rank` over the result. Async — a produce call suspends
the guest, never the keystroke path. An `err` string is logged and the
source contributes no rows (the LSP-failure precedent). Candidates carry
plugin-specific data via the `candidate-data.extension` hatch.

### `spec`

```wit
spec: func() -> completion-source-spec
```

The source's identity (`name` + `doc`), the `insert_generator` pair.
Called once at registration.

