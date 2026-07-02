# Dashboard

Authoritative design for Lattice's launch **dashboard** — the branded start
page shown when the editor opens with no file argument. The dashboard is a
read-only, section-composed buffer named `*dashboard*`, owned by a
`dashboard-mode` major mode, extensible by config and (later) plugins.

This document is the *companion* to `design.md` (§5.9 everything-is-a-buffer,
§5.6 rendering, §5.11 introspection), and builds on `theme-system.md`
(mode-registered theme elements), `virtual-rows.md` + `headerline.md` (the
sticky/anchored row primitive used for the centred branding block), and the
brand assets in `assets/` (`lattice-mark.svg`, `lattice-lockup.svg`).

Slice plan (the *when* / *in what order*):
[`../operations/slice-plans/dashboard.md`](../operations/slice-plans/dashboard.md).
This fragment owns *what* and *why*.

## 1. Design goals

A first-run surface that answers "what is this, and how do I start?" without a
manual, while staying a first-class buffer (not a bespoke overlay). Concretely:

- **Branding.** The Lattice mark + wordmark + tagline, rendered as terminal art
  in the TUI (no image dependency) and matching the brand geometry/colour.
- **Orientation.** What Lattice is, its four paramount goals, links to the
  repo/docs, getting-started steps, and pointers to the self-documenting help
  surface — `:tutor`, `:help`, key-binding discovery, the `:describe-*` family,
  `:help` topics.
- **Composable.** The page is a **composition of sections**, each individually
  toggled/reordered, the whole page user-overridable, and — in a later phase —
  extensible by plugins that add, replace, or wholly author sections.
- **A buffer, not a mode-of-the-editor.** Reached by `:dashboard`, by
  `:b *dashboard*`, listed conceptually alongside `*messages*`. Read-only,
  never dirty, never saved.

Two saved invariants frame the design:

> Everything is a buffer. — `CLAUDE.md`, §5.9

> Modes own their full surface. — `feedback_mode_owns_its_surface`

The first rules out a bespoke splash overlay with its own render path. The
second rules out `Editor::do_dashboard` / `App::ensure_dashboard_buffer` on the
host: the mode owns its keymap, its `:dashboard` handler body, its content
production, its theme elements, and its startup trigger.

## 2. Shape

```
                      lattice-dashboard  (new leaf crate)
   ┌──────────────────────────────────────────────────────────────┐
   │  DashboardSection (trait)        DashboardRegistry             │
   │      id / order / enabled            ordered, id-keyed         │
   │      render(&ctx) -> Fragment        built-ins + (future)      │
   │            │                          plugin providers          │
   │            ▼                                                    │
   │  DashboardFragment  ── rows: {text, role, align, link?}        │
   │            │                                                    │
   │   compose(registry, cfg) ──► seed *dashboard* buffer           │
   │            │                                                    │
   │   ┌────────┴─────────┐                                         │
   │   ▼                  ▼                                         │
   │ branding block     body sections                               │
   │ (virtual rows,     (document content,                          │
   │  resolved cells,    ExtraHighlights links,                     │
   │  centred padding)   cursor-navigable)                          │
   └──────────────────────────────────────────────────────────────┘
            │                                    │
            ▼                                    ▼
   dashboard-mode (major)              :dashboard  +  startup subscription
     ReadOnly / NoFile / gutterless      (event-bus, mode-owned)
     <CR> link-follow / Esc dismiss
     registers dashboard.* theme elements
```

The **registry is the single extensibility seam**: config selects/orders
built-in sections today; the same registry accepts plugin-contributed sections
tomorrow. The **fragment** is the single content contract: a section never
touches cells, theme ids, or the renderer — it emits styled, aligned, optionally
linked text, and the compositor turns that into a branding virtual-row block
plus document body.

## 3. Data model

### 3.1 `DashboardSection`

```rust
pub trait DashboardSection: Send + Sync {
	fn id(&self) -> &str;                 // "branding", "about", "links", …
	fn order(&self) -> i32;               // default sort key; ties break by id
	fn default_enabled(&self) -> bool;
	fn render(&self, ctx: &DashboardCtx) -> DashboardFragment;
}
```

`DashboardCtx` carries the read-only facts a section needs to render: pane
width (for centring), the resolved theme snapshot, the nerd-font toggle, the
build version string, and handles to look up help topics / key bindings so the
"help & bindings" section stays truthful. It is a plain value passed in — a
section performs no I/O and holds no editor state.

