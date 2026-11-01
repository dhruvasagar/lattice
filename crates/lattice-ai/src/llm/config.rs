//! The `[llm]` table of the user config: named providers and a default model.
//!
//! Read straight from `lattice.toml` at boot rather than through the typed
//! option registry. The registry has no notion of a table of named providers,
//! and reading the file here keeps this crate from depending on `lattice-host`.
//! The cost is that edits apply on the next start; layered settings (global,
//! profile, project, session) arrive with LL.7 and will move this onto the
//! typed options.
//!
//! ```toml
//! [llm]
//! default_provider = "anthropic"
//! default_model = "claude-sonnet-4-5"
//!
//! [llm.providers.anthropic]
//! api_key_env = "ANTHROPIC_API_KEY"     # or api_key_command; never a key in TOML
//!
//! [llm.providers.local]
//! kind = "openai-compatible"
//! base_url = "http://localhost:11434/v1"
//! model = "qwen2.5-coder"
//! ```
//!
//! A key is never read from this file. It comes from an environment variable
//! (`api_key_env`) or a command whose first output line is the key
//! (`api_key_command`, run off the editor thread). Neither the command's output
//! nor its stderr ever reaches an error message.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::conversation::{FoldDisplay, ProjectionOptions};

use super::{AnthropicProvider, ApiKey, OpenAiProvider, Provider};

const ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
const OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// How long `api_key_command` may run before the request fails.
pub const KEY_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Anthropic,
    OpenAiCompatible,
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

impl ProviderConfig {
    pub fn build(&self) -> Arc<dyn Provider> {
        match self.kind {
            ProviderKind::Anthropic => Arc::new(AnthropicProvider::new(&self.base_url)),
            ProviderKind::OpenAiCompatible => Arc::new(OpenAiProvider::new(&self.base_url)),
        }
    }
}

