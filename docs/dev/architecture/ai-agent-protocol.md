# AI agent protocol — lattice as an ACP client for Claude Code, opencode & friends

Lattice hosts AI coding agents through **one generalized integration** rather than
a bespoke per-agent crate. The substrate is the **Agent Client Protocol (ACP)** —
the Zed-originated, LSP-shaped standard (`agentclientprotocol.com`) that both
Claude Code (via the `claude-code-acp` adapter) and opencode (native) already
speak, alongside Gemini CLI, Codex CLI, and Copilot CLI.

The payoff: adding a new agent is *launch configuration*, not a new subsystem.
Sessions, streaming, tool-call rendering, and — crucially — **interactive diff
review with accept/reject** are implemented once, in `lattice-ai`, and every ACP
agent lights up.

This is the sibling of `docs/dev/architecture/ide-protocol.md` (lattice as a
Claude Code *IDE peer*) but with the **topology inverted** — see §2. It relates
to `comparison-zed.md`: this is lattice adopting the same external-agent
architecture Zed has standardized on.

> **Status (updated 2026-07-10):** the umbrella design is **realized**. The
> ACP transport, session lifecycle, streamed conversation, interactive diff
> review (`session/request_permission` → `review_diff` → verdict), and trust
> mode are implemented in `lattice-ai/acp`. This document is now partly
> **superseded**:
> - §1, §2, §3, §8 (why-one-integration, topology, crate seam) →
>   `agent-integration.md` governs. It reframes the seam as the shared
>   *capability surface* (`EditorAccess`), not the transport, and explains
>   why Claude Code (MCP server) and opencode (ACP client) are one subsystem.
> - The conversation UI (buffer, modal input, review, trust) →
>   `agent-ui.md` governs.
> - This document remains authoritative for the **ACP wire contract** (§5),
>   **auth** (§5b), and the **per-process trace-log buffers** (§6).
> The original four-slice roadmap (§7) is historical; the work actually
> landed via the `agent-integration` and `agent-ui` slice plans (both
> archived). See `docs/dev/operations/slice-plans/archive/`.

## 1. Why ACP (and why not extend the existing claude-code peer)

The existing `lattice-claude-code` crate makes lattice an **MCP server**: the
external `claude` CLI runs in a lattice terminal buffer and connects *back* over
loopback WebSocket, driving the editor. Its interactive diff (`openDiff`) works
precisely *because* the agent calls back into lattice.

opencode's terminal TUI has **no such callback** — its permission/diff prompts
render inside *its own* TUI. So running opencode's TUI in a lattice terminal
gives the user opencode's diff review, **not lattice's**. To get lattice-native
accept/reject for opencode — the stated requirement — opencode must run
*headless*, driven by lattice. That is either opencode's REST + permission API,
or, cleanly and portably, **ACP**.

Generalizing across agents, the honest conclusion is: **"a shared terminal-AI UX
with shared `openDiff`" is, at its core, an ACP client.** The terminal becomes an
incidental rendering choice; the real shared substrate is the protocol. The whole
field agrees — Zed has moved its entire external-agent story (including Claude
Code) onto ACP with a native panel.

### Topology comparison

| | `lattice-claude-code` (existing) | `lattice-ai` (this doc) |
|---|---|---|
| Role | lattice is the **server** | lattice is the **client** |
| Who connects | agent → lattice (WS) | lattice spawns agent (stdio) |
| Wire | MCP over WebSocket | JSON-RPC (ACP) over stdio |
| Agent UI | agent's own terminal TUI | lattice-native conversation buffer |
| Diff review | agent calls `openDiff` back | ACP `request_permission` + diff content |
| Agents | Claude Code only | any ACP agent |

`lattice-claude-code` is **kept as-is** — a working, legacy MCP-server path. The
two topologies are not merged into one crate (that would muddy both). Genuinely
shared low-level bits (the read cache, selection→context formatting) may be
lifted into a shared module *later, if duplication proves real* — not upfront.

## 2. Mode-owned, minimum host touch

`lattice-ai` follows the same discipline as `lattice-claude-code`
(`feedback_mode_owns_its_surface`, `boot-composition.md`): a mode that owns its
full surface, with the App staying a thin host of generic primitives. The acid
test — *a new provider adds zero `Editor::` methods and zero new `Action`
variants* — must hold, and is stronger here: **a new agent adds zero code**, only
a `providers/` entry.

