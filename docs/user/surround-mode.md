---
summary: "vim-surround: ds, cs, ys operators — delete, change, and add surrounding pairs of brackets, quotes, and tags."
related: [surround, ds, cs, ys]
---

# surround-mode

vim-surround operators for working with surrounding pairs — delete,
change, or add brackets, quotes, and other paired characters around
text. Always active on document buffers; no toggle needed.

> **Status:** native built-in (not WASM). Full `ys{motion}{char}`,
> `ds{char}`, `cs{char1}{char2}`, and visual `S{char}` supported.
> HTML/XML tag targets (`cst<div>`, `dst`, `ysiwt<div>`) deferred
> to a future release.

---

## Quick reference

| Keystroke | Meaning |
|-----------|---------|
| `ds"` | **Delete** the surrounding double quotes |
| `cs"'` | **Change** surrounding double quotes to single quotes |
| `cs(]` | **Change** surrounding parens to square brackets |
| `ysiw"` | **Wrap** the inner word in double quotes |
| `ysw)` | **Wrap** from cursor to next word-start in parens |
| `yss"` | **Wrap** the current line in double quotes |
| `S"` (visual) | **Wrap** the visual selection in double quotes |

---

## Three operators

### `ds` — delete surrounding

Type `ds` followed by a pair character to delete the nearest
enclosing pair.

```
Before:  "hello world"         cursor on 'w'
Type:    ds"
After:   hello world
```

Any bracket or quote works:

```
Before:  (some text)           cursor inside parens
Type:    ds(
After:   some text

Before:  [list item]           cursor inside brackets
Type:    ds]
After:   list item
```

If the cursor is not inside a matching pair, nothing happens.

### `cs` — change surrounding

Type `cs` followed by the OLD pair character, then the NEW pair
character. The surrounding pair is replaced in place.

```
Before:  "hello"               cursor in "hello"
Type:    cs"'
After:   'hello'

Before:  (some text)           cursor inside parens
Type:    cs(]
After:   [some text]

Before:  {key: value}          cursor inside braces
Type:    cs}]
After:   [key: value]
```

The operator also handles symmetric pairs (`"` → `"`, `'` → `'`):

```
Before:  "quoted text"         cursor in "quoted"
Type:    cs"`                  double → backtick
After:   `quoted text`
```

### `ys` — add surrounding (wrap)

Wrap text in a pair. The general form is `ys{motion}{char}`:

```
Before:  hello world           cursor on 'h'
Type:    ysw"                  "yank-surround word with double-quotes"
After:   "hello" world

Before:  hello world           cursor on 'l' (in hello)
Type:    ysiw(                 "inner word with parens"
After:   (hello) world
```

#### `yss` — wrap the current line

Doubling the `s` wraps the entire current line:

```
Before:  some text              cursor anywhere on the line
Type:    yss"
After:   "some text"
```

Works with any pair character:

```
Before:  fn main() {            cursor on this line
Type:    yss{
After:   {fn main() {}}
```

#### Visual `S` — wrap the selection

In Visual mode, press `S` then a pair character to wrap the
selection:

```
1. Select text in Visual mode:  v   (or V for linewise)
2. Press S followed by the pair char

Visual selection: hello
Type:             S"
Result:           "hello"
```

---

## Supported pairs

| Character | Open | Close | Symmetric? |
|-----------|------|-------|-----------|
| `(` / `)` | `(` | `)` | No |
| `[` / `]` | `[` | `]` | No |
| `{` / `}` | `{` | `}` | No |
| `<` / `>` | `<` | `>` | No |
| `"` | `"` | `"` | Yes |
| `'` | `'` | `'` | Yes |
| `` ` `` | `` ` `` | `` ` `` | Yes |

Both the opening and closing character can be used as the target
in `ds` and `cs`. `ds)` and `ds(` both delete the nearest `()`
pair.

---

## Pair detection

The cursor must be inside (or between) the pair characters.
Nested pairs are handled correctly — `ds(` inside `(outer (inner) tail)`
deletes the innermost `()` pair. Symmetric pairs (`"`, `'`, `` ` ``)
are balanced by alternating open/close semantics as the scanner
walks outward from the cursor.

If no matching pair encloses the cursor, the operator silently
does nothing (vim's "no surrounding" beep-equivalent is a no-op).

---

## Dot-repeat and registers

All three operators participate in dot-repeat (`.`):

```
ds"   → deletes "..."       on "hello"
.     → deletes "..."       on the next pair you're inside
```

The yanked content from `ds` and `cs` goes to the unnamed
register and the `"0` register. Use `"ads"` to delete a
surrounding pair into register `a`:

```
"ads"  → delete the surround, yank it into register a
"ap    → paste the deleted surround text somewhere else
```

---

## Compared with vim-surround

| Feature | vim-surround | lattice surround |
|---------|-------------|-----------------|
| `ds{char}` | Yes | Yes |
| `cs{char1}{char2}` | Yes | Yes |
| `ys{motion}{char}` | Yes | Yes |
| `yss{char}` | Yes | Yes |
| Visual `S{char}` | Yes | Yes |
| Tag targets (`t`) | Yes | Deferred |
| `ysiw{char}` with count | `2ysiw"` | `2ysiw"` |
| Nested pair handling | Yes | Yes |
| Dot-repeat | Yes | Yes |
| Register yank (`"ads"`) | Yes | Yes |

---

## Activation

`surround-mode` uses `ActivationPolicy::Global` — it activates
automatically on every document buffer. There is no toggle
command (`:surround-mode`) in this release; the mode is always
active. A future update may add a typed-option gate
(`surround.enabled`).
