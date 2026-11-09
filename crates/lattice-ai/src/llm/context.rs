//! Context: text the user attaches to requests, and the view of exactly what a
//! request sends (LL.15, design `llm-layer.md` §5).
//!
//! **Nothing is sent unless it can be seen first.** Attaching is explicit
//! (`<leader>ac{motion}`, `:llm-context-add`), `:llm-context` lists what is
//! attached (`dd` removes), and `:llm-inspect` shows the request body.
//!
//! # Whose context
//!
//! Context belongs to the buffer requests are made from, like its session
//! settings (design §4). Asks from `foo.rs` carry `foo.rs`'s; a chat opened
//! from it, or an ask's own buffer, takes a **copy** when it is made and has
//! its own from then on, so attaching in one chat never leaks into another.
//!
//! # What is attached, and when it is read
//!
//! A whole buffer, a span of one, or a file. Text is read **when a request is
//! sent**, never at attach time, so the model sees the text as it is then. A
//! span is a tracked range in its document ([`lattice_core::tracked`]), so it
//! follows edits above and inside it: attach a function, edit it, and the
//! edited function is what goes.
//!
//! An item that cannot be read as the list shows it fails the request instead
//! of being skipped: its buffer closed, its span was deleted, its file cannot
//! be read, or it now matches `llm.exclude`. The list is what the user agreed
//! to send; sending something else, even less, would make it a lie.
//!
//! # How it is sent
//!
//! As a block at the head of the request's **last** user message: every item,
//! fenced with its name. Earlier turns go as they were, so each request
//! carries the context once, current.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lattice_core::BufferId;
use lattice_core::tracked::TrackedRangeId;
use lattice_mode::BufferStoreHandle;

use super::chat::{LlmChats, LlmChatsHandle};
use super::{HttpRequest, Message};

/// Stands in for the key wherever a request is shown (`:llm-inspect`). Built
/// into the shown request in the key's place, so no key is ever read for it.
pub const REDACTED: &str = "<redacted>";

/// One attached thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextItem {
    /// A whole buffer.
    Buffer(BufferId),
    /// Part of a buffer, kept in place across edits by a tracked range.
    Span {
        buffer: BufferId,
        range: TrackedRangeId,
    },
    /// A file, read from disk when a request is sent.
    File(PathBuf),
}

/// An item and the name it was attached under: the file name, or the
/// buffer's name for a buffer with no file. Kept so a closed buffer is still
/// named in the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    pub item: ContextItem,
    pub name: String,
}

impl Attached {
    /// The buffer this item reads, if it reads one.
    pub fn buffer(&self) -> Option<BufferId> {
        match &self.item {
            ContextItem::Buffer(b) | ContextItem::Span { buffer: b, .. } => Some(*b),
            ContextItem::File(_) => None,
        }
    }
}

/// A file's name for the list, or the buffer's, or `[no name]` as the
/// modeline calls a buffer with neither.
pub fn name_for(store: &BufferStoreHandle, buffer: BufferId) -> String {
    store
        .path_for(buffer)
        .as_deref()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| store.name_for(buffer))
        .unwrap_or_else(|| "[no name]".to_string())
}

/// The 1-based lines bytes `range` covers in `snap`, `3` or `3-10`. A span
/// ending at a line's start (linewise) does not include that line.
fn lines_of(snap: &lattice_runtime::DocumentSnapshot, range: std::ops::Range<usize>) -> String {
    let at = |b: usize| snap.buffer.byte_to_position(b).ok();
    let (Some(start), Some(end)) = (at(range.start), at(range.end)) else {
        return "?".to_string();
    };
    let last = if end.byte == 0 && end.line > start.line {
        end.line - 1
    } else {
        end.line
    };
    if last == start.line {
        format!("{}", start.line + 1)
    } else {
        format!("{}-{}", start.line + 1, last + 1)
    }
}

/// A span's current bytes, or `None` when it no longer covers any text.
fn span_bytes(
    snap: &lattice_runtime::DocumentSnapshot,
    range: TrackedRangeId,
) -> Option<std::ops::Range<usize>> {
    snap.tracked_ranges
        .get(range)
        .map(|r| r.bytes())
        .filter(|b| b.start < b.end)
}

/// Where an item stands right now, for the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// `buffer`, `lines` or `file`.
    pub kind: &'static str,
    /// `main.rs`, `lib.rs:3-10`, `notes/todo.md`.
    pub label: String,
    /// `120 lines`, `closed`, `gone`.
    pub status: String,
}

