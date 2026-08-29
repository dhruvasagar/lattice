# Markup that exists to be parsed stops being shown — slice plan (H / OL)

> Design: [`../../architecture/conceal.md`](../../architecture/conceal.md) (the
> host primitive, phase H),
> [`../../architecture/org-mode.md`](../../architecture/org-mode.md) §7 (links —
> rendering and following, which phase OL implements),
> [`../../architecture/display-line.md`](../../architecture/display-line.md) (the
> `DisplayLine` substrate H.1 changes).
>
> Plugin repo: [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin).
> Its `wit/` is generated from `lattice-wit` (WT.2), so the `conceal-rule` record
> H.2 adds reaches it by regeneration rather than by hand-vendoring.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

**Status:** 📝 planned (2026-08-29).

---

## Why

Two phases, and the second is the reason the first is worth building now.

### Phase H — the editor cannot hide markup

`design.md:3414` named concealment as a v1 carve-out and recorded that nothing
implements it. Nothing has since. Markdown wants it (`**`, `#`, `[]()`), org
wants it (`[[target][description]]`), and both are stuck rendering the syntax
they exist to abstract.

Help buffers *appear* to disprove this. They render `[label](url)` as `label`
and follow it on `<CR>`. But they do it by **stripping the markup out of the
buffer text at build time** and keeping byte ranges as metadata
(`lattice-help/src/lib.rs:196`) — legitimate for a synthetic read-only buffer,
and unavailable for a file the user edits. There is no path from "help does it"
to "org can do it"; the mechanism does not generalise, and the first editable
buffer that wants rendered markup needs a display-time one.

### Phase OL — org links render raw and open on the wrong key

Org's links work: `links.rs` classifies `file:`, `http(s):`, `mailto:` and
`*Headline`, and `<leader>oo` opens them (OM.10). Two things are wrong with the
surface rather than the substance. The link *renders* as its full source text,
so a roam-style link eats 70 columns to say four words. And the key to follow
one is a three-keystroke leader chord, where every other editor — and lattice's
own help buffers — uses `<CR>`.

Phase OL also adds the `id:` arm to `Target`, which phase OR needs and which
nothing can resolve until OR ships an index. That split is deliberate: OL makes
`id:` a *recognised* link kind that reports honestly when it cannot be resolved,
rather than one that silently does nothing.

---

## Decisions locked before slicing

Recorded here because each was chosen against a real alternative, and the
reasoning belongs where the work is rather than only in the design fragment.

1. **Conceal bakes into `DisplayLine.text`, not into the renderers.** Parity
   between `lattice-ui-tui` and `lattice-ui-gpui` becomes structural — a
   renderer that does nothing new is already correct. Renderer-side elision
   would make parity a matter of discipline, which the standing cross-renderer
   rule exists because it decays.

2. **Rules come from patterns, not from tree-sitter captures.** Two independent
   reasons, either sufficient. `tree-sitter-org` has no `link` rule at all —
   `[[id:X][Title]]` is undifferentiated `expr` tokens, so there is nothing to
   capture. And a tree is absent during a reparse, so tree-driven conceal would
   flicker between concealed and raw while typing — a pixel change to unedited
   content, which is a standing veto. `links.rs` already recorded the same
   reasoning for its own text scanning.

3. **~~`col_map` goes signed rather than gaining a sibling table.~~
   Retracted at H.1 — `col_map` is unchanged.** The premise was that inlay
   insertion and conceal elision are one axis with opposite sign. Reading the
   code killed it: display columns in this space are already char-resolved, so a
   hidden range removes exactly `end - start` of them and conceal needs no width
   table. `conceals` had to exist anyway for the clamp, so the real choice was
   "two tables" versus "two tables *and* a signed rewrite of the first" — and
   the second would have widened `col_map` through `cells_worker`, `CellRow`,
   `cells_paint` and ten GPU call sites for a behaviour-preserving slice. See
   `conceal.md` §Data model, which records the correction rather than
   overwriting it.

3b. **The arithmetic lives in `lattice-cells`, not in each carrier.** Three
   carriers hold these tables and each had its own copy of the walk. One term in
   the sum made that survivable; two does not, and the symptom of a stale copy
   is a caret sitting off its own search match.

