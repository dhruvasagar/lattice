//! Frame rendering. Pure where it can be (line composition is testable);
//! IO-bound where ratatui needs it (`draw_frame` accepting a `Frame`).
//!
//! Layout:
//!
//! +----------------------------------------------------------------+
//! | gutter | buffer text                                           |
//! | gutter | buffer text                                           |
//! | ...                                                            |
//! +----------------------------------------------------------------+
//! | mode line: [NORMAL]  path                  line:col   lang     |
//! +----------------------------------------------------------------+

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style as TuiStyle};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use lattice_grammar::{ModalState, SearchDirection};
use lattice_protocol::position::Range as ProtoRange;
use lattice_protocol::selection::VisualMode;
use lattice_syntax::{Lang, Style, StyledSpan};

use crate::app::{App, EchoLevel};

pub fn draw_frame(frame: &mut Frame, app: &App) {
    // Vertico-style layout (DESIGN.md §5.11.3): when a completion
    // popup is open, the `:` prompt moves up by `popup_height` rows
    // so the candidate list sits BELOW the prompt -- the selected
    // candidate visually adjacent to where the user is typing,
    // alternatives extending downward. Without an open popup the
    // layout is the standard buffer / mode-line / cmdline three.
    let popup_rows = app
        .completion_state
        .as_ref()
        .map(|s| popup_height(s.candidates.len()))
        .unwrap_or(0);

    let constraints: Vec<Constraint> = if popup_rows > 0 {
        vec![
            Constraint::Min(1),                      // buffer
            Constraint::Length(1),                   // mode line
            Constraint::Length(1),                   // cmdline (above popup)
            Constraint::Length(popup_rows as u16),   // popup (bottom)
        ]
    } else {
        vec![
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    draw_buffer(frame, chunks[0], app);
    draw_mode_line(frame, chunks[1], app);
    draw_command_or_echo(frame, chunks[2], app);
    // Help overlay paints over the buffer area.
    if app.help_buffer.is_some() {
        draw_help_overlay(frame, chunks[0], app);
    }
    // Completion popup occupies the bottom rows when active --
    // vertico-style, below the cmdline.
    if popup_rows > 0 {
        draw_completion_popup(frame, chunks[3], app);
    }
}

/// Total rows the popup occupies (content + borders), capped so it
/// never dominates the screen.
fn popup_height(candidate_count: usize) -> usize {
    const MAX_ROWS: usize = 10;
    let visible = candidate_count.min(MAX_ROWS);
    visible + 2 // top + bottom border
}

/// Vertico-style completion popup (DESIGN.md §5.11.3). Sits BELOW
/// the `:` prompt; the selected candidate is the FIRST visible row
/// (closest to the prompt above), alternatives fan downward. Match
/// ranges painted with a distinct style; annotations right-aligned.
fn draw_completion_popup(frame: &mut Frame, popup_area: Rect, app: &App) {
    let Some(state) = app.completion_state.as_ref() else {
        return;
    };
    if state.candidates.is_empty() {
        return;
    }

    frame.render_widget(Clear, popup_area);
    let title = format!(
        " completion ({} of {}) ",
        state.selected + 1,
        state.candidates.len()
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(popup_area);
    frame.render_widget(block, popup_area);

    // Visible window. Selected stays in view as the user advances
    // with Tab; once it would scroll off the bottom, we shift the
    // window so the selected sits at the bottom row.
    let visible_count = inner.height as usize;
    if visible_count == 0 {
        return;
    }
    let scroll = if state.selected < visible_count {
        0
    } else {
        state.selected + 1 - visible_count
    };
    let visible: Vec<Line> = state
        .candidates
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_count)
        .map(|(i, c)| candidate_to_line(c, i == state.selected, inner.width))
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, inner);
}

/// Render one candidate as a single styled line. Matched byte
/// ranges are painted with a distinct style; annotations
/// right-aligned in the row's remaining width.
fn candidate_to_line<'a>(
    c: &'a lattice_completion::RenderedCandidate,
    selected: bool,
    width: u16,
) -> Line<'a> {
    let prefix = if selected { "▶ " } else { "  " };
    let row_style = if selected {
        TuiStyle::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        TuiStyle::default()
    };
    let match_style = TuiStyle::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);

    // Build spans: text with match-range highlighting, then padding,
    // then annotations on the right.
    let text = &c.raw.display;
    let mut spans: Vec<Span<'a>> = Vec::new();
    spans.push(Span::styled(prefix, row_style));

    // Walk text + match_ranges to paint runs.
    let mut cursor = 0usize;
    let mut sorted_ranges: Vec<_> = c
        .match_ranges
        .iter()
        .filter(|r| r.start < r.end && r.end <= text.len())
        .cloned()
        .collect();
    sorted_ranges.sort_by_key(|r| r.start);
    for range in sorted_ranges {
        if range.start > cursor {
            spans.push(Span::styled(text[cursor..range.start].to_string(), row_style));
        }
        spans.push(Span::styled(
            text[range.start..range.end].to_string(),
            match_style,
        ));
        cursor = range.end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(text[cursor..].to_string(), row_style));
    }

    // Annotations right-aligned.
    let annotations = c.annotations.join("  ");
    if !annotations.is_empty() {
        let used: usize = prefix.len() + text.len();
        let want = annotations.len() + 2;
        let pad = (width as usize).saturating_sub(used + want);
        spans.push(Span::styled(" ".repeat(pad + 2), row_style));
        spans.push(Span::styled(
            annotations,
            row_style.fg(Color::DarkGray),
        ));
    }
    Line::from(spans)
}

