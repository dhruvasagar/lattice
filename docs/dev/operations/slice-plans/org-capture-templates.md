# Slice plan — org capture templates: one shape, three axes

Design: [`../../architecture/org-capture-templates.md`](../../architecture/org-capture-templates.md).

Supersedes the two-shape declaration from
[`archive/org-capture.md`](archive/org-capture.md) (OC.2) and
[`archive/org-roam.md`](archive/org-roam.md) (OR.14's roam-only `body-file`).
Independent of [`org-capture-drafts.md`](org-capture-drafts.md) — that plan
changes the capture buffer's substrate, this one changes the template shape;
either may land first.

**No host slice anywhere in this plan.** `host-services.read-file` and
`Effect::WriteToFile { anchor: Line(…) }` already carry everything. CT.1–CT.8
land in [`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin);
CT.9 lands in the user's config crate (`~/.config/lattice/init`). The design
fragment and its `dev-nav.toml` entry land in the lattice tree and are the only
thing in this repo.

| Slice | Tree | What | Status |
|---|---|---|---|
| CT.1 | org-plugin | `body-file` becomes a shared body-source seam | 📝 |
| CT.2 | org-plugin | Tilde-expand the target path before the read | 📝 |
| CT.3 | org-plugin | `target.kind` discriminant; `file+olp` | 📝 |
| CT.4 | org-plugin | `type` axis; `table-line` resolver + placement | 📝 |
| CT.5 | org-plugin | `entry` re-levels under a heading target | 📝 |
| CT.6 | org-plugin | `file+datetree` — creation, `tree-type`, `olp` above | 📝 |
| CT.7 | org-plugin | `sub-olp` below the date node; table-line scope | 📝 |
| CT.8 | org-plugin | Roam adopts the shape; `file+head`; `RoamTemplate` removed | 📝 |
| CT.9 | user config | `init.rs` declaration + the tracker file's conversion | 📝 |

**Ordering rationale.** CT.1 and CT.2 are independently useful and unblock
nothing else, so they go first and deliver the original request on their own.
CT.5 must precede CT.6 — a datetree target without re-levelling writes
structurally broken files (design §4.2), so landing them the other way round
ships a known-corrupting feature. CT.7 depends on both CT.4 and CT.6. CT.8 is
last because it is the only breaking migration and wants everything it migrates
*onto* to already work.

---

## CT.1 — `body-file` becomes a shared body-source seam

Design §6. The original request, and it stands alone.

**Touches.** `src/capture_templates.rs` (`RawTemplate` gains `body_file`);
`src/roam_templates.rs` (`resolve_body` moves out); a new
`src/template_body.rs` holding the shared resolver; `src/lib.rs` at the capture
draft site.

**Behaviour.** `body` and `body_file` mutually exclusive; both set is a skip
named in `skipped`; blank counts as absent for both. The read happens at **draft
time**, not config-read time. A non-roam template gets tilde and environment
expansion on the path but no `${…}` pass — there is no node to interpolate from.
Missing or unreadable file warns with the template key and the resolved path, and
the capture does not open.

**Extraction discipline.** `resolve_body` currently takes
`node: &roam_capture::Node<'_>`. The shared version takes an interpolation
closure so the roam pass stays roam's; do **not** push `Node` into the shared
module to avoid the parameter.

**Tests.** A capture template with `body_file` opens a draft with the file's
text; both fields set is skipped and named; a missing file warns and opens
nothing; a blank `body_file` falls through to `body`; roam's existing
`body_file` behaviour is unchanged (the existing roam tests must pass verbatim,
not be rewritten).

---

## CT.2 — tilde-expand the target path before the read

Design §6. A live trap, independent of everything else here.

**The bug.** `capture_effects` reads the target through
`host_services::read_file`, which does not expand `~`. A `~/…` `file+headline`
target therefore fails its read, **silently loses the headline, and appends at
end-of-file** — a write that lands in the wrong place with no error. The user
init documents it as a known constraint ("paths are ABSOLUTE rather than `~/…`").

**Fix.** Apply the expansion `roam_templates.rs:213-216` already performs before
its own read. The helper exists; it is simply not on this path.

**Tests.** A `~/…` `file+headline` target resolves its headline (the assertion
that pins today's wrong behaviour is inverted here, not deleted); an absolute
path is unchanged; a path with no `~` is unchanged. Assert the resolved
**insertion line**, not just the file written — a test that checks only the path
passes on the broken version.

**Then delete the warning** from the user init's doc comment in CT.9, rather than
leaving a stale constraint documented.

---

## CT.3 — `target.kind` discriminant; `file+olp`

Design §4.

**Behaviour.** `RawTarget` gains `kind: Option<String>` (schema `Enum`), `olp:
Option<Vec<String>>`. `kind` absent keeps today's inference — `headline` present
⇒ `file+headline`, else `file` — so **no existing config changes**. A `kind`
carrying a field it does not take is a named skip, not a silent ignore.

`file+olp` resolves a full outline path; it exists both for non-unique headings
and because CT.7's `sub-olp` needs the path-walking machinery anyway.

**Derive `Default` alongside `ConfigShape`** on `RawTarget` and `RawTemplate`
(design §9): the target record reaches six fields here and at most two are set
by any one `kind`, so without it every declaration carries four `None`s that
hide the two that matter. A default-constructed target must be today's `file`
inference, so the default is verified, not assumed.

**Tests.** Each `kind` resolves to its `Target` variant; `kind` absent infers
both legacy shapes; `file+olp` finds a heading whose name repeats under a
different parent; an unknown `kind` string is a named skip; `kind = "file"` plus
`headline` is a named skip. Round-trip the schema both directions.

---

## CT.4 — `type` axis; `table-line` resolver and placement

Design §3, §5.

**Behaviour.** `type` is an `Enum` defaulting to `entry`; `entry` is exactly
today's path. `table-line` adds a resolver alongside `resolve_in`, over text
`capture_effects` has already read:

1. scope — heading body, or whole file for a bare `file` target
2. first org table in scope
3. absent ⇒ create `|   |\n|---|\n`
4. placement — `table-line-pos` (emacs's `\(I+\)\([-+][0-9]+\)`, kept verbatim),
   then `prepend`, then end-of-table
5. row — trim, prefix `"| "` if it is not already a row; a multi-line body
   inserts multiple rows

**No alignment pass** (design §5).

**Tests.** Append to a table at end; `prepend` lands after the first hline;
`table-line-pos = "II-1"` lands in the right hline group; an absent table is
created and the row lands in it; a body that is already `| … |` is not
double-prefixed; a multi-line body becomes multiple rows; a bare `file` target
picks the first table in the file; `table-line-pos` naming a nonexistent hline
group warns and falls back to end-of-table. **Assert the inserted line number,
not only the text** — a wrong scope produces the right text in the wrong table.

**Bench.** One case: table location over a large org file (the tracker after a
year of daily nodes is the realistic shape). Capture runs on an explicit action
and already reads the file, so this is a guard against an accidentally quadratic
scan, not a latency budget — say so in the bench's comment.

---

## CT.5 — `entry` re-levels under a heading target

Design §4.2. **Must precede CT.6.**

**Behaviour.** For `type = entry` under a heading-bearing target, shift the
body's shallowest heading so it becomes a child of the target, and shift every
deeper heading by the same delta so relative structure survives. A body with no
headings is untouched. `Target::File` appends at top level and re-levels nothing.

**This changes existing behaviour** — today's verbatim insert is what forced the
user init's vocab template to be written with `**` instead of `*`. That template
becomes correct-as-written in CT.9; until then it over-shifts by one level. Land
the two within the same batch and say so in the commit.

**Tests.** A `*` body under a level-1 headline becomes `**`; a body with `*` and
`***` preserves the two-level gap; a body with no headings is byte-identical; a
`Target::File` append is byte-identical; a body whose shallowest heading is
already deeper than the target still shifts (up, not only down).

---

## CT.6 — `file+datetree`

Design §4.2. Depends on CT.5.

**Behaviour.** Find or create today's date node, per `org-datetree.el:138-158`:

```org
* 2026
** 2026-09 September
*** 2026-09-16 Wednesday
```

`tree-type` selects `day` (default), `week` (`%G-W%V`), or `month`. `olp`, when
present, puts the whole tree under that outline path — emacs's meaning. Creation
is ordered: a new date node is inserted in date order among its siblings, not
appended.

**Date source.** The existing `clock::Now` seam (`roam_dailies.rs:50` is the
precedent), so tests can pin a date without touching the host clock.

**Tests.** An empty file gets all three levels; an existing year reuses it and
adds the month; an existing day node is reused, not duplicated; a new date sorts
before a later existing sibling; `tree-type = "week"` builds `2026-W38`; `olp`
puts the tree under the named path; the weekday name matches the date.

---

## CT.7 — `sub-olp`, and `table-line`'s scope under it

Design §4.1. Depends on CT.4 and CT.6.

**Behaviour.** `sub-olp` descends from the resolved date node into a named
descendant; `table-line` then scopes its search to *that* node's body. A path not
found warns and **falls back to the date node's own body** — the note the user
just typed is what is at risk, so it is kept with a warning rather than refused.

**Tests.** A row lands in the table under `sub-olp = ["Urge / Habit Episode
Tracker"]` and **not** in the `Daily Overview` table above it (this is the
assertion the whole slice exists for — a scope bug puts the row in the first
table and every other test still passes); a two-element `sub-olp`; an absent path
warns and lands in the date node's body; `sub-olp` on a non-datetree target
resolves from the headline target instead.

---

## CT.8 — roam adopts the shape

Design §7, §10. The only breaking slice.

**Behaviour.** `RoamTemplate` is deleted. `org.roam-capture-templates` holds the
same `Template` type; roam's destination becomes
`target = { kind = "file+head", file = …, head = … }`, with `file` carrying the
`${…}` pass as it does today and `head` seeding the file when absent. The
`${title}` / `${slug}` / `${id}` pass stays roam-only and still runs before the
`%` pass.

**Also delete** the "the difference is org's rather than ours" comment at
`roam_templates.rs:5-17` and replace it with a pointer to design §1 — leaving it
would re-justify the split that was just removed.

**Tests.** Every existing roam capture test passes against the unified shape;
`file` absent still means org-roam's `YYYYMMDDHHMMSS-${slug}.org` default; a
roam template with `type = "table-line"` works (proving the axes are actually
independent, not just declared so); an old-shape config with a bare `file` field
is a named skip with a message that says what to write instead.

**Migration message.** The skip text must name the replacement literally
(`target = { kind = "file+head", file = "…" }`), not merely report the field as
unknown.

---

## CT.9 — `init.rs` and the tracker file

Tree: `~/.config/lattice/init`, plus
`~/src/dhruvasagar/org-files/`. Depends on CT.1–CT.8 released.

**Behaviour.**

- `Template` / `Target` / `EntryType` in the init crate track the plugin's shape;
  `RoamTemplate` is deleted and the eight PKOS templates move to
  `target = { kind = "file+head" }`.
- The two habit-tracker templates are added as design §9 declares them.
- The vocab template's `**` reverts to `*` now CT.5 re-levels (see CT.5's note).
- Two stale doc comments are deleted: the "paths are ABSOLUTE rather than `~/…`"
  warning (fixed in CT.2) and "the difference is org's rather than ours" (retired
  in CT.8).

**The template file converts from a roam node to a capture body.** As written,
`roam/templates/habit-episode-tracker.org` opens with `#+Title:`, `#+Filetags:`
and `#+subtitle:` — *file-level* keywords that identify a roam node. Filed as the
body of a date node they land mid-file, where org ignores them and
`#+FILETAGS:` is simply dead. So:

- the three keywords go; tags that are wanted become `:concept:` on the date
  headline
- `* How to use this` moves to the top of `habit-tracker.org` once, rather than
  being repeated in every day's entry
- the file moves out of `roam/templates/` — it is no longer a roam template

**Verification is manual and stated as such**: capture `h` on a fresh file and on
a file that already has today's node; capture `he` three times and confirm three
rows in the Episode Tracker table and none in Daily Overview; confirm the day's
node collapses to one line.

---

## This tree

One commit, no code: the design fragment plus its `site/data/dev-nav.toml` entry
in the `org` section (the sync **fails** if a `docs/dev/` page is unlisted —
`site/scripts/sync-docs.sh:425`), and a pointer row in
`docs/dev/operations/implementation.md`.

Slice plans under `slice-plans/` are not site-synced (`collect_dev_pages` globs
`docs/dev/<subdir>/*.md` non-recursively), so this file needs no nav entry.
