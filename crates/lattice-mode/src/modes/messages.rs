//! `messages-mode` -- major mode for the editor's `*messages*`
//! audit-log buffer (design.md §5.10.6).
//!
//! Single buffer in the editor (`*messages*`). The mode's job
//! is small but architecturally important:
//!
//! - **Identity:** the buffer's major mode IS `messages-mode`,
//!   not `text-mode + read-only-mode`. Symmetric with
//!   `lsp-log-mode` for `*lsp*`. The renderer can branch on
//!   `messages-mode` if it ever needs to (today it doesn't).
//! - **Read-only contribution:** the mode contributes
//!   `ReadOnly = true` so the modal dispatcher gates user
//!   keystrokes; subsystem writes bypass via
//!   `apply_edit_batch_blocking`.
//! - **Future home for the tracing-subscriber lifecycle**
//!   (deferred to v1.1). msg-mode.1 installs the global
//!   subscriber once at App boot; making the subscriber's
//!   enable/disable mode-driven is a refinement that lives in
//!   the mode's `Guard` once it lands.
//!
//! Marker mode for v1: `type Guard = ();`, trivial
//! `on_activate`. The work the spec attributes to
//! "registers a `tracing::Subscriber` at activate time" is
//! split across the App boot path
//! (`lattice_runtime::install_messages_subscriber`) for v1
//! simplicity. The mode-driven lifecycle binding lands when
//! reload-based subscriber control is wired in v1.1.

use crate::{CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};
use lattice_cells::style::{Style, StyledSpan};
use lattice_config::OptionOverrideSet;
use lattice_core::BufferKind;

/// The width of the timestamp field `format_message_record` writes
/// (`HH:MM:SS.mmm`), and the level field that follows it.
///
/// The record format lives in `lattice-host`; these two constants are the
/// mode's half of that contract, and [`line_spans`] degrades to plain text
/// whenever a line does not match — a misformatted record stays readable
/// instead of being coloured from the middle.
const TIMESTAMP_LEN: usize = 12;
const LEVEL_LEN: usize = 5;

/// Syntax-highlight one `*messages*` line.
///
/// **The mode owns this, not a renderer.** The TUI used to compose message
/// bodies itself, behind `if is_messages_buffer`, and GPUI had no equivalent —
/// so the log was coloured in one renderer and plain in the other. A
/// kind-specific body composer in a renderer is the shape the architecture
/// rules forbid, and the TUI's own comment said so ("the deeper issue is that
/// this branch exists at all"). Emitting renderer-neutral [`StyledSpan`]s
/// through the synthetic-highlight pipeline is what makes both peers agree
/// without either of them knowing what a log line is.
///
/// The styles are the `Messages*` family, which resolves to the
/// `messages.timestamp` / `messages.error` / … theme elements that already
/// exist and that a user can already set. Folding them into the `Diagnostic*`
/// colours would have been fewer variants and would have silently orphaned
/// that vocabulary. Both renderers resolve the family through the one
/// `theme_style` map, so neither needs an arm of its own.
///
/// Returns an empty vec for a line that is not a record; the renderer then
/// paints it plain, which is what an unparseable line should look like.
pub fn line_spans(line: &str) -> Vec<StyledSpan> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let level_start = TIMESTAMP_LEN + 1;
    let body_start = level_start + LEVEL_LEN + 1;
    if line.len() < body_start.saturating_sub(1)
        || !line.is_char_boundary(TIMESTAMP_LEN)
        || line.as_bytes().get(TIMESTAMP_LEN) != Some(&b' ')
        || line.as_bytes().get(level_start + LEVEL_LEN) != Some(&b' ')
    {
        return Vec::new();
    }
    let level_style = match &line[level_start..level_start + LEVEL_LEN] {
        "ERROR" => Style::MessagesError,
        " WARN" => Style::MessagesWarn,
        " INFO" => Style::MessagesInfo,
        "DEBUG" => Style::MessagesDebug,
        "TRACE" => Style::MessagesTrace,
        // An unrecognised level means this is not one of our records after
        // all. Plain, rather than coloured from the middle.
        _ => return Vec::new(),
    };
    vec![
        StyledSpan {
            start: 0,
            end: TIMESTAMP_LEN,
            style: Style::MessagesTimestamp,
        },
        StyledSpan {
            start: level_start,
            end: level_start + LEVEL_LEN,
            style: level_style,
        },
    ]
}

/// [`line_spans`] for a whole buffer, one entry per line.
pub fn buffer_spans(text: &str) -> Vec<Vec<StyledSpan>> {
    text.lines().map(line_spans).collect()
}

/// Major mode for the `*messages*` buffer.
pub struct MessagesMode;

