---
title: Typed configuration
related: [config, init, plugin, options, customize, describe-option, schema]
---

# Typed configuration — a declared schema, not a TOML blob

Slice plan: [`typed-configuration.md`](../operations/slice-plans/typed-configuration.md).
Extends design §5.12 (typed options + customize) and
[`mode-architecture.md`](mode-architecture.md) §6.6, whose WIT sketch this
supersedes. Companion: [`config-and-init.md`](config-and-init.md), which owns
*when* configuration arrives; this fragment owns *what shape it has*.

## 1. The problem

Five org options are hand-rolled encodings. `capture-templates`,
`agenda-sections` and `agenda-custom-commands` are **TOML inside a string**;
`todo-keywords`, `todo-keyword-styles` and `agenda-files` are line formats. Each
ships its own parser and its own error messages, and org carries the `toml`
crate inside its wasm to read three of them.

The cause is one narrow seam, not a general limitation. The ABI already carries
~147 records and variants — `transient-spec`, `picker-source-spec`, `entry`,
`clock-span`. Structured data crosses everywhere **except configuration**, where
`register-option` takes `boolean | integer | string` and values move as
`get-option -> option<string>` / `set-option(name, value: string)`.

`capture_templates.rs` states the constraint plainly and correctly: "an
array-of-tables cannot reach an option at all." The blob is not a shortcut
somebody took; it is the only thing that fits through the hole.

**What it costs, beyond ugliness.** Design §5.12 promises `:customize` as "a
type-aware editing buffer" that writes back to user TOML. That is impossible
over a blob and straightforward over a declared schema — so the promise is
currently unkeepable, which is the strongest argument for doing this rather than
living with the encodings. `:describe-option` shows a wall of TOML.
`:set org.capture-templates=…` is unusable. And a malformed field is reported by
whichever plugin happened to write the best message.

**What it is NOT.** This does not fix a value arriving too late — that is
ordering, and [`config-and-init.md`](config-and-init.md) §4.1's
`pre-plugin-loaded` is its answer. No option *shape* fixes an ordering defect
and no ordering fixes a shape one; the two were entangled in the original
problem report and are separate work.

## 2. The shape

WIT has no generics, so a plugin-defined record cannot be a fixed host-side
type — the host would need a different record per plugin, which a shared ABI
cannot have. The expressible answer is **self-description**:

- an option **declares a schema** — field descriptors (name, kind, required,
  doc, nested fields), which is ordinary WIT data;
- values cross as a **generic value tree**;
- the **host** validates the tree against the schema, so a bad `todo-only` is
  rejected with a *path* rather than by each plugin's hand-rolled message.

```
schema  = scalar(bool | int | string)
        | enum(list<string>)
        | list(schema)
        | record(list<field>)          field = { name, schema, required, doc }

value   = bool(bool) | int(s64) | string(string)
        | list(list<value>)
        | record(list<tuple<string, value>>)
```

### 2.0 …except that WIT has no recursive types

The shape above is the *host's*. It is not what crosses, and the difference is
not cosmetic: WIT rejects a variant whose arm holds the same variant —
`type config-schema depends on itself` — so a schema cannot be spelled that way
on the wire at all. This was discovered by the compiler while implementing TC.3;
the fragment as first written proposed something unrepresentable.

Both trees therefore cross as an **arena**: a flat list of nodes plus the index
of the root, with children referenced by index.

```wit
record schema-field { name: string, schema: u32, required: bool, doc: string }
variant schema-node  { scalar(option-type), enum-of(list<string>),
                       list-of(u32), record(list<schema-field>) }
record config-schema { nodes: list<schema-node>, root: u32 }
// …and the same shape for value-node / config-value.
```

An arena is guest-controlled input in a shape that invites two failures nothing
else on this boundary has: an index pointing outside the list, and an index
pointing back up the tree. The second is the dangerous one — following it
naively is unbounded recursion on the *host's* stack, from a value a plugin
chose. So the tree-building walk carries the set of nodes on its current path
and refuses one it is already inside. It tracks the *path*, not every node seen,
because a guest that emits one `string` node and points three fields at it has
sent a DAG, which is a perfectly good encoding of a tree; rejecting that would
punish exactly the encoding a careful generator produces.

