//! AT.14: `lattice --mcp`, the stdio bridge an agent runs to reach a running
//! editor's plugin tools.
//!
//! An MCP client starts it as a stdio server (`claude mcp add lattice --
//! lattice --mcp`); it finds the editor, sends the listener's token, and from
//! then on copies bytes both ways. It speaks no MCP itself, so it cannot
//! disagree with the server about the protocol, and a newer client talks to
//! the editor exactly as it would to any stdio server.
//!
//! **Which editor.** The one named by `LATTICE_MCP_LOCK`, when set: lattice
//! sets it for the agents it starts, so an agent always reaches the editor
//! that started it. Otherwise the one whose project holds the directory the
//! bridge was started in (the deepest, when projects nest), then the most
//! recently started. A lock whose editor is gone is skipped.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::net::TcpStream;

use crate::mcp::tools_listener::{ToolsLock, lock_dir};

/// The lock file of the editor a bridge must reach.
pub const LOCK_ENV: &str = "LATTICE_MCP_LOCK";

/// How long one editor has to accept before the next is tried.
const CONNECT_WAIT: Duration = Duration::from_secs(2);

fn read_lock(path: &Path) -> std::io::Result<ToolsLock> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

/// The editors whose locks are in `dir`, best first for a bridge started in
/// `cwd`: a project holding `cwd`, deepest first, then the newest lock.
pub fn candidates(dir: &Path, cwd: &Path) -> Vec<ToolsLock> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(usize, SystemTime, ToolsLock)> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let lock = read_lock(&p).ok()?;
            let at = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            let root = Path::new(&lock.root);
            let depth = if !lock.root.is_empty() && cwd.starts_with(root) {
                root.components().count()
            } else {
                0
            };
            Some((depth, at, lock))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    found.into_iter().map(|(_, _, lock)| lock).collect()
}

/// Connect to the first editor in `locks` that accepts, and send its token.
pub async fn connect(locks: &[ToolsLock]) -> std::io::Result<TcpStream> {
    for lock in locks {
        let attempt =
            tokio::time::timeout(CONNECT_WAIT, TcpStream::connect(("127.0.0.1", lock.port))).await;
        let Ok(Ok(mut stream)) = attempt else {
            continue;
        };
        stream
            .write_all(format!("{}\n", lock.token).as_bytes())
            .await?;
        return Ok(stream);
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "no running lattice answered; is it still open with `llm.serve_tools` on?",
    ))
}

/// Copy `input` to the editor and the editor to `output` until the editor
/// closes the connection. The end of `input` closes the editor's side of it,
/// which ends the session there and so the relay.
pub async fn relay<I, O>(stream: TcpStream, mut input: I, mut output: O) -> std::io::Result<()>
where
    I: AsyncRead + Unpin + Send + 'static,
    O: AsyncWrite + Unpin,
{
    let (mut from_editor, mut to_editor) = stream.into_split();
    let up = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut input, &mut to_editor).await;
        let _ = to_editor.shutdown().await;
    });
    let mut buf = vec![0u8; 64 * 1024];
    let ended = loop {
        match from_editor.read(&mut buf).await {
            Ok(0) => break Ok(()),
            Ok(n) => {
                // Flushed per read: a client waits for each reply whole.
                if let Err(e) = async {
                    output.write_all(&buf[..n]).await?;
                    output.flush().await
                }
                .await
                {
                    break Err(e);
                }
            }
            Err(e) => break Err(e),
        }
    };
    up.abort();
    ended
}

/// The editors a bridge started here may reach, best first.
fn locks_here() -> std::io::Result<(Vec<ToolsLock>, PathBuf)> {
    if let Some(path) = std::env::var_os(LOCK_ENV) {
        let path = PathBuf::from(path);
        let lock = read_lock(&path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!(
                    "{LOCK_ENV}={}: {e}; the editor that started this agent is gone or no longer serves tools",
                    path.display()
                ),
            )
        })?;
        return Ok((vec![lock], path));
    }
    let dir = lock_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config home, so no running lattice can be found",
        )
    })?;
    let cwd = std::env::current_dir().unwrap_or_default();
    Ok((candidates(&dir, &cwd), dir))
}

