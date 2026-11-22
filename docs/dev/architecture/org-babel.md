# Org babel — running source blocks, tangling, and blocks that talk to each other

> **Where the code is.** The babel *feature* lives in
> [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), a **separate repository**,
> as one mode (`org-babel-mode`). What lives in *this* tree is the generic
> host changes it forces (§4): option-bound **process** grants, option-bound
> grants that read a **list**, an events-seam path for **effects**, a whole-file
> `write-to-file` anchor (§7) and, last, **stdin** on jobs (§6.3). Org is their
> first consumer, not their owner — any user-installed plugin can use them.

Sequencing and status: [`slice-plans/org-babel.md`](../operations/slice-plans/org-babel.md). User documentation: [`org.md`](../../user/org.md)
and the plugin's own `doc/org.md`.

This reverses a cut. [`org-mode.md`](org-mode.md) §8 lists "babel / source
execution" as *out*, and §5.4 lists "source blocks (no babel)" among the
`C-c C-c` contexts with no arm. The cut was made because a user-installed
plugin could not run a program at all (`proc:spawn` is bundled-only); §4 is
the answer to that, and it is the reason the feature is now buildable.

## 0. Ownership — the host lends mechanisms, `org-babel-mode` owns babel

**Everything that is babel lives in `lattice-org-plugin`, inside one minor
mode, `org-babel-mode`.** The host's share is exactly the five generic
mechanisms in §4, §6.3 and §7, and each must be justifiable without the word
"org" (a REPL plugin, a test runner or a dotfiles manager would use the same
one). Nothing else crosses into lattice.

What the mode owns, all declared by the plugin:

- **Its own keymap layer.** Every babel chord — `<C-c><C-c>` in a src block
  and the whole `<C-c><C-v>` prefix — is bound at
  `KeymapLayer::MinorMode(org-babel-mode)`, never at `Builtin` and never in
  `org-mode`'s major layer. The layer sits above org's major, so babel's
  `C-c C-c` sees the key first and returns `Effect::Declined` outside a src
  block, falling through to org's dispatcher (§5.4).
- **Its activation.** An `ActivationPolicy` naming org's major mode, and an
  entry in the manifest's `default_modes` (an unlisted minor registers and
  never activates). Toggled by `org.babel.enabled`; turning it off removes
  the layer, the commands' effect and the decorations together. The switch
  is the host's, declared in the manifest's `mode_gates` (OB.3b,
  [`plugin-manager.md`](plugin-manager.md)): the plugin cannot disable its own
  default mode without racing the loader's `org.enabled` gate.
- **Every action handler body** — run, execute-buffer/-subtree, tangle,
  remove-result, cancel, the confirm's yes-action — registered as plugin
  actions bound to the mode's own `ActionId`s, and every `:org-babel-*`
  ex-command.
- **Every option** under `org.babel.*` (`enabled`, `confirm`, `languages`,
  `tangle-directories`, `min-lines-for-block-output`, `max-output-lines`).
- **Its events-seam handlers** (`job-output`, `job-finished`), its
  decorations (the running sign), its `*org-babel*` output buffer, its
  store keys, and its `:help org-babel` page.
- **Its code**, in its own modules (`src/babel/…`), not grown into
  `lib.rs`; its tests in the plugin repo; its user documentation in the
  plugin's `doc/`.

**The acid test**, the same one org itself passed: babel lands with **zero
`Editor::` methods, zero host `Action` variants, no `BufferKind`, no `Lang`
arm and no babel-specific code in lattice.** A host slice that would need an
org- or babel-shaped name is the wrong host slice.

## 1. The files have another reader

As with habits, these are the user's own org files and emacs opens them too.
So the source block, its header arguments and its `#+RESULTS:` block are
**byte-compatible with org-babel**, or the feature is a regression:

```org
#+NAME: disk
#+begin_src sh :results output :dir ~/src
	df -h .
#+end_src

#+RESULTS: disk
: Filesystem  Size  Used Avail Use% Mounted on
: /dev/disk1  460G  210G  250G  46% /
```

