# Session persistence

Authoritative design for restoring what the editor knew when you last
closed it: which buffers were open and how the panes were arranged, plus
the global editing state (registers, marks, histories) that should
follow you between projects.

Companion to `design.md` (§5.1.1 position history, §5.12 configuration),
to `dashboard.md` (the launch surface this restores *through*), and to
`pane-groups.md` / `pane-buffer-history.md` (the layout structures being
serialised).

Sequencing lives in `docs/dev/operations/slice-plans/session-persistence.md`.

## 1. Why this exists

Lattice currently forgets everything on quit. There is no session save,
no restore, and no shada/viminfo equivalent anywhere in the tree — a
grep for session save/restore returns nothing, and the only persisted
state is the picker's frecency index. Every one of vim, Emacs, VS Code
and Zed persists at least buffers and layout.

This is felt on every single launch, which makes it disproportionate to
its size: it is a small feature that removes a papercut paid daily.

## 2. Two stores, not one

Vim splits `:mksession` (layout) from `viminfo`/shada (global state).
Emacs splits `desktop.el` from `savehist`. Both arrived at the same
split independently, and the reason is not historical accident: **the
two have opposite scoping requirements.**

| | Session | State |
|---|---|---|
| Scope | Per workspace | Global |
| Holds | Open buffers, pane tree, per-pane cursor + scroll, active buffer, buffer-local options | Registers, named marks, command history, search history, position-history ring |
| Should follow you across projects? | **No** — a layout from project A is meaningless in project B | **Yes** — your yank register and command history are yours, not the project's |

Collapsing them into one per-workspace file would either lose your
registers when you switch projects, or leak project A's pane layout into
project B. So: two stores.

## 3. Format and location

