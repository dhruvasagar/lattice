# Mode option overrides across the plugin seam

**Status:** designed, unbuilt. Extends
[`mode-architecture.md`](mode-architecture.md) §6 (the layered override
resolver) and [`plugin-host.md`](plugin-host.md) §5 (the `modes` seam).

## 1. The gap

A native mode says what options it needs:

```rust
fn options(&self) -> OptionOverrideSet {
    lattice_config::overrides! {
        ReadOnly = true,
        NoFile   = true,
        Number   = false,
    }
}
```

A **plugin** mode cannot. `mode-declaration` (`wit/modes.wit`) carries `id`,
`kind`, `activation-policy`, `capabilities`, `keymap` and `target-language` —
and nothing else. The seam's own comment records the omission as deliberate:
*"option-overrides / bundled modes-as-components remain Phase 8."*

**This is a mode-ownership hole, not a missing convenience.** The standing rule
is that a mode owns its full surface — keymap, lifecycle, handlers, and the
options its buffers need. A plugin mode owns everything except the last one,
and the part it cannot own is the part that decides how its buffer *behaves*:
whether it is writable, whether it wraps, whether it shows numbers, how it
folds.

The immediate consumer: org wants `foldmethod = syntax` in org buffers.
Today that works only because the user happens to set it globally, which means
org's folding is correct by coincidence on one machine and wrong everywhere
else. A native mode would simply declare it.

## 2. What has to cross

An `OptionOverride` is:

```rust
pub struct OptionOverride {
    pub option_type_id: TypeId,               // which option
    pub value: Arc<dyn Any + Send + Sync>,    // type-erased value
    pub priority: OverridePriority,           // Low | Normal | High
}
```

Two of those three have no WIT form. A `TypeId` is a Rust identity and a
plugin must not be able to forge one — the same rule as `CommandId`. An
`Arc<dyn Any>` is not data.

So a plugin sends the two things it *can* mean:

| WIT | Native |
|---|---|
| `name: string` | `TypeId`, resolved host-side via `ConfigRegistry::type_id_for_name` |
| `value: string` | coerced to the declaration's type through the same parser `:set name=value` uses |
| `priority` | `override-priority` enum — a straight mirror |

`overrides.rs` already anticipates exactly this: *"Direct `OptionOverride::new`
is reserved for the WIT plugin adapter (M.10), where declarations are runtime
data and TypeId is the only handle."* The erased constructor exists for this
caller.

### The crux: an option a plugin declared has no `TypeId`

`type_id_for_name` resolves a *native* option, because a native option is a
Rust `OptionDecl` type. An option registered through the **config seam**
(`register_plugin_option`) has no Rust type behind it, so it has no `TypeId` to
resolve — and a plugin overriding **its own** option is the obvious case (org's
`org.inline-images` in org buffers only).

This is the decision the slice turns on, and §3's options differ mainly in how
they answer it.

## 3. Options

### (a) Name + string value, native options only

`mode-declaration` gains `options: list<mode-option-override>` where the record
is `{ name, value, priority }`. The host resolves the name through
`type_id_for_name`, coerces the string, and builds an `OptionOverride`. A name
that does not resolve — including any plugin-declared option — is **skipped
with a warning naming it**.

> **UX (higher court):** org buffers fold correctly on a machine that never
> configured `foldmethod`, which is the reported problem. No flicker, no
> latency: overrides resolve at activation, which is already a layer-recompute
> point.
> **Paramount goals:** protects #2 (a plugin mode owns its options like a
> native one) and #3 (options stay a typed registry, not strings the mode
> interprets). Sacrifices nothing at #1 — activation is not a hot path.
> **Heuristic #1 (long-term fit):** the smallest thing that is *correct*
> rather than the smallest thing that works. It leaves one real case
> unserved, and says so out loud rather than half-supporting it.
> **Heuristic #2 (paramount, not other editors):** anchored on the
> mode-ownership rule, not on "vim has `setlocal`".
> **Heuristic #3 (third option):** (c) below is the one this is measured
> against.
> **Standing-rule check (mode ownership):** satisfied for native options —
> declaration and effect both live with the mode. NOT satisfied for a
> plugin's own options, which is the honest cost.

### (b) Name + typed value variant

As (a), but `value` is `option-value = bool | int | string` rather than a
string coerced host-side.

