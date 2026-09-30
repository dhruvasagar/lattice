<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `host-services`

**Direction:** guest calls into the host through it · **Capability:** filesystem · **Worlds:** `completion-source-plugin` (imports), `context-plugin` (imports), `decorations-plugin` (imports), `events-plugin` (imports), `media-plugin` (imports), `multibuffer-view-plugin` (imports), `picker-source-plugin` (imports), `plugin` (imports), `project-plugin` (imports)

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

## Uses

- [`position`](types.md#record-position) from [`types`](types.md)

## Functions (20)

### `can-write-file`

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

**Example — Check whether a write to a path would land, before committing to it** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let text = match host_services::can_write_file(&path) {
    Ok(()) => "writable".to_string(),
    Err(e) => format!("error: {e}"),
};
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text,
})])
```

### `clamp-position`

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

**Example — Clamp a remembered position into a buffer's current bounds; `none` means the buffer closed** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let text = match host_services::clamp_position(buffer, Position { line, byte }) {
    Some(p) => format!("{}:{}", p.line, p.byte),
    None => "none".to_string(),
};
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text,
})])
```

### `delete-file`

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

**Example — Delete a file from the sync grammar seam, surfacing the host's error text** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let text = match host_services::delete_file(&path) {
    Ok(()) => "deleted".to_string(),
    Err(e) => format!("error: {e}"),
};
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text,
})])
```

### `emit-event`

```wit
emit-event: func(name: string, payload: list<u8>)
```

Publish a plugin-defined event on the editor's event bus (PH7.8b). `name`
is the event identifier (typically pre-declared via `register-event`);
`payload` is opaque MessagePack the plugin owns — the host moves the bytes
onto the bus (`event::plugin`) and NEVER interprets them. Fire-and-forget:
the bus is observation-only (§5.10), so there is no reply. Subscribers
(native or other plugins) filter by `name` in their handler.

**Example — Emit a typed plugin event on save, its payload MessagePack-encoded by the SDK derive** · [`crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs)

```rust
// PH7.8b.2/3: on a save, EMIT a plugin-defined event. The SDK derive
// MessagePack-encodes a typed struct (`SavedEcho`) into the opaque
// payload; it crosses to the bus verbatim and a consumer sharing the type
// decodes it (the e2e test). The host never parses the bytes.
if handler == 1 {
    let echo = SavedEcho {
        path: match &ev {
            Event::DocumentSaved(p) => p.path.clone(),
            _ => String::new(),
        },
    };
    host_services::emit_event(SavedEcho::NAME, &echo.encode());
}
```

### `excerpt-source`

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

**Example — Resolve the multibuffer row under the cursor to its source file and line** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let answer = match host_services::excerpt_source(
    u64::from(ctx.buffer_id),
    ctx.cursor.line,
) {
    Some(loc) => format!("{}@{} in {}", loc.path, loc.line, loc.buffer),
    None => "none".to_string(),
};
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!(
        "excerpt-source({},{})={answer}",
        ctx.buffer_id, ctx.cursor.line
    ),
})])
```

### `local-utc-offset-seconds`

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

**Example — Read the host's local UTC offset; the guest's own clock (`wasi:clocks`) is UTC-only** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let utc = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs() as i64)
    .unwrap_or(0);
let offset = host_services::local_utc_offset_seconds();
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("{offset}:{utc}"),
})])
```

### `new-uuid`

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

**Example — Mint ids from the sync grammar seam, propagating an entropy failure as an err** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let a = host_services::new_uuid()?;
let b = host_services::new_uuid()?;
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("{a}|{b}"),
})])
```

### `read-file`

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

### `refresh-decorations`

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

### `register-event`

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

**Example — Declare a plugin-defined event, taking its name and doc from the SDK's `PluginEvent` derive** · [`crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs)

```rust
// PH7.8b.2/3: declare a plugin-defined event via the `register-event`
// host-service, using the SDK-derived `NAME` + `DOC` (the doc-comment).
// It self-registers into the host's runtime event registry under this
// plugin's provenance; `on-event` handler 1 emits it on save.
host_services::register_event(SavedEcho::NAME, SavedEcho::DOC);
```

### `source-line`

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

### `store-delete`

```wit
store-delete: func(key: string) -> result<_, string>
```

Forget `key`. Deleting a key that is not there is `ok` — a retraction
that has already happened is not an error.

