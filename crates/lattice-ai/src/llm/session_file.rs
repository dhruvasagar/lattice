//! LL.20: session files, a chat kept to be resumed, and the commands that
//! read and write files: `:llm-save`, `:llm-resume` and `:llm-export`.
//!
//! # The format
//!
//! JSON, versioned, with its own shapes rather than the in-memory model's, so
//! the model can change without breaking a file someone kept:
//!
//! ```text
//! { "format": "lattice-llm-session", "version": 1, "checksum": "fnv1a64:…",
//!   "session": { "label", "settings", "turns", "reasoning", "tool_calls" } }
//! ```
//!
//! What a resumed chat needs on the wire comes back exactly: tool-call ids
//! and arguments, and reasoning data (a provider's signature over its own
//! reasoning). A source block's link to its buffer does not: buffer ids do not
//! outlive the editor.
//!
//! # What a file may do
//!
//! A session file may have been downloaded, so it gets the trust a project's
//! config gets ([`settings::restrict`](super::settings::restrict)): it may
//! choose the model, a system prompt (as text, never a path), parameters and
//! tools no wider than the user's own config enables. It cannot name a key
//! source, a `base_url`, trust or MCP servers, and what it tried is reported.
//! No key is ever written: a chat's settings hold none.
//!
//! The checksum catches a file edited or damaged after lattice wrote it, whose
//! history may no longer be what a provider signed. It is not a signature: a
//! forger can recompute it, and what a forged file can do is bounded by the
//! trust above, not by the checksum.
//!
//! # Off the editor thread
//!
//! The commands take what they need on the editor thread (a snapshot) and do
//! every read, write and render on the runtime. Their result returns through
//! an inbound bus, which wakes the editor, so it shows without a keystroke.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use lattice_agent::parse_rest_as_text;
use lattice_core::BufferId;
use lattice_grammar::args::Args;
use lattice_grammar::command::LatencyClass;
use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_grammar::registry::{CommandRegistry, ExCommandSpec, SurfaceForm};
use lattice_mode::inbound::InboundBus;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::runtime::Handle;

use super::chat::LlmChatsHandle;
use super::modes::chat::LlmChatMode;
use super::settings::{RawSet, SettingKey, file_key, system_is_path};
use crate::conversation::projection::{edit_tag, permission_tag, status_tag};
use crate::conversation::{
    Block, Conversation, EditStatus, OpaqueReasoning, PermissionStatus, Role, ToolCallData,
    ToolKind, ToolStatus, Turn,
};

/// The `format` a session file names.
pub const FORMAT: &str = "lattice-llm-session";
/// The newest format this build writes and reads.
pub const VERSION: u64 = 1;

/// A session file, read and checked: the settings it asks for (still to go
/// through the trust table) and the conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    /// `(key, text)` as the file spells them (`max_tokens`).
    pub settings: Vec<(String, String)>,
    pub conversation: Conversation,
}

#[derive(Serialize)]
struct FileOut<'a> {
    format: &'a str,
    version: u64,
    checksum: String,
    session: &'a Value,
}

#[derive(Serialize, Deserialize)]
struct SessionDto {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    #[serde(default)]
    settings: BTreeMap<String, String>,
    turns: Vec<TurnDto>,
    #[serde(default)]
    reasoning: Vec<ReasoningDto>,
    #[serde(default)]
    tool_calls: BTreeMap<String, ToolCallDto>,
}

