//! The `[llm]` table of the user config: named providers, profiles and the
//! global values of the session options.
//!
//! The scalar settings (`model`, `system`, `temperature`, ...) are the typed
//! `llm.*` options ([`super::options`]); the loader writes them into the
//! registry like any option, and [`super::settings`] reads them from there so
//! `:set` applies live. What the registry cannot hold is a table of named
//! things, so providers (`[llm.providers.<name>]`), profiles
//! (`[llm.profiles.<name>]`) and MCP servers (`[llm.mcp.<name>]`, LL.24) are
//! read here, once at boot. Editing those applies on the next start.
//!
//! ```toml
//! [llm]
//! default_provider = "anthropic"
//! default_profile = "general"
//! model = "claude-sonnet-4-5"
//!
//! [llm.providers.anthropic]
//! api_key_env = "ANTHROPIC_API_KEY"     # or api_key_command; never a key in TOML
//!
//! [llm.providers.gemini]                # kind `gemini` by name
//! api_key_env = "GEMINI_API_KEY"
//!
//! [llm.providers.local]
//! kind = "openai-compatible"
//! base_url = "http://localhost:11434/v1"
//! model = "qwen2.5-coder"
//!
//! [llm.profiles.writer]
//! model = "local/qwen2.5-coder"
//! system = "~/.config/lattice/llm/editor.md"
//!
//! [llm.mcp.github]                      # tools are github.<tool>
//! command = "github-mcp-server"
//! args = ["stdio"]
//! env = { GITHUB_PERSONAL_ACCESS_TOKEN = "$GITHUB_TOKEN" }
//! allow = ["get_issue"]                 # runs without asking
//!
//! [llm.mcp.docs]
//! url = "https://example.com/mcp"       # streamable HTTP
//! headers = { Authorization = "Bearer $DOCS_TOKEN" }
//! ```
//!
//! A key is never read from this file. It comes from an environment variable
//! (`api_key_env`) or a command whose first output line is the key
//! (`api_key_command`, run off the editor thread). A literal `api_key` is
//! refused with a message. Neither the command's output nor its stderr ever
//! reaches an error message.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::settings::{RawSet, SettingKey, parse_profile, validate, value_text};
use super::{AnthropicProvider, ApiKey, GeminiProvider, OpenAiProvider, Provider};

const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const GEMINI_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// How long `api_key_command` may run before the request fails.
pub const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAiCompatible,
    Gemini,
}

/// Where a provider's key comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeySource {
    /// No key: a local server.
    None,
    Env(String),
    Command(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    pub base_url: String,
    pub key: KeySource,
    pub model: Option<String>,
    /// USD per million input / output tokens, for the headerline's cost. Absent
    /// means no cost is shown: a made-up price is worse than none.
    pub input_cost_per_mtok: Option<f64>,
    pub output_cost_per_mtok: Option<f64>,
}

/// `[llm.web]`: what `web.search` asks (AT.2). Only ever from the user's own
/// file: a backend holds a key source, which a project may not name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebConfig {
    /// `search = "brave" | "searxng" | "duckduckgo"`. `None` lists no
    /// `web.search` tool.
    pub search: Option<SearchBackend>,
}

/// A search backend and what it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchBackend {
    /// The Brave Search API: a subscription key.
    Brave { key: KeySource },
    /// A SearXNG instance, usually self-hosted, with its JSON format enabled.
    Searxng { base_url: String },
    /// DuckDuckGo's HTML results page, scraped (AT.15). Needs nothing, and can
    /// break from a markup change or a CAPTCHA, so it is never the default.
    DuckDuckGo,
}

impl SearchBackend {
    /// Its name as `search = ...` spells it.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Brave { .. } => "brave",
            Self::Searxng { .. } => "searxng",
            Self::DuckDuckGo => "duckduckgo",
        }
    }
}

impl ProviderKind {
    /// The tag on reasoning data this kind of provider writes and accepts
    /// back, `None` for one that round-trips none.
    pub fn reasoning_tag(self) -> Option<&'static str> {
        match self {
            ProviderKind::Anthropic => Some(super::provider::ANTHROPIC_REASONING_TAG),
            ProviderKind::Gemini => Some(super::provider::GEMINI_REASONING_TAG),
            ProviderKind::OpenAiCompatible => None,
        }
    }
}

impl ProviderConfig {
    pub fn build(&self) -> Arc<dyn Provider> {
        match self.kind {
            ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(&self.base_url)),
            ProviderKind::OpenAiCompatible => Arc::new(OpenAiProvider::new(&self.base_url)),
            ProviderKind::Gemini => Arc::new(GeminiProvider::new(&self.base_url)),
        }
    }
}

/// The parsed `[llm]` table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmConfig {
    providers: BTreeMap<String, ProviderConfig>,
    default_provider: Option<String>,
    /// `[llm.profiles.<name>]`, trusted: it is the user's own file.
    pub profiles: BTreeMap<String, RawSet>,
    /// `default_profile`, if the table names one.
    pub default_profile: Option<String>,
    /// The scalar settings the table itself holds (`model = ...`). In the editor
    /// the loader has also written these into the registry, whose live value
    /// wins; this copy is what a config read without a registry (a test, the
    /// `LlmConfig::load` path) resolves from.
    pub global: RawSet,
    /// `[llm.mcp.<name>]`, in name order. Only ever from the user's own file:
    /// a project file cannot declare one (settings' trust table).
    pub mcp: Vec<super::tools::mcp::McpServerConfig>,
    /// `[llm.web]` (AT.2), from the user's own file only.
    pub web: WebConfig,
    /// Problems found while reading, for the log. A bad provider is skipped,
    /// not fatal: one typo must not take the others down.
    pub warnings: Vec<String>,
}

