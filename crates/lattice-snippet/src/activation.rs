//! SN.3b — config-driven snippet activation policy.
//!
//! `SnippetMode`'s [`ActivationPolicy`](lattice_mode::ActivationPolicy)
//! is folded from two user-facing options the mode owns
//! ([[feedback_mode_owns_its_surface]]):
//!
//! - **`snippet.activation`** (`global` | `supported-languages` |
//!   `off`, default `global`) — the gate shape. A closed three-value
//!   set, so it's a typed enum ([`SnippetActivationMode`]) with an
//!   [`OptionType`](lattice_config::OptionType) impl, giving
//!   `:set snippet.activation=<Tab>` completion the same way
//!   `foldmethod` does.
//! - **`snippet.languages`** (default empty) — a comma-separated
//!   language-id allowlist, consulted only when
//!   `snippet.activation = supported-languages`. It's a plain
//!   `String` because the typed-option loader rejects TOML arrays
//!   for scalar options (`lattice_config::loader::apply_scalar`):
//!   `snippet.languages = "rust,python"` in TOML, or
//!   `:set snippet.languages=rust,python`.
//!
//! The host folds both into a shared
//! [`SnippetActivationPolicyHandle`] at boot and re-folds on the
//! `apply_option_cascade` arm for either key (`:set` live). The
//! resolver reads `SnippetMode::activation_policy()` — which loads
//! the shared cell — on each `MajorEntered`, so new buffers pick up
//! the live policy. (Already-open buffers are not retroactively
//! re-resolved in SN.3b; the policy applies on their next major
//! entry.)

use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_mode::{ActivationPolicy, ModeId};

/// Shared, swappable snippet activation policy. `SnippetMode` reads
/// it via `activation_policy()`; the host folds config into it at
/// boot and on every `snippet.activation` / `snippet.languages`
/// `:set`. Cloning the outer `Arc` shares the same cell — the
/// host's `Editor` keeps one clone to `store()` into and the mode
/// keeps another to `load()` from.
pub type SnippetActivationPolicyHandle = Arc<ArcSwap<ActivationPolicy>>;

/// `snippet.activation` — which buffers get snippets.
///
/// A closed three-value set (hence a typed enum rather than a
/// free-form string): `Global` activates on every real document
/// buffer; `SupportedLanguages` activates only for major modes in
/// the `snippet.languages` allowlist; `Off` never auto-activates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnippetActivationMode {
    /// Snippets on every document buffer (the default). The
    /// completion *source* still self-filters by language, so
    /// `Global` means "each buffer sees its own language's
    /// snippets", not "all snippets everywhere".
    #[default]
    Global,
    /// Snippets only for languages listed in `snippet.languages`.
    /// Resolves to `Majors([<lang>-mode …])`, which matches only
    /// *registered* major modes.
    SupportedLanguages,
    /// Snippets disabled — never auto-activate. Explicit
    /// `:snippet-mode` / `<C-x><C-s>` paths still work where wired.
    Off,
}

impl SnippetActivationMode {
    /// Canonical `:set` label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::SupportedLanguages => "supported-languages",
            Self::Off => "off",
        }
    }

    /// Parse a `:set snippet.activation=<value>` label.
    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s {
            "global" => Ok(Self::Global),
            "supported-languages" => Ok(Self::SupportedLanguages),
            "off" => Ok(Self::Off),
            other => Err(format!(
                "expected `global`, `supported-languages`, or `off`, got `{other}`"
            )),
        }
    }
}

impl lattice_config::OptionType for SnippetActivationMode {
    fn parse(s: &str) -> Result<Self, String> {
        Self::parse_label(s)
    }

    fn format(&self) -> String {
        self.label().to_string()
    }

    fn type_label() -> &'static str {
        "snippet-activation"
    }

    fn enumerate() -> Option<Vec<&'static str>> {
        Some(vec!["global", "supported-languages", "off"])
    }

    /// TC.1: closed — `parse` accepts these forms and nothing else, so
    /// the schema is an `enum` and `:customize` can offer a picker.
    fn enumerate_is_exhaustive() -> bool {
        true
    }
}

