---
summary: "llm: configure the LLM layer. Providers, profiles, the llm.* options, project config and what a project may not set, keys, switching profile, and how a change reaches a request."
related: [llm, ai, model, profile, provider, settings, config, llm-chat-mode, setlocal]
---

# llm

The LLM layer calls a model directly over its API. [`:llm`](help:llm-chat-mode)
opens a chat; this page is how it is configured. A request's settings come from
layers, and you can change any of them from where you are.

## The layers

Lowest to highest, a higher layer wins, key by key:

| Layer | Where it comes from | Shown by `:set llm.model?` as |
|---|---|---|
| defaults | the provider's own `model`, and nothing else | `default` |
| global | `lattice.toml` `[llm]`, or `:set llm.model=...` | `global` |
| profile | `[llm.profiles.<name>]` in `lattice.toml` | `profile: writer` |
| project | `.lattice/llm.toml` in the project root | `project` |
| session | `:setlocal llm.model=...` in a buffer | `buffer-local` |
| request | arguments on one command | (one request only) |

A **session is a buffer.** There is no session to create or name: a chat is its own
buffer, and `:setlocal` in any other buffer sets that buffer's session. A new chat
starts with a **copy** of the session of the buffer it was opened from, and then
stands alone: changing one afterwards never changes the other.

## A complete config

`~/.config/lattice/lattice.toml`:

```toml
[llm]
# Which provider a bare model name runs on. Optional with one provider.
default_provider = "anthropic"
# The profile new sessions start from. Optional.
default_profile  = "general"
# provider/model, or a bare model name on the default provider.
model            = "claude-sonnet-4-5"
temperature      = 0.7
max_tokens       = 4096

[llm.fold]
reasoning = "closed"     # closed | open | hidden
tools     = "closed"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.openai]
kind        = "openai-compatible"
base_url    = "https://api.openai.com/v1"
api_key_command = "pass show openai/api-key"
input_cost_per_mtok  = 2.5      # both prices, or no cost is shown
output_cost_per_mtok = 10.0

[llm.providers.local]
kind     = "openai-compatible"
base_url = "http://localhost:11434/v1"
model    = "qwen2.5-coder"      # this provider's own default model

[llm.profiles.general]
system = "You are a careful assistant. Answer briefly."

[llm.profiles.writer]
model       = "local/qwen2.5-coder"
system      = "~/.config/lattice/llm/editor.md"   # a file, read when a request starts
temperature = 0.2
destination = "below"
tools       = []

[llm.profiles.review]
model     = "anthropic/claude-sonnet-4-5"
reasoning = "high"
messages  = "last"
```

`.lattice/llm.toml` in a project (see [what a project may set](#what-a-project-may-not-set)):

```toml
model       = "anthropic/claude-sonnet-4-5"
system      = "This is a Rust workspace. Prefer iterators to index loops."
temperature = 0.2
```

Editing providers or profiles applies the next time the editor starts. The scalar
settings are ordinary options, so `:set llm.temperature=0.3` applies at once.

## The options

Every session setting is an option: `:set llm.model?` shows the value and the layer
it came from, `:setlocal llm.model=local/qwen` changes it for this buffer only, and
`:setlocal llm.model&` goes back to what the layers below say. An **empty value means
"not set at this layer"**, so an unset global never hides a profile.

| Option | Values | Notes |
|---|---|---|
| `llm.model` | `provider/model` or a bare model | A bare name runs on the default provider; a name with a slash that is not a provider (`meta/llama-3`) is still a bare model |
| `llm.system` | text | In your own config it may be a path (`~/...`, `/...`, `./...`) whose file is read when a request starts |
| `llm.temperature` | 0 to 2 | |
| `llm.max_tokens` | a number, 0 for the provider default | |
| `llm.reasoning` | `off` `low` `medium` `high` | Not sent by any adapter yet; see below |
| `llm.messages` | `all`, `last`, or a number | How much history a request sends |
| `llm.tools` | `none` or names | Tools arrive in a later slice |
| `llm.destination` | `chat` `below` `review` `register` `echo` | |
| `llm.fold.reasoning`, `llm.fold.tools` | `closed` `open` `hidden` | Apply to open chats **live** |
| `llm.profile` | a profile name | See [switching profile](#switching-profile) |
| `llm.default_profile`, `llm.default_provider` | a name | Global only |

## A change applies to the next request

A request reads its settings **once, when it starts**. Change the model while a
reply is streaming and that reply finishes as it began; the next prompt uses the new
model. The headerline shows the model the next request would use.

The fold options are the exception that proves it: they change how the transcript is
*shown*, not what is sent, so an open chat re-projects the moment you set one. Text
and folds are always taken from one projection, so they cannot disagree.

## Switching profile

```vim
:llm writer            " a new chat that starts from the writer profile
:llm-profile writer    " switch this session to the writer profile
```

A profile replaces the base your overrides sit on, so an override you made on the old
base no longer means what it did. `:llm-profile` **clears the session's overrides and
says which**:

```
llm: profile writer; cleared llm.model, llm.temperature
```

`:setlocal llm.profile=writer` changes the base too but cannot clear its siblings, so
`:llm-profile` is the way to switch. An unknown name is an error that lists the known
ones. `:llm writer` from a buffer whose session already uses another profile starts
the chat fresh from `writer`, without copying overrides made on the other.

## Keys

A key never goes in a config file. Give a provider one of:

- `api_key_env = "NAME"`: read from that environment variable.
- `api_key_command = "pass show ..."`: run through `sh -c`, first line is the key.
  It runs in the background, with 30 seconds.

A literal `api_key = "sk-..."` is **refused**, with a message, and the provider
otherwise loads. If the variable is unset the chat says which one. If the command
fails the chat gives its exit status, never its output.

## What a project may not set

A project's `.lattice/llm.toml` can arrive in a cloned repository, so it is not you.
It may set the model, an inline system prompt, parameters, and tools **narrower** than
what you already enabled. It may not set:

| Refused | Why |
|---|---|
| `base_url`, `[providers.*]` | would send your key to the project's server |
| `api_key_env`, `api_key_command`, `api_key` | a key command is a command to run |
| `tools` wider than yours | a project cannot switch a tool on (with none enabled, none) |
| `trust` | a project cannot widen its own trust |
| `[mcp.*]` | a stdio MCP server is a program to run (servers arrive later) |
| `[profiles.*]` | a profile can widen tools |
| `destination` | not one of the things a project may set |
| `system` naming a file | would make the editor read a file the project chose |

Each refused key is reported in `:messages` naming the key and the file, **once per
chat**, and **the rest of the file still applies**:

```
llm: refused `base_url` from .lattice/llm.toml: a project may not redirect requests,
which would send your key to its server (the rest of it still applies)
```

LLM settings in `.lattice/config.toml` are not read at all (a warning says so): that
file is applied before any check, so these live in `.lattice/llm.toml` where the
table is enforced. The same table applies to a profile named in a prompt (it may set
the model, system prompt and parameters, never wider tools).

## Parameters a model does not take

A parameter the provider or model will reject is **omitted, and the request says
so** (a line in `:messages`), instead of failing with a 400. This is a short list of
known cases, not a capability table: OpenAI's reasoning models (`o1`, `o3`, `o4`,
`gpt-5...`) take only the default `temperature`, and `llm.reasoning` is not sent by
any adapter yet. A model the list does not know gets the parameter, and if it
objects, its own error reaches the chat.

## See also

- [`llm-chat-mode`](help:llm-chat-mode): the chat buffer.
- [`ai-conversation-mode`](help:ai-conversation-mode): the agent conversation.
