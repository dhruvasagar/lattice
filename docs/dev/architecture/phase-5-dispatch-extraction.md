# Phase 5.5 — Dispatch extraction (`App::apply` → `Editor::dispatch`)

Anchor: [`phase-5-extraction.md`](phase-5-extraction.md) (Phase 5 master plan), [`phase-5b-app-design.md`](phase-5b-app-design.md) (Option-E composition that this slice builds on), DESIGN.md §3 (three-layer architecture), §5.2 (modal engine), §5.7 (async runtime).

## Why this slice exists

Phase 5.4 closed with every renderer-neutral *input* path living in `lattice-host`: the keymap catalogs, the unified `translate(ctx, chord) -> Action`, the chord types, the dispatch logic from chord to action. The renderer crate's job, on the input side, has shrunk to a ~30-line crossterm → `KeyChord` shim.

The *output* side — `Action → state mutation` — is still bound to the TUI's `App` struct. `lattice-ui-tui::app::dispatch::apply` is 2,625 LoC of `match action { ... }` arms that mutate `self.editor.*` and call helper methods on `App`. The helpers themselves (`clamp_cursor_to_buffer`, `ensure_cursor_visible`, `maybe_reparse_syntax`, `dismiss_popup`, the dozens of `do_*` ex-command effects) are also on `App` but their bodies mutate `editor` state, not renderer state.

A GPUI renderer cannot reuse any of this without depending on `lattice-ui-tui`, which would pull in ratatui and crossterm as transitive dependencies. The choices for `lattice-ui-gpui` today are: (a) depend on ui-tui — wrong; (b) duplicate ~thousands of lines of dispatch logic — long-term debt explosion; (c) reach into `Editor` directly and re-orchestrate from scratch — re-invents the wheel.

**Slice 5.5 fixes this by relocating `apply` to `Editor::dispatch` in `lattice-host`.** After this slice the renderer's `App` collapses to "compose `Editor` + renderer-specific caches, drive an event loop, paint." Both `lattice-ui-tui` and the upcoming `lattice-ui-gpui` reach the same `editor.dispatch(action)` entry point.

This is the largest single architectural unlock left in Phase 5. Without it, parallel GPUI work is impossible; with it, GPUI starts from a renderer-neutral substrate that already does dispatch, undo/redo, LSP coordination, mode lifecycle, search, completion, and every ex-command.

## What's actually there today

`App::apply(action: Action)` (`crates/lattice-ui-tui/src/app/dispatch.rs:168`) is the single entry point every keystroke / event / replayed macro flows through. Its structure:

```rust
pub fn apply(&mut self, action: Action) {
	// 1. Snapshot pre-dispatch state for hover-popup auto-dismiss heuristics.
	let pre_active = self.editor.active_buffer;
	let pre_cursor = self.editor.cursor;
	let popup_in_state_a = /* ... reads self.editor.popup_buffer + active_modes ... */;

	// 2. Macro recording capture (host state mutation).
	if let Some(rec) = self.editor.macro_recording.as_mut()
		&& !matches!(action, /* recording-management actions */)
	{
		rec.actions.push(action.clone());
	}

	// 3. Partial-chord lifecycle (host state mutation).
	if !matches!(action, Action::AbsorbPartialChord(_) | Action::PushDigit(_)) {
		self.editor.partial_chord.clear();
	}

	// 4. Read-only-help guard (host state read + echo).
	if matches!(self.editor.active_buffer, BufferKind::Help) && action_is_document_mutation(&action) {
		self.set_message(EchoLevel::Info, "buffer is read-only".to_string());
		self.ensure_cursor_visible();
		self.maybe_reparse_syntax();
		return;
	}

	// 5. The big match — one arm per Action variant.
	match action {
		Action::None => {}
		Action::Quit => {
			self.editor.event_bus.publish(Event::BeforeQuit);
			self.editor.should_quit = true;
		}
		Action::Invoke(inv) => self.run_invocation(inv),
		Action::Insert(s) => self.do_insert_text(&s),
		Action::DeleteCharBackward => self.do_delete_char_backward(),
		Action::EnterMode(state) => self.enter_mode(state),
		Action::OpenLineBelow => self.do_open_line_below(),
		Action::Undo => { let _ = self.undo_blocking(); self.clamp_cursor_to_buffer(); }
		// ... ~150 more arms ...
	}

	// 6. Post-dispatch invariants (host state mutations).
	self.ensure_cursor_visible();
	self.maybe_reparse_syntax();
	if popup_in_state_a && self.editor.cursor != pre_cursor { self.dismiss_popup(); }
	// ...
}
```

