# Plugin manifest — one file, and grants that follow your configuration

Status: design fragment (2026-10-17). Slice plan:
[`../operations/slice-plans/plugin-manifest.md`](../operations/slice-plans/plugin-manifest.md).

Anchors: [`plugin-host.md`](plugin-host.md) (the capability model this
narrows), [`plugin-manager.md`](plugin-manager.md) §4 (source kinds and
staging), [`plugin-distribution.md`](plugin-distribution.md) (the release
package this simplifies), paramount goal #2.

A plugin today is two files: the component, and a `plugin.toml` beside it
that the loader drains seams and grants capabilities from. Two problems follow
from that split:

- **User data lives in a plugin-owned file.** A filesystem grant is a path,
  and a path is the user's — the org plugin's shipped manifest granted
  `fs:write:~/src/dhruvasagar/org-files`, its author's notes directory. To
  point it elsewhere a user edits `plugin.toml`, and for a `Git` / `Local`
  source the build stages `plugin.toml` from the source tree on every
  rebuild, so the edit is silently undone by the next update.
- **Two files that must agree.** `provides` names the seams the component
  implements; nothing checks they match, and a hand install has to copy both.

This fragment makes the component the only file, and makes a filesystem grant
a function of the user's own configuration rather than of the manifest.

## 1. The manifest is inside the component

A component carries its manifest in a wasm **custom section** named
`lattice-manifest`. The payload is the manifest in the same TOML schema
`plugin.toml` uses today, so nothing about its meaning changes — only where it
lives.

- **Written at build time** with `#[link_section]` on a static. Authors never
  write that: `lattice-plugin-sdk` provides `manifest!(<TOML text>)`, which emits
  the section from the same TOML a `plugin.toml` holds (and contains the
  `#[unsafe(link_section)]` attribute, so a crate that denies unsafe code
  still builds). Any language
  whose toolchain can emit a custom section can carry one; the section name
  and payload schema are the contract, documented beside the WIT.
- **Read without instantiating.** The loader walks the component's sections —
  the custom section sits inside the wrapped core module — before compile,
  exactly where it reads `plugin.toml` now. A scan of the bytes already in
  memory; off the UI thread like the rest of loading.
- **Verified, not assumed** (2026-10-17 spike): a `#[link_section]` static in
  a `wasm32-wasip2` cdylib arrives in the component as a genuine
  `lattice-manifest` custom section of the nested core module, intact under
  `strip = "symbols"`, LTO and `opt-level = "s"` — org's release profile.
- **`plugin.toml` keeps working.** A directory with a `plugin.toml` and a
  component without the section loads exactly as today. When both exist the
  embedded manifest wins and the loader logs (debug) that the file is
  ignored — the component is what was built, the file is what was copied.

**A manifest says which editor it needs: `min_lattice`.** Neither embedding
nor option-bound grants (§2) changes the plugin API — no WIT version moves —
yet an editor older than the one that introduced them cannot load such a
plugin: it never looks for the section, and it rejects
`fs:write:{org.directory|~/org}` as an unrecognised capability. The plugin-API
version cannot say so, because what changed is the manifest format, not the
API. So the manifest carries an optional `min_lattice = "0.9.10"`. A loader
older than that refuses the plugin by name — *"org needs lattice ≥ 0.9.10;
this is 0.9.9"* — in `:plugins`, instead of failing on the first field it
does not understand. Absent means "any version that can read the manifest",
which is every plugin written so far. Both features ship in 0.9.10, so a
plugin using either declares at least that.

**Why not a WIT export (`manifest() -> plugin-manifest`).** Calling an export
needs an instance, and an instance needs its capability grant — which is what
the manifest decides. Instantiating twice (once ungranted to ask, once to run)
doubles a plugin's cold start for a value that is constant per build.

## 2. A grant can name an option

A filesystem capability may name one of the plugin's own options instead of a
path, with the default the plugin ships:

	capabilities = [
		"fs:write:{org.directory|~/org}",
		"fs:write:{org.roam-directory|~/org/roam}",
		"state:write",
	]