4. **Insert reveals buffer-wide.** Chosen over vim's `concealcursor` line-scoped
   reveal, with the cost accepted explicitly: `i` repaints every visible line
   carrying a concealed range. It is a *caused* change at a mode boundary that
   is already a visual event, `:set list` already does the same class of thing,
   and the mental model — Normal reads, Insert edits — is worth it.

5. **Motions do not learn about conceal.** `l` walks source bytes; the caret
   rests at a concealed range's start until it leaves. Vim's behaviour, and the
   alternative would make `dw` in a macro replay differently under a different
   conceal setting.

---

## Not in this plan

**Markdown's conceal rules.** The mechanism is general and markdown is the
second consumer named in `design.md`, but markdown's rule set is its own UX
question (which of `**`, `_`, `#`, `[]()` collapse, and what a horizontal rule
becomes) and it wants `cchar`-style replacement that H deliberately defers.
Landing org first proves the mechanism against a consumer whose rules are pure
elision.

**Replacement characters.** `conceal-rule` hides spans; it does not substitute
glyphs. Deferred in `conceal.md` with the reason: every rule in the first two
consumers is pure elision, and the WIT regenerates, so the field can arrive
later without hand-vendoring.

---

## Phase H slices

| Slice | Description | Status |
|---|---|---|
| H.1 | the shared coordinate fn, the `conceals` range list, the clamp | ✅ |
| H.2 | `conceal-rule` on the `language` seam; compile + validate at registration | ✅ |
| H.3 | the matrix build elides; the `conceal` axis; the bench | ✅ |
| H.4 | mode scoping — Insert reveals, gated on the language having rules | ✅ |

### H.1 — the substrate learns that a display line can be shorter ✅ (2026-08-29)

**Deps:** none.

`lattice_cells::coords::source_byte_to_display_col(byte, inlay_offsets,
conceals)` is the one place the translation lives; `DisplayLine` gains
`conceals: Arc<[ConcealRange]>` and its `byte_to_combined_col` delegates.

**The slice changed shape once the code was read, and the plan's own decision #3
is retracted above rather than quietly amended.** `col_map` stays unsigned:
display columns here are already char-resolved, so a hidden range removes
exactly `end - start` of them and conceal needs no width table. The signed
rewrite would have touched `cells_worker`, `CellRow`, `cells_paint` and ten GPU
call sites to change no behaviour.

**The clamp fell out of the arithmetic rather than needing a branch**, which is
the sign the representation is right. Subtracting only the hidden width lying
*strictly before* `byte` lands a byte inside a range on that range's start
column with no special case:

```
col = byte + Σ inlay_width(≤ byte) − Σ (min(end, byte) − start)
```

**Behaviour is unchanged by this slice** — nothing populates `conceals` yet — so
it lands and stays green on its own, and H.3 turns it on. `CellRow` is
deliberately untouched: **`CellRow::byte_to_combined_col` turns out to have no
production caller**, only its own tests, so widening the cell path would have
been churn on a dead method.

**Landed:** 10 tests in `coords.rs` plus 3 on `DisplayLine`. The two worth
naming because neither would fail for the obvious reason: `h1_no_conceals_is_the_pre_h1_behaviour`
re-asserts `CellRow`'s own inlay cases against the new function, so it is pinned
as a drop-in rather than a re-derivation; and `h1_with_source_line_shares_the_conceal_arc`
asserts `Arc::ptr_eq`, because a new field cloned instead of shared breaks
pixel-stability for unedited lines and **no behaviour test would catch it**.

Gates green on `lattice-cells` + `lattice-host` (1420 tests) and on
`lattice-ui-gpui` + `lattice-ui-tui` (1765) — the renderer crates because their
`DisplayLine` literals needed the new field, and gpui is not in a default build.

### H.2 — a language can declare what to hide ✅ (2026-08-29)

**Deps:** none (parallel with H.1).

`conceal-rule { pattern: string, hide: list<u32> }` joins the `language` seam's
spec; the compiled rules land on **`LangConfig`**, beside `highlights`.

**`LangConfig` rather than `LanguageRegistration`, and the choice pays off
immediately.** This is per-language *render* config, the same shelf the
highlight query sits on — which means a **native** language gains rules by
populating one field in `build_native_config`, with no new plumbing at all.
Markdown's `**`/`[]()` set becomes a rule list rather than a mechanism. It also
means teardown is already correct: `unregister_plugin` retains `LangConfig` by
provenance, so the rules leave with the language and there is no second
lifetime to get wrong. `h2_unloading_the_plugin_takes_its_conceal_rules` pins
that rather than assuming it.

