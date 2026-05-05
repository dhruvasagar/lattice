//! Static config-file loader (DESIGN.md §5.12; first slice of the
//! TOML surface called out in the design doc's "TOML covers static
//! option overrides only" line).
//!
//! Reads `lattice.toml` from the user config dir
//! (`~/.config/lattice/lattice.toml` on Linux, the XDG-equivalent on
//! macOS / Windows via [`dirs::config_dir`]) and `.lattice/config.toml`
//! from the workspace root, in that precedence order. Project beats
//! user; `:set` writes after startup beat both.
//!
//! ## Design (read-only, walk-and-set)
//!
//! The loader walks the parsed TOML table breadth-first. For each
//! sub-table, it asks: does its dotted path match a registered
//! *structural prefix* (e.g. `completion.per-language`)? If so, the
//! table is recorded verbatim in [`LoadOutcome::structural`] and
//! descent stops there -- the caller (App, plugin host) owns the
//! interpretation of structural sections. If not, descent continues
//! and any scalar leaf becomes a `parse_and_set_command("key=value")`
//! call against the supplied [`ConfigRegistry`].
//!
//! This keeps the loader policy-free for everything map-shaped:
//! plugins (Phase 7), per-language overrides (Phase 4.2.g.5 (3b)),
//! and any future structurally-typed config all flow through the
//! same `structural` bucket.
//!
//! ## Errors are warnings
//!
//! Parse failures, unknown keys, and validation rejects all
//! produce [`LoadMessage`]s the caller surfaces in its message
//! buffer. Nothing here panics or aborts startup -- a bad config
//! file is recoverable with an editor and a re-launch.
//!
//! ## What this is NOT
//!
//! - Hot reload. The loader runs once at startup; `:reload-config`
//!   is a future addition (will share the walk-and-set core).
//! - A writer. `:customize` post-1.0 is the round-trip surface;
//!   it'll move to `toml_edit` for write-preserving edits.
//! - A keymap loader. Keymaps are a separate registry; their
//!   loader will compose with this one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::registry::{ConfigError, ConfigRegistry};

/// Outcome of loading one or more TOML files. The caller drains
/// `messages` into its echo / message buffer and walks `structural`
/// to dispatch sub-tables to their owners.
#[derive(Debug, Default)]
pub struct LoadOutcome {
    pub messages: Vec<LoadMessage>,
    /// Tables whose path matched a structural prefix. Keyed by the
    /// full dotted path (e.g. `"completion.per-language.markdown"`);
    /// value is the sub-table verbatim. `BTreeMap` so iteration
    /// order is stable -- tests depend on this.
    pub structural: BTreeMap<String, toml::Table>,
}

impl LoadOutcome {
    /// Append another outcome's messages and structural sections
    /// onto this one, preserving order. Used to merge user +
    /// project file outcomes.
    pub fn extend(&mut self, other: LoadOutcome) {
        self.messages.extend(other.messages);
        for (k, v) in other.structural {
            self.structural.insert(k, v);
        }
    }
}

/// One diagnostic from the loader. Caller decides how to surface
/// it (echo, message buffer, log line); the loader stays
/// IO-agnostic.
#[derive(Debug, Clone)]
pub struct LoadMessage {
    pub level: LoadMessageLevel,
    pub source: PathBuf,
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMessageLevel {
    /// Couldn't read or parse the file; nothing was applied.
    Error,
    /// File loaded but a key was rejected (unknown / invalid /
    /// validator failure). Other keys still applied.
    Warning,
}

/// Default user config path. `Some` on platforms where
/// [`dirs::config_dir`] resolves; `None` on a few obscure
/// platforms or when `$HOME` is unset.
pub fn default_user_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("lattice").join("lattice.toml"))
}

/// Default project config path: `<workspace_root>/.lattice/config.toml`.
/// Caller supplies `workspace_root` (typically the directory the
/// editor was launched in, walked up to the first `.git` /
/// `.lattice/` marker if desired).
pub fn project_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".lattice").join("config.toml")
}

/// Load both default paths in standard precedence order: user
/// first, then project. Missing files are silent (no message).
/// `structural_prefixes` lists dotted prefixes the caller wants
/// to handle itself (e.g. `&["completion.per-language", "plugin"]`).
pub fn load_default_paths(
    registry: &ConfigRegistry,
    workspace_root: Option<&Path>,
    structural_prefixes: &[&str],
) -> LoadOutcome {
    let mut out = LoadOutcome::default();
    if let Some(user) = default_user_config_path()
        && user.exists()
    {
        out.extend(load_file(registry, &user, structural_prefixes));
    }
    if let Some(root) = workspace_root {
        let proj = project_config_path(root);
        if proj.exists() {
            out.extend(load_file(registry, &proj, structural_prefixes));
        }
    }
    out
}

