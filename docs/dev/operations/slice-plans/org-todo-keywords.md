# A TODO state is not a language keyword — slice plan (TK)

> Design: [`../../architecture/org-todo-keywords.md`](../../architecture/org-todo-keywords.md).
> Anchors [`../../architecture/theme-system.md`](../../architecture/theme-system.md)
> §3.1 + §5 (element identity and the override stack) and
> [`../../architecture/plugin-transients.md`](../../architecture/plugin-transients.md)
> (TK.6's menu).
>
> Plugin repo: [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** 📝 planned (2026-08-29).

---

## Why

Reported from use: *"TODO looks weird purple and all other states seem like a
comment."* Both halves are accurate, and they have three separate causes that
compound.

`org.todo-keywords` is **one flat string**, so the configuration people
actually have cannot be written down. Pasting emacs' `WAITING(w@/!)` produces a
keyword whose name is `WAITING(w@/!)`, matching nothing.

`queries/highlights.scm` carries a **hardcoded** word list and says so in its
own comment — *"a static query cannot read an option"*. Against a real
three-sequence configuration, nine of thirteen keywords are in neither list and
render as plain title text.

The four that do match **borrow the wrong meaning**: `TODO` → `Style::Keyword`,
which is whatever the theme paints `if` and `return` (the purple), and `DONE` /
`CANCELLED` → `Style::Comment`, one grey for a set that wants five colours.

---

## Decisions locked before slicing

1. **Tree-sitter, not pattern rules — the opposite of `conceal.md`, from the
   same premises.** Org's grammar *does* model the keyword position
   (`(headline (item . (expr)))`), and highlighting changes colour rather than
   text, which the keystroke contract explicitly allows to be eventual
   (*"syntax recolour may be eventual"*). A regex would additionally have to
   know the headline is not inside a `#+BEGIN_SRC` block — the phantom-match
   class OT.3 spent a phase eliminating.

2. **The query is generated from the option, at load.** `register-language`
   and `register-element` are both drained once, so colour resolves at load
   while cycling stays live (that option is already read per keystroke). Emacs
   is the same: `org-todo-keywords` is read at mode init and needs
   `org-mode-restart`. Per-file `#+TODO:` is out for the same reason — a query
   is per-language and there is no per-buffer query.

3. **Element captures take `keyword`'s priority.** Not invented — chosen so the
   change is behaviour-preserving in every dimension except the one being
   changed. Unknown capture names get `u32::MAX` (lowest), and org's query
   captures the whole `(item)` as `@text.title.N`, so a naive element capture
   would lose the overlap and paint nothing while every part looked correct.

4. **Logging specs are parsed and inert.** Acting on `@` / `!` means writing
   `:LOGBOOK:` notes on every state change — its own slice. Parsing them now is
   what lets a configuration be pasted without anything being silently misread.

5. **The user override is a theme override.** `org-todo-keyword-faces` needs no
   new mechanism; it must land in the *override* scope rather than as an element
   default, because a default sits below the theme and would lose to it.

---

## Not in this plan

**Acting on logging specs**, **`org-todo-state-tags-triggers`**, **per-file
`#+TODO:`**, and **live re-highlight on `:set`** — all four cut in the design
with reasons, the last three for the same structural one.

---

## Slices

| Slice | Description | Status |
|---|---|---|
| TK.1 | a capture name may name a theme element | 📝 |
| TK.2 | the `org-todo-keywords` grammar | 📝 |
| TK.3 | elements, and defaults that reference the palette | 📝 |
| TK.4 | the query is generated from the keywords | 📝 |
| TK.5 | `org.todo-keyword-styles` — the org-shaped override spelling | 📝 |
| TK.6 | fast select | 📝 |
| TK.7 | docs | 📝 |

### TK.1 — a capture name may name a theme element 📝

**Deps:** none. Host-side, in `lattice-syntax`.

`name_to_style` gains a fallback: a capture name that is not a builtin category
but **is** a registered theme element resolves to `Style::Element(id)`.
`capture_priority` gives such a name the priority `keyword` has.

**Generic by construction** — the host learns nothing about org, and every
language's query gains the whole theme vocabulary. `Style::Element`'s own doc
says this is what it is for; it was reachable from decorations and listing icons
and not from a query, which is the gap.

**The priority half is the part that would fail silently.** A test that only
checks "the capture resolves to an element" passes while the span loses every
overlap and paints nothing. So the test asserts the overlap directly: an element
capture over the same range as `@text.title.1` wins, exactly as `@keyword` does
today.

**Tests:** a registered element name resolving to `Style::Element`; an
unregistered dotted name still resolving to `Style::Default` (no accidental
match); a builtin name unaffected (`keyword` stays `Style::Keyword`, never an
element even if one is registered under that name); the overlap test above;
`capture_priority` equal to `keyword`'s for an element name; the resolution
happening at query-compile time rather than per span (asserted by construction —
`highlight_styles` is built once).

### TK.2 — the `org-todo-keywords` grammar 📝

**Deps:** none (plugin-side, parallel with TK.1).

One sequence per line, `sequence:` or `type:`, `|` separating not-done from
done, `(k)` fast-select keys, `(@)` / `(!)` / `(@/!)` logging specs.

Replaces `split_keywords` / `parse_keywords`, which stay as the thin accessors
the rest of the plugin already calls — the parse result grows, the call sites do
not change shape.

**Tests, and the fixture is the real configuration.** Dhruva's own three
sequences, verbatim, asserting: thirteen keywords in order; `DONE`, `CANCELLED`
done and the rest not; `type:` distinguished from `sequence:`; every
fast-select key recovered; logging specs recovered and not mistaken for part of
the name. Plus the failure modes — a malformed line skipped with the rest
surviving, a duplicate keyword keeping the first, a duplicate fast-select key
keeping the first, a keyword with a space refused.

### TK.3 — elements, and defaults that reference the palette 📝

**Deps:** TK.2.

Register `org.todo`, `org.todo.active`, `org.todo.done`, and one
`org.todo.<KEYWORD>` per configured keyword, with the conventional-vocabulary
defaults from the design's table. Palette **keys**, never literal colours, so a
colourscheme swap recolours them.

`CANCELLED` is deliberately not `DONE`'s green: achieved and abandoned are the
distinction someone scanning an agenda actually wants, and emacs' own default
config conflates them.

**Tests:** every configured keyword getting an element; the inherit chain
resolving a keyword with no explicit default through `active` / `done`; a
palette missing a key falling through rather than failing; registration failure
for one keyword costing only that keyword.

### TK.4 — the query is generated from the keywords 📝

**Deps:** TK.1, TK.2, TK.3.

`queries/highlights.scm`'s two hardcoded `#any-of?` rules are replaced by
generated per-keyword rules naming each keyword's element, appended to the
static query at `register-language`.

**The generator escapes what it interpolates.** A keyword is a plain word by
TK.2's parse, so this is belt-and-braces rather than a live hazard — but the
alternative is a config value reaching a query compiler unescaped, and TK.2's
refusal is the only thing standing between them.

**Tests:** the real configuration producing a query that compiles; each keyword
resolving to its own element in a real editor; a keyword the option does *not*
name rendering as title text (the degradation the old comment promised and the
hardcoded list could not deliver); `TODO` in the middle of a title still being
prose; a keyword inside a `#+BEGIN_SRC` block not highlighted — the case the
regex alternative could not get right, asserted here so the choice is on the
record.

### TK.5 — `org.todo-keyword-styles` 📝

**Deps:** TK.3.

The org-shaped spelling of a theme override, landing in the override scope so
it beats the theme.

**Verify the scope before building the option.** `theme-system.md` §5 puts a
theme, `:set ui.*`, user TOML and `init.rs` in one scope; which wins between
them is what decides whether this option can be implemented as written or
whether the honest answer is to document the native `ui.elements` path instead.
If the native path is the only correct one, this slice ships documentation and
`:describe-*` discoverability rather than a second option — that is a smaller
deliverable, not a failed one.

**Tests:** an override beating the element default; an override beating an
active theme's styling of the same element; an override for an unknown keyword
being inert rather than an error.

### TK.6 — fast select 📝

**Deps:** TK.2, TK.3.

A `transient-source` whose `build(ctx)` reads the current keyword set and emits
one entry per keyword, keyed by its `(k)` and styled with that keyword's own
element so the menu looks like the buffer.

**A keyword with no `(k)` still appears**, reachable by motion. A menu that
cannot reach a state the file already contains is worse than a menu with a gap
in its shortcuts.

The direct cycle chords keep working — fast-select is an addition, as it is in
emacs under `org-use-fast-todo-selection`.

**Tests:** the menu listing every configured keyword; a key selecting its
state; a keyless keyword present and selectable; a duplicate key resolving to
the first with the second still reachable; the menu reflecting a `:set` of the
option (it reads at `build`, unlike colour); dismissal leaving the headline
untouched.

### TK.7 — docs 📝

**Deps:** TK.1–TK.6.

The design fragment lands amended where the build disagreed with it.
`org-mode.md` cross-references it from the mode table. `theme-system.md` records
TK.1's capture→element bridge, since that is a general capability of the theme
system rather than an org detail. The plugin's `doc/org.md` gains the option
syntax, the element names, an override example, and the fast-select menu.
`implementation.md` gains the rows; the site sync runs.