**Validation is split across the two crates, deliberately.** The boundary
(`language_host.rs`) drops the two shapes that could not work under *any* engine
— blank pattern, empty `hide` — because that needs no regex and the guest is
still alive to be told. Everything needing the engine (does it compile, does
group N exist) happens at compile time in `lattice-syntax`, mirroring why the
queries are not compiled at the boundary either: that path runs with the guest's
`Store` held open.

**`regex`, not `fancy-regex`, and this is the slice's one new dependency edge.**
The workspace's existing engine is right where a *human* writes the pattern
(`/`, `:s`) and wants backrefs. A conceal rule is written by a *plugin* and
matched against every rebuilt display line, so the property that matters is that
backtracking is not reachable. `fancy-regex`'s recursion limit *bounds* a
pathological pattern rather than preventing it, and a bounded backtracker inside
a per-viewport loop still burns its whole budget on every line of every rebuild
before failing. `regex` is already in the tree transitively, so this is a direct
edge to something we ship regardless.

**A refused rule is dropped, never fatal** — asymmetric with query compilation,
which rejects the whole language, and the asymmetry is proportionality. A broken
`folds.scm` means the language cannot fold at all and silence is
indistinguishable from the feature not existing. A broken conceal rule means one
pattern does not hide. Losing org over a typo in a cosmetic regex costs more
than it protects.

**⚠️ Landing this rebuilds the org plugin, or it stops loading.** `language-spec`
gained a field, and Component Model records are structural — an existing
compiled guest declaring the old shape will fail to instantiate against the new
world. Only language-contributing plugins are affected (org is the only one).
This is the accepted pre-1.0 API-churn risk `design.md` §14 names, and the
`wit/` regeneration from `lattice-wit` is the mechanism, but it is a rebuild
rather than a no-op.

**Landed:** 18 tests. `conceal.rs` carries 11 (org's real two rules compiling;
each rejection dropping exactly one; the cap keeping `MAX_CONCEAL_RULES` and
refusing the overflow; a duplicated group normalised so its width cannot be
subtracted twice; and `a_regex_error_renders_on_one_line`, because `regex`
renders errors as a five-row diagram and a log line nobody reads is not a log
line). `plugin_lang.rs` carries 3 registration/teardown tests and
`language_host.rs` 2 boundary ones.

**Found on the way:** `crates/lattice-plugin-loader/tests/ex_command_surface.rs`
did not compile against `ExCommandContext`'s post-OC.10 shape, so that crate's
whole test binary had been failing to build and none of its five tests were
running. Proven pre-existing by stashing, fixed in its own commit.

### H.3 — the display line elides ✅ (2026-08-29)

**Deps:** H.1, H.2.

The matrix build matches the buffer language's compiled rules against each
display line, takes the **union** of the hidden capture-group spans, sorts and
coalesces them, and builds `text` with those spans absent — populating
`conceals` and the negative `col_map` deltas as it goes. `MatrixVersion` gains
its `conceal` axis, painting-class.

Coalescing before building is not tidiness: two rules hiding overlapping spans
must produce one hidden span, because a double-elision would subtract the
overlap's width twice and corrupt every column past it on that line.

**After this slice links are concealed in every mode, including Insert.** That
is a coherent editor — it is emacs' `org-descriptive-links` default — and it is
not the final UX. H.4 is what makes it mode-scoped, and the two are separate
because "conceal exists" and "conceal is mode-scoped" fail in different ways and
deserve to be bisectable apart.

**The claim that "both renderers need no change" was half wrong, and the half
that was wrong is the one that matters.** The *text* does come free — `text` +
`runs` are what both peers already consume. But GPUI computes its own
source-byte→column mapping for the caret and every overlay quad
(`byte_to_combined_col` in `editor_element.rs`), and that mapping has to
subtract concealed columns or the caret sits off its own match on that renderer
and not the other. So GPUI *did* change, in this patch, per the standing rule.

