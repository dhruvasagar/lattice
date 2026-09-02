# Typed configuration — slice plan

> **Status: Active.** Opened 2026-09-02. Implements
> [`typed-configuration.md`](../../architecture/typed-configuration.md), which
> extends design §5.12 and supersedes
> [`mode-architecture.md`](../../architecture/mode-architecture.md) §6.6's WIT
> sketch.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Graduated out of [`org-agenda.md`](org-agenda.md) OA.14c, which recorded the
problem statement because org's options are what motivate it. Spans two repos:
slices marked **(plugin)** land in `~/src/dhruvasagar/lattice-org-plugin`.

Independent of [`config-and-init.md`](../../architecture/config-and-init.md)
§4.1's `pre-plugin-loaded` (OA.14d, landed). That fixed *when* a value arrives;
this fixes *what shape* it has. Neither substitutes for the other and they were
entangled in one bug report, which is why this note exists.

## Status

| Slice | Title | Status |
|---|---|---|
| **Phase 1 — the native surface** | | |
| TC.1 | `ConfigValue` + `ConfigSchema`; every option describes its shape | ✅ |
| TC.2 | `lattice.toml` writes a real tree, not a string containing one | ✅ |
| **Phase 2 — the ABI** | | |
| TC.3 | An option can be declared with a schema; values cross as a tree | ✅ |
| TC.4 | The SDK derive — a Rust struct becomes a schema and a value | ✅ |
| **Phase 3 — the encodings go** | | |
| TC.5 | `capture-templates` is a record list **(cross-repo)** | ✅ |
| TC.6 | `agenda-sections` + `agenda-custom-commands`; `toml` leaves the wasm **(plugin)** | 📝 |
| TC.7 | The three line formats become list schemas **(plugin)** | 📝 |
| **Phase 4 — the payoff that is in scope** | | |
| TC.8 | `:describe-option` renders the schema | 📝 |

`:customize` is NOT here. This plan ends at "every option describes its shape as
data, and every path in validates against it" — the buffer, its major mode and
TOML write-back are design §5.12 / `mode-architecture.md` §6.7's, and they
become possible because of this rather than being part of it.

Phase 3 is the only cross-repo phase and each of its slices is independently
shippable: an option that has not migrated keeps working as a string option,
because a string option is a degenerate schema rather than a separate mechanism
(design §2.1). That is what lets phases 1–2 land without a flag day.

---

## Phase 1 — the native surface

### TC.1 — `ConfigValue` + `ConfigSchema`; every option describes its shape ✅

`lattice-config` grows the two types and threads them through the surfaces that
already exist:

- `OptionType::schema()`, `to_value()`, `from_value()` — **defaulted**, so no
  existing option declaration changes. The default `schema()` reads
  `enumerate()` *for a type that declares its enumeration closed* (see the trap
  below); everything else gets `scalar(string)`. `bool` and `i64` override to
  their own scalar kinds.
- `ErasedOption::schema()`, `get_value()`, `set_value()` — the runtime-name
  surface `:set`, the loader and plugin introspection already go through.

Additive by construction: nothing is removed, `parse` / `format` keep their
round-trip contract, and a scalar option behaves identically before and after.

**Tests.** `lattice-config/tests/every_option_describes_its_shape.rs`, plus the
module's own unit tests. The round-trip that matters is
`from_value(to_value(v)) == v` across a spread of real option types, because
`set_value`/`get_value` become a second way to move a value and a type that
overrides one of the three defaults and not the others compiles, registers and
is silently lossy. Its companion is `assert_value_matches_schema`: a type can
round-trip perfectly with itself while describing that value as something else,
and then every consumer that trusts the schema is wrong about it. Plus:
`set_value` refuses a `Str("4")` for an integer option rather than coercing it
(coercion is what the string ABI did, and why failures surfaced far from their
cause), and it runs the option's own validator, so a range-checked option is
enforceable through `lattice.toml` exactly as through `:set`.

**The free win was a trap, and the test found it.** The first cut derived
`enum(...)` from `OptionType::enumerate`, which most enum-like types already
implement for `:set foo=<Tab>`. But `enumerate`'s documented job is completion,
and three types read it as a HINT over an open space: `ModelineZone` advertises
`auto` while accepting any comma list, `ExpandHeight` advertises `half`/`full`
while also accepting a bare number, `RootMarkers` advertises the defaults while
accepting others. Derived blindly, all three would have been described as closed
sets — `:customize` offering a one-item picker for an option that accepts
arbitrary lists, and `lattice.toml` rejecting every value the option actually
wants.

