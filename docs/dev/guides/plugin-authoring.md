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

> **Status — read this first.** Phase 7 shipped the plugin **host runtime**
> (`lattice-plugin-host`): the WIT package, the capability/fuel/crash-isolation
> model, and every extension seam, each exercised end-to-end by a real guest
> fixture. **Phase 8 shipped the editor-side loader** (`lattice-plugin-loader`):
> `lattice-host` now depends on the host transitively, and a running editor loads
> plugins from `${XDG_DATA_HOME}/lattice/plugins/` (on-disk discovery) or on demand
> via `:plugin-load <path>` / `:plugin-unload <name>` / `:plugin-reload <name>`,
> manages them in the `:plugins` view, and loads your `init.rs` as a
> boot-capability config plugin. Plugin **observability** (`:plugin-trace`, the
> `plugin.trace-level` option, and the `wasi:logging` guest import) ships too.
>
> So you can now **both** drive a guest through the `lattice-plugin-host` API in a
> test/bench (how the `fuzzy-finder` plugin and the `tests/fixtures/*-guest`
> fixtures work) **and** drop a built `.wasm` + `manifest.toml` into the plugins
> directory and have a running editor pick it up. The frontier is the bundled
> first-party reference plugins + shipping the built-in modes as components (8b).

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
| **The WIT package version** | The ABI identity. `package lattice:plugin-host@0.1.0` at the top of every `.wit` file. Component Model bakes it into the interface names your component imports, so the host either provides `lattice:plugin-host/buffer@0.1.0` or your component does not instantiate. | `crates/lattice-wit/wit/*.wit` |
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
lattice-wit = "0.1"      # this pin IS your ABI generation
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

## Lifecycle + manifest

Every plugin implements the `plugin` lifecycle world: `activate()` on load,
`deactivate()` on unload. Seam contributions are registered from `activate()`.

A plugin ships a **`manifest.toml`** declaring its identity and the capabilities
it requests. (The host consumes an already-parsed `PluginManifest`; on-disk
discovery of this file is the Phase-8 manager's job — today the manifest is
built programmatically in tests, but the committed TOML format is stable.)

```toml
# manifest.toml
id = "my-plugin"                       # required, non-empty; keys the data dir
capabilities = [                       # OS capabilities (the plugin's WASI view)
    "fs:read:/home/me/notes",          #   read under a path prefix
    "net:http:api.example.com",        #   HTTP to a host (via a gated host-service)
]
editor_capabilities = ["tree-sitter"]  # editor subsystems a declared mode needs
doc = "One-line description shown by :describe-plugin."
```

Capability forms: `fs:read:<prefix>`, `fs:write:<prefix>`, `net:http:<host>`,
`proc:spawn` (bundled-only). Editor capabilities: `buffer-uri`, `lsp`,
`tree-sitter`, `folds`, `writable`, `diagnostics`. A malformed capability or an
empty `id` is a typed parse error — the host logs and skips a bad manifest, never
panics.

## The seams

`wit/` **is** the API, and the catalog `:describe-plugin-api` reads is parsed
from it at build time — so that command can never disagree with the WIT. This
table can, which is why it says what each seam is *for* and leaves signatures
to the catalog.

Status legend: **usable** = a real guest drives it end to end in a host-crate
test; **partial** = wired with a named deferred piece; **type-mirror** = the
WIT types exist but the interface has no functions yet.

### Contribution seams — what you put in `provides`

