# lighthouse — install language servers

Lattice talks to language servers for diagnostics, completion, go-to-definition
and the rest. Normally you install each server yourself and put it on `PATH`.
Lighthouse does that part for you:

```
:lsp-install rust-analyzer
```

It downloads the server, checks it, unpacks it into a directory lattice
manages, and tells the editor to use it. No `PATH` entry, no toolchain. The
command returns at once; the work is reported, live, in a buffer named
`*lsp-install:rust-analyzer*`.

## Commands

| | |
|---|---|
| `:lsp-servers` | List every server lighthouse knows, and what state each is in. |
| `:lsp-install <server>` | Install a server. Run it again to reinstall. |
| `:lsp-update <server>` | Move an installed server to the version lighthouse pins. |
| `:lsp-update-all` | Do that for every installed server that is behind. |
| `:lsp-uninstall <server>` | Remove a server and stop using it. |

## The list: `:lsp-servers`

```
  Server         Version     Status
  rust-analyzer  2026-10-05  installed

i install   u update   x uninstall   <CR> show log   gr refresh
```

Put the cursor on a server's row and press:

| | |
|---|---|
| `i` | install it (or reinstall it) |
| `u` | update it |
| `x` | uninstall it |
| `<CR>` | open its install log |
| `gr` | redraw the list |

The list redraws by itself while an install runs — the row goes from
`installing…` to `installed` without your touching it. These keys exist only
in this buffer.

A status tells you what you can do next:

| Status | Meaning |
|---|---|
| `not installed` | `i` installs it. |
| `installing…` | In progress; `<CR>` shows how far along. |
| `installed` | In use for its files. |
| `installed X — update available` | Lighthouse now pins a newer version; `u` moves to it. |
| `installed, but its files are missing — reinstall` | The record is there and the files are not; `i` repairs it. |
| `installed, no longer in the registry` | Still on disk, but lighthouse cannot describe it to the editor any more; `x` removes it. |
| `no build for <platform>` | The registry has no download for this machine. |

## What "installed" changes

Once a server is installed, files of its language opened **from then on** use
it, instead of whatever is on `PATH`. A server that is already running keeps
running until you restart the editor — installing does not restart anything,
and neither does uninstalling.

The install survives restarts. Uninstalling puts the language back on the
`PATH` server, if you have one.

## Updates

Lighthouse pins one version of each server, and a new pin arrives with a new
lattice release (or with your own registry — below). `:lsp-update` installs
the pinned version when it differs from yours.

An update never leaves you without a server. The new version is downloaded and
checked beside the old one, the editor is moved onto it, and only then is the
old one deleted. If the download fails, nothing changes.

## What is checked

Every download is pinned to a SHA-256 digest, and the bytes are checked against
it as they arrive. If they do not match — a corrupted download, a replaced
release, anything — nothing is installed and nothing is left behind, and the
install buffer says so. This is the reason lighthouse installs a *pinned*
version rather than "the latest".

## When an install fails

The install buffer has the reason on a line beginning `error:` — no network,
a digest that did not match, a full disk. Nothing partial is kept, and a
server you already had installed is left exactly as it was. Fix the cause and
run `:lsp-install` again; it starts from a clean page.

## Where servers live

In lighthouse's own data directory:

```
~/.config/lattice/plugins/lighthouse/data/lsp/<server>/<version>/
```

(`$XDG_CONFIG_HOME` is honoured; `%APPDATA%` on Windows.) Server binaries are
tens of megabytes each, so if you sync your config directory you will probably
want to exclude `lattice/plugins/*/data/`. Deleting that `lsp/` directory by
hand is safe: `:lsp-servers` will show what is missing, and `i` reinstalls.

## Adding a server of your own

The list of servers is a file. Lighthouse ships one, and reads a second from
its data directory if you write it:

```
~/.config/lattice/plugins/lighthouse/data/registry.toml
```

```toml
[[server]]
name = "rust-analyzer"          # what you type after :lsp-install
lsp-id = "rust"                 # the editor's id for this language's server
language-id = "rust"            # the LSP languageId
version = "2026-10-05"
args = []
file-patterns = ["*.rs"]
root-markers = ["Cargo.toml", ".git"]

[server.platform.linux-x86_64]  # <os>-<arch>
url = "https://github.com/rust-lang/rust-analyzer/releases/download/2026-10-05/rust-analyzer-x86_64-unknown-linux-gnu.gz"
sha256 = "28070188df63b6f217768040781decc8db43bc9d29b126847acb365575b09bc9"
archive = "gz"                  # or "tar-gz"
binary = "rust-analyzer"        # the executable inside the unpacked tree
```

An entry with a new `name` adds a server. An entry with the name of one
lighthouse ships **replaces** it — that is how you pin a different version.
The `sha256` is required; `sha256sum <file>` prints it.

Two limits:

- **Downloads come only from the hosts lighthouse was built to reach** —
  today, GitHub releases. A URL anywhere else is refused when the download
  starts, and the install buffer says why. The registry is yours to edit;
  where the editor may connect is not.
- **Archives are `.gz` (one compressed file) or `.tar.gz`.** `.zip` is not
  supported yet.

If your file has a mistake, lighthouse ignores the whole file and keeps its
own servers. `:lsp-servers` shows the problem under the list, naming the
server and the field.

## Which servers ship

`rust-analyzer`, for Linux and macOS on x86_64 and aarch64. More arrive over
time; `:lsp-servers` is the list for the version you are running.
