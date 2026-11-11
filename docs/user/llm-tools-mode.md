---
summary: "llm-tools-mode: tools for llm chats beyond lattice's own, from MCP servers ([llm.mcp.<name>], a stdio command or an HTTP url) and from plugins. Select them with llm.tools = [\"name.*\"]; :llm-tools lists each source, its state and its tools; r restarts a server, dd stops it."
related: [llm, mcp, tools, server, plugin, llm-chat-mode, llm-requests]
---

# llm-tools-mode

A chat's tools come from sources. Lattice ships two, `lattice.*` (files,
search, edits, commands) and `web.*`. This page is about the others:

- **MCP servers** you configure. An [MCP](https://modelcontextprotocol.io)
  server gives a model tools of its own: GitHub issues, a database, a
  documentation search.
- **Plugins** that offer tools, such as org's agenda and planning tools.

`:llm-tools` lists both, each with its state and the tools it offers.

## MCP servers

### Configure a server

Each server is a table in **your own** `lattice.toml`. A project's
`.lattice/llm.toml` cannot declare one: a stdio server is a command to run.

```toml
[llm.mcp.github]                       # its tools are github.<tool>
command = "github-mcp-server"          # stdio: lattice starts it
args = ["stdio"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "$GITHUB_TOKEN" }
allow = ["get_issue", "search_issues"] # these run without asking

[llm.mcp.docs]
url = "https://example.com/mcp"        # streamable HTTP
headers = { Authorization = "Bearer $DOCS_TOKEN" }
```

| Key | Meaning |
|---|---|
| `command`, `args` | A server lattice starts and talks to on stdin/stdout |
| `env` | Added to its environment |
| `cwd` | Where it runs (lattice's directory by default) |
| `url` | A server reached over MCP's streamable HTTP |
| `headers` | Sent with every request to it |
| `allow` | Tools, by their name on this server, that run without asking |
| `timeout` | Seconds one tool call may take, 1 to 3600 (300 by default) |

Give `command` or `url`, not both. A name is letters, digits and `-`, and
cannot be `lattice`.

A call that runs past `timeout` is cancelled on the server and fails, and
the model is told it took too long; the server stays up for the next call.

**Keep tokens out of the file.** A value in `env` or `headers` may name a
variable, `$NAME` or `${NAME}` (`$$` is a literal `$`); it is read when the
server starts, and a variable that is not set stops the server with a message
naming it. Neither the values nor the variables are ever shown in
`*llm-tools*`, and anything the server prints that contains one (or the
token inside a `Bearer …` value) is shown as `***`. A token passed as an
argument is hidden the same way: the value after a flag naming one
(`--token X`, `--api-key=X`), and any argument or URL path segment shaped
like a token. Values under six characters are settings, not tokens, and
are left alone.

### Use its tools

A server's tools are offered only to a request whose `llm.tools` selects
them:

```toml
[llm.profiles.coding]
tools = ["lattice.read", "lattice.edit", "github.*"]
```

`github.*` selects all of them, `github.get_issue` one. The kind groups
(`read`, `edit`, `exec`) apply to lattice's own tools only: a server's word
for what its tool does is not trusted, so `github.read` selects nothing
unless the server has a tool called `read`. `:setlocal llm.tools=…` selects
for one chat.

**Every server tool asks before it runs**: allow once, allow for this
chat (every later call of that tool in the chat), or deny. A tool named
in `allow` runs without asking. A server that marks a tool read-only
(`readOnlyHint`) gets that said in the question and in `*llm-tools*`, and the
tool asks all the same. A server tool's edits happen in its own process, so
they cannot go through lattice's diff review: allowing it is the only
review there is.

A tool whose name model providers do not accept (anything but letters,
digits, `_` and `-`, or too long once `server__` is in front of it) is
listed in `*llm-tools*` as not offered.

### When servers run

A server starts when a request first selects one of its tools. The request
waits for it at most 10 seconds; a server that fails or is slower goes
without, and the request carries on with its other tools. A server is never
started for a request that does not select it.

A server that **failed to start** is not tried again until you restart it
(below): a wrong command fails the same way each time. A server that was
running and then **exited** is started again by the next request that needs
it. A tool call in progress when its server dies comes back to the model as
that call's error.

Stopping a request (`:llm-stop`, `<C-c>`) cancels its tool call on the
server, which stays up for the next one. Stopping a server kills its whole
process tree, so a server started through `npx` or a shell leaves nothing
behind.

Changes to `[llm.mcp]` apply on the next start of lattice.

## Plugin tools

A plugin that offers tools needs no configuration: they are there once it
loads, named by the plugin, and selected like any source's, `org.*` or
`org.agenda`.

**Whether a plugin's tool asks depends on what the plugin can do, not on what
it says.** A plugin that can reach neither the network nor another program
can only answer from what it reads, so its read tools run without asking, as
`lattice.read` does. One that can reach out could send a tool's input
anywhere, so every one of its tools asks first.

**A plugin's tool cannot change a file itself.** It returns the edits it
wants, and each goes through the same review as the model's own edits
(`lattice.edit_file`): fenced to the project, refused if the file changed
since, and shown as a diff unless you trusted the chat (`<C-t>`). An
`llm.deny` rule on `edit_file` refuses a plugin's edit too.

A plugin named `lattice`, `web` or like one of your MCP servers, or offering
tools under a name another plugin already uses, is not offered, and
`*llm-tools*` says why. Its other features still work.

## `:llm-tools`

`:llm-tools` opens `*llm-tools*` (`:llm-mcp` still works, the old name). It
has a section for each kind of source:

- **MCP servers**: each server, how it is reached, its state (`not
  started`, `starting`, `running`, `failed: …`, `exited: …`, `stopped`) and
  its tools, each marked `allowed` or `asks`.
- **Plugins**: each plugin offering tools, and each tool marked `runs`,
  `reviewed`, `asks` or `asks, then reviewed`; or why the plugin is not
  offered.

The list keeps itself current.

| Chord | Action |
|---|---|
| `r` | Start the MCP server on this line again (on a tool's line, its server) |
| `dd` | Stop the MCP server on this line; requests leave it alone until `r` |

## See also

[`llm`](help:llm), [`llm-chat-mode`](help:llm-chat-mode).