/// A provider and model chosen for a chat.
#[derive(Debug, Clone, PartialEq)]
pub struct ChosenModel {
    pub provider: ProviderConfig,
    pub model: String,
}

impl LlmConfig {
    /// Read `[llm]` from the user's `lattice.toml`. A missing file is an empty
    /// config; an unreadable or malformed one is an empty config with a warning.
    pub fn load_user() -> Self {
        let Some(path) = lattice_config::default_user_config_path() else {
            return Self::default();
        };
        Self::load(&path)
    }

    pub fn load(path: &Path) -> Self {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                return Self {
                    warnings: vec![format!("llm: cannot read {}: {e}", path.display())],
                    ..Self::default()
                };
            }
        };
        match text.parse::<toml::Table>() {
            Ok(table) => Self::from_table(&table),
            Err(e) => Self {
                warnings: vec![format!(
                    "llm: {} is not valid TOML: {}",
                    path.display(),
                    lattice_config::loader::describe_toml_error(&text, &e)
                )],
                ..Self::default()
            },
        }
    }

    /// Parse the `llm` table out of a whole config file's table.
    pub fn from_table(root: &toml::Table) -> Self {
        let mut cfg = Self::default();
        let Some(llm) = root.get("llm").and_then(toml::Value::as_table) else {
            return cfg;
        };
        cfg.default_provider = str_of(llm, "default_provider");
        cfg.default_profile = str_of(llm, "default_profile");
        cfg.read_global(llm);
        if let Some(profiles) = llm.get("profiles").and_then(toml::Value::as_table) {
            for (name, value) in profiles {
                match value.as_table() {
                    Some(t) => {
                        let set = parse_profile(name, t, &mut cfg.warnings);
                        cfg.profiles.insert(name.clone(), set);
                    }
                    None => cfg
                        .warnings
                        .push(format!("llm: profile `{name}` must be a table")),
                }
            }
        }
        if let Some(servers) = llm.get("mcp").and_then(toml::Value::as_table) {
            for (name, value) in servers {
                match value.as_table() {
                    Some(t) => match parse_mcp_server(name, t) {
                        Ok((server, mut warns)) => {
                            cfg.warnings.append(&mut warns);
                            cfg.mcp.push(server);
                        }
                        Err(w) => cfg.warnings.push(w),
                    },
                    None => cfg
                        .warnings
                        .push(format!("llm: MCP server `{name}` must be a table")),
                }
            }
        }
        if let Some(web) = llm.get("web") {
            match web.as_table() {
                Some(t) => cfg.web = parse_web(t, &mut cfg.warnings),
                None => cfg.warnings.push("llm: `web` must be a table".to_string()),
            }
        }
        if let Some(providers) = llm.get("providers").and_then(toml::Value::as_table) {
            for (name, value) in providers {
                match value.as_table() {
                    Some(t) => match parse_provider(name, t) {
                        Ok((p, mut warns)) => {
                            cfg.warnings.append(&mut warns);
                            cfg.providers.insert(name.clone(), p);
                        }
                        Err(w) => cfg.warnings.push(w),
                    },
                    None => cfg
                        .warnings
                        .push(format!("llm: provider `{name}` must be a table")),
                }
            }
        }
        cfg
    }

    /// The scalar settings in `[llm]` and `[llm.fold]`.
    fn read_global(&mut self, llm: &toml::Table) {
        for key in SettingKey::ALL {
            let value = match key {
                SettingKey::FoldReasoning | SettingKey::FoldTools => llm
                    .get("fold")
                    .and_then(toml::Value::as_table)
                    .and_then(|f| f.get(key.option_name().trim_start_matches("llm.fold."))),
                SettingKey::Profile => continue,
                _ => llm.get(key.option_name().trim_start_matches("llm.")),
            };
            let Some(value) = value else { continue };
            let text = value_text(value);
            match validate(key, &text) {
                Ok(()) => {
                    self.global.insert(key, text);
                }
                Err(e) => self.warnings.push(format!("llm: {e}")),
            }
        }
        if llm.contains_key("default_model") {
            self.warnings.push(
                "llm: `default_model` is now `model` (or `provider/model`); it is ignored"
                    .to_string(),
            );
        }
        if let Some(fold) = llm.get("fold").and_then(toml::Value::as_table) {
            for key in fold
                .keys()
                .filter(|k| !matches!(k.as_str(), "reasoning" | "tools"))
            {
                self.warnings
                    .push(format!("llm: unknown key llm.fold.{key}"));
            }
        }
    }

    /// The environment variables the configured providers read their keys
    /// from. A process the model can start (a command, an MCP server) does not
    /// inherit them: the key is lattice's to send, not the model's to read.
    pub fn key_variables(&self) -> Vec<String> {
        let search = match &self.web.search {
            Some(SearchBackend::Brave { key }) => Some(key),
            _ => None,
        };
        self.providers
            .values()
            .map(|p| &p.key)
            .chain(search)
            .filter_map(|key| match key {
                KeySource::Env(var) => Some(var.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn provider_names(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    pub fn profile_names(&self) -> impl Iterator<Item = &str> {
        self.profiles.keys().map(String::as_str)
    }

    /// Choose the provider and model for `model` (`provider/model`, a bare
    /// model, or nothing).
    ///
    /// A leading `name/` selects that provider when `name` is a configured one;
    /// anything else is a model on the default provider (so `meta/llama-3` still
    /// works on a gateway). The default provider is `default_provider` (the
    /// option when set, else the table's), or the only one configured. With no
    /// model, the provider's own `model` is used. Failures say what to
    /// configure, since this text lands in the chat buffer.
    pub fn choose(
        &self,
        model: Option<&str>,
        default_provider: Option<&str>,
    ) -> Result<ChosenModel, String> {
        if self.providers.is_empty() {
            return Err(
                "no LLM provider configured: add an [llm.providers.<name>] table \
                        to lattice.toml (see :help llm)"
                    .to_string(),
            );
        }
        let (named, bare) = match model.map(str::trim).filter(|m| !m.is_empty()) {
            Some(m) => match m.split_once('/') {
                Some((p, rest)) if self.providers.contains_key(p) && !rest.is_empty() => {
                    (Some(p), Some(rest))
                }
                _ => (None, Some(m)),
            },
            None => (None, None),
        };
        let default_name = default_provider
            .filter(|p| !p.trim().is_empty())
            .or(self.default_provider.as_deref());
        let provider = match (named, default_name, self.providers.len()) {
            (Some(p), _, _) => self.providers.get(p).ok_or("no provider")?,
            (None, Some(name), _) => self.providers.get(name).ok_or_else(|| {
                format!("llm.default_provider `{name}` is not a configured provider")
            })?,
            (None, None, 1) => self.providers.values().next().ok_or("no provider")?,
            (None, None, _) => {
                let names: Vec<&str> = self.provider_names().collect();
                return Err(format!(
                    "several LLM providers are configured ({}): set llm.default_provider, \
                     or name one in llm.model as provider/model",
                    names.join(", ")
                ));
            }
        };
        let model = bare
            .map(str::to_string)
            .or_else(|| provider.model.clone())
            .ok_or_else(|| {
                format!(
                    "no model for provider `{}`: set llm.model or llm.providers.{}.model",
                    provider.name, provider.name
                )
            })?;
        Ok(ChosenModel {
            provider: provider.clone(),
            model,
        })
    }
}

fn str_of(t: &toml::Table, key: &str) -> Option<String> {
    t.get(key)
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_provider(name: &str, t: &toml::Table) -> Result<(ProviderConfig, Vec<String>), String> {
    let mut warnings = Vec::new();
    // A literal key is the one thing this file must never hold: it ends up in
    // backups, dotfile repos and screen shares. Refuse it by name, and do not
    // echo the value.
    if t.contains_key("api_key") {
        warnings.push(format!(
            "llm: provider `{name}`: `api_key` is refused: a key never goes in TOML. \
             Use api_key_env = \"NAME\" or api_key_command = \"pass show ...\""
        ));
    }
    // The design's example gives `anthropic` no `kind`; the name says it.
    let kind = match str_of(t, "kind").as_deref() {
        Some("anthropic") => ProviderKind::Anthropic,
        Some("openai-compatible") => ProviderKind::OpenAiCompatible,
        Some("gemini") => ProviderKind::Gemini,
        Some(other) => {
            return Err(format!(
                "llm: provider `{name}`: unknown kind `{other}` \
                 (expected `anthropic`, `gemini` or `openai-compatible`)"
            ));
        }
        None if name == "anthropic" => ProviderKind::Anthropic,
        None if name == "gemini" => ProviderKind::Gemini,
        None => ProviderKind::OpenAiCompatible,
    };
    let base_url = str_of(t, "base_url").unwrap_or_else(|| {
        match kind {
            ProviderKind::Anthropic => ANTHROPIC_BASE_URL,
            ProviderKind::OpenAiCompatible => OPENAI_BASE_URL,
            ProviderKind::Gemini => GEMINI_BASE_URL,
        }
        .to_string()
    });
    let key = match (str_of(t, "api_key_env"), str_of(t, "api_key_command")) {
        (Some(var), _) => KeySource::Env(var),
        (None, Some(cmd)) => KeySource::Command(cmd),
        (None, None) => KeySource::None,
    };
    Ok((
        ProviderConfig {
            name: name.to_string(),
            kind,
            base_url,
            key,
            model: str_of(t, "model"),
            input_cost_per_mtok: t.get("input_cost_per_mtok").and_then(number),
            output_cost_per_mtok: t.get("output_cost_per_mtok").and_then(number),
        },
        warnings,
    ))
}

/// `[llm.web]`. A table naming no backend, or one that is misconfigured, is
/// no `web.search` and a warning saying why; `web.fetch` needs nothing here.
fn parse_web(t: &toml::Table, warnings: &mut Vec<String>) -> WebConfig {
    for key in t.keys() {
        if !matches!(key.as_str(), "search" | "brave" | "searxng") {
            warnings.push(format!("llm: unknown key llm.web.{key}"));
        }
    }
    let Some(name) = t.get("search") else {
        return WebConfig::default();
    };
    let section = |key: &str| t.get(key).and_then(toml::Value::as_table);
    let search = match name.as_str() {
        Some("brave") => {
            let b = section("brave");
            let get = |k: &str| b.and_then(|b| str_of(b, k));
            match (get("api_key_env"), get("api_key_command")) {
                (Some(var), _) => Some(SearchBackend::Brave {
                    key: KeySource::Env(var),
                }),
                (None, Some(cmd)) => Some(SearchBackend::Brave {
                    key: KeySource::Command(cmd),
                }),
                (None, None) => {
                    warnings.push(
                        "llm: web search `brave` needs [llm.web.brave] api_key_env or \
                         api_key_command; web.search is off"
                            .to_string(),
                    );
                    None
                }
            }
        }
        Some("duckduckgo") => Some(SearchBackend::DuckDuckGo),
        Some("searxng") => match section("searxng").and_then(|s| str_of(s, "base_url")) {
            Some(url) if url.starts_with("http://") || url.starts_with("https://") => {
                Some(SearchBackend::Searxng {
                    base_url: url.trim_end_matches('/').to_string(),
                })
            }
            Some(_) => {
                warnings.push(
                    "llm: [llm.web.searxng] base_url must be an http(s) URL; web.search is off"
                        .to_string(),
                );
                None
            }
            None => {
                warnings.push(
                    "llm: web search `searxng` needs [llm.web.searxng] base_url; web.search is off"
                        .to_string(),
                );
                None
            }
        },
        _ => {
            warnings.push(format!(
                "llm: llm.web.search must be \"searxng\", \"brave\" or \"duckduckgo\", not \
                 {name}; web.search is off"
            ));
            None
        }
    };
    WebConfig { search }
}

/// One `[llm.mcp.<name>]` table. Exactly one of `command` (stdio) and `url`
/// (streamable HTTP). Values are never echoed: `env` and `headers` can hold
/// tokens.
fn parse_mcp_server(
    name: &str,
    t: &toml::Table,
) -> Result<(super::tools::mcp::McpServerConfig, Vec<String>), String> {
    use super::tools::mcp::{McpServerConfig, McpTransport};
    let mut warnings = Vec::new();
    let valid_name =
        !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if !valid_name {
        return Err(format!(
            "llm: MCP server `{name}`: a server name is letters, digits and `-` only \
             (its tools are `{name}.<tool>`)"
        ));
    }
    if super::tools::broker::NATIVE.contains(&name) {
        return Err(format!(
            "llm: MCP server `{name}`: that name is lattice's own tools"
        ));
    }
    let strings = |key: &str, warnings: &mut Vec<String>| -> Vec<String> {
        match t.get(key) {
            None => Vec::new(),
            Some(toml::Value::Array(items)) => items
                .iter()
                .filter_map(|v| match v.as_str() {
                    Some(s) => Some(s.to_string()),
                    None => {
                        warnings.push(format!(
                            "llm: MCP server `{name}`: `{key}` holds something that is not a string; it is skipped"
                        ));
                        None
                    }
                })
                .collect(),
            Some(_) => {
                warnings.push(format!(
                    "llm: MCP server `{name}`: `{key}` must be a list of strings"
                ));
                Vec::new()
            }
        }
    };
    let table = |key: &str, warnings: &mut Vec<String>| -> BTreeMap<String, String> {
        match t.get(key) {
            None => BTreeMap::new(),
            Some(toml::Value::Table(entries)) => entries
                .iter()
                .filter_map(|(k, v)| match v.as_str() {
                    Some(s) => Some((k.clone(), s.to_string())),
                    None => {
                        warnings.push(format!(
                            "llm: MCP server `{name}`: `{key}.{k}` must be a string; it is skipped"
                        ));
                        None
                    }
                })
                .collect(),
            Some(_) => {
                warnings.push(format!(
                    "llm: MCP server `{name}`: `{key}` must be a table of strings"
                ));
                BTreeMap::new()
            }
        }
    };
    let transport = match (str_of(t, "command"), str_of(t, "url")) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "llm: MCP server `{name}`: give `command` (stdio) or `url` (HTTP), not both"
            ));
        }
        (None, None) => {
            return Err(format!(
                "llm: MCP server `{name}`: needs `command` (stdio) or `url` (HTTP)"
            ));
        }
        (Some(command), None) => McpTransport::Stdio {
            command,
            args: strings("args", &mut warnings),
            env: table("env", &mut warnings),
            cwd: str_of(t, "cwd").map(|c| PathBuf::from(lattice_core::home::expand_tilde(&c))),
        },
        (None, Some(url)) => {
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err(format!(
                    "llm: MCP server `{name}`: `url` must start with http:// or https://"
                ));
            }
            McpTransport::Http {
                url,
                headers: table("headers", &mut warnings),
            }
        }
    };
    let known: &[&str] = match transport {
        McpTransport::Stdio { .. } => &["command", "args", "env", "cwd", "allow", "timeout"],
        McpTransport::Http { .. } => &["url", "headers", "allow", "timeout"],
    };
    let call_timeout = match t.get("timeout") {
        None => super::tools::mcp::DEFAULT_CALL_TIMEOUT,
        Some(v) => match v.as_integer().filter(|n| (1..=3600).contains(n)) {
            Some(secs) => std::time::Duration::from_secs(secs as u64),
            None => {
                warnings.push(format!(
                    "llm: MCP server `{name}`: `timeout` is whole seconds from 1 to 3600; \
                     the default ({}s) applies",
                    super::tools::mcp::DEFAULT_CALL_TIMEOUT.as_secs()
                ));
                super::tools::mcp::DEFAULT_CALL_TIMEOUT
            }
        },
    };
    for key in t.keys().filter(|k| !known.contains(&k.as_str())) {
        warnings.push(format!(
            "llm: MCP server `{name}`: unknown key `{key}`; it is ignored"
        ));
    }
    let allow = strings("allow", &mut warnings).into_iter().collect();
    Ok((
        McpServerConfig {
            name: name.to_string(),
            transport,
            allow,
            call_timeout,
        },
        warnings,
    ))
}

