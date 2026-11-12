# Assistant tools — the web, plugin tool sources, and an org assistant

> **Design fragment. Status: 📝 designed 2026-11-11, nothing built.** Slice sequencing:
> [`../operations/slice-plans/llm-assistant-tools.md`](../operations/slice-plans/llm-assistant-tools.md).
>
> **Builds on** [`llm-layer.md`](llm-layer.md) (requests, profiles, the `ToolBroker`, the
> trust table and safety lists of §6), [`plugin-host.md`](plugin-host.md) (seams and
> capabilities) and [`org-mode.md`](org-mode.md) (the plugin whose verbs the org tools wrap).
> This document adds to those and restates none of them.

## 1. Why

The llm layer gives a model three groups of tools: read, edit and exec, all over files in the
project. That is enough for coding. It is not enough for anything that needs the world outside
the project, and it cannot reach knowledge that lives in a plugin. Org is where both gaps show.
Asked to plan a day, a model with only `read_file` and a regex `search` has to parse
timestamps, repeaters and drawers itself, and small models are worst at exactly that.

The goal is an assistant for org files that can plan, report, manage tasks, search and take
notes, and that works on a small local model. **The model is swappable; the harness is the
feature.** Nearly all of it is already built:

- the request loop, profiles, the review gate and the permission popup (`llm-layer.md`);
- an OpenAI-compatible provider, which Ollama and `llama-server` already speak.

What is missing is three mechanisms, and nothing else:

1. **Web tools, native**: search and fetch, under the same trust rules as every other tool.
2. **Plugin tool sources**: a WIT interface through which a plugin gives the model tools.
   Org uses it for the tools only org can provide.
3. **Profiles a plugin contributes**: so an org workflow is a system prompt and a tool
   selection, not new code in the harness.

## 2. Shape

	                ┌──────────── the llm layer, unchanged in shape ─────────────┐
	:org-plan-day ─▶│ profile "org-plan" = system prompt + tool selection        │
	:llm org-plan   │        │                                                   │
	                │        ▼                                                   │
	                │   ToolBroker ── lattice.*  read / list / search / edit / … │
	                │              ├─ web.*      search, fetch           (§3)    │
	                │              ├─ org.*      a plugin tool source    (§5)    │
	                │              └─ <mcp>.*    servers the user names          │
	                │        │                                                   │
	                │        ▼                                                   │
	                │   any provider: hosted, Ollama, later a bundled one (§9)   │
	                └────────────────────────────────────────────────────────────┘

**Native tools do the general work; a plugin adds only what nothing else can.** The org
plugin gets no `org.read_file` or `org.web_search`: it gets `lattice.read` and `web.*` like
every other profile. Its own tools are the ones that need org's parser: the agenda, matches,
clock totals and structured changes.

## 3. Native web tools

A second native source, `web`, beside `lattice`, in `lattice-ai/src/llm/tools/web.rs`. It is a
separate source rather than two more `lattice.*` tools, because the kind groups mean
something: `lattice.read` runs without asking, and a fetch must not join that group by being
a kind of read.

| Tool | Does | Permission |
|---|---|---|
| `web.search` | A query to the search backend the user configured; returns title, URL and snippet for up to `n` results | Runs |
| `web.fetch` | GET a URL; HTML converted to text; capped, with an offset to read on | Asks, per domain |

	[llm.web]
	search = "searxng"	# or "brave"
	
	[llm.web.searxng]
	base_url = "http://localhost:8888"	# a self-hosted instance; no key
	
	[llm.web.brave]
	api_key_env = "BRAVE_API_KEY"	# hosted; paid past a monthly credit

**Why search runs and fetch asks.** A search goes to a backend the user chose, with a query
the user can read in the transcript. A fetch can go anywhere. A model holding private data
(the read tools), untrusted input (any tool result) and a way to reach an arbitrary host can
be talked by an injected page into fetching `evil.example/?q=<your notes>`. The question is
the gate. It names the host, since the host is what the user is deciding on.

**"Allow for this chat" covers the domain.** Per tool would allow every host after the first.
Per exact URL would ask again for every page of the same documentation. The domain is the
unit a person reasons about. This takes one generic change to the broker: a source names the
grant key for a call, as `run_command` already does implicitly with the exact command (§6 of
`llm-layer.md`).

