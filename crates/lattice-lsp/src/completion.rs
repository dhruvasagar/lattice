//! LSP completion source (CSM.8a / CSM.8b).
//!
//! [`LspCompletionSource`] is the `AsyncCompletionSource` impl
//! that the `lsp-completion-mode` contributes. It owns an
//! `LspSupervisorHandle` and is wired into the
//! `ActiveCompletionSources` cache via the mode's
//! `completion_sources()` contribution. The source's
//! `produce_async` future fans out across attached servers,
//! dedupes by (label, kind), and pushes one `RawCandidate`
//! per item into the supplied [`CandidateSink`]. Each
//! candidate carries an `CandidateData::Extension` payload
//! whose bytes are a JSON-encoded [`LspCompletionMeta`]; the
//! host decodes the payload at accept / docs / commit-char
//! time via [`decode_meta`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lattice_completion::{
    AsyncCompletionSource, CandidateSink, CompletionSourceContribution, CompletionSourceKind,
    InsertContextSnapshot, SourceId,
};
use lattice_config::OptionOverrideSet;
use lattice_mode::{
    CapabilitySet, Mode, ModeActivationError, ModeContext, ModeId, ModeKind, ModeRegistry,
};
use lattice_protocol::CancellationToken;

use crate::supervisor::LspSupervisorHandle;

/// `Extension::kind_id` discriminant for LSP-sourced
/// candidates. Values 0-99 reserved for first-party host
/// data (snippet uses 2); plugins use 1000+. The host's
/// decoder checks this id before attempting [`decode_meta`].
pub const LSP_COMPLETION_KIND_ID: u32 = 1;

/// LSP-sourced insert-completion candidate metadata. Carried
/// inside the `RawCandidate`'s `CandidateData::Extension`
/// payload as a JSON blob (see [`encode_meta`] / [`decode_meta`]).
/// Replaces the pre-CSM.8b host-side sidecar -- the candidate
/// IS the metadata; the host decodes on demand.
///
/// JSON over bincode keeps the payload debuggable and avoids
/// pinning lsp-types' bincode behavior. Decoding cost is
/// amortised by the host's per-popup decoded cache (rebuilt
/// only when `state.raw` mutates), so the per-frame docs /
/// glyph / commit-char paths don't repeatedly deserialise.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LspCompletionMeta {
    pub label: String,
    pub insert_text: String,
    pub filter_text: Option<String>,
    pub sort_text: Option<String>,
    pub detail: Option<String>,
    pub documentation: Option<String>,
    pub kind: Option<lsp_types::CompletionItemKind>,
    pub deprecated: bool,
    pub preselect: bool,
    pub commit_characters: Vec<char>,
    pub additional_text_edits: Vec<lsp_types::TextEdit>,
    pub command: Option<lsp_types::Command>,
    pub insert_text_format: lsp_types::InsertTextFormat,
    /// Range to replace, when the LSP item carries
    /// `textEdit.range`. `None` ⇒ host uses the popup's anchor
    /// / cursor as the replace bounds.
    pub replace_range: Option<lsp_types::Range>,
    /// Server that produced the item. Resolve / executeCommand
    /// route back to the same server. Stored as `String` for
    /// serde-friendliness; the host can `Arc::from` it on
    /// decode if it wants the shared form back.
    pub server_id: String,
    /// Original server `CompletionItem` preserved verbatim so
    /// `completionItem/resolve` round-trips it unchanged
    /// (servers use the `data` field as an opaque blob;
    /// mutating any field would break resolve).
    pub original_item: lsp_types::CompletionItem,
    /// True once `completionItem/resolve` has filled in the
    /// missing fields. Subsequent docs-popup focuses don't
    /// re-fire the resolve.
    pub resolved: bool,
}

/// Encode `meta` to a JSON byte blob suitable for embedding
/// in a `RawCandidate`'s `CandidateData::Extension::payload`.
/// Errors are caller-handled by panicking in the source's
/// produce path -- a serialize failure on lsp-types output
/// indicates a malformed item, which the supervisor should
/// have rejected before we got here.
pub fn encode_meta(meta: &LspCompletionMeta) -> Vec<u8> {
    serde_json::to_vec(meta).expect("LspCompletionMeta must serialise (all fields serde-derived)")
}

