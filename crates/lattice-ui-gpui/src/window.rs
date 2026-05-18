//! GPUI window-opening entry point — [`run`] opens a native
//! GPUI window, constructs an [`EditorView`], drives the event
//! loop, and tears down cleanly on `editor.should_quit`.
//!
//! Phase 5.9 migration: the body used to live in
//! `src/bin/lattice_gpui.rs`. Moving it into the lib (behind
//! the `window` Cargo feature) lets `lattice-cli` route through
//! a single `lattice --gpu` flag without duplicating the
//! window setup. The `lattice-gpui` binary becomes a thin
//! shim: tracing init + CLI arg parse + [`run`].
//!
//! The whole module is `#[cfg(feature = "window")]`-gated by
//! `lib.rs`, so `cargo test -p lattice-ui-gpui` on a headless
//! host (without display libs) still works — gpui only links
//! when the `window` feature is on.
//!
//! ## Pipeline
//!
//! 1. [`run`] takes a [`Document`] (path-bearing or empty),
//!    constructs `gpui::Application::new()`, and opens a 720×480
//!    centered window whose root element is an [`EditorView`].
//! 2. [`EditorView::new`] calls `GpuiApp::new(document)`, which
//!    delegates to [`lattice_host::editor::Editor::boot`] for the
//!    full renderer-neutral setup (LSP, command + mode + snippet
//!    registries, syntax handle, buffer registry, persistent
//!    config, picker registry, ...).
//! 3. After `cx.open_window` returns, we do a second
//!    `window.update(cx, ...)` to focus the editor view + call
//!    `cx.activate(true)`. The focus call HAS to happen after
//!    `open_window` -- calling it inside the builder closure
//!    runs before gpui's focus tree is fully initialised and is
//!    silently dropped. The pattern matches gpui's
//!    `examples/input.rs`.
//! 4. Keystrokes flow `on_key_down` → `dispatch_keystroke` →
//!    `editor.dispatch` → renderer signals + next-action chain;
//!    `cx.notify()` schedules a repaint; `cx.stop_propagation()`
//!    prevents the platform default action map from claiming the
//!    chord.
//! 5. `editor.should_quit` → `cx.quit()` tears the application
//!    down.
//!
//! ## What renders today
//!
//! - **Document area**: per-character cells laid out under
//!   `flex_row` lines + `flex_col` columns on a monospace font,
//!   with a left-side line-number gutter (5.8.D). Syntax
//!   highlights (5.8.A) color each span via the Catppuccin Mocha
//!   palette when an `editor.syntax` handle exists.
//! - **Vim-style cursor**: three shapes via [`CursorShape`]
//!   (block / bar / underline) based on `editor.modal` (5.7.B.11).
//! - **Status line / minibuffer**: shows
//!   `<MODAL>   <path>[+]   L:<n>  C:<n>` (5.8.B), or the
//!   in-progress `:command` / `/pattern` line when in Command /
//!   Search modes (5.8.C).
//! - **DisplayBuffer popup** (5.7.B.10): centered overlay for
//!   `:ls`, `:describe-buffer`, etc. Esc dismisses.
//! - **Picker overlay** (5.8.E): for `:picker files`,
//!   `:picker commands`.
//! - **Insert-mode completion popup** (5.8.F): top-right
//!   anchored panel.

use anyhow::{Context as _, Result};
use gpui::{
    App, AppContext, Application, Bounds, Context, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Styled, Window, WindowBounds, WindowOptions,
    div, px, rgb, size,
};
use lattice_core::Document;
use lattice_core::ui::pane::{PaneNode, PaneState};
use lattice_grammar::ModalState;
use lattice_syntax::Style as SyntaxStyle;

use crate::{GpuiApp, GpuiTheme};

