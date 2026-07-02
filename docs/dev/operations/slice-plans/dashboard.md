# Slice plan — Dashboard launch buffer (DB)

**Design home:** [`../../architecture/dashboard.md`](../../architecture/dashboard.md).
That fragment owns *what* and *why*; this file owns *when* and *in what order*.
Authoritative per-slice status lives in
[`../implementation.md`](../implementation.md).

## Why

Lattice opens to an empty scratch buffer. There is no first-run surface telling
a new user what Lattice is or how to start — no branding, no pointer to
`:tutor` / `:help` / `:describe-*`. The dashboard is a read-only, section-
composed `*dashboard*` buffer shown when the editor launches with no file
argument, reachable any time via `:dashboard` / `:b *dashboard*`, styled with
brand-matching terminal art and a `dashboard.*` theme namespace, and extensible
by config now and plugins later.

The build order below front-loads the **pure, testable core** (registry +
fragment + config), then the **buffer + mode + command** surface, then the
**styling and branding** payoff, then **startup wiring** and the **override**
escape hatch — each slice landing green on its own.

## Slices

### DB.1 — crate + section registry + fragment model + config  ✅
New leaf crate `lattice-dashboard`. Define `DashboardSection` trait,
`DashboardRegistry` (ordered, id-keyed), `DashboardFragment` / `DashboardRow` /
`DashboardSpan` / `DashboardRole` / `Align` / `LinkTarget` (design §3). Register
the eight built-in sections (§3.1) returning fragments; content can be
placeholder prose refined in later slices. Add the three `dashboard.*` config
options (§8) via the `options!` macro. **No editor wiring yet** — this slice is
pure library + unit tests.
- *paramount:* #2 (the registry is the extensibility seam).
- *test:* registry orders by `order`; `dashboard.sections` selects + reorders;
  empty list ⇒ all built-ins default order; unknown id skipped + logged;
  each built-in renders a non-empty fragment.
- *doc:* design §3, §8 (landed with this plan).
- *error handling:* unknown section id in `dashboard.sections` ⇒ warn + skip,
  never a hard error.

### DB.2 — `*dashboard*` buffer + `dashboard-mode` + `:dashboard`  ✅
Realized as Option D (design §9.2). Add `BufferKind::Dashboard` +
`BufferData::Dashboard(DocumentEntry)` (parallel to `Messages`; every forced
`match BufferKind` arm grouped with the semantically-matching kind; regular-
buffer parity). Register `dashboard-mode` (major, `target_buffer_kind =
Dashboard`) contributing `ReadOnly`/`Wrap`/`NoFile`/`Number=false`/
`signcolumn=no` (help-mode's set) — self-contained, no help-mode dependency.
**Extend the three `BufferKind::Help` gates to include `Dashboard`** (grouped +
commented): `input.rs` `<CR>`→`FollowLink` / `<Esc>`→dismiss, `dispatch.rs`
`Action::FollowLink` arm, and the `do_help_follow_link` guard. `:dashboard` =
`Effect::OpenDashboard` + `Editor::do_open_dashboard` (lifecycle residue
mirroring `:messages`); the applier reads config, composes via the crate-owned
`DashboardRegistry` service, builds a `HelpContent` from the fragments (body
links via help-link markup: `cmd:`→`execute:`, `topic:`→`topic:`), creates a
`BufferData::Dashboard` buffer named `*dashboard*`, seeds content +
`HelpLinks`/`ExtraHighlights` + markdown syntax via `seed_help_metadata_locals`,
assigns `dashboard-mode` major, and activates. Branding block is a plain-text
placeholder here (styled in DB.4).
- *paramount:* #3 (a buffer reached by `:dashboard` / `:b *dashboard*` / `:ls`,
  read-only via `dashboard-mode` option contribution); mode-ownership (crate
  owns mode/options/registry/composition/config; host residue is the
  `Dashboard` kind arms + one lifecycle effect + applier, the sanctioned
  synthetic-buffer boundary).
