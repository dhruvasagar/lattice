//! Phase 4.2.a — typed wrapper integration tests against MockServer
//! (DESIGN.md §5.4 / Phase 4.2).
//!
//! Each wrapper sends a typed request through the actor's mailbox,
//! receives a canned response from the mock, and round-trips
//! through `serde_json` into the `lsp-types` typed shape. The
//! cancellation test confirms the relay task observes a flipped
//! token before the response arrives.
#![allow(clippy::unwrap_used, clippy::panic)]

mod common;

use std::str::FromStr;
use std::time::Duration;

use lattice_protocol::CancellationToken;
use lattice_runtime::block_on;
use lsp_types::{
    DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse,
    Hover, HoverContents, HoverParams, Location, MarkedString, Position as LspPosition,
    Range as LspRange, ReferenceContext, ReferenceParams, SymbolInformation, SymbolKind,
    TextDocumentIdentifier, TextDocumentPositionParams, Uri, WorkspaceSymbolParams,
};
use serde_json::json;

use common::{MockResult, MockServer};

fn fake_uri() -> Uri {
    Uri::from_str("file:///tmp/test.rs").unwrap()
}

fn position(line: u32, character: u32) -> LspPosition {
    LspPosition { line, character }
}

fn position_params(line: u32, character: u32) -> TextDocumentPositionParams {
    TextDocumentPositionParams {
        text_document: TextDocumentIdentifier { uri: fake_uri() },
        position: position(line, character),
    }
}

#[tokio::test]
async fn hover_round_trips_through_typed_wrapper() {
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/hover", |_params| {
            MockResult::Ok(json!({
                "contents": "fn foo() -> u32",
                "range": {
                    "start": {"line": 0, "character": 0},
                    "end": {"line": 0, "character": 3}
                }
            }))
        })
        .await;

    let result = mock
        .handle
        .hover(
            HoverParams {
                text_document_position_params: position_params(0, 0),
                work_done_progress_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("hover succeeds");
    let hover: Hover = result.expect("server returned a hover body");
    match hover.contents {
        HoverContents::Scalar(MarkedString::String(s)) => {
            assert_eq!(s, "fn foo() -> u32");
        }
        other => panic!("expected scalar string, got {other:?}"),
    }
}

#[tokio::test]
async fn goto_definition_returns_single_location() {
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/definition", |_params| {
            MockResult::Ok(json!({
                "uri": "file:///tmp/lib.rs",
                "range": {
                    "start": {"line": 10, "character": 4},
                    "end": {"line": 10, "character": 7}
                }
            }))
        })
        .await;

    let result = mock
        .handle
        .goto_definition(
            GotoDefinitionParams {
                text_document_position_params: position_params(0, 0),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("definition succeeds");
    match result.expect("server returned a definition") {
        GotoDefinitionResponse::Scalar(loc) => {
            assert_eq!(loc.range.start.line, 10);
        }
        other => panic!("expected scalar Location, got {other:?}"),
    }
}

#[tokio::test]
async fn references_returns_array_of_locations() {
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/references", |_params| {
            MockResult::Ok(json!([
                {
                    "uri": "file:///tmp/a.rs",
                    "range": {
                        "start": {"line": 1, "character": 2},
                        "end": {"line": 1, "character": 5}
                    }
                },
                {
                    "uri": "file:///tmp/b.rs",
                    "range": {
                        "start": {"line": 99, "character": 0},
                        "end": {"line": 99, "character": 3}
                    }
                }
            ]))
        })
        .await;

    let locs: Vec<Location> = mock
        .handle
        .references(
            ReferenceParams {
                text_document_position: position_params(0, 0),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: ReferenceContext {
                    include_declaration: true,
                },
            },
            CancellationToken::never(),
        )
        .await
        .expect("references succeeds")
        .expect("server returned locations");
    assert_eq!(locs.len(), 2);
    assert_eq!(locs[1].range.start.line, 99);
}

