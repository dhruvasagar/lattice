+++
title = "Tree-sitter Text Objects"
+++


Design fragment for Lattice's **structural text objects** — `af` (a function),
`ic` (inner class), `aa` (an argument), etc. — read from the tree-sitter parse
tree. They are first-class citizens of the vim grammar: registered through the
same `register_text_object` API as the classic objects (`iw`, `ap`, `i"`), and
composable with **every** operator — `daf`, `vic`, `yaa`, and the `zn` narrow
operator (`znaf`). Slice sequencing lives in
[`../operations/slice-plans/archive/narrow-mode.md`](../operations/slice-plans/archive/narrow-mode)
(N.1.4); narrow-mode is the first consumer but not the owner.

---

## 1. Design goal

The classic vim text objects are delimiter- and whitespace-based — `iw`, `ap`,
`i{`, `i"`. They don't know what a *function* or a *type* is. Tree-sitter text
objects add **structural** objects sourced from the syntax tree, the same parse
that drives highlighting and folding:

- They compose with the whole grammar: any operator + any structural object.
  Adding a structural object is one `register_text_object` call — paramount
  goal #3 (the grammar IS the public API; new text objects are first-class).
- Capture names follow nvim-treesitter / Helix (`@function.outer`,
  `@class.outer`, …); `.outer` is the whole construct, `.inner` the body
  without delimiters.
- They are **universal**, not narrow-scoped. `daf` works in any code buffer.
  A language with no `textobjects.scm` simply has no structural objects; the
  classic objects still work.

---

## 2. Reference points + the key-space constraint

| Editor | Function | Class/type | Parameter | Comment | Basis |
|---|---|---|---|---|---|
| nvim-treesitter (vim ecosystem) | `f` | **`c`** | `a` | (varies / unbound) | community standard |
| Helix | `f` | **`t`** | `a` | **`c`** | built-in `mi`/`ma` |
| vim-commentary | — | — | — | `gc` (operator + object) | tpope |

Two conventions **disagree on `c`**: nvim uses `c` = class; Helix uses `c` =
comment, `t` = type. The deciding constraint is local: **`t` is already the tag
object in Lattice**, so Helix's `t` = type is unavailable. That makes `c` =
class (nvim) the natural pick, and pushes comment off `c`.

Per the editor-design-references rule (Vim/Emacs ecosystem weighted heavily for
grammar-convention decisions), nvim-treesitter is the anchor for
function/class/parameter. Comment has no nvim convention and `gc`-the-object
can't express inner-vs-outer (§5.3), so Lattice uses `aC`/`iC` and reserves
`gc` for a future commentary-style comment *operator*.

---

## 3. The keybinding catalog (locked 2026-06-10)

Reserved by the classic objects (cannot reuse): `w W s p b B t ( ) { } [ ] < > " ' \``.

| Object | `a` (outer) | `i` (inner) | `capture_suffix` | Basis |
|---|---|---|---|---|
| **Function** | `af` | `if` | `function.outer` / `.inner` | nvim + Helix |
| **Class / type** | `ac` | `ic` | `class.outer` / `.inner` | nvim (`t`=type blocked by tag) |
| **Parameter / arg** | `aa` | `ia` | `parameter.outer` / `.inner` | nvim + Helix |
| **Loop** | `al` | `il` | `loop.outer` / `.inner` | `l` = loop, free |

Innermost-wins: a cursor inside a closure nested in a function resolves the
**closure** for `af` (smallest containing span — see §5.1).

**Comment (`aC` / `iC`) is NOT here.** A comment object needs only the
language's comment leaders (`//`, `#`, `/* */`) — a `commentstring`-style
descriptor — not the parse tree. It is text/line-oriented like `ip` / `i{`,
works for any language with a known comment leader (even with no tree-sitter
grammar), and so belongs in **`lattice-grammar`** with the classic objects, not
here. The keys stay `aC`/`iC` (capital `C` frees lowercase `c` for class and
leaves `gc` for a future comment *operator*); `iC` = the comment run with its
leaders stripped, `aC` = with them. Its design — the `commentstring` / comment-
leader source and the line-run scan — is deferred to its own slice; do not fold
it into the tree-sitter object slices.

**Deferred** (documented for users, not in the first slices): `call`,
`conditional`, `block` as `a`/`i` objects. `block`'s natural keys (`b`/`B`/`{`)
are all taken by the classic paren/brace objects; it stays reachable via the
classic `i{`/`iB` and via `zn`+motion. `call`/`conditional` have no clean free
mnemonic and low marginal value (reachable via `i(` / `if` / `zn`+motion); they
land if a concrete need surfaces, each as one `register_text_object` line.