#[derive(Serialize, Deserialize)]
struct TurnDto {
    role: String,
    blocks: Vec<BlockDto>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BlockDto {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<String>,
        title: String,
        status: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    Source {
        label: String,
        lang: String,
        text: String,
    },
    Edit {
        path: String,
        status: String,
    },
    Permission {
        id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        status: String,
    },
}

#[derive(Serialize, Deserialize)]
struct ReasoningDto {
    turn: usize,
    block: usize,
    provider: String,
    data: String,
}

#[derive(Serialize, Deserialize)]
struct ToolCallDto {
    name: String,
    input: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    malformed: Option<String>,
}

/// The file `conv` and `settings` are kept as. `settings` holds only keys a
/// file may carry; any other is left out.
pub fn encode(conv: &Conversation, settings: &RawSet) -> Result<String, String> {
    let dto = SessionDto {
        label: conv.assistant_label.clone(),
        settings: settings
            .iter()
            .filter_map(|(k, v)| Some((file_key(*k)?.to_string(), v.clone())))
            .collect(),
        turns: conv.turns.iter().map(turn_dto).collect(),
        reasoning: conv
            .reasoning_data
            .iter()
            .map(|((turn, block), d)| ReasoningDto {
                turn: *turn,
                block: *block,
                provider: d.provider.clone(),
                data: d.data.clone(),
            })
            .collect(),
        tool_calls: conv
            .tool_calls
            .iter()
            .map(|(id, c)| {
                (
                    id.clone(),
                    ToolCallDto {
                        name: c.name.clone(),
                        input: c.input.clone(),
                        malformed: c.malformed.clone(),
                    },
                )
            })
            .collect(),
    };
    let fail = |e: serde_json::Error| format!("llm: cannot encode the session: {e}");
    let session = serde_json::to_value(&dto).map_err(fail)?;
    let file = FileOut {
        format: FORMAT,
        version: VERSION,
        checksum: checksum(&serde_json::to_string(&session).map_err(fail)?),
        session: &session,
    };
    let mut text = serde_json::to_string_pretty(&file).map_err(fail)?;
    text.push('\n');
    Ok(text)
}

/// Read a session file. `source` names it in messages. Refused, saying why:
/// not a session file, one from a newer lattice, one changed after it was
/// written, or one whose contents do not hold together.
pub fn decode(text: &str, source: &str) -> Result<Saved, String> {
    let not_one = || format!("llm: {source} is not a lattice llm session file");
    let file: Value = serde_json::from_str(text).map_err(|_| not_one())?;
    if file.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(not_one());
    }
    let version = file
        .get("version")
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .ok_or_else(not_one)?;
    if version > VERSION {
        return Err(format!(
            "llm: {source} was written by a newer lattice (session format {version}; this one \
             reads up to {VERSION}): update lattice to resume it"
        ));
    }
    let session = file.get("session").ok_or_else(not_one)?;
    let have = checksum(&serde_json::to_string(session).map_err(|_| not_one())?);
    if file.get("checksum").and_then(Value::as_str) != Some(have.as_str()) {
        return Err(format!(
            "llm: {source} was changed after lattice wrote it (its checksum does not match), so \
             its history may not be what the provider signed; not resumed"
        ));
    }
    let damaged = |e: String| format!("llm: {source} is damaged: {e}");
    let dto: SessionDto =
        serde_json::from_value(session.clone()).map_err(|e| damaged(e.to_string()))?;
    let settings = dto.settings.clone().into_iter().collect();
    let conversation = conversation_of(dto).map_err(damaged)?;
    Ok(Saved {
        settings,
        conversation,
    })
}

/// FNV-1a, 64 bits: stable across builds and platforms, which a file needs
/// and `std`'s hasher does not promise.
fn checksum(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("fnv1a64:{h:016x}")
}

fn turn_dto(turn: &Turn) -> TurnDto {
    TurnDto {
        role: match turn.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
        .to_string(),
        blocks: turn.blocks.iter().map(block_dto).collect(),
    }
}

fn block_dto(block: &Block) -> BlockDto {
    match block {
        Block::Text(text) => BlockDto::Text { text: text.clone() },
        Block::Reasoning(text) => BlockDto::Reasoning { text: text.clone() },
        Block::ToolCall {
            id,
            origin,
            title,
            status,
            kind,
            input,
            output,
        } => BlockDto::ToolCall {
            id: id.clone(),
            origin: origin.clone(),
            title: title.clone(),
            status: status_tag(status).to_string(),
            kind: kind_tag(*kind).to_string(),
            input: input.clone(),
            output: output.clone(),
        },
        Block::Source {
            label, lang, text, ..
        } => BlockDto::Source {
            label: label.clone(),
            lang: lang.clone(),
            text: text.clone(),
        },
        Block::Edit { path, status } => BlockDto::Edit {
            path: path.clone(),
            status: edit_tag(status).to_string(),
        },
        Block::Permission {
            id,
            title,
            description,
            status,
            ..
        } => BlockDto::Permission {
            id: id.clone(),
            title: title.clone(),
            description: description.clone(),
            status: permission_tag(status).to_string(),
        },
    }
}