- **Boot:** one Phase-B line — `lattice_ai::install(&mut boot)` — spawns the ACP
  supervisor (idle until `:ai`/`:opencode`), registers the crate-owned
  ex-commands, the `ai-mode`, and the `AiClientHandle` service. Mirrors
  `lattice_claude_code::install`.
- **Commands:** bare dashed ex-command names (`:ai`, `:opencode`, `:ai-send`,
  `:ai-stop`) whose `apply` closures capture the `AiClientHandle` and drive it
  directly — the same mode-ownership-compliant route used by `:claude`.
- **Diff review:** reuses the **existing** `lattice_diff::ProgrammaticDiffBus`
  (§4). No new host write path; `origin_session` already anticipates non-Claude
  producers.

## 3. Architecture — the `lattice-ai` crate

```
lattice-ai/
  acp/        JSON-RPC-over-stdio transport; ACP message types; subprocess
              lifecycle (spawn agent, initialize handshake, shutdown).
  session/    session/new · session/prompt · session/update stream state
              machine (assistant text, reasoning, tool-call lifecycle).
  review/     maps ACP diff-content + session/request_permission
              → ProgrammaticDiffBus → DiffOutcome → allow_once/reject_once.
              ← the shared openDiff, reused wholesale.
  fsbridge/   fs/read_text_file · fs/write_text_file client handlers
              (the agent reads/writes through the editor's buffers/disk).
  ui/         native conversation buffer (BufferKind::AiChat) — follows the
              Messages/Dashboard rope-backed, read-only, mode-owned pattern;
              streaming transcript + tool-call render + input affordance.
  providers/  launch configs: opencode | claude-code-acp | gemini | codex.
              Each = command + args + env + display name. The extension point.
  commands.rs :ai / :opencode / :ai-send / :ai-stop …
  modes.rs    ai-mode (+ per-provider marker modes) on the AiChat buffer.
  status.rs   modeline segment (provider · session · running).
  install.rs  the single crate-owned boot entry point.
```

### Dependencies (mirroring `lattice-claude-code`)

`lattice-protocol` (ids, event bus types), `lattice-mode` (`SubsystemBoot`,
registries), `lattice-grammar` (ex-commands, `Effect`), `lattice-runtime` (async
runtime + event bus), `lattice-core` (`BufferStore`, `BufferId`, new
`BufferKind::AiChat`), `lattice-diff` (`ProgrammaticDiffBus`, `DiffOutcome`),
`lattice-lsp` (diagnostics for context, optional). tokio (process, io-util,
sync), serde/serde_json, thiserror, tracing.

## 4. The shared `openDiff` seam (already provider-agnostic)

`lattice-diff` already exposes exactly what ACP needs:

- `ProgrammaticDiffRequest { old_file_path, new_file_path, new_contents,
  tab_name, origin_session, response: oneshot::Sender<DiffOutcome> }`
- `ProgrammaticDiffBus = InboundBus<ProgrammaticDiffRequest>` (host-drained)
- `DiffOutcome::{ Accept, Reject }`