Writing an arena by hand is unpleasant, and that is the argument for the SDK
derive rather than an objection to the encoding — there is no alternative
encoding to prefer.

### 2.0.1 Where the schema lives

On the **option**, not in the value. `Option<T>` grows a `schema: Option<ConfigSchema>`
that a plugin's declaration fills in, beside the doc and the default; it is
`None` for every option written in Rust, whose type answers `OptionType::schema()`
for itself.

A value carrying its own schema is the arrangement that does not work, and it is
worth naming because it is the obvious one. `OptionType::from_value` is a
*static* function — it has no access to the option being set — so a value that
carried its shape would lose it on the first write. Metadata about an option
belongs with the option.

The corollary is that `ConfigValue` is itself an `OptionType`, and its
`parse` / `format` are **TOML text**. That keeps the `:set` contract intact for
structured options at no cost (the host already has a TOML parser), and it means
migrating an option that *was* a TOML-in-a-string is not a break for anyone who
was setting it that way: the text they wrote still parses, it is simply now
validated against a schema.

A dictionary — a bag of string keys — was considered and rejected by the person
who would use it: it reproduces the blob's weakness (nothing to validate
against, nothing for `:customize` to render) while adding a second encoding.
Types are the point; the schema is how types survive an ABI with no generics.

### 2.1 One mechanism, not a fourth kind

**Every option is a schema plus a value.** `boolean | integer | string` are
degenerate schemas, not a separate kind of thing. The rejected alternative was
additive — a second option mechanism beside the existing one, leaving scalar
options outside the schema world — and it is rejected on heuristic #1: it is the
smaller change and the worse end state. It would leave the registry with two
option mechanisms permanently, and every consumer that renders an option
(`:customize`, `:describe-option`, `:set` completion, the TOML loader) would
grow two renderers, forever, to serve a distinction that has no meaning to a
user.

**One mechanism is a claim about the registry, not about the call count.** The
WIT surface keeps `register-option(name, ty, default, doc)` beside
`register-structured-option(name, schema, default, doc)`, and that is
ergonomics rather than a second mechanism: declaring `boolean` should not
require hand-building a one-node arena, and both calls produce the same kind of
registry entry. The test of the decision is downstream — `ErasedOption` has one
`schema()`, one `get_value()`, one `set_value()`, and a consumer never asks
which call declared the option it is looking at.

The blast radius is real and is stated rather than discovered later: every
option in the workspace goes through the re-based surface. What makes it
tractable is that **the native side is already typed** — `Option<T>` over an
`OptionType` — so a scalar's schema is *derived*, not written. `OptionType`
gains `schema()`, `to_value()` and `from_value()` with defaults good enough that
a type which declares its enumeration CLOSED gets an `enum` schema for free and
everything else gets `scalar(string)`; only `bool` and `i64` need to say so. No
existing option declaration changes.

(The "closed" qualifier is load-bearing and was learnt the hard way — see the
slice plan's TC.1 entry. `enumerate()` is documented as feeding `:set`
completion, and three types use it as a hint over an open space; deriving an
`enum` from it blindly described those as finite sets.)

### 2.2 Strings stay the `:set` surface

`:set name=value` remains string-in, string-out, and `OptionType::parse` /
`format` keep their round-trip contract. That is not a concession — a command
line is a text surface and typing a record into one is not an improvement. What
changes is that the string is now *one* front-end over the value rather than the
storage format. `:set` on a composite option addresses a leaf by path
(`:set org.capture-templates.0.key=t`) or is refused with the schema; the
detailed design of that surface is deferred to the slice that needs it, and
`:customize` is the intended editor for composites.

## 3. Both homes agree about the tree

The blob has exactly one genuine merit, and `agenda_sections`' header names it:
ONE string serves `lattice.toml` and `init.rs` identically, with no second
ordering rule to learn. A schema-shaped option gives that up, and the design has
to be honest about what replaces it.

**Each home writes the tree natively.** Not one serialization shared by both —
that would keep text-at-rest and buy only host-side validation — but the same
tree, spelled the way each home already spells structured data:

