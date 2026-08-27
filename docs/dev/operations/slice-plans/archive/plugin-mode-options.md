# Mode option overrides across the plugin seam — slice plan

> Design: [`../../../architecture/plugin-mode-options.md`](../../../architecture/plugin-mode-options.md).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** ✅ complete (2026-08-27). MO.1 + MO.2 landed together in this repo;
MO.3 landed in `lattice-org-plugin` (`2363ed4`).

One thing stays open and is deliberately **not** a slice here: a plugin cannot
override an option it registered itself. That is design §3(c), it has no
consumer, and it is pinned by a test so the limit is on record. See "The known
hole" below.

## Why now

A plugin mode owned its keymap, its handlers and its lifecycle — but not the
options deciding how its buffers actually behave. That is a hole in the
mode-ownership rule, and org was standing in it: `foldmethod = syntax` in org
buffers worked only because the user happened to set it globally, so org's
folding was correct by coincidence on one machine and wrong everywhere else.

| Slice | Description | Status |
|---|---|---|
| MO.1 | `mode-option-override` in `wit/modes.wit`; projected onto `PluginModeDecl` | ✅ |
| MO.2 | Resolve name→`TypeId` + coerce value; `PluginMode::options()` | ✅ |
| MO.3 | Org declares `foldmethod = syntax` on `org-mode` | ✅ |

## MO.1 + MO.2 — landed together, deliberately ✅

**They could not be split.** MO.1 alone adds `PluginModeDecl.options` with no
reader, which is a `dead_code` warning — and the warning gate treats those as
real, because a field nothing reads is exactly the half-finished conversion the
gate exists to catch. So the WIT surface and its consumer land in one commit,
per the "cannot compile without its neighbour" exception.

### The WIT

`enum override-priority { low, normal, high }` mirroring the native
`OverridePriority`, and:

```wit
record mode-option-override {
    name: string,
    value: string,
    priority: override-priority,
}
```

on `mode-declaration.options: list<mode-option-override>`.

Name and value cross as **strings**, which is option (a) of the design and not
an accident. The config seam already registers plugin options with string
defaults, so a typed `option-value` variant here would make one subsystem speak
two dialects for the same job — and the check it would buy is one the coercion
performs anyway, with a better message.

### The resolution

`resolve_mode_options` puts every entry through **`parse_for_buffer_local` — the
same parse-and-validate `:setlocal name=value` uses**. That is the whole point:
a mode and a user cannot end up disagreeing about what a value means, and the
option's own validator is the one that judges it. It resolves and coerces
*without writing*, returning exactly the `(TypeId, erased value)` an
`OptionOverride` is made of.

Always spelled `name=value`, so it is `parse_set`'s Assign arm rather than the
bare-name / `no`-prefix vim shorthands. A mode declares a value; giving it the
shorthand grammar too would make `{ name, value }` mean two different things.

Resolved **once at registration**, not per `options()` call: the trait requires
that answer be pure, and `options()` is read on every layer recompute — every
mode activation and every buffer switch. Registry lookups and string parsing
there would put work on a path that currently does none.

Downstream, nothing distinguishes a plugin mode's set from a native one's.
`recompute_options_for_buffer` reads it through the same `DynMode` blanket impl,
tags it with the same `OptionOrigin::ModeContribution`, and the same conflict
policy applies. Mode ownership means owning the surface, not getting a parallel
one.

### Failure behaviour

Skip-and-warn **per entry, never per set** — one unresolvable name must not cost
a mode its other options. Same rule `bind_mode_keymap` follows for an unknown
command. The warning names the mode *and* the option, because what it describes
is otherwise a buffer that quietly behaves wrong.

A host with no config registry (the minimal harnesses) skips the overrides as a
set with one warning and **still registers the mode**. Refusing the mode would
turn a missing handle into a missing feature.

### The known hole, asserted rather than left to surprise

An option a plugin registered through the **config seam** has no native type
identity — `register_plugin_option` goes through `try_register`, which never
populates `by_typeid`, and `OptionOverride` is keyed by `TypeId`. So a plugin
cannot yet override its *own* option. It is skipped and named like any other
unresolvable entry, and there is a test (`a_plugin_declared_option_cannot_yet_be_overridden`)
pinning it, so the limit is a decision on record rather than something the next
person discovers by being confused. Design §3(c) is the fix; it waits for a
consumer, because widening the resolver key is read per option per buffer and
speculative widening is what heuristic #1 forbids from the other direction.

### The ABI break, and what it validated

Adding a field to a WIT record is a breaking change — records are structural in
the Component Model, so there is no "just be additive" escape (design.md §15
Q7 / `wit-ownership.md` §5). Every guest fixture had to gain `options: vec![]`.

That is also the first real exercise of the machinery that landed this morning:
the build service now rewrites each plugin's `wit/` before compiling it, and
WT.3's fingerprint makes every already-built artifact `Stale` rather than a
silent instantiate failure. What used to be a day of debugging is a rebuild.

### Tests

Unit (`mode_host`), against a hand-built declaration: a declared override
reaches the registered mode as the native `TypeId` + coerced value; the global
option is **not** written (it is a layer, not a write); an unresolvable name is
dropped while the rest of the set applies; a value the option rejects is
dropped; a plugin's own option is refused; priority crosses rather than being
flattened to `Normal`; and with no config registry the mode still registers.

End-to-end (`mode_source`), through a real `wasm32-wasip2` component: the
fixture major declares two overrides, one resolvable and one not, and the test
asserts the good one arrives as `FoldMethod::Syntax` while the bad one is
dropped. The unit tests prove the resolution logic; only this proves the WIT
record actually carries the data and that bindgen projects it. A seam wired
end-to-end can still answer nothing.

## MO.3 — org declares its folding ✅

Landed in `lattice-org-plugin` (`2363ed4`). One entry on the `org-mode` major:

```rust
options: vec![ModeOptionOverride {
    name: "foldmethod".to_string(),
    value: "syntax".to_string(),
    priority: OverridePriority::Normal,
}],
```

The test (`an_org_buffer_folds_by_syntax_without_the_user_setting_it`,
`tests/org_major_mode.rs`) boots a sealed editor, loads the real component,
opens a `.org` file and asserts both halves — `foldmethod` resolves to `syntax`
in that buffer, and the user's global setting is still `manual`. It also pins
the precondition that makes it mean anything: the global default is `manual`
*before* the plugin loads, so `syntax` can only have come from the mode.

**Verified by falsification**, not just by passing. Declaring `indent` instead
and rebuilding makes the assertion fail with `Indent` — so the test reads org's
declaration rather than something incidental. Worth the two-minute wasm rebuild:
a seam wired end-to-end can still answer nothing, and a green test that would be
green either way is how that goes unnoticed.

Org's other four modes gained an explicit empty `options`, and its `wit/`
regenerated itself from the `lattice-wit` build-dependency — WT.2 doing its job,
an ABI field added upstream in the morning picked up by a rebuild.

224 org tests green afterwards.
