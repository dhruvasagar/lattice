# Using help-guest

This page exists to prove one thing: a plugin's markdown ships **inside
its own component**, not in a directory somebody has to copy it into.

The body you are reading was `include_str!`'d at build time, baked into
`help-guest.wasm`, and handed across the WIT boundary once at load.

| What | Where it lives |
|---|---|
| The code | `help-guest.wasm` |
| This page | `help-guest.wasm` |

See [the index](help:index) — a cross-link to a *builtin* topic, which
resolves because plugin pages land in the same registry the builtins do.