Two decisions came out of doing it. The conceal clamp lives in
`lattice_cells::subtract_conceals`, called by both the display substrate and the
GPU peer — the peer computes its inlay term differently (it resolves the byte to
a column itself and filters inlays by *byte*), and reconciling that is a
separate question with its own non-ASCII risk, but two copies of the *clamp* is
exactly the drift this design refuses. And the peer's per-row tables became one
`RowCoords { inlays, conceals }` value rather than two parallel vectors, because
a row that carried one without the other would place the caret confidently in
the wrong column, and pairing them is what stops a future row-producing path
from remembering one and forgetting the other.

**Conceal ranges are stored in COLUMN space, not byte space.**
`byte_to_combined_col`'s baseline is `col = byte` — callers hand it an
already-char-resolved position — so byte ranges would be right only for ASCII
and wrong by exactly the number of extra UTF-8 bytes earlier on the line.
`h3_conceal_ranges_are_in_column_space_not_byte_space` uses `café [[id:A][x]]`,
where byte 6 is column 5, and is the test that fails if this is got wrong.

**Bench (heuristic #5):** `lattice-syntax/benches/conceal.rs`, and it is a
per-line bench rather than the per-rebuild one the plan specified — reaching the
rules through `recompute` needs a registered wasm grammar, so that bench would
have measured Cranelift. Numbers in `benchmarks.md`: **3.25 ns** for the
no-rules path (the zero-cost claim, now a measurement rather than a sentence),
99 ns for org's rules over prose, 1.54 µs over a line with three links — ~5 µs
per 50-line viewport of real org, off-thread, per rebuild.

**Tests:** 8 in `cells_worker`, plus 14 more on the matcher in `conceal.rs`. The
two worth naming: the column-space one above, and
`h3_no_rules_leaves_the_row_byte_identical`, which is the regression guard for
every other buffer in the editor.

**A design claim was disproved here and is retracted rather than quietly
dropped** — see the OL.2 note below on rule ordering.

### H.4 — Normal reads, Insert edits ✅ (2026-08-29)

**Deps:** H.3.

Crossing the insert/non-insert boundary bumps the conceal axis. Insert and
Replace render raw; Normal, Visual, Select, Operator-pending, Command, Search
and Prompt render concealed.

**Revealing is expressed as having no rules**, not as a flag threaded beside
them. `conceal_rules_for(handle, reveal)` returns the empty set when revealing,
and both the `conceal` version axis and the built rows derive from that one
call — so there is no state in which the axis says "concealed" and the row says
otherwise. It also means revealing reuses H.3's byte-identical empty-rules path
rather than needing a second one.

**The gate is two conditions, and the second was not in the plan.**
`ModalState` is editor-**global**, so gating on the mode alone would repaint
every visible org buffer in every split the moment `i` was pressed in one of
them — a pixel change to content the user did not touch, which is the standing
veto. So a pane reveals iff `is_active_buffer && conceal_reveal()`. The
parameter was already there; the conjunction is the fix.

**Visual stays concealed**, deliberately: the split is "am I editing text",
not "am I in a modal state". Insert and Replace reveal; Normal, Visual, Select,
Operator-pending, Command, Search and Prompt do not.

**The zero-cost gate is asserted as an absence.**
`h4_a_language_with_no_rules_never_moves_the_axis` checks the version is
*unchanged* across the reveal boundary for a language with no rules — the only
way to catch a regression that costs a viewport rebuild per `i` in every Rust
file and changes not one pixel while doing it.

**Landed:** 6 tests — 4 in `cells_worker` (reveal ≡ no rules; the axis moving
across the boundary; the zero-cost absence; a revealed row byte-identical to
its source) and 2 in `dispatch` (every modal state's reveal answer; reveal
being an editor property that the pane-level conjunction narrows).

**Gate note:** `scripts/precommit.sh lattice-ui-gpui lattice-ui-tui` in ONE run
flakes the magit `settle_mode` tests under the extra parallel load — reproduced
on clean HEAD with changes stashed, failing set differing run to run. Gate the
two renderer crates **separately**.

---

## Phase OL slices

| Slice | Description | Status |
|---|---|---|
| OL.1 | `Target::Id` — a recognised kind that fails honestly | 📝 |
| OL.2 | org declares its two conceal rules | 📝 |
| OL.3 | `<CR>` follows, and declines when there is nothing to follow | 📝 |
| OL.4 | docs — design §7, the org help page, the ledger, the site | 📝 |

### OL.1 — `id:` becomes a link kind before anything can resolve it 📝

**Deps:** none. Lands independently of phase H.

`links.rs`'s `Target` gains `Id(String)`; `classify` grows an `id:` arm ahead of
the `has_scheme` check. `org-open-link` on an `Id` reports that there is no
index and stops.

**The slice exists because the alternative is a misleading error, not a missing
feature.** `classify` today has no `id:` arm, so `[[id:6F398E54-…]]` falls to
the final `Some(Target::File(p))` branch and opening it produces *"no such
file: id:6F398E54-…"*. That blames the filesystem for a missing index and sends
the user looking for a file that was never meant to exist. Recognising the kind
and failing on it is strictly better than resolving it wrongly, and it is worth
its own commit because it is the boundary between phases OL and OR: after this,
OR's index is the only thing missing.

**Tests:** `id:` classifying as `Id` and not as `File`; the message naming the
absent index rather than an absent file; `file:id-something.org` still
classifying as a file (the arm must key on the scheme, not on the substring);
an empty `id:` rejected like every other empty target.

### OL.2 — org declares what to hide 📝

**Deps:** H.2, H.3, H.4.

Two `conceal-rule`s on org's `language` contribution, and nothing else — the
mechanism, the coordinate maths and the mode scoping are all H's.

```
(\[\[[^]]+\]\[)[^]]+(\]\])     hide [1, 2]   described link
(\[\[)([^]]+)(\]\])            hide [1, 3]   bare link
```

**An earlier revision of this plan said the described rule "must be tried
first", and H.3 disproved it twice over.** The worry was that a described link
also matches the bare pattern, so matching it as bare would hide `[[` and `]]`
and leave `id:6F398E54-…][Project Kickoff Checklist` on screen. Neither half
holds: `conceal_spans` unions every rule's hidden spans, so no rule can consume
text before another sees it and the output is identical under any permutation;
and independently, the bare pattern's `[^]]+` stops at the first `]`, so it
never reaches a described link's closing `]]` and does not match it at all.

What must be asserted instead is what is actually true — and both are, because
the second is a property of how the patterns are *written* and a future edit
could destroy it: `declaration_order_cannot_change_what_is_hidden` and
`orgs_two_patterns_are_disjoint_by_construction`.

**Tests, and the corpus is the fixture.** A described link collapsing to its
description; a bare link keeping its target; a link inside a `#+BEGIN_SRC`
block — which conceals, because conceal is textual and does not know about
blocks, and that is the correct and documented behaviour rather than a bug to be
surprised by later; two links on one line; a malformed `[[` with no closing
`]]` left entirely alone.

### OL.3 — `<CR>` follows 📝

**Deps:** OL.1.

`<CR>` binds to `org-open-link` on the `org-mode` major layer, returning
`Effect::Declined` when the cursor is not inside a link's span. `<leader>oo`
stays — it is the explicit form, it works when the cursor is outside the span,
and removing a working chord to make room costs muscle memory for nothing.

**`Effect::Declined` is correct here and the general rule says otherwise, so the
slice records why.** The standing hazard is that `Declined` re-runs a multi-key
chord's *trailing key alone*, which is why a plugin-owned prefix must return
`Effect::None`. `<CR>` is a single key. There is no trailing key, so the hazard
cannot arise — and the test pins that by asserting the builtin motion runs
exactly once, not twice.

**Tests:** `<CR>` on a link opening it; `<CR>` on ordinary prose performing the
builtin first-non-blank-of-next-line motion **once**; `<CR>` on the last line
behaving as the builtin does there; `<CR>` in the agenda multibuffer still doing
what the agenda binds it to (org-mode's major is not active in that buffer, and
the test is what makes that a fact rather than an expectation); the round-trip
in a real editor rather than against a synthesised context — the failure mode
OC.10 and OT.4 both hit was a seam that answered nothing when reached for real.

### OL.4 — docs 📝

**Deps:** OL.1–OL.3.

`org-mode.md` §7 lands with the phase rather than ahead of it if any of the
above changes shape. The plugin's own `doc/org.md` gains the `<CR>` chord in its
key table and a line on rendering. `implementation.md` gains the OL rows.
`site/data/dev-nav.toml` needs no change for §7 (it is a section of a page
already listed), but the sync must run — a docs change is not finished until the
site carries it.