const KINDS: [ToolKind; 5] = [
    ToolKind::Read,
    ToolKind::Search,
    ToolKind::Edit,
    ToolKind::Execute,
    ToolKind::Other,
];

fn kind_tag(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Search => "search",
        ToolKind::Edit => "edit",
        ToolKind::Execute => "execute",
        ToolKind::Other => "other",
    }
}

/// The variant of `all` whose tag is `text`.
fn parse_tag<T: Copy>(
    all: &[T],
    tag: impl Fn(&T) -> &'static str,
    what: &str,
    text: &str,
) -> Result<T, String> {
    all.iter()
        .copied()
        .find(|v| tag(v) == text)
        .ok_or_else(|| format!("unknown {what} `{text}`"))
}

fn conversation_of(dto: SessionDto) -> Result<Conversation, String> {
    use EditStatus as E;
    use PermissionStatus as P;
    use ToolStatus as S;
    let mut turns = Vec::with_capacity(dto.turns.len());
    for t in dto.turns {
        let role = match t.role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            other => return Err(format!("unknown role `{other}`")),
        };
        let mut blocks = Vec::with_capacity(t.blocks.len());
        for b in t.blocks {
            blocks.push(match b {
                BlockDto::Text { text } => Block::Text(text),
                BlockDto::Reasoning { text } => Block::Reasoning(text),
                BlockDto::ToolCall {
                    id,
                    origin,
                    title,
                    status,
                    kind,
                    input,
                    output,
                } => Block::ToolCall {
                    id,
                    origin,
                    title,
                    status: parse_tag(
                        &[S::Pending, S::Running, S::Ok, S::Err],
                        status_tag,
                        "tool status",
                        &status,
                    )?,
                    kind: parse_tag(&KINDS, |k| kind_tag(*k), "tool kind", &kind)?,
                    input,
                    output,
                },
                BlockDto::Source { label, lang, text } => Block::Source {
                    label,
                    lang,
                    text,
                    link: None,
                },
                BlockDto::Edit { path, status } => Block::Edit {
                    path,
                    status: parse_tag(
                        &[E::Proposed, E::Accepted, E::Rejected],
                        edit_tag,
                        "edit status",
                        &status,
                    )?,
                },
                BlockDto::Permission {
                    id,
                    title,
                    description,
                    status,
                } => Block::Permission {
                    id,
                    title,
                    description,
                    options: Vec::new(),
                    status: parse_tag(
                        &[P::Pending, P::Allowed, P::Denied],
                        permission_tag,
                        "permission status",
                        &status,
                    )?,
                },
            });
        }
        turns.push(Turn { role, blocks });
    }
    let mut reasoning_data = BTreeMap::new();
    for r in dto.reasoning {
        let is_reasoning = turns
            .get(r.turn)
            .and_then(|t: &Turn| t.blocks.get(r.block))
            .is_some_and(|b| matches!(b, Block::Reasoning(_)));
        if !is_reasoning {
            return Err(format!(
                "reasoning data for turn {} block {} names no reasoning block",
                r.turn, r.block
            ));
        }
        reasoning_data.insert(
            (r.turn, r.block),
            OpaqueReasoning {
                provider: r.provider,
                data: r.data,
            },
        );
    }
    Ok(Conversation {
        turns,
        assistant_label: dto.label,
        reasoning_data,
        tool_calls: dto
            .tool_calls
            .into_iter()
            .map(|(id, c)| {
                (
                    id,
                    ToolCallData {
                        name: c.name,
                        input: c.input,
                        malformed: c.malformed,
                    },
                )
            })
            .collect(),
        ..Conversation::default()
    })
}