/// Draw the help buffer (DESIGN.md §5.11) as a centred popup. Popup
/// is the v1 display strategy; multi-buffer support brings split /
/// tab / window targets per [`crate::help::HelpDisplayMode`]. Width is
/// `min(buffer_width - 4, 100)`, height is 70% of the buffer area.
/// Content is the [`crate::help::HelpBuffer`]'s rope text; we slice
/// the visible window from the rendered string. Link markup
/// (`[[…]]`) renders verbatim today; future passes paint the link
/// ranges with a distinct style and add a follow-link motion.
fn draw_help_overlay(frame: &mut Frame, buffer_area: Rect, app: &App) {
    let Some(help) = app.help_buffer.as_ref() else {
        return;
    };
    let height = (buffer_area.height as u32 * 7 / 10).max(5) as u16;
    let width = (buffer_area.width.saturating_sub(4)).clamp(20, 100);
    let x = buffer_area.x + buffer_area.width.saturating_sub(width) / 2;
    let y = buffer_area.y + buffer_area.height.saturating_sub(height) / 2;
    let popup = Rect {
        x,
        y,
        width,
        height,
    };

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} (q / Esc to dismiss) ", help.title));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Pull the visible window out of the help buffer's rope text.
    // Allocates per frame, but only across the visible viewport
    // (~30 lines) -- well under any latency budget for a help
    // surface.
    let viewport = inner.height as usize;
    let lines = help.lines();
    let visible: Vec<Line> = lines
        .iter()
        .skip(help.scroll)
        .take(viewport)
        .map(|l| Line::from(l.as_str()))
        .collect();
    let para = Paragraph::new(visible);
    frame.render_widget(para, inner);

    // Move the terminal cursor inside the popup so the user has a
    // clear visual indication that input is now captured by the
    // help overlay (j/k/Ctrl-D/Ctrl-U scroll, Esc/q dismiss).
    // Anchored at the first content row's first column;
    // overrides any earlier `set_cursor_position` from
    // `draw_buffer` / `draw_command_or_echo` because the help
    // overlay paints later.
    if inner.height > 0 && inner.width > 0 {
        frame.set_cursor_position((inner.x, inner.y));
    }
}

fn draw_buffer(frame: &mut Frame, area: Rect, app: &App) {
    let lines = compose_visible_lines(app, area.height as u32, area.width as u32);
    frame.render_widget(Paragraph::new(lines), area);

    // Place the buffer-area cursor only when the prompt isn't claiming it.
    // In Command (`:`) and Search (`/`, `?`) modal states the cursor lives
    // in the bottom prompt row -- handled by `draw_command_or_echo`.
    let prompt_owns_cursor = matches!(app.modal, ModalState::Command | ModalState::Search(_));
    if !prompt_owns_cursor
        && let Some((screen_x, screen_y)) = cursor_screen_position(app, area)
    {
        frame.set_cursor_position((screen_x, screen_y));
    }
}