impl MessagesMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("messages-mode")
    }
}

impl Mode for MessagesMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    /// H.2: `*messages*` and any future `BufferKind::Messages`
    /// buffer dispatches to this major via the registry's kind
    /// index.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Messages)
    }
    /// MG.RO: `read-only-mode` is where the operator gate actually is.
    ///
    /// `ReadOnly = true` above stops Insert-mode TYPING and nothing else — it
    /// is read by `read_only_edit_rejected`, which guards the char path, while
    /// a `Document`'s grammar dispatch applies its own edits and hands the host
    /// an already-applied `Effect::Edits`. So `x` deleted a character out of
    /// this buffer while it reported itself read-only. Verified, not inferred.
    ///
    /// `read-only-mode` carries the option AND the `invocation_runner`
    /// (`Editor::run_read_only_motion`): motions move, `:` and `/` fall
    /// through, mutating operators echo instead of silently editing. Declared
    /// on the MAJOR because an implied mode is followed from the mode being
    /// activated.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: std::sync::OnceLock<Vec<ModeId>> = std::sync::OnceLock::new();
        IMPLIED.get_or_init(|| vec![crate::modes::ReadOnlyMode::mode_id()])
    }

    fn options(&self) -> OptionOverrideSet {
        // User keystrokes can't mutate `*messages*` -- the
        // subsystem owns the content. Subsystem writes route
        // through `apply_edit_batch_blocking` which bypasses
        // the dispatcher's read-only gate.
        //
        // `NoFile = true`: `*messages*` is a transcript, not an
        // on-disk file. `:q` must not warn about unsaved
        // changes; `:w` is a no-op.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod highlight_tests {
    use super::*;

    fn record(level: &str, body: &str) -> String {
        format!("12:34:56.789 {level} {body}")
    }

    /// Every level the formatter writes gets a style, and the severity ones
    /// map onto the vocabulary both renderers already resolve.
    #[test]
    fn each_level_is_styled() {
        for (level, expected) in [
            ("ERROR", Style::MessagesError),
            (" WARN", Style::MessagesWarn),
            (" INFO", Style::MessagesInfo),
            ("DEBUG", Style::MessagesDebug),
            ("TRACE", Style::MessagesTrace),
        ] {
            let spans = line_spans(&record(level, "something happened"));
            assert_eq!(spans.len(), 2, "{level}: timestamp + level");
            assert_eq!(spans[0].style, Style::MessagesTimestamp, "{level}");
            assert_eq!((spans[0].start, spans[0].end), (0, 12));
            assert_eq!(spans[1].style, expected, "{level}");
            assert_eq!((spans[1].start, spans[1].end), (13, 18), "{level}");
        }
    }

    /// The body is left unstyled — it is the message, not syntax.
    #[test]
    fn the_body_carries_no_span() {
        let spans = line_spans(&record("ERROR", "boom"));
        assert!(
            spans.iter().all(|s| s.end <= 18),
            "no span may cover the message body"
        );
    }

    /// A line that is not a record renders plain rather than coloured from
    /// the middle — a misformatted entry stays readable.
    #[test]
    fn a_non_record_line_is_left_alone() {
        for line in [
            "",
            "just some text",
            "12:34:56.789 HUH   unknown level",
            "short",
            "12:34:56.789|WARN no space where one is required",
        ] {
            assert!(
                line_spans(line).is_empty(),
                "{line:?} must not be highlighted"
            );
        }
    }

    /// A trailing newline is the buffer's, not the record's.
    #[test]
    fn a_trailing_newline_does_not_change_the_spans() {
        assert_eq!(
            line_spans(&record("ERROR", "boom")),
            line_spans(&format!("{}\n", record("ERROR", "boom")))
        );
    }

    /// Multi-byte bodies must not panic the byte-offset scan.
    #[test]
    fn a_utf8_body_is_safe() {
        let spans = line_spans(&record(" INFO", "héllo — wörld"));
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn buffer_spans_is_one_entry_per_line() {
        let text = format!("{}\n{}\n", record("ERROR", "a"), record(" INFO", "b"));
        let spans = buffer_spans(&text);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0][1].style, Style::MessagesError);
        assert_eq!(spans[1][1].style, Style::MessagesInfo);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(MessagesMode.id(), MessagesMode::mode_id());
        assert_eq!(MessagesMode::mode_id().as_str(), "messages-mode");
        assert_eq!(MessagesMode.kind(), ModeKind::Major);
    }

    #[test]
    fn contributes_read_only_and_no_file() {
        let opts = <MessagesMode as Mode>::options(&MessagesMode);
        assert_eq!(
            opts.iter().count(),
            2,
            "expected ReadOnly + NoFile contributions",
        );
    }
}