| Seam / world | Status | You implement / call |
|---|---|---|
| `grammar` (`grammar-plugin`) | **usable** | `register-motion` / `register-operator` / `register-text-object` / `register-action` / `register-ex-command`. The `apply` callback runs **synchronously on the keystroke** — see below. |
| `picker-source` (`picker-source-plugin`) | **usable** | Export `spec` / `init` / `accept`; produce candidates, route an accept to an editor action. |
| `completion-source` | **partial** | Export `generate` (async, off-keystroke — the LSP pattern). `Matcher` / `Ranker` / `Annotator` are type-mirrored; matching and ranking stay native. |
| `events` (`events-plugin`) | **usable** | `subscribe` to typed events; your sink is invoked off the hot path. |
| `config` (`config-plugin`) | **usable** | `register-option` / `get-option` against the same registry `:set` reads. Auto-namespaced by your plugin id. |
| `modes` (`modes-plugin`) | **usable** | `register-mode` (kind, policy, capabilities, keymap, **options**). The editor auto-generates the `:<mode>` toggle, and it shows in `:list-modes` / `:describe-mode`. `options` declares what the mode's buffers need (`foldmethod=syntax`) — resolved against the same registry `:set` writes to, applied as a resolution *layer* for those buffers only. An entry naming an unknown option, or carrying a value the option rejects, is skipped with a warning and the rest of the set still applies. An option the plugin registered *itself* through the `config` seam cannot be overridden yet — it has no native type identity. |
| `keymap` (`keymap-plugin`) | **usable** | Bind user keys above the built-in grammar — the `init.rs` keybinding path. |
| `decorations` | **usable** | Produce gutter decorations as an off-render producer. The host refreshes them off the render path and the gutter repaints off-keystroke (PL8.E). |
| `context` (`context-plugin`) | **usable** | The sticky-context producer — walks a handed `tree-snapshot`, host-cached per parse version. |
| `theme` (`theme-plugin`) | **usable** | `register-element` with a default style. Your element lands in the registry builtins use, so themes override it and `:customize` edits it. Auto-namespaced. |
| `error-parser` (`error-parser-plugin`) | **usable** | `feed` one compilation-output line at a time, `reset` between runs — teach lattice a build tool's diagnostic format. **Sync**, and on a fast producer's critical path. |
| `help` (`help-plugin`) | **usable** | `register-topic` — ship your own `:help` pages. Bodies are `include_str!`'d into your component; names are auto-namespaced. |
| `dashboard` (`dashboard-plugin`) | **usable** | `register-section` + `render-section` — add or replace a `:dashboard` block, rendered from the live ctx on every compose. **Sync**, on the compositor. |
| `plugin-manager` (`plugin-manager-plugin`) | **usable** | `require` — declare the plugins you want; the host resolves, builds and loads them off-thread. A config-guest seam (`init.rs`), and a strictly larger authority than setting an option. |

### Host APIs — you import these, they are not `provides` entries

| Interface | Status | What it gives you |
|---|---|---|
| `host-services` | **partial** | Capability-gated callbacks. `walk` (filesystem enumeration) and `read-file` are implemented; `emit-event` / `register-event` exist but no-op without a wired bus. |

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
| `project` | **usable** | Resolve a buffer or path to its project root. Walks the filesystem on a cache miss — which is why `error-parser-plugin` deliberately does **not** import it. |
| `tree-sitter` | **usable** | Query the parse tree through a handed `tree-snapshot` borrow. |
| `buffer` | **usable** | Read buffer text through a `document` borrow. |
| `logging` | **usable** | Emit your own log narrative into the boundary trace (Layer 2) — see below. |
| `command`, `ui` | **type-mirror** | Reserved. `ui` types (`ui-segment`, `ui-notification`, `ui-zone`) exist; the emit functions do not. |

Run `:describe-plugin-api <seam>` for exact signatures, `:list-plugin-apis`
for the whole set, and `:export-plugin-api` to dump it as Markdown or JSON.

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

## Worked example: `fuzzy-finder`

The reference plugin lives at [`plugins/fuzzy-finder/`](../../../plugins/fuzzy-finder)
— a `wasm32-wasip2` guest implementing the `picker-source-plugin` world. It
replicates the native `files` picker to prove the substrate end-to-end (parity +
overhead), and is a **validation artifact, not a shipped plugin**: it uses a
distinct id (`"fuzzy-finder"`, not `files`) precisely so it is an *additive
custom source*, never a cutover — built-in sources stay native Rust.

Its shape is the template for any picker plugin:

