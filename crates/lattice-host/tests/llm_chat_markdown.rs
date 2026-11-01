//! LL.6: the chat transcript is highlighted markdown with folded blocks.
//!
//! Asserts RESOLVED styles on a live chat buffer, not that a query compiled: a
//! grammar that loads and paints nothing would pass the second and fail these.
//!
//! Drives a real `Editor::boot`, so the whole chain is live: the ex-command,
//! the synthetic-buffer open, `llm-chat-mode`'s activation, the drain, the
//! chord handlers and the request task. The server is a thread on a std socket,
//! so the test needs no runtime features beyond the ones it already has.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use lattice_ai::llm::chat::LlmChatsHandle;
use lattice_ai::llm::config::LlmConfig;
use lattice_core::Document as CoreDocument;
use lattice_host::dispatch::DispatchOutcome;
use lattice_host::editor::Editor;
use lattice_protocol::edit::Edit;
use lattice_protocol::position::Position;
use lattice_protocol::{KeyChord, parse_chord_sequence};

const REPLY: &str = concat!(
    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hello \"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"from the mock\"}}]}\n\n",
    "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: [DONE]\n\n",
);

/// A server that answers every request with [`REPLY`] after `delay`.
fn serve(delay: Duration) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { continue };
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 65536];
                let mut seen = 0;
                // Read until the head and the (small) body are in.
                while let Ok(n) = s.read(&mut buf[seen..]) {
                    if n == 0 {
                        break;
                    }
                    seen += n;
                    if buf[..seen].windows(4).any(|w| w == b"\r\n\r\n") && seen > 200 {
                        break;
                    }
                }
                std::thread::sleep(delay);
                let _ = s.write_all(REPLY.as_bytes());
            });
        }
    });
    addr
}

fn config_for(addr: std::net::SocketAddr) -> LlmConfig {
    let text = format!(
        "[llm.providers.mock]\nkind = \"openai-compatible\"\nbase_url = \"http://{addr}/v1\"\nmodel = \"mock-model\"\n"
    );
    LlmConfig::from_table(&text.parse::<toml::Table>().unwrap())
}

fn chord(s: &str) -> KeyChord {
    parse_chord_sequence(s).unwrap().into_iter().next().unwrap()
}

fn open_chat(editor: &mut Editor) -> lattice_core::BufferId {
    let mut out = DispatchOutcome::default();
    editor.execute_ex_line("llm", &mut out);
    for effect in out.effects {
        if let lattice_grammar::Effect::OpenSyntheticBuffer {
            name,
            mode_id,
            content,
            cursor,
            activate_minor,
        } = effect
        {
            editor.open_synthetic_buffer_seeded(
                &name,
                &mode_id,
                content.as_deref(),
                cursor,
                activate_minor.as_deref(),
            );
        }
    }
    editor.active_pane_buffer_id()
}

