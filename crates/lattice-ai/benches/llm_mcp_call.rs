//! LL.24 -- one external tool call through the broker and rmcp's client.
//!
//! The model's call to `bench__echo` as the tool loop makes it: through the
//! offer (an allowed tool, so nothing asks), `McpServer::call`, rmcp's client,
//! an in-memory pipe, an in-process rmcp server, and back. The server does no
//! work of its own, so the number is what lattice and the protocol add to an
//! external tool, beside LL.22's `mcp_call` for the server side. Off the
//! editor thread.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_ai::llm::tools::CallContext;
use lattice_ai::llm::tools::broker::ToolBroker;
use lattice_ai::llm::tools::mcp::{Connect, Connection, McpServer, McpServerConfig, McpTransport};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ListToolsResult,
    PaginatedRequestParams, ServerConfig,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData, ServerHandler};
use serde_json::json;

#[derive(Clone)]
struct Echo;

impl ServerHandler for Echo {
    fn get_info(&self) -> ServerConfig {
        serde_json::from_value(json!({
            "protocolVersion": "2025-06-18",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "bench", "version": "0" },
        }))
        .expect("server info")
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(serde_json::from_value(json!({ "tools": [{
            "name": "echo", "description": "echo", "inputSchema": { "type": "object" }
        }] }))
        .expect("tool list"))
    }

    async fn call_tool(
        &self,
        _request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let r: CallToolResult =
            serde_json::from_value(json!({ "content": [{ "type": "text", "text": "ok" }] }))
                .expect("result");
        Ok(r.into())
    }
}

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let connect: Connect = Arc::new(|_config, client| {
        Box::pin(async move {
            let (client_io, server_io) = tokio::io::duplex(64 * 1024);
            tokio::spawn(async move {
                if let Ok(running) = rmcp::serve_server(Echo, server_io).await {
                    let _ = running.waiting().await;
                }
            });
            rmcp::serve_client(client, client_io)
                .await
                .map(Connection::new)
                .map_err(|e| e.to_string())
        })
    });
    let config = McpServerConfig {
        name: "bench".to_string(),
        transport: McpTransport::Stdio {
            command: "in-process".to_string(),
            args: vec![],
            env: BTreeMap::new(),
            cwd: None,
        },
        allow: BTreeSet::from(["echo".to_string()]),
        call_timeout: lattice_ai::llm::tools::mcp::DEFAULT_CALL_TIMEOUT,
    };
    let offer = rt.block_on(async {
        let broker = ToolBroker::new();
        broker.add_source(
            "bench",
            Arc::new(McpServer::new(
                config,
                connect,
                tokio::runtime::Handle::current(),
                Arc::new(|| {}),
            )),
        );
        broker.offer(&BTreeSet::from(["bench.*".to_string()])).await
    });
    assert_eq!(offer.tools().len(), 1, "the server started and listed echo");
    c.bench_function("llm_mcp_call/allowed_echo", |b| {
        b.iter(|| {
            rt.block_on(async {
                offer
                    .call("bench__echo", json!({}), CallContext::default())
                    .await
                    .expect("the call returns")
            })
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
