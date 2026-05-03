//! Document synchronisation: `didOpen` / `didChange` (incremental
//! or full) / `didClose`. One [`DocSync`] is held per `ServerHandle`;
//! it shadows every buffer the server cares about with a string
//! mirror so we can translate `Position { line, byte }` to the
//! negotiated LSP encoding without re-querying the editor's rope.
//!
//! ## Lifecycle
//!
//! ```text
//!  editor                      DocSync                    server
//!  ------                      -------                    ------
//!  open file        ----->    open(uri, lang, text)
//!                              didOpen via notify   --->
//!  apply_edit Ok    ----->    record_edit(uri, edit)
//!                              [mirror updated, change queued]
//!  ...50ms idle...
//!  flush(uri)       ----->    didChange via notify  --->
//!  bdelete          ----->    close(uri)
//!                              didClose via notify   --->
//! ```
//!
//! `record_edit` does not eagerly send `didChange`. The editor
//! batches edits between flushes; one keystroke commits one edit
//! but generates one queued change event, sized down to the
//! affected range. The flush cadence is the caller's choice;
//! integration with the App's mainloop sets ~50ms idle as the
//! default (matching common LSP client conventions).
//!
//! ## Sync mode honour
//!
//! - `Incremental`: queued change events are sent verbatim.
//! - `Full`: queued events are dropped; the entire post-edit
//!   mirror text is sent as one change. Most modern servers
//!   (rust-analyzer, pyright, gopls, clangd, tsserver) advertise
//!   Incremental; Full is the LSP 3.0 fallback.
//! - `None`: didChange is a no-op. Some servers prefer pull-based
//!   diagnostics and don't want continuous text sync.
//!
//! ## Position encoding
//!
//! `record_edit` reads the negotiated encoding off the
//! `Capabilities` snapshot embedded in the `ServerHandle`. utf-8
//! is a fast path (no conversion); utf-16 walks the affected
//! line text. See `crate::position` for the converters.
//!
//! ## Mirror cost
//!
//! One `String` per attached buffer per server. For a typical
//! editor session with a handful of open files this is a few MB
//! at worst. Not a `ropey::Rope` because the LSP layer doesn't
//! benefit from `O(log n)` edits -- we only ever splice one
//! contiguous region per edit, and indexing into a String by
//! line is `O(n)` but rare (only the affected lines of the
//! edit).

use std::collections::HashMap;
use std::str::FromStr;

use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    PositionEncodingKind, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentSyncKind, Uri, VersionedTextDocumentIdentifier,
};
use lsp_types::Range as LspRange;
use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::position::{Position, Range};

use crate::actor::ServerHandle;
use crate::error::{LspError, LspResult};
use crate::position::byte_to_lsp_character;

/// Per-document mirror state, keyed by URI inside [`DocSync`].
///
/// Holds a String mirror so the converter has access to BEFORE-
/// state line text without recomputing from the editor's rope on
/// every edit.
struct DocState {
    /// LSP `languageId` from `didOpen`. Held for re-emitting on a
    /// language-server restart (4.1.b crash recovery hooks): we
    /// re-issue didOpen with the same language id for every doc
    /// the supervisor was tracking.
    #[allow(dead_code)]
    language_id: String,
    /// LSP `version`. Bumped on every committed edit. LSP requires
    /// monotonic increase; we start at 1 to match the convention
    /// most servers expect (initial didOpen = 1).
    version: i32,
    /// Mirrors the editor's buffer text. Kept in sync by every
    /// `record_edit`. For Full sync mode this becomes the full
    /// payload of `didChange`.
    text: String,
    /// Queued change events. Cleared by `flush`.
    pending: Vec<TextDocumentContentChangeEvent>,
}

/// Document-sync facade for one [`ServerHandle`].
///
/// All methods are synchronous from the caller's perspective;
/// the underlying `notify` calls go through the actor's mailbox
/// and resolve when the actor accepts them.
pub struct DocSync {
    server: ServerHandle,
    docs: HashMap<Uri, DocState>,
}

impl DocSync {
    /// Construct a `DocSync` bound to a server.
    pub fn new(server: ServerHandle) -> Self {
        Self {
            server,
            docs: HashMap::new(),
        }
    }

