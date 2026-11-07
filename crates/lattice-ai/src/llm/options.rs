//! The `llm.*` options: the session values a chat or request resolves.
//!
//! They are ordinary typed options, registered the way every native subsystem's
//! are (an `options!` block linked into the global slice), so `:set llm.model?`
//! shows the value and where it came from, and `:setlocal llm.model=x` changes it
//! for one buffer. A buffer is a session (design §4), and `:setlocal` is the
//! buffer-local layer.
//!
//! **Every session option defaults to the empty value, which means "not set at
//! this layer".** That is what lets [`super::settings`] tell a deliberate value
//! from the registry default: a layer contributes a key only when it is
//! non-empty, so an unset global does not mask a profile and a profile does not
//! mask a model the user typed.
//!
//! The text is validated once, in [`super::settings::validate`], which the
//! validators here, the profile parser and the project-file parser all call. A
//! value that `:setlocal` accepts is therefore a value a profile may carry, and
//! the reverse.

use super::settings::{SettingKey, validate};

/// What `llm.exclude` refuses when nothing overrides it: files that hold
/// secrets by convention.
pub const DEFAULT_EXCLUDE: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    ".netrc",
];

/// `llm.exclude`: globs naming files whose text is never sent (design §5).
///
/// A glob without a `/` is matched against the file name, so `.env` refuses
/// every `.env` in the tree; one with a `/` is matched against the whole path.
/// The value **replaces** the default, as `:set` does for every list option, so
/// `:set llm.exclude?` shows exactly what a user is about to replace. Empty
/// excludes nothing, deliberately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExcludeGlobs(pub Vec<String>);

impl Default for ExcludeGlobs {
    fn default() -> Self {
        Self(DEFAULT_EXCLUDE.iter().map(|g| g.to_string()).collect())
    }
}

impl ExcludeGlobs {
    /// The first glob `path` matches, if any.
    pub fn matching(&self, path: &std::path::Path) -> Option<&str> {
        let name = path.file_name()?;
        self.0.iter().map(String::as_str).find(|glob| {
            let Ok(matcher) = globset::Glob::new(glob).map(|g| g.compile_matcher()) else {
                return false;
            };
            if glob.contains('/') {
                matcher.is_match(path)
            } else {
                matcher.is_match(name)
            }
        })
    }
}

impl lattice_config::OptionType for ExcludeGlobs {
    fn parse(s: &str) -> Result<Self, String> {
        let globs: Vec<String> = s
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .collect();
        for glob in &globs {
            globset::Glob::new(glob)
                .map_err(|e| format!("llm.exclude: `{glob}` is not a glob ({e})"))?;
        }
        Ok(Self(globs))
    }

    fn format(&self) -> String {
        self.0.join(",")
    }

    fn type_label() -> &'static str {
        "globs"
    }

    /// A TOML array joins into this (`exclude = [".env", "*.pem"]`).
    fn accepts_list() -> bool {
        true
    }
}

fn check(key: SettingKey, value: &str) -> Result<(), String> {
    validate(key, value)
}

#[allow(clippy::ptr_arg)]
fn v_model(v: &String) -> Result<(), String> {
    check(SettingKey::Model, v)
}
#[allow(clippy::ptr_arg)]
fn v_temperature(v: &String) -> Result<(), String> {
    check(SettingKey::Temperature, v)
}
fn v_max_tokens(v: &i64) -> Result<(), String> {
    check(SettingKey::MaxTokens, &v.to_string())
}
#[allow(clippy::ptr_arg)]
fn v_reasoning(v: &String) -> Result<(), String> {
    check(SettingKey::Reasoning, v)
}
#[allow(clippy::ptr_arg)]
fn v_messages(v: &String) -> Result<(), String> {
    check(SettingKey::Messages, v)
}
#[allow(clippy::ptr_arg)]
fn v_tools(v: &String) -> Result<(), String> {
    check(SettingKey::Tools, v)
}
#[allow(clippy::ptr_arg)]
fn v_destination(v: &String) -> Result<(), String> {
    check(SettingKey::Destination, v)
}
#[allow(clippy::ptr_arg)]
fn v_fold_reasoning(v: &String) -> Result<(), String> {
    check(SettingKey::FoldReasoning, v)
}
#[allow(clippy::ptr_arg)]
fn v_fold_tools(v: &String) -> Result<(), String> {
    check(SettingKey::FoldTools, v)
}
#[allow(clippy::ptr_arg)]
fn v_profile(v: &String) -> Result<(), String> {
    check(SettingKey::Profile, v)
}

