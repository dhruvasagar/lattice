---
summary: "magit-submodule-mode: the configured submodules with git's own status marker — a adds, u updates, s syncs, d removes (asks first)."
related: [magit, magit-remote-mode, ex:magit-submodule, magit-transient]
---

# magit-submodule-mode

Your submodules, each with the marker `git submodule status` prints,
the commit the superproject records, and the path. `:magit-submodule`,
or `o` in the repo [dispatch transient](help:magit-transient).

```
Submodules (3)
  - a1b2c3d  vendor/uninitialised
    e4f5a6b  vendor/tracking      (v1.2.3)
  + c7d8e9f  vendor/moved         (v1.2.3-4-gabc123)
```

The marker column is git's, unchanged, so a row reads the same here as
in a terminal:

| Marker | Meaning |
|---|---|
| (blank) | At the commit the superproject records |
| `-` | Not initialised — nothing checked out yet. `u` populates it |
| `+` | Checked out at a *different* commit than recorded |
| `U` | Has merge conflicts |

`-` and `U` are coloured as removals and `+` as an addition, so the
ones needing attention are findable by scanning the column. The
headerline carries the count plus how many are uninitialised, modified
or conflicted — a bare total would not tell you that any of them need
anything.

## Chords

| Chord | Action |
|---|---|
| `a` | Add a submodule — asks for the URL, then the path |
| `u` | Update the submodule at cursor (`--init --recursive`) |
| `s` | Sync the submodule at cursor's URL from `.gitmodules` |
| `d` | Remove the submodule at cursor — **asks first** |
| `gr` | Refresh (re-read `git submodule status`) |

`q` and navigation come from
[`magit-core-mode`](help:magit-core-mode).

The add prompt seeds the path git itself would choose — the URL's last
segment without `.git` — so accepting the default is one keystroke.

Magit's `p` populate and `r` register are not separate keys here: `u`
runs `submodule update --init --recursive`, which subsumes both. Three
keys for one intent is three chances to pick the wrong one.

## Why `d` asks

Removing a submodule runs `git submodule deinit -f` and then `git rm
-f` — it **deletes the submodule's whole working tree**, including
anything uncommitted inside it, and git keeps no copy. That is
irreversible in the way that matters, so it routes through a
confirmation naming the submodule and saying what is lost, and the
chord itself performs no git call at all.

This is the opposite call from [`magit-remote-mode`](help:magit-remote-mode)'s
`d`, which does *not* ask — removing a remote drops config you can
retype. Confirmations are spent only where work is destroyed.

Magit's `d` (unpopulate) has no key here. It is `u`'s inverse, rarely
wanted, and putting it next to a destructive `d` would make the
dangerous one easy to hit by accident.

## No `<CR>`, and why

The obvious binding would be "open this submodule's own magit-status".
It is not here because magit's working directory is currently
**process-wide** — every magit buffer in the editor is bound to the
repository lattice was launched in, and there is no way to point a
status buffer at a subdirectory yet. A chord that opened the
*superproject's* status while claiming to open the submodule's would be
worse than no chord. The same limitation is what blocks worktree
support.

## See also

- [`magit-remote-mode`](help:magit-remote-mode) — the same buffer shape
  for remotes.
- [`magit-transient`](help:magit-transient) — `o`, and the rest of the
  repo dispatch.
