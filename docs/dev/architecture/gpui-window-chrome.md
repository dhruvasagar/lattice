# GPUI Window Chrome & Control

Two configurable GPUI-window behaviors: a **borderless** (decoration-free)
window, and **maximize-on-launch** driven by the boot `Startup` event. Both are
GPUI-peer concerns — the TUI has no OS window — surfaced as renderer-agnostic
`ui.window.*` options that the TUI simply never reads.

Slice plan: `docs/dev/operations/slice-plans/gpui-window-chrome.md` (sequencing,
status icons). This fragment carries the contracts, the per-platform mapping,
and the rejected alternatives.

## Motivation

Users who run tiling / manual window managers (yabai, Raycast, sway, Rectangle)
want the editor window without OS chrome, exactly as they configure
`alacritty` (`window.decorations = none`), `kitty` (`hide_window_decorations`),
or `emacs` (`undecorated t`). They also want the window to fill the screen on
launch. Both are static presentation preferences, so they belong in the typed
option system (declarative config → TOML / `:set` / `:customize`), not in the
programmable init layer.

## What GPUI 0.2.2 actually permits

GPUI is an external crates.io dependency (`gpui = "0.2.2"`); we do **not** fork
it. The design uses only its public `WindowOptions` surface and public `Window`
methods. The relevant primitives, verified against the vendored source:

- `WindowOptions.titlebar: Option<TitlebarOptions>` — `None` requests a
  chrome-free window; `Some { appears_transparent, .. }` keeps a (optionally
  transparent) system titlebar.
- `WindowOptions.window_decorations: Option<WindowDecorations>` — `Server`
  (WM draws) vs `Client` (app draws / CSD). Linux-only; ignored elsewhere.
- `Window::zoom_window()` — maximize (macOS zoom / X11 maximize state /
  Windows `SW_MAXIMIZE`). Distinct from `toggle_fullscreen()`, which we do
  **not** use (native fullscreen takes a separate Space / hides the menu bar —
  not what "maximize on launch" means).

### Per-platform mapping for `decorations = none`

| Platform | `WindowOptions` | Resulting window |
|----------|-----------------|------------------|
| **Linux X11** | `titlebar: None`, `window_decorations: Some(Client)` | `_MOTIF_WM_HINTS` decorations=0 → WM strips the titlebar. **True borderless, still WM-resizable.** The most flexible platform. |
| **Windows** | `titlebar: None` (→ `hide_title_bar = true`) | No caption; **still edge-resizable** — `WS_THICKFRAME` is kept via `is_resizable`, independent of the titlebar. |
| **macOS** | `titlebar: None` | No titlebar, no traffic-light buttons, window stays movable. Rounded corners + system shadow remain; no internal edge-resize. |
| **Linux Wayland / WSLg** | same as X11 | CSD-off best-effort; the blade renderer may still draw a minimal shadow. |

`decorations = full` (default) → today's behavior verbatim: `titlebar:
Some(TitlebarOptions { title: "Lattice", .. })`, `window_decorations`
unset/`Server`.

### macOS honesty note

macOS is the one platform where the *public* API cannot produce a truly
borderless (`NSBorderlessWindowMask`) window, and where `titlebar: None` drops
the resizable style bit (GPUI only sets `NSResizableWindowMask` in the
titlebar-`Some` branch). This is acceptable because:

1. External window managers (Raycast, yabai, Rectangle) resize/move the window
   through the macOS Accessibility API, which does **not** depend on the
   `NSResizableWindowMask` bit — the same reason a borderless kitty/emacs window
   is still resizable via those tools.
2. Removing rounded corners + shadow, or restoring internal edge-resize, would
   require forking GPUI to thread `is_resizable` into the `None` branch and/or
   expose a borderless mask. That maintenance cost buys only cosmetic corners on
   one platform — not worth it (see Rejected alternatives).

## Feature 1 — borderless window (`window.decorations`)

**Option.** New `Window` option group (`group.rs`, `NAME = "window"`, so
`:customize window` shows the group) and a `Decorations` enum in
`lattice-config`. The option *name* carries the `ui.` prefix, matching the
established UI-presentation convention (`ui.modeline.*`, `ui.diagnostics.*`,
`ui.diff.*`) — the group NAME and the dotted option name are decoupled exactly
as they are for `Modeline` (NAME `"modeline"`, options `ui.modeline.left`):

```
// lattice-config: window enum, modeled on SignColumn
pub enum Decorations { Full, None }   // labels: "full" | "none"; default Full

