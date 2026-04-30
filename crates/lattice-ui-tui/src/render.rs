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
use ratatui::widgets::Paragraph;

use lattice_grammar::ModalState;
use lattice_syntax::{Lang, Style, StyledSpan};

use crate::app::{App, EchoLevel};

pub fn draw_frame(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),    // buffer
            Constraint::Length(1), // mode line
            Constraint::Length(1), // command line / echo area
        ])
        .split(frame.area());

    draw_buffer(frame, chunks[0], app);
    draw_mode_line(frame, chunks[1], app);
    draw_command_or_echo(frame, chunks[2], app);
}

fn draw_buffer(frame: &mut Frame, area: Rect, app: &App) {
    let lines = compose_visible_lines(app, area.height as u32, area.width as u32);
    frame.render_widget(Paragraph::new(lines), area);

    // Place the buffer-area cursor only when not in Command modal: while
    // typing a `:` command the cursor lives in the echo / command-line row
    // (handled by draw_command_or_echo).
    if !matches!(app.modal, ModalState::Command)
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
    let gutter_w = gutter_width(total_lines);
    let buffer_w = width.saturating_sub(gutter_w);

    let mut out = Vec::with_capacity(height as usize);
    for i in 0..height {
        let line_idx = app.scroll + i;
        if (line_idx as usize) >= raw_lines.len() {
            out.push(empty_marker_line(gutter_w));
            continue;
        }
        let line_text = &raw_lines[line_idx as usize];
        let gutter = render_gutter(line_idx, gutter_w);
        let spans = app.highlights_for_viewport_row(i);
        let body = render_styled_line(line_text, spans, buffer_w);
        out.push(combine(gutter, body));
    }
    out
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
    let gutter_w = gutter_width(total_lines);
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
}