Same `#+NAME:`, same header-argument grammar (`:results`, `:dir`, `:var`,
`:tangle`, `:noweb`, `:session`), same `#+RESULTS:` shapes (`: ` lines, an
org table, an `#+begin_example` block for long output), same property
inheritance (`#+PROPERTY: header-args:sh :dir ~/src`, a headline's
`:header-args:` drawer). A block run here must produce a result emacs leaves
alone, and a result emacs wrote must be one lattice replaces cleanly.

**Not byte-compatible, deliberately:** nothing. Where lattice cannot do what
babel does (§7), the header argument is refused with a message, not
reinterpreted.

## 2. What the user does

| chord / command | does |
|---|---|
| `C-c C-c` in a src block | run it, replace its `#+RESULTS:` |
| `C-c C-v b` · `:org-babel-execute-buffer` | run every block in the buffer, in order |
| `C-c C-v s` · `:org-babel-execute-subtree` | run every block under the current headline |
| `C-c C-v t` · `:org-babel-tangle` | write every `:tangle` block to its file |
| `C-c C-v k` · `:org-babel-remove-result` | delete the block's `#+RESULTS:` |
| `:org-babel-cancel` | kill the running block(s) in this buffer |

The `C-c C-v` prefix and its letters are emacs's (`org-babel-key-prefix`).
Ex-commands are dashed and namespaced, per the naming rule.

**Running is asynchronous and visible.** `C-c C-c` returns at once; the
block's `#+begin_src` line gets a **gutter sign** (`org.babel-running`, through
org's existing `signs` and `decorations` seams) so the user sees *which* block
is working, and the output lands in `#+RESULTS:` when the job finishes — without
a keypress, because job events ride the plugin's event actor (lighthouse
§3.0). A sign and not end-of-line virtual text, which this fragment first
said: the decorations seam carries gutter signs only, and a virtual-text seam
would be a sixth host mechanism (§0) for a status marker. The gutter column
is already reserved, so the sign appearing shifts nothing. The pending marker is deliberately **not a buffer edit**: an edit would
be an undo step that leaves `: running…` behind when undone, would dirty the
file, and would be saved into what emacs reads if the user saved mid-run. The
result is one edit and one undo step. A block that is already running is not
started twice; `C-c C-c` on it echoes that it is running and how to cancel.

**Nothing runs without consent, twice over.** A language runs only if the
user configured it (§3), and each run asks `Run sh block "disk"? (y/n)` —
emacs's `org-confirm-babel-evaluate`, default on, an `org.babel.confirm`
option to turn it off. The prompt is the host's `effect.confirm`; `y`
dispatches a registered run action carrying the block's **key** (§5.3), not
its position — the user may edit while the prompt is open. The key travels
packed in ONE argument slot, because confirm args are truncated to the
action's declared arity. `execute-buffer` / `-subtree` ask once for the run
(`Run 4 blocks (sh ×3, python)? (y/n)`), not once per block. With
`org.babel.confirm = false` the prompt is skipped entirely and the run action
is invoked directly.

## 3. Configuration — the languages table

Emacs needs two things before a block runs: the language loaded
(`org-babel-load-languages`) and an interpreter on the path. Lattice
collapses them into one table, because here the table is also the
permission (§4):

```toml
[org.babel]
confirm = true
tangle-directories = ["~/dotfiles", "~/.config"]

[[org.babel.languages]]
name    = "sh"            # the word after #+begin_src
command = "sh"            # program, on PATH or by path
args    = ["{file}"]      # {file} = the block body, written to a script
ext     = "sh"

[[org.babel.languages]]
name    = "python"
command = "python3"
args    = ["{file}"]
ext     = "py"
value   = "python"        # see §5.2 — a built-in value wrapper
```

