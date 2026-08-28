# The agenda scans what you configure, not what you happen to be in — slice plan (AF)

> Design: [`../../architecture/org-mode.md`](../../architecture/org-mode.md) §6
> (the agenda seam and its world), which AF.1 extends.
>
> Plugin repo: [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** ✅ complete (2026-08-28). AF.1–AF.4 and AG.1 all landed.

---

## Why

Today the agenda scans **one root**, and the root is either an explicit
`:agenda <path>` argument or the project root derived from the working directory
(`providers/agenda.rs`, `AgendaOptions { root: PathBuf, max_files }`).

That means the agenda answers a different question depending on which checkout
you happen to be in. Org's agenda is not a property of the current project — it
is the set of files you keep your life in, and it is the same set from anywhere.
Emacs states this directly:

```elisp
(setq org-directory "~/src/dhruvasagar/org-files")
(setq org-agenda-files (list org-directory))
;; …and a custom command narrowing to one file:
(org-agenda-files '("~/src/dhruvasagar/org-files/anniversaries.org"))
```

Two shapes in one list: a **directory** (walked) and a **file** (taken as-is).
Both are ordinary org usage and the configuration has to carry both.

There is no option today at all — not a missing default, a missing setting.

## The ownership question, decided

Presented as a choice because it changes which repo the work lands in, and
decided against the recommendation, which is recorded here so the cost is not
rediscovered as a surprise.

**Chosen: the option is the plugin's — `org.agenda-files`.**

> **UX (higher court):** the name is the one the user's fingers already know
> from `org-agenda-files`, and org-mode users are the entire population of this
> feature today. Muscle memory across editors is the dominant cost on a
> user-facing surface ("UX follows convention").
>
> **Paramount goals:** protects #2 (extensibility) in the sense that a source
> owns its own configuration surface; **sacrifices** #2 in another sense, which
> must be said plainly — the `agenda-source` seam deliberately gives a guest no
> say in which files are offered, and this adds one.
>
> **Heuristic #1 (long-term fit):** the honest cost is a new WIT export plus a
> host merge step, bought substantially for a name. The recommendation was the
> host-side `agenda.files`, because the walk, the file offering and the
> multibuffer are all already host-side. Dhruva chose plugin ownership; that is
> the decision and the rest of this plan implements it fully.
>
> **Heuristic #3 (third option):** an alias (`agenda.files` real,
> `org.agenda-files` writing through) was offered and not taken — two names for
> one setting means `:describe-option` shows both and `:set` on either has to
> keep them coherent.

**What keeps it from being org-specific machinery:** the host never learns the
option's name. It asks every registered source "what should I walk?" and merges
the answers. A markdown TODO source can answer the same question with its own
option, and the host code does not change.

## The option's value shape