Built-in sections (native Rust — the built-in surface stays native, like the
vim grammar):

| id | content |
|---|---|
| `branding` | mark art + "Lattice" wordmark + tagline (see §5) |
| `about` | one-paragraph identity + the four paramount goals |
| `links` | GitHub repo, docs, issue tracker (`url:` links) |
| `getting-started` | open a file, `:` command line, modal basics |
| `tutor` | `:tutor` — interactive lessons (`cmd:tutor` link) |
| `help-and-bindings` | `:help`, `<leader>`-discovery, `:describe-key` |
| `describe` | the `:describe-*` family, one line each |
| `help-topics` | `:help <topic>` index entry points |

### 3.2 `DashboardFragment`

```rust
pub struct DashboardFragment {
	pub rows: Vec<DashboardRow>,
}

pub struct DashboardRow {
	pub spans: Vec<DashboardSpan>,   // one visual line, left→right
	pub align: Align,                // Left | Center  (Right reserved)
}

pub struct DashboardSpan {
	pub text: String,
	pub role: DashboardRole,
	pub link: Option<LinkTarget>,    // scheme:value, reusing help-link syntax
}

pub enum DashboardRole {
	Logo, Cursor, Title, Tagline,
	SectionHeading, Body, Key, Hint, Link,
}
```

`role` is *semantic*, never a colour. Each role resolves to a `dashboard.*`
theme element (§4). `link` reuses the existing help-link `scheme:value` model
(`cmd:`, `topic:`, `url:`) so `<CR>` follow-through is the help mechanism, not
new code.

### 3.3 `LinkTarget`

The same three schemes the help buffer already follows:

- `cmd:<ex-command>` — run an ex-command (`cmd:tutor`, `cmd:help`).
- `topic:<help-topic>` — open a `:help` topic buffer.
- `url:<https://…>` — open externally (`shell-execute`-style; logged + skipped
  if no opener is configured, never a panic).

## 4. Theme elements (custom, theme-overridable)

The mode registers a `dashboard.*` element namespace at boot, exactly as the
multibuffer mode registers `multibuffer.excerpt_header.*`
(`register_multibuffer_theme_elements`, `crates/lattice-multibuffer/src/lib.rs`).
`ThemeRegistry::register(name, ElementOwner::Mode("dashboard-mode"), default,
doc) -> ElementId`; the mode caches the returned ids in a `Copy` struct and
reads `resolved.get(id)` at compose time.

| element | default | notes |
|---|---|---|
| `dashboard.logo` | literal `#1f6feb` (brand blue) | mark art |
| `dashboard.cursor` | literal `#f59e0b` (brand amber) | cursor bar in mark |
| `dashboard.title` | literal `#1f6feb` | "Lattice" wordmark |
| `dashboard.tagline` | palette `muted` | tagline / `Hint` |
| `dashboard.section` | palette `heading` | `SectionHeading` |
| `dashboard.key` | palette `accent` | `Key` (keycaps) |
| `dashboard.link` | palette `link` | `Link` |
| `dashboard.body` | `Default` (inherits fg) | `Body` |

Defaults use `ColorRef::Literal` for the three brand colours so the mark reads
correctly out of the box under any theme, and palette references for the rest so
they reskin naturally. Every element is theme-overridable via the normal scopes
(`theme-system.md` §5); `dashboard.*` sits under `ElementOwner::Mode`, so a
theme author can restyle the whole page without the mode's cooperation. **Zero
new fields anywhere in the host** — registration is the entire mechanism.

## 5. Branding block

The visual centrepiece. Recreated as **terminal art** so it works in every
terminal with no image protocol (inline media / sprite rendering is post-1.0,
`design.md` §5.6.7). Rendered as a **virtual-row block** anchored above line 0
(`virtual-rows.md`): the mode resolves `dashboard.logo` / `dashboard.cursor` /
`dashboard.title` / `dashboard.tagline` directly into cell colours (the
headerline/multibuffer pattern), giving full custom colour and per-row `bg`
without touching the `Style` enum or the cells pipeline. Body sections below it
are ordinary document lines (§6) so links stay cursor-navigable.

### 5.1 The mark

The brand mark (`assets/lattice-mark.svg`) is an interlocking blue bracket — a
stylised "L"/"7" — with an amber vertical cursor bar centred inside:

