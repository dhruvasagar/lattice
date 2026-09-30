<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `keymap`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `keymap-plugin` (imports)

The `keymap` guest→host binding-registration seam (PL8.D.1).

Mirrors the native `KeymapHandle` write path. The first (and canonical)
consumer is the user's `init.rs`: plain global keybinds — the one config kind
with no other seam — register here. A binding names an EXISTING command (by
name, resolved against the `CommandRegistry`) and lands in
[`KeymapLayer::User`], gated by `KeymapCapability::User` — above the built-in
vim grammar, never in `KeymapLayer::Builtin` (the standing keymap-ownership
rule; user config layers on top).

Registration-only: the guest declares bindings once (at `register-keymap`);
binding *resolution* on every keystroke stays native (`KeymapHandle` trie
lookup) — no per-keystroke WASM. So this rides the async linker like `config`
/ `events`, not the sync grammar linker.

This is the CANONICAL, language-agnostic keybinding API — any component-model
language calls `register-binding` directly.

## Functions (1)

### `register-binding`

```wit
register-binding: func(binding-mode: binding-mode, chord: string, command: string) -> bool
```

Bind `chord` in `binding-mode` to an EXISTING command named `command`
(resolved against the `CommandRegistry` at registration), landing in
`KeymapLayer::User`. `chord` is a vim-notation chord sequence (`<leader>f`,
`<C-s>`, `gd`). Returns `false` (binding nothing) if the chord is
unparseable, the command is unregistered, or the User-layer capability was
withheld — a plugin never silently mis-binds. The keystroke path is
unaffected until the binding lands.

## Types (1)

### enum `binding-mode`

```wit
enum binding-mode {
    normal,
    insert,
    visual,
    select,
    replace,
    command,
    search,
}
```

The vim binding mode a keybinding lives in — the plugin-facing subset of
the native `BindingMode` (the transient operator-pending / after-key
states are internal grammar states, not plugin-bindable). Matches the
`modes` seam's `binding-mode` (the same native mapping).

