# Org TODO keywords, and what colour they are

Status: design fragment (2026-08-29). Slice plan:
`../operations/slice-plans/org-todo-keywords.md`.

Anchors: [`org-mode.md`](org-mode.md) §4 (the mode decomposition —
`org-todo-mode` owns this surface), [`theme-system.md`](theme-system.md)
§3.1 (element identity), §5 (the override stack), and
[`plugin-transients.md`](plugin-transients.md) (the fast-select menu).

A TODO keyword is the one part of org's syntax that is **defined by the
user**. `TODO` is only a keyword because configuration says so; under
`(type "PROJECT" "TO-READ" …)` those are keywords and `TODO` is prose.
Everything hard about this follows from that one fact.

## 1. What is wrong today

Three things, and they compound.

**The option cannot express the configuration people actually have.**
`org.todo-keywords` is one flat string — `"TODO NEXT | DONE"`. Emacs'
`org-todo-keywords` is a list of *sequences*, each `sequence` or `type`,
each keyword optionally carrying a fast-select key and a logging spec:

```elisp
(setq org-todo-keywords
      '((sequence "TODO(t)" "NEXT(n)" "|" "DONE(d)")
        (sequence "WAITING(w@/!)" "HOLD(h@/!)" "|" "CANCELLED(c@/!)" "PHONE" "MEETING")
        (type "PROJECT" "TO-READ" "READING(!/!)" "TO-WATCH" "WATCHING(!/!)")))
```

Pasted into the current option, `WAITING(w@/!)` becomes a keyword whose
*name* is `WAITING(w@/!)`, and it matches nothing.

**The highlight list is hardcoded, and the query admits it.**
`queries/highlights.scm` carries

```scheme
(#any-of? @keyword  "TODO" "NEXT" "STARTED" "WAITING" "HOLD" "PROJ")
(#any-of? @comment  "DONE" "CANCELLED" "CANCELED" "KILL")
```

with the comment *"a static query cannot read an option"*. Against the
configuration above, nine of thirteen keywords are in neither list and
render as ordinary title text.

**The four that do match borrow the wrong meaning.** `TODO` resolves to
`Style::Keyword` — whatever the theme paints `if`, `fn` and `return`,
which in a typical dark theme is a purple. A TODO state is not a
language keyword; it looked wrong because it *is* wrong. `DONE` and
`CANCELLED` resolve to `Style::Comment` and recede into grey, which is
right for `DONE` and wrong for `CANCELLED`, and in any case is one
colour for a set the user wants five colours for.

## 2. The shape of the answer

Three layers, each already existing, none of them new mechanism:

```
org.todo-keywords          →  which words are keywords, and their sequences
    ↓
tree-sitter query          →  WHERE a keyword is (first expr of a headline)
    ↓
theme element per keyword  →  what colour it is, and what a user overrides
```

The middle layer is the one that has to change, and §3 is why it stays
tree-sitter rather than becoming a pattern rule.

## 3. Why the tree, when links went the other way

[`conceal.md`](conceal.md) rejected tree-sitter for org links and this
adopts it, from the same premises. The difference is real and worth
stating, because the two decisions look contradictory:

|  | org links | TODO keywords |
|---|---|---|
| Does the grammar model it? | **No** — no `link` rule; `[[a][b]]` is undifferentiated `expr` | **Yes** — `(headline (item . (expr)))`, anchored to the first expr |
| What does the styling change? | display **text**, so geometry | display **colour** only |
| Cost of a stale tree | markup flickers between raw and concealed — a pixel change to unedited content, a standing veto | a keyword recolours a frame late, which the UX contract explicitly permits |

*"the typed character appears immediately (text synchronous; syntax
recolour may be eventual)"* — CLAUDE.md's keystroke contract. Conceal is
on the wrong side of that sentence and highlighting is on the right one.

