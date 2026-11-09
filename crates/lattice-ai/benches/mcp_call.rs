//! LL.22 -- one MCP tool call through rmcp.
//!
//! The Claude Code connection now runs on rmcp. This measures what rmcp adds
//! to a call: a
//! newline-delimited `tools/call` written to an in-memory pipe, decoded,
//! handled on its own task, encoded, and read back. The tool is
//! `getWorkspaceFolders`, which does no work of its own, so the number is the
//! protocol's cost, not a tool's. Off the editor thread.

use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_ai::mcp::lattice_server::{LatticeServer, ToolContext};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn ctx() -> ToolContext {
    ToolContext {
        conn_id: 0,
        reads: lattice_ai::mcp::reads::ReadContext {
            editor: lattice_agent::EditorAccess::new(
                Arc::new(std::sync::Mutex::new(
                    lattice_agent::EditorStateCache::default(),
                )),
                None,
                vec!["/work".to_string()],
                None,
            ),
            diagnostics: None,
        },
        diff: None,
        review: lattice_ai::mcp::status::ReviewState::new(Arc::new(tokio::sync::Notify::new())),
    }
}

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("a runtime");
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (read, mut write) = tokio::io::split(client);
    let mut lines = BufReader::new(read).lines();
    let running = rt.block_on(async {
        let serve = tokio::spawn(rmcp::service::serve_server(LatticeServer::new(ctx()), server));
        let init = r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"bench","version":"0"}}}"#;
        write.write_all(format!("{init}\n").as_bytes()).await.expect("write");
        lines.next_line().await.expect("read").expect("initialize answered");
        write
            .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
            .await
            .expect("write");
        // `tools/list_changed` follows `initialized`.
        lines.next_line().await.expect("read").expect("list_changed");
        serve.await.expect("join").expect("served")
    });
    let call = b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"getWorkspaceFolders\",\"arguments\":{}}}\n";
    c.bench_function("mcp_call/get_workspace_folders", |b| {
        b.iter(|| {
            rt.block_on(async {
                write.write_all(call).await.expect("write");
                lines.next_line().await.expect("read").expect("answered")
            })
        })
    });
    drop(running);
}

criterion_group!(benches, bench);
criterion_main!(benches);
