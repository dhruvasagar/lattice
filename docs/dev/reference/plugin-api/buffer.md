<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `buffer`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (imports), `comment-plugin` (imports), `grammar-plugin` (imports), `plugin` (imports), `project-plugin` (imports), `treesitter-context-plugin` (imports)

Mirrors the native `Document` / `Buffer` read seam (plugin-host.md §4.2,
§9.6). The host owns the buffer; the guest gets a `document` **resource
handle** and calls back for the text slices it needs, so bulk rope text
never crosses the boundary. The owned `buffer-snapshot` record carries the
non-bulk metadata — the borrows-projected form of
`lattice_picker::context::ActiveBufferSnapshot`. Populated at PH7.3c; the
guest→host call through the canonical ABI is exercised at PH7.3d/PH7.4.

## Uses

- [`position`](types.md#record-position) from [`types`](types.md)
- [`range`](types.md#record-range) from [`types`](types.md)

## Functions (0)

_(none outside its resources — see Resources below)_

## Resources

### resource `document`

A host-owned, point-in-time view of a document's text (PH7.3c,
decision A: backed by an `Arc<DocumentSnapshot>`). Because it is a
snapshot, edits landing after the handle is minted never shift byte
ranges under the guest mid-read (the §4.2 mutation-under-read hazard).

#### `document.byte-len`

```wit
byte-len: func() -> u64
```

Total byte length.

#### `document.get-text-range`

```wit
get-text-range: func(r: range) -> result<string, string>
```

The text of the `[start, end)` byte range. `err` on an out-of-range
or `end < start` range (mirrors `Buffer::slice`). Only the requested
range is sliced out of the rope — the whole document never crosses
("zero-copy at the slice level", §9.6).

**Example — Read the one byte after the caret, treating a read error as nothing there** · [`plugins/auto-pair/src/lib.rs`](../../../../plugins/auto-pair/src/lib.rs)

```rust
/// The single byte after the caret (empty string at EOL / on a read error —
/// which just means "nothing to step over", so insert).
fn char_after(ctx: &ActionContext, doc: &Document) -> String {
    doc.get_text_range(Range {
        start: ctx.cursor,
        end: one_right(ctx.cursor),
    })
    .unwrap_or_default()
}
```

#### `document.line`

```wit
line: func(n: u32) -> option<string>
```

Line `n` (0-based) as text without its trailing newline (matching
`Buffer::line`), or `none` when `n` is past the last line.

**Example — Read the first line of an operator's range (and the buffer's path), erring if it is gone** · [`crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs)

```rust
let line = doc
    .line(ctx.range.start.line)
    .ok_or_else(|| format!("fixture: no line {}", ctx.range.start.line))?;
// The PATH as well as the text. An operator's handle was minted
// with `path: None` at first, so `document.path()` answered
// `none` for every real file — invisible until a plugin asked.
let path = doc.path().unwrap_or_else(|| "<none>".to_string());
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("op|{path}|{line}"),
})])
```

#### `document.line-count`

```wit
line-count: func() -> u32
```

Lines the document has: `"a\nb\n"` is two lines, and so is
`"a\nb"` — a trailing newline terminates the last line rather
than starting another. Safe as the bound of a
`0..line-count` walk calling `line`.

#### `document.path`

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

**Example — Derive a sibling `<file>_archive` path from the buffer's own path and append to it** · [`crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/grammar-guest/src/lib.rs)

```rust
let Some(mine) = doc.path() else {
    return Err("fixture: this buffer has no file".to_string());
};
Ok(vec![Effect::WriteToFile(WriteToFilePayload {
    path: format!("{mine}_archive"),
    anchor: FileAnchor::End,
    text: "* Archived beside me\n".to_string(),
    cut: None,
    create_parents: false,
    save: false,
})])
```

## Types (1)

### record `buffer-snapshot`

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

**Fields**

- `buffer-id`: `u32`
- `path`: `option<string>`
- `language`: `option<string>`
- `cursor`: [`position`](types.md#record-position)
- `selection`: `option<tuple<position, position>>`