So `OptionType::enumerate_is_exhaustive()` names the ambiguity, defaults to
`false`, and the eight genuinely-closed types opt in with one line each. Opting
in cannot be wrong by omission; deriving could be, and was. The ambiguity
predates this slice — it was harmless while `enumerate` only fed a popup — and
`an_open_enumeration_is_not_mistaken_for_a_closed_one` pins each of the three
open types by name, because the failure mode is a type opting IN when it should
not have, which a test of the default cannot see.

### TC.2 — `lattice.toml` writes a real tree, not a string containing one ✅

The loader distinguishes a *structural namespace* (a sub-table captured whole)
from a scalar leaf, and joins a TOML array into a delimited string for
list-typed options. A composite-schema option replaces the join with a real
conversion: `toml::Value` → `ConfigValue` → schema check → `set_value`.

The interesting case is the one that used to warn. `[[org.capture-templates]]`
today produces "expected a sub-table; got a scalar" or an array-join into
nonsense; after this it is the option's value.

**The branch is on the OPTION, not on the TOML shape**, and that turned out to
be the whole trick. `[[org.capture-templates]]` and `[completion.per-language]`
are both tables; what makes one a value and the other a namespace is whether an
option by that exact name exists and says it has structure. Inspecting the TOML
shape instead would have had to guess.