**The rules extend, they do not fork.** `allow` and `deny` take `web.fetch(<host>)` in the
shape `run_command(...)` already uses: `allow = ["web.fetch(docs.rs)", "web.fetch(*.rust-lang.org)"]`.
A project file can deny a host and cannot allow one, as for every other list.

**Fetch limits.**

- A redirect to another host is a new host: it is checked and asked about again.
- Loopback, link-local and private addresses are refused unless a rule allows that host by
  name. A model should not be able to probe the user's network through a page that says to.
- The body is read to at most 2 MiB. The text returned is capped like `read_file`'s, with an
  `offset` to read on.
- Only `text/*`, HTML, JSON and XML are converted. Anything else is reported by type and size.
- The request runs on the layer's runtime and is dropped on cancel, like every other call.

Search backends are a small enum, not a plugin seam: two prove the shape, one keyless and
self-hostable (SearXNG) and one hosted and keyed (Brave). **SearXNG leads**: the
assistant this serves is local and private by default, and a hosted search API would send
every query to a third party behind an account and a card (Brave's free plan ended in 2026;
it is now a monthly credit, then paid). Anything else reaches the model through MCP today
(`[llm.mcp.*]`), with no lattice code.

**Why two tools, not one.** Search is discovery (a query in, a few pointers out); fetch is
retrieval (a URL in, a page out). Merged, either every search fetches every result, which
floods the context, or the model can never read the page it chose. More decisively, they
differ in trust: a search's destination is fixed by the user, a fetch's is chosen by the
model, possibly from text it just read. One tool would carry one permission, and either
choice is wrong for half of it. Each is also useful alone: fetching a URL the user pasted,
answering from a snippet.

**A third, for zero setup: DuckDuckGo (AT.15).** gptel-agent's choice: it scrapes
`html.duckduckgo.com`, so it needs no account, key or server, and it breaks when the markup
changes or a CAPTCHA appears. So it is opt-in and never the default, and it fails loudly: a
page with no result it can read is an error naming the backend and pointing at SearXNG,
because an empty list would read to the model as "nothing on the web", which is the worst
wrong answer a search can give. At most two of its searches run at once, so a model looping
on search does not get the user's address rate-limited.

**Considered for later, in the slice plan:** provider-native search
(Anthropic's, OpenAI's and Gemini's server tools, no extra key but hosted models only, and
each adapter must round-trip its own result blocks); readability scoring before conversion,
to drop a page's boilerplate before it reaches a small model's context.

## 4. Built-in profiles

Tools are off unless a profile or `:setlocal` selects them, and today no profile ships. That
stays the default: a plain `:llm` chat has no tools. What changes is that two profiles come
with lattice, so the basics need no config:

| Profile | Tools |
|---|---|
| `general` | `lattice.read`, `web.search`, `web.fetch` |
| `coding` | `lattice.read`, `lattice.edit`, `lattice.exec`, `web.search`, `web.fetch` |

A user profile with the same name overlays the built-in one key by key, so
`[llm.profiles.coding] model = "local/qwen"` changes the model and keeps the tools. `:llm-menu`
shows `builtin` as the origin of a value that came from one. `OptionOrigin` gains that
variant, beside the `Profile` and `Project` variants the layer already added.

`:llm <profile> <prompt…>` opens a chat with that profile and sends the prompt as its first
turn. `:llm <profile>` keeps its meaning. This is what lets a plugin command start a workflow
(§7) with nothing but `invoke-command`.

## 5. Plugin tool sources

A plugin exports `llm-tools`; the host wraps it as a `PluginToolSource`, one more
`ToolSource` beside `LatticeTools`, `web` and each MCP connection. The descriptors are
MCP's shape, as the broker's already are, so the plugin gets MCP's data model without its
transport.

	interface llm-tools {
		use types.{llm-tool, llm-tool-result, llm-profile};
	
		/// The tools this plugin offers, listed once at registration.
		tools: func() -> list<llm-tool>;
	
		/// Run one. `args` is the model's JSON input as text. The result is data:
		/// text for the model, and edits the host reviews before any is applied.
		call: func(name: string, args: string) -> result<llm-tool-result, string>;
	
		/// Profiles this plugin contributes (§7). Listed once at registration.
		profiles: func() -> list<llm-profile>;
	}

	record llm-tool {
		name: string,
		description: string,
		/// JSON Schema for the input, as text.
		input-schema: string,
		/// `read` or `propose`. There is no exec kind for a plugin tool.
		kind: llm-tool-kind,
	}
	
	record llm-tool-result {
		text: string,
		/// Exact replacements, with the semantics of `lattice.edit_file`.
		proposals: list<proposed-edit>,
	}
	
	record proposed-edit {
		path: string,
		old-string: string,
		new-string: string,
	}