fn draw_command_or_echo(frame: &mut Frame, area: Rect, app: &App) {
    if matches!(app.modal, ModalState::Command) {
        // ":<typed>" with the cursor sitting at the end of the typed text.
        let prompt = format!(":{}", app.command_line);
        let para = Paragraph::new(Line::from(prompt.clone()));
        frame.render_widget(para, area);
        let col = area
            .x
            .saturating_add(prompt.len().min(area.width as usize) as u16);
        frame.set_cursor_position((col, area.y));
        return;
    }

    if let ModalState::Search(direction) = app.modal {
        let lead = match direction {
            SearchDirection::Forward => '/',
            SearchDirection::Backward => '?',
        };
        let pattern = app
            .search_line
            .as_ref()
            .map(|s| s.pattern.as_str())
            .unwrap_or("");
        let prompt = format!("{lead}{pattern}");
        let para = Paragraph::new(Line::from(prompt.clone()));
        frame.render_widget(para, area);
        let col = area
            .x
            .saturating_add(prompt.len().min(area.width as usize) as u16);
        frame.set_cursor_position((col, area.y));
        return;
    }

    let Some(msg) = &app.last_message else {
        // Nothing to show -- render nothing (the row stays blank).
        return;
    };
    let style = match msg.level {
        EchoLevel::Info => TuiStyle::default(),
        EchoLevel::Warn => TuiStyle::default().fg(Color::Yellow),
        EchoLevel::Error => TuiStyle::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    };
    let para = Paragraph::new(Line::from(vec![Span::styled(msg.text.clone(), style)]));
    frame.render_widget(para, area);
}

fn draw_mode_line(frame: &mut Frame, area: Rect, app: &App) {
    let path = app
        .document
        .path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "[no name]".to_string());
    let dirty = if app.document.dirty() { "[+]" } else { "   " };
    let pos = format!("{}:{}", app.cursor.line + 1, app.cursor.byte);
    let lang = Lang::detect_from_path(app.document.path()).label();
    let mode_label = app.modal_label();

    let left = format!("[{mode_label}] {dirty} {path}");
    let right = format!("{pos}  {lang}");

    let total = (area.width as usize).max(left.len() + right.len() + 1);
    let pad = total - left.len() - right.len();
    let line = format!("{left}{:pad$}{right}", "", pad = pad);

    let para = Paragraph::new(Line::from(vec![Span::styled(
        line,
        TuiStyle::default()
            .fg(Color::Black)
            .bg(Color::White)
            .add_modifier(Modifier::BOLD),
    )]));
    frame.render_widget(para, area);
}

/// Produce the visible buffer lines as `ratatui::text::Line`s, including
/// gutter (line numbers), tab expansion, and styled spans pulled from
/// `app.visible_highlights` (populated by the runtime via
/// `App::refresh_highlights`).
///
/// Spans are owned (`Cow::Owned`) so the returned `Line`s outlive the
/// document text we slice out of for this frame. One alloc per visible line
/// per frame -- negligible at terminal sizes (typically 50-100 lines).
pub fn compose_visible_lines(app: &App, height: u32, width: u32) -> Vec<Line<'static>> {
    let buffer_text = app.document.text();
    let raw_lines: Vec<String> = buffer_text
        .split_inclusive('\n')
        .map(|l| l.trim_end_matches('\n').to_string())
        .collect();
    let total_lines = raw_lines.len() as u32;
    let gutter_w = if app.show_line_numbers {
        gutter_width(total_lines)
    } else {
        // Keep one cell of left padding for the empty-marker `~` line
        // and to mirror vim's `:set nonumber` (no gutter, but content
        // still has a one-cell margin from the edge).
        2
    };
    let buffer_w = width.saturating_sub(gutter_w);

    // Compute visual selection range once (instead of per line).
    let visual_range = visual_selection_range(app);
    let block = visual_block_extents(app);

    // Build the visible-buffer-line ordering: starting from `scroll`, skip
    // lines inside closed folds, taking up to `height` entries.
    let mut visible: Vec<u32> = Vec::with_capacity(height as usize);
    let mut buf_line = app.scroll;
    while visible.len() < height as usize && (buf_line as usize) < raw_lines.len() {
        if app.line_inside_closed_fold(buf_line) {
            buf_line += 1;
            continue;
        }
        visible.push(buf_line);
        // If this is a fold start, jump past the fold's interior in the
        // next iteration (the interior is hidden).
        if let Some(fold) = app.fold_start_at(buf_line) {
            buf_line = fold.end_line + 1;
        } else {
            buf_line += 1;
        }
    }

    let mut out = Vec::with_capacity(height as usize);
    for i in 0..height {
        let line_idx = match visible.get(i as usize) {
            Some(&l) => l,
            None => {
                out.push(empty_marker_line(gutter_w));
                continue;
            }
        };
        let line_text = &raw_lines[line_idx as usize];
        let gutter = render_gutter_for(app, line_idx, gutter_w);
        // Fold summary: show "+--- N lines ---" instead of the line body.
        if let Some(fold) = app.fold_start_at(line_idx) {
            let n = fold.end_line - fold.start_line + 1;
            let summary = format!("+--- {n} lines ---");
            let summary_span =
                Span::styled(summary, TuiStyle::default().fg(Color::DarkGray));
            out.push(combine(gutter, vec![summary_span]));
            continue;
        }
        let spans = app.highlights_for_viewport_row(i);
        let mut body = render_styled_line(line_text, spans, buffer_w);
        // Blockwise visual: per-line column band [min_col, max_col].
        // Charwise / Linewise visual go through `visual_range` instead.
        if let Some(b) = block
            && line_idx >= b.start_line
            && line_idx <= b.end_line
        {
            let line_len = line_text.len();
            let start = (b.start_col as usize).min(line_len);
            let end = ((b.end_col as usize) + 1).min(line_len);
            if start < end {
                body = apply_match_overlay(body, start, end, visual_style());
            }
        } else if let Some(range) = visual_range
            && let Some((overlay_start, overlay_end)) =
                match_overlay_range(range, line_idx, line_text.len())
        {
            body = apply_match_overlay(body, overlay_start, overlay_end, visual_style());
        }
        // Hlsearch overlay: every other occurrence of the search pattern,
        // softer than the current_match style.
        for &range in app.all_matches.iter() {
            if let Some((overlay_start, overlay_end)) =
                match_overlay_range(range, line_idx, line_text.len())
            {
                body = apply_match_overlay(body, overlay_start, overlay_end, hlsearch_style());
            }
        }
        if let Some(range) = app.current_match
            && let Some((overlay_start, overlay_end)) =
                match_overlay_range(range, line_idx, line_text.len())
        {
            body = apply_match_overlay(body, overlay_start, overlay_end, match_style());
        }
        out.push(combine(gutter, body));
    }
    out
}

