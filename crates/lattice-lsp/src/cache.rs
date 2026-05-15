//! Per-buffer LSP response caches + outcome enums.
//!
//! Phase 5.2: extracted from `lattice-ui-tui::app` to their proper
//! home in `lattice-lsp`. Every type here is renderer-agnostic
//! pure data describing the *result* of an LSP request -- the
//! shape the App's per-tick drain expects after the spawned task
//! delivers its response.
//!
//! Cache types are keyed on `(BufferId, document_version)` so the
//! pump can invalidate-by-version when the buffer mutates.
//! Outcome enums carry the same key plus the actual payload (or
//! a sentinel variant for `Empty` / `NoProvider` / etc.) so the
//! drain can route to the appropriate echo / apply path.
//!
//! `lattice-ui-tui::app` re-exports every type from this module
//! so existing `crate::app::HoverOutcome` etc. references in the
//! host crate's source continue to resolve unchanged.

use std::sync::Arc;

use lattice_core::{BufferId, Fold};
use lattice_protocol::position::Position;

use crate::Uri;

// ---- Selection range ---------------------------------------------------

/// 4.4.e: cached `textDocument/selectionRange` chain anchored
/// at a specific buffer + cursor position. Flat `Vec<Range>`
/// (innermost first) instead of the LSP linked-list shape so
/// the operator can index into it in O(1). Captured once on
/// the first `:lsp-expand-region` after a fresh cursor; reused
/// across subsequent expand/shrink steps until the cursor
/// moves outside `ranges[0]` (the innermost) or the buffer
/// changes.
#[derive(Debug, Clone)]
pub struct LspSelectionChain {
    pub buffer_id: BufferId,
    pub anchor_cursor: Position,
    pub ranges: Vec<lsp_types::Range>,
}