- *test:* `:dashboard` opens the buffer; `:b *dashboard*` resolves; read-only
  invariants (Insert/operators inert, never dirty, `:q` no prompt, excluded from
  modified set, `listed:false` skipped by `:bn`/`:bp` but reached by `:b`/`:ls`);
  `<CR>` on a `cmd:`/`topic:` span fires the target; Esc dismisses; **help-buffer
  follow-link regression** (still works with `Dashboard` added alongside);
  regular-buffer parity (`multibuffer_is_a_regular_buffer.rs` shape).
- *doc:* design §6, §9, §9.2.
- *error handling:* `url:` link ⇒ `Unresolved` (no external opener in help-follow
  yet) ⇒ logged no-op, never panic.

### DB.3 — `dashboard.*` theme elements  ✅
Register the `dashboard.*` namespace (design §4) under
`ElementOwner::Mode("dashboard-mode")`, copying
`register_multibuffer_theme_elements`. Cache ids in a `Copy` struct; brand
colours as `ColorRef::Literal`, the rest palette-referencing. Body headings/
links keep the semantic `Style` bridge for now (custom-element body remap is
deferred, DB.8).
- *paramount:* #2 (themes restyle the page without the mode's cooperation).
- *test:* elements registered under the Mode owner; a theme override changes the
  resolved colour; default resolves to the brand colours.
- *doc:* design §4.

### DB.4 — branding block (terminal art + symmetry + centring)  🚧
`DashboardBrandingProvider` (a per-buffer `VirtualRowProvider` in
`lattice-dashboard`) emits the mark (`assets/lattice-mark.svg`) as a BMP-block
terminal-art grid — 7-col × 5-row (wide-and-short so it reads ~square given the
~2:1 cell aspect; two opposite corners cut for the interlocking look; amber
cursor bar with a gap row above *and* below) — plus the "Lattice" wordmark +
tagline to the right, vertically centred against the mark. Colours resolve from
the `dashboard.*` elements (DB.3) at `collect()` time (theme-overridable);
`Filler` kind ⇒ **transparent** backdrop (no bg block, gutter-aligned). Rows are
padded to one block width and tagged `VirtualRowAlign::Center` so the renderer
centres the block as a unit. Registered host-side in `do_open_dashboard`
(unregister-first, mirroring tutor). The brand mark is no longer a document
section — it is this virtual-row block above the body.

**Centring** promotes the line-level alignment attribute the design deferred
(§10): a new `VirtualRowAlign` field on `VirtualRow` (default `Left`), honored
by the renderer (it has the pane width; the provider does not). **TUI honors it
now**; **GPUI honors it as part of DB.4-gpui** (below) — a tracked, intentional
interim divergence, since GPUI's branding is being reworked there anyway.

- *paramount:* #1 (resolved cells through the existing fast path; no renderer
  kind-branch — centring is a generic `align` property); #2 (theme-overridable).
- *test:* provider emits the mark + wordmark with brand colours; rows padded to
  one block width + `Center`; branding reaches the pane virtual-row matrix
  (regression: registered-but-not-rendered).
- *deferred to DB.4-gpui:* GPUI `align` honoring + the **(ii) 2-D GPUI branding
  component** (mark block + independently-scaled wordmark matching the mark
  height + tight leading) — the custom-paint class the design scopes with the
  post-1.0 GPUI branding image (§5, §5.6.7). Uniform block-art stays the interim
  GPUI treatment.
- *doc:* design §5.
- *error handling (superseded — kept for reference):* pane too narrow ⇒ clamp
  padding to 0, never
  underflow.

### DB.5 — startup gating + mode-owned trigger  📝
Publish a generic `Startup { opened_file: Option<PathBuf> }` event at boot
(verify one doesn't already exist; add the minimal generic seam if not). The
mode's `install(&mut boot)` subscribes: on `Startup`, if
`opened_file.is_none() && dashboard.enabled`, create + compose + activate
`*dashboard*` through the generic `BufferStore` + activate-buffer signal (design
§9.1). Wire the same at both post-boot seams — TUI
`crates/lattice-ui-tui/src/app/boot.rs` and GPUI `App::new` — in one patch.
- *paramount:* #4 (event-driven, off the UI thread); mode-ownership (creation +
  activation-decision live in `lattice-dashboard`; acid test: zero `Editor::`
  additions, zero new host `Action`).
