# Agent integration — the `EditorAccess` port, and the adapters built on it

> **Status:** implemented (2026-07-10). The `EditorAccess` port and both
> adapters (Claude Code / MCP, opencode / ACP) are live in `lattice-agent`
> and `lattice-ai`; the ACP adapter additionally owns the conversation UI
> (see `agent-ui.md`) with interactive diff review and trust mode.
> Sequencing lived in
> `docs/dev/operations/slice-plans/archive/agent-integration.md` (now
> archived — all slices complete). This fragment supersedes
> `ai-agent-protocol.md` §1, §3 and §8; that document remains authoritative
> for the ACP wire contract (§5), auth (§5b), and the per-process log
> buffers (§6).

lattice integrates two coding agents today — Claude Code and opencode — over two
different protocols, in two opposite directions. This document explains why that
is not an accident, why it does not require two subsystems, and where the seam
between them belongs.

## 1. The boundary is the capability surface, not the transport

The obvious reading of the two integrations is that they are inverted topologies
and therefore unrelated:

- **Claude Code.** lattice binds a loopback WebSocket and writes a discovery
  lockfile to `~/.claude/ide/<port>.lock`. The stock `claude` CLI finds it and
  connects *into* lattice, then calls tools on us. Claude's own TUI runs in a
  lattice terminal buffer. **lattice is a server; the agent is the client.**
- **opencode.** lattice spawns `opencode acp` as a child process and drives it
  over stdio, sending `initialize` → `session/new` → `session/prompt` and
  consuming a `session/update` stream. **lattice is the client; the agent is a
  subprocess.**

That framing is what led an earlier revision of `ai-agent-protocol.md` §8 to
reject folding the two together as "mixes two topologies in one crate."

It is the wrong axis. Transport direction is not what the code is made of. Look
at what the two protocols actually carry:

