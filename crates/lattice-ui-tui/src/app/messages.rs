//! `*messages*` buffer -- the emacs `*Messages*` analogue.
//!
//! The renderer-agnostic message stream types
//! ([`lattice_runtime::MessageRecord`],
//! [`lattice_runtime::MessagesRing`], the
//! [`lattice_runtime::MessagePushed`] typed event) live in
//! `lattice-runtime` so any host-side crate (this TUI
//! renderer, a future GPU renderer, plugins via the WIT
//! bridge, telemetry hooks) can subscribe without taking a
//! dep on `lattice-ui-tui`. This module owns only the
//! renderer-side concerns: the `:messages` ex-command opens
//! a help-style buffer rendered from the ring snapshot
//! (display preference [`BufferDisplayCategory::Messages`],
//! defaulting to `ActivePane`); the per-tick drain
//! ([`App::drain_message_events`]) rebuilds the buffer view
//! when at least one event landed.

use crate::app::App;
use crate::help::HelpContent;
use lattice_core::ui::display::BufferDisplayCategory;
use lattice_runtime::MessagesRing;

const MESSAGES_TITLE: &str = "messages";

impl App {
    /// `:messages` -- open the `*messages*` buffer. If it's
    /// already registered (a prior `:messages` left it in the
    /// registry), refreshes its content from the current ring
    /// and switches focus to it. Otherwise creates and opens.
    pub fn do_open_messages(&mut self) {
        let content = build_messages_help(&self.messages);
        self.display_buffer(content, BufferDisplayCategory::Messages);
    }

    /// Drain queued [`lattice_runtime::MessagePushed`] events; rebuild the
    /// `*messages*` buffer once per tick if any landed. Called
    /// from the runtime's per-frame tick. Coalescing matters
    /// during bursts (LSP `$/progress` floods, batch echo):
    /// the buffer rebuild is O(records) so doing it once per
    /// tick keeps the cost bounded regardless of event rate.
    pub fn drain_message_events(&mut self) {
        let Some(mut rx) = self.pending_message_event_rx.take() else {
            return;
        };
        let mut received = false;
        while rx.try_recv().is_ok() {
            received = true;
        }
        self.pending_message_event_rx = Some(rx);
        if !received {
            return;
        }
        let Some(id) = self.buffers.help_with_title(MESSAGES_TITLE) else {
            return;
        };
        let new_buf = build_messages_help(&self.messages);
        self.replace_help_buffer_preserving_cursor(id, new_buf);
    }
}

/// Build a help-style view of the messages ring. Latest at
/// the bottom (emacs's `*Messages*` convention -- new lines
/// append, the cursor naturally trails); a one-line header
/// names the buffer + records-vs-capacity bookkeeping. Empty
/// rings show a hint instead of just whitespace.
pub(crate) fn build_messages_help(ring: &MessagesRing) -> HelpContent {
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!(
        "# *messages* ({} of {} records)",
        ring.len(),
        ring.capacity(),
    ));
    lines.push(String::new());
    if ring.is_empty() {
        lines.push(
            "(no messages yet -- every `:echo` / minibuffer notification \
             lands here)"
                .to_string(),
        );
    } else {
        for r in ring.records().iter() {
            lines.push(format_message_record(r));
        }
    }
    HelpContent::from_lines(MESSAGES_TITLE, lines)
}

/// Render one record as `HH:MM:SS.mmm <level> <text>`. The
/// level prefix lets the reader scan for warns / errors at a
/// glance; the timestamp anchors the entry to wall-clock so
/// users can correlate with logs / external tools.
fn format_message_record(r: &lattice_runtime::MessageRecord) -> String {
    let elapsed = r
        .timestamp
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .ok();
    let secs = elapsed.map(|d| d.as_secs()).unwrap_or(0);
    let ms = elapsed.map(|d| d.subsec_millis()).unwrap_or(0);
    let hh = (secs / 3600) % 24;
    let mm = (secs / 60) % 60;
    let ss = secs % 60;
    let level = match r.level {
        lattice_grammar::EchoLevel::Info => " INFO",
        lattice_grammar::EchoLevel::Warn => " WARN",
        lattice_grammar::EchoLevel::Error => "ERROR",
    };
    let text = one_line(&r.text);
    format!("{hh:02}:{mm:02}:{ss:02}.{ms:03} {level} {text}")
}

