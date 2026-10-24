//! TextMate JSON loader -- the format VS Code uses and
//! `friendly-snippets` ships in. Reading a friendly-snippets
//! pack is a matter of pointing at its JSON files; no
//! conversion or shim code.
//!
//! Format:
//!
//! ```json
//! {
//!     "for loop": {
//!         "prefix": "for",
//!         "body": [
//!             "for ${1:i} in ${2:iter} {",
//!             "\t$0",
//!             "}"
//!         ],
//!         "description": "for-in loop"
//!     },
//!     "impl Display": {
//!         "prefix": ["impl_display", "displ"],
//!         "body": ["impl Display for ${1:Ty} { ... }"],
//!         "description": "implement Display"
//!     }
//! }
//! ```
//!
//! - `prefix`: string OR string array.
//! - `body`: string OR string array (joined with `\n`).
//! - `description`: optional string.
//! - `scope`: optional string (comma-separated source scopes).
//!
//! Top-level keys are snippet names (free-form). Unknown
//! fields are ignored -- forward-compatible with VS Code's
//! occasional extensions.

use serde::Deserialize;

use crate::parse;
use crate::registry::Snippet;

/// Snippet load error. Wraps JSON / parse failures so callers
/// can surface a clear "snippet pack X failed" message.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("JSON parse: {0}")]
    Json(#[from] serde_json::Error),
    #[error("snippet body parse for `{name}`: {error}")]
    Body {
        name: String,
        #[source]
        error: parse::ParseError,
    },
    /// One entry of a pack is malformed (no `prefix`, a `body` that is not
    /// a string or a list of strings …). The rest of the pack still loads.
    #[error("snippet `{name}`: {error}")]
    Entry {
        name: String,
        #[source]
        error: serde_json::Error,
    },
}

/// A loaded pack: the snippets that parsed, and the entries that did not.
#[derive(Debug, Default)]
pub struct PackLoad {
    pub snippets: Vec<Snippet>,
    /// Entries skipped, each with why. One malformed entry used to fail the
    /// whole file (SI.8); friendly-snippets packs run to hundreds of
    /// entries.
    pub skipped: Vec<LoadError>,
}

/// Parse a friendly-snippets-style JSON pack. Each entry
/// becomes a [`Snippet`]; the body is parsed eagerly so the
/// per-keystroke `gen:snippet` source doesn't pay re-parse
/// cost. An entry that does not decode is skipped and reported in
/// [`PackLoad::skipped`]; only a file that is not a JSON object of
/// entries fails as a whole.
pub fn load_pack(json: &serde_json::Value) -> Result<PackLoad, LoadError> {
    let raw: std::collections::BTreeMap<String, serde_json::Value> =
        serde_json::from_value(json.clone())?;
    let mut out = PackLoad::default();
    for (name, value) in raw {
        let r: RawSnippet = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(error) => {
                out.skipped.push(LoadError::Entry { name, error });
                continue;
            }
        };
        let prefixes = match r.prefix {
            StringOrArray::String(s) => vec![s],
            StringOrArray::Array(v) => v,
        };
        let body_str = match r.body {
            StringOrArray::String(s) => s,
            StringOrArray::Array(v) => v.join("\n"),
        };
        let body = match parse::parse(&body_str) {
            Ok(body) => body,
            Err(error) => {
                out.skipped.push(LoadError::Body { name, error });
                continue;
            }
        };
        out.snippets.push(Snippet {
            name,
            prefixes,
            body,
            description: r.description,
            scope: r.scope.unwrap_or_default(),
        });
    }
    Ok(out)
}

/// Convenience: parse a JSON string directly. Most loader
/// call sites read a file then route through here.
pub fn load_pack_from_str(json: &str) -> Result<PackLoad, LoadError> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    load_pack(&v)
}

#[derive(Debug, Deserialize)]
struct RawSnippet {
    prefix: StringOrArray,
    body: StringOrArray,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum StringOrArray {
    String(String),
    Array(Vec<String>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_string_prefix_and_body() {
        let json = r#"{
            "for loop": {
                "prefix": "for",
                "body": "for ${1:i} in ${2:iter} {}",
                "description": "for-in loop"
            }
        }"#;
        let snips = load_pack_from_str(json).unwrap().snippets;
        assert_eq!(snips.len(), 1);
        let s = &snips[0];
        assert_eq!(s.name, "for loop");
        assert_eq!(s.prefixes, vec!["for"]);
        assert_eq!(s.description.as_deref(), Some("for-in loop"));
    }

    #[test]
    fn parses_array_prefix_and_body() {
        let json = r#"{
            "impl Display": {
                "prefix": ["impl_display", "displ"],
                "body": [
                    "impl Display for ${1:Ty} {",
                    "\tfn fmt(&self, f: &mut Formatter) -> Result {",
                    "\t\twrite!(f, \"${2}\")",
                    "\t}",
                    "}"
                ]
            }
        }"#;
        let snips = load_pack_from_str(json).unwrap().snippets;
        let s = &snips[0];
        assert_eq!(s.prefixes, vec!["impl_display", "displ"]);
        // Body parses without errors -- placeholders / tabstops
        // present.
        assert!(!s.body.is_empty());
    }

    #[test]
    fn unknown_top_level_fields_are_ignored() {
        let json = r#"{
            "x": {
                "prefix": "x",
                "body": "x",
                "future_field": 42
            }
        }"#;
        let snips = load_pack_from_str(json).unwrap().snippets;
        assert_eq!(snips.len(), 1);
    }

    #[test]
    fn a_missing_field_skips_only_that_entry() {
        let json = r#"{
            "broken": { "prefix": "br" },
            "fine": { "prefix": "ok", "body": "ok $1" }
        }"#;
        let pack = load_pack_from_str(json).unwrap();
        assert_eq!(pack.snippets.len(), 1, "the good entry loads");
        assert_eq!(pack.snippets[0].name, "fine");
        assert!(matches!(
            &pack.skipped[..],
            [LoadError::Entry { name, .. }] if name == "broken"
        ));
    }

    #[test]
    fn a_file_that_is_not_an_object_of_entries_fails_whole() {
        assert!(matches!(
            load_pack_from_str("[1, 2]"),
            Err(LoadError::Json(_))
        ));
    }

    #[test]
    fn scope_field_is_carried() {
        let json = r#"{
            "for": {
                "prefix": "for",
                "body": "for $1",
                "scope": "source.rust,source.markdown.injection.rust"
            }
        }"#;
        let snips = load_pack_from_str(json).unwrap().snippets;
        assert_eq!(snips[0].scope, "source.rust,source.markdown.injection.rust");
    }

    #[test]
    fn empty_pack_loads_to_empty_vec() {
        let snips = load_pack_from_str("{}").unwrap().snippets;
        assert!(snips.is_empty());
    }

    #[test]
    fn friendly_snippets_for_loop_shape_round_trips() {
        // Slice of a real friendly-snippets entry shape.
        let json = r#"{
            "For Range Loop": {
                "prefix": ["for", "for-range"],
                "body": [
                    "for ${1:i} in ${2:iter} {",
                    "\t$0",
                    "}"
                ],
                "description": "Iterate over a range using a for loop."
            }
        }"#;
        let snips = load_pack_from_str(json).unwrap().snippets;
        let s = &snips[0];
        assert_eq!(s.prefixes.len(), 2);
        // Render to verify parser + render round-trip.
        let r = crate::render::render(&s.body, &crate::variables::VariableContext::default());
        assert_eq!(r.text, "for i in iter {\n\t\n}");
        assert_eq!(r.tabstops.len(), 3);
    }
}