#[tokio::test]
async fn document_symbol_returns_flat_information_list() {
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/documentSymbol", |_params| {
            // Legacy SymbolInformation shape (flat list).
            MockResult::Ok(json!([
                {
                    "name": "foo",
                    "kind": 12, // Function
                    "location": {
                        "uri": "file:///tmp/test.rs",
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 5, "character": 1}
                        }
                    }
                }
            ]))
        })
        .await;

    let result = mock
        .handle
        .document_symbol(
            DocumentSymbolParams {
                text_document: TextDocumentIdentifier { uri: fake_uri() },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("documentSymbol succeeds");
    match result.expect("non-empty response") {
        DocumentSymbolResponse::Flat(syms) => {
            assert_eq!(syms.len(), 1);
            assert_eq!(syms[0].name, "foo");
            assert_eq!(syms[0].kind, SymbolKind::FUNCTION);
        }
        other => panic!("expected Flat list, got {other:?}"),
    }
}

#[tokio::test]
async fn workspace_symbol_returns_legacy_symbol_information() {
    let mock = MockServer::start().await;
    mock.mock
        .on("workspace/symbol", |_params| {
            MockResult::Ok(json!([
                {
                    "name": "Foo",
                    "kind": 5, // Class
                    "location": {
                        "uri": "file:///tmp/lib.rs",
                        "range": {
                            "start": {"line": 0, "character": 0},
                            "end": {"line": 0, "character": 3}
                        }
                    }
                }
            ]))
        })
        .await;

    let resp = mock
        .handle
        .workspace_symbol(
            WorkspaceSymbolParams {
                query: "Foo".into(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("workspace/symbol succeeds")
        .expect("server returned symbols");
    let syms: Vec<SymbolInformation> = match resp {
        lsp_types::WorkspaceSymbolResponse::Flat(s) => s,
        lsp_types::WorkspaceSymbolResponse::Nested(_) => {
            panic!("expected Flat shape from this mock fixture")
        }
    };
    assert_eq!(syms.len(), 1);
    assert_eq!(syms[0].name, "Foo");
}

#[tokio::test]
async fn workspace_symbol_returns_nested_workspace_symbol_with_workspace_location() {
    // Modern LSP 3.17+ shape: server returns a
    // `WorkspaceSymbol` whose `location` is the
    // `WorkspaceLocation` (URI only) variant -- the editor is
    // expected to fire `workspaceSymbol/resolve` on accept.
    let mock = MockServer::start().await;
    mock.mock
        .on("workspace/symbol", |_params| {
            MockResult::Ok(json!([
                {
                    "name": "Bar",
                    "kind": 5,
                    "location": {
                        "uri": "file:///tmp/other.rs"
                    }
                }
            ]))
        })
        .await;
    let resp = mock
        .handle
        .workspace_symbol(
            WorkspaceSymbolParams {
                query: "Bar".into(),
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("workspace/symbol succeeds")
        .expect("server returned symbols");
    match resp {
        lsp_types::WorkspaceSymbolResponse::Nested(syms) => {
            assert_eq!(syms.len(), 1);
            assert_eq!(syms[0].name, "Bar");
            assert!(matches!(
                syms[0].location,
                lsp_types::OneOf::Right(_),
            ));
        }
        lsp_types::WorkspaceSymbolResponse::Flat(_) => {
            panic!("expected Nested shape -- payload had no `range`")
        }
    }
}

#[tokio::test]
async fn workspace_symbol_resolve_fills_in_range() {
    // The server returns a Nested symbol with WorkspaceLocation
    // and resolves it on the follow-up `workspaceSymbol/resolve`
    // by upgrading the location to a full `Location` with range.
    let mock = MockServer::start().await;
    mock.mock
        .on("workspaceSymbol/resolve", |_params| {
            MockResult::Ok(json!({
                "name": "Bar",
                "kind": 5,
                "location": {
                    "uri": "file:///tmp/other.rs",
                    "range": {
                        "start": {"line": 7, "character": 3},
                        "end": {"line": 7, "character": 6}
                    }
                }
            }))
        })
        .await;
    let unresolved = lsp_types::WorkspaceSymbol {
        name: "Bar".into(),
        kind: lsp_types::SymbolKind::CLASS,
        tags: None,
        container_name: None,
        location: lsp_types::OneOf::Right(lsp_types::WorkspaceLocation {
            uri: "file:///tmp/other.rs".parse().unwrap(),
        }),
        data: None,
    };
    let resolved = mock
        .handle
        .workspace_symbol_resolve(unresolved, CancellationToken::never())
        .await
        .expect("workspaceSymbol/resolve succeeds");
    let lsp_types::OneOf::Left(loc) = resolved.location else {
        panic!("resolve returned still-unresolved shape")
    };
    assert_eq!(loc.range.start.line, 7);
    assert_eq!(loc.range.start.character, 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_token_resolves_request_with_cancelled_error() {
    // Slow handler: `std::thread::sleep` blocks the mock task long
    // enough for the cancellation poll loop to observe a flipped
    // token before the response arrives. Multi-thread runtime so
    // the std-sleep doesn't starve the other tasks.
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/hover", |_params| {
            std::thread::sleep(Duration::from_millis(500));
            MockResult::Ok(json!({"contents": "would have arrived too late"}))
        })
        .await;

    let token = CancellationToken::new();
    let cancel_handle = token.clone();
    let pending = mock.handle.hover(
        HoverParams {
            text_document_position_params: position_params(0, 0),
            work_done_progress_params: Default::default(),
        },
        token,
    );
    tokio::spawn(async move {
        // Flip the token before the slow handler finishes. The
        // relay polls the token every 10ms; observing a cancel
        // takes one tick.
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel_handle.cancel();
    });
    let result = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .expect("relay should resolve within 2s");
    assert!(
        matches!(result, Err(lattice_lsp::LspError::Cancelled)),
        "expected Cancelled, got {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn already_cancelled_token_resolves_within_one_poll_window() {
    // Pre-cancelled token + slow handler: the relay's first poll
    // observes the flipped flag and resolves immediately,
    // independent of when the (slow) response arrives.
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/hover", |_params| {
            std::thread::sleep(Duration::from_millis(500));
            MockResult::Ok(json!({"contents": "stale"}))
        })
        .await;

    let token = CancellationToken::new();
    token.cancel();
    let pending = mock.handle.hover(
        HoverParams {
            text_document_position_params: position_params(0, 0),
            work_done_progress_params: Default::default(),
        },
        token,
    );
    let result = tokio::time::timeout(Duration::from_millis(200), pending)
        .await
        .expect("relay should resolve within 200ms of a pre-cancelled token");
    assert!(matches!(result, Err(lattice_lsp::LspError::Cancelled)));
}

#[test]
fn block_on_drives_the_async_path() {
    // Sanity check that the typed wrapper works through the
    // editor's sync bridge (`block_on`), not just inside an async
    // test runtime. The App's keystroke handlers are sync; they
    // drive `Pending::await` through `block_on`.
    block_on(async {
        let mock = MockServer::start().await;
        mock.mock
            .on("textDocument/hover", |_params| {
                MockResult::Ok(json!({"contents": "x"}))
            })
            .await;
        let result = mock
            .handle
            .hover(
                HoverParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: Uri::from_str("file:///tmp/t.rs").unwrap(),
                        },
                        position: LspPosition {
                            line: 0,
                            character: 0,
                        },
                    },
                    work_done_progress_params: Default::default(),
                },
                CancellationToken::never(),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result.contents, HoverContents::Scalar(_)));
        // Touch a couple of unused-warning spots so the imports
        // stay justified for the remaining feature tests above.
        let _ = LspRange {
            start: position(0, 0),
            end: position(0, 1),
        };
    });
}

/// 4.4.k: `did_change_configuration` is a notification (no
/// response). The mock receives it in its notification log
/// with the `settings` JSON tree we hand in.
#[tokio::test]
async fn did_change_configuration_notifies_with_settings() {
    let mock = MockServer::start().await;
    let baseline = mock.mock.notifications().await.len();
    mock.handle
        .did_change_configuration(lsp_types::DidChangeConfigurationParams {
            settings: json!({
                "rust-analyzer": {
                    "checkOnSave": true,
                    "cargo": { "features": ["foo", "bar"] }
                }
            }),
        })
        .expect("notification queued");
    // The notification fires via the actor's outbound channel; the
    // wire flush is asynchronous, so give it a tick to land.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let notes = mock.mock.notifications().await;
    let new_notes = &notes[baseline..];
    let our = new_notes
        .iter()
        .find(|n| n.method == "workspace/didChangeConfiguration")
        .expect("mock received didChangeConfiguration");
    let params = our.params.as_ref().expect("notification carries params");
    let settings = &params["settings"];
    assert_eq!(settings["rust-analyzer"]["checkOnSave"], json!(true));
    assert_eq!(
        settings["rust-analyzer"]["cargo"]["features"],
        json!(["foo", "bar"]),
    );
}

/// 4.4.m: `did_create_files` is a notification with the file
/// list payload; the mock records it in arrival order.
#[tokio::test]
async fn did_create_files_notifies_with_file_list() {
    let mock = MockServer::start().await;
    let baseline = mock.mock.notifications().await.len();
    mock.handle
        .did_create_files(lsp_types::CreateFilesParams {
            files: vec![lsp_types::FileCreate {
                uri: "file:///tmp/newfile.rs".into(),
            }],
        })
        .expect("notification queued");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let notes = mock.mock.notifications().await;
    let our = notes[baseline..]
        .iter()
        .find(|n| n.method == "workspace/didCreateFiles")
        .expect("mock received didCreateFiles");
    let params = our.params.as_ref().expect("notification carries params");
    assert_eq!(params["files"][0]["uri"], "file:///tmp/newfile.rs");
}

/// 4.4.m: `will_rename_files` is a request whose response is
/// `Option<WorkspaceEdit>`. A server returning `null` resolves
/// the relay with `Ok(None)`.
#[tokio::test]
async fn will_rename_files_round_trips_workspace_edit_option() {
    let mock = MockServer::start().await;
    mock.mock
        .on("workspace/willRenameFiles", |_params| {
            MockResult::Ok(json!({
                "changes": {
                    "file:///tmp/old.rs": [{
                        "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 3}},
                        "newText": "use new::name"
                    }]
                }
            }))
        })
        .await;
    let edit = mock
        .handle
        .will_rename_files(
            lsp_types::RenameFilesParams {
                files: vec![lsp_types::FileRename {
                    old_uri: "file:///tmp/old.rs".into(),
                    new_uri: "file:///tmp/new.rs".into(),
                }],
            },
            CancellationToken::never(),
        )
        .await
        .expect("response decodes");
    let workspace_edit = edit.expect("server returned an edit");
    let changes = workspace_edit.changes.expect("legacy changes map");
    assert_eq!(changes.len(), 1);
}

/// 4.4.m: server returning `null` to a will* request means
/// "no edits needed"; the relay resolves with `Ok(None)`.
#[tokio::test]
async fn will_delete_files_handles_null_response() {
    let mock = MockServer::start().await;
    mock.mock
        .on("workspace/willDeleteFiles", |_params| {
            MockResult::Ok(json!(null))
        })
        .await;
    let edit = mock
        .handle
        .will_delete_files(
            lsp_types::DeleteFilesParams {
                files: vec![lsp_types::FileDelete {
                    uri: "file:///tmp/gone.rs".into(),
                }],
            },
            CancellationToken::never(),
        )
        .await
        .expect("response decodes");
    assert!(edit.is_none(), "null response surfaces as None");
}

/// 4.5.a: `prepare_call_hierarchy` round-trips a single
/// `CallHierarchyItem` from the server.
#[tokio::test]
async fn prepare_call_hierarchy_returns_item_at_cursor() {
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/prepareCallHierarchy", |_params| {
            MockResult::Ok(json!([{
                "name": "foo",
                "kind": 12,
                "uri": "file:///tmp/lib.rs",
                "range": {
                    "start": {"line": 10, "character": 0},
                    "end": {"line": 10, "character": 8}
                },
                "selectionRange": {
                    "start": {"line": 10, "character": 4},
                    "end": {"line": 10, "character": 7}
                }
            }]))
        })
        .await;
    let items = mock
        .handle
        .prepare_call_hierarchy(
            lsp_types::CallHierarchyPrepareParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Uri::from_str("file:///tmp/lib.rs").unwrap(),
                    },
                    position: LspPosition { line: 10, character: 4 },
                },
                work_done_progress_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("response decodes")
        .expect("server returned items");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "foo");
    assert_eq!(items[0].selection_range.start.line, 10);
}