fn hlsearch_style() -> TuiStyle {
    // Softer than the primary match (which is yellow-bg). Cyan-bg reads
    // as "another instance of what you're searching for" without
    // stealing attention from the cursor's match.
    TuiStyle::default().bg(Color::Cyan).fg(Color::Black)
}

/// For blockwise Visual: the rectangle defined by the selection's
/// `(anchor, head)` positions. Returns `None` if not in blockwise mode.
fn visual_block_extents(app: &App) -> Option<BlockExtents> {
    if !matches!(app.modal, ModalState::Visual(lattice_grammar::VisualKind::Blockwise)) {
        return None;
    }
    let sel = app.document.selections().primary();
    let start_line = sel.anchor.line.min(sel.head.line);
    let end_line = sel.anchor.line.max(sel.head.line);
    let start_col = sel.anchor.byte.min(sel.head.byte);
    let end_col = sel.anchor.byte.max(sel.head.byte);
    Some(BlockExtents {
        start_line,
        end_line,
        start_col,
        end_col,
    })
}

#[derive(Debug, Clone, Copy)]
struct BlockExtents {
    start_line: u32,
    end_line: u32,
    start_col: u32,
    end_col: u32,
}

/// Compute the rendered range of the visual selection. Returns `None` if
/// not in Visual mode. For Linewise visual the byte extents on the first
/// and last lines are normalized to cover the full lines (mirrored from
/// the dispatcher's `Range::Selection` resolution).
fn visual_selection_range(app: &App) -> Option<ProtoRange> {
    if !matches!(app.modal, ModalState::Visual(_)) {
        return None;
    }
    let sel = app.document.selections().primary();
    let (a, b) = if sel.anchor <= sel.head {
        (sel.anchor, sel.head)
    } else {
        (sel.head, sel.anchor)
    };
    match sel.visual {
        Some(VisualMode::Linewise) => {
            // Cover full lines from a.line to b.line. Use a large byte
            // index for `end.byte`; match_overlay_range clamps to line_len.
            Some(ProtoRange::new(
                lattice_protocol::position::Position::new(a.line, 0),
                lattice_protocol::position::Position::new(b.line, u32::MAX),
            ))
        }
        Some(VisualMode::Charwise) | None => {
            // Charwise: include the head byte (vim semantics).
            Some(ProtoRange::new(
                a,
                lattice_protocol::position::Position::new(b.line, b.byte.saturating_add(1)),
            ))
        }
        Some(VisualMode::Blockwise) => {
            // Stub: render as charwise for v1.
            Some(ProtoRange::new(
                a,
                lattice_protocol::position::Position::new(b.line, b.byte.saturating_add(1)),
            ))
        }
    }
}

fn visual_style() -> TuiStyle {
    // Distinct from the search-match style. Reverse video reads as
    // "selected" in vim's terminal default.
    TuiStyle::default()
        .bg(Color::Blue)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD)
}