The `origin_session: u64` doc already anticipates non-Claude producers ("a
plugin", "an LSP `WorkspaceEdit` preview"). `review/` becomes a second consumer:

1. Agent streams a `tool_call` with `content: [{ type: "diff", path, oldText,
   newText }]` and status `pending`.
2. Agent invokes `session/request_permission` with options `allow_once` /
   `reject_once`.
3. `review/` builds a `ProgrammaticDiffRequest` (oldText → `old_file_path`
   baseline, newText → `new_contents`) and `send`s it on the bus; awaits the
   `DiffOutcome`.
4. `DiffOutcome::Accept` → respond `{ outcome: "selected", optionId:
   "allow_once" }`; `Reject` → `reject_once`. The agent then applies (or skips)
   the edit via `fs/write_text_file`, which `fsbridge/` services against the
   editor.

This is the headline reuse: the Keep/Reject UX users already know from Claude
Code, now driven by ACP, shared across every agent, with **no new diff code**.

## 5. Wire contract (ACP, summary)

JSON-RPC 2.0 over the spawned agent's stdio. Editor (client) ⇄ agent:

- **Lifecycle:** `initialize` (capabilities), `session/new`, `session/load`.
- **Prompting:** `session/prompt` (user text + resource blocks for
  file/selection context — the `:ai-send` payload).
- **Streaming (agent→client):** `session/update` notifications carrying
  `agent_message_chunk`, `agent_thought_chunk`, `tool_call`, `tool_call_update`
  (statuses `pending`→`in_progress`→`completed`/`failed`).
- **Permission (agent→client req):** `session/request_permission` → client
  responds `selected`/`cancelled`.
- **Filesystem (agent→client req):** `fs/read_text_file`, `fs/write_text_file`.

Provider specifics live in `providers/`: opencode is native ACP; Claude Code runs
through `npx @zed-industries/claude-code-acp`; Gemini/Codex via their ACP entry
points.

### 5b. Authentication — agent-owned, subscription-preserving

**Auth belongs to the spawned agent process, never to lattice.** lattice launches
the ACP adapter as a stdio subprocess; the adapter uses whatever credentials the
underlying CLI already holds. lattice sees no API key and manages no token.

- **Claude:** the current `@zed-industries/claude-code-acp` adapter is built on the
  Claude Agent SDK and **supports Claude Pro/Max subscription login** — it reuses
  the existing `claude login` OAuth credential (subscription quota, *not*
  pay-per-token API billing). Users are **not** forced onto an API key by adopting
  ACP. (An older adapter, `claude-agent-acp`, did require `ANTHROPIC_API_KEY`; that
  is the source of "ACP needs a key" claims and is *not* the path we take.)
- **opencode / Gemini / Codex:** each owns its own auth & model selection; lattice
  is uninvolved.

**Client requirement (rough edge):** Claude-via-ACP is beta, with known OAuth-loop
issues when the adapter must (re)authenticate *through* the protocol (zed#59767,
zed#50660). Two paths: (a) pre-authenticate out of band (`claude login` once) and
the spawned adapter just works; (b) lattice's ACP client handles ACP's
`authenticate` method / login prompt. AI‑1 assumes (a) (pre-authed) to de-risk;
handling the in-protocol `authenticate` flow is explicit scope for **AI‑4**.

## 6. Rendering — per-process log buffers (mirrors LSP logging)

Agent output is **not** routed into the shared `*messages*` buffer (that buries
genuine one-shot info events under streamed agent text — the same concern behind
the `debug!`-not-`info!` standing rule). Instead lattice-ai mirrors the **LSP
logging subsystem** (`lattice-lsp/src/logging.rs` + `lattice-ui-tui`'s
`*lsp:<server>:<workspace>*` views): a producer/consumer split with **one
dedicated buffer per agent process**, gated by user config.

- **Producer — `AiLogger`** (in `lattice-ai`, mirroring `LspLogger`): per-session
  bounded `LogRing`s keyed by a `SessionKey { provider, index }` (a second
  `opencode` session is a distinct ring/buffer, exactly as two `rust-analyzer`
  instances are). An `AiLogLevel` (Trace..Error, parseable from config) gates
  records; capacity `0` = disabled. An event publisher fires `AiLogPushed` on each
  append so views tail-follow live. Cheap clone (`Arc<Mutex<…>>`); logging is not
  a hot path.
- **Consumer — per-process buffer views** (in `lattice-ui-tui`, GPUI peer in
  lockstep): `*ai:<provider>:<index>*` synthetic Document buffers (`ReadOnly` +
  `NoFile`, like the LSP log buffers). Each snapshots its ring and refreshes on
  `AiLogPushed`. `:ai-log [session]` opens one. `session/update` chunks stream in
  as they arrive — a dedicated per-process log buffer is exactly the right home
  for streaming text, so **no per-turn coalescing is needed** (the earlier
  `*messages*` coalescing question is moot).
- **Config gate:** `ai.log` (bool, default off/on TBD-at-impl) enables capture;
  `ai.log_level` (level, default `info`) sets the default min level. Both are typed
  registered options (`:set ai.log_level=debug`), with per-session runtime
  overrides on the `AiLogger` (the LSP `set_instance_level` shape).

This front-loads the dedicated-buffer half of what §would-be AI‑3's `AiChat` buffer
becomes. AI‑3 later layers the *interactive* conversation UI (prompt affordance,
tool-call blocks, diffs-as-sessions per §4) on top; the log buffers remain the
raw per-process record view (the LSP model, where `*lsp:…*` coexists with richer
surfaces). `BufferKind::AiChat` and the interactive transcript are deferred to
AI‑3; AI‑1b ships only the log-buffer views.

## 7. Slices (each its own slice-plan → plan → build)

- **AI‑1 — ACP transport + session skeleton.** Spawn agent subprocess,
  JSON-RPC/stdio, `initialize` → `session/new` → `session/prompt`; consume
  `session/update` into a Messages-style buffer. Proves the wire against
  **opencode** (native ACP). Introduces the minimal `providers/` abstraction.
  *Exit:* type a prompt, see streamed response text in a buffer.
- **AI‑2 — Shared diff review (openDiff via ACP).** `session/request_permission`
  + diff content → `ProgrammaticDiffBus` → `DiffOutcome` → `allow_once`/
  `reject_once`; `fs/read_text_file` / `fs/write_text_file`. *Exit:* agent
  proposes an edit, user Keeps/Rejects in lattice's diff UI, disk reflects the
  verdict.
- **AI‑3 — Native conversation UI.** `BufferKind::AiChat`, streaming assistant +
  tool-call rendering, prompt affordance, `ai-mode` keymap, status segment.
  *Exit:* a pleasant native panel, not a raw log.
- **AI‑4 — Context push + breadth.** `:ai-send` (selection/file → resource
  blocks), wire `claude-code-acp` + `gemini`, a provider picker. *Exit:* multiple
  agents, context-aware prompting.

AI‑1 → a talking agent; AI‑2 → the diff-review payoff; AI‑3 → polish; AI‑4 →
breadth.

## 8. Rejected alternatives

- **Extend `lattice-claude-code` to opencode (MCP-server topology).** opencode's
  TUI can't call `openDiff` back, so lattice-native accept/reject is
  unreachable that way. Rejected (§1). **Re-verified 2026-07-09 and still true:**
  opencode is itself a server (editors connect *in* over HTTP/SSE); it has no
  lockfile discovery and no outbound editor callback, and it edits files with
  its built-in `edit`/`write`/`patch` tools, so an `openDiff` tool we expose over
  MCP would never intercept a native edit. All three of opencode's own editor
  plugins confirm this — only the Zed integration gets pre-write approval, and it
  does so over ACP. See `agent-integration.md` §5.
- **Per-agent REST/SSE clients (opencode's own server API).** Works, but is
  opencode-specific; ACP gets Claude/Gemini/Codex/Copilot for the same effort.
  Rejected in favor of the standard.
- **Terminal-hosted agent TUIs with a shared wrapper.** Keeps the agent's UI in
  a terminal buffer but then diff review lives in *that* UI, defeating the shared
  `openDiff` goal. Rejected (§1); a terminal-host mode may return later as an
  alternate, not the core.
- ~~**Fold `lattice-claude-code` into `lattice-ai`.** Mixes two topologies in one
  crate. Rejected; kept separate (§1).~~ **OVERTURNED 2026-07-09 — see
  `agent-integration.md`.** This rejection reasoned about *transport direction*.
  The shared substance is the **capability surface** an agent exercises on the
  editor (read selection, write file, review diff), which is
  direction-independent: MCP serves it, ACP serves it *and* drives a
  conversation. The two topologies survive as two adapters over one
  `EditorAccess` port; they do not survive as two subsystems. Keeping them
  disjoint would have had AI‑2 write a second `ProgrammaticDiffBus` producer
  against the same bus.

## 9. Open questions / risks

- **ACP maturity & churn:** ACP is young and moving; pin the schema version and
  isolate wire types in `acp/`. Verify the exact `claude-code-acp` invocation and
  opencode's ACP entry at implementation time.
- **Prompt input UX in a modal editor:** how the user composes a prompt (footer
  line vs. Insert capture vs. a scratch buffer) needs a focused decision in AI‑3.
- **Auth / model selection:** resolved for auth — agent-owned, subscription
  preserved (§5b); the only work is handling ACP's in-protocol `authenticate`
  flow (scheduled AI‑4; AI‑1 assumes pre-auth). Surfacing/switching *models* via
  ACP remains out of scope until AI‑4+.
- **fs/write vs. diff-review ordering:** confirm whether agents gate *every*
  write behind `request_permission` or only some; `fsbridge/` must handle
  ungated writes safely (respecting lattice's dirty-buffer state + autoread).