**A call cannot write. That is structural, not trusted.** `call` returns data and nothing
else. It has no effect list, and the store a call runs in has every filesystem prefix of the
plugin's grant made read-only: a plugin may hold `fs:write` for its other seams (org's capture
writes files), and a tool call still cannot use it, through WASI or `host-services.delete-file`.
Only the plugin's private `/data` stays writable, which is its own state and not the user's
files. A plugin that wants to change a file returns a proposal, and the broker sends it through the
same review `lattice.edit_file` uses, against the file as the user has it, refused if stale,
and applied as one minimal edit. Trust in the chat (`<C-t>`) applies to proposals as it does
to lattice's edits, and `llm.review` still fences paths.

**Whether a plugin's tool asks depends on what the plugin can do, not on what it claims.**

| The plugin holds | `read` tools | `propose` tools |
|---|---|---|
| No `net:*`, no `proc:spawn` | Run | Review |
| Either | Ask, unless allowed by name | Ask, then Review |

A plugin with no network and no process capability can only answer from what it reads. Its
`read` tool is then no more dangerous than `lattice.read`. A plugin that can reach the
network could send a tool's arguments anywhere, so its tools ask like an MCP server's, and
that includes its `propose` tools: a review gates the edit a call returns, not the call,
which has seen the arguments by then. The row is decided from the capability grant at
registration, so a plugin cannot move between rows by describing its tools differently.

**Where the pieces live.** The plugin host owns the `llm-tools` world, the actor and a plain
client with no llm types. lattice-ai owns `PluginToolSource`: the permission row, the
proposal path and the name rules, over a small `ToolPlugin` trait. The plugin loader, which
already depends on every subsystem it registers seams into, adapts the client to that trait
and registers it into lattice-ai's `PluginTools` service. So the plugin host does not compile
the llm layer, and lattice-ai does not compile wasmtime.

**A proposal applies through `lattice.edit_file` itself**, called by `PluginToolSource` with
the request's context: one code path for the fence, the staleness check, review and `<C-t>`.
Because that call does not pass through the broker, `PluginToolSource` checks `llm.deny`'s
`edit_file(...)` rules for each proposal first.

**The fence for a proposal is the project or the plugin's `fs:write` grant.** Org's files live
in `~/org`, not in the project a chat was opened from, so the project fence alone would refuse
every org change made from a code project. `PluginToolSource` sets the context's `writable`
to the grant's write prefixes, already filtered by trust tier, for its own proposals only;
`edit_file` then also takes an absolute path under one of them, through the same lexical,
symlink and `llm.exclude` checks. This adds no write power: the plugin can already write
there through its other seams, and the model never chooses the path, since the plugin derives
it from a ref it resolved. An edit outside the project is **always reviewed**, `<C-t>` or not:
trust in a chat is trust in what it does to the project. A request's own context never carries
`writable`, so the model's `edit_file` stays in the project. Rejected: a user option widening
every edit (`llm.edit-roots`), which would hand the model's raw `edit_file` write reach over
`~/org`, where text a page fetched into the chat could ask for anything.

**Calls run where every guest call runs:** on the plugin's own actor, fuel-limited, async to
the request. A trap is that call's error; the plugin host's restart policy applies to the
plugin as it does for any seam. The broker sees an error result and the loop continues.

Tools are named `<plugin>.<tool>` like any source's. A plugin whose short name collides with
a native source (`lattice`, `web`) or a configured MCP server is not offered, and `*llm-mcp*`
says so. The listing is renamed `*llm-tools*` with a section per source, since it now lists
more than MCP.

## 6. Org's tools

Only what needs org's parser. Each returns compact text, never raw org, and starts with
today's date in the user's time zone, because the model must never infer it.

| Tool | Kind | Returns |
|---|---|---|
| `org.agenda` | read | Scheduled items, deadlines and TODOs for `day`, `week` or a date range, with overdue and upcoming split out, optionally narrowed by a `match` (`+work`). Dates computed by the plugin, repeaters expanded |
| `org.query` | read | Entries matching the plugin's own agenda match syntax (`+work-meeting/TODO`, `STYLE="habit"`): tags, properties and TODO state, as rows |
| `org.entry` | read | One entry: heading, state, tags, planning, properties, body, children's headings |
| `org.clock` | read | Clocked time per entry over a range, grouped by tag or file |
| `org.notes` | read | Roam notes matching a title, alias or tag, with their backlinks |
| `org.change` | propose | One structured change: `set_state`, `schedule`, `deadline`, `tag`, `refile`, `capture`, `add_note` |