fn number(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// A parameter a request will not send, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Omission {
    pub param: &'static str,
    pub reason: &'static str,
}

impl Omission {
    /// Remove the parameter from `params`.
    pub fn clear(&self, params: &mut super::Params) {
        if self.param == "temperature" {
            params.temperature = None;
        }
    }
}

/// OpenAI models that accept only the default temperature and answer anything
/// else with a 400.
const FIXED_TEMPERATURE_PREFIXES: &[&str] = &["o1", "o3", "o4", "gpt-5"];

/// The parameters in `settings` that this provider and model will not take, so
/// the request omits them instead of failing with a 400 and says so.
///
/// A small per-provider list rather than a capability table, and rather than
/// retrying after a 400 that names the parameter: a table per model is a
/// maintenance burden that is wrong the week after it is written, and a retry
/// doubles the first request's latency and has to parse each provider's error
/// shape. The list holds only what is known to fail; a model it does not know
/// gets the parameter, and the provider's own error text reaches the chat if it
/// objects.
pub fn omitted_params(
    provider: &ProviderConfig,
    model: &str,
    settings: &super::settings::Settings,
) -> Vec<Omission> {
    let mut out = Vec::new();
    if settings.reasoning.is_some() {
        out.push(Omission {
            param: "reasoning",
            reason: "this build does not send a reasoning effort to any provider yet",
        });
    }
    let bare = model.rsplit('/').next().unwrap_or(model);
    if settings.temperature.is_some()
        && provider.kind == ProviderKind::OpenAiCompatible
        && provider.base_url.starts_with(OPENAI_BASE_URL)
        && FIXED_TEMPERATURE_PREFIXES
            .iter()
            .any(|p| bare.starts_with(p))
    {
        out.push(Omission {
            param: "temperature",
            reason: "this model only accepts its default temperature",
        });
    }
    out
}

