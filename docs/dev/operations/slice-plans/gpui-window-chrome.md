# GPUI Window Chrome & Control — Slice Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development`
> (recommended) or `superpowers:executing-plans` to implement task-by-task.
> Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add two configurable GPUI-window behaviors — a borderless window
(`ui.window.decorations = full|none`) and maximize-on-launch
(`ui.window.start-maximized`).

**Architecture:** Two new UI-presentation options in `lattice-config`; the GPUI
peer (`lattice-ui-gpui`) reads `decorations` before `open_window` (early
standalone config read) and maps it per-platform to `WindowOptions`, and
enqueues a `WindowCommand::Maximize` in the boot seam after config load, drained
on the UI thread in `render()` via `Window::zoom_window()`. TUI stays inert.

**Tech Stack:** Rust, `gpui = "0.2.2"` (external, no fork), `lattice-config`
`options!` macro + `linkme` self-registration.

**Design fragment:** `docs/dev/architecture/gpui-window-chrome.md` (contracts,
per-platform mapping, rejected alternatives). This plan owns sequencing only.

## Global Constraints

- No `gpui` fork; public `WindowOptions` / `Window` API only.
- Option value type is `Decorations` (NOT `WindowDecorations` — avoids collision
  with `gpui::WindowDecorations` in the GPUI peer).
- Option keys carry the `ui.` prefix: `ui.window.decorations`,
  `ui.window.start-maximized`. Group NAME is `window` (`:customize window`).
- Default is unchanged behavior: `decorations = full`, `start-maximized = false`.
- GPUI edits compile only under `--features window`; verify with
  `cargo build -p lattice-ui-gpui --features window` (a plain
  `cargo build -p lattice-cli` does NOT compile `lattice-ui-gpui`).
- `lattice-config` edits: verify with `cargo test -p lattice-config`.
- Diagnostic logs use `tracing::debug!`, never `info!`.

## File Structure

| File | Responsibility | Slice |
|------|----------------|-------|
| `crates/lattice-config/src/decorations.rs` (create) | `Decorations` enum + `OptionType` impl + tests | W.1 |
| `crates/lattice-config/src/group.rs` (modify) | add `Window` group | W.2 |
| `crates/lattice-config/src/window_options.rs` (create) | `options!` block for the two `ui.window.*` options | W.2 |
| `crates/lattice-config/src/lib.rs` (modify) | module decls + re-exports | W.1, W.2 |
| `crates/lattice-ui-gpui/src/window_chrome.rs` (create) | pure `window_chrome()` platform-map fn + `WindowCommand` enum + tests | W.3, W.5 |
| `crates/lattice-ui-gpui/src/window.rs` (modify) | `run()` reads `decorations` → `WindowOptions`; `render()` drains queue | W.4, W.5 |
| `crates/lattice-ui-gpui/src/lib.rs` (modify) | `GpuiApp.window_commands` field; boot-seam maximize push | W.5, W.6 |
| `docs/user/display.md`, `docs/user/options.md` (modify) | user docs | W.7 |

---

## Slice W.0 — design fragment ✅

Committed as `18c7bc10` (`docs(gpui-window-chrome): design fragment … (slice 0)`).
The Feature-2 refinement (synchronous boot-seam maximize push instead of an async
`Startup` subscriber) landed with the slice plan in `dc791736`.

---

## Slice W.1 — `Decorations` value type ✅

**Files:**
- Create: `crates/lattice-config/src/decorations.rs`
- Modify: `crates/lattice-config/src/lib.rs` (add `mod decorations;` + re-export)
- Test: inline `#[cfg(test)]` in `decorations.rs`

**Interfaces:**
- Produces: `lattice_config::Decorations` (`Full` | `None_`, default `Full`),
  impl `OptionType`; `Decorations::is_borderless() -> bool`.

- [ ] **Step 1: Write `decorations.rs`** (modeled verbatim on `signcolumn.rs`):

