//! Chat sessions: one per `*llm:<n>*` buffer, and the turn driver.
//!
//! A [`ChatSession`] owns a [`ConversationStore`], the model of the chat; the
//! buffer is only its projection (see `conversation::transcript`). Submitting a
//! prompt folds a user turn into the store and starts a request whose
//! [`ChatSink`] folds the streamed events back in, so every byte the user sees
//! arrives through the model and the drain, with no keystroke in between.
//!
//! [`LlmChats`] is the registry and the driver. It is the one thing the ex-command
//! (which creates a chat), the mode (which binds it to a buffer) and the chords
//! (which submit and interrupt) share, registered as a service.
//!
//! # Turn lifecycle
//!
//! `busy` is set when a turn is submitted and cleared when its request reaches a
//! terminal state, which the registry announces on the bus. A prompt submitted
//! while busy goes to the model's `queued` list and is shown in the transcript;
//! it becomes a user turn only when it is actually sent, so a streaming reply is
//! never split by a later question. Queued prompts are sent in order.
//!
//! A turn that ends by interrupt hands over to the next queued prompt. A turn
//! that ends in failure does not: the error is shown, the queued prompts stay
//! visible, and the next explicit send resumes them in order. The failed user
//! turn is left out of the provider history (see `history::messages`), so two
//! user turns are never sent back to back.
//!
//! The queue and `busy` are decided together under the session's turn lock, in
//! `submit` and in `end_turn`, so a send landing between a turn ending and the
//! next being chosen cannot strand a prompt.

use super::path_rules::{PathRules, ProjectRules};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};

use lattice_agent::SessionKey;
use lattice_config::ConfigRegistry;
use lattice_core::BufferId;
use tokio::runtime::Handle;

use super::ApiKey;
use super::config::{LlmConfig, ProviderConfig, omitted_params, resolve_key};
use super::context::{Attached, ContextItem, WatchHandle};
use super::history;
use super::settings::{
    Layers, ProjectLayer, RawSet, RequestOverrides, Resolved, SettingKey, SettingsError, Trust,
    global_set, resolve, session_set, system_is_path,
};
use super::{
    LlmRequest, Message, Params, RequestId, RequestRegistryHandle, RequestState, RequestTarget,
    Sink, SinkFlow, StopReason, StreamEvent,
};
use crate::conversation::{
    Conversation, ConversationStore, Cost, FoldDisplay, OpaqueReasoning, ProjectionOptions, Role,
    SessionStatus, ToolCallData, ToolKind, ToolStatus, UsageSnapshot,
};

/// The part of a buffer name a chat's name carries: no `*` and no whitespace,
/// so `*llm:<hint>:<n>*` stays one recognisable token, and at most 40 chars.
fn name_hint(hint: &str) -> String {
    hint.chars()
        .filter(|c| *c != '*')
        .map(|c| if c.is_whitespace() { '-' } else { c })
        .take(40)
        .collect()
}

/// Provider tag of a chat's [`SessionKey`].
pub const SESSION_PROVIDER: &str = "llm";

pub type LlmChatsHandle = Arc<LlmChats>;

/// Sees each piece of reply text as it streams, on the request task: inline
/// output (LL.13b) writes the reply into a buffer from here. Must not block.
pub type TextTap = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default)]
struct TurnState {
    /// Set by [`ChatSession::set_text_tap`]; every request this session
    /// starts hands its reply text to it as well as to the conversation.
    tap: Option<TextTap>,
    busy: bool,
    request: Option<RequestId>,
    interrupted: bool,
    /// What the turn in flight was asked with: the request-layer overrides of
    /// the prompt that started it (a command argument, an `@profile`, a menu
    /// choice for the next request). Read once, when the request starts.
    current: RequestOverrides,
    /// The same for each queued prompt, in step with `Conversation::queued`:
    /// both are pushed and popped under the turn lock, so a queued prompt
    /// keeps its own overrides and the next one never inherits them.
    pending: VecDeque<RequestOverrides>,
}

/// What a typed prompt asks of the request, decided from its `@profile` cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendPlan {
    /// No cookie: send with the session's own settings.
    Plain,
    /// A known profile that does not widen tools. It applies to this request
    /// through the restricted path, as pasted text may.
    Profile(String),
    /// A known profile that would enable tools the session does not have. The
    /// user is asked first; only a yes applies it, with full trust, to this
    /// request alone.
    NeedsConfirm(String),
}

/// Where a `:llm-menu` change goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MenuScope {
    /// The buffer's session: every request from it until cleared.
    #[default]
    Session,
    /// The next request only.
    NextRequest,
}

pub struct ChatSession {
    pub name: String,
    pub key: SessionKey,
    pub store: ConversationStore,
    /// The session settings copied when the chat was created: the requesting
    /// buffer's overrides, or just a profile. Nothing stays linked to the
    /// source. The chat buffer's own `:setlocal` values sit above these.
    seed: Mutex<RawSet>,
    /// The model label shown in the headerline: what the next request would use.
    label: Mutex<Option<String>>,
    turn: Mutex<TurnState>,
    /// The resolved key and the provider it belongs to, fetched once per
    /// provider so a password manager prompts once, not once per turn.
    api_key: Mutex<Option<(String, ApiKey)>>,
    /// Refusals and omissions already reported for this chat, so a project's
    /// bad key is named once, not on every turn.
    warned: Mutex<HashSet<String>>,
    /// Set when the buffer closes: no queued prompt is sent after it.
    closed: AtomicBool,
    /// Told when the turn in flight ends: the reply's text, or why there is
    /// none. A rewrite (LL.11) waits here for the text it sends to review.
    turn_waiters: Mutex<Vec<tokio::sync::oneshot::Sender<Result<String, String>>>>,
    /// The buffer the chat was made from, if any. A hidden ask's requests
    /// belong to it: `:llm-stop` there stops them (LL.14b).
    source: Option<BufferId>,
    /// Where this session's answers go, for `:llm-requests`: `below foo.rs:4`,
    /// `echo`, a register, a rewrite. A chat's is its own name.
    destination: Mutex<Option<String>>,
    /// What this session's requests carry as context (LL.15): copied from
    /// its source when it was made, its own from then on.
    context: Mutex<Vec<Attached>>,
    /// The tag this session's diffs carry, so closing the chat or stopping
    /// its request closes them (LL.17). Above [`ORIGIN_BASE`]: an IDE
    /// connection's ids count from 1, and the two must never meet.
    origin: u64,
    /// `<C-t>`: this session's edits apply without a review. Never saved and
    /// never copied: it ends when the chat closes.
    trusted: Arc<AtomicBool>,
    /// Tools the user allowed "for this chat" when asked (LL.18). Like trust,
    /// never saved, never copied, and gone when the chat closes.
    allowed_tools: Arc<Mutex<HashSet<String>>>,
    /// The text a fork's prompt starts with (LL.19), taken by the first
    /// activation so a reopened buffer does not refill it.
    prompt_seed: Mutex<Option<String>>,
}

/// Where llm sessions' diff tags start (see [`ChatSession::origin`]).
pub const ORIGIN_BASE: u64 = 1 << 62;