/// Load a single TOML file, applying scalar leaves to `registry`
/// and bucketing structural sub-tables. The path is recorded in
/// every emitted message so the caller can surface
/// `path:reason`-style diagnostics.
pub fn load_file(
    registry: &ConfigRegistry,
    path: &Path,
    structural_prefixes: &[&str],
) -> LoadOutcome {
    let mut out = LoadOutcome::default();
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            out.messages.push(LoadMessage {
                level: LoadMessageLevel::Error,
                source: path.to_path_buf(),
                body: format!("read failed: {e}"),
            });
            return out;
        }
    };
    let table: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => {
            // toml::de::Error already includes line/col in its
            // Display; pass the body through verbatim.
            out.messages.push(LoadMessage {
                level: LoadMessageLevel::Error,
                source: path.to_path_buf(),
                body: format!("parse failed: {e}"),
            });
            return out;
        }
    };
    walk_table(registry, path, &mut out, structural_prefixes, &[], &table);
    out
}

/// Recursive walker. `prefix` is the dotted path leading to
/// `table` (empty at the root). For each child:
///
/// - **Sub-table whose path EQUALS a structural prefix** -> enter
///   namespace mode: each direct child sub-table of this node
///   becomes a structural entry (keyed by its full dotted path);
///   non-table children warn (a structural namespace can't hold
///   bare scalars in v1).
/// - **Sub-table otherwise** -> recurse with extended prefix.
/// - **Scalar leaf** -> call `parse_and_set_command` on the
///   registry, capturing any error as a warning.
///
/// The "namespace" semantics matches what the per-language
/// override + plugin-config use cases actually want:
/// `[completion.per-language.markdown]` is one entry, keyed by
/// the language id; `[plugin.rust-analyzer]` is one plugin, keyed
/// by the plugin id. Recording the *parent* (`completion.per-language`)
/// would force every caller to re-walk one extra level.
fn walk_table(
    registry: &ConfigRegistry,
    source: &Path,
    out: &mut LoadOutcome,
    structural_prefixes: &[&str],
    prefix: &[String],
    table: &toml::Table,
) {
    for (key, value) in table {
        let mut path: Vec<String> = prefix.to_vec();
        path.push(key.clone());
        let dotted = path.join(".");
        match value {
            toml::Value::Table(sub) => {
                if structural_prefixes.iter().any(|p| *p == dotted) {
                    record_namespace_children(source, out, &dotted, sub);
                } else {
                    walk_table(registry, source, out, structural_prefixes, &path, sub);
                }
            }
            scalar => {
                apply_scalar(registry, source, out, &dotted, scalar);
            }
        }
    }
}

/// Treat `table` as a namespace -- each direct child becomes a
/// structural entry keyed by `<namespace_path>.<child>`. Non-
/// table children warn; structural namespaces in v1 hold sub-
/// tables only.
fn record_namespace_children(
    source: &Path,
    out: &mut LoadOutcome,
    namespace_dotted: &str,
    table: &toml::Table,
) {
    for (key, value) in table {
        let dotted = format!("{namespace_dotted}.{key}");
        match value {
            toml::Value::Table(sub) => {
                out.structural.insert(dotted, sub.clone());
            }
            _ => {
                out.messages.push(LoadMessage {
                    level: LoadMessageLevel::Warning,
                    source: source.to_path_buf(),
                    body: format!(
                        "`{dotted}`: structural namespace `{namespace_dotted}` \
                         expected a sub-table; got a scalar",
                    ),
                });
            }
        }
    }
}