// ---------------------------------------------------------------------------
// The commands
// ---------------------------------------------------------------------------

/// Effects a file task hands back to the editor.
pub struct SessionFileEffects(pub Vec<Effect>);

/// The file side of `:llm-save`, `:llm-resume` and `:llm-export`.
pub struct LlmSessionFiles {
    runtime: Handle,
    chats: LlmChatsHandle,
    effects: Mutex<Option<InboundBus<SessionFileEffects>>>,
}

pub type LlmSessionFilesHandle = Arc<LlmSessionFiles>;

impl LlmSessionFiles {
    pub fn new(runtime: Handle, chats: LlmChatsHandle) -> Self {
        Self {
            runtime,
            chats,
            effects: Mutex::new(None),
        }
    }

    /// Connect the bus a task's result returns on.
    pub fn set_effects(&self, bus: InboundBus<SessionFileEffects>) {
        *self.effects.lock().unwrap_or_else(|e| e.into_inner()) = Some(bus);
    }

    fn send(&self, effects: Vec<Effect>) {
        let guard = self.effects.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(bus) => {
                let _ = bus.send(SessionFileEffects(effects));
            }
            // A harness with no editor: the result still reaches the log.
            None => {
                tracing::debug!(target: "lattice::llm", "llm: a file task finished with no editor to tell")
            }
        }
    }

    fn report(&self, result: Result<String, String>) {
        let (level, text) = match result {
            Ok(text) => (EchoLevel::Info, text),
            Err(text) => (EchoLevel::Error, text),
        };
        self.send(vec![Effect::Echo { level, text }]);
    }

    /// `:llm-export file`: the chat in `buffer` as markdown, or org for a
    /// `.org` file. An existing file is kept unless `overwrite` (`!`).
    pub fn export(self: &Arc<Self>, buffer: BufferId, file: &str, overwrite: bool) -> Effect {
        let Some(session) = self.chats.for_buffer(buffer) else {
            return error("llm: :llm-export works in a chat buffer".into());
        };
        let (path, shown) = match target(file, "llm-export") {
            Ok(t) => t,
            Err(e) => return error(e),
        };
        let conv = session.store.snapshot();
        if conv.turns.is_empty() {
            return error("llm: nothing to export yet".into());
        }
        let name = session.name.clone();
        let this = self.clone();
        let org = is_org(&path);
        self.runtime.spawn(async move {
            let done = async {
                let title = name.clone();
                let text = cpu(move || {
                    if org {
                        super::export::org(&conv, &title)
                    } else {
                        super::export::markdown(&conv)
                    }
                })
                .await?;
                let lines = text.lines().count();
                write_file(&path, &shown, text, overwrite).await?;
                Ok(format!("llm: wrote {name} to {shown} ({lines} lines)"))
            }
            .await;
            this.report(done);
        });
        Effect::None
    }

    /// `:llm-save file`: the chat in `buffer` as a session file. A system
    /// prompt read from a file is kept as its text, since a session file may
    /// not name a path.
    pub fn save(self: &Arc<Self>, buffer: BufferId, file: &str, overwrite: bool) -> Effect {
        let (path, shown) = match target(file, "llm-save") {
            Ok(t) => t,
            Err(e) => return error(e),
        };
        let (conv, mut settings) = match self.chats.saved_settings(buffer) {
            Ok(saved) => saved,
            Err(e) => return error(e),
        };
        let name = self
            .chats
            .for_buffer(buffer)
            .map(|s| s.name.clone())
            .unwrap_or_default();
        let this = self.clone();
        self.runtime.spawn(async move {
            let done = async {
                if let Some(system) = settings
                    .get(&SettingKey::System)
                    .filter(|s| system_is_path(s))
                    .cloned()
                {
                    let text = super::chat::read_system_file(&system)
                        .await
                        .map_err(|e| format!("llm: {e}"))?;
                    settings.insert(SettingKey::System, text);
                }
                let text = cpu(move || encode(&conv, &settings)).await??;
                write_file(&path, &shown, text, overwrite).await?;
                Ok(format!(
                    "llm: saved {name} to {shown}; :llm-resume {shown} continues it"
                ))
            }
            .await;
            this.report(done);
        });
        Effect::None
    }

    /// `:llm-resume file`: a new chat from a session file, opened when read.
    pub fn resume(self: &Arc<Self>, file: &str) -> Effect {
        let (path, shown) = match target(file, "llm-resume") {
            Ok(t) => t,
            Err(e) => return error(e),
        };
        let this = self.clone();
        self.runtime.spawn(async move {
            let read = async {
                let text = tokio::fs::read_to_string(&path)
                    .await
                    .map_err(|e| format!("llm: cannot read {shown}: {}", e.kind()))?;
                let source = shown.clone();
                cpu(move || decode(&text, &source)).await?
            }
            .await;
            let saved = match read {
                Ok(saved) => saved,
                Err(e) => return this.report(Err(e)),
            };
            // The file may choose the model, and with it another of your
            // providers: say so, since your code would go there.
            let model = saved
                .settings
                .iter()
                .find(|(k, _)| k == "model")
                .map(|(_, v)| v.clone());
            let elsewhere = this.chats.other_provider(model.as_deref());
            let (session, refused) = this.chats.resume(saved, &shown);
            for refusal in &refused {
                tracing::warn!(target: "lattice::llm", "{refusal}");
            }
            let mut text = format!("llm: resumed {shown} as {}", session.name);
            if !refused.is_empty() {
                text.push_str(&format!(
                    "; {} of its settings refused (see *messages*)",
                    refused.len()
                ));
            }
            if let Some((other, usual)) = &elsewhere {
                text.push_str(&format!(
                    "; it uses provider `{other}`, not your default `{usual}`"
                ));
            }
            let level = if refused.is_empty() && elsewhere.is_none() {
                EchoLevel::Info
            } else {
                EchoLevel::Warn
            };
            this.send(vec![
                Effect::OpenSyntheticBuffer {
                    name: session.name.clone(),
                    mode_id: LlmChatMode::mode_id().as_str().to_string(),
                    content: None,
                    cursor: None,
                    activate_minor: None,
                },
                Effect::Echo { level, text },
            ]);
        });
        Effect::None
    }
}