### `store-generation`

```wit
store-generation: func() -> u64
```

Bumped on every successful mutation, never on a read. A reader compares
it against what it last built from and rebuilds only when it moved.

This is what makes one-writer/many-readers work across separate `Store`s
(see the block comment above): the number is host-side, so a reader
instance sees the writer instance's bump without sharing memory with it.
`0` for a plugin with no store.

**Example — Read a store key another seam wrote, alongside the store's generation counter** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let value = host_services::store_get("multiseam/probe")
    .and_then(|b| String::from_utf8(b).ok())
    .unwrap_or_else(|| "none".to_string());
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("{}:{}", host_services::store_generation(), value),
})])
```

### `store-get`

```wit
store-get: func(key: string) -> option<list<u8>>
```

The bytes stored under `key`, or `none` when nothing is stored there.

`none` also covers every degraded case (no grant, no data dir, a store
discarded as corrupt). A reader for whom absence is ordinary — a first
index that has not run yet — cannot distinguish them, and does not need
to: the answer to all four is "build it".

**Example — Load a plugin-private value, treating an absent key as a fresh install** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// Read the remembered list.
///
/// A `none` from `store-get` covers every degraded case — no grant, no data
/// dir, a store discarded as corrupt — and the seam's own doc says a reader for
/// whom absence is ordinary cannot distinguish them and does not need to. Here
/// absence genuinely is ordinary: it is a fresh install.
fn load() -> Vec<String> {
    host_services::store_get(STORE_KEY)
        .map(|bytes| projects::decode(&bytes))
        .unwrap_or_default()
}
```

### `store-keys`

```wit
store-keys: func(prefix: string) -> list<string>
```

Keys carrying `prefix`, sorted. `""` lists everything.

**Example — Write a key to the plugin store, then list every key under a prefix** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let put = match host_services::store_put("multiseam/from-grammar", b"g") {
    Ok(()) => "ok".to_string(),
    Err(e) => format!("err({e})"),
};
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("{put}:{}", host_services::store_keys("multiseam/").join(",")),
})])
```

### `store-put`

```wit
store-put: func(key: string, value: list<u8>) -> result<_, string>
```

Persist `value` under `key`. `err` names why — no grant, no data dir,
a value larger than the whole store may hold, or a write that failed.

**Example — Persist a plugin-private value and surface the error rather than swallow it** · [`plugins/project/src/lib.rs`](../../../../plugins/project/src/lib.rs)

```rust
/// Persist the list. The `Err` is returned rather than swallowed so a command
/// can echo it — a `:project-remember` that reports success and stored nothing
/// is precisely the silent failure this plugin must not have.
fn save(list: &[String]) -> Result<(), String> {
    host_services::store_put(STORE_KEY, &projects::encode(list))
}
```

### `unwatch`

```wit
unwatch: func(path: string) -> result<_, string>
```

Stop watching `path`. Unwatching a path that is not watched is `ok` — a
disarm is idempotent, because the alternative is a guest that must track
host state to avoid an error.

**Example — Disarm a directory watch from inside the batch handler that decided to stop** · [`crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs)

```rust
if let Ok(target) = std::fs::read_to_string(WATCH_TARGET) {
    let outcome = match host_services::unwatch(target.trim()) {
        Ok(()) => "unwatch:ok".to_string(),
        Err(e) => format!("unwatch:err({e})"),
    };
    record(&outcome);
}
```

### `view-args`

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

### `walk`

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

**Example — Walk a directory, handling the `err` a plugin without an `fs` grant gets** · [`crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/multiseam-guest/src/lib.rs)

```rust
let text = match host_services::walk("/") {
    Ok(paths) => format!("walked:{}", paths.len()),
    Err(_) => "refused".to_string(),
};
```

### `watch`

```wit
watch: func(path: string) -> result<_, string>
```

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

**Example — Subscribe to `files-changed`, then watch a directory and record whether the grant allowed it** · [`crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/events-guest/src/lib.rs)

```rust
events::subscribe(&kind_filter(EventKind::FilesChanged), 6);
let outcome = match host_services::watch(target) {
    Ok(()) => "watch:ok".to_string(),
    Err(e) => format!("watch:err({e})"),
};
record(&outcome);
```

## Types (1)

### record `source-location`

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