```toml
# lattice.toml — real TOML. No blob, no escaping, no inner parser.
[[org.capture-templates]]
key = "t"
description = "todo"
target = { file = "~/org/refile.org" }
body = """
* TODO %?
%U
"""
```

```rust
// init.rs — the tree, built by the SDK derive from an ordinary Rust struct.
config::set_option_value("org.capture-templates", &templates.to_value());
```

The homes are no longer copy-pasteable into each other, and that is the cost.
What they gain is that each is now *native*: the TOML is TOML rather than a
string containing TOML, so an editor highlights it and a typo is a TOML error at
the point of the typo; and the Rust is a struct rather than a raw literal, so a
missing field is a compile error rather than a runtime warning. The thing both
homes must agree about is the **schema**, which is declared once, by the plugin,
and is introspectable from `:describe-option` — a better shared reference than a
format documented in a doc comment.

The loader already has the machinery: it distinguishes a *structural namespace*
(a sub-table it captures whole) from a scalar leaf, and joins a TOML array into
a delimited string for list-typed options. A composite option replaces that join
with a real conversion.

## 4. Where validation lands

One place: the host, against the declared schema, on every path in.

| Path | Today | After |
|---|---|---|
| `lattice.toml` | scalar coerce, then the plugin's parser on the blob | tree → schema check → `set_value` |
| `init.rs` | plugin's parser on the blob | tree → schema check → `set_value` |
| `:set` | `OptionType::parse` | unchanged for scalars; path-addressed for leaves |
| plugin read | plugin's parser, every read | `get_value` → typed |

A rejection names a **path** (`capture-templates[2].target.file: expected
string, got integer`), which no hand-rolled parser produced and which is the
concrete thing a user gets out of this before `:customize` exists.

The guest still deserializes. The parse changes *shape* — walk a tree instead of
parse text — rather than disappearing; the win is that the walk is total and
mechanical (an SDK derive) where the text parse was bespoke.

## 5. Paramount-goal alignment

**UX (higher court):** nothing user-visible changes until `:customize`; the
interim gain is that a malformed option is rejected with a path instead of a
plugin's guess. No latency surface is touched.

**#2 Extensibility** is what this protects, directly: a plugin's configuration
becomes introspectable data rather than an opaque string, which is what makes
`:customize`, `:describe-option` and TOML write-back possible for plugin options
at all. **#1 Performance** is untouched: schema declaration is load-time and
one-shot; value reads are cold-path (`get-option` for one-offs, `OptionChanged`
subscriptions for anything hot, per §6.6.1) and the value tree never crosses on
a keystroke path.

**Heuristic #1** drove §2.1 — the additive fourth kind is easier and worse.
**Heuristic #2**: the justification is the §5.12 promise and the ABI's own
asymmetry (147 records everywhere but config), not that emacs has `defcustom`
types — though it does, and the resemblance is not accidental.
**Heuristic #6**: no new crate. `lattice-config` owns the option domain and the
schema is part of it; the WIT types join the existing config interface.

## 6. Rejected alternatives

- **A dictionary of string keys.** Rejected by the user who would use it:
  nothing to validate against, nothing to render, and a second encoding beside
  the blob rather than instead of it.
- **A fourth option kind, additive.** §2.1 — smaller change, permanently two
  mechanisms.
- **One serialization in both homes, parsed host-side.** Keeps
  `lattice.toml`-and-`init.rs`-are-the-same-string, moves parsing and validation
  to the host, and is a fraction of the ABI churn. Rejected because the value
  stays text at rest, so `:customize` has to round-trip through a formatter that
  must preserve the user's comments and layout to be non-destructive — which is
  a harder problem than the one being solved, and one that native TOML does not
  have.
- **Decompose composites into multiple scalar options**, as §6.6.3 suggested
  ("the remaining 10% can decompose"). It does not survive contact with a *list*
  of records: `capture-templates` would need indexed option names invented at
  runtime, which is a dictionary with extra steps.

## 7. Out of scope

`:customize` itself. This fragment makes it *possible* — it is the reason to do
the work — but the buffer, its major mode, and TOML write-back are separate,
and design §5.12 / `mode-architecture.md` §6.7 own them. The deliverable here
ends at: every option describes its shape as data, and every path in validates
against that description.