/// Describe `items` as the list shows them. Reads only snapshots: no file is
/// touched (a file's size is learnt when it is sent).
pub fn rows(items: &[Attached], store: &BufferStoreHandle) -> Vec<Row> {
    items
        .iter()
        .map(|a| match &a.item {
            ContextItem::Buffer(b) => Row {
                kind: "buffer",
                label: a.name.clone(),
                status: match store.handle_for(*b) {
                    Some(h) => {
                        let n = h.snapshot().buffer.content_line_count();
                        format!("{n} line{}", if n == 1 { "" } else { "s" })
                    }
                    None => "closed".to_string(),
                },
            },
            ContextItem::Span { buffer, range } => match store.handle_for(*buffer) {
                Some(h) => {
                    let snap = h.snapshot();
                    match span_bytes(&snap, *range) {
                        Some(bytes) => Row {
                            kind: "lines",
                            label: format!("{}:{}", a.name, lines_of(&snap, bytes)),
                            status: String::new(),
                        },
                        None => Row {
                            kind: "lines",
                            label: a.name.clone(),
                            status: "gone (its text was deleted)".to_string(),
                        },
                    }
                }
                None => Row {
                    kind: "lines",
                    label: a.name.clone(),
                    status: "closed".to_string(),
                },
            },
            ContextItem::File(path) => Row {
                kind: "file",
                label: path.display().to_string(),
                status: String::new(),
            },
        })
        .collect()
}

/// Read every item as it is now and fence it for the request, or say which
/// item cannot be sent as the list shows it. `excluded` names the
/// `llm.exclude` glob a path matches, `composed` why a composed view's text
/// may not go (it shows an excluded file). `Ok(None)` when nothing is
/// attached.
pub async fn read(
    items: &[Attached],
    store: Option<&BufferStoreHandle>,
    excluded: impl Fn(&Path) -> Option<String>,
    composed: impl Fn(BufferId) -> Option<String>,
) -> Result<Option<String>, String> {
    if items.is_empty() {
        return Ok(None);
    }
    let fix = ":llm-context lists it; `dd` there removes it";
    let mut out = String::from("<context>\n");
    for a in items {
        let refuse = |path: &Path| {
            excluded(path).map(|glob| {
                format!(
                    "llm: context `{}` matches llm.exclude (`{glob}`); nothing was sent ({fix})",
                    a.name
                )
            })
        };
        let (label, text) = match &a.item {
            ContextItem::Buffer(b) | ContextItem::Span { buffer: b, .. } => {
                let handle = store.and_then(|s| s.handle_for(*b)).ok_or_else(|| {
                    format!(
                        "llm: context `{}` is closed; nothing was sent ({fix})",
                        a.name
                    )
                })?;
                if let Some(why) = store.and_then(|s| s.path_for(*b)).and_then(|p| refuse(&p)) {
                    return Err(why);
                }
                // Checked when sent: a view's files change as results stream in.
                if let Some(why) = composed(*b) {
                    return Err(format!(
                        "llm: context `{}`: {why}; nothing was sent ({fix})",
                        a.name
                    ));
                }
                let snap = handle.snapshot();
                match &a.item {
                    ContextItem::Span { range, .. } => {
                        let bytes = span_bytes(&snap, *range).ok_or_else(|| {
                            format!(
                                "llm: the text attached from `{}` was deleted; nothing was sent ({fix})",
                                a.name
                            )
                        })?;
                        let label = format!("{}:{}", a.name, lines_of(&snap, bytes.clone()));
                        let whole = snap.buffer.as_string();
                        let text = whole.get(bytes).map(str::to_string).unwrap_or_default();
                        (label, text)
                    }
                    _ => (a.name.clone(), snap.buffer.as_string()),
                }
            }
            ContextItem::File(path) => {
                if let Some(why) = refuse(path) {
                    return Err(why);
                }
                let text = tokio::fs::read_to_string(path).await.map_err(|e| {
                    format!(
                        "llm: context `{}` could not be read ({e}); nothing was sent ({fix})",
                        path.display()
                    )
                })?;
                (path.display().to_string(), text)
            }
        };
        out.push_str(&format!("<item name=\"{label}\">\n{text}"));
        if !text.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("</item>\n");
    }
    out.push_str("</context>");
    Ok(Some(out))
}

/// Put `block` at the head of the last user message. Earlier turns go as they
/// were sent. Nothing changes when there is no user message.
pub fn prepend(messages: &mut [Message], block: &str) {
    if let Some(Message::User(text)) = messages
        .iter_mut()
        .rev()
        .find(|m| matches!(m, Message::User(_)))
    {
        *text = format!("{block}\n\n{text}");
    }
}

