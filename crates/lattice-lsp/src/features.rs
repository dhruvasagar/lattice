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
    SignatureHelp, SignatureHelpParams, TextDocumentPositionParams, TextEdit, WorkspaceEdit,
    WorkspaceSymbolParams,
    request::{
        GotoDeclarationParams, GotoDeclarationResponse, GotoImplementationParams,
        GotoImplementationResponse, GotoTypeDefinitionParams, GotoTypeDefinitionResponse,
    },
};

use crate::actor::ServerHandle;
use crate::pending::Pending;

impl ServerHandle {
    /// `textDocument/hover` (DESIGN.md §5.4 / docs/dev/notes/lsp-features.md).
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
    /// docs/dev/notes/lsp-features.md). Returns the location(s) where the
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
    /// docs/dev/notes/lsp-features.md). `gD` family. Same response shape as
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
    ///
    /// Response shape: `WorkspaceSymbolResponse` -- the
    /// `Flat(Vec<SymbolInformation>)` variant is the legacy
    /// shape every server emits; the
    /// `Nested(Vec<WorkspaceSymbol>)` variant (LSP 3.17+) lets
    /// the server defer the `location.range` and have the
    /// client fire `workspaceSymbol/resolve` on accept. The
    /// editor handles both shapes -- legacy rows jump
    /// immediately; nested rows with a `WorkspaceLocation`
    /// route through the resolve path before jumping.
    pub fn workspace_symbol(
        &self,
        params: WorkspaceSymbolParams,
        token: CancellationToken,
    ) -> Pending<Option<lsp_types::WorkspaceSymbolResponse>> {
        self.request_with_cancel("workspace/symbol", params, token)
    }

    /// `workspaceSymbol/resolve` (LSP 3.17+, Phase 4.2 follow-up).
    /// Sent for `WorkspaceSymbol` rows whose `location` came back
    /// as the `WorkspaceLocation` (URI-only) variant; the server
    /// returns the same symbol with `location.range` populated.
    /// Wired into the `:workspace-symbols` picker accept path
    /// when the server advertises
    /// `workspaceSymbolProvider.resolveProvider`.
    pub fn workspace_symbol_resolve(
        &self,
        symbol: lsp_types::WorkspaceSymbol,
        token: CancellationToken,
    ) -> Pending<lsp_types::WorkspaceSymbol> {
        self.request_with_cancel("workspaceSymbol/resolve", symbol, token)
    }

