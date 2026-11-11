# Assistant tools — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons. Design contract:
> [`../../architecture/llm-assistant-tools.md`](../../architecture/llm-assistant-tools.md), read it
> first; this plan argues from it and does not restate it.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.

**Status: 🚧 AT.1–AT.6 ✅, AT.15 ✅; next is the plugin repo (AT.7–AT.10), after the plugin API release.**

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
| AT.4 | Built-in profiles; `:llm <profile> <prompt…>` | AT.2 | ✅ |
| AT.5 | `llm-tools` WIT interface and `PluginToolSource` | — | ✅ |
| AT.5a | lattice-ai: `ToolPlugin`, `PluginToolSource`, `PluginTools` service | — | ✅ |
| AT.5b | plugin host: `llm-tools` world, actor, client, fixture guest | — | ✅ |
| AT.5c | loader: drain, adapter, teardown; round-trip bench | AT.5a, AT.5b | ✅ |
| AT.5d | `*llm-tools*` listing; CHANGELOG | AT.5c | ✅ |
| AT.5e | guest SDK ergonomics for `llm-tools` | AT.5b | ✅ |
| AT.6 | Plugin profiles, trusted like project config | AT.4, AT.5 | ✅ |
| AT.7 | Org read tools (plugin repo) | AT.5 | 🚧 |
| AT.7a | Repeaters expanded in the shared agenda scan | — | 📝 |
| AT.7b | The `llm-tools` export; `org.agenda` with `match` | AT.7a | 📝 |
| AT.7c | `org.query`, `org.entry`; refs | AT.7b | 📝 |
| AT.7d | `org.clock` | AT.7b | 📝 |
| AT.7e | `org.notes` | AT.7b | 📝 |
| AT.8 | `org.change` proposals (plugin repo) | AT.7 | 📝 |
| AT.9 | Org profiles and commands (plugin repo) | AT.6, AT.8 | 📝 |
| AT.10 | The eval: fixture corpus, tasks, per-model report | AT.9 | 📝 |
| AT.11 | Embedding index, `lattice.recall` | AT.10's numbers | ⛔ |
| AT.12 | Bundled local model (managed `llama-server`) | AT.10's numbers | ⛔ |
| AT.13 | Constrained decoding for tool arguments | AT.12 | ⛔ |
| AT.14 | Plugin tool sources served to external agents over `LatticeServer` | AT.5 | ⛔ |
| AT.15 | Keyless DuckDuckGo search backend, opt-in | AT.2 | ✅ |
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

### AT.4 — Built-in profiles; `:llm <profile> <prompt…>` ✅

Built as below. The built-in profiles carry `tools` only: the model stays the user's, so a
fresh install's `:llm coding` uses whatever provider is the default. The origin variant is
`OptionOrigin::BuiltinProfile { name }`, shown as `builtin profile: coding`. The prompt form
takes the first word as the profile, always: `:llm what is new` is an unknown profile
`what`, not a prompt, so a prompt never goes to a profile it was not meant for. The prompt
is submitted before the buffer opens, as an ask's first turn is.


`general` and `coding` as in design §4, overlaid key by key by user profiles of the same
name. `:llm-menu` shows the origin of each value. `:llm <profile> <prompt…>` sends the prompt
as the first turn. User docs: `llm-guide.md` sections for both.

Tests: a plain `:llm` still offers no tools; `:llm coding` offers the five; a user
`[llm.profiles.coding] model = …` keeps the built-in tools; the prompt form sends and streams
without a keypress.

### AT.5 — `llm-tools` and `PluginToolSource`

**Carved into four, 2026-11-11**, once the verify-first step was done. The guest-call path
fits: every seam is its own world and actor, and a caller on any thread sends over an
unbounded channel and awaits a oneshot, so neither side blocks; a trap quarantines the
plugin and the caller gets an error. What it did not settle was the dependency direction
(neither lattice-ai nor the plugin host depends on the other). Decided with Dhruva: the
adapter lives in the **plugin loader**, the composition crate; lattice-ai owns the domain
logic over a `ToolPlugin` trait (design §5 "Where the pieces live"). Rejected: the plugin
host depending on lattice-ai as it does on `lattice-completion` (it would compile the whole
llm layer for no runtime gain), and a neutral trait in `lattice-mode` (llm vocabulary in a
crate that does not own the domain).

One correction to design §5 made while building AT.5a: a network-capable plugin's `propose`
tools ask before they run, as its `read` tools do, since the call sees the arguments before
any review.

