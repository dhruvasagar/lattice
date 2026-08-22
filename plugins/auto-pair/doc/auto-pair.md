# auto-pair

Auto-closes brackets and quotes as you type; a `manual` style instead closes the
nearest unmatched opener when you press one key.

**On by default.** Type `(` and you get `()` with the caret between; type `)`
before an existing `)` and the caret steps over it; backspace inside an empty pair
deletes both. The full set is `() [] {}` and `"" '' \`\``.

**Options** (set via `:set`, `lattice.toml`, or `config::set_option` in `init.rs`):

| Option | Values | Default | Meaning |
|---|---|---|---|
| `auto-pair.enabled` | bool | `true` | Enable/disable `auto-pair-mode`. |
| `auto-pair.style` | `auto` \| `manual` | `auto` | `auto` completes pairs on the opening key; `manual` self-inserts and closes on the close key. |
| `auto-pair.close-key` | key | `<C-j>` | The manual-style close key. |

**The `manual` style.** With `auto-pair.style=manual`, the pair keys just insert
themselves; a single key (`<C-j>`) closes the nearest *unmatched* opener above the
caret, scanning only the enclosing lexical scope (via the tree-sitter seam, so it
stays fast on large files) and falling through to whatever else the key is bound to
when nothing is open. This is the [`vim-pairify`](https://github.com/dhruvasagar/vim-pairify)
model.

```toml
# lattice.toml — manual pairing
auto-pair.style = "manual"
```

**Testing it.** Open any file, enter insert mode, and:

- `auto` (default): type `(` → `()`, caret between; type `"` → `""`; type `)` over
  an existing `)` → steps over; `<BS>` inside `()` → deletes both.
- `manual` (`:set auto-pair.style=manual`): type `(` → just `(`; move on, then
  press `<C-j>` → the matching `)` is inserted at the nearest unmatched `(`.
