<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `dashboard`

**Direction:** guest calls into the host through it · **Capability:** none (pure data / dispatch) · **Worlds:** `dashboard-plugin` (imports)

CR.4: plugin-contributed dashboard sections.

A plugin puts its own block on the launch page — recent projects, a git
summary, whatever it is for. The section lands in the SAME registry the
built-in sections live in, so `dashboard.sections` orders it, the
compositor renders it, and the theme styles it, with no host kind-branch.

### A function, not data — unlike `help`

The `help` seam hands over a string once and drops the guest, because a
help page does not change between load and read. A dashboard section is
different in kind: `render-section` takes a `ctx` the guest cannot know at
load — the pane width, whether Nerd Font glyphs are available, the editor
version — and DB.6 exists precisely because those change while the editor
runs. So the guest stays instantiated and the host calls it per compose.

Freezing a section into text at registration would make it blind to the
icon palette and unable to show anything live, which is most of what
"whole-author a section" is for.

### Where it runs, and what that costs

`render-section` is a **sync** call on the host's sync linker (the one
`grammar` and `error-parser` share), carrying the Reflex-class budget
rather than the generous lifecycle default. It executes on the actor
thread inside the dashboard compositor.

That cost is real and deliberate. Composition is a `LatencyClass::Display`
action — `:dashboard`, startup, or a DB.6 option change — never
per-keystroke and never per-frame, and the fuel budget bounds a
pathological guest to a bounded stall rather than a hang.

The alternative, rendering off-actor and recomposing when the fragment
lands, is purer on paramount goal #1 and was rejected on UX: it makes the
launch page visibly reflow a frame or two after it appears, at startup,
which is the content-jump the UX contract vetoes.

### What the host does with a bad fragment

Validates and drops, never traps. Guest output is untrusted: a row with no
spans, a span whose link does not parse, or a fragment longer than the row
cap is dropped at `debug!`. A trap poisons the section — it renders
nothing further this session and the REST OF THE PAGE still composes,
exactly as a trapping `error-parser` costs its own entries and not the
build.

## Functions (1)

### `register-section`

```wit
register-section: func(id: string, order: s32, default-enabled: bool) -> result<_, string>
```

Declare a section.

`id` is **NOT** auto-namespaced, unlike `help.register-topic` and
`theme.register-element`. That is deliberate: replacing a built-in
section by id is a supported thing to want, so a plugin registering
`getting-started` is exercising the feature rather than squatting.
Unload restores whatever it displaced — the registry shadows rather
than overwrites.

`order` is the default sort key (lower sorts first); `default-enabled`
is whether it shows when the user has not set `dashboard.sections`.

`err` when the spec is malformed (an empty id) — never a trap.

## Types (7)

### record `ctx`

```wit
record ctx {
    pane-width: u32,
    nerd-fonts: bool,
    version: string,
}
```

Read-only facts a section renders against. Mirrors the native
`DashboardCtx`.

**Fields**

- `pane-width`: `u32` — Pane width in cells.
- `nerd-fonts`: `bool` — Whether Nerd Font glyphs may be used. A section that draws icons
  MUST honour this and fall back to the BMP-block palette at the
  same cell width, or the page's column geometry shifts when the
  user toggles `ui.nerd_fonts`.
- `version`: `string` — The editor version string.

### enum `role`

```wit
enum role {
    logo,
    cursor,
    title,
    tagline,
    section-heading,
    body,
    key,
    hint,
    link,
}
```

Semantic style role. Never a colour — each resolves to a `dashboard.*`
theme element at compose time, so a section re-colours on
`:colorscheme` like everything else.

### enum `align`

```wit
enum align {
    left,
    center,
}
```

Line-level alignment.

### variant `link-target`

```wit
variant link-target {
    command(string),
    topic(string),
    url(string),
}
```

What `<CR>` on a link span follows.

**Cases**

- `command`: `string` — Run an ex-command — `command("tutor")` STARTS the tutor.
- `topic`: `string` — Open a `:help` topic.
- `url`: `string` — Open a URL externally.

### record `span`

```wit
record span {
    text: string,
    role: role,
    link: option<link-target>,
}
```

A run of text with a role and an optional follow target.

**Fields**

- `text`: `string`
- `role`: [`role`](#enum-role)
- `link`: `option<link-target>`

### record `row`

```wit
record row {
    spans: list<span>,
    align: align,
}
```

One visual line: spans laid out left→right.

**Fields**

- `spans`: `list<span>`
- `align`: [`align`](#enum-align)

### record `fragment`

```wit
record fragment {
    rows: list<row>,
}
```

A section's rendered contribution.