```
path 1 (foot):  M 0 20 L 20 20 L 20 100 L 80 100 L 80 120 L 0 120 Z   #1f6feb
path 2 (hook):  M 20 0 L 100 0 L 100 100 L 80 100 L 80 20 L 20 20 Z   #1f6feb
cursor:         rect x40 y36 w20 h48                                    #f59e0b
```

Rendered to a fixed cell canvas (target ≈ 6 rows × 6 cols) with block/box
glyphs in `dashboard.logo`, with a `dashboard.cursor` bar (`▌`/`█`) centred in
the negative space. Per the icon-palette rule (`CLAUDE.md` UX rules), it ships
**two same-width palettes** — a Nerd-Font-v3 form when `ui.nerd_fonts=on` and a
BMP-block fallback (Geometric Shapes / Block Elements) by default — so the first
frame is correct in any font and the toggle re-renders without shifting column
geometry.

### 5.2 Layout — the symmetry we adopt

The current lockup (`assets/lattice-lockup.svg`) places the wordmark with a
loose gap (`x=150` vs mark right edge ~120) and a low baseline (`y=98` against a
mark spanning `y=10..130`) — bottom-heavy and slightly detached. The
`banner-dark.svg` reference fixes exactly this: a **tight, consistent
icon↔wordmark gap** and the **wordmark block vertically centred against the
icon** (name and tagline stacked, their combined midpoint aligned to the mark's
midpoint). We adopt that metric — *not* the blinking cursor animation, which is
irrelevant here.

```
  ▛▀▜        Lattice
  ▌ ▐        A modal, GPU-accelerated, plugin-first
  ▙▄▟        text editor in Rust
```

