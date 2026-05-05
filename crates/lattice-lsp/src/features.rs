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
    CompletionParams, CompletionResponse, DocumentSymbolParams, DocumentSymbolResponse,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverParams, Location, ReferenceParams,
    SymbolInformation, WorkspaceSymbolParams,
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
