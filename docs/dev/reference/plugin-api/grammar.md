<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `grammar`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `grammar-plugin` (imports), `project-plugin` (imports), `treesitter-context-plugin` (imports)

The grammar-**extension** API (plugin-host.md §4.1, PH7.7). This is the
surface a plugin calls to *contribute* new vim grammar —
`register_{motion,operator,text_object,ex_command,action}` — mirroring the
native `CommandRegistry::register_*` imperative API. The host provides these
functions; the guest **imports** them and calls them (from its
`register-grammar` export). Each records the contribution into `PluginState`;
after `register-grammar` returns, the host builds a native `*Spec` with a
trampoline `apply` stamped `SourceLayer::Plugin(id)` and registers it into the
SAME `CommandRegistry` a builtin lives in (PH7.7c) — so a plugin command is
indistinguishable from a builtin to the dispatcher (paramount #3).

The grammar *handling* (dispatcher, `:`-line + chord parser, operator∘motion
composition, ranges, counts, registers) stays native, sync, and untouched; a
plugin only adds entries here. `spec` carries the metadata; the behavior is a
guest export in `grammar-callbacks`, dispatched by a guest-chosen `callback`
id (the PH7.3d trampoline pattern). Registration returns nothing — the guest
dispatches by its own `callback`, and the host stamps the `CommandId` /
provenance (a plugin cannot forge either, §6).

## Uses

- [`motion-spec`](types.md#record-motion-spec) from [`types`](types.md)
- [`operator-spec`](types.md#record-operator-spec) from [`types`](types.md)
- [`text-object-spec`](types.md#record-text-object-spec) from [`types`](types.md)
- [`ex-command-spec`](types.md#record-ex-command-spec) from [`types`](types.md)
- [`action-spec`](types.md#record-action-spec) from [`types`](types.md)

## Functions (5)

### `register-action`

```wit
register-action: func(name: string, doc: string, spec: action-spec, callback: u32)
```

Contribute a chord-bound action. `callback` → `grammar-callbacks.apply-action`.

**Example — Register one action per chord from a table, each with its own callback id** · [`plugins/auto-pair/src/lib.rs`](../../../../plugins/auto-pair/src/lib.rs)

```rust
let spec = || ActionSpec {
    args_schema: Vec::new(),
};
for (name, doc, cb) in [
    ("auto-pair-open-round", "insert ()", CB_OPEN_ROUND),
    ("auto-pair-open-square", "insert []", CB_OPEN_SQUARE),
    ("auto-pair-open-curly", "insert {}", CB_OPEN_CURLY),
    ("auto-pair-close-round", "step over )", CB_CLOSE_ROUND),
    ("auto-pair-close-square", "step over ]", CB_CLOSE_SQUARE),
    ("auto-pair-close-curly", "step over }", CB_CLOSE_CURLY),
    ("auto-pair-quote-double", "pair \"\"", CB_QUOTE_DOUBLE),
    ("auto-pair-quote-single", "pair ''", CB_QUOTE_SINGLE),
    ("auto-pair-quote-backtick", "pair ``", CB_QUOTE_BACKTICK),
    (
        "auto-pair-close-manual",
        "close the nearest unmatched opener in scope (manual style)",
        CB_CLOSE_MANUAL,
    ),
    (
        "auto-pair-backspace",
        "delete an empty pair, else fall through to normal backspace",
        CB_BACKSPACE,
    ),
] {
    grammar::register_action(name, doc, &spec(), cb);
}
```

### `register-ex-command`

```wit
register-ex-command: func(name: string, doc: string, spec: ex-command-spec, parse-callback: u32, apply-callback: u32)
```

Contribute an ex-command. TWO callbacks — `parse-callback` →
`grammar-callbacks.parse-ex-args` (the `:` line's rest → typed `args`),
`apply-callback` → `grammar-callbacks.apply-ex-command`.

**Example — Register an argument-less ex-command with its parse and apply callbacks** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
lattice::plugin_host::grammar::register_ex_command(
    "project-switch",
    "Choose a project, then act on it. The verb this whole plugin \
     exists for: every other project-aware surface roots itself at the \
     buffer you are standing in, which is right until you want the one \
     you are not.",
    &ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    },
    CB_PARSE,
    CB_SWITCH,
);
```

### `register-motion`

```wit
register-motion: func(name: string, doc: string, spec: motion-spec, callback: u32)
```

Contribute a motion. `callback` is the id the host passes back to
`grammar-callbacks.apply-motion` on dispatch.

**Example — Register a linewise, non-jump motion answered by callback 1** · [`crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs)

```rust
grammar::register_motion(
    "down-n",
    "jump count lines down (fixture)",
    &MotionSpec {
        jump: false,
        exclusive: false,
        args_schema: Vec::new(),
    },
    1,
);
```

### `register-operator`

```wit
register-operator: func(name: string, doc: string, spec: operator-spec, callback: u32)
```

Contribute an operator. `callback` → `grammar-callbacks.apply-operator`.

**Example — Register an operator with its own chord (`gc`, doubled `gcc`)** · [`plugins/comment/src/lib.rs`](../../../../plugins/comment/src/lib.rs)

```rust
grammar::register_operator(
    "comment-toggle",
    "toggle line comments over the operated range",
    &OperatorSpec {
        repeatable: true,
        args_schema: Vec::new(),
        // `false`, and load-bearing: a blockwise `<C-v>` selection
        // arrives as ONE contiguous range rather than per row, like
        // `>` / `gU` and unlike `d` / `y`. Rule 1 is a property of the
        // range — decided per row, a mixed block inverts. See
        // `toggle::tests::a_mixed_block_must_be_decided_as_one_range`.
        blockwise_per_row: false,
        post_motion_char: false,
        // CM.2: the chord travels with the operator. `doubled` is the
        // TRAILING key, so this is `gcc` — the spelling commentary and
        // Neovim use — rather than `gcgc`.
        chord: Some("gc".to_string()),
        doubled: Some("c".to_string()),
    },
    CB_TOGGLE,
);
```

### `register-text-object`

```wit
register-text-object: func(name: string, doc: string, spec: text-object-spec, callback: u32)
```

Contribute a text object. `callback` → `grammar-callbacks.apply-text-object`.

**Example — Register a text object answered by callback 2** · [`crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs)

```rust
grammar::register_text_object(
    "to-cursor",
    "line start to cursor (fixture)",
    &TextObjectSpec {
        args_schema: Vec::new(),
    },
    2,
);
```

