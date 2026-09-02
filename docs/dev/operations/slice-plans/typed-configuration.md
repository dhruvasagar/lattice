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
| TC.3 | `register-option` takes a schema; values cross as a tree | 📝 |
| TC.4 | The SDK derive — a Rust struct becomes a schema and a value | 📝 |
| **Phase 3 — the encodings go** | | |
| TC.5 | `capture-templates` is a record list **(plugin)** | 📝 |
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

### Phase 2 — the ABI

### TC.3 — `register-option` takes a schema; values cross as a tree 📝

WIT gains `option-schema` / `option-value`; `register-option` is re-based onto
the schema-taking form and `get-option-value` / `set-option-value` join the
string pair, which stays as the scalar front-end (`:set` is a text surface by
design — typed-configuration.md §2.2).

`register_plugin_option` re-bases with it. Every seam store can already read the
registry as of OA.14d, so there is no new wiring here — which is worth saying,
because that gap is what made the same class of bug invisible last time.

**Tests.** A guest declaring a record-schema option and reading it back typed,
through the real component. A tree that violates the schema is rejected with a
path and registers nothing — asserted at the boundary, since a partial
registration is worse than a refused one.

### TC.4 — The SDK derive — a Rust struct becomes a schema and a value 📝

`lattice-plugin-sdk` grows the derive that turns an ordinary Rust struct into a
`schema()` and a `to_value()` / `from_value()`. Without it every guest
hand-writes trees, which is worse than the text parse it replaces — the design's
claim is that the guest's parse becomes *total and mechanical*, and this slice
is what makes that true rather than aspirational.

**Tests.** A nested struct with a list of records round-trips. A missing
required field and a wrong-typed field each fail with the field's path.

## Phase 3 — the encodings go **(plugin)**

Ordered easiest-shape-first, so the SDK derive is exercised on a clean record
list before it meets the two awkward ones.

### TC.5 — `capture-templates` is a record list 📝

The cleanest shape: a list of `{ key, description, target, body }` where
`target` is a two-variant record. Its hand-rolled parser goes.

**Test:** the multi-line `body` survives the crossing. It is the field the blob
handled *well* (a `'''` literal block preserves newlines and nested `"""`), so
it is the one a tree could plausibly regress.

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
