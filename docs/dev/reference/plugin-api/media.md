<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `media`

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

## Uses

- [`decoration-context`](types.md#record-decoration-context) from [`types`](types.md)
- [`media-block`](types.md#record-media-block) from [`types`](types.md)

## Functions (1)

### `media-blocks`

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

**Example — Anchor image blocks to buffer lines; relative paths resolve beside the buffer** · [`crates/lattice-plugin-host/tests/fixtures/media-guest/src/lib.rs`](../../../../crates/lattice-plugin-host/tests/fixtures/media-guest/src/lib.rs)

```rust
fn media_blocks(ctx: DecorationContext, _text: String) -> Result<Vec<MediaBlock>, String> {
    if ctx.line_count == 0 {
        // Graceful: nothing to scan → a typed guest err, not a trap.
        return Err("media-guest: empty buffer".to_string());
    }
    Ok(vec![
        MediaBlock {
            anchor_line: 1,
            // Relative — the host resolves it against the buffer's own
            // directory, which is what `[[file:diagram.png]]` means.
            path: "img/diagram.png".to_string(),
            alt: Some("a wiring diagram".to_string()),
            fit: MediaFit::Contain,
        },
        MediaBlock {
            anchor_line: ctx.line_count - 1,
            path: "/tmp/absolute.png".to_string(),
            // No alt — the host falls back to the file name.
            alt: None,
            fit: MediaFit::Width,
        },
    ])
}
```

