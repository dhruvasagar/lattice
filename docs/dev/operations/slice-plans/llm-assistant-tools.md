# Assistant tools — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons. Design contract:
> [`../../architecture/llm-assistant-tools.md`](../../architecture/llm-assistant-tools.md), read it
> first; this plan argues from it and does not restate it.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.

**Status: 🚧 AT.1–AT.3 ✅; AT.4 next.**

**Goal:** native web tools, a WIT seam through which a plugin gives the model tools and
profiles, and an org assistant built on both, measured by an eval that decides whether a
local model is worth bundling.

**Prefix:** `AT` (assistant tools). Checked unused across `docs/` on 2026-11-11.

**Repos:** AT.1–AT.6 land in this repo. AT.7–AT.10 land in
[`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin) and need a plugin
API release carrying AT.5's interface first.

## Global constraints

The llm layer plan's constraints apply unchanged
([`llm-layer.md`](llm-layer.md) "Global constraints"): one slice per commit, targeted tests
plus `cargo check --tests`, `scripts/precommit.sh` per touched crate, the four artefacts, no
UI-thread work, async results landing through `inbound`. In addition:

- **A CHANGELOG `## Unreleased` entry in every user-facing slice.**
- **WIT changes bump the plugin API minor** and regenerate the plugin-API reference
  (`scripts/docs-check.sh` gate 4).
- **Security slices test the refusal, not just the success.** Every permission row in design
  §3 and §5 has a test that the wrong row is not reachable.

## Slices

| ID | Slice | Depends on | Status |
|---|---|---|---|
| AT.1 | Tool descriptions without stray indentation | — | ✅ |
| AT.2 | `web.search`, `web.fetch`, the `web` source, per-host grants | — | ✅ |
| AT.3 | `web.fetch(<host>)` rules; a private host allowed by name | AT.2 | ✅ |
| AT.4 | Built-in profiles; `:llm <profile> <prompt…>` | AT.2 | 📝 |
| AT.5 | `llm-tools` WIT interface and `PluginToolSource` | — | 📝 |
| AT.6 | Plugin profiles, trusted like project config | AT.4, AT.5 | 📝 |
| AT.7 | Org read tools (plugin repo) | AT.5 + API release | 📝 |
| AT.8 | `org.change` proposals (plugin repo) | AT.7 | 📝 |
| AT.9 | Org profiles and commands (plugin repo) | AT.6, AT.8 | 📝 |
| AT.10 | The eval: fixture corpus, tasks, per-model report | AT.9 | 📝 |
| AT.11 | Embedding index, `lattice.recall` | AT.10's numbers | ⛔ |
| AT.12 | Bundled local model (managed `llama-server`) | AT.10's numbers | ⛔ |
| AT.13 | Constrained decoding for tool arguments | AT.12 | ⛔ |
| AT.14 | Plugin tool sources served to external agents over `LatticeServer` | AT.5 | ⛔ |
| AT.15 | Keyless DuckDuckGo search backend, opt-in | AT.2 | 📝 |
| AT.16 | Provider-native search (Anthropic, OpenAI, Gemini server tools) | AT.2 | ⛔ |
| AT.17 | Readability scoring before HTML conversion | AT.2 | ⛔ |

### AT.1 — Tool descriptions without stray indentation ✅

`edit_file` and `write_file` descriptions (`crates/lattice-ai/src/llm/tools/lattice.rs:162`,
`:176`) continue string literals across lines without a trailing `\`, so every provider
receives runs of about 18 spaces. Fix both. Add a test that no descriptor in any catalogue
holds two consecutive spaces, so the next one cannot come back. Not user-visible; no
CHANGELOG line.

### AT.2 — The `web` source ✅

**Absorbed AT.3's grant key.** Shipping `web.fetch` with "allow for this chat" covering every
URL, even for one commit, would have been the weaker permission the design rejects. So the
broker learned `ToolSource::chat_scope` and `ChatAllow::Host` here, and AT.3 keeps the rules.
The converter is `htmd` 0.5.6 (markdown, links kept; html5ever underneath), chosen with
Dhruva on 2026-11-11. Private addresses are refused twice: on a literal host, and in the
fetch client's resolver (`PublicOnly`), so a name re-resolved at connect cannot slip past.
Search uses a second client without the filter, because a SearXNG is often on localhost.


`tools/web.rs`: a `ToolSource` with `search` and `fetch`, registered with the broker beside
`LatticeTools`. Config `[llm.web]` with the `brave` and `searxng` backends. Keys go through
the existing `api_key_env` / `api_key_command` path and are withheld from `run_command` like
every provider key. The broker's `ours` check (`broker.rs`, `offered`) becomes a set of
native sources, with `web.search` → Run and `web.fetch` → Ask.

Fetch limits as design §3: cross-host redirects re-checked, private and loopback addresses
refused, a 2 MiB body cap, text capped with an `offset`, cancel on drop. The HTML-to-text
crate is chosen here (design §12) on a fixed page set; record the choice and its compile
cost in the commit.

Tests: each backend's response decoded from a fixture without a socket; a local server for
fetch covering redirect to another host, a private address, an oversized body, a binary type
and a cancel mid-body; the key never in a tool result or `*messages*`.

### AT.3 — `web.fetch(<host>)` rules ✅

Built as below, plus one refusal the plan had not named: before AT.3, `allow = ["web.fetch"]`
and `["web.*"]` parsed as MCP-style whole-tool rules and would have let every fetch run
unasked. In `allow` they are now refused ("would allow every site"), as `lattice.*` is; in
`deny` they turn fetching off. The private-host exception needs no resolver plumbing: the
rules already travel with each call (`CallContext::rules`), so the fetch asks them whether
its host is named exactly and, if so, uses a client without the address filter.


The per-host grant landed with AT.2. What is left: `rules.rs` accepts `web.fetch(<host-glob>)`
in `allow` and `deny`, and a host the user's own `allow` names by exact name may be private
(design §3: "refused unless a rule allows that host by name"), which needs the resolver to
learn the allowed set. A project `allow` is refused as today.

Tests: an allowed host runs without asking; a glob matches subdomains and not lookalikes
(`*.rust-lang.org` against `rust-lang.org.evil.example`); a project file denying a host wins
over the user's allow; a project allow is refused and named; a private host allowed by name
is fetched and one not named is still refused.

### AT.4 — Built-in profiles; `:llm <profile> <prompt…>`

`general` and `coding` as in design §4, overlaid key by key by user profiles of the same
name. `:llm-menu` shows the origin of each value. `:llm <profile> <prompt…>` sends the prompt
as the first turn. User docs: `llm-guide.md` sections for both.

Tests: a plain `:llm` still offers no tools; `:llm coding` offers the five; a user
`[llm.profiles.coding] model = …` keeps the built-in tools; the prompt form sends and streams
without a keypress.

### AT.5 — `llm-tools` and `PluginToolSource`

WIT interface and records as design §5, in `crates/lattice-wit/wit/llm-tools.wit`, with a
world for plugins exporting it. The host side wraps a plugin's exports as a `ToolSource`:
`list` from `tools()`, `call` dispatched to the plugin's actor, proposals routed through
`lattice_agent::review_text` exactly as `edit_file`'s are. The permission row comes from the
plugin's capability grant at registration. `*llm-mcp*` becomes `*llm-tools*` with a section
per source; `:llm-mcp` stays as its alias for one release.

**Verify first:** how a guest export is called from the llm runtime (another thread than the
plugin's actor) without blocking either. The picker and multibuffer seams call guest exports
asynchronously; use the same path, and stop and ask if it does not fit.

The guest SDK in `lattice-plugin-api` gains the trait and registration macro, and a fixture
plugin exercises both kinds.

Tests: the fixture plugin's read tool runs unasked; the same plugin granted `net:http` asks;
a proposal opens a review and applies once accepted; a stale proposal is refused; a trap is
that call's error and the loop continues; a name colliding with `lattice` or `web` is not
offered and is listed as such. Bench: a plugin tool round trip, so the WASM per-call budget
(CLAUDE.md paramount #2) covers this seam.

### AT.6 — Plugin profiles

`profiles()` read at registration and merged below user profiles. The trust filter of design
§7: system prompt, parameters, destination, and tools limited to the plugin's own,
`lattice.read` and `web.*`. Refused keys are named in `*messages*`.

Tests: a profile selecting `lattice.exec` loses that selection and says so; one naming a
model loses it; a user overlay widens tools; two plugins contributing the same profile name
keep the first and name the second.

### AT.7 — Org read tools (plugin repo)

`org.agenda`, `org.query`, `org.entry`, `org.clock`, `org.notes` over the plugin's existing
agenda, match, clock and roam code. Every result opens with today's date. Rows carry refs
(design §6). Descriptions written for small models: one sentence of what, one of when.

Tests in the plugin's integration suite, against a fixture tree: repeaters expanded, overdue
split out, a match with properties and a deadline window, clock totals over a range that
cuts an entry, roam backlinks.

### AT.8 — `org.change` (plugin repo)

The seven ops of design §6 through the existing verbs, each returning proposals. A stale ref
is refused with "query again". Refile returns two proposals (design §12).

Tests: each op's proposal against an expected edit; a ref whose line moved; a refile target
that does not exist; a capture template that needs a prompt the model did not give.

### AT.9 — Org profiles and commands (plugin repo)

The five profiles of design §7 and their system prompts, kept in the plugin as text files.
Commands `:org-plan-day`, `:org-plan-week`, `:org-review`, `:org-report` as thin
`invoke-command` wrappers. Plugin `:help` page.

Tests: each command opens a chat with its profile and the expected tool offer.

### AT.10 — The eval

A fixture corpus and task set as design §9, in the plugin repo under `integration/eval/`,
with a runner driven by `LATTICE_EVAL_MODELS` (a list of `provider/model`) that writes a
table of pass rate, tool rounds and time per task per model. It does not run in CI.

The first report goes into this plan, with the models run, and closes the question for
AT.11 and AT.12: each is either un-deferred with the numbers that justify it, or dropped
with them.

### AT.11 — Embedding index ⛔

Deferred until AT.10 shows structured and lexical retrieval missing prose questions. If
built: a native `lattice.recall` over any text, an off-thread incremental index, embeddings
from any OpenAI-compatible `/v1/embeddings` endpoint.

### AT.12 — Bundled local model ⛔

Deferred until AT.10 shows a laptop-sized model passing with this harness. If built:
`llama-server` and a GGUF installed SHA-pinned and supervised through Lighthouse's
machinery, registered as a provider, opt-in only. Includes a latency bench with inference
running, against the one-frame ceiling.

### AT.13 — Constrained decoding ⛔

With AT.12: a JSON Schema for tool arguments through the runtime's grammar support, in the
OpenAI-compatible adapter.

### AT.14 — External agents ⛔

Plugin tool sources served through `LatticeServer`, so Claude Code and other MCP clients can
use org's tools. Deferred until someone wants it: the design's reason for not building an org
MCP server is that this costs one slice whenever it is wanted.

### AT.15 — Keyless DuckDuckGo search

`search = "duckduckgo"`: the `html.duckduckgo.com` results page, parsed for result links and
snippets, as gptel-agent does through eww. It needs no account or server, which makes it the
zero-setup option. It is scraping, so it is **opt-in and never the default**, and the guide
says plainly that it can break from a markup change or a CAPTCHA. At most two searches in
flight, like gptel-agent's, so a model looping on search does not get the user rate-limited.
Tests: a fixture page decoded without a socket; a CAPTCHA or changed page is an error that
names the backend and suggests SearXNG, never an empty result that reads as "nothing found".

### AT.16 — Provider-native search ⛔

Anthropic's `web_search`, OpenAI's `web_search` and Gemini's Google Search grounding, as
`search = "provider"`. No extra key for a hosted model, and nothing for a local one. Deferred
because each adapter must carry its own result blocks (some with encrypted content that
round-trips) through the conversation store, and the local-first assistant does not need it.

### AT.17 — Readability before conversion ⛔

Score the page for its main content before `htmd`, so navigation and boilerplate the skip
list misses stay out of a small model's context. Deferred until the eval (AT.10) shows fetched
pages crowding the context.