/// The parsed `[llm]` table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LlmConfig {
    providers: BTreeMap<String, ProviderConfig>,
    default_provider: Option<String>,
    default_model: Option<String>,
    /// `llm.fold.reasoning` and `llm.fold.tools`: how each block type is shown.
    /// LL.7 moves these onto layered settings.
    pub fold_reasoning: FoldDisplay,
    pub fold_tools: FoldDisplay,
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
                warnings: vec![format!("llm: {} is not valid TOML: {e}", path.display())],
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
        cfg.default_model = str_of(llm, "default_model");
        if let Some(fold) = llm.get("fold").and_then(toml::Value::as_table) {
            for key in fold
                .keys()
                .filter(|k| !matches!(k.as_str(), "reasoning" | "tools"))
            {
                tracing::warn!("llm: unknown key llm.fold.{key}; ignoring it");
                cfg.warnings
                    .push(format!("llm: unknown key llm.fold.{key}"));
            }
            for (key, slot) in [
                ("reasoning", &mut cfg.fold_reasoning),
                ("tools", &mut cfg.fold_tools),
            ] {
                let Some(value) = fold.get(key) else {
                    continue;
                };
                let Some(raw) = value
                    .as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                else {
                    tracing::warn!(
                        "llm: llm.fold.{key} must be a string (closed, open or hidden); keeping the default"
                    );
                    cfg.warnings.push(format!(
                        "llm: llm.fold.{key} must be closed, open or hidden, not `{value}`"
                    ));
                    continue;
                };
                match FoldDisplay::parse(&raw) {
                    Some(d) => *slot = d,
                    None => {
                        tracing::warn!(
                            "llm: llm.fold.{key} must be closed, open or hidden, not `{raw}`; keeping the default"
                        );
                        cfg.warnings.push(format!(
                            "llm: llm.fold.{key} must be closed, open or hidden, not `{raw}`"
                        ));
                    }
                }
            }
        }
        if let Some(providers) = llm.get("providers").and_then(toml::Value::as_table) {
            for (name, value) in providers {
                match value.as_table() {
                    Some(t) => match parse_provider(name, t) {
                        Ok(p) => {
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

    /// The projection options a chat transcript uses: markdown on, folds as
    /// configured.
    pub fn projection_options(&self) -> ProjectionOptions {
        ProjectionOptions {
            markdown: true,
            reasoning: self.fold_reasoning,
            tools: self.fold_tools,
        }
    }

    pub fn provider_names(&self) -> impl Iterator<Item = &str> {
        self.providers.keys().map(String::as_str)
    }

    /// Choose the provider and model a new chat uses.
    ///
    /// The provider is `default_provider`, or the only one configured. The model
    /// is the provider's own `model`, else `default_model`. Failures say what to
    /// configure, since this text lands in the chat buffer.
    pub fn choose(&self) -> Result<ChosenModel, String> {
        let provider = match (&self.default_provider, self.providers.len()) {
            (_, 0) => {
                return Err(
                    "no LLM provider configured: add an [llm.providers.<name>] table \
                            to lattice.toml (see :help llm-chat-mode)"
                        .to_string(),
                );
            }
            (Some(name), _) => self.providers.get(name).ok_or_else(|| {
                format!("llm.default_provider `{name}` is not a configured provider")
            })?,
            (None, 1) => self.providers.values().next().ok_or("no provider")?,
            (None, _) => {
                let names: Vec<&str> = self.provider_names().collect();
                return Err(format!(
                    "several LLM providers are configured ({}): set llm.default_provider",
                    names.join(", ")
                ));
            }
        };
        let model = provider
            .model
            .clone()
            .or_else(|| self.default_model.clone())
            .ok_or_else(|| {
                format!(
                    "no model for provider `{}`: set llm.default_model or \
                     llm.providers.{}.model",
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

fn parse_provider(name: &str, t: &toml::Table) -> Result<ProviderConfig, String> {
    // The design's example gives `anthropic` no `kind`; the name says it.
    let kind = match str_of(t, "kind").as_deref() {
        Some("anthropic") => ProviderKind::Anthropic,
        Some("openai-compatible") => ProviderKind::OpenAiCompatible,
        Some(other) => {
            return Err(format!(
                "llm: provider `{name}`: unknown kind `{other}` \
                 (expected `anthropic` or `openai-compatible`)"
            ));
        }
        None if name == "anthropic" => ProviderKind::Anthropic,
        None => ProviderKind::OpenAiCompatible,
    };
    let base_url = str_of(t, "base_url").unwrap_or_else(|| {
        match kind {
            ProviderKind::Anthropic => ANTHROPIC_BASE_URL,
            ProviderKind::OpenAiCompatible => OPENAI_BASE_URL,
        }
        .to_string()
    });
    let key = match (str_of(t, "api_key_env"), str_of(t, "api_key_command")) {
        (Some(var), _) => KeySource::Env(var),
        (None, Some(cmd)) => KeySource::Command(cmd),
        (None, None) => KeySource::None,
    };
    Ok(ProviderConfig {
        name: name.to_string(),
        kind,
        base_url,
        key,
        model: str_of(t, "model"),
        input_cost_per_mtok: t.get("input_cost_per_mtok").and_then(number),
        output_cost_per_mtok: t.get("output_cost_per_mtok").and_then(number),
    })
}

fn number(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
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
            ProviderKind::Anthropic => Err(format!(
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
    fn an_absent_table_is_an_empty_config_that_says_how_to_fix_it() {
        let c = cfg("[ui]\nx = 1\n");
        let err = c.choose().unwrap_err();
        assert!(err.contains("llm.providers"), "{err}");
    }

    #[test]
    fn the_design_example_parses() {
        let c = cfg(r#"
            [llm]
            default_model = "claude-x"
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
    }

    #[test]
    fn several_providers_need_a_default() {
        let c = cfg(r#"
            [llm]
            default_model = "m"
            [llm.providers.a]
            [llm.providers.b]
        "#);
        assert!(c.choose().unwrap_err().contains("default_provider"));
    }

    #[test]
    fn the_only_provider_is_the_default_and_its_model_wins() {
        let c = cfg(r#"
            [llm]
            default_model = "fallback"
            [llm.providers.local]
            model = "own"
        "#);
        let chosen = c.choose().unwrap();
        assert_eq!(chosen.provider.name, "local");
        assert_eq!(chosen.model, "own");
    }

    #[test]
    fn a_missing_model_names_where_to_set_it() {
        let c = cfg("[llm.providers.local]\n");
        assert!(c.choose().unwrap_err().contains("default_model"));
    }

    #[test]
    fn a_bad_provider_is_skipped_with_a_warning_and_the_rest_survive() {
        let c = cfg(r#"
            [llm]
            default_model = "m"
            [llm.providers.bad]
            kind = "gemini"
            [llm.providers.good]
        "#);
        assert_eq!(c.warnings.len(), 1);
        assert_eq!(c.choose().unwrap().provider.name, "good");
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
