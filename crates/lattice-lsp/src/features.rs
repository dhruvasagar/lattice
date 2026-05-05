//! Typed wrappers around [`crate::ServerHandle::request_with_cancel`]
//! for the LSP navigation features (DESIGN.md §5.4 + Phase 4.2).
//!
//! Each method is a thin shim: build the typed `*Params`, call
//! `request_with_cancel(method_name, params, token)`, return a
//! [`Pending<R>`] over the typed response. The wire formats live in
//! `lsp-types` (`textDocument/hover` ↔ [`Hover`], etc.); the
//! wrappers exist so call sites don't sprinkle method-name strings
//! and so cancellation is impossible to forget.
//!
//! **Cancellation discipline.** Every wrapper takes a
//! [`lattice_protocol::CancellationToken`]. The App passes a fresh
//! token per request and flips it on motion / Esc / mode change so
//! a stale response from a slow server can't drop a popup over the
//! user's new cursor position. Local-only cancellation today (the
//! server keeps computing; we drop its reply on arrival) -- wire-
//! level `$/cancelRequest` is a Phase 4.2 polish item.
//!
//! **Multi-server merge.** Wrappers run *per-server*. The App's
//! per-feature dispatcher fires the same wrapper across every
//! server attached to the buffer and merges according to the
//! per-feature strategy (hover: concat with `--- name ---` sep;
//! definition / references: concat + dedup by URI+range; symbols:
//! concat; completion: concat + dedup by `text`).

use lattice_protocol::CancellationToken;
use lsp_types::{
    CompletionParams, CompletionResponse, DocumentFormattingParams, DocumentRangeFormattingParams,
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverParams, Location, PrepareRenameResponse, ReferenceParams, RenameParams,
    SignatureHelp, SignatureHelpParams, SymbolInformation, TextDocumentPositionParams, TextEdit,
    WorkspaceEdit, WorkspaceSymbolParams,
    request::{
        GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
        GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
    },
};

use crate::actor::ServerHandle;
use crate::pending::Pending;

impl ServerHandle {
    /// `textDocument/hover` (DESIGN.md §5.4 / docs/lsp-features.md).
    /// Returns `None` when the server has nothing to say at the
    /// cursor position. The body's `contents` field is what the
    /// renderer feeds into the [`crate::HoverPopup`] markdown
    /// pipeline; the optional `range` highlights the symbol
    /// hovered (renderer integration is Phase 4.2.b polish).
    pub fn hover(
        &self,
        params: HoverParams,
        token: CancellationToken,
    ) -> Pending<Option<Hover>> {
        self.request_with_cancel("textDocument/hover", params, token)
    }

    /// `textDocument/definition` (DESIGN.md §5.4 /
    /// docs/lsp-features.md). Returns the location(s) where the
    /// symbol under the cursor is defined. `GotoDefinitionResponse`
    /// is an enum: single `Location`, `Vec<Location>`, or
    /// `Vec<LocationLink>` (richer links carrying origin range).
    /// Phase 4.2.c picks one (single → jump, multiple → list).
    pub fn goto_definition(
        &self,
        params: GotoDefinitionParams,
        token: CancellationToken,
    ) -> Pending<Option<GotoDefinitionResponse>> {
        self.request_with_cancel("textDocument/definition", params, token)
    }

    /// `textDocument/declaration` (DESIGN.md §5.4 /
    /// docs/lsp-features.md). `gD` family. Same response shape as
    /// `goto_definition`; servers usually point at the *forward
    /// declaration* (header file in C / extern statement in Rust)
    /// rather than the implementation. Multi-server merge dedups
    /// by (uri, range.start) like definition.
    pub fn goto_declaration(
        &self,
        params: GotoDeclarationParams,
        token: CancellationToken,
    ) -> Pending<Option<GotoDeclarationResponse>> {
        self.request_with_cancel("textDocument/declaration", params, token)
    }