lattice_config::options! {
    group = lattice_config::Llm;

    /// The model a request uses: `provider/model` (`local/qwen2.5-coder`), or a
    /// bare model name on the default provider. Empty leaves the choice to the
    /// layer below (the profile, then the provider's own `model`).
    #[name("llm.model")]
    #[validate(v_model)]
    pub LlmModel: String = String::new();

    /// The system prompt, as text. A profile in your own config may instead give
    /// a path (`~/.config/lattice/llm/editor.md`) whose file is read when the
    /// request starts. Empty means none.
    #[name("llm.system")]
    pub LlmSystem: String = String::new();

    /// Sampling temperature, 0 to 2. Empty leaves the provider's default; a
    /// model that rejects the parameter has it omitted, and the request says so.
    #[name("llm.temperature")]
    #[validate(v_temperature)]
    pub LlmTemperature: String = String::new();

    /// Output token limit. `0` leaves the provider's default (Anthropic, which
    /// requires one, uses 4096).
    #[name("llm.max_tokens")]
    #[validate(v_max_tokens)]
    pub LlmMaxTokens: i64 = 0;

    /// Reasoning effort: `off`, `low`, `medium` or `high`. Empty leaves the
    /// model's default. Not every provider adapter sends it yet; one that does
    /// not omits it and says so.
    #[name("llm.reasoning")]
    #[validate(v_reasoning)]
    pub LlmReasoning: String = String::new();

    /// How much history a request sends: `all`, `last` (the final user message
    /// only) or a number of trailing messages. Empty means `all`.
    #[name("llm.messages")]
    #[validate(v_messages)]
    pub LlmMessages: String = String::new();

    /// Tools the model may call: a comma-separated list of tool names, or
    /// `none`. Empty means the profile's selection, and with no profile, none.
    /// Config that arrives with a repository may only narrow this.
    #[name("llm.tools")]
    #[validate(v_tools)]
    pub LlmTools: String = String::new();

    /// Where output goes: `chat`, `below`, `review`, `register` or `echo`, or
    /// an open chat's buffer name (`*llm:3*`) to continue that chat. Empty
    /// means the invocation's own default.
    #[name("llm.destination")]
    #[validate(v_destination)]
    pub LlmDestination: String = String::new();

    /// How reasoning blocks show in a chat transcript: `closed`, `open` or
    /// `hidden`. Applies live to open chats. Empty means `closed`.
    #[name("llm.fold.reasoning")]
    #[validate(v_fold_reasoning)]
    pub LlmFoldReasoning: String = String::new();

    /// How tool-call blocks show in a chat transcript: `closed`, `open` or
    /// `hidden`. Applies live to open chats. Empty means `closed`.
    #[name("llm.fold.tools")]
    #[validate(v_fold_tools)]
    pub LlmFoldTools: String = String::new();

    /// The profile this buffer's session starts from. Switch it with
    /// `:llm-profile`, which also clears the session's other overrides; setting
    /// it here changes the base and leaves them.
    #[name("llm.profile")]
    #[validate(v_profile)]
    pub LlmProfile: String = String::new();

    /// The profile new sessions start from when none is named. Global only.
    #[name("llm.default_profile")]
    #[validate(v_profile)]
    pub LlmDefaultProfile: String = String::new();

    /// Files whose text is never sent, as globs: `.env`, `*.pem`. A glob with
    /// no `/` matches the file name anywhere; one with a `/` matches the path.
    /// Replaces the default list rather than adding to it. Global only.
    #[name("llm.exclude")]
    pub LlmExclude: ExcludeGlobs = ExcludeGlobs::default();

    /// The provider a bare `llm.model` runs on. Optional when only one provider
    /// is configured. Global only.
    #[name("llm.default_provider")]
    pub LlmDefaultProvider: String = String::new();

    /// The keys every llm chord starts with: `<leader>a` gives `<leader>aa`
    /// (ask), `<leader>ar` (rewrite) and the rest; `<C-c>.` gives `<C-c>.a`.
    /// Pressed alone and held, which-key lists what follows. Changing it
    /// rebinds at once. Global only.
    #[name("llm.prefix")]
    #[validate(v_prefix)]
    pub LlmPrefix: String = DEFAULT_PREFIX.to_string();

    /// How many times one request may run tools before it stops and asks
    /// you to send again: a guard against a model calling tools forever.
    /// Global only.
    #[name("llm.tool_rounds")]
    #[validate(v_tool_rounds)]
    pub LlmToolRounds: i64 = DEFAULT_TOOL_ROUNDS;
}

/// `llm.tool_rounds` when nothing sets it.
pub const DEFAULT_TOOL_ROUNDS: i64 = 25;

fn v_tool_rounds(v: &i64) -> Result<(), String> {
    if *v >= 1 {
        Ok(())
    } else {
        Err(format!("llm.tool_rounds must be at least 1, not {v}"))
    }
}

/// `llm.prefix` when nothing sets it.
pub const DEFAULT_PREFIX: &str = "<leader>a";

/// A prefix must be keys: it parses as a chord sequence (`<leader>` standing
/// for whatever the leader is).
#[allow(clippy::ptr_arg)]
fn v_prefix(v: &String) -> Result<(), String> {
    let keys = v
        .replace("<leader>", "<Space>")
        .replace("<Leader>", "<Space>");
    match lattice_protocol::parse_chord_sequence(&keys) {
        Ok(chords) if !chords.is_empty() => Ok(()),
        _ => Err(format!(
            "llm.prefix: `{v}` is not a key sequence (try `<leader>a` or `<C-c>.`)"
        )),
    }
}