/// Apply a scalar leaf via `parse_and_set_command`. Captures
/// every failure mode (unknown option, validation reject, parse
/// error) as a warning -- the loader never aborts on a single
/// bad key.
fn apply_scalar(
    registry: &ConfigRegistry,
    source: &Path,
    out: &mut LoadOutcome,
    dotted: &str,
    value: &toml::Value,
) {
    let formatted = match format_scalar(value) {
        Some(s) => s,
        None => {
            // Arrays at scalar position aren't a known option-
            // value shape today; warn so the user notices their
            // config didn't apply.
            out.messages.push(LoadMessage {
                level: LoadMessageLevel::Warning,
                source: source.to_path_buf(),
                body: format!(
                    "`{dotted}`: list / inline-table values aren't \
                     applicable to scalar options; move it under a \
                     structural section",
                ),
            });
            return;
        }
    };
    let assign = format!("{dotted}={formatted}");
    if let Err(err) = registry.parse_and_set_command(&assign) {
        let body = match err {
            ConfigError::UnknownOption(name) => format!("unknown option `{name}`"),
            ConfigError::Validation(msg) => format!("`{dotted}`: {msg}"),
            ConfigError::Parse(msg) => format!("`{dotted}`: parse error: {msg}"),
            other => format!("`{dotted}`: {other}"),
        };
        out.messages.push(LoadMessage {
            level: LoadMessageLevel::Warning,
            source: source.to_path_buf(),
            body,
        });
    }
}