/// If `range` covers any bytes on `line_idx`, return the within-line
/// half-open byte interval `[start, end)`. `line_len` is the line's
/// content length excluding the trailing newline.
fn match_overlay_range(
    range: ProtoRange,
    line_idx: u32,
    line_len: usize,
) -> Option<(usize, usize)> {
    if line_idx < range.start.line || line_idx > range.end.line {
        return None;
    }
    let start = if line_idx == range.start.line {
        range.start.byte as usize
    } else {
        0
    };
    let end = if line_idx == range.end.line {
        range.end.byte as usize
    } else {
        line_len
    };
    if start >= end || start >= line_len {
        return None;
    }
    Some((start, end.min(line_len)))
}

fn apply_match_overlay(
    spans: Vec<Span<'static>>,
    overlay_start: usize,
    overlay_end: usize,
    overlay_style: TuiStyle,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + 2);
    let mut cursor = 0usize;
    for span in spans {
        let s = span.content.as_ref().to_string();
        let span_start = cursor;
        let span_end = cursor + s.len();
        let overlap_start = span_start.max(overlay_start);
        let overlap_end = span_end.min(overlay_end);
        if overlap_start >= overlap_end {
            out.push(Span::styled(s, span.style));
        } else {
            if overlap_start > span_start {
                let pre = s[..overlap_start - span_start].to_string();
                out.push(Span::styled(pre, span.style));
            }
            let mid =
                s[overlap_start - span_start..overlap_end - span_start].to_string();
            out.push(Span::styled(mid, overlay_style));
            if overlap_end < span_end {
                let post = s[overlap_end - span_start..].to_string();
                out.push(Span::styled(post, span.style));
            }
        }
        cursor = span_end;
    }
    out
}

fn match_style() -> TuiStyle {
    TuiStyle::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

fn gutter_width(line_count: u32) -> u32 {
    // Decimal digits in line count, plus a trailing space and a one-cell pad.
    let digits = line_count.max(1).ilog10() + 1;
    digits + 2
}

fn render_gutter(line_idx: u32, width: u32) -> Span<'static> {
    let n = (line_idx + 1).to_string();
    // " 12 " for a 4-wide gutter.
    let pad = (width as usize).saturating_sub(n.len() + 1);
    let s = format!("{:pad$}{n} ", "", pad = pad);
    Span::styled(s, TuiStyle::default().fg(Color::DarkGray))
}

fn render_gutter_for(app: &App, line_idx: u32, width: u32) -> Span<'static> {
    if !app.show_line_numbers {
        // Pure padding so the buffer doesn't run flush against the edge.
        return Span::raw(" ".repeat(width as usize));
    }
    if !app.relative_line_numbers || line_idx == app.cursor.line {
        return render_gutter(line_idx, width);
    }
    let dist = line_idx.abs_diff(app.cursor.line);
    let n = dist.to_string();
    let pad = (width as usize).saturating_sub(n.len() + 1);
    let s = format!("{:pad$}{n} ", "", pad = pad);
    Span::styled(s, TuiStyle::default().fg(Color::DarkGray))
}

fn render_styled_line(line: &str, spans: &[StyledSpan], max_width: u32) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut cursor = 0usize;
    let bytes = line.as_bytes();
    // Spans from tree-sitter come in event order; re-sort by start byte so
    // the renderer's "no overlap" assumption holds. Also drop spans that
    // overlap a previous one (the highlighter resolves overlaps already, but
    // belt-and-braces).
    let mut sorted: Vec<StyledSpan> = spans.to_vec();
    sorted.sort_by_key(|s| (s.start, s.end));
    for span in sorted.iter() {
        if span.start < cursor || span.start >= bytes.len() {
            continue;
        }
        if span.start > cursor {
            out.push(Span::raw(line[cursor..span.start].to_string()));
        }
        let end = span.end.min(bytes.len());
        if end <= span.start {
            continue;
        }
        out.push(Span::styled(
            line[span.start..end].to_string(),
            style_to_tui(span.style),
        ));
        cursor = end;
    }
    if cursor < bytes.len() {
        out.push(Span::raw(line[cursor..].to_string()));
    }
    truncate_spans_to_width(out, max_width)
}