The second row is the one that matters most. Re-deriving "the first word
of a headline" as a regex would have to also know it is **not** inside a
`#+BEGIN_SRC` block — which is exactly the phantom-match class OT.3
proved a line matcher cannot get right, and which cost a whole phase to
eliminate. The tree already knows. What the tree cannot know is *which
words count*, and that is a query parameter, not a structural fact.

So: **generate the query from the option**, and keep the structure where
it already lives.

## 4. The host change: a capture name may name a theme element

One fallback in `lattice_syntax::style::name_to_style`:

> a capture name that is not a builtin category, but **is** a registered
> theme element, resolves to `Style::Element(id)`.

`Style::Element` exists for this and says so:

> *"a WASM plugin can register a theme element by name but can **never**
> add a variant to a Rust enum. Without this, themed highlighting is
> reachable only by editing core, which makes it impossible for plugins
> by construction (paramount goal #2)."*

It was reachable from decorations and listing icons and **not** from a
tree-sitter query, which is the gap. Closing it is generic: any
language's query can now name any registered element, and org is merely
the first caller.

### 4.1 Priority, which is not a detail

`capture_priority` returns `u32::MAX` — the *lowest* precedence — for a
name it does not recognise. Org's own query captures the whole `(item)`
as `@text.title.N`, so a naive `@org.todo.TODO` would overlap the title
capture and **lose to it**, painting nothing. The symptom would be "the
feature does nothing", with every part correct in isolation.

Element-backed captures therefore take **the priority `keyword` has**.
Chosen rather than invented: `TODO` is `@keyword` today, so this keeps
overlap behaviour byte-identical and changes only the colour. A rule
that is behaviour-preserving except in the one dimension being changed
is the one that cannot surprise.

## 5. The keyword grammar

`org.todo-keywords` accepts emacs' syntax, one sequence per line:

```
sequence: TODO(t) NEXT(n) | DONE(d)
sequence: WAITING(w@/!) HOLD(h@/!) | CANCELLED(c@/!) PHONE MEETING
type: PROJECT TO-READ READING(!/!) TO-WATCH WATCHING(!/!)
```

- **`sequence:` vs `type:`** — a sequence is a workflow, cycled in
  order; a type is a set of alternatives. Both contribute keywords;
  they differ in what cycling does.
- **`|`** separates not-done from done. A sequence with no `|` has no
  done states, which is org's rule rather than a degradation.
- **`(k)`** is a fast-select key (§7).
- **`(@)` / `(!)` / `(@/!)`** are logging specs — note on entry,
  timestamp on entry, timestamp on leaving.

**Logging specs are parsed and then deliberately inert.** Acting on them
means writing `:LOGBOOK:` notes on every state change, which is its own
slice with its own tests. Parsing them now is what lets a user paste
their emacs configuration and have nothing silently misread — the
failure this replaces is `WAITING(w@/!)` becoming a keyword *named*
`WAITING(w@/!)`.

**The keyword set resolves at load.** Both `register-language` and
`register-element` are drained once, so a `:set org.todo-keywords` mid
session changes *cycling* (that option is already read per keystroke)
and not *colour*, until reload. Emacs behaves the same way —
`org-todo-keywords` is read at mode init and needs `org-mode-restart`.
Per-file `#+TODO:` lines are out for the same reason: a query is
per-language, and there is no per-buffer query.

## 6. Elements, defaults, and overrides

### 6.1 Names and the inherit chain

```
org.todo.<KEYWORD>          e.g. org.todo.WAITING
   inherits → org.todo.active   (not-done)  or  org.todo.done
   inherits → org.todo
   inherits → org
```

A keyword the user adds after load has no element of its own and
resolves through the chain, so it is styled sensibly rather than
unstyled. That is what makes the load-time resolution in §5 tolerable
instead of a cliff.

### 6.2 Built-in defaults reference the palette, not colours

Org's conventional vocabulary gets distinct defaults so the editor looks
right before anyone configures anything:

| Keyword | Default | Why |
|---|---|---|
| `TODO` | `red` / error, bold | org's own default, and what emacs users expect |
| `NEXT` | `blue` / info, bold | the "do this one" state |
| `STARTED`, `READING`, `WATCHING` | `orange` / warning | in flight |
| `WAITING`, `HOLD` | `amber` / warning, italic | blocked on someone else |
| `PROJECT` | `blue` / info | a container, not a task |
| `DONE` | `green` / success | finished |
| `CANCELLED`, `KILL` | muted + strikethrough | abandoned, not achieved |
| anything else | `org.todo.active` / `.done` | the chain |

These are **palette keys**, so a colourscheme swap recolours them, and a
palette missing a key falls through the inherit chain rather than
failing (§3.3 of `theme-system.md`).

`CANCELLED` is deliberately not `DONE`'s green. Emacs' default config
paints both green; the distinction — achieved vs abandoned — is the one
a person scanning an agenda actually wants.

### 6.3 The user override is a theme override

`org-todo-keyword-faces` needs no new mechanism: an element override in
the theme scope *is* that feature. `org.todo-keyword-styles` is its
org-shaped spelling, and both land in the same place.

The requirement is that a user override beats the theme, so it must
resolve in the **override** scope of `theme-system.md` §5 rather than as
the element's default — a default would sit *below* the theme and lose
to it, which is backwards.

## 7. Fast select

`(t)` in `TODO(t)` binds a key in a transient menu, contributed through
the existing `transient-source` seam: `build(ctx)` reads the current
keyword set and emits one entry per keyword, labelled with its state and
styled with that state's own element — so the menu looks like the
buffer.

The menu opens from org's TODO chord; the direct cycle keys keep
working, because fast-select is an *addition* to cycling and not a
replacement (emacs keeps both under `org-use-fast-todo-selection`).

A keyword with no `(k)` still appears in the menu and is reachable by
motion — dropping it would make the menu disagree with the buffer, and
a menu that cannot reach a state the file already contains is worse
than a menu with a gap in its shortcuts.

## 8. Failure behaviour

- **A malformed sequence line** is skipped with a `warn` naming the line
  and the rest of the configuration still loads — one bad line must not
  cost a user every keyword, the same proportionality `conceal.md`
  applies to a bad regex.
- **A duplicate keyword across sequences** keeps the first and warns
  with both, which is emacs' rule.
- **A duplicate fast-select key** keeps the first, warns, and leaves the
  later keyword reachable by motion in the menu.
- **A keyword that is not a plain word** (spaces, brackets) is refused
  at parse — it could never match a headline's first expr.
- **An element that fails to register** costs that keyword its own
  colour and nothing else; it inherits.
- **Diagnostics are `debug!`** except the parse warnings above, which
  are one-shot at load and genuinely user-actionable.

## 9. Paramount-goal alignment

**#1 Performance.** No new per-line work: highlighting stays in the
existing tree-sitter pass, and the query is compiled once at
registration like every other. The `name_to_style` fallback is one
hash lookup per *capture name* at query-compile time, not per span —
capture names resolve once into `highlight_styles`.

**#2 Extensibility.** The host gains one generic fallback and learns
nothing about org. A capture name naming a theme element is available to
every language, and the org plugin is the first consumer rather than the
reason.

**#3 Vim modal editing.** Cycling keeps its chords; fast-select is an
added transient, not a replacement.

**#4 Asynchronicity.** Nothing new is async. Query compilation is at
load, on the loader's off-boot thread, where grammar compilation already
happens.

## 10. Deferred

- **Acting on logging specs** (`@` / `!`) — writing `:LOGBOOK:` notes
  and timestamps on state change. Parsed now, inert now.
- **Per-file `#+TODO:` / `#+SEQ_TODO:`** — needs per-buffer queries,
  which the language registry does not have.
- **`org-todo-state-tags-triggers`** — the config sets tags on state
  change. Orthogonal to colour, and wants the same edit path the logging
  specs want.
- **Live re-highlight on `:set org.todo-keywords`** — needs a runtime
  seam to recompile one language's query; emacs does not do it either.