/// Render a scalar TOML value as a string the registry's
/// per-option parser will accept. `None` for shapes (arrays,
/// inline tables) that aren't a scalar option value.
fn format_scalar(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(if *b { "true".into() } else { "false".into() }),
        toml::Value::Datetime(d) => Some(d.to_string()),
        toml::Value::Array(_) | toml::Value::Table(_) => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::option::Option as ConfigOption;

    fn registry_with_options() -> ConfigRegistry {
        let r = ConfigRegistry::new();
        r.register(ConfigOption::<bool>::new(
            "number",
            true,
            "Show absolute line numbers.",
        ));
        r.register(ConfigOption::<i64>::new(
            "tabstop",
            8,
            "Tab width.",
        ));
        r.register(
            ConfigOption::<i64>::builder(
                "scrolloff",
                0,
                "Scroll-off margin.",
            )
            .validate(|i| {
                if (0..=64).contains(i) {
                    Ok(())
                } else {
                    Err(format!("scrolloff out of range [0, 64]: {i}"))
                }
            })
            .build(),
        );
        r.register(ConfigOption::<String>::new(
            "ui.separator",
            "│".into(),
            "Pane separator glyph.",
        ));
        r
    }

    fn write_temp(name: &str, contents: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lattice-loader-test-{}-{}",
            std::process::id(),
            name,
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lattice.toml");
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn missing_file_returns_empty_outcome() {
        let r = registry_with_options();
        let path = std::env::temp_dir().join("lattice-loader-no-such-file.toml");
        let _ = std::fs::remove_file(&path);
        let out = load_file(&r, &path, &[]);
        // Read failure -> one error message, no panics.
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].level, LoadMessageLevel::Error);
        assert!(out.structural.is_empty());
    }

    #[test]
    fn parses_well_formed_toml_and_writes_scalars() {
        let r = registry_with_options();
        let p = write_temp(
            "well-formed",
            "number = false\ntabstop = 4\n[ui]\nseparator = \"|\"\n",
        );
        let out = load_file(&r, &p, &[]);
        assert!(out.messages.is_empty(), "messages: {:?}", out.messages);
        let opt_number = r.lookup("number").unwrap();
        assert_eq!(opt_number.get_formatted(), "false");
        let opt_tabstop = r.lookup("tabstop").unwrap();
        assert_eq!(opt_tabstop.get_formatted(), "4");
        let opt_sep = r.lookup("ui.separator").unwrap();
        assert_eq!(opt_sep.get_formatted(), "|");
    }

    #[test]
    fn parse_error_emits_one_error_and_stops() {
        let r = registry_with_options();
        let p = write_temp("malformed", "number = ?broken?\n");
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].level, LoadMessageLevel::Error);
        assert!(out.messages[0].body.contains("parse failed"));
        // Default unchanged: no scalars applied past the parse error.
        assert_eq!(r.lookup("number").unwrap().get_formatted(), "true");
    }

    #[test]
    fn unknown_key_emits_warning_other_keys_still_apply() {
        let r = registry_with_options();
        let p = write_temp(
            "unknown",
            "number = false\nbogus.key = 42\ntabstop = 2\n",
        );
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].level, LoadMessageLevel::Warning);
        assert!(out.messages[0].body.contains("unknown option"));
        assert_eq!(r.lookup("number").unwrap().get_formatted(), "false");
        assert_eq!(r.lookup("tabstop").unwrap().get_formatted(), "2");
    }

    #[test]
    fn validation_failure_warns_with_dotted_path_and_skips() {
        let r = registry_with_options();
        let p = write_temp("invalid", "scrolloff = 999\n");
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].level, LoadMessageLevel::Warning);
        assert!(out.messages[0].body.contains("scrolloff"));
        // Default preserved.
        assert_eq!(r.lookup("scrolloff").unwrap().get_formatted(), "0");
    }

    #[test]
    fn list_at_scalar_position_warns_without_panic() {
        let r = registry_with_options();
        let p = write_temp("list", "number = [1, 2, 3]\n");
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].level, LoadMessageLevel::Warning);
        assert!(out.messages[0].body.contains("list"));
    }

    #[test]
    fn structural_prefix_section_is_buckted_not_walked() {
        // `completion.per-language` is structural; loader records
        // the markdown sub-table verbatim and does NOT try to
        // call parse_and_set on its leaves (which would warn
        // "unknown option `completion.per-language.markdown.sources`").
        let r = registry_with_options();
        let p = write_temp(
            "structural",
            "[completion.per-language.markdown]\n\
             sources = [\"snippet\", \"buffer-words\"]\n\
             auto_trigger = false\n",
        );
        let out = load_file(&r, &p, &["completion.per-language"]);
        assert!(out.messages.is_empty(), "messages: {:?}", out.messages);
        let md = out
            .structural
            .get("completion.per-language.markdown")
            .expect("markdown sub-table recorded");
        assert!(md.contains_key("sources"));
        assert!(md.contains_key("auto_trigger"));
    }

    #[test]
    fn structural_namespace_with_scalar_child_warns() {
        // Namespaces hold sub-tables in v1; a stray scalar at
        // namespace level (e.g. `completion.per-language.foo = 1`)
        // is a warning, not silent acceptance.
        let r = registry_with_options();
        let p = write_temp(
            "ns-scalar",
            "[completion.per-language]\nbroken = 1\n",
        );
        let out = load_file(&r, &p, &["completion.per-language"]);
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].level, LoadMessageLevel::Warning);
        assert!(out.messages[0].body.contains("structural namespace"));
        assert!(out.structural.is_empty());
    }

    #[test]
    fn structural_namespace_records_each_child_separately() {
        // Two sibling languages -> two structural entries.
        let r = registry_with_options();
        let p = write_temp(
            "two-langs",
            "[completion.per-language.markdown]\n\
             auto_trigger = false\n\
             [completion.per-language.rust]\n\
             auto_trigger = true\n",
        );
        let out = load_file(&r, &p, &["completion.per-language"]);
        assert!(out.messages.is_empty(), "messages: {:?}", out.messages);
        assert_eq!(out.structural.len(), 2);
        assert!(out.structural.contains_key("completion.per-language.markdown"));
        assert!(out.structural.contains_key("completion.per-language.rust"));
    }

    #[test]
    fn structural_section_alongside_scalar_keys_in_same_parent() {
        // [completion] has `auto_insert_single = true` AND a
        // nested `[completion.per-language.markdown]`. Loader
        // applies the scalar AND records the structural section
        // independently.
        let r = registry_with_options();
        r.register(ConfigOption::<bool>::new(
            "completion.auto_insert_single",
            true,
            "",
        ));
        let p = write_temp(
            "mixed",
            "[completion]\n\
             auto_insert_single = false\n\
             [completion.per-language.markdown]\n\
             auto_trigger = false\n",
        );
        let out = load_file(&r, &p, &["completion.per-language"]);
        assert!(out.messages.is_empty(), "messages: {:?}", out.messages);
        assert_eq!(
            r.lookup("completion.auto_insert_single").unwrap().get_formatted(),
            "false",
        );
        assert!(out.structural.contains_key("completion.per-language.markdown"));
    }

    #[test]
    fn extend_merges_messages_and_structural_in_order() {
        let mut a = LoadOutcome::default();
        a.messages.push(LoadMessage {
            level: LoadMessageLevel::Warning,
            source: PathBuf::from("/a"),
            body: "first".into(),
        });
        a.structural.insert("k1".into(), toml::Table::new());
        let mut b = LoadOutcome::default();
        b.messages.push(LoadMessage {
            level: LoadMessageLevel::Warning,
            source: PathBuf::from("/b"),
            body: "second".into(),
        });
        b.structural.insert("k2".into(), toml::Table::new());
        a.extend(b);
        assert_eq!(a.messages.len(), 2);
        assert_eq!(a.messages[0].body, "first");
        assert_eq!(a.messages[1].body, "second");
        assert_eq!(a.structural.len(), 2);
    }

    #[test]
    fn project_path_helper_lands_at_dot_lattice_config_toml() {
        let p = project_config_path(Path::new("/workspace/foo"));
        assert_eq!(p, PathBuf::from("/workspace/foo/.lattice/config.toml"));
    }
}