Concretely: the mark occupies the left N rows; "Lattice"
(`dashboard.title`) + tagline (`dashboard.tagline`) form a right block whose
vertical centre aligns to the mark's vertical centre, separated by a fixed
2-cell gap. In the TUI the wordmark is one styled row (fonts don't scale).

**GPUI (DB.4-gpui, landed):** the "Lattice" wordmark renders **larger** than
the mark blocks and tagline via **per-token virtual-row scaling** — the
branding provider tags the wordmark's columns with a scale (`WORDMARK_SCALE`)
in `VirtualRow::scales`, and the GPUI peer shapes that run at
`font_size × scale` on a shared baseline (the N-piece `ScaledLine` path, shared
with document headings). This is the general variable-font primitive
(`virtual-rows.md` §3.4, `theme-system.md` Thread F), not a dashboard-specific
render branch. The mark blocks and tagline stay base-size; the TUI ignores the
scale and paints every cell base-size (tracked cross-renderer divergence).
A pixel-perfect *side-by-side* scaled lockup (a big wordmark spanning the mark's
full height beside it) is a renderer-side custom-paint follow-on; the per-token
primitive scales the wordmark within the shared row model.

### 5.3 Centring

Center alignment is **new** work — there is no alignment attribute on
`Cell`/`CellRow`, and only the modeline centres (via manual padding). Per the
substrate-vs-helper rule (`CLAUDE.md`), alignment stays a **content concern**:
the compositor computes leading padding cells against `ctx.pane_width` at
compose time (the modeline pattern), so both renderers paint the already-centred
cells through the existing fast path — no renderer branches on the dashboard,
no hot-path struct grows a field for a single consumer. On resize the block is
recomposed (§7); a frame-late recentre on resize is within UX tolerance (resize
is not a keystroke path). A line-level `align` attribute is a **rejected**
alternative until a second consumer needs it (§10).

## 6. Body sections

Everything below the branding block is ordinary **document content** (rope
text) so the cursor can land on links and `<CR>` follows them. Section headings
use `SectionHeading`, links carry `Style::Link` spans seeded into the buffer's
`ExtraHighlights` local (the help mechanism, `crates/lattice-help`), and body
prose is `Body`. Links resolve via §3.3.

For v1, body link/heading colour uses the existing semantic `Style` bridge
(theme-overridable via `syntax.heading.*` / `Style::Link`). Routing body spans
through the *custom* `dashboard.section` / `dashboard.link` elements needs the
buffer-local theme-remap seam (`theme-system.md` §5 scope 2); it is a noted
enhancement (§10), not v1-blocking — the branding block already exercises the
custom `dashboard.*` elements where the visual payoff is highest.

## 7. Lifecycle & composition

- **Creation is off-thread.** The `*dashboard*` buffer is a synthetic Document
  (`ensure_named_document`) with `name = "*dashboard*"`. Composition
  (registry → fragments → cells + rope) runs on the actor / `spawn_blocking`,
  never in a render frame (paramount #1). Owner writes go through
  `apply_edit_batch_blocking`; read-only enforcement at the dispatcher's
  Insert/operator path means owner writes bypass naturally without a token.
- **Recompose triggers.** Pane resize (re-centre), theme change (re-resolve
  colours), nerd-font toggle (re-pick palette), config change to
  `dashboard.sections` / `dashboard.source`. Each is an event-bus subscription
  the mode owns; recompose is O(sections), not per-frame, and idle frames do
  **zero** dashboard work.
- **Idempotent.** `:dashboard` and the startup path both call the same
  `ensure + compose-if-stale + activate`; re-running never duplicates the
  buffer.

## 8. Configuration (declarative)

Three options, in a `dashboard.*` group (`options!` macro,
`crates/lattice-config`):

| option | type | default | effect |
|---|---|---|---|
| `dashboard.enabled` | bool | `true` | auto-open `*dashboard*` on launch when no file arg |
| `dashboard.sections` | string | `""` | ordered section ids; present ⇒ shown in this order, omitted ⇒ hidden; empty ⇒ all built-ins in default order |
| `dashboard.source` | string | `""` | path to a user file that **replaces** section composition; empty ⇒ unset |

The single ordered `dashboard.sections` value covers **both** toggle and
reorder declaratively — chosen over per-section `dashboard.section.<id>.enabled`
booleans, which cannot express order (§10). The config system has no native
list type, so the v1 encoding is a comma/whitespace-separated string parsed by
`SectionSelection::parse` (empty ⇒ all built-ins in default order; a list ⇒
exactly those ids in that order, unknown ids skipped). `dashboard.source` is the "author
the entire dashboard" escape hatch; a missing/unreadable path logs a warning and
falls back to section composition — never a panic, never an empty page.

## 9. Command & buffer surface

- **`:dashboard`** — a `dashboard`-namespaced ex-command whose `apply` returns
  the lifecycle effect `Effect::OpenDashboard`, handled host-side by
  `do_open_dashboard` (ensure + compose + activate). This is the same sanctioned
  lifecycle-open residue `:messages`→`OpenMessages` and `:diff`→`DiffOpen` use —
  buffer creation mutates `&mut Editor`, which a mode crate cannot hold, so the
  *applier* is the host boundary. The *content* is built entirely in the crate
  (§9.2). Dashed, namespaced; no invented short alias (short slots are reserved).
- **`:b *dashboard*`** — resolves via `BufferRegistry::by_name`, identical to
  `*messages*`. The buffer carries `listed:false`, so `:bn`/`:bp` skip it while
  `:b`/`:ls` reach it.
- **Startup** — with no file arg and `dashboard.enabled`, the initial shown
  buffer is `*dashboard*` instead of an empty scratch document. With a file
  arg, the file opens directly and the dashboard is merely reachable on demand.

### 9.2 Buffer realization + link-follow (Option D)

The `*dashboard*` buffer is a distinct `BufferKind::Dashboard`
(`BufferData::Dashboard(DocumentEntry)`, parallel to `Messages`), read-only,
flowing through the same Document code paths — no kind-specific
render/motion/scroll branch. `dashboard-mode` is its **major** mode
(`target_buffer_kind = Dashboard`), and owns its option surface directly —
`ReadOnly`/`Wrap`/`NoFile`/`Number=false`/`signcolumn=no` (the same set
help-mode contributes) — plus the `dashboard.*` theme elements (§4) and the
branding provider (§5). It is self-contained: no help-mode dependency for
behaviour.

Link-follow, Esc-dismiss, and markdown highlighting are **reused from the help
machinery via the buffer kind**, not by activating help-mode. The three
existing `BufferKind::Help` gates gain `BufferKind::Dashboard` alongside Help,
because a dashboard is behaviourally the same shape — a read-only, link-bearing,
help-style buffer:

1. `input.rs` Normal-mode gate (`<CR>` → `FollowLink`, `<Esc>` → dismiss) —
   `Help | FileTree` → `Help | FileTree | Dashboard`.
2. `dispatch.rs` `Action::FollowLink` arm — `Help` → `Help | Dashboard` calls
   `do_help_follow_link`.
3. `do_help_follow_link` guard — `active_buffer != Help` → `!matches!(active,
   Help | Dashboard)`.

These are grouped-with-Help (identical behaviour, aligned by explicit
enumeration + comment per the no-kind-branch rule), not divergent per-kind
logic. The link *data* is seeded kind-agnostically: `do_open_dashboard` builds a
`HelpContent` from the composed `DashboardFragment`s, writes the text via
`append_to_owned_buffer`, and seeds the `HelpLinks`/`HelpAnchors`/
`ExtraHighlights` locals + markdown syntax via `seed_help_metadata_locals`
(reusing `lattice_help::link_highlights`). `do_help_follow_link` reads
`HelpLinks` by `BufferId` — already kind-agnostic. Every new `match BufferKind`
arm the variant forces is resolved by grouping `Dashboard` with the
semantically-matching kind (mostly `Document`/`Messages`; `Help` for the follow
gates), and the buffer must pass the regular-buffer parity test.

### 9.1 Mode-ownership of the startup trigger

Boot publishes a generic **`Startup { opened_file: Option<PathBuf> }`** event
(or a `StartupContext` service). The dashboard mode's `install(&mut boot)`
registers a subscription: on `Startup`, if `opened_file.is_none() &&
dashboard.enabled`, it emits the *same* `Effect::OpenDashboard` the `:dashboard`
command emits — one applier, two triggers. The activation-decision (no file +
enabled) and all content live in `lattice-dashboard`.

**Where the host boundary honestly sits.** The crate owns the mode, its
options, its `dashboard.*` theme elements, the section registry, composition,
and config — zero `Editor::` methods for *that* surface, zero new host `Action`
variants. The host residue is (a) the `BufferKind::Dashboard` variant + its
match arms (grouped with the semantically-matching kind, §9.2), (b) one
lifecycle effect (`Effect::OpenDashboard`) + its applier (`do_open_dashboard`),
because buffer creation mutates `&mut Editor` — exactly what every
synthetic-buffer subsystem carries (`:messages`/`:diff`). This is the sanctioned
boundary, not a mode-surface leak. (A future generic
`Effect::ActivateNamedBuffer` could retire the per-feature appliers across
messages/diff/dashboard alike — noted as a separate generalisation, not taken
here.)

## 10. Rejected alternatives

- **Make the dashboard a Help-kind buffer (help-mode's own shape).** Rejected as
  the *identity*: the buffer loses a distinct `:ls` identity and `dashboard-mode`
  can't be its major. The chosen shape (§9.2) is a distinct `Dashboard` kind +
  major, reusing help's *behaviour* (follow/dismiss/markdown) via the kind
  gates.
- **Property-gate link-follow on help-mode-active (de-kind the 3 help gates).**
  Genuinely-better long-term (removes anti-pattern kind-branches, completes the
  decoupling help-mode's docstring documents) — but it is *help-mode's* refactor,
  spanning three sites (`input.rs` + two in `dispatch.rs`); doing only the slice
  the dashboard needs is a half-migration. Rejected for DB.2 in favour of
  extending the existing kind-gates (grouped-with-Help), which is self-contained.
  The full de-kinding is noted as a future help-mode slice that would then let
  the gates drop `Dashboard` again.
- **Static markdown-file dashboard only.** Cannot carry plugin-contributed
  sections; the section registry is the stated requirement. *(Paramount #2.)*
- **WASM-init contributes the content.** Static branding is *content*, not
  *logic*; the "static stays declarative, logic stays code" rule puts it in a
  file/registry, not the init module. Reserved as the future plugin-section
  path, not the v1 authoring path.
- **TOML `dashboard.content` scalar.** A multiline document body in a TOML
  scalar is awkward; TOML is for static option overrides, not document bodies.
- **Line-level `align` attribute on `Cell`/`CellRow` + both renderers honour
  it.** Over-generalises for a single consumer. Alignment is baked into cells
  content-side (§5.3) per the substrate-vs-helper rule; promote to a row
  attribute only when a second consumer appears.
- **Per-section `dashboard.section.<id>.enabled` booleans.** Cannot express
  order; the ordered list subsumes it.
- **Kind-specific render logic for `BufferKind::Dashboard`.** The `Dashboard`
  kind exists for identity/labelling (parallel to `Messages`), but it must flow
  through the same Document code paths — behaviour comes from `dashboard-mode`
  and property-gated follow (§9.2), never a `match kind { Dashboard => … }` in
  render/motion/scroll. Must pass the regular-buffer parity test
  (`multibuffer_is_a_regular_buffer.rs` shape).

## 11. Paramount-goal alignment

- **#1 Performance.** Composition is off-thread at creation / resize / theme /
  config change only; idle frames do zero dashboard work; branding virtual rows
  and body cells paint through the existing fast paths; no new hot-path struct
  field. UX: read-only, so no keystroke-latency surface at all.
- **#2 Extensibility.** The registry is the section seam (config today,
  plugins tomorrow — add / replace by id / whole-buffer author); theme elements
  are user-overridable; content is user-authorable via `dashboard.source`;
  config is declarative.
- **#3 Vim modal grammar.** The dashboard is a buffer: `:dashboard`,
  `:b *dashboard*`, `:ls`, and every motion behave uniformly; read-only is
  enforced at the dispatcher Insert/operator path; no dirty flag, no `:q`
  prompt, never in the modified set.
- **#4 Asynchronicity.** Creation + compose run on the actor / `spawn_blocking`;
  the startup trigger is an event subscription; nothing blocks the UI.

## 12. Testing strategy

- **Registry** — ordering by `order`; `dashboard.sections` selects + reorders;
  empty list ⇒ all built-ins default order; unknown id in the list is skipped
  with a logged warning.
- **Full override** — `dashboard.source` present ⇒ file content replaces
  sections; missing/unreadable ⇒ falls back to sections + logs, no panic, never
  empty.
- **Startup gating** — no file + enabled ⇒ `*dashboard*` active; file arg ⇒
  file active, dashboard reachable not auto-shown; `enabled=false` ⇒ not
  auto-shown but `:dashboard` still works.
- **Read-only invariants** — Insert/operators are inert; buffer never dirty;
  `:q` on it never prompts; excluded from the modified set; `listed:false`
  (skipped by `:bn`/`:bp`, reached by `:b`/`:ls`).
- **Regular-buffer parity** — passes the `multibuffer_is_a_regular_buffer.rs`
  shape: motions/scroll/cursor identical to a Document.
- **Link-follow** — `<CR>` on a `cmd:` / `topic:` / `url:` span fires the
  target; `url:` with no opener logs + skips; Esc dismisses. Regression: help
  buffers still follow after `Dashboard` is added alongside `Help` in the three
  gates (grouped, not replaced).
- **Theme** — `dashboard.*` registered under `ElementOwner::Mode`; a theme
  override changes the rendered colour; default resolves to the brand colours.
- **Branding** — nerd-font vs BMP-block art are the same cell width; toggle
  re-renders; wordmark block is vertically centred against the mark (dimension
  assertions) with the fixed gap.
- **Cross-renderer** — end-of-slice grep audit that the branding + body paths
  need no TUI-only or GPUI-only branch (colours are resolved cells; alignment is
  baked cells).

## 13. Benchmarks

The dashboard is **not on the keystroke path** — it is composed off-thread at
creation, never typed into. A keystroke→glyph bench does not apply. Coverage is
instead:

- a **creation-time assertion**: compose + seed of the default page completes
  under a recorded threshold on the actor;
- an **idle-frame assertion**: rendering the dashboard for a frame with no input
  does zero recompose / zero I/O (guards paramount #1).

Recorded in `BENCHMARKS.md` alongside the other display-path numbers; this is
noted explicitly so a reviewer does not expect (or add) a keystroke bench that
doesn't fit.

## 14. Cross-references

- `crates/lattice-dashboard/` — mode, registry, sections, theme elements,
  `:dashboard`, startup subscription (new crate).
- `crates/lattice-multibuffer/src/lib.rs` — `register_multibuffer_theme_elements`,
  the copy-from template for §4.
- `crates/lattice-help/` — help-link parsing + `<CR>` follow reused by §3.3/§6.
- `crates/lattice-cells/src/virtual_rows.rs`, `headerline.rs` — the anchored /
  sticky row primitive for the §5 branding block.
- `docs/dev/architecture/theme-system.md` — §4 element registration, §5 override
  scopes, Thread F scale vocabulary.
- `docs/dev/architecture/virtual-rows.md`, `headerline.md` — §5 substrate.
- `assets/lattice-mark.svg`, `lattice-lockup.svg`, `assets/README.md` — brand
  geometry + colours for §5.
- `docs/dev/operations/slice-plans/dashboard.md` — sequencing / status.
