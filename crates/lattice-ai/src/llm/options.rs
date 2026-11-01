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

    /// Where output goes: `chat`, `below`, `review`, `register` or `echo`.
    /// Empty means the invocation's own default.
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

    /// The provider a bare `llm.model` runs on. Optional when only one provider
    /// is configured. Global only.
    #[name("llm.default_provider")]
    pub LlmDefaultProvider: String = String::new();
}
