//! AT.14: `lattice --mcp`, the binary itself, reaches a listening editor's
//! plugin tools the way an agent's stdio MCP client would: it finds the
//! editor's lock under the config home, and its stdout carries nothing but
//! the MCP replies.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;

use lattice_ai::llm::tools::plugin::{
    PluginTool, PluginToolKind, PluginToolResult, PluginTools, PluginToolsHandle, Reach, ToolPlugin,
};
use lattice_ai::llm::tools::{
    BoxFuture, CallContext, SourceTool, ToolBroker, ToolBrokerHandle, ToolError, ToolOutput,
    ToolSource,
};
use lattice_ai::mcp::tools_listener::ToolsListener;
use lattice_ai::mcp::tools_server::PluginToolsServer;

struct Org;

impl ToolPlugin for Org {
    fn tools(&self) -> BoxFuture<'_, Result<Vec<PluginTool>, String>> {
        Box::pin(async {
            Ok(vec![PluginTool {
                name: "agenda".into(),
                description: "What is on today.".into(),
                input_schema: r#"{"type":"object"}"#.into(),
                kind: PluginToolKind::Read,
            }])
        })
    }

    fn call(&self, _: &str, _: String) -> BoxFuture<'_, Result<PluginToolResult, String>> {
        Box::pin(async {
            Ok(PluginToolResult {
                text: "Standup at 09:30".into(),
                proposals: Vec::new(),
            })
        })
    }
}

/// Lattice's own tools: none, and no edits.
struct NoLattice;

impl ToolSource for NoLattice {
    fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn call(
        &self,
        _: &str,
        _: serde_json::Value,
        _: CallContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        Box::pin(async { Err(ToolError::Failed("no edits".into())) })
    }
}

/// An editor's listener over org's tools, its lock under `config_home`.
async fn listen(config_home: &Path, root: &Path) -> ToolsListener {
    let broker: ToolBrokerHandle = Arc::new(ToolBroker::new());
    let plugins: PluginToolsHandle = Arc::new(PluginTools::new(
        broker.clone(),
        Arc::new(NoLattice),
        Arc::new(Vec::new),
    ));
    plugins
        .register("org", Arc::new(Org), Reach::Contained)
        .await
        .unwrap();
    let dir = lattice_config::cache_home_from(Some(config_home))
        .unwrap()
        .join("mcp");
    ToolsListener::start(
        PluginToolsServer::new(broker, plugins, Arc::new(|| None), 0),
        root,
        &dir,
        Arc::new(|_| {}),
        tokio::sync::broadcast::channel(4).0,
    )
    .await
    .unwrap()
}

/// `lattice --mcp` in `cwd`, with `config_home` and `lock` (for
/// `LATTICE_MCP_LOCK`) in its environment.
fn bridge(config_home: &Path, cwd: &Path, lock: Option<&Path>) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_lattice"));
    cmd.arg("--mcp")
        .current_dir(cwd)
        .env("XDG_CONFIG_HOME", config_home)
        .env_remove("LATTICE_MCP_LOCK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(lock) = lock {
        cmd.env("LATTICE_MCP_LOCK", lock);
    }
    cmd
}

/// Initialize, call `org__agenda`, close stdin: every stdout line is a
/// JSON-RPC message, the call answers with org's text, and the bridge exits
/// cleanly once the client is done.
fn converse(mut cmd: Command) {
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for line in [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"org__agenda","arguments":{}}}"#,
    ] {
        writeln!(stdin, "{line}").unwrap();
    }
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let mut answered = false;
    for line in stdout.lines() {
        let line = line.unwrap();
        assert!(
            line.starts_with('{') && line.contains(r#""jsonrpc":"2.0""#),
            "stdout carries only MCP: {line}"
        );
        if line.contains(r#""id":2"#) {
            assert!(line.contains("Standup at 09:30"), "{line}");
            answered = true;
            break;
        }
    }
    assert!(answered, "no reply to the call");
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
}

fn scratch() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("config");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(project.join("src")).unwrap();
    (dir, home, project)
}

/// Started inside the editor's project, the bridge finds it by its lock.
#[tokio::test(flavor = "multi_thread")]
async fn the_bridge_finds_the_editor_of_the_project_it_starts_in() {
    let (_dir, home, project) = scratch();
    let _listener = listen(&home, &project).await;
    let cmd = bridge(&home, &project.join("src"), None);
    tokio::task::spawn_blocking(move || converse(cmd))
        .await
        .unwrap();
}

/// `LATTICE_MCP_LOCK` names the editor outright, wherever the bridge runs.
#[tokio::test(flavor = "multi_thread")]
async fn the_lock_in_the_environment_names_the_editor() {
    let (_dir, home, project) = scratch();
    let listener = listen(&home, &project).await;
    let elsewhere = tempfile::tempdir().unwrap();
    let cmd = bridge(
        &elsewhere.path().join("no-config"),
        elsewhere.path(),
        Some(listener.lock_path()),
    );
    tokio::task::spawn_blocking(move || converse(cmd))
        .await
        .unwrap();
}

/// With no editor serving, the bridge exits non-zero and says what to turn
/// on, on stderr, leaving stdout empty.
#[test]
fn with_no_editor_the_bridge_says_what_to_turn_on() {
    let (_dir, home, project) = scratch();
    let out = bridge(&home, &project, None)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        out.stdout.is_empty(),
        "{:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("llm.serve_tools"), "{err}");
}