static NEXT_ORIGIN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl ChatSession {
    /// The tag this session's diffs carry.
    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// The text this chat's prompt starts with, once (LL.19's fork).
    pub fn take_prompt_seed(&self) -> Option<String> {
        self.prompt_seed
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Whether this session's edits apply without a review.
    pub fn trusted(&self) -> bool {
        self.trusted.load(Ordering::Acquire)
    }

    /// Flip `<C-t>` trust; returns the new value.
    pub fn toggle_trust(&self) -> bool {
        !self.trusted.fetch_xor(true, Ordering::AcqRel)
    }

    /// Resolves when the turn now in flight (or the next one, if none is)
    /// ends: `Ok` with the reply's text, `Err` with the error, or
    /// `"interrupted"` when it was stopped.
    pub fn wait_turn_end(&self) -> tokio::sync::oneshot::Receiver<Result<String, String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.turn_waiters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Also hand this session's reply text to `tap` as it streams, for every
    /// request it starts from now on. The conversation still records it, so
    /// the full reply can be seen and followed up as a chat.
    pub fn set_text_tap(&self, tap: TextTap) {
        self.turn().tap = Some(tap);
    }

    /// The text of the latest assistant turn.
    pub fn last_reply(&self) -> String {
        self.store
            .snapshot()
            .turns
            .iter()
            .rev()
            .find(|t| t.role == crate::conversation::Role::Assistant)
            .map(|t| {
                t.blocks
                    .iter()
                    .filter_map(|b| match b {
                        crate::conversation::Block::Text(text) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<String>()
            })
            .unwrap_or_default()
    }

    /// The model the next request would use, for the headerline.
    pub fn model(&self) -> Option<String> {
        self.label.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_label(&self, label: Option<String>) {
        *self.label.lock().unwrap_or_else(|e| e.into_inner()) = label;
    }

    /// Say where this session's answers go, for `:llm-requests`. Set before
    /// the request starts: the listing reads it when the request does.
    pub fn set_destination(&self, destination: String) {
        *self.destination.lock().unwrap_or_else(|e| e.into_inner()) = Some(destination);
    }

    /// Where this session's answers go: what [`set_destination`](Self::set_destination)
    /// said, else the chat itself.
    pub fn destination(&self) -> String {
        self.destination
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_else(|| self.name.clone())
    }

    /// The settings this chat was created with.
    pub fn seed(&self) -> RawSet {
        self.seed.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn turn(&self) -> MutexGuard<'_, TurnState> {
        self.turn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True while a request is in flight or being prepared.
    pub fn is_busy(&self) -> bool {
        self.turn().busy
    }

    /// True the first time `what` is reported for this chat.
    fn first_report(&self, what: String) -> bool {
        self.warned
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(what)
    }
}

/// The providers whose reasoning data `messages` carry but a provider with
/// `tag` would not accept (LL.19: a retry on another model).
fn foreign_reasoning(
    messages: &[Message],
    tag: Option<&str>,
) -> std::collections::BTreeSet<String> {
    messages
        .iter()
        .filter_map(|m| match m {
            Message::Assistant(parts) => Some(parts),
            _ => None,
        })
        .flatten()
        .filter_map(|p| match p {
            super::AssistantPart::Reasoning { data: Some(d), .. }
                if Some(d.provider.as_str()) != tag =>
            {
                Some(d.provider.clone())
            }
            _ => None,
        })
        .collect()
}

/// A request ready to send but for its key ([`LlmChats::prepare`]).
struct Prepared {
    req: LlmRequest,
    provider: ProviderConfig,
    /// The tools this request may call, fixed for its whole loop.
    offer: Arc<super::tools::Offer>,
}

#[derive(Default)]
struct Registry {
    /// Chats no buffer is bound to: freshly created ones awaiting their buffer's
    /// activation, and ones whose buffer's guard dropped, awaiting a
    /// reactivation. Oldest first, bounded by `UNBOUND_CAP`.
    unbound: Vec<Arc<ChatSession>>,
    by_buffer: HashMap<BufferId, (u64, Arc<ChatSession>)>,
    by_request: HashMap<RequestId, Arc<ChatSession>>,
    /// Terminal states announced before their request was registered here.
    ended_early: HashMap<RequestId, RequestState>,
    /// Insertion order of `ended_early`, for evicting the oldest.
    ended_order: VecDeque<RequestId>,
}

/// Bound on `ended_early`: requests this registry never owns would otherwise
/// accumulate there.
const ENDED_EARLY_CAP: usize = 256;

/// Bound on `Registry::unbound`: a buffer that never activates would otherwise
/// leave its chat there for good.
const UNBOUND_CAP: usize = 32;

/// What a menu-first operator holds while `:llm-menu` is open over its
/// buffer: the menu's `a` row moves it on to its instruction prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHold {
    /// `<leader>aA`: an ask.
    Ask,
    /// `<leader>aR`: a rewrite.
    Rewrite,
}

/// A held operation: which kind, its token, and the lines it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuHeld {
    pub kind: MenuHold,
    pub token: u64,
    pub lines: String,
}

/// [`LlmChats::exclude_rules_for`]'s cache entry.
struct CompiledExclude {
    globs: super::options::ExcludeGlobs,
    root: std::path::PathBuf,
    rules: Arc<PathRules>,
}

pub struct LlmChats {
    runtime: Handle,
    requests: RequestRegistryHandle,
    bus: Arc<lattice_runtime::EventBus>,
    config: RwLock<LlmConfig>,
    /// `llm.exclude` compiled against the tools root, with the list and root
    /// it was compiled from: rebuilt when either changes, never per check.
    exclude_rules: RwLock<Option<CompiledExclude>>,
    /// The global registry, where `:set llm.*` and the config file's scalar
    /// settings land. Absent in tests that run without a host.
    registry: RwLock<Option<Arc<ConfigRegistry>>>,
    /// Buffer-local `llm.*` values per buffer, mirrored from the host by
    /// [`on_buffer_options`](Self::on_buffer_options). Looked up per call: this
    /// map is replaced entry by entry, so a clone of one entry would go stale.
    sessions: RwLock<HashMap<BufferId, RawSet>>,
    /// Buffers whose `ReadOnly` option is on, mirrored by
    /// [`on_buffer_options`](Self::on_buffer_options): inline output is never
    /// written into one (LL.13b).
    read_only: RwLock<std::collections::HashSet<BufferId>>,
    /// The fold values each chat buffer was last re-projected under, so a change
    /// at ANY layer (a global `:set` leaves the session values untouched) is seen.
    folds_seen: RwLock<HashMap<BufferId, (FoldDisplay, FoldDisplay)>>,
    /// `:llm-menu` choices made for the next request only, per buffer. Taken by
    /// the next submission, so they never reach a second request.
    next_request: RwLock<HashMap<BufferId, RawSet>>,
    /// Which scope `:llm-menu` writes to, per buffer. It stays where the user
    /// left it: switching it is a mode, not a one-shot.
    menu_scope: RwLock<HashMap<BufferId, MenuScope>>,
    /// The ask `<leader>aA` holds while its menu is open over a buffer: the
    /// ask's token and the lines it covers. The menu shows an `a` row for it,
    /// which opens the instruction prompt; opening the menu any other way
    /// disarms it, so a later `:llm-menu` offers no stale ask.
    menu_ask: RwLock<HashMap<BufferId, MenuHeld>>,
    /// Where `.lattice/llm.toml` is looked for. `None` finds the project root
    /// from the working directory when a request starts.
    project_root: RwLock<Option<std::path::PathBuf>>,
    /// The project layer as last read, for the menu: it is built on the editor
    /// thread, where no file may be read. Refreshed off-thread at chat start and
    /// by every request. `Arc` so a refresh task can write it.
    project_cache: Arc<RwLock<Option<ProjectLayer>>>,
    /// The project's safety-list additions, compiled off-thread with the
    /// layer above and refreshed with it (design §6 "Safety lists").
    project_rules: Arc<RwLock<Option<Arc<ProjectRules>>>>,
    /// The latest ask's session, by name, for `:llm-last` (LL.14a). Every ask
    /// has one, shown or hidden: echo, register, `below` and rewrite answers
    /// keep theirs so the full reply can be seen and followed up.
    last_ask: Mutex<Option<String>>,
    /// Told when a shown chat's turn ends (LL.14d): the chat's buffer, its
    /// name, and the failure if it failed. Install sets it to a notice for a
    /// chat whose buffer is not displayed; unset, nothing is told.
    done_hook: std::sync::OnceLock<DoneHook>,
    /// The host's buffers: context is read from them when a request is sent,
    /// and the list names them. Set once, at install; absent in bare tests.
    store: std::sync::OnceLock<lattice_mode::BufferStoreHandle>,
    /// The multibuffer views, so `llm.exclude` can see which files a composed
    /// view shows. Set once, at install; absent, every composed view is
    /// refused, since nothing can tell what it holds.
    composed: std::sync::OnceLock<lattice_multibuffer::MultibufferRegistryHandle>,
    /// Context attached in buffers that are not chats, per buffer (LL.15). A
    /// chat's own is on its session.
    source_context: RwLock<HashMap<BufferId, Vec<Attached>>>,
    /// Whose context `*llm-context*` shows: the buffer `:llm-context` ran in.
    listing_owner: Mutex<Option<BufferId>>,
    /// Bumped when any context changes, or the list's owner does.
    context_watch: WatchHandle,
    /// What `*llm-inspect*` shows, and its counter.
    inspect: Mutex<String>,
    inspect_watch: WatchHandle,
    /// The last request sent from each buffer, as `:llm-inspect` shows it.
    last_sent: RwLock<HashMap<BufferId, Arc<String>>>,
    /// LL.16: every tool source, namespaced; a request's tools are an offer
    /// made from it when the request starts.
    tools: super::tools::ToolBrokerHandle,
    next_chat: AtomicU32,
    next_system: AtomicU32,
    next_bind: AtomicU64,
    inner: Mutex<Registry>,
}

impl LlmChats {
    pub fn new(
        runtime: Handle,
        requests: RequestRegistryHandle,
        bus: Arc<lattice_runtime::EventBus>,
        config: LlmConfig,
    ) -> Self {
        Self {
            runtime,
            requests,
            bus,
            config: RwLock::new(config),
            exclude_rules: RwLock::new(None),
            registry: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
            read_only: RwLock::new(Default::default()),
            folds_seen: RwLock::new(HashMap::new()),
            next_request: RwLock::new(HashMap::new()),
            menu_scope: RwLock::new(HashMap::new()),
            menu_ask: RwLock::new(HashMap::new()),
            project_root: RwLock::new(None),
            project_cache: Arc::new(RwLock::new(None)),
            project_rules: Arc::new(RwLock::new(None)),
            last_ask: Mutex::new(None),
            done_hook: std::sync::OnceLock::new(),
            store: std::sync::OnceLock::new(),
            composed: std::sync::OnceLock::new(),
            source_context: RwLock::new(HashMap::new()),
            listing_owner: Mutex::new(None),
            context_watch: Arc::default(),
            inspect: Mutex::new(String::new()),
            inspect_watch: Arc::default(),
            last_sent: RwLock::new(HashMap::new()),
            tools: Arc::default(),
            next_chat: AtomicU32::new(1),
            next_system: AtomicU32::new(1),
            next_bind: AtomicU64::new(1),
            inner: Mutex::new(Registry::default()),
        }
    }

    /// Replace the configuration new chats are created from. A chat already open
    /// resolves against it on its next request, like any other layer.
    /// The variables the configured providers' keys are in, as configured
    /// now (P.1): what a command or MCP server the model starts is not given.
    pub fn key_variables(&self) -> Vec<String> {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .key_variables()
    }

    pub fn set_config(&self, config: LlmConfig) {
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = config;
    }

    /// Give chats the global registry, so `:set llm.model=x` applies live.
    pub fn set_registry(&self, registry: Arc<ConfigRegistry>) {
        *self.registry.write().unwrap_or_else(|e| e.into_inner()) = Some(registry);
    }

    /// Look for `.lattice/llm.toml` under `root` instead of the project root
    /// found from the working directory.
    /// LL.16: the tool broker. A source added here is offered to any request
    /// whose `llm.tools` selects it.
    pub fn tools(&self) -> &super::tools::ToolBrokerHandle {
        &self.tools
    }

    /// The root lattice's tools are confined to: the configured project root,
    /// or the project around the working directory.
    pub fn tools_root(&self) -> std::path::PathBuf {
        self.configured_root()
            .or_else(lattice_core::project::root_from_cwd)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    /// `llm.tool_rounds`, the most rounds of tool calls one request may run.
    fn tool_rounds(&self) -> usize {
        self.registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|r| r.get_typed::<super::options::LlmToolRounds>())
            .map(|v| *v)
            .unwrap_or(super::options::DEFAULT_TOOL_ROUNDS)
            .max(1) as usize
    }

    pub fn set_project_root(&self, root: std::path::PathBuf) {
        *self.project_root.write().unwrap_or_else(|e| e.into_inner()) = Some(root);
        // The lists belong to the root: read the new one's now, off-thread.
        self.refresh_project();
    }

    /// The names of the configured profiles.
    pub fn profile_names(&self) -> Vec<String> {
        self.config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .profile_names()
            .map(str::to_string)
            .collect()
    }

    fn global_layer(&self) -> RawSet {
        let mut set = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .clone();
        if let Some(reg) = self
            .registry
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            set.extend(global_set(reg));
        }
        set
    }

    /// When `model` names a configured provider (`name/model`) other than
    /// the one requests go to by default, that provider's name and the
    /// default's. A project file or a session file may choose the model, so
    /// it may send your code, with that provider's key, somewhere you did not
    /// pick; this is what lets that be said.
    pub fn other_provider(&self, model: Option<&str>) -> Option<(String, String)> {
        let config = self.config.read().unwrap_or_else(|e| e.into_inner());
        let default = self.default_provider();
        let chosen = config.choose(model, default.as_deref()).ok()?;
        let usual = config.choose(None, default.as_deref()).ok()?;
        (chosen.provider.name != usual.provider.name)
            .then(|| (chosen.provider.name.clone(), usual.provider.name.clone()))
    }

    fn default_provider(&self) -> Option<String> {
        let reg = self.registry.read().unwrap_or_else(|e| e.into_inner());
        reg.as_ref()
            .and_then(|r| r.get_typed::<super::options::LlmDefaultProvider>())
            .map(|v| (*v).clone())
            .filter(|v| !v.trim().is_empty())
    }

    /// Everything but a freshly read project layer: the project layer is the
    /// cached one (a file, read off-thread), which a request replaces with its
    /// own read.
    fn layers_for(&self, buffer: BufferId, session: RawSet) -> Layers {
        let cfg = self.config.read().unwrap_or_else(|e| e.into_inner());
        Layers {
            global: self.global_layer(),
            profiles: cfg.profiles.clone(),
            default_profile: cfg.default_profile.clone(),
            project: self
                .project_cache
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            sessions: [(buffer, session)].into_iter().collect(),
        }
    }

    /// A buffer's session overrides: the chat's creation-time seed, if the
    /// buffer is a chat, under the live `:setlocal` values.
    pub fn session_overrides(&self, buffer: BufferId) -> RawSet {
        let chat = self.for_buffer(buffer);
        self.overrides_of(chat.as_deref(), buffer)
    }

    /// [`session_overrides`](Self::session_overrides) for a chat already in hand,
    /// which need not be bound to a buffer yet (a test, a chat awaiting its
    /// buffer's activation).
    fn overrides_of(&self, chat: Option<&ChatSession>, buffer: BufferId) -> RawSet {
        let mut set = chat.map(ChatSession::seed).unwrap_or_default();
        if let Some(live) = self
            .sessions
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&buffer)
        {
            set.extend(live.iter().map(|(k, v)| (*k, v.clone())));
        }
        set
    }

    /// Take a buffer's re-resolved options (the bus event the host publishes).
    /// Returns true when its session values changed. An open chat is then
    /// re-projected, so a fold option applies without a keypress: the update the
    /// drain consumes is the same one a streamed chunk publishes.
    pub fn on_buffer_options(
        &self,
        buffer: BufferId,
        options: &lattice_config::ResolvedOptions,
    ) -> bool {
        {
            let read_only = options
                .get::<lattice_config::ReadOnly>()
                .is_some_and(|v| *v);
            let mut set = self.read_only.write().unwrap_or_else(|e| e.into_inner());
            if read_only {
                set.insert(buffer);
            } else {
                set.remove(&buffer);
            }
        }
        let new = session_set(options);
        let changed = {
            let mut map = self.sessions.write().unwrap_or_else(|e| e.into_inner());
            let old = map.get(&buffer).cloned().unwrap_or_default();
            if old == new {
                false
            } else if new.is_empty() {
                map.remove(&buffer);
                true
            } else {
                map.insert(buffer, new);
                true
            }
        };
        // Compare the EFFECTIVE fold values (session over global), not just the
        // session set: `:set llm.fold.reasoning=open` changes only the global
        // registry, and the host's re-resolution then arrives with the session
        // values unchanged.
        let mut repaint = changed;
        if let Some(chat) = self.for_buffer(buffer) {
            let now = self.projection_options_for(buffer);
            let now = (now.reasoning, now.tools);
            let before = self
                .folds_seen
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(buffer, now)
                .unwrap_or_default();
            repaint |= before != now;
            if repaint {
                chat.store.mutate(&chat.key, |_| {});
            }
        }
        self.prune_closed();
        changed
    }

    /// Drop what is kept per buffer for buffers that have closed. Run on each
    /// option snapshot (a buffer opening sends one), so what a closed buffer
    /// left lasts until the next buffer opens or changes an option, never for
    /// the rest of the session. Without the host's store nothing can be
    /// told, and nothing is dropped.
    fn prune_closed(&self) {
        let Some(store) = self.store.get() else {
            return;
        };
        let open = |b: &BufferId| store.contains_buffer(*b);
        fn keep<V>(map: &RwLock<HashMap<BufferId, V>>, open: impl Fn(&BufferId) -> bool) {
            map.write()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|b, _| open(b));
        }
        keep(&self.sessions, open);
        keep(&self.folds_seen, open);
        keep(&self.next_request, open);
        keep(&self.menu_scope, open);
        keep(&self.menu_ask, open);
        keep(&self.last_sent, open);
        let gone = {
            let mut ctx = self
                .source_context
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let before = ctx.len();
            ctx.retain(|b, _| open(b));
            before != ctx.len()
        };
        self.read_only
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .retain(open);
        let owner_gone = {
            let mut owner = self.listing_owner.lock().unwrap_or_else(|e| e.into_inner());
            let gone = owner.is_some_and(|b| !open(&b));
            if gone {
                *owner = None;
            }
            gone
        };
        if gone || owner_gone {
            self.context_watch.bump();
        }
    }

    /// How a chat buffer's transcript is projected right now. Fold values come
    /// from the session and global layers only: a project or profile cannot set
    /// them, so no file is read and no profile resolved here. Called once per
    /// projection, by the drain, which hands the result to the fold sources too.
    pub fn projection_options_for(&self, buffer: BufferId) -> ProjectionOptions {
        let session = self.session_overrides(buffer);
        let global = self.global_layer();
        let pick = |key: SettingKey| {
            session
                .get(&key)
                .or_else(|| global.get(&key))
                .and_then(|v| FoldDisplay::parse(v))
                .unwrap_or_default()
        };
        ProjectionOptions {
            markdown: true,
            reasoning: pick(SettingKey::FoldReasoning),
            tools: pick(SettingKey::FoldTools),
        }
    }

    /// The model a chat would use now, or the reason it cannot send. No file is
    /// read, so this is safe on the editor thread.
    fn provisional(&self, buffer: BufferId, session: RawSet) -> Result<String, String> {
        let layers = self.layers_for(buffer, session);
        let resolved = resolve(&layers, buffer, &RequestOverrides::default(), Trust::User)
            .map_err(|e: SettingsError| e.to_string())?;
        let cfg = self.config.read().unwrap_or_else(|e| e.into_inner());
        cfg.choose(
            resolved.values.model.as_deref(),
            self.default_provider().as_deref(),
        )
        .map(|c| c.model)
    }

    /// Resolve a buffer's settings as a request from it would see them, with the
    /// project layer as last cached. For the menu, introspection and tests.
    /// Whether `buffer`'s `ReadOnly` option is on, as last resolved.
    pub fn is_read_only(&self, buffer: BufferId) -> bool {
        self.read_only
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&buffer)
    }

    pub fn resolve_for(&self, buffer: BufferId) -> Result<Resolved, SettingsError> {
        let layers = self.layers_for(buffer, self.session_overrides(buffer));
        resolve(&layers, buffer, &RequestOverrides::default(), Trust::User)
    }

    /// Forget the creation-time seed of the chat bound to `buffer`, returning the
    /// keys it held. The profile switch clears it along with the live values.
    pub fn clear_seed(&self, buffer: BufferId) -> Vec<SettingKey> {
        let Some(chat) = self.for_buffer(buffer) else {
            return Vec::new();
        };
        let mut seed = chat.seed.lock().unwrap_or_else(|e| e.into_inner());
        let keys = seed.keys().copied().collect();
        seed.clear();
        keys
    }

    /// A fresh number for a `:llm-system` buffer's name.
    pub fn next_system_open(&self) -> u32 {
        self.next_system.fetch_add(1, Ordering::Relaxed)
    }

    /// The scope `:llm-menu` currently writes to for `buffer`.
    pub fn menu_scope(&self, buffer: BufferId) -> MenuScope {
        self.menu_scope
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&buffer)
            .copied()
            .unwrap_or_default()
    }

    /// Hold ask `token` (covering `lines`) for the menu opened over `buffer`.
    pub fn arm_menu_ask(&self, buffer: BufferId, token: u64, lines: String) {
        self.arm_menu(
            buffer,
            MenuHeld {
                kind: MenuHold::Ask,
                token,
                lines,
            },
        );
    }

    /// Hold rewrite `token` (covering `lines`) for the menu opened over
    /// `buffer` (`<leader>aR`).
    pub fn arm_menu_rewrite(&self, buffer: BufferId, token: u64, lines: String) {
        self.arm_menu(
            buffer,
            MenuHeld {
                kind: MenuHold::Rewrite,
                token,
                lines,
            },
        );
    }

    fn arm_menu(&self, buffer: BufferId, held: MenuHeld) {
        self.menu_ask
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(buffer, held);
    }

    /// The ask or rewrite the menu over `buffer` is holding, if any.
    pub fn menu_ask(&self, buffer: BufferId) -> Option<MenuHeld> {
        self.menu_ask
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&buffer)
            .cloned()
    }

    /// Release what the menu over `buffer` was holding.
    pub fn disarm_menu_ask(&self, buffer: BufferId) -> Option<MenuHeld> {
        self.menu_ask
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&buffer)
    }

    /// The `llm.exclude` glob `path` matches, if any: its text must not be sent.
    /// Without a registry (a bare test) the default list applies.
    ///
    /// Checked as given and as the file really is: a symlink named
    /// `notes.txt` pointing at `.env`, or a `../` path into an excluded
    /// directory, must not get past a glob that names the target. One
    /// `canonicalize` per user action, not per frame.
    pub fn excluded(&self, path: &std::path::Path) -> Option<String> {
        let reg = self.registry.read().unwrap_or_else(|e| e.into_inner());
        let globs = reg
            .as_ref()
            .and_then(|r| r.get_typed::<super::options::LlmExclude>())
            .map(|v| (*v).clone())
            .unwrap_or_default();
        drop(reg);
        // A list that will not compile (it was validated when set, so only a
        // hand-built registry can hold one) refuses every file: the safe side.
        let rules = match self.exclude_rules_for(globs) {
            Ok(rules) => rules,
            Err(e) => return Some(format!("llm.exclude does not compile: {e}")),
        };
        // The project's additions, a set of their own: its `!pattern` cannot
        // re-include what the global list excludes. Only for the root they
        // were read under; a root change refreshes them.
        let project = self
            .project_rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .filter(|p| p.root == self.tools_root());
        let check = |p: &std::path::Path| -> Option<String> {
            if let Some(pattern) = rules.matching(p) {
                return Some(pattern.to_string());
            }
            project
                .as_ref()
                .and_then(|r| r.exclude.matching(p))
                .map(|pattern| format!("{pattern} (project)"))
        };
        if let Some(why) = check(path) {
            return Some(why);
        }
        let real = std::fs::canonicalize(path).ok()?;
        check(&real)
    }

    /// The project's safety-list additions as last read, if any.
    pub fn project_rules(&self) -> Option<Arc<ProjectRules>> {
        self.project_rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `globs` compiled against the tools root, from the cache when neither
    /// changed.
    fn exclude_rules_for(
        &self,
        globs: super::options::ExcludeGlobs,
    ) -> Result<Arc<PathRules>, String> {
        let root = self.tools_root();
        if let Some(c) = self
            .exclude_rules
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|c| c.globs == globs && c.root == root)
        {
            return Ok(c.rules.clone());
        }
        let rules = Arc::new(PathRules::new(&root, &globs.0)?);
        *self
            .exclude_rules
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(CompiledExclude {
            globs,
            root,
            rules: rules.clone(),
        });
        Ok(rules)
    }

    /// Flip the menu's scope for `buffer` and return the new one.
    pub fn toggle_menu_scope(&self, buffer: BufferId) -> MenuScope {
        let mut map = self.menu_scope.write().unwrap_or_else(|e| e.into_inner());
        let now = match map.get(&buffer).copied().unwrap_or_default() {
            MenuScope::Session => MenuScope::NextRequest,
            MenuScope::NextRequest => MenuScope::Session,
        };
        map.insert(buffer, now);
        now
    }

    /// The menu choices waiting for `buffer`'s next request.
    pub fn next_request(&self, buffer: BufferId) -> RawSet {
        self.next_request
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&buffer)
            .cloned()
            .unwrap_or_default()
    }

    /// Set (or, with empty text, unset) one choice for `buffer`'s next request.
    pub fn set_next_request(&self, buffer: BufferId, key: SettingKey, text: &str) {
        let mut map = self.next_request.write().unwrap_or_else(|e| e.into_inner());
        let set = map.entry(buffer).or_default();
        if text.trim().is_empty() {
            set.remove(&key);
        } else {
            set.insert(key, text.to_string());
        }
        if set.is_empty() {
            map.remove(&buffer);
        }
    }

    /// Drop the pending next-request choices, returning the keys they held.
    pub fn clear_next_request(&self, buffer: BufferId) -> Vec<SettingKey> {
        self.next_request
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&buffer)
            .map(|s| s.into_keys().collect())
            .unwrap_or_default()
    }

    /// Decide what `typed` asks for. `typed` is ONLY what the user typed: an
    /// operator's selected source and anything pasted are not passed in, so a
    /// file that contains `@coding` cannot select a profile.
    pub fn plan_send(&self, buffer: BufferId, typed: &str) -> SendPlan {
        match super::prompt_cookie::find(typed, &self.profile_names()) {
            None => SendPlan::Plain,
            Some(p) if self.profile_widens_tools(buffer, &p) => SendPlan::NeedsConfirm(p),
            Some(p) => SendPlan::Profile(p),
        }
    }

    /// True when `profile`, applied from pasted text on top of what `buffer`
    /// has now, would ask for tools the session does not already have. Pure
    /// resolution, no file read, so it is safe on the editor thread; the project
    /// layer comes after the profile in the fold and can only narrow, so leaving
    /// it out cannot hide a widening.
    pub fn profile_widens_tools(&self, buffer: BufferId, profile: &str) -> bool {
        let layers = self.layers_for(buffer, self.session_overrides(buffer));
        let req = RequestOverrides {
            profile: Some(profile.to_string()),
            pasted_profile: true,
            ..RequestOverrides::default()
        };
        resolve(&layers, buffer, &req, Trust::User).is_ok_and(|r| {
            r.refused
                .iter()
                .any(|f| f.source == "a prompt" && f.key == "tools")
        })
    }

    /// Every session's store, shown or not: where the permission popup looks
    /// for a tool's question (LL.18).
    pub fn stores(&self) -> Vec<ConversationStore> {
        let reg = self.reg();
        reg.by_buffer
            .values()
            .map(|(_, s)| s.store.clone())
            .chain(reg.unbound.iter().map(|s| s.store.clone()))
            .collect()
    }

    fn reg(&self) -> MutexGuard<'_, Registry> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Create a chat from the current config. Cheap and non-blocking, so the
    /// ex-command can call it on the editor thread. A configuration problem
    /// does not fail the creation: the buffer opens with the problem shown.
    pub fn create(&self) -> Arc<ChatSession> {
        self.create_with(None, None)
    }

    /// [`create`](Self::create) with a starting profile and a source buffer.
    ///
    /// The new chat's session settings are **copied** from `from`'s session,
    /// then the chat stands alone (design §4). Naming a profile that differs
    /// from the source's starts the chat fresh from that profile: the source's
    /// overrides were made on top of another base and do not carry over.
    pub fn create_with(&self, profile: Option<&str>, from: Option<BufferId>) -> Arc<ChatSession> {
        self.create_named(profile, from, None)
    }

    /// LL.10b: the dedicated buffer of an ask from `from`: a chat whose name
    /// says what it is about (`*llm:foo.rs:4*`, `hint` being `foo.rs`), with
    /// `from`'s session settings copied into it as [`create_with`](Self::create_with)
    /// copies them. The counter keeps every request its own buffer.
    pub fn create_for_source(&self, from: BufferId, hint: &str) -> Arc<ChatSession> {
        let session = self.create_named(None, Some(from), Some(hint));
        *self.last_ask.lock().unwrap_or_else(|e| e.into_inner()) = Some(session.name.clone());
        session
    }

    /// Set what a shown chat's finished turn tells. Once, at install.
    pub fn set_done_hook(&self, hook: DoneHook) {
        let _ = self.done_hook.set(hook);
    }

    /// The host's buffers, which context is read from. Once, at install.
    pub fn set_store(&self, store: lattice_mode::BufferStoreHandle) {
        let _ = self.store.set(store);
    }

    /// The multibuffer views, through which a composed view's files are
    /// found. Once, at install.
    pub fn set_composed(&self, views: lattice_multibuffer::MultibufferRegistryHandle) {
        let _ = self.composed.set(views);
    }

    /// Why `buffer`'s text may not be sent when it is a composed view (search
    /// results, a multibuffer): it shows a file `llm.exclude` names, or it is
    /// not a view whose files can be told. `None` for any other buffer.
    pub fn composed_refusal(&self, buffer: BufferId) -> Option<String> {
        let store = self.store.get()?;
        let handle = store.handle_for(buffer)?;
        handle.display_line_numbers()?;
        let Some(view) = self.composed.get().and_then(|v| v.handle(buffer)) else {
            return Some(
                "this view shows several files and which is not known; use the file itself"
                    .to_string(),
            );
        };
        view.source_buffer_ids().into_iter().find_map(|source| {
            let path = view.source_path(source)?;
            let glob = self.excluded(&path)?;
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            Some(format!(
                "this view shows {name}, which matches llm.exclude (`{glob}`)"
            ))
        })
    }

    /// What requests from `buffer` carry as context (LL.15): a chat's own, or
    /// what was attached in the buffer.
    pub fn context_of(&self, buffer: BufferId) -> Vec<Attached> {
        match self.for_buffer(buffer) {
            Some(chat) => chat
                .context
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            None => self
                .source_context
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&buffer)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Run `f` on `buffer`'s context list, then tell the views.
    fn with_context<R>(&self, buffer: BufferId, f: impl FnOnce(&mut Vec<Attached>) -> R) -> R {
        let out = match self.for_buffer(buffer) {
            Some(chat) => f(&mut chat.context.lock().unwrap_or_else(|e| e.into_inner())),
            None => f(self
                .source_context
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .entry(buffer)
                .or_default()),
        };
        self.context_watch.bump();
        out
    }

    /// Attach `item` to `buffer`'s context. `false` when it is already there.
    pub fn attach(&self, buffer: BufferId, item: Attached) -> bool {
        self.with_context(buffer, |list| {
            if list.iter().any(|a| a.item == item.item) {
                return false;
            }
            list.push(item);
            true
        })
    }

    /// Remove the `index`th item of `buffer`'s context. A span's tracked
    /// range is released once no list holds it: a chat made from the buffer
    /// shares it, and its copy must keep following edits.
    pub fn detach(&self, buffer: BufferId, index: usize) -> Option<Attached> {
        let removed = self.with_context(buffer, |list| {
            (index < list.len()).then(|| list.remove(index))
        })?;
        if let ContextItem::Span { buffer: doc, range } = removed.item {
            let held = {
                let reg = self.reg();
                let sessions = reg
                    .by_buffer
                    .values()
                    .map(|(_, s)| s)
                    .chain(reg.unbound.iter());
                let in_sessions = sessions.into_iter().any(|s| {
                    s.context
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .iter()
                        .any(|a| a.item == removed.item)
                });
                in_sessions
                    || self
                        .source_context
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .values()
                        .flatten()
                        .any(|a| a.item == removed.item)
            };
            if !held && let Some(handle) = self.store.get().and_then(|s| s.handle_for(doc)) {
                drop(handle.update_tracked_ranges(vec![
                    lattice_core::tracked::TrackedRangeChange::Untrack(range),
                ]));
            }
        }
        Some(removed)
    }

    /// Show `buffer`'s context in `*llm-context*`.
    pub fn set_listing_owner(&self, buffer: BufferId) {
        *self.listing_owner.lock().unwrap_or_else(|e| e.into_inner()) = Some(buffer);
        self.context_watch.bump();
    }

    /// Whose context `*llm-context*` shows.
    pub fn listing_owner(&self) -> Option<BufferId> {
        *self.listing_owner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Bumped on every context change, for the list view.
    pub fn context_watch(&self) -> &WatchHandle {
        &self.context_watch
    }

    /// What `*llm-inspect*` shows now.
    pub fn inspect_text(&self) -> String {
        self.inspect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Bumped when the inspect text changes.
    pub fn inspect_watch(&self) -> &WatchHandle {
        &self.inspect_watch
    }

    fn set_inspect(&self, text: String) {
        *self.inspect.lock().unwrap_or_else(|e| e.into_inner()) = text;
        self.inspect_watch.bump();
    }

    /// The last request sent from `buffer`, as `:llm-inspect` shows it.
    pub fn last_sent(&self, buffer: BufferId) -> Option<Arc<String>> {
        self.last_sent
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&buffer)
            .cloned()
    }

    /// `:llm-inspect` in a chat: show what sending `prompt` now would send,
    /// built exactly as the request will be (settings, project file, system
    /// prompt, context read now), with the key's place redacted. Shows the
    /// failure instead when the request could not be built.
    pub fn inspect_next(self: &Arc<Self>, buffer: BufferId, prompt: Option<String>) {
        let Some(session) = self.for_buffer(buffer) else {
            return;
        };
        self.set_inspect("(building the request…)\n".to_string());
        let layers = self.layers_for(buffer, self.overrides_of(Some(&session), buffer));
        let mut history = history::messages(&session.store.snapshot());
        if let Some(p) = prompt {
            history.push(Message::User(p));
        }
        let mut request = RequestOverrides {
            values: self.next_request(buffer),
            ..RequestOverrides::default()
        };
        request.profile = request.values.remove(&SettingKey::Profile);
        let chats = self.clone();
        self.runtime.spawn(async move {
            let shown = match chats
                .prepare(&session, buffer, layers, history, request)
                .await
            {
                Ok(p) => match p
                    .provider
                    .build()
                    .build(&p.req, &ApiKey::new(super::context::REDACTED))
                {
                    Ok(http) => super::context::show(&http),
                    Err(e) => format!("llm: the request could not be built: {e}\n"),
                },
                Err(e) => format!("{e}\n"),
            };
            chats.set_inspect(shown);
        });
    }

    /// `:llm-inspect` anywhere else: the last request sent from `buffer`.
    pub fn inspect_last(&self, buffer: BufferId) -> bool {
        match self.last_sent(buffer) {
            Some(shown) => {
                self.set_inspect((*shown).clone());
                true
            }
            None => false,
        }
    }

    /// The latest ask's session name, while the session is still kept: an
    /// unshown one is dropped once [`UNBOUND_CAP`] newer ones are waiting.
    pub fn last_ask(&self) -> Option<String> {
        let name = self
            .last_ask
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let reg = self.reg();
        let kept = reg.unbound.iter().any(|s| s.name == name)
            || reg.by_buffer.values().any(|(_, s)| s.name == name);
        kept.then_some(name)
    }

    fn create_named(
        &self,
        profile: Option<&str>,
        from: Option<BufferId>,
        hint: Option<&str>,
    ) -> Arc<ChatSession> {
        let mut seed = from.map(|b| self.session_overrides(b)).unwrap_or_default();
        if let Some(p) = profile {
            if seed.get(&SettingKey::Profile).map(String::as_str) != Some(p) {
                seed.clear();
            }
            seed.insert(SettingKey::Profile, p.to_string());
        }
        self.create_seeded(seed, from, hint)
    }

    /// A new chat whose session settings start as `seed`.
    fn create_seeded(
        &self,
        seed: RawSet,
        from: Option<BufferId>,
        hint: Option<&str>,
    ) -> Arc<ChatSession> {
        let n = self.next_chat.fetch_add(1, Ordering::Relaxed);
        let name = match hint.map(name_hint).filter(|h| !h.is_empty()) {
            Some(h) => format!("*llm:{h}:{n}*"),
            None => format!("*llm:{n}*"),
        };
        let key = SessionKey::new(SESSION_PROVIDER, n);
        let bus = self.bus.clone();
        let store = ConversationStore::new(Arc::new(move |ev| bus.publish_typed(ev)));
        self.refresh_project();
        let provisional = self.provisional(BufferId(0), seed.clone());
        store.mutate(&key, |c| {
            c.assistant_label = Some(provisional.clone().unwrap_or_else(|_| "model".to_string()));
            if let Err(e) = &provisional {
                c.error = Some(e.clone());
            }
        });
        let session = Arc::new(ChatSession {
            name,
            key,
            store,
            seed: Mutex::new(seed),
            label: Mutex::new(provisional.ok()),
            turn: Mutex::new(TurnState::default()),
            api_key: Mutex::new(None),
            warned: Mutex::new(HashSet::new()),
            closed: AtomicBool::new(false),
            turn_waiters: Mutex::new(Vec::new()),
            source: from,
            destination: Mutex::new(None),
            context: Mutex::new(from.map(|b| self.context_of(b)).unwrap_or_default()),
            origin: ORIGIN_BASE + NEXT_ORIGIN.fetch_add(1, Ordering::Relaxed),
            trusted: Arc::new(AtomicBool::new(false)),
            allowed_tools: Arc::default(),
            prompt_seed: Mutex::new(None),
        });
        {
            let mut reg = self.reg();
            reg.unbound.push(session.clone());
            trim_unbound(&mut reg.unbound);
        }
        session
    }

    /// Attach the chat named `name` to `buffer`. Returns the session and the
    /// binding generation [`release`](Self::release) must present. A chat whose
    /// earlier guard was released (the mode was reactivated) is reopened, with
    /// its history intact.
    pub fn bind(&self, name: &str, buffer: BufferId) -> Option<(Arc<ChatSession>, u64)> {
        let generation = self.next_bind.fetch_add(1, Ordering::Relaxed);
        let mut reg = self.reg();
        let session = match reg.unbound.iter().position(|s| s.name == name) {
            Some(i) => reg.unbound.remove(i),
            None => reg
                .by_buffer
                .values()
                .find(|(_, s)| s.name == name)
                .map(|(_, s)| s.clone())?,
        };
        session.closed.store(false, Ordering::Release);
        reg.by_buffer.insert(buffer, (generation, session.clone()));
        Some((session, generation))
    }

    /// How many chats no buffer is bound to.
    #[doc(hidden)]
    pub fn unbound_count(&self) -> usize {
        self.reg().unbound.len()
    }

    fn buffer_of(&self, session: &Arc<ChatSession>) -> Option<BufferId> {
        self.reg()
            .by_buffer
            .iter()
            .find(|(_, (_, s))| Arc::ptr_eq(s, session))
            .map(|(b, _)| *b)
    }

    pub fn for_buffer(&self, buffer: BufferId) -> Option<Arc<ChatSession>> {
        self.reg().by_buffer.get(&buffer).map(|(_, s)| s.clone())
    }

    /// The buffer's guard dropped: unbind the chat and cancel what it has in flight. A
    /// stale generation (the buffer was re-bound since) does nothing.
    pub fn release(&self, buffer: BufferId, generation: u64) {
        let session = {
            let mut reg = self.reg();
            match reg.by_buffer.get(&buffer) {
                Some((g, _)) if *g == generation => {}
                _ => return,
            }
            let Some((_, session)) = reg.by_buffer.remove(&buffer) else {
                return;
            };
            reg.unbound.push(session.clone());
            trim_unbound(&mut reg.unbound);
            session
        };
        session.closed.store(true, Ordering::Release);
        // Trust was given to a chat on screen; closed, it lapses.
        session.trusted.store(false, Ordering::Release);
        session
            .allowed_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Stopping the request drops any edit waiting on a review, and that
        // closes the session's diffs.
        self.interrupt(&session);
    }

    /// Submit a prompt: send it now, or queue it behind the running turn.
    ///
    /// Idle with prompts still queued (a failure held them) the new prompt joins
    /// the back of the queue and the front is sent, so the queue resumes in order.
    pub fn submit(self: &Arc<Self>, session: &Arc<ChatSession>, text: String) {
        self.submit_with(session, text, RequestOverrides::default());
    }

    /// [`submit`](Self::submit) with the request-layer overrides this prompt
    /// carries (a command argument, an `@profile`). They apply to this prompt's
    /// request only. Whatever `:llm-menu` set for the next request is taken here
    /// and sits under `overrides`, so an explicit argument wins over a menu choice.
    pub fn submit_with(
        self: &Arc<Self>,
        session: &Arc<ChatSession>,
        text: String,
        overrides: RequestOverrides,
    ) {
        self.submit_text(session, text, overrides, true);
    }

    /// [`submit_with`](Self::submit_with); `from_prompt` is false for text
    /// sent into the chat from elsewhere, which must leave its prompt alone.
    fn submit_text(
        self: &Arc<Self>,
        session: &Arc<ChatSession>,
        text: String,
        mut overrides: RequestOverrides,
        from_prompt: bool,
    ) {
        let buffer = self.buffer_of(session).unwrap_or(BufferId(0));
        let mut values = self
            .next_request
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&buffer)
            .unwrap_or_default();
        values.extend(std::mem::take(&mut overrides.values));
        // A profile is not a value in the fold: the request names it separately.
        if let Some(p) = values.remove(&SettingKey::Profile)
            && overrides.profile.is_none()
        {
            overrides.profile = Some(p);
        }
        overrides.values = values;
        let send_now = {
            let mut turn = session.turn();
            let busy = turn.busy;
            let mut started = false;
            turn.pending.push_back(overrides);
            session.store.mutate(&session.key, |c| {
                c.queued.push(text);
                if !from_prompt {
                    c.external_submissions += 1;
                }
                if !busy {
                    let next = c.queued.remove(0);
                    c.error = None;
                    c.push_user_text(&next);
                    c.status = SessionStatus::Thinking;
                    started = true;
                }
            });
            if started {
                turn.busy = true;
                turn.interrupted = false;
                turn.current = turn.pending.pop_front().unwrap_or_default();
            }
            started
        };
        if send_now {
            self.start_request(session);
        }
    }

    /// LL.10b: send `source` from a buffer, with the typed `instruction`, as
    /// the first turn of a chat [`create_for_source`](Self::create_for_source)
    /// made. A chat that already has a turn running, or prompts a failure
    /// held, queues it as text behind them instead, so it never jumps the
    /// queue; only a caller reusing a chat (LL.10c's `destination=`) can
    /// reach that.
    pub fn submit_source(
        self: &Arc<Self>,
        session: &Arc<ChatSession>,
        source: crate::conversation::Block,
        instruction: String,
        mut overrides: RequestOverrides,
    ) {
        if let Some(p) = overrides.values.remove(&SettingKey::Profile)
            && overrides.profile.is_none()
        {
            overrides.profile = Some(p);
        }
        let started = {
            let mut turn = session.turn();
            // Read under the turn lock, which every change to the queue holds.
            let held = !session.store.snapshot().queued.is_empty();
            if turn.busy || held {
                false
            } else {
                session.store.mutate(&session.key, |c| {
                    c.error = None;
                    c.push_user_source(source.clone(), &instruction);
                    // A chat reused as a destination may have a prompt the
                    // user is typing; this turn did not come from it.
                    c.external_submissions += 1;
                    c.status = SessionStatus::Thinking;
                });
                turn.busy = true;
                turn.interrupted = false;
                turn.current = overrides.clone();
                true
            }
        };
        if started {
            self.start_request(session);
        } else {
            let text = match &source {
                crate::conversation::Block::Source { text, .. } if instruction.is_empty() => {
                    text.clone()
                }
                crate::conversation::Block::Source { text, .. } => {
                    format!("{text}\n\n{instruction}")
                }
                _ => instruction,
            };
            self.submit_text(session, text, overrides, false);
        }
    }

    /// Stop the running turn. A queued prompt, if any, is sent next.
    /// Stop request `id` as `<C-c>` would: through its session, so whatever
    /// waits on the turn hears "interrupted" (an inline answer marks itself,
    /// a quiet one says so). A request no session owns is cancelled outright.
    pub fn stop_request(&self, id: RequestId) {
        let session = {
            let reg = self.reg();
            reg.by_request.get(&id).cloned().or_else(|| {
                // Started but not yet registered: find it by its turn.
                reg.by_buffer
                    .values()
                    .map(|(_, s)| s)
                    .chain(reg.unbound.iter())
                    .find(|s| s.turn().request == Some(id))
                    .cloned()
            })
        };
        match session {
            Some(s) => self.interrupt(&s),
            None => self.requests.cancel(id),
        }
    }

    /// How many sessions have a turn in flight: from the submit, before the
    /// request exists (its key and settings resolve first, off the editor
    /// thread), until the turn ends. What quitting would lose (LL.14d); the
    /// registry alone misses a request still being prepared.
    pub fn in_flight(&self) -> usize {
        let reg = self.reg();
        reg.by_buffer
            .values()
            .map(|(_, s)| s)
            .chain(reg.unbound.iter())
            .filter(|s| s.is_busy())
            .count()
    }

    /// The request registry, for listings.
    /// The multi-thread runtime the layer's background work runs on (see
    /// [`super::sink::LlmInline::runtime`] for why not `try_current`).
    pub fn runtime(&self) -> &Handle {
        &self.runtime
    }

    pub fn requests(&self) -> &RequestRegistryHandle {
        &self.requests
    }

    /// LL.19: a new chat holding `buffer`'s history before the question at
    /// `turn`, with that question in its prompt, ready to edit and send. It
    /// starts from `buffer`'s settings and context, like any chat made from a
    /// buffer; trust and "allow for this chat" are not copied. The original
    /// chat is not touched.
    pub fn fork(&self, buffer: BufferId, turn: usize) -> Result<Arc<ChatSession>, String> {
        let source = self
            .for_buffer(buffer)
            .ok_or_else(|| "llm: not a chat buffer".to_string())?;
        let (history, prompt) = source
            .store
            .snapshot()
            .fork_at(turn)
            .ok_or_else(|| "llm: no question here to fork from".to_string())?;
        let fork = self.create_with(None, Some(buffer));
        let label = fork.store.snapshot().assistant_label;
        fork.store.mutate(&fork.key, |c| {
            *c = Conversation {
                assistant_label: label.or(history.assistant_label.clone()),
                error: c.error.take(),
                ..history
            };
        });
        *fork.prompt_seed.lock().unwrap_or_else(|e| e.into_inner()) = Some(prompt);
        Ok(fork)
    }

    /// LL.20: what `:llm-save` keeps of the chat in `buffer`: its conversation
    /// and its effective settings, the ones a session file may carry. Refused
    /// while a reply is coming (a half-received turn and a running tool are
    /// not history yet) and for an empty chat.
    pub fn saved_settings(&self, buffer: BufferId) -> Result<(Conversation, RawSet), String> {
        let session = self
            .for_buffer(buffer)
            .ok_or_else(|| "llm: not a chat buffer".to_string())?;
        if session.is_busy() {
            return Err(
                "llm: a reply is still coming; save once it is done, or stop it (<C-c>)".into(),
            );
        }
        let conv = session.store.snapshot();
        if conv.turns.is_empty() {
            return Err("llm: nothing to save yet".into());
        }
        let resolved = self.resolve_for(buffer).map_err(|e| format!("llm: {e}"))?;
        let settings = super::settings::SAVED_KEYS
            .into_iter()
            .filter_map(|k| resolved.values.text(k).map(|t| (k, t)))
            .collect();
        Ok((conv, settings))
    }

    /// LL.20: a new chat from a session file. Its settings go through the
    /// trust table as project config does, so a file from anywhere can choose
    /// a model but cannot widen tools beyond what the user's own config
    /// enables, redirect requests or name a key; what it tried is returned.
    pub fn resume(
        &self,
        saved: super::session_file::Saved,
        source: &str,
    ) -> (Arc<ChatSession>, Vec<super::settings::Refusal>) {
        let base_tools = resolve(
            &self.layers_for(BufferId(0), RawSet::new()),
            BufferId(0),
            &RequestOverrides::default(),
            Trust::User,
        )
        .map(|r| r.values.tools)
        .unwrap_or_default();
        let (seed, refused) =
            super::settings::restrict(saved.settings.into_iter(), source, base_tools);
        let session = self.create_seeded(seed, None, None);
        let label = session.store.snapshot().assistant_label;
        session.store.mutate(&session.key, |c| {
            *c = Conversation {
                assistant_label: label.or(saved.conversation.assistant_label.clone()),
                error: c.error.take(),
                ..saved.conversation
            };
        });
        (session, refused)
    }

    /// LL.19: send the last question again, with the settings as they are
    /// now, in place of its reply. Refused while a reply is still coming:
    /// stopping it is a separate decision.
    pub fn retry(self: &Arc<Self>, session: &Arc<ChatSession>) -> Result<(), String> {
        {
            let mut turn = session.turn();
            if turn.busy {
                return Err("llm: a reply is still coming; stop it (<C-c>) before retrying".into());
            }
            let mut retried = false;
            session.store.mutate(&session.key, |c| {
                if c.drop_last_reply() {
                    c.error = None;
                    c.status = SessionStatus::Thinking;
                    retried = true;
                }
            });
            if !retried {
                return Err("llm: nothing to retry yet".into());
            }
            turn.busy = true;
            turn.interrupted = false;
            turn.current = RequestOverrides::default();
        }
        self.start_request(session);
        Ok(())
    }

    pub fn interrupt(&self, session: &Arc<ChatSession>) {
        let request = {
            let mut turn = session.turn();
            turn.interrupted = true;
            turn.request
        };
        if let Some(id) = request {
            self.requests.cancel(id);
        }
    }

    /// Start the request for the conversation as it now stands. The caller has
    /// already folded the user turn in and marked the session busy.
    ///
    /// The layers are snapshotted here, once: a setting changed after this point
    /// applies to the next request, never to this one in flight. Everything
    /// that can block (the project file, a system-prompt file, the key command)
    /// happens on the runtime, never on the editor thread.
    fn start_request(self: &Arc<Self>, session: &Arc<ChatSession>) {
        let buffer = self.buffer_of(session).unwrap_or(BufferId(0));
        let layers = self.layers_for(buffer, self.overrides_of(Some(session), buffer));
        let history = history::messages(&session.store.snapshot());
        let request = session.turn().current.clone();
        let chats = self.clone();
        let session = session.clone();
        self.runtime.spawn(async move {
            let prepared = match chats
                .prepare(&session, buffer, layers, history, request)
                .await
            {
                Ok(p) => p,
                Err(e) => {
                    chats.end_turn(&session, Some(e));
                    return;
                }
            };
            session.set_label(Some(prepared.req.model.clone()));
            session.store.mutate(&session.key, |c| {
                c.assistant_label = Some(prepared.req.model.clone());
            });
            let provider_cfg = prepared.provider;
            let cached = session
                .api_key
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .filter(|(name, _)| *name == provider_cfg.name)
                .map(|(_, k)| k);
            let key = match cached {
                Some(k) => k,
                None => match resolve_key(&provider_cfg).await {
                    Ok(k) => {
                        *session.api_key.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some((provider_cfg.name.clone(), k.clone()));
                        k
                    }
                    Err(e) => {
                        chats.end_turn(&session, Some(e));
                        return;
                    }
                },
            };
            if session.turn().interrupted || session.closed.load(Ordering::Acquire) {
                chats.end_turn(&session, None);
                return;
            }
            let req = prepared.req;
            let offer = prepared.offer;
            let mut sink = ChatSink::new(session.store.clone(), session.key.clone(), &provider_cfg);
            sink.tap = session.turn().tap.clone();
            sink.offer = Some(offer.clone());
            // A shown chat's requests belong to its buffer; a hidden ask's to
            // the buffer it asked from, where `:llm-stop` finds them.
            let target = RequestTarget {
                buffer: chats
                    .buffer_of(&session)
                    .or(session.source)
                    .unwrap_or(BufferId(0)),
                label: session.destination(),
            };
            // What `:llm-inspect` shows for this buffer from now on: the same
            // request, built with the key's place redacted (LL.15).
            if let Ok(http) = provider_cfg
                .build()
                .build(&req, &ApiKey::new(super::context::REDACTED))
            {
                chats
                    .last_sent
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(target.buffer, Arc::new(super::context::show(&http)));
            }
            let id = if offer.is_empty() {
                chats.requests.start_with_key(
                    target,
                    req,
                    provider_cfg.build(),
                    key,
                    Box::new(sink),
                )
            } else {
                let rounds = super::tool_loop::ChatRounds::new(
                    session.store.clone(),
                    session.key.clone(),
                    offer,
                    req.clone(),
                    chats.tool_rounds(),
                )
                .for_session(
                    session.origin,
                    session.trusted.clone(),
                    session.allowed_tools.clone(),
                );
                chats.requests.start_loop(
                    target,
                    req,
                    provider_cfg.build(),
                    key,
                    Box::new(sink),
                    Box::new(rounds),
                )
            };
            let interrupted = {
                let mut turn = session.turn();
                turn.request = Some(id);
                turn.interrupted
            };
            // An interrupt that landed between the check above and the id being
            // recorded found nothing to cancel; honour it now.
            if interrupted {
                chats.requests.cancel(id);
            }
            chats.register_request(id, &session);
        });
    }

    /// Build what a request from `session` sends, all but the key: settings
    /// resolved over a freshly read project file, the model chosen, the system
    /// prompt read, unsupported parameters dropped, the message scope applied
    /// and the context read now and put at the head of the last user message.
    /// `start_request` sends it; `:llm-inspect` shows it, so what is shown is
    /// what would be sent.
    async fn prepare(
        self: &Arc<Self>,
        session: &Arc<ChatSession>,
        buffer: BufferId,
        mut layers: Layers,
        history: Vec<Message>,
        request: RequestOverrides,
    ) -> Result<Prepared, String> {
        layers.project = self.read_project(session).await;
        let resolved =
            resolve(&layers, buffer, &request, Trust::User).map_err(|e| e.to_string())?;
        for refusal in &resolved.refused {
            if session.first_report(format!("{}\u{0}{}", refusal.source, refusal.key)) {
                tracing::warn!(target: "lattice::llm", "{refusal}");
            }
        }
        let chosen = self
            .config
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .choose(
                resolved.values.model.as_deref(),
                self.default_provider().as_deref(),
            )?;
        if resolved.origins.get(&SettingKey::Model) == Some(&lattice_config::OptionOrigin::Project)
            && let Some((other, usual)) = self.other_provider(resolved.values.model.as_deref())
            && session.first_report(format!("provider\u{0}{other}"))
        {
            tracing::warn!(
                target: "lattice::llm",
                "llm: .lattice/llm.toml sends this chat to provider `{other}`, not your default `{usual}`"
            );
        }
        let system = match &resolved.values.system {
            Some(text) if system_is_path(text) => Some(read_system_file(text).await?),
            other => other.clone(),
        };
        let mut params = Params {
            max_tokens: resolved.values.max_tokens,
            temperature: resolved.values.temperature,
        };
        for omission in omitted_params(&chosen.provider, &chosen.model, &resolved.values) {
            omission.clear(&mut params);
            if session.first_report(format!("omit\u{0}{}", omission.param)) {
                tracing::info!(
                    target: "lattice::llm",
                    "llm: {} is not sent to {} ({}): {}",
                    omission.param,
                    chosen.provider.name,
                    chosen.model,
                    omission.reason
                );
            }
        }
        let mut messages = resolved
            .values
            .messages
            .apply(&history, |m| matches!(m, Message::User(_)));
        // A provider can verify only its own reasoning, so another's is left
        // out of the request (the adapter drops it); say so once per chat.
        for from in foreign_reasoning(&messages, chosen.provider.kind.reasoning_tag()) {
            if session.first_report(format!("reasoning\u{0}{from}\u{0}{}", chosen.provider.name)) {
                tracing::info!(
                    target: "lattice::llm",
                    "llm: reasoning written by {from} is not sent to {} ({}): only the \
                     provider that wrote it can check it",
                    chosen.provider.name,
                    chosen.model
                );
            }
        }
        let items = session
            .context
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(block) = super::context::read(
            &items,
            self.store.get(),
            |p| self.excluded(p),
            |b| self.composed_refusal(b),
        )
        .await?
        {
            super::context::prepend(&mut messages, &block);
        }
        let offer = Arc::new(self.tools.offer(&resolved.values.tools).await);
        Ok(Prepared {
            req: LlmRequest {
                model: chosen.model,
                system,
                messages,
                tools: offer.descriptors(),
                params,
            },
            provider: chosen.provider,
            offer,
        })
    }

    /// Read `.lattice/llm.toml` for a request, report what could not be read once,
    /// and keep the result for the menu. Absent is no project layer; unreadable
    /// or malformed is treated as absent, since a broken file in a repository
    /// must not stop the chat.
    async fn read_project(&self, session: &ChatSession) -> Option<ProjectLayer> {
        let root = self.configured_root();
        let ProjectLoad {
            layer,
            rules,
            problem,
        } = load_project(root).await;
        if let Some((key, text)) = problem
            && session.first_report(key)
        {
            tracing::warn!(target: "lattice::llm", "{text}");
        }
        *self
            .project_cache
            .write()
            .unwrap_or_else(|e| e.into_inner()) = layer.clone();
        *self
            .project_rules
            .write()
            .unwrap_or_else(|e| e.into_inner()) = rules;
        layer
    }

    fn configured_root(&self) -> Option<std::path::PathBuf> {
        self.project_root
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Refresh the cached project layer and its safety-list additions on the
    /// runtime, without waiting. A problem is logged at debug: the request
    /// that follows reports it once.
    pub fn refresh_project(&self) {
        let reload = reload_project(
            self.configured_root(),
            self.project_cache.clone(),
            self.project_rules.clone(),
        );
        self.runtime.spawn(reload);
    }

    /// [`Self::refresh_project`], awaited: for a caller that must see the
    /// project's lists before it goes on.
    pub async fn reload_project(&self) {
        reload_project(
            self.configured_root(),
            self.project_cache.clone(),
            self.project_rules.clone(),
        )
        .await;
    }

    fn register_request(self: &Arc<Self>, id: RequestId, session: &Arc<ChatSession>) {
        let already_ended = {
            let mut reg = self.reg();
            match reg.ended_early.remove(&id) {
                Some(state) => {
                    reg.ended_order.retain(|o| *o != id);
                    Some(state)
                }
                None => {
                    reg.by_request.insert(id, session.clone());
                    None
                }
            }
        };
        if let Some(state) = already_ended {
            self.request_ended(session, state);
        }
    }

    /// The bus listener's entry: a request changed state.
    pub fn on_request_state(self: &Arc<Self>, id: RequestId, state: RequestState) {
        if !state.is_terminal() {
            return;
        }
        let session = {
            let mut reg = self.reg();
            match reg.by_request.remove(&id) {
                Some(s) => Some(s),
                None => {
                    // Evict the oldest only: clearing would drop a state whose
                    // request is about to register, leaving its turn busy forever.
                    while reg.ended_early.len() >= ENDED_EARLY_CAP {
                        let Some(old) = reg.ended_order.pop_front() else {
                            break;
                        };
                        reg.ended_early.remove(&old);
                    }
                    reg.ended_early.insert(id, state.clone());
                    reg.ended_order.push_back(id);
                    None
                }
            }
        };
        if let Some(session) = session {
            self.request_ended(&session, state);
        }
    }

    fn request_ended(self: &Arc<Self>, session: &Arc<ChatSession>, state: RequestState) {
        let error = match state {
            RequestState::Failed(msg) => Some(msg),
            _ => None,
        };
        self.end_turn(session, error);
    }

    /// The turn is over: show the failure, if any, and send the next queued
    /// prompt unless the turn failed. Dequeuing and folding the prompt in as a
    /// user turn is ONE mutation, so the drain never sees the submission count
    /// dip and recover (which it would read as a fresh submission and clear the
    /// prompt). The queue decision and `busy` are one critical section under
    /// the turn lock, shared with `submit`.
    fn end_turn(self: &Arc<Self>, session: &Arc<ChatSession>, error: Option<String>) {
        let closed = session.closed.load(Ordering::Acquire);
        let failed = error.is_some();
        let interrupted = session.turn().interrupted;
        // A reply the provider ended early (a safety stop, a refusal, the
        // token limit) finishes as `Done` with the reason in the transcript's
        // error; whoever waits on the turn (a rewrite, an answer for the echo
        // area or a register) gets that reason, not the partial text.
        let mut ended_early = None;
        let started_next = {
            let mut turn = session.turn();
            let mut started = false;
            session.store.mutate(&session.key, |c| {
                c.status = SessionStatus::Idle;
                c.settle_tool_calls(super::tool_loop::STOPPED);
                if failed {
                    c.error = error.clone();
                } else {
                    ended_early = c.error.clone();
                }
                if !failed && !closed && !c.queued.is_empty() {
                    let next = c.queued.remove(0);
                    c.error = None;
                    c.push_user_text(&next);
                    c.status = SessionStatus::Thinking;
                    started = true;
                }
            });
            turn.request = None;
            turn.interrupted = false;
            turn.busy = started;
            turn.current = if started {
                turn.pending.pop_front().unwrap_or_default()
            } else {
                RequestOverrides::default()
            };
            started
        };
        let told = match (&error, ended_early) {
            (Some(e), _) => Err(e.clone()),
            (None, _) if interrupted => Err("interrupted".to_string()),
            (None, Some(why)) => Err(why),
            (None, None) => Ok(()),
        };
        let waiters: Vec<_> = std::mem::take(
            &mut *session
                .turn_waiters
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if !waiters.is_empty() {
            let outcome = told.clone().map(|()| session.last_reply());
            for tx in waiters {
                let _ = tx.send(outcome.clone());
            }
        }
        // A shown chat's turn is over (nothing queued behind it, and the
        // user did not stop it): say so, if the hook decides it should be
        // said. A hidden ask's session has no buffer; its sink reports.
        if !started_next
            && !closed
            && !interrupted
            && let (Some(hook), Some(buffer)) = (self.done_hook.get(), self.buffer_of(session))
        {
            hook(
                buffer,
                &session.name,
                told.as_ref().err().map(String::as_str),
            );
        }
        if started_next {
            self.start_request(session);
        }
    }
}

/// See [`LlmChats::set_done_hook`]: the chat's buffer, its name, its failure.
pub type DoneHook = Arc<dyn Fn(BufferId, &str, Option<&str>) + Send + Sync>;

/// Read a system prompt that names a file (`~/...`, `/...`, `./...`). Only the
/// user's own config reaches here: a project's file path is refused in
/// `resolve`. The message names the path and the failure kind, nothing of the
/// contents.
pub(crate) async fn read_system_file(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let path = match raw.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => std::path::PathBuf::from(home).join(rest),
            None => return Err(format!("cannot read system prompt {raw}: HOME is not set")),
        },
        None => std::path::PathBuf::from(raw),
    };
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("cannot read system prompt {raw}: {}", e.kind()))
}