/// A request as `:llm-inspect` shows it: the method and URL, the headers, a
/// blank line, the body pretty-printed. Built with [`REDACTED`] in the key's
/// place, so the key is not in it to leak; any header carrying it shows that.
/// The URL is [`redacted_url`](super::redacted_url): a key put in a custom
/// `base_url` is not shown either.
pub fn show(http: &HttpRequest) -> String {
    let mut out = format!("{} {}\n", http.method, super::redacted_url(&http.url));
    for (name, value) in &http.headers {
        out.push_str(&format!("{name}: {value}\n"));
    }
    out.push('\n');
    match serde_json::from_slice::<serde_json::Value>(&http.body)
        .ok()
        .and_then(|v| serde_json::to_string_pretty(&v).ok())
    {
        Some(pretty) => out.push_str(&pretty),
        None => out.push_str(&String::from_utf8_lossy(&http.body)),
    }
    out.push('\n');
    out
}

/// The body of a shown request ([`show`]): everything after the first blank
/// line. For tests comparing it with what a server received.
pub fn shown_body(shown: &str) -> &str {
    shown.split_once("\n\n").map(|(_, b)| b).unwrap_or("")
}

/// `<leader>ac{motion}`'s name in the registry.
pub const ATTACH_OPERATOR: &str = "llm-attach";

fn echo(level: lattice_grammar::effect::EchoLevel, text: String) -> lattice_grammar::Effect {
    lattice_grammar::Effect::Echo { level, text }
}

/// Why `buffer`'s text may not be attached: its file matches `llm.exclude`,
/// or it is a composed view showing one.
fn excluded_buffer(
    chats: &LlmChats,
    store: &BufferStoreHandle,
    buffer: BufferId,
) -> Option<lattice_grammar::Effect> {
    if let Some(why) = chats.composed_refusal(buffer) {
        return Some(echo(
            lattice_grammar::effect::EchoLevel::Error,
            format!("llm: {why}; not attached"),
        ));
    }
    let path = store.path_for(buffer)?;
    let glob = chats.excluded(&path)?;
    Some(echo(
        lattice_grammar::effect::EchoLevel::Error,
        format!(
            "llm: {} matches llm.exclude (`{glob}`); not attached",
            name_for(store, buffer)
        ),
    ))
}

/// `<leader>ac{motion}`: attach the span to this buffer's context, anchored by
/// a tracked range so it follows edits.
fn operator_attach(
    chats: &LlmChats,
    store: &BufferStoreHandle,
    ctx: &mut lattice_grammar::registry::OperatorContext,
) -> Result<lattice_grammar::Effect, lattice_grammar::CommandError> {
    use lattice_core::tracked::{Stickiness, TrackedRangeChange};
    use lattice_grammar::effect::EchoLevel;
    let buffer = ctx.buffer_id;
    if let Some(refused) = excluded_buffer(chats, store, buffer) {
        return Ok(refused);
    }
    let Some(handle) = store.handle_for(buffer) else {
        return Ok(echo(EchoLevel::Error, "llm: no such buffer".to_string()));
    };
    // A composed view (several files) has no text of its own to anchor to.
    if handle.display_line_numbers().is_some() {
        return Ok(echo(
            EchoLevel::Error,
            "llm: this view shows several files; attach from the file itself".to_string(),
        ));
    }
    let doc = ctx.document.buffer();
    let (Ok(start), Ok(end)) = (
        doc.position_to_byte(ctx.range.start),
        doc.position_to_byte(ctx.range.end),
    ) else {
        return Ok(echo(
            EchoLevel::Error,
            "llm: the span is out of range".to_string(),
        ));
    };
    let text = doc.slice(ctx.range).unwrap_or_default();
    if start >= end || text.trim().is_empty() {
        return Ok(echo(
            EchoLevel::Info,
            "llm: nothing to attach (the span is empty)".to_string(),
        ));
    }
    // Lines are tracked from the first line's start to the last line's end,
    // without its newline, and grow at both edges: text typed at the start of
    // the first line, or the end of the last, is on those lines, so it is
    // part of what was attached. A typed line after the last one starts past
    // the newline, outside. A charwise span keeps to exactly what was chosen.
    let (end, stickiness) = if ctx.linewise && text.ends_with('\n') {
        (end - 1, Stickiness::GrowsAtEdges)
    } else if ctx.linewise {
        (end, Stickiness::GrowsAtEdges)
    } else {
        (end, Stickiness::NeverGrows)
    };
    let range = TrackedRangeId::next();
    // FIFO with edits: any edit after this call sees the range tracked.
    drop(
        handle.update_tracked_ranges(vec![TrackedRangeChange::Track {
            id: range,
            bytes: start..end,
            stickiness,
        }]),
    );
    let name = name_for(store, buffer);
    let lines = super::operators::line_span(ctx.range);
    chats.attach(
        buffer,
        Attached {
            item: ContextItem::Span { buffer, range },
            name: name.clone(),
        },
    );
    Ok(echo(
        EchoLevel::Info,
        format!("llm: attached {name} {lines} to the context (:llm-context lists it)"),
    ))
}

