//! Client capability advertisement (sent during `initialize`)
//! and server capability storage (returned by `initialize`).
//!
//! ## Advertise discipline
//!
//! Per LSP spec, advertising a capability obliges the client to
//! handle the corresponding server behaviour. We grow the
//! advertised set in lockstep with feature implementations:
//!
//! | Phase | Capability bucket |
//! |---|---|
//! | 4.1 (this commit) | `general` (encoding + stale-request), `workspace` (applyEdit, configuration, workspaceFolders), `textDocument.synchronization`, `textDocument.publishDiagnostics` |
//! | 4.2 | `textDocument`: hover, definition/declaration/typeDefinition/implementation, references, documentSymbol, workspace.symbol, completion |
//! | 4.3 | `textDocument`: codeAction, rename, formatting/rangeFormatting/onTypeFormatting, signatureHelp |
//! | 4.4 | `textDocument`: semanticTokens, inlayHint, foldingRange, documentHighlight, selectionRange, callHierarchy, etc. |
//!
//! Each phase ADDs to [`client_capabilities`]; the server side
//! gating just reads from [`Capabilities`] and skips features the
//! server didn't advertise back.
//!
//! ## Position encoding
//!
//! We prefer `utf-8` (LSP 3.17 introduced `general.positionEncodings`).
//! Most modern servers honour the negotiation; older ones default
//! to utf-16, which we handle via the column converter (queued for
//! 4.1.c). Keeping both in the advertised list lets us survive
//! servers that ignore `positionEncodings` entirely.

use std::sync::Arc;

use lsp_types::{
    ClientCapabilities, GeneralClientCapabilities, PositionEncodingKind,
    PublishDiagnosticsClientCapabilities, ServerCapabilities,
    StaleRequestSupportClientCapabilities, TagSupport, TextDocumentClientCapabilities,
    TextDocumentSyncClientCapabilities, WorkspaceClientCapabilities,
    WorkspaceEditClientCapabilities,
};

/// Build the full set of capabilities the client advertises in
/// `initialize`. Pure -- safe to call from any task / thread.
///
/// The structure is verbose by design: each `Some(...)` here is
/// a deliberate "we handle this" promise. Anything left as
/// `None` (or absent from the struct) is something we don't yet
/// implement; servers MAY still send it, but we ignore the
/// response.
pub fn client_capabilities() -> ClientCapabilities {
    ClientCapabilities {
        general: Some(general_capabilities()),
        workspace: Some(workspace_capabilities()),
        text_document: Some(text_document_capabilities()),
        // window, notebook_document, experimental: not used in
        // 4.1; added per phase as features land.
        ..Default::default()
    }
}

fn general_capabilities() -> GeneralClientCapabilities {
    GeneralClientCapabilities {
        // Prefer utf-8 (one byte == one code unit; matches our
        // internal Position::byte). Accept utf-16 as a fallback
        // for servers that don't honour positionEncodings.
        // utf-32 is allowed by the spec but practically unused;
        // we'd add it if a server demanded it.
        position_encodings: Some(vec![
            PositionEncodingKind::UTF8,
            PositionEncodingKind::UTF16,
        ]),
        // Tells the server we honour `$/cancelRequest` -- it can
        // free its scheduling slot when we cancel. Without this,
        // some servers run the cancelled request to completion.
        stale_request_support: Some(StaleRequestSupportClientCapabilities {
            cancel: true,
            // No specific retry-on-content-modified routing yet;
            // when 4.4 lands semanticTokens delta we'll add the
            // affected method names here.
            retry_on_content_modified: Vec::new(),
        }),
        // markdown / regular-expressions advertisement is
        // server-specific; rust-analyzer / pyright / gopls don't
        // strictly require them. Leave None until a feature
        // depends on the negotiation.
        ..Default::default()
    }
}

fn workspace_capabilities() -> WorkspaceClientCapabilities {
    WorkspaceClientCapabilities {
        // We honour `workspace/applyEdit` -- the server-initiated
        // request that drives rename / code-action edits in 4.3.
        // Advertising in 4.1 is harmless: a server that doesn't
        // use it still gets a server-error if it tries before
        // we ship the handler.
        apply_edit: Some(true),
        // workspaceEdit shape: documentChanges + resourceOperations
        // are needed by 4.3's rename + code-action pipeline. We
        // advertise them now so servers that begin sending them
        // pre-4.3 get the promise honoured by then; the receiver
        // is implemented when the feature lands.
        workspace_edit: Some(WorkspaceEditClientCapabilities {
            document_changes: Some(true),
            resource_operations: Some(vec![
                lsp_types::ResourceOperationKind::Create,
                lsp_types::ResourceOperationKind::Rename,
                lsp_types::ResourceOperationKind::Delete,
            ]),
            failure_handling: Some(lsp_types::FailureHandlingKind::TextOnlyTransactional),
            normalizes_line_endings: Some(true),
            change_annotation_support: None,
        }),
        // workspace/configuration: lets the server pull config
        // from us at runtime. Useful even before §5.12 lands --
        // we can return the empty object and most servers cope.
        configuration: Some(true),
        // Single-root workspace for v1; multi-root WorkspaceFolder
        // arrives later. Advertising true here means we send
        // `workspaceFolders` in initialize and emit the
        // `workspace/didChangeWorkspaceFolders` notification.
        workspace_folders: Some(true),
        // 4.4 features that need workspace-side advertisement
        // (semanticTokens.refresh, inlayHint.refresh, codeLens.refresh,
        // diagnostics.refresh) are added when those phases land.
        ..Default::default()
    }
}