- **AT.5a** — lattice-ai only. `ToolPlugin` over plain records, `PluginToolSource` (row from
  `Reach`, proposals through `lattice.edit_file` after `llm.deny`), `PluginTools` (name rules,
  status for the listing), `ToolSource::permission`, `ToolBroker::remove_source`. Tests with
  a scripted plugin: rows per reach, proposals applied and refused, a trap as the call's
  error, every reserved name refused without hiding the owner.
- **AT.5b** — `llm-tools.wit` and its world, the bindgen, `LlmToolsClient` and actor,
  `PluginSeam::LlmTools`, a fixture guest; host tests for list, call and trap. `profiles()`
  is in the interface now, so AT.6 adds no WIT. Found while building it: the world imports
  `host-services`, whose `delete-file` and the WASI preopens follow the plugin's grant, so a
  plugin holding `fs:write` for another seam could have written from a tool call. The seam's
  store is built with every prefix read-only, which makes design §5's "a call cannot write"
  true; a host test deletes under an `fs:write` grant and is refused. The loader arm is a
  `debug!` placeholder until AT.5c drains the seam.
- **AT.5c** — the loader's drain: grant → `Reach`, client → `ToolPlugin`, register, unregister
  on unload. The round-trip bench. `Reach` reads the grant the plugin was given after its
  trust tier, so a user-installed plugin whose `proc:spawn` was dropped is contained.
  Filesystem grants do not make a plugin outward: on this seam they are read-only (AT.5b),
  and a read is what `lattice.read` runs unasked. A refused registration (a native or MCP
  name, another plugin's) does not fail the load; the plugin's other seams still work.
  Tests drive the fixture through discovery: rows per grant, a call to the guest and back,
  a proposal through `edit_file`, unload withdrawing the tools, `web` refused and listed.
- **AT.5d** — `*llm-mcp*` becomes `*llm-tools*` (`llm-tools-mode`, `:help llm-tools-mode`)
  with a section for MCP servers and one for plugins, each plugin tool marked with its row;
  `:llm-mcp` kept as an alias. It repaints on `PluginToolsChanged` as on
  `McpServersChanged`. lattice's own and `web`'s tools are not listed: they are fixed and
  documented in `:help llm-chat-mode`, and the listing is for sources that can fail, be
  refused or change. CHANGELOG for AT.5 as a whole, now that it is visible.
- **AT.5e** — split from AT.5d, because the plan's "trait and registration macro in
  `lattice-plugin-api`" does not fit either crate as built. `lattice-plugin-api` is the API
  catalog and reference, not guest code; `lattice-plugin-sdk` is guest code but names no
  WIT-generated type by rule, so it composes with every world. A trait over the world's
  types would break that rule. Decided 2026-11-11: reuse `ConfigShape` rather than a new
  trait. `lattice_plugin_sdk::llm` has `input_schema::<T>()` (JSON Schema text from
  `T::schema()`, field docs as `description`, `Option<T>` fields not required) and
  `parse_args::<T>(args)` (JSON into the shape `Value`, then `T::from_value`; `null` is
  absent, empty input is `{}`, a float where an integer is wanted is an error). Both deal in
  strings, so the SDK still names no WIT type; `serde_json` joins its dependencies. The
  fixture guest now writes `outline`'s schema and parses its input through it, so
  `llm_tools_source.rs` proves the SDK's text is accepted by the host and a wrong input comes
  back as the typed `err` naming the field. Keys are the derive's kebab-case wire names.
  `plugin-patterns.md` gains "Tools for the assistant", quoting the fixture. No bench: it
  runs in the guest, once per call, beside the `call` round trip AT.5c measures.

The original plan for the whole slice:

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

Landed as: `filter_plugin_profile` in `settings.rs` (the trust filter, one message per
refusal) and a `PluginProfileStore` beside `PluginTools`, which fills it at registration and
empties it per plugin on unregister. `Layers.plugin_profiles` is a snapshot per request, as
the other layers are, applied between the built-in profile and the user's under
`OptionOrigin::PluginProfile { name, plugin }`; `profile_names()` lists them, so `:llm
org-plan`, `:llm-profile` and `@org-plan` reach them with no further change. Also refused:
a built-in profile's name, and a name that is not one word. `*llm-tools*` lists each
plugin's profiles under its tools, and refusals beside the tools' problems. The loader's
`ClientPlugin` forwards `profiles()`; `llm_tools_drain.rs` proves the fixture's profile
crosses the WIT filtered (its `fixture.outline` selection is not the plugin's own, loaded as
`notes`) and leaves on unload. A plugin whose `profiles()` fails keeps its tools; the
listing says the profiles could not be listed. No bench: the filter runs once per plugin
load, and resolution clones one more map per request, of a handful of entries.

### AT.7 — Org read tools (plugin repo)

`org.agenda`, `org.query`, `org.entry`, `org.clock`, `org.notes` over the plugin's existing
agenda, match, clock and roam code. Every result opens with today's date. Rows carry refs
(design §6). Descriptions written for small models: one sentence of what, one of when.

Tests in the plugin's integration suite, against a fixture tree: repeaters expanded, overdue
split out, a match with properties and a deadline window, clock totals over a range that
cuts an entry, roam backlinks.

Decided 2026-11-12, after mapping the plugin: its agenda did not expand repeaters (a row is
one stamp date; `repeat.rs` served habits only) and its matcher refuses date comparisons.
Repeaters are expanded in the shared agenda scan, so `:org-agenda` and `org.agenda` agree;
dates stay `org.agenda`'s, which takes a `match`, and `org.query` has no date terms (design
§6). Built on a plugin branch against the sibling checkout through an uncommitted
`[patch.crates-io]`; the crates.io pins and `min_lattice` move when the API is published.