    /// 4.5.a: `textDocument/prepareCallHierarchy`. Asks the
    /// server which callable(s) live at the cursor; the
    /// response feeds the subsequent
    /// [`Self::call_hierarchy_incoming_calls`] /
    /// [`Self::call_hierarchy_outgoing_calls`] request.
    /// Servers typically return a single-element vec for a
    /// position inside a function body; macros / overloads
    /// can produce multiple items. `None` means "no callable
    /// here", which short-circuits the navigation.
    pub fn prepare_call_hierarchy(
        &self,
        params: lsp_types::CallHierarchyPrepareParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::CallHierarchyItem>>> {
        self.request_with_cancel("textDocument/prepareCallHierarchy", params, token)
    }

    /// 4.5.a: `callHierarchy/incomingCalls`. Given a
    /// `CallHierarchyItem` from `prepareCallHierarchy`,
    /// returns the call sites that invoke it. Each
    /// `CallHierarchyIncomingCall` carries the *caller* item
    /// (`from`) plus the ranges inside the caller where the
    /// call appears (`from_ranges`).
    pub fn call_hierarchy_incoming_calls(
        &self,
        params: lsp_types::CallHierarchyIncomingCallsParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::CallHierarchyIncomingCall>>> {
        self.request_with_cancel("callHierarchy/incomingCalls", params, token)
    }

    /// 4.5.a: `callHierarchy/outgoingCalls`. Symmetric peer
    /// of `incomingCalls`; given a callable, returns the
    /// callables it invokes plus the call sites inside its
    /// own body (`from_ranges` on the caller's text).
    pub fn call_hierarchy_outgoing_calls(
        &self,
        params: lsp_types::CallHierarchyOutgoingCallsParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::CallHierarchyOutgoingCall>>> {
        self.request_with_cancel("callHierarchy/outgoingCalls", params, token)
    }

    /// 4.5.b: `textDocument/prepareTypeHierarchy`. Same
    /// preparation shape as `prepareCallHierarchy` but
    /// targets type relationships (super/sub-types). Used by
    /// `:lsp-supertypes` / `:lsp-subtypes`.
    pub fn prepare_type_hierarchy(
        &self,
        params: lsp_types::TypeHierarchyPrepareParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::TypeHierarchyItem>>> {
        self.request_with_cancel("textDocument/prepareTypeHierarchy", params, token)
    }

    /// 4.5.b: `typeHierarchy/supertypes`. Returns the types
    /// the given item is a subtype of (e.g. trait
    /// supertraits, class superclasses).
    pub fn type_hierarchy_supertypes(
        &self,
        params: lsp_types::TypeHierarchySupertypesParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::TypeHierarchyItem>>> {
        self.request_with_cancel("typeHierarchy/supertypes", params, token)
    }

    /// 4.5.b: `typeHierarchy/subtypes`. Returns the types that
    /// subtype the given item (e.g. trait implementors,
    /// class subclasses).
    pub fn type_hierarchy_subtypes(
        &self,
        params: lsp_types::TypeHierarchySubtypesParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::TypeHierarchyItem>>> {
        self.request_with_cancel("typeHierarchy/subtypes", params, token)
    }

    /// 4.5.g: `textDocument/moniker`. Returns the stable
    /// cross-project identifier(s) for the symbol at the
    /// cursor -- e.g. SCIP / LSIF emit monikers so a build
    /// indexer can join symbols across repos. The response
    /// is `Option<Vec<Moniker>>`; each moniker has a `scheme`,
    /// `identifier`, optional `kind`, and `unique` level.
    /// `:lsp-moniker` ex-command surfaces the list as an echo.
    pub fn moniker(
        &self,
        params: lsp_types::MonikerParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::Moniker>>> {
        self.request_with_cancel("textDocument/moniker", params, token)
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

    /// 4.4.k: `workspace/didChangeConfiguration` (notification).
    /// Fan-out fires when any `lsp.*` typed option changes
    /// (via `OptionChanged` cascade). The notification's
    /// `settings` carries the full `lsp` subtree from the
    /// merged config TOML; most servers ignore the inline
    /// payload and pull fresh values via
    /// `workspace/configuration` (the host's drain serves
    /// from the same tree), but servers that read inline get
    /// the values too. Notification-only -- no response, no
    /// cancellation token.
    pub fn did_change_configuration(
        &self,
        params: lsp_types::DidChangeConfigurationParams,
    ) -> crate::error::LspResult<()> {
        self.notify("workspace/didChangeConfiguration", params)
    }

    /// 4.4.l: `workspace/didChangeWatchedFiles` (notification).
    /// Fan-out fires when the host's file-watcher observes an
    /// fs event whose path matches a glob from this server's
    /// `client/registerCapability`-issued
    /// `DidChangeWatchedFilesRegistrationOptions`. The host
    /// batches per-tick into one notification per server (the
    /// LSP spec allows multiple `FileEvent`s in one payload);
    /// servers receive the events in arrival order.
    /// Notification-only -- no response, no cancellation.
    pub fn did_change_watched_files(
        &self,
        params: lsp_types::DidChangeWatchedFilesParams,
    ) -> crate::error::LspResult<()> {
        self.notify("workspace/didChangeWatchedFiles", params)
    }

    /// 4.4.m: `workspace/willCreateFiles` (request).
    /// Pre-create hook -- server MAY return a `WorkspaceEdit`
    /// the client applies BEFORE the actual file is created on
    /// disk (e.g. add an import to a sibling module). Callers
    /// must gate on `Capabilities::supports_will_create_files`
    /// + filter the URIs against the registration's
    /// `FileOperationFilter`s before issuing. The host pump
    /// (when wired) blocks the create path on the response;
    /// timeouts skip the edits and proceed.
    pub fn will_create_files(
        &self,
        params: lsp_types::CreateFilesParams,
        cancel: lattice_protocol::CancellationToken,
    ) -> crate::pending::Pending<Option<lsp_types::WorkspaceEdit>> {
        self.request_with_cancel(
            "workspace/willCreateFiles",
            params,
            cancel,
        )
    }

    /// 4.4.m: `workspace/didCreateFiles` (notification).
    /// Post-create fan-out. The wire wrapper is straight-line;
    /// trigger discipline (when to fire) lives in the host
    /// save / create paths.
    pub fn did_create_files(
        &self,
        params: lsp_types::CreateFilesParams,
    ) -> crate::error::LspResult<()> {
        self.notify("workspace/didCreateFiles", params)
    }

    /// 4.4.m: `workspace/willRenameFiles` (request). Same
    /// response shape as `willCreateFiles`. Triggered when the
    /// user renames a file in-place (`:saveas` follow-up that
    /// removes the original); server returns edits to keep
    /// imports / references in sync with the new path.
    pub fn will_rename_files(
        &self,
        params: lsp_types::RenameFilesParams,
        cancel: lattice_protocol::CancellationToken,
    ) -> crate::pending::Pending<Option<lsp_types::WorkspaceEdit>> {
        self.request_with_cancel(
            "workspace/willRenameFiles",
            params,
            cancel,
        )
    }

    /// 4.4.m: `workspace/didRenameFiles` (notification).
    pub fn did_rename_files(
        &self,
        params: lsp_types::RenameFilesParams,
    ) -> crate::error::LspResult<()> {
        self.notify("workspace/didRenameFiles", params)
    }

    /// 4.4.m: `workspace/willDeleteFiles` (request). Server
    /// returns edits to clean up references before the delete.
    pub fn will_delete_files(
        &self,
        params: lsp_types::DeleteFilesParams,
        cancel: lattice_protocol::CancellationToken,
    ) -> crate::pending::Pending<Option<lsp_types::WorkspaceEdit>> {
        self.request_with_cancel(
            "workspace/willDeleteFiles",
            params,
            cancel,
        )
    }

    /// 4.4.m: `workspace/didDeleteFiles` (notification).
    pub fn did_delete_files(
        &self,
        params: lsp_types::DeleteFilesParams,
    ) -> crate::error::LspResult<()> {
        self.notify("workspace/didDeleteFiles", params)
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

    /// 4.4.e: `textDocument/documentHighlight`. Returns the
    /// references to the symbol at the cursor inside the
    /// current document; each entry carries an optional `kind`
    /// (`Text` / `Read` / `Write`) so the overlay can paint
    /// reads / writes differently. Response is `Vec<DocumentHighlight>`
    /// or null when the cursor isn't on a known symbol.
    pub fn document_highlight(
        &self,
        params: lsp_types::DocumentHighlightParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::DocumentHighlight>>> {
        self.request_with_cancel("textDocument/documentHighlight", params, token)
    }

    /// 4.4.e: `textDocument/selectionRange`. Given a slice of
    /// positions (almost always one cursor position), the server
    /// returns the structural ranges that surround each
    /// position, walking outward (token → expression → statement
    /// → block → function → module). The operator-side
    /// `expand-region` / `shrink-region` consumes the linked list.
    pub fn selection_range(
        &self,
        params: lsp_types::SelectionRangeParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::SelectionRange>>> {
        self.request_with_cancel("textDocument/selectionRange", params, token)
    }

    /// 4.4.f: `textDocument/foldingRange`. Returns line-based
    /// fold extents (with optional character columns + kind tag
    /// like `comment` / `imports` / `region`). The host's
    /// per-tick pump refreshes the cache when the document
    /// version bumps; `:set foldmethod=lsp` reads from the
    /// cache.
    pub fn folding_range(
        &self,
        params: lsp_types::FoldingRangeParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::FoldingRange>>> {
        self.request_with_cancel("textDocument/foldingRange", params, token)
    }

    /// 4.4.g: `textDocument/inlayHint`. Returns inline
    /// virtual-text annotations (type hints, parameter
    /// names, etc.) over the requested range. Each hint
    /// carries its position, a label (single string or
    /// composite `Vec<InlayHintLabelPart>`), and optional
    /// kind / padding / tooltip fields. The host caches the
    /// response per buffer-version and the renderer splices
    /// each hint into the line span list.
    pub fn inlay_hint(
        &self,
        params: lsp_types::InlayHintParams,
        token: CancellationToken,
    ) -> Pending<Option<Vec<lsp_types::InlayHint>>> {
        self.request_with_cancel("textDocument/inlayHint", params, token)
    }

    /// 4.4.g follow-up: `inlayHint/resolve`. Lazy-resolves a
    /// single inlay hint -- the server populates the
    /// `tooltip` and `text_edits` fields it skipped on the
    /// initial batched response. Servers gate this behind
    /// the `InlayHintOptions.resolve_provider` capability;
    /// callers must check [`crate::Capabilities::supports_inlay_hint_resolve`]
    /// before issuing. The wrapper ships ahead of the
    /// interaction UX (no gesture is wired to fire it today
    /// -- see lsp-features.md for the deferral rationale);
    /// this lets future work plug a trigger into a stable
    /// surface.
    pub fn inlay_hint_resolve(
        &self,
        hint: lsp_types::InlayHint,
        token: CancellationToken,
    ) -> Pending<lsp_types::InlayHint> {
        self.request_with_cancel("inlayHint/resolve", hint, token)
    }

    /// 4.4.h: `textDocument/semanticTokens/full`. Returns the
    /// whole-buffer semantic token list in the LSP
    /// relative-position varint encoding (5 u32s per token:
    /// deltaLine, deltaStart, length, tokenType, modifiers
    /// bitfield). The host decodes against the server's
    /// `SemanticTokensLegend` (cached at attach time via
    /// [`crate::Capabilities::semantic_token_types`] /
    /// `semantic_token_modifiers`). Wrapped response variant
    /// `Tokens` carries `data: Vec<u32>` plus an optional
    /// `result_id` for the 4.4.i delta path.
    pub fn semantic_tokens_full(
        &self,
        params: lsp_types::SemanticTokensParams,
        token: CancellationToken,
    ) -> Pending<Option<lsp_types::SemanticTokensResult>> {
        self.request_with_cancel("textDocument/semanticTokens/full", params, token)
    }

    /// 4.4.i: `textDocument/semanticTokens/full/delta`. Sends
    /// the previous response's `result_id`; server either
    /// returns a new full token list (`Tokens` variant) when
    /// it can't compute a delta cheaply, or a list of edit
    /// operations (`TokensDelta`) the host applies to the
    /// cached raw token vec. Edit shape:
    /// `SemanticTokensEdit { start, delete_count, data }`
    /// where `start` is the index into the previous flat
    /// token vec, `delete_count` is how many `SemanticToken`
    /// entries to remove, and `data` is what to splice in.
    /// The host re-decodes the spliced vec into absolute
    /// positions.
    pub fn semantic_tokens_full_delta(
        &self,
        params: lsp_types::SemanticTokensDeltaParams,
        token: CancellationToken,
    ) -> Pending<Option<lsp_types::SemanticTokensFullDeltaResult>> {
        self.request_with_cancel("textDocument/semanticTokens/full/delta", params, token)
    }

    /// 4.4.i: `textDocument/semanticTokens/range`. Viewport-
    /// bounded request -- the host can issue this for very
    /// large files to skip decoding tokens outside the
    /// visible window. Returns a plain `SemanticTokens`
    /// (no `Delta` variant for the range flavour).
    /// Exposed as a typed wrapper today; the v1 pump uses
    /// full/delta. Viewport-aware fetching can switch over
    /// in a follow-up without re-touching the wire path.
    pub fn semantic_tokens_range(
        &self,
        params: lsp_types::SemanticTokensRangeParams,
        token: CancellationToken,
    ) -> Pending<Option<lsp_types::SemanticTokensRangeResult>> {
        self.request_with_cancel("textDocument/semanticTokens/range", params, token)
    }

    /// 4.4.j: `textDocument/diagnostic`. Pull-based
    /// diagnostics (LSP 3.17). The server returns either a
    /// `Full` report (entire diagnostics list for the URI,
    /// plus optional `result_id` for the next delta) or an
    /// `Unchanged` report ("no diagnostics moved since the
    /// previous `result_id`"). The host caches the
    /// `result_id` per buffer-version and threads it back in
    /// `DocumentDiagnosticParams.previous_result_id` so the
    /// server can answer `Unchanged` cheaply. Used when a
    /// server prefers pull over push, or alongside push for
    /// servers that support both.
    pub fn document_diagnostic(
        &self,
        params: lsp_types::DocumentDiagnosticParams,
        token: CancellationToken,
    ) -> Pending<lsp_types::DocumentDiagnosticReportResult> {
        self.request_with_cancel("textDocument/diagnostic", params, token)
    }

    /// 4.4.j: `workspace/diagnostic`. Workspace-wide pull --
    /// returns reports for every URI the server tracks
    /// diagnostics for, even ones the client hasn't opened.
    /// Wrapper ships ahead of the host pump (strong-reason
    /// deferred; the per-document pump already covers every
    /// open buffer's diagnostics, and the closed-file
    /// workspace pull is niche -- see lsp-features.md). The
    /// callable exists so future workspace-view rework has a
    /// stable surface.
    pub fn workspace_diagnostic(
        &self,
        params: lsp_types::WorkspaceDiagnosticParams,
        token: CancellationToken,
    ) -> Pending<lsp_types::WorkspaceDiagnosticReportResult> {
        self.request_with_cancel("workspace/diagnostic", params, token)
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
        drop::<Pending<Option<lsp_types::WorkspaceSymbolResponse>>>(handle.workspace_symbol(
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