The default is **empty**: a fresh install runs nothing. `C-c C-c` on a block
in an unconfigured language answers
`org: no babel language "python" — add it to org.babel.languages`, never
silence. A language named but whose `command` is missing from `PATH` fails as
the job's outcome, with the spawn error in `#+RESULTS:` (§5.3).

`{file}` is the only placeholder in v1. The body is written to the plugin's
private data directory — the guest writes `/data/babel/<uuid>.<ext>` over
WASI, and `data-dir()` returns the matching host path that is substituted;
neither needs an `fs:` grant. A `new-uuid` name keeps concurrent jobs apart.
A script file and not `-c BODY` because it has no argument-length ceiling,
keeps the body out of `ps`, and is what babel itself does for most languages.
The file is deleted on `job-finished`, and `/data/babel/` is swept in
`register-events` so a crash or unload leaves nothing behind for long.

`:dir` is tilde-expanded and must be a directory (`spawn-process` refuses
otherwise). It is **not** checked against an `fs` grant: it is a working
directory, not a write, and the program can reach anywhere the user can
regardless — pretending otherwise would be a guarantee the design does not
give.

Two records may share a `command` with different `args` (`python3` for
`python` and for `jupyter-python`); the grant is the set of commands, and
`args` are not checked — a block body is arbitrary code already, so checking
arguments would narrow nothing.

**Editing the table reloads the plugin** (PMF.4, because the grant changed),
and a reload drops the `PluginState` that owns its jobs — so running blocks
and sessions are cancelled. Their blocks keep their old results; the echo
area says why.

## 4. The host change: grants that come from the user's config

This is lattice's half, and it is generic.

**Today.** `fs:write:{org.directory|~/org}` is an *option-bound* grant
(PMF.8): the path the plugin may write is read from an option in the
plugin's own namespace, re-read whenever the grant is built, and a change to
it reloads the plugin (PMF.4). The plugin cannot set that option itself
(PMF.5) — only the user can. But it binds **one path**, and `proc:spawn` is
not option-bindable at all: it is bundled-only, so a user-installed plugin
cannot run anything.

**The change has two parts, one mechanism.**

1. **Option-bound grants may read a list.** `{org.babel.tangle-directories}`
   names a `list<string>` option; each entry becomes one `FsGrant`. The
   existing scalar form is the one-element case. Each entry is reported as its
   own resolved grant naming the option, and a list that gains or loses an
   entry counts as a moved grant (PMF.4 reload). This is what lets tangling
   write outside `org.directory` without granting the plugin the whole disk.
2. **`proc:spawn:{option:field}` — an option-bound process grant.** The option is a
   list of records (the config seam's structured `schema-node`s already carry
   lists and records; `[[org.babel.languages]]` is that shape in TOML); the
   host reads each record's `command` field and grants spawning **exactly
   those programs**. `spawn-process` checks `command` against the set,
   host-side, in the call itself (where the `proc_spawn` check already sits —
   so it holds on the synchronous grammar seam too), and refuses anything else
   with an error naming the option. A user-installed plugin may declare it;
   the tier rule becomes "unbound `proc:spawn` is bundled-only".
   **It follows only a value the user wrote** — a config file, `init.rs`, or
   `:set` — never the default the plugin registered for the option. A
   directory grant may follow its default because the manifest states it in
   its own template (`{org.directory|~/org}`), visible before load; a process
   grant has no template default, and a plugin whose registered default listed
   `sh` would otherwise grant itself a shell. An option still at its registered
   default therefore grants no program, and is not remembered across a reload.
   A remembered value carries whether the user wrote it, so a directory grant
   bound to the same option cannot launder the default into a program either.
   (Both found by a security review during OB.2; pinned by
   `a_plugins_own_default_never_grants_a_program` and
   `a_default_remembered_for_a_directory_grant_is_no_program`.)