/// `:llm-context-add [file…]`: attach the files, or the whole buffer.
fn add_effect(
    chats: &LlmChats,
    store: &BufferStoreHandle,
    buffer: BufferId,
    args: &str,
) -> lattice_grammar::Effect {
    use lattice_grammar::effect::EchoLevel;
    if args.trim().is_empty() {
        if let Some(refused) = excluded_buffer(chats, store, buffer) {
            return refused;
        }
        let name = name_for(store, buffer);
        let added = chats.attach(
            buffer,
            Attached {
                item: ContextItem::Buffer(buffer),
                name: name.clone(),
            },
        );
        return echo(
            EchoLevel::Info,
            if added {
                format!("llm: attached {name} to the context (read when a request is sent)")
            } else {
                format!("llm: {name} is already in the context")
            },
        );
    }
    let mut said = Vec::new();
    for word in args.split_whitespace() {
        let path = match word.strip_prefix("~/") {
            Some(rest) => dirs_home()
                .map(|h| h.join(rest))
                .unwrap_or_else(|| PathBuf::from(word)),
            None => PathBuf::from(word),
        };
        if let Some(glob) = chats.excluded(&path) {
            return echo(
                EchoLevel::Error,
                format!("llm: {word} matches llm.exclude (`{glob}`); not attached"),
            );
        }
        let added = chats.attach(
            buffer,
            Attached {
                item: ContextItem::File(path),
                name: word.to_string(),
            },
        );
        if added {
            said.push(word.to_string());
        }
    }
    match said.as_slice() {
        [] => echo(EchoLevel::Info, "llm: already in the context".to_string()),
        names => echo(
            EchoLevel::Info,
            format!(
                "llm: attached {} to the context (read when a request is sent)",
                names.join(", ")
            ),
        ),
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// `:llm-inspect`: in a chat, what sending its prompt now would send; in any
/// other buffer, the last request sent from it.
fn inspect_effect(
    chats: &LlmChatsHandle,
    store: &BufferStoreHandle,
    anchors: &crate::conversation::transcript::PromptAnchors,
    buffer: BufferId,
) -> lattice_grammar::Effect {
    use super::modes::context::{INSPECT_BUFFER, LlmInspectMode};
    if chats.for_buffer(buffer).is_some() {
        let prompt = store.handle_for(buffer).and_then(|h| {
            let anchor = anchors.get(buffer)?;
            crate::conversation::transcript::prompt_in(&h.snapshot(), anchor)
        });
        chats.inspect_next(buffer, prompt);
    } else if !chats.inspect_last(buffer) {
        return echo(
            lattice_grammar::effect::EchoLevel::Info,
            "llm: nothing has been sent from this buffer yet (in a chat, :llm-inspect shows \
             what sending the prompt would send)"
                .to_string(),
        );
    }
    lattice_grammar::Effect::DisplaySyntheticBuffer {
        name: INSPECT_BUFFER.to_string(),
        mode_id: LlmInspectMode::mode_id().as_str().to_string(),
        content: None,
        split: lattice_core::ui::pane::SplitOrientation::Vertical,
    }
}

/// The attach operator, for `install` to bind under `llm-mode`.
pub fn register(
    registry: &mut lattice_grammar::CommandRegistry,
    chats: LlmChatsHandle,
    store: BufferStoreHandle,
    anchors: crate::conversation::transcript::PromptAnchors,
) -> lattice_grammar::registry::OperatorId {
    use super::modes::context::{CONTEXT_BUFFER, LlmContextMode};
    use lattice_grammar::command::LatencyClass;
    use lattice_grammar::registry::{ExCommandSpec, OperatorSpec, SurfaceForm};
    let text = |args: &lattice_grammar::args::Args| match args {
        lattice_grammar::args::Args::String(t) => t.trim().to_string(),
        _ => String::new(),
    };
    let (c, st) = (chats.clone(), store.clone());
    let op = registry.register_operator(
        ATTACH_OPERATOR,
        "Attach the {motion}/{text-object} that follows to this buffer's LLM context: its \
         text is read and sent with each request made from here, as it is then. \
         `:llm-context` lists what is attached.",
        OperatorSpec {
            // It reads the span and changes nothing.
            mutates: false,
            repeatable: false,
            blockwise_per_row: false,
            post_motion_char: false,
            args_schema: vec![],
            apply: Arc::new(move |ctx| operator_attach(&c, &st, ctx)),
        },
    );
    let (c, st) = (chats.clone(), store.clone());
    registry.register_ex_command(
        "llm-context",
        "List what requests from this buffer carry as context: attached buffers, spans and \
         files, read when each request is sent. `dd` on a row removes it.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(lattice_agent::parse_rest_as_text),
            apply: Arc::new(move |ctx| {
                // Run inside the list itself, it keeps showing whose it was.
                if st.name_for(ctx.buffer_id).as_deref() != Some(CONTEXT_BUFFER) {
                    c.set_listing_owner(ctx.buffer_id);
                }
                Ok(lattice_grammar::Effect::OpenSyntheticBuffer {
                    name: CONTEXT_BUFFER.to_string(),
                    mode_id: LlmContextMode::mode_id().as_str().to_string(),
                    content: None,
                    cursor: None,
                    activate_minor: None,
                })
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let (c, st) = (chats.clone(), store.clone());
    registry.register_ex_command(
        "llm-context-add",
        "Attach files to this buffer's LLM context, or with no file the whole buffer. \
         Read when each request is sent; `:llm-context` lists what is attached.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(lattice_agent::parse_rest_as_text),
            apply: Arc::new(move |ctx| Ok(add_effect(&c, &st, ctx.buffer_id, &text(&ctx.args)))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    registry.register_ex_command(
        "llm-inspect",
        "Show exactly what an LLM request sends, key redacted: in a chat, what sending the \
         prompt now would send; elsewhere, the last request sent from this buffer.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(lattice_agent::parse_rest_as_text),
            apply: Arc::new(move |ctx| Ok(inspect_effect(&chats, &store, &anchors, ctx.buffer_id))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    op
}

/// A version counter the list and inspect views watch, so a change repaints
/// them without a keystroke.
#[derive(Debug)]
pub struct Watch(tokio::sync::watch::Sender<u64>);

impl Default for Watch {
    fn default() -> Self {
        Self(tokio::sync::watch::channel(0).0)
    }
}

impl Watch {
    /// Something the view shows changed.
    pub fn bump(&self) {
        self.0.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Wait on changes from now on.
    pub fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
        self.0.subscribe()
    }
}

/// Shared so the views and the commands hold the same counters.
pub type WatchHandle = Arc<Watch>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_heads_the_last_user_message_only() {
        let mut messages = vec![
            Message::User("first".into()),
            Message::Assistant(vec![super::super::AssistantPart::Text("ok".into())]),
            Message::User("second".into()),
        ];
        prepend(&mut messages, "<context>x</context>");
        assert_eq!(messages[0], Message::User("first".into()));
        assert_eq!(
            messages[2],
            Message::User("<context>x</context>\n\nsecond".into())
        );
    }

    #[test]
    fn a_shown_request_names_no_key_and_splits_at_its_body() {
        let http = HttpRequest {
            method: "POST".into(),
            url: "http://h/v1/messages".into(),
            headers: vec![("x-api-key".into(), REDACTED.into())],
            body: br#"{"model":"m"}"#.to_vec(),
        };
        let shown = show(&http);
        assert!(shown.starts_with("POST http://h/v1/messages\nx-api-key: <redacted>\n\n{"));
        let body: serde_json::Value = serde_json::from_str(shown_body(&shown)).unwrap();
        assert_eq!(body, serde_json::json!({"model": "m"}));
    }

    #[tokio::test]
    async fn nothing_attached_reads_as_nothing_and_a_missing_file_refuses() {
        assert_eq!(read(&[], None, |_| None, |_| None).await, Ok(None));
        let missing = Attached {
            item: ContextItem::File(PathBuf::from("/nonexistent/lattice-ctx.txt")),
            name: "lattice-ctx.txt".into(),
        };
        let err = read(std::slice::from_ref(&missing), None, |_| None, |_| None)
            .await
            .unwrap_err();
        assert!(err.contains("could not be read"), "{err}");
        let err = read(&[missing], None, |_| Some(".env".into()), |_| None)
            .await
            .unwrap_err();
        assert!(err.contains("matches llm.exclude"), "{err}");
    }
}
