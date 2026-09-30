<!-- @generated from wit/ by crates/lattice-plugin-api/tests/site_reference_is_current.rs.
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# Lattice Plugin API

Derived from the canonical `wit/` package — 31 seam(s).

## `buffer`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `grammar-plugin` (imports), `multiseam-fixture` (imports), `plugin` (imports), `project-plugin` (imports), `treesitter-context-plugin` (imports)

Mirrors the native `Document` / `Buffer` read seam (plugin-host.md §4.2,
§9.6). The host owns the buffer; the guest gets a `document` **resource
handle** and calls back for the text slices it needs, so bulk rope text
never crosses the boundary. The owned `buffer-snapshot` record carries the
non-bulk metadata — the borrows-projected form of
`lattice_picker::context::ActiveBufferSnapshot`. Populated at PH7.3c; the
guest→host call through the canonical ABI is exercised at PH7.3d/PH7.4.

### Uses

- `position` from `types`
- `range` from `types`

### Functions (0)

_(none outside its resources — see Resources below)_

### Resources

#### resource `document`

A host-owned, point-in-time view of a document's text (PH7.3c,
decision A: backed by an `Arc<DocumentSnapshot>`). Because it is a
snapshot, edits landing after the handle is minted never shift byte
ranges under the guest mid-read (the §4.2 mutation-under-read hazard).

##### `document.byte-len`

```wit
byte-len: func() -> u64
```

Total byte length.

##### `document.get-text-range`

```wit
get-text-range: func(r: range) -> result<string, string>
```

The text of the `[start, end)` byte range. `err` on an out-of-range
or `end < start` range (mirrors `Buffer::slice`). Only the requested
range is sliced out of the rope — the whole document never crosses
("zero-copy at the slice level", §9.6).

##### `document.line`

```wit
line: func(n: u32) -> option<string>
```

Line `n` (0-based) as text without its trailing newline (matching
`Buffer::line`), or `none` when `n` is past the last line.

##### `document.line-count`

```wit
line-count: func() -> u32
```

Lines the document has: `"a\nb\n"` is two lines, and so is
`"a\nb"` — a trailing newline terminates the last line rather
than starting another. Safe as the bound of a
`0..line-count` walk calling `line`.

##### `document.path`

```wit
path: func() -> option<string>
```

OM.6b: the file this document is backed by, absolute. `none` for a
buffer with no file on disk — a scratch buffer, a synthetic one, or
a file whose path is not UTF-8 (it cannot cross as a `string`, and
one oddly-named file must not fail the call).

**Why the resource and not a context field.** "Which file am I
editing" is a question every content-aware guest asks, and a guest
asking it always holds a `document`. On a context it would have to
be re-added to `motion-context`, `text-object-context` and
`ex-command-context` in turn, and every dispatch would pay the
string clone whether or not the guest read it.

Snapshot semantics, like every other method here: this is the path
as of the handle's mint. A `set-path` landing mid-action is
invisible, which is the same trade `get-text-range` already makes
and for the same reason.

### Types (1)

#### record `buffer-snapshot`

```wit
record buffer-snapshot {
    buffer-id: u32,
    path: option<string>,
    language: option<string>,
    cursor: position,
    selection: option<tuple<position, position>>,
}
```

The owned projection of `ActiveBufferSnapshot`'s metadata (§4.2). Bulk
text is NOT a field here — it rides the `document` handle. `selection`
is `(anchor, head)` when a Visual selection is active.


## `command`

**Direction:** shared types only (not called directly) · **Capability:** none (pure data / dispatch)

Mirrors `CommandRegistry` + `CommandInvocation` + the closed `Effect`
enum (lattice-grammar). Guest→host `invoke`; host→guest `apply`. The
`effect` WIT variant mirrors the ~105-variant enum whole (§4.4) so the
boundary stays typed. Populated in PH7.3 (Effect round-trip) / PH7.7.

### Functions (0)

_(none — a shared type interface)_


## `completion-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `completion-source-plugin` (exports)

Mirrors `lattice_completion` completion sources (PH7.6). A WASM completion
source *exports* this interface; the host drives its async `generate` off the
keystroke path (the LSP-async-completion precedent, `pipeline.rs`
`match_and_rank` "pre-supplies rows from async LSP responses") and feeds the
produced candidates through the **native** matcher / ranker / annotator.

**Generator only, by design (option A, locked with Dhruva).** The four native
traits — `Candidate{Generator,Matcher,Ranker,Annotator}` — are NOT four guest
exports: `matches` + `annotate` run *per candidate* on the synchronous
keystroke pipeline, so crossing them to an async, actor-bound guest per item
would fire hundreds of boundary calls per keystroke (paramount #1). The
plugin's value-add is the GENERATOR (async produce, like LSP); matching /
ranking / annotation stay native (they have good defaults a plugin rarely
overrides — "the API grows from real plugins", design §5.5). The matcher /
ranker / annotator data types are still mirrored in `types.wit` so the WIT is
sized against the whole trait set before the ABI freeze.

### Uses

- `completion-source-spec` from `types`
- `generate-context` from `types`
- `raw-candidate` from `types`

### Functions (2)

#### `generate`

```wit
generate: func(ctx: generate-context) -> result<list<raw-candidate>, string>
```

Produce raw candidates for the current slot. `ctx` carries the query
prefix + case flag (§4.2 owned projection); the host then runs the
native `match_and_rank` over the result. Async — a produce call suspends
the guest, never the keystroke path. An `err` string is logged and the
source contributes no rows (the LSP-failure precedent). Candidates carry
plugin-specific data via the `candidate-data.extension` hatch.

#### `spec`

```wit
spec: func() -> completion-source-spec
```

The source's identity (`name` + `doc`), the `insert_generator` pair.
Called once at registration.


## `config`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `config-plugin` (imports), `init-fixture` (imports), `multiseam-fixture` (imports), `preload-fixture` (imports), `project-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `treesitter-context-plugin` (imports)

Mirrors `ConfigRegistry` (lattice-config). The guest declares an option
(name + type + default + doc); the host registers it into the *same* registry
core options live in, so `:set` / `:describe-option` / `:customize` /
`gen:options` completion treat plugin options uniformly (no host kind-branch).
Values round-trip as strings via the native `OptionType` parse/format
contract. Populated in PH7.10.

This interface is the CANONICAL, language-agnostic option API — any
component-model language (Go, JS, Zig, Python, ...) calls these directly. The
Rust `lattice-plugin-sdk` `#[derive(PluginOption)]` (PH7.10b) is optional
ergonomics that expands to these same calls; it adds no capability not here.

### Functions (8)

#### `get-option`

```wit
get-option: func(name: string) -> option<string>
```

Read an option's current value, formatted as a string (the `OptionType`
`format` contract). `none` if no option by that name is registered.
Resolves the plugin's OWN namespace first (`style` → `<id>.style`), then
the raw name — so a plugin reads its own options with short names AND can
still read a core option (`tabstop`) that isn't in its namespace.

#### `get-option-value`

```wit
get-option-value: func(name: string) -> option<config-value>
```

Read an option's current value as a tree. `none` if no option by that
name is registered. Resolves the caller's OWN namespace first, exactly
like `get-option`.

Works for scalar options too — a scalar is a degenerate schema, so a
guest that wants typed reads everywhere can use this one call rather
than choosing per option.

#### `option-diagnostic`

```wit
option-diagnostic: func(name: string) -> option<config-diagnostic>
```

**Did the last assignment to `name` fail, and what did it say?**

A failed assignment is a no-op — vim's rule, which lattice keeps — so
the option is left holding whatever it had, and for one that was never
successfully set that is its registered DEFAULT. Reading the value
therefore cannot distinguish "the user configured this and it did not
parse" from "the user never configured this". This can.

org-capture is the case that forced it: a `capture-templates` whose
TOML did not fit its schema read back as the empty default, so capture
filed through the legacy `capture-file` believing nothing had been
configured — and the user's note went somewhere they thought they had
stopped using.

**This is not a status the option carries.** The option has no such
state; an assignment errored, which is an event, and this is the record
of it. `none` means the last assignment succeeded, or there was never
one — those two are not distinguished, and deliberately: a plugin's
question is "can I trust this value", and both answers are yes.

Cleared for a name as soon as an assignment to it succeeds, and the
whole record is rebuilt on each config load, so a user who fixes their
file stops being told it is broken.

Resolves the caller's OWN namespace first, like `get-option`.

#### `register-option`

```wit
register-option: func(name: string, ty: option-type, default: string, doc: string) -> bool
```

Declare a plugin option into the editor's `ConfigRegistry`. `default` is
the initial value as a string (parsed via the chosen `option-type`); `doc`
is the `:describe-option` summary. Returns `false` (registering nothing) if
`default` doesn't parse for `ty` OR `name` collides with an existing option
— a plugin must not silently shadow another option. Idempotent to retry
after a rejected default.

**Auto-namespaced.** `name` is prefixed with the plugin's id — a plugin
with id `auto-pair` registering `style` contributes `auto-pair.style`. Use
SHORT names; the host owns the namespace so plugins can't collide (and a
user sets it as `:set auto-pair.style=…`). `get`/`set-option` resolve the
same way (short name → own namespace).

#### `register-structured-option`

```wit
register-structured-option: func(name: string, schema: config-schema, default: config-value, doc: string) -> bool
```

Declare an option whose value has structure. The schema-taking peer of
`register-option`, with the same namespacing and the same collision
rules.

`default` is validated against `schema` before anything is registered:
a plugin whose own default does not fit its own declaration registers
NOTHING and gets `false`, rather than an option that exists and cannot
hold a legal value.

#### `set-option`

```wit
set-option: func(name: string, value: string) -> bool
```

Set (override) an EXISTING option's value (CI.7) — the init.rs config
front-end symmetric with `lattice.toml` and `:set`. Backed by the same
`parse_and_set_command` path `:set name=value` uses: the value string is
type-coerced and validated, and a successful set publishes
`OptionChanged` so subscribers react uniformly. Returns `false` (setting
nothing) if the option is unregistered, the value is invalid for its type,
or no registry is wired — never a trap. An `init.rs` `on-plugin-loaded`
handler uses this to configure a plugin's options the moment it loads
(config-and-init.md §5). Like `get-option`, resolves the caller's OWN
namespace first (`style` → `<id>.style`), else the raw name — so a config
can set another plugin's option by its full `auto-pair.style` name.

#### `set-option-in-buffer`

```wit
set-option-in-buffer: func(buffer: u64, name: string, value: string) -> bool
```

Set an option for ONE buffer — the `:setlocal` front-end, and the call a
mode-lifecycle handler needs.

`set-option` above writes the GLOBAL layer (it is the `:set` path), so a
handler that wants *wrap in org buffers* cannot use it: it would wrap
everything, and nothing would unwrap on leaving org. This writes the
buffer-local override layer instead, which is exactly the scope the
question has.

The canonical use is a `major-entered` / `minor-activated` subscription
filtered to one mode — `add-hook 'org-mode-hook` in this editor's
vocabulary:

```ignore
subscribe(&EventFilter {
    kinds: Some(vec![EventKind::MajorEntered]),
    major_modes: Some(vec!["org-mode".into()]),
    ..
}, ON_ORG);
// in the handler:
set_option_in_buffer(ev.buffer, "autowrap", "all");
```

Works uniformly for built-in, core-plugin and external-plugin modes:
the lifecycle events are published by the mode dispatcher, which does
not know which of those declared the mode.

Parsed by the same path `:setlocal name=value` uses, so a guest can
express nothing `:setlocal` could not and a bad value is refused with
the same message. Returns `false` — setting nothing — on an unknown
option, an invalid value, or an unknown buffer; never a trap, the
`set-option` contract.

**Applied on the next host tick, not synchronously.** The buffer-local
layer lives on the Editor rather than in the config registry, so this
publishes a host-internal request the Editor drains. A handler cannot
observe its own write by reading the option back in the same call.

#### `set-option-value`

```wit
set-option-value: func(name: string, value: config-value) -> bool
```

Set an option from a tree. Validated against the option's declared
schema, so a bad field is refused with a PATH
(`templates[2].target.file: expected string, got integer`) rather than
by whatever message the plugin would have written. `false` on an unknown
option, a value that does not fit, or no registry — never a trap, the
`set-option` contract.

### Types (7)

#### enum `option-type`

```wit
enum option-type {
    boolean,
    integer,
    string,
}
```

The value type of a plugin option. Maps 1:1 to a native `OptionType`
impl: `boolean`→`bool`, `integer`→`i64`, `string`→`String`. The option's
value is set / read as a `string` and parsed/formatted through that type
(so `:set name=value` and `get-option` share one string contract).

**Cases**

- `boolean`
- `integer`
- `string` — `%`-escaped: `string` is a reserved WIT keyword. Generates the
  `OptionType::String` variant.

#### record `schema-field`

```wit
record schema-field {
    name: string,
    schema: u32,
    required: bool,
    doc: string,
}
```

── TC.3: options that have STRUCTURE ─────────────────────────────────

WIT has no generics, so a plugin-defined record cannot be a fixed
host-side type — the host would need a different record per plugin,
which a shared ABI cannot have. The expressible answer is
self-description: the guest declares a SCHEMA (ordinary WIT data), values
cross as a generic value TREE, and the HOST validates one against the
other. See `typed-configuration.md`.

`option-type` above is not replaced — it is the three-scalar shorthand
for the common case, and `register-option` remains the call almost every
plugin makes. What changes is that a scalar is now understood as a
degenerate schema rather than as the only thing an option can be.
**WIT has no recursive types**, and that is not a detail to route
around quietly — a schema and a value are both trees, and the obvious
spelling (a variant whose arm holds another variant) fails to parse:
"type `config-schema` depends on itself". So both cross as an ARENA: a
flat list of nodes plus the index of the root, with children referenced
by index. The guest builds the arena (the SDK derive does it
mechanically); the host rebuilds the tree, and rejects a bad index or a
cycle at the boundary rather than following it.
One field of a `schema-node.record`. `schema` is an INDEX into the
owning `config-schema.nodes`, which is how nesting survives an ABI with
no recursion.

`doc` is per field, not only per option, because that is what
`:describe-option` and `:customize` render beside it — an option-level
doc string describing six fields is the wall of prose this replaces.

**Fields**

- `name`: `string`
- `schema`: `u32`
- `required`: `bool` — A missing required field is a validation error naming its path; a
  missing optional one is simply absent from the value.
- `doc`: `string`

#### variant `schema-node`

```wit
variant schema-node {
    scalar(option-type),
    enum-of(list<string>),
    list-of(u32),
    record(list<schema-field>),
}
```

One node of a schema arena. Mirrors `lattice_config::ConfigSchema`, with
child links as indices.

`enum-of` is not sugar for a string: it is the difference between
`:customize` offering a picker and offering a text field.

**Cases**

- `scalar`: `option-type`
- `enum-of`: `list<string>`
- `list-of`: `u32` — The element shape, by index.
- `record`: `list<schema-field>`

#### record `config-schema`

```wit
record config-schema {
    nodes: list<schema-node>,
    root: u32,
}
```

The declared shape of an option's value, as an arena.

`root` is explicit rather than "node 0 by convention": a convention is
an invariant nothing checks, and this one has to be range-checked at the
boundary regardless.

#### variant `value-node`

```wit
variant value-node {
    bool(bool),
    int(s64),
    string(string),
    list(list<u32>),
    record(list<tuple<string, u32>>),
}
```

One node of a value arena. Mirrors `lattice_config::ConfigValue`.

A record's fields are an association list because WIT has no map; the
host converts to an ordered map on arrival, so two values differing only
in field order are the same value — which they must be, since one config
home writes TOML (unordered) and the other writes a struct.

#### record `config-value`

```wit
record config-value {
    nodes: list<value-node>,
    root: u32,
}
```

A value shaped by a `config-schema`, as an arena.

#### record `config-diagnostic`

```wit
record config-diagnostic {
    message: string,
    source: string,
}
```

OC.11c: one failed assignment to an option.

**Fields**

- `message`: `string` — The message the loader or the registry produced, verbatim. For a
  composite it carries the schema PATH —
  `[2].target.file: expected string, got integer` — which is the
  whole reason this is worth surfacing over a bare "it failed".
- `source`: `string` — The config file the assignment came from, or empty for a runtime
  `:set`. That distinction is "go fix your config" versus "the thing
  you just typed did not take", and a guest reporting one as the
  other sends the user to the wrong place.


## `context`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `context-plugin` (exports), `treesitter-context-plugin` (exports)

The structural-**context** producer API (treesitter-context.md, TC.2): the
scopes a pane pins above its text once their own header lines have scrolled
away — the `nvim-treesitter-context` / sticky-scroll idea.

**Scopes cross, not rows.** A `context-scope` is a pure function of the parse
tree, so the host caches the set per parse version and resolves "which of
these apply to THIS pane right now" itself, per pane, per frame
(`lattice_cells::context::resolve_context`, TC.1). Returning finished rows
instead would put a WASM call on the scroll path and give the host a cache
keyed on the cursor — one that thrashes by construction. Paramount #1.

**Producer, async, host-cached** — the `decorations` shape (PH7.9), for the
same reason: the host calls `context-scopes` OFF the render path on a trigger
(a completed reparse), caches the result, and every later read is native. The
guest never runs on a keystroke.

### Uses

- `context-request` from `types`
- `context-scope` from `types`
- `tree-snapshot` from `tree-sitter`

### Functions (1)

#### `context-scopes`

```wit
context-scopes: func(req: context-request, tree: option<borrow<tree-snapshot>>) -> result<list<context-scope>, string>
```

Produce the structural context scopes for a buffer.

`tree` is the buffer's point-in-time parse snapshot, handed in the way
`grammar.apply-action` hands it (TS.1) rather than acquired by the guest:
call-scoped access keeps the `tree-sitter` capability meaning "the tree
you were given" instead of "any buffer's tree, any time". It is `none`
when the buffer has no parse (plain text, or a parse still pending), and
a guest with nothing to work from should return an empty list.

Async — a produce call suspends the guest and never the render path. The
guest is expected to run a whole-buffer `run-query` here, which is why
this must not be synchronous.

Graceful (§8): an `err` is logged and the buffer KEEPS its previously
cached scopes rather than being cleared. A failed refresh must not blank
the strip — a transient error would otherwise read as the feature
breaking. Same contract as `decorations.gutter-decorations`.


## `dashboard`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `dashboard-plugin` (imports)

CR.4: plugin-contributed dashboard sections.

A plugin puts its own block on the launch page — recent projects, a git
summary, whatever it is for. The section lands in the SAME registry the
built-in sections live in, so `dashboard.sections` orders it, the
compositor renders it, and the theme styles it, with no host kind-branch.

#### A function, not data — unlike `help`

The `help` seam hands over a string once and drops the guest, because a
help page does not change between load and read. A dashboard section is
different in kind: `render-section` takes a `ctx` the guest cannot know at
load — the pane width, whether Nerd Font glyphs are available, the editor
version — and DB.6 exists precisely because those change while the editor
runs. So the guest stays instantiated and the host calls it per compose.

Freezing a section into text at registration would make it blind to the
icon palette and unable to show anything live, which is most of what
"whole-author a section" is for.

#### Where it runs, and what that costs

`render-section` is a **sync** call on the host's sync linker (the one
`grammar` and `error-parser` share), carrying the Reflex-class budget
rather than the generous lifecycle default. It executes on the actor
thread inside the dashboard compositor.

That cost is real and deliberate. Composition is a `LatencyClass::Display`
action — `:dashboard`, startup, or a DB.6 option change — never
per-keystroke and never per-frame, and the fuel budget bounds a
pathological guest to a bounded stall rather than a hang.

The alternative, rendering off-actor and recomposing when the fragment
lands, is purer on paramount goal #1 and was rejected on UX: it makes the
launch page visibly reflow a frame or two after it appears, at startup,
which is the content-jump the UX contract vetoes.

#### What the host does with a bad fragment

Validates and drops, never traps. Guest output is untrusted: a row with no
spans, a span whose link does not parse, or a fragment longer than the row
cap is dropped at `debug!`. A trap poisons the section — it renders
nothing further this session and the REST OF THE PAGE still composes,
exactly as a trapping `error-parser` costs its own entries and not the
build.

### Functions (1)

#### `register-section`

```wit
register-section: func(id: string, order: s32, default-enabled: bool) -> result<_, string>
```

Declare a section.

`id` is **NOT** auto-namespaced, unlike `help.register-topic` and
`theme.register-element`. That is deliberate: replacing a built-in
section by id is a supported thing to want, so a plugin registering
`getting-started` is exercising the feature rather than squatting.
Unload restores whatever it displaced — the registry shadows rather
than overwrites.

`order` is the default sort key (lower sorts first); `default-enabled`
is whether it shows when the user has not set `dashboard.sections`.

`err` when the spec is malformed (an empty id) — never a trap.

### Types (7)

#### record `ctx`

```wit
record ctx {
    pane-width: u32,
    nerd-fonts: bool,
    version: string,
}
```

Read-only facts a section renders against. Mirrors the native
`DashboardCtx`.

**Fields**

- `pane-width`: `u32` — Pane width in cells.
- `nerd-fonts`: `bool` — Whether Nerd Font glyphs may be used. A section that draws icons
  MUST honour this and fall back to the BMP-block palette at the
  same cell width, or the page's column geometry shifts when the
  user toggles `ui.nerd_fonts`.
- `version`: `string` — The editor version string.

#### enum `role`

```wit
enum role {
    logo,
    cursor,
    title,
    tagline,
    section-heading,
    body,
    key,
    hint,
    link,
}
```

Semantic style role. Never a colour — each resolves to a `dashboard.*`
theme element at compose time, so a section re-colours on
`:colorscheme` like everything else.

#### enum `align`

```wit
enum align {
    left,
    center,
}
```

Line-level alignment.

#### variant `link-target`

```wit
variant link-target {
    command(string),
    topic(string),
    url(string),
}
```

What `<CR>` on a link span follows.

**Cases**

- `command`: `string` — Run an ex-command — `command("tutor")` STARTS the tutor.
- `topic`: `string` — Open a `:help` topic.
- `url`: `string` — Open a URL externally.

#### record `span`

```wit
record span {
    text: string,
    role: role,
    link: option<link-target>,
}
```

A run of text with a role and an optional follow target.

#### record `row`

```wit
record row {
    spans: list<span>,
    align: align,
}
```

One visual line: spans laid out left→right.

#### record `fragment`

```wit
record fragment {
    rows: list<row>,
}
```

A section's rendered contribution.


## `decorations`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `decorations-plugin` (exports)

The decoration **producer** API (plugin-host.md §5 `decorations`, PH7.9),
mirroring `Mode::gutter_decorations` + `GutterDecoration` (lattice-mode). A
WASM decoration provider *exports* this interface; the host calls its
`gutter-decorations` producer **off the render path** on a trigger (edit /
scroll / diagnostic change), caches the returned `list<gutter-decoration>`
per buffer, and the renderer reads the cache.