fn text_of(editor: &Editor, id: lattice_core::BufferId) -> String {
    let snap = editor.buffers.document_handle(id).unwrap().snapshot();
    (0..snap.buffer.rope_line_count())
        .map(|n| snap.buffer.line(n).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Tick the editor the way its actor does, until `done` or a deadline. Ticks
/// only when `async_landed` fired, so a producer that wakes nothing fails here
/// instead of passing on a timer.
async fn settle_woken(editor: &mut Editor, mut done: impl FnMut(&Editor) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if done(editor) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let notified = editor.async_landed.clone();
        if tokio::time::timeout(left, notified.notified())
            .await
            .is_err()
        {
            return done(editor);
        }
        editor.run_tick_pending();
    }
}

/// Tick on a timer until `done`: for steps with no wake to wait on (activation).
async fn settle(editor: &mut Editor, mut done: impl FnMut(&Editor) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if done(editor) {
            return true;
        }
        if Instant::now() >= deadline {
            return done(editor);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        editor.run_tick_pending();
    }
}

fn chats(editor: &Editor) -> LlmChatsHandle {
    (*editor
        .services
        .get::<LlmChatsHandle>()
        .expect("llm chats service"))
    .clone()
}

use lattice_ai::conversation::{Role, ToolKind, ToolStatus};
use lattice_cells::ExcerptHighlighter;

fn coloured(editor: &Editor, id: lattice_core::BufferId, line: u32) -> bool {
    let Some(syntax) = editor.document_syntax_for(id) else {
        return false;
    };
    syntax
        .highlight_lines(line, line + 1)
        .and_then(|rows| rows.into_iter().next())
        .is_some_and(|spans| {
            spans
                .iter()
                .any(|s| s.style != lattice_cells::Style::Default)
        })
}

/// The resolved spans on one line, as (start, end, style).
fn spans_of(
    editor: &Editor,
    id: lattice_core::BufferId,
    line: u32,
) -> Vec<(usize, usize, lattice_cells::Style)> {
    editor
        .document_syntax_for(id)
        .and_then(|s| s.highlight_lines(line, line + 1))
        .and_then(|rows| rows.into_iter().next())
        .unwrap_or_default()
        .into_iter()
        .map(|s| (s.start, s.end, s.style))
        .collect()
}

fn line_of(text: &str, needle: &str) -> u32 {
    text.lines().position(|l| l.contains(needle)).unwrap() as u32
}

const MD_REPLY: &str =
    "# Title\n\nbody text\n\n```rust\nfn main() { let x = 1; }\n```\n\n## Sub\n\nmore";

async fn chat_with_reply(editor: &mut Editor) -> lattice_core::BufferId {
    let id = open_chat(editor);
    assert!(settle(editor, |e| text_of(e, id) == "> ").await);
    let session = chats(editor).for_buffer(id).unwrap();
    session.store.mutate(&session.key, |c| {
        c.push_user_text("q");
        c.extend_text(Role::Assistant, MD_REPLY);
    });
    assert!(settle_woken(editor, |e| text_of(e, id).contains("more")).await);
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_chat_buffer_gets_the_markdown_grammar_with_resolved_styles() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let id = chat_with_reply(&mut editor).await;
    let text = text_of(&editor, id);
    let (heading, code, plain) = (
        line_of(&text, "# Title"),
        line_of(&text, "fn main()"),
        line_of(&text, "body text"),
    );
    assert!(
        settle(&mut editor, |e| coloured(e, id, heading)
            && coloured(e, id, code))
        .await,
        "heading coloured: {}, code coloured: {} in {text:?}",
        coloured(&editor, id, heading),
        coloured(&editor, id, code),
    );
    assert_eq!(
        editor.document_syntax_for(id).unwrap().lang(),
        lattice_syntax::Lang::Markdown
    );
    assert!(!coloured(&editor, id, plain), "plain prose stays default");
}

/// The case the projection exists for: a code block that is still arriving.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_half_received_fence_does_not_colour_the_prompt() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let id = chat_with_reply(&mut editor).await;
    // `> ` is also a markdown blockquote marker, so the prompt carries a style
    // of its own. What it must not gain is a code block's: compare against the
    // prompt's styling while every fence is closed.
    let before = text_of(&editor, id);
    let before_prompt = before.lines().count() as u32 - 1;
    assert!(
        settle(&mut editor, |e| !spans_of(
            e,
            id,
            line_of(&before, "fn main()")
        )
        .is_empty())
        .await
    );
    let baseline = spans_of(&editor, id, before_prompt);
    let session = chats(&editor).for_buffer(id).unwrap();
    session.store.mutate(&session.key, |c| {
        c.extend_text(Role::Assistant, "\n\n```rust\nlet half = vec![");
    });
    assert!(settle_woken(&mut editor, |e| text_of(e, id).contains("let half")).await);
    let text = text_of(&editor, id);
    let half = line_of(&text, "let half");
    let prompt = text.lines().count() as u32 - 1;
    assert!(
        text.ends_with("```\n\n> "),
        "fence closed before the prompt: {text:?}"
    );
    assert!(
        settle(&mut editor, |e| coloured(e, id, half)).await,
        "the open block itself is highlighted as code: {text:?}"
    );
    assert_eq!(
        spans_of(&editor, id, prompt),
        baseline,
        "the prompt is styled as it is with every fence closed: {text:?}"
    );
    // The prompt is still the editable line, at the anchor the projection moved.
    let ok = editor.apply_edit_blocking(Edit::insert(Position::new(prompt, 2), "typed"));
    assert!(ok.is_ok(), "{ok:?}");
    let refused = editor.apply_edit_blocking(Edit::insert(Position::new(half, 0), "x"));
    assert!(refused.is_err(), "the synthetic fence line is history");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn enter_does_not_follow_a_markdown_link_in_the_transcript() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let id = open_chat(&mut editor);
    assert!(settle(&mut editor, |e| text_of(e, id) == "> ").await);
    let session = chats(&editor).for_buffer(id).unwrap();
    session.store.mutate(&session.key, |c| {
        c.push_user_text("# Top\n\nsee [top](#top) now");
    });
    assert!(settle_woken(&mut editor, |e| text_of(e, id).contains("now")).await);
    let text = text_of(&editor, id);
    let link = line_of(&text, "[top]");
    editor.modal = lattice_grammar::ModalState::Normal;
    editor.cursor = Position::new(link, 6);
    let mut partial: Vec<KeyChord> = Vec::new();
    editor.dispatch_chord(chord("<CR>"), &mut partial);
    assert_ne!(
        editor.cursor.line,
        line_of(&text, "# Top"),
        "<CR> jumped to the link target: markdown-mode's chord leaked in"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn zm_and_za_work_on_blocks_with_markdown_headings_around_them() {
    let addr = serve(Duration::ZERO);
    let mut editor = Editor::boot(CoreDocument::from_text("scratch\n"));
    chats(&editor).set_config(config_for(addr));
    let id = chat_with_reply(&mut editor).await;
    let session = chats(&editor).for_buffer(id).unwrap();
    session.store.mutate(&session.key, |c| {
        c.push_tool_call(
            "t1".into(),
            Some("github".into()),
            "search_issues".into(),
            ToolStatus::Ok,
            ToolKind::Search,
            Some("{ \"query\": \"x\" }".into()),
            Some("{ \"total_count\": 3 }".into()),
        );
    });
    assert!(settle_woken(&mut editor, |e| text_of(e, id).contains("total_count")).await);
    editor.recompute_folds();
    let text = text_of(&editor, id);
    let head = line_of(&text, "github");
    let inner = head + 2;
    let prompt = text.lines().count() as u32 - 1;

    // Folds start closed: the head reads, the fenced bodies are hidden.
    assert!(!editor.line_inside_closed_fold(head));
    assert!(editor.line_inside_closed_fold(inner), "{text:?}");

    // `za` on the head opens it and `zM` closes everything again.
    editor.cursor = Position::new(head, 0);
    editor.modal = lattice_grammar::ModalState::Normal;
    let mut partial: Vec<KeyChord> = Vec::new();
    for c in ['z', 'a'] {
        editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
    assert!(!editor.line_inside_closed_fold(inner), "za opens the block");
    for c in ['z', 'M'] {
        editor.dispatch_chord(KeyChord::char(c), &mut partial);
    }
    assert!(editor.line_inside_closed_fold(inner), "zM closes it");
    assert!(
        !editor.line_inside_closed_fold(prompt),
        "the prompt is never swallowed by a fold"
    );
    assert!(!editor.line_inside_closed_fold(line_of(&text, "# Title")));
}