// window_options.rs
crate::options! {
    group = crate::Window;

    /// OS window chrome. `full` (default) keeps the system titlebar and
    /// controls. `none` removes them for a borderless window (as in
    /// alacritty `decorations = none` / kitty / emacs `undecorated`).
    #[name("ui.window.decorations")]
    pub WindowDecorationsOption: Decorations = Decorations::Full;
}
```

Enum name is `Decorations` (not `WindowDecorations`) to avoid colliding with
`gpui::WindowDecorations` when both are in scope in the GPUI peer.

**Application seam.** A pure, platform-cfg'd mapping function owns the
translation, so it is unit-testable without opening a real window:

```
// lattice-ui-gpui, pure — no Window/App needed
fn window_chrome(dec: Decorations) -> (Option<TitlebarOptions>, Option<gpui::WindowDecorations>)
```

**Boot-ordering wrinkle (and its fix).** The `WindowOptions` literal is built
*before* `cx.open_window(...)`, but the `Editor` — which owns the config and
runs `load_persistent_config` (the user-TOML read) — is booted *inside* the
`open_window` builder closure (`GpuiApp::new` → `Editor::boot`). So the window
option cannot be read off the booted editor at the point `WindowOptions` is
constructed. Resolution: `run()` does an **early standalone config read** before
`open_window` — `ConfigRegistry::new()` + `init_from_linkme()` (registers every
option, including `window.*`) + `load_default_paths(&reg, workspace_root, &prefixes)`
+ `reg.get_typed::<WindowDecorationsOption>()` (the `ui.window.decorations`
option). The value feeds the pure
`window_chrome()` mapping into the `WindowOptions` literal. The editor's own
boot re-parses config as usual; `window.decorations` is thus parsed twice (once
early for chrome, once at boot). This is a cheap TOML read on a cold path and
avoids restructuring the delicate boot/focus ordering — preferred over
pre-booting the `Editor` outside the builder. `start_maximized` has no such
wrinkle: it is read after boot, at `Startup`.

**Read-once semantics.** `decorations` is applied at window creation only.
Live re-toggle via `:set window.decorations=none` mid-session is out of scope
(GPUI has no public post-creation style-mask setter); a `:set` takes effect on
next launch. This is called out in the user docs, not silently dropped.

## Feature 2 — maximize on launch (`window.start_maximized`)

**Option.**

```
crate::options! {
    group = crate::Window;

    /// Maximize the window on launch (fill the work area, keep the menu
    /// bar — not native fullscreen). GPUI peer only; ignored by the TUI.
    #[name("ui.window.start-maximized")]
    pub StartMaximized: bool = false;
}
```

The `start-maximized` key is hyphenated to match the `ui.*` family's multi-word
convention (`ui.diagnostics.inline-min-severity`, `ui.diff.fold-unchanged`).

**Why a GPUI-local command queue, not a shared `Effect`.** Window control is
inherently GPUI-only — the TUI owns no window. Adding an `Effect::WindowControl`
variant to the shared cross-renderer enum would force a TUI no-op arm, exactly
the "one peer must no-op a shared variant" smell the cross-renderer discipline
warns against. Instead the mechanism lives entirely in the GPUI peer:

```
// lattice-ui-gpui — extensible; only Maximize implemented now
enum WindowCommand { Maximize }