/// Folds a request's stream events into a chat's conversation model.
struct ChatSink {
    store: ConversationStore,
    key: SessionKey,
    /// USD per million tokens, both or the cost is not shown.
    pricing: Option<(f64, f64)>,
    /// The session's [`TextTap`], if it has one.
    tap: Option<TextTap>,
    /// The request's tools, to show a call by its tool's name and server.
    offer: Option<Arc<super::tools::Offer>>,
    /// Calls whose arguments are still arriving: id -> (name called, raw JSON).
    calls: HashMap<String, (String, String)>,
}

impl ChatSink {
    fn new(store: ConversationStore, key: SessionKey, provider: &ProviderConfig) -> Self {
        let pricing = provider
            .input_cost_per_mtok
            .zip(provider.output_cost_per_mtok);
        Self {
            store,
            key,
            pricing,
            tap: None,
            offer: None,
            calls: HashMap::new(),
        }
    }

    /// A call the model started: shown at once, pending, under its tool's
    /// name (`read_file`), with its server as the label unless it is
    /// lattice's own.
    fn call_started(&mut self, c: &mut Conversation, id: String, name: String) {
        let tool = self.offer.as_ref().and_then(|o| o.find(&name));
        let (origin, title, kind) = match tool {
            Some(t) => (
                (t.server != super::tools::broker::LATTICE).then(|| t.server.clone()),
                t.name.clone(),
                t.kind,
            ),
            None => (None, name.clone(), ToolKind::Other),
        };
        c.push_tool_call(
            id.clone(),
            origin,
            title,
            ToolStatus::Pending,
            kind,
            None,
            None,
        );
        self.calls.insert(id, (name, String::new()));
    }