**The tools read what the agenda reads.** Repeaters are expanded in the agenda's own scan,
so `:org-agenda` shows a `+1d` item on every day of its span as Org does, and the assistant
cannot answer "what's on Thursday" differently from the agenda the user is looking at. Two
answers to one question is the failure this rules out, so expansion is not done in the tool.

**Dates are `org.agenda`'s; `org.query` has none.** Org's own match syntax compares dates
(`DEADLINE<"<+1w>"`), but the plugin's matcher does not, and a row carries one date, not
every stamp of an entry. Rather than a second date vocabulary, `org.agenda` takes an optional
`match` in the existing syntax: "work deadlines this week" is `org.agenda` over `week` with
`+work`. One date path and one match grammar give a small model fewer forms to get wrong.
Comparisons in the matcher wait until a question needs them that this cannot answer.

**Rows carry a ref**, `path::line::heading`. `org.entry` and `org.change` take it. A ref whose
heading is no longer at that line is refused with "query again", the same rule
`lattice.edit_file` follows for a stale target. An `ID` property is used instead when the
entry has one.

**The model emits intent; the plugin writes org.** `org.change { "ref": …, "op": "schedule",
"date": "2026-11-14" }` is turned into text by the plugin's existing verbs (`planning.rs`,
`todo.rs`, `refile.rs`, `capture.rs`), and the result is a proposal. The model never writes a
timestamp, a drawer or a `:LOGBOOK:`. Those are the edits small models get wrong, and their
being wrong is invisible until the agenda misses an item.

**General work uses general tools.** Free-text search is `lattice.search`. Reading a whole
file is `lattice.read_file`. A note written from scratch can be `lattice.write_file`, and
research is `web.*`. None of them is duplicated under `org.`.

## 7. Workflows are profiles

A workflow is a system prompt, a tool selection and nothing else. The org plugin contributes
these through `profiles()`:

| Profile | For | Tools |
|---|---|---|
| `org` | General help over your org files | `org.*`, `lattice.read` |
| `org-plan` | Plan a day or a week | `org.agenda`, `org.query`, `org.entry`, `org.change` |
| `org-review` | A weekly review: done, slipped, stuck | `org.agenda`, `org.query`, `org.clock`, `org.change` |
| `org-report` | A report of work over a period | `org.clock`, `org.query`, `org.entry` |
| `org-research` | Research a topic into a note | `org.notes`, `lattice.read`, `web.*`, `org.change` |

**A plugin profile is trusted like project config, because it arrives with something
installed, not something written.** It may set a system prompt, parameters and a
destination. It may select its own tools, `lattice.read` and `web.*`. It may not select edit
or exec tools, another source's tools, a model or a provider. The plugin cannot know which
model the user has, and naming one would be a provider choice made by the wrong party. A
refused key is named in `*messages*`, as for a project file.

**A name belongs to whoever took it first.** A built-in profile's name is never a plugin's:
`coding` from a plugin would change what `:llm coding` means for everyone who installed it.
Between two plugins the first loaded keeps the name and the second is told, in
`*llm-tools*` as well as `*messages*`, rather than one silently replacing the other. A
plugin's profiles leave with it when it unloads.

**The user overlays it,** as for built-in profiles: `[llm.profiles.org-plan] model =
"local/qwen3"` picks the model for that workflow, and `tools = …` there widens it, since that
is the user's own file.

**Commands are thin.** `:org-plan-day` is a plugin command whose handler returns
`invoke-command` for `llm org-plan Plan today.` (§4). It adds no invocation seam, no
harness code and nothing in `lattice-ai` that knows about org.

## 8. What retrieval means here

The assistant is retrieval plus inference plus research, and the retrieval is structured
first: `org.agenda`, `org.query` and `org.notes` find what the question names, and
`lattice.search` covers free text. Personal org files are mostly found by date, state, tag
and title, which a structured query answers exactly and an embedding answers approximately.

**An embedding index is deferred, not rejected.** It costs chunking, an index kept current
off-thread, an embeddings endpoint and invalidation. Whether it earns that is measured by the
eval (§9): if lexical and structured retrieval miss the questions people ask in prose, it is
built then, as a native source over any text (`lattice.recall`), not an org one.

