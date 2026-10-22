//! Insert completion, end to end (IC.1,
//! `docs/dev/operations/slice-plans/insert-completion-integrity.md`).
//!
//! Every source reaches the popup a real `<C-Space>` opens, in every peer.
//! Before these, each source's tests called `do_completion_trigger()`
//! directly — around the key path — and none ran in the GUI peer at all. The
//! merge defects that plan fixes went unseen for exactly that reason.
//!
//! Each scenario asserts on two things: the source's rows are in
//! `state.raw` (it contributed), and its candidate is a RENDERED row (the
//! user can see it). Where two sources produce the same text, the popup
//! keeps one row, so the scenario narrows to the source with its filter
//! chord — pressed, like everything else.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{PeerUnderTest, boot, press};

/// `(text, source id)` of every rendered popup row; empty with no popup.
fn rendered<P: PeerUnderTest>(peer: &P) -> Vec<(String, String)> {
    peer.editor()
        .insert_completion
        .as_ref()
        .map(|s| {
            s.rendered
                .iter()
                .map(|r| {
                    let source = r.raw.source.as_ref().map(|s| s.as_str()).unwrap_or("");
                    (r.raw.text.clone(), source.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The texts `source` contributed to the open popup's raw set.
fn raw_from<P: PeerUnderTest>(peer: &P, source: &str) -> Vec<String> {
    peer.editor()
        .insert_completion
        .as_ref()
        .map(|s| {
            s.raw
                .iter()
                .filter(|c| c.source.as_ref().map(|s| s.as_str()) == Some(source))
                .map(|c| c.text.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn shows<P: PeerUnderTest>(peer: &P, text: &str, source: &str) -> bool {
    rendered(peer).iter().any(|(t, s)| t == text && s == source)
}

/// Wait, the way a running peer does, for background work to land: what
/// the editor actor does on its `async_landed` wake (`run_tick_pending`,
/// then publish), followed by the peer's own idle repaint. Polls until
/// `done` holds, for up to five seconds.
fn settle<P: PeerUnderTest>(peer: &mut P, done: impl Fn(&P) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if done(peer) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        let _ = peer.editor_mut().run_tick_pending();
        peer.republish();
        peer.idle_wake();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A fresh directory for one scenario, unique per process and test thread
/// (both peers' copies of a scenario can run in one process).
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "lattice-peer-conformance-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Open `path` with `:e`, and wait for its syntax tree — tree-sitter and
/// path completion both read it.
fn open<P: PeerUnderTest>(peer: &mut P, path: &Path) {
    press(peer, &format!(":e {}<CR>", path.display()));
    assert!(
        settle(peer, |p| p.editor().syntax.is_some()),
        "precondition: {} has a syntax tree",
        path.display()
    );
}

/// Buffer words: a word elsewhere in the buffer completes.
pub fn ctrl_space_completes_from_buffer_words<P: PeerUnderTest>() {
    let mut peer = boot::<P>("alphabet bravo\n");
    press(&mut peer, "Goalp<C-Space>");
    assert!(
        shows(&peer, "alphabet", lattice_completion::BufferWordsSource::ID),
        "buffer words: {:?}",
        rendered(&peer)
    );
}

/// Tree-sitter symbols: a definition in a `.rs` file completes. Buffer
/// words offer the same name and win the shared row, so `<C-t>` narrows the
/// popup to tree-sitter — the user's way of asking for it.
pub fn ctrl_space_completes_from_tree_sitter_symbols<P: PeerUnderTest>() {
    let dir = temp_dir("ts-symbols");
    let path = dir.join("lib.rs");
    std::fs::write(&path, "fn compute_total() {}\n").expect("write");
    let mut peer = boot::<P>("\n");
    open(&mut peer, &path);

    press(&mut peer, "Gocomp<C-Space>");
    let ts = lattice_completion::TREE_SITTER_SYMBOL_SOURCE_ID;
    assert!(
        raw_from(&peer, ts).contains(&"compute_total".to_string()),
        "tree-sitter contributed nothing: {:?}",
        raw_from(&peer, ts)
    );
    press(&mut peer, "<C-t>");
    assert!(
        shows(&peer, "compute_total", ts),
        "filtered to tree-sitter: {:?}",
        rendered(&peer)
    );
}

/// Paths: inside a string literal, the files beside the buffer complete.
pub fn ctrl_space_completes_paths_inside_a_string<P: PeerUnderTest>() {
    let dir = temp_dir("paths");
    std::fs::write(dir.join("alpha.txt"), "").expect("write");
    std::fs::write(dir.join("beta.txt"), "").expect("write");
    let path = dir.join("main.rs");
    std::fs::write(&path, "let p = \"\";\n").expect("write");
    let mut peer = boot::<P>("\n");
    open(&mut peer, &path);

    // Onto the opening quote, then append: the cursor is inside the string.
    press(&mut peer, "gg0f\"a<C-Space>");
    let id = lattice_completion::PATH_SOURCE_ID;
    assert!(
        shows(&peer, "alpha.txt", id) && shows(&peer, "beta.txt", id),
        "paths: {:?}",
        rendered(&peer)
    );
}

/// Snippets: a snippet's prefix completes.
pub fn ctrl_space_completes_from_snippets<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    {
        let editor = peer.editor_mut();
        let mut next = (**editor.snippet_registry.load()).clone();
        next.insert(
            "*",
            lattice_snippet::Snippet {
                name: "conformance-loop".into(),
                prefixes: vec!["zzloop".into()],
                body: lattice_snippet::parse::parse("loop { $0 }").expect("snippet parses"),
                description: None,
                scope: String::new(),
            },
        );
        editor.snippet_registry.store(Arc::new(next));
    }
    peer.republish();

    press(&mut peer, "izzl<C-Space>");
    assert!(
        shows(&peer, "zzloop", lattice_completion::SNIPPET_SOURCE_ID),
        "snippets: {:?}",
        rendered(&peer)
    );
}

/// A background source answering a fixed list — a plugin's
/// `completion-source`, as the host sees one.
#[derive(Debug)]
struct ListSource {
    id: &'static str,
    items: Vec<&'static str>,
}

impl lattice_completion::AsyncCompletionSource for ListSource {
    fn produce_async(
        &self,
        _ctx: lattice_completion::InsertContextSnapshot,
        sink: Arc<dyn lattice_completion::CandidateSink>,
        _token: lattice_protocol::CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        let id = lattice_completion::SourceId::new(self.id);
        let items = self.items.clone();
        Box::pin(async move {
            for text in items {
                sink.push(
                    lattice_completion::RawCandidate::plain(
                        text.to_string(),
                        lattice_completion::CandidateKind::Plain,
                    )
                    .with_source(id.clone()),
                );
            }
        })
    }
}

/// A minor mode contributing one background source, as a plugin's carrier
/// mode does.
struct ListSourceMode(Arc<ListSource>);

impl lattice_mode::Mode for ListSourceMode {
    type Guard = ();
    fn id(&self) -> lattice_mode::ModeId {
        lattice_mode::ModeId::new("conformance-list-source-mode")
    }
    fn kind(&self) -> lattice_mode::ModeKind {
        lattice_mode::ModeKind::Minor
    }
    fn completion_sources(&self) -> Vec<lattice_completion::CompletionSourceContribution> {
        vec![lattice_completion::CompletionSourceContribution {
            accepts_non_word_query: false,
            id: lattice_completion::SourceId::new(self.0.id),
            default_priority: 100,
            auto_trigger: false,
            trigger_chars: Vec::new(),
            popup_filter_chord: None,
            kind: lattice_completion::CompletionSourceKind::Async(self.0.clone()),
        }]
    }
    fn on_activate(
        &self,
        _ctx: lattice_mode::ModeContext,
    ) -> lattice_mode::LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `mode` and turn it on for the active buffer.
fn activate<P: PeerUnderTest, M: lattice_mode::Mode + 'static>(peer: &mut P, mode: M) {
    let editor = peer.editor_mut();
    let id = {
        let mut registry = (**editor.mode_registry.load()).clone();
        let id = registry.register(mode).expect("register the mode");
        editor.mode_registry.store(Arc::new(registry));
        id
    };
    let buffer = editor.document_buffer_id;
    let _ = editor.activate_mode_by_id(buffer, id);
    peer.republish();
}

/// A background (non-LSP) source: its rows land after the popup opened.
pub fn ctrl_space_completes_from_a_background_source<P: PeerUnderTest>() {
    let mut peer = boot::<P>("\n");
    activate(
        &mut peer,
        ListSourceMode(Arc::new(ListSource {
            id: "gen:conformance-list",
            items: vec!["asyncword"],
        })),
    );

    press(&mut peer, "iasy<C-Space>");
    assert!(
        settle(&mut peer, |p| shows(p, "asyncword", "gen:conformance-list")),
        "background source: {:?}",
        rendered(&peer)
    );
}

/// An in-process language server answering `textDocument/completion` with
/// `items`, over a pipe — nothing is spawned. Its runtime is returned so it
/// outlives the scenario's keys.
fn attach_mock_lsp<P: PeerUnderTest>(
    peer: &mut P,
    path: &Path,
    items: &[&str],
) -> tokio::runtime::Runtime {
    use lattice_lsp::{LspReader, LspWriter, Message, Response};

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("runtime");
    let items: Vec<serde_json::Value> = items
        .iter()
        .map(|label| serde_json::json!({ "label": label }))
        .collect();
    let root = path.parent().expect("a parent").to_path_buf();
    let handle = rt.block_on({
        let root = root.clone();
        async move {
            let (actor_side, mock_side) = tokio::io::duplex(64 * 1024);
            let (actor_read, actor_write) = tokio::io::split(actor_side);
            let (mock_read, mock_write) = tokio::io::split(mock_side);
            tokio::spawn(async move {
                let mut reader = LspReader::new(mock_read);
                let mut writer = LspWriter::new(mock_write);
                while let Ok(Some(message)) = reader.read_message().await {
                    let Message::Request(req) = message else {
                        continue;
                    };
                    let result = match req.method.as_str() {
                        "initialize" => serde_json::json!({
                            "capabilities": {
                                "positionEncoding": "utf-8",
                                "textDocumentSync": 2,
                                "completionProvider": {}
                            },
                            "serverInfo": { "name": "conformance-mock" }
                        }),
                        "textDocument/completion" => {
                            serde_json::json!({ "isIncomplete": false, "items": items })
                        }
                        _ => serde_json::Value::Null,
                    };
                    let reply = Message::Response(Response::ok(req.id.clone(), result));
                    if writer.write_message(&reply).await.is_err() {
                        return;
                    }
                }
            });
            lattice_lsp::spawn_with_io(
                lattice_lsp::ServerConfig::new(
                    "conformance-mock",
                    PathBuf::from("conformance-mock"),
                    "rust",
                ),
                root,
                LspReader::new(actor_read),
                LspWriter::new(actor_write),
                None,
                None,
                lattice_lsp::LspLogger::new(lattice_lsp::LogLevel::Warn, 64),
                None,
                None,
                None,
                None,
                None,
            )
            .await
            .expect("the mock server handshakes")
        }
    });

    let uri = lattice_lsp::actor::uri_from_path(path);
    let text = std::fs::read_to_string(path).expect("read");
    rt.block_on(peer.editor().lsp.attach_handle(
        uri.clone(),
        root,
        "conformance-mock".into(),
        "rust".into(),
        text,
        handle,
    ))
    .expect("attach");
    let editor = peer.editor_mut();
    let buffer = editor.document_buffer_id;
    editor.buffer_uris.insert(buffer, uri);
    let _ = editor.activate_mode_by_id(
        buffer,
        lattice_lsp::completion::LspCompletionMode::mode_id(),
    );
    peer.republish();
    rt
}

/// LSP: a language server's items reach the popup.
pub fn ctrl_space_completes_from_a_language_server<P: PeerUnderTest>() {
    let dir = temp_dir("lsp");
    let path = dir.join("main.rs");
    std::fs::write(&path, "fn main() {}\n").expect("write");
    let mut peer = boot::<P>("\n");
    open(&mut peer, &path);
    let _server = attach_mock_lsp(&mut peer, &path, &["lsp_only_item"]);

    press(&mut peer, "Golsp<C-Space>");
    let id = lattice_completion::LSP_COMPLETION_SOURCE_ID;
    assert!(
        settle(&mut peer, |p| shows(p, "lsp_only_item", id)),
        "language server: {:?}",
        rendered(&peer)
    );
}
