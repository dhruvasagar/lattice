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
    ClientCapabilities, GeneralClientCapabilities, HoverClientCapabilities, MarkupKind,
    PositionEncodingKind, PublishDiagnosticsClientCapabilities, ServerCapabilities,
    SignatureHelpClientCapabilities, SignatureInformationSettings,
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
        // workspace/symbol with lazy-resolve support (Phase
        // 4.2 follow-up). When the server returns a
        // `WorkspaceSymbol` whose `location` is the
        // `WorkspaceLocation` variant (URI only, no range), the
        // client fires `workspaceSymbol/resolve` to fill in the
        // range on accept. We list `location.range` as the
        // resolvable property -- matches what rust-analyzer /
        // gopls populate today.
        symbol: Some(lsp_types::WorkspaceSymbolClientCapabilities {
            dynamic_registration: Some(false),
            symbol_kind: None,
            tag_support: None,
            resolve_support: Some(
                lsp_types::WorkspaceSymbolResolveSupportCapability {
                    properties: vec!["location.range".into()],
                },
            ),
        }),
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
        // Advertise that we render hover / signatureHelp content as
        // markdown (preferred) or plaintext. Without this the
        // server defaults to plaintext per the LSP spec, which
        // strips fenced code blocks and inline markup -- the
        // markdown highlighter then has no patterns to colour and
        // the popup renders as flat grey text.
        hover: Some(hover_capabilities()),
        signature_help: Some(signature_help_capabilities()),
        ..Default::default()
    }
}

fn hover_capabilities() -> HoverClientCapabilities {
    HoverClientCapabilities {
        dynamic_registration: Some(false),
        content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
    }
}

