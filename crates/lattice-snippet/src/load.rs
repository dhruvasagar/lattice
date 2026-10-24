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

/// What [`load_dir`] found in one directory of packs.
#[derive(Debug, Default)]
pub struct DirLoad {
    /// Each snippet with the language pack it goes in (`"*"` for every
    /// language).
    pub snippets: Vec<(String, Snippet)>,
    /// Problems, each naming its file: an unreadable file, a file that is
    /// not a pack, an entry skipped.
    pub errors: Vec<String>,
}

/// Load every snippet pack in `dir`, in whichever layout it uses
/// (snippet-integrity SI.11):
///
/// - **friendly-snippets / a VS Code extension:** a `package.json` whose
///   `contributes.snippets` lists `{ "language": id | [ids], "path": … }`.
///   Each listed file is loaded for its languages, wherever it lives
///   under `dir`.
/// - **a snippets folder:** `<language>.json` per language (`_global.json`
///   for every language), and `*.code-snippets` files whose entries name
///   their languages in `scope` (`"javascript,typescript"`; none means
///   every language) — VS Code's user-snippet layout.
///
/// A missing directory is not an error (the default one is absent until
/// the user adds a pack).
pub fn load_dir(dir: &std::path::Path) -> DirLoad {
    let mut out = DirLoad::default();
    let manifest = dir.join("package.json");
    if manifest.is_file() {
        load_manifest(dir, &manifest, &mut out);
        return out;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            out.errors.push(format!("{}: {e}", dir.display()));
            return out;
        }
    };
    let mut paths: Vec<std::path::PathBuf> = entries.flatten().map(|e| e.path()).collect();
    // Load in a stable order: a later pack overrides an earlier one on a
    // shared prefix, so the order must not depend on the filesystem.
    paths.sort();
    for path in paths {
        match path.extension().and_then(|e| e.to_str()) {
            Some("json") => {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default();
                let language = if stem == "_global" { "*" } else { stem };
                load_file(&path, &[language.to_string()], &mut out);
            }
            Some("code-snippets") => load_file(&path, &[], &mut out),
            _ => {}
        }
    }
    out
}

/// `package.json`'s `contributes.snippets`.
fn load_manifest(dir: &std::path::Path, manifest: &std::path::Path, out: &mut DirLoad) {
    #[derive(Deserialize)]
    struct Manifest {
        #[serde(default)]
        contributes: Contributes,
    }
    #[derive(Deserialize, Default)]
    struct Contributes {
        #[serde(default)]
        snippets: Vec<Contribution>,
    }
    #[derive(Deserialize)]
    struct Contribution {
        language: StringOrArray,
        path: String,
    }
    let parsed = std::fs::read_to_string(manifest)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str::<Manifest>(&text).map_err(|e| e.to_string()));
    let manifest_json = match parsed {
        Ok(m) => m,
        Err(e) => {
            out.errors.push(format!("{}: {e}", manifest.display()));
            return;
        }
    };
    for contribution in manifest_json.contributes.snippets {
        let languages = match contribution.language {
            StringOrArray::String(s) => vec![s],
            StringOrArray::Array(v) => v,
        };
        load_file(&dir.join(&contribution.path), &languages, out);
    }
}

/// One pack file, into `languages` — or, when that is empty (a
/// `.code-snippets` file), into the languages each entry's `scope` names.
fn load_file(path: &std::path::Path, languages: &[String], out: &mut DirLoad) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            out.errors.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    let pack = match load_pack_from_str(&text) {
        Ok(pack) => pack,
        Err(e) => {
            out.errors.push(format!("{}: {e}", path.display()));
            return;
        }
    };
    for skipped in pack.skipped {
        out.errors.push(format!("{}: {skipped}", path.display()));
    }
    for snippet in pack.snippets {
        let scoped: Vec<String> = if languages.is_empty() {
            let named: Vec<String> = snippet
                .scope
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect();
            if named.is_empty() {
                vec!["*".into()]
            } else {
                named
            }
        } else {
            languages.to_vec()
        };
        for language in scoped {
            out.snippets.push((language, snippet.clone()));
        }
    }
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

    fn write(dir: &std::path::Path, name: &str, text: &str) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn found(load: &DirLoad) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = load
            .snippets
            .iter()
            .map(|(l, s)| (l.clone(), s.name.clone()))
            .collect();
        v.sort();
        v
    }

    /// SI.11: a snippets folder — per-language files, `_global`, and a
    /// `.code-snippets` file whose entries name their languages.
    #[test]
    fn a_snippets_folder_loads_each_layout() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "rust.json",
            r#"{ "fn": { "prefix": "fn", "body": "fn $1" } }"#,
        );
        write(
            dir.path(),
            "_global.json",
            r#"{ "todo": { "prefix": "todo", "body": "TODO" } }"#,
        );
        write(
            dir.path(),
            "web.code-snippets",
            r#"{
                "log": { "prefix": "log", "body": "console.log($1)", "scope": "javascript,typescript" },
                "any": { "prefix": "any", "body": "x" }
            }"#,
        );
        write(dir.path(), "notes.txt", "ignored");
        let load = load_dir(dir.path());
        assert!(load.errors.is_empty(), "{:?}", load.errors);
        assert_eq!(
            found(&load),
            vec![
                ("*".into(), "any".into()),
                ("*".into(), "todo".into()),
                ("javascript".into(), "log".into()),
                ("rust".into(), "fn".into()),
                ("typescript".into(), "log".into()),
            ]
        );
    }

    /// SI.11: friendly-snippets' layout — `package.json` maps files, in
    /// subdirectories, to one or several languages.
    #[test]
    fn a_package_json_maps_files_to_languages() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "package.json",
            r#"{ "contributes": { "snippets": [
                { "language": "rust", "path": "./snippets/rust/rust.json" },
                { "language": ["javascript", "typescriptreact"], "path": "./snippets/javascript/javascript.json" }
            ] } }"#,
        );
        write(
            dir.path(),
            "snippets/rust/rust.json",
            r#"{ "impl": { "prefix": "impl", "body": "impl $1 {}" } }"#,
        );
        write(
            dir.path(),
            "snippets/javascript/javascript.json",
            r#"{ "fn": { "prefix": "fn", "body": "function $1() {}" } }"#,
        );
        let load = load_dir(dir.path());
        assert!(load.errors.is_empty(), "{:?}", load.errors);
        assert_eq!(
            found(&load),
            vec![
                ("javascript".into(), "fn".into()),
                ("rust".into(), "impl".into()),
                ("typescriptreact".into(), "fn".into()),
            ]
        );
    }

    #[test]
    fn a_missing_directory_is_empty_not_an_error() {
        let load = load_dir(std::path::Path::new("/nonexistent/lattice/snippets"));
        assert!(load.snippets.is_empty() && load.errors.is_empty());
    }

    #[test]
    fn problems_name_their_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "rust.json", "[not a pack]");
        let load = load_dir(dir.path());
        assert_eq!(load.errors.len(), 1);
        assert!(load.errors[0].contains("rust.json"), "{:?}", load.errors);
    }
}