**bincode with a leading schema-version byte**, extending the precedent
`lattice-picker/src/mru.rs` already set for persisted state ("persistence
is a thin wrapper around `entries` + a schema version byte"). Not TOML:
this is machine state, not user intent, and nobody hand-edits a pane
tree.

Location is **`~/.local/state/lattice/`** (XDG *state*, not config).
Config is what the user wrote; state is what the editor observed. Mixing
them makes `~/.config/lattice` unsafe to copy between machines, which is
exactly what people do with dotfiles.

```text
~/.local/state/lattice/
  state.bin                 # global: registers, marks, histories
  sessions/<workspace-hash>.bin
```

Sessions are keyed by a hash of the canonicalised workspace root, with
the human-readable root stored *inside* the file so the dashboard can
list recent workspaces without decoding every filename.

**Unknown schema version is a discard, not an error.** A session that
cannot be decoded is dropped with a `debug!` line and the editor boots
clean. Refusing to start because a cache is stale would be the worst
possible failure mode for a convenience feature.

## 4. What is deliberately not persisted

Each of these is an active decision, not an oversight:

- **Undo history.** Large, and a restored undo stack that no longer
  matches the file on disk is a corruption vector — `u` would apply
  edits computed against different content. Vim's `undofile` solves this
  with careful file-hash validation; that is its own feature, not a
  rider on this one.
- **Terminal buffers.** The PTY is gone. Restoring a terminal buffer
  would produce a dead shell that looks alive.
- **LSP state, diagnostics, multibuffer provider results.** All
  re-derived cheaply on open, and stale versions are worse than absent
  ones — a restored diagnostic pointing at a line that has since moved
  is actively misleading.
- **Buffers whose file no longer exists.** Skipped with a `debug!` line;
  the rest of the session still restores. A session must degrade
  buffer-by-buffer, never all-or-nothing.

## 5. Restore through the dashboard

The no-file-argument launch slot is **already owned** by the dashboard
(`dashboard.enabled`: *"Open the `*dashboard*` launch page when the
editor starts with no file argument"*). Session restore wants the same
slot, and that collision is the design's only real decision.

**The dashboard becomes the restore surface.** A no-arg launch opens the
dashboard as it does today; the dashboard gains a **Restore** section
listing this workspace's last session and recent workspaces. Restore is
one keystroke, it is discoverable by being visible, and nothing the user
already relies on is silently replaced.

This needs no new mechanism. `DashboardRegistry::register(Arc<dyn
DashboardSection>)` is the existing extension point, and
`dashboard.sections` already selects and orders sections by id (unknown
ids are skipped with a warning), so users who do not want the section
simply omit it.

Ex-commands cover the explicit path: `:session-save`, `:session-restore`,
`:session-discard`.

```
  session.restore = dashboard | auto | off        (default: dashboard)
    dashboard — no-arg launch shows the dashboard with a Restore section
    auto      — no-arg launch restores immediately, dashboard only when
                there is no session for this workspace
    off       — never restore without an explicit :session-restore
  session.save = on-exit | manual                 (default: on-exit)
  session.max-workspaces = 20                     (LRU eviction)
```

`auto` exists because it is the right default for some people and the
wrong one for others, and the disagreement is genuine — vim and Emacs
are explicit, VS Code and Zed are automatic. An option is the honest
resolution of a split that has no correct answer.

## 6. Restore is asynchronous and progressive

**Restore must never be on the path to the first frame.** The order is:

1. Paint the dashboard (or, under `auto`, the previously-active buffer)
   as soon as its content is ready.
2. Load the remaining buffers off-thread.
3. Apply the pane tree once its buffers exist.

A synchronous restore of a 30-buffer session would put file I/O
proportional to session size in front of the first frame — precisely the
class of violation paramount goal #1 forbids, and precisely what the
keystroke→glyph ratchet and the "UI thread does no I/O" rule exist to
prevent.

The mechanism is the established one: buffers land through
`SubsystemBoot::inbound`, whose `send` bakes in the `async_landed` wake,
so a restored buffer reaches the screen without requiring a keypress.
Reaching for a bare `TickCallback` here would reproduce the
works-but-only-after-you-hit-something bug that
`docs/dev/architecture/boot-composition.md` §3 designs out.

## 7. Ownership

No new crate — heuristic #6. Session persistence *extends* existing
domains rather than introducing a mechanism:

| Piece | Home |
|-------|------|
| Session + state types, encode/decode, schema versioning | `lattice-core` (owns `Document`, buffers) |
| Capture / apply against the live editor, ex-commands, options | `lattice-host` subsystem installed via `SubsystemBoot` |
| The `Restore` dashboard section | `lattice-dashboard` (owns the launch surface) |

Per the mode-ownership rule the subsystem's `install(boot)` does all its
own wiring; the acid test holds — no `Editor::do_session_*` method and
no new host `Action` variant.

## 8. Paramount-goal alignment

**#1 Performance.** Nothing on the keystroke path. Save runs on exit (or
on an explicit command); restore is off-thread and progressive so the
first frame never waits on session size.

**#2 Extensibility.** The dashboard section is a normal
`DashboardSection` registration, so a plugin could contribute its own
restore surface. Persisted plugin state is explicitly **out of scope for
v1** — it needs a per-plugin capability and a schema story, and adding
it speculatively would be surface without a consumer.

**#3 Vim modal editing.** `:session-save` / `:session-restore` are
ex-commands in the unified registry like everything else. The default
(`dashboard`) is deliberately *not* vim's behaviour, and §5 says why:
discoverability beats fidelity for a feature vim users mostly never turn
on.

**#4 Asynchronicity.** Restore is inbound-bus driven; the editor is
usable while buffers are still landing.

**UX (higher court).** Nothing the user already relies on is replaced.
The dashboard still owns the no-arg slot; restore is additive and
visible. A corrupt or stale session degrades to a clean boot rather than
to an error.

## 9. Rejected alternatives

- **One combined per-workspace file.** Loses global registers and
  histories on project switch, or leaks layout across projects. §2.
- **TOML for sessions.** Human-readable but nobody edits a serialised
  pane tree by hand, and it would make schema evolution a parsing
  problem rather than a version-byte problem.
- **Storing under `~/.config/lattice`.** Makes the config directory
  unsafe to sync between machines — the thing people most want to do
  with it.
- **Auto-restore as the default.** Silently demotes a shipped feature
  (the dashboard) and takes a contested UX position by default. Available
  as `session.restore = auto`.
- **Persisting undo history.** §4 — a corruption vector that belongs
  with a proper `undofile`-style file-hash design.

## 10. Testing strategy

- **Round-trip:** capture → encode → decode → apply reproduces buffers,
  pane tree, per-pane cursor and scroll.
- **Schema tolerance:** a file with an unknown version byte, and a
  truncated file, each boot clean with no panic.
- **Degradation:** a session referencing a deleted file restores every
  *other* buffer; a session referencing only deleted files boots clean.
- **Scoping:** state written in workspace A is visible in workspace B;
  session written in A is **not**.
- **Async, tested the way it fails:** assert restored buffers are
  visible *without* dispatching a keystroke — a test that presses a key
  first passes on the broken version too.
- **No first-frame regression:** the keystroke→glyph ratchet
  (`keystroke_glyph_ratchet.rs`) must stay within baseline with a
  restored multi-buffer session.
- **Dashboard:** the Restore section appears only when a session exists
  for the workspace, and omitting its id from `dashboard.sections`
  removes it.

## 11. Cross-references

- `dashboard.md` — the launch surface and its section registry.
- `boot-composition.md` §3 — `SubsystemBoot::inbound` and the
  async-results-need-a-wake rule.
- `pane-groups.md`, `pane-buffer-history.md` — the layout structures
  serialised here.
- `design.md` §5.1.1 — the position-history ring in the global store.
- `docs/dev/operations/slice-plans/session-persistence.md` — sequencing.