/// 4.4.e: outcome of an in-flight `textDocument/selectionRange`
/// request. The drain consumes one of these per response and
/// either seats the chain into `App::lsp_selection_chain` or
/// surfaces an error echo. `pending_step` carries whether the
/// triggering invocation was `:lsp-expand-region` or the first
/// step of `:lsp-shrink-region` (the latter is rare -- shrink
/// without an existing chain is a user error -- but the drain
/// handles it uniformly).
#[derive(Debug, Clone)]
pub enum SelectionRangeOutcome {
    /// Server returned a non-empty chain; `ranges[0]` is the
    /// innermost. The drain stores this and applies the first
    /// step at `index = 0` (then bumps for expand).
    Items {
        anchor_cursor: Position,
        anchor_buffer: BufferId,
        ranges: Vec<lsp_types::Range>,
        pending_step: SelectionRangeStep,
    },
    NoProvider,
    Empty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRangeStep {
    Expand,
    Shrink,
}

// ---- Document highlight ------------------------------------------------

/// 4.4.e: cached `textDocument/documentHighlight` result
/// anchored at a specific buffer + cursor position. The
/// renderer reads `highlights` to paint a soft overlay; the
/// pump compares `cursor` to the live cursor to decide whether
/// to invalidate and re-request.
#[derive(Debug, Clone)]
pub struct DocumentHighlightCache {
    pub buffer_id: BufferId,
    pub cursor: Position,
    pub highlights: Vec<lsp_types::DocumentHighlight>,
}

/// 4.4.e: in-flight `documentHighlight` request outcome.
#[derive(Debug, Clone)]
pub enum DocumentHighlightOutcome {
    Items {
        buffer_id: BufferId,
        cursor: Position,
        highlights: Vec<lsp_types::DocumentHighlight>,
    },
    /// Server returned null (cursor not on a known symbol);
    /// the pump clears the cache so we don't paint stale
    /// highlights.
    Empty { buffer_id: BufferId },
}

// ---- Folding range -----------------------------------------------------

/// 4.4.f: cached `textDocument/foldingRange` response for one
/// buffer. `document_version` is the buffer's
/// [`lattice_core::Document`] version at the time the request
/// was issued; the pump compares it against the live version
/// to decide whether to refresh.
#[derive(Debug, Clone)]
pub struct LspFoldsCache {
    pub document_version: u64,
    pub folds: Vec<Fold>,
}

/// 4.4.f: in-flight `foldingRange` request outcome.
#[derive(Debug, Clone)]
pub enum FoldingRangeOutcome {
    Items {
        buffer_id: BufferId,
        document_version: u64,
        folds: Vec<Fold>,
    },
    Empty {
        buffer_id: BufferId,
        document_version: u64,
    },
}

// ---- Inlay hint --------------------------------------------------------

/// 4.4.g: cached `textDocument/inlayHint` response for one
/// buffer. Keyed on `(BufferId, document_version)`; pump
/// invalidates when the version changes. `hints` are sorted
/// by position so the renderer can stop scanning once it
/// walks past the current line.
///
/// 4.4.g viewport polish: `requested_first_line` /
/// `requested_last_line` (inclusive, 0-based LSP line indices)
/// record the line range that produced this cache entry. The
/// pump refetches when the visible viewport scrolls outside
/// this window plus an overscan margin -- so small scrolls
/// stay cached, larger ones trigger a new request bounded to
/// the new viewport. Small files where viewport + overscan
/// covers the whole buffer cache exactly once and never
/// refetch on scroll.
#[derive(Debug, Clone)]
pub struct LspInlayHintCache {
    pub document_version: u64,
    pub hints: Vec<lsp_types::InlayHint>,
    pub requested_first_line: u32,
    pub requested_last_line: u32,
}

/// 4.4.g: in-flight `inlayHint` request outcome. The
/// `requested_*_line` pair rides through so the drain can seat
/// the cache with the range that actually produced these
/// hints (matters for the viewport pump -- subsequent scrolls
/// reuse the cache only while the viewport sits inside that
/// range).
#[derive(Debug, Clone)]
pub enum InlayHintOutcome {
    Items {
        buffer_id: BufferId,
        document_version: u64,
        hints: Vec<lsp_types::InlayHint>,
        requested_first_line: u32,
        requested_last_line: u32,
    },
    Empty {
        buffer_id: BufferId,
        document_version: u64,
        requested_first_line: u32,
        requested_last_line: u32,
    },
}

// ---- Document link -----------------------------------------------------

/// 4.5.c: per-buffer cache of `textDocument/documentLink`
/// responses. Filled by the pump on document-version change;
/// `gx` walks the entries looking for the first link whose
/// range covers the cursor and follows it. Cache invalidates
/// when the version changes.
#[derive(Debug, Clone)]
pub struct LspDocumentLinksCache {
    pub document_version: u64,
    pub links: Vec<lsp_types::DocumentLink>,
}

/// 4.5.c: outcome of an in-flight `textDocument/documentLink`
/// request. `Empty` for server responses that returned no
/// links (still updates the cache so we don't keep
/// re-issuing for the same version).
#[derive(Debug, Clone)]
pub enum DocumentLinksOutcome {
    Items {
        buffer_id: BufferId,
        document_version: u64,
        links: Vec<lsp_types::DocumentLink>,
    },
    Empty {
        buffer_id: BufferId,
        document_version: u64,
    },
}

// ---- Code lens ---------------------------------------------------------

/// 4.5.d: per-buffer cache of `textDocument/codeLens` results.
/// Filled by the pump on doc-version change or after
/// `workspace/codeLens/refresh` evicts the entry. The
/// `:lsp-code-lens` picker reads the cache; accept routes the
/// chosen lens's `command` through `workspace/executeCommand`.
#[derive(Debug, Clone)]
pub struct LspCodeLensCache {
    pub document_version: u64,
    pub lenses: Vec<lsp_types::CodeLens>,
    /// Which server produced the cached batch. Code-lens
    /// `command` payloads are server-specific; routing the
    /// `executeCommand` to the originating server keeps the
    /// dispatch unambiguous when multiple servers attach.
    pub server_id: Arc<str>,
}

/// 4.5.d: outcome of an in-flight `textDocument/codeLens`
/// request. Carries the server id so the eviction-on-refresh
/// path can match by server.
#[derive(Debug, Clone)]
pub enum CodeLensOutcome {
    Items {
        buffer_id: BufferId,
        document_version: u64,
        server_id: Arc<str>,
        lenses: Vec<lsp_types::CodeLens>,
    },
    Empty {
        buffer_id: BufferId,
        document_version: u64,
    },
}

// ---- Document color ----------------------------------------------------

/// 4.5.e: per-buffer cache of color literals + their resolved
/// values. Filled by the per-tick pump on document-version
/// change; consumed by `:lsp-color-presentation`. Renderer
/// swatch overlay queued -- today the cache only feeds the
/// picker.
#[derive(Debug, Clone)]
pub struct LspDocumentColorCache {
    pub document_version: u64,
    pub colors: Vec<lsp_types::ColorInformation>,
    /// Server that produced the cache. Routing `colorPresentation`
    /// back to the same server keeps the alternatives consistent
    /// (a CSS server's `rgb()` formats aren't useful for a Rust
    /// `rgb!` macro).
    pub server_id: Arc<str>,
}

/// 4.5.e: outcome of an in-flight `documentColor` request.
#[derive(Debug, Clone)]
pub enum DocumentColorOutcome {
    Items {
        buffer_id: BufferId,
        document_version: u64,
        server_id: Arc<str>,
        colors: Vec<lsp_types::ColorInformation>,
    },
    Empty {
        buffer_id: BufferId,
        document_version: u64,
    },
}

// ---- Semantic tokens ---------------------------------------------------

/// 4.4.h: one decoded LSP semantic token, expanded from the
/// server's relative-position varint encoding into absolute
/// positions. `token_type` is the canonical name from the
/// server's legend (e.g. `"keyword"`, `"function"`) so the
/// renderer can pick a style without looking up the index.
/// `length` is in utf-16 code units per the LSP spec.
#[derive(Debug, Clone)]
pub struct DecodedSemanticToken {
    pub line: u32,
    pub start_char: u32,
    pub length: u32,
    pub token_type: String,
    /// Names of every modifier bit set on this token (legend
    /// resolved). Empty when the server set no modifier bits.
    pub modifiers: Vec<String>,
}

/// 4.4.h: cached decoded `textDocument/semanticTokens/full`
/// response for one buffer. Same shape as the other LSP
/// per-buffer caches: keyed on `(BufferId, document_version)`,
/// invalidated by the pump when the version changes.
///
/// `result_id` is the server-issued tag the host sends back
/// on the next `full/delta` request so the server knows which
/// baseline to diff against (4.4.i).
///
/// `raw_data` (4.4.i) keeps the un-decoded `Vec<SemanticToken>`
/// alongside the decoded view so delta edits can splice into
/// it before the host re-decodes for the renderer.
#[derive(Debug, Clone)]
pub struct LspSemanticTokensCache {
    pub document_version: u64,
    pub result_id: Option<String>,
    pub raw_data: Vec<lsp_types::SemanticToken>,
    pub tokens: Vec<DecodedSemanticToken>,
}

/// 4.4.h: in-flight `semanticTokens/full` request outcome.
/// 4.4.i extends with the `Delta` variant for the
/// `full/delta` path.
#[derive(Debug, Clone)]
pub enum SemanticTokensOutcome {
    Items {
        buffer_id: BufferId,
        document_version: u64,
        result_id: Option<String>,
        raw_data: Vec<lsp_types::SemanticToken>,
        tokens: Vec<DecodedSemanticToken>,
    },
    /// 4.4.i: server returned a delta against `previous_result_id`.
    /// The drain looks up the previous cache, splices the edits
    /// into `raw_data`, and re-decodes using the legend captured
    /// at request time.
    Delta {
        buffer_id: BufferId,
        document_version: u64,
        previous_result_id: String,
        new_result_id: Option<String>,
        edits: Vec<lsp_types::SemanticTokensEdit>,
        token_types: Vec<lsp_types::SemanticTokenType>,
        token_modifiers: Vec<lsp_types::SemanticTokenModifier>,
    },
    Empty {
        buffer_id: BufferId,
        document_version: u64,
    },
}

/// 4.4.i: apply a server-issued `SemanticTokensEdit` script
/// to `raw_data` in place. Each edit specifies a start index
/// (into the previous token vec), a count to delete, and a
/// replacement slice. Edits are applied in order; the server
/// constructs them against the index space of the input vec.
///
/// Returns `Err(())` and leaves `raw_data` untouched when an
/// edit references a range outside the current vec (defensive
/// guard against server bugs; the host falls back to a fresh
/// full request when this fires).
pub fn apply_semantic_token_edits(
    raw_data: &mut Vec<lsp_types::SemanticToken>,
    edits: &[lsp_types::SemanticTokensEdit],
) -> Result<(), ()> {
    for edit in edits {
        let start = edit.start as usize;
        let delete_count = edit.delete_count as usize;
        let end = start.checked_add(delete_count).ok_or(())?;
        if end > raw_data.len() {
            return Err(());
        }
        let replacement = edit.data.clone().unwrap_or_default();
        raw_data.splice(start..end, replacement);
    }
    Ok(())
}

/// 4.4.h: decode the LSP semantic-tokens stream into
/// absolute-position tokens. Format per LSP §3.17.6: each
/// token's `delta_line` is relative to the previous token's
/// line; `delta_start` is relative to the previous token's
/// start when on the same line, otherwise relative to column
/// 0. `token_types` / `token_modifiers` are the server's
/// legend slices -- indexes outside the legend are dropped
/// (defense-in-depth; real servers don't emit out-of-range).
pub fn decode_semantic_tokens(
    data: &[lsp_types::SemanticToken],
    token_types: &[lsp_types::SemanticTokenType],
    token_modifiers: &[lsp_types::SemanticTokenModifier],
) -> Vec<DecodedSemanticToken> {
    let mut out = Vec::with_capacity(data.len());
    let mut cur_line: u32 = 0;
    let mut cur_start: u32 = 0;
    for tok in data {
        if tok.delta_line == 0 {
            cur_start += tok.delta_start;
        } else {
            cur_line += tok.delta_line;
            cur_start = tok.delta_start;
        }
        let Some(token_type) = token_types.get(tok.token_type as usize) else {
            continue;
        };
        let mut modifiers: Vec<String> = Vec::new();
        for (i, m) in token_modifiers.iter().enumerate() {
            if (tok.token_modifiers_bitset >> i) & 1 == 1 {
                modifiers.push(m.as_str().to_string());
            }
        }
        out.push(DecodedSemanticToken {
            line: cur_line,
            start_char: cur_start,
            length: tok.length,
            token_type: token_type.as_str().to_string(),
            modifiers,
        });
    }
    out
}

// ---- Pull diagnostics --------------------------------------------------

/// 4.4.j: cached `textDocument/diagnostic` state per buffer.
/// `result_id` is what the server issued on the previous
/// response; threading it back in `previous_result_id` lets
/// the server answer `Unchanged` when nothing moved.
#[derive(Debug, Clone, Default)]
pub struct LspPullDiagnosticsCache {
    pub document_version: u64,
    pub result_id: Option<String>,
}

/// 4.4.j: in-flight `textDocument/diagnostic` request outcome.
/// `Full` means "here are the diagnostics" (apply to the
/// layer); `Unchanged` means "nothing moved since the
/// previous `result_id`" (no-op on the layer, just refresh
/// the cache's version). `Empty` is the "no server / cancelled
/// / error" path -- still seats a cache entry with the
/// current version so the pump doesn't re-fire on the next
/// tick without an actual edit.
#[derive(Debug, Clone)]
pub enum PullDiagnosticsOutcome {
    Full {
        buffer_id: BufferId,
        server_id: Arc<str>,
        uri: Uri,
        document_version: u64,
        result_id: Option<String>,
        diagnostics: Vec<lsp_types::Diagnostic>,
    },
    Unchanged {
        buffer_id: BufferId,
        document_version: u64,
        result_id: String,
    },
    Empty {
        buffer_id: BufferId,
        document_version: u64,
    },
}

// ---- Hover -------------------------------------------------------------

/// Result of a `K` (LSP hover) request, sent from the spawned
/// task to the App's main thread. Carrying the no-result
/// variants explicitly (instead of just dropping the channel
/// send) lets the drain echo a clear message so the user
/// always gets feedback on `K`.
#[derive(Debug, Clone)]
pub enum HoverOutcome {
    /// Markdown body to feed into the popup. First non-empty
    /// wins across attached servers.
    Body(String),
    /// Walked every attached server; each returned an empty /
    /// missing hover.
    NoBody { servers_tried: usize },
    /// The buffer's URI maps to no attached servers.
    NoServers,
}

// ---- References --------------------------------------------------------

/// Result of a `gr` (LSP references) request. Carries the
/// symbol-under-cursor verbatim so the rendered help buffer's
/// title reads `References for "foo"` and the user has
/// confirmation of what they searched for.
#[derive(Debug, Clone)]
pub enum ReferencesOutcome {
    /// Merged + deduped reference list across attached
    /// servers. May be empty (Found(symbol, [])) when servers
    /// know about the symbol but it has no other call sites.
    Found {
        symbol: String,
        locations: Vec<lsp_types::Location>,
    },
    /// The buffer's URI maps to no attached servers.
    NoServers,
}

// ---- Completion (LSP-specific) -----------------------------------------

/// Drain payload for `completionItem/resolve` (Phase
/// 4.2.g.3). CSM.8b.5: `meta_index` is now the index of the
/// fired candidate within state.raw's LSP-row sequence; the
/// drain decodes that row's payload, applies the resolved
/// fields, re-encodes in place. Multiple resolves in flight
/// (selection change → cancel prior → fire new) cancel via
/// the supplied token.
#[derive(Debug, Clone)]
pub struct CompletionResolveOutcome {
    pub meta_index: usize,
    pub resolved: lsp_types::CompletionItem,
}

/// Drain payload for the async LSP insert-completion source.
/// Replaces (rather than appends to) the current LSP slice of
/// `state.raw` -- previous items get pruned by the drain so the
/// popup reflects the freshest server response.
#[derive(Debug, Clone)]
pub enum InsertCompletionLspOutcome {
    Items {
        candidates: Vec<lattice_completion::RawCandidate>,
        is_incomplete: bool,
    },
    NoServers,
}

/// One row of an LSP completion picker. Carries the item
/// label, kind glyph, optional detail blurb, and the insert
/// text. `replace_range` is the byte range in the active line
/// to splice the insert text over.
#[derive(Debug, Clone)]
pub struct CompletionItemRow {
    pub label: String,
    pub kind_glyph: &'static str,
    pub detail: Option<String>,
    /// Text to insert (raw -- no snippet expansion yet).
    pub insert_text: String,
    /// Replace range as `(start_byte, end_byte)` on the active
    /// line.
    pub replace_range: (u32, u32),
    /// Line the replace range is on (LSP 0-based). Always the
    /// cursor's line; carried explicitly to keep the accept
    /// path independent of cursor mutations.
    pub line: u32,
}

/// Outcome of a `textDocument/completion` request.
#[derive(Debug, Clone)]
pub enum CompletionOutcome {
    Items(Vec<CompletionItemRow>),
    NoServers,
}

// ---- Code action -------------------------------------------------------

/// One row of a code-action picker. Carries the action title,
/// kind glyph, and the original `CodeAction` payload (or its
/// raw `Command`-only form). Action items survive on the App
/// across the request → picker accept gap so the resolve /
/// apply path can read them by index.
#[derive(Debug, Clone)]
pub struct CodeActionRow {
    pub title: String,
    pub kind_glyph: &'static str,
    pub action: lsp_types::CodeActionOrCommand,
}

/// Outcome of a `:code-actions` request. Drained per frame.
#[derive(Debug, Clone)]
pub enum CodeActionOutcome {
    /// Fresh code-action result list. Drain opens a picker.
    Items(Vec<CodeActionRow>),
    /// Resolved action (post-`codeAction/resolve`). Drain
    /// applies directly -- the picker is already gone.
    Resolved(lsp_types::CodeAction),
    NoProvider,
}

// ---- Rename ------------------------------------------------------------

/// Outcome of a `:rename` request. The success arm
/// pre-flattens the WorkspaceEdit into a per-file
/// `Vec<TextEdit>` map so the App-side apply path doesn't have
/// to walk lsp-types' enum shapes. `NoProvider` echoes when no
/// attached server advertises `renameProvider`; `NotRenameable`
/// when prepareRename refused; `Empty` when the rename
/// succeeded but the server returned no edits.
#[derive(Debug, Clone)]
pub enum RenameOutcome {
    Edits {
        /// Per-file edits keyed by URI string. Each Vec is
        /// already in the order the server returned (the apply
        /// path reverse-sorts before applying, same as
        /// formatting).
        per_file: Vec<(lsp_types::Uri, Vec<lsp_types::TextEdit>)>,
        new_name: String,
    },
    NoProvider,
    NotRenameable {
        reason: String,
    },
    Empty,
}

// ---- Signature help ----------------------------------------------------

/// Outcome of a `textDocument/signatureHelp` request. The
/// response carries multiple signatures (one per overload)
/// plus the active signature/parameter indices. We collapse
/// to the active overload + parameter highlight for the popup
/// body.
#[derive(Debug, Clone)]
pub enum SignatureHelpOutcome {
    /// Pre-rendered markdown body for the popup. Empty body
    /// means "no signature info" (server returned None or an
    /// empty `signatures` array).
    Body(String),
    /// No server attached / no provider advertised.
    NoServers,
}

// ---- Format ------------------------------------------------------------

/// Outcome of a `:format` / `:format-range` request. Drained
/// per frame; the App applies the edits as one undo unit or
/// echoes the appropriate failure / no-op state.
#[derive(Debug, Clone)]
pub enum FormatOutcome {
    /// Server returned a (possibly empty) edit list. Empty ==
    /// no changes needed; non-empty == apply.
    Edits(Vec<lsp_types::TextEdit>),
    /// No attached server advertises the relevant formatting
    /// provider (`is_range` distinguishes whole-buffer from
    /// range-format providers since they're separate caps).
    NoProvider { is_range: bool },
}

// ---- Symbols -----------------------------------------------------------

/// One row of a document-symbol / workspace-symbol picker.
/// Carries the symbol's name, kind, depth (for in-document
/// hierarchy indent), and the location to jump to. Built
/// host-side from the LSP `DocumentSymbolResponse` /
/// `Vec<SymbolInformation>` so the picker doesn't depend on
/// lsp-types.
#[derive(Debug, Clone)]
pub struct SymbolRow {
    pub name: String,
    pub kind_glyph: &'static str,
    pub container: Option<String>,
    /// Indent depth (0 = top-level). Document-symbol responses
    /// nest; workspace-symbol responses are flat (depth = 0).
    pub depth: u32,
    pub path: std::path::PathBuf,
    /// LSP 0-based line.
    pub line: u32,
    /// utf-8 byte column.
    pub col: u32,
}

/// Outcome of a document-symbol / workspace-symbol request --
/// drained per frame and either opens a picker or echoes.
#[derive(Debug, Clone)]
pub enum SymbolsOutcome {
    Found { title: String, rows: Vec<SymbolRow> },
    NoServers,
}

// ---- Nav kind ----------------------------------------------------------

/// Which navigation request flavour produced an in-flight nav
/// response (Phase 4.2.c). All four share the same dispatch
/// shape (per-server `Vec<Location>` merge + dedup + jump-or-
/// list) -- the kind only changes the LSP method called and
/// the user-facing "no X found" echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspNavKind {
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
}

impl LspNavKind {
    /// Verb used in echoes ("no definitions found",
    /// "3 implementations; jumping to first", etc.).
    pub fn noun_plural(self) -> &'static str {
        match self {
            Self::Definition => "definitions",
            Self::Declaration => "declarations",
            Self::TypeDefinition => "type definitions",
            Self::Implementation => "implementations",
        }
    }

    /// Single-word noun used in error contexts ("definition
    /// target uri is not a file", etc.).
    pub fn noun_singular(self) -> &'static str {
        match self {
            Self::Definition => "definition",
            Self::Declaration => "declaration",
            Self::TypeDefinition => "type definition",
            Self::Implementation => "implementation",
        }
    }
}