| Concern | MCP (Claude Code) | ACP (opencode) |
|---|---|---|
| Read a file / the selection | `getCurrentSelection`, `getOpenEditors`, `checkDocumentDirty` | `fs/read_text_file` |
| Mutate the editor | `openFile`, `saveDocument` | `fs/write_text_file` |
| Get the user's verdict on an edit | `openDiff` (blocking) → `DiffOutcome` | `session/request_permission` + diff content |
| Diagnostics | `getDiagnostics` | *(none — pushed as prompt context)* |
| Conversation | *(none — lives in Claude's TUI)* | `session/prompt` + `session/update` |
| Lifecycle | bind WS, write lockfile, agent dials in | spawn child, ACP handshake |

The bottom two rows genuinely differ. **The top three are the same operations in
different encodings.** "Give me the selection", "write this file", "ask the user
to approve this diff" mean one thing regardless of who opened the socket.

That yields the statement this whole design rests on:

> **MCP serves. ACP serves *and* drives.**
>
> They share the serving half completely. The ACP adapter additionally owns a
> conversation; the MCP adapter delegates it to the agent's own TUI.

So the seam belongs at the **capability surface** — what an agent may ask the
editor to do — which is direction-independent. Not at the transport.

## 2. `EditorAccess` — the port

`lattice-agent` owns one clone-able handle. It is the complete set of things an
agent may exercise on the editor:

```rust
impl EditorAccess {
    // Reads — served from the state cache + the buffer store.
    async fn current_selection(&self) -> Option<Selection>;
    async fn open_editors(&self) -> Vec<OpenEditor>;
    async fn workspace_folders(&self) -> Vec<String>;
    async fn document_dirty(&self, path: &Path) -> bool;
    async fn read_text_file(&self, path: &Path, line: Option<u32>, limit: Option<u32>)
        -> Result<String, AgentError>;

    // Writes — emitted as `Effect`s over the inbound bus, applied on the editor thread.
    async fn open_file(&self, path: &Path, column: Option<u32>) -> Result<(), AgentError>;
    async fn write_text_file(&self, path: &Path, contents: String) -> Result<(), AgentError>;
    async fn save_document(&self, path: &Path) -> Result<(), AgentError>;

    // Approval — the headline seam. Blocks until the user decides.
    async fn review_diff(&self, req: DiffReviewRequest) -> Result<DiffOutcome, AgentError>;

    // Session-scoped teardown. Rides the *write* bus (→ `Effect::CloseSessionDiffs`
    // / `CloseAllSessionDiffs`, applied host-side), NOT `ProgrammaticDiffBus`.
    async fn close_session_diffs(&self, origin_session: u64, tab_name: Option<String>);
}
```

It is a **concrete struct, not a trait.** There is exactly one implementation —
the host — and a trait would buy nothing while costing `async fn` dyn-compat
pain. Its constructor takes its seams (`BufferStoreHandle`, the inbound-bus
sender, `ProgrammaticDiffBus`, a `DiagnosticsSource`), so adapters are tested
against in-memory seams without a trait. If a second host ever exists, extract
the trait then, and not before.

`EditorAccess` is the surface an agent can exercise on the editor; **each adapter
exposes only the subset its protocol names.** MCP maps its tools onto it. ACP
maps `fs/*` and `session/request_permission`. Methods no ACP agent asks for
(`open_editors`, `document_dirty`) are not dead — the context-push command
(`:ai-send` today, `:agent-send` after the naming slice) builds prompt resource
blocks out of `current_selection()`, the same method backing
`getCurrentSelection`.

**`getDiagnostics` is deliberately NOT in the port.** It has no ACP counterpart,
and its natural payload is `lsp_types::Diagnostic`. Putting it in an LSP-free
port would mean declaring a parallel `AgentDiagnostic` mirroring nine fields, and
preserving its JSON shape byte-for-byte, purely to launder a type across a crate
boundary for a single consumer that already holds the real handle. That is
abstraction without a merit win (heuristic #1). `getDiagnostics` stays in the MCP
adapter, reading `lattice_lsp::modes::DiagnosticsQueryHandle` directly, and the
`DiagnosticsSource` trait this document once proposed does not exist. Revisit
only if `:agent-send` genuinely needs diagnostics as prompt context.

### `review_diff` is the unification

Both adapters converge here. `review_diff` builds a `ProgrammaticDiffRequest`,
sends it on the existing `lattice_diff::ProgrammaticDiffBus`, and awaits the
`DiffOutcome` oneshot:

- MCP's `openDiff` maps the outcome to `FILE_SAVED` / `DIFF_REJECTED`.
- ACP's `session/request_permission` maps it to `allow_once` / `reject_once`.

One implementation, two encodings, no new diff code. The host keeps what it
already keeps: it registers the bus and drains the receiver with `&mut Editor`,
opening the side-by-side diff and resolving the oneshot on `:diff-accept` /
`:diff-reject`. **`lattice-agent` is the producer and awaiter; the host is the
consumer and resolver.** That drain is irreducibly host-side and does not move.

Session-scoped teardown — reject and close every diff an origin session opened,
once that session dies — is written once. For MCP that fires on WebSocket
disconnect; for ACP it fires when the supervisor observes the child exit. Same
code, two triggers. Note this teardown is a *write*, not a diff-bus operation:
it sends `InboundKind::CloseAllDiffTabs` on the write bus, which the host maps
to `Effect::CloseAllSessionDiffs { origin_session }` and applies against its
programmatic-diff state.

## 3. How Claude Code is built on the port

`:claude` emits `Effect::SpawnTerminal` with `CLAUDE_CODE_SSE_PORT` and
`ENABLE_IDE_INTEGRATION` injected. The host spawns `claude` in a terminal
buffer; the agent reads the lockfile, opens a WebSocket back into lattice, and
authenticates with the per-session token from `x-claude-code-ide-authorization`.

From then on Claude drives lattice. Every `tools/call` is marshalling over
`EditorAccess`. The MCP adapter owns only what is protocol-shaped: the listener
and accept loop, the WebSocket handshake and auth, the lockfile's RAII
lifecycle, the JSON-RPC envelope and tool catalogue, the outbound notification
task (`selection_changed`, `didChangeActiveEditor`, `at_mentioned`), and the
modeline segment.

**Claude's conversation is Claude's problem.** Its TUI is mature, and lattice
renders none of it. This is a feature, not a gap: lattice does the one thing an
editor uniquely can — show a diff, take a verdict — and delegates the rest.

`openDiff` is the only blocking tool, so it is dispatched on its own task; the
read loop keeps polling the socket and the shutdown signal, and a pending review
never strands the connection.

## 4. How opencode is built on the port

`:opencode` spawns `opencode acp` as a stdio child. lattice drives the ACP
handshake, sends prompts, and consumes the `session/update` stream. When opencode
proposes an edit it calls `session/request_permission` back to lattice, carrying
`{ type: "diff", path, oldText, newText }`; the ACP adapter feeds that straight
into `EditorAccess::review_diff` and answers with the user's verdict.

The result is the same Keep/Reject UX Claude has, before anything touches disk.

**The cost is that lattice owns opencode's conversation surface.** Agent output
arrives as a `session/update` stream with no UI attached, so lattice must render
it: today a read-only per-process `*ai:<provider>:<index>*` log buffer, and in
time a proper conversation buffer with streaming, tool-call display, and a
prompt affordance. That is real, permanent work, and it will trail opencode's own
TUI in polish. It is accepted deliberately, because it is the only way to get
pre-write approval out of opencode (§5).

Because ACP is a standard rather than an opencode private protocol, the same
adapter carries `gemini`, `codex`, and — if ever wanted — Claude via
`@zed-industries/claude-code-acp`. Adding an agent is a `ProviderConfig`
constructor, not a subsystem.

## 5. Why opencode cannot use the Claude Code shape

This was investigated rather than assumed, because the answer determines whether
lattice takes on a conversation UI at all.

**opencode has no outbound editor callback.** It is itself a server: `opencode
serve` exposes an HTTP + SSE API and editors connect *in*. There is no lockfile
it watches, and no mechanism for its TUI to ask an external editor to approve an
edit. Its permission system resolves a deferred promise against a `permission.asked`
event rendered by *its own* UI.

**Exposing an MCP `openDiff` tool from lattice would not work.** opencode is an
MCP client and would happily list our tool — but it edits files with its built-in
`edit` / `write` / `patch` tools. Our tool would be one the model *might* call,
never an interception of a native edit. Consuming MCP is not the same as
delegating edit approval, and opencode lands on the wrong side of that line.

**opencode's own ecosystem corroborates this.** Every editor integration solves
diff review differently, and none of them by calling back into the editor from
the TUI:

| Integration | Diff review mechanism |
|---|---|
| Official VS Code extension | Client of opencode's server; renders diffs from server state |
| `sudo-tee/opencode.nvim` | **Post-hoc**: opencode writes first, the plugin shows the diff, "reject" is a *revert* |
| `nickjvandyke/opencode.nvim` | Terminal embed + context push. No diff review. |
| **Zed** | **ACP** — the only pre-write, blocking approval callback |

Zed's is the only one with the UX we want, and it is ACP. So the trade is
forced: **pre-write diff review from opencode requires ACP, and ACP requires
owning the conversation.** Terminal-hosting opencode and driving it over ACP are
mutually exclusive — one process, one mode.

Rejected along the way: a lattice-shipped opencode plugin using the awaited
`tool.execute.before` hook to block on our diff and `throw` on reject. It would
give pre-write review *and* keep opencode's TUI, but the hook is undocumented for
approval gating, runs in opencode's process, surfaces rejection as a tool error
rather than a protocol signal, and is brittle across versions. Revisit only if
the conversation UI proves untenable.

## 6. Crate layout and dependency discipline

```
lattice-agent/            # the port. no *agent-protocol* dependencies.
  editor_access.rs        # the capability handle
  diff_review.rs          # ProgrammaticDiffBus producer + awaiter
  state_cache.rs          # open-buffer set + active selection, fed by the event bus
  log/                    # AiLogger, LogRing, SessionKey, AiLogPushed, buffer names, log mode
  commands.rs             # parse_no_args + ex-command registration helpers
  error.rs                # AgentError

lattice-ai/               # the integrations (both transports, feature-gated)
  acp/                    # feature = "acp": connection, session, providers,
                          #   handle, supervisor, error, install, ACP ex-commands
  mcp/                    # feature = "mcp": server, transport, auth, lockfile,
                          #   protocol, dispatch, notifications, status, reads,
                          #   writes, diff, install, MCP ex-commands
  commands.rs             # transport-neutral :ai-log (register_ai_log_command)
  install.rs              # one Phase-B boot line: the port log substrate always,
                          #   each transport behind its #[cfg(feature = …)]
```

A "provider" is a launch spec, not a protocol — `acp/providers.rs` holds the
ACP `ProviderConfig`s (opencode/gemini/codex); the Claude Code launch spec
(terminal command + env) lives with the MCP adapter.

A "provider" is a launch spec, not a protocol. `ProviderConfig { command, args,
env, display_name }` describes an ACP child; the Claude Code entry describes the
terminal command and the environment (`CLAUDE_CODE_SSE_PORT`,
`ENABLE_IDE_INTEGRATION`) the host injects. Both are data; neither is a
subsystem.

`lattice-agent` must not depend on `agent-client-protocol`, `tokio-tungstenite`,
`dirs`, `getrandom`, or `lattice-lsp`. A third party adding an integration
depends on the port alone. That constraint is why the port is its own crate
rather than a module inside `lattice-ai`.

It *does* depend on lattice's own internal crates — `lattice-protocol`,
`lattice-mode`, `lattice-grammar`, `lattice-runtime`, `lattice-core`,
`lattice-diff`, `lattice-config` — and on `linkme`, because `AiLogPushed` is
registered as a typed bus event via `register_event!`, whose `linkme`
distributed-slice expansion also requires `#![allow(unsafe_code)]` in the
declaring module. "Protocol-free" means free of *agent wire protocols*, not of
lattice's internals.

**What is deliberately NOT extracted: the supervisor.** `AiClientHandle` is
`cmd_tx` + `ArcSwap<AiState>`, spawned as `impl AiClientHandle::spawn`.
`ClaudeCodeServerHandle` carries ten fields — dispatch context, notification
broadcast sender, status signals, review and mention trackers — and is spawned
by a free function taking a `ServerConfig` and an `EventBus`. They share the
`ArcSwap`-snapshot *idea*, not code. Unifying them would be an abstraction with
no concrete merit win, which heuristic #1 forbids. Each adapter keeps its own
supervisor; only `parse_no_args` (byte-identical in both crates today) moves.

`lattice-lsp` sits behind `feature = "mcp"` and never reaches a port-only
consumer, because `getDiagnostics` — the only thing that wants it — stays in the
MCP adapter (§2).

`lattice-claude-code` is deleted; its contents split between `lattice-agent`
(the port implementation, extracted from `reads.rs`, `writes.rs`, `inbound.rs`,
`diff.rs`) and `lattice-ai/mcp/` (everything protocol-shaped).

## 7. Mode ownership and the host contract

The acid test from `CLAUDE.md` holds: a provider landing requires **zero** new
`Editor` fields and **zero** new host `Action` variants. Handles are reached
through the boot-registered `ServiceRegistry`.

Both adapters reach the host only through pre-existing `Effect` variants —
`Echo`, `SpawnTerminal`, `TerminalInput`, `OpenBufferAtColumn`, `SaveBuffer`,
`CloseSessionDiffs`, `CloseAllSessionDiffs` — plus the one bespoke
`Effect::OpenAiLog` that opens a synthetic log buffer by name. That effect is
peer-applied through three host `Editor` *methods* (`snapshot_ai_sessions`,
`open_ai_log_in_pane`, `do_open_ai_log`), a deliberate and approved mirror of how
`:lsp-server-log` is wired: no existing `Effect` opens a synthetic buffer by
name. Ex-commands are registered once each, dashed and namespaced, with the
handler bodies in the owning crate.

Boot is one Phase-B line: `lattice_ai::install(&mut boot)`, which installs the
adapters enabled by cargo features.

## 8. Error handling

`lattice-agent` owns `AgentError` for port-level failures:

- `Bus` — nothing is draining `ProgrammaticDiffBus`; the host is misconfigured.
- `Cancelled` — the review's oneshot dropped before a verdict (buffer closed,
  session died).
- `Io` — a read/write against the editor failed.

Adapters keep their protocol errors and convert inward. This distinction is
load-bearing for retry policy: a killed child (`Process`), a dropped socket
(`Transport`), and a malformed frame (`Protocol`) demand different recoveries,
and collapsing them — as the ACP adapter did before the port existed — makes a
dead subprocess report "protocol error".

## 9. Alignment with the paramount goals

- **#1 Performance.** All protocol I/O is off the editor thread. The editor
  thread does a non-blocking channel send and a wait-free `ArcSwap` snapshot
  read. `review_diff` awaits a oneshot on a background task, never on the UI
  thread.
- **#2 Extensibility.** The port is the extension point. A new agent is a
  `ProviderConfig`; a new protocol is an adapter over `EditorAccess`; a
  third-party integration depends on `lattice-agent` alone.
- **#3 Modal editing.** Diff review is the existing `:diff-accept` /
  `:diff-reject` grammar. No agent-specific chords.
- **#4 Asynchronicity.** Each adapter owns an idle tokio supervisor task driven
  by a command channel, publishing a wait-free `ArcSwap<State>` snapshot. The
  *pattern* is shared; the code is not, and deliberately so (§6).

**UX is the higher court**, and it decided §5: post-hoc revert-based review was
rejected because the file mutates under the user before they consent, colliding
with `autoread` and undo grouping. Pre-write approval is the UX users already
know from Claude Code, and it is worth the conversation-UI cost.

## 10. Rejected alternatives

- **Keep `lattice-claude-code` and `lattice-ai` as disjoint crates.** Rejected.
  They already duplicate `parse_no_args` verbatim, the `Handle`/supervisor shape,
  and the `install(boot)` sequence — and AI‑2 would have written a second
  `ProgrammaticDiffBus` producer against the same bus, with a second chance to
  get session teardown wrong.
- **Fold everything into one crate with no port crate.** Rejected: a third party
  extending it would compile every protocol stack, and the ports would not be
  independently depend-able — defeating goal #2.
- **Make `EditorAccess` a trait now.** Rejected. One implementation exists;
  abstracting before a second is exactly the "abstraction without a concrete
  merit win" that heuristic #1 forbids. Adapters are tested against in-memory
  seams.
- **Extract a shared supervisor / client-handle.** Rejected on inspection. The
  two handles differ in arity (2 fields vs 10), in what they supervise (a child
  process vs a TCP listener), and in construction (`impl ... ::spawn` vs a free
  fn taking a `ServerConfig` + `EventBus`). Only `parse_no_args` is genuinely
  shared. Revisit if a third adapter lands and the shape recurs.
- **Extend the MCP-server topology to opencode.** Rejected — *not possible*.
  See §5: opencode has no outbound editor callback, and will not route native
  edits through an MCP tool we expose.
- **Terminal-host opencode with post-hoc revert-based review.** Rejected on UX
  (§9). Retained as the fallback if the conversation UI proves untenable.
- **A lattice-shipped opencode plugin gating `tool.execute.before`.** Rejected
  as brittle and undocumented for this use (§5). The best fallback if ACP's UI
  cost becomes unacceptable.
- **Per-agent REST/SSE clients against opencode's own server API.** Works, but
  is opencode-specific; ACP buys gemini/codex/Zed's ecosystem for the same
  effort.

> **Overturns `ai-agent-protocol.md` §8's** "Fold `lattice-claude-code` into
> `lattice-ai` — mixes two topologies in one crate. Rejected." That rejection
> reasoned about transport direction. The shared substance is the capability
> surface, which is direction-independent (§1). The two topologies survive as
> two adapters; they do not survive as two subsystems.

## 11. Open questions and version pins

- **opencode ACP diff content is version-sensitive.** Renderable
  `{ type: "diff", path, oldText, newText }` on `session/request_permission`
  landed in a 2026 fix (opencode PR #15517). Older builds send metadata with no
  diff block. The ACP adapter must verify the diff content is present and
  degrade loudly, not silently.
- **`claude-code-acp` and subscription auth.** If Claude is ever run over ACP,
  `ANTHROPIC_API_KEY` must be *unset* for the spawned process, or Claude Code
  bills per-token against the API instead of the Pro/Max subscription. Anthropic
  paused the 2026 billing changes; re-verify before depending on this.
- **Conversation UI scope.** How far lattice's ACP conversation buffer chases
  opencode's TUI (slash-commands, session picker, model switching, fuzzy file
  mentions) is undecided, and is the main risk this design accepts.
- **Log-buffer naming.** Buffers are `*ai:<provider>:<index>*` today. Once the
  MCP path also logs, the `ai:` prefix reads oddly for Claude Code. Renaming is
  deferred out of the behavior-preserving extraction.
- **Ex-command reconciliation.** Target is provider-named entry points
  (`:opencode`, `:claude`, `:gemini`) with shared namespaced verbs
  (`:agent-prompt`, `:agent-stop`, `:agent-log`, `:agent-send`). Deferred to its
  own slice; the extraction changes no command names.