/// Map a `lattice_syntax::Style` to a Catppuccin Mocha hex
/// palette value. Phase 5.8.A: keeps the palette inline so
/// the window can color highlighted spans without depending on
/// any host-side palette plumbing yet.
fn syntax_color(style: SyntaxStyle) -> u32 {
    match style {
        SyntaxStyle::Default => 0xcdd6f4,
        SyntaxStyle::Comment | SyntaxStyle::LineComment => 0x6c7086,
        SyntaxStyle::String => 0xa6e3a1,
        SyntaxStyle::Keyword => 0xcba6f7,
        SyntaxStyle::Type => 0xf9e2af,
        SyntaxStyle::Number => 0xfab387,
        SyntaxStyle::Function => 0x89b4fa,
        SyntaxStyle::Constant => 0xfab387,
        SyntaxStyle::Variable => 0xcdd6f4,
        SyntaxStyle::Operator => 0x94e2d5,
        SyntaxStyle::Punctuation => 0x9399b2,
        SyntaxStyle::Attribute => 0xf38ba8,
        SyntaxStyle::Heading1 => 0xf38ba8,
        SyntaxStyle::Heading2 => 0xfab387,
        SyntaxStyle::Heading3 => 0xf9e2af,
        SyntaxStyle::Heading4 => 0xa6e3a1,
        SyntaxStyle::Heading5 => 0x89b4fa,
        SyntaxStyle::Heading6 => 0xcba6f7,
        SyntaxStyle::Bold => 0xeba0ac,
        SyntaxStyle::Italic => 0xf5c2e7,
        SyntaxStyle::Link => 0x89b4fa,
        SyntaxStyle::Url => 0x74c7ec,
        SyntaxStyle::MarkupRaw => 0x6c7086,
        SyntaxStyle::Markup => 0x9399b2,
    }
}

/// Walk `spans` (one entry per line) and find the `Style` that
/// covers `byte`. Spans are non-overlapping so a linear scan is
/// sufficient and matches what the TUI peer does for the same
/// lookup.
fn style_at(spans: &[lattice_syntax::StyledSpan], byte: usize) -> SyntaxStyle {
    for span in spans {
        if byte >= span.start && byte < span.end {
            return span.style;
        }
    }
    SyntaxStyle::Default
}

/// Vim-style cursor shape derived from [`ModalState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorShape {
    Block,
    Bar,
    Underline,
}

impl CursorShape {
    pub(crate) fn for_mode(modal: ModalState) -> Self {
        match modal {
            ModalState::Insert | ModalState::Command | ModalState::Search(_) => Self::Bar,
            ModalState::Replace => Self::Underline,
            ModalState::Normal | ModalState::Visual(_) | ModalState::OperatorPending => Self::Block,
        }
    }
}

/// The renderer-side composition root rendered as a GPUI
/// `Entity`. Holds the [`GpuiApp`] + a [`FocusHandle`] so the
/// window's key events actually route to our dispatcher.
struct EditorView {
    app: GpuiApp,
    focus_handle: FocusHandle,
}