/// 4.5.a: `call_hierarchy_incoming_calls` returns the
/// `IncomingCall { from, from_ranges }` shape with the
/// caller item plus call-site ranges.
#[tokio::test]
async fn call_hierarchy_incoming_calls_returns_callers() {
    let mock = MockServer::start().await;
    mock.mock
        .on("callHierarchy/incomingCalls", |_params| {
            MockResult::Ok(json!([{
                "from": {
                    "name": "bar",
                    "kind": 12,
                    "uri": "file:///tmp/lib.rs",
                    "range": {
                        "start": {"line": 20, "character": 0},
                        "end": {"line": 22, "character": 1}
                    },
                    "selectionRange": {
                        "start": {"line": 20, "character": 4},
                        "end": {"line": 20, "character": 7}
                    }
                },
                "fromRanges": [{
                    "start": {"line": 21, "character": 8},
                    "end": {"line": 21, "character": 11}
                }]
            }]))
        })
        .await;
    let item = lsp_types::CallHierarchyItem {
        name: "foo".into(),
        kind: lsp_types::SymbolKind::FUNCTION,
        tags: None,
        detail: None,
        uri: Uri::from_str("file:///tmp/lib.rs").unwrap(),
        range: LspRange {
            start: LspPosition { line: 10, character: 0 },
            end: LspPosition { line: 10, character: 8 },
        },
        selection_range: LspRange {
            start: LspPosition { line: 10, character: 4 },
            end: LspPosition { line: 10, character: 7 },
        },
        data: None,
    };
    let calls = mock
        .handle
        .call_hierarchy_incoming_calls(
            lsp_types::CallHierarchyIncomingCallsParams {
                item,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("response decodes")
        .expect("server returned calls");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].from.name, "bar");
    assert_eq!(calls[0].from_ranges[0].start.line, 21);
}

