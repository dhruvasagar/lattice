---
summary: "llm: configure the LLM layer. Layers, the llm.* options, profiles, project config and what a project may not set, keys."
related: [llm, ai, model, profile, provider, settings, config, llm-chat-mode, setlocal]
---

# llm

[`:llm`](help:llm-chat-mode) opens a chat on a model called directly over its API;
this page is how it is configured.

## The layers

A higher layer wins, key by key. `:set llm.model?` shows which one won.

| Layer | Source |
|---|---|
| default | the provider's own `model` |
| global | `lattice.toml` `[llm]`, or `:set llm.model=...` |
| profile | `[llm.profiles.<name>]` in `lattice.toml` |
| project | `.lattice/llm.toml` |
| session | `:setlocal llm.model=...` |
| request | arguments on one command |

A **session is a buffer.** A new chat starts with a copy of the session of the buffer
it was opened from, then stands alone.

## Config

`~/.config/lattice/lattice.toml`:

```toml
[llm]
default_provider = "anthropic"
default_profile  = "general"
model            = "claude-sonnet-4-5"

[llm.providers.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[llm.providers.local]
kind     = "openai-compatible"
base_url = "http://localhost:11434/v1"
model    = "qwen2.5-coder"

[llm.profiles.writer]
model  = "local/qwen2.5-coder"
system = "~/.config/lattice/llm/editor.md"   # a file, read when a request starts
tools  = []
```

Providers and profiles load at startup. Scalar settings are options and apply at once.

## Options

`:set llm.model?` shows the value and its layer, `:setlocal llm.model=x` changes it
for this buffer, `:setlocal llm.model&` goes back. An **empty value means "not set at
this layer"**.

`llm.model` (`provider/model` or a bare model), `llm.system` (text; a path in your own
config), `llm.temperature`, `llm.max_tokens`, `llm.reasoning`, `llm.messages` (`all`,
`last`, a number), `llm.tools`, `llm.destination`, `llm.fold.reasoning` and
`llm.fold.tools` (`closed` `open` `hidden`, applied to open chats live), `llm.profile`,
and the global-only `llm.default_profile`, `llm.default_provider`.

A request reads its settings **once, when it starts**: change the model mid-reply and
that reply finishes as it began.

## Switching profile

`:llm writer` opens a chat from the `writer` profile. `:llm-profile writer` switches
this session, **clearing its overrides and saying which**. An unknown name lists the
known ones.

## Overriding on the go

`:llm-menu` (`<M-m>` in a chat) lists each setting with **where its value came from**
and names the session the next request would use. `=` switches changes between the
**session** and the **next request only**, and stays switched; `x` clears. For one
request, put `key=value` words first (`model=local/qwen temperature=0 ...`).
`@writer` in the text you type, with a known profile name, applies that profile to that
request. If it would enable tools you do not have, you are asked first. `@unknown`
is just text. `:llm-system` edits the system prompt ([`llm-system-mode`](help:llm-system-mode)).

## Keys

A key never goes in a config file. Use `api_key_env = "NAME"` or
`api_key_command = "pass show ..."` (run through `sh -c`, first line is the key, in
the background). A literal `api_key` is refused. A failing command reports its exit
status, never its output.

## What a project may not set

`.lattice/llm.toml` can arrive in a cloned repository. It may set the model, an
inline system prompt, parameters, and tools **narrower** than yours. It may not set
`base_url`, providers, key sources, wider `tools`, `trust`, `[mcp.*]`, profiles,
`destination`, or a `system` naming a file. Each refused key is reported in
`:messages` once per chat, and the rest of the file still applies.

LLM keys in `.lattice/config.toml` are not read, in any spelling; a warning says so.
A profile named in a prompt follows the same table.

## See also

[`llm-chat-mode`](help:llm-chat-mode), [`ai-conversation-mode`](help:ai-conversation-mode).
