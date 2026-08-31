# Org capture: many templates, a capture buffer

> **Where the code is.** Everything this page describes is implemented in
> [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), a **separate repository**. It
> is a WASM Component plugin: nothing here is compiled into the editor, and
> lattice has no `BufferKind::Org`, no `Lang::Org` arm and no `Editor::`
> method for any of it. What lives in *this* tree is the seams the plugin
> contributes through — see [`plugin-host.md`](plugin-host.md).

**Status:** built (OC.1–OC.7). **OC.7 replaced the capture SURFACE** — where
this page says "prompt", read §8: a capture now opens a real editable buffer
holding the expanded template, and `C-c C-c` / `C-c C-k` file or discard it.
Sections 1–7 describe how a template is chosen, expanded and targeted, and all
of that is unchanged. Supersedes the single-template capture that
shipped with [`org-mode.md`](org-mode.md)'s OM.11. Depends on
[`plugin-transients.md`](plugin-transients.md) for the menu. Slice plan:
[`../operations/slice-plans/archive/org-capture.md`](../operations/slice-plans/archive/org-capture.md).

**§4's read mechanism changed during the build** — the guest cannot read a file
from a grammar action, so the target read goes through `host-services.read-file`
rather than WASI. The section says why; read it before touching that path.

## 1. What OM.11 shipped, and why it is not enough

One template, one target, one prompt. `org.capture-template` expands `%?` /
`%U` / `%T` and appends to `org.capture-file`.

Measured against a real org config, that misses most of the feature. A user
with nine templates — todo, respond, meeting, phone, to-read, to-watch,
habit, vocab — needs:

- **many keyed templates**, chosen by a keystroke,
- **prompted placeholders** (`%^{Word}`), which is the only way a template
  like a vocabulary entry can exist at all,
- **per-template targets**, including *under a named headline*,
- **an annotation** (`%a`) pointing back at where the capture was made.

And capture is bound in **org-mode's major keymap**, so today it only fires
inside an org buffer — backwards for the one verb whose purpose is capturing
from wherever you happen to be.

## 2. Where templates are declared

**A single string option, `org.capture-templates`, whose value is TOML.**

This is forced, not preferred. Capture templates are records — key,
description, target, body — and no option can hold a record: `OptionType` is
`boolean | integer | string`, and the loader's list support (ML.5) requires
*scalar* elements ("list elements must be scalars in v1"). An array-of-tables
cannot reach an option.

What makes it tolerable is that TOML carries the payload verbatim. A `'''`
literal block preserves newlines and nested `"""` blocks, so the option value
re-parses as TOML on org's side with bodies intact:

```toml
[org]
capture-templates = '''
[[template]]
key = "t"
description = "todo"
target = { file = "~/org/refile.org" }
body = """
* TODO %?
%U
%a
"""

[[template]]
key = "v"
description = "Vocab (French)"
target = { file = "~/org/vocab-french.org", headline = "Vocabulary" }
body = """
* %^{Word} :fc:
- Context: %^{Context sentence}
- Translation: %^{Translation}
"""
'''
```

`init.rs` sets the identical string as a Rust raw literal. One format, both
homes, no third place to look.

**The cost, stated:** `:describe-option org.capture-templates` shows a blob and
`:set` cannot meaningfully edit it. If structured options ever land, the
declaration migrates without the template language changing — which is why the
template language is defined here and not by the option's shape.