fn text_document_capabilities() -> TextDocumentClientCapabilities {
    TextDocumentClientCapabilities {
        synchronization: Some(synchronization_capabilities()),
        publish_diagnostics: Some(publish_diagnostics_capabilities()),
        // 4.2 capabilities (hover, definition, etc.) are added
        // alongside their feature commits. 4.3 (codeAction, rename,
        // formatting, signatureHelp), 4.4 (semanticTokens, inlayHint,
        // foldingRange, documentHighlight) likewise.
        ..Default::default()
    }
}

fn synchronization_capabilities() -> TextDocumentSyncClientCapabilities {
    TextDocumentSyncClientCapabilities {
        dynamic_registration: Some(false),
        // We honour will-save and will-save-wait-until in 4.3
        // (when format-on-save lands). Advertising now is
        // harmless; the methods are gated by `text_document_sync`
        // on the *server* side.
        will_save: Some(true),
        will_save_wait_until: Some(true),
        did_save: Some(true),
    }
}

fn publish_diagnostics_capabilities() -> PublishDiagnosticsClientCapabilities {
    PublishDiagnosticsClientCapabilities {
        // We surface `relatedInformation` in the diagnostics
        // buffer (a diagnostic with linked secondary locations
        // shows them as sub-rows).
        related_information: Some(true),
        // tagSupport lets the renderer render `Unnecessary` /
        // `Deprecated` differently (strikethrough / dim).
        tag_support: Some(TagSupport {
            value_set: vec![
                lsp_types::DiagnosticTag::UNNECESSARY,
                lsp_types::DiagnosticTag::DEPRECATED,
            ],
        }),
        // version_support: server tags each diagnostic with the
        // doc version it computed against; we drop diagnostics
        // older than the current version so a stale
        // publishDiagnostics from before our last edit doesn't
        // overwrite fresher state.
        version_support: Some(true),
        // codeDescriptionSupport: adds `codeDescription.href` to
        // each diagnostic -- a deep link to the rule's docs (e.g.
        // rustc explain output). Used by the diagnostics buffer's
        // `gx` handler.
        code_description_support: Some(true),
        // dataSupport: lets the server attach an opaque blob to a
        // diagnostic that we round-trip on `codeAction.diagnostics`.
        // No-op until 4.3.
        data_support: Some(true),
    }
}

/// Snapshot of the negotiated capabilities. The actor stores one
/// of these (in an `Arc`) after `initialize` completes; per-feature
/// dispatch reads from it before issuing a request.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// What we advertised. Stable for the actor's lifetime.
    pub client: ClientCapabilities,
    /// What the server advertised back.
    pub server: ServerCapabilities,
    /// Final negotiated position encoding. utf-8 if the server
    /// honoured our preference; utf-16 otherwise (older servers
    /// that ignore `general.positionEncodings`).
    pub position_encoding: PositionEncodingKind,
}

impl Capabilities {
    /// Construct from server's `initialize` response. Picks the
    /// position encoding the server advertised (if any) or
    /// defaults to utf-16 -- the LSP 3.17 fallback when the
    /// server doesn't honour negotiation.
    pub fn from_initialize(client: ClientCapabilities, server: ServerCapabilities) -> Arc<Self> {
        // Server's position encoding preference wins. If absent,
        // the spec says utf-16 is the default (3.16 and earlier
        // had no negotiation at all -- everything was utf-16).
        let position_encoding = server
            .position_encoding
            .clone()
            .unwrap_or(PositionEncodingKind::UTF16);
        Arc::new(Self {
            client,
            server,
            position_encoding,
        })
    }

    /// True iff the negotiated encoding is utf-8. Used by the
    /// position-conversion shim to bypass utf-16 conversion when
    /// the server agrees.
    pub fn is_utf8(&self) -> bool {
        self.position_encoding == PositionEncodingKind::UTF8
    }

    /// Server's `hoverProvider` presence -- gates 4.2's hover
    /// dispatch. Returns false until 4.2 supplements the helper.
    pub fn supports_hover(&self) -> bool {
        self.server.hover_provider.is_some()
    }

    /// Server's `definitionProvider` presence.
    pub fn supports_definition(&self) -> bool {
        self.server.definition_provider.is_some()
    }

    /// Server's `referencesProvider` presence -- gates 4.2.d's
    /// `gr` dispatch.
    pub fn supports_references(&self) -> bool {
        self.server.references_provider.is_some()
    }