3. **Effects from the events seam.** `on-event` returns nothing, and the
   events world imports no effect sink — so a job that finishes on the events
   actor has no way to write `#+RESULTS:`. It can *read* (`source-line`), not
   edit. The addition is a `host-services` import,
   `post-effects(effects: list<effect>)`, which hands the effects to the
   host's inbound path — the same one that wakes the actor off-keystroke, so
   the result paints without a keypress. Additive: no existing guest changes,
   and it is wired on both linkers like the rest of `host-services` (a guest
   importing it against an unwired linker fails instantiation outright). It is
   chosen over making `on-event` return `list<effect>` because that is an ABI
   break for every events guest for a capability most do not use. Effects
   posted this way pass the same capability checks as returned ones — a
   refused write is shown as a warning, as from a command — and `err` means
   only that no sink was wired.

Org's manifest gains:

```toml
capabilities = [
	"fs:write:{org.directory|~/org}",
	"fs:write:{org.roam-directory|~/org-roam}",
	"fs:write:{org.babel.tangle-directories}",
	"proc:spawn:{org.babel.languages:command}",
	"state:write",
]
mode_gates = { "org-babel-mode" = "org.babel.enabled" }
```

**What this protects and what it does not — said plainly.** The allowlist
bounds *which programs* run, not *what they do*: granting `sh` grants
everything `sh` can do, and a block body is arbitrary code by definition. The
real guarantees are (a) nothing runs until the user writes a language entry,
(b) the plugin cannot write that entry itself (PMF.5's rule, extended to the
new grant), (c) the grant is visible in `:plugins` like every other, and
(d) each run is confirmed unless the user turned that off. That is the same
trust emacs asks for, made explicit and default-off.

**Paramount goals.** Protects #2 (extensibility): a capability any external
plugin can use — a REPL plugin, a test runner — rather than an org special
case, and it closes the gap that forced the original cut. Protects #4
(asynchronicity): execution rides the existing `spawn-process` job, so
nothing about babel blocks the actor or the UI thread.

## 5. Execution

### 5.1 Finding the block and its arguments

The tree-sitter `block` node covers every `#+begin_X`; babel acts when
`name` matches `src` case-insensitively. The first `parameter` is the
language (the same anchor `injections.scm` already uses); the rest are
header arguments, parsed as `:key value…` pairs. Effective arguments are
merged in babel's order, later wins: built-in defaults → `#+PROPERTY:
header-args` → `header-args:<lang>` → the headline's `:header-args:` drawer
(inherited) → the block's own line. Pure guest-side text work on the
snapshot the grammar seam already has.

### 5.2 `:results`

Two axes, as in babel. **Collection:** `output` (what the program printed)
or `value` (the value of the last expression). **Insertion:** `replace`
(default), `append`, `prepend`, `silent` (echo only), `none`. **Format:**
`verbatim` (`: ` lines, or `#+begin_example` past
`org.babel.min-lines-for-block-output`, default 10 like emacs), `table` (the
output parsed as rows), `raw`, `drawer`, `code`.

`value` needs the body wrapped so the last expression is printed — babel
does this per language in elisp. Here it is a **named built-in wrapper**
selected by the language entry's `value` field (`python`, `ruby`, `node`,
`sh`); a language with no wrapper treats `value` as `output` and says so
once in the echo area. **Default collection is `output`** when the language
has no value wrapper, and babel's `value` when it does — the one place
lattice's defaulting is narrower than emacs's, and it is refused-visibly
rather than guessed.

### 5.3 Results, and the block that moved

The job reports output as batched lines and then one `job-finished`
(lighthouse §3.0). The events seam accumulates the lines per job id — capped
at `org.babel.max-output-lines` (default 10 000; past it, a final
`… N lines truncated` line) — and, on finish, posts (§4, part 3) one
`effect.apply-edit` that replaces the `#+RESULTS:` block.