impl EditorView {
    fn new(document: Document, cx: &mut Context<Self>) -> Self {
        Self {
            app: GpuiApp::new(document),
            focus_handle: cx.focus_handle(),
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        tracing::debug!(
            key = %ks.key,
            ctrl = ks.modifiers.control,
            alt = ks.modifiers.alt,
            shift = ks.modifiers.shift,
            platform = ks.modifiers.platform,
            "lattice-gpui: key down"
        );
        if self.app.popup_content.is_some()
            && (ks.key.eq_ignore_ascii_case("escape") || ks.key.eq_ignore_ascii_case("esc"))
        {
            tracing::debug!("lattice-gpui: dismissing popup overlay (Esc)");
            self.app.dismiss_popup();
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let outcome = self.app.dispatch_keystroke(
            &ks.key,
            ks.modifiers.control,
            ks.modifiers.alt,
            ks.modifiers.shift,
            ks.modifiers.platform,
        );
        tracing::debug!(
            modal = ?self.app.editor.modal,
            cursor_line = self.app.editor.cursor.line,
            cursor_byte = self.app.editor.cursor.byte,
            dispatched = outcome.is_some(),
            "lattice-gpui: post-dispatch state"
        );
        cx.stop_propagation();
        cx.notify();
        if self.app.editor.should_quit {
            tracing::info!("lattice-gpui: editor.should_quit set; closing application");
            cx.quit();
        }
    }
}

impl Focusable for EditorView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EditorView {
    /// Recursive walker over `editor.pane_tree.root()`. Each
    /// [`PaneNode::Leaf`] paints via [`Self::paint_pane`]; splits
    /// wrap children in `flex_col` (horizontal split: stacked
    /// vertically) or `flex_row` (vertical split: side by side)
    /// with a thin divider border between them. The active leaf
    /// gets the richer rendering (cursor + cached highlights);
    /// inactive leaves get a simpler text + gutter + per-pane
    /// status view.
    ///
    /// Phase 5.8.H: multi-pane visible in the GPUI peer. Single-
    /// pane case still works (the root is just `Leaf(0)`); v1
    /// inactive-pane rendering deliberately omits highlights +
    /// cursor markers — those need per-pane caches the host
    /// already populates for the TUI peer via
    /// `refresh_pane_highlights`, queued for a future slice.
    fn paint_pane_tree(&self, node: &PaneNode, theme: &GpuiTheme, active_idx: usize) -> gpui::Div {
        match node {
            PaneNode::Leaf(idx) => self.paint_pane(*idx, theme, *idx == active_idx),
            PaneNode::HorizontalSplit { top, bottom } => div()
                .flex()
                .flex_col()
                .flex_grow()
                .size_full()
                .child(
                    self.paint_pane_tree(top, theme, active_idx)
                        .flex_grow()
                        .border_b_1()
                        .border_color(rgb(theme.popup_border)),
                )
                .child(self.paint_pane_tree(bottom, theme, active_idx).flex_grow()),
            PaneNode::VerticalSplit { left, right } => div()
                .flex()
                .flex_row()
                .flex_grow()
                .size_full()
                .child(
                    self.paint_pane_tree(left, theme, active_idx)
                        .flex_grow()
                        .border_r_1()
                        .border_color(rgb(theme.popup_border)),
                )
                .child(self.paint_pane_tree(right, theme, active_idx).flex_grow()),
        }
    }

    /// Paint a single pane. Active pane uses `editor.cursor` +
    /// `editor.visible_highlights` (refreshed at the top of
    /// render); inactive panes use the stashed `PaneState::cursor`
    /// and no highlights. Each pane gets its own status line at
    /// its bottom (path + cursor coords), which keeps the visible
    /// boundary between panes legible without a hard chrome border.
    fn paint_pane(&self, pane_idx: usize, theme: &GpuiTheme, is_active: bool) -> gpui::Div {
        let editor = &self.app.editor;
        let leaves = editor.pane_tree.leaves();
        if pane_idx >= leaves.len() {
            return div().child(format!("(stale pane index {pane_idx})"));
        }
        let pane: &PaneState = &leaves[pane_idx];
        // Resolve the buffer's document handle. Inactive panes may
        // reference buffers different from `editor.document`; lookup
        // by buffer_id.
        let snapshot_opt = editor
            .buffers
            .document_handle(pane.buffer_id)
            .map(|h| h.snapshot());
        let Some(snapshot) = snapshot_opt else {
            return div()
                .p_3()
                .child(format!("(buffer {:?} unavailable)", pane.buffer_id));
        };
        let text = snapshot.text();
        let cursor = if is_active {
            editor.cursor
        } else {
            pane.cursor
        };
        let cursor_line = cursor.line as usize;
        let cursor_byte = cursor.byte as usize;
        let raw_lines: Vec<&str> = text.split('\n').collect();

        let cursor_shape = if is_active {
            Some(CursorShape::for_mode(editor.modal))
        } else {
            None
        };
        let cursor_fg = rgb(theme.cursor_foreground);
        let cursor_bg = rgb(theme.cursor_background);

        // Highlights: active pane reads the live cache. Inactive
        // panes show plain text for v1 (the host's
        // `refresh_pane_highlights` populates `pane_highlights`
        // keyed by pane index; wiring those into the GPUI peer is
        // a follow-up slice).
        let highlights: &[Vec<lattice_syntax::StyledSpan>] = if is_active {
            editor.visible_highlights.as_slice()
        } else {
            &[]
        };

        let total_lines = raw_lines.len().max(1);
        let gutter_width = total_lines.to_string().len();
        let gutter_pad_len = gutter_width + 1;
        let gutter_normal = rgb(0x9399b2); // Catppuccin overlay2.

        let style_cursor_cell = |c: &str| -> gpui::Div {
            let cell = div().child(c.to_string());
            match cursor_shape {
                Some(CursorShape::Block) => cell.bg(cursor_bg).text_color(cursor_fg),
                Some(CursorShape::Bar) => cell.border_l_2().border_color(cursor_bg),
                Some(CursorShape::Underline) => cell.border_b_2().border_color(cursor_bg),
                None => cell,
            }
        };

        let make_gutter = |line_idx: usize, is_cursor_line: bool| -> gpui::Div {
            let label = format!("{:>width$} ", line_idx + 1, width = gutter_width);
            let color = if is_cursor_line && is_active {
                cursor_bg
            } else {
                gutter_normal
            };
            div().text_color(color).child(label)
        };

        let mut rows: Vec<gpui::Div> = raw_lines
            .iter()
            .enumerate()
            .map(|(line_idx, line)| {
                let is_cursor_line = line_idx == cursor_line;
                let line_spans: &[lattice_syntax::StyledSpan] =
                    highlights.get(line_idx).map(Vec::as_slice).unwrap_or(&[]);
                let mut cells: Vec<gpui::Div> = Vec::with_capacity(line.len() + 2);
                cells.push(make_gutter(line_idx, is_cursor_line));
                cells.extend(line.char_indices().map(|(byte_idx, c)| {
                    let is_cursor = is_active && is_cursor_line && byte_idx == cursor_byte;
                    if is_cursor {
                        style_cursor_cell(&c.to_string())
                    } else {
                        let span_style = style_at(line_spans, byte_idx);
                        div()
                            .text_color(rgb(syntax_color(span_style)))
                            .child(c.to_string())
                    }
                }));
                if is_active && is_cursor_line && cursor_byte >= line.len() {
                    cells.push(style_cursor_cell(" "));
                }
                div().flex().flex_row().children(cells)
            })
            .collect();

        let cursor_past_last_line = is_active && cursor_line >= raw_lines.len();
        if cursor_past_last_line {
            let blank_gutter = div().child(" ".repeat(gutter_pad_len));
            rows.push(
                div()
                    .flex()
                    .flex_row()
                    .child(blank_gutter)
                    .child(style_cursor_cell(" ")),
            );
        }

        // Per-pane status line at the pane's bottom. Format
        // matches the TUI's per-pane status: path + cursor coords.
        // Active pane uses the cursor color for the bar so the
        // user can tell which pane has focus at a glance.
        let path_label = snapshot
            .path()
            .map(|p| {
                let display = match std::env::current_dir()
                    .ok()
                    .and_then(|cwd| p.strip_prefix(&cwd).ok().map(|s| s.to_path_buf()))
                {
                    Some(rel) => rel.display().to_string(),
                    None => p.display().to_string(),
                };
                if snapshot.dirty {
                    format!("{display} [+]")
                } else {
                    display
                }
            })
            .unwrap_or_else(|| {
                if snapshot.dirty {
                    "[scratch][+]".to_string()
                } else {
                    "[scratch]".to_string()
                }
            });
        let status_line = format!("  {path_label}   L:{}  C:{}", cursor.line + 1, cursor.byte);
        let (status_bg, status_fg) = if is_active {
            (rgb(theme.cursor_background), rgb(theme.cursor_foreground))
        } else {
            (rgb(theme.status_background), rgb(theme.status_foreground))
        };

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .overflow_hidden()
            .child(
                div()
                    .flex_grow()
                    .p_3()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .children(rows),
            )
            .child(
                div()
                    .bg(status_bg)
                    .text_color(status_fg)
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_row()
                    .child(div().child(status_line)),
            )
    }
}

impl Render for EditorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 5.8.G: refresh the host-side highlight cache before
        // reading spans. Cache-hit path is ~50ns; cache-miss path
        // walks `highlight_lines` exactly once and stores into
        // `editor.visible_highlights`. The per-frame
        // `highlight_lines` call this replaces ran ~178µs at 80
        // lines unconditionally.
        self.app.refresh_highlights();
        let modal = self.app.editor.modal;

        let modal_label = match modal {
            ModalState::Normal => "NORMAL",
            ModalState::Insert => "INSERT",
            ModalState::Visual(_) => "VISUAL",
            ModalState::OperatorPending => "PENDING",
            ModalState::Command => "COMMAND",
            ModalState::Search(_) => "SEARCH",
            ModalState::Replace => "REPLACE",
        };
        // 5.8.C / 5.8.H: bottom global row. In Command/Search
        // modes it shows the in-progress `:cmd` / `/pattern`
        // minibuffer; otherwise it shows the global modal label.
        // Per-pane path + cursor coords now live inside each
        // pane's own status line (built in `paint_pane`).
        let bottom_row: String = match modal {
            ModalState::Command => format!(":{}", self.app.editor.command_line),
            ModalState::Search(dir) => {
                let prefix = match dir {
                    lattice_grammar::SearchDirection::Forward => '/',
                    lattice_grammar::SearchDirection::Backward => '?',
                };
                let pattern = self
                    .app
                    .editor
                    .search_line
                    .as_ref()
                    .map(|s| s.pattern.as_str())
                    .unwrap_or("");
                format!("{prefix}{pattern}")
            }
            _ => format!("  {modal_label}"),
        };
        let bottom_is_minibuffer = matches!(modal, ModalState::Command | ModalState::Search(_));

        let theme = self.app.theme;
        // 5.8.H: render the pane tree. `paint_pane_tree` walks
        // `editor.pane_tree.root()` recursively; each leaf paints
        // via `paint_pane` with active/inactive style. The active
        // leaf gets the refreshed `visible_highlights` cache + a
        // visible cursor; inactive leaves show plain text + no
        // cursor (their own stashed `PaneState::cursor` is read
        // for the per-pane status coords but no visible marker is
        // painted).
        let active_idx = self.app.editor.pane_tree.active_index();
        let document_area = self
            .paint_pane_tree(self.app.editor.pane_tree.root(), &theme, active_idx)
            .flex_grow();

        let completion_overlay: Option<gpui::Div> = self
            .app
            .editor
            .insert_completion
            .as_ref()
            .filter(|ic| !ic.rendered.is_empty())
            .map(|ic| {
                let max_visible = 10usize;
                let total = ic.rendered.len();
                let window_start = ic
                    .selected
                    .saturating_sub(max_visible / 2)
                    .min(total.saturating_sub(max_visible.min(total)));
                let window_end = (window_start + max_visible).min(total);
                let visible: Vec<gpui::Div> = ic.rendered[window_start..window_end]
                    .iter()
                    .enumerate()
                    .map(|(i, cand)| {
                        let abs_idx = window_start + i;
                        let row = div().child(cand.raw.display.clone());
                        if abs_idx == ic.selected {
                            row.bg(rgb(theme.status_background))
                                .text_color(rgb(theme.status_foreground))
                        } else {
                            row
                        }
                    })
                    .collect();
                div()
                    .flex()
                    .flex_col()
                    .max_w(px(360.0))
                    .p_2()
                    .bg(rgb(theme.popup_background))
                    .text_color(rgb(theme.foreground))
                    .border_2()
                    .border_color(rgb(theme.popup_border))
                    .children(visible)
                    .child(
                        div()
                            .pt_1()
                            .text_color(rgb(theme.popup_border))
                            .child(format!(
                                " {} of {}  ·  <C-n>/<C-p> · <Tab>/<CR> accept · <Esc> cancel ",
                                if total == 0 { 0 } else { ic.selected + 1 },
                                total,
                            )),
                    )
            });

        let picker_overlay: Option<gpui::Div> = self.app.editor.picker.as_ref().map(|picker| {
            let max_visible = 20usize;
            let total = picker.candidates.len();
            let window_start = picker
                .selected
                .saturating_sub(max_visible / 2)
                .min(total.saturating_sub(max_visible.min(total)));
            let window_end = (window_start + max_visible).min(total);
            let visible_candidates: Vec<gpui::Div> = picker.candidates[window_start..window_end]
                .iter()
                .enumerate()
                .map(|(i, cand)| {
                    let abs_idx = window_start + i;
                    let row = div().child(cand.raw.display.clone());
                    if abs_idx == picker.selected {
                        row.bg(rgb(theme.status_background))
                            .text_color(rgb(theme.status_foreground))
                    } else {
                        row
                    }
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .max_w(px(720.0))
                .max_h(px(440.0))
                .p_4()
                .bg(rgb(theme.popup_background))
                .text_color(rgb(theme.foreground))
                .border_2()
                .border_color(rgb(theme.popup_border))
                .child(
                    div()
                        .text_color(rgb(theme.popup_border))
                        .pb_1()
                        .child(format!(
                            " {} ({} / {}) ",
                            picker.title,
                            if total == 0 { 0 } else { picker.selected + 1 },
                            total,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .pb_2()
                        .child(div().text_color(rgb(theme.cursor_background)).child("> "))
                        .child(div().child(picker.query.clone()))
                        .child(
                            div()
                                .border_l_2()
                                .border_color(rgb(theme.cursor_background))
                                .child(" "),
                        ),
                )
                .child(div().child("───────────────".to_string()))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .children(visible_candidates),
                )
                .child(
                    div()
                        .pt_2()
                        .text_color(rgb(theme.popup_border))
                        .child("[ <C-n>/<C-p> navigate · <CR> accept · <Esc> cancel ]".to_string()),
                )
        });

        let popup_overlay: Option<gpui::Div> = self.app.popup_content.as_ref().map(|content| {
            let title = content.buffer.title.clone();
            let body_text = content.buffer.content.as_string();
            let popup_lines: Vec<gpui::Div> = body_text
                .split('\n')
                .map(|line| div().child(line.to_string()))
                .collect();
            div()
                .flex()
                .flex_col()
                .max_w(px(640.0))
                .max_h(px(400.0))
                .p_4()
                .bg(rgb(theme.popup_background))
                .text_color(rgb(theme.foreground))
                .border_2()
                .border_color(rgb(theme.popup_border))
                .child(
                    div()
                        .text_color(rgb(theme.popup_border))
                        .pb_2()
                        .child(format!(" {title} "))
                        .child(div().child("───────────────".to_string())),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .overflow_hidden()
                        .children(popup_lines),
                )
                .child(
                    div()
                        .pt_2()
                        .text_color(rgb(theme.popup_border))
                        .child("[ Esc to dismiss ]".to_string()),
                )
        });

        let mut root = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(theme.background))
            .text_color(rgb(theme.foreground))
            .text_sm()
            .font_family("DejaVu Sans Mono")
            .child(document_area)
            .child({
                let row = div()
                    .bg(rgb(theme.status_background))
                    .text_color(rgb(theme.status_foreground))
                    .px_2()
                    .py_1()
                    .flex()
                    .flex_row()
                    .child(div().child(bottom_row));
                if bottom_is_minibuffer {
                    row.child(
                        div()
                            .border_l_2()
                            .border_color(rgb(theme.cursor_background))
                            .child(" "),
                    )
                } else {
                    row
                }
            });

        if let Some(overlay) = picker_overlay {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(overlay),
            );
        }
        if let Some(overlay) = completion_overlay {
            root = root.child(div().absolute().top_8().right_4().child(overlay));
        }
        if let Some(overlay) = popup_overlay {
            root = root.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .justify_center()
                    .items_center()
                    .child(overlay),
            );
        }
        root
    }
}