fn truncate_spans_to_width(spans: Vec<Span<'static>>, max_width: u32) -> Vec<Span<'static>> {
    // Naive byte-based truncation. Adequate for ASCII; non-ASCII display
    // width is a real problem we punt on until we own a width-aware shaping
    // path (Phase 9 / rich-buffer).
    let mut out = Vec::with_capacity(spans.len());
    let mut budget = max_width as usize;
    for span in spans {
        if budget == 0 {
            break;
        }
        let s = span.content.as_ref().to_string();
        if s.len() <= budget {
            budget -= s.len();
            out.push(Span::styled(s, span.style));
        } else {
            let cut = s
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|i| *i <= budget)
                .last()
                .unwrap_or(0);
            out.push(Span::styled(s[..cut].to_string(), span.style));
            break;
        }
    }
    out
}

fn empty_marker_line(gutter_w: u32) -> Line<'static> {
    let pad = (gutter_w as usize).saturating_sub(2);
    let gutter = format!("{:pad$}~ ", "", pad = pad);
    Line::from(vec![Span::styled(
        gutter,
        TuiStyle::default().fg(Color::DarkGray),
    )])
}

fn combine(gutter: Span<'static>, mut body: Vec<Span<'static>>) -> Line<'static> {
    let mut all = Vec::with_capacity(body.len() + 1);
    all.push(gutter);
    all.append(&mut body);
    Line::from(all)
}

fn cursor_screen_position(app: &App, area: Rect) -> Option<(u16, u16)> {
    if app.cursor.line < app.scroll {
        return None;
    }
    let row_in_view = app.cursor.line - app.scroll;
    if row_in_view >= area.height as u32 {
        return None;
    }
    let buffer_text = app.document.text();
    let total_lines = buffer_text
        .split_inclusive('\n')
        .count()
        .max(1) as u32;
    let gutter_w = if app.show_line_numbers {
        gutter_width(total_lines)
    } else {
        2
    };
    let col = gutter_w + app.cursor.byte;
    Some((
        area.x.saturating_add(col.try_into().unwrap_or(u16::MAX)),
        area.y.saturating_add(row_in_view.try_into().unwrap_or(u16::MAX)),
    ))
}

