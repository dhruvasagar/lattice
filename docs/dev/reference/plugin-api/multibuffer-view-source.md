<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `multibuffer-view-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `multibuffer-view-plugin` (exports)

## Uses

- [`multibuffer-view-result`](types.md#record-multibuffer-view-result) from [`types`](types.md)

## Functions (1)

### `build`

```wit
build: func(view: string, args: list<string>) -> result<multibuffer-view-result, string>
```

Produce a `pull` view's excerpts, **in final order**.

`view` names which of this guest's registered views is being built; one
actor and one guest instance serve them all, as with `picker-source`.
`args` are the trigger's arguments verbatim — from the ex-command, the
transient row, or the `gr` that refreshed the view.

##### Why the guest orders, when a scan source only supplies a sort key

The asymmetry is deliberate and it turns on **who can see the whole set
at ordering time**. A scan guest is handed one file and cannot know
where its rows land once every other file's rows interleave, so only the
host can sort and the guest supplies an `s64` key. A pull guest computes
the entire set in this one call, so requiring a key would make the host
re-sort what is already ordered — and would force orderings that are not
numeric (by title, by file-then-line) through an integer that cannot
express them.

An `err` **declines** the view with the guest's own message rather than
opening an empty one. Declining is a first-class outcome: an empty view
leaves the user to guess whether it is broken or genuinely empty.