// on GpuiApp: thread-safe queue drained on the UI thread
window_commands: Arc<Mutex<VecDeque<WindowCommand>>>
```

**Trigger (off the UI thread).** During the GPUI boot seam the peer subscribes
to `lattice_mode::Startup` via `event_bus().subscribe_typed(tx)` — the exact
pattern `lattice-dashboard::install` uses. On receipt, if
`config.get::<StartMaximized>()` is true, it pushes `WindowCommand::Maximize`
onto the queue and fires the existing `paint_request` wake.

**Application (on the UI thread).** `EditorView::render` — which is handed
`&mut Window` — drains the queue at the top of the frame and calls
`window.zoom_window()` per command. The queue is FIFO, drained-to-empty, so a
command posted from an async task lands on the very next paint. `zoom_window`
failures are impossible to observe (infallible GPUI call), but the drain path
logs at `debug!` if a command is dropped for any future fallible variant.

**This queue is the reusable "window-control API."** A future `:maximize`
ex-command or a WASM-init call would push `WindowCommand::Maximize` onto the
same queue — the Startup subscriber is just the first producer. No new seam is
needed for those; they are out of scope for this change (YAGNI) but the shape
does not preclude them.

## Cross-renderer stance

The TUI peer reads neither option and has no window-command queue. `ui.window.*`
options are declared once in `lattice-config` (always compiled) and are inert
under the TUI — no `BufferKind`/renderer branching, no shared-enum no-op arm.
This satisfies the "modes/peers own their surface; no cross-renderer variant a
peer must no-op" rule: the GPUI peer wholly owns both the option *reads* and the
window-control *mechanism*.

## Paramount-goal & heuristic alignment

- **UX (higher court):** no effect on keystroke→glyph latency or pixel
  stability — window chrome is set once at creation; maximize fires at most once
  per boot before the user types. Borderless is opt-in; the default is
  unchanged, so no regression for existing users.
- **Paramount #1 (performance):** `render`'s per-frame drain is an
  `Option`-cheap `VecDeque::pop_front` on an almost-always-empty queue —
  O(1), no allocation, no I/O. Not proportional to document content.
- **Paramount #4 (async):** the Startup subscriber runs on a tokio task (never
  the UI thread); the queue marshals the command back to the UI thread at the
  render seam. No blocking, no UI-thread I/O.
- **Heuristic #1 (long-term fit on merit):** uses GPUI's public API to its
  fullest per platform rather than forking for cosmetic macOS parity; the
  `WindowCommand` enum is the genuinely-right primitive for future window ops,
  not an over-built abstraction (one variant now).
- **Heuristic #2 (paramount, not other editors):** alacritty/kitty/emacs are
  cited as *user-expectation* precedent for a UX-convention surface (per the
  "UX follows convention" rule), not as architectural justification.
- **Heuristic #3 (third option):** the GPUI-local queue is the third option that
  beats both "shared `Effect` variant" and "inline read in `run()`" — it keeps
  the shared surface clean *and* yields a reusable window-control API.

## Deliverables (four-artefact rule)

- **Docs.** This fragment; a slice plan; **user docs** — a new
  `ui.window.decorations` / `ui.window.start-maximized` section in
  `docs/user/display.md` (window chrome is display-adjacent) and rows in
  `docs/user/options.md`'s option tables. The macOS resize caveat and the
  "applies on next launch" caveat are documented explicitly.
- **Tests.** Option parse/validate + label round-trip for `Decorations`; the
  pure `window_chrome()` mapping per platform (cfg-gated unit tests); the
  `WindowCommand` queue drain (push → drain-to-empty ordering). No real-window
  test (GPUI windows are not headless-testable here).
- **Bench.** None — window config is not a hot path. Called out explicitly
  rather than adding a hollow benchmark.
- **Graceful handling.** Invalid `decorations` value → validator error at
  `:set` parse time (never panics); the render-drain never panics on an empty
  or unexpected queue state.

## Rejected alternatives

- **Fork GPUI for `NSBorderlessWindowMask` / resizable-`None` on macOS.**
  Delivers cosmetic corner/shadow removal and internal edge-resize on one
  platform, at the cost of a maintained `[patch.crates-io.gpui]` fork. External
  WM tools already resize the window; not worth the fork (heuristic #1: don't
  rewrite for marginal gain).
- **Shared `Effect::WindowControl` variant.** Pollutes the cross-renderer
  `Effect` enum with a variant the TUI must no-op. Rejected for the GPUI-local
  queue (cross-renderer discipline).
- **Inline maximize in `run()` after `open_window`.** Simplest for the launch
  case, but not driven by `Startup` and yields no reusable API. Rejected: the
  user wants a window-control mechanism the `Startup` event (and later
  `:maximize`) can call, which the queue provides for negligible extra cost.
- **`toggle_fullscreen()` for "maximize."** Native fullscreen (separate Space /
  hidden menu bar) is not what "maximize on launch" means. `zoom_window()` is
  the correct primitive.
- **Flat `window.*` key namespace (no `ui.` prefix).** Rejected: the codebase
  reserves a `ui.` prefix for UI-presentation options (`ui.modeline.*`,
  `ui.diagnostics.*`, `ui.diff.*`), and window chrome is a UI-presentation
  concern, so `ui.window.*` is the consistent choice. The group NAME stays the
  single segment `window` (for `:customize window`), decoupled from the dotted
  name — the same split `Modeline` uses.