/// The bridge: find the editor, then relay stdin and stdout to it.
pub async fn run() -> std::io::Result<()> {
    let (locks, from) = locks_here()?;
    if locks.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "no running lattice is serving its tools (nothing in {}); \
                 turn it on in the editor with `:set llm.serve_tools`",
                from.display()
            ),
        ));
    }
    let stream = connect(&locks).await?;
    relay(stream, tokio::io::stdin(), tokio::io::stdout()).await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt as _, BufReader};

    use super::*;
    use crate::mcp::tools_listener::tests::{Scratch, running};

    fn lock(root: &str, port: u16) -> ToolsLock {
        ToolsLock {
            pid: 1,
            port,
            token: "t".into(),
            root: root.into(),
            version: "0".into(),
        }
    }

    /// Write `lock` as `name`, last modified `secs` after the epoch.
    fn put(dir: &Path, name: &str, lock: &ToolsLock, secs: u64) {
        let path = dir.join(name);
        std::fs::write(&path, serde_json::to_vec(lock).unwrap()).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    fn ports(locks: &[ToolsLock]) -> Vec<u16> {
        locks.iter().map(|l| l.port).collect()
    }

    /// Inside a project, its editor first, the deepest project winning;
    /// outside every project, the newest first.
    #[test]
    fn the_editor_of_the_project_here_comes_first_then_the_newest() {
        let dir = Scratch::new();
        put(dir.path(), "1-1.json", &lock("/work", 1), 100);
        put(dir.path(), "2-2.json", &lock("/work/inner", 2), 200);
        put(dir.path(), "3-3.json", &lock("/elsewhere", 3), 300);
        std::fs::write(dir.path().join("junk.json"), "not a lock").unwrap();
        std::fs::write(dir.path().join("4-4.lock"), "{}").unwrap();
        assert_eq!(
            ports(&candidates(dir.path(), Path::new("/work/inner/src"))),
            [2, 1, 3]
        );
        assert_eq!(
            ports(&candidates(dir.path(), Path::new("/work/src"))),
            [1, 3, 2]
        );
        assert_eq!(ports(&candidates(dir.path(), Path::new("/tmp"))), [3, 2, 1]);
        assert!(candidates(&dir.path().join("none"), Path::new("/")).is_empty());
    }

    /// A port nobody listens on any more.
    async fn dead_port() -> u16 {
        let l = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        l.local_addr().unwrap().port()
    }

    /// Through the bridge, an MCP client lists and calls a plugin's tool:
    /// a stale lock ahead of the live one is skipped.
    #[tokio::test]
    async fn a_client_reaches_the_plugins_tools_through_the_bridge() {
        let r = running().await;
        let stale = lock("/gone", dead_port().await);
        let stream = connect(&[stale, r.lock.clone()]).await.unwrap();
        let (client_in, bridge_in) = tokio::io::duplex(1 << 16);
        let (bridge_out, client_out) = tokio::io::duplex(1 << 16);
        let bridge = tokio::spawn(relay(stream, bridge_in, bridge_out));
        let (mut to_bridge, mut from_bridge) = (client_in, BufReader::new(client_out));
        for v in [
            json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                           "clientInfo": {"name": "test", "version": "0"}}
            }),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                   "params": {"name": "org__agenda", "arguments": {}}}),
        ] {
            to_bridge
                .write_all(format!("{v}\n").as_bytes())
                .await
                .unwrap();
        }
        let mut replies = Vec::new();
        while replies.len() < 2 {
            let mut line = String::new();
            tokio::time::timeout(Duration::from_secs(10), from_bridge.read_line(&mut line))
                .await
                .expect("a reply within 10s")
                .unwrap();
            let v: Value = serde_json::from_str(&line).unwrap();
            if v.get("id").is_some() {
                replies.push(v);
            }
        }
        assert_eq!(replies[0]["result"]["serverInfo"]["name"], "lattice");
        assert_eq!(
            replies[1]["result"]["content"][0]["text"],
            "Standup at 09:30"
        );
        // The client closing its side ends the bridge.
        drop(to_bridge);
        tokio::time::timeout(Duration::from_secs(10), bridge)
            .await
            .expect("the bridge ends")
            .unwrap()
            .unwrap();
    }

    /// With no editor answering, the bridge says so.
    #[tokio::test]
    async fn no_editor_answering_is_an_error() {
        let e = connect(&[lock("/gone", dead_port().await)])
            .await
            .unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::ConnectionRefused);
        assert!(e.to_string().contains("llm.serve_tools"), "{e}");
    }
}