```rust
//! Value type for the `ui.window.decorations` typed option.
//!
//! Controls OS window chrome on the GPUI peer: `full` (default) keeps the
//! system titlebar + controls; `none` requests a borderless window (as in
//! alacritty `decorations = none` / kitty / emacs `undecorated`). Pure
//! presentation policy read only by the GPUI renderer — like [`crate::SignColumn`]
//! the value type lives here and impls [`OptionType`] locally. The TUI never
//! reads it. See `docs/dev/architecture/gpui-window-chrome.md`.

use crate::option_type::{EnumeratedValue, OptionType};

/// `ui.window.decorations` — OS window chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decorations {
    /// System titlebar + controls (the default; today's behavior).
    #[default]
    Full,
    /// Borderless: no titlebar / controls. `None_` avoids the `Option::None`
    /// name clash; the on-disk / `:set` label is `none`.
    None_,
}

impl Decorations {
    pub fn label(&self) -> &'static str {
        match self {
            Decorations::Full => "full",
            Decorations::None_ => "none",
        }
    }

    pub fn doc(&self) -> &'static str {
        match self {
            Decorations::Full => "System titlebar and window controls (default)",
            Decorations::None_ => "Borderless window: no titlebar or controls",
        }
    }

    /// True when the window should be drawn without OS chrome.
    pub fn is_borderless(&self) -> bool {
        matches!(self, Decorations::None_)
    }

    pub fn all() -> [Decorations; 2] {
        [Decorations::Full, Decorations::None_]
    }

    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s {
            "full" => Ok(Decorations::Full),
            "none" => Ok(Decorations::None_),
            other => Err(format!(
                "ui.window.decorations: expected `full` or `none`, got `{other}`"
            )),
        }
    }
}

impl OptionType for Decorations {
    fn parse(s: &str) -> Result<Self, String> {
        Decorations::parse_label(s)
    }
    fn format(&self) -> String {
        self.label().to_string()
    }
    fn type_label() -> &'static str {
        "decorations"
    }
    fn enumerate() -> Option<Vec<&'static str>> {
        Some(Decorations::all().iter().map(|v| v.label()).collect())
    }
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            Decorations::all()
                .iter()
                .map(|v| EnumeratedValue { form: v.label(), doc: v.doc() })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_full_not_borderless() {
        assert_eq!(Decorations::default(), Decorations::Full);
        assert!(!Decorations::default().is_borderless());
    }

    #[test]
    fn parse_round_trips_every_value() {
        for v in Decorations::all() {
            assert_eq!(Decorations::parse_label(v.label()).unwrap(), v);
            assert_eq!(Decorations::parse(v.label()).unwrap(), v);
            assert_eq!(v.format(), v.label());
        }
    }

    #[test]
    fn parse_rejects_unknown() {
        assert!(Decorations::parse_label("transparent").is_err());
        assert!(Decorations::parse_label("true").is_err());
    }

    #[test]
    fn none_is_borderless() {
        assert!(Decorations::None_.is_borderless());
    }

    #[test]
    fn enumerate_lists_both_forms() {
        assert_eq!(Decorations::enumerate().unwrap(), vec!["full", "none"]);
    }
}
```

- [ ] **Step 2: Register the module + re-export** in `crates/lattice-config/src/lib.rs`.
  Add near the `mod signcolumn;` line (76): `mod decorations;`. Add near the
  `pub use signcolumn::SignColumn;` line (152): `pub use decorations::Decorations;`.

- [ ] **Step 3: Run tests — expect PASS**

Run: `cargo test -p lattice-config decorations`
Expected: the 5 `decorations::tests::*` pass.

- [ ] **Step 4: Commit**

```bash
git add crates/lattice-config/src/decorations.rs crates/lattice-config/src/lib.rs
git commit -m "feat(config): Decorations value type for ui.window.decorations (W.1)"
```

---

## Slice W.2 — `Window` group + `ui.window.*` options ✅

**Files:**
- Modify: `crates/lattice-config/src/group.rs` (add `Window` group + re-export via lib.rs)
- Create: `crates/lattice-config/src/window_options.rs`
- Modify: `crates/lattice-config/src/lib.rs` (`mod window_options;` + re-export decl types; add `Window` to the `group::{…}` re-export)
- Test: `crates/lattice-config/src/window_options.rs` inline + existing registry init test