/// Collapse internal newlines so multi-line echo bodies (rare
/// but possible -- e.g. a multi-line server error) still
/// render as a single transcript row. Mirrors the formatting
/// the LSP log buffer uses.
fn one_line(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut last_was_space = false;
    for c in s.chars() {
        if c == '\n' || c == '\r' || c == '\t' {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
        } else {
            out.push(c);
            last_was_space = c == ' ';
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::EchoLevel;
    use lattice_grammar::EchoLevel as WireLevel;
    use lattice_runtime::MessageRecord;

    #[test]
    fn build_messages_help_empty_renders_hint() {
        let ring = MessagesRing::with_capacity(10);
        let content = build_messages_help(&ring);
        assert_eq!(content.buffer.title, MESSAGES_TITLE);
        let body = content.buffer.content.as_string();
        assert!(body.contains("(no messages yet"));
    }

    #[test]
    fn build_messages_help_renders_records_in_order() {
        let mut ring = MessagesRing::with_capacity(10);
        ring.push(MessageRecord {
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            level: WireLevel::Info,
            text: "hello".into(),
        });
        ring.push(MessageRecord {
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            level: WireLevel::Warn,
            text: "watch out".into(),
        });
        let content = build_messages_help(&ring);
        let body = content.buffer.content.as_string();
        let hello_at = body.find("hello").expect("hello in body");
        let warn_at = body.find("watch out").expect("warn in body");
        assert!(hello_at < warn_at, "records render in arrival order");
        assert!(body.contains(" INFO "));
        assert!(body.contains(" WARN "));
    }

    /// End-to-end check that `set_message` streams every echo
    /// over the event bus. Mirrors how a plugin (or any other
    /// renderer-agnostic subscriber) taps in: subscribe a
    /// channel via `event_bus.subscribe_typed::<MessagePushed>`,
    /// drive `set_message`, assert every record landed on
    /// the subscriber's queue in arrival order with wire-
    /// typed levels. The renderer's own buffer view is just
    /// another peer subscriber.
    #[test]
    fn message_pushed_event_streams_to_external_subscriber() {
        use crate::app::test_helpers::app_with;
        use lattice_runtime::MessagePushed;
        let mut app = app_with("hi\n", 5);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<MessagePushed>();
        app.event_bus.subscribe_typed(tx);
        app.set_message(EchoLevel::Info, "first");
        app.set_message(EchoLevel::Warn, "second");
        app.set_message(EchoLevel::Error, "third");
        let mut got: Vec<(WireLevel, String)> = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            got.push((ev.record.level, ev.record.text));
        }
        assert_eq!(
            got,
            vec![
                (WireLevel::Info, "first".to_string()),
                (WireLevel::Warn, "second".to_string()),
                (WireLevel::Error, "third".to_string()),
            ],
        );
    }

    /// Live-tail: when the `*messages*` buffer is open, the
    /// drain rebuilds its content from the ring on each
    /// tick. Three echoes + one drain produce a buffer body
    /// containing every record.
    #[test]
    fn drain_message_events_refreshes_open_messages_buffer() {
        use crate::app::test_helpers::app_with;
        let mut app = app_with("hi\n", 5);
        app.do_open_messages();
        let buffer_id = app
            .buffers
            .help_with_title(MESSAGES_TITLE)
            .expect("messages buffer registered");
        app.set_message(EchoLevel::Info, "alpha");
        app.set_message(EchoLevel::Warn, "bravo");
        app.drain_message_events();
        let body = app
            .buffers
            .help(buffer_id)
            .expect("messages help buffer")
            .content
            .as_string();
        assert!(body.contains("alpha"));
        assert!(body.contains("bravo"));
    }

    #[test]
    fn format_message_record_collapses_internal_newlines() {
        let r = MessageRecord {
            timestamp: std::time::SystemTime::UNIX_EPOCH,
            level: WireLevel::Error,
            text: "line one\nline two".into(),
        };
        let s = format_message_record(&r);
        assert!(!s.contains('\n'), "newlines collapsed");
        assert!(s.contains("line one line two"));
    }
}