/// Run CPU-bound work (an encode, a decode, a render) on the blocking pool.
/// The editor's runtime may be single-threaded, where a spawned task's work
/// would run on the editor's own thread.
async fn cpu<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("llm: the file task stopped: {e}"))
}

fn error(text: String) -> Effect {
    Effect::Echo {
        level: EchoLevel::Error,
        text,
    }
}

/// The path `file` names, `~/` expanded, and how to show it. Empty is refused:
/// these commands never pick a name for you.
fn target(file: &str, command: &str) -> Result<(PathBuf, String), String> {
    let shown = file.trim();
    if shown.is_empty() {
        return Err(format!("llm: :{command} needs a file name"));
    }
    let path = match shown.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => return Err(format!("llm: cannot expand {shown}: HOME is not set")),
        },
        None => PathBuf::from(shown),
    };
    Ok((path, shown.to_string()))
}

fn is_org(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("org"))
}

/// Write `text` to `path` whole or not at all: to a sibling first, then
/// moved into place, so a failed write never leaves half a file. Without
/// `overwrite` an existing file is kept, as `:w` keeps one, and that holds
/// even if one appears meanwhile: the move is a link that fails on an
/// existing name.
///
/// A chat holds file contents and tool output, so a new file is the owner's
/// alone (`0600`); one overwritten keeps the mode it had. The sibling has an
/// unguessable name and is created only if nothing is there, so a link
/// planted at it cannot redirect the write.
async fn write_file(path: &Path, shown: &str, text: String, overwrite: bool) -> Result<(), String> {
    let (path, shown) = (path.to_path_buf(), shown.to_string());
    tokio::task::spawn_blocking(move || write_file_blocking(&path, &shown, &text, overwrite))
        .await
        .unwrap_or_else(|e| Err(format!("llm: the write stopped ({e})")))
}

