<!-- @generated from wit/ by crates/lattice-plugin-api/tests/site_reference_is_current.rs.
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# Lattice Plugin API

Derived from the canonical `wit/` package — 30 seam(s).

## buffer  (guest calls into the host through it, capability: none (pure data / dispatch))

Mirrors the native `Document` / `Buffer` read seam (plugin-host.md §4.2,
§9.6). The host owns the buffer; the guest gets a `document` **resource
handle** and calls back for the text slices it needs, so bulk rope text
never crosses the boundary. The owned `buffer-snapshot` record carries the
non-bulk metadata — the borrows-projected form of
`lattice_picker::context::ActiveBufferSnapshot`. Populated at PH7.3c; the
guest→host call through the canonical ABI is exercised at PH7.3d/PH7.4.

### Functions (5)

- `[method]document.byte-len` — Total byte length.
- `[method]document.get-text-range` — The text of the `[start, end)` byte range. `err` on an out-of-range
- `[method]document.line` — Line `n` (0-based) as text without its trailing newline (matching
- `[method]document.line-count` — Lines the document has: `"a\nb\n"` is two lines, and so is
- `[method]document.path` — OM.6b: the file this document is backed by, absolute. `none` for a

## command  (shared types only (not called directly), capability: none (pure data / dispatch))

Mirrors `CommandRegistry` + `CommandInvocation` + the closed `Effect`
enum (lattice-grammar). Guest→host `invoke`; host→guest `apply`. The
`effect` WIT variant mirrors the ~105-variant enum whole (§4.4) so the
boundary stays typed. Populated in PH7.3 (Effect round-trip) / PH7.7.

### Functions (0)

_(none — a shared type interface)_

## completion-source  (guest implements this interface, capability: none (pure data / dispatch))

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

### Functions (2)

- `generate` — Produce raw candidates for the current slot. `ctx` carries the query
- `spec` — The source's identity (`name` + `doc`), the `insert_generator` pair.

## config  (guest calls into the host through it, capability: none (pure data / dispatch))

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

### Functions (3)

- `get-option` — Read an option's current value, formatted as a string (the `OptionType`
- `register-option` — Declare a plugin option into the editor's `ConfigRegistry`. `default` is
- `set-option` — Set (override) an EXISTING option's value (CI.7) — the init.rs config

## context  (guest implements this interface, capability: none (pure data / dispatch))

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

### Functions (1)

- `context-scopes` — Produce the structural context scopes for a buffer.

## dashboard  (guest calls into the host through it, capability: none (pure data / dispatch))

CR.4: plugin-contributed dashboard sections.

A plugin puts its own block on the launch page — recent projects, a git
summary, whatever it is for. The section lands in the SAME registry the
built-in sections live in, so `dashboard.sections` orders it, the
compositor renders it, and the theme styles it, with no host kind-branch.

## A function, not data — unlike `help`

The `help` seam hands over a string once and drops the guest, because a
help page does not change between load and read. A dashboard section is
different in kind: `render-section` takes a `ctx` the guest cannot know at
load — the pane width, whether Nerd Font glyphs are available, the editor
version — and DB.6 exists precisely because those change while the editor
runs. So the guest stays instantiated and the host calls it per compose.

Freezing a section into text at registration would make it blind to the
icon palette and unable to show anything live, which is most of what
"whole-author a section" is for.

## Where it runs, and what that costs

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

## What the host does with a bad fragment

Validates and drops, never traps. Guest output is untrusted: a row with no
spans, a span whose link does not parse, or a fragment longer than the row
cap is dropped at `debug!`. A trap poisons the section — it renders
nothing further this session and the REST OF THE PAGE still composes,
exactly as a trapping `error-parser` costs its own entries and not the
build.

### Functions (1)

- `register-section` — Declare a section.

## decorations  (guest implements this interface, capability: none (pure data / dispatch))

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

### Functions (1)

- `gutter-decorations` — Produce the per-line gutter decorations for a buffer. `ctx` is the owned

## error-parser  (shared types only (not called directly), capability: none (pure data / dispatch))

CM.6: plugin-contributed compilation-output parsers.

A plugin teaches lattice to recognise diagnostics from a build tool the
editor has never heard of. The native set covers cargo/rustc, gnu-style,
and test panics; everything else in the world — a bespoke linter, an
in-house build system, a language whose compiler predates all of them — is
what this is for.

## Line at a time, because the format is

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

## Where it runs

Off the UI and actor threads, in the compilation reader (see
`compilation-mode.md` §5). Not the keystroke path — but it IS the critical
path of a fast producer, so a guest that blocks here backs up a build's
output. The host budgets it per call like every other seam.

## What the host does with a bad entry

Validates and drops, never traps. A returned `line`/`col` is guest data
and the host treats it as untrusted: a nonsense path or an entry with an
empty path is logged at debug and skipped, exactly as a native parser's
malformed-but-claimed match is. One bad line must not fail a build.

### Functions (0)

_(none — a shared type interface)_

## events  (guest calls into the host through it, capability: none (pure data / dispatch))

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

### Functions (3)

- `cancel-wake` — Disarm a wake. Unknown / already-cancelled / `0` ids are ignored — a
- `subscribe` — Subscribe `handler` to every event matching `filter` (the declarative
- `wake-every` — Ask to be woken every `ms` milliseconds, forever, until `cancel-wake`

## grammar  (guest calls into the host through it, capability: none (pure data / dispatch))

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

### Functions (5)

- `register-action` — Contribute a chord-bound action. `callback` → `grammar-callbacks.apply-action`.
- `register-ex-command` — Contribute an ex-command. TWO callbacks — `parse-callback` →
- `register-motion` — Contribute a motion. `callback` is the id the host passes back to
- `register-operator` — Contribute an operator. `callback` → `grammar-callbacks.apply-operator`.
- `register-text-object` — Contribute a text object. `callback` → `grammar-callbacks.apply-text-object`.

## grammar-callbacks  (guest implements this interface, capability: none (pure data / dispatch))

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

### Functions (6)

- `apply-action` — 
- `apply-ex-command` — OC.10 gave this `doc` and `tree`, so a plugin ex-command can read the
- `apply-motion` — OM.4: a motion receives `borrow<document>` too. The `apply-action`
- `apply-operator` — 
- `apply-text-object` — OM.4b: a text object receives `borrow<document>` too — `text-object-context`
- `parse-ex-args` — 

## help  (guest calls into the host through it, capability: none (pure data / dispatch))

CR.3: plugin-contributed `:help` pages.

A plugin ships its own manual. The topic lands in the SAME registry the
builtin docs live in, so `:help <name>` opens it, `:help <Tab>` completes
it, markdown renders through the same pipeline, and `:describe-command`
can cross-link to it — with no host kind-branch anywhere.

## The body ships INSIDE the component

A plugin's markdown is `include_str!`'d at build time and baked into its
own `.wasm`, exactly the way lattice's own docs are baked into the lattice
binary. Docs and code are then one artefact with one lifetime: unloading
the plugin removes its pages, and a plugin that failed to load has left
none behind.

This is deliberately NOT a runtime doc directory. That model (designed
2026-07-29, retired 2026-08-22 — see `contributable-registries.md` §4)
would need plugins to copy markdown into a shared directory at install
time, which separates the docs from the thing that owns them.

## Data, not a callback

The body crosses ONCE, at registration, and the host keeps the string.
There is no `render-topic` export, because a help page does not change
between the moment the plugin loads and the moment someone reads it —
so nothing about the guest needs to stay alive to serve one. (Compare
`dashboard`, whose sections ARE functions of a live context and therefore
do keep a guest instantiated.)

## Where it runs

Once per load, on the loader's off-boot-thread task. Never on the
keystroke or frame path, and never again after the load.

### Functions (1)

- `register-topic` — Register one free-form `:help` topic.

## host-services  (guest calls into the host through it, capability: filesystem)

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

### Functions (13)

- `emit-event` — Publish a plugin-defined event on the editor's event bus (PH7.8b). `name`
- `local-utc-offset-seconds` — The host's offset from UTC, in seconds, **at this instant** (OC.4).
- `new-uuid` — A fresh random (v4) UUID, uppercase, in the canonical
- `read-file` — Read a UTF-8 file, capability-gated the same way `walk` is.
- `register-event` — Declare a plugin-defined event (PH7.8b). Registers `name` + `doc` into
- `store-delete` — Forget `key`. Deleting a key that is not there is `ok` — a retraction
- `store-generation` — Bumped on every successful mutation, never on a read. A reader compares
- `store-get` — The bytes stored under `key`, or `none` when nothing is stored there.
- `store-keys` — Keys carrying `prefix`, sorted. `""` lists everything.
- `store-put` — ---------------------------------------------------------------------
- `unwatch` — Stop watching `path`. Unwatching a path that is not watched is `ok` — a
- `walk` — Recursively enumerate files under `root`, returning absolute UTF-8 paths.
- `watch` — ---------------------------------------------------------------------

## keymap  (guest calls into the host through it, capability: none (pure data / dispatch))

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

- `register-binding` — Bind `chord` in `binding-mode` to an EXISTING command named `command`

## language  (guest calls into the host through it, capability: none (pure data / dispatch))

LG.3c: plugin-contributed languages.

A plugin ships a tree-sitter grammar compiled to WebAssembly and the
queries that go with it. The language lands in the SAME registry the
bundled ones live in, so `Lang::detect_from_path` selects it by extension,
`:describe-buffer` names it, highlighting, folding, indenting and
incremental reparse all run through the ordinary paths — with no host
kind-branch anywhere.

## The host still owns the parse loop

The guest ships the grammar; it does not run it. `WasmStore::load_language`
turns the bytes into an ordinary `tree_sitter::Language`, and from that
point nothing downstream can tell where the grammar came from. There is no
guest call on the keystroke path at all — the plugin is consulted once, at
load.

This preserves the rejection recorded in `plugin-treesitter-seam.md` §9: a
text-only seam where the guest re-parses would duplicate the host's live
incremental tree. What changes here is only where the grammar comes from,
never who runs it.

## Data, not a callback

A language is a static description. Nothing about the guest needs to be
alive once the bytes and query sources are across, so the store is dropped
when registration returns — the `help` seam's shape and reasoning, not
`dashboard`'s live sections.

## Where it runs

Once per load, on the loader's off-boot-thread task. Compiling a grammar
costs ~100 ms (Cranelift), which is exactly why it happens here and once,
rather than on first open of a matching file.

### Functions (1)

- `register-language` — Register one language.

## logging  (guest calls into the host through it, capability: none (pure data / dispatch))

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

- `log` — Emit one log line. `level` gates it against the plugin's trace verbosity

## media  (guest implements this interface, capability: none (pure data / dispatch))

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

### Functions (1)

- `media-blocks` — Produce the media blocks for a buffer.

## modes  (guest calls into the host through it, capability: none (pure data / dispatch))

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

- `disable-mode` — Disable a registered minor mode globally (CI.4) — the inverse of
- `enable-mode` — Enable a registered minor mode globally (CI.4) — the user-enablement path
- `register-mode` — Declare a mode. Records the declaration; the host builds a `PluginMode`

## multibuffer-view-registry  (guest calls into the host through it, capability: none (pure data / dispatch))

MV.1 — the seam by which a plugin **owns a multibuffer view**.

Design: `docs/dev/architecture/plugin-multibuffer-views.md`.

## What was missing

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

## The registry shape, not the one-view-per-component shape

A guest calls `register-multibuffer-view` once per view it owns, exactly as
`picker-registry` works and for the reason OR.5b records: every other
contribution seam in the system is "the guest calls a host import to register
N things", and the one seam shaped "the component IS one source" had to be
changed the moment a plugin wanted two.

### Functions (1)

- `register-multibuffer-view` — Declare one view. Called from the guest's `register-multibuffer-views`

## multibuffer-view-source  (guest implements this interface, capability: none (pure data / dispatch))

### Functions (1)

- `build` — Produce a `pull` view's excerpts, **in final order**.

## picker-registry  (guest calls into the host through it, capability: none (pure data / dispatch))

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

### Functions (1)

- `register-picker-source` — Declare one picker source. Called from the guest's

## picker-source  (guest implements this interface, capability: none (pure data / dispatch))

### Functions (2)

- `accept` — Translate the user's chosen `routing` token into a typed
- `init` — Build the candidate set for `:picker <id> <args>`. `ctx` is the owned

## plugin-manager  (guest calls into the host through it, capability: subprocess)

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

## Recording, not doing

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

- `require` — Declare a plugin. Records the spec; the host resolves, builds and loads

## project  (guest calls into the host through it, capability: filesystem)

Guest→host project resolution (PR.6, design
`docs/dev/architecture/project-resolution.md` §6).

A **project** is the tree a buffer belongs to — the answer `:terminal`,
`:compile` and `:search` root themselves at. It is found by walking up from
the buffer's own directory to the first directory holding a marker (`.git`,
`Cargo.toml`, …, configurable via `project.root-markers`); with no marker
anywhere, the editor's working directory stands in.

## An import, not a contribution seam

The host answers; the guest asks. Project resolution is CORE — terminal,
compilation, search, the file picker and magit all root from it — so it can
never depend on a plugin being alive. Were this a contribution seam, each of
those would need an "if the project plugin loaded, ask it, else fall back"
branch, and boot ordering would become load-bearing for correctness rather
than for features.

A `project.el`-style plugin therefore READS the root here and acts through
the ordinary effect seams; it does not supply the root.

## Resolution only

Deliberately just "where is the project". No file listing, no project list,
no switching — those are the plugin's job, and a host seam that grew them
would be re-implementing the plugin inside the host.

Sync, and available in every world. It may walk the filesystem on a cache
miss, but it runs on the plugin's own store and task — never the UI or actor
thread — and the host's cache is keyed by directory, so a project's buffers
share one walk.

### Functions (2)

- `root-for-buffer` — The project containing `buffer`.
- `root-for-path` — The project containing `path`, which may name a file or a directory and

## scanned-excerpt-source  (shared types only (not called directly), capability: none (pure data / dispatch))

OM.A1: plugin-contributed agenda rows.

A plugin teaches lattice to recognise "things with a date on them" in a
filetype the editor has never heard of. Org's agenda is the first and the
motivating one, but nothing here is org: a source names the file
extensions it wants offered, is handed one file's text at a time, and
returns the rows it found.

## It is a multibuffer, so the row shape is an excerpt

The host turns each [`entry`] into an `Excerpt { source, start_line,
end_line, header }` in a multibuffer view — which buys jump-to-source,
edit-propagates-to-source, headerline status and refresh from machinery
that already ships (`org-mode.md` §6.1). That is why an entry carries a
*line* rather than a rendered string: an agenda you can only read is a
lesser feature wearing the name.

## Text AND a tree — structure from one, characters from the other

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

## Where it runs

Off the UI and actor threads, on a spawned scan task. Not the keystroke
path — but it IS the critical path of a producer, so a guest that blocks
in `scan` backs up the agenda the way a slow `error-parser` backs up a
build. Budgeted per call like every other seam.

## What the host does with a bad entry

Validates and drops, never traps. A malformed file must not fail the
agenda — `error-parser`'s rule, because it is the same failure class.

### Functions (0)

_(none — a shared type interface)_

## theme  (guest calls into the host through it, capability: none (pure data / dispatch))

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

- `register-element` — Declare a theme element with its default style.
- `set-element-override` — TK.5: override an element this plugin owns, ABOVE the theme.

## transient-source  (guest implements this interface, capability: none (pure data / dispatch))

TR.2b: plugin-contributed transient menus.

A transient is a keyed menu — one keystroke per row, fires and closes. The
mechanism belongs to `lattice-picker` (`TransientSpec`,
`TransientSourceRegistry`); magit is its first *user*, not its owner. Until
this seam a plugin could `Effect::OpenTransient` one of magit's menus and
none of its own, which made org's capture menu — one row per template —
inexpressible.

## Mirrors `picker-source`, because it is the same shape

A named thing the host asks a guest to build, given a context the host
owns: `id()` names the registry entry once at load, `build(ctx)` produces
the menu per open.

## Per open, not once at registration

A builder's rows depend on where the user is — which is why
`transient-context` exists at all, and why the host calls `build` on every
open rather than caching a spec. Emacs magit answers the same question with
`:if-mode` / `:if-derived` predicates on its prefixes; the two mode axes
are separate fields here for the same reason.

## Where it runs

On the plugin's own actor task, off the editor actor. `build` is reached by
an explicit user action (a chord, an ex-command) — never per keystroke and
never per frame — and the host parks on it, seating the menu when it lands.
A slow guest delays its own menu and nothing else.

### Functions (2)

- `build` — Build the menu for the place it was opened from.
- `id` — The menu's name, as `Effect::OpenTransient` names it. Called once, at

## tree-sitter  (guest calls into the host through it, capability: none (pure data / dispatch))

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

### Functions (25)

- `[method]node.byte-range` — The node's `[start, end)` span as byte-columns per line (matching the
- `[method]node.child-by-field` — The child under the grammar field `name` (e.g. `"body"`), or `none`.
- `[method]node.is-error` — Whether the node is a tree-sitter ERROR node (a parse error).
- `[method]node.is-named` — Whether the node is *named* (a grammar rule) vs an anonymous token.
- `[method]node.kind` — The node's grammar kind (e.g. `"function_item"`).
- `[method]node.named-child` — The `index`-th NAMED child (0-based), or `none` past the end.
- `[method]node.named-child-count` — Count of NAMED children.
- `[method]node.next-named-sibling` — The next NAMED sibling, or `none`.
- `[method]node.parent` — The parent node, or `none` at the root.
- `[method]node.prev-named-sibling` — The previous NAMED sibling, or `none`.
- `[method]node.walk` — TS.2: a stateful cursor positioned at this node, for structural walks
- `[method]tree-cursor.current-field` — The grammar field of the current node relative to its parent (e.g.
- `[method]tree-cursor.current-node` — The node the cursor currently sits on.
- `[method]tree-cursor.goto-first-named-child` — Move to the first NAMED child; `false` (and no move) if there is none.
- `[method]tree-cursor.goto-next-named-sibling` — Move to the next NAMED sibling; `false` (and no move) if there is none.
- `[method]tree-cursor.goto-parent` — Move to the parent; `false` (and no move) at the root.
- `[method]tree-cursor.reset` — Reposition the cursor onto `n` (must be a node of the same snapshot).
- `[method]tree-snapshot.compile-query` — TS.2: compile a tree-sitter query (S-expression) against THIS
- `[method]tree-snapshot.enclosing` — The nearest ancestor of `pos` whose `kind` is in `kinds` (the
- `[method]tree-snapshot.language` — The grammar id (e.g. `"rust"`), so a plugin can pick the right query.
- `[method]tree-snapshot.node-at` — The smallest NAMED node spanning `pos`
- `[method]tree-snapshot.root` — The tree root.
- `[method]tree-snapshot.run-query` — TS.2: run `q` over the whole tree, or `within` a point range. Returns
- `[method]tree-snapshot.run-query-ranges` — TS.2b: the same query, returning RANGES instead of node handles.
- `parse-file` — OT.2: parse a file that is **not an open buffer**, and hand back a

## types  (shared types only (not called directly), capability: none (pure data / dispatch))

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

## ui  (guest calls into the host through it, capability: none (pure data / dispatch))

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

### Functions (3)

- `clear-segment` — Hide this element. Idempotent, and safe for an id that was never
- `emit-segment` — Push this element's content. Empty text hides it (equivalent to
- `register-segment` — Register a modeline element descriptor and take ownership of it