Rejected: a `capture-templates.toml` in an org directory (a third config
location, and a new `org.directory` option to find it — config belongs where
the user's config is); per-template flat options (`org.capture.t.body`, …),
which do not scale past two templates and make ordering implicit.

## 3. The template language

| Placeholder | Expands to |
|---|---|
| `%?` | Where the typed body goes. A template without one appends it on its own line rather than discarding it. |
| `%^{Prompt}` | Prompts, in template order. The answer is substituted at that point. |
| `%U` / `%T` | Today, inactive `[2026-08-26 Wed]` / active `<…>`. |
| `%t` | Today, active date only — org's `SCHEDULED:` form. |
| `%a` | An org link back to the buffer + line the capture fired from. |
| `%%` | A literal `%`. |

An unknown `%x` survives **verbatim**. A template is user text, and a `%d`
that did not expand can be found and fixed; one that vanished cannot.

**Cut deliberately, with reasons rather than silence:**

- `%(elisp)` — there is no elisp. `%(org-id-new)` and
  `%(format-time-string …)` would need named equivalents, which is a
  vocabulary decision, not a parser one.
- `%:from` / `%:subject` — these read a *capture context* supplied by mail or
  org-protocol. Lattice has neither, so the placeholder would always be empty.
- ~~`:clock-in` / `:clock-resume`~~ — **both ship** (OC.11, OC.9); clocking
  landed at OC.1–OC.11. `clock-in = true` on a template starts a clock on the
  entry it captures, with the `:LOGBOOK:` drawer written into the captured text
  so it rides the same single write — capture files into another file, and an
  `apply-edit` names a buffer id an unopened file does not have. `:clock-resume`
  is an ex-command rather than a template key, because resuming is something you
  do to the last entry you clocked, not something a template describes.

## 4. Targets

```toml
target = { file = "…" }                        # append at end
target = { file = "…", headline = "Projects" } # after that headline's subtree
```

`file` is `FileAnchor::End`. `file+headline` needs the insertion **line**, which
means reading the target file: find the headline, then anchor at the last line of
its subtree plus one — the same computation refile makes, sharing
`headline::subtree_end` so the two cannot drift apart.

**The read goes through `host-services.read-file`, not WASI**, and this paragraph
originally said the opposite. Capture runs as a *grammar action*, which the host
calls synchronously on the dispatch thread from a separate sync linker;
`wasmtime-wasi`'s sync filesystem shim blocks on a runtime internally, so a guest
`std::fs::read_to_string` there panics rather than reading. Refile is not the
precedent it looked like — refile never reads a file, it computes its insertion
line from buffer text handed to it by a *picker*, and pickers run on the async
linker where WASI works. `read-file` is gated on the same `fs:` grant capture
already needs in order to write, so this costs no new capability.

Inserting after the **whole subtree** rather than directly under the headline is
deliberate: filing at the top would put each new entry in front of everything
already there, so the subtree would read newest-first while the file around it
reads oldest-first.

Headline matching ignores case, collapses inner whitespace, and strips a leading
TODO keyword and trailing `:tags:`. A user names the target once in a config
file; adding `:drill:` to that headline months later must not silently send
every future capture to the bottom of the file. (This is the opposite of
`refile::title_of`, which deliberately *keeps* both — its consumer is a picker,
where typing `TODO` is a useful way to find unfinished work.)

A named headline that is absent **appends and says so**, rather than creating
it or refusing. The note is not lost, and the echo is what tells the user
their target moved.

## 5. Collecting several answers

`%^{Prompt}` needs N answers before a single write. That is the only way a
template like a vocabulary entry can exist at all: it is not one line of typed
text, it is several named fields.

**The template's questions become FIELDS on a menu**, not a run of prompts.
`<leader>oc` opens the template chooser; the key picks a template; a template
that asks questions then opens its own fields menu — a row per question, a row
for the body, and a row that captures.

The mechanism is the host's and magit already uses it: pressing a field's key
parks the whole menu, a one-line prompt takes the value, it lands in
`TransientState`, and the menu comes back. `<Esc>` cancels the value with the
menu untouched. See `plugin-transients.md` §7 for the seam half (TR.3b).

**Rejected: sequential prompts carrying their answers in
`open-prompt-payload.buffer-name`.** That channel is real and documented, and
magit's blame / diff / revision modes use it — but for buffer *identity*, not
multi-step input. Accumulating answers through it would have been a second
spelling of a mechanism the editor already has, with a bespoke codec on top.

The visible difference is that the menu stays the surface throughout, so an
answer can be re-edited before anything is written; a questionnaire has already
moved on by the time you notice the typo. It diverges from emacs org-capture,
which asks sequentially — a real muscle-memory cost, accepted because the
mechanism is one the editor already has and the form is the better surface.

**A template with no questions skips the fields menu.** Nothing to ask, so the
capture buffer (§8) opens straight away. Before OC.7 this said "keeps the
direct prompt", which was the same decision about a surface that no longer
exists.

**The fields menu survived OC.7 unchanged**, and the order it produces is
emacs's: answers first, then the buffer. What changed is only what happens
after the last answer — the menu used to write the file, and now it opens the
capture buffer with the answers already substituted. Its body row seeds the
`%?` point rather than being the final word, so an answer typed into the menu
is a draft you keep editing.

### Two submit actions, not one that guesses

The prompt hop hands its action `[text, buffer-name]`; the fields hop hands its
action `[key, answer…]`. A single action would have to sniff which shape it
got, so they are named separately (`org-capture-submit`,
`org-capture-fields-submit`) and each one's arguments are a fact rather than an
inference.

OC.7 adds a third, `org-capture-finalize`, which takes no arguments at all —
its input is the buffer. The same reasoning applies: the surfaces differ, so
the actions do.

### Field names are positional

`q0`, `q1`, … rather than the question text. A template may legitimately ask
the same question twice — `%^{Line}` in a list template plainly means two
different lines — and two rows sharing a state key would overwrite each other.

Expansion consumes the answers positionally too, which is why a question that
was never asked (an empty `%^{}`) consumes none: shifting the sequence would
substitute every later answer one slot early, and the result would look
plausible while being wrong.

### Two host bugs this uncovered

Both are recorded in the slice plan and were the same shape: a seam wired end
to end whose one real path no test took.

- `Effect::OpenPrompt` was **unusable by any plugin** — the submit resolved its
  handler in the `ActionHandlerRegistry`, which the plugin seams do not
  register into (OC.3a).
- A plugin's transient **row could never fire**, for the same reason, and the
  transient seam's store had no config registry, so org's menu built from a
  template set it could not read (OC.3).

## 6. The menu, and the prefix

`org-global-mode` — a minor with `ActivationPolicy::Universal`, the
`magit-global-mode` precedent — owns the `<leader>o` prefix everywhere:

- `<leader>oa` → agenda,
- `<leader>oc` → the capture transient, one key per template.

Universal because both verbs are global: capture is worthless if it only
works where org files already are.

**A second on-by-default mode needed a host change (OC.1a).** A plugin minor
stays inert until it is enabled (`auto_activatable_minors` filters on
enablement, CI.3), and enablement is triggered by the manifest's `default_mode`
— which was a single string. Org now has two modes that must be on out of the
box: `org-todo-mode` inside org files and `org-global-mode` everywhere. Naming
one left the other registered, correct, and permanently inert, which presents
as a chord that silently does nothing. `default_modes` (plural) is the fix; the
singular key still parses and folds in. Still ONE `<id>.enabled` gate, because
the user is turning org on or off, not curating its internals.

**This fragment originally specified `<C-x>o`, and that prefix cannot work.**
Org's own MAJOR keymap already binds a *terminal* `<C-x>` — timestamp decrement
(OM.9, which deliberately shadows vim's decrement inside org buffers). A prefix
in one layer and a terminal binding in another is precisely the ambiguity vim
resolves with `timeoutlen`, and this editor has no ambiguous-chord timeout: in
an org buffer `<C-x>` fires the decrement and the second key never arrives. The
collision was found by driving the real chord in a test rather than by reading
the keymap.

`<leader>o` replaces it and is strictly better on every count. It breaks no
muscle memory — capture already lived at `<leader>oc`, and the only change is
that it now works outside an org file. It needs no new territory. And it drops
the `action:next-pane` shadowing (emacs's `other-window`) this section had
accepted as a price, so the emacs-keys layer keeps its promise.

Layered prefixes compose: the major's `<leader>oh` and the universal minor's
`<leader>oc` both resolve, which the tests pin — that was the one property
worth checking before committing to the prefix.

## 7. Paramount-goal alignment

**#2 Extensibility.** Nothing in lattice learns what a capture template is.
The one host change (`plugin-transients.md`) is generic and names no org
concept.

**#1 Performance.** Parsing happens on option change, not per keystroke; the
menu builds on open; the target read happens on submit. Nothing is on the
typing path.

**#3 Vim modal editing.** The prefix is a chord in a mode's keymap layer, and
the templates' actions are ordinary grammar actions — capture extends the
grammar rather than escaping it.

---

## 8. The capture buffer (OC.7)

**Emacs opens a buffer; lattice opened a one-line prompt.** `Effect::OpenPrompt`
carried `initial: String::new()`, so the template was never shown — it was
expanded only at submit. You typed into an empty minibuffer and found out what
the template did afterwards. Reported as "I don't see a capture buffer at all",
which was literally true.

Now: pick a template → prompts for any `%^{…}` → a real editable buffer holding
the expanded template, caret where `%?` was. `C-c C-c` files it, `C-c C-k`
discards it.

| | emacs | lattice |
|---|---|---|
| template menu | temp window, one key each | the `<leader>oc` transient |
| capture surface | a buffer | **a buffer** |
| editing | free, multi-line, point at `%?` | **the same** |
| finalize / abort | `C-c C-c` / `C-c C-k` | **the same** |

### It was not possible before OC.7a

A native mode fills its own synthetic buffer from `on_activate`. The `modes`
WIT seam is **declaration-only** — a guest exports `register-modes` and nothing
else — so a plugin mode has no such hook, and a guest emitting
`Effect::OpenSyntheticBuffer` got a buffer it could never put a character into.
The other routes are closed too: `effect.apply-edit` names a `buffer-id` the
open does not hand back, and event handlers act through APIs rather than
returning effects.

So `open-synthetic-buffer-payload` gained `content`, `cursor` and
`activate-minor` (OC.7a). All optional; omitting them is the pre-OC.7a effect
exactly, which a dozen native emitters rely on. See
[`plugin-host.md`](plugin-host.md).

### `%?` inverts

The prompt substituted `%?` with text that arrived *before* the expansion. A
buffer expands first and you type into it, so `%?` stops being a value and
becomes a **position**.

`expand_for_buffer` finds it by expanding with a NUL sentinel and removing it,
rather than walking the template a second time — every other placeholder
(`%U`, `%T`, `%^{…}`, `%a`) still has to expand around it, and a second copy of
those rules would drift. (`%t` was added to one such copy and not another
once already.)

A template with **no** `%?` puts the caret at the end, which is emacs's
behaviour and which falls out rather than being coded: `expand_with` already
appends non-empty text on its own line so the prompt flow could not silently
discard what you typed. One rule, two jobs. Finalize trims trailing newlines so
that placement does not write a blank line into your file on every capture.

### The chords ride a minor, on an `org-mode` major

A capture buffer **is** an org buffer — you want org's grammar, motions,
folding and TODO cycling while writing the entry — so its major is `org-mode`
and only the finalize/abort pair is capture-specific. Those live on
`org-capture-mode`, a minor with `Manual` activation named by the effect that
opens the buffer.

On the major, `C-c C-c` would file-and-close every org file you touched.
Scoping to a minor activated on exactly one buffer is also what makes `<C-c>`
safe to bind at all, since it is vim's interrupt.

**Org-roam capture reuses this unchanged** (OR.11): same buffer, same chords,
same handlers. Only the order of what is asked before the buffer opens differs
— roam picks the title first, then the template.

### Aborting creates nothing

`C-c C-k` writes no file, and that **falls out of the buffer model** rather
than being cleaned up: the file is written on finalize, so an abort has nothing
to undo. It is the property OR.6's `WriteToFile`-on-create does not have, and a
large part of what the ABI addition bought.

### Known gaps

- **The pane does not return to where the capture was fired from.** Finalize
  closes the buffer and lands on whatever the host falls back to. A guest
  cannot fix it: `switch-buffer` is a picker-accept outcome, not an `effect`.
  Tracked as OC.7d; pinned by an assertion so the day it changes, a test says
  so.
- **One capture in flight.** The finalize handler recovers its target from
  guest-side state, because the action context carries a `buffer-id` but no
  buffer *name*, and a synthetic buffer's `document.path()` is `none`. Emacs's
  default is likewise one.