fn write_file_blocking(
    path: &Path,
    shown: &str,
    text: &str,
    overwrite: bool,
) -> Result<(), String> {
    use std::io::Write as _;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let exists = |shown: &str| format!("llm: {shown} exists (add ! to overwrite)");
    if !overwrite && path.symlink_metadata().is_ok() {
        return Err(exists(shown));
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp = path.with_file_name(format!(
        ".{name}.{}-{}-{nonce:x}.lattice-tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let fail = |e: std::io::Error| format!("llm: cannot write {shown}: {}", e.kind());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = (|| {
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
        if let Ok(kept) = std::fs::metadata(path) {
            file.set_permissions(kept.permissions())?;
        }
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(fail(e));
    }
    let placed = if overwrite {
        std::fs::rename(&tmp, path)
    } else {
        match std::fs::hard_link(&tmp, path) {
            // A filesystem without links: check, then move, as before.
            Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
                if path.symlink_metadata().is_ok() {
                    Err(std::io::ErrorKind::AlreadyExists.into())
                } else {
                    std::fs::rename(&tmp, path)
                }
            }
            linked => linked,
        }
    };
    let _ = std::fs::remove_file(&tmp).or_else(|e| match e.kind() {
        std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(e),
    });
    match placed {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(exists(shown)),
        Err(e) => Err(fail(e)),
    }
}

fn arg_text(args: &Args) -> String {
    super::commands::arg_text(args)
}

/// Register `:llm-export`, `:llm-save` and `:llm-resume`.
pub fn register(registry: &mut CommandRegistry, files: LlmSessionFilesHandle) {
    let export = files.clone();
    registry.register_ex_command(
        "llm-export",
        "Write this chat to a file for keeping: markdown, or org for a `.org` file, with every \
         block shown. `!` overwrites an existing file. Not read back: `:llm-save` keeps a chat \
         to resume.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| {
                Ok(export.export(ctx.buffer_id, &arg_text(&ctx.args), ctx.bang))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    let save = files.clone();
    registry.register_ex_command(
        "llm-save",
        "Save this chat to a session file, to continue later with `:llm-resume`: its history, \
         tool calls and model settings. Never a key. `!` overwrites an existing file.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: true,
            accepts_range: false,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| {
                Ok(save.save(ctx.buffer_id, &arg_text(&ctx.args), ctx.bang))
            }),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
    registry.register_ex_command(
        "llm-resume",
        "Open a new chat from a session file `:llm-save` wrote. A file from elsewhere may choose \
         the model and narrow tools, never widen them or name a key or server. A file changed \
         after it was written, or from a newer lattice, is refused.",
        ExCommandSpec {
            latency_class: LatencyClass::Reflex,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(parse_rest_as_text),
            apply: Arc::new(move |ctx| Ok(files.resume(&arg_text(&ctx.args)))),
            args_schema: vec![],
            surface_form: SurfaceForm::Keyword,
        },
    );
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A new save is the owner's alone, an overwritten one keeps its mode,
    /// and without `!` an existing file is never replaced.
    #[cfg(unix)]
    #[test]
    fn a_saved_chat_is_private_and_never_clobbered() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("lattice-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("chat.json");
        write_file_blocking(&path, "chat.json", "one", false).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");

        let err = write_file_blocking(&path, "chat.json", "two", false).unwrap_err();
        assert!(err.contains("exists"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_file_blocking(&path, "chat.json", "three", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "three");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "{mode:o}");
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
        assert_eq!(left.len(), 1, "no temp file left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_checksum_is_fixed_for_a_given_text() {
        // Pinned: a changed function would refuse every file already written.
        assert_eq!(checksum(""), "fnv1a64:cbf29ce484222325");
        assert_eq!(checksum("a"), "fnv1a64:af63dc4c8601ec8c");
    }
}