**The buffer may have changed in between** — the user kept typing, or ran
another block above this one. So the result is never written at a remembered
line. Each run records a **block key**: the `#+NAME:` when there is one,
otherwise the language plus a hash of the body as it was when the run
started, plus the line it was on as a tie-breaker between identical blocks.
At finish the events seam re-reads the buffer (`source-line`), finds the
block by key, computes the current `#+RESULTS:` range, and edits exactly
that. If the block is gone or its body changed (no key match), the output
goes to `*org-babel*` (an output buffer, `output-append`) and the echo area
says so; nothing is written to the org buffer. Never an edit at a stale
offset.

**The outcome string is shown, not parsed.** `job-finished` is `ok` or an
`err(string)` — a non-zero exit, `"cancelled"`, and a program that could not
start all arrive as strings. Babel treats the first two alike (output
written, status echoed) and the third as an error whose message *is* the
result, which needs no parsing either: whatever output arrived is written,
and the `err` text is echoed verbatim. A structured exit status is not
needed for anything this fragment specifies.

**Job state crosses seams through the plugin store, not a `thread_local`.**
The grammar seam starts the job; the events seam hears it finish. They are
separate instances, so the job → (buffer, block key, merged arguments) map
is written with `state:write` at spawn and read at finish.

**A failing program is a result.** Non-zero exit: the output is written as
usual and the echo area names the status; `#+RESULTS:` is still replaced, as
babel does. stdout and stderr arrive interleaved (the host does not separate
them) — v1 accepts that and notes it; babel's separate error buffer is a
follow-up if it proves wanted.

### 5.4 `C-c C-c` is an arm on babel's mode, not a branch in org's

Per [`org-mode.md`](org-mode.md) §5.4 and the "a feature set is a mode"
rule: `org-babel-mode`'s layer binds `<C-c><C-c>` *above* org's major layer
and returns `Effect::Declined` outside a src block — exactly the shape
`org-table-mode` already uses. Org's `ctrl_c_ctrl_c` does not grow a
src-block branch, and org's major mode binds no babel chord at all. Babel is
`org.babel.enabled` (default on — inert anyway until a language is
configured). Ownership in full: §0.

`C-c C-c` is shared (table, checkbox, headline, babel), so declining is
right for it. The `<C-c><C-v>` prefix is babel's alone and its chords never
decline.

## 6. Cross-block features

### 6.1 `:var`

`:var x=disk` binds `x` to the **result** of the block named `disk`;
`:var n=5`, `:var s="text"` bind literals; `:var t=tbl` binds a named org
table as a list of rows. The value reaches the program as a prelude
generated into the script file — a per-language assignment (`x='…'` for sh,
`x = [...]` for python), from the same built-in table as the value
wrappers. A language with no prelude support refuses `:var` by name.

**A referenced block's result is read from its `#+RESULTS:`**, not re-run,
unless that result is absent — then it is run first, and the referring block
waits. Dependencies are a chain of jobs driven by `job-finished`, with
cycle detection over names; the running decoration shows on each block
in the chain as it runs.

### 6.2 noweb

`:noweb yes` expands `<<name>>` in the body to the body of the block named
`name` (recursively, cycle-checked) before running or tangling; `<<name()>>`
expands to its result. Pure text, guest-side. `:noweb tangle` / `no-export`
follow babel's meanings for when expansion applies.

### 6.3 `:session`

A session is a long-lived interpreter that successive blocks share. It needs
two things the host does not have: **stdin** on a spawned job, and a job
that outlives one block. The host addition is
`spawn-interactive` — `spawn-process`'s signature and gate, with stdin left
open — plus `write-job-stdin(id, bytes)` / `close-job-stdin(id)`.
`spawn-process` itself is unchanged (stdin stays null), so no existing guest
changes; the additions follow the WIT package's versioning rule like any
new import. Each block's output is delimited by an echoed sentinel line,
as babel does; sessions are listed in `*org-babel*` and killed by
`:org-babel-cancel`, by unload, or when their buffer closes. This is the last
slice because it is the only one that needs a new kind of job.

## 7. Tangling