    /// `textDocument/typeDefinition` (DESIGN.md §5.4). `gy` family.
    /// "Where is the *type* of this expression defined?" Useful
    /// for stepping from a value to its struct / class / interface.
    pub fn goto_type_definition(
        &self,
        params: GotoTypeDefinitionParams,
        token: CancellationToken,
    ) -> Pending<Option<GotoTypeDefinitionResponse>> {
        self.request_with_cancel("textDocument/typeDefinition", params, token)
    }

    /// `textDocument/implementation` (DESIGN.md §5.4). `gI` family.
    /// "Where are the implementations of this trait / interface?"
    /// Often returns multiple locations (one per impl); we share
    /// definition's pick-or-list dispatch.
    pub fn goto_implementation(
        &self,
        params: GotoImplementationParams,
        token: CancellationToken,
    ) -> Pending<Option<GotoImplementationResponse>> {
        self.request_with_cancel("textDocument/implementation", params, token)
    }

    /// `textDocument/references` (DESIGN.md §5.4). Returns every
    /// reference site to the symbol under the cursor. The
    /// `include_declaration` flag sits on `ReferenceContext` inside
    /// `ReferenceParams`; callers usually want it `true` for `gr`
    /// (vim convention).
    pub fn references(
        &self,
        params: ReferenceParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<Location>>> {
        self.request_with_cancel("textDocument/references", params, token)
    }

    /// `textDocument/documentSymbol` (DESIGN.md §5.4). Returns the
    /// symbol outline for the buffer. Response is either flat
    /// `Vec<SymbolInformation>` (legacy) or hierarchical
    /// `Vec<DocumentSymbol>` (modern). Phase 4.2.e flattens the
    /// hierarchy to a list with depth-indent for the picker.
    pub fn document_symbol(
        &self,
        params: DocumentSymbolParams,
        token: CancellationToken,
    ) -> Pending<Option<DocumentSymbolResponse>> {
        self.request_with_cancel("textDocument/documentSymbol", params, token)
    }

    /// `workspace/symbol` (DESIGN.md §5.4). Workspace-scoped
    /// symbol search. The `query` string filters server-side; the
    /// editor sends `query=""` for an everything-list and
    /// re-queries as the user types in the picker (Phase 4.2.f).
    /// Response shape: `Vec<SymbolInformation>` -- the modern
    /// `WorkspaceSymbol` variant carries `location` as a
    /// `OneOf<Location, WorkspaceLocation>` which most servers
    /// don't emit yet, so we deserialize as the legacy shape and
    /// upgrade later if servers start populating the modern one.
    pub fn workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<SymbolInformation>>> {
        self.request_with_cancel("workspace/symbol", params, token)
    }

    /// `textDocument/completion` (DESIGN.md §5.4 / Phase 4.2.g).
    /// Returns either an array of items or an `isIncomplete` list
    /// the editor must re-query as the user types more. The
    /// completion pipeline (`lattice-completion`) registers a
    /// `gen:lsp-completion` source backed by this call.
    pub fn completion(
        &self,
        params: CompletionParams,
        token: CancellationToken,
    ) -> Pending<Option<CompletionResponse>> {
        self.request_with_cancel("textDocument/completion", params, token)
    }

    /// `textDocument/formatting` (DESIGN.md §5.4 / Phase 4.3).
    /// Whole-buffer formatter; the response is a `Vec<TextEdit>`
    /// the editor applies as a single undo unit. Single-server
    /// strategy per the architecture doc -- highest-priority
    /// server with `documentFormattingProvider` advertised wins.
    pub fn formatting(
        &self,
        params: DocumentFormattingParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<TextEdit>>> {
        self.request_with_cancel("textDocument/formatting", params, token)
    }

    /// `textDocument/rangeFormatting` (Phase 4.3). Same shape as
    /// `formatting` but bounded to the supplied range -- bound to
    /// the `=` operator on motions / objects / Visual selection.
    pub fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<TextEdit>>> {
        self.request_with_cancel("textDocument/rangeFormatting", params, token)
    }

    /// `textDocument/signatureHelp` (Phase 4.3). Trigger-character
    /// driven (`,`, `(`) when the server advertises the trigger.
    /// Response carries the active signature + parameter; renderer
    /// integration overlays a popup similar to hover.
    pub fn signature_help(
        &self,
        params: SignatureHelpParams,
        token: CancellationToken,
    ) -> Pending<Option<SignatureHelp>> {
        self.request_with_cancel("textDocument/signatureHelp", params, token)
    }

    /// `textDocument/prepareRename` (Phase 4.3). Validates the
    /// cursor is on a renameable identifier and returns the
    /// placeholder + range. `None` means "the symbol can't be
    /// renamed here" -- the editor echoes and bails. Optional
    /// in the spec (servers may skip prepareRename and accept
    /// rename directly), so callers should treat `None` from
    /// `prepare_rename` as "fall through to rename".
    pub fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
        token: CancellationToken,
    ) -> Pending<Option<PrepareRenameResponse>> {
        self.request_with_cancel("textDocument/prepareRename", params, token)
    }

    /// `textDocument/rename` (Phase 4.3). Renames the symbol
    /// under cursor across the workspace. Response is a
    /// `WorkspaceEdit` with per-file `Vec<TextEdit>`s; the
    /// editor applies all edits as a single undoable unit.
    pub fn rename(
        &self,
        params: RenameParams,
        token: CancellationToken,
    ) -> Pending<Option<WorkspaceEdit>> {
        self.request_with_cancel("textDocument/rename", params, token)
    }

    /// `textDocument/willSave` (Phase 4.3 -- notification).
    /// Fired before the editor commits the buffer to disk.
    /// Servers use this to clean up state, finalise indexing,
    /// or prepare didSave-driven validation.
    pub fn will_save(
        &self,
        params: lsp_types::WillSaveTextDocumentParams,
    ) -> crate::error::LspResult<()> {
        self.notify("textDocument/willSave", params)
    }

    /// `textDocument/willSaveWaitUntil` (Phase 4.3). Same
    /// trigger as `will_save` but request-shaped: server
    /// returns a `Vec<TextEdit>` to apply pre-save.
    /// format-on-save flows through here when the server
    /// advertises `will_save_wait_until` on its
    /// `TextDocumentSyncOptions.save`.
    pub fn will_save_wait_until(
        &self,
        params: lsp_types::WillSaveTextDocumentParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<TextEdit>>> {
        self.request_with_cancel("textDocument/willSaveWaitUntil", params, token)
    }

    /// `textDocument/didSave` (Phase 4.3 -- notification).
    /// Fired after a successful disk write. Carries the
    /// post-save text iff the server's
    /// `TextDocumentSaveRegistrationOptions.include_text` is
    /// true.
    pub fn did_save(
        &self,
        params: lsp_types::DidSaveTextDocumentParams,
    ) -> crate::error::LspResult<()> {
        self.notify("textDocument/didSave", params)
    }

    /// `textDocument/codeAction` (Phase 4.3). Returns the list
    /// of quick fixes / refactors / source actions available
    /// for the supplied range. Each item carries either an
    /// inline `edit` (apply directly), a `command` (route
    /// through `executeCommand`), or both. Items with neither
    /// need `codeAction/resolve` to fill in the missing
    /// `edit`.
    pub fn code_action(
        &self,
        params: lsp_types::CodeActionParams,
        token: CancellationToken,
    ) -> Pending<Option<lsp_types::CodeActionResponse>> {
        self.request_with_cancel("textDocument/codeAction", params, token)
    }

    /// `codeAction/resolve` (Phase 4.3). Lazy-resolve a
    /// codeAction that arrived without `edit`. Servers that
    /// advertise `codeActionProvider.resolveProvider` may
    /// return action stubs (label + kind only) and fill in
    /// `edit` here, on demand. Cheaper than computing every
    /// edit upfront.
    pub fn code_action_resolve(
        &self,
        action: lsp_types::CodeAction,
        token: CancellationToken,
    ) -> Pending<lsp_types::CodeAction> {
        self.request_with_cancel("codeAction/resolve", action, token)
    }

    /// `workspace/executeCommand` (Phase 4.3). Run a server-
    /// registered command identified by string id. Used by
    /// codeAction items that carry a `command` rather than an
    /// inline `edit`. Response shape varies per command --
    /// servers usually return null + side-effect via
    /// `workspace/applyEdit`.
    pub fn execute_command(
        &self,
        params: lsp_types::ExecuteCommandParams,
        token: CancellationToken,
    ) -> Pending<Option<serde_json::Value>> {
        self.request_with_cancel("workspace/executeCommand", params, token)
    }

    /// `textDocument/onTypeFormatting` (Phase 4.3). Trigger-
    /// character driven formatting that adjusts surrounding
    /// whitespace / indentation as the user types (commonly
    /// fires on `;`, `}`, `\n` for C-family). Returns the same
    /// `Vec<TextEdit>` shape as the other formatting flavours.
    pub fn on_type_formatting(
        &self,
        params: lsp_types::DocumentOnTypeFormattingParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<TextEdit>>> {
        self.request_with_cancel("textDocument/onTypeFormatting", params, token)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    use lsp_types::{
        Position as LspPosition, TextDocumentIdentifier, TextDocumentPositionParams, Uri,
    };
    use std::str::FromStr;

    fn fake_uri() -> Uri {
        Uri::from_str("file:///tmp/test.rs").unwrap()
    }

    fn position_params(line: u32, character: u32) -> TextDocumentPositionParams {
        TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: fake_uri() },
            position: LspPosition { line, character },
        }
    }

    /// Compile-time presence check: every wrapper has the expected
    /// signature `(Params, CancellationToken) -> Pending<Option<R>>`.
    /// The function bodies never run -- this asserts the API surface.
    /// `drop` (rather than `let _`) sidesteps clippy's
    /// `let_underscore_future` lint -- we genuinely don't want to
    /// poll these futures, the compile-time bounds check is the
    /// whole point.
    #[allow(dead_code)]
    fn _api_surface_compiles(handle: &ServerHandle, token: CancellationToken) {
        let pos = position_params(0, 0);
        drop::<Pending<Option<Hover>>>(handle.hover(
            HoverParams {
                text_document_position_params: pos.clone(),
                work_done_progress_params: Default::default(),
            },
            token.clone(),
        ));
        drop::<Pending<Option<GotoDefinitionResponse>>>(handle.goto_definition(
            GotoDefinitionParams {
                text_document_position_params: pos.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            token.clone(),
        ));
        drop::<Pending<Option<GotoDeclarationResponse>>>(handle.goto_declaration(
            GotoDeclarationParams {
                text_document_position_params: pos.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            token.clone(),
        ));
        drop::<Pending<Option<GotoTypeDefinitionResponse>>>(handle.goto_type_definition(
            GotoTypeDefinitionParams {
                text_document_position_params: pos.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            token.clone(),
        ));
        drop::<Pending<Option<GotoImplementationResponse>>>(handle.goto_implementation(
            GotoImplementationParams {
                text_document_position_params: pos.clone(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            token.clone(),
        ));
        drop::<Pending<Option<Vec<Location>>>>(handle.references(
            ReferenceParams {
                text_document_position: pos,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: lsp_types::ReferenceContext {
                    include_declaration: true,
                },
            },
            token.clone(),
        ));
        drop::<Pending<Option<DocumentSymbolResponse>>>(handle.document_symbol(
            DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri: fake_uri() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            token.clone(),
        ));
        drop::<Pending<Option<Vec<SymbolInformation>>>>(handle.workspace_symbol(
            WorkspaceSymbolParams {
                query: "foo".into(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            token.clone(),
        ));
        drop::<Pending<Option<CompletionResponse>>>(handle.completion(
            CompletionParams {
                text_document_position: position_params(0, 0),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            },
            token,
        ));
    }
}