- *test:* no file + enabled ⇒ `*dashboard*` is the active buffer; file arg ⇒
  file active, dashboard reachable not auto-shown; `enabled=false` ⇒ not
  auto-shown but `:dashboard` still works.
- *doc:* design §9.1.
- *error handling:* compose failure at startup ⇒ log + fall back to the empty
  scratch buffer, never a blank/broken initial frame.

### DB.6 — full override + recompose triggers  📝
Implement `dashboard.source` (design §8): when set, the file content replaces
section composition; missing/unreadable ⇒ warn + fall back to sections. Wire the
recompose subscriptions (design §7): pane resize (re-centre), theme change
(re-resolve), nerd-font toggle (re-pick palette), `dashboard.sections` /
`dashboard.source` change. Recompose is O(sections), off-thread; idle frames do
zero dashboard work.
- *paramount:* #2 (user authors the whole page); #1 (recompose is event-driven,
  never per-frame).
- *test:* `dashboard.source` present ⇒ file replaces sections; missing file ⇒
  fallback + log, no panic, never empty; resize re-centres; theme change
  re-resolves colours; `dashboard.sections` change re-composes.
- *doc:* design §7, §8.
- *error handling:* override file read error ⇒ warn + section fallback.

### DB.7 — benches + ledger  📝
Add the two assertions (design §13): creation-time compose+seed under a recorded
threshold; idle-frame zero-recompose / zero-I/O. Record in `BENCHMARKS.md` with
the note that a keystroke bench does not apply. Update `implementation.md` with
the DB.* status.
- *paramount:* #1 (guards the off-thread + idle-zero contracts with recorded
  numbers).
- *test:* the bench harness itself; CI ratchet entry.
- *doc:* `BENCHMARKS.md`, `implementation.md`.

### DB.8 — plugin sections + body custom roles (deferred)  📝
Not v1. The registry already accepts non-native providers; this slice adds the
WASM host API for a plugin to **add / replace / whole-author** sections (design
§1, §10), and routes body headings/links through the custom `dashboard.section`
/ `dashboard.link` elements via the buffer-local theme-remap seam
(`theme-system.md` §5 scope 2) once that seam lands. Drawn now so the trait
boundary is right; built when the plugin host / remap seam are ready.
- *paramount:* #2.
- *doc:* design §6, §10.

## Sequencing

DB.1 (pure core) → DB.2 (buffer + mode + `:dashboard`) → DB.3 (theme elements) →
DB.4 (branding, depends on DB.3 colours) → DB.5 (startup, depends on DB.2
buffer) → DB.6 (override + recompose) → DB.7 (benches/ledger). DB.3 and DB.5 are
independent of each other and can interleave after DB.2. DB.8 is post-v1,
gated on the plugin host + theme-remap seam.

## Status

| Slice | State |
|---|---|
| DB.1 — crate + registry + fragment + config | ✅ |
| DB.2 — `*dashboard*` buffer + `dashboard-mode` + `:dashboard` | ✅ |
| DB.3 — `dashboard.*` theme elements | ✅ |
| DB.4 — branding block (art, colour, gutter-centring, help-mode, cursorline) | ✅ |
| DB.4-cleanup — revert redundant `VirtualRowAlign` (gutter supersedes it) | ✅ |
| DB.4-gpui — per-token virtual-row scaling (F.3) + scaled wordmark; side-by-side custom-paint lockup deferred (post-1.0 w/ image) | 🚧 |
| DB.5 — startup gating + mode-owned trigger | 📝 |
| DB.6 — full override + recompose triggers | 📝 |
| DB.7 — benches + ledger | 📝 |
| DB.8 — plugin sections + body custom roles (deferred) | 📝 |
