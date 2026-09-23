# lattice-plugin-sdk

Helpers for writing [lattice](https://github.com/dhruvasagar/lattice) plugins as
WebAssembly components: typed config shapes and event payloads.

This is a convenience layer, not the API. The API is WIT — see
[`lattice-wit`](https://crates.io/crates/lattice-wit). You can write a plugin
without this crate; you will just hand-build the value trees that
`#[derive(ConfigShape)]` builds for you.

## Use

```toml
[dependencies]
lattice-plugin-sdk = "0.1"
```

```rust
use lattice_plugin_sdk::ConfigShape;

/// A plugin's options, declared as a Rust struct rather than assembled as a
/// nested value tree by hand.
#[derive(ConfigShape)]
struct Options {
    enabled: bool,
    max_results: i64,
    separator: String,
}
```

The derive produces both the *schema* the editor reads to type-check and
document your options, and the flattening into the arena representation the
config seam takes across the component boundary.

## The version is the ABI generation

This crate shares one version with `lattice-wit` and
`lattice-plugin-sdk-derive`, and its `major.minor` is always the WIT package's:

```
lattice-plugin-sdk = "0.2"   ⟺   package lattice:plugin-host@0.2.x
```

It carries the number even though it has no `lattice-wit` dependency and is
WIT-agnostic Rust. The coupling is real but semantic rather than structural —
`ConfigShape` flattens into exactly the shape the config seam consumes — and a
semantic coupling with independent version lines is the worst case, because
nothing says the two are related until something is subtly wrong at runtime.

## Stability

Pre-1.0. SemVer applies only post-1.0; a `0.1 → 0.2` bump may break.

## More

- [Plugin authoring guide](https://github.com/dhruvasagar/lattice/blob/main/docs/dev/guides/plugin-authoring.md)
- [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin) —
  a complete worked example: org-mode as a component, across more than a dozen
  seams.

## License

MIT