/// 4.5.b: `prepare_type_hierarchy` + `type_hierarchy_supertypes`
/// round-trip a `TypeHierarchyItem` list end-to-end.
#[tokio::test]
async fn type_hierarchy_supertypes_returns_parent_types() {
    let mock = MockServer::start().await;
    mock.mock
        .on("textDocument/prepareTypeHierarchy", |_params| {
            MockResult::Ok(json!([{
                "name": "MyTrait",
                "kind": 11,
                "uri": "file:///tmp/lib.rs",
                "range": {
                    "start": {"line": 5, "character": 0},
                    "end": {"line": 5, "character": 12}
                },
                "selectionRange": {
                    "start": {"line": 5, "character": 6},
                    "end": {"line": 5, "character": 13}
                }
            }]))
        })
        .await;
    mock.mock
        .on("typeHierarchy/supertypes", |_params| {
            MockResult::Ok(json!([{
                "name": "ParentTrait",
                "kind": 11,
                "uri": "file:///tmp/parent.rs",
                "range": {
                    "start": {"line": 1, "character": 0},
                    "end": {"line": 1, "character": 16}
                },
                "selectionRange": {
                    "start": {"line": 1, "character": 6},
                    "end": {"line": 1, "character": 17}
                }
            }]))
        })
        .await;
    // Prepare.
    let items = mock
        .handle
        .prepare_type_hierarchy(
            lsp_types::TypeHierarchyPrepareParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: Uri::from_str("file:///tmp/lib.rs").unwrap(),
                    },
                    position: LspPosition { line: 5, character: 6 },
                },
                work_done_progress_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("response decodes")
        .expect("server returned items");
    assert_eq!(items.len(), 1);
    let item = items.into_iter().next().unwrap();
    // Supertypes.
    let supers = mock
        .handle
        .type_hierarchy_supertypes(
            lsp_types::TypeHierarchySupertypesParams {
                item,
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            },
            CancellationToken::never(),
        )
        .await
        .expect("response decodes")
        .expect("server returned supertypes");
    assert_eq!(supers.len(), 1);
    assert_eq!(supers[0].name, "ParentTrait");
}