    /// Open a buffer with `language_id` and initial `text`. Emits
    /// `textDocument/didOpen` immediately. Returns
    /// [`LspError::ActorGone`] if the server actor has shut down.
    pub fn open(
        &mut self,
        uri: Uri,
        language_id: impl Into<String>,
        text: impl Into<String>,
    ) -> LspResult<()> {
        let language_id = language_id.into();
        let text = text.into();
        let version: i32 = 1;
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.clone(),
                language_id: language_id.clone(),
                version,
                text: text.clone(),
            },
        };
        self.server.notify("textDocument/didOpen", params)?;

        self.docs.insert(
            uri,
            DocState {
                language_id,
                version,
                text,
                pending: Vec::new(),
            },
        );
        Ok(())
    }

    /// Record one committed edit. Updates the mirror, builds the
    /// LSP change event for queued send, and bumps the version.
    /// Does NOT send `didChange` -- the caller flushes when ready
    /// (debounced or eager).
    pub fn record_edit(&mut self, uri: &Uri, edit: &Edit) -> LspResult<()> {
        let encoding = self.server.capabilities().position_encoding.clone();
        let state = self
            .docs
            .get_mut(uri)
            .ok_or_else(|| LspError::HandshakeFailed(format!("doc not open: {}", uri.as_str())))?;

        // Translate the edit's lattice range into LSP coordinates
        // against the BEFORE-state mirror.
        let lsp_range = lattice_range_to_lsp(&state.text, edit.range, &encoding);
        let new_text = match &edit.kind {
            EditKind::Replace { text } => text.as_str(),
        };

        // Apply to mirror. We need the byte offset of the range
        // inside the FULL text (not just within the line). Walk
        // line starts to find it.
        let (start_byte, end_byte) = byte_range_in_full_text(&state.text, edit.range);
        // Defensive bound: an edit whose range falls outside the
        // mirror's current text would corrupt the mirror. Skip
        // it and surface a clear error.
        if start_byte > state.text.len() || end_byte > state.text.len() || start_byte > end_byte {
            return Err(LspError::HandshakeFailed(format!(
                "edit range {:?} out of mirror bounds (len {})",
                edit.range,
                state.text.len()
            )));
        }
        state
            .text
            .replace_range(start_byte..end_byte, new_text);
        state.version = state.version.saturating_add(1);

        // Queue the change event.
        state.pending.push(TextDocumentContentChangeEvent {
            range: Some(lsp_range),
            range_length: None,
            text: new_text.to_string(),
        });
        Ok(())
    }

    /// Send queued change events as one `didChange`. No-op when
    /// the queue is empty. Honours the negotiated sync mode:
    ///
    /// - `Incremental`: send queued events verbatim.
    /// - `Full`: send the entire mirror text as one change with
    ///   no range (LSP convention for full sync).
    /// - `None`: skip the wire entirely; just clear the queue.
    pub fn flush(&mut self, uri: &Uri) -> LspResult<()> {
        let kind = self
            .server
            .capabilities()
            .text_document_sync_kind()
            .unwrap_or(TextDocumentSyncKind::FULL);
        let state = self
            .docs
            .get_mut(uri)
            .ok_or_else(|| LspError::HandshakeFailed(format!("doc not open: {}", uri.as_str())))?;

        if state.pending.is_empty() {
            return Ok(());
        }

        let changes: Vec<TextDocumentContentChangeEvent> = match kind {
            TextDocumentSyncKind::INCREMENTAL => std::mem::take(&mut state.pending),
            TextDocumentSyncKind::FULL => {
                state.pending.clear();
                vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: state.text.clone(),
                }]
            }
            // None or unknown: clear pending, send nothing.
            _ => {
                state.pending.clear();
                return Ok(());
            }
        };

        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.clone(),
                version: state.version,
            },
            content_changes: changes,
        };
        self.server.notify("textDocument/didChange", params)?;
        Ok(())
    }

    /// Flush every open doc's pending changes. Used during
    /// editor shutdown so the server sees a coherent final
    /// state before `didClose`.
    pub fn flush_all(&mut self) -> LspResult<()> {
        let uris: Vec<Uri> = self.docs.keys().cloned().collect();
        for uri in uris {
            self.flush(&uri)?;
        }
        Ok(())
    }

    /// Send `didClose` and drop the mirror. Pending changes are
    /// flushed first.
    pub fn close(&mut self, uri: &Uri) -> LspResult<()> {
        // Flush any pending changes so the server's last view of
        // the doc matches what we observed before `didClose`.
        // Errors are swallowed -- the close itself must still
        // happen even if the flush fails (server might be in a
        // weird state but we want it to know we closed).
        let _ = self.flush(uri);

        if self.docs.remove(uri).is_some() {
            let params = DidCloseTextDocumentParams {
                text_document: TextDocumentIdentifier { uri: uri.clone() },
            };
            self.server.notify("textDocument/didClose", params)?;
        }
        Ok(())
    }

    /// True iff the URI is currently open under this DocSync.
    pub fn is_open(&self, uri: &Uri) -> bool {
        self.docs.contains_key(uri)
    }

    /// The version we'll attach to the next `didChange` for
    /// `uri` if no further edits land before flush. Used by
    /// tests + diagnostics gating (drop diagnostics older than
    /// our version).
    pub fn version(&self, uri: &Uri) -> Option<i32> {
        self.docs.get(uri).map(|d| d.version)
    }

    /// True iff at least one queued change exists for `uri`.
    pub fn has_pending(&self, uri: &Uri) -> bool {
        self.docs
            .get(uri)
            .map(|d| !d.pending.is_empty())
            .unwrap_or(false)
    }
}