---

## 4. Architecture — where they live

The `register_text_object` API already exists and is symmetric to
`register_operator` / `register_motion`:

```rust
// lattice-grammar::registry — ALREADY PRESENT
pub fn register_text_object(&mut self, name: &str, doc: &str, spec: TextObjectSpec) -> TextObjectId;
type TextObjectFn = Box<dyn Fn(&TextObjectContext) -> GrammarResult<ProtoRange> + Send + Sync>;
```

The **only** gap is that `TextObjectContext` can't see the parse tree — the
classic objects compute from raw buffer text. One new grammar primitive closes
it, keeping `lattice-grammar` syntax-agnostic:

```rust
// lattice-grammar — NEW (N.1.4)
pub trait ScopeResolver {
    /// Innermost tree-sitter node whose capture name ends with `suffix`
    /// containing (line, col_byte), as a byte-precise half-open
    /// `[start, end)` ProtoRange (N.1.4c — line + byte *column*, not just
    /// rows, so intra-line objects like `aa` are charwise-accurate; end
    /// exclusive, matching tree-sitter node ranges + the operator slice
    /// convention).
    fn scope_at(&self, line: u32, col_byte: u32, suffix: &str) -> Option<ProtoRange>;
}

pub struct TextObjectContext<'a> {
    pub buffer: &'a Buffer,
    pub at: Position,
    pub count: Count,
    pub args: Args,
    pub cancel: &'a CancellationToken,
    pub scope_resolver: Option<&'a dyn ScopeResolver>,   // ← NEW
}
```

Three-crate split (no `grammar → syntax` dependency, so no cycle):

| Crate | Owns |
|---|---|
| **`lattice-grammar`** | the `ScopeResolver` trait + the `scope_resolver` context field. One small primitive; stays syntax-agnostic. |
| **`lattice-syntax`** | **owns and defines the objects.** New `lattice-syntax → lattice-grammar` dep. A `register_syntax_text_objects(&mut CommandRegistry)` entry point registers the `af`/`if`/`ac`/`ic`/`aa`/`ia`/`al`/`il`/`aC`/`iC` `TextObjectSpec`s (each `apply` calls `ctx.scope_resolver?.scope_at(…)`). `impl ScopeResolver for SyntaxSnapshot` forwards to the `scope_at_cursor` shipped in N.1.0. |
| **`lattice-host`** | at boot calls `register_syntax_text_objects`; at dispatch threads the active buffer's `SyntaxSnapshot` into `TextObjectContext.scope_resolver`. The only layer holding both grammar and syntax. |

`builtins.rs` keeps the **text-based** objects; `lattice-syntax` owns the
**tree-based** ones. Both register into the same `CommandRegistry`, so both are
first-class — visible to `:describe-key`, usable with every operator, no
kind-branching anywhere.

**Why the trait, not a direct `SyntaxSnapshot` field.** Naming `SyntaxSnapshot`
in `TextObjectContext` would force `grammar → syntax`, inverting the layering
(modal grammar is lower-level than tree-sitter; vim editing must work with no
parser). The trait keeps grammar agnostic; `lattice-syntax` provides the impl.
The same hook serves any future syntax-backed object and plugin-contributed
objects.

---

## 5. Resolution

### 5.1 `scope_at_cursor`

`SyntaxSnapshot::scope_at_cursor(line, col_byte, suffix) -> Option<ProtoRange>`
runs the language's `textobjects.scm`, restricts the query to the cursor byte,
and among captures whose name ends with `suffix` and whose span contains the
cursor, returns the **smallest** (innermost) as a byte-precise `[start, end)`
range. `None` when no parse, no `textobjects.scm`, or no match.

Capture status: `function.outer` / `class.outer` / `block.outer` shipped in
N.1.0 (rows only). N.1.4c (a) made the return **byte-precise** (line + byte
column), and (b) authored the remaining captures — `function.inner`,
`class.inner`, `parameter.outer` / `.inner`, `loop.outer` / `.inner` — for
rust / python / javascript, each unit-tested against known snippets.

### 5.2 `a` vs `i` (outer vs inner)

- **outer** (`af`/`ac`/…) = the whole construct: signature → closing brace.
  `@<obj>.outer`.