lattice_config::options! {
    group = lattice_config::Snippet;

    /// Which buffers get snippets. Default `global` — snippets on
    /// every document buffer, each buffer seeing only its own
    /// language's snippets (the completion source self-filters by
    /// language). `supported-languages` restricts activation to the
    /// `snippet.languages` allowlist; `off` disables auto-activation
    /// entirely.
    #[name("snippet.activation")]
    pub SnippetActivation: SnippetActivationMode = SnippetActivationMode::Global;

    /// Comma-separated language ids for which snippets activate when
    /// `snippet.activation = supported-languages`. Each id `L` maps
    /// to the major mode `L-mode`, so only *registered* majors match
    /// (today: rust / python / javascript / markdown). Ignored when
    /// `snippet.activation` is `global` or `off`. A list (not a
    /// scalar) in spirit, but stored as a string because the typed-
    /// option loader rejects TOML arrays: `snippet.languages =
    /// "rust,python"`.
    #[name("snippet.languages")]
    pub SnippetLanguages: String = String::new();
}

/// Split a `snippet.languages` value into trimmed, non-empty
/// language ids. Tolerant of stray whitespace and empty segments
/// (`"rust, ,python,"` → `["rust", "python"]`).
fn parse_languages(languages: &str) -> Vec<&str> {
    languages
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

/// Fold `snippet.activation` + `snippet.languages` into the
/// concrete [`ActivationPolicy`] the resolver consults.
///
/// - `Global` → [`ActivationPolicy::Global`] (every document buffer).
/// - `Off` → [`ActivationPolicy::Manual`] (never auto-activate).
/// - `SupportedLanguages` → [`ActivationPolicy::Majors`] of
///   `<lang>-mode` for each id in `languages`. An empty allowlist
///   yields `Majors([])`, which `ActivationPolicy::admits` treats as
///   matching no major (Manual-equivalent) — the graceful "opted
///   into language gating but named no languages" case.
pub fn fold_activation_policy(
    activation: SnippetActivationMode,
    languages: &str,
) -> ActivationPolicy {
    match activation {
        SnippetActivationMode::Global => ActivationPolicy::Global,
        SnippetActivationMode::Off => ActivationPolicy::Manual,
        SnippetActivationMode::SupportedLanguages => ActivationPolicy::Majors(
            parse_languages(languages)
                .into_iter()
                .map(|lang| ModeId::new(&format!("{lang}-mode")))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_config::OptionType;

    #[test]
    fn activation_mode_round_trips_through_parse_and_format() {
        for m in [
            SnippetActivationMode::Global,
            SnippetActivationMode::SupportedLanguages,
            SnippetActivationMode::Off,
        ] {
            assert_eq!(SnippetActivationMode::parse(&m.format()), Ok(m));
        }
    }

    #[test]
    fn activation_mode_default_is_global() {
        assert_eq!(
            SnippetActivationMode::default(),
            SnippetActivationMode::Global
        );
    }

    #[test]
    fn activation_mode_rejects_garbage_with_helpful_message() {
        let e = SnippetActivationMode::parse("sometimes").unwrap_err();
        assert!(e.contains("global"), "got `{e}`");
        assert!(e.contains("sometimes"), "got `{e}`");
    }

    #[test]
    fn activation_mode_enumerates_three_forms() {
        assert_eq!(
            SnippetActivationMode::enumerate(),
            Some(vec!["global", "supported-languages", "off"])
        );
    }

    #[test]
    fn fold_global_is_policy_global() {
        assert_eq!(
            fold_activation_policy(SnippetActivationMode::Global, "rust,python"),
            ActivationPolicy::Global,
            "global ignores the language list"
        );
    }

    #[test]
    fn fold_off_is_manual() {
        assert_eq!(
            fold_activation_policy(SnippetActivationMode::Off, "rust"),
            ActivationPolicy::Manual,
        );
    }

    #[test]
    fn fold_supported_languages_maps_to_lang_mode_majors() {
        let policy =
            fold_activation_policy(SnippetActivationMode::SupportedLanguages, "rust, python ,");
        assert_eq!(
            policy,
            ActivationPolicy::Majors(vec![ModeId::new("rust-mode"), ModeId::new("python-mode"),]),
            "trims whitespace + drops empty segments; appends `-mode`"
        );
    }

    #[test]
    fn fold_supported_languages_empty_list_matches_nothing() {
        let policy = fold_activation_policy(SnippetActivationMode::SupportedLanguages, "");
        // Majors([]) == Manual-equivalent per ActivationPolicy::admits.
        assert_eq!(policy, ActivationPolicy::Majors(vec![]));
        assert!(
            !policy.admits("rust-mode", lattice_core::BufferKind::Document),
            "empty allowlist admits no major"
        );
    }
}