fn signature_help_capabilities() -> SignatureHelpClientCapabilities {
    SignatureHelpClientCapabilities {
        dynamic_registration: Some(false),
        signature_information: Some(SignatureInformationSettings {
            documentation_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
            parameter_information: None,
            active_parameter_support: None,
        }),
        context_support: None,
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

    /// Whether the server advertises `resolveProvider` on its
    /// workspaceSymbolProvider options. When true, the server
    /// MAY return `WorkspaceSymbol` entries whose `location` is
    /// the `WorkspaceLocation` (URI only) variant; the client
    /// fires `workspaceSymbol/resolve` on accept to fill in the
    /// `range` before jumping. Phase 4.2 follow-up.
    pub fn workspace_symbol_resolve_provider(&self) -> bool {
        match &self.server.workspace_symbol_provider {
            Some(lsp_types::OneOf::Right(opts)) => opts.resolve_provider.unwrap_or(false),
            // The `OneOf::Left(bool)` form doesn't carry options
            // -- the boolean only signals presence; resolve is
            // implicitly false there.
            _ => false,
        }
    }

    /// Server's `completionProvider` presence -- gates 4.2.g's
    /// `gen:lsp-completion` source.
    pub fn supports_completion(&self) -> bool {
        self.server.completion_provider.is_some()
    }

    /// Whether the server advertises `resolveProvider` on its
    /// completionProvider options. When true, completion items
    /// may arrive with `documentation` / `additionalTextEdits`
    /// missing and need `completionItem/resolve` before the
    /// docs popup or accept paths read those fields.
    pub fn completion_resolve_provider(&self) -> bool {
        self.server
            .completion_provider
            .as_ref()
            .and_then(|p| p.resolve_provider)
            .unwrap_or(false)
    }

    /// Trigger characters the server wants completion to fire
    /// on (auto-trigger mode). Empty when none advertised or
    /// the provider is absent.
    pub fn completion_trigger_chars(&self) -> Vec<char> {
        self.server
            .completion_provider
            .as_ref()
            .and_then(|p| p.trigger_characters.as_ref())
            .map(|v| v.iter().filter_map(|s| s.chars().next()).collect())
            .unwrap_or_default()
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

    /// Server's `renameProvider` presence -- gates `:rename`
    /// (Phase 4.3). Returns true for both bool and options
    /// shapes the LSP spec allows.
    pub fn supports_rename(&self) -> bool {
        match self.server.rename_provider.as_ref() {
            Some(lsp_types::OneOf::Left(b)) => *b,
            Some(lsp_types::OneOf::Right(_)) => true,
            None => false,
        }
    }

    /// Server's `codeActionProvider` presence -- gates
    /// `:code-actions` (Phase 4.3). Returns true for both
    /// bool and options shapes the LSP spec allows.
    pub fn supports_code_action(&self) -> bool {
        match self.server.code_action_provider.as_ref() {
            Some(lsp_types::CodeActionProviderCapability::Simple(b)) => *b,
            Some(lsp_types::CodeActionProviderCapability::Options(_)) => true,
            None => false,
        }
    }

    /// Whether the server advertises `resolveProvider` on its
    /// codeActionProvider options. When true, codeAction
    /// items may arrive without `edit` and need
    /// `codeAction/resolve` before apply.
    pub fn code_action_resolve_provider(&self) -> bool {
        match self.server.code_action_provider.as_ref() {
            Some(lsp_types::CodeActionProviderCapability::Options(o)) => {
                o.resolve_provider.unwrap_or(false)
            }
            _ => false,
        }
    }

    /// Server's `executeCommandProvider` presence -- gates
    /// `workspace/executeCommand` for codeAction items that
    /// carry a `Command` payload.
    pub fn supports_execute_command(&self) -> bool {
        self.server.execute_command_provider.is_some()
    }

    /// 4.4.e: `documentHighlightProvider` -- references in the
    /// current document at the cursor; used to paint same-symbol
    /// occurrences as a soft overlay.
    pub fn supports_document_highlight(&self) -> bool {
        self.server.document_highlight_provider.is_some()
    }

    /// 4.4.e: `selectionRangeProvider` -- structural smart-
    /// expansion ranges around a position (token → expression →
    /// statement → block → function ...).
    pub fn supports_selection_range(&self) -> bool {
        self.server.selection_range_provider.is_some()
    }

    /// 4.4.f: `foldingRangeProvider` -- feeds the LSP
    /// foldmethod. The host's per-tick pump fires
    /// `textDocument/foldingRange` when the buffer's document
    /// version changes; the response seats into a per-buffer
    /// cache the `recompute_folds` dispatcher reads.
    pub fn supports_folding_range(&self) -> bool {
        self.server.folding_range_provider.is_some()
    }

    /// 4.4.g: `inlayHintProvider` -- type / parameter
    /// annotations rendered as virtual text inline with the
    /// buffer's actual characters. The host's per-tick pump
    /// fires `textDocument/inlayHint` over the visible range
    /// when the buffer's document version changes; the
    /// renderer overlay splices each hint's label at its
    /// position.
    pub fn supports_inlay_hint(&self) -> bool {
        self.server.inlay_hint_provider.is_some()
    }

    /// 4.4.h: `semanticTokensProvider` -- LSP-side highlight
    /// layer that augments tree-sitter. The provider's legend
    /// (declared at handshake) names the token types
    /// (e.g. `"keyword"`, `"function"`) and modifiers
    /// (e.g. `"static"`, `"readonly"`) the server's
    /// `textDocument/semanticTokens/full` response references
    /// by index. The host caches the legend at attach time so
    /// decoding doesn't re-read capability state per request.
    pub fn supports_semantic_tokens(&self) -> bool {
        self.server.semantic_tokens_provider.is_some()
    }

    /// 4.4.h: token-type legend declared by the server. Used
    /// by the host's decoder to map the integer token-type
    /// index in `SemanticTokens.data` back to a semantic name
    /// (`"keyword"`, `"function"`, etc.) for the renderer's
    /// per-kind styling. Returns an empty vec when the server
    /// doesn't advertise semantic tokens (the decoder then
    /// skips every token as unrecognized).
    pub fn semantic_token_types(&self) -> Vec<lsp_types::SemanticTokenType> {
        use lsp_types::SemanticTokensServerCapabilities;
        let Some(p) = self.server.semantic_tokens_provider.as_ref() else {
            return Vec::new();
        };
        match p {
            SemanticTokensServerCapabilities::SemanticTokensOptions(opts) => {
                opts.legend.token_types.clone()
            }
            SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(opts) => {
                opts.semantic_tokens_options.legend.token_types.clone()
            }
        }
    }

    /// 4.4.h: token-modifier legend declared by the server.
    /// Same shape as [`Self::semantic_token_types`] but for
    /// the bit-flag modifiers each token can carry.
    pub fn semantic_token_modifiers(&self) -> Vec<lsp_types::SemanticTokenModifier> {
        use lsp_types::SemanticTokensServerCapabilities;
        let Some(p) = self.server.semantic_tokens_provider.as_ref() else {
            return Vec::new();
        };
        match p {
            SemanticTokensServerCapabilities::SemanticTokensOptions(opts) => {
                opts.legend.token_modifiers.clone()
            }
            SemanticTokensServerCapabilities::SemanticTokensRegistrationOptions(opts) => {
                opts.semantic_tokens_options.legend.token_modifiers.clone()
            }
        }
    }

    /// Server's `documentOnTypeFormattingProvider` presence
    /// (Phase 4.3). Trigger-character driven formatting in
    /// Insert mode -- returns the first character that fires
    /// the request, plus more in `more_trigger_character`.
    pub fn supports_on_type_formatting(&self) -> bool {
        self.server
            .document_on_type_formatting_provider
            .is_some()
    }

    /// Trigger characters for onTypeFormatting. Empty when
    /// the provider isn't advertised.
    pub fn on_type_formatting_trigger_chars(&self) -> Vec<char> {
        let Some(p) = self.server.document_on_type_formatting_provider.as_ref()
        else {
            return Vec::new();
        };
        let mut out: Vec<char> = Vec::new();
        if let Some(c) = p.first_trigger_character.chars().next() {
            out.push(c);
        }
        if let Some(more) = p.more_trigger_character.as_ref() {
            for s in more {
                if let Some(c) = s.chars().next() {
                    if !out.contains(&c) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }

    /// Whether the server advertises `prepareProvider` on its
    /// rename options -- if so, `prepareRename` should run
    /// before `rename` to validate the cursor and pick up the
    /// placeholder. Most modern servers (rust-analyzer,
    /// pyright, gopls) advertise this.
    pub fn supports_prepare_rename(&self) -> bool {
        match self.server.rename_provider.as_ref() {
            Some(lsp_types::OneOf::Right(opts)) => opts.prepare_provider.unwrap_or(false),
            _ => false,
        }
    }

    /// Whether the server wants `textDocument/willSave`
    /// notifications. Reads `text_document_sync.will_save`
    /// (Phase 4.3).
    pub fn wants_will_save(&self) -> bool {
        self.sync_options()
            .and_then(|o| o.will_save)
            .unwrap_or(false)
    }

    /// Whether the server wants `textDocument/willSaveWaitUntil`
    /// requests. Used for format-on-save when true.
    pub fn wants_will_save_wait_until(&self) -> bool {
        self.sync_options()
            .and_then(|o| o.will_save_wait_until)
            .unwrap_or(false)
    }

    /// Whether the server wants `textDocument/didSave`
    /// notifications. True when the server advertises any
    /// save options (LSP spec: "if save is set, send didSave").
    pub fn wants_did_save(&self) -> bool {
        self.sync_options()
            .map(|o| o.save.is_some())
            .unwrap_or(false)
    }

    /// Whether didSave should include the post-save text.
    /// Reads `text_document_sync.save.include_text`.
    pub fn did_save_include_text(&self) -> bool {
        self.text_document_save_options()
            .and_then(|s| s.include_text)
            .unwrap_or(false)
    }

    /// Pull the negotiated `TextDocumentSyncOptions` if the
    /// server advertised the options shape. Returns `None` for
    /// the legacy `Kind(bool)` shape.
    fn sync_options(&self) -> Option<&lsp_types::TextDocumentSyncOptions> {
        match self.server.text_document_sync.as_ref()? {
            lsp_types::TextDocumentSyncCapability::Options(o) => Some(o),
            _ => None,
        }
    }

    /// Pull the negotiated save-options shape (the modern
    /// `SaveOptions` struct, not the legacy bool). Some servers
    /// emit `Save(bool)` -- treat that as "default options" by
    /// returning None here; callers fall back via
    /// `wants_did_save`.
    fn text_document_save_options(&self) -> Option<&lsp_types::SaveOptions> {
        let opts = self.sync_options()?;
        match opts.save.as_ref()? {
            lsp_types::TextDocumentSyncSaveOptions::SaveOptions(s) => Some(s),
            lsp_types::TextDocumentSyncSaveOptions::Supported(_) => None,
        }
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
    fn client_advertises_markdown_for_hover() {
        // rust-analyzer (and most servers) downgrade to plaintext
        // unless the client opts into Markdown via
        // `textDocument.hover.contentFormat`. Without this advert
        // hover popups arrive as flat plaintext and the markdown
        // grammar has no patterns to colour.
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let hover = td.hover.expect("hover capability advertised");
        let formats = hover.content_format.expect("content_format advertised");
        assert!(
            formats.contains(&MarkupKind::Markdown),
            "expected Markdown in hover content_format, got {:?}",
            formats
        );
    }

    #[test]
    fn client_advertises_markdown_for_signature_help() {
        let caps = client_capabilities();
        let td = caps.text_document.unwrap();
        let sig = td.signature_help.expect("signature_help advertised");
        let info = sig.signature_information.expect("signature info advertised");
        let formats = info
            .documentation_format
            .expect("documentation_format advertised");
        assert!(
            formats.contains(&MarkupKind::Markdown),
            "expected Markdown in signatureHelp documentation_format, got {:?}",
            formats
        );
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