`:tangle path` (or `yes`, meaning the org file's name with the language's
`ext`) writes the block body — noweb-expanded — to `path`, relative to the
org file. Blocks tangling to the same file are concatenated in document
order, with `:padline` and `:shebang` honoured (a shebang also sets the
executable bit via `set-executable`). `#+PROPERTY: header-args :tangle …`
applies file-wide.

**Tangle replaces whole files**, which `effect.write-to-file` cannot do — it
inserts at an anchor. The tangle slice therefore adds a `whole` anchor to
`write-to-file` (replace the buffer's entire text, creating the file if
absent), gated on the same `fs:write` grant, and tangle sends it with
`save = true`. `write-to-file` goes through the document pipeline rather than
straight to disk, which is what tangle wants: a target the user has open gets
the new text as an undoable edit, and the save is what puts it on disk. A
target not already open is opened `nobuflisted` so tangling twenty files does
not fill `:ls`.

Every target is checked *before* any file is written, with the existing
`can-write-file` host call: a tangle with one disallowed target writes
nothing and names the target and the option to extend
(`org.babel.tangle-directories`).

Detangling (`org-babel-detangle`, writing edits back from the tangled file)
is out.

## 8. Not built, and why

- **`:exports`, export backends.** Lattice has no exporter; `:exports` is
  parsed and ignored (it changes nothing at execution time).
- **`:cache`.** Needs babel's hash in `#+RESULTS[hash]:`; deferred until
  someone wants it. The hash form is still *read* so an emacs-cached result
  is replaced cleanly.
- **Graphics results** (`:file plot.png`). The file is written by the
  program; lattice writes the `[[file:…]]` link. Inline display is the media
  seam's business and comes free when it renders links.
- **Languages babel evaluates inside emacs** (`emacs-lisp`, `org`, `calc`).
  There is no elisp; they are unconfigurable here and say so.
- **Remote `:dir`** (TRAMP). No remote filesystem.

## 9. Rejected alternatives

- **A user grant of full `proc:spawn`** in the `require` spec. Simpler, but
  the plugin could run any program, and the trust is recorded somewhere other
  than the config that enables babel. The option-bound form makes the
  enabling line and the permission the same line.
- **Make org a bundled plugin.** Reverses the decision that keeps 2.2 MB of
  generated `parser.c` out of every workspace build.
- **A host-native babel executor.** Puts a feature's handler bodies in the
  host — the mode-ownership half-migration the standing rules forbid.
- **`-c BODY` instead of a script file.** Argument-length ceiling, visible in
  `ps`, and not every interpreter has `-c`.
- **Branching org's `ctrl_c_ctrl_c`.** Grows a match in the major mode
  instead of giving babel's arm to the mode that owns babel (§5.4).

## 10. Order, and the two repositories

The slice plan sequences this; the constraint it must respect is that the
plugin cannot use a host change before a lattice release carries it, and the
plugin's `min_lattice` moves with each one it starts using. So the order is
host-first per phase:

1. **Execute** — host: list-valued + process option-bound grants, and
   `post-effects`. Plugin: `org-babel-mode`, the languages table, header
   arguments, `:results` (output; value wrappers), confirm, cancel,
   execute-buffer/-subtree.
2. **Tangle** — host: the `whole` anchor. Plugin: `:tangle`, `:padline`,
   `:shebang`, tangle directories.
3. **Cross-block** — plugin only: `:var`, noweb, named results, dependency
   chains.
4. **Sessions** — host: `spawn-interactive` and job stdin. Plugin:
   `:session`.

## 11. Performance and the UX contract

Nothing on the UI thread: parsing header arguments is a few lines of text on
the dispatch seam; the program runs in a host job; the result is one edit.
The keystroke contract holds — the only pixels that change are the block's
own `#+RESULTS:`, which the user asked to change. Benches: header-argument
merge on a 5 000-line file with deep `:header-args:` inheritance, and
result-block location after edits above it.