## 9. Models, and the eval that chooses them

**Local works today**: `[llm.providers.local] base_url = "http://localhost:11434/v1"` and any
model Ollama serves. The design adds nothing for that.

**The eval decides what ships.** It is a fixture org corpus (projects, dailies, habits, clocks
and roam notes over three months) and a task set, each with a check that does not need a
model to grade:

- "What's on today?" is graded on the entries named.
- "Mark the review task done" is graded on the proposal against the expected edit.
- "What did I spend time on last week?" is graded on the totals in the answer.
- "Find my notes on X and summarise them" is graded on the notes it read.

Each task runs against a list of provider/model pairs, and the report is a table of pass
rate, tool rounds and time per model. It runs by hand, not in CI, because it needs a model.

Two pieces wait on its numbers, with the gate written down:

- **A bundled local model.** A managed `llama-server` and a downloaded GGUF, installed and
  supervised the way Lighthouse installs language servers, is worth building only if a model
  small enough to run on a laptop passes the eval with this harness. If none does, shipping
  gigabytes buys nothing that pointing at Ollama does not.
- **Constrained decoding** (a JSON Schema for tool arguments) needs a runtime we control.
  It comes with the bundled model, if that is built.

## 10. Alignment

**UX.** No new surface: workflows open ordinary chats, changes open the existing diff
review, and a fetch asks in the existing popup. Nothing streams into a buffer the user did
not choose. Org files change only through review.

**Performance (#1).** Every call is off the editor thread: web I/O on the layer's runtime, a
plugin call on its actor. Nothing here adds to the keystroke path. A model running locally
loads the machine, not the editor; the latency bench under background load already covers
that, and the bundled-model slice adds a run with inference going.

**Extensibility (#2).** The WIT carries the whole seam, so a plugin in any component-model
language can give the model tools. Org is the first consumer, not a special case. Magit,
project and lighthouse are candidates.

**Modal editing (#3).** Untouched.

**Asynchronicity (#4).** A plugin call is one more async call on an actor that already
serves seams. A trap or a fuel cut is that call's error, and the plugin stays isolated.

**Heuristic #6.** No new crate. Web tools and the plugin source sit in
`lattice-ai/src/llm/tools/`, the WIT in `lattice-wit`, the guest adapter in `lattice-plugin-api`,
and the org tools in the org plugin.

## 11. Rejected

- **An org MCP server.** Org's logic lives in WASM. A server would be a second org parser
  outside the plugin or a host bridge plus a transport. It would also break the single
  extension substrate with a native binary beside the plugin, and every read would ask.
  What MCP really offers is that external agents could use the tools too, and that comes
  through the host: lattice already serves a catalogue to Claude Code over `LatticeServer`,
  and exposing plugin sources there later costs a slice, not a server (see the slice plan).
- **Org tools native in `lattice-ai`.** The native layer has no org knowledge by design
  (`llm-layer.md` §1). It would also put a plugin's parser in the host.
- **A plugin tool that writes.** Even with review afterwards, a write the user can only undo
  is the wrong order. Proposals keep "the user has seen it" true for every change.
- **Trusting a plugin's declared kind** without looking at its capabilities. A claim is a
  claim, as MCP's `readOnlyHint` is (`llm-layer.md` §6).
- **An org harness**: an agent loop of org's own, with its own prompts and state. The llm
  layer's loop already is that, and a second one would split stop, inspect, requests and
  review across two systems.
- **Per-heading settings or org properties driving the model.** Left out of the llm layer
  for the reason it gives (§1). Profiles carry settings; files carry content.
- **Tools on by default.** A plain chat that reads the project unasked changes what `:llm`
  means for everyone who has it today. Built-in profiles make the opt-in one word.

## 12. Open questions

- **Search backend set.** Brave and SearXNG prove the shape. Whether a third is needed at
  release (Tavily, Kagi) is settled when someone asks.
- **HTML to text.** The converter is chosen in its slice by output quality on a fixed page
  set, against compile cost.
- **Proposals spanning files.** A `refile` touches two files. It goes as two proposals
  reviewed in order. Whether a two-diff review reads well enough, or wants one combined
  view, is settled when refile is built.
- **The ref format** `path::line::heading` is a first cut. If the eval shows models
  mangling it, refs become short opaque ids handed out per request.