The grant is the option's value. A user who sets `org.directory` — which they
must do anyway, for the plugin to know where their notes are — has set the
grant with it, in their own config, and never edits a plugin file.

**Resolution.** When the loader grants capabilities:

1. the option's value if the user has set one (`lattice.toml`, `init.rs`,
   `:set`) — including a value set before the plugin registered the option,
   which the registry holds until it does (PMF.15/16);
2. otherwise the template's default (`~/org`), so the plugin starts with
   access to where it would look by default;
3. `~` expands; an explicitly empty value grants nothing.

**The default must agree with the plugin's.** The template carries the default
because grants are fixed at instantiation, before the plugin's
`register-options` export has declared its defaults. After registration the
loader compares the two and warns (in `:plugins` and `*messages*`) if they
differ — a plugin whose grant and option disagree about where it writes is a
bug the user would otherwise meet as a refusal.

**When the value changes the plugin reloads.** A grant is fixed for the life
of an instance — it becomes WASI pre-opened directories and host-side checks —
so a grant-bearing option changing (`:set`, a config reload, or `init.rs`
setting it while the plugin loads) re-resolves the template and, if
the path differs, reloads the plugin through the existing reload path. Reload
is per plugin and off the keystroke path; a plugin's stored state survives it.

**A plugin cannot widen its own grant.** `set-option` writes the same layer
`:set` does, so without a rule a plugin could set `org.directory` to `/` and
reload into a grant on the whole disk. The host refuses `set-option` /
`set-option-value` on an option named in any plugin's grant template, from any
plugin except the user's `init.rs` (the config authority, loaded at the boot
tier). Refused by name, with the reason, at the boundary.

**What may be templated.** `fs:read` and `fs:write` only — the kinds that take
a path. `{name}` must be an option the same plugin declares; a template naming
another plugin's option, or one the plugin never registers, is refused at load
with that reason. Other capabilities (`state:write`, `net:http:<host>`,
`proc:spawn`) are unchanged.

## 3. What the user sees

- **`:plugins`** shows each grant expanded, with where it came from:
  `fs:write ~/src/dhruvasagar/org-files (org.directory)` or
  `fs:write ~/org (org.directory, default)`.
- **A refusal names the option.** An archive or refile outside the grant
  fails with *"org: ~/elsewhere/x.org is outside org.directory
  (~/notes) — set org.directory to change where org may write"*, rather than
  a generic capability error.
- **Plugin help documents the pattern** (`:help plugins`, "Capabilities and
  your configuration"), and each plugin's page names the options that carry
  its grants. A user who has never heard of capabilities still meets the
  right fact: *setting this option also decides what the plugin may touch.*

## 4. Effect on distribution

[`plugin-distribution.md`](plugin-distribution.md) planned a package of
`plugin.toml` + component and a `grants` field on `require-release`. With the
manifest embedded and grants following options:

- the release asset is the component alone (`org-0.1.1-api0.3.wasm`) and its
  `.sha256`;
- `require-release` needs no `grants` field — the user's config already holds
  them;
- PRL.6 (user grants) is replaced by §2 here.

A hand install becomes: download one file into the plugins directory, set
`org.directory`.

## 5. Paramount goals

- **#2 extensibility** — one-file plugins in any language that can emit a
  custom section, and no plugin-owned file a user must edit.
- **#1 performance** — the section read is a scan of bytes already loaded;
  a reload happens only when a grant-bearing option actually changes.
- **Security** — narrower than today: the paths come from the user, and the
  plugin cannot change them.

## 6. Rejected alternatives

- **A user grants file** (`plugin-grants.toml`) and an `init.rs`
  `set-grants` call. Both work, and both make the user state a path twice —
  once for the plugin to look in, once to allow it. Kept in reserve for a
  plugin that needs a path no option of its own describes; none does today.
- **A consent prompt for every path.** With defaults and option-bound grants
  there is nothing to ask at first run; the refusal message (§3) covers the
  case where a user's notes are elsewhere.
- **Keeping user paths in `plugin.toml`** with a merge rule for updates.
  Fixes the overwrite but keeps user data in a plugin-owned file.