- **inner** (`if`/`ic`/…) = the body sub-node (a function's block, a class's
  field/declaration list, a loop's body). `@<obj>.inner`, authored per language.

**v1 resolution limitations** (the single-node resolver returns a capture
node's span verbatim; refinements are follow-ups, not blockers):

- **Inner-body delimiters.** `@function.inner` / `@loop.inner` capture the body
  *block* node, whose span **includes** its braces in brace languages
  (rust / js). Python's suite is delimiter-free, so its inner objects are clean.
  Delimiter-stripping (returning the span *between* `{` and `}`) is a future
  resolver refinement.
- **Parameter outer == inner.** `@parameter.outer` falls back to the bare
  parameter node — the single-node resolver can't extend the span to include a
  trailing comma/separator (nvim does this with `#make-range!` predicates Lattice
  doesn't yet model). So `aa` and `ia` resolve identically for now. The
  byte-precision win still holds: `daa` deletes exactly `x: i32`, not the whole
  signature line.
- **Visual objects** (`vaf`) are not yet bound: the builtin objects (`viw`) have
  no Visual keymap binding either, so structural objects match that parity.
  Visual text-object support is a separate slice covering *all* objects.

### 5.3 Comment lives in `lattice-grammar`, not this fragment

`aC`/`iC` is **not** a tree-sitter object (§3) — it's driven by the language's
comment leaders (`commentstring`-style), not the parse tree, so it works for any
language with a known leader and belongs with the classic objects in
`lattice-grammar`. Its inner/outer split is leader-stripping (`iC` = the comment
run minus `//` / `#` / `/* */`; `aC` = with them) — text-level, not a query
capture, which is exactly why `aC`/`iC` beat a single-unit `gc` object. Full
design (the comment-leader source + the line-run scan) is deferred to its own
slice.

---

## 6. Composition

Because they register as ordinary grammar objects, they compose with every
operator with zero per-operator work:

```
daf   delete a function          vac   select a class
yif   yank a function body       cia   change an argument
=af   reindent a function        zniC  narrow a comment's text  (zn = narrow operator)
guif  lowercase a function body  >al   indent a loop body
```

The `zn` narrow operator (design: narrow-mode.md §6) is just another operator —
it consumes whatever range the object produces. `znaf` narrows a function;
`znip` narrows a paragraph (classic object); both flow through the same
operator-pending → `OperatorContext.range` path.

---

## 7. Performance + error handling

| Operation | Cost | Note |
|---|---|---|
| object resolution (`scope_at`) | O(matches at cursor) ≈ O(1) | byte-range-restricted query; one-shot per object use |
| comment marker strip | O(marker length) | a few bytes |

`scope_at` returning `None` makes the text object **fail the operator** — vim's
bell / no-op, exactly as `di(` does with no surrounding parens. Never panics,
never edits. A Plain buffer (no `scope_resolver`) fails the same way. No new
bench gate; `scope_at_cursor` is already covered by `lattice-syntax`'s tests and
runs off any hot path (one-shot per keystroke-sequence completion).

---

## 8. Rejected alternatives

**`gc` as the comment object (vim-commentary).** Strong muscle memory, but a
single-unit object — it cannot express inner (text) vs outer (with markers),
which is genuinely useful (`diC` rewrites a comment's text in place). It is also
a `g`-prefixed object, needing a new resolution path distinct from the `a`/`i`
machinery. Rejected for the object; **`gc` is reserved for a future
commentary-style comment operator** (`gcc`, `gcap`), where it belongs.

**Helix's `c` = comment, `t` = type.** Blocked: `t` is the tag object in
Lattice. Following Helix would strand `class`/`type` with no good key. nvim's
`c` = class is the better fit given the constraint.

**`grammar → syntax` dependency** (so `TextObjectContext` could carry a
`SyntaxSnapshot` directly). Rejected: inverts the layering — modal grammar is
lower-level than tree-sitter and must function without a parser. The
`ScopeResolver` trait gives the same capability with the dependency pointing the
right way (`syntax → grammar`).

**Objects owned by narrow-mode / a provider.** Rejected: these are universal
vim-grammar objects (`daf` everywhere), not narrow-specific. They live in
`lattice-syntax` and register as first-class grammar objects; the `zn` operator
(narrow-owned) merely composes with them.

**`block` on `ab`/`ib`.** Rejected: those are the classic paren object. The
tree-sitter block stays reachable via classic `i{`/`iB` + `zn`+motion rather
than shadowing a long-standing binding.