**Interfaces:**
- Consumes: `crate::Decorations` (W.1).
- Produces: `lattice_config::{Window, WindowDecorationsOption, StartMaximized}`.
  `WindowDecorationsOption::Value = Decorations`; `StartMaximized::Value = bool`.

- [ ] **Step 1: Add the `Window` group** in `group.rs`, after the `Modeline`
  block (~line 268). Mirror `Modeline`:

```rust
/// GPUI window options: OS chrome (`ui.window.decorations`) and
/// maximize-on-launch (`ui.window.start-maximized`). GPUI peer only.
pub struct Window;
impl OptionGroup for Window {
    const NAME: &'static str = "window";
    const DOC: &'static str =
        "GPUI window options (borderless chrome, maximize on launch). GPUI peer only.";
}
```

- [ ] **Step 2: Add `Window` to the group re-export** in `lib.rs` (the
  `pub use group::{ … };` block at line 135): insert `Window` into the list
  (alphabetically, after `Terminal,`).

- [ ] **Step 3: Write `window_options.rs`**:

```rust
//! GPUI window options (`ui.window.*`). GPUI peer only; the TUI never reads
//! these. `decorations` is applied at window creation; `start-maximized`
//! drives a one-shot maximize on launch. See
//! `docs/dev/architecture/gpui-window-chrome.md`.

use crate::Decorations;

crate::options! {
    group = crate::Window;

    /// OS window chrome. `full` (default) keeps the system titlebar and
    /// controls. `none` removes them for a borderless window (as in
    /// alacritty `decorations = none` / kitty / emacs `undecorated`).
    /// Applied at window creation; a change takes effect on next launch.
    #[name("ui.window.decorations")]
    pub WindowDecorationsOption: Decorations = Decorations::Full;

    /// Maximize the window on launch (fill the work area, keep the menu
    /// bar — not native fullscreen). GPUI peer only; ignored by the TUI.
    #[name("ui.window.start-maximized")]
    pub StartMaximized: bool = false;
}

#[cfg(test)]
mod tests {
    use crate::{ConfigRegistry, Decorations, StartMaximized, WindowDecorationsOption};

    fn reg() -> ConfigRegistry {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        r
    }

    #[test]
    fn defaults_match_spec() {
        let r = reg();
        assert_eq!(*r.get_typed::<WindowDecorationsOption>().unwrap(), Decorations::Full);
        assert!(!*r.get_typed::<StartMaximized>().unwrap());
    }

    #[test]
    fn set_decorations_none_parses() {
        let r = reg();
        r.parse_and_set_command("ui.window.decorations=none").unwrap();
        assert_eq!(*r.get_typed::<WindowDecorationsOption>().unwrap(), Decorations::None_);
    }

    #[test]
    fn set_start_maximized_true_parses() {
        let r = reg();
        r.parse_and_set_command("ui.window.start-maximized=true").unwrap();
        assert!(*r.get_typed::<StartMaximized>().unwrap());
    }

    #[test]
    fn bad_decorations_value_errors() {
        let r = reg();
        assert!(r.parse_and_set_command("ui.window.decorations=wat").is_err());
    }
}
```

> Confirm `parse_and_set_command` + `get_typed` + `init_from_linkme` signatures
> against `registry.rs` while writing (verified present: `get_typed`,
> `init_from_linkme`, `new`). If `parse_and_set_command` lives on a different
> type in tests, mirror the call form used in `diagnostics_options.rs` tests.

