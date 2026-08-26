# Org capture: many templates, prompted input

**Status:** designed (OC.1–OC.6). Supersedes the single-template capture that
shipped with [`org-mode.md`](org-mode.md)'s OM.11. Depends on
[`plugin-transients.md`](plugin-transients.md) for the menu. Slice plan:
[`../operations/slice-plans/org-capture.md`](../operations/slice-plans/org-capture.md).

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
- `:clock-in` / `:clock-resume` — clocking is not built (deferred in
  `org-mode.md` §9: it needs persistent state and a modeline contribution).

## 4. Targets

```toml
target = { file = "…" }                        # append at end
target = { file = "…", headline = "Projects" } # after that headline's subtree
```

`file` is `FileAnchor::End`. `file+headline` needs the insertion **line**,
which means reading the target — the guest does it through WASI inside the
`fs:` grant capture already needs to write there, exactly as refile's picker
source reads candidate files (`org-mode.md` OM.11).

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

**A template with no questions keeps the direct prompt.** One hop, exactly as
before. The common template is a single `%?`, and routing it through a menu
would cost three keystrokes to collect the one value a prompt already asks for.

### Two submit actions, not one that guesses

The prompt hop hands its action `[text, buffer-name]`; the fields hop hands its
action `[key, answer…]`. A single action would have to sniff which shape it
got, so they are named separately (`org-capture-submit`,
`org-capture-fields-submit`) and each one's arguments are a fact rather than an
inference.

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
