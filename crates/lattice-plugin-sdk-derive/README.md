# lattice-plugin-sdk-derive

Derive macros for [`lattice-plugin-sdk`](https://crates.io/crates/lattice-plugin-sdk)
— currently `#[derive(ConfigShape)]`.

You do not depend on this crate directly. Depend on `lattice-plugin-sdk`, which
re-exports the macros; this is published only because a proc-macro crate cannot
be bundled into its own consumer.

It shares one version with `lattice-plugin-sdk` and `lattice-wit`, whose
`major.minor` is the WIT package's — see those crates' READMEs, or the
[plugin authoring guide](https://github.com/dhruvasagar/lattice/blob/main/docs/dev/guides/plugin-authoring.md).

## License

MIT