**Producer, not per-frame (the completion PH7.6 fork).** The native
`Mode::gutter_decorations` is a SYNCHRONOUS trait the renderer reads *every
frame* — a WASM mode cannot satisfy it inline (that would be per-frame WASM,
a paramount-#1 violation, §7 rule 7). So the seam is an ASYNC producer whose
result the host caches; the renderer never calls WASM on the tick. The
matching / layout of the cached decorations into physical gutter columns stays
native (the host builds the snapshot).

### Uses

- `decoration-context` from `types`
- `gutter-decoration` from `types`

### Functions (1)

#### `gutter-decorations`

```wit
gutter-decorations: func(ctx: decoration-context) -> result<list<gutter-decoration>, string>
```

Produce the per-line gutter decorations for a buffer. `ctx` is the owned
projection (buffer id / path / line count, §4.2); bulk buffer text (a diff
producer's input) rides `host-services` / the deferred `document` handle,
not the context. Async — a produce call suspends the guest, never the
render path. An `err` string is logged and the provider contributes no
decorations for this trigger (graceful, §8) — the cached snapshot keeps its
prior value so cues never flicker mid-refresh.


## `error-parser`

**Direction:** shared types only (not called directly) · **Capability:** none (pure data / dispatch) · **Worlds:** `error-parser-plugin` (imports)

CM.6: plugin-contributed compilation-output parsers.

A plugin teaches lattice to recognise diagnostics from a build tool the
editor has never heard of. The native set covers cargo/rustc, gnu-style,
and test panics; everything else in the world — a bespoke linter, an
in-house build system, a language whose compiler predates all of them — is
what this is for.

#### Line at a time, because the format is

`feed` takes ONE line and returns the entries that line *completed*. A
multi-line format (cargo's `error:` header followed by an `--> file:l:c`
arrow two lines later) keeps its own pending state inside the guest and
emits when the location arrives; a single-line format emits or returns
nothing. It mirrors the native `CompilationParser` trait exactly, because
a plugin parser and a native one are the same job and should not have
different shapes.

`reset` drops that pending state at the start of a run, so a build
interrupted mid-diagnostic cannot leak a half-parsed entry into the next
one.

#### Where it runs

Off the UI and actor threads, in the compilation reader (see
`compilation-mode.md` §5). Not the keystroke path — but it IS the critical
path of a fast producer, so a guest that blocks here backs up a build's
output. The host budgets it per call like every other seam.

#### What the host does with a bad entry

Validates and drops, never traps. A returned `line`/`col` is guest data
and the host treats it as untrusted: a nonsense path or an entry with an
empty path is logged at debug and skipped, exactly as a native parser's
malformed-but-claimed match is. One bad line must not fail a build.

### Functions (0)

_(none — a shared type interface)_

### Types (2)

#### enum `severity`

```wit
enum severity {
    error,
    warning,
    info,
    note,
}
```

Severity of a parsed diagnostic. Mirrors the host's `ErrorSeverity`.

#### record `entry`

```wit
record entry {
    path: string,
    line: u32,
    col: u32,
    severity: severity,
    message: string,
}
```

One diagnostic the parser recognised.

**Fields**

- `path`: `string` — Path as the tool printed it. Relative paths resolve against the
  compilation's working directory, host-side — the guest does not
  need to know where the build ran.
- `line`: `u32` — **0-based** line, like the host's `ErrorEntry`. A tool printing
  1-based line numbers (nearly all of them) subtracts one; doing
  that in the guest keeps one convention on this side of the
  boundary instead of two.
- `col`: `u32` — 0-based column.
- `severity`: `severity`
- `message`: `string`


## `events`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `events-plugin` (imports), `init-fixture` (imports), `multiseam-fixture` (imports), `project-plugin` (imports)

The event/hook **subscription** API (plugin-host.md §5 `events`, PH7.8). The
surface a plugin calls to *observe* editor state transitions — mirroring
`EventBus::subscribe` (lattice-runtime). The host provides this function; the
guest **imports** it and calls it (from its `register-events` export). Each
call records the `(handler, filter)` pair into `PluginState`; after
`register-events` returns, the host wires each recorded subscription to the
native `EventBus` with a host-owned `SubscriptionTarget::Plugin { plugin,
handler, tx }` (PH7.8c) — so a plugin subscription is dispatched by the SAME
bus a native subscriber uses (paramount #2). `:autocmd` from a plugin
desugars to this call.

**Observation-only in v1** (the native bus is observation-only, §5.10): a
plugin sees events, it does not veto or mutate them. The before-class
veto/mutation seam is deferred with the bus's.

`handler` is the guest-chosen id the host passes back to the world's
`on-event` export on delivery (the grammar `callback` precedent) — the
guest's own dispatch key, so the host never allocates it and a plugin can
route many `:autocmd`s to distinct handlers behind one `on-event`. No
`unsubscribe` in v1: a plugin's subscriptions live for its lifetime and tear
down en masse on deactivate/quarantine (the reload/lifecycle seam, PH7.12).

### Uses

- `event-filter` from `types`

### Functions (3)

#### `cancel-wake`

```wit
cancel-wake: func(id: wake-id)
```

Disarm a wake. Unknown / already-cancelled / `0` ids are ignored — a
cancel is idempotent, because the alternative is a guest that must track
host state to avoid a trap. There is deliberately no bulk form: wakes are
cancelled en masse on deactivate / quarantine by the host, for the same
reason `events` has no `unsubscribe`.

#### `subscribe`

```wit
subscribe: func(filter: event-filter, handler: u32)
```

Subscribe `handler` to every event matching `filter` (the declarative
`kinds` / `path-globs` / `major-modes` subset; a custom predicate is the
guest filtering inside `on-event`). The host delivers each match to the
world's `on-event(handler, ev)` export.

#### `wake-every`

```wit
wake-every: func(ms: u32) -> wake-id
```

Ask to be woken every `ms` milliseconds, forever, until `cancel-wake`
(OC.2). Delivery is `on-wake(id)` on the plugin's own actor task — the
SAME channel `on-event` arrives on, so a wake is subject to the same
budget, the same quarantine, and the same "never on the keystroke path"
guarantee (paramount #4). It is not a precise timer: a wake fires no
sooner than the interval and may be late under load, and a late one does
not queue a backlog — the period restarts when the wake is delivered.

Intended for the low-frequency "recompute my own display string" shape
(org's clock re-renders its modeline segment once a minute, `design.md`
Appendix B's idle hooks). It is NOT a frame or animation source: each
firing is a full guest call, so a small `ms` buys a guest call at that
rate for as long as the plugin is loaded.

Returns `0` when no wake mechanism is wired on this seam — a plugin
instantiated on a store with no timer (the sync grammar seam, a test
harness). Like every other seam here that answers rather than traps, the
degradation is honest and visible in the log, and a guest that treats a
`0` as armed simply never hears back.

### Types (1)

#### type `wake-id`

```wit
type wake-id = u32;
```

The host-issued handle for one armed periodic wake (OC.2). Host-allocated
rather than guest-chosen — unlike `handler` above, which the guest picks
because it is a *dispatch key*. A wake is a live resource the host must be
able to cancel unambiguously, so the host names it; `0` is never a valid
id and is what a refused `wake-every` returns.


## `grammar`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `grammar-plugin` (imports), `multiseam-fixture` (imports), `project-plugin` (imports), `treesitter-context-plugin` (imports)

The grammar-**extension** API (plugin-host.md §4.1, PH7.7). This is the
surface a plugin calls to *contribute* new vim grammar —
`register_{motion,operator,text_object,ex_command,action}` — mirroring the
native `CommandRegistry::register_*` imperative API. The host provides these
functions; the guest **imports** them and calls them (from its
`register-grammar` export). Each records the contribution into `PluginState`;
after `register-grammar` returns, the host builds a native `*Spec` with a
trampoline `apply` stamped `SourceLayer::Plugin(id)` and registers it into the
SAME `CommandRegistry` a builtin lives in (PH7.7c) — so a plugin command is
indistinguishable from a builtin to the dispatcher (paramount #3).

The grammar *handling* (dispatcher, `:`-line + chord parser, operator∘motion
composition, ranges, counts, registers) stays native, sync, and untouched; a
plugin only adds entries here. `spec` carries the metadata; the behavior is a
guest export in `grammar-callbacks`, dispatched by a guest-chosen `callback`
id (the PH7.3d trampoline pattern). Registration returns nothing — the guest
dispatches by its own `callback`, and the host stamps the `CommandId` /
provenance (a plugin cannot forge either, §6).

### Uses

- `motion-spec` from `types`
- `operator-spec` from `types`
- `text-object-spec` from `types`
- `ex-command-spec` from `types`
- `action-spec` from `types`

### Functions (5)

#### `register-action`

```wit
register-action: func(name: string, doc: string, spec: action-spec, callback: u32)
```

Contribute a chord-bound action. `callback` → `grammar-callbacks.apply-action`.

#### `register-ex-command`

```wit
register-ex-command: func(name: string, doc: string, spec: ex-command-spec, parse-callback: u32, apply-callback: u32)
```

Contribute an ex-command. TWO callbacks — `parse-callback` →
`grammar-callbacks.parse-ex-args` (the `:` line's rest → typed `args`),
`apply-callback` → `grammar-callbacks.apply-ex-command`.

#### `register-motion`

```wit
register-motion: func(name: string, doc: string, spec: motion-spec, callback: u32)
```

Contribute a motion. `callback` is the id the host passes back to
`grammar-callbacks.apply-motion` on dispatch.

#### `register-operator`

```wit
register-operator: func(name: string, doc: string, spec: operator-spec, callback: u32)
```

Contribute an operator. `callback` → `grammar-callbacks.apply-operator`.

#### `register-text-object`

```wit
register-text-object: func(name: string, doc: string, spec: text-object-spec, callback: u32)
```

Contribute a text object. `callback` → `grammar-callbacks.apply-text-object`.


## `grammar-callbacks`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (exports), `comment-plugin` (exports), `grammar-plugin` (exports), `multiseam-fixture` (exports), `project-plugin` (exports), `treesitter-context-plugin` (exports)

The behavior callbacks a grammar plugin **exports**; the host calls one by
`callback` id on dispatch (the PH7.3d callback-id trampoline). **Synchronous**
— a grammar `apply` resolves on the keystroke path (the PH7.7 fork: a motion
must return inline to compose with its operator; async would break
operator∘motion atomicity + dot-repeat/macros). Each maps its native
evaluator's `GrammarResult<...>`: `ok` is the produced value; an `err` string
is logged and the contribution is a no-op (graceful degradation, §8). A trap
(fuel/epoch) is the runaway guard — the host catches it, logs, and the
contribution no-ops, never a hang (a Reflex-class budget bounds it, PH7.7c).

An operator/ex-command/action returns `list<effect>` — the boundary form of
the closed `Effect` enum (`Effect::Many` flattens to the list; §4.4). A text
object returns the `range` it resolved; a motion its `motion-result`.

### Uses

- `motion-context` from `types`
- `motion-result` from `types`
- `operator-context` from `types`
- `text-object-context` from `types`
- `ex-command-context` from `types`
- `action-context` from `types`
- `range` from `types`
- `effect` from `types`
- `args` from `types`
- `document` from `buffer`
- `tree-snapshot` from `tree-sitter`

### Functions (6)

#### `apply-action`

```wit
apply-action: func(callback: u32, ctx: action-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<list<effect>, string>
```

#### `apply-ex-command`

```wit
apply-ex-command: func(callback: u32, ctx: ex-command-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<list<effect>, string>
```

OC.10 gave this `doc` and `tree`, so a plugin ex-command can read the
buffer it was invoked from — the same pair `apply-action` receives, minted
at the same instant so their versions agree (§7). `tree` is `none` for a
plain-text buffer, a parse still in flight, or a plugin without the
`tree-sitter` grant.

#### `apply-motion`

```wit
apply-motion: func(callback: u32, ctx: motion-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<motion-result, string>
```

OM.4: a motion receives `borrow<document>` too. The `apply-action`
doc-comment below anticipated this — "text-reading motions (structural /
word motions) can reuse the same handle when a motion signature needs
it" — and org's headline motions (`]]` / `[[` / `g{`) are the first that
do: finding the next headline means reading lines.

OT.1: a motion receives the tree too, on the same terms as an action —
acquired the same instant as `doc`, `none` when the buffer has no parse.

This doc-comment used to say a motion gets the document but NOT the tree,
because "the native `MotionContext` carries a `ScopeResolver` rather than
a `SyntaxSnapshot`, so there is no tree handle to mint here without
changing the native context — and no motion has yet needed one. When one
does, that is the slice that adds it." Org's headline motions are that
motion: they resolve `(section)` / `(headline)` structure, and hand-rolled
star-counting is what OT.x exists to end.

The native change was smaller than that paragraph predicted.
`GrammarEnv::syntax` already carried the type-erased snapshot on every
dispatch — `execute_action` cloned it into `ActionContext` and the motion
and text-object contexts simply never read it. So this cost two borrowed
fields, not new plumbing. Borrowed rather than cloned because motions fire
on every `j`: a native motion pays nothing, and only a plugin motion that
actually mints the resource pays the `Arc` bump.

#### `apply-operator`

```wit
apply-operator: func(callback: u32, ctx: operator-context, doc: borrow<document>) -> result<list<effect>, string>
```

CM.1: an operator receives `borrow<document>`, the pair the motion,
text-object, action and ex-command callbacks already had (AP.0.1,
OM.4b, OT.1, OC.10). It was the last one without, because no plugin had
contributed an operator — and a comment operator cannot work without
the text: comment-vs-uncomment, the indent column, and stripping an
existing leader are all reads.

**No `tree`, deliberately.** `OperatorContext` carries `document` and
`comment_syntax` but, unlike `TextObjectContext`, no `path` or
`syntax` — minting a tree resource would mean widening the native
context and every operator call site for a capability no operator has
asked for. Add it when one does; the asymmetry is a decision, not an
oversight.

#### `apply-text-object`

```wit
apply-text-object: func(callback: u32, ctx: text-object-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<range, string>
```

OM.4b: a text object receives `borrow<document>` too — `text-object-context`
has always said "buffer text + the scope/comment env ride the `document`
handle", and AP.0.1 simply wired the action path first. Org's headline
and subtree objects are the first plugin ones, and resolving a subtree's
bounds means reading lines.

OT.1: and the tree, for the `apply-motion` reason above — org's `ir` / `ar`
resolve a subtree, which IS the `(section)` node. A text object gets the
tree rather than only the `scope-resolver` the native structural objects
use, because the resolver answers "what encloses this point" while a
plugin object needs to query the tree itself.

#### `parse-ex-args`

```wit
parse-ex-args: func(callback: u32, rest: string, bang: bool) -> result<args, string>
```


## `help`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `help-plugin` (imports), `project-plugin` (imports), `treesitter-context-plugin` (imports)

CR.3: plugin-contributed `:help` pages.

A plugin ships its own manual. The topic lands in the SAME registry the
builtin docs live in, so `:help <name>` opens it, `:help <Tab>` completes
it, markdown renders through the same pipeline, and `:describe-command`
can cross-link to it — with no host kind-branch anywhere.

#### The body ships INSIDE the component

A plugin's markdown is `include_str!`'d at build time and baked into its
own `.wasm`, exactly the way lattice's own docs are baked into the lattice
binary. Docs and code are then one artefact with one lifetime: unloading
the plugin removes its pages, and a plugin that failed to load has left
none behind.

This is deliberately NOT a runtime doc directory. That model (designed
2026-07-29, retired 2026-08-22 — see `contributable-registries.md` §4)
would need plugins to copy markdown into a shared directory at install
time, which separates the docs from the thing that owns them.

#### Data, not a callback

The body crosses ONCE, at registration, and the host keeps the string.
There is no `render-topic` export, because a help page does not change
between the moment the plugin loads and the moment someone reads it —
so nothing about the guest needs to stay alive to serve one. (Compare
`dashboard`, whose sections ARE functions of a live context and therefore
do keep a guest instantiated.)

#### Where it runs

Once per load, on the loader's off-boot-thread task. Never on the
keystroke or frame path, and never again after the load.

### Functions (1)

#### `register-topic`

```wit
register-topic: func(name: string, summary: string, body: string, related-commands: list<string>) -> result<_, string>
```

Register one free-form `:help` topic.

**Auto-namespaced**, like `config.register-option` and
`theme.register-element`: `name` is prefixed with the plugin's id, so a
plugin with id `fugitive` registering `status` contributes
`fugitive.status`. The host owns the namespace, so a plugin can neither
shadow a builtin page nor collide with another plugin.

**The single-page case keeps the bare id.** A `name` that is empty, or
that already equals the plugin's id, lands at the bare id — `:help
fugitive`, not `:help fugitive.fugitive`. A one-page plugin is the
common case and no editor's `:help` has ever looked like the latter.

`body` is markdown, rendered by the same help pipeline the builtin
docs use (tables, `[label](help:topic)` links, heading anchors).

`related-commands` are substring patterns matched against command
names; `:describe-command` walks them to emit a `See also` link, the
same way a builtin doc's frontmatter `related` list does.

`err` when the spec is malformed — never a trap, and never a
partially-registered topic. A rejected topic costs itself and nothing
else: the plugin's other pages still register.


## `host-services`

**Direction:** guest calls into the host through it · **Capability:** filesystem · **Worlds:** `completion-source-plugin` (imports), `context-plugin` (imports), `decorations-plugin` (imports), `events-plugin` (imports), `media-plugin` (imports), `multibuffer-view-plugin` (imports), `multiseam-fixture` (imports), `picker-source-plugin` (imports), `plugin` (imports), `project-plugin` (imports)

Guest→host services (plugin-host.md §5). Capability-gated calls a plugin
makes INTO the host, checked against its `CapabilityGrant` (PH7.2). Unlike
the guest's WASI filesystem view — sandboxed by the `Store`'s preopens —
these run host-side with full host authority, so each call re-checks the
grant itself (the host is not sandboxed). Errors cross as strings (§4
`result<_, string>` convention).

OC.5a adds `read-file` for a second, sharper reason: the guest's WASI view is
not reachable from every seam. See its doc comment — a grammar action that
reads a file through WASI panics rather than reading it, so a host-side read
is the only one that works on the dispatch thread.

PH7.4b lands the first seam: `walk`, the capability-gated workspace
enumeration the `fuzzy-finder` (PH7.4d) uses to replicate the native `files`
picker. The `net:http` / `proc:spawn` / tree-sitter seams follow (design.md
§15 Q15); the streaming `dir`-iterator shape (design.md §15, the deferred
streaming-result question) lands when a real streaming consumer (live-grep)
does — a bounded `walk` covers the fuzzy-finder.

### Uses

- `position` from `types`

### Functions (20)

#### `can-write-file`

```wit
can-write-file: func(path: string) -> result<_, string>
```

CD.3b: would an `effect.write-to-file` of `path` from this plugin land?

The same grant test the boundary applies to a returned write, and the
same checks the host's applier makes: the path is not a directory, its
directory exists, an existing file is readable UTF-8 and not read-only.
`ok` when all hold, otherwise `err` naming the first that failed.

For checking a destination **before** asking the user for anything —
capture checks its target when it opens, as emacs's
`org-capture-set-target-location` does, so a misconfigured target is
reported before a word is typed rather than at commit. A query: it
changes nothing, and a later write can still fail if the file changes
in between (a failed write stops the rest of its action's effects).

#### `clamp-position`

```wit
clamp-position: func(buffer: u32, at: position) -> option<position>
```

CD.6b: `at`, moved to the nearest position that exists in `buffer`
**now**; `none` when no buffer has that id.

A line past the end becomes the last line; a byte past its line's end
becomes that end, before the newline. Clamping only moves a position
backwards, so a range stays ordered. No text crosses.

For writing back into a buffer the guest last saw some time ago: a
capture records where it was started, and by the time it is filed the
caller may be shorter. `effect.apply-edit` refuses a position that is
not there, and says so only in the host log, so a guest that wants the
write to land clamps first, and a `none` tells it the buffer has closed
and there is nothing to write into.

#### `delete-file`

```wit
delete-file: func(path: string) -> result<_, string>
```

CD.3: delete a file — `read-file`'s peer, for the same reason.

A grammar action runs on the synchronous linker, where a guest's own
`std::fs::remove_file` goes through `wasmtime-wasi`'s sync shim and
takes the plugin down instead of deleting. Discarding a saved capture
draft is exactly such an action.

Gated on **`fs:write`** — a read grant is not enough — and re-checked
host-side, since the host runs with ambient authority. The check
canonicalizes the file itself when it exists, so a symlink inside the
grant pointing outside it is refused rather than followed. Only regular
files and symlinks are deleted; a directory is an `err`.

**A path with nothing there is `ok`**, as `store-delete` treats a
retraction that already happened: the caller wanted the file gone, and
it is. `err` for a denied path, a directory, or an OS failure, each
named.

#### `emit-event`

```wit
emit-event: func(name: string, payload: list<u8>)
```

Publish a plugin-defined event on the editor's event bus (PH7.8b). `name`
is the event identifier (typically pre-declared via `register-event`);
`payload` is opaque MessagePack the plugin owns — the host moves the bytes
onto the bus (`event::plugin`) and NEVER interprets them. Fire-and-forget:
the bus is observation-only (§5.10), so there is no reply. Subscribers
(native or other plugins) filter by `name` in their handler.

#### `excerpt-source`

```wit
excerpt-source: func(buffer: u64, line: u32) -> option<source-location>
```

OA.23: where a line of a MULTIBUFFER came from.

A multibuffer composes excerpts of other files, so a guest acting on a
row sees composed coordinates and cannot say which file it is looking
at. The agenda is the case that needs this: rewriting a headline in
place propagates through the excerpt, but writing a planning line BELOW
it targets a line the view does not contain — and `document.path()`
answers for the view, which is a synthetic buffer with no path at all.

`none` when `buffer` is not a multibuffer, when `line` falls outside
every excerpt (a header or a separator row is not source text), or when
the source buffer has no path. All three are ordinary answers rather
than errors: a guest asks about the cursor's line and the cursor can be
anywhere.

A plain buffer answers `none` too, not its own path. The question is
"which file does this COMPOSED line come from", and a guest that wants
the current file already has `document.path()`.

#### `local-utc-offset-seconds`

```wit
local-utc-offset-seconds: func() -> s32
```

The host's offset from UTC, in seconds, **at this instant** (OC.4).
East of Greenwich is positive: `+05:30` is `19800`, `-08:00` is `-28800`.

A guest cannot work this out. `wasi:clocks` is UTC, and the host builds
each plugin's `WasiCtxBuilder` with no environment inheritance — so there
is no `TZ` either, and `SystemTime::now()` in a component is UTC with no
way to know it. Org writes `CLOCK: [2026-08-28 Fri 16:02]` in **local**
time by definition, so without this every clock line, every `%U` / `%T` /
`%t` capture stamp and the agenda's "today" anchor is wrong by the user's
offset — and near midnight, wrong by a day.

**At this instant**, not a fixed configured number, so DST is simply
correct: the offset is resolved per call against the current time. A
rejected alternative was an `org.utc-offset` option, which makes the user
maintain what the OS already knows and is wrong twice a year.

Not capability-gated. It is a scalar the user's own clock displays, it
names no path and reaches no resource, and gating it would mean a plugin
with no filesystem grant renders timestamps in the wrong timezone.
`0` if the platform cannot answer — UTC, which is a legible wrong answer
rather than a fabricated one.

#### `new-uuid`

```wit
new-uuid: func() -> result<string, string>
```

A fresh random (v4) UUID, uppercase, in the canonical
`8-4-4-4-12` hyphenated form (OR.3).

**This is host-side for `read-file`'s exact reason.** `:org-roam-id-create`
mints an `:ID:` for the headline at point, and that is a *grammar action*:
it runs on the grammar seam's SYNCHRONOUS linker, where — as `read-file`'s
doc comment records — `wasmtime-wasi`'s sync shim blocks on a runtime
internally and panics on a thread already inside one. A guest minting its
own id through `wasi:random` would therefore work perfectly on the async
picker path and take the plugin down on the grammar path: correct in every
test that builds its own context, broken in the editor.

**Uppercase** because the reference corpus is uppercase throughout (macOS
`uuidgen`, which `org-id` shells out to). A consumer must still compare ids
case-INSENSITIVELY regardless — org is not consistent about case across
platforms, and a link that fails to resolve over letter case looks exactly
like a missing note, which is the worst way for this to fail.

Not capability-gated. It names no path, reaches no resource and reveals
nothing about the host; gating it would mean a plugin with no filesystem
grant cannot give its own records identities.

**`result`, not a degraded value**, and this is the one call here that
earns it. Its neighbours answer `0` when unwired (`wake-every`,
`local-utc-offset-seconds`) on the argument that a legible wrong answer
beats a fabricated one — but those values are READ. An id is WRITTEN,
into the user's own file, as an `:ID:` that outlives the session and
every other tool's view of that note. A guest handed an empty string on
entropy failure would write an empty drawer and nothing would ever say
so. One `match` at the call site buys that being impossible. `err` only
when the OS entropy source is unavailable, which is to say almost never.

#### `read-file`

```wit
read-file: func(path: string) -> result<string, string>
```

Read a UTF-8 file, capability-gated the same way `walk` is.

**This exists because the guest's own WASI filesystem view cannot serve
every seam.** The grammar seam is wired to a SEPARATE, synchronous linker
so the trampoline can call a guest action synchronously on the dispatch
thread — and `wasmtime-wasi`'s sync filesystem shim blocks on a runtime
internally, which panics on a thread already inside one. So a grammar
action calling `std::fs::read_to_string` does not read a file; it takes
the plugin down. Async seams (pickers, completion) are unaffected and may
keep using WASI directly.

Like `walk`, this runs host-side with ambient authority, so the grant is
re-checked here rather than relied on from the sandbox: `path` must lie
within one of the plugin's granted `fs:read` (or `fs:write`) prefixes.

`err` for a denied path, a missing file, or bytes that are not UTF-8 —
each with a message naming which, because "the read failed" tells a
plugin author nothing about whether to fix their manifest or their path.
A caller that treats absence as an ordinary case (a first capture into a
file that does not exist yet) checks for it rather than distinguishing.

#### `refresh-decorations`

```wit
refresh-decorations: func()
```

OA.30: say that this plugin's gutter decorations have changed, though
the document has not.

The host re-runs a `decorations` producer on two triggers, and both are
about things the HOST can see: the producer registry changed, or the
buffer's text version moved. A producer whose output depends on its own
view-local state changes neither — so its first answer is cached forever
and every later change paints nothing. The agenda's bulk marks are the
case that found this: a mark is guest state over an unchanged read-only
buffer, which is exactly the blind spot.

Call it after changing whatever the producer reads. The next tick
refetches; the producer still runs off the render path, and the renderer
still only ever reads the cache. This says "ask me again", it does not
run anything itself.

A REQUEST, not an apply — `refresh-view`'s shape, and for its reason: the
guest cannot reach the editor's tick. Cheap enough to call per keystroke
(one relaxed increment) and a no-op when nothing wired a counter, which
is the honest degradation everywhere else on this interface.

#### `register-event`

```wit
register-event: func(name: string, doc: string) -> bool
```

Declare a plugin-defined event (PH7.8b). Registers `name` + `doc` into
the host's RUNTIME event registry (`event_registry`) under this plugin's
provenance (`plugin:<id>`), so the event surfaces in introspection
(`:describe-event(s)`) and `:`-completion exactly like a built-in one.
Returns `false` (and registers nothing) if `name` would shadow a BUILT-IN
event — a plugin must not hijack a native event's subscribers. Idempotent
by name: a re-register refreshes the doc (a plugin reload).

#### `source-line`

```wit
source-line: func(buffer: u32, line: u32) -> option<string>
```

OA.23b: one line of a source document, without its trailing newline.

The read half of acting on an excerpt's source, and it takes a
`source-location.buffer` — not an arbitrary buffer id, which a guest
has no way to come by. `none` for anything no view owns, and for a
line past the source's last.

**The alternative is wrong twice.** `read-file` reads DISK, so it
misses edits the view has made and not yet saved — press the agenda's
`s` twice and the second read sees no `SCHEDULED:` line and stacks a
duplicate. It also reads a file the guest may not be editing at all,
per `source-location.buffer` above. The `document` resource is no help
either: it is the guest's OWN buffer, and the line in question is one
the view does not compose.

#### `store-delete`

```wit
store-delete: func(key: string) -> result<_, string>
```

Forget `key`. Deleting a key that is not there is `ok` — a retraction
that has already happened is not an error.

#### `store-generation`

```wit
store-generation: func() -> u64
```

Bumped on every successful mutation, never on a read. A reader compares
it against what it last built from and rebuilds only when it moved.

This is what makes one-writer/many-readers work across separate `Store`s
(see the block comment above): the number is host-side, so a reader
instance sees the writer instance's bump without sharing memory with it.
`0` for a plugin with no store.

#### `store-get`

```wit
store-get: func(key: string) -> option<list<u8>>
```

The bytes stored under `key`, or `none` when nothing is stored there.

`none` also covers every degraded case (no grant, no data dir, a store
discarded as corrupt). A reader for whom absence is ordinary — a first
index that has not run yet — cannot distinguish them, and does not need
to: the answer to all four is "build it".

#### `store-keys`

```wit
store-keys: func(prefix: string) -> list<string>
```

Keys carrying `prefix`, sorted. `""` lists everything.

#### `store-put`

```wit
store-put: func(key: string, value: list<u8>) -> result<_, string>
```

---------------------------------------------------------------------
OR.1 — durable, plugin-scoped key/value storage.

Scoped to the plugin's own data dir **by manifest id**, so every seam
instance of one plugin sees ONE store and two plugins cannot collide.
That scoping is the whole point rather than an implementation note:
`spawn_event_plugin`, `spawn_config_plugin` and
`instantiate_grammar_plugin` build SEPARATE `wasmtime::Store`s with
separate guest memory, so "keep it in guest state" means N copies
drifting — and the drift is invisible, because each instance stays
internally consistent while answering a different question.

**The host stores bytes under strings and never interprets either.**
Keys are guest-chosen strings, NOT paths: nothing derives a path from a
key (the store encodes them into its own layout), so there is no
traversal to defend against and no path sanitiser to keep correct.

These are five functions on `host-services` rather than a `store`
interface of their own, and that is deliberate. A component's import set
is fixed for the whole artefact and must resolve on EVERY linker it is
instantiated against — including the grammar seam's sync one. A new
interface is a new import each world must declare and both linkers must
wire, and a miss there does not degrade one seam: it fails the WHOLE
component at instantiation (OC.2 did exactly this with one `logging`
call). `host-services` is already imported by every world that wants a
store and already wired on both linkers, so putting them here makes the
half-wiring structurally impossible instead of merely tested for.

Capability-gated on `state:write`. A plugin without the grant gets `err`
from `store-put` / `store-delete`, `none` from `store-get` and an empty
list from `store-keys` — the honest "no store wired" degradation the
`config_registry` and `event_emit` seams already use, never a panic.
---------------------------------------------------------------------
Persist `value` under `key`. `err` names why — no grant, no data dir,
a value larger than the whole store may hold, or a write that failed.

#### `unwatch`

```wit
unwatch: func(path: string) -> result<_, string>
```

Stop watching `path`. Unwatching a path that is not watched is `ok` — a
disarm is idempotent, because the alternative is a guest that must track
host state to avoid an error.

#### `view-args`

```wit
view-args: func(buffer: u64) -> list<string>
```

OA.27: the scan arguments the provider view in `buffer` is showing.

**A view's arguments are HOST state, and this is the only way a guest
can read them back.** A scan view is opened with `scan-args` the host
routes verbatim and then keeps (`gr` re-scans with them), so they are
the whole of what the view is displaying: which command, which span,
which day, which filters. A chord that changes one of them is "re-open
this view with one argument different", and that requires reading the
other arguments first.

**Why the guest cannot just remember them.** It has nowhere to. The
arguments arrive on the `scanned-excerpt-source` seam (`begin`) and the
chord runs on the grammar seam, and those are separate
`wasmtime::Store`s with separate linear memory — the same N-copies drift
the store functions below document. A guest that parked them in a
`thread_local` reads a DEFAULT view on every chord: each key looks right
in isolation (setting a span works, adding a filter works) while
anything that has to read prior state silently starts over. That is the
bug this exists to make unrepresentable, and org shipped it.

An empty list for a buffer that is not a provider view, for a view the
host has no state for, and when nothing wired a resolver. All three are
ordinary: a guest asks about the buffer its chord fired in, and a chord
can fire anywhere. An empty list parses as "no arguments", which is what
a fresh view has.

#### `walk`

```wit
walk: func(root: string) -> result<list<string>, string>
```

Recursively enumerate files under `root`, returning absolute UTF-8 paths.
Host-side policy mirrors the native file picker (`walk_files_for_picker`):
a bounded entry count, skipping `.git`/`target`/`node_modules`/`dist`/
`.cache` and dotfiles. A non-UTF-8 path is skipped (it cannot cross as a
`string`), never an error — one oddly-named file must not fail the walk.

Capability-gated: `root` must lie within one of the plugin's granted
`fs:read` (or `fs:write`) prefixes, else `err` — a plugin with no fs
grant reaches nothing. The check runs host-side because the host, unlike
the guest's WASI view, has ambient authority the grant must bound.

#### `watch`

```wit
watch: func(path: string) -> result<_, string>
```

---------------------------------------------------------------------
OR.2 — a plugin can be told a file changed.

A **watcher, not a save hook**, because the corpus a plugin indexes is
edited from outside lattice: emacs writes a note, a `git pull` lands
twenty, a sync daemon rewrites a directory. A save hook observes none of
those, and the symptom — an index missing files you know you wrote —
reads as data loss rather than as a stale cache.

Delivery is the `files-changed` arm of `event`, through the same
`events.subscribe` a plugin already uses. It is **addressed**: the host
routes a batch only to the plugin that armed the watch, so a plugin
granted `fs:read` over one directory never learns what changed under
another plugin's. Because delivery rides the plugin's own event actor,
it reaches the guest on that actor's task — no keystroke required, which
is the failure mode ("it works, but only after I hit something") this
seam is most likely to have.

Gated on the same `fs:read` (or `fs:write`) grant `walk` and `read-file`
check — a watch reveals filesystem activity, so it is the same
authorization question, answered by the same line.
---------------------------------------------------------------------
Watch `path` (a directory, recursively) for changes. Bursts are
coalesced host-side behind a quiet window, so a `git pull` rewriting two
hundred files delivers one event carrying two hundred paths rather than
two hundred events.

Watching the same path twice is `ok` and arms nothing new. The watch
lives as long as this plugin instance: it is torn down when the instance
is unloaded or quarantined, with no bookkeeping on the guest's part.

`err` names which of the four refusals happened — outside the grant, no
event bus wired on this seam, an unwatchable path, or a watcher the
platform refused to create. A plugin whose watch fails should fall back
to indexing on boot plus an explicit resync command, which is degraded
and honest rather than appearing to work and going stale.

### Types (1)

#### record `source-location`

```wit
record source-location {
    path: string,
    line: u32,
    buffer: u32,
}
```

OA.23: a file and a 0-based line in it — where a composed line came
from.

**Fields**

- `path`: `string`
- `line`: `u32`
- `buffer`: `u32` — OA.23b: the source DOCUMENT's buffer id — what to act on.

  Not interchangeable with `path`, and the difference costs data if
  it is treated as though it were. A multibuffer's sources are
  documents the VIEW owns; no buffer store holds them, and the
  editor may separately have the user's own buffer open on the same
  file. A guest that resolved the path and then wrote to the file by
  name would be editing the other document, and the view's `:w`
  could later overwrite one with the other.

  So: `path` to SHOW the user which file a row came from, `buffer`
  to EDIT it — `effect.apply-edit` takes exactly this id, and
  `source-line` reads by it.


## `keymap`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `keymap-plugin` (imports)

The `keymap` guest→host binding-registration seam (PL8.D.1).

Mirrors the native `KeymapHandle` write path. The first (and canonical)
consumer is the user's `init.rs`: plain global keybinds — the one config kind
with no other seam — register here. A binding names an EXISTING command (by
name, resolved against the `CommandRegistry`) and lands in
[`KeymapLayer::User`], gated by `KeymapCapability::User` — above the built-in
vim grammar, never in `KeymapLayer::Builtin` (the standing keymap-ownership
rule; user config layers on top).

Registration-only: the guest declares bindings once (at `register-keymap`);
binding *resolution* on every keystroke stays native (`KeymapHandle` trie
lookup) — no per-keystroke WASM. So this rides the async linker like `config`
/ `events`, not the sync grammar linker.

This is the CANONICAL, language-agnostic keybinding API — any component-model
language calls `register-binding` directly.

### Functions (1)

#### `register-binding`

```wit
register-binding: func(binding-mode: binding-mode, chord: string, command: string) -> bool
```

Bind `chord` in `binding-mode` to an EXISTING command named `command`
(resolved against the `CommandRegistry` at registration), landing in
`KeymapLayer::User`. `chord` is a vim-notation chord sequence (`<leader>f`,
`<C-s>`, `gd`). Returns `false` (binding nothing) if the chord is
unparseable, the command is unregistered, or the User-layer capability was
withheld — a plugin never silently mis-binds. The keystroke path is
unaffected until the binding lands.

### Types (1)

#### enum `binding-mode`

```wit
enum binding-mode {
    normal,
    insert,
    visual,
    select,
    replace,
    command,
    search,
}
```

The vim binding mode a keybinding lives in — the plugin-facing subset of
the native `BindingMode` (the transient operator-pending / after-key
states are internal grammar states, not plugin-bindable). Matches the
`modes` seam's `binding-mode` (the same native mapping).


## `language`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `language-plugin` (imports)

LG.3c: plugin-contributed languages.

A plugin ships a tree-sitter grammar compiled to WebAssembly and the
queries that go with it. The language lands in the SAME registry the
bundled ones live in, so `Lang::detect_from_path` selects it by extension,
`:describe-buffer` names it, highlighting, folding, indenting and
incremental reparse all run through the ordinary paths — with no host
kind-branch anywhere.

#### The host still owns the parse loop

The guest ships the grammar; it does not run it. `WasmStore::load_language`
turns the bytes into an ordinary `tree_sitter::Language`, and from that
point nothing downstream can tell where the grammar came from. There is no
guest call on the keystroke path at all — the plugin is consulted once, at
load.

This preserves the rejection recorded in `plugin-treesitter-seam.md` §9: a
text-only seam where the guest re-parses would duplicate the host's live
incremental tree. What changes here is only where the grammar comes from,
never who runs it.

#### Data, not a callback

A language is a static description. Nothing about the guest needs to be
alive once the bytes and query sources are across, so the store is dropped
when registration returns — the `help` seam's shape and reasoning, not
`dashboard`'s live sections.

#### Where it runs

Once per load, on the loader's off-boot-thread task. Compiling a grammar
costs ~100 ms (Cranelift), which is exactly why it happens here and once,
rather than on first open of a matching file.

### Functions (1)

#### `register-language`

```wit
register-language: func(spec: language-spec) -> result<_, string>
```

Register one language.

`err` when the grammar fails to load or a query fails to compile,
carrying a reason that names the language and the offending query.
Never a trap, and never a half-registered language: a rejected
language costs itself and nothing else, so the plugin's other
contributions — including its other languages — still register and the
load still succeeds.

### Types (2)

#### record `conceal-rule`

```wit
record conceal-rule {
    pattern: string,
    hide: list<u32>,
    slot: option<string>,
}
```

H.2: one display-time elision rule.

The host compiles the pattern once at registration and matches it
against each display line during a matrix rebuild — never per frame,
and never for a language that declares none. See
`docs/dev/architecture/conceal.md`.

The engine is RE2-style and cannot backtrack. That is a property, not
an implementation note: these patterns come from a plugin and run over
every rebuilt line, so an engine with a pathological input would be a
plugin's ability to freeze the renderer. Lookaround and backreferences
are therefore unavailable, and a pattern using them is refused at
registration rather than being slow later.

**Fields**

- `pattern`: `string` — Regex matched against a single line. Anchoring is the rule's
  business; the host adds none.
- `hide`: `list<u32>` — 1-based capture-group indices whose spans are hidden.

  Group 0 (the whole match) is REFUSED — a rule that hides its
  entire match is a deletion rather than a concealment, and is
  almost always a pattern that forgot its capture parentheses.
  An index the pattern has no group for is refused too, at
  registration, because checking it per line would log at
  rebuild rate.
- `slot`: `option<string>` — OL.1: a capture or theme-element NAME for what stays VISIBLE in
  each match. `none` — every rule before this — conceals only.

  Per-RULE, because conceal is a general mechanism and most rules
  hide punctuation that means nothing on its own. Only a rule whose
  REMAINDER is a thing — an org link's description — declares a
  style, so adding a conceal rule for something else cannot
  accidentally paint it.

  "What survives concealment" is the styling unit rather than a named
  group, and that is what lets one declaration serve both org link
  forms: `[[t][d]]` hides the brackets and target and styles `d`;
  `[[t]]` hides the brackets and styles `t`. The mechanism that
  decides where a link IS also decides what to paint — two mechanisms
  could disagree about that, and a link that renders as prose is a
  link nobody can see is under the cursor.

  Resolved at REGISTRATION, through the same path a `highlights.scm`
  capture name takes, so a concealed link follows a colourscheme swap
  exactly as a heading does. A name that resolves to nothing conceals
  without painting rather than failing the rule.

#### record `language-spec`

```wit
record language-spec {
    name: string,
    extensions: list<string>,
    grammar-name: option<string>,
    grammar: list<u8>,
    highlights: option<string>,
    folds: option<string>,
    injections: option<string>,
    indents: option<string>,
    textobjects: option<string>,
    conceal-rules: list<conceal-rule>,
}
```

Everything the host needs to make a language real.

**Fields**

- `name`: `string` — The grammar's tree-sitter name — `org` for `tree_sitter_org`.
  Becomes the language id `:set filetype` and `:describe-buffer`
  report, and the key its queries are cached under. MUST match the
  grammar's exported entry point or the module loads with nothing
  to call.

  A name that collides with a bundled language is REFUSED, not
  shadowed: a plugin silently replacing `rust` would be a miserable
  thing to debug. Claiming a bundled *extension* is allowed but never
  wins, because the native table is consulted first.
- `extensions`: `list<string>` — `["org"]` — extensions that select this language, matched
  case-insensitively, with or without a leading dot. A language with
  none can never be selected and is refused as a manifest mistake.
- `grammar-name`: `option<string>` — The grammar's own entry-point name, when it differs from `name`.

  `load_language` finds a grammar by its `tree_sitter_<x>` export, and
  that is not always what users call the language. lattice's own
  bundled `sql` is the case in point: the grammar is
  `tree-sitter-sequel` and exports `tree_sitter_sequel`, while the
  language everyone types is `sql`. Absent means "same as `name`",
  which is the common case.
- `grammar`: `list<u8>` — The grammar, compiled to wasm. `tree-sitter build --wasm` produces
  one; so does `scripts/build-wasm-grammar.sh` with nothing but clang
  and a rustup toolchain.
- `highlights`: `option<string>` — Tree-sitter queries, as source. Absent means that feature is simply
  unavailable for this language — never an error.

  All of these compile at REGISTRATION, not first use. A malformed
  query is the plugin author's mistake and must surface at load with
  the offending query named; compiling lazily would turn a typo in
  `folds.scm` into "folding silently does nothing", which is
  indistinguishable from the feature not existing and surfaces days
  later.
- `folds`: `option<string>`
- `injections`: `option<string>`
- `indents`: `option<string>`
- `textobjects`: `option<string>`
- `conceal-rules`: `list<conceal-rule>` — H.2: what to hide when rendering this language, empty for
  most languages.

  **A refused rule is dropped, not fatal** — deliberately unlike a
  malformed query above, which rejects the whole language. The
  asymmetry is the proportionality: a broken `folds.scm` means the
  language cannot fold at all, and silence there is
  indistinguishable from the feature not existing; a broken conceal
  rule means one pattern does not hide, the others are unaffected,
  and the language is otherwise entirely usable. Losing a language
  over a typo in a cosmetic regex would cost far more than it
  protects.


## `logging`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `completion-source-plugin` (imports), `config-plugin` (imports), `context-plugin` (imports), `dashboard-plugin` (imports), `decorations-plugin` (imports), `error-parser-plugin` (imports), `events-plugin` (imports), `help-plugin` (imports), `keymap-plugin` (imports), `language-plugin` (imports), `media-plugin` (imports), `modes-plugin` (imports), `multibuffer-view-plugin` (imports), `picker-source-plugin` (imports), `plugin` (imports), `plugin-manager-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `sign-plugin` (imports), `theme-plugin` (imports), `transient-source-plugin` (imports)

Guest→host structured logging (plugin observability Layer 2, design
`docs/dev/architecture/plugin-observability.md` §8). Shaped like
`wasi:logging/logging` so any component-model language calls it with no
lattice-specific glue: a guest emits its OWN narrative ("parsing X",
"reindexed 40 files") and the host routes each call into the same
`PluginTracer` that carries the boundary trace (Layer 1), tagged by plugin +
level, so the guest's intent interleaves with the host's observed behaviour in
one `*plugin-trace*` buffer.

Language-agnostic and off the hot path: `logging` is an async-linker import
(never wired into the sync grammar seam), so it cannot touch the keystroke
path. `context` is a free-form category the guest chooses (e.g. a subsystem
name); an empty string is fine. Fire-and-forget — no reply, the host cannot
fail the call.

### Functions (1)

#### `log`

```wit
log: func(level: level, context: string, message: string)
```

Emit one log line. `level` gates it against the plugin's trace verbosity
(the same per-plugin gate the boundary trace uses); `context` is a
free-form category; `message` is the line. Dropped silently when the
plugin's gate is below `level` — exactly like a boundary-trace record.

### Types (1)

#### enum `level`

```wit
enum level {
    trace,
    debug,
    info,
    warn,
    error,
    critical,
}
```

Severity, mirroring `wasi:logging`. `critical` folds into the host's
`error` trace level (the tracer has no separate critical tier); `off` is
not a log level (it is a gate-only value on the host side).


## `media`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `media-plugin` (exports)

The inline-media **producer** API (IM.6, `inline-media.md` §7).

A guest tells the host "there is an image at line N, here is its path".
The host resolves the file's intrinsic size, decides how many display rows
it reserves, builds the virtual rows and — on a peer that draws pixels —
decodes and paints it.

**Producer, not per-frame**, exactly like `decorations` (PH7.9). The host
calls this on a trigger (buffer opened, edited, option changed) and caches
the result per buffer; the renderer reads the cache. A guest called on the
render path would be a paramount-#1 violation.

**The guest names a file; it never sends pixels.** Three consequences, all
deliberate: no decoded image is copied across the boundary per load; the
`fs:read` capability decision stays with the HOST, which is what stops a
plugin putting arbitrary bytes on screen regardless of its grant; and
`(path, mtime, size)` remains a usable cache key.

**The guest does not choose a size.** There is no row count or pixel
dimension in `media-block`. The host owns that, so sizing policy lives in
one place and a plugin cannot reserve arbitrary vertical space in a buffer
it does not own.

### Uses

- `decoration-context` from `types`
- `media-block` from `types`

### Functions (1)

#### `media-blocks`

```wit
media-blocks: func(ctx: decoration-context, text: string) -> result<list<media-block>, string>
```

Produce the media blocks for a buffer.

`ctx` is the owned projection (buffer id / path / line count); `text` is
the buffer's contents.

**Text, not a `borrow<document>` handle**, and that is the opposite of
what `grammar.apply-action` does — deliberately, because the access
pattern is the opposite. An action reads a handful of lines near the
cursor, where a handle costs a few crossings and a bulk copy would waste
the rest. A media scan reads EVERY line, so a handle costs one crossing
per line — ten thousand for a large org file — where one copy costs one.

The copy is affordable because this is a producer: it runs on open and
on edit, not per frame.

Async — a produce call suspends the guest, never the render path. An
`err` is logged and the buffer keeps its PRIOR blocks for this trigger
rather than losing them, so a transient failure mid-edit does not make
every image in the document blink out.


## `modes`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `init-fixture` (imports), `modes-plugin` (imports), `multiseam-fixture` (imports), `project-plugin` (imports), `treesitter-context-plugin` (imports)

Mirrors the `Mode` trait declaration surface + `ModeRegistry` (lattice-mode).
The guest declares a minor mode as DATA (id + kind + activation policy +
capability requirements); the host builds a marker `Mode` impl (`PluginMode`,
the `EmacsKeysMode` template) and registers it into the SAME `ModeRegistry`
builtins use, so `:describe-mode` / mode introspection treat it uniformly.

PH7.11a lands the declaration + registration path (this file); keymap bindings
(chord→command-name at the mode's OWN layer, the `KeymapCapability`
write-gate) are PH7.11b. **OM.2 lands major modes** — a plugin that
contributes a language contributes its major too, which is the only way a
plugin language can have one (`Lang::Plugin(_)` has no arm in the host's
hand-written table). **MO.1 lands typed option-overrides** — the last part
of its surface a plugin mode could not own. Lifecycle callbacks /
decorations / bundled modes-as-components remain Phase 8.

The CANONICAL, language-agnostic surface — any component-model language calls
`register-mode` directly (see the WIT-canonical principle).

### Functions (3)

#### `disable-mode`

```wit
disable-mode: func(id: string)
```

Disable a registered minor mode globally (CI.4) — the inverse of
`enable-mode`; the Editor deactivates it on open buffers.

#### `enable-mode`

```wit
enable-mode: func(id: string)
```

Enable a registered minor mode globally (CI.4) — the user-enablement path
(config-and-init.md §6). A plugin minor mode is registered
available-but-off; an `init.rs` `on-plugin-loaded` handler calls this to
turn it on. The host publishes a `mode-enablement-requested` signal and
the Editor flips the enablement + re-activates open buffers (this call is
the request, not the apply — the guest can't reach the activator). A
no-bus / unknown-id case is a `warn` + drop (graceful), never a trap.

#### `register-mode`

```wit
register-mode: func(decl: mode-declaration)
```

Declare a mode. Records the declaration; the host builds a `PluginMode`
and registers it into the `ModeRegistry` after `register-modes` returns
(the `register-grammar` drain precedent — registration needs `&mut
ModeRegistry`, not a live handle). Registration failures (bad `-mode`
suffix, id collision, `major` kind) are logged + skipped at drain.

### Types (8)

#### enum `mode-kind`

```wit
enum mode-kind {
    major,
    minor,
}
```

Major (content-type identity) vs minor (orthogonal behavior). Both
register (OM.2); a buffer has exactly one major and any number of
minors. A major's keymap lands at `KeymapLayer::MajorMode(id)`, below
active minors and above the built-in vim grammar — so a minor can
refine what its major bound, and neither can shadow the grammar
globally.

#### variant `activation-policy`

```wit
variant activation-policy {
    manual,
    global,
    universal,
    majors(list<string>),
}
```

Which buffers a mode auto-activates on — mirrors `ActivationPolicy`.
`manual` = only on explicit activation; `global` = document buffers only;
`universal` = every buffer; `majors` = an allowlist of major-mode ids.

#### flags `mode-capabilities`

```wit
flags mode-capabilities {
    buffer-uri,
    lsp,
    tree-sitter,
    folds,
    writable,
    diagnostics,
}
```

Capability requirements a mode declares — mirrors the `CapabilitySet`
bitflags (lattice-mode). Enforcement stays the native mode-activation
path; the declaration sizes the requirement honestly (fragment §6).

#### enum `binding-mode`

```wit
enum binding-mode {
    normal,
    insert,
    visual,
    select,
    replace,
    command,
    search,
}
```

The vim binding mode a keymap entry lives in — the plugin-facing subset of
the native `BindingMode` (the transient operator-pending / after-key states
are internal grammar states, not plugin-bindable).

#### record `mode-keymap-binding`

```wit
record mode-keymap-binding {
    binding-mode: binding-mode,
    chord: string,
    command: string,
}
```

One keymap binding a mode contributes (PH7.11b): bind `chord` in
`binding-mode` to an EXISTING command named `command`, resolved against the
`CommandRegistry` at registration. `command` is any registered command name
— a built-in (`ex:write`), a host action (`action:split-pane-horizontal`),
or the plugin's OWN grammar contribution (PH7.7 `register-action`). An
unparseable chord or unknown command skips that one binding (logged).

#### enum `override-priority`

```wit
enum override-priority {
    low,
    normal,
    high,
}
```

Where a mode's option override sits in the resolver — mirrors the native
`OverridePriority`. `normal` is what a mode should almost always use;
within a layer the last-activated `normal` wins and the host fires a
`ModeEvent::OptionConflict`. `high` and `low` exist for a mode that
genuinely must out-rank or yield to its peers, and reaching for either to
win an argument with another mode is how a conflict becomes invisible
instead of reported.

#### record `mode-option-override`

```wit
record mode-option-override {
    name: string,
    value: string,
    priority: override-priority,
}
```

MO.1: one option a mode sets for its own buffers — the plugin-facing form
of a native mode's `options()` set.

`name` is the option's registered name (`foldmethod`), resolved host-side
against the SAME `ConfigRegistry` `:set` uses. `value` is its value in
exactly the spelling `:set name=value` accepts, coerced host-side through
the same parser and validator — so a mode declares an override in the
vocabulary the user already knows, and does not get a private settings
channel that could disagree with `:set` about what a value means.

**This is a LAYER, not a write.** It changes what the option resolves to
in this mode's buffers; it does not alter the user's global setting, and
it does not outrank a buffer-local `:setlocal`.

**An override the host cannot resolve is skipped, warned, and named** —
the rest of the set still applies, because one bad entry must not cost a
mode its other options. Two things do not resolve: an option name nothing
registered, and a value the option's own validator rejects. A third is
worth stating because it will surprise: an option the PLUGIN ITSELF
registered through the `config` seam has no native type identity, so it
cannot be overridden here yet. That is a known hole with a known fix
(`plugin-mode-options.md` §3c) and no consumer yet.

#### record `mode-declaration`

```wit
record mode-declaration {
    id: string,
    kind: mode-kind,
    activation-policy: activation-policy,
    capabilities: mode-capabilities,
    keymap: list<mode-keymap-binding>,
    target-language: option<string>,
    options: list<mode-option-override>,
}
```

A mode declaration. `id` must carry the conventional `-mode` suffix (the
`ModeRegistry::register` gate, e.g. `git-blame-mode`); a bare or
mis-suffixed id is rejected at registration. `keymap` bindings land at
`KeymapLayer::MinorMode(id)` gated by `KeymapCapability::OwnedLayer{id}` —
a plugin mode can write ONLY its own layer (PH7.11b write-gate).

**Fields**

- `id`: `string`
- `kind`: `mode-kind`
- `activation-policy`: `activation-policy`
- `capabilities`: `mode-capabilities`
- `keymap`: `list<mode-keymap-binding>`
- `target-language`: `option<string>` — OM.2: for a MAJOR, the language this mode is the default major for,
  by canonical name (`"org"`) — the name the plugin's `language` seam
  registered. The host indexes it (`ModeRegistry::find_major_for_lang`)
  and a document of that language activates this mode, the same path a
  built-in language major takes.

  Ignored (with a warning) on a `minor`: a buffer has exactly one
  major, and indexing a minor here would install it AS the major. A
  minor that wants to ride a major names it in
  `activation-policy.majors` instead.

  `none` on a major means manual activation only — it is not the
  default for any language.

  A language the host resolves through its own table (`rust`,
  `markdown`, …) is NOT claimable: the built-in table is consulted
  first, so a claim on one is inert rather than a hijack.
- `options`: `list<mode-option-override>` — MO.1: options this mode sets for its own buffers.

  Until this existed a plugin mode owned its keymap, its handlers and
  its lifecycle but NOT the options deciding how its buffers actually
  behave — whether they are writable, whether they wrap, how they fold.
  That was a hole in the mode-ownership rule, and org was standing in
  it: org folding worked only because the user happened to set
  `foldmethod` globally, making it correct by coincidence on one
  machine and wrong everywhere else.

  Empty is the normal case and costs nothing.


## `multibuffer-view-registry`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `events-plugin` (imports), `multibuffer-view-plugin` (imports)

MV.1 — the seam by which a plugin **owns a multibuffer view**.

Design: `docs/dev/architecture/plugin-multibuffer-views.md`.

#### What was missing

A plugin could already own a view's *interactions* — `scanned-excerpt-source`
exports `view-mode`, and the host activates that minor on the view, so
`org-agenda-mode`'s chords and their handler bodies live in org. It could
already *open* a view: `app-effect::open-provider-view(provider, args)` is
ungated, on the `open-picker` precedent.

What it could not do is have a view at all. `ProviderViewOpener` is
`Arc<dyn Fn(&mut dyn ModeActivator, &Args)>` — a Rust closure — so a view
existed only if the host had hand-built a provider for it. The agenda is the
one that got built. Org's second view had nowhere to go, and neither did any
third-party plugin's first: the acid test `multibuffer-views.md` sets ("a new
provider should require zero host additions") failed outright for plugins,
which cannot add host code at all.

#### The registry shape, not the one-view-per-component shape

A guest calls `register-multibuffer-view` once per view it owns, exactly as
`picker-registry` works and for the reason OR.5b records: every other
contribution seam in the system is "the guest calls a host import to register
N things", and the one seam shaped "the component IS one source" had to be
changed the moment a plugin wanted two.

### Uses

- `multibuffer-view-spec` from `types`

### Functions (2)

#### `refresh-view`

```wit
refresh-view: func(view: string, args: list<string>)
```

OA.15a: re-open one of THIS guest's views with `args`, from somewhere
that cannot return an effect.

###### Why this is not `open-provider-view`

`app-effect::open-provider-view` already says "open my view", and every
trigger that RETURNS an effect should keep using it — an action handler,
an ex-command, a transient row. This exists for the producers that do
not return anything at all: `on-event` is `func(handler, ev)` with no
result by construction (the event seam is observation-shaped, §5.10),
and a wake handler is the same.

###### What made the gap visible

A guest MODE that is supposed to change its view. The host delivers
`minor-activated` / `minor-deactivated`, so a guest can see its mode go
on and off — but a plugin mode is DATA (`mode-declaration`), and the
host builds it into a `PluginMode` whose `on_activate` is a no-op. A
native mode supplies that body itself: `scan-view-clockreport-mode`
registers its provider on activation and drops it on deactivation,
which is what makes the mode the single switch rather than a label
beside one. Without this call a guest mode has no equivalent, so
`org-agenda-log-mode` could be toggled and change nothing.

###### Contract

A REQUEST, not an apply — `enable-mode`'s shape (`modes.wit`), and for
the same reason: the activator is `&mut`-backed and the guest cannot
reach it. The host publishes it and the Editor re-opens on its next
tick, with the wake baked in so the result reaches the screen WITHOUT a
keystroke.

`view` must name a view this guest registered; the host refuses an
unknown name with a warning rather than trapping, because a stale name
after a reload is a plugin-author mistake, not a reason to kill a
running plugin. `args` are routed verbatim and never read — the same
contract `scan-args` carries, since they are the provider's own
vocabulary.

A view declared `reuse: true` (the agenda) re-scans in place; one
declared `reuse: false` opens another view, so a guest calling this on
a non-reuse view from a handler that fires often will accumulate views.
That is the guest's choice to make and the host does not second-guess
it.

#### `register-multibuffer-view`

```wit
register-multibuffer-view: func(spec: multibuffer-view-spec)
```

Declare one view. Called from the guest's `register-multibuffer-views`
export. The host registers a provider-view opener under `spec.id`, so
`open-provider-view` and the view's `gr` both reach it.

An id already claimed by a NATIVE provider is refused with a warning
naming both, and this guest's other views still register — one bad name
must not cost a plugin its whole contribution.


## `multibuffer-view-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `multibuffer-view-plugin` (exports)

### Uses

- `multibuffer-view-result` from `types`

### Functions (1)

#### `build`

```wit
build: func(view: string, args: list<string>) -> result<multibuffer-view-result, string>
```

Produce a `pull` view's excerpts, **in final order**.

`view` names which of this guest's registered views is being built; one
actor and one guest instance serve them all, as with `picker-source`.
`args` are the trigger's arguments verbatim — from the ex-command, the
transient row, or the `gr` that refreshed the view.

###### Why the guest orders, when a scan source only supplies a sort key

The asymmetry is deliberate and it turns on **who can see the whole set
at ordering time**. A scan guest is handed one file and cannot know
where its rows land once every other file's rows interleave, so only the
host can sort and the guest supplies an `s64` key. A pull guest computes
the entire set in this one call, so requiring a key would make the host
re-sort what is already ordered — and would force orderings that are not
numeric (by title, by file-then-line) through an integer that cannot
express them.

An `err` **declines** the view with the guest's own message rather than
opening an empty one. Declining is a first-class outcome: an empty view
leaves the user to guess whether it is broken or genuinely empty.


## `picker-registry`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `picker-source-plugin` (imports), `project-plugin` (imports)

Mirrors `PickerSourceGenerator` (lattice-picker/src/source.rs:294). A WASM
picker source *exports* this interface; the host wraps its exports as an
`Arc<dyn PickerSourceGenerator>` (PH7.4c.2) and registers it through the
`SubsystemBoot` install seam → `PickerRegistry::register_generator`, so a
plugin source is indistinguishable from a first-party one at the registry.
The ⭐ Phase-7-exit interface; validated by `plugins/fuzzy-finder` (PH7.4d).
OR.5b — the host import a picker plugin registers its sources through.

**Why this is an import and not an export.** Before OR.5b the seam was
shaped "the component IS one picker source": it exported `spec()`, and the
host registered exactly one source per component. That made picker-source
the only contribution seam in the system shaped that way — `language`,
`grammar`, `config`, `modes`, `theme`, `help` and `keymap` are all "the
guest calls a host import to register N things" — and the exception was not
free. Org needs three pickers (refile, roam find-node, roam insert-node) and
could register one.

So this matches the rest: the host calls `register-picker-sources` once, the
guest calls `register-picker-source` for each, and `init` / `accept` take the
source id so one actor serves them all.

### Uses

- `picker-source-spec` from `types`

### Functions (1)

#### `register-picker-source`

```wit
register-picker-source: func(spec: picker-source-spec)
```

Declare one picker source. Called from the guest's
`register-picker-sources` export; the host registers each into the same
`PickerRegistry` a first-party source lives in.

A second registration under an id this plugin already used replaces it —
a plugin reload, not a collision. Two DIFFERENT plugins claiming one id
is resolved the way the registry has always resolved it: last write
wins, and the teardown token unregisters by id.


## `picker-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `picker-source-plugin` (exports), `project-plugin` (exports)

### Uses

- `raw-candidate` from `types`
- `routing-payload` from `types`
- `picker-context` from `types`
- `picker-accept-outcome` from `types`

### Functions (2)

#### `accept`

```wit
accept: func(source: string, ctx: picker-context, routing: routing-payload) -> result<picker-accept-outcome, string>
```

Translate the user's chosen `routing` token into a typed
`PickerAcceptOutcome` the host applies. A mismatch is an `err` (echoed).

#### `init`

```wit
init: func(source: string, ctx: picker-context, args: list<string>) -> result<list<candidate-pair>, string>
```

Build the candidate set for `:picker <id> <args>`. `ctx` is the owned
`PickerContext` projection (§4.2). Returns the `(candidate, routing)`
pairs; an `err` string is echoed and the picker stays closed. (One-shot
list; the incremental `Stream` shape — the deferred §15 streaming
question — lands with a live source.)

NB: the active buffer's bulk **text** rides a `borrow<document>` handle
(PH7.3c `DocumentResource`) that a text-reading source (`:picker lines`)
needs — deferred here (the `fuzzy-finder`/`files` exit reads no buffer
text, only walks the fs via `host-services`). Passing a host-owned
resource into a guest *export* has a bindgen-modeling subtlety to resolve;
tracked as a focused follow-up (see the slice plan).

`source` names WHICH of this plugin's registered sources is being built
— one component may register several (see `picker-registry`), and they
share one actor and one guest instance.

### Types (1)

#### record `candidate-pair`

```wit
record candidate-pair {
    candidate: raw-candidate,
    routing: routing-payload,
}
```

One `(candidate, routing)` pair — the WIT form of the native
`CandidateBatch` element (`Vec<(RawCandidate, RoutingPayload)>`). The
`routing` token is opaque to the picker; the source emits it here and
consumes it in `accept`.


## `plugin-manager`

**Direction:** guest calls into the host through it · **Capability:** subprocess · **Worlds:** `plugin-manager-plugin` (imports)

PM.7: the `require` seam — how a user's `init.rs` declares the plugins it
wants (plugin-manager.md §3).

This is the **user**-plugin surface. Core plugins (the ones that ship with
lattice) are NOT `require`d: they are discovered from the runtime root and
enabled by a `<id>.enabled` config gate (§7), so a fresh editor with no
user `init.rs` still gets its batteries. `require` exists for plugins the
*user* names, with a source the host must resolve and build.

It is programmatic rather than a TOML list on purpose (§3, rejected
alternatives). use-package is programmatic — conditional loading, per-plugin
setup — and the standing principle is that logic stays code while static
settings stay declarative. A `[[plugin]]` table would be simpler and would
lose exactly the expressiveness the feature is for.

#### Recording, not doing

`require` **records** a spec and returns immediately. It performs no
resolution, no clone, no build, no load. The host drains the recorded specs
after the guest's registration export returns and runs the pipeline
off-thread (§5) — the `register-mode` / `register-grammar` precedent.

That split is not an implementation detail. A `require` that resolved
inline would put a git clone and a cargo build inside a guest call on the
boot path, which paramount goal #1 forbids outright and which would make a
cold first boot hang on the network with no way to render a frame.
Contributions from a required plugin therefore appear a frame or two after
boot — the eventual consistency the UX contract already permits for plugin
cold-start.

### Functions (1)

#### `require`

```wit
require: func(spec: plugin-spec) -> bool
```

Declare a plugin. Records the spec; the host resolves, builds and loads
it after the calling export returns.

Returns `false` when the spec is rejected outright — today, an unsafe
`name`. A rejection is a logged skip, never a trap: one bad entry in an
`init.rs` must not take the whole config down.

### Types (3)

#### record `git-source`

```wit
record git-source {
    url: string,
    rev: option<string>,
}
```

A git source. `rev` pins a revision; omitted tracks the default branch.

#### variant `plugin-source`

```wit
variant plugin-source {
    local(string),
    git(git-source),
    prebuilt(string),
}
```

Where a plugin comes from. Mirrors the host's `PluginSource`.

**Cases**

- `local`: `string` — A cargo project on disk, built in place (never copied).
- `git`: `git-source` — A git repository, cloned into the source cache.
- `prebuilt`: `string` — A URL serving a ready-built `.wasm` — no build, no toolchain.

#### record `plugin-spec`

```wit
record plugin-spec {
    name: string,
    source: plugin-source,
    enable-mode: option<string>,
    pinned: bool,
}
```

One declared plugin.

**Fields**

- `name`: `string` — The plugin's name — the directory it caches under, and the key the
  host reports it by. Must be a single safe path component; the host
  rejects anything else rather than letting a name escape the cache
  root (the same validation the manifest `id` already gets).
- `source`: `plugin-source`
- `enable-mode`: `option<string>` — use-package sugar: enable this mode once the plugin loads.
  Desugars to the CI.5 `on-plugin-loaded` → `enable-mode` path, so
  the host never learns a mode-id statically
  (`feedback_mode_owns_its_surface`).
- `pinned`: `bool` — Skip the rebuild-on-change check; build only if the artifact is
  absent. The escape hatch for a known-good build that should stay
  put regardless of what the source tree does.


## `project`

**Direction:** guest calls into the host through it · **Capability:** filesystem · **Worlds:** `completion-source-plugin` (imports), `config-plugin` (imports), `context-plugin` (imports), `dashboard-plugin` (imports), `decorations-plugin` (imports), `events-plugin` (imports), `help-plugin` (imports), `keymap-plugin` (imports), `language-plugin` (imports), `media-plugin` (imports), `modes-plugin` (imports), `multibuffer-view-plugin` (imports), `picker-source-plugin` (imports), `plugin` (imports), `plugin-manager-plugin` (imports), `project-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `sign-plugin` (imports), `theme-plugin` (imports), `transient-source-plugin` (imports)

Guest→host project resolution (PR.6, design
`docs/dev/architecture/project-resolution.md` §6).

A **project** is the tree a buffer belongs to — the answer `:terminal`,
`:compile` and `:search` root themselves at. It is found by walking up from
the buffer's own directory to the first directory holding a marker (`.git`,
`Cargo.toml`, …, configurable via `project.root-markers`); with no marker
anywhere, the editor's working directory stands in.

#### An import, not a contribution seam

The host answers; the guest asks. Project resolution is CORE — terminal,
compilation, search, the file picker and magit all root from it — so it can
never depend on a plugin being alive. Were this a contribution seam, each of
those would need an "if the project plugin loaded, ask it, else fall back"
branch, and boot ordering would become load-bearing for correctness rather
than for features.

A `project.el`-style plugin therefore READS the root here and acts through
the ordinary effect seams; it does not supply the root.

#### Resolution only

Deliberately just "where is the project". No file listing, no project list,
no switching — those are the plugin's job, and a host seam that grew them
would be re-implementing the plugin inside the host.

Sync, and available in every world. It may walk the filesystem on a cache
miss, but it runs on the plugin's own store and task — never the UI or actor
thread — and the host's cache is keyed by directory, so a project's buffers
share one walk.

### Functions (2)

#### `root-for-buffer`

```wit
root-for-buffer: func(buffer: u64) -> option<project-info>
```

The project containing `buffer`.

`none` means **no such buffer** — an id the host does not know, which is
untrusted input from the guest rather than a real answer. A buffer that
exists always resolves: one with no path on disk (a scratch buffer, a
terminal) reports the working directory with `kind = pwd`.

#### `root-for-path`

```wit
root-for-path: func(path: string) -> option<project-info>
```

The project containing `path`, which may name a file or a directory and
need not exist yet.

`none` only when the host has no resolver wired, which a real editor
always does; a relative path resolves against the editor's working
directory, never the plugin's.

### Types (2)

#### enum `project-kind`

```wit
enum project-kind {
    marker,
    pwd,
}
```

How the root was decided.

**Cases**

- `marker` — A marker was found. The common case.
- `pwd` — No marker anywhere up the tree; this is the editor's working
  directory standing in. A guest that wants to say "not in a project"
  checks for this rather than for an absent root.

#### record `project-info`

```wit
record project-info {
    root: string,
    kind: project-kind,
    marker: string,
}
```

A resolved project.

**Fields**

- `root`: `string` — Absolute path to the project root.
- `kind`: `project-kind` — How `root` was decided.
- `marker`: `string` — The marker that decided it (`.git`, `Cargo.toml`, …), or the empty
  string when `kind` is `pwd`. Carried because "why is my root here"
  is the question that follows "where is it".


## `scanned-excerpt-source`

**Direction:** shared types only (not called directly) · **Capability:** none (pure data / dispatch) · **Worlds:** `scanned-excerpt-source-plugin` (imports)

OM.A1: plugin-contributed agenda rows.

A plugin teaches lattice to recognise "things with a date on them" in a
filetype the editor has never heard of. Org's agenda is the first and the
motivating one, but nothing here is org: a source names the file
extensions it wants offered, is handed one file's text at a time, and
returns the rows it found.

#### It is a multibuffer, so the row shape is an excerpt

The host turns each [`entry`] into an `Excerpt { source, start_line,
end_line, header }` in a multibuffer view — which buys jump-to-source,
edit-propagates-to-source, headerline status and refresh from machinery
that already ships (`org-mode.md` §6.1). That is why an entry carries a
*line* rather than a rendered string: an agenda you can only read is a
lesser feature wearing the name.

#### Text AND a tree — structure from one, characters from the other

The text was always here. OT.3 adds the tree beside it, because a scan
that recognises structure by matching line prefixes cannot see CONTEXT.
`* TODO ` at the start of a line inside a `#+BEGIN_SRC` block is example
text, not a headline, and no line matcher can tell — the fact is not on
the line. org's text scan invented a phantom agenda row there.

**Both, not either.** An earlier draft of this slice replaced the text
with the tree, on the theory that the per-file copy was the cost worth
removing. Two measurements killed that: the copy is **217 ns** per file
(`benches/agenda_scan_input.rs`), and the parse that buys the tree is
**1–2 ms** — so the copy was never the expense. Worse, a tree alone
cannot answer what a scanner asks: this seam exposes node kinds and
ranges but no node TEXT, so a guest would need one boundary crossing per
headline to read a TODO keyword — about 50 µs per file, 200× the copy it
was avoiding. Structure from the tree, characters from the text.

`tree` is `none` when the extension resolves to no registered language or
the parse yields nothing. A source is independent of the `language` seam
(see `extensions` below), so a filetype with no grammar must still scan —
it simply scans text, as it always did.

**The guest still touches no filesystem** — no preopens, no `walk`, and
not `tree-sitter.parse-file` either. The host must read the file anyway
to build the source `Document`, so it reads once and parses once, and
the guest is handed both results. That keeps this the one seam that
needs no capability at all.

#### Where it runs

Off the UI and actor threads, on a spawned scan task. Not the keystroke
path — but it IS the critical path of a producer, so a guest that blocks
in `scan` backs up the agenda the way a slow `error-parser` backs up a
build. Budgeted per call like every other seam.

#### What the host does with a bad entry

Validates and drops, never traps. A malformed file must not fail the
agenda — `error-parser`'s rule, because it is the same failure class.

### Uses

- `display-span` from `types`

### Functions (0)

_(none — a shared type interface)_

### Types (4)

#### record `annotation`

```wit
record annotation {
    text: string,
    spans: list<display-span>,
}
```

HB.5: one line hung below a row, and how it is coloured.

One line rather than a list: the consistency graph is one row, and
heights and scroll interactions are not worth inventing for a consumer
that does not exist. A `list<annotation>` is the obvious widening.

**Fields**

- `text`: `string` — The line's text, rendered as-is. Not an excerpt of anything — this
  is the one place a scan source draws content of its own.
- `spans`: `list<display-span>` — Byte spans into `text` (NOT into the row's source line, which this
  is not part of), so a guest's own registered elements
  (`org.habit.overdue`) reach the row with the active colourscheme
  applied and no colour crosses the boundary.

  **A slot here names a THEME ELEMENT, and only that.** `entry.spans`
  also accepts tree-sitter capture names (`keyword`, `string`),
  because those stay a semantic style the cells worker colours at
  paint time — but a virtual row's cells carry a baked colour, so the
  annotation resolves its slots when the row is built and a capture
  name has nothing to resolve against. An unknown slot renders in the
  renderer's default foreground rather than failing the row.

  Validated per span like `entry.spans`: a bad one costs itself, and
  an annotation whose spans are all bad still renders its text. A row
  must never lose its annotation because a decoration was malformed.

#### record `entry`

```wit
record entry {
    line: u32,
    end-line: u32,
    group: string,
    label: string,
    sort-key: s64,
    spans: list<display-span>,
    annotation: option<annotation>,
    emphasis: bool,
}
```

One agenda row the guest recognised in a file.

**Fields**

- `line`: `u32` — **0-based** line of the row's anchor, `error-parser`'s
  convention. Becomes the excerpt's `start_line`.
- `end-line`: `u32` — Last 0-based line of the excerpt, inclusive. Equal to `line` for
  the one-row-per-headline case. A guest wanting the headline plus
  its `SCHEDULED:` line returns `line + 1` here.
- `group`: `string` — Grouping **key**. Rows that sort next to each other and share a
  key render under ONE header — which is how a date group shows
  one header for N rows drawn from N different files.

  It is a key, not a label, because the guest cannot know which
  of its rows will land first once every other file's rows are
  interleaved by the sort. The host compares keys AFTER sorting
  and titles the first row of each run; the rest render no header.
- `label`: `string` — The header title for this row, used when it turns out to start
  a group — `"Today"`, `"2026-08-27 Thu"`. Rows sharing a `group`
  should carry the same `label`; the first one after the sort is
  the one rendered.

  The row's own text is the source line itself. This is a header,
  not a rendered agenda line: an excerpt shows the file, which is
  what makes the agenda editable rather than a list of strings.
- `sort-key`: `s64` — Host stable-sorts across files on this, ascending. The guest
  owns what it means (an epoch day, a priority rank, a composite).
- `spans`: `list<display-span>` — OA.5: how this row is COLOURED, as byte spans into the row's own
  first line.

  Without this a row is painted by the source file's tree-sitter
  grammar, because that is all the host has — so an agenda looks
  like org text that happens to be out of order, rather than like an
  agenda. The keyword, the priority, the tags and the date are
  semantics only the guest knows.

  Offsets are relative to the start of `line`, not to the composed
  view: the guest cannot know where its row lands once every other
  file's rows are interleaved by the sort. The host translates after
  sorting, the same way it titles group runs.

  `display-span.slot` names a style rather than carrying one, so a
  guest's own registered theme elements (`org.todo.WAITING`) resolve
  through exactly the path a `highlights.scm` capture takes. Empty is
  the ordinary case for a source with nothing to say about colour —
  the grammar's own highlighting is then what shows, unchanged.
- `annotation`: `option<annotation>` — HB.5: a row to hang BELOW this one, or `none`.

  A row's own text is a verbatim excerpt of a source line, so there is
  nowhere in it to put something the guest computed — org writes a
  habit's consistency graph at column 50 because its agenda line is
  generated text, and ours is the file. The annotation becomes a
  `virtual-row` anchored below the row instead.

  It rides the entry rather than a producer seam of its own for
  [`spans`]'s reason: a general producer would be handed the COMPOSED
  buffer, and a guest cannot know where its row lands until the sort
  has interleaved every other file's rows. See `org-agenda.md` §5b.

  `none` is the ordinary case — an agenda of plain TODOs grows no
  second rows.
- `emphasis`: `bool` — MH.A6: render this row's header EMPHASISED, if it turns out to
  start a group.

  Read only from the row that starts the group — the same rule
  [`label`] already lives by, and for the same reason: the guest
  cannot know which of its rows lands first once the sort has
  interleaved every other file's, so it sets this on EVERY row of
  the group and the host reads whichever one wins. Setting it on
  some rows of a group and not others is a guest bug whose symptom
  is "the header is sometimes emphasised".

  What emphasis LOOKS like is the colourscheme's business, not the
  guest's: the host renders these headers from
  `multibuffer.excerpt_header.emphasis[.title]` rather than from
  anything named here. A theme that does not define those elements
  renders an emphasised header exactly like an ordinary one —
  undistinguished, never invisible.

  **One per view is a convention this cannot enforce.** A guest that
  emphasises every group has emphasised nothing.

  `false` is the ordinary case and is byte-identical to the
  behaviour before this field existed.

#### record `clock-span`

```wit
record clock-span {
    line: u32,
    outline: list<string>,
    day: s64,
    minutes: u32,
}
```

OA.14b: time clocked on one headline on one day.

Independent of [`entry`] on purpose — see `scan`'s doc. A span is
reported for every clocked headline the guest saw, whether or not that
headline became an agenda row.

**Aggregated per (headline, day) by the guest**, not one span per
`CLOCK:` line. A headline clocked four times in a morning is one span,
which is the granularity every report actually renders and keeps a file
with years of history from crossing thousands of records it would only
sum again.

**Every span the file has, not just the ones in view.** The report's
range is the agenda's span — day, week, month or year — and the host
filters on `day` when it builds the table. Carrying them all is what
lets `gD` switch that range and redraw from data already in hand
instead of re-walking the corpus for each answer.

**Fields**

- `line`: `u32` — 0-based line of the HEADLINE the time was logged under (not of the
  `CLOCK:` line), so a report row can locate its entry.
- `outline`: `list<string>` — The headline's outline path: outermost ancestor first, the headline
  itself last. Its length is the outline level, which is what emacs's
  `:maxlevel` bounds.

  A PATH rather than a name plus a level, because the report is a
  hierarchy and totals roll up it. An ancestor that logged no time of
  its own emits no span, so the host cannot name it from the span
  list — carrying the chain is what lets the tree be rebuilt without
  inventing zero-minute rows for every parent.
- `day`: `s64` — Days since the Unix epoch that the clocked time is filed under.

  A span crossing midnight is counted whole on the day it began
  rather than split. Emacs splits it; matching that is a refinement
  this record can carry later without changing shape.
- `minutes`: `u32` — Minutes clocked. A running (unclosed) clock contributes nothing —
  its duration is not yet a fact, and guessing one would make the
  report disagree with the file.

#### record `scan-result`

```wit
record scan-result {
    entries: list<entry>,
    clock: list<clock-span>,
}
```

What one file's scan produced.

**Fields**

- `entries`: `list<entry>` — The agenda rows, filtered by whatever the guest's sections admit.
- `clock`: `list<clock-span>` — Every clocked span in the file, unfiltered. Empty for the
  overwhelming majority of files, which costs nothing.


## `signs`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `sign-plugin` (imports)

Mirrors the sign registry (`lattice_mode::SignRegistry`). A plugin declares
the signs it places — glyph, fallback glyph, theme element, priority — and
the host registers each into the SAME registry native producers use, owned
by the plugin so unload reverses it.

See `docs/dev/architecture/gutter-signs.md`.

**Why a plugin declares signs rather than drawing glyphs.** The alternative
— a placement that carries its own glyph and colour — puts the palette in
the plugin (so `:colorscheme` cannot touch it) and re-crosses the same glyph
and theme key for every marked line of every refresh, to restate something
that was already true at load. Declaring once and placing by name is the
only shape where the cost is paid where the information actually changes.

The definition/placement split is `:sign define` / `:sign place`, and it is
load-bearing rather than historical — see the design doc §1.

### Functions (1)

#### `define-sign`

```wit
define-sign: func(name: string, spec: sign-spec) -> result<_, string>
```

Declare a sign.

**Auto-namespaced**, like `theme.register-element` and
`config.register-option`: `name` is prefixed with the plugin's id, so a
plugin with id `debugger` declaring `breakpoint` contributes
`debugger.breakpoint`. The host owns the namespace, so plugins cannot
collide with each other or shadow a native producer's sign.

Idempotent by name (the native registry's contract): redefining KEEPS
the id, so a plugin reloading with a new glyph does not orphan
placements already in flight — they simply start painting the new
glyph, which is what "redefine" should mean.

`err` when the spec is malformed — never a trap, and never a
partially-registered sign.

### Types (1)

#### record `sign-spec`

```wit
record sign-spec {
    text: string,
    fallback: string,
    theme-element: string,
    priority: s32,
    column: string,
}
```

What a sign looks like and how it competes for its cell. Mirrors
`lattice_mode::SignDefinition` minus the name, which is the key.

**Fields**

- `text`: `string` — The glyph when `ui.nerd_fonts` is on. **One cell** — a sign paints
  into the gutter's single shared mark cell, so a wider glyph would
  push every line of content right. The host truncates rather than
  widening the gutter; the tail is lost, which is much cheaper than
  a viewport that shifts sideways.
- `fallback`: `string` — The glyph when it is off — the SAME cell width, per the
  icon-degradation rule, so toggling `ui.nerd_fonts` cannot shift the
  gutter's geometry. The theme decides the COLOUR and the font
  capability decides the GLYPH; conflating the two is how a themed
  editor renders tofu.
- `theme-element`: `string` — The theme element the glyph is painted in. Register it via the
  `theme` interface and name it here, and a user or a theme retunes
  this sign without either knowing about the other.

  An element the theme does not know falls back to `gutter.sign`
  rather than to no style at all — a sign was placed to say
  something, and painting it invisibly is the one outcome that loses
  the information entirely rather than showing it in the wrong tone.
- `priority`: `s32` — Which sign wins when two land on one line OF THE SAME COLUMN.
  Higher wins; ties break on name, so the painted glyph is stable
  rather than incidental to hash order.

  Diagnostics are signs too, and they span `10..40` — hint 10, info
  20, warning 30, error 40 — which is how "most severe wins" is
  expressed now that there is no separate severity mechanism. `10` is
  vim's default sign priority and the floor: a sign shipping it ties
  with a hint and loses to everything above.

  Exceed `40` only for something that genuinely outranks a compiler
  error — a debugger stopped on this very line. Displacing an error
  hides a state of the user's code they did not ask for, so the bar
  is deliberately high.
- `column`: `string` — SG.4a: which gutter column this sign paints in.

  `"mark"` is the leftmost column — vim's `signcolumn`, shared with
  diagnostics — and is what an empty string means. `"diff"` is the
  git-diff column. Columns exist because contention is only
  meaningful between marks that answer the same question: a single
  contended cell would drop the git gutter on exactly the lines a
  diagnostic touches, which are the lines a user is most likely to
  be looking at.

  A column the host does not paint falls back to the leftmost one
  rather than vanishing — the same principle as the `gutter.sign`
  theme fallback.


## `theme`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `preload-fixture` (imports), `theme-plugin` (imports)

Mirrors the theme-element registry (`lattice-theme`). A plugin declares the
elements it paints with (name + doc + default style); the host registers each
into the SAME registry builtins live in, under `SourceLayer::Plugin(id)` so
unload reverses it. A plugin-registered element is then indistinguishable
from a builtin: themes override it, `:customize` edits it, `:describe-element`
documents it.

This closes the deferred item in `theme-system.md` — WIT element registration
was designed there and waited for a real consumer, which the sticky-context
plugin is (TC.4/TC.5).

**Why a plugin registers elements rather than naming colours.** The
alternative — the plugin passes literal colours, or names host-owned
`context.*` builtins — puts the palette in the plugin (so a `:colorscheme`
swap cannot touch it) or the element vocabulary in the host (so the plugin
cannot be uninstalled without leaving debris in `:customize`). Registering
the element and letting the theme own what it looks like is the only shape
where both stay where they belong.

### Functions (2)

#### `register-element`

```wit
register-element: func(name: string, doc: string, default: style-spec) -> result<_, string>
```

Declare a theme element with its default style.

**Auto-namespaced**, like `config.register-option`: `name` is prefixed
with the plugin's id, so a plugin with id `treesitter-context`
registering `background` contributes `treesitter-context.background`.
The host owns the namespace, so plugins cannot collide with each other
or shadow a builtin.

Idempotent by name (the native registry's contract): re-registering
returns the existing id and leaves its default unchanged, so a reload
is free. `err` when the spec is malformed — never a trap, and never a
partially-registered element.

#### `set-element-override`

```wit
set-element-override: func(name: string, style: style-spec) -> result<_, string>
```

TK.5: override an element this plugin owns, ABOVE the theme.

`register-element` supplies a *default*, which sits BELOW the active
theme in the resolution stack (`theme-system.md` §5) — so a plugin
cannot express "the user configured this and it must win" with a
default alone. This is that missing step, and it is what lets an
org-shaped `org.todo-keyword-styles` behave the way
`org-todo-keyword-faces` does in emacs.

**Auto-namespaced exactly like `register-element`**, which is what
bounds it: the prefix is the calling plugin's id, so a plugin can only
ever name elements inside its own namespace and cannot restyle a
builtin or another plugin's element. The host re-checks ownership
anyway — namespacing is the mechanism, the check is the guarantee.

`err` for an element this plugin has not registered, so a typo is a
named refusal rather than an override that lands nowhere.

###### Lifetime, which is a real limitation

`:colorscheme` replaces the palette AND the whole override map
atomically, so an override set here does not survive one. Re-applying
after a colourscheme change needs the `theme` import to be reachable
from a path that is alive when the change happens; today this seam's
store is dropped when `register-theme-elements` returns. Documented
rather than worked around.

### Types (3)

#### variant `color-ref`

```wit
variant color-ref {
    palette(string),
    literal-rgb(u32),
    default,
}
```

A colour by reference. Mirrors `lattice_theme::ColorRef`.

`palette` is the path a plugin should normally take: it names a key in
the ACTIVE palette (`"blue"`, `"overlay"`, `"text"`), so the element
re-colours when the user swaps colourscheme. `literal-rgb` is the escape
hatch for a colour no palette key expresses; `default` means the
terminal/window default channel.

An unknown palette key resolves to the inherited parent rather than
failing loudly — the same forgiving resolution native elements get. The
symptom of a typo is therefore "everything looks the same", not a crash.

#### record `modifier-set`

```wit
record modifier-set {
    bold: option<bool>,
    italic: option<bool>,
    underline: option<bool>,
    dim: option<bool>,
    reverse: option<bool>,
}
```

Tri-state modifiers. Mirrors `lattice_theme::ModifierSet`: `some(true)`
sets, `some(false)` CLEARS an inherited one, `none` leaves it
unspecified. The three-way distinction is load-bearing — an element that
inherits a bold parent must be able to turn bold off, which a plain bool
cannot express.

#### record `style-spec`

```wit
record style-spec {
    inherit: option<string>,
    fg: option<color-ref>,
    bg: option<color-ref>,
    modifiers: modifier-set,
    scale: option<f32>,
}
```

How an element is styled, by reference. Mirrors
`lattice_theme::StyleSpec`.

`family` and `weight` are deliberately ABSENT. `family` is an interned
`FamilyId` a plugin cannot produce — crossing it would need a
name-to-id interning contract that no consumer has asked for — and
`weight` is a variable-font axis whose only users are native heading
treatments. Shipping half-designed fields to "size the ABI" is worse
than adding them when something needs them; the WIT is explicitly
unstable until three real plugins have exercised it (plugin-host.md §12).

**Fields**

- `inherit`: `option<string>` — Inherit another element's resolved style; this spec's set fields
  override. The name is resolved at theme-build time, so inheriting an
  element that does not exist yet is fine as long as it exists by the
  time the table is built.
- `fg`: `option<color-ref>`
- `bg`: `option<color-ref>`
- `modifiers`: `modifier-set`
- `scale`: `option<f32>` — Relative height ratio (the emacs `:height` float). Quantized to
  fixed-point at resolution.


## `transient-source`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `project-plugin` (exports), `transient-source-plugin` (exports)

TR.2b: plugin-contributed transient menus.

A transient is a keyed menu — one keystroke per row, fires and closes. The
mechanism belongs to `lattice-picker` (`TransientSpec`,
`TransientSourceRegistry`); magit is its first *user*, not its owner. Until
this seam a plugin could `Effect::OpenTransient` one of magit's menus and
none of its own, which made org's capture menu — one row per template —
inexpressible.

#### Mirrors `picker-source`, because it is the same shape

A named thing the host asks a guest to build, given a context the host
owns: `id()` names the registry entry once at load, `build(ctx)` produces
the menu per open.

#### Per open, not once at registration

A builder's rows depend on where the user is — which is why
`transient-context` exists at all, and why the host calls `build` on every
open rather than caching a spec. Emacs magit answers the same question with
`:if-mode` / `:if-derived` predicates on its prefixes; the two mode axes
are separate fields here for the same reason.

#### Where it runs

On the plugin's own actor task, off the editor actor. `build` is reached by
an explicit user action (a chord, an ex-command) — never per keystroke and
never per frame — and the host parks on it, seating the menu when it lands.
A slow guest delays its own menu and nothing else.

### Uses

- `transient-spec` from `types`
- `transient-context` from `types`

### Functions (2)

#### `build`

```wit
build: func(ctx: transient-context) -> result<transient-spec, string>
```

Build the menu for the place it was opened from.

An `err` is echoed with the plugin named and the menu does NOT open —
the `picker-source::init` rule, and for the same reason: a menu that
opens empty is worse than one that says why it did not.

#### `id`

```wit
id: func() -> string
```

The menu's name, as `Effect::OpenTransient` names it. Called once, at
load, to key the registry entry.

Guest-controlled, so it is a *name* and nothing more: it grants no
authority, and a plugin that picks a name another source already holds
simply overwrites it (`register`'s last-writer-wins, as for pickers).


## `tree-sitter`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `context-plugin` (imports), `grammar-plugin` (imports), `multiseam-fixture` (imports), `project-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `treesitter-context-plugin` (imports)

Structural queries for plugins (plugin-treesitter-seam.md). The host already
parses every buffer with tree-sitter (`lattice-syntax`) and publishes an
immutable `SyntaxSnapshot` per buffer; this seam **publishes that snapshot to
a plugin, read-only**, so a WASM plugin can navigate the parse tree exactly
as native structural code does. First consumer: `auto-pair`'s manual style
queries the enclosing lexical scope to bound its backward scan (design §7).

The tree NEVER crosses the boundary — walks execute host-side against the
snapshot's `tree_sitter::Tree`; only *results* (a node projection, a kind
string) cross. A plugin reads a POINT-IN-TIME snapshot: it acquires the
handle alongside the `document` handle from the same dispatch context (same
instant → tree + text versions agree, §7); an edit landing after swaps a
newer snapshot without disturbing the read (the `document`-handle
mutation-under-read discipline, applied to structure). Gated on the
`tree-sitter` editor-capability — no grant, no handle (design §5).

**TS.1 scope:** the snapshot + node core (enough for auto-pair's `enclosing`).
Queries (`compile-query` / `run-query` with host-side predicates) and the
`tree-cursor` walk land at TS.2; see the design fragment §3.3–§3.4 / §10.

### Uses

- `position` from `types`
- `range` from `types`

### Functions (1)

#### `parse-file`

```wit
parse-file: func(path: string) -> option<tree-snapshot>
```

OT.2: parse a file that is **not an open buffer**, and hand back a
snapshot on the same terms as a buffer's.

Every other snapshot in this interface belongs to a buffer the editor
already parsed. A plugin acting on project files it never opened — org's
capture resolving a `file+headline` target, its refile picker listing
every headline in the project — had no way to get structure, so it
hand-parsed text and diverged from the grammar. That divergence is the
bug class OT.x exists to end, and this is the primitive that ends it for
off-buffer content.

Names no plugin and no language: the extension resolves through the same
registry a buffer's does (native languages first, then plugin-registered
ones), so a plugin gets a tree for `.org` for exactly the reason the
editor would.

**`none`, never a trap**, when any link in the chain is missing: no
`tree-sitter` capability, the path is outside the plugin's `fs:` grant,
the file is unreadable or not UTF-8, the extension maps to no language,
or the parse yields no tree. A caller that cannot tell these apart is
making one decision — "can I read structure here?" — and the answer is
no. `error-parser`'s rule: one bad file must not fail the walk.

**The host reads and parses; only the path crosses.** The tree never
crosses the boundary (§7) and neither does the file's text, so this is
strictly cheaper than `read-file` plus a guest-side scan.

**Cost, stated rather than buried.** Reachable from the SYNC grammar
linker, where the sibling comment says reads are "no I/O, no parse — the
tree is already there". This one is both, so it belongs on explicit user
actions (capture's chord) and NOT in a motion or text object, which fire
per keystroke. `read-file` set the I/O precedent here; this adds the
parse on top of it.

### Resources

#### resource `tree-snapshot`

A host-owned, point-in-time view of a buffer's parse tree — backed by an
`Arc<SyntaxSnapshot>` (an O(1) `ArcSwap` bump, no parse, no copy). An
`apply-action` receives it as `option<borrow<tree-snapshot>>` (absent
when the buffer has no parse: plain text / parse pending). Every `node`
it hands out is anchored to THIS snapshot.

##### `tree-snapshot.compile-query`

```wit
compile-query: func(source: string) -> result<query, string>
```

TS.2: compile a tree-sitter query (S-expression) against THIS
snapshot's grammar. `err` (with the tree-sitter message) on a
malformed query. The returned `query` is reusable across snapshots of
the same language — compile once, run many.

##### `tree-snapshot.enclosing`

```wit
enclosing: func(pos: position, kinds: list<string>) -> option<node>
```

The nearest ancestor of `pos` whose `kind` is in `kinds` (the
auto-pair scope query; the native `scope_toward` precedent). `kinds`
empty → the nearest named ancestor. `none` when there's no match / no
parse.

##### `tree-snapshot.language`

```wit
language: func() -> string
```

The grammar id (e.g. `"rust"`), so a plugin can pick the right query.

##### `tree-snapshot.node-at`

```wit
node-at: func(pos: position) -> option<node>
```

The smallest NAMED node spanning `pos`
(`Tree::named-descendant-for-point-range`), or `none` when the buffer
is empty / `pos` is out of range.

##### `tree-snapshot.root`

```wit
root: func() -> node
```

The tree root.

##### `tree-snapshot.run-query`

```wit
run-query: func(q: borrow<query>, within: option<range>) -> list<capture>
```

TS.2: run `q` over the whole tree, or `within` a point range. Returns
the surviving captures — the `#eq?` / `#match?` / `#any-of?` predicates
are evaluated HOST-side (against the snapshot's source), so the guest
never re-filters. Empty when `q` was compiled for a different grammar
than this snapshot's (graceful — never a trap).

##### `tree-snapshot.run-query-ranges`

```wit
run-query-ranges: func(q: borrow<query>, within: option<range>) -> list<capture-range>
```

TS.2b: the same query, returning RANGES instead of node handles.

`run-query` mints one `node` resource per capture. A resource is a
table entry with a host-side snapshot bump and a guest-side drop, so
a whole-file structural query pays that per capture — and a
structural query over a large file has tens of thousands of them.
That cost is what forced `treesitter-context`'s `max-file-lines`
guard, and it is pure overhead for the (common) plugin that only
ever reads a capture's extent.

Same predicates, same host-side filtering, same graceful-empty on a
grammar mismatch. `match-index` groups captures that came from ONE
pattern match, so a query can capture a construct and its body
(`@context` + `@context.end`) and the guest can pair them without a
second query or a containment test.

Use `run-query` when the capture must be NAVIGATED (parent, field,
sibling); use this when its extent is the answer.

#### resource `node`

An opaque, navigable handle into the snapshot's tree (design §3.2). Owned
by the guest and dropped when it goes out of scope; each holds its own
snapshot bump so it stays coherent for the call. Projection is cheap and
value-returning; navigation returns a FRESH `node` (or `none`). The tree
itself never crosses — a handle is a host-side `(snapshot, path)` pair.

##### `node.byte-range`

```wit
byte-range: func() -> range
```

The node's `[start, end)` span as byte-columns per line (matching the
native structural objects' `ProtoRange`, N.1.4c).

##### `node.child-by-field`

```wit
child-by-field: func(name: string) -> option<node>
```

The child under the grammar field `name` (e.g. `"body"`), or `none`.

##### `node.is-error`

```wit
is-error: func() -> bool
```

Whether the node is a tree-sitter ERROR node (a parse error).

##### `node.is-named`

```wit
is-named: func() -> bool
```

Whether the node is *named* (a grammar rule) vs an anonymous token.

##### `node.kind`

```wit
kind: func() -> string
```

The node's grammar kind (e.g. `"function_item"`).

##### `node.named-child`

```wit
named-child: func(index: u32) -> option<node>
```

The `index`-th NAMED child (0-based), or `none` past the end.

##### `node.named-child-count`

```wit
named-child-count: func() -> u32
```

Count of NAMED children.

##### `node.next-named-sibling`

```wit
next-named-sibling: func() -> option<node>
```

The next NAMED sibling, or `none`.

##### `node.parent`

```wit
parent: func() -> option<node>
```

The parent node, or `none` at the root.

##### `node.prev-named-sibling`

```wit
prev-named-sibling: func() -> option<node>
```

The previous NAMED sibling, or `none`.

##### `node.walk`

```wit
walk: func() -> tree-cursor
```

TS.2: a stateful cursor positioned at this node, for structural walks
without per-step parent/child handle churn.

#### resource `query`

TS.2: a compiled tree-sitter query — opaque, owned by the guest (dropped
when it leaves scope), reusable across snapshots of the same language.

#### resource `tree-cursor`

TS.2: a stateful walk cursor over the snapshot's tree (design §3.4).
Anchored to one snapshot; `goto-*` move it and report whether they could.

##### `tree-cursor.current-field`

```wit
current-field: func() -> option<string>
```

The grammar field of the current node relative to its parent (e.g.
`"body"`), or `none` (root, or an unnamed field slot).

##### `tree-cursor.current-node`

```wit
current-node: func() -> node
```

The node the cursor currently sits on.

##### `tree-cursor.goto-first-named-child`

```wit
goto-first-named-child: func() -> bool
```

Move to the first NAMED child; `false` (and no move) if there is none.

##### `tree-cursor.goto-next-named-sibling`

```wit
goto-next-named-sibling: func() -> bool
```

Move to the next NAMED sibling; `false` (and no move) if there is none.

##### `tree-cursor.goto-parent`

```wit
goto-parent: func() -> bool
```

Move to the parent; `false` (and no move) at the root.

##### `tree-cursor.reset`

```wit
reset: func(n: borrow<node>)
```

Reposition the cursor onto `n` (must be a node of the same snapshot).

### Types (2)

#### record `capture`

```wit
record capture {
    name: string,
    node: node,
}
```

TS.2: one query match capture — the `@name` and the node it bound.

#### record `capture-range`

```wit
record capture-range {
    name: string,
    match-index: u32,
    range: range,
}
```

TS.2b: a capture reduced to its extent — no resource, no drop.

`match-index` is the ordinal of the pattern match this capture belongs
to WITHIN this call's results (not a stable tree id): captures sharing
one value came from one match of one pattern.


## `types`

**Direction:** shared types only (not called directly) · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `completion-source-plugin` (imports), `context-plugin` (imports), `decorations-plugin` (imports), `events-plugin` (imports), `grammar-plugin` (imports), `init-fixture` (imports), `media-plugin` (imports), `multibuffer-view-plugin` (imports), `multiseam-fixture` (imports), `picker-source-plugin` (imports), `plugin` (imports), `project-plugin` (imports), `scanned-excerpt-source-plugin` (imports), `transient-source-plugin` (imports), `treesitter-context-plugin` (imports)

Shared boundary records/variants — the owned, WIT-serializable mirrors of
the native grammar + picker/completion types (plugin-host.md §4). Every
interface that crosses one of these `use`s it from here; the host
round-trips native ↔ these generated types via the `WitBoundary` adapter
trait (`boundary.rs`, PH7.3a). Bulk rope text never rides these records —
it crosses via the `buffer` `document` resource handle (PH7.3c).

Populated incrementally across PH7.3: `args`/`arg-value` (PH7.3a),
`raw-candidate` + `picker-accept-outcome` (PH7.3a), the `effect` variant
mirror (PH7.3b). Types whose native form carries a nested
`CommandInvocation` (e.g. `arg-value::invocation`) are deferred to the
command mirror (§4.1) and cross as a typed error until then.

### Functions (0)

_(none — a shared type interface)_

### Types (147)

#### variant `arg-value`

```wit
variant arg-value {
    string(string),
    char(char),
    bool(bool),
    int(s64),
    pattern(string),
    chord(string),
    raw(string),
}
```

Mirrors `lattice_grammar::args::ArgValue`. The native
`Invocation(Box<CommandInvocation>)` variant is intentionally absent
until the command mirror lands (§4.1); crossing it before then is a
typed `WitBoundary` error, never a lossy encoding.

#### variant `args`

```wit
variant args {
    none,
    char(char),
    string(string),
    bytes(list<u8>),
    list(list<arg-value>),
}
```

Mirrors `lattice_grammar::args::Args`. `bytes` is the msgpack escape
hatch (`Args::Bytes`) retained for now; typed calls prefer `list`.

#### variant `candidate-kind`

```wit
variant candidate-kind {
    command,
    option,
    file,
    directory,
    pattern,
    buffer,
    register,
    mark,
    chord,
    plain,
    extension(u32),
}
```

Mirrors `lattice_completion::candidate::CandidateKind`.

**Cases**

- `command`
- `option`
- `file`
- `directory`
- `pattern`
- `buffer`
- `register`
- `mark`
- `chord`
- `plain`
- `extension`: `u32` — Plugin-defined kind; the u32 is the registered kind tag.

#### record `candidate-file`

```wit
record candidate-file {
    path: string,
    is-dir: bool,
    size: option<u64>,
}
```

#### record `candidate-option`

```wit
record candidate-option {
    name: string,
    current-value: string,
    doc: string,
}
```

#### record `candidate-option-value`

```wit
record candidate-option-value {
    option-name: string,
    value: string,
    doc: string,
}
```

#### record `candidate-chord`

```wit
record candidate-chord {
    chord: string,
    mode-label: string,
    doc: string,
}
```

#### record `candidate-register`

```wit
record candidate-register {
    name: char,
    preview: string,
}
```

#### record `candidate-mark`

```wit
record candidate-mark {
    name: char,
    position: string,
}
```

#### record `candidate-extension`

```wit
record candidate-extension {
    kind-id: u32,
    payload: list<u8>,
}
```

#### variant `candidate-data`

```wit
variant candidate-data {
    file(candidate-file),
    option(candidate-option),
    option-value(candidate-option-value),
    chord(candidate-chord),
    register(candidate-register),
    mark(candidate-mark),
    plain,
    extension(candidate-extension),
}
```

Mirrors `lattice_completion::candidate::CandidateData`. The native
`Command { .., source: SourceLocation }` variant is intentionally
absent: `SourceLocation` is recursive (`DotRepeat(Box<Self>)`), which a
WIT variant cannot express directly, and command candidates are a
native-generator concern, not a plugin one. Crossing it is a typed
`WitBoundary` error until the provenance mirror lands — never lossy.

**Cases**

- `file`: `candidate-file`
- `option`: `candidate-option`
- `option-value`: `candidate-option-value`
- `chord`: `candidate-chord`
- `register`: `candidate-register`
- `mark`: `candidate-mark`
- `plain`
- `extension`: `candidate-extension` — Plugin-defined arbitrary payload (the `Extension` hatch): the
  `kind-id` routes to the registering plugin's annotator, which
  decodes `payload`.

#### variant `special-key`

```wit
variant special-key {
    esc,
    enter,
    tab,
    backspace,
    space,
    up,
    down,
    left,
    right,
    home,
    end,
    page-up,
    page-down,
    insert,
    delete,
    f(u8),
}
```

---- Marginalia annotations (PH7.4a, marginalia.md §8 / MARG.1) ----
The whole closed `Annotation` enum crosses so a plugin picker source can
define + populate marginalia columns (the user's PH7.4 requirement). The
host lays out `AnnotationColumns` from the visible set (a render-consumed
projection), so only the per-candidate annotations cross, never the column
layout. `slot` / `category` are theme element KEYS resolved at paint, never
baked colors, so a `:colorscheme` swap recolors plugin marginalia live.
Mirrors `lattice_protocol::chord::SpecialKey`. `f` carries the function-key
number (`1..=24`; `0` is invalid and rejected at the boundary).

#### variant `key-kind`

```wit
variant key-kind {
    char(char),
    special(special-key),
}
```

Mirrors `lattice_protocol::chord::KeyKind`.

#### record `key-chord`

```wit
record key-chord {
    key: key-kind,
    mods: u8,
}
```

Mirrors `lattice_protocol::chord::KeyChord`. `mods` is the raw `KeyMods`
bitfield (Ctrl=1, Shift=2, Alt=4, Super=8) — structural, not lossy.

#### record `annotation-segment`

```wit
record annotation-segment {
    text: string,
    slot: string,
}
```

Mirrors `lattice_completion::candidate::AnnotationSegment` — one run of
marginalia text sharing a theme `slot` key.

#### record `annotation-custom`

```wit
record annotation-custom {
    text: string,
    slot: string,
}
```

Payload of `annotation::custom` (the plugin escape hatch): pre-formatted
`text` + a theme `slot` key.

#### record `annotation-styled`

```wit
record annotation-styled {
    category: string,
    segments: list<annotation-segment>,
}
```

Payload of `annotation::styled` (§8: a multi-slot column cell) — a `category`
key plus per-segment slot-keyed runs (file-permission strings, size+unit, …).

#### variant `annotation`

```wit
variant annotation {
    kind(string),
    doc-snippet(string),
    keybinding(list<key-chord>),
    source(string),
    custom(annotation-custom),
    styled(annotation-styled),
}
```

Mirrors `lattice_completion::candidate::Annotation` (whole closed enum).

#### record `display-span`

```wit
record display-span {
    start: u32,
    end: u32,
    slot: string,
}
```

PS.1: a styled run of a candidate's `display` text.

`start` / `end` are BYTE offsets into `display`, half-open. A range that
is out of bounds, inverted, or not on a UTF-8 boundary is dropped with a
warning naming the source — one bad span must not cost a row its other
runs, and must never panic the picker.

`slot` is a **capture or theme-element name**, resolved host-side
through exactly the path a `highlights.scm` capture takes
(`name_to_style_with_theme`): a builtin category (`keyword`,
`text.title.1`, `comment`) wins first, and any other name resolves
against the theme registry as a `Style::Element` — which is how a
plugin's own registered element (`org.todo.WAITING`) reaches a picker
row. An unresolvable name renders unstyled rather than failing the row.

Naming a style rather than carrying one is deliberate, and follows
`annotation-custom.slot`: a `Style` is a closed Rust enum plus an
interned element id, and neither crosses an ABI meaningfully. A name
does, and it means a guest's picker row is coloured by the SAME
vocabulary — and the same active colourscheme — as the buffer it came
from.

#### record `raw-candidate`

```wit
record raw-candidate {
    text: string,
    insert-text: option<string>,
    display: string,
    source: option<string>,
    kind: candidate-kind,
    data: candidate-data,
    annotations: list<annotation>,
    display-spans: list<display-span>,
}
```

Mirrors the crossable core of `lattice_completion::candidate::RawCandidate`
**plus marginalia** (PH7.4a). `accept_action` remains host-only
(`#[serde(skip)]`, reconstructed host-side, §4.4); `annotations` crosses
so plugin sources contribute themed columns. `source` is the optional
source id.

PS.1: `display-spans` now crosses too. It was host-only on the reasoning
that render-time fields are "re-derived host-side when needed" — which
holds for a grep hit (the host has the path and the line, and re-derives
through the preview highlighter) and does not hold for a row that is not
a line of a file. An org-roam node title is a *headline's text* with no
stars and no file line to parse, so there was nothing to re-derive from
and every plugin picker row rendered plain, with no way for the plugin
that owns the domain to say otherwise.

**Fields**

- `text`: `string`
- `insert-text`: `option<string>` — OR.7: what to insert on accept when that differs from the text
  the query matched. `none` ⇒ insert `text`. A completion source
  that offers a human-readable label but inserts machine syntax
  (org-roam offering a node title and inserting an `[[id:…][…]]`
  link) needs both, and matching against the machine syntax
  instead would score every candidate on its id.
- `display`: `string`
- `source`: `option<string>`
- `kind`: `candidate-kind`
- `data`: `candidate-data`
- `annotations`: `list<annotation>`
- `display-spans`: `list<display-span>` — PS.1: styled runs over `display`. Empty for every source that does
  not style itself, which is the pre-PS.1 behaviour exactly.

#### record `jump-target`

```wit
record jump-target {
    buffer-id: u32,
    line: u32,
    col: u32,
}
```

#### record `location`

```wit
record location {
    path: string,
    line: u32,
    col: u32,
}
```

#### record `command-ref`

```wit
record command-ref {
    id: string,
    args: args,
}
```

#### record `lsp-code-action-ref`

```wit
record lsp-code-action-ref {
    handle: u64,
    index: u32,
}
```

#### variant `picker-accept-outcome`

```wit
variant picker-accept-outcome {
    open-file(string),
    switch-buffer(u32),
    jump-in-buffer(jump-target),
    jump-to-mark(char),
    jump-to-location(location),
    invoke-command(command-ref),
    paste-register(char),
    expand-snippet(string),
    open-lsp-log(string),
    open-lsp-trace-log(string),
    apply-lsp-code-action(lsp-code-action-ref),
    apply-lsp-completion(u32),
    apply-colorscheme(string),
    no-op,
}
```

Mirrors `lattice_picker::outcome::PickerAcceptOutcome`. All flat pure
data; paths cross as strings; `invoke-command` reuses `args`.

#### record `position`

```wit
record position {
    line: u32,
    byte: u32,
}
```

---- Effect payload mirrors (PH7.3b1a) ----
The nested payload types the `effect` variant (PH7.3b1b) composes.
Mirrors `lattice_protocol::position::Position`.

#### record `range`

```wit
record range {
    start: position,
    end: position,
}
```

Mirrors `lattice_protocol::position::Range`.

#### variant `edit-kind`

```wit
variant edit-kind {
    replace(string),
}
```

Mirrors `lattice_protocol::edit::EditKind`.

#### record `edit`

```wit
record edit {
    range: range,
    kind: edit-kind,
}
```

Mirrors `lattice_protocol::edit::Edit`.

#### record `edit-delta`

```wit
record edit-delta {
    start-byte: u32,
    old-end-byte: u32,
    new-end-byte: u32,
    start-position: position,
    old-end-position: position,
    new-end-position: position,
}
```

Mirrors `lattice_protocol::edit::EditDelta`.

#### record `applied-edit`

```wit
record applied-edit {
    original-range: range,
    inserted-range: range,
    replaced-text: string,
    inserted-text: string,
    delta: edit-delta,
}
```

Mirrors `lattice_core::buffer::AppliedEdit`.

#### variant `visual-mode`

```wit
variant visual-mode {
    charwise,
    linewise,
    blockwise,
}
```

Mirrors `lattice_protocol::selection::VisualMode`.

#### record `selection`

```wit
record selection {
    anchor: position,
    head: position,
    visual: option<visual-mode>,
}
```

Mirrors `lattice_protocol::selection::Selection`.

#### record `selection-set`

```wit
record selection-set {
    selections: list<selection>,
    primary: u32,
}
```

Mirrors `lattice_protocol::selection::SelectionSet` (reconstructed via
`SelectionSet::from_parts`). Always non-empty; `primary` indexes
`selections`.

#### variant `visual-kind`

```wit
variant visual-kind {
    charwise,
    linewise,
    blockwise,
}
```

Mirrors `lattice_grammar::modal::VisualKind`.

#### variant `search-direction`

```wit
variant search-direction {
    forward,
    backward,
}
```

Mirrors `lattice_grammar::modal::SearchDirection`.

#### variant `modal-state`

```wit
variant modal-state {
    normal,
    insert,
    visual(visual-kind),
    select(visual-kind),
    operator-pending,
    command,
    search(search-direction),
    replace,
    prompt,
}
```

Mirrors `lattice_grammar::modal::ModalState`.

#### variant `register`

```wit
variant register {
    unnamed,
    named(char),
    system,
    black-hole,
    expression,
    read-only(char),
    numbered(u8),
}
```

Mirrors `lattice_grammar::register::Register`.

#### variant `yank-kind`

```wit
variant yank-kind {
    charwise,
    linewise,
    blockwise,
}
```

Mirrors `lattice_grammar::effect::YankKind`.

#### variant `quit-scope`

```wit
variant quit-scope {
    pane,
    all,
}
```

---- The `effect` variant mirror (PH7.3b1b) ----
Mirrors the whole crossable surface of `lattice_grammar::effect::Effect`
(§4.4: the closed "host boundary vocabulary"). Three arms are absent by
design and cross as typed `WitBoundary` errors (never lossy):
  - `Effect::Many(Vec<Effect>)` — WIT value types cannot be recursive, so
    `Many` is not an arm. The boundary crosses `list<effect>` instead:
    `to_wit` flattens `Many` (associative composition), `from_wit`
    rebuilds `Many` when the list has >1 element.
  - `Effect::Global { body: Box<CommandInvocation>, .. }` — needs the
    command mirror (§4.1); typed error until then.
  - `Effect::AppAction(AppEffect)` — needs the `AppEffect` mirror (PH7.3b2);
    typed error until then.
Mirrors `lattice_grammar::effect::QuitScope`.

#### variant `echo-level`

```wit
variant echo-level {
    trace,
    debug,
    info,
    warn,
    error,
}
```

Mirrors `lattice_grammar::effect::EchoLevel`.

#### variant `substitute-scope`

```wit
variant substitute-scope {
    current-line,
    whole,
}
```

Mirrors `lattice_grammar::effect::SubstituteScope`.

#### record `utf16-pos`

```wit
record utf16-pos {
    line: u32,
    col: u32,
}
```

Mirrors `lattice_grammar::effect::Utf16Pos`.

#### variant `lsp-request`

```wit
variant lsp-request {
    hover,
    definition,
    declaration,
    type-definition,
    implementation,
    references,
    follow-link,
}
```

Mirrors `lattice_grammar::effect::LspRequest`.

#### enum `popup-placement`

```wit
enum popup-placement {
    centered,
    cursor-anchored,
    minibuffer-band,
}
```

Payload records for the multi-field `effect` arms. Paths cross as
`option<string>` / `string` (a non-UTF-8 path is a typed error, §4.4).
Mirrors `lattice_core::ui::popup::PopupPlacement`.

WK.5 added `minibuffer-band` (full pane width, flush to its bottom
edge — which-key's placement). Mirrored here rather than collapsed
to `centered` at the boundary because this enum's contract is that
it IS the mirror: a placement reachable natively but not from a
plugin is an arbitrary gap in the canonical API (paramount #2).

#### enum `popup-focus`

```wit
enum popup-focus {
    steal,
    passive,
}
```

Mirrors `lattice_core::ui::popup::PopupFocus`.

#### record `open-popup-payload`

```wit
record open-popup-payload {
    name: string,
    mode-id: string,
    placement: popup-placement,
    focus: popup-focus,
}
```

Payload for the `open-popup` effect (popup-api.md §4.3). Name-based:
the host ensures a popup buffer named `name` under major mode `mode-id`.

#### record `confirm-payload`

```wit
record confirm-payload {
    prompt: string,
    yes-action: string,
    args: args,
}
```

IX.3: the payload of `effect.confirm`.

`yes-action` names an action the guest (or host) registered; the
host resolves it through the command registry when the user
answers `y`. A **name**, not a command id — a guest cannot hold a
host-internal id, and names are what a plugin registers under.

`args` is what the yes-action receives when it fires, so the
confirmed target and the executed target are the same thing.
Without it the yes-half must re-derive its target at answer time
from context that may have changed while the dialog was open.

**Fields**

- `prompt`: `string` — Shown as the dialog's title. Name the target in it — a
  question has to be answerable without dismissing it to go
  look.
- `yes-action`: `string` — Action name dispatched on `y`. `n` / `q` / Esc dismiss and
  dispatch nothing.
- `args`: `args` — Arguments handed to `yes-action`, positional against its
  declared `args-schema`.

#### record `open-transient-payload`

```wit
record open-transient-payload {
    source: string,
    args: args,
}
```

IX.5: the payload of `effect.open-prompt`.

A one-line minibuffer prompt. On submit the host dispatches
`on-submit-action`, handing it the typed text — the action reads
it from its context's `prompt-value`, not from `args`, because
the value is what the *user* typed rather than what the caller
chose.
TR.3a: what `effect::open-transient` carries.

**Fields**

- `source`: `string` — The registered source name.
- `args`: `args` — Arguments for this open, handed to the builder as
  `transient-context.args`. `args::none` for a plain open.

#### record `open-prompt-payload`

```wit
record open-prompt-payload {
    prompt: string,
    initial: string,
    on-submit-action: string,
    buffer-name: option<string>,
}
```

**Fields**

- `prompt`: `string` — Shown before the input area.
- `initial`: `string` — Pre-filled text; empty for a blank prompt.
- `on-submit-action`: `string` — Action dispatched on submit. Escape dismisses and dispatches
  nothing.
- `buffer-name`: `option<string>` — Optional synthetic name for the prompt buffer. Callers that
  need to smuggle state through a multi-step flow encode it
  here; `none` gets the default name.

#### variant `file-anchor`

```wit
variant file-anchor {
    end,
    start,
    line(u32),
}
```

XF.4: where in a target file a `write-to-file` lands.

A *position*, not a range, and the asymmetry with `apply-edit` is
deliberate: for its own buffer a guest holds `borrow<document>` and can
compute a range that means something; for a file it has never read, a
range would be a guess. These are the three positions namable without
reading. Insert-only also means a guest cannot silently destroy content
in a file the user was not looking at.

**Cases**

- `end` — After the last line. The common case — archive, refile and capture
  all append.
- `start` — Before the first line.
- `line`: `u32` — Before this 0-based line. Past the end clamps to `end` rather than
  failing: a guest computing a line from a file it has not read can
  legitimately be off, and refusing to file the text at all is worse
  than filing it at the end.

#### record `write-to-file-payload`

```wit
record write-to-file-payload {
    path: string,
    anchor: file-anchor,
    text: string,
    cut: option<range>,
    create-parents: bool,
    save: bool,
}
```

XF.4: move text into a file the editor may not have open.

The primitive `org-archive-subtree`, `org-refile` and `org-capture`
were blocked on. `apply-edit` addresses a `buffer-id`, which a guest
cannot learn for a file that has never been opened.

**The write goes through the document pipeline, not to disk.** The host
resolves `path` to a buffer, reusing one already open — so the user's
unsaved changes are what the write lands on, `u` covers it, and the LSP
hears about it. The target is left MODIFIED, not saved: a plugin that
silently writes files is a larger authority than one that edits
buffers.

**Fields**

- `path`: `string` — Absolute, or relative to the editor's working directory.

  **Checked against this plugin's `fs:write` grant**, host-side, at
  the boundary — a path outside it is refused and echoed, and the
  effect never reaches the editor. The check runs here rather than at
  the applier because the applier cannot tell a plugin's effect from
  a native mode's; only the boundary still knows whose this is.
- `anchor`: `file-anchor`
- `text`: `string` — Inserted verbatim. A trailing newline is the guest's business —
  except that the host supplies a line break when appending to a
  target whose last line has none, which the guest cannot know.
- `cut`: `option<range>` — When present, this range is removed from the buffer the action ran
  in — and ONLY after the insert has landed.

  One effect rather than two, because as two the failure modes are
  "the text exists twice" and "the text is gone". The second is data
  loss from a keystroke, and an effect cannot report failure, so two
  ordered effects could not be made to depend on each other.
- `create-parents`: `bool` — OR.10: create missing parent directories rather than refusing.

  **False is the rule, not caution.** The host refuses a missing
  parent because creating directories is a larger authority than
  creating a file, and a typo'd path must not silently build a tree.
  That stays true for every guest that does not ask.

  A guest asks when the directory is part of the LAYOUT IT OWNS rather
  than something the user typed. org-roam's `daily/YYYY-MM-DD.org` is
  the case that forced this: the folder is named by an option with a
  default, no user ever types it, and without this the very first
  `:org-roam-dailies-today` on a fresh corpus fails — the one use
  where the feature has to work.

  Still bounded by `path`'s `fs:write` check above, which runs BEFORE
  this is read. Asking widens what is created inside the grant, never
  what is reachable outside it.
- `save`: `bool` — OC.9: persist the target to disk once the write has landed, rather
  than leaving the buffer modified.

  **False is the rule.** `cross-file-writes.md` §7 leaves a target
  open, listed and MODIFIED, and that is what emacs's `org-refile` and
  `org-archive-subtree` do: the user reviews the change and writes it
  themselves. Every guest that does not ask keeps that behaviour.

  A guest asks when its whole operation is "commit this somewhere",
  and org-capture is that case — `org-capture.el`'s finalize runs
  `(unless (org-capture-get :no-save) (save-buffer))`, so saving is
  emacs's DEFAULT there and `:no-save` exists to opt out of it. The
  asymmetry with refile is not an inconsistency in either editor: a
  refile moves text you are looking at, a capture files text you are
  done with.

  It also decides whether anything that reads the FILE can see the
  write. The agenda scan reads from disk, so an unsaved capture is
  invisible to a refresh no matter how correct the buffer is.

  **Only after a landed insert**, and after `cut` — the ordering
  `cut` already documents extends to this. A write that failed saves
  nothing, so the flag can never persist a half-applied effect.
  Bounded by the same `fs:write` grant as `path`: this reaches disk
  only where the guest could already have created the file.

#### record `apply-edit-payload`

```wit
record apply-edit-payload {
    target: u32,
    edit: edit,
    cursor: option<position>,
}
```

**Fields**

- `target`: `u32` — The target `BufferId` (its inner `u32`).
- `edit`: `edit`
- `cursor`: `option<position>` — Where to park the caret after the edit — a column-precise `position`
  (line + byte), so a plugin action can place it *between* an inserted
  pair, not only at a row start (AP.2). `none` leaves the caret put.

#### record `yank-payload`

```wit
record yank-payload {
    register: register,
    content: string,
    kind: yank-kind,
    explicit-yank: bool,
}
```

**Fields**

- `register`: `register`
- `content`: `string`
- `kind`: `yank-kind`
- `explicit-yank`: `bool` — `true` for an explicit yank (`y`/`yy`/Visual `y`); `false` for the
  register writes delete/change/`x` also perform. Drives the yank-only
  system-clipboard mirror (clipboard.md §5).

#### record `quit-payload`

```wit
record quit-payload {
    force: bool,
    scope: quit-scope,
}
```

#### record `open-buffer-payload`

```wit
record open-buffer-payload {
    path: option<string>,
    force: bool,
}
```

#### record `open-buffer-at-payload`

```wit
record open-buffer-at-payload {
    path: option<string>,
    position: position,
    force: bool,
    content: option<string>,
    activate-minor: option<string>,
}
```

**Fields**

- `path`: `option<string>`
- `position`: `position`
- `force`: `bool`
- `content`: `option<string>` — CD.2: seed text, applied only when the file is NOT on disk —
  reopening an existing file never replaces what is in it.
- `activate-minor`: `option<string>` — CD.2: a minor to activate alongside the major the path resolves,
  before the buffer is shown. The file-backed peer of
  `open-synthetic-buffer-payload`'s field, for OC.7a's reason.

#### record `open-buffer-at-column-payload`

```wit
record open-buffer-at-column-payload {
    path: option<string>,
    column: option<utf16-pos>,
    force: bool,
}
```

#### record `open-synthetic-buffer-payload`

```wit
record open-synthetic-buffer-payload {
    name: string,
    mode-id: string,
    content: option<string>,
    cursor: option<position>,
    activate-minor: option<string>,
}
```

OC.7a: `content` / `cursor` / `activate-minor` close a hole a guest
could not work around.

A native mode fills its own synthetic buffer from `on_activate`. The
`modes` seam is DECLARATION-ONLY — a guest exports `register-modes` and
nothing else — so a plugin mode has no such hook, and a guest that
emitted this effect got a buffer it could never put text in. The other
route, `effect.apply-edit`, needs the target's `buffer-id`, which this
effect does not hand back and which the guest cannot look up.

So the payload carries what the guest would otherwise have to write:
the text, where to leave the caret in it, and a minor to ride the
major. Every field is optional and omitting all three is the pre-OC.7a
behaviour exactly.

**Fields**

- `name`: `string`
- `mode-id`: `string` — The buffer's MAJOR mode.
- `content`: `option<string>` — Seed text, applied before the buffer is shown so the first frame is
  the finished one — a buffer that appears empty and fills a tick
  later is the content-jump the UX contract vetoes.

  `none` leaves the buffer empty (the mode fills it, or nothing does).
  Ignored when the buffer already existed: a re-open must not
  overwrite what the user has typed into it, which is the difference
  between reopening a capture and losing one.
- `cursor`: `option<position>` — Where to park the caret in `content` — org capture's `%?` point.
  Out of range is clamped rather than refused; a template whose `%?`
  sits past its own text is a template bug that must not cost the
  user the capture.
- `activate-minor`: `option<string>` — A minor to activate on the buffer alongside its major. Mirrors
  `spawn-terminal-payload.activate-minor`, and exists for the same
  reason: the interesting behaviour belongs to a minor that rides a
  general-purpose major. An org capture buffer IS an org buffer — it
  wants org's grammar, motions and folding — so its major is
  `org-mode` and only the `C-c C-c` / `C-c C-k` finalize/abort pair
  is capture-specific. Naming the minor here is what keeps those
  chords off every other org buffer.

#### record `spawn-terminal-payload`

```wit
record spawn-terminal-payload {
    cwd: option<string>,
    cmd-line: option<string>,
    env: list<tuple<string, string>>,
    activate-minor: option<string>,
}
```

**Fields**

- `cwd`: `option<string>` — PC.2: working directory to spawn in, overriding the active buffer's
  project root for this spawn only.

  `lattice_terminal::SpawnConfig` has carried a `cwd` since the
  terminal shipped ("`none` = inherit parent's cwd"); this is the
  boundary catching up, so a producer that knows WHICH project it
  means can say so. `none` keeps PR.3's behaviour exactly.
- `cmd-line`: `option<string>`
- `env`: `list<tuple<string, string>>`
- `activate-minor`: `option<string>`

#### record `echo-payload`

```wit
record echo-payload {
    level: echo-level,
    text: string,
}
```

#### record `substitute-payload`

```wit
record substitute-payload {
    scope: substitute-scope,
    pattern: string,
    replacement: string,
    global: bool,
}
```

#### record `describe-command-payload`

```wit
record describe-command-payload {
    name: string,
    anchor: option<string>,
}
```

#### record `open-picker-payload`

```wit
record open-picker-payload {
    source: string,
    args: list<string>,
    root: option<string>,
    fill-action: option<string>,
    query: option<string>,
}
```

**Fields**

- `source`: `string`
- `args`: `list<string>`
- `root`: `option<string>` — PC.1: the root this picker resolves against, overriding the active
  buffer's project for this open only.

  **Why the context and not an argument.** A `live` source (`grep`)
  re-queries through `on-query-changed`, which sees the query and the
  context and NOT the open's args — and a source is a shared generator
  with no per-open state. A root passed as an argument would apply to
  the first query and silently revert to the workspace root on the
  next keystroke, which is worse than not having it.

  `none` resolves from the active buffer, exactly as before.
- `fill-action`: `option<string>` — PC.11: this picker is being opened **to answer a question**, and
  the answer goes to the named ex-command as its first argument.

  `picker-accept-outcome`'s `fill-caller` already means "hand this
  value to whoever opened me". What it lacked was a destination a
  GUEST can own: the host's fill targets are the document, the `:`
  line, a prompt, a transient argument and another picker's query,
  and a plugin owns none of them. It does own an ex-command.

  `open-prompt-payload.on-submit-action` is this shape already, for
  this reason — the asymmetry between the two, where a guest could
  be handed a prompt's answer but not a picker's, is what this
  closes.

  **Not an override of the source's own accept.** A source decides
  what accepting one of its candidates means; `file-pick` and
  `dir-pick` exist as separate sources precisely so "supply a value"
  is the source's decision rather than the caller's. This names
  where such a value lands.

  `none` leaves the target as whatever surface was captured at open.
- `query`: `option<string>` — CD.6a: text the query starts with. For a static source it narrows the
  rows from the first frame (emacs's `completing-read` initial input);
  org-roam's node insert seeds it from the active region. Appended
  last, so earlier fields keep their positions.

#### record `set-lsp-log-level-payload`

```wit
record set-lsp-log-level-payload {
    server-id: option<string>,
    level: string,
}
```

#### record `diffsplit-payload`

```wit
record diffsplit-payload {
    path: string,
    remote: option<string>,
}
```

#### record `close-session-diffs-payload`

```wit
record close-session-diffs-payload {
    origin-session: u64,
    tab-name: string,
}
```

#### variant `viewport-pos`

```wit
variant viewport-pos {
    top,
    middle,
    bottom,
}
```

---- The `app-effect` variant mirror (PH7.3b2) ----
Mirrors `lattice_grammar::app_effect::AppEffect` — the App-side typed
effect carried by `Effect::AppAction` (chord-bound work with no grammar
concept: `<Esc>`, `<C-w>v`, `o`, …). Pure, flat, non-recursive data. One
arm is absent by design: `AppEffect::NarrowTrigger { range: Option<Range> }`
carries `lattice_grammar::range::Range`, which is recursive
(`RangeBound::Offset { base: Box<RangeBound> }`) and carries a plugin
`RangeId` — WIT cannot express it, so `NarrowTrigger` crosses as a typed
`WitBoundary` error until a range mirror lands (the `Global` precedent).
`NarrowLines` (pre-resolved line span) crosses fine.
Mirrors `lattice_grammar::app_effect::ViewportPos` (`H`/`M`/`L`).

#### variant `scroll-pos`

```wit
variant scroll-pos {
    top,
    center,
    bottom,
}
```

Mirrors `lattice_grammar::app_effect::ScrollPos` (`zt`/`zz`/`zb`).

#### variant `pane-direction`

```wit
variant pane-direction {
    left,
    down,
    up,
    right,
}
```

Mirrors `lattice_grammar::app_effect::PaneDirection`.

#### variant `insert-line-edit`

```wit
variant insert-line-edit {
    cursor-line-start,
    cursor-line-end,
    cursor-char-left,
    cursor-char-right,
    delete-word-backward,
    delete-to-line-start,
    kill-to-line-end,
    indent-line,
    dedent-line,
}
```

Mirrors `lattice_grammar::app_effect::InsertLineEdit` — the `<C-a>`,
`<C-e>`, `<C-b>`, `<C-f>`, `<C-w>`, `<C-u>`, `<C-k>`, `<C-t>`, `<C-d>`
readline/vim line-editing family within Insert mode.

#### variant `hscroll`

```wit
variant hscroll {
    columns(bool),
    half-screen(bool),
    cursor-to-edge(bool),
}
```

Mirrors `lattice_grammar::app_effect::HScroll` (vim `z{l,h,L,H,s,e}`).
Each arm's bool is the native struct field: `columns`/`half-screen`
carry `right`, `cursor-to-edge` carries `end`.

#### record `narrow-lines-payload`

```wit
record narrow-lines-payload {
    start-line: u32,
    end-line: u32,
}
```

Mirrors `AppEffect::NarrowLines` and `AppEffect::CreateFold`: a
pre-resolved inclusive 0-based line span.

#### enum `format-intent`

```wit
enum format-intent {
    indent,
    reflow,
    reformat,
}
```

RF.5b: which of the three formatting jobs a range wants done.

Mirrors `lattice_core::FormatIntent`. Separate values rather than
one "format" because they are not substitutable: an indent that
reflows is destructive, and a reformatter asked to reflow prose
mostly does nothing. See `docs/dev/architecture/text-reflow.md` §2.

**Cases**

- `indent` — Leading whitespace only (`=`).
- `reflow` — Line breaks within a paragraph (`gq` / `gw`).
- `reformat` — Anything the formatter likes (`:format`, `g=`).

#### record `format-range-payload`

```wit
record format-range-payload {
    intent: format-intent,
    start-line: u32,
    end-line: u32,
}
```

Mirrors `AppEffect::FormatRange` — an operator has resolved its
range and the buffer's chain says a non-native provider owns it.

#### record `open-provider-view-payload`

```wit
record open-provider-view-payload {
    provider: string,
    argument: option<string>,
    scan-args: list<string>,
}
```

AG.1: what `app-effect::open-provider-view` carries.

`provider` is the name a provider registered on the generic
provider-view seam (`"agenda"`, `"search"`).

`argument` is the **host-interpreted** parameter: a root for the agenda,
a query for search. One free-text string rather than the full recursive
`Args` shape a command handler receives, because mirroring that enum
here would cost a second args encoding on the boundary to express cases
no provider has. A native caller passing anything richer is refused with
a typed error rather than silently flattened, which is the
`NarrowTrigger` precedent.

`scan-args` (OA.11a) is the **guest-interpreted** one, passed through to
a scan source's `begin` verbatim and never read by the host.

Two slots because they have two owners, and conflating them breaks. The
host must understand `argument` — it does the walk, and it *replaces*
the source's roots with it. So a guest sending a command key down that
slot would set the scan root to a path that does not exist and quietly
cover nothing. `scan-args` is the channel for anything only the guest
can read.

Empty `scan-args` reproduces the pre-OA.11a boundary exactly: the
payload maps to `Args::None` / `Args::String`, so every existing trigger
is unchanged.

#### variant `app-effect`

```wit
variant app-effect {
    quit,
    match-bracket,
    toggle-case-at-cursor,
    open-line-below,
    open-line-above,
    search-next,
    search-previous,
    jump-history-back,
    jump-history-forward,
    pane-history-back,
    pane-history-forward,
    walk-mark-history-back,
    walk-mark-history-forward,
    tag-stack-pop,
    open-fold-at-cursor,
    close-fold-at-cursor,
    toggle-fold-at-cursor,
    open-all-folds,
    close-all-folds,
    cycle-fold-at-cursor,
    cycle-folds-global,
    goto-parent-fold,
    delete-fold-at-cursor,
    goto-next-fold,
    goto-prev-fold,
    toggle-fold-enable,
    open-folds-recursively,
    close-folds-recursively,
    delete-folds-recursively,
    undo,
    redo,
    repeat-last-change,
    page-down,
    page-up,
    half-page-down,
    half-page-up,
    scroll-line-up,
    scroll-line-down,
    redraw-screen,
    open-command-picker,
    enter-command-line,
    oil-navigate-up,
    reselect-last-visual,
    swap-visual-ends,
    paste-after,
    paste-before,
    enter-append,
    enter-insert-first-non-blank,
    enter-append-end-of-line,
    display-line-down,
    display-line-up,
    display-line-start,
    display-line-end,
    create-fold-from-visual,
    delete-char-backward,
    completion-trigger,
    exit-visual,
    replace-undo-last,
    enter-mode(modal-state),
    enter-visual(visual-kind),
    enter-select(visual-kind),
    enter-search(search-direction),
    search-word-under-cursor(search-direction),
    jump-viewport(viewport-pos),
    scroll-cursor-to(scroll-pos),
    horizontal-scroll(hscroll),
    insert-line-edit(insert-line-edit),
    join-lines(bool),
    find-repeat(bool),
    insert-newline,
    insert-tab,
    overwrite-char(char),
    set-mark(char),
    jump-to-mark-line(char),
    jump-to-mark-exact(char),
    select-register(register),
    start-macro-record(char),
    play-macro(char),
    play-last-macro,
    absorb-operator-prefix(u64),
    split-pane-horizontal,
    split-pane-vertical,
    close-pane,
    only-pane,
    toggle-zoom-pane,
    navigate-pane(pane-direction),
    next-pane,
    prev-pane,
    next-tab,
    prev-tab,
    go-to-tab(u32),
    new-tab,
    new-tab-at(string),
    terminal-spawn(option<string>),
    terminal-spawn-in-new-tab(option<string>),
    move-pane-to-new-tab,
    close-tab,
    only-tab,
    move-tab(u32),
    picker-accept-in-split,
    picker-accept-in-vsplit,
    picker-accept-in-tab,
    equalize-panes,
    grow-pane-height,
    shrink-pane-height,
    grow-pane-width,
    shrink-pane-width,
    completion-next,
    completion-prev,
    completion-accept,
    completion-cancel,
    completion-cancel-and-exit-insert,
    completion-toggle-docs,
    completion-docs-scroll-down,
    completion-docs-scroll-up,
    completion-accept-then-insert(char),
    snippet-next-placeholder,
    snippet-prev-placeholder,
    completion-filter-to-source(string),
    completion-filter-clear,
    diff-get,
    diff-put,
    tutor-advance,
    tutor-retreat,
    multibuffer-expand(s32),
    narrow-widen,
    narrow-lines(narrow-lines-payload),
    create-fold(narrow-lines-payload),
    format-range(format-range-payload),
    search-trigger(string),
    search-refresh,
    open-provider-view(open-provider-view-payload),
}
```

Mirrors `lattice_grammar::app_effect::AppEffect` (PH7.3b2). `NarrowTrigger`
is absent by design (recursive `Range`; see the note above).

**Cases**

- `quit`
- `match-bracket`
- `toggle-case-at-cursor`
- `open-line-below`
- `open-line-above`
- `search-next`
- `search-previous`
- `jump-history-back`
- `jump-history-forward`
- `pane-history-back`
- `pane-history-forward`
- `walk-mark-history-back`
- `walk-mark-history-forward`
- `tag-stack-pop`
- `open-fold-at-cursor`
- `close-fold-at-cursor`
- `toggle-fold-at-cursor`
- `open-all-folds`
- `close-all-folds`
- `cycle-fold-at-cursor`
- `cycle-folds-global`
- `goto-parent-fold`
- `delete-fold-at-cursor`
- `goto-next-fold`
- `goto-prev-fold`
- `toggle-fold-enable`
- `open-folds-recursively`
- `close-folds-recursively`
- `delete-folds-recursively`
- `undo`
- `redo`
- `repeat-last-change`
- `page-down`
- `page-up`
- `half-page-down`
- `half-page-up`
- `scroll-line-up`
- `scroll-line-down`
- `redraw-screen`
- `open-command-picker`
- `enter-command-line`
- `oil-navigate-up`
- `reselect-last-visual`
- `swap-visual-ends`
- `paste-after`
- `paste-before`
- `enter-append`
- `enter-insert-first-non-blank`
- `enter-append-end-of-line`
- `display-line-down`
- `display-line-up`
- `display-line-start`
- `display-line-end`
- `create-fold-from-visual`
- `delete-char-backward`
- `completion-trigger`
- `exit-visual`
- `replace-undo-last`
- `enter-mode`: `modal-state`
- `enter-visual`: `visual-kind`
- `enter-select`: `visual-kind`
- `enter-search`: `search-direction`
- `search-word-under-cursor`: `search-direction`
- `jump-viewport`: `viewport-pos`
- `scroll-cursor-to`: `scroll-pos`
- `horizontal-scroll`: `hscroll`
- `insert-line-edit`: `insert-line-edit`
- `join-lines`: `bool`
- `find-repeat`: `bool`
- `insert-newline`
- `insert-tab`
- `overwrite-char`: `char`
- `set-mark`: `char`
- `jump-to-mark-line`: `char`
- `jump-to-mark-exact`: `char`
- `select-register`: `register`
- `start-macro-record`: `char`
- `play-macro`: `char`
- `play-last-macro`
- `absorb-operator-prefix`: `u64`
- `split-pane-horizontal`
- `split-pane-vertical`
- `close-pane`
- `only-pane`
- `toggle-zoom-pane`
- `navigate-pane`: `pane-direction`
- `next-pane`
- `prev-pane`
- `next-tab`
- `prev-tab`
- `go-to-tab`: `u32`
- `new-tab`
- `new-tab-at`: `string`
- `terminal-spawn`: `option<string>`
- `terminal-spawn-in-new-tab`: `option<string>`
- `move-pane-to-new-tab`
- `close-tab`
- `only-tab`
- `move-tab`: `u32`
- `picker-accept-in-split`
- `picker-accept-in-vsplit`
- `picker-accept-in-tab`
- `equalize-panes`
- `grow-pane-height`
- `shrink-pane-height`
- `grow-pane-width`
- `shrink-pane-width`
- `completion-next`
- `completion-prev`
- `completion-accept`
- `completion-cancel`
- `completion-cancel-and-exit-insert`
- `completion-toggle-docs`
- `completion-docs-scroll-down`
- `completion-docs-scroll-up`
- `completion-accept-then-insert`: `char`
- `snippet-next-placeholder`
- `snippet-prev-placeholder`
- `completion-filter-to-source`: `string`
- `completion-filter-clear`
- `diff-get`
- `diff-put`
- `tutor-advance`
- `tutor-retreat`
- `multibuffer-expand`: `s32`
- `narrow-widen`
- `narrow-lines`: `narrow-lines-payload`
- `create-fold`: `narrow-lines-payload` — VM.3h: vim's `zf` operator. A closed fold over the span.
- `format-range`: `format-range-payload` — RF.5b: hand a resolved line range to the buffer's
  `format.{indent,reflow,reformat}` chain. Emitted by `=`, `gq`
  and `g=` when the winning rung is not `native`; the host runs
  the provider asynchronously and applies a minimal edit set.
- `search-trigger`: `string`
- `search-refresh`
- `open-provider-view`: `open-provider-view-payload` — AG.1: open a registered provider's multibuffer view by name.

  Withheld from this mirror until now, on the reasoning that letting a
  plugin open any registered provider by name is a capability question
  belonging with the host's capability model. The precedent had
  already answered it: `effect::open-picker` and
  `effect::open-transient` both let a guest open any registered source
  by name, ungated, and this is the same authority in the same shape.
  Withholding it did not withhold the capability — it only made the
  one seam that needed it borrow a host ex-command instead.

  What that borrowing cost is the reason this landed: a plugin whose
  trigger is a host command cannot name it. The agenda's `:agenda`
  therefore had a generic name for a feature every user calls
  `org-agenda`, and the plugin could not fix that from its own side.

#### variant `effect`

```wit
variant effect {
    none,
    declined,
    edits(list<applied-edit>),
    apply-edit(apply-edit-payload),
    write-to-file(write-to-file-payload),
    selection-change(selection-set),
    cursor-move(position),
    confirm(confirm-payload),
    open-prompt(open-prompt-payload),
    open-transient(open-transient-payload),
    yank(yank-payload),
    enter-mode(modal-state),
    save-buffer(option<string>),
    quit-editor(quit-payload),
    open-buffer(open-buffer-payload),
    open-buffer-at(open-buffer-at-payload),
    open-external-uri(string),
    open-buffer-at-column(open-buffer-at-column-payload),
    spawn-terminal(spawn-terminal-payload),
    terminal-input(list<u8>),
    set-option(string),
    set-local-option(string),
    set-global-option(string),
    clear-search-highlight,
    set-colorscheme(string),
    echo(echo-payload),
    show-diagnostics-popup(list<tuple<string, u8>>),
    lsp(lsp-request),
    echo-registers,
    echo-marks,
    substitute(substitute-payload),
    delete-current-line,
    describe-command(describe-command-payload),
    describe-buffer,
    apropos(string),
    describe-key(string),
    list-keymap,
    buffer-next,
    buffer-prev,
    list-buffers,
    open-buffer-picker,
    open-picker(open-picker-payload),
    buffer-delete(bool),
    open-file-tree(option<string>),
    close-file-tree,
    open-oil(option<string>),
    describe-option(string),
    describe-element(string),
    list-options,
    describe-plugin-api(option<string>),
    list-plugin-apis,
    export-plugin-api(option<string>),
    list-commands,
    describe-plugin(string),
    list-plugins,
    open-hover(string),
    dismiss-popup,
    dismiss-popup-named(string),
    open-popup(open-popup-payload),
    open-help-topic(option<string>),
    list-diagnostics,
    next-diagnostic,
    prev-diagnostic,
    open-lsp-log(option<string>),
    open-messages,
    open-dashboard,
    toggle-lsp-trace(string),
    open-lsp-trace-log(option<string>),
    lsp-status,
    lsp-server-log-listing,
    lsp-restart(string),
    lsp-progress-cancel(option<string>),
    lsp-expand-region,
    lsp-shrink-region,
    set-lsp-log-level(set-lsp-log-level-payload),
    lsp-log-clear(option<string>),
    lsp-document-symbol,
    lsp-workspace-symbol(string),
    lsp-incoming-calls,
    lsp-outgoing-calls,
    lsp-supertypes,
    lsp-subtypes,
    lsp-moniker,
    lsp-code-lens,
    lsp-color-presentation,
    lsp-format,
    lsp-format-range,
    lsp-signature-help,
    lsp-complete,
    lsp-rename(string),
    lsp-code-action,
    expand-snippet(range),
    reload-snippets,
    describe-events,
    describe-diff,
    diff-open,
    diff-off(bool),
    diffthis,
    diffsplit(diffsplit-payload),
    diff-get-cmd(option<u32>),
    diff-put-cmd(option<u32>),
    diff-accept,
    diff-reject,
    diff-accept-all,
    diff-reject-all,
    close-session-diffs(close-session-diffs-payload),
    close-all-session-diffs(u64),
    next-hunk,
    prev-hunk,
    describe-event(string),
    list-modes,
    describe-mode(string),
    describe-active-modes,
    describe-active-bindings,
    describe-option-resolution(string),
    customize(option<string>),
    tutor(option<u32>),
    toggle-mode(string),
    app-action(app-effect),
    record-jump,
    open-ai-log(option<string>),
    open-synthetic-buffer(open-synthetic-buffer-payload),
    focus-buffer(u32),
    invoke-command(command-ref),
}
```

Mirrors `lattice_grammar::effect::Effect` (§4.4). Every arm is pure data;
`Many`/`Global`/`AppAction` are absent by design (see the note above).

**Cases**

- `none`
- `declined` — AP.0.2: the action DECLINES the chord (it did nothing) — the
  dispatcher re-resolves as if this action's keymap layer weren't there,
  falling through to the next binding. Distinct from `none` (a no-op
  that consumes the chord). A guest returns `[declined]` to fall through.
- `edits`: `list<applied-edit>`
- `apply-edit`: `apply-edit-payload`
- `write-to-file`: `write-to-file-payload` — XF.4: move text into another file. See `write-to-file-payload`.
- `selection-change`: `selection-set`
- `cursor-move`: `position`
- `confirm`: `confirm-payload` — IX.3: ask the user a yes/no question, then dispatch an action.
  Available to plugins because asking the user something is
  table stakes, not an advanced capability.
- `open-prompt`: `open-prompt-payload` — IX.5: ask the user for a line of text, then dispatch an
  action with it. The other half of "a plugin can collect
  input" — without it a guest can only ask yes/no questions.
- `open-transient`: `open-transient-payload` — IX.6: open a named transient menu. The payload names the
  *source* the owning crate registered with the
  `TransientSourceRegistry`, not a menu structure — the menu is
  built host-side from that registration, so a guest opens its
  own menu by naming it rather than by shipping a spec across
  on every press.

  TR.3a: it also carries the ARGUMENTS the open was requested
  with, which reach the builder as `transient-context.args`.
  Without them a menu cannot drill down — a row that opens a
  second menu has no way to say what it opened it FOR, and the
  builder would have to keep the answer in guest memory where
  nothing clears it.
- `yank`: `yank-payload`
- `enter-mode`: `modal-state`
- `save-buffer`: `option<string>`
- `quit-editor`: `quit-payload`
- `open-buffer`: `open-buffer-payload`
- `open-buffer-at`: `open-buffer-at-payload`
- `open-external-uri`: `string`
- `open-buffer-at-column`: `open-buffer-at-column-payload`
- `spawn-terminal`: `spawn-terminal-payload`
- `terminal-input`: `list<u8>`
- `set-option`: `string`
- `set-local-option`: `string`
- `set-global-option`: `string`
- `clear-search-highlight`
- `set-colorscheme`: `string`
- `echo`: `echo-payload`
- `show-diagnostics-popup`: `list<tuple<string, u8>>`
- `lsp`: `lsp-request`
- `echo-registers`
- `echo-marks`
- `substitute`: `substitute-payload`
- `delete-current-line`
- `describe-command`: `describe-command-payload`
- `describe-buffer`
- `apropos`: `string`
- `describe-key`: `string`
- `list-keymap`
- `buffer-next`
- `buffer-prev`
- `list-buffers`
- `open-buffer-picker`
- `open-picker`: `open-picker-payload`
- `buffer-delete`: `bool`
- `open-file-tree`: `option<string>`
- `close-file-tree`
- `open-oil`: `option<string>`
- `describe-option`: `string`
- `describe-element`: `string`
- `list-options`
- `describe-plugin-api`: `option<string>` — PI.2: plugin-API introspection help effects.
- `list-plugin-apis`
- `export-plugin-api`: `option<string>`
- `list-commands`
- `describe-plugin`: `string`
- `list-plugins`
- `open-hover`: `string`
- `dismiss-popup`
- `dismiss-popup-named`: `string` — Dismiss the popup only if it is the named one; a no-op otherwise.
  `dismiss-popup` is the user's verb ("close what I am looking at");
  this is the one a mode uses when it dismisses on its own schedule,
  where the slot may hold someone else's popup by the time the effect
  lands. See `Effect::DismissPopupNamed` for the bug that motivated it.
- `open-popup`: `open-popup-payload`
- `open-help-topic`: `option<string>`
- `list-diagnostics`
- `next-diagnostic`
- `prev-diagnostic`
- `open-lsp-log`: `option<string>`
- `open-messages`
- `open-dashboard`
- `toggle-lsp-trace`: `string`
- `open-lsp-trace-log`: `option<string>`
- `lsp-status`
- `lsp-server-log-listing`
- `lsp-restart`: `string`
- `lsp-progress-cancel`: `option<string>`
- `lsp-expand-region`
- `lsp-shrink-region`
- `set-lsp-log-level`: `set-lsp-log-level-payload`
- `lsp-log-clear`: `option<string>`
- `lsp-document-symbol`
- `lsp-workspace-symbol`: `string`
- `lsp-incoming-calls`
- `lsp-outgoing-calls`
- `lsp-supertypes`
- `lsp-subtypes`
- `lsp-moniker`
- `lsp-code-lens`
- `lsp-color-presentation`
- `lsp-format`
- `lsp-format-range`
- `lsp-signature-help`
- `lsp-complete`
- `lsp-rename`: `string`
- `lsp-code-action`
- `expand-snippet`: `range`
- `reload-snippets`
- `describe-events`
- `describe-diff`
- `diff-open`
- `diff-off`: `bool`
- `diffthis`
- `diffsplit`: `diffsplit-payload`
- `diff-get-cmd`: `option<u32>`
- `diff-put-cmd`: `option<u32>`
- `diff-accept`
- `diff-reject`
- `diff-accept-all`
- `diff-reject-all`
- `close-session-diffs`: `close-session-diffs-payload`
- `close-all-session-diffs`: `u64`
- `next-hunk`
- `prev-hunk`
- `describe-event`: `string`
- `list-modes`
- `describe-mode`: `string`
- `describe-active-modes`
- `describe-active-bindings`
- `describe-option-resolution`: `string`
- `customize`: `option<string>`
- `tutor`: `option<u32>`
- `toggle-mode`: `string`
- `app-action`: `app-effect`
- `record-jump`
- `open-ai-log`: `option<string>`
- `open-synthetic-buffer`: `open-synthetic-buffer-payload`
- `focus-buffer`: `u32` — CD.1: show the buffer with this id in the active pane. The peer of
  `apply-edit`'s `target`: a guest can edit a buffer it knows only by
  id, and this shows one. An id that no longer names a buffer is a
  no-op. Appended last so existing variant indices do not move.
- `invoke-command`: `command-ref` — CD.3d: run a registered command — an action with typed args, else
  an ex line — AFTER the effects before it in the same batch. A
  write-to-file that does not land stops the batch, so work that must
  follow a successful write (deleting what was filed) goes here rather
  than in a host call, which runs before any effect is applied.

#### variant `arg-kind`

```wit
variant arg-kind {
    string,
    char,
    bool,
    int,
    pattern,
    chord,
    body,
    raw,
}
```

---- The picker-source seam (PH7.4a, §4.2 / §5 `picker-source`) ----
Mirrors the data types a plugin picker source authors against:
`PickerSourceSpec` (+`ArgSpec`), `RoutingPayload`, `OpenTarget`, and the
owned `PickerContext` projection the host hands `init`. This is the
plugin-facing API (the user's "expose the api, not sources"): native
sources stay native Rust; a plugin implements a source against these types
and registers through the same `PickerRegistry::register_generator` seam.

The active buffer's bulk rope text and syntax-highlight overlay do NOT ride
`active-buffer-snapshot`; they cross via the `buffer` `document` resource
handle, wired with the `init(ctx)` guest export at PH7.4c.
Mirrors `lattice_grammar::args::ArgKind`.

#### variant `arg-default`

```wit
variant arg-default {
    required,
    none,
    literal(arg-value),
    use-selection,
    use-cursor-word,
    use-last-response,
}
```

Mirrors `lattice_grammar::args::ArgDefault`. `literal` reuses `arg-value`.

#### record `arg-spec`

```wit
record arg-spec {
    name: string,
    kind: arg-kind,
    doc: string,
    prompt: string,
    default: arg-default,
    completion: option<string>,
    picker: option<string>,
}
```

Mirrors `lattice_grammar::args::ArgSpec`. Native `name`/`doc`/`prompt`/
`completion` are `&'static str`; the host adapter interns the plugin's
owned strings at registration (`Box::leak`, bounded by loaded-source count
— see the slice plan note on hot-reload).

**Fields**

- `name`: `string`
- `kind`: `arg-kind`
- `doc`: `string`
- `prompt`: `string`
- `default`: `arg-default`
- `completion`: `option<string>` — A registered COMPLETION source (`gen:files`, ...) — inline
  candidates as the user types this argument.
- `picker`: `option<string>` — YR.6: a registered PICKER source offered for this argument.

  Two fields because they name two registries, which is a
  decision rather than an oversight: a completion source is
  engine-shaped, a picker source is surface-shaped and needs
  `PickerContext`. An argument may set both — `<Tab>` completes
  inline, `<C-x><C-o>` opens the picker on the same question.

#### record `multibuffer-view-excerpt`

```wit
record multibuffer-view-excerpt {
    path: string,
    start-line: u32,
    end-line: u32,
    header: string,
    match-count: option<u32>,
}
```

Mirrors `lattice_picker::source::PickerSourceSpec`. `live` opts the source
out of the picker's fuzzy refilter (the source owns filtering).
─── MV.1: plugin-owned multibuffer views ────────────────────────
Design: `docs/dev/architecture/plugin-multibuffer-views.md`.
One row of a plugin-owned multibuffer view.

`path` names a FILE, not a buffer id. `Excerpt` carries a `BufferId`
and only the host can mint one — so the host opens (or reuses) the
document and adds it as this excerpt's source. A path is the stable
name both sides already share; `effect::write-to-file` resolves paths
to buffers for the same reason.

**Fields**

- `path`: `string`
- `start-line`: `u32` — 0-based, inclusive of `end-line`, matching `scanned-excerpt`.
- `end-line`: `u32`
- `header`: `string` — Rendered above this excerpt. **Empty renders no header row**, which
  is the entire grouping mechanism: a group is "title on the first
  excerpt of a run, empty on the rest". A pull guest computes the runs
  itself, because unlike a scan guest it can see the whole ordered set.
- `match-count`: `option<u32>` — The `· N matches` badge beside the header. `none` ⇒ no badge.

#### variant `multibuffer-view-input`

```wit
variant multibuffer-view-input {
    pull,
    scan,
}
```

Where a view's rows come from. Declared on the spec rather than per
`build` call: the host must know BEFORE it calls anything, because a
scan view needs the walk driven and a pull view needs `build` invoked.
Per-call, the host would have to call `build` to learn it should not
have. It also matches the seam next door, where `extensions` and
`view-mode` are load-time facts and only `roots` is per-scan.

**Cases**

- `pull` — The guest already knows the answer — an index lookup, a computed
  set. The host calls `build` and renders what comes back.
- `scan` — The host walks, reads and parses; the guest classifies each file
  through `scanned-excerpt-source`.

  **No payload, deliberately.** An earlier draft carried the file
  extensions here and that was a second source of truth: a scan source
  already declares its own `extensions()`, and the walk reads them
  from the live sources. Two places to say it is one place to get it
  wrong.

  Kept as a separate input rather than folded into `pull` because the
  two are different COST MODELS, not two spellings of one. The host
  must read each file anyway to build the source document, so it reads
  once, parses once, and hands over text *and* tree: a 1-2 ms parse for
  a 217 ns copy (`benches/agenda_scan_input.rs`), and the guest needs
  no filesystem capability at all. A pull-only world makes the guest
  discover and read files itself, and reading node text back through
  the tree seam costs ~50 us per file — 200x the copy it avoided.

#### record `multibuffer-view-spec`

```wit
record multibuffer-view-spec {
    id: string,
    doc: string,
    buffer-name: string,
    view-mode: option<string>,
    reuse: bool,
    input: multibuffer-view-input,
}
```

A view a plugin owns.

**Fields**

- `id`: `string` — The provider name. `app-effect::open-provider-view` and the view's
  own `gr` both reach it by this.
- `doc`: `string` — Shown in `:describe-command` and the provider listing.
- `buffer-name`: `string` — The view's buffer name — the GUEST's to choose (`*agenda*`,
  `*org-roam-backlinks*`). Host constants are what made the agenda's
  identity un-ownable.
- `view-mode`: `option<string>` — A minor mode to activate on the view, by name. This is where the
  view's chords and their handler bodies live, and it is a property of
  the VIEW rather than of how its rows were found. A name that is not
  registered warns through the ordinary activation path rather than
  failing the open — the rows are still worth showing.
- `reuse`: `bool` — Reuse one buffer across triggers (a second `:agenda` re-scans into
  the same buffer) or open a fresh view each time.
- `input`: `multibuffer-view-input`

#### record `multibuffer-view-result`

```wit
record multibuffer-view-result {
    excerpts: list<multibuffer-view-excerpt>,
    summary: string,
}
```

What `build` returns.

**Fields**

- `excerpts`: `list<multibuffer-view-excerpt>` — In FINAL order — see `multibuffer-view-source.build`.
- `summary`: `string` — The headerline's terminal summary ("42 backlinks"). Returned with
  the rows rather than fetched by a second call: one crossing, and the
  count is a fact the guest already has.

#### record `picker-source-spec`

```wit
record picker-source-spec {
    id: string,
    doc: string,
    args-schema: list<arg-spec>,
    args-hint: string,
    live: bool,
    create-label: option<string>,
    rooted: bool,
    delete-command: option<string>,
}
```

**Fields**

- `id`: `string`
- `doc`: `string`
- `args-schema`: `list<arg-spec>`
- `args-hint`: `string`
- `live`: `bool`
- `create-label`: `option<string>` — OR.5: when set, the picker offers one synthetic **create** row
  whenever the query is non-empty — the offer to make the thing the
  user was looking for and did not find. `%s` in the label is replaced
  by the query.

  Two things about the row are load-bearing and neither is obvious.
  It appears whenever the query is non-empty, NOT only when nothing
  matches: offering it only on zero matches makes it impossible to
  create *Rust* while *Rust Async* exists, which is precisely when you
  most want to. And it is pinned last and never ranked, because a
  create row that could sort above a real match would let `<CR>`
  produce a duplicate through ranking noise — destructive rather than
  merely wrong.

  Accepting it hands `routing-payload::create(query)` to this source's
  `accept`, which decides what creation means. `none` — every source
  but roam's — behaves exactly as before.
- `rooted`: `bool` — PP.2: these results are scoped to a project / workspace root, so the
  picker prompt names the root it is operating on
  (`files ~/src/lattice> `).

  A DECLARATION, not an inference. The host resolves a root for every
  picker open — `picker-context.workspace-root` is always filled — so
  it could show one everywhere, and a path on a list that spans every
  open project is noise on the one line the user reads to know what
  they are looking at. Only the source knows whether the root is part
  of what its list MEANS.

  Set it when the answer to *"would these results be different in
  another project?"* is yes. `false` is the answer for a list that is
  global, buffer-local, or registry-wide — and for a source whose
  QUERY already names a path, where a root beside it would be a
  second and staler answer to the same question.
- `delete-command`: `option<string>` — PD.1: `<C-d>` — the ex-command that REMOVES the selected row from
  whatever backs this list, invoked with that row's routing argument.
  `none` leaves `<C-d>` doing nothing.

  The SOURCE owns the verb and the host owns only the key. `<C-s>` /
  `<C-v>` / `<C-t>` are host concerns — it knows how to open a thing
  in a split without asking. Deletion is not: only the source knows
  that removing a row from a project list means forgetting a root.
  Naming a command is how a source says so, and it is the same
  routing its rows already take, so declaring this needs no new seam
  and no new capability.

  **Not destructive to the filesystem, and must not be.** A project
  list forgets a path; deleting a directory is oil's job and the file
  tree's. A source whose delete verb touched disk would make `<C-d>`
  mean two very different things depending on which picker had focus.

#### record `generate-context`

```wit
record generate-context {
    prefix: string,
    case-sensitive: bool,
    line-before-cursor: string,
    language: string,
}
```

Mirrors the crossable core of `lattice_completion::traits::GenerateContext`
(PH7.6). `buffer` (`&Buffer`) + `registry` (`&CommandRegistry`) do NOT
cross — a plugin generator that needs buffer text waits for the `document`
handle (the picker-source precedent); v1 carries the query prefix + the
case-sensitivity flag, which is all a produce-then-`match_and_rank`
generator needs (matching stays native — the sync-pipeline + paramount-#1
reason a plugin completion source is a GENERATOR, not four trait objects).
OR.7 widened this. `prefix` alone cannot answer *whether a source
applies* — org-roam's node source offers link targets only inside
`[[…]]`, and the anchor scan stops at `[`, so the prefix it receives
is indistinguishable from an ordinary word. The alternative was a
host-side `link-context` flag beside `path-context`, i.e. teaching
the host one plugin's syntax; `line-before-cursor` lets every source
answer for itself and the host stay ignorant.

**Fields**

- `prefix`: `string`
- `case-sensitive`: `bool`
- `line-before-cursor`: `string` — The cursor's line from its start up to the cursor, verbatim.
  The replacement region is still `[anchor, cursor]` — a source
  whose insert would cover more than the prefix must check that
  this string ends with its own opener plus `prefix`, and decline
  otherwise, or it will splice over the wrong span.
- `language`: `string` — The buffer's language id (`"org"`, `"rust"`, …); empty when no
  language is detected. A source registered by a plugin is offered
  to every buffer, so this is how it scopes itself to its own.

#### record `completion-source-spec`

```wit
record completion-source-spec {
    id: string,
    doc: string,
    accepts-non-word-query: bool,
}
```

A completion source's identity — the `(name, doc)` pair `insert_generator`
stamps (`registry.rs`). Mirrors nothing structural; the host interns the
owned strings at registration like `picker-source-spec`.

**Fields**

- `id`: `string`
- `doc`: `string`
- `accepts-non-word-query`: `bool` — OR.7: keep the popup open when the query picks up a non-word
  character. Default behaviour dismisses there — right for
  identifier completion, wrong for a source completing phrases
  (org-roam node titles contain spaces, and the popup used to
  close at the first one).

#### variant `open-target`

```wit
variant open-target {
    default,
    split,
    vsplit,
    tab,
}
```

Mirrors `lattice_picker::outcome::OpenTarget` (`<CR>`/`<C-s>`/`<C-v>`/`<C-t>`).

#### record `resolve-diff-payload`

```wit
record resolve-diff-payload {
    primary: u32,
    accept: bool,
}
```

Payload records for the multi-field `routing-payload` arms. `lsp-location`
and `jump-in-buffer` reuse the shared `location` / `jump-target` records;
`invoke-command` reuses `command-ref`.

#### record `lsp-instance-payload`

```wit
record lsp-instance-payload {
    server-id: string,
    workspace: string,
}
```

#### record `show-message-action-payload`

```wit
record show-message-action-payload {
    request-id: u32,
    action-index: u32,
}
```

#### record `ai-session-payload`

```wit
record ai-session-payload {
    provider: string,
    index: u32,
}
```

Mirrors `lattice_picker::RoutingPayload::AiSession` — the
`(provider, index)` key for opening the per-session AI log buffer.

#### variant `routing-payload`

```wit
variant routing-payload {
    buffer(u32),
    resolve-diff(resolve-diff-payload),
    lsp-instance(lsp-instance-payload),
    lsp-location(location),
    lsp-completion(u32),
    lsp-code-action(u32),
    open-file(string),
    jump-in-buffer(jump-target),
    invoke-command(command-ref),
    paste-register(char),
    jump-to-mark(char),
    expand-snippet(string),
    accept-show-message-action(show-message-action-payload),
    lsp-code-lens(u32),
    color-presentation(u32),
    colorscheme(string),
    ai-session(ai-session-payload),
    pane-history-entry(u32),
    file-location(location),
    create(string),
}
```

Mirrors `lattice_picker::RoutingPayload` — the opaque token a source emits
per candidate and consumes in `accept`. All flat pure data; paths cross as
strings (a non-UTF-8 path is a typed error, §4.4).

**Cases**

- `buffer`: `u32`
- `resolve-diff`: `resolve-diff-payload`
- `lsp-instance`: `lsp-instance-payload`
- `lsp-location`: `location`
- `lsp-completion`: `u32`
- `lsp-code-action`: `u32`
- `open-file`: `string`
- `jump-in-buffer`: `jump-target`
- `invoke-command`: `command-ref`
- `paste-register`: `char`
- `jump-to-mark`: `char`
- `expand-snippet`: `string`
- `accept-show-message-action`: `show-message-action-payload`
- `lsp-code-lens`: `u32`
- `color-presentation`: `u32`
- `colorscheme`: `string`
- `ai-session`: `ai-session-payload`
- `pane-history-entry`: `u32`
- `file-location`: `location` — OR.6: a place in a file on disk. The peer `picker-accept-outcome`'s
  `jump-to-location` already had and this side lacked — without it a
  row standing for a position in a file can only carry `open-file`,
  which drops the line and lands at the top. Distinct from
  `lsp-location`, which is the same shape under a name that says where
  it came from.
- `create`: `string` — OR.5: the query the user typed, carried by the picker's synthetic
  create row. Verbatim — spaces and non-ASCII included — because the
  source is creating something the USER named, and trimming here would
  be the picker having an opinion about a namespace it does not own.

#### record `buffer-entry`

```wit
record buffer-entry {
    id: u32,
    kind-label: string,
    path: option<string>,
    title: string,
    dirty: bool,
}
```

---- The owned `PickerContext` projection (§4.2) ----
Host→guest only: the host projects live borrows into these owned records at
`init` time; the guest never sends a context back. So these mirror one-way
(a `project_picker_context` fn, the `project_buffer_snapshot` precedent),
no `from_wit`.
Mirrors `lattice_picker::context::BufferEntry`. `kind-label` is a display
string — the picker seam stays oblivious to `BufferKind` (CLAUDE.md rule).

#### variant `position-source`

```wit
variant position-source {
    auto-jump,
    explicit-mark,
    plugin-push,
    named-mark(char),
}
```

Mirrors `lattice_picker::context::PositionSource`.

#### record `position-entry`

```wit
record position-entry {
    buffer-id: u32,
    line: u32,
    col: u32,
    source: position-source,
}
```

Mirrors `lattice_picker::context::PositionEntry`.

#### record `symbol-location`

```wit
record symbol-location {
    name: string,
    line: u32,
    col: u32,
}
```

One tree-sitter symbol location `(name, line, byte-col)` — the owned form
of `ActiveBufferSnapshot::syntax_symbols`.

#### record `active-buffer-snapshot`

```wit
record active-buffer-snapshot {
    buffer-id: u32,
    path: option<string>,
    language: option<string>,
    cursor: position,
    selection: option<tuple<position, position>>,
    syntax-symbols: list<symbol-location>,
}
```

Mirrors `lattice_picker::context::ActiveBufferSnapshot` (metadata only).
`buffer` (the rope) + `syntax_highlights` are deferred to the `document`
resource wiring (PH7.4c); a fuzzy-finder needs neither.

#### record `picker-context`

```wit
record picker-context {
    active-buffer: active-buffer-snapshot,
    workspace-root: string,
    recent-files: list<string>,
    position-history: list<position-entry>,
    buffers: list<buffer-entry>,
    marks: list<tuple<char, position>>,
    registers: list<tuple<string, string>>,
}
```

Mirrors `lattice_picker::context::PickerContext` (the owned projection).

#### record `transient-context`

```wit
record transient-context {
    major-mode: option<string>,
    minor-modes: list<string>,
    buffer: option<u32>,
    args: args,
}
```

---- The transient seam (TR.2b, plugin-transients.md §5) ----
A transient is a keyed menu: one keystroke per row, fires and closes.
The mechanism is the picker's; these are the owned mirrors a plugin
authors a menu against.
Mirrors `lattice_picker::TransientContext` — where the menu was opened
from, so a builder can vary its rows. Host→guest only (a `project_*`
fn, the `picker-context` precedent); the guest never sends one back.

Deliberately NOT the cursor or the selection: a builder produces rows,
it does not act. Each row's action receives its own `ActionContext` at
FIRE time, when those are current.

**Fields**

- `major-mode`: `option<string>` — The active major's id, if the buffer has one. Emacs magit's
  `:if-mode` question.
- `minor-modes`: `list<string>` — The active minor ids — the looser `:if-derived` family test. A
  separate field from `major-mode` on purpose: a flat list of active
  mode ids can only answer one of the two questions.
- `buffer`: `option<u32>` — The buffer the menu was opened over. `none` mid-boot, where a
  builder degrades exactly as it does for the mode fields.
- `args`: `args` — TR.3a: the arguments the open carried
  (`effect::open-transient`'s payload).

  This is what lets a menu DRILL DOWN: org's capture menu has a
  row per template, and the fields menu that row opens needs to
  know which template it is collecting for. The alternative is
  guest memory, which `<Esc>` never clears — the next open would
  inherit the last one's subject.

#### record `transient-action`

```wit
record transient-action {
    command: string,
    args: args,
}
```

An action row's target: a command **name** plus the arguments this
particular row fires it with.

A name, not an id, because a `CommandId` is host-issued and a plugin
must not be able to forge one — the host resolves the name against the
`CommandRegistry` at build time, and an unresolvable name drops that
row rather than failing the menu.

The args are per-ROW, which is the whole reason the slot exists: the
menu-wide `TransientState` projection cannot distinguish rows that
differ only in a parameter, and that is the shape a plugin menu has
(one row per capture template). `args::none` for a row that wants the
state projection instead, which is what every native row does.

#### record `transient-argument`

```wit
record transient-argument {
    name: string,
    default: option<string>,
    prompt: string,
}
```

TR.3b: a field the menu collects before anything fires.

Pressing its key PARKS the whole menu, opens a one-line prompt, writes
the answer into the menu's state under `name`, and puts the menu back —
`<Esc>` cancels the value with the menu untouched. That mechanism is
the host's (`PendingTransientArgument` → `resume_parked_transient`) and
magit's argument rows already use it; this record is only what lets a
guest declare one.

It is what makes a template's `%^{Question}` expressible: several named
answers collected before one write, with the menu as the surface
throughout rather than a run of prompts the user cannot go back into.

**Fields**

- `name`: `string` — Key in the menu's state this answer lands under. Also what names
  it when the fired row's command has no `args-schema` — the rows'
  order is then the schema.
- `default`: `option<string>` — Pre-filled on first ask; `none` for an empty field. A re-edit
  always seeds with the value already held.
- `prompt`: `string` — The prompt's label.

#### variant `transient-item-kind`

```wit
variant transient-item-kind {
    action(transient-action),
    argument(transient-argument),
    dismiss,
}
```

Mirrors `lattice_picker::TransientItemKind`. v1 crosses three of its
six variants; `submenu` / `flag` / `variable` are deferred with reasons
in `plugin-transients.md` §5.

**Cases**

- `action`: `transient-action` — Fires a command and closes the menu.
- `argument`: `transient-argument` — Collects a named value into the menu's state (TR.3b).
- `dismiss` — Closes the menu without firing anything. Free, and a menu with no
  `q` is a trap.

#### record `transient-item`

```wit
record transient-item {
    key: list<string>,
    label: string,
    description: string,
    kind: transient-item-kind,
}
```

One row. `key` is a list of STRINGS, not chars: magit binds multi-key
rows (`, k`, `= f`) and the resolver walks them one keystroke at a
time, so a plugin gets the same expressiveness.

#### record `transient-group`

```wit
record transient-group {
    label: string,
    items: list<transient-item>,
}
```

A named group of rows — one header, its items, one separator.

#### record `transient-spec`

```wit
record transient-spec {
    title: string,
    groups: list<transient-group>,
    footer: option<string>,
}
```

Mirrors `lattice_picker::TransientSpec`, minus `preview`: that is a
`Box<dyn Fn(&TransientState) -> String>` and a closure has no WIT
form, so a guest-built menu has no live preview pane. Stated here
rather than discovered at bindgen.

#### type `count`

```wit
type count = u32;
```

---- The grammar-extension seam (PH7.7, §4.1) ----
Mirrors the data types a plugin authors against when it EXTENDS the vim
grammar via `register_{motion,operator,text_object,ex_command,action}`.
The grammar *handling* (dispatcher, `:`-line + chord parser, operator∘
motion composition, ranges, counts, registers) stays native, sync, and
untouched — a plugin only CONTRIBUTES entries through these types; it can
neither observe nor reimplement dispatch. A plugin-registered command
lands in the same `CommandRegistry` via the same `register_*` path,
stamped `SourceLayer::Plugin(id)`, so it is indistinguishable from a
builtin (paramount #3).

Direction: each *context* is a one-way host→guest projection of the
dispatch environment (a `project_*` fn, the `project_picker_context`
precedent — no `from_wit`); the *result*/*effect*/*args* come back
guest→host. Bulk buffer text never rides a context — it crosses via the
`buffer` `document` resource handle (§4.2). The tree-sitter env
(`scope_resolver` / `comment_syntax`) is host-owned trait objects a v1
grammar plugin reaches through that handle, deferred like the picker's
syntax overlay.

The `apply` / `parse_args` closures are NOT fields on the spec records:
the behavior is a sync guest export the host calls back by callback-id
(PH7.7b/c), so each spec mirrors its native `*Spec` struct with the
closure field dropped. `name` / `doc` are `register_*` arguments (PH7.7b),
not spec fields, matching the native imperative API.
Mirrors `lattice_grammar::command::Count` — a repeat count. `Count(1)` is
the bare invocation; `has-explicit-count` on the motion context
disambiguates `G` from `1G`.

#### enum `latency-class`

```wit
enum latency-class {
    reflex,
    display,
    background,
}
```

Mirrors `lattice_grammar::command::LatencyClass` (§5.2.5 budget class).

#### variant `surface-form`

```wit
variant surface-form {
    keyword,
    delimiter(string),
}
```

Mirrors `lattice_grammar::registry::SurfaceForm`. `delimiter` carries the
canonical-syntax `hint` shown when the keyword form is (deliberately) a
hard error (`:s/pat/repl/`, `:g/pat/body`).

#### record `motion-context`

```wit
record motion-context {
    buffer-id: u32,
    from: position,
    count: count,
    has-explicit-count: bool,
    args: args,
}
```

Mirrors `lattice_grammar::registry::MotionContext` (owned projection).
`from` is the cursor the motion evaluates from; `buffer-id` is the active
buffer's registry identity (mode-state lookups). Buffer text + the
tree-sitter resolver ride the `document` handle, not this record.

#### record `motion-result`

```wit
record motion-result {
    target: position,
    linewise: bool,
}
```

Mirrors `lattice_grammar::registry::MotionResult`. `linewise` expands the
resolved range to whole lines.

#### record `operator-context`

```wit
record operator-context {
    buffer-id: u32,
    range: range,
    linewise: bool,
    register: register,
    count: count,
    args: args,
}
```

Mirrors `lattice_grammar::registry::OperatorContext` (owned projection).
The `&mut Document` is NOT a field — document mutation is expressed by the
returned `effect` (§4.5), and the operator reads text through the
`document` handle. `range` is the operator's target span (the
`lattice_protocol` position `range`, which crosses; the recursive grammar
`Range` is a dispatcher concern the guest never sees).

**Fields**

- `buffer-id`: `u32` — CM.3: the buffer being operated on — the `target` an `apply-edit`
  effect names. `action-context` and `motion-context` have always
  carried it; an operator did not, because a NATIVE operator mutates
  the document in place and never needs to name it. A plugin operator
  holds a read-only handle and must ask the host to apply, so without
  this it can read its range and never change it.
- `range`: `range`
- `linewise`: `bool`
- `register`: `register`
- `count`: `count`
- `args`: `args`

#### record `text-object-context`

```wit
record text-object-context {
    at: position,
    count: count,
    args: args,
}
```

Mirrors `lattice_grammar::registry::TextObjectContext` (owned
projection). `at` is the cursor; buffer text + the scope/comment env ride
the `document` handle.

#### record `ex-command-context`

```wit
record ex-command-context {
    bang: bool,
    args: args,
    register: register,
    count: count,
    cursor: position,
    buffer-id: u32,
}
```

Mirrors `lattice_grammar::registry::ExCommandContext` (owned projection).
The native `range: option<lattice_grammar::range::Range>` is ABSENT by
design: the grammar `Range` is recursive (`RangeBound::Offset { base:
Box<..> }`) and carries a plugin `RangeId`, which a WIT record cannot
express (the `Global` / `NarrowTrigger` precedent). A v1 ex-command
plugin gets `bang` / `args` / `register` / `count`; the resolved range
lands with the range mirror.

**Fields**

- `bang`: `bool`
- `args`: `args`
- `register`: `register`
- `count`: `count`
- `cursor`: `position` — OC.10: where the caret sits when the `:` line is submitted, and the
  buffer it was submitted from — the two `action-context` below already
  carries.

  They are here because `apply-ex-command` returns `list<effect>` and
  `effect.apply-edit` names a `target` buffer id. Without these a guest
  could be handed that vocabulary and had no way to build a value for
  it, which is a seam that looks usable and is not. The native
  `ExCommandContext` gained `buffer-id` at MR.2 on the same reasoning —
  "a command reached that way was seeing strictly less than the same
  command reached by a chord" — and the mirror simply never followed.
- `buffer-id`: `u32`

#### record `action-context`

```wit
record action-context {
    args: args,
    register: register,
    count: count,
    cursor: position,
    buffer-id: u32,
    selection: option<range>,
}
```

Mirrors `lattice_grammar::registry::ActionContext` (owned projection) —
the count/register prefixes typed before a chord-bound action.

**Fields**

- `args`: `args`
- `register`: `register`
- `count`: `count`
- `cursor`: `position` — Where the caret sits when the action fires (AP.0.1) — the action's
  equivalent of `motion-context.from`. A plugin action pairs it with
  the `borrow<document>` handle `apply-action` receives to read the
  buffer around the cursor.
- `buffer-id`: `u32` — The active buffer's id (AP.2) — the `target` a plugin action names in
  an `apply-edit` effect. Mirrors `motion-context.buffer-id`.
- `selection`: `option<range>` — OS.2: the active region when the action fired from a Visual/Select
  chord; `none` in Normal and on every non-chord firing path. Carries
  no visual kind — the row span is what every consumer reads.

  The `ex-command-context` precedent (OC.10): a command reached one
  way must not see less than the same command reached another. The
  native peer is `lattice_mode::ActionContext::selection` (MG.18e),
  and both are filled from ONE host resolver.

#### record `motion-spec`

```wit
record motion-spec {
    jump: bool,
    exclusive: bool,
    args-schema: list<arg-spec>,
}
```

Mirrors `lattice_grammar::registry::MotionSpec` (metadata; `apply` is a
guest export, not a field).

#### record `operator-spec`

```wit
record operator-spec {
    repeatable: bool,
    args-schema: list<arg-spec>,
    blockwise-per-row: bool,
    post-motion-char: bool,
    chord: option<string>,
    doubled: option<string>,
}
```

Mirrors `lattice_grammar::registry::OperatorSpec` (metadata; `apply` is a
guest export). `blockwise-per-row` is the block-visual dispatch hint.

**Fields**

- `repeatable`: `bool`
- `args-schema`: `list<arg-spec>`
- `blockwise-per-row`: `bool`
- `post-motion-char`: `bool` — When true, the operator's keymap bindings need a trailing
  character wildcard after each motion path (e.g. surround's
  `ys{motion}{char}` captures the wrapping char).
- `chord`: `option<string>` — CM.2: the chord that invokes this operator, in vim notation
  (`gc`, `zn`). `none` registers the operator without keys — it is
  then reachable only by name, through the palette or an ex-command.

  Declared HERE rather than through the `keymap` seam because the
  operator-pending states are not plugin-bindable and cannot be: the
  host composes motion targets, text-object pendings and find-char
  pendings around an operator, and that composition needs
  host-resolved builtins. A plugin says which keys it wants; the host
  builds the same surface a native operator gets. Binding the chord
  through `register-binding` instead would fire the operator with no
  motion AND kill its doubled form, because a bound prefix kills its
  longer chords.
- `doubled`: `option<string>` — The TRAILING key of the doubled, linewise form — `c` for `gcc`,
  `U` for `gUU`, `d` for `dd`. Not the whole chord.

  `none` binds no doubled form, which is right for operators that
  have none: vim has no `zff`, and binding one would shadow the
  longer `zff{char}`.

#### record `text-object-spec`

```wit
record text-object-spec {
    args-schema: list<arg-spec>,
}
```

Mirrors `lattice_grammar::registry::TextObjectSpec` (metadata; `apply` is
a guest export).

#### record `ex-command-spec`

```wit
record ex-command-spec {
    latency-class: latency-class,
    accepts-bang: bool,
    accepts-range: bool,
    args-schema: list<arg-spec>,
    surface-form: surface-form,
}
```

Mirrors `lattice_grammar::registry::ExCommandSpec` (metadata; `parse_args`
+ `apply` are two guest exports, not fields).

#### record `action-spec`

```wit
record action-spec {
    args-schema: list<arg-spec>,
}
```

Mirrors `lattice_grammar::registry::ActionSpec` (metadata; `apply` is a
guest export).

#### record `event-applied-edit`

```wit
record event-applied-edit {
    original-range: range,
    inserted-range: range,
    replaced-text: string,
    inserted-text: string,
}
```

--- Event / hook seam (PH7.8, plugin-host.md §5 `events`) -----------------

Mirrors `lattice_runtime::EventBus` + the `lattice_protocol::Event` enum. A
plugin subscribes (guest→host `events.subscribe`, an `event-filter`) and
receives each matching event on its `on-event` export (host→guest, the
owned `event` variant). Observation-only in v1 — no before-class veto (the
native bus is observation-only, §5.10). The payloads reuse the PH7.3b
mirrors (`range` / `selection-set`); ids cross as `u64` (`.raw()`).
Mirrors `lattice_protocol::event::AppliedEdit`. Distinct from the PH7.3b
`applied-edit` record: the event form carries NO `delta` (the tree-sitter
re-parse delta is a document-actor concern, not published to observers).

#### enum `event-kind`

```wit
enum event-kind {
    document-opened,
    document-closed,
    before-save,
    document-saved,
    document-changed,
    selections-changed,
    modal-mode-changed,
    before-quit,
    option-changed,
    major-entered,
    major-exiting,
    minor-activated,
    minor-deactivated,
    plugin,
    pre-plugin-loaded,
    plugin-loaded,
    plugin-unloaded,
    files-changed,
}
```

Mirrors `lattice_protocol::EventKind` — the discriminator a subscription
filters on. Each arm pairs 1:1 with an `event` variant arm.

**Cases**

- `document-opened`
- `document-closed`
- `before-save`
- `document-saved`
- `document-changed`
- `selections-changed`
- `modal-mode-changed`
- `before-quit`
- `option-changed`
- `major-entered`
- `major-exiting`
- `minor-activated`
- `minor-deactivated`
- `plugin` — Discriminator for EVERY plugin-defined event (PH7.8b). All plugin
  events share this one kind; the per-event `name` is not a bus
  discriminator — a subscriber filters by name in its `on-event`.
- `pre-plugin-loaded` — OA.14d: a named plugin is about to run the load-time exports that
  read its OWN options. Delivery is awaited — the loader does not
  continue the load until every handler has returned — which is what
  lets an `init.rs` `set-option` reach a value the plugin consumes at
  load. `plugin-loaded` is too late for those.
- `plugin-loaded` — CI.1: plugin-lifecycle signals delivered to guests. An `init.rs`
  subscribes to `plugin-loaded` (filtering by name in its handler) to run
  deferred config against a now-present plugin (`with-eval-after-load`).
- `plugin-unloaded`
- `files-changed` — OR.2: a directory this plugin asked the host to watch changed. One
  kind for every watch; the host addresses each batch to the plugin
  that armed it, so subscribing to this kind never surfaces another
  plugin's watch.

#### record `event-filter`

```wit
record event-filter {
    kinds: option<list<event-kind>>,
    path-globs: option<list<string>>,
    major-modes: option<list<string>>,
    minor-modes: option<list<string>>,
}
```

Mirrors `lattice_runtime::EventFilter` — the DECLARATIVE subset a plugin
can express at subscribe time. `kinds = none` is the wildcard (every
kind); `path-globs` / `major-modes` AND-combine on top (each `none` is
unconstrained), matching the native EF.1 semantics. The native
`predicate` (an arbitrary Rust closure) does NOT cross — a plugin that
needs custom logic filters inside its `on-event` handler (the grammar
typed-error-defer precedent).

**Fields**

- `kinds`: `option<list<event-kind>>`
- `path-globs`: `option<list<string>>`
- `major-modes`: `option<list<string>>`
- `minor-modes`: `option<list<string>>` — Restrict minor-mode lifecycle events to ones naming these minors —
  the peer of `major-modes`, and NOT the same field.

  `minor-activated` / `minor-deactivated` carry the MINOR's name, so
  a `major-modes` constraint rejects every one of them. Without this
  a subscriber wanting one specific minor had to subscribe
  unfiltered and compare names in its handler — which wakes the
  plugin's task for every minor activation in every buffer, to do
  nothing.

  Separate rather than merged into one `modes` list because there
  are far more minors than majors and the two ask different
  questions: `major-modes` means *the buffer is entering one of
  these majors*, this means *this specific minor turned on*. A
  merged field would answer both at once and let a subscription fire
  on a name collision across the two namespaces.

  Constraining both matches NOTHING, since no event carries both
  names — the honest reading of "a major event AND a minor event".

#### record `event-plugin-lifecycle`

```wit
record event-plugin-lifecycle {
    name: string,
    id: u32,
}
```

`Event::PluginLoaded` / `Event::PluginUnloaded` payload (CI.1). `name` is
the plugin's manifest id (what a handler matches on); `id` the host-issued
numeric plugin id.

#### record `event-document-opened`

```wit
record event-document-opened {
    id: u64,
    path: option<string>,
    version: u64,
    text: string,
}
```

`Event::DocumentOpened` payload. `path` is `none` for scratch buffers.

#### record `event-document-path`

```wit
record event-document-path {
    id: u64,
    path: string,
}
```

`Event::BeforeSave` / `Event::DocumentSaved` payload — both always carry a
concrete path (a save target).

#### record `event-document-changed`

```wit
record event-document-changed {
    id: u64,
    path: option<string>,
    version: u64,
    edits: list<event-applied-edit>,
}
```

`Event::DocumentChanged` payload. `path` is `none` for scratch buffers.

#### record `event-selections-changed`

```wit
record event-selections-changed {
    id: u64,
    version: u64,
    selections: selection-set,
}
```

`Event::SelectionsChanged` payload.

#### record `event-modal-mode-changed`

```wit
record event-modal-mode-changed {
    from-state: string,
    to-state: string,
}
```

`Event::ModalModeChanged` payload — the previous / next modal-state labels.

#### record `event-option-changed`

```wit
record event-option-changed {
    name: string,
    old: option<string>,
    new-value: string,
}
```

`Event::OptionChanged` payload. `old` is `none` on the first publish after
registration (default init, no prior value).

#### record `event-mode-lifecycle`

```wit
record event-mode-lifecycle {
    buffer: u64,
    mode: string,
}
```

`Event::{MajorEntered,MajorExiting,MinorActivated,MinorDeactivated}`
payload — the buffer + the mode's canonical name.

#### record `event-plugin`

```wit
record event-plugin {
    name: string,
    payload: list<u8>,
}
```

`Event::Plugin` payload (PH7.8b) — a plugin-defined event. `name` is the
plugin's event identifier (declared via `host-services register-event`);
`payload` is opaque MessagePack the plugin owns and the host NEVER
interprets. The host is a thin router: it moves the bytes, it does not
parse them (the boundary discipline the plugin host rests on). The
ergonomic typed wrapper (`#[derive(PluginEvent)]`) is a guest-side SDK
layer OVER this opaque wire (PH7.8b.3) — the wire is identical with or
without it.

#### variant `event`

```wit
variant event {
    document-opened(event-document-opened),
    document-closed(u64),
    before-save(event-document-path),
    document-saved(event-document-path),
    document-changed(event-document-changed),
    selections-changed(event-selections-changed),
    modal-mode-changed(event-modal-mode-changed),
    before-quit,
    option-changed(event-option-changed),
    major-entered(event-mode-lifecycle),
    major-exiting(event-mode-lifecycle),
    minor-activated(event-mode-lifecycle),
    minor-deactivated(event-mode-lifecycle),
    plugin(event-plugin),
    pre-plugin-loaded(string),
    plugin-loaded(event-plugin-lifecycle),
    plugin-unloaded(event-plugin-lifecycle),
    files-changed(list<string>),
}
```

Mirrors `lattice_protocol::Event` (owned; delivered to `on-event`). Each
multi-field arm carries an explicit payload record (the `effect` mirror
precedent); ids cross as `u64`, paths as `string` (a non-UTF-8 path is a
typed boundary error, never lossy). Text-bearing arms (`document-opened`)
carry the initial content the native event already clones for observers.

**Cases**

- `document-opened`: `event-document-opened`
- `document-closed`: `u64`
- `before-save`: `event-document-path`
- `document-saved`: `event-document-path`
- `document-changed`: `event-document-changed`
- `selections-changed`: `event-selections-changed`
- `modal-mode-changed`: `event-modal-mode-changed`
- `before-quit`
- `option-changed`: `event-option-changed`
- `major-entered`: `event-mode-lifecycle`
- `major-exiting`: `event-mode-lifecycle`
- `minor-activated`: `event-mode-lifecycle`
- `minor-deactivated`: `event-mode-lifecycle`
- `plugin`: `event-plugin`
- `pre-plugin-loaded`: `string` — OA.14d: the manifest id of the plugin whose load-time exports are
  about to run. No numeric id: the plugin has not finished loading, so
  the id its contributions will carry is not yet settled — and a
  handler matches on the name anyway.
- `plugin-loaded`: `event-plugin-lifecycle`
- `plugin-unloaded`: `event-plugin-lifecycle`
- `files-changed`: `list<string>` — OR.2: absolute paths that changed under a directory this plugin
  watches, coalesced — a `git pull` rewriting two hundred files
  arrives as ONE delivery carrying two hundred paths. Deduplicated and
  sorted; a removal is reported as a change (the consumer stats it),
  because an index that cannot see deletions offers destinations that
  no longer exist. A non-UTF-8 path is skipped rather than failing the
  batch — `walk`'s rule, for `walk`'s reason.

  No plugin id crosses: a guest only ever receives its own watch.

#### enum `gutter-diff-kind`

```wit
enum gutter-diff-kind {
    add,
    remove,
    change,
    conflict,
}
```

--- Decoration seam (PH7.9, plugin-host.md §5 `decorations`) --------------

Mirrors `Mode::gutter_decorations` + `GutterDecoration` (lattice-mode). A
WASM decoration provider is an ASYNC PRODUCER (the completion PH7.6 fork —
the sync `gutter_decorations` trait is read PER-FRAME by the renderer, so a
WASM mode can't satisfy it inline): the host calls the guest's producer OFF
the render path on a trigger, caches the returned `list<gutter-decoration>`
per buffer, and the renderer reads the cache (never WASM on the tick,
paramount #1). Per-line data only — no draw calls cross.
Mirrors `lattice_mode::GutterDiffKind` — the diff-sign column.

#### enum `gutter-severity-level`

```wit
enum gutter-severity-level {
    hint,
    info,
    warning,
    error,
}
```

Mirrors `lattice_mode::GutterSeverityLevel` — the diagnostic column
(ascending severity; `max()` selects the most severe).

#### record `gutter-diff`

```wit
record gutter-diff {
    line: u32,
    kind: gutter-diff-kind,
}
```

`GutterDecoration::Diff { line, kind }` payload.

#### record `gutter-severity`

```wit
record gutter-severity {
    line: u32,
    level: gutter-severity-level,
}
```

`GutterDecoration::Severity { line, level }` payload.

#### record `gutter-sign`

```wit
record gutter-sign {
    line: u32,
    name: string,
}
```

SG.3b: `GutterDecoration::Sign { line, sign }` payload — vim's
`:sign place`.

Carries the definition's NAME, not an id. A guest has no id to carry:
ids are interned by the host and the resolution happens ONCE, at this
boundary, off the render path — which is exactly what keeps the native
placement `Copy` and free of a per-line `String`.

`name` is the plugin's own namespaced name as `define-sign` returned it
(`debugger.breakpoint`). A name nothing has defined resolves to nothing
and the placement is SKIPPED — the same answer the native path gives an
unknown id, because a definition that has not registered yet is
recoverable and failing the whole batch would take the plugin's other
marks down with it.

#### variant `gutter-decoration`

```wit
variant gutter-decoration {
    diff(gutter-diff),
    severity(gutter-severity),
    sign(gutter-sign),
}
```

Mirrors `lattice_mode::GutterDecoration` — one per-line gutter cell a
provider contributes. Each arm maps to one physical gutter column.

#### record `decoration-context`

```wit
record decoration-context {
    buffer-id: u64,
    path: option<string>,
    line-count: u32,
}
```

The owned projection of `lattice_mode::DecorationCtx` (host→guest). The
native ctx is `buffer_id` + a `ServiceRegistry` of render-state snapshots
(host-owned, can't cross); the projection carries the owned scalars a v1
producer computes from — buffer id / path / line count. Bulk buffer text
(a diff producer's input) rides `host-services` or the deferred `document`
handle (the picker/grammar precedent), NOT this record.

#### enum `media-fit`

```wit
enum media-fit {
    contain,
    width,
}
```

--- Inline media seam (IM.6, inline-media.md §7) ---------------------------
How a media block's intrinsic size maps into its box. Mirrors
`lattice_cells::MediaFit`.

**Cases**

- `contain` — Scale down to fit, preserving aspect ratio; never scale up.
- `width` — Scale to the pane width, up or down; the height follows.

#### record `media-block`

```wit
record media-block {
    anchor-line: u32,
    path: string,
    alt: option<string>,
    fit: media-fit,
}
```

One inline media block a guest wants drawn.

The guest names a FILE and a LINE; it never sends pixels. That keeps the
`fs:read` decision host-side — the host decides whether this plugin may
read that path — and stops a plugin putting arbitrary bytes on screen.
It also avoids copying a decoded image across the boundary per load.

Note what is ABSENT: any notion of size. The host resolves the intrinsic
dimensions and computes the reserved rows, so sizing policy lives in one
place and a guest cannot reserve arbitrary vertical space.

**Fields**

- `anchor-line`: `u32` — 0-based source line the block hangs below.
- `path`: `string` — Path to the image. Relative paths resolve against the buffer's own
  directory, which is what an org `[[file:diagram.png]]` means.
- `alt`: `option<string>` — What a renderer that cannot draw shows instead, and what a screen
  reader reads. `none` falls back to the file name — never nothing,
  because a blank box tells the user nothing about what is missing.
- `fit`: `media-fit`

#### record `context-scope`

```wit
record context-scope {
    scope-start: u32,
    scope-end: u32,
    header-start: u32,
    header-end: u32,
}
```

--- Sticky-context seam (TC.2, treesitter-context.md) ----------------------
One structural scope: the range it spans, plus the line span that NAMES
it. Mirrors `lattice_cells::context::ContextScope` exactly (TC.1).

`header-start ..= header-end` is normally one line and spans several when
a signature wraps. All four are inclusive, 0-based source lines.

A scope is a **pure function of the parse tree** — no viewport, no cursor,
no options. That is what lets the guest compute the set once per parse and
the host resolve it per pane afterwards without another guest call, which
is the whole reason this seam returns scopes rather than finished rows.

#### record `context-request`

```wit
record context-request {
    buffer-id: u64,
    path: option<string>,
    line-count: u32,
}
```

The owned projection handed to a context producer (host→guest), same
shape rule as `decoration-context` (§4.2): owned scalars only, bulk text
and structure ride handles instead.

Deliberately NOT reusing `decoration-context` even though the fields
coincide today: the two describe different things (a decoration trigger
vs a context request), and sharing the record would make a field one seam
needs into ABI churn for the other.

No `language` and no `parse-version` field: the guest reads the language
off the `tree-snapshot` it is handed (so the two can never disagree), and
the parse version is host-side cache bookkeeping the guest has no use for.

#### enum `ui-zone`

```wit
enum ui-zone {
    left,
    center,
    right,
}
```

--- UI-contribution seam (plugin-host.md §5 `ui`) — TYPE-MIRROR ----------

Sized for the ABI freeze (§14) when the emit producer was still deferred.
**OC.3 / ML.6 landed that producer for the modeline half** (`ui.wit`), and
building it against a real consumer reshaped the mirror — which is what the
deferral note said should happen ("waits for a real plugin that needs
more", §5.5).

What changed and why: `ui-segment` bundled `zone` with `text` and `role`,
which turned out to conflate two different lifetimes. The zone belongs to
the *descriptor*, registered once and owned by the plugin for its whole
life (`modeline.md` §6); the text is *content*, pushed many times per
descriptor. A record carrying both forces a plugin to restate its zone on
every push and gives the host no way to tell a re-registration from an
update. So `ui.register-segment(id, zone, priority)` takes the descriptor
half and `ui.emit-segment(id, text)` the content half, and the record
dissolves — leaving `ui-zone` as the only piece with a native counterpart
to mirror.

`role` did not survive the same review. Both renderers match role names
against a closed set and *disagree* on the fallback (TUI defaults to no
style, GPUI to the path colour), so a role parameter would have shipped a
silent cross-renderer difference. Neither native modeline producer uses
more than one role either. It returns when a plugin registers its own
theme element (TC.4) and can be styled coherently.

Notifications remain a mirror with no producer — `effect.echo` still
carries them. Sprites (§5.6.7) have no native struct yet, so mirroring one
would violate exercised-trait-first.
A modeline zone (mirrors `lattice_mode::modeline::Zone`).

#### record `ui-notification`

```wit
record ui-notification {
    level: echo-level,
    message: string,
}
```

A user notification a plugin emits — reuses the `echo-level` severity the
`effect.echo` path already carries.


## `ui`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `multiseam-fixture` (imports), `plugin` (imports)

The UI-contribution surface (design.md §9.4 `ui`): guest→host emits **data
only**, never draw calls (§7, paramount #1).

**OC.3 / ML.6 populates the modeline half.** `modeline.md` §6 is the
governing contract, and its rule is that whoever registers an element owns it
end to end — descriptor, content, and (later) interaction handlers. So this
interface hands a plugin the same three primitives a native mode gets from
`ModelineService`, and nothing more: register a descriptor, push content,
clear it. There is no host-side branch on which plugin is asking, and the
acid test `modeline.md` states — a provider adding a modeline element needs
zero `Editor::` methods and zero new host `Action` variants — holds.

**Not a draw call, and not a poll.** `emit-segment` publishes a
`ModelineElementUpdate` on the event bus, exactly as `lattice-lsp::modeline`
and `lattice-ai::mcp::status` do; the host's wake forwarder repaints
off-keystroke. A per-frame WASM callback would violate paramount #1. An
event-driven push does not — which is precisely why plugins get this path and
no other.

**Off the keystroke path — by context, not by linker.** The plan for this
slice said "wired on the async linker only". That does not survive the
Component Model: a plugin's import set is fixed for the whole component, and
the *same* artefact is instantiated against the sync grammar linker for its
grammar seam — so an import missing there fails the WHOLE plugin, not just
the seam that uses it (the TC.6 / CR.3 / LG.3c / OM.11 lesson, and org has
already been broken this exact way once by a single `logging::log` call). So
`ui` IS on both linkers, and the guarantee is enforced one layer in: the
modeline handle is stamped only on the async spawn paths, so a grammar
action's `emit-segment` finds no context and is a warn + drop. Same shape as
`config`, `theme` and `keymap`, and it is tested rather than assumed.

### Uses

- `ui-zone` from `types`

### Functions (3)

#### `clear-segment`

```wit
clear-segment: func(id: string)
```

Hide this element. Idempotent, and safe for an id that was never
registered — a plugin should not have to mirror host state to avoid a
trap. The descriptor survives; only the content is dropped, so a later
`emit-segment` brings it back without re-registering.

#### `emit-segment`

```wit
emit-segment: func(id: string, text: string)
```

Push this element's content. Empty text hides it (equivalent to
`clear-segment`), which is how a native element signals "nothing to say
right now" and costs the plugin no extra call.

The text is styled as an ordinary modeline item — the one role both the
TUI and GPUI peers resolve identically, and the only one either native
modeline producer uses. A per-span themed role is deliberately absent:
the renderers match role names against a closed set and *disagree* on the
fallback, so a role knob would ship a silent cross-renderer difference.
A plugin that needs its own colour registers a theme element (TC.4)
first; that is the slice which earns the role parameter.

#### `register-segment`

```wit
register-segment: func(id: string, zone: ui-zone, priority: s32) -> bool
```

Register a modeline element descriptor and take ownership of it
(`modeline.md` §6). `id` is namespaced with the plugin's own name — a
`register-segment("clock")` from `org` owns `org.clock` — so one plugin
can never shadow another's element or a built-in `core.*` one.

`priority` orders within the zone, ascending, ties broken by id. The
native neighbours in `Right` are `lsp` at 5, `claude-code` at 6,
`core.position` at 10 and `core.lang` at 20; pick accordingly.

The element is **global**, not per-pane: it shows in every window
regardless of which buffer is focused. Per-buffer plugin segments are not
mirrored here because no plugin needs one yet — LSP and MCP status are
per-buffer because they track buffers, and a plugin that starts to will
be the slice that adds the buffer parameter (§5.5, "the API grows from
real plugins").

Returns `false` when no modeline is wired on this seam (see the interface
note) — the honest "nothing to register into" degradation, never a trap.
Re-registering the same id is last-write-wins, so a reload re-registers
rather than duplicating.