    /// A call's arguments are complete: kept exactly for the next request,
    /// and shown. Arguments that are not a JSON object are kept as `{}` with
    /// the reason, which the call's result reports to the model.
    fn call_ended(&mut self, c: &mut Conversation, id: String) {
        let Some((name, raw)) = self.calls.remove(&id) else {
            return;
        };
        let parsed = if raw.trim().is_empty() {
            Ok(serde_json::json!({}))
        } else {
            serde_json::from_str::<serde_json::Value>(&raw)
                .map_err(|e| format!("the arguments are not valid JSON ({e})"))
                .and_then(|v| {
                    if v.is_object() {
                        Ok(v)
                    } else {
                        Err("the arguments must be a JSON object".to_string())
                    }
                })
        };
        let (input, malformed, shown) = match parsed {
            Ok(v) => {
                let shown = serde_json::to_string_pretty(&v).unwrap_or_else(|_| raw.clone());
                (v, None, shown)
            }
            Err(why) => (serde_json::json!({}), Some(why), raw),
        };
        c.merge_tool_update(&id, None, None, Some(shown), None);
        c.tool_calls.insert(
            id,
            ToolCallData {
                name,
                input,
                malformed,
            },
        );
    }
}

impl Sink for ChatSink {
    fn event(&mut self, ev: StreamEvent) -> SinkFlow {
        if let (StreamEvent::Text(t), Some(tap)) = (&ev, &self.tap) {
            tap(t);
        }
        let pricing = self.pricing;
        let (store, key) = (self.store.clone(), self.key.clone());
        store.mutate(&key, |c| match ev {
            StreamEvent::Text(t) => c.extend_text(Role::Assistant, &t),
            StreamEvent::Reasoning(t) => c.extend_reasoning(&t),
            StreamEvent::ReasoningData(d) => c.attach_reasoning_data(OpaqueReasoning {
                provider: d.provider,
                data: d.data,
            }),
            StreamEvent::Usage(u) => {
                let used =
                    u.input_tokens + u.output_tokens + u.cache_read_tokens + u.cache_write_tokens;
                let cost = pricing.map(|(input, output)| Cost {
                    amount: (u.input_tokens as f64 * input + u.output_tokens as f64 * output)
                        / 1_000_000.0,
                    currency: "USD".to_string(),
                });
                c.usage = Some(UsageSnapshot {
                    used,
                    size: 0,
                    cost,
                });
            }
            StreamEvent::Done(StopReason::MaxTokens) => {
                c.error = Some("the reply was cut off at the output token limit".to_string());
            }
            StreamEvent::Done(StopReason::Other(reason)) => {
                c.error = Some(format!("the model stopped: {reason}"));
            }
            StreamEvent::Done(_) => {}
            StreamEvent::Error(e) => c.error = Some(e.to_string()),
            StreamEvent::ToolCallStart { id, name } => self.call_started(c, id, name),
            StreamEvent::ToolCallInput { id, json_fragment } => {
                if let Some((_, raw)) = self.calls.get_mut(&id) {
                    raw.push_str(&json_fragment);
                }
            }
            StreamEvent::ToolCallEnd { id } => self.call_ended(c, id),
        });
        SinkFlow::Continue
    }
}