/// Open a GPUI window backed by [`EditorView`] and drive the
/// event loop until the editor signals quit. Synchronous —
/// blocks the calling thread for the lifetime of the window
/// (same shape as [`lattice_ui_tui::run`] for symmetry).
///
/// Phase 5.9: the body of `fn main` from the old `lattice-gpui`
/// binary, lifted into the lib so `lattice-cli` can route
/// `lattice --gpu <file>` through this single entry. The
/// `lattice-gpui` binary keeps a thin shim that calls this.
pub fn run(document: Document) -> Result<()> {
    Application::new().run(move |cx| {
        let bounds = Bounds::centered(None, size(px(720.0), px(480.0)), cx);
        let window = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| EditorView::new(document, cx)),
        );
        let window = match window {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = ?e, "lattice-gpui: failed to open editor window");
                return;
            }
        };
        // Focus + activation has to run AFTER `open_window`
        // returns -- calling `window.focus(...)` from inside the
        // builder closure runs before gpui has fully initialised
        // the window's focus tree, and the focus call is silently
        // dropped. The `window.update(cx, ...)` re-entry hands
        // back a real window context where focus actually sticks
        // (this is the pattern gpui's own `examples/input.rs`
        // uses).
        let focus_result = window.update(cx, |view, window, cx| {
            window.focus(&view.focus_handle.clone());
            cx.activate(true);
        });
        if let Err(e) = focus_result {
            tracing::error!(error = ?e, "lattice-gpui: failed to focus editor window");
        }
    });
    Ok(())
}

