<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `plugin-manager`

**Direction:** guest calls into the host through it · **Capability:** subprocess · **Worlds:** `plugin-manager-plugin` (imports)

PM.7: the `require` seam — how a user's `init.rs` declares the plugins it
wants (plugin-manager.md §3).

This is the **user**-plugin surface. Core plugins (the ones that ship with
lattice) are NOT `require`d: they are discovered from the runtime root and
enabled by a `<id>.enabled` config gate (§7), so a fresh editor with no
user `init.rs` still gets its batteries. `require` exists for plugins the
*user* names, with a source the host must resolve and build.

It is programmatic rather than a TOML list on purpose (§3, rejected
alternatives). use-package is programmatic — conditional loading, per-plugin
setup — and the standing principle is that logic stays code while static
settings stay declarative. A `[[plugin]]` table would be simpler and would
lose exactly the expressiveness the feature is for.

### Recording, not doing

`require` **records** a spec and returns immediately. It performs no
resolution, no clone, no build, no load. The host drains the recorded specs
after the guest's registration export returns and runs the pipeline
off-thread (§5) — the `register-mode` / `register-grammar` precedent.

That split is not an implementation detail. A `require` that resolved
inline would put a git clone and a cargo build inside a guest call on the
boot path, which paramount goal #1 forbids outright and which would make a
cold first boot hang on the network with no way to render a frame.
Contributions from a required plugin therefore appear a frame or two after
boot — the eventual consistency the UX contract already permits for plugin
cold-start.

## Functions (1)

### `require`

```wit
require: func(spec: plugin-spec) -> bool
```

Declare a plugin. Records the spec; the host resolves, builds and loads
it after the calling export returns.

Returns `false` when the spec is rejected outright — today, an unsafe
`name`. A rejection is a logged skip, never a trap: one bad entry in an
`init.rs` must not take the whole config down.

**Example — Declare a pinned git plugin and a prebuilt-wasm plugin from `register-plugins`** · [`crates/lattice-plugin-host/tests/fixtures/plugin-manager-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/plugin-manager-guest/src/lib.rs)

```rust
// A pinned git source.
plugin_manager::require(&PluginSpec {
    name: "git_demo".to_string(),
    source: PluginSource::Git(GitSource {
        url: "https://example.invalid/demo.git".to_string(),
        rev: Some("abc123".to_string()),
    }),
    enable_mode: None,
    pinned: true,
});

// A prebuilt download — no build, no toolchain.
plugin_manager::require(&PluginSpec {
    name: "prebuilt-demo".to_string(),
    source: PluginSource::Prebuilt("https://example.invalid/d.wasm".to_string()),
    enable_mode: None,
    pinned: false,
});
```

## Types (3)

### record `git-source`

```wit
record git-source {
    url: string,
    rev: option<string>,
}
```

A git source. `rev` pins a revision; omitted tracks the default branch.

### variant `plugin-source`

```wit
variant plugin-source {
    local(string),
    git(git-source),
    prebuilt(string),
}
```

Where a plugin comes from. Mirrors the host's `PluginSource`.

**Cases**

- `local`: `string` — A cargo project on disk, built in place (never copied).
- `git`: [`git-source`](#record-git-source) — A git repository, cloned into the source cache.
- `prebuilt`: `string` — A URL serving a ready-built `.wasm` — no build, no toolchain.

### record `plugin-spec`

```wit
record plugin-spec {
    name: string,
    source: plugin-source,
    enable-mode: option<string>,
    pinned: bool,
}
```

One declared plugin.

**Fields**

- `name`: `string` — The plugin's name — the directory it caches under, and the key the
  host reports it by. Must be a single safe path component; the host
  rejects anything else rather than letting a name escape the cache
  root (the same validation the manifest `id` already gets).
- `source`: [`plugin-source`](#variant-plugin-source)
- `enable-mode`: `option<string>` — use-package sugar: enable this mode once the plugin loads.
  Desugars to the CI.5 `on-plugin-loaded` → `enable-mode` path, so
  the host never learns a mode-id statically
  (`feedback_mode_owns_its_surface`).
- `pinned`: `bool` — Skip the rebuild-on-change check; build only if the artifact is
  absent. The escape hatch for a known-good build that should stay
  put regardless of what the source tree does.