/// Read `.lattice/llm.toml` under `root` (or the project root of the working
/// directory) on the blocking pool. The second value is a problem worth
/// reporting: `(dedup key, message)`.
/// Read the project and store it in the two caches.
async fn reload_project(
    root: Option<std::path::PathBuf>,
    cache: Arc<RwLock<Option<ProjectLayer>>>,
    rules_cache: Arc<RwLock<Option<Arc<ProjectRules>>>>,
) {
    let ProjectLoad {
        layer,
        rules,
        problem,
    } = load_project(root).await;
    if let Some((_, text)) = problem {
        tracing::debug!(target: "lattice::llm", "{text}");
    }
    *cache.write().unwrap_or_else(|e| e.into_inner()) = layer;
    *rules_cache.write().unwrap_or_else(|e| e.into_inner()) = rules;
}

/// What [`load_project`] read: the settings layer, the project's safety-list
/// additions compiled for its root, and the first problem, keyed for
/// once-per-session reporting.
struct ProjectLoad {
    layer: Option<ProjectLayer>,
    rules: Option<Arc<ProjectRules>>,
    problem: Option<(String, String)>,
}

async fn load_project(root: Option<std::path::PathBuf>) -> ProjectLoad {
    let empty = ProjectLoad {
        layer: None,
        rules: None,
        problem: None,
    };
    let root = match root {
        Some(r) => r,
        None => match tokio::task::spawn_blocking(lattice_core::project::root_from_cwd)
            .await
            .ok()
            .flatten()
        {
            Some(r) => r,
            None => return empty,
        },
    };
    let path = root.join(".lattice").join("llm.toml");
    let (layer, mut problem) = match tokio::fs::read_to_string(&path).await {
        Ok(text) => match ProjectLayer::parse(".lattice/llm.toml", &text) {
            Ok(layer) => (Some(layer), None),
            Err(e) => (
                None,
                Some((format!("parse\u{0}{}", path.display()), format!("llm: {e}"))),
            ),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(e) => (
            None,
            Some((
                format!("read\u{0}{}", path.display()),
                format!("llm: cannot read {}: {}", path.display(), e.kind()),
            )),
        ),
    };
    // The safety lists and the ignore files compile here, on the runtime: the
    // editor thread only ever reads the compiled result.
    let lists = layer.as_ref().map(|l| l.lists.clone()).unwrap_or_default();
    let compiled = {
        let root = root.clone();
        tokio::task::spawn_blocking(move || ProjectRules::load(&root, &lists)).await
    };
    let rules = match compiled {
        Ok((rules, problems)) => {
            if problem.is_none()
                && let Some(first) = problems.first()
            {
                problem = Some((format!("rules\u{0}{first}"), first.clone()));
            }
            for p in problems.iter().skip(1) {
                tracing::warn!(target: "lattice::llm", "{p}");
            }
            rules.map(Arc::new)
        }
        Err(e) => {
            tracing::warn!(target: "lattice::llm", "llm: reading the project's lists failed: {e}");
            None
        }
    };
    ProjectLoad {
        layer,
        rules,
        problem,
    }
}

/// Hold `unbound` to [`UNBOUND_CAP`] by dropping the oldest session that is
/// not busy. One with a request in flight stays past the cap: dropped, quitting
/// would no longer warn about its turn, `:llm-last` could not reopen its reply,
/// and a stop could not find it. So the list exceeds the cap only by the number
/// of requests running, which is bounded by what the user started.
fn trim_unbound(unbound: &mut Vec<Arc<ChatSession>>) {
    while unbound.len() > UNBOUND_CAP {
        let Some(idle) = unbound.iter().position(|s| !s.is_busy()) else {
            break;
        };
        unbound.remove(idle);
    }
}

/// Subscribe `chats` to request state changes. The listener runs on the
/// runtime, off the editor thread; what it does is cheap model folds and a
/// spawn, and the drain's wake (`ConversationProjected`) carries the result to
/// the screen.
pub fn spawn_turn_listener(chats: LlmChatsHandle, bus: &Arc<lattice_runtime::EventBus>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<super::RequestStateChanged>();
    bus.subscribe_typed::<super::RequestStateChanged>(tx);
    let runtime = chats.runtime.clone();
    runtime.spawn(async move {
        while let Some(ev) = rx.recv().await {
            chats.on_request_state(ev.id, ev.state);
        }
    });
}

/// Subscribe `chats` to the host's per-buffer option snapshots, so `:setlocal
/// llm.*` reaches requests that run off the editor thread.
pub fn spawn_options_listener(chats: LlmChatsHandle, bus: &Arc<lattice_runtime::EventBus>) {
    let (tx, mut rx) =
        tokio::sync::mpsc::unbounded_channel::<lattice_config::BufferOptionsResolved>();
    bus.subscribe_typed::<lattice_config::BufferOptionsResolved>(tx);
    let runtime = chats.runtime.clone();
    runtime.spawn(async move {
        while let Some(ev) = rx.recv().await {
            chats.on_buffer_options(ev.buffer, &ev.options);
        }
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::llm::RequestRegistry;

    /// LL.19: reasoning another provider wrote is named, once per provider;
    /// the provider's own, and reasoning with no data, are not.
    #[test]
    fn foreign_reasoning_names_the_providers_that_cannot_be_checked() {
        let part = |provider: Option<&str>| super::super::AssistantPart::Reasoning {
            text: "t".into(),
            data: provider.map(|p| super::super::ReasoningData {
                provider: p.into(),
                data: "sig".into(),
            }),
        };
        let messages = vec![
            Message::User("q".into()),
            Message::Assistant(vec![part(Some("anthropic")), part(None)]),
            Message::Assistant(vec![part(Some("anthropic")), part(Some("gemini"))]),
        ];
        assert_eq!(
            foreign_reasoning(&messages, None)
                .into_iter()
                .collect::<Vec<_>>(),
            ["anthropic", "gemini"]
        );
        assert_eq!(
            foreign_reasoning(&messages, Some("anthropic"))
                .into_iter()
                .collect::<Vec<_>>(),
            ["gemini"]
        );
    }

    /// Past the cap the oldest IDLE session goes; one with a request in
    /// flight is kept, so quitting still warns about it and its reply can be
    /// reopened.
    #[tokio::test]
    async fn a_busy_hidden_session_is_not_evicted() {
        let handle = Handle::current();
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
            handle.clone(),
            Some(bus.clone()),
            None,
        ));
        let chats = Arc::new(LlmChats::new(handle, requests, bus, LlmConfig::default()));
        let busy = chats.create();
        busy.turn().busy = true;
        let idle = chats.create();
        for _ in 0..UNBOUND_CAP {
            chats.create();
        }
        assert_eq!(chats.in_flight(), 1, "the busy session is still counted");
        let reg = chats.reg();
        assert!(reg.unbound.iter().any(|s| Arc::ptr_eq(s, &busy)), "kept");
        assert!(
            !reg.unbound.iter().any(|s| Arc::ptr_eq(s, &idle)),
            "the oldest idle went"
        );
        assert_eq!(reg.unbound.len(), UNBOUND_CAP);
    }

    #[tokio::test]
    async fn the_early_ended_cache_evicts_the_oldest_and_keeps_the_rest() {
        let handle = Handle::current();
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
            handle.clone(),
            Some(bus.clone()),
            None,
        ));
        let chats = Arc::new(LlmChats::new(handle, requests, bus, LlmConfig::default()));
        for i in 0..=ENDED_EARLY_CAP as u64 {
            chats.on_request_state(RequestId(i), RequestState::Done);
        }
        let reg = chats.reg();
        assert_eq!(reg.ended_early.len(), ENDED_EARLY_CAP);
        assert!(!reg.ended_early.contains_key(&RequestId(0)), "oldest goes");
        assert!(reg.ended_early.contains_key(&RequestId(1)));
        assert!(
            reg.ended_early
                .contains_key(&RequestId(ENDED_EARLY_CAP as u64))
        );
    }

    /// A model naming another configured provider is told apart from one
    /// on the default, so where a project or session file sends a chat can
    /// be said.
    #[tokio::test]
    async fn a_model_on_another_provider_is_named() {
        let handle = Handle::current();
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
            handle.clone(),
            Some(bus.clone()),
            None,
        ));
        let config = LlmConfig::from_table(
            &"[llm]\ndefault_provider = \"local\"\n\
              [llm.providers.local]\nkind = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:1/v1\"\nmodel = \"m\"\n\
              [llm.providers.paid]\nkind = \"anthropic\"\nmodel = \"big\"\napi_key_env = \"X\"\n"
                .parse::<toml::Table>()
                .unwrap(),
        );
        let chats = LlmChats::new(handle, requests, bus, config);
        assert_eq!(chats.other_provider(Some("m2")), None);
        assert_eq!(chats.other_provider(Some("local/m2")), None);
        assert_eq!(
            chats.other_provider(Some("paid/big")),
            Some(("paid".to_string(), "local".to_string()))
        );
    }

    /// A reply the provider cut short (a safety stop, the token limit) is
    /// not handed to whoever waits on the turn as if it were the answer.
    #[tokio::test]
    async fn a_reply_stopped_early_reaches_a_waiter_as_the_reason() {
        let handle = Handle::current();
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
            handle.clone(),
            Some(bus.clone()),
            None,
        ));
        let chats = Arc::new(LlmChats::new(handle, requests, bus, LlmConfig::default()));
        let session = chats.create_with(None, None);
        let ended = session.wait_turn_end();
        session.store.mutate(&session.key, |c| {
            c.error = Some("the model stopped: safety".to_string());
        });
        chats.end_turn(&session, None);
        assert_eq!(
            ended.await.unwrap(),
            Err("the model stopped: safety".to_string())
        );
        // A clean end still hands over the reply.
        let ended = session.wait_turn_end();
        session.store.mutate(&session.key, |c| c.error = None);
        chats.end_turn(&session, None);
        assert!(ended.await.unwrap().is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_link_to_an_excluded_file_is_excluded() {
        let handle = Handle::current();
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let requests: RequestRegistryHandle = Arc::new(RequestRegistry::new(
            handle.clone(),
            Some(bus.clone()),
            None,
        ));
        let chats = LlmChats::new(handle, requests, bus, LlmConfig::default());
        let dir = std::env::temp_dir().join(format!("lattice-excl-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join(".env"), "KEY=1").unwrap();
        std::os::unix::fs::symlink(dir.join(".env"), dir.join("notes.txt")).unwrap();
        assert_eq!(
            chats.excluded(&dir.join("notes.txt")).as_deref(),
            Some(".env")
        );
        assert_eq!(
            chats.excluded(&dir.join("sub/../.env")).as_deref(),
            Some(".env")
        );
        assert_eq!(chats.excluded(&dir.join("sub")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