- [ ] **Step 4: Register module** in `lib.rs`: add `mod window_options;` (near
  line 69's `pub mod core_options;`) and re-export the decl types:
  `pub use window_options::{StartMaximized, WindowDecorationsOption};`.

- [ ] **Step 5: Run tests — expect PASS**

Run: `cargo test -p lattice-config window`
Expected: the 4 `window_options::tests::*` pass; existing `init_from_linkme`
registry tests still pass (new options registered without a central body).

- [ ] **Step 6: Commit**

```bash
git add crates/lattice-config/src/group.rs crates/lattice-config/src/window_options.rs crates/lattice-config/src/lib.rs
git commit -m "feat(config): Window group + ui.window.decorations/start-maximized options (W.2)"
```

---

## Slice W.3 — pure `window_chrome()` platform map ✅

**Files:**
- Create: `crates/lattice-ui-gpui/src/window_chrome.rs`
- Modify: `crates/lattice-ui-gpui/src/lib.rs` (add `mod window_chrome;` — gate to
  match how sibling modules are declared; `window_chrome` is pure and needs no
  `window` feature, but the `gpui` imports do — see Step 1)
- Test: inline `#[cfg(test)]`

**Interfaces:**
- Consumes: `lattice_config::Decorations`.
- Produces: `pub fn window_chrome(dec: Decorations) -> (Option<TitlebarOptions>, Option<gpui::WindowDecorations>)`.

- [ ] **Step 1: Write `window_chrome.rs`.** The fn is pure (no `Window`/`App`),
  so it is unit-testable. It uses `gpui` types, so the module is compiled only
  under the `window` feature (same gate as `window.rs`):

```rust
//! Pure mapping from the `ui.window.decorations` option to GPUI window
//! chrome, per platform. Isolated + pure so it is testable without opening a
//! real window. See `docs/dev/architecture/gpui-window-chrome.md` for the
//! per-platform rationale (Linux X11 true borderless via Client CSD-off;
//! Windows borderless+resizable; macOS borderless via titlebar:None).

use gpui::{SharedString, TitlebarOptions};
use lattice_config::Decorations;

/// The default `full`-chrome titlebar used today by `run()`.
pub fn full_titlebar() -> TitlebarOptions {
    TitlebarOptions {
        title: Some(SharedString::from("Lattice")),
        ..Default::default()
    }
}

/// Map `decorations` to `(titlebar, window_decorations)` for `WindowOptions`.
///
/// - `full`  → `(Some(full_titlebar()), None)` — today's behavior.
/// - `none`  → `(None, Some(Client))` — `titlebar: None` drops OS chrome on
///   every platform; on Linux, `WindowDecorations::Client` additionally asks
///   the WM to strip server-side decorations (`_MOTIF_WM_HINTS` decorations=0
///   on X11 → true borderless). `window_decorations` is ignored on macOS /
///   Windows, so requesting `Client` there is harmless.
pub fn window_chrome(
    dec: Decorations,
) -> (Option<TitlebarOptions>, Option<gpui::WindowDecorations>) {
    match dec {
        Decorations::Full => (Some(full_titlebar()), None),
        Decorations::None_ => (None, Some(gpui::WindowDecorations::Client)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_keeps_titlebar_and_no_decoration_override() {
        let (tb, dec) = window_chrome(Decorations::Full);
        assert!(tb.is_some());
        assert!(dec.is_none());
    }

    #[test]
    fn none_drops_titlebar_and_requests_client_csd() {
        let (tb, dec) = window_chrome(Decorations::None_);
        assert!(tb.is_none());
        assert_eq!(dec, Some(gpui::WindowDecorations::Client));
    }
}
```

- [ ] **Step 2: Declare the module** in `lib.rs`, matching the feature gate on
  the existing `window`/`editor_element` modules (find their `#[cfg(feature = "window")] mod window;`
  line and add `mod window_chrome;` alongside, with `pub` if `run()` needs
  cross-module access — same visibility as `window`).

- [ ] **Step 3: Run tests — expect PASS**

Run: `cargo test -p lattice-ui-gpui --features window window_chrome`
Expected: both `window_chrome::tests::*` pass.

- [ ] **Step 4: Commit**

```bash
git add crates/lattice-ui-gpui/src/window_chrome.rs crates/lattice-ui-gpui/src/lib.rs
git commit -m "feat(gpui): pure window_chrome() decorations→WindowOptions map (W.3)"
```

---

## Slice W.4 — wire `decorations` into `run()` ✅

**Files:**
- Modify: `crates/lattice-ui-gpui/src/window.rs` (`run()`, ~4668–4685)

**Interfaces:**
- Consumes: `window_chrome::window_chrome`, `lattice_config::{ConfigRegistry,
  WindowDecorationsOption, load_default_paths, Decorations}`, `Editor::workspace_root_from_cwd`.

- [ ] **Step 1: Add an early standalone decorations read** at the top of the
  `Application::new().run(move |cx| { … })` closure in `run()`, before
  `cx.open_window`. This resolves the user's TOML value before the editor boots
  (the editor's own `load_persistent_config` runs later, inside the builder):

```rust
// Early standalone read of `ui.window.decorations` — the editor (and its
// config load) boots *inside* the open_window builder below, too late to
// shape WindowOptions. A throwaway registry parses the same default paths.
let decorations = {
    let reg = lattice_config::ConfigRegistry::new();
    reg.init_from_linkme();
    let root = lattice_host::editor::Editor::workspace_root_from_cwd();
    let _ = lattice_config::load_default_paths(
        &reg,
        root.as_deref(),
        &[], // structural prefixes: none needed for a scalar/enum read
    );
    reg.get_typed::<lattice_config::WindowDecorationsOption>()
        .map(|v| *v)
        .unwrap_or_default()
};
let (titlebar, window_decorations) = crate::window_chrome::window_chrome(decorations);
```

> Confirm the third arg to `load_default_paths` — verified signature is
> `(registry, workspace_root: Option<&Path>, structural_prefixes: &[&str])`.
> `&[]` is correct for reading a non-structural scalar option. Confirm
> `Editor::workspace_root_from_cwd` is importable (it is `pub`,
> `dispatch.rs:25117`).

- [ ] **Step 2: Fold into the `WindowOptions` literal.** Replace the current
  `titlebar: Some(TitlebarOptions { … })` field (window.rs ~4674) and add the
  decorations field:

```rust
WindowOptions {
    window_bounds: Some(WindowBounds::Windowed(bounds)),
    titlebar,
    app_id: Some("com.lattice-editor.lattice".to_string()),
    window_decorations,
    ..Default::default()
},
```

  Remove the now-unused local `TitlebarOptions { title: … }` construction (moved
  into `window_chrome::full_titlebar()`); drop the `TitlebarOptions` import from
  `window.rs` if it becomes unused (`cargo build` will warn).

- [ ] **Step 3: Type-check under the window feature**

Run: `cargo build -p lattice-ui-gpui --features window`
Expected: builds clean (no unused-import warnings for `TitlebarOptions`).

- [ ] **Step 4: Manual smoke (borderless)** — documented, not automated (GPUI
  windows aren't headless-testable here):

```bash
echo 'ui.window.decorations = "none"' >> ~/.config/lattice/config.toml   # or project .lattice/config.toml
cargo run --features gui -- --gui README.md
# Expect: window opens with no titlebar / traffic lights. Remove the line to restore.
```

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ui-gpui/src/window.rs
git commit -m "feat(gpui): apply ui.window.decorations to WindowOptions at open (W.4)"
```

---

## Slice W.5 — `WindowCommand` queue + render drain ✅

**Files:**
- Modify: `crates/lattice-ui-gpui/src/window_chrome.rs` (add `WindowCommand` + queue type alias + tests)
- Modify: `crates/lattice-ui-gpui/src/lib.rs` (`GpuiApp.window_commands` field + init)
- Modify: `crates/lattice-ui-gpui/src/window.rs` (`render()` drains, ~3043)

**Interfaces:**
- Produces: `pub enum WindowCommand { Maximize }`;
  `pub type WindowCommandQueue = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<WindowCommand>>>`;
  `GpuiApp.window_commands: WindowCommandQueue`.

- [ ] **Step 1: Add the command type + a drain test** in `window_chrome.rs`:

```rust
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A window-management action applied on the UI thread by the render drain.
/// Extensible (Fullscreen/Minimize/Restore) — only `Maximize` is wired now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowCommand {
    Maximize,
}

/// FIFO hand-off from producers (boot seam today; a future `:maximize`) to the
/// UI-thread render drain. `Arc<Mutex<…>>` so a future off-thread producer is
/// safe; today both ends run on the UI thread.
pub type WindowCommandQueue = Arc<Mutex<VecDeque<WindowCommand>>>;

pub fn new_window_command_queue() -> WindowCommandQueue {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Drain every queued command in FIFO order, returning them for application.
/// Separated from the `zoom_window` call so it is testable without a `Window`.
pub fn drain_window_commands(queue: &WindowCommandQueue) -> Vec<WindowCommand> {
    let mut q = queue.lock().expect("window command queue poisoned");
    q.drain(..).collect()
}
```

  Extend the test module:

```rust
#[test]
fn queue_drains_fifo_and_empties() {
    let q = new_window_command_queue();
    q.lock().unwrap().push_back(WindowCommand::Maximize);
    q.lock().unwrap().push_back(WindowCommand::Maximize);
    let drained = drain_window_commands(&q);
    assert_eq!(drained, vec![WindowCommand::Maximize, WindowCommand::Maximize]);
    assert!(drain_window_commands(&q).is_empty());
}
```

- [ ] **Step 2: Add the field to `GpuiApp`** (`lib.rs`, struct at ~290): add
  `pub window_commands: crate::window_chrome::WindowCommandQueue,` and initialize
  it in `GpuiApp::new` (`let window_commands = crate::window_chrome::new_window_command_queue();`
  early, then include in the returned struct literal).

- [ ] **Step 3: Drain in `render()`** (`window.rs:3043`), at the very top of the
  method body, before any element construction:

```rust
// Apply any pending window commands on the UI thread (we hold &mut Window
// here). Empty on all but the first post-launch frame when maximize is set.
for cmd in crate::window_chrome::drain_window_commands(&self.app.window_commands) {
    match cmd {
        crate::window_chrome::WindowCommand::Maximize => window.zoom_window(),
    }
}
```

> Confirm the field path: `render` is `impl Render for EditorView`; the
> `GpuiApp` is held on `EditorView` (grep `self.app` / the field name in
> `window.rs`; adjust `self.app` to the actual field). `Window::zoom_window`
> is the verified public API (`gpui .../window.rs:1741`).

- [ ] **Step 4: Run tests + type-check**

Run: `cargo test -p lattice-ui-gpui --features window window_chrome`
then `cargo build -p lattice-ui-gpui --features window`
Expected: 3 `window_chrome::tests::*` pass; builds clean.

- [ ] **Step 5: Commit**

```bash
git add crates/lattice-ui-gpui/src/window_chrome.rs crates/lattice-ui-gpui/src/lib.rs crates/lattice-ui-gpui/src/window.rs
git commit -m "feat(gpui): WindowCommand queue + render-drain via zoom_window (W.5)"
```

---

## Slice W.6 — boot-seam maximize push ✅

**Files:**
- Modify: `crates/lattice-ui-gpui/src/lib.rs` (`GpuiApp::new`, after `load_persistent_config` ~415)
- Modify: `docs/dev/architecture/gpui-window-chrome.md` (commit the W.0 refinement)

**Interfaces:**
- Consumes: `GpuiApp.window_commands` (W.5); `lattice_config::StartMaximized`;
  `editor.config` (`Arc<ConfigRegistry>`, `editor.rs:847`); `editor.paint_request`.

- [ ] **Step 1: Push `Maximize` after config load.** In `GpuiApp::new`, after
  `editor.load_persistent_config(...)` and `apply_per_language_toml_overrides()`
  / `rebuild_option_cache()` (so the value is resolved), before
  `spawn_editor_actor(editor)` (~line 431):

```rust
// ui.window.start-maximized: enqueue a one-shot maximize now that config is
// loaded. Drained on the UI thread by EditorView::render (W.5). Config loads
// AFTER the Startup publish above, so this cannot move to the publish site.
let start_maximized = editor
    .config
    .get_typed::<lattice_config::StartMaximized>()
    .map(|v| *v)
    .unwrap_or(false);
if start_maximized {
    window_commands
        .lock()
        .expect("window command queue poisoned")
        .push_back(crate::window_chrome::WindowCommand::Maximize);
    // Wake a paint so the drain runs even if no input is in flight.
    editor.paint_request.notify_one();
}
```

> `window_commands` is the local created in W.5 Step 2; ensure this block is
> placed after that local exists and before it is moved into the returned
> struct. Confirm `editor.paint_request` is the `Arc<Notify>` field
> (`lib.rs:314`) and `notify_one()` is the wake used elsewhere.

- [ ] **Step 2: Type-check**

Run: `cargo build -p lattice-ui-gpui --features window`
Expected: builds clean.

- [ ] **Step 3: Manual smoke (maximize)**:

```bash
echo 'ui.window.start-maximized = true' >> ~/.config/lattice/config.toml
cargo run --features gui -- --gui README.md
# Expect: window fills the work area on launch. Remove the line to restore.
```

- [ ] **Step 4: Commit** (fold in the fragment refinement from W.0):

```bash
git add crates/lattice-ui-gpui/src/lib.rs docs/dev/architecture/gpui-window-chrome.md
git commit -m "feat(gpui): maximize on launch via ui.window.start-maximized (W.6)"
```

---

## Slice W.7 — user docs ✅

**Files:**
- Modify: `docs/user/display.md` (new "Window chrome" section)
- Modify: `docs/user/options.md` (rows in the option tables)

- [ ] **Step 1: Add a "Window chrome (GPUI)" section to `docs/user/display.md`**
  covering both options, values, defaults, and the caveats — matching the file's
  existing prose style:
  - `ui.window.decorations` = `full` (default) | `none`. `none` = borderless
    (alacritty/kitty/emacs style). Per-platform note: Linux X11 = true borderless
    + WM-resizable; Windows = borderless + resizable; macOS = no titlebar/traffic
    lights, rounded corners remain, and **resize via an external tool** (Raycast /
    yabai / Rectangle) since there is no internal edge-resize. GPUI peer only
    (ignored in the terminal). **Applies on next launch** (no live re-toggle).
  - `ui.window.start-maximized` = `true` | `false` (default). Maximizes on
    launch (fills the work area, keeps the menu bar — not native fullscreen).
    GPUI peer only.

- [ ] **Step 2: Add option rows to `docs/user/options.md`** in the same table
  format as the existing `ui.*` entries (`ui.modeline.*`, `ui.diagnostics.*`),
  one row per option with name, type/values, default, and a one-line summary.

- [ ] **Step 3: Verify no stale claims** — grep the two files for the option
  names and confirm the text matches the shipped behavior:

Run: `rg 'ui.window' docs/user/`
Expected: only the intended new mentions.

- [ ] **Step 4: Commit**

```bash
git add docs/user/display.md docs/user/options.md
git commit -m "docs(user): document ui.window.decorations + start-maximized (W.7)"
```

---

## Self-review notes

- **Spec coverage:** decorations option (W.1/W.2/W.3/W.4), maximize option +
  queue + Startup-sequenced push (W.2/W.5/W.6), per-platform mapping (W.3),
  TUI-inert (options declared once, no GPUI dep in TUI), user docs (W.7),
  graceful handling (validator errors in W.1/W.2; empty-queue-safe drain in W.5).
  Bench: none — window config is not a hot path (design fragment §Deliverables).
- **Type consistency:** `Decorations::None_` (label `none`) used everywhere;
  `WindowDecorationsOption`/`StartMaximized` decl types; `window_chrome()` and
  `WindowCommand`/`WindowCommandQueue`/`drain_window_commands` names consistent
  across W.3/W.5/W.6.
- **Confirm-against-source flags** (called out inline, not placeholders):
  `load_default_paths` 3rd arg, `self.app` field name in `render`,
  `parse_and_set_command` test-call form. All have a verified fallback noted.
