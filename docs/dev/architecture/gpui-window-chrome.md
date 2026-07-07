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
the resizable style bit — GPUI only sets `NSResizableWindowMask` in the
titlebar-`Some` branch. The consequence is stronger than cosmetic: a borderless
GPUI window on macOS is **non-resizable by every means** —

1. `zoom()`/maximize is a no-op — which is why `start-maximized` opens the
   borderless window *pre-sized to the display* rather than maximizing it after
   the fact (see Feature 2).
2. Edge-drag resize is gone.
3. Even external window managers (Raycast, yabai, Rectangle) **cannot** resize or
   move it: the Accessibility `setFrame` is rejected by a non-resizable NSWindow
   (observed in practice: Raycast reports "failed to set window rect in 3
   attempts"). This *corrects* an earlier assumption in this design that AX tools
   bypass the style bit — they do not.

The only way to make a borderless macOS window resizable (edge-drag AND
AX-controllable by Raycast/yabai) is to fork GPUI to thread `is_resizable` into
the `titlebar: None` branch (add `NSResizableWindowMask`) — a minimal
`[patch.crates-io.gpui]` fork; see Rejected alternatives. Absent that fork, a
borderless macOS window is fixed at its creation size.

## Feature 1 — borderless window (`window.decorations`)

**Option.** New `Window` option group (`group.rs`, `NAME = "window"`, so
`:customize window` shows the group) and a `Decorations` enum in
`lattice-config`. The option *name* carries the `ui.` prefix, matching the
established UI-presentation convention (`ui.modeline.*`, `ui.diagnostics.*`,
`ui.diff.*`) — the group NAME and the dotted option name are decoupled exactly
as they are for `Modeline` (NAME `"modeline"`, options `ui.modeline.left`):

```
// lattice-config: window enum, modeled on SignColumn
pub enum Decorations { Full, None_, Transparent }  // "full" | "none" | "transparent"; default Full

// window_options.rs
crate::options! {
    group = crate::Window;

    /// OS window chrome. `full` (default) keeps the system titlebar and
    /// controls. `none` removes them for a borderless window (as in
    /// alacritty `decorations = none` / kitty / emacs `undecorated`).
    /// `transparent` looks frameless but stays resizable.
    #[name("ui.window.decorations")]
    pub WindowDecorationsOption: Decorations = Decorations::Full;
}
```

Enum name is `Decorations` (not `WindowDecorations`) to avoid colliding with
`gpui::WindowDecorations` when both are in scope in the GPUI peer.

**Three values, mapped by `window_chrome()`:**

- `full` → `(Some(full_titlebar()), None)` — system chrome (default).
- `none` → `(None, Some(WindowDecorations::Client))` — borderless. Per the
  per-platform table above; **non-resizable on macOS** (no `NSResizableWindowMask`).
- `transparent` → `Some(TitlebarOptions { appears_transparent: true, .. })` (buttons
  left at their default position) + a post-open `hide_traffic_lights(window)` call
  that sends `setHidden:` to the three standard buttons via gpui's raw window
  handle. The titlebar is `Some`, so GPUI keeps `NSResizableWindowMask`: the window
  stays **resizable** (edge-drag + AX tools like Raycast/yabai work) while the
  transparent titlebar + hidden buttons make it look frameless. Critically, the
  buttons are *hidden* (`setHidden:`), NOT *moved* — parking them off-screen breaks
  the window's Accessibility geometry so Raycast can no longer set its frame (this
  cost a real debugging round). Rounded corners + shadow remain (not truly
  borderless); truly-frameless-and-resizable still needs the fork.
  This is the macOS-friendly frameless option — the no-fork answer to "borderless
  *and* Raycast-controllable" (the fork in Rejected alternatives is the only path
  to truly-frameless *and* resizable). `is_borderless()` is `true` only for
  `none`, so the maximize path treats `transparent` as a normal resizable window.

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

**Mechanism: `WindowBounds` at `open_window`, not a runtime action.** Maximize is
a window-*creation* state, so it is set on `WindowOptions` in `run()` from the
same early standalone config read that resolves `decorations` — never applied
after the window exists. The strategy branches on whether the window is resizable,
which `decorations` determines:

- **Decorated (`full`) → `WindowBounds::Maximized(restore)`.** The window is
  resizable, so GPUI's maximized state (macOS zoom / X11 maximized / Windows
  `SW_MAXIMIZE`, applied by GPUI during window construction) works.
- **Borderless (`none`) → `WindowBounds::Windowed(display.bounds())`.** On macOS a
  borderless window is non-resizable (see the macOS note), so `zoom()` is a no-op.
  Instead the window opens already sized to the full display via
  `cx.primary_display().bounds()` — the creation-time frame is honored regardless
  of later resizability, so it fills the screen on every platform.

Note: `display.bounds()` is the *full* display frame (GPUI does not expose the
work area / `visibleFrame`), so a borderless maximized window covers the whole
screen including the macOS menu-bar strip.

**Rejected: a runtime `WindowCommand` queue + render-drain `zoom_window()`.** The
first implementation pushed `WindowCommand::Maximize` onto a GPUI-local
`Arc<Mutex<VecDeque<…>>>` in the boot seam and drained it in `render()` via
`window.zoom_window()`. It did **not** work: `zoom_window()` → macOS `zoom()` is a
no-op on the non-resizable borderless window, and applying window state from the
render path fought GPUI's window lifecycle. The queue, the boot-seam push, and the
render drain were all removed. A future runtime `:maximize` would set
`WindowBounds` at open for the launch case, or call `zoom_window()` only on
decorated (resizable) windows.

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
- **Paramount #4 (async):** the maximize is enqueued synchronously in the boot
  seam (no task, no I/O) and applied on the UI thread at the render-drain seam.
  The queue is the thread-safe hand-off; no blocking, no UI-thread I/O.
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