/// Resolve a provider's key. Runs `api_key_command` as a child process, so
/// call it from a task, never from a handler on the editor thread.
///
/// A provider with no key source is allowed an empty key only if it is
/// OpenAI-compatible (Ollama, llama.cpp run without one); Anthropic without a
/// key is a configuration error worth naming before the request fails with a
/// 401. Error text names the variable or the failure, never the key and never
/// the command's output.
pub async fn resolve_key(provider: &ProviderConfig) -> Result<ApiKey, String> {
    match &provider.key {
        KeySource::None => match provider.kind {
            ProviderKind::OpenAiCompatible => Ok(ApiKey::new("")),
            ProviderKind::Anthropic | ProviderKind::Gemini => Err(format!(
                "provider `{}` has no key: set api_key_env or api_key_command",
                provider.name
            )),
        },
        KeySource::Env(var) => match std::env::var(var) {
            Ok(v) if !v.trim().is_empty() => Ok(ApiKey::new(v.trim())),
            _ => Err(format!(
                "environment variable {var} is not set (provider `{}`)",
                provider.name
            )),
        },
        KeySource::Command(cmd) => run_key_command(&provider.name, cmd).await,
    }
}

/// Resolve the key of something that is not a provider: `whom` names it in
/// errors (`web search brave`), in place of the key. `None` is the empty key.
pub async fn resolve_key_source(whom: &str, key: &KeySource) -> Result<ApiKey, String> {
    match key {
        KeySource::None => Ok(ApiKey::new("")),
        KeySource::Env(var) => match std::env::var(var) {
            Ok(v) if !v.trim().is_empty() => Ok(ApiKey::new(v.trim())),
            _ => Err(format!("environment variable {var} is not set ({whom})")),
        },
        KeySource::Command(cmd) => run_key_command(whom, cmd).await,
    }
}