**Tests.** Six, in `loader.rs`'s own module, against a `list<record>` fixture
modelled on org's `capture-templates` — nothing in the workspace has that shape
yet (phase 3's job) and waiting for it would have landed the loader change with
no test of the case it exists for.

- an array-of-tables lands, and the **multi-line body survives verbatim** — the
  field the blob handled *well*, so the one a tree could plausibly regress. The
  triple-quote form eating its own leading newline is TOML's rule, and the test
  says so rather than asserting around it;
- a table at an option's name is that option's value, and the assertion is that
  its fields do **not** appear as `unknown option` warnings — the failure that
  descending into them would produce;
- a shape mismatch reports `org.capture-templates[1].target.file`, index and
  field, and commits nothing: a half-applied list reads as a bug in the feature
  rather than a typo in the file;
- a misspelled field names the key and the alternatives;
- a float is refused by name rather than stringified (`ConfigValue` has no float
  kind, and rendering `1.5` as `"1.5"` would make the value depend on the host's
  float formatter);
- and the no-regression case: a structural namespace is still captured whole, a
  scalar option fed a table still walks in and warns exactly as before.

Validation runs in the loader *and* inside `set_value`. Deliberate: only in the
loader is the error still structured, so it can splice the schema path onto the
option's dotted name. One redundant walk of a cold-path config value is a fair
price for the message reading `org.capture-templates[1].target.file: ...`
instead of `org.capture-templates: [1].target.file: ...`.

## Phase 2 — the ABI

### TC.3 — An option can be declared with a schema; values cross as a tree ✅

WIT gains `config-schema` / `config-value` and three calls:
`register-structured-option`, `get-option-value`, `set-option-value`. The string
pair stays as the scalar front-end — `:set` is a text surface by design
(typed-configuration.md §2.2) — and `register-option` stays as the shorthand for
declaring a scalar, which is ergonomics rather than a second mechanism (§2.1).

Every seam store can already read the registry as of OA.14d, so there was no new
wiring here — worth saying, because that gap is what made this class of bug
invisible last time.

**WIT has no recursive types**, which the compiler said and the design fragment
had not: `type config-schema depends on itself`. Both trees cross as an ARENA —
a flat node list plus a root index, children by index — and
`boundary_config.rs` is where an arena becomes a tree. The fragment's §2.0 now
carries this; it is not a detail, it is the wire format.

That boundary has to defend against two things nothing else in the crate does:
an index pointing outside the list, and an index pointing back up the tree. The
second is the dangerous one — following it naively is unbounded recursion on the
HOST's stack from a value a plugin chose — so the walk carries the nodes on its
current path and refuses one it is already inside. It tracks the *path*, not
every node seen: a guest that emits one `string` node and points three fields at
it has sent a DAG, which is a fine encoding of a tree, and rejecting it would
punish exactly the encoding a careful generator produces.

**The schema lives on the OPTION, not in the value** (§2.0.1). A value carrying
its own shape is the obvious arrangement and it does not work:
`OptionType::from_value` is static, so the shape would be lost on the first
write. `Option<T>` grows a `schema` field beside its doc and its default, and
`ConfigValue` becomes an `OptionType` whose `parse`/`format` are TOML text —
which keeps `:set` working for structured options and means migrating an option
that WAS a TOML-in-a-string breaks nobody who was setting it that way.

**Tests.** `boundary_config.rs`'s own — round-trips of a two-level nested
schema and value, every scalar kind crossing as itself, an out-of-range index
(including the root, the one nobody checks because it does not arrive through a
child link), a cycle refused rather than followed, and a shared node accepted.
Plus `config_source.rs` end to end through the real component: the fixture
declares org's `capture-templates` shape by hand as an arena, sets a value
through `set-option-value` and reads it back through `get-option-value`. The
assertion is on what came BACK, flattened with its links resolved — a seam that
accepted the tree and stored a mangled one passes any assertion made on the
write alone. An ill-shaped tree is refused AND leaves the previous value intact,
both halves, because returning `false` while clearing the option would satisfy
the first.

### TC.4 — The SDK derive — a Rust struct becomes a schema and a value ✅

`lattice-plugin-sdk` grows the derive that turns an ordinary Rust struct into a
`schema()` and a `to_value()` / `from_value()`. Without it every guest
hand-writes trees, which is worse than the text parse it replaces — the design's
claim is that the guest's parse becomes *total and mechanical*, and this slice
is what makes that true rather than aspirational.

`lattice_plugin_sdk::shape` carries the trait, the tree types, impls for
`bool` / `i64` / `String` / `Vec<T>` / `Option<T>`, and the arena flattening.
WIT-agnostic like the rest of the SDK — a proc-macro crate cannot name a
per-world WIT type — so a plugin writes one function mapping the SDK's node
types to its generated ones, once, the same tax `#[derive(PluginOption)]`
already charges for `OptionKind`.

**Optionality comes from the type.** An `Option<T>` field is optional and
everything else is required; there is no `#[shape(optional)]`, because the type
already says it and a second place to say it is a second place to disagree. An
absent optional field is OMITTED from the tree rather than emitted as a
placeholder — "absent" and "present but empty" are different values, and the
host's `required` check reads absence.

**Field names cross kebab-cased** (`max_depth` → `max-depth`), so an option's
fields read like every other key in a TOML file rather than like Rust
identifiers. This widened `to_kebab_case`, which previously only saw the
`CamelCase` type and variant names its two older callers pass; an underscore is
not kebab-case by any reading, so one answer to "what is this called on the
wire" is better than a second helper.

An all-unit enum becomes `enum-of` over its kebab-cased variants — which is what
lets `:customize` offer a picker. A data-carrying variant is **rejected** rather
than flattened: a tagged union has no `config-schema` arm, and inventing one
would describe a shape the host then validates against wrongly.

**Tests.** Ten, against org's actual shapes rather than a toy struct — a list of
records with a nested record, an optional field, and an enum field. A unit test
of the macro proves it expands; these test the properties a hand-written parser
kept getting wrong. The round-trip (`from_value` inverts `to_value`) is the
contract that makes the derive trustworthy at all. Then: an absent optional
survives as `None` rather than `Some("")`; a missing required field reports
`target.file`, assembled from two segments discovered at different depths; a
wrong-typed leaf inside a list reports `[1].target.file`, which is the message a
user of org's `capture-templates` will actually get; a non-record is refused
before any field is read, so the message blames the value rather than a field;
and the whole loop — struct → tree → arena → tree → struct — because the derive
produces a tree while the wire carries an arena, and a flattening that lost the
nesting would leave every other assertion passing.

## Phase 3 — the encodings go **(plugin)**

Ordered easiest-shape-first, so the SDK derive is exercised on a clean record
list before it meets the two awkward ones.

### TC.5 — `capture-templates` is a record list ✅ **(cross-repo)**

The cleanest shape: `list<record { key, description?, target: record { file,
headline? }, body?, clock-in? }>`. Its hand-rolled TOML parser is gone.

**Cross-repo, and it turned out to be three repos' worth of change** — org, the
lattice-side text surface, and the user's own `init.rs`. That last one is the
part a plan that said "(plugin)" had not accounted for: an option that changes
shape changes what every config home must write, and `init.rs` was setting this
one as a TOML string containing `[[template]]`. It now builds a `Vec<Template>`
through `#[derive(ConfigShape)]`, which is the design's "each home writes the
tree natively" arriving in the only place it can be observed.

**A one-per-plugin adapter, not a per-option one.** `config_shape.rs` in org
(and a write-only twin in `init.rs`) maps the SDK's WIT-agnostic node types to
the generated ones and hides the arena entirely. It is the same one-off tax
`#[derive(PluginOption)]` already charges for `OptionKind`.

**`ConfigValue`'s `:set` text form needed a wrapper.** A TOML document cannot
BE an array, so a list-rooted option has no bare text spelling; `format` wraps a
non-record root under the reserved key `value` and `parse` unwraps it, because
`parse(&v.format()) == Ok(v)` is `OptionType`'s contract and a structured option
does not get an exemption. The one ambiguity — a record option whose only field
is called `value` — is documented, is not silent when it happens (the unwrapped
tree fails validation with a path), and does not touch `lattice.toml` or
`set-option-value`, both of which carry the tree natively.

**One behaviour deliberately changed, and it is the interesting part of the
slice.** A template missing its `key` or its `target` used to be SKIPPED and
named, on org's stated principle that one typo should not cost the feature. It
is now a hard rejection of the whole option, because the host validates
structure before org sees it. That principle was compelling when the
alternative was a parser error with no location; it is not compelling against
`capture-templates[2].key: required field is missing`. A silently absent menu
row is the failure that sends a user looking in the wrong place, and now that
the message says which template and which field, refusing is the kinder answer.

What stayed a skip: a **blank** key or `target.file` (present, so `required` is
satisfied, but meaningless — the schema cannot say "non-empty"), and a
**duplicate** key. Both are cases where the value is usable and the only
question is which row wins.

**Tests.** The org-side suite rewritten against the declared shape rather than
against TOML text. The multi-line `body` test survives the mechanism it was
written for — it existed because a `'''` block preserving newlines was the
reason the option COULD be a string, and it is now the field a tree could
plausibly regress. Added: the declared shape itself is asserted (field names,
kebab-casing, which fields are required), because the schema IS the
documentation now — `:describe-option` renders it and `lattice.toml` is
validated against it, so a renamed field is a user-visible change; and a
round-trip through `to_value`/`from_value`, because a derive where those
disagree changes a template on its way through the option and nothing else
would notice.

### TC.6 — `agenda-sections` + `agenda-custom-commands`; `toml` leaves the wasm 📝

The two nested shapes, and the slice that pays the visible dividend: with all
three TOML-in-a-string options migrated, org drops the `toml` crate from its
component.

**Test:** a custom command whose sections nest — the case that motivated the
array-of-tables in the first place. Plus a size assertion is *not* worth
writing; the dependency's absence from `Cargo.toml` is the durable check.

### TC.7 — The three line formats become list schemas 📝

`todo-keywords`, `todo-keyword-styles`, `agenda-files`. Lower value than TC.5–6
(a line format is at least readable) but they are the remaining bespoke parsers,
and `todo-keywords` in particular is read at load — so its errors currently
surface as *missing colour*, which is the least legible failure org has.

**Test:** the OA.14d load-time path still works through the new shape — a
keyword set supplied from `init.rs` produces theme elements for every keyword.
That test already exists; this slice must not need it rewritten, only re-pointed.

## Phase 4

### TC.8 — `:describe-option` renders the schema 📝

Today it prints a wall of TOML for a composite. After: the declared shape, its
fields and their docs, and the current value rendered against it. The smallest
slice that turns the schema from an internal invariant into something a user can
see — and the reason it is in scope while `:customize` is not.

---

## Cross-renderer note

None. Configuration has no renderer surface until `:customize`, which is out of
scope; TC.8 renders into a help buffer, which both renderers already paint
through the ordinary Document path.
