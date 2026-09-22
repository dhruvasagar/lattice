# comment — toggle line comments

`gc` is an operator, so it composes with every motion and text object the
grammar has:

| | |
|---|---|
| `gcc` | the current line |
| `gc3j` | this line and the three below |
| `gcap` | the paragraph |
| `gci{` | the inside of a block |
| `gc` (Visual) | the selection |
| `3gcc` | three lines |

Run it again on the same range to uncomment.

## What "toggle" means exactly

- **A range uncomments only if every non-blank line in it is already
  commented.** A mixed range comments, rather than inverting each line —
  per-line toggling turns a partly-commented selection inside out, which is
  never the intent.
- **The leader goes at the range's shallowest indent**, not at column 0, so
  relative indentation survives the round trip.
- **Blank lines are skipped.** A commented blank line is trailing whitespace
  with extra steps.

Comment then uncomment returns the buffer to exactly what it was.

## Languages

The leader is chosen by file extension:

| Leader | Extensions |
|---|---|
| `//` | `rs` `c` `h` `cpp` `hpp` `go` `java` `js` `jsx` `ts` `tsx` `wit` |
| `#` | `py` `rb` `sh` `bash` `yaml` `yml` `toml` |
| `--` | `sql` `lua` |

An extension not in that list has no comment syntax here, and `gc` says so
rather than guessing. CSS and Markdown are deliberately absent: `/*` and
`<!--` are *block* delimiters, and one per line would write syntax errors into
your file.

## Options

| | |
|---|---|
| `comment.enabled` | Turn the plugin off. Takes `gc` with it — the chord belongs to `comment-mode`, not to the builtin grammar. |
| `comment.leader-space` | Insert a space after the leader (`// code`, not `//code`). Default on. |

## How it is put together

Worth reading if you are writing a plugin, because `comment` is the first one
to contribute an **operator** rather than actions or motions.

The operator is registered through the `grammar` seam and declares its own
chord:

```rust
grammar::register_operator(
    "comment-toggle",
    "toggle line comments over the operated range",
    &OperatorSpec {
        repeatable: true,
        chord: Some("gc".to_string()),
        doubled: Some("c".to_string()),   // the TRAILING key: `gcc`
        ..
    },
    CB_TOGGLE,
);
```

The host binds that into the universal operator-pending layer, so `gc` gets
motion targets, the doubled form, text-object pendings and find-char pendings —
the same surface a builtin operator has. That binding is a declared capability,
`grammar:chord` in `plugin.toml`: claiming keys in the grammar every buffer
shares is the most user-visible power a plugin can take, so it is requested
rather than assumed. Withheld, `comment-toggle` still registers and remains
reachable by name; only the chord stops working.

Everything lives on `comment-mode`, a `global` minor mode — every document
buffer, and deliberately not `universal`, which would also put `gc` in
`*messages*`, the file tree and help popups.

See [`plugins`](help:plugins) for the seams, and `:describe-plugin comment`
for what this one contributed.