async fn run_key_command(provider: &str, cmd: &str) -> Result<ApiKey, String> {
    run_key_command_within(provider, cmd, KEY_COMMAND_TIMEOUT).await
}

/// `kill_on_drop` is what makes the timeout real: when the timeout drops the
/// pending `output()` future, the child is killed rather than left running.
async fn run_key_command_within(
    provider: &str,
    cmd: &str,
    timeout: Duration,
) -> Result<ApiKey, String> {
    let job = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(timeout, job).await {
        Err(_) => {
            return Err(format!(
                "api_key_command for `{provider}` did not finish within {}s",
                timeout.as_secs().max(1)
            ));
        }
        Ok(Err(e)) => {
            return Err(format!(
                "api_key_command for `{provider}` could not start ({})",
                e.kind()
            ));
        }
        Ok(Ok(o)) => o,
    };
    if !output.status.success() {
        return Err(match output.status.code() {
            Some(c) => format!("api_key_command for `{provider}` failed (exit status {c})"),
            None => format!("api_key_command for `{provider}` was terminated"),
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    match text.lines().next().map(str::trim).filter(|l| !l.is_empty()) {
        Some(key) => Ok(ApiKey::new(key)),
        None => Err(format!("api_key_command for `{provider}` printed no key")),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn cfg(text: &str) -> LlmConfig {
        LlmConfig::from_table(&text.parse::<toml::Table>().unwrap())
    }

    #[test]
    fn a_web_search_backend_is_read_with_what_it_needs() {
        let c =
            cfg("[llm.web]\nsearch = \"brave\"\n[llm.web.brave]\napi_key_env = \"BRAVE_KEY\"\n");
        assert_eq!(
            c.web.search,
            Some(SearchBackend::Brave {
                key: KeySource::Env("BRAVE_KEY".into())
            })
        );
        // The key's variable is withheld from commands and MCP servers like a
        // provider's.
        assert!(c.key_variables().contains(&"BRAVE_KEY".to_string()));

        let c = cfg(
            "[llm.web]\nsearch = \"searxng\"\n[llm.web.searxng]\nbase_url = \"http://localhost:8888/\"\n",
        );
        assert_eq!(
            c.web.search,
            Some(SearchBackend::Searxng {
                base_url: "http://localhost:8888".into()
            })
        );
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    }

    #[test]
    fn a_web_backend_missing_what_it_needs_is_off_and_says_why() {
        for (text, says) in [
            ("[llm.web]\nsearch = \"brave\"\n", "api_key_env"),
            ("[llm.web]\nsearch = \"searxng\"\n", "base_url"),
            (
                "[llm.web]\nsearch = \"searxng\"\n[llm.web.searxng]\nbase_url = \"file:///x\"\n",
                "http(s)",
            ),
            (
                "[llm.web]\nsearch = \"google\"\n",
                "\"searxng\", \"brave\" or \"duckduckgo\"",
            ),
            ("[llm.web]\nsearhc = \"brave\"\n", "llm.web.searhc"),
        ] {
            let c = cfg(text);
            assert_eq!(c.web.search, None, "{text}");
            assert!(
                c.warnings.iter().any(|w| w.contains(says)),
                "{text}: {:?}",
                c.warnings
            );
        }
        // DuckDuckGo needs no table of its own.
        let c = cfg("[llm.web]\nsearch = \"duckduckgo\"\n");
        assert_eq!(c.web.search, Some(SearchBackend::DuckDuckGo));
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        // No `[llm.web]` at all is no search and nothing to warn about.
        let c = cfg("[llm]\nmodel = \"m\"\n");
        assert_eq!(c.web, WebConfig::default());
        assert!(c.warnings.is_empty());
    }

    #[test]
    fn an_mcp_server_may_not_take_a_native_sources_name() {
        for name in ["lattice", "web"] {
            let c = cfg(&format!("[llm.mcp.{name}]\ncommand = \"x\"\n"));
            assert!(c.mcp.is_empty(), "{name}");
            assert!(
                c.warnings.iter().any(|w| w.contains("lattice's own tools")),
                "{name}: {:?}",
                c.warnings
            );
        }
    }

    #[test]
    fn an_absent_table_is_an_empty_config_that_says_how_to_fix_it() {
        let c = cfg("[ui]\nx = 1\n");
        let err = c.choose(Some("m"), None).unwrap_err();
        assert!(err.contains("llm.providers"), "{err}");
    }

    #[test]
    fn the_design_example_parses() {
        let c = cfg(r#"
            [llm]
            model = "claude-x"
            [llm.providers.anthropic]
            api_key_env = "ANTHROPIC_API_KEY"
            [llm.providers.local]
            kind = "openai-compatible"
            base_url = "http://localhost:11434/v1"
        "#);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        let a = &c.providers["anthropic"];
        assert_eq!(a.kind, ProviderKind::Anthropic);
        assert_eq!(a.base_url, ANTHROPIC_BASE_URL);
        assert_eq!(a.key, KeySource::Env("ANTHROPIC_API_KEY".into()));
        assert_eq!(c.providers["local"].key, KeySource::None);
        assert_eq!(c.global[&SettingKey::Model], "claude-x");
    }

    #[test]
    fn several_providers_need_a_default() {
        let c = cfg(r#"
            [llm.providers.a]
            [llm.providers.b]
        "#);
        let err = c.choose(Some("m"), None).unwrap_err();
        assert!(
            err.contains("default_provider") && err.contains("provider/model"),
            "{err}"
        );
        // A named provider, or a default, settles it.
        assert_eq!(c.choose(Some("b/m"), None).unwrap().provider.name, "b");
        assert_eq!(c.choose(Some("m"), Some("a")).unwrap().provider.name, "a");
    }

    #[test]
    fn the_only_provider_is_the_default_and_a_bare_model_is_its_own() {
        let c = cfg(r#"
            [llm.providers.local]
            model = "own"
        "#);
        let chosen = c.choose(None, None).unwrap();
        assert_eq!(chosen.provider.name, "local");
        assert_eq!(chosen.model, "own");
        // An explicit model beats the provider's.
        assert_eq!(c.choose(Some("other"), None).unwrap().model, "other");
        // A slash that is not a provider name stays part of the model.
        assert_eq!(
            c.choose(Some("meta/llama"), None).unwrap().model,
            "meta/llama"
        );
        assert_eq!(c.choose(Some("local/qwen"), None).unwrap().model, "qwen");
    }

    /// LL.24: `[llm.mcp.<name>]`, stdio and HTTP, with the allow list.
    #[test]
    fn mcp_servers_are_read_with_their_transport_and_allow_list() {
        use crate::llm::tools::mcp::McpTransport;
        let c = cfg(r#"
            [llm.mcp.github]
            command = "github-mcp-server"
            args = ["stdio"]
            env = { GITHUB_PERSONAL_ACCESS_TOKEN = "$GITHUB_TOKEN" }
            allow = ["get_issue"]
            timeout = 30
            [llm.mcp.docs]
            url = "https://example.com/mcp"
            headers = { Authorization = "Bearer $DOCS_TOKEN" }
        "#);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        let names: Vec<&str> = c.mcp.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["docs", "github"]);
        let github = &c.mcp[1];
        assert!(github.allow.contains("get_issue"));
        assert_eq!(github.call_timeout, std::time::Duration::from_secs(30));
        assert_eq!(
            c.mcp[0].call_timeout,
            crate::llm::tools::mcp::DEFAULT_CALL_TIMEOUT
        );
        let bad = cfg("[llm.mcp.x]\ncommand = \"x\"\ntimeout = 0\n");
        assert!(
            bad.warnings.iter().any(|w| w.contains("timeout")),
            "{:?}",
            bad.warnings
        );
        match &github.transport {
            McpTransport::Stdio {
                command, args, env, ..
            } => {
                assert_eq!(command, "github-mcp-server");
                assert_eq!(args, &["stdio"]);
                assert_eq!(env["GITHUB_PERSONAL_ACCESS_TOKEN"], "$GITHUB_TOKEN");
            }
            other => panic!("expected stdio, got {other:?}"),
        }
        assert!(
            matches!(&c.mcp[0].transport, McpTransport::Http { url, headers }
            if url == "https://example.com/mcp" && headers.contains_key("Authorization"))
        );
    }

    /// A bad server is skipped with a reason; the rest still load, and no
    /// warning repeats an `env` or `headers` value (either can be a token).
    #[test]
    fn a_bad_mcp_server_is_skipped_and_its_values_never_echoed() {
        let c = cfg(r#"
            [llm.mcp.both]
            command = "x"
            url = "https://x"
            [llm.mcp.neither]
            allow = ["a"]
            [llm."mcp"."has.dot"]
            command = "x"
            [llm.mcp.lattice]
            command = "x"
            [llm.mcp.ftp]
            url = "ftp://x"
            [llm.mcp.ok]
            command = "ok-server"
            env = { TOKEN = "ghp_literal_secret_value", N = 3 }
            colour = "blue"
        "#);
        let names: Vec<&str> = c.mcp.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["ok"]);
        let all = c.warnings.join("\n");
        for needle in [
            "`both`",
            "`neither`",
            "`has.dot`",
            "`lattice`",
            "`ftp`",
            "env.N",
            "`colour`",
        ] {
            assert!(all.contains(needle), "no warning names {needle}: {all}");
        }
        assert!(!all.contains("ghp_literal_secret_value"), "{all}");
    }

    #[test]
    fn gemini_is_a_kind_and_a_provider_named_gemini_defaults_to_it() {
        let c = cfg(r#"
            [llm.providers.gemini]
            api_key_env = "GEMINI_API_KEY"
            model = "gemini-2.5-flash"
            [llm.providers.vertex-proxy]
            kind = "gemini"
            base_url = "https://proxy.example/v1beta"
        "#);
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        let g = &c.providers["gemini"];
        assert_eq!(g.kind, ProviderKind::Gemini);
        assert_eq!(g.base_url, GEMINI_BASE_URL);
        assert_eq!(c.providers["vertex-proxy"].kind, ProviderKind::Gemini);
        assert_eq!(ProviderKind::Gemini.reasoning_tag(), Some("gemini"));
    }

    #[test]
    fn a_missing_model_names_where_to_set_it() {
        let c = cfg("[llm.providers.local]\n");
        assert!(c.choose(None, None).unwrap_err().contains("llm.model"));
    }

    #[test]
    fn a_bad_provider_is_skipped_with_a_warning_and_the_rest_survive() {
        let c = cfg(r#"
            [llm.providers.bad]
            kind = "carrier-pigeon"
            [llm.providers.good]
        "#);
        assert_eq!(c.warnings.len(), 1);
        assert_eq!(c.choose(Some("m"), None).unwrap().provider.name, "good");
    }

    #[test]
    fn a_literal_api_key_is_refused_by_name_and_never_echoed() {
        let c = cfg(r#"
            [llm.providers.p]
            api_key = "sk-SECRET-VALUE"
            api_key_env = "E"
        "#);
        let w = c.warnings.join("\n");
        assert!(
            w.contains("`api_key` is refused") && w.contains("api_key_env"),
            "{w}"
        );
        assert!(!w.contains("SECRET"), "{w}");
        // The provider still loads, with its safe key source.
        assert_eq!(c.providers["p"].key, KeySource::Env("E".into()));
    }

    #[test]
    fn profiles_and_global_values_are_read_and_bad_ones_skipped() {
        let c = cfg(r#"
            [llm]
            default_profile = "writer"
            temperature = 0.3
            default_model = "old"
            [llm.fold]
            tools = "open"
            reasoning = "folded"
            [llm.profiles.writer]
            model = "local/qwen"
            tools = []
            nonsense = 1
        "#);
        assert_eq!(c.default_profile.as_deref(), Some("writer"));
        assert_eq!(c.global[&SettingKey::Temperature], "0.3");
        assert_eq!(c.global[&SettingKey::FoldTools], "open");
        assert!(!c.global.contains_key(&SettingKey::FoldReasoning));
        assert_eq!(c.profiles["writer"][&SettingKey::Tools], "none");
        let w = c.warnings.join("\n");
        assert!(
            w.contains("reasoning") && w.contains("nonsense") && w.contains("default_model"),
            "{w}"
        );
    }

    #[test]
    fn env_beats_command_and_neither_means_none() {
        let c = cfg(r#"
            [llm.providers.a]
            api_key_env = "E"
            api_key_command = "pass x"
            [llm.providers.b]
            api_key_command = "pass x"
        "#);
        assert_eq!(c.providers["a"].key, KeySource::Env("E".into()));
        assert_eq!(c.providers["b"].key, KeySource::Command("pass x".into()));
    }

    #[test]
    fn a_temperature_the_model_rejects_is_omitted_and_named() {
        use super::super::Params;
        let openai = ProviderConfig {
            name: "openai".into(),
            kind: ProviderKind::OpenAiCompatible,
            base_url: OPENAI_BASE_URL.into(),
            key: KeySource::None,
            model: None,
            input_cost_per_mtok: None,
            output_cost_per_mtok: None,
        };
        let settings = super::super::settings::Settings {
            temperature: Some(0.2),
            ..Default::default()
        };
        let om = omitted_params(&openai, "o3-mini", &settings);
        assert_eq!(om.len(), 1);
        assert_eq!(om[0].param, "temperature");
        let mut p = Params {
            temperature: Some(0.2),
            max_tokens: Some(5),
        };
        om[0].clear(&mut p);
        assert_eq!((p.temperature, p.max_tokens), (None, Some(5)));
        // A model not on the list, and a local server, keep it.
        assert!(omitted_params(&openai, "gpt-4o", &settings).is_empty());
        let local = ProviderConfig {
            base_url: "http://localhost:11434/v1".into(),
            ..openai
        };
        assert!(omitted_params(&local, "o3-mini", &settings).is_empty());
    }

    fn provider(kind: ProviderKind, key: KeySource) -> ProviderConfig {
        ProviderConfig {
            name: "p".into(),
            kind,
            base_url: "http://x".into(),
            key,
            model: None,
            input_cost_per_mtok: None,
            output_cost_per_mtok: None,
        }
    }

    #[tokio::test]
    async fn a_keyless_local_server_gets_an_empty_key_but_anthropic_does_not() {
        let local = provider(ProviderKind::OpenAiCompatible, KeySource::None);
        assert!(resolve_key(&local).await.unwrap().is_empty());
        let a = provider(ProviderKind::Anthropic, KeySource::None);
        assert!(resolve_key(&a).await.unwrap_err().contains("api_key_env"));
        let g = provider(ProviderKind::Gemini, KeySource::None);
        assert!(
            resolve_key(&g).await.unwrap_err().contains("api_key_env"),
            "Gemini needs a key too"
        );
    }

    #[tokio::test]
    async fn an_unset_variable_is_named_and_a_command_gives_its_first_line() {
        let p = provider(
            ProviderKind::Anthropic,
            KeySource::Env("LATTICE_LL5_SURELY_UNSET_VAR".into()),
        );
        let err = resolve_key(&p).await.unwrap_err();
        assert!(err.contains("LATTICE_LL5_SURELY_UNSET_VAR"), "{err}");

        let p = provider(
            ProviderKind::Anthropic,
            KeySource::Command("printf 'sk-from-cmd\\nignored\\n'".into()),
        );
        assert_eq!(resolve_key(&p).await.unwrap().expose(), "sk-from-cmd");
    }

    /// A failing command's output, on either stream, must not reach the error.
    #[tokio::test]
    async fn a_failing_key_command_never_echoes_its_output() {
        let p = provider(
            ProviderKind::Anthropic,
            KeySource::Command("echo sk-LEAKED-secret; echo sk-LEAKED-err >&2; exit 3".into()),
        );
        let err = resolve_key(&p).await.unwrap_err();
        assert!(err.contains("exit status 3"), "{err}");
        assert!(!err.contains("LEAKED"), "{err}");
    }

    #[tokio::test]
    async fn a_key_command_that_outlives_the_timeout_is_killed() {
        let dir = std::env::temp_dir().join(format!("lattice-ll5-kill-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let cmd = format!("sleep 1; touch {}", dir.display());
        let err = run_key_command_within("p", &cmd, Duration::from_millis(200))
            .await
            .unwrap_err();
        assert!(err.contains("did not finish"), "{err}");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(!dir.exists(), "the child survived the timeout");
    }
}