> **UX (higher court):** identical.
> **Paramount goals:** marginally better at #3 (a type error surfaces at the
> boundary rather than at coercion). Sacrifices nothing.
> **Heuristic #1:** more WIT surface for a check the coercion already
> performs — `parse_and_set` rejects a bad value and names the option either
> way. The config seam already registers options with **string** defaults
> (`register_plugin_option(..., "true", …)`), so a typed value here would
> make one subsystem speak two dialects.
> **Heuristic #2:** no editor-shaped argument either way.
> **Heuristic #3:** —

### (c) Name + string value, and plugin options get a synthetic identity

As (a), plus: the `ConfigRegistry` mints a stable synthetic `TypeId`-equivalent
for each plugin-declared option, so `type_id_for_name` answers for those too
and a plugin can override its own options.

The resolver keys on `TypeId` today. Serving plugin options means either
widening that key to `enum OptionKey { Native(TypeId), Plugin(Name) }` — which
touches every resolver read — or minting a real per-option `TypeId`, which Rust
cannot do at runtime.

> **UX (higher court):** identical to (a), plus the case (a) refuses.
> **Paramount goals:** fully protects #2 — a plugin mode owns *all* its
> options, including its own. Sacrifices some of #1's simplicity: the
> resolver's key becomes an enum on a path read per option per buffer.
> **Heuristic #1 (long-term fit):** this is the genuinely-complete answer,
> and "the rewrite is bigger" is explicitly not a reason to refuse it. The
> reason to defer is different and better: **nothing needs it yet.** Org's
> ask is `foldmethod`, a native option. Widening the resolver's key before a
> consumer exists is abstraction for its own sake, which the same heuristic
> forbids from the other direction.
> **Heuristic #2:** anchored on mode-ownership.
> **Heuristic #3:** —
> **Heuristic #6 (crate boundary):** no new crate — this is `lattice-config`
> and `lattice-mode` widening a key they already own.

## 4. Recommendation

**(a)**, because heuristic #1 cuts against (c) *only* on the "no consumer yet"
limb, and that limb is load-bearing: the resolver key is read per option per
buffer, and widening it speculatively is the kind of change that is easy to
justify and hard to undo.

(a) also leaves (c) reachable without rework — the WIT record does not change,
only what `type_id_for_name` can answer. The moment a plugin mode wants to
override a plugin option, (c) is a follow-on slice with a real consumer to
shape it.

**The refusal must be loud.** A skipped override is exactly the class of
silent-nothing this codebase keeps getting bitten by (a chord that does not
fire, a menu that does not open, a language with no grammar). An unresolvable
name is a `warn!` naming the mode and the option — not a `debug!`.

## 5. Failure behaviour

- **Unknown option name** → that override is skipped, the rest of the set
  applies, and the skip is warned with the mode and name. One bad entry must
  not cost a mode its other options — the same rule the transient seam's rows
  and the capture template set follow.
- **Value that will not coerce** → same: skipped, warned, named. The coercion
  is `parse_and_set`'s, so the message is the one `:set` would have given.
- **Conflict with another mode's override** → unchanged. The existing resolver
  policy (`mode-architecture.md` §6.2) decides: last-activated among `Normal`
  wins and a `ModeEvent::OptionConflict` fires. A plugin mode is not special.
- **Conflict with the user's own `:set`** → unchanged, and worth stating
  because it is the question users ask: the layered resolver's precedence is
  what it already is. A mode override is a *layer*, not a write.

## 6. Paramount-goal alignment

**#2 Extensibility.** The goal this serves: a plugin mode becomes able to own
the last part of its surface it could not.

**#3 Vim modal editing.** Options stay a typed registry with one dispatcher.
A plugin declares an override in the same vocabulary `:set` uses; it does not
get a private settings channel.

**#1 Performance.** Overrides resolve at mode activation, which already
recomputes the layer stack. No per-keystroke or per-frame cost.

## 7. Slice sketch

| Slice | What |
|---|---|
| MO.1 | `mode-option-override` in `wit/modes.wit` + `mode-declaration.options` |
| MO.2 | Host adapter: name → `TypeId`, string → typed value, build `OptionOverrideSet`; skip-and-warn on either failure |
| MO.3 | Org declares `foldmethod = syntax` on `org-mode`; test that an org buffer folds by syntax with the option unset globally |

Tests worth naming now: a plugin mode's override reaches
`ResolvedOptions` for its buffers and **not** for others; an unknown name is
skipped and warned with the rest applying; the user's global setting is
unaffected outside the mode; two modes overriding the same option resolve by
the existing conflict policy rather than a plugin-specific one.
