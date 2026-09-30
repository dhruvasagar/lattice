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

**Example — Count a periodic wake's fires in `on-wake` and cancel it after the last one** · [`crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs)

```rust
let n = wake_state::FIRES.with(|f| {
    let n = f.get() + 1;
    f.set(n);
    n
});
record(&format!("wake:{n}"));
if n >= wake_state::CANCEL_AFTER {
    events::cancel_wake(id);
}
```

### `subscribe`

```wit
subscribe: func(filter: event-filter, handler: u32)
```

Subscribe `handler` to every event matching `filter` (the declarative
`kinds` / `path-globs` / `major-modes` subset; a custom predicate is the
guest filtering inside `on-event`). The host delivers each match to the
world's `on-event(handler, ev)` export.

**Example — Subscribe to one event kind and handle it in `on-event`** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// Subscribe to `document-opened` — how a project comes to be remembered at
/// all, and `project.el`'s `project-remember-project` in one line.
///
/// Filtered to the one kind rather than taking everything and branching: the
/// filter is the host's, so an unfiltered subscription would wake this
/// plugin's task for every modal-mode change and every option write in the
/// editor, to do nothing.
fn register_events() {
    lattice::plugin_host::events::subscribe(
        &EventFilter {
            kinds: Some(vec![EventKind::DocumentOpened]),
            path_globs: None,
            major_modes: None,
            minor_modes: None,
        },
        ON_DOCUMENT_OPENED,
    );
}

/// Runs on the event actor's own task, never a keystroke — which is the
/// property that lets it do a store read+write at all.
///
/// Silent by construction: a handler that echoed would announce a project on
/// every file you open. A store failure is dropped here rather than shown,
/// because there is no user action that provoked it and nothing they could
/// do about it mid-open; `:project-remember` is the path that reports.
fn on_event(handler: u32, ev: Event) {
    if handler != ON_DOCUMENT_OPENED {
        return;
    }
    let Event::DocumentOpened(opened) = ev else {
        return;
    };
    // A buffer with no path on disk resolves to `pwd`, which
    // `project_of_buffer` already refuses — but checking here avoids a host
    // call per scratch buffer, and the field is right there.
    if opened.path.is_none() {
        return;
    }
    // `opened.id` is a `DocumentId` by TYPE and a buffer id by VALUE:
    // `publish_document_opened_for_active` builds it as
    // `DocumentId::new(buffer_id.0 as u64)`. `root-for-buffer` wants the
    // buffer id, so passing this straight through is correct — verified
    // rather than assumed, because the two type names disagree and a wrong
    // id here would resolve to `none` and silently remember nothing.
    if let Some(root) = project_of_buffer(opened.id) {
        let _ = remember_root(&root);
    }
}
```

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

**Example — Arm a periodic wake at registration and keep its id for `cancel-wake`** · [`crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs)

```rust
// OC.2: arm a periodic wake from registration. 50 ms is the seam's
// floor — fast enough that a test does not sit on a real clock, and the
// guest cancels itself after a few fires so it cannot run away.
wake_state::TICKER.with(|t| t.set(events::wake_every(50)));
```

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

