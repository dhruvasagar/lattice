# Plugin authoring guide

How to write a lattice plugin: the toolchain, the WIT package, the lifecycle,
the capability manifest, the per-seam surface, and how to build and test a guest
against the host runtime.

This is the **how-to** companion to two other docs:

- [`../architecture/plugin-host.md`](../architecture/plugin-host.md) — the design
  fragment: *what* the host is and *why* it is shaped this way (the exercised-trait
  → WIT-mirror spine, the capability/security model, rejected alternatives).
- [`../operations/slice-plans/plugin-host.md`](../operations/slice-plans/plugin-host.md)
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
| `host-services` | **partial** | Capability-gated callbacks. Only `walk` (filesystem enumeration) is implemented; `emit-event` / `register-event` exist but no-op without a wired bus. |
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