/// Decode a payload produced by [`encode_meta`]. Returns
/// `None` when the bytes don't deserialise as
/// [`LspCompletionMeta`] -- the candidate's `kind_id` may not
/// be ours (plugin source colliding on the kind, or a stray
/// payload), or the wire format may have drifted across
/// versions.
pub fn decode_meta(payload: &[u8]) -> Option<LspCompletionMeta> {
    serde_json::from_slice(payload).ok()
}

/// CSM.8a stub. Holds the supervisor handle so CSM.8b can move
/// the fan-out into `produce_async` without changing the mode's
/// public surface. Today the future is a no-op.
#[derive(Debug, Clone)]
pub struct LspCompletionSource {
    pub lsp: LspSupervisorHandle,
}

impl AsyncCompletionSource for LspCompletionSource {
    fn produce_async(
        &self,
        ctx: InsertContextSnapshot,
        sink: Arc<dyn CandidateSink>,
        token: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let lsp = self.lsp.clone();
        Box::pin(async move {
            // CSM.8b: full multi-server fan-out + dedup +
            // isIncomplete + per-item payload encode. Each
            // server's response items become one
            // `RawCandidate` whose `CandidateData::Extension`
            // payload is the serde-encoded
            // `LspCompletionMeta`; the host decodes via
            // `decode_meta` at accept / docs / commit-char
            // time without a sidecar.
            const MAX_LSP_ITEMS: usize = 500;
            use std::str::FromStr;
            let Some(uri_string) = ctx.uri.as_deref() else {
                return;
            };
            let Ok(uri) = lsp_types::Uri::from_str(uri_string) else {
                return;
            };
            let Some((line, character)) = ctx.lsp_position else {
                return;
            };
            let lsp_position = lsp_types::Position { line, character };
            let (lsp_trigger_kind, lsp_trigger_char) = match ctx.trigger {
                lattice_completion::CompletionTrigger::TriggerChar(c) => (
                    lsp_types::CompletionTriggerKind::TRIGGER_CHARACTER,
                    Some(c.to_string()),
                ),
                lattice_completion::CompletionTrigger::IncompleteRefresh => (
                    lsp_types::CompletionTriggerKind::TRIGGER_FOR_INCOMPLETE_COMPLETIONS,
                    None,
                ),
                _ => (lsp_types::CompletionTriggerKind::INVOKED, None),
            };
            let handles: Vec<crate::ServerHandle> = lsp.servers_for(&uri);
            if handles.is_empty() {
                return;
            }
            let mut emitted = 0usize;
            let mut any_incomplete = false;
            let mut seen_keys: std::collections::HashSet<(String, String)> =
                std::collections::HashSet::new();
            let lsp_source_id = lattice_completion::SourceId::new(
                lattice_completion::LSP_COMPLETION_SOURCE_ID,
            );
            for handle in handles {
                if token.is_cancelled() {
                    return;
                }
                if !handle.capabilities().supports_completion() {
                    continue;
                }
                let params = lsp_types::CompletionParams {
                    text_document_position: lsp_types::TextDocumentPositionParams {
                        text_document: lsp_types::TextDocumentIdentifier {
                            uri: uri.clone(),
                        },
                        position: lsp_position,
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                    context: Some(lsp_types::CompletionContext {
                        trigger_kind: lsp_trigger_kind,
                        trigger_character: lsp_trigger_char.clone(),
                    }),
                };
                let Ok(Some(resp)) = handle.completion(params, token.clone()).await else {
                    continue;
                };
                let (items, is_incomplete) = match resp {
                    lsp_types::CompletionResponse::Array(items) => (items, false),
                    lsp_types::CompletionResponse::List(list) => {
                        (list.items, list.is_incomplete)
                    }
                };
                if is_incomplete {
                    any_incomplete = true;
                }
                for ci in items {
                    let kind = ci.kind;
                    let label = ci.label.clone();
                    let kind_tag = kind
                        .map(|k| format!("{k:?}"))
                        .unwrap_or_else(|| "none".to_string());
                    let key = (label.clone(), kind_tag);
                    if !seen_keys.insert(key) {
                        continue;
                    }
                    let deprecated = ci
                        .tags
                        .as_ref()
                        .map(|t| t.contains(&lsp_types::CompletionItemTag::DEPRECATED))
                        .unwrap_or(false)
                        || ci.deprecated.unwrap_or(false);
                    let (insert_text, replace_range) = match ci.text_edit.as_ref() {
                        Some(lsp_types::CompletionTextEdit::Edit(te)) => {
                            (te.new_text.clone(), Some(te.range))
                        }
                        Some(lsp_types::CompletionTextEdit::InsertAndReplace(ir)) => {
                            (ir.new_text.clone(), Some(ir.replace))
                        }
                        None => (
                            ci.insert_text.clone().unwrap_or_else(|| label.clone()),
                            None,
                        ),
                    };
                    let documentation = ci.documentation.as_ref().map(|d| match d {
                        lsp_types::Documentation::String(s) => s.clone(),
                        lsp_types::Documentation::MarkupContent(mc) => mc.value.clone(),
                    });
                    let commit_characters = ci
                        .commit_characters
                        .as_ref()
                        .map(|chars| chars.iter().filter_map(|s| s.chars().next()).collect())
                        .unwrap_or_default();
                    let meta = LspCompletionMeta {
                        label: label.clone(),
                        insert_text,
                        filter_text: ci.filter_text.clone(),
                        sort_text: ci.sort_text.clone(),
                        detail: ci.detail.clone(),
                        documentation,
                        kind,
                        deprecated,
                        preselect: ci.preselect.unwrap_or(false),
                        commit_characters,
                        additional_text_edits: ci
                            .additional_text_edits
                            .clone()
                            .unwrap_or_default(),
                        command: ci.command.clone(),
                        insert_text_format: ci
                            .insert_text_format
                            .unwrap_or(lsp_types::InsertTextFormat::PLAIN_TEXT),
                        replace_range,
                        server_id: handle.server_id().to_string(),
                        original_item: ci,
                        resolved: false,
                    };
                    let display = match meta.detail.as_ref() {
                        Some(d) => format!("{}  {}", meta.label, d),
                        None => meta.label.clone(),
                    };
                    let match_text = meta
                        .filter_text
                        .clone()
                        .unwrap_or_else(|| meta.label.clone());
                    let payload = encode_meta(&meta);
                    let mut raw = lattice_completion::RawCandidate::plain(
                        match_text,
                        lattice_completion::CandidateKind::Plain,
                    )
                    .with_source(lsp_source_id.clone());
                    raw.display = display;
                    raw.data = lattice_completion::CandidateData::Extension {
                        kind_id: LSP_COMPLETION_KIND_ID,
                        payload,
                    };
                    sink.push(raw);
                    emitted += 1;
                    if emitted >= MAX_LSP_ITEMS {
                        break;
                    }
                }
                if emitted >= MAX_LSP_ITEMS {
                    break;
                }
            }
            if any_incomplete {
                sink.mark_incomplete();
            }
        })
    }
}

/// `lsp-completion-mode` -- LSP-driven insert-mode completion
/// (CSM.8a). M.6.0 declared it as a marker minor; CSM.8a makes
/// it source-contributing. Holds the [`LspSupervisorHandle`]
/// so the contributed source can fan out to attached servers.
/// `popup_filter_chord = Some('o')` ⇒ `<C-o>` inside
/// `completion-popup-mode` narrows the popup to LSP only
/// (CSM.K2).
#[derive(Debug, Clone)]
pub struct LspCompletionMode {
    pub lsp: LspSupervisorHandle,
}

impl LspCompletionMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("lsp-completion-mode")
    }
}