/// Convert a lattice `Position` to an LSP `Position`, given the
/// line text from the BEFORE-state mirror.
fn lattice_position_to_lsp(
    line_text: &str,
    pos: Position,
    encoding: &PositionEncodingKind,
) -> lsp_types::Position {
    lsp_types::Position {
        line: pos.line,
        character: byte_to_lsp_character(line_text, pos.byte, encoding),
    }
}

/// Convert a lattice `Range` to an LSP `Range`. Reads the start
/// line and end line from the mirror to handle multi-byte
/// characters when utf-16 is the negotiated encoding.
fn lattice_range_to_lsp(
    full_text: &str,
    range: Range,
    encoding: &PositionEncodingKind,
) -> LspRange {
    let start_line = nth_line(full_text, range.start.line);
    let end_line = if range.start.line == range.end.line {
        start_line
    } else {
        nth_line(full_text, range.end.line)
    };
    LspRange {
        start: lattice_position_to_lsp(start_line, range.start, encoding),
        end: lattice_position_to_lsp(end_line, range.end, encoding),
    }
}

/// Borrow the `n`th line of `text` (0-based), without the
/// trailing newline. Returns `""` if `n` is past the last line.
fn nth_line(text: &str, n: u32) -> &str {
    let mut current_line: u32 = 0;
    let mut start = 0usize;
    for (i, b) in text.bytes().enumerate() {
        if current_line == n {
            // Walk to the end of this line.
            let mut end = i + 1;
            while end <= text.len() {
                if end == text.len() || text.as_bytes()[end - 1] == b'\n' {
                    break;
                }
                end += 1;
            }
            // start..end now spans this line including the
            // trailing '\n' (if any). Strip it.
            let mut line_end = end.min(text.len());
            while line_end > start && (text.as_bytes()[line_end - 1] == b'\n') {
                line_end -= 1;
            }
            // Strip trailing '\r' for CRLF lines.
            while line_end > start && text.as_bytes()[line_end - 1] == b'\r' {
                line_end -= 1;
            }
            return &text[start..line_end];
        }
        if b == b'\n' {
            current_line += 1;
            start = i + 1;
        }
    }
    if current_line == n {
        // Last line, no trailing newline.
        let mut line_end = text.len();
        while line_end > start && text.as_bytes()[line_end - 1] == b'\r' {
            line_end -= 1;
        }
        return &text[start..line_end];
    }
    ""
}

/// Compute the (start_byte, end_byte) of `range` inside `text`,
/// where range is in (line, byte_within_line) units.
fn byte_range_in_full_text(text: &str, range: Range) -> (usize, usize) {
    let start = line_byte_to_text_byte(text, range.start);
    let end = line_byte_to_text_byte(text, range.end);
    (start, end)
}