- **AT.7a** — `agenda.rs`: a row whose stamp has a repeater yields one dated row per
  occurrence inside the span (`repeat::next_date` from the stamp), the past stamp staying
  the overdue one, as Org shows it. User-visible in `:org-agenda`. Tests: `+1d`, `+1w`,
  `.+`/`++` cookies, a repeater starting after the span, a done item not repeated.
- **AT.7b** — `export llm-tools` in the `org-plugin` world, `"llm-tools"` in `provides`,
  `fs:read` reach via the existing grants. `org.agenda { range, match? }` over `scan_file`
  (text, no tree: the tool runs with no buffer). Every answer opens with today. The tool
  inputs are `lattice_plugin_sdk::llm` structs.
- **AT.7c** — `org.query { match }` and `org.entry { ref }`; the ref is
  `path::line::heading` or the entry's `ID`, a stale one refused with "query again".
- **AT.7d** — `org.clock { from, to, by }`, totals from `clock_scan::scan` cut to the range.
- **AT.7e** — `org.notes { query }` over the roam index (title, alias, tag) with backlinks.

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

### AT.15 — Keyless DuckDuckGo search ✅

`search = "duckduckgo"`: the `html.duckduckgo.com` results page, parsed for result links and
snippets, as gptel-agent does through eww. It needs no account or server, which makes it the
zero-setup option. It is scraping, so it is **opt-in and never the default**, and the guide
says plainly that it can break from a markup change or a CAPTCHA. At most two searches in
flight, like gptel-agent's, so a model looping on search does not get the user rate-limited.
Tests: a fixture page decoded without a socket; a CAPTCHA or changed page is an error that
names the backend and suggests SearXNG, never an empty result that reads as "nothing found".

Landed as `SearchBackend::DuckDuckGo` (no table: `search = "duckduckgo"` is the whole
config) and `decode_duckduckgo` in `web.rs`: the page is cut at each `result` block, ads
(`result--ad`) dropped, and each block's `result__a` link and `result__snippet` read, the
target taken from the `/l/?uddg=` redirect. Only DuckDuckGo's own `no-results` notice is
an empty answer; any other page with no readable result is an error, worded as a CAPTCHA
when the page carries DuckDuckGo's anomaly/challenge markers, and every DuckDuckGo
failure, a bad status included, ends with the SearXNG hint. The limit is a two-permit
semaphore held from the request to the last byte read; a third search waits rather than
failing. Tests: the fixture page, the no-results page, a CAPTCHA and a changed page, the
request over loopback, and the third search held until one answers. The real endpoint is
never reached from a test. No bench: one regex pass over a page of at most 2 MiB, off the
UI thread, next to a network round trip.

### AT.16 — Provider-native search ⛔

Anthropic's `web_search`, OpenAI's `web_search` and Gemini's Google Search grounding, as
`search = "provider"`. No extra key for a hosted model, and nothing for a local one. Deferred
because each adapter must carry its own result blocks (some with encrypted content that
round-trips) through the conversation store, and the local-first assistant does not need it.

### AT.17 — Readability before conversion ⛔

Score the page for its main content before `htmd`, so navigation and boilerplate the skip
list misses stay out of a small model's context. Deferred until the eval (AT.10) shows fetched
pages crowding the context.