impl Mode for LspCompletionMode {
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn options(&self) -> OptionOverrideSet {
        OptionOverrideSet::default()
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn completion_sources(&self) -> Vec<CompletionSourceContribution> {
        vec![CompletionSourceContribution {
            id: SourceId::new(lattice_completion::LSP_COMPLETION_SOURCE_ID),
            // 200 per insert-completion.md §3.4 -- LSP outranks
            // every sync source (snippets 150, buffer-words 100,
            // path 90, tree-sitter 80) because language-server
            // candidates are the most contextually accurate.
            default_priority: 200,
            auto_trigger: true,
            // Server-advertised triggers (`.`, `::`, ...) are
            // populated dynamically post-CSM.8b when the source
            // reads the attached server's
            // `completionProvider.triggerCharacters`. CSM.8a
            // ships an empty list; sync triggers and manual
            // `<C-Space>` fire the source regardless.
            trigger_chars: Vec::new(),
            popup_filter_chord: Some('o'),
            kind: CompletionSourceKind::Async(Arc::new(LspCompletionSource {
                lsp: self.lsp.clone(),
            })),
        }]
    }
    fn on_activate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
    fn on_deactivate(&self, _ctx: &mut ModeContext<'_>) -> Result<(), ModeActivationError> {
        Ok(())
    }
}

/// Register `lsp-completion-mode` against `registry`. Called
/// from the App's boot path alongside
/// [`crate::modes::register_lsp_log_modes`]; the supervisor
/// handle is shared so source produce + host accept-path read
/// the same data.
pub fn register_lsp_completion_mode(
    registry: &mut ModeRegistry,
    lsp: LspSupervisorHandle,
) {
    registry
        .register(LspCompletionMode { lsp })
        .expect("lsp-completion-mode must register without conflict");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn sample_meta() -> LspCompletionMeta {
        LspCompletionMeta {
            label: "println!".into(),
            insert_text: "println!(\"$1\")$0".into(),
            filter_text: Some("println".into()),
            sort_text: Some("00println".into()),
            detail: Some("macro_rules! println".into()),
            documentation: Some("Prints to the standard output.".into()),
            kind: Some(lsp_types::CompletionItemKind::SNIPPET),
            deprecated: false,
            preselect: true,
            commit_characters: vec!['(', '!'],
            additional_text_edits: vec![lsp_types::TextEdit {
                range: lsp_types::Range {
                    start: lsp_types::Position { line: 0, character: 0 },
                    end: lsp_types::Position { line: 0, character: 0 },
                },
                new_text: "use std::println;\n".into(),
            }],
            command: None,
            insert_text_format: lsp_types::InsertTextFormat::SNIPPET,
            replace_range: Some(lsp_types::Range {
                start: lsp_types::Position { line: 1, character: 4 },
                end: lsp_types::Position { line: 1, character: 11 },
            }),
            server_id: "rust-analyzer".to_string(),
            original_item: lsp_types::CompletionItem::default(),
            resolved: false,
        }
    }

    /// CSM.8b.1: full round-trip preserves every field. JSON is
    /// the wire format -- exotic types (Range, TextEdit,
    /// CompletionItemKind) must serialise / deserialise without
    /// loss for the host's accept path to be correct.
    #[test]
    fn encode_decode_round_trip_preserves_all_fields() {
        let meta = sample_meta();
        let bytes = encode_meta(&meta);
        let decoded = decode_meta(&bytes).expect("decode");
        assert_eq!(decoded.label, meta.label);
        assert_eq!(decoded.insert_text, meta.insert_text);
        assert_eq!(decoded.filter_text, meta.filter_text);
        assert_eq!(decoded.sort_text, meta.sort_text);
        assert_eq!(decoded.detail, meta.detail);
        assert_eq!(decoded.documentation, meta.documentation);
        assert_eq!(decoded.kind, meta.kind);
        assert_eq!(decoded.deprecated, meta.deprecated);
        assert_eq!(decoded.preselect, meta.preselect);
        assert_eq!(decoded.commit_characters, meta.commit_characters);
        assert_eq!(decoded.additional_text_edits, meta.additional_text_edits);
        assert_eq!(decoded.insert_text_format, meta.insert_text_format);
        assert_eq!(decoded.replace_range, meta.replace_range);
        assert_eq!(decoded.server_id, meta.server_id);
        assert_eq!(decoded.resolved, meta.resolved);
    }

    /// CSM.8b.1: garbled bytes don't decode -- the host's
    /// `lsp_completion_meta_for` returns `None` for stale /
    /// foreign payloads and falls through to the sync-source
    /// path.
    #[test]
    fn decode_meta_returns_none_for_garbage() {
        assert!(decode_meta(b"not json").is_none());
        assert!(decode_meta(b"").is_none());
    }

}
