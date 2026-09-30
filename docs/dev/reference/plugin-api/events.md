<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `events`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `events-plugin` (imports), `project-plugin` (imports)

The event/hook **subscription** API (plugin-host.md §5 `events`, PH7.8). The
surface a plugin calls to *observe* editor state transitions — mirroring
`EventBus::subscribe` (lattice-runtime). The host provides this function; the
guest **imports** it and calls it (from its `register-events` export). Each
call records the `(handler, filter)` pair into `PluginState`; after
`register-events` returns, the host wires each recorded subscription to the
native `EventBus` with a host-owned `SubscriptionTarget::Plugin { plugin,
handler, tx }` (PH7.8c) — so a plugin subscription is dispatched by the SAME
bus a native subscriber uses (paramount #2). `:autocmd` from a plugin
desugars to this call.

**Observation-only in v1** (the native bus is observation-only, §5.10): a
plugin sees events, it does not veto or mutate them. The before-class
veto/mutation seam is deferred with the bus's.

`handler` is the guest-chosen id the host passes back to the world's
`on-event` export on delivery (the grammar `callback` precedent) — the
guest's own dispatch key, so the host never allocates it and a plugin can
route many `:autocmd`s to distinct handlers behind one `on-event`. No
`unsubscribe` in v1: a plugin's subscriptions live for its lifetime and tear
down en masse on deactivate/quarantine (the reload/lifecycle seam, PH7.12).

## Uses

- [`event-filter`](types.md#record-event-filter) from [`types`](types.md)

## Functions (3)

### `cancel-wake`

```wit
cancel-wake: func(id: wake-id)
```

Disarm a wake. Unknown / already-cancelled / `0` ids are ignored — a
cancel is idempotent, because the alternative is a guest that must track
host state to avoid a trap. There is deliberately no bulk form: wakes are
cancelled en masse on deactivate / quarantine by the host, for the same
reason `events` has no `unsubscribe`.

### `subscribe`

```wit
subscribe: func(filter: event-filter, handler: u32)
```

Subscribe `handler` to every event matching `filter` (the declarative
`kinds` / `path-globs` / `major-modes` subset; a custom predicate is the
guest filtering inside `on-event`). The host delivers each match to the
world's `on-event(handler, ev)` export.

### `wake-every`

```wit
wake-every: func(ms: u32) -> wake-id
```

Ask to be woken every `ms` milliseconds, forever, until `cancel-wake`
(OC.2). Delivery is `on-wake(id)` on the plugin's own actor task — the
SAME channel `on-event` arrives on, so a wake is subject to the same
budget, the same quarantine, and the same "never on the keystroke path"
guarantee (paramount #4). It is not a precise timer: a wake fires no
sooner than the interval and may be late under load, and a late one does
not queue a backlog — the period restarts when the wake is delivered.

Intended for the low-frequency "recompute my own display string" shape
(org's clock re-renders its modeline segment once a minute, `design.md`
Appendix B's idle hooks). It is NOT a frame or animation source: each
firing is a full guest call, so a small `ms` buys a guest call at that
rate for as long as the plugin is loaded.

Returns `0` when no wake mechanism is wired on this seam — a plugin
instantiated on a store with no timer (the sync grammar seam, a test
harness). Like every other seam here that answers rather than traps, the
degradation is honest and visible in the log, and a guest that treats a
`0` as armed simply never hears back.

## Types (1)

### type `wake-id`

```wit
type wake-id = u32;
```

The host-issued handle for one armed periodic wake (OC.2). Host-allocated
rather than guest-chosen — unlike `handler` above, which the guest picks
because it is a *dispatch key*. A wake is a live resource the host must be
able to cancel unambiguously, so the host names it; `0` is never a valid
id and is what a refused `wake-every` returns.

