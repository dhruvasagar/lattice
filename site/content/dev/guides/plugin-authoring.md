+++
title = "Plugin authoring guide"
+++

# Plugin authoring guide

How to write a lattice plugin: the toolchain, the WIT package, the lifecycle,
the capability manifest, the per-seam surface, and how to build and test a guest
against the host runtime.

This is the **how-to** companion to two other docs:

- [`../architecture/plugin-host.md`](../architecture/plugin-host) — the design
  fragment: *what* the host is and *why* it is shaped this way (the exercised-trait
  → WIT-mirror spine, the capability/security model, rejected alternatives).
- [`../operations/slice-plans/plugin-host.md`](../operations/slice-plans/plugin-host)
  — the slice plan: what landed, in what order.

For the end-user view (the `:*-plugin-api` introspection commands and the model
at a glance), see [`../../user/plugins.md`](../../user/plugins).

> **Status — read this first.** Phase 7 shipped the plugin **host runtime**
> (`lattice-plugin-host`): the WIT package, the capability/fuel/crash-isolation
> model, and every extension seam, each exercised end-to-end by a real guest
> fixture. **What does not exist yet is the editor-side loader** — `lattice-host`
> has no dependency on `lattice-plugin-host` (a guard test enforces this), there
> is no `:load-plugin` command, no on-disk plugin discovery, and no `init.rs`
> path. Those are **Phase 8** (the plugin *manager*).
>
> Concretely: you can write a Component Model guest against the WIT package today
> and drive it through the `lattice-plugin-host` API in a test or bench — that is
> exactly how the `fuzzy-finder` plugin and the eight `tests/fixtures/*-guest`
> fixtures work. You **cannot** yet drop a `.wasm` into a config directory and
> have a running editor pick it up. This guide covers the reachable part and
> flags every Phase-8 gap inline.

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

The canonical API is the WIT package under [`wit/`](../../../wit) — the plugin
API *is* WIT, not a Rust crate you link. Each `.wit` file is one seam; `types.wit`
holds the shared record/enum vocabulary; `plugin.wit` defines the lifecycle world
and composes the seam interfaces into per-seam **worlds** (`picker-source-plugin`,
`completion-source-plugin`, `grammar-plugin`, `events-plugin`, …).

Generate guest bindings by pointing `wit-bindgen` at the package and naming the
world you implement:

```rust
wit_bindgen::generate!({
    world: "picker-source-plugin",
    path: "../../wit",
});
```

You never hand-write the API surface — browse it with `:describe-plugin-api
<seam>` in the editor, or dump it with `:export-plugin-api markdown` / `json` to
generate scaffolding.

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

Status legend: **usable** = a real guest drives it end-to-end today (in a
host-crate test); **partial** = wired with a named deferred piece;
**type-mirror** = the WIT types exist but the interface has no functions yet.

| Seam / world | Status | You implement / call |
|---|---|---|
| `picker-source` (`picker-source-plugin`) | **usable** | Export `spec` / `init` / `accept`; produce candidates, route an accept to an editor action. *Deferred:* reading buffer text (the `document` borrow) and live/stream sources. |
| `grammar` (`grammar-plugin`) | **usable** | Call `register-motion` / `register-operator` / `register-text-object` / `register-action` / `register-ex-command` from `activate`; the `apply` callback runs **synchronously** on the keystroke under a sub-frame budget. |
| `events` (`events-plugin`) | **usable** | `subscribe` to typed events; your sink is invoked off the hot path. |
| `config` (`config-plugin`) | **usable** | `register-option` / `get-option` against the same registry `:set` reads. |
| `modes` (`modes-plugin`) | **usable** | `register-mode` (kind, policy, capabilities, keymap) into the mode registry. Full modes-as-components is Phase 8. |
| `completion-source` | **partial** | Export `generate` (async, off-keystroke — the LSP pattern). `Matcher` / `Ranker` / `Annotator` are type-mirrored; matching/ranking stay native for now. |
| `decorations` | **partial** | Produce gutter decorations as an off-render producer. *Deferred:* the renderer *reading* the decoration cache (Phase-8 boot-wiring) — your producer runs, but nothing paints it yet. |
| `host-services` | **partial** | Call back into the editor. Only `walk` (capability-gated filesystem enumeration) is implemented; `emit-event` / `register-event` exist but no-op without a wired bus. `net` / `proc` / tree-sitter host-services are not implemented. |
| `command`, `ui` | **type-mirror** | Reserved. `ui` types (`ui-segment`, `ui-notification`, `ui-zone`) exist; the emit functions do not. |

Run `:describe-plugin-api <seam>` for exact signatures.

### The one synchronous seam: `grammar`

Every seam is async / off-keystroke **except grammar**. A plugin motion,
operator, or text object must resolve *synchronously* so it composes with its
operator (`d` + plugin-motion) and keeps dot-repeat and macros synchronous. The
host runs the grammar `apply` through a sync trampoline under a tight "Reflex"
fuel budget (~10M fuel / 50 epoch ticks; measured ~340 ns release round-trip).
The renderer itself never calls WASM — that invariant is absolute.

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

See [`../../user/plugins.md`](../../user/plugins#the-security-model) for the
model at a glance and [`../architecture/plugin-host.md`](../architecture/plugin-host)
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

## What's deferred to Phase 8

The runtime is done; making it reachable from a running editor is Phase 8:

- **The plugin manager / loader** — on-disk discovery + `manifest.toml` parsing
  off disk + the load path into `lattice-host`.
- **`init.rs` as configuration** — user config compiled to WASM, loaded with a
  boot-capability set.
- **Decoration rendering** — the renderer reading plugin-produced decorations.
- **Full modes-as-components** — bundled major/minor modes shipping as plugins.
- **User-installed trust flow** — the consent prompt that narrows a
  user-installed plugin's grant (bundled plugins are pre-granted today).

Until then, author and test guests against the host library — the seam surface
you write to now is the surface the loader will feed.
