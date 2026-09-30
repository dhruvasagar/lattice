<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `help`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `help-plugin` (imports), `project-plugin` (imports), `treesitter-context-plugin` (imports)

CR.3: plugin-contributed `:help` pages.

A plugin ships its own manual. The topic lands in the SAME registry the
builtin docs live in, so `:help <name>` opens it, `:help <Tab>` completes
it, markdown renders through the same pipeline, and `:describe-command`
can cross-link to it — with no host kind-branch anywhere.

### The body ships INSIDE the component

A plugin's markdown is `include_str!`'d at build time and baked into its
own `.wasm`, exactly the way lattice's own docs are baked into the lattice
binary. Docs and code are then one artefact with one lifetime: unloading
the plugin removes its pages, and a plugin that failed to load has left
none behind.

This is deliberately NOT a runtime doc directory. That model (designed
2026-07-29, retired 2026-08-22 — see `contributable-registries.md` §4)
would need plugins to copy markdown into a shared directory at install
time, which separates the docs from the thing that owns them.

### Data, not a callback

The body crosses ONCE, at registration, and the host keeps the string.
There is no `render-topic` export, because a help page does not change
between the moment the plugin loads and the moment someone reads it —
so nothing about the guest needs to stay alive to serve one. (Compare
`dashboard`, whose sections ARE functions of a live context and therefore
do keep a guest instantiated.)

### Where it runs

Once per load, on the loader's off-boot-thread task. Never on the
keystroke or frame path, and never again after the load.

## Functions (1)

### `register-topic`

```wit
register-topic: func(name: string, summary: string, body: string, related-commands: list<string>) -> result<_, string>
```

Register one free-form `:help` topic.

**Auto-namespaced**, like `config.register-option` and
`theme.register-element`: `name` is prefixed with the plugin's id, so a
plugin with id `fugitive` registering `status` contributes
`fugitive.status`. The host owns the namespace, so a plugin can neither
shadow a builtin page nor collide with another plugin.

**The single-page case keeps the bare id.** A `name` that is empty, or
that already equals the plugin's id, lands at the bare id — `:help
fugitive`, not `:help fugitive.fugitive`. A one-page plugin is the
common case and no editor's `:help` has ever looked like the latter.

`body` is markdown, rendered by the same help pipeline the builtin
docs use (tables, `[label](help:topic)` links, heading anchors).

`related-commands` are substring patterns matched against command
names; `:describe-command` walks them to emit a `See also` link, the
same way a builtin doc's frontmatter `related` list does.

`err` when the spec is malformed — never a trap, and never a
partially-registered topic. A rejected topic costs itself and nothing
else: the plugin's other pages still register.

**Example — Ship the plugin's `:help` page, embedded at build time** · [`plugins/comment/src/lib.rs`](../../../../plugins/comment/src/lib.rs)

```rust
let _ = help::register_topic(
    "",
    "Toggle line comments with `gc` — an operator, so it takes any motion or text object.",
    include_str!("../doc/comment.md"),
    &["comment".to_string()],
);
```