/// Open a [`Document`] from `path`, with friendly error
/// context. Used by the lattice-cli `--gpu` route, where the
/// caller has already parsed the path via clap and wants a
/// hard error rather than a fallback to empty.
pub fn document_from_path(path: &std::path::Path) -> Result<Document> {
    Document::open(path).with_context(|| format!("opening {}", path.display()))
}

/// Read the first positional CLI argument and open it as a
/// [`Document`]. Used by the `lattice-gpui` shim binary —
/// fallback to empty on error so the user always gets a window.
pub fn document_from_first_arg() -> Document {
    std::env::args().nth(1).map_or_else(Document::empty, |path| {
        match Document::open(&path) {
            Ok(doc) => {
                tracing::info!("lattice-gpui: opened {path}");
                doc
            }
            Err(e) => {
                tracing::warn!(error = ?e, "lattice-gpui: failed to open {path}; using empty buffer");
                Document::empty()
            }
        }
    })
}

#[cfg(test)]
mod cursor_shape_tests {
    use super::CursorShape;
    use lattice_grammar::{ModalState, SearchDirection, VisualKind};

    #[test]
    fn normal_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Normal),
            CursorShape::Block
        );
    }

    #[test]
    fn visual_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Visual(VisualKind::Charwise)),
            CursorShape::Block
        );
        assert_eq!(
            CursorShape::for_mode(ModalState::Visual(VisualKind::Linewise)),
            CursorShape::Block
        );
        assert_eq!(
            CursorShape::for_mode(ModalState::Visual(VisualKind::Blockwise)),
            CursorShape::Block
        );
    }

    #[test]
    fn operator_pending_uses_block() {
        assert_eq!(
            CursorShape::for_mode(ModalState::OperatorPending),
            CursorShape::Block
        );
    }

    #[test]
    fn insert_uses_bar() {
        assert_eq!(CursorShape::for_mode(ModalState::Insert), CursorShape::Bar);
    }

    #[test]
    fn command_uses_bar() {
        assert_eq!(CursorShape::for_mode(ModalState::Command), CursorShape::Bar);
    }

    #[test]
    fn search_uses_bar() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Search(SearchDirection::Forward)),
            CursorShape::Bar
        );
        assert_eq!(
            CursorShape::for_mode(ModalState::Search(SearchDirection::Backward)),
            CursorShape::Bar
        );
    }

    #[test]
    fn replace_uses_underline() {
        assert_eq!(
            CursorShape::for_mode(ModalState::Replace),
            CursorShape::Underline
        );
    }
}
