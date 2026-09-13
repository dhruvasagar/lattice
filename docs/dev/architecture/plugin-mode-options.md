# Mode option overrides across the plugin seam

**Status:** built to the recommendation — option **(a)**, name + string value,
native options only. MO.1–MO.3 all landed. Sequencing and what each slice did:
[slice plan](../operations/slice-plans/archive/plugin-mode-options.md).

§3(c) — synthetic identity so a plugin can override its *own* options — remains
the deliberate non-goal it was recommended as, now pinned by a test rather than
only described. It waits for a consumer.

Extends [`mode-architecture.md`](mode-architecture.md) §6 (the layered override
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

---

## 6. User overrides for a mode you do not own

MO.1 lets a mode declare options for **its own** buffers. It does not let a
*user* say "wrap in org buffers" — `mode-declaration.options` is a declaration,
so only whoever declares the mode can use it, and a user's `init.rs` does not
declare `org-mode`.

The mechanism for that is the **event bus**, not a second declaration seam, and
the reason is uniformity: the mode dispatcher publishes `MajorEntered` /
`MajorExiting` / `MinorActivated` / `MinorDeactivated` without knowing whether
the mode came from the built-in table, a core plugin, or an external one. A
subscription keys on the mode *id* at activation time, so it works the same for
all three by construction. This is `add-hook 'org-mode-hook` in this editor's
vocabulary, and design.md §5.10's "hooks ≡ autocmds ≡ typed event
subscriptions" is the claim it makes good on.

```rust
// init.rs
events::subscribe(&EventFilter {
    kinds: Some(vec![EventKind::MajorEntered]),
    major_modes: Some(vec!["org-mode".to_string()]),
    ..
}, ON_ORG);

fn on_event(handler: u32, ev: Event) {
    if let (ON_ORG, Event::MajorEntered(l)) = (handler, &ev) {
        config::set_option_in_buffer(l.buffer, "autowrap", "all");
    }
}
```

### 6.1 Two gaps this needed, and why each was a gap

**`minor_modes` on `EventFilter`.** `major_modes` was the only mode filter, and
`event_major_mode` answers `None` for the minor lifecycle — so a
`major_modes`-constrained subscription to `MinorActivated` matched *nothing*,
and an unconstrained one matched *everything*. A subscriber wanting one minor
had to compare names in its own handler, which for a plugin is a WASM crossing
per activation per buffer to do nothing. There are far more minors than majors,
so that cost is not theoretical.

Kept as a **separate field** rather than merged into one `modes` list: the two
ask different questions. `major_modes` means *the buffer is entering one of
these majors* (§7.4's minor-activation allowlist); `minor_modes` means *this
specific minor turned on*. A merged field answers both at once, so a
subscription meaning the second would also fire on a major sharing the name —
and mode ids are user-chosen strings, so that collision is available to anyone.
Constraining both matches nothing, since no event carries both names.

**`set-option-in-buffer` in the config seam.** WIT's `set-option` is the `:set`
path and writes the GLOBAL layer. A handler using it to wrap org buffers would
wrap every buffer in the editor, with nothing to unwrap on leaving. The
buffer-local layer is the scope the question actually has, and it lives on the
`Editor` (`buffer_local_overrides`) while a guest holds a `ConfigRegistry`
handle — hence a host-internal `BufferOptionOverrideRequested` bridge, the
shape `enable-mode` already uses for the same reason.

### 6.2 Precedence, and the one place it stops

A user override **beats a mode's contribution**, which is what makes this worth
having: `recompute_options_for_buffer` ranks *Layer 1: modal-state, Layer 2:
buffer-local, Layers 3+: modes*, so buffer-local outranks every mode.

**Including against `OverridePriority::High`.** `Resolver::candidate_better`
makes `High` win *absolute* — ahead of layer rank, not within it — so a mode
declaring it was unoverridable from a user's config. That is right for the case
the rule was written for (`read-only-mode` declares `writable=false` at `High`
so no *other mode* can quietly flip it) and wrong as a general rule, because
any mode may declare `High` and a user has no way to know which did.

So `candidate_better` now checks authorship first: a `BufferLocal` candidate
outranks a `ModeContribution` one, priority included. **Mode-versus-mode is
untouched** — `read-only-mode` still beats every other mode regardless of
activation order, which is the threat model `High` exists for. What changed is
that the person who owns the editor can say otherwise about one buffer.

This is the behaviour the seam already *claimed*: a mode contribution is
documented as "a LAYER, not a write … a `:setlocal` in that buffer still wins
over it, which is the right way round — the user gets the last word in their
own buffer." It was true only against `Normal` contributions.

**`BufferLocal` only, not `GlobalConfig`.** Global config is the baseline a
mode is *supposed* to refine: org setting `foldmethod=syntax` over a global
`foldmethod=indent` is the seam working. If global config outranked modes,
`mode-declaration.options` would do nothing for any option the user had ever
set. A buffer-local value is a different act — it names one buffer, so there is
no reading under which the mode is the more specific answer.

All three directions are pinned in
`lattice-host/tests/buffer_scoped_option_override.rs`.

### 6.3 Known: the override lands after the first paint

`MajorEntered` is published from a **spawned** cascade task, so a handler's
write arrives after the buffer has opened and rendered. For `autowrap` that is
invisible in practice; for an option that changes layout it would be a visible
re-flow of content the user did not edit, which the UX rules name explicitly as
unacceptable.

`pre-plugin-loaded` sets the precedent for the fix — its delivery is **awaited**
so an `init.rs` handler can affect what the plugin then reads — and the same
treatment would apply here at the cost of a WASM round-trip on buffer-open (not
a keystroke path). Not done yet, deliberately: the flicker is predicted rather
than observed, and it should be measured on a real option before event delivery
semantics are changed for every subscriber.
