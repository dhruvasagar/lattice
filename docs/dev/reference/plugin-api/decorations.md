<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `decorations`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `decorations-plugin` (exports)

The decoration **producer** API (plugin-host.md §5 `decorations`, PH7.9),
mirroring `Mode::gutter_decorations` + `GutterDecoration` (lattice-mode). A
WASM decoration provider *exports* this interface; the host calls its
`gutter-decorations` producer **off the render path** on a trigger (edit /
scroll / diagnostic change), caches the returned `list<gutter-decoration>`
per buffer, and the renderer reads the cache.

**Producer, not per-frame (the completion PH7.6 fork).** The native
`Mode::gutter_decorations` is a SYNCHRONOUS trait the renderer reads *every
frame* — a WASM mode cannot satisfy it inline (that would be per-frame WASM,
a paramount-#1 violation, §7 rule 7). So the seam is an ASYNC producer whose
result the host caches; the renderer never calls WASM on the tick. The
matching / layout of the cached decorations into physical gutter columns stays
native (the host builds the snapshot).

## Uses

- [`decoration-context`](types.md#record-decoration-context) from [`types`](types.md)
- [`gutter-decoration`](types.md#variant-gutter-decoration) from [`types`](types.md)

## Functions (1)

### `gutter-decorations`

```wit
gutter-decorations: func(ctx: decoration-context) -> result<list<gutter-decoration>, string>
```

Produce the per-line gutter decorations for a buffer. `ctx` is the owned
projection (buffer id / path / line count, §4.2); bulk buffer text (a diff
producer's input) rides `host-services` / the deferred `document` handle,
not the context. Async — a produce call suspends the guest, never the
render path. An `err` string is logged and the provider contributes no
decorations for this trigger (graceful, §8) — the cached snapshot keeps its
prior value so cues never flicker mid-refresh.