```rust
wit_bindgen::generate!({ world: "picker-source-plugin", path: "../../wit" });

use exports::lattice::plugin_host::picker_source::{CandidatePair, Guest};
use lattice::plugin_host::host_services::walk;               // gated fs enumeration
use lattice::plugin_host::types::{PickerContext, PickerSourceSpec, /* … */};

struct Component;

impl Guest for Component {
    fn spec() -> PickerSourceSpec { /* id, doc, args_hint, live … */ }

    fn init(ctx: PickerContext, args: Vec<String>) -> Result<Vec<CandidatePair>, String> {
        // resolve a root (args[0] or ctx's projected workspace root),
        // call `walk(...)` (capability-gated), map each path to a
        // RawCandidate + a RoutingPayload::OpenFile.
    }

    fn accept(/* … */) -> Result<PickerAcceptOutcome, String> { /* route to OpenFile */ }
}

export!(Component);
```

Because the host's `walk` reuses the same `walk_files_for_picker` the native
source uses, the candidate set matches native by construction — the parity test
(`crates/lattice-plugin-host/tests/fuzzy_finder_parity.rs`) formalises it.

## Building + testing a guest

The host crate's [`build.rs`](../../../crates/lattice-plugin-host/build.rs) builds
each guest to a component (stripping inherited target/RUSTFLAGS so the standalone
guest workspace compiles cleanly for `wasm32-wasip2`) and exposes the bytes as a
`const` (e.g. `FUZZY_FINDER_WASM`). The eight fixtures under
`crates/lattice-plugin-host/tests/fixtures/*-guest/` follow the same pattern —
each is a minimal guest exercising one seam.

To drive a guest from a test, use the host API:

```rust
let host = PluginHost::new()?;                       // or with_dirs(...) for cache/data
let component = host.compile(GUEST_WASM)?;
let manifest  = PluginManifest::new("my-plugin", requested_caps, editor_caps);
let plugin    = host.instantiate_plugin(&component, &manifest, TrustTier::Bundled, budget)?;
// then the per-seam spawn, e.g.:
let source = host.spawn_picker_source(/* … */)?;     // picker seam
```

Per-seam entry points: `spawn_picker_source`, `spawn_completion_source`,
`spawn_event_plugin`, `spawn_decoration_source`, `spawn_config_plugin`,
`spawn_mode_plugin`, `instantiate_grammar_plugin`. Every slice ships happy-path
**and** failure-mode tests (trap isolation, denied capabilities, malformed
manifest) — mirror that when adding a plugin.

## Shipped in Phase 8 (and what's still ahead)

The runtime is reachable from a running editor now:

- **The plugin loader** — on-disk discovery + `manifest.toml` parsing +
  `:plugin-load` / `:plugin-unload` / `:plugin-reload`, the `:plugins` manager
  view. ✅
- **`init.rs` as configuration** — user config compiled to WASM, loaded with a
  boot-capability set, auto-reloaded on rebuild. ✅
- **Observability** — the boundary trace (`:plugin-trace`), the live
  `plugin.trace-level` option, and the `wasi:logging` guest import so a plugin
  narrates its own work into the trace buffer. ✅ (see
  [`../architecture/plugin-observability.md`](../architecture/plugin-observability.md))

Still ahead (8b):

- **Decoration rendering** — the renderer reading plugin-produced decorations
  (the producer half works).
- **Full modes-as-components** — bundled major/minor modes shipping as plugins.
- **User-installed trust flow** — the consent prompt that narrows a
  user-installed plugin's grant (bundled plugins are pre-granted today).
- **Bundled first-party reference plugins** — git-gutter, auto-pair, etc.

### The `logging` seam

Any async-world guest can emit its own log lines into the boundary trace via the
`wasi:logging`-shaped import:

```rust
use lattice::plugin_host::logging::{self, Level};
logging::log(Level::Info, "parser", "reindexed 40 files");
```

The host tags each line with the plugin id + level and routes it into the same
`*plugin-trace*` buffer as the boundary trace, gated by the plugin's
`plugin.trace-level`. `context` is a free-form category; `critical` folds into the
host's `error` level. It's an async-linker import (never the sync grammar seam),
so it can't touch the keystroke path.
