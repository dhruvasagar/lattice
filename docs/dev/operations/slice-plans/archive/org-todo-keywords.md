# A TODO state is not a language keyword — slice plan (TK)

> Design: [`../../architecture/org-todo-keywords.md`](../../../architecture/org-todo-keywords.md).
> Anchors [`../../architecture/theme-system.md`](../../../architecture/theme-system.md)
> §3.1 + §5 (element identity and the override stack) and
> [`../../architecture/plugin-transients.md`](../../../architecture/plugin-transients.md)
> (TK.6's menu).
>
> Plugin repo: [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** ✅ complete (2026-08-29). TK.1–TK.7 all landed. Two decisions
changed during the build and are recorded in place: TK.5's override surface was
chosen against the recommendation, and TK.6 found that `Effect::ApplyEdit` is
deferred to the renderer rather than applied where it is produced — which had
been reading as a product bug in the test harness.

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
| TK.1 | a capture name may name a theme element | ✅ |
| TK.2 | the `org-todo-keywords` grammar |✅ |
| TK.3 | elements, and defaults that reference the palette |✅ |
| TK.4 | the query is generated from the keywords |✅ |
| TK.5 | `org.todo-keyword-styles` — the org-shaped override spelling |✅ |
| TK.6 | fast select |✅ |
| TK.7 | docs |✅ |

### TK.1 — a capture name may name a theme element ✅ (2026-08-29)

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

**Threaded, not global.** The theme registry lives in `ServiceRegistry`, not in
a process-wide `OnceLock`, so `compile_plugin_config` takes
`Option<&dyn ThemeRegistry>` and `register_with_grammar_themed` is the entry
point the loader calls — it already holds `env.theme_registry`. `None` is the
honest answer for the native `LangRegistry::standard()` path and every test that
does not care, and it reproduces the pre-TK.1 mapping exactly. Adding a global
would have been smaller and would have undone the reason the service registry
exists.

**Drain order is a gate, again.** The elements must exist before the language
registers, or a capture naming one resolves to `Style::Default` and renders
unstyled — silent. The loader already drains `theme` before `language`; the
`register_with_grammar_themed` doc records the dependency, the same way OM.0
recorded `grammar` before `modes`.

**Landed:** 7 tests. Beyond the obvious ones, three carry their reason:
`tk1_an_element_capture_outranks_a_title_capture` is the silent-failure guard;
`tk1_a_builtin_capture_name_is_never_shadowed_by_an_element` pins that a plugin
cannot redefine `keyword` for every language at once; and
`tk1_no_registry_is_exactly_the_pre_tk1_mapping` compares the new function
against the old one name for name, so the `None` path is proven identical rather
than assumed.

### TK.2 — the `org-todo-keywords` grammar✅ (2026-08-29)

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

### TK.3 — elements, and defaults that reference the palette✅ (2026-08-29)

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

### TK.4 — the query is generated from the keywords✅ (2026-08-29)

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

### TK.5 — `org.todo-keyword-styles`✅ (2026-08-29)

**Deps:** TK.3.

The org-shaped spelling of a theme override, landing in the override scope so
it beats the theme.

**The verification found something neither imagined option accounted for.**
`set_theme(palette, overrides)` replaces the override set **atomically**, so
there is one override map shared by the theme and by `:set ui.*`, and anything
written into it is wiped by `:colorscheme` unless something re-applies it. The
host does re-apply its own `ui.*_color` overrides; nothing would re-apply a
plugin's. Separately, the `theme` WIT seam had only `register-element` — a
plugin could not set an override at all.

**A third option was surfaced and declined.** A generic `ui.element.*` config
surface would have served every plugin, needed no new WIT, and is the missing
implementation of a mechanism `theme-system.md` §5 already describes. It was
recommended on heuristic #1 and Dhruva chose the org-shaped option instead;
that is the decision, and the cost is recorded rather than rediscovered: org
now carries a styling mechanism beside the theme's own, and the colourscheme
fragility is documented on the option itself rather than fixed.

So the slice grew a host half: `theme.set-element-override`, auto-namespaced
exactly like `register-element` so a plugin can only name elements in its own
namespace. Ownership is re-checked host-side anyway — namespacing is the
mechanism, the check is the guarantee, and "should be unreachable" is the
reasoning that makes a boundary depend on every call site staying correct.

**Landed:** 7 parser tests in the plugin, 3 host tests. The first version of
`tk5_an_override_beats_the_registered_default` asserted `None != None` —
`Palette::default()` is *empty*, so every palette reference resolved to `None`
and an override was indistinguishable from a default. It uses the populated
`default_palette()` now.

**Also fixed here, found while wiring it:** `config` and `theme` both drain at
rank 0 and the sort is **stable**, so `provides` order decides. `theme` was
listed first, which meant `register_theme_elements` could not read
`org.todo-keywords` and silently registered elements for the *default* keyword
set. A user's own states would have had no elements at all.

### TK.6 — fast select✅ (2026-08-29)

**Deps:** TK.2, TK.3.

A `transient-source` whose `build(ctx)` reads the current keyword set and emits
one entry per keyword, keyed by its `(k)` and styled with that keyword's own
element so the menu looks like the buffer.

**A keyword with no `(k)` still appears**, reachable by motion. A menu that
cannot reach a state the file already contains is worse than a menu with a gap
in its shortcuts.

The direct cycle chords keep working — fast-select is an addition, as it is in
emacs under `org-use-fast-todo-selection`.

**Landed:** 5 unit tests on `set_keyword`, 3 through a real editor. Two bugs
surfaced, and neither was where the first three rounds of diagnosis looked.

The TODO branch in `build` sat **after** the capture-templates parse, so a user
with no `org.capture-templates` got *"no capture templates"* when they pressed
the TODO chord. The test reported it in those words.

And `apply_renderer_effects` — the *test harness* — had no arm for
`Effect::ApplyEdit`, which is pushed onto `out.effects` for the renderer to
re-dispatch rather than applied where it is produced (the deferral
`apply_write_to_file` documents). The edit was correct and present in the
outcome and dropped by the harness, which read as a product bug through three
rounds. `press()` never hit it because `dispatch_chord` applies on the way
through. The arm is added, so every future test driving an edit through a
returning path is honest.

### TK.7 — docs✅ (2026-08-29)

**Deps:** TK.1–TK.6.

The design fragment lands amended where the build disagreed with it.
`org-mode.md` cross-references it from the mode table. `theme-system.md` records
TK.1's capture→element bridge, since that is a general capability of the theme
system rather than an org detail. The plugin's `doc/org.md` gains the option
syntax, the element names, an override example, and the fast-select menu.
`implementation.md` gains the rows; the site sync runs.
