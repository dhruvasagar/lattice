# Markup that exists to be parsed stops being shown — slice plan (H / OL)

> Design: [`../../architecture/conceal.md`](../../architecture/conceal.md) (the
> host primitive, phase H),
> [`../../architecture/org-mode.md`](../../architecture/org-mode.md) §OM.10 (link
> opening, which phase OL extends),
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

3. **`col_map` goes signed rather than gaining a sibling table.** Inlays insert
   columns, conceal removes them; they are one axis with opposite sign. Two
   tables could disagree about the same byte, and the symptom of that
   disagreement is a caret half a glyph off its highlight.

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
| H.1 | signed `col_map`, the `conceals` range list, the clamp | 📝 |
| H.2 | `conceal-rule` on the `language` seam; compile + validate at registration | 📝 |
| H.3 | the matrix build elides; the `conceal` axis; the bench | 📝 |
| H.4 | mode scoping — Insert reveals, gated on the language having rules | 📝 |

### H.1 — the substrate learns that a display line can be shorter 📝

**Deps:** none.

`DisplayLine.col_map` becomes `Arc<[(u32, i32)]>` and `byte_to_combined_col`
walks it with `saturating_add_signed`. A new `conceals: Arc<[(u32, u32)]>` of
sorted, coalesced source-byte ranges rides beside it, and the same function
clamps a byte falling *inside* a range to the column of that range's start.

**Behaviour is unchanged by this slice.** Nothing populates `conceals` yet and
every existing `col_map` entry is a positive inlay/tab delta, so the signed walk
computes what the unsigned one did. That is the point: the substrate change
lands and stays green on its own, and H.3 turns it on.

**Why the clamp is a separate list rather than falling out of the walk.** A byte
inside a concealed range has no column of its own. A pure cumulative walk hands
back a partial column for it — a position between the range's start and where
its end would have been — and that is worse than either endpoint because it is
*plausible*: a caret one column into a hidden span looks like a rounding bug
rather than a missing rule.

**Tests:** the clamp for a byte before / at the start of / inside / at the end
of / after a concealed range; a line with an inlay *and* a conceal, asserting the
two deltas compose in byte order rather than by category; `with_source_line`
sharing the new `Arc`; and the existing inlay + tab-expansion coordinate tests
re-run unchanged, which is the evidence the sign change is behaviour-preserving.

### H.2 — a language can declare what to hide 📝

**Deps:** none (parallel with H.1).

`conceal-rule { pattern: string, hide: list<u32> }` joins the `language` seam's
spec in `language.wit`; the host compiles each pattern once when the language
registers and holds the compiled set beside the language's other contributions.

**Validation happens at registration, never per line.** A pattern that does not
compile, a `hide` index naming a group the pattern lacks, and `hide: [0]` are
each logged at `warn` once with the offending rule named, and that rule alone is
dropped — the language's other rules still apply, and a plugin does not lose a
language over one bad regex. Per-line validation would log at rebuild rate,
which is the `debug!`-not-`info!` mistake in a different costume.

Rules are capped per language and each pattern is length-bounded. Not because
the engine backtracks — it is linear-time — but because an unbounded rule list
turns every rebuild into a scan of someone else's configuration.

**Tests:** a valid rule set compiling; each of the three rejection cases dropping
exactly one rule and leaving the rest live; the cap refusing the overflow rule
and saying so; teardown removing a plugin's rules with its language.

### H.3 — the display line elides 📝

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

**Both renderers need no change, and the plan says so rather than leaving an
empty grep to be read as an oversight.** `text` + `runs` are what TUI and GPUI
already consume; conceal changes their contents, not their shape. The standing
cross-renderer audit (`grep -rn "conceal" crates/lattice-ui-gpui/`) is expected
to be empty *for that reason*, and H.3's test asserts both peers emit the
concealed string from the same matrix — aligned by evidence, not by silence.

**Bench (required, heuristic #5):** `benches/conceal_rebuild.rs` — matrix rebuild
over a viewport of link-dense org with rules active against the same viewport
with rules disabled. The number lands in `benchmarks.md` so the axis's cost is
visible rather than asserted.

**Tests:** a described link collapsing to its description; a bare link keeping
its target; two links on one line; overlapping rules coalescing to one span; a
line with no match untouched byte-for-byte (the `Arc` reuse that keeps unedited
lines pixel-stable); a buffer whose language declares no rules taking the
zero-cost path; cursor, search highlight and selection all landing on the same
column for a byte inside a concealed range.

### H.4 — Normal reads, Insert edits 📝

**Deps:** H.3.

Crossing the insert/non-insert boundary bumps the conceal axis. Insert and
Replace render raw; Normal, Visual, Select, Operator-pending, Command, Search
and Prompt render concealed.

**The gate is the slice's real content.** Mode changes happen constantly and in
every buffer; an axis that bumped globally would put a viewport rebuild on every
`i` in every Rust file in the editor. The bump is conditional on the buffer's
language having compiled rules, so a buffer with none never enters the path —
and the test for that asserts the version is *unchanged* across `i` in a Rust
buffer, which is the only way to catch a regression that costs performance
without changing a pixel.

**Tests:** `i` revealing and `<Esc>` re-concealing; `R` revealing; `v` and `:`
not revealing; a Rust buffer's matrix version unchanged across a full
Normal→Insert→Normal cycle; the one-frame drop-to-raw on a version mismatch
being raw rather than corrupt.

---

## Phase OL slices

📝 To be written when phase H lands. Shape: `Target::Id` on `links.rs`, org's two
conceal rules declared through H.2's seam, and `<CR>` bound to `org-open-link`
with `Effect::Declined` fall-through.