`Effect` *already* exists in `lattice_grammar` and `apply_effect` (line 1078, 135 LoC) handles ~60 variants flowing back from `editor.document.dispatch_with_cancel(invocation)`. These are renderer-neutral by construction — grammar is in `lattice-grammar`, host neither — but their handlers on `App` aren't yet.

The helper methods called from `apply` and `apply_effect`:

| Helper | File | What it does | Renderer-coupled? |
|---|---|---|---|
| `clamp_cursor_to_buffer` | `app/motions.rs:306` | Reads `active_text()`, mutates `editor.cursor` | No — pure editor state |
| `ensure_cursor_visible` | `app/motions.rs:325` | Mutates `editor.scroll` based on `editor.cursor`, `editor.viewport_height` | No — pure editor state |
| `maybe_reparse_syntax` | `app/syntax.rs:32` | Triggers async reparse on `editor.document.text_version()` change | No — pure host (calls `syntax.request_reparse`) |
| `set_message(level, text)` | `app.rs:964` | Writes `editor.last_message` | No — pure host |
| `dismiss_popup` | `app/popup.rs:451` | Removes popup buffer from registry, clears `editor.popup_buffer` | No — pure host |
| `recompute_folds` | `app/lifecycle.rs` | Mutates `editor.folds` | No — pure host |
| `refresh_completion_popup` | `app/cmdline.rs:343` | Re-runs filter against `editor.completion_state` | No — pure host |
| `do_write` / `do_edit` / `do_quit` | various | File I/O + buffer-registry mutation | No — pure host (the I/O is host's responsibility) |
| `do_lsp_*` | `app/lsp.rs` | Sends LSP requests via `editor.lsp.*` | No — pure host |
| `do_open_hover` / `do_open_help_topic` | `app/popup.rs`, `app/help.rs` | Construct help buffers, register them | No — pure host |
| `do_open_file_tree` / `do_open_oil` | `app/file_tree.rs`, `app/oil.rs` | Mode lifecycle + buffer registration | No — pure host |
| `refresh_highlights` / `refresh_pane_highlights` | `app/highlights.rs` | **Mutates the VisibleHighlights cache for paint** | **Yes — render-coupled** |

The render-coupled `refresh_highlights` family is explicitly flagged in the `app/syntax.rs` docstring as "stays in app.rs (deferred); moves with a render-coupled slice." That's the right call — it's a per-frame cache feeding the paint loop, not a state mutation. It stays in the renderer crate; `dispatch` will not call into it.

The hover-popup State-A / State-B logic (snapshot pre-cursor, dismiss after if cursor moved) reads `editor.active_buffer`, `editor.popup_buffer`, `editor.active_modes`, `editor.cursor` — all host. It mutates `editor.popup_buffer` and `editor.active_modes` via `dismiss_popup` — also host. **It's renderer-neutral logic.**

## Conclusion from the audit

`apply`'s ~150 match arms, the ~60-variant `apply_effect`, and the helper methods they call are *almost entirely* renderer-neutral state mutation. The renderer-coupled work is:

1. **The visible-highlights cache** (`refresh_highlights` and friends) — stays in renderer crate.
2. **Per-frame paint** — stays in renderer crate (already is).
3. Possibly a small set of terminal/window-side concerns (cursor shape, window title) that are currently *implicit* — the TUI's runtime loop just always repaints and crossterm carries cursor-shape via the rendered frame.

That last point matters: today the TUI relies on "the runtime loop repaints every tick, so dispatch doesn't need to signal repaints." GPUI may want explicit "I changed state, schedule a paint" signals to integrate with GPUI's frame model. **That's the one shape an `Effect`-style return-value enum buys us.**

## Proposed design

### Where things land

```
lattice-host:
	pub struct Editor { /* unchanged composition root */ }

	impl Editor {
		// New: every Action goes through here. Returns the
		// renderer-side effects the caller must surface
		// (today's implicit "always repaint" becomes explicit).
		pub fn dispatch(&mut self, action: Action) -> DispatchOutcome;
	}

	// All the `do_*` helpers move to `impl Editor` or to
	// `lattice-host::action_handlers` (separate module for
	// orgnisation; the impl block is already enormous).
	pub(crate) fn handle_action(editor: &mut Editor, action: Action, out: &mut DispatchOutcome);

	// Effect handling moves too -- already grammar-typed.
	pub(crate) fn handle_effect(editor: &mut Editor, effect: lattice_grammar::Effect, out: &mut DispatchOutcome);

	pub struct DispatchOutcome {
		pub renderer_signals: Vec<RendererSignal>,
		// Possibly other return data (eg. error to surface).
	}

	pub enum RendererSignal {
		/// State changed; the renderer should refresh its
		/// per-frame caches (visible highlights, theme-derived
		/// cached styles if invalidated) and schedule a paint.
		/// TUI's runtime loop already repaints unconditionally
		/// so it can ignore this; GPUI needs it to request a
		/// paint via the platform's invalidation API.
		Repaint,
		/// `:set ui.*` triggered a theme cascade; the renderer
		/// should rebuild its theme cache from `editor.host_theme`.
		ThemeChanged,
		/// `:cd` / `:edit` changed the working dir / window
		/// title; renderers that surface titles update them.
		TitleChanged,
		/// Quit requested. Today expressed via `editor.should_quit`;
		/// keep that flag for back-compat but also emit the signal
		/// so renderers can begin shutdown without polling.
		Quit,
	}
}

lattice-ui-tui:
	impl App {
		pub fn apply(&mut self, action: Action) {
			let outcome = self.editor.dispatch(action);
			for signal in outcome.renderer_signals {
				match signal {
					RendererSignal::Repaint => { /* implicit -- runtime loop repaints */ }
					RendererSignal::ThemeChanged => self.rebuild_tui_theme(),
					RendererSignal::TitleChanged => { /* terminal title escape */ }
					RendererSignal::Quit => self.editor.should_quit = true, // already set; idempotent
				}
			}
			// Render-coupled per-frame cache refresh (stays here).
			self.refresh_highlights_if_dirty();
		}
	}

lattice-ui-gpui (5.7+):
	impl App {
		pub fn apply(&mut self, action: Action) {
			let outcome = self.editor.dispatch(action);
			for signal in outcome.renderer_signals {
				match signal {
					RendererSignal::Repaint => self.window.notify_paint(),
					RendererSignal::ThemeChanged => self.rebuild_gpui_theme(),
					RendererSignal::TitleChanged => self.window.set_title(...),
					RendererSignal::Quit => self.window.close(),
				}
			}
		}
	}
```

### `RendererSignal` scope

The audit suggests `RendererSignal` will be small — likely just the four variants above, possibly fewer. We won't speculatively design it; we'll define it from the actual call sites that turn out to need it, the same way `lattice_grammar::Effect` grew organically as ex-commands landed.

Specifically: most of the action arms today *don't* need a signal at all. The renderer just refreshes its caches and paints on every tick. The cases where a signal is non-trivially useful:

- **Theme changes** — the TUI's cached `Style` mirrors need rebuild. Today this is wired through `App::do_set` calling back into `App` to call `rebuild_tui_theme`. After 5.5 the rebuild can't run from inside `Editor::dispatch` (no renderer state available there); the signal carries the trigger.
- **Quit** — already conveyed via `editor.should_quit`. The signal is redundant for TUI but useful for renderers that want event-driven shutdown rather than per-tick polling. Optional.
- **Title** — same shape as theme.

For the v1 of `RendererSignal` I'd start with **just `ThemeChanged` and `Quit`** (the two with real call sites today). `Repaint` and `TitleChanged` are deferred until a concrete need surfaces.

### Slicing strategy

`apply` is 2,625 LoC; moving it in one commit would be unreviewable. Slice plan:

**5.5.A — Foundational scaffolding.** Define `Editor::dispatch(action) -> DispatchOutcome` as a stub that calls into a free function `lattice_host::action_handlers::handle(editor, action)`. The free function is initially empty (returns `DispatchOutcome::default()`). `App::apply` keeps its full body. Tests pass — no behaviour change. (~50 LoC.)

**5.5.B — Move pre-match host-state mutations (clean subset).** The macro-recording capture and partial-chord clear. Pure `editor` reads and writes; no helper calls. `App::apply` starts calling `editor.dispatch(action.clone())` at the top before its own match. (~30 LoC moved, structural.)

The **read-only-help guard** (originally listed in 5.5.B) moves with 5.5.D instead — its body calls `set_message` / `ensure_cursor_visible` / `maybe_reparse_syntax`, all `App` helpers that 5.5.D relocates to `Editor`. Pulling the guard forward would force either inlining those helper bodies into the host-side guard or inventing a `consumed: bool` coordination field on `DispatchOutcome`. Both vanish if the guard lands with its helpers.

**5.5.C — Move the simplest match arms first.** The audit-confirmed helper-free arms — bodies that touch only `self.editor.*` with zero `self.do_*` / `self.refresh_*` / `self.dismiss_*` calls. Inventory landed: `Action::None`, `Action::Quit` (first emission of [`RendererSignal::Quit`]), `Action::AbsorbPartialChord`, `Action::PushDigit`, `Action::Echo`, `Action::CommandLineCancel`, `Action::SelectRegister`, `Action::CommandLineDeleteChord`, `Action::CommandLineDismissCompletion`, `Action::EnterSearch`. (~80 LoC moved — smaller than the original 300-LoC estimate; the bulk of `Action::*Command*` / `Action::EnterMode` / `Action::CommandLineAppend|Backspace|Submit` etc. all call helpers and defer to 5.5.D.)

**5.5.D — Move helpers, batch 1: pure-editor mutations.** `clamp_cursor_to_buffer`, `ensure_cursor_visible`, `dismiss_popup`, `recompute_folds`, `set_message`, `enter_mode`, `do_insert_text`, `do_delete_char_backward`, the open/close-line family. These all live on `App` today but their bodies mutate `editor` only. Move to `impl Editor`. Also move the **read-only-help guard** (deferred from 5.5.B) — its post-effect helpers (`ensure_cursor_visible`, `maybe_reparse_syntax`) now live alongside it on `Editor`, so the guard becomes a clean self-contained host-side block. (~500 LoC moved.)

**5.5.E — Move helpers, batch 2: ex-command effect handlers.** The big `apply_effect` table moves to `Editor::handle_effect`. Most `do_*` functions move with it. The LSP request senders, file ops, picker openers, format requests, snippet expansion — all of `app/lsp.rs`, most of `app/cmdline.rs`. (~3,000 LoC moved across multiple sub-slices; biggest cluster of the slice.)

  - **5.5.E.1 ✅** scaffolds `Editor::handle_effect(effect: Effect) -> DispatchOutcome` next to `Editor::dispatch`, plus a private free function `handle_effect(editor, effect, out)` carrying the host-side match. Migrates the three trivially helper-free arms — `Effect::None`, `Effect::ClearSearchHighlight` (`:noh`), and `Effect::Echo { level, text }` — and relocates the `echo_level_from_grammar` translator into `lattice_host::dispatch` next to its sole caller. `App::apply_effect` clones the effect, calls `editor.handle_effect`, and the three migrated variants collapse into a grouped no-op arm (the 5.5.C seam pattern). `RendererSignal` is unchanged — none of the migrated arms emit signals. Behavioural coverage stays at the App layer (`app::search::tests::nohlsearch_clears_overlay`, `app::search::tests::substitute_no_match_emits_error`); host-side tests are limited to the type-level shape guard until `Editor::new` extraction lands.
  - **5.5.E.2 ✅** migrates the two echo-only ex-command listers: `Effect::EchoMarks` (`:marks`) wires to a new `Editor::do_list_marks`, and `Effect::EchoRegisters` (`:reg`) wires to `Editor::do_list_registers`. Both bodies were already pure `editor.*` reads + `set_message` on App; the move was mechanical. The `preview_register` free fn moves from `lattice-ui-tui::app` to `lattice_host::dispatch::preview_register` (now `pub`); the picker source's two call sites switch from `super::preview_register` to the host path. App's `apply_effect` adds `EchoMarks` and `EchoRegisters` to the grouped no-op fallthrough. The 5.5.F-coupled `do_list_buffers` / `do_describe_buffer` / `do_buffer_next` / `do_buffer_prev` / `do_delete_line` arms stay on App: each depends on a non-migrated helper (`display_buffer`, `activate_buffer`, `apply_edit_blocking`) that's the scope of its own future slice. Behavioural coverage lives in `app::tests::{list_registers_*, list_marks_*}` (four tests; all green).
  - **5.5.E.3 ✅** migrates the register-stash helper: `store_yank` moves from `lattice-ui-tui::app::edit` to `lattice_host::dispatch::Editor::store_yank`, unlocking `Effect::Yank`. The body was already pure `editor.unnamed_register` / `editor.registers` mutation. Two App-side call sites — the main `apply_effect` arm (now collapsed into the grouped no-op fallthrough) and the oil-buffer narrow `apply_effect` re-implementation (line ~700 of `app::dispatch`, which intentionally bypasses the document actor + `handle_edits`) — both reroute through `self.editor.store_yank(...)`. `read_register` stays on App (paste path, not in `apply_effect`). Behavioural coverage: 50 yank- / register-related tests across `app::tests` and `app::visual::tests` exercise the new host path.
  - 5.5.E.4+ pick up the next wave. The cleanest unblock is `Effect::SelectionChange`: requires migrating `set_selections_blocking` + `publish_selections_changed` + the small `visual_kind_to_mode` translator (all renderer-neutral by audit). After that, the `apply_edit_blocking` / `handle_edits` cluster (`Effect::Edits`, `Effect::DeleteCurrentLine`, `Effect::Substitute`, `Effect::Global`) lands once the render-coupled `shift_highlights_for_edit` cache is sorted — that's the visible-highlights slice the 5.5 design doc explicitly flags as out of scope, so this cluster may need to wait until post-5.5 work touches the highlights cache. In parallel, `display_buffer` host-side (a 5.5.F-shaped move) unlocks `Effect::ListBuffers` / `Effect::DescribeBuffer` / the describe-* family; `activate_buffer` migration unlocks `Effect::BufferNext` / `BufferPrev` / `BufferDelete`. Then come the LSP request senders, snippet ops, and finally `Effect::Many` (recursion through host) once enough inner arms have migrated. `Effect::SetOption` lands with `RendererSignal::ThemeChanged`'s first emission site; the call-site outcome plumbing through `apply_effect` upgrades from the discarded `_outcome` to a threaded `&mut DispatchOutcome` at that point.

**5.5.F — Move helpers, batch 3: mode lifecycle.** `do_open_file_tree`, `do_open_oil`, `do_open_hover`, `do_open_help_topic`, `do_open_lsp_log`. Mostly buffer-registry + mode-activation logic; pure host. (~500 LoC moved.)

**5.5.G — Move the final remnants + collapse `App::apply`.** Anything still on `App`. `apply` reduces to the dispatch call + signal handling. (~200 LoC removed from ui-tui; ~50 LoC added to ui-tui's signal handler.)

**5.5.H — Render-coupled cleanup.** Remove the now-vestigial `App` methods that just forwarded to Editor. Tighten `App`'s public surface. (~100 LoC removed.)

Each sub-slice lands green: workspace builds, 1424 ui-tui + 180 host + 180 lsp tests pass (5.5.E.1 baseline; was 1424 + 177 + 180 at the start of 5.5). The order is mechanical — by the time 5.5.G runs, `apply`'s body is mostly empty and the collapse is trivial.

### Where the tests move

Today most dispatch tests live in `lattice-ui-tui::app::dispatch::tests` because that's where `apply` lives. Per sub-slice:

- 5.5.A–D: tests stay in ui-tui. They construct `App` and call `app.apply(action)`; nothing changes semantically.
- 5.5.E+: some tests can move to `lattice-host::editor::tests` and construct `Editor` directly. Tests that need to assert renderer-side side-effects (rare) stay in ui-tui.

We won't force tests to move just because their target code moved. The integration shape (App-level) remains a valid test surface; it's just no longer the *only* place dispatch can be tested.

### `Effect` vs `RendererSignal` — the layering

Today `apply` does:

```
Action -> [apply] -> mutates editor + sometimes calls do_*
                  -> do_* may call editor.document.dispatch_with_cancel
                                  -> returns Effect
                                  -> [apply_effect] -> mutates editor / calls more do_*
```

`Effect` is grammar-emitted. It's already the renderer-neutral abstraction for "this ex-command needs to do X." It stays as-is.

`RendererSignal` is the **outer** wrapper — the host-to-renderer boundary. `dispatch(action)` returns `DispatchOutcome { renderer_signals: Vec<RendererSignal> }`. Internally during dispatch, host may emit zero or more `RendererSignal`s as `editor` state changes.

The two are at different layers:

- `Effect` (grammar) → host (`Effect` -> editor mutation) — already exists.
- `RendererSignal` (host) → renderer (`RendererSignal` -> paint/title/theme rebuild) — new with 5.5.

There's a temptation to collapse both into one enum. Resist it: they have different cardinality (one Action produces 0-1 grammar Effects but possibly multiple renderer signals across nested helpers) and different consumers (grammar Effects are processed inside host; renderer signals leave host).

## Risk analysis

- **Subtle ordering changes.** The current `apply` mixes mutations and helper calls in a specific order; `ensure_cursor_visible` runs after the match. Moving piecemeal risks a sub-slice where the order flips for one Action variant. Mitigation: each sub-slice is mechanical; the full test suite runs after each one; a behaviour difference shows up as a failing test.

- **`set_message` cycles.** Some helpers call `set_message` to surface errors. If `set_message` moves to `Editor`, the helpers that move with it work directly. If a helper stays on `App` but `set_message` moves, the helper needs to call `editor.set_message(...)`. Mitigation: move `set_message` early (5.5.D) so most call sites just work.

- **`enter_mode` is more renderer-coupled than it looks.** Mode entry can trigger keymap-overlay sync, which affects the keymap registry — but the registry is in editor (5.4 ✅). Cursor visibility recompute is host. Insert recording start/stop is host. I don't expect a renderer-side concern here, but flag it for the actual move.

- **Performance.** Adding one indirection (`app.apply` → `editor.dispatch` → handler) per keystroke is negligible — Rust inlines through it. The signal-handling loop adds at most O(signal-count) work per dispatch where signal-count is typically 0. The keystroke-to-paint benchmark (existing) is the regression net.

- **`block_on` calls inside dispatch.** `do_lsp_*` and several `do_*` blocking calls today use `block_on` on the App's runtime. The runtime handle lives on `editor.runtime` (already host) but the `block_on` call is on the helper. After move, the host helpers will `block_on` on `editor.runtime` — no semantic change.

- **Test churn.** Some tests construct `App` via `app_with(...)` and call `app.apply(...)`. After move they still work (App's `apply` delegates). Tests that introspect intermediate `App` state to verify dispatch behaviour need to migrate to introspect `Editor` state — almost all of them already do (`a.editor.cursor`, `a.editor.modal`, etc.).

## Deliverables checklist (per CLAUDE.md heuristic 5)

- **Architecture docs**: this file (focused 5.5 design) + revision to `phase-5-extraction.md` updating the roadmap. Sub-slice commits update the session log section in the master plan.
- **Benchmark coverage**: existing dispatch / keystroke benches under `crates/lattice-ui-tui/benches/` keep measuring keystroke-to-state-update latency. No new benches in 5.5 itself; performance is structural relocation, not new logic. If a measurable regression appears, the offending sub-slice gets reverted and re-sliced.
- **Test coverage**: per sub-slice, existing tests stay green. New tests for `Editor::dispatch` get added for the new public surface (one per direct dispatch invocation pattern). The `RendererSignal` enum gets a coverage test (every variant must be produced by at least one Action under realistic state).
- **Error handling**: `DispatchOutcome` can carry an `Option<Error>` for the rare dispatch path that fails (today many `do_*` functions log + suppress; the explicit error channel makes that visible to the renderer for error-toast surfacing). No silent swallows added.

## Out of scope (deferred)

- Visible-highlights cache (`refresh_highlights`) — stays in ui-tui; render-coupled by design (per-frame paint cache).
- The `lattice_host::Renderer` trait (currently unused) — leave alone in 5.5. After 5.7 GPUI scaffold lands we can revisit whether the trait should be deleted, simplified, or used.
- `lattice-render` crate — dropped from the plan (see [`phase-5-extraction.md`](phase-5-extraction.md)'s revised roadmap).
- Pane-provider lookup move — that's slice 5.6 (separate, small, mechanical).
- Runtime loop split — TUI's `runtime.rs` stays TUI-specific; GPUI ships its own. No shared `run()` driver in Phase 5.

## Acceptance criteria

- `crates/lattice-host/src/editor.rs` (or a new sibling module) exposes `pub fn dispatch(&mut self, action: Action) -> DispatchOutcome`.
- `crates/lattice-ui-tui/src/app/dispatch.rs`'s `apply` body is ≤200 LoC (the signal-handling wrapper + render-coupled cache refresh).
- All `do_*` functions that mutate only `editor` live in `lattice-host`.
- `lattice-ui-gpui` (when it lands in 5.7) can `use lattice_host::editor::Editor;` and call `editor.dispatch(action)` with **zero** `lattice-ui-tui` dependency.
- 1424 ui-tui + 177 host + 180 lsp tests green throughout the slicing.
- Phase-5 ledger row 5.5 marked ✅; `phase-5-extraction.md` session log updated.
