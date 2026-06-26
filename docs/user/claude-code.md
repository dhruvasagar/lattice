---
summary: "Claude Code IDE integration: :claude launches the agent wired to this editor, it reads/edits via reviewable side-by-side diffs you accept or reject with :diff-accept / :diff-reject."
related: [claude, claude-send, claude-interrupt, claude-code-start, claude-code-stop]
---

# Claude Code IDE integration

Lattice can act as the **IDE side** of Anthropic's Claude Code agent.
An external `claude` CLI attaches to the editor over a loopback
WebSocket and drives it: reading your selection, open buffers, and
diagnostics, opening and saving files, and proposing edits as
interactive side-by-side diffs you review and accept or reject. The
point is to develop lattice *from within lattice* — the agent edits
your code through the same diff UI you already drive by hand.

This is a **network peer**, not an in-process plugin. Architecturally
it works like the LSP client: a JSON-RPC connection to an external
process. No agent runs inside the editor, and no third-party code
executes in lattice's address space.

> **Status:** The full launch → read → edit-via-diff → accept/reject
> loop is implemented. The on-the-wire reply and notification shapes
> are **provisional** until validated against a live `claude` CLI —
> see [Provisional status](#provisional-status) below. This doc
> describes the user workflow, which is stable.

---

## Quick reference

| Command | Behavior |
|---|---|
| `:claude` | Start the IDE server (if needed) and launch the `claude` CLI in a terminal buffer wired to this editor |
| `:claude-send` | Send the current file + selection to the attached agent as an `@`-mention (adds it to the agent's context) |
| `:claude-interrupt` | Interrupt the agent mid-turn (send the equivalent of `Ctrl-C` to the running `claude` CLI) |
| `:claude-code-start` | Start the IDE server explicitly (no terminal) |
| `:claude-code-stop` | Stop the IDE server and disconnect the agent |

---

## Quick start

```
:claude
```

`:claude` does three things in one step:

1. Starts the IDE server (a loopback WebSocket the agent connects
   back to), binding a fresh port on `127.0.0.1`.
2. Spawns the `claude` CLI in a [terminal buffer](terminal.md), with
   `CLAUDE_CODE_SSE_PORT` and `ENABLE_IDE_INTEGRATION` injected into
   its environment so the agent discovers and attaches to *this*
   editor.
3. Activates `claude-code-mode` on that terminal buffer — the minor
   mode that carries the protocol status and diff affordances.

The agent runs inside a normal terminal buffer, so everything from
[Terminal Mode](terminal.md) applies: type your prompt to the agent
in Terminal-Insert (`i` / `a`), scroll its output with vim motions in
Normal-in-terminal, exit Insert with `<C-\><C-n>` or `<Esc>`.

If you only want the server running (for example, to attach a `claude`
process you started yourself), use `:claude-code-start` and
`:claude-code-stop` instead — they manage the server without spawning
a terminal.

---

## What the agent can do

Once attached, the agent drives the editor through three kinds of
operation.

### Reads

The agent can ask the editor for context without changing anything:

- the **current selection** and the file it's in,
- the **open editors** (which buffers you have open and their paths),
- **diagnostics** for a file (errors / warnings from the LSP),
- whether a document has **unsaved changes**,
- the **workspace folders**.

These reads answer from a snapshot — they never block your typing.

### Edits via diff

When the agent wants to change a file, it does **not** mutate the
buffer behind your back. It opens an interactive **side-by-side diff**
that you review (see the next section). This is the agent's only edit
path for in-place changes — nothing is written until you accept.

### Opens / saves

The agent can open a file (jumping you to a specific line/column),
save a buffer, or close one. These reuse the editor's normal open /
save / close paths.

### Push context to the agent

The reverse direction: `:claude-send` takes the current file plus your
selection and sends it to the attached agent as an `@`-mention,
adding it to the agent's context. Use it when you want the agent to
look at the lines you're staring at without describing them in prose.

---

## Reviewing proposed edits

When the agent proposes a change, lattice opens it as a **side-by-side
two-pane diff** — the original file on the **left** and the agent's
proposed change on the **right** — in fresh splits to the right of the
`:claude` terminal, which stays put. This is the same diff engine
described in [Diff & merge](diff.md), so the sign column, both-pane
hunk tints, and `]c` / `[c` hunk navigation all work as usual.

To keep review fast, the diff opens **folded to just the changes**
(unchanged code collapsed, both panes in lockstep) with the cursor
already on the **first change** — see
[Folding the unchanged code](diff.md#folding-the-unchanged-code). `zR`
shows the whole file; `ui.diff.fold-unchanged` / `ui.diff.context` tune
or disable it.

You resolve the proposal with:

| Command | Outcome |
|---|---|
| `:diff-accept` | Accept the proposed change — it's **written to disk**, and the agent sees the file as saved |
| `:diff-reject` | Discard the proposal — nothing is written, and the agent is told the diff was rejected |

The agent's request blocks until you resolve it; there's no timeout —
review at your own pace. Closing the diff mid-review counts as a
reject, and so does the **agent** abandoning the proposal — if you tell
`claude` "no" in the terminal, it withdraws the edit and lattice tears
the diff down for you (same outcome as `:diff-reject`). That teardown is
**scoped to the agent session that opened the diff**: if you have more
than one `claude` running, one session closing its diff never touches
another's.

**Both sides are editable.** Just like `:diffsplit`, neither pane is
read-only. You can tweak the agent's proposal on the right (or adjust
the baseline on the left) before accepting — accept writes the
proposed side's **live** content, so your edits to the proposal
persist. The review *is* the save: there's no separate write step.

---

## Interrupting the agent

To stop the agent mid-turn — it's heading down the wrong path, or
you've seen enough — use:

```
:claude-interrupt
```

It sends the interrupt (the equivalent of `Ctrl-C`) to the running
`claude` CLI, the same as pressing `Ctrl-C` in a normal terminal.

Why a command instead of a key? The agent runs in a [terminal
buffer](terminal.md), and in that buffer `<Esc>` is **modal** — it
leaves Terminal-Insert and drops you into Normal mode (so you can
scroll the agent's output with vim motions). That `<Esc>` never reaches
the agent, so it can't interrupt. `:claude-interrupt` is the explicit,
unambiguous way to signal the running turn.

---

## Server lifecycle and security

The IDE server is what the agent connects to. It starts on `:claude`
or `:claude-code-start` and stops on `:claude-code-stop`. Stopping it
**disconnects the attached agent** — the connection is closed, not
left dangling.

Security is enforced at two layers:

- **Loopback only.** The server binds `127.0.0.1`, so nothing off the
  local machine can reach it.
- **Token auth.** On start, the server writes a discovery lockfile at
  `~/.claude/ide/<port>.lock` containing an auth token. The agent
  reads the lockfile to find the port and present the token; a
  connection without the right token is rejected. The lockfile is
  removed when the server stops.

`claude-code-mode` shows a status segment on the agent terminal's
modeline while the server is running: `claude :PORT` plus the live
connection count. It's hidden when the server is stopped.

---

## Provisional status

The integration speaks the **same WebSocket + MCP contract** as the
VS Code, JetBrains, and `claude-code-ide.el` (Emacs) integrations, so
the same `claude` CLI attaches to any of them. The exact reply and
notification payload shapes lattice sends back are **provisional**
until validated against a live `claude` CLI — they're modeled on the
VS Code contract but not yet confirmed end-to-end. The user-facing
workflow (launch the agent, it proposes edits as reviewable diffs, you
accept or reject) is stable; only the wire details may still shift.

---

## Related

- [`terminal.md`](terminal.md) — the terminal buffer the agent runs in.
- [`diff.md`](diff.md) — the side-by-side diff UI used to review edits.
- [`docs/dev/architecture/ide-protocol.md`](../dev/architecture/ide-protocol.md)
  — developer reference (protocol, mode ownership, data paths).
