# lattice-wit

The WIT interface package for [lattice](https://github.com/dhruvasagar/lattice)
plugins — the API definitions a WebAssembly Component Model plugin builds
against.

Lattice's plugin API *is* WIT, not a Rust crate you link. This crate exists to
get those `.wit` files onto your disk at build time, and to tell you which
generation of the API you are compiling against.

**Zero dependencies, deliberately.** A crate that pulled in an editor crate
would defeat its own purpose — you would be compiling the editor in order to
compile against its API.

## Use

```toml
[build-dependencies]
lattice-wit = "0.1"
```

```rust
// build.rs
fn main() {
    lattice_wit::write_to("wit").expect("write the lattice WIT API package");
}
```

```rust
// src/lib.rs
wit_bindgen::generate!({ world: "picker-source-plugin", path: "wit" });
```

Add `/wit` to `.gitignore`. It is generated, and committing it is how a copy
drifts behind the editor.

## The version is the ABI generation

This crate's `major.minor` is always the WIT package's `major.minor`:

```
lattice-wit = "0.2"   ⟺   package lattice:plugin-host@0.2.x
```

So the dependency line above says which generation of the API you target.
Patch is this crate's own — a packaging fix ships as `0.2.1` and leaves the ABI
alone.

`lattice-plugin-sdk` and `lattice-plugin-sdk-derive` carry the same number.

## Declaring this dependency opts you out of automatic ABI tracking

Worth understanding before you add it. When the plugin manager builds your
plugin it first writes the **running editor's** WIT into your `wit/`. Your
`build.rs` then runs and overwrites that with the version you pinned — a pin
your repo declares deliberately beats the ambient refresh.

You want the dependency anyway, because without it `cargo build` outside the
editor has no `wit/` at all. But it means that when lattice moves to a newer
ABI generation, you are pinned to the old one: the editor warns, naming both
fingerprints, and your component then fails to instantiate. A rebuild does not
fix it — bump the pin, or drop the dependency and let the loader keep you
current.

## Stability

Pre-1.0, and SemVer applies only post-1.0. Under Cargo's 0.x rules a
`0.1 → 0.2` bump may break, and it will be used that way. What this crate buys
is not stability — it is the ability to *name* a generation instead of pointing
at a directory in someone's checkout, and to be told when you are behind.

The editor implements one ABI generation at a time. There is no compatibility
shim.

## More

- [Plugin authoring guide](https://github.com/dhruvasagar/lattice/blob/main/docs/dev/guides/plugin-authoring.md)
- [Plugin host design](https://github.com/dhruvasagar/lattice/blob/main/docs/dev/architecture/plugin-host.md)
- Browse the live API from inside the editor: `:describe-plugin-api <seam>`,
  or dump it with `:export-plugin-api markdown`.

## License

MIT