    /// Server's `documentSymbolProvider` presence -- gates the
    /// `:document-symbols` outline view (Phase 4.2.e).
    pub fn supports_document_symbol(&self) -> bool {
        self.server.document_symbol_provider.is_some()
    }

    /// Server's `workspaceSymbolProvider` presence -- gates the
    /// `:workspace-symbols` picker (Phase 4.2.f).
    pub fn supports_workspace_symbol(&self) -> bool {
        self.server.workspace_symbol_provider.is_some()
    }

    /// Server's `completionProvider` presence -- gates 4.2.g's
    /// `gen:lsp-completion` source.
    pub fn supports_completion(&self) -> bool {
        self.server.completion_provider.is_some()
    }

    /// Server's `documentFormattingProvider` presence -- gates
    /// `:format` (Phase 4.3).
    pub fn supports_formatting(&self) -> bool {
        self.server.document_formatting_provider.is_some()
    }

    /// Server's `documentRangeFormattingProvider` presence --
    /// gates `:format-range` / `=` operator on motions / objects.
    pub fn supports_range_formatting(&self) -> bool {
        self.server.document_range_formatting_provider.is_some()
    }

    /// Server's `signatureHelpProvider` presence -- gates the
    /// trigger-character signature popup (Phase 4.3).
    pub fn supports_signature_help(&self) -> bool {
        self.server.signature_help_provider.is_some()
    }

    /// Trigger characters that should fire `textDocument/signatureHelp`
    /// in Insert mode. Empty when the server doesn't advertise the
    /// provider or doesn't list any.
    pub fn signature_help_trigger_chars(&self) -> Vec<char> {
        self.server
            .signature_help_provider
            .as_ref()
            .and_then(|p| p.trigger_characters.as_ref())
            .map(|v| v.iter().filter_map(|s| s.chars().next()).collect())
            .unwrap_or_default()
    }

    /// Server's text-document sync mode -- determines whether we
    /// send incremental or full content on `didChange`. None is
    /// the LSP signal that the server doesn't want sync at all.
    pub fn text_document_sync_kind(&self) -> Option<lsp_types::TextDocumentSyncKind> {
        match self.server.text_document_sync.as_ref()? {
            lsp_types::TextDocumentSyncCapability::Kind(k) => Some(*k),
            lsp_types::TextDocumentSyncCapability::Options(o) => o.change,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_capabilities_advertise_minimum_4_1_set() {
        let caps = client_capabilities();
        // general
        let g = caps.general.unwrap();
        let encs = g.position_encodings.unwrap();
        assert!(encs.contains(&PositionEncodingKind::UTF8));
        assert!(encs.contains(&PositionEncodingKind::UTF16));
        assert!(g.stale_request_support.unwrap().cancel);
        // workspace
        let w = caps.workspace.unwrap();
        assert_eq!(w.apply_edit, Some(true));
        assert_eq!(w.configuration, Some(true));
        assert_eq!(w.workspace_folders, Some(true));
        // textDocument
        let td = caps.text_document.unwrap();
        assert!(td.synchronization.is_some());
        assert!(td.publish_diagnostics.is_some());
    }

    #[test]
    fn capabilities_picks_utf8_when_server_advertises_it() {
        let server = ServerCapabilities {
            position_encoding: Some(PositionEncodingKind::UTF8),
            ..Default::default()
        };
        let caps = Capabilities::from_initialize(client_capabilities(), server);
        assert!(caps.is_utf8());
    }

    #[test]
    fn capabilities_falls_back_to_utf16_when_server_silent() {
        // Older servers don't advertise position_encoding at all;
        // LSP 3.16 said utf-16 by default.
        let server = ServerCapabilities::default();
        let caps = Capabilities::from_initialize(client_capabilities(), server);
        assert!(!caps.is_utf8());
        assert_eq!(caps.position_encoding, PositionEncodingKind::UTF16);
    }

    #[test]
    fn capabilities_text_document_sync_kind_extraction() {
        // Kind variant.
        let server = ServerCapabilities {
            text_document_sync: Some(lsp_types::TextDocumentSyncCapability::Kind(
                lsp_types::TextDocumentSyncKind::INCREMENTAL,
            )),
            ..Default::default()
        };
        let caps = Capabilities::from_initialize(client_capabilities(), server);
        assert_eq!(
            caps.text_document_sync_kind(),
            Some(lsp_types::TextDocumentSyncKind::INCREMENTAL)
        );

        // Options variant.
        let server = ServerCapabilities {
            text_document_sync: Some(lsp_types::TextDocumentSyncCapability::Options(
                lsp_types::TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(lsp_types::TextDocumentSyncKind::FULL),
                    ..Default::default()
                },
            )),
            ..Default::default()
        };
        let caps = Capabilities::from_initialize(client_capabilities(), server);
        assert_eq!(
            caps.text_document_sync_kind(),
            Some(lsp_types::TextDocumentSyncKind::FULL)
        );
    }
}