Options are `boolean | integer | string` — there is no list type
(`config`'s registered kinds). So the value is a string, **one path per line**,
with blank lines and `#` comments ignored:

```toml
[org]
agenda-files = """
~/src/dhruvasagar/org-files
~/work/notes/standup.org
"""
```

Newline-separated rather than `:`- or `,`-separated because a path may contain
either, and rather than TOML-inside-a-string (`capture-templates`' shape)
because a list of paths is not a record and does not need one. `~` is expanded
host-side, where `shellexpand_tilde` already lives.

Same cost `capture-templates` records: `:describe-option` shows a blob and
`:set org.agenda-files=…` cannot meaningfully edit a multi-line value. If typed
options ever grow a list kind, this declaration migrates and the meaning does
not change.

## Slices

| Slice | Description | Status |
|---|---|---|
| AF.1 | `agenda-source.roots()` — a source names what to walk | ✅ |
| AF.2 | host: `AgendaOptions.roots`, files-or-directories, precedence | ✅ |
| AF.3 | plugin: `org.agenda-files` + `roots()`; the config registry the agenda store lacked | ✅ |
| AF.4 | docs — `doc/org.md`, `agenda-view-mode`, ledger, site | ✅ |

### AF.1 — `agenda-source.roots()` ✅

One export on `agenda-source-plugin`:

```wit
/// Paths this source wants scanned. Each entry is a FILE or a DIRECTORY;
/// the host walks directories and takes files as given. `~` is expanded
/// host-side.
///
/// Empty means "no opinion" — the host then falls back to the root it
/// would have used, so a source that does not implement this behaves
/// exactly as it did before.
///
/// Called per scan, not once at load: the answer comes from user
/// configuration and must follow a `:set` without a reload.
export roots: func() -> list<string>;
```

Called per scan and not once at load, deliberately: `extensions` is a fact about
the source and is cached; this is a fact about the *user's config* and has to
track `:set`.

Every path still passes the host's existing `fs` grant check — a source naming a
directory does not acquire the right to read it.

### AF.2 — the host walks a list ✅

`AgendaOptions.root: PathBuf` → `roots: Vec<PathBuf>`. Resolution order, most
specific first:

1. an explicit `:org-agenda <path>` argument — one root, unchanged;
2. the roots the OPEN view already shows, so `gr` refreshes what is on screen
   rather than resetting (the PD.9 rule the current code already follows);
3. the union of every registered source's `roots()`;
4. the project root from the working directory — today's behaviour, and still
   the answer for an editor with no agenda configuration.

Each entry is stat'd once: a file is offered directly, a directory is walked as
today. `max_files` caps the union, not each root, and a truncated walk must
`log()` what it dropped rather than silently returning a short agenda.

A configured path that does not exist is skipped with an `info!` — user
actionable, and one bad entry must not fail the agenda (`error-parser`'s rule,
which this provider already follows per file).

### AF.3 — `org.agenda-files` ✅

The plugin registers the option and implements `roots()` by splitting its value.
Unset ⇒ empty list ⇒ the host's fallback, so an org user who has configured
nothing keeps exactly today's behaviour.

### AF.4 — docs ✅

`org-mode.md` §6.2's WIT block gains `roots`; `agenda-view-mode` stops naming a
host command that no longer exists; `doc/org.md` documents the option
with Dhruva's own config as the worked example; ledger entry; site sync.

## Tests

- A directory entry and a file entry in one list both contribute rows.
- A file entry OUTSIDE any configured directory contributes rows (the
  anniversaries case).
- `:org-agenda <path>` still overrides the option — the argument is the escape
  hatch, and losing it would make the option a cage.
- `gr` on an open view re-scans that view's roots, not the option's, when they
  differ.
- No option set ⇒ byte-identical behaviour to today (the regression guard that
  matters most, since every existing agenda test depends on it).
- A configured path that does not exist is skipped and the rest still scan.

---

## AG.1 — org owns its trigger ✅ (2026-08-28)

Not in the original plan; it arrived mid-build as three related objections —
org specifics should not leak into lattice, the command should be
`:org-agenda`, and every command the plugin ships should carry the `org-`
prefix. The audit found 37 of 37 plugin commands already prefixed; the single
exception was `<leader>oa` binding to the HOST's `agenda`, so all three
objections were one problem.

`OpenProviderView` was withheld from the WIT app-effect mirror as a capability
question — which providers may a plugin trigger? The precedent had answered it
already: `Effect::OpenPicker` and `Effect::OpenTransient` cross ungated and open
any registered source by name. So the effect crosses, `lattice-multibuffer`
stops registering an ex-command, and org registers `:org-agenda` itself with its
own `parse`/`apply` callbacks — this plugin's first ex-command.

The name is the plugin's to choose rather than derived by the host from the
plugin id. The host-derived form would make the convention unbreakable; it was
rejected because a source's users have a name for this in their own ecosystem
and only the source knows it. The convention is asserted as a property over the
command registry instead, so a new command cannot quietly break it.

`:agenda` is gone, not aliased.

**What did NOT move, and why.** Whether the whole provider belongs in the plugin
was raised and checked rather than assumed: all 42 occurrences of "org" in
`providers/agenda.rs` are `.org` sample paths in its own tests, and with no org
plugin installed there is now no command, no source and no rows. What the
provider does — create a multibuffer view, `spawn_document` per source file, add
excerpts, activate modes, drive batched off-thread reads — is buffer, document
and thread work with no WIT surface, and giving it one would put buffer
construction and off-thread I/O in the guest, which is what paramount #1 and #4
exist to prevent. Policy is the plugin's; mechanism is the host's.