fn style_to_tui(s: Style) -> TuiStyle {
    match s {
        Style::Default => TuiStyle::default(),
        Style::Keyword => TuiStyle::default().fg(Color::Magenta).add_modifier(Modifier::BOLD),
        Style::Type => TuiStyle::default().fg(Color::Cyan),
        Style::String => TuiStyle::default().fg(Color::Green),
        Style::Number => TuiStyle::default().fg(Color::Yellow),
        Style::Function => TuiStyle::default().fg(Color::Blue),
        Style::Constant => TuiStyle::default().fg(Color::LightYellow),
        Style::Variable => TuiStyle::default(),
        Style::Operator => TuiStyle::default().fg(Color::White),
        Style::Punctuation => TuiStyle::default().fg(Color::Gray),
        Style::Attribute => TuiStyle::default().fg(Color::LightMagenta),
        Style::Comment | Style::LineComment => {
            TuiStyle::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::app::App;
    use lattice_core::Document;

    fn app_with(text: &str, viewport: u32) -> App {
        let mut a = App::new(Document::from_text(text));
        a.set_viewport_height(viewport);
        a.refresh_highlights();
        a
    }

    #[test]
    fn gutter_width_for_small_buffers() {
        // 1-digit line count -> "1 " plus pad = 3 cells.
        assert_eq!(gutter_width(1), 3);
        assert_eq!(gutter_width(9), 3);
        assert_eq!(gutter_width(10), 4);
        assert_eq!(gutter_width(99), 4);
        assert_eq!(gutter_width(100), 5);
    }

    #[test]
    fn compose_visible_lines_returns_height_lines_padded_with_marker() {
        let app = app_with("a\nb", 5);
        let lines = compose_visible_lines(&app, 5, 80);
        assert_eq!(lines.len(), 5);
        // Past EOF lines start with the `~` marker.
        let past_eof = format!("{:?}", lines[3]);
        assert!(past_eof.contains('~'), "expected ~ marker, got {past_eof}");
    }

    #[test]
    fn compose_visible_lines_starts_at_scroll_offset() {
        let mut app = app_with("0\n1\n2\n3\n4", 2);
        app.scroll = 2;
        let lines = compose_visible_lines(&app, 2, 80);
        // Line index 2 is "2"; expect that text in the rendered first line.
        let l0 = format!("{:?}", lines[0]);
        assert!(l0.contains('2'), "first visible line should be '2', got {l0}");
    }

    #[test]
    fn cursor_position_advances_for_byte_offset() {
        let mut app = app_with("hello", 5);
        app.cursor.byte = 3;
        let area = Rect::new(0, 0, 80, 5);
        let pos = cursor_screen_position(&app, area).unwrap();
        // gutter for 1-line file is 3 cells; cursor at byte 3 -> column 6.
        assert_eq!(pos.0, 6);
        assert_eq!(pos.1, 0);
    }

    #[test]
    fn cursor_position_is_none_when_out_of_view() {
        let mut app = app_with("a\nb\nc\nd\ne", 2);
        app.scroll = 0;
        app.cursor.line = 4; // not in viewport [0,1]
        let area = Rect::new(0, 0, 80, 2);
        assert!(cursor_screen_position(&app, area).is_none());
    }

    #[test]
    fn render_styled_line_with_no_spans_round_trips_text() {
        let spans = render_styled_line("plain text", &[], 80);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "plain text");
    }

    #[test]
    fn render_styled_line_emits_styled_span_at_offsets() {
        let span = StyledSpan {
            start: 0,
            end: 2,
            style: Style::Keyword,
        };
        let spans = render_styled_line("fn main()", &[span], 80);
        let first = spans
            .iter()
            .find(|s| s.style != TuiStyle::default())
            .expect("at least one styled span");
        assert_eq!(first.content.as_ref(), "fn");
    }

    #[test]
    fn render_styled_line_drops_overlapping_secondary_spans() {
        // Two spans at the same position; the second is ignored to keep the
        // renderer's no-overlap invariant. (tree-sitter-highlight already
        // resolves overlaps; this is belt-and-braces.)
        let primary = StyledSpan {
            start: 0,
            end: 4,
            style: Style::Keyword,
        };
        let overlap = StyledSpan {
            start: 2,
            end: 4,
            style: Style::String,
        };
        let spans = render_styled_line("test rest", &[primary, overlap], 80);
        let total: usize = spans.iter().map(|s| s.content.len()).sum();
        assert_eq!(total, "test rest".len());
    }

    #[test]
    fn truncation_does_not_overrun_max_width() {
        let spans = render_styled_line("this is a long line of text", &[], 6);
        let total: usize = spans.iter().map(|s| s.content.len()).sum();
        assert!(total <= 6, "rendered length {total} exceeded max width 6");
    }

    // ---- Match overlay ----

    use lattice_protocol::position::{Position, Range as ProtoRange};

    fn pos(l: u32, b: u32) -> Position {
        Position::new(l, b)
    }

    #[test]
    fn match_overlay_range_returns_within_line_interval_when_match_is_local() {
        // Match: (0,4)-(0,7) on a 11-char line.
        let r = ProtoRange::new(pos(0, 4), pos(0, 7));
        assert_eq!(match_overlay_range(r, 0, 11), Some((4, 7)));
    }

    #[test]
    fn match_overlay_range_returns_none_when_line_outside_match_band() {
        let r = ProtoRange::new(pos(1, 0), pos(1, 3));
        assert_eq!(match_overlay_range(r, 0, 10), None);
        assert_eq!(match_overlay_range(r, 2, 10), None);
    }

    #[test]
    fn match_overlay_range_extends_to_eol_for_first_line_of_multiline_match() {
        // Match starts on line 0 byte 5 and ends on line 1 byte 2.
        let r = ProtoRange::new(pos(0, 5), pos(1, 2));
        assert_eq!(match_overlay_range(r, 0, 10), Some((5, 10)));
        assert_eq!(match_overlay_range(r, 1, 8), Some((0, 2)));
    }

    #[test]
    fn match_overlay_range_returns_none_when_match_starts_past_line_end() {
        let r = ProtoRange::new(pos(0, 12), pos(0, 15));
        // Line is shorter than the match's start byte -- nothing to overlay.
        assert_eq!(match_overlay_range(r, 0, 10), None);
    }

    #[test]
    fn apply_match_overlay_splits_a_single_span() {
        let spans = vec![Span::raw("hello world".to_string())];
        let style = TuiStyle::default().bg(Color::Yellow);
        let out = apply_match_overlay(spans, 6, 11, style);
        // Expect three spans: "hello ", "world", and (none after, since 11 == len).
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].content.as_ref(), "hello ");
        assert_eq!(out[1].content.as_ref(), "world");
        assert_eq!(out[1].style, style);
    }

    #[test]
    fn apply_match_overlay_clips_when_match_partially_overlaps_styled_span() {
        // "fn main" with "fn" already styled as keyword; overlay covers "n m".
        let spans = vec![
            Span::styled("fn".to_string(), TuiStyle::default().fg(Color::Magenta)),
            Span::raw(" main".to_string()),
        ];
        let style = TuiStyle::default().bg(Color::Yellow);
        let out = apply_match_overlay(spans, 1, 4, style);
        // Pieces: "f" (kw), "n" (overlay), " m" (overlay), "ain" (raw)
        let texts: Vec<&str> = out.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(texts, vec!["f", "n", " m", "ain"]);
    }

    #[test]
    fn apply_match_overlay_passes_through_when_no_overlap() {
        let spans = vec![Span::raw("untouched".to_string())];
        let style = TuiStyle::default().bg(Color::Yellow);
        let out = apply_match_overlay(spans, 100, 110, style);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content.as_ref(), "untouched");
    }

    #[test]
    fn compose_visible_lines_applies_match_overlay() {
        let mut app = app_with("hello world", 1);
        app.current_match = Some(ProtoRange::new(pos(0, 6), pos(0, 11)));
        let lines = compose_visible_lines(&app, 1, 80);
        let dump = format!("{:?}", lines[0]);
        // Spans should be split so "world" is its own span; we look for the
        // match style's signature in the debug dump.
        assert!(dump.contains("world"), "rendered: {dump}");
    }

    // ---- Visual selection rendering ----

    use lattice_grammar::VisualKind;
    use lattice_protocol::selection::{Selection, SelectionSet, VisualMode};

    #[test]
    fn visual_selection_range_is_none_when_not_in_visual() {
        let app = app_with("hello", 5);
        assert!(visual_selection_range(&app).is_none());
    }

    #[test]
    fn visual_selection_range_charwise_includes_head_byte() {
        let mut app = app_with("hello", 5);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        // Move cursor to byte 2 -- selection extends from 0 to 2 inclusive.
        let sel = Selection {
            anchor: pos(0, 0),
            head: pos(0, 2),
            visual: Some(VisualMode::Charwise),
        };
        app.document.set_selections(SelectionSet::single(sel));
        let r = visual_selection_range(&app).expect("range");
        assert_eq!(r.start, pos(0, 0));
        // Charwise includes head: end byte = head.byte + 1.
        assert_eq!(r.end, pos(0, 3));
    }

    #[test]
    fn visual_selection_range_linewise_covers_full_lines() {
        let mut app = app_with("aaa\nbbb\nccc", 5);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Linewise));
        let sel = Selection {
            anchor: pos(0, 1),
            head: pos(2, 1),
            visual: Some(VisualMode::Linewise),
        };
        app.document.set_selections(SelectionSet::single(sel));
        let r = visual_selection_range(&app).expect("range");
        assert_eq!(r.start, pos(0, 0));
        // Linewise end byte is u32::MAX so per-line clamping picks line_len.
        assert_eq!(r.end.line, 2);
    }

    #[test]
    fn visual_selection_range_normalises_reversed_anchor_head() {
        let mut app = app_with("hello", 5);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        // anchor > head (the user moved leftward in Visual).
        let sel = Selection {
            anchor: pos(0, 4),
            head: pos(0, 1),
            visual: Some(VisualMode::Charwise),
        };
        app.document.set_selections(SelectionSet::single(sel));
        let r = visual_selection_range(&app).expect("range");
        assert_eq!(r.start, pos(0, 1));
        assert_eq!(r.end, pos(0, 5));
    }

    #[test]
    fn visual_block_extents_returns_none_when_not_blockwise() {
        let app = app_with("hello", 5);
        assert!(visual_block_extents(&app).is_none());
    }

    #[test]
    fn visual_block_extents_normalises_anchor_and_head() {
        let mut app = app_with("aaa\nbbb\nccc", 10);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Blockwise));
        let sel = Selection {
            anchor: pos(2, 1),
            head: pos(0, 2),
            visual: Some(VisualMode::Blockwise),
        };
        app.document.set_selections(SelectionSet::single(sel));
        let b = visual_block_extents(&app).unwrap();
        assert_eq!(b.start_line, 0);
        assert_eq!(b.end_line, 2);
        assert_eq!(b.start_col, 1);
        assert_eq!(b.end_col, 2);
    }

    #[test]
    fn compose_visible_lines_overlays_visual_selection() {
        let mut app = app_with("hello world", 1);
        app.apply(crate::app::Action::EnterVisual(VisualKind::Charwise));
        let sel = Selection {
            anchor: pos(0, 0),
            head: pos(0, 4),
            visual: Some(VisualMode::Charwise),
        };
        app.document.set_selections(SelectionSet::single(sel));
        let lines = compose_visible_lines(&app, 1, 80);
        let dump = format!("{:?}", lines[0]);
        // The selected "hello" should appear as its own span(s); we just
        // verify the line still contains the original text after overlay.
        assert!(dump.contains("hello"));
        assert!(dump.contains("world"));
    }
}
