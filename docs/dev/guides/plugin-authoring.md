# Plugin authoring guide

How to write a lattice plugin: the toolchain, the WIT package, the lifecycle,
the capability manifest, the per-seam surface, and how to build and test a guest
against the host runtime.

This is the **how-to** companion to two other docs:

- [`../architecture/plugin-host.md`](../architecture/plugin-host.md) — the design
  fragment: *what* the host is and *why* it is shaped this way (the exercised-trait
  → WIT-mirror spine, the capability/security model, rejected alternatives).
- [`../operations/slice-plans/archive/plugin-host.md`](../operations/slice-plans/archive/plugin-host.md)
  — the slice plan: what landed, in what order.

For the end-user view (the `:*-plugin-api` introspection commands and the model
at a glance), see [`../../user/plugins.md`](../../user/plugins.md).

> **Status — read this first.** The plugin **host runtime**
> (`lattice-plugin-host`) and the editor-side **loader and manager**
> (`lattice-plugin-loader`, `lattice-plugin-manager`) both ship. A running editor
> discovers plugins on disk, builds them from source against its own WIT, and
> loads them; `:plugins` manages them, `:plugin-load` / `:plugin-unload` /
> `:plugin-reload` drive them by hand, and your `init.rs` loads as a
> boot-capability config plugin. Four plugins ship bundled —
> [`auto-pair`](../../../plugins/auto-pair), [`comment`](../../../plugins/comment),
> [`project`](../../../plugins/project) and
> [`treesitter-context`](../../../plugins/treesitter-context) — and the org plugin
> ([`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin)) is
> the largest external one. For what is built versus planned, the
> [implementation ledger](../operations/implementation.md) is authoritative.
>
> **Where to go next.** This guide covers the toolchain, the ABI, the manifest
> and the runtime contract. The [patterns guide](plugin-patterns.md) walks
> through building each kind of contribution with code quoted from those
> plugins, and the [plugin-API reference](../reference/plugin-api.md) — generated
> from the WIT — has every signature, type and field.

---

## Toolchain

A plugin is a **WebAssembly Component Model** component targeting WASI
preview 2.

| Piece | Version / value | Notes |
|---|---|---|
| Target | `wasm32-wasip2` | `rustup target add wasm32-wasip2`. The host's `build.rs` warns + skips guest builds if the target is missing; CI installs it. |
| Guest bindings | `wit-bindgen = "0.58"` | Generates the guest-side Rust bindings from the WIT package. |
| Host runtime | `wasmtime = "46"` + `wasmtime-wasi = "46"` | Component Model + WASI preview 2. You only touch this when *testing* a guest against the host. |
| Crate type | `crate-type = ["cdylib"]` | A component, not an rlib. |
| Toolchain isolation | a **standalone `[workspace]`** | A guest must not inherit the host workspace's target/lints/RUSTFLAGS. Keep it a self-contained crate. |

Any Component Model language works in principle (Zig, Go, AssemblyScript, …);
the bindings, fixtures, and this guide use Rust.

## The WIT package

The canonical API is the WIT package under [`wit/`](../../../crates/lattice-wit/wit) — the plugin
API *is* WIT, not a Rust crate you link. Each `.wit` file is one seam; `types.wit`
holds the shared record/enum vocabulary; `plugin.wit` defines the lifecycle world
and composes the seam interfaces into per-seam **worlds** (`picker-source-plugin`,
`completion-source-plugin`, `grammar-plugin`, `events-plugin`, …).

Generate guest bindings by pointing `wit-bindgen` at the package and naming the
world you implement:

```rust
wit_bindgen::generate!({
    world: "picker-source-plugin",
    path: "wit",
});
```

`path` is `"wit"` — a directory beside your `Cargo.toml`, which you do **not**
write by hand and do **not** commit. Where it comes from is the next section,
and it is the single most consequential thing to understand about building
against lattice.

You never hand-write the API surface — browse it with `:describe-plugin-api
<seam>` in the editor, or dump it with `:export-plugin-api markdown` / `json` to
generate scaffolding.

## ABI, versions, and what happens when lattice moves

The question this section answers: **a plugin was built months ago against an
older API. The user upgrades lattice. What happens?**

### Three versions, and they are not the same thing

| | What it is | Where it lives |
|---|---|---|
| **The WIT package version** | The ABI identity. `package lattice:plugin-host@0.2.0` at the top of every `.wit` file. Component Model bakes it into the interface names your component imports, so the host either provides `lattice:plugin-host/buffer@0.1.0` or your component does not instantiate. | `crates/lattice-wit/wit/*.wit` |
| **The `lattice-wit` crate version** | The delivery vehicle — the crate that carries those files to you. Versioned independently of the editor, because the ABI does not change every time the editor does. | `crates/lattice-wit/Cargo.toml` |
| **The editor version** | `lattice --version`. Says nothing directly about the ABI. | `[workspace.package]` |

A plugin does not "target lattice 0.9". It targets an ABI generation.

### Plugins ship as source, and are built on boot

This is the part that makes the whole model work, and it is unusual enough to
state plainly: **the plugin manager clones a plugin's source and compiles it on
the machine it will run on.** It does not download a prebuilt `.wasm`.

So the upgrade question is not "does this old binary still run" — it is "does
this source still compile, and against which WIT".

Rebuilds are cached on a `.build-stamp` recording two fingerprints
(`lattice-plugin-loader/src/build.rs`):

- `source` — the plugin's source tree;
- `abi` — `lattice_wit::ABI_FINGERPRINT`, an FNV-1a over the WIT files **the
  running editor carries**.

A stamp matching on *both* short-circuits to a pure load, so a warm boot with
nothing changed invokes no toolchain — a machine without Rust installed still
boots every already-built plugin. Either fingerprint differing forces a rebuild.

The `abi` half is not symmetry. A source that did not change, compiled against
an ABI that did, looks current under a source-only stamp: it gets loaded, fails
to instantiate, and says nothing. No amount of source fingerprinting can see
that, which is why the ABI is stamped explicitly.

### Where your `wit/` actually comes from

Two writers, and the order decides the outcome:

1. **The loader refreshes it before cargo runs.** `refresh_wit_package` writes
   the running editor's WIT into your source directory. Not the `lattice` that
   happens to be on `PATH` — *the process that is about to instantiate your
   component*. Left alone, this keeps every plugin automatically current: a new
   editor means a new ABI fingerprint, a forced rebuild, and a component built
   against the WIT it is about to be loaded by.

2. **Your `lattice-wit` build-dependency overwrites it, and wins**, because
   `build.rs` runs after the loader's refresh.

That second point is the one to internalise:

> **Declaring a `lattice-wit` build-dependency opts you OUT of automatic ABI
> tracking.** A pin your repo declares deliberately overrides the ambient
> refresh.

You still want it, because without it `cargo build` outside the editor has no
`wit/` at all and cannot compile. The cost is that the version you pin is the
ABI you get, including when the editor has moved on.

```toml
[build-dependencies]
lattice-wit = "0.2"      # this pin IS your ABI generation
```

```rust
// build.rs
fn main() {
    lattice_wit::write_to("wit").expect("write the lattice WIT API package");
}
```

Add `/wit` to `.gitignore`. Committing it is how a copy drifts behind the
editor: it happened here, three ABI changes in one day, and the only symptom
was a plugin that silently stopped loading.

### So: old plugin, new editor

Follow it through. Your plugin pins `lattice-wit = "0.1"`; the user upgrades to
an editor whose WIT has moved to `0.2`.

1. The editor's `ABI_FINGERPRINT` changed, so your stamp no longer matches and
   the manager rebuilds you. Good.
2. The loader refreshes `wit/` to the editor's 0.2 package. Then your `build.rs`
   overwrites it back to 0.1, because your pin wins.
3. You compile cleanly — against 0.1 — and your component imports
   `lattice:plugin-host/…@0.1.0`.
4. The host provides `@0.2.0`. The names do not match, and instantiation fails.

The editor does not hide this. `warn_if_abi_skewed` logs one `warn!` naming
both fingerprints — what you were built against, what this editor is — before
loading you anyway, on the principle that a coarse signal does not justify
refusing to try. If instantiation then fails, that line is already in the log
explaining why.

**A rebuild does not fix a generation mismatch.** Rebuilding against a pin
produces the same mismatched component. The fixes are yours to make:

- **bump the pin** — `lattice-wit = "0.2"`, fix whatever the compiler now
  objects to, release;
- **or drop the pin** and let the loader's refresh keep you current, accepting
  that a standalone `cargo build` then needs the editor to have run once.

### Integration tests that boot a real editor

Everything above concerns the **component**, whose dependencies are
`lattice-wit` and `lattice-plugin-sdk` from crates.io and nothing else. Tests
that drive a *running editor* — boot one, load your component through the
loader, dispatch chords — are a different problem, because they need the host
crates, and those are not published.

**Put them in a separate package.** Cargo resolves `[dev-dependencies]` as part
of the BUILD graph, not just the test graph, so a dev-dependency that cannot
resolve stops `cargo build --release --target wasm32-wasip2` — the command the
plugin manager runs on boot. Test-only dependencies in your component's
manifest therefore gate every user's install. `lattice-org-plugin` shipped that
way for months and could only be built on its author's laptop.

Give the test package its own `[workspace]` and exclude it from the root, so
the component's resolution never reaches it:

```toml
# integration/Cargo.toml
[dev-dependencies]
lattice-host = { path = "../../lattice/crates/lattice-host" }
# ...

[workspace]
```

```toml
# the component's Cargo.toml
[workspace]
exclude = ["integration"]
```

Two ways to name the host crates from there, and the trade is not obvious:

**Path dependencies to a sibling checkout** — what org does. Nothing extra on
disk, and editing lattice and your plugin together just works. The cost is that
running the tests requires that checkout, so contributors clone two repos.

**Git dependencies** — `{ git = "https://github.com/dhruvasagar/lattice" }`.
The tests then run from a bare clone of your plugin alone, which is friendlier
for CI and for a contributor who only wants to run them once. Cargo clones the
repo once and locks every crate to one commit, so it stays reproducible.

The cost is disk, and it is larger than it looks. Measured on lattice at
`32514ad`: a 194 MB bare repository under `~/.cargo/git/db/`, plus **~2 GB per
revision** under `~/.cargo/git/checkouts/`. The bulk is not source — the
plugin-host's `build.rs` compiles its guest fixtures into `target/` directories
*inside the source tree*, so a git checkout of lattice accumulates two
gigabytes of build output that cargo never prunes, once per revision your lock
has pointed at.

So: paths if you already keep a lattice checkout, git if you would rather trade
disk for not needing one. Neither belongs in the component's own manifest.

### One number: the crate version IS the ABI generation

The three published crates — `lattice-wit`, `lattice-plugin-sdk` and
`lattice-plugin-sdk-derive` — share one version, and its `major.minor` is
always the WIT package's `major.minor`:

```
lattice-wit = "0.2"   ⟺   package lattice:plugin-host@0.2.x
```

So the dependency line answers "which ABI generation am I compiled against"
without you looking anywhere else. Patch is the crates' own, which means a
packaging fix ships as `0.2.1` without pretending to be an ABI change.

This is why these crates are not `version.workspace = true`: the editor's
version cannot answer that question, because the ABI does not move when the
editor does.

`the_crate_versions_track_the_wit_package_version` enforces all of it —
that the crates agree with each other, that their `major.minor` matches the
package, and that all 36 `.wit` files declare the *same* package version. That
last check has no version rule behind it; it is there because one file drifting
produces a package that fails to parse or links only half its interfaces, and
nothing else would notice.

### What "0.x" promises, which is not much

The WIT is pre-1.0 and `plugin-host.md` §12 is explicit: **SemVer applies only
post-1.0**, and the ABI-freeze policy is a deferred design fragment. Under
Cargo's 0.x rules a `0.1 → 0.2` bump is allowed to break, and it will be used
that way.

What publishing `lattice-wit` buys is not stability — it is the ability to
*name* a generation instead of pointing at a directory in somebody's checkout,
and to be told when you are behind. Before it, the only way to express "this
plugin targets that API" was a filesystem path, which is why the reference org
plugin built on exactly one machine.

Concretely, expect:

- **Additive changes** — a new seam, a new function on an existing interface —
  to leave your plugin compiling and loading. You import only what you use.
- **A changed signature, a renamed record field, a removed function** to break
  you at compile time, which is the good case: you get a compiler error and not
  a plugin that loads and misbehaves.
- **A WIT package version bump** to break you at instantiation, which is the
  case the fingerprint warning exists to explain.

The editor runs **one ABI generation at a time**. There is no compatibility
shim and no side-by-side generation support; if that changes, it lands as the
§12 fragment and this section changes with it.

## Manifest, world and entry points

A plugin directory holds a component crate and a **`plugin.toml`** manifest.
The manifest declares identity and asks for capabilities; nothing in it is
executed. A malformed manifest is a typed error — the host logs it and skips
the plugin, never panics.

<!-- manifest -->
```toml
id = "my-plugin"                         # required; keys the plugin's data dir
doc = "One line shown by :describe-plugin."
provides = ["grammar", "modes", "config", "help"]   # the seams it implements
capabilities = [                         # OS + editor powers it requests
    "fs:read:~/notes",                   #   read under a path prefix
    "state:write",                       #   the plugin-private key/value store
    "grammar:chord",                     #   bind an operator's chord
]
editor_capabilities = ["tree-sitter"]    # subsystems a declared mode needs
default_modes = ["my-plugin-mode"]       # on by default, gated by my-plugin.enabled
```

| Key | Meaning |
|---|---|
| `id` | Required. A single safe path component; keys the per-plugin data directory. |
| `doc` | Shown by `:describe-plugin`. |
| `provides` | The seams the component implements — which of the loader's per-seam paths it drives. Empty means a lifecycle-only component (the base `plugin` world, as `init.rs` is). |
| `capabilities` | `fs:read:<prefix>`, `fs:write:<prefix>`, `net:http:<host>`, `proc:spawn` (bundled plugins only), `state:write` (the plugin store), `grammar:chord` (bind an operator's chord). Deny-by-default: the grant is the intersection of the request and the trust tier. |
| `editor_capabilities` | `buffer-uri`, `lsp`, `tree-sitter`, `folds`, `writable`, `diagnostics` — what a mode the plugin declares requires of the buffers it activates on. |
| `default_modes` / `default_mode` | Minor modes enabled by default. The loader registers a `<id>.enabled` option that gates them; either spelling works. |

The manifest above is parsed by the real parser in a test
(`lattice-plugin-host/tests/documented_manifests_parse.rs`), so every key and
capability form on this page is one the loader accepts.

**The world** decides what the component imports and exports; the
[worlds page](../reference/plugin-api/worlds.md) lists them all. A plugin
contributing to several seams declares its own world that composes the
per-seam ones — `comment-plugin`, `auto-pair-plugin` and `project-plugin` are
worked examples in `crates/lattice-wit/wit/`, and an external plugin can do
the same locally with WIT `include` (the `language-guest` fixture shows how).

**Entry points** are the `register-*` functions a world exports. The host
calls each once at load; inside them the plugin calls host imports to declare
what it contributes. The [patterns guide](plugin-patterns.md) shows each one
end to end.

## Choosing a seam

The [reference index](../reference/plugin-api.md) lists every seam
with its direction and capability; it is generated from the WIT and cannot be
out of date. What it cannot tell you is which seam a goal needs:

| You want to… | Seam | Pattern |
|---|---|---|
| add an operator, motion, text object, action or ex-command | `grammar` + `grammar-callbacks` | [operator](plugin-patterns.md#an-operator), [action](plugin-patterns.md#an-action-bound-to-keys), [motion / text object](plugin-patterns.md#a-motion-or-a-text-object), [ex-command](plugin-patterns.md#an-ex-command) |
| own a mode, its keymap and its options | `modes`, `config`, `keymap` | [mode + options](plugin-patterns.md#a-mode-that-owns-your-surface-and-its-options) |
| contribute a picker | `picker-registry` + `picker-source` | [picker](plugin-patterns.md#a-picker) |
| react to editor events, timers, file changes | `events`, `host-services` | [events](plugin-patterns.md#reacting-to-events-and-time) |
| read buffer text or the syntax tree | `buffer`, `tree-sitter` | [buffer + tree](plugin-patterns.md#reading-the-buffer-and-the-syntax-tree) |
| remember state between sessions | `host-services` (store) | [state](plugin-patterns.md#remembering-state-across-restarts) |
| ship `:help` pages, log to the trace | `help`, `logging` | [help + logging](plugin-patterns.md#shipping-help-and-logging) |
| add a completion source | `completion-source` | reference: [`completion-source`](../reference/plugin-api/completion-source.md) |
| add gutter signs or decorations | `signs`, `decorations` | reference: [`signs`](../reference/plugin-api/signs.md), [`decorations`](../reference/plugin-api/decorations.md) |
| add a language (grammar + queries) | `language` | reference: [`language`](../reference/plugin-api/language.md) |
| add a dashboard section, a transient menu, a multibuffer view | `dashboard`, `transient-source`, `multibuffer-view-source` | reference pages of those seams |
| teach lattice a build tool's error format | `error-parser` | reference: [`error-parser`](../reference/plugin-api/error-parser.md) |

In the editor, `:describe-plugin-api <seam>` shows the same reference and
`:export-plugin-api` dumps it as Markdown.

> **Reading a file from a grammar action: use `read-file`, not `std::fs`.**
> Grammar actions (motions, operators, text objects, `register-action` bodies,
> ex-commands) run on a **synchronous** linker so the host can call them on the
> dispatch thread. `wasmtime-wasi`'s sync filesystem shim blocks on a runtime
> internally, and that thread is already inside one — so `std::fs::read_to_string`
> in an action does not read a file, it **panics and takes your plugin down**.
> `host-services.read-file` is a host-side read gated on the same `fs:` grant,
> and it works from every seam.
>
> Async seams — `picker-source`, `completion-source`, `transient-source` — run on
> the async linker and may use `std::fs` directly. The distinction is invisible
> until it panics, so when in doubt use `read-file`.

### Sync or async, and why it matters

Most seams are async and off the keystroke path. **Three are synchronous**,
each for its own reason, and they share one linker:

- `grammar` — a plugin motion must resolve synchronously so it composes with
  its operator (`d` + plugin-motion) and keeps dot-repeat and macros
  synchronous.
- `error-parser` — parsing one line is a pure function of the line plus
  pending state, called in arrival order by a single reader. An async call per
  line would buy nothing and cost a suspend per line of build output.
- `dashboard` — `render-section` runs inside the compositor, which is building
  a page that is about to paint.

All three carry the Reflex-class fuel budget rather than the generous
lifecycle default, and **all three re-arm that budget per call** — fuel is
spent per call, so a seam invoked repeatedly must re-arm or it works for a
while and then traps permanently.

The grammar `apply` runs through a sync trampoline under that budget
(~10M fuel / 50 epoch ticks; measured ~340 ns release round-trip). **The
renderer itself never calls WASM** — that invariant is absolute, and it is why
`dashboard`'s render is on the *compositor* (which builds content) rather than
in a paint path.

## The runtime contract you author against

- **Capabilities are deny-by-default.** You get the *intersection* of what you
  request and what your trust tier allows. With no grant, no filesystem at all;
  with a grant, only `/data` (your private, always-writable data dir) plus your
  declared `fs` prefixes. `proc:spawn` is bundled-only.
- **Fuel + epoch budget.** Every call is bounded by a fuel cap and a wall-clock
  epoch deadline. Don't assume unbounded loops complete — a runaway call is
  trapped. Budgets are per-seam and re-armed per call (a fresh allowance each
  time), not a shared pool.
- **Traps quarantine you.** Out of fuel, past deadline, panic, or an OOB access
  → your instance is quarantined: one `PluginCrashed` event fires and every later
  call short-circuits. Fail gracefully; return errors as values (the WIT
  functions return `result<_, string>`), don't trap.

See [`../../user/plugins.md`](../../user/plugins.md#the-security-model) for the
model at a glance and [`../architecture/plugin-host.md`](../architecture/plugin-host.md)
for the full rationale (the audit doc covers the load-bearing invariants).

## Start from a real plugin

The bundled plugins are small, complete and built by CI against the current
WIT — the best templates there are:

| Plugin | Shows |
|---|---|
| [`comment`](../../../plugins/comment) | an operator with its own chord, a mode, an option, a help page — the smallest complete plugin |
| [`auto-pair`](../../../plugins/auto-pair) | actions that decline to fall through, reading options per call, tree-sitter scoping |
| [`project`](../../../plugins/project) | pickers, transient menus, ex-commands, events, the persistent store, structured options |
| [`treesitter-context`](../../../plugins/treesitter-context) | a context producer driven by compiled tree-sitter queries |

## Building + testing a guest

Out-of-tree, test against a running editor from a separate package — see
[Integration tests that boot a real editor](#integration-tests-that-boot-a-real-editor).

In-tree, the host crate's [`build.rs`](../../../crates/lattice-plugin-host/build.rs)
builds every guest under `crates/lattice-plugin-host/tests/fixtures/` and
`plugins/` to a component (stripping inherited target/RUSTFLAGS so the
standalone guest workspace compiles cleanly for `wasm32-wasip2`) and exposes
each artifact's path as an env var (`COMMENT_PLUGIN_WASM`, …). A guest that
fails to compile fails the build when the wasm target is installed, as it is
in CI. Drive one from a test through the host API:

```rust
let host = PluginHost::new()?;                        // or with_dirs(...) for cache/data
let component = host.compile(&std::fs::read(env!("COMMENT_PLUGIN_WASM"))?)?;
let manifest = PluginManifest::new("my-plugin", requested_caps, editor_caps);
let plugin = host
    .instantiate_plugin(&component, &manifest, TrustTier::Bundled, budget)
    .await?;
// then the per-seam path, e.g. host.spawn_picker_source(...).await
```

Every seam has its own `spawn_*` (async seams) or `instantiate_grammar_plugin`
(the sync grammar seam); the fixtures' tests are the working examples. Ship
happy-path **and** failure-mode tests — trap isolation, denied capabilities,
malformed input — as every seam in the tree does.