fn line_byte_to_text_byte(text: &str, pos: Position) -> usize {
    let mut current_line: u32 = 0;
    for (i, b) in text.bytes().enumerate() {
        if current_line == pos.line {
            return (i + pos.byte as usize).min(text.len());
        }
        if b == b'\n' {
            current_line += 1;
        }
    }
    if current_line == pos.line {
        return (text.len()).min(text.len() + pos.byte as usize);
    }
    text.len()
}

/// Helper for callers: parse a filesystem path into an LSP `Uri`.
/// Re-exported here so consumers don't need to import the
/// internal `actor::uri_from_path` helper.
pub fn uri_from_str(s: &str) -> LspResult<Uri> {
    Uri::from_str(s).map_err(|e| {
        LspError::HandshakeFailed(format!("invalid URI {s:?}: {e:?}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nth_line_returns_lf_separated_segments() {
        let text = "line0\nline1\nline2";
        assert_eq!(nth_line(text, 0), "line0");
        assert_eq!(nth_line(text, 1), "line1");
        assert_eq!(nth_line(text, 2), "line2");
        assert_eq!(nth_line(text, 3), "");
    }

    #[test]
    fn nth_line_strips_crlf() {
        let text = "first\r\nsecond";
        assert_eq!(nth_line(text, 0), "first");
        assert_eq!(nth_line(text, 1), "second");
    }

    #[test]
    fn nth_line_handles_trailing_newline() {
        let text = "x\n";
        assert_eq!(nth_line(text, 0), "x");
        assert_eq!(nth_line(text, 1), "");
    }

    #[test]
    fn byte_range_translates_simple_position() {
        let text = "abc\ndef\nghi";
        let r = Range::new(Position::new(1, 0), Position::new(1, 3));
        // Line 1 starts at byte 4 ("def"), 0..3 = bytes 4..7
        assert_eq!(byte_range_in_full_text(text, r), (4, 7));
    }

    #[test]
    fn lattice_range_to_lsp_is_byte_for_byte_in_utf8_mode() {
        let text = "abc\ndéf"; // line 1 has multi-byte 'é'
        let r = Range::new(Position::new(1, 0), Position::new(1, 4));
        let lsp = lattice_range_to_lsp(text, r, &PositionEncodingKind::UTF8);
        assert_eq!(lsp.start.line, 1);
        assert_eq!(lsp.start.character, 0);
        assert_eq!(lsp.end.line, 1);
        // utf-8: character == byte; "dé" is 3 bytes, 'd' + 2-byte 'é'.
        assert_eq!(lsp.end.character, 4);
    }

    #[test]
    fn lattice_range_to_lsp_converts_to_utf16_when_negotiated() {
        let text = "abc\ndéf";
        let r = Range::new(Position::new(1, 0), Position::new(1, 4));
        let lsp = lattice_range_to_lsp(text, r, &PositionEncodingKind::UTF16);
        // utf-16: "déf" is 3 code units (d=1, é=1, f=1).
        // byte 4 = past 'f' = utf-16 column 3.
        assert_eq!(lsp.end.character, 3);
    }

    #[test]
    fn lattice_range_to_lsp_utf16_with_emoji() {
        // 😀 is 4 utf-8 bytes, 2 utf-16 code units.
        let text = "x😀y";
        // byte 5 = past '😀'
        let r = Range::new(Position::new(0, 0), Position::new(0, 5));
        let lsp = lattice_range_to_lsp(text, r, &PositionEncodingKind::UTF16);
        // utf-16: 'x' (1) + '😀' (2) = 3 code units.
        assert_eq!(lsp.start.character, 0);
        assert_eq!(lsp.end.character, 3);
    }

    #[test]
    fn lattice_range_to_lsp_handles_cross_line_range() {
        let text = "first\nsecond\nthird";
        let r = Range::new(Position::new(0, 5), Position::new(2, 0));
        let lsp = lattice_range_to_lsp(text, r, &PositionEncodingKind::UTF8);
        assert_eq!(lsp.start.line, 0);
        assert_eq!(lsp.start.character, 5);
        assert_eq!(lsp.end.line, 2);
        assert_eq!(lsp.end.character, 0);
    }
}
