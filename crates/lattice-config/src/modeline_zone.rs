//! [`ModelineZone`] — the value type for the `ui.modeline.{left,
//! center,right}` typed options (slice ML.5).
//!
//! The configurable modeline (`docs/dev/architecture/modeline.md` §11)
//! lets the user assign element ids to zones, in order, Helix-style:
//!
//! ```toml
//! [ui.modeline]
//! left  = ["core.mode", "core.path"]
//! right = ["lsp", "core.position", "core.lang"]
//! ```
//!
//! Each zone option holds a [`ModelineZone`]: either [`ModelineZone::Auto`]
//! (the default — fall back to the producer-registered descriptor
//! placement, so a newly-registered mode element auto-appears without
//! the user editing config) or [`ModelineZone::Ids`] (an explicit,
//! ordered list; an empty list is an explicitly-cleared zone).
//!
//! ## Why a typed list and not a `String`
//!
//! This is the **first list-valued option** in the tree. It is a real
//! [`OptionType`] (round-trips `:set`/`:customize`/TOML) rather than a
//! delimited `String`, so the TOML surface keeps the Helix-shaped array
//! the design specifies and any future list-shaped option reuses the
//! same loader array-support path ([`OptionType::accepts_list`]). See
//! the slice plan (ML.5) for the rejected `String`-per-zone alternative.
//!
//! ## Delimiters
//!
//! `format` joins ids with `,` (comma) — chosen so `:set
//! ui.modeline.left=core.mode,core.path` works through the cmdline
//! `:set` tokenizer, which splits args on whitespace. `parse` is
//! lenient: it splits on commas **and** whitespace, so a TOML array
//! joined either way round-trips. Element ids are namespaced with dots
//! (`core.mode`, `<plugin>.<name>`) and never contain commas or spaces,
//! so neither delimiter is ambiguous.

use std::sync::Arc;

use crate::option_type::OptionType;

/// Reserved keyword: a zone value of exactly `auto` (case-insensitive)
/// means [`ModelineZone::Auto`]. No built-in or mode element id is
/// `auto`, so this never shadows a real id.
const AUTO_KEYWORD: &str = "auto";

/// A modeline zone's configured layout: descriptor-driven ([`Auto`]) or
/// an explicit ordered element-id list ([`Ids`]).
///
/// [`Auto`]: ModelineZone::Auto
/// [`Ids`]: ModelineZone::Ids
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ModelineZone {
    /// Use the producer-registered descriptor placement for this zone
    /// (the built-in default). The default variant: with no config a
    /// newly-registered mode element appears in its descriptor's zone
    /// automatically (preserves extensibility, paramount #2).
    #[default]
    Auto,
    /// An explicit, ordered list of element ids. Unknown / unregistered
    /// ids are skipped + logged by the host layout resolver
    /// (`lattice_host::modeline`); an empty list is an explicitly-blank
    /// zone (distinct from `Auto`).
    Ids(Vec<Arc<str>>),
}

impl ModelineZone {
    /// The configured ids, or `None` for [`Self::Auto`].
    pub fn ids(&self) -> std::option::Option<&[Arc<str>]> {
        match self {
            ModelineZone::Auto => None,
            ModelineZone::Ids(v) => Some(v),
        }
    }

    /// Whether this is the descriptor-driven default.
    pub fn is_auto(&self) -> bool {
        matches!(self, ModelineZone::Auto)
    }
}

impl OptionType for ModelineZone {
    fn parse(s: &str) -> Result<Self, String> {
        let trimmed = s.trim();
        // A bare `auto` (case-insensitive) is the descriptor-driven
        // default; everything else is an explicit list (possibly empty).
        if trimmed.eq_ignore_ascii_case(AUTO_KEYWORD) {
            return Ok(ModelineZone::Auto);
        }
        let ids: Vec<Arc<str>> = trimmed
            .split([',', ' ', '\t'])
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(Arc::from)
            .collect();
        Ok(ModelineZone::Ids(ids))
    }

    fn format(&self) -> String {
        match self {
            ModelineZone::Auto => AUTO_KEYWORD.to_string(),
            ModelineZone::Ids(ids) => ids
                .iter()
                .map(|s| s.as_ref())
                .collect::<Vec<_>>()
                .join(","),
        }
    }

    fn type_label() -> &'static str {
        "modeline-zone"
    }

    /// The id space is open-ended (built-ins + any mode/plugin element),
    /// so there is no fixed completion set — `auto` is surfaced as the
    /// one keyword form.
    fn enumerate() -> std::option::Option<Vec<&'static str>> {
        Some(vec![AUTO_KEYWORD])
    }

    /// ML.5: this is the list-shaped option the loader joins TOML arrays
    /// into. See [`OptionType::accepts_list`] + `loader::apply_array`.
    fn accepts_list() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn ids(v: &[&str]) -> ModelineZone {
        ModelineZone::Ids(v.iter().map(|s| Arc::from(*s)).collect())
    }

    #[test]
    fn parse_auto_keyword_case_insensitive() {
        assert_eq!(ModelineZone::parse("auto").unwrap(), ModelineZone::Auto);
        assert_eq!(ModelineZone::parse("  AUTO  ").unwrap(), ModelineZone::Auto);
    }

    #[test]
    fn parse_comma_list() {
        assert_eq!(
            ModelineZone::parse("core.mode,core.path").unwrap(),
            ids(&["core.mode", "core.path"])
        );
    }

    #[test]
    fn parse_whitespace_list_is_lenient() {
        // A TOML array joined with spaces (or mixed) round-trips too.
        assert_eq!(
            ModelineZone::parse("core.mode core.path").unwrap(),
            ids(&["core.mode", "core.path"])
        );
        assert_eq!(
            ModelineZone::parse(" core.mode , core.path ").unwrap(),
            ids(&["core.mode", "core.path"])
        );
    }

    #[test]
    fn parse_empty_is_explicit_empty_not_auto() {
        // Distinct from Auto: an explicitly-blank zone renders nothing.
        assert_eq!(ModelineZone::parse("").unwrap(), ModelineZone::Ids(vec![]));
        assert_eq!(ModelineZone::parse("   ").unwrap(), ModelineZone::Ids(vec![]));
    }

    #[test]
    fn format_round_trips() {
        for z in [
            ModelineZone::Auto,
            ModelineZone::Ids(vec![]),
            ids(&["core.mode", "core.path"]),
            ids(&["lsp", "core.position", "core.lang"]),
        ] {
            assert_eq!(ModelineZone::parse(&z.format()).unwrap(), z);
        }
    }

    #[test]
    fn format_auto_is_keyword_ids_are_comma_joined() {
        assert_eq!(ModelineZone::Auto.format(), "auto");
        assert_eq!(ids(&["core.mode", "core.path"]).format(), "core.mode,core.path");
        assert_eq!(ModelineZone::Ids(vec![]).format(), "");
    }

    #[test]
    fn accepts_list_marks_the_loader_array_path() {
        assert!(ModelineZone::accepts_list());
        // Scalar types stay false (default).
        assert!(!bool::accepts_list());
        assert!(!String::accepts_list());
    }

    #[test]
    fn ids_and_is_auto_accessors() {
        assert!(ModelineZone::Auto.is_auto());
        assert!(ModelineZone::Auto.ids().is_none());
        let z = ids(&["a", "b"]);
        assert!(!z.is_auto());
        assert_eq!(z.ids().unwrap().len(), 2);
    }
}
