//! Static config-file loader (DESIGN.md §5.12; first slice of the
//! TOML surface called out in the design doc's "TOML covers static
//! option overrides only" line).
//!
//! Reads `lattice.toml` from the XDG config home
//! (`~/.config/lattice/lattice.toml` on all Unix incl. macOS, honouring
//! `$XDG_CONFIG_HOME`; see [`config_home`]) and `.lattice/config.toml`
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
// TC.2: a composite-schema option takes its TOML value whole, as a tree.
use crate::schema::{ConfigValue, SchemaError};

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
    /// Merged TOML tree of every loaded file -- preserves all
    /// keys (registered + structural + unknown) verbatim. The
    /// `extend` method deep-merges with later files overriding
    /// earlier ones at scalar leaves; nested tables merge
    /// recursively rather than being clobbered. Consumers like
    /// `workspace/configuration` (Phase 4.1 follow-up) walk this
    /// by dotted-path to surface server-namespaced config the
    /// typed registry doesn't know about.
    pub raw_tree: toml::Table,
}

impl LoadOutcome {
    /// Append another outcome's messages and structural sections
    /// onto this one, preserving order, AND deep-merge the
    /// raw-tree (later overrides earlier at scalar level; nested
    /// tables merge recursively).
    pub fn extend(&mut self, other: LoadOutcome) {
        self.messages.extend(other.messages);
        for (k, v) in other.structural {
            self.structural.insert(k, v);
        }
        deep_merge_table(&mut self.raw_tree, other.raw_tree);
    }
}

/// Deep-merge `incoming` into `base`. For each key:
/// - Both sides are tables -> recurse.
/// - Else -> incoming wins (later loaded file overrides).
///
/// Used by `LoadOutcome::extend` so a project config's
/// `[lsp.rust-analyzer.cargo]` doesn't clobber the user's
/// `[lsp.rust-analyzer.checkOnSave]` -- both survive in the
/// merged tree.
pub(crate) fn deep_merge_table(base: &mut toml::Table, incoming: toml::Table) {
    for (key, incoming_val) in incoming {
        match (base.get_mut(&key), incoming_val) {
            (Some(toml::Value::Table(base_sub)), toml::Value::Table(in_sub)) => {
                deep_merge_table(base_sub, in_sub);
            }
            (_, val) => {
                base.insert(key, val);
            }
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

/// Walk a TOML table by a dotted path, returning the value at
/// the leaf or `None` if any segment is missing or steps through
/// a non-table. Used by `workspace/configuration` to look up
/// server-namespaced keys (e.g. `"rust-analyzer.cargo.features"`
/// walks `tree["rust-analyzer"]["cargo"]["features"]`).
pub fn lookup_dotted_path<'a>(tree: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut node: &toml::Table = tree;
    let segments: Vec<&str> = path.split('.').collect();
    let last_idx = segments.len().checked_sub(1)?;
    for (i, seg) in segments.iter().enumerate() {
        let value = node.get(*seg)?;
        if i == last_idx {
            return Some(value);
        }
        node = value.as_table()?;
    }
    None
}

/// The XDG config base directory for lattice's config root.
///
/// Resolves the XDG Base Directory config home so lattice's config lives
/// under `~/.config/lattice/` on **every** Unix — macOS included — instead of
/// the platform-native `~/Library/Application Support` that `dirs::config_dir`
/// returns there. This matches the convention every developer CLI / editor
/// uses (Helix, Neovim, Zed's CLI, alacritty, starship, …): on macOS they read
/// `~/.config/<app>`, not `~/Library/Application Support/<app>`.
///
/// Precedence (per the XDG Base Directory spec):
/// 1. `$XDG_CONFIG_HOME` when set to an **absolute** path — honoured on every
///    platform so an explicit override always wins (a set-but-relative or
///    empty value is spec-invalid and ignored).
/// 2. `~/.config` on Unix (via `$HOME`).
/// 3. `dirs::config_dir()` elsewhere (Windows → `%APPDATA%`).
///
/// `None` only when neither the override nor the home directory resolves.
pub fn config_home() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
    #[cfg(unix)]
    let fallback = dirs::home_dir().map(|h| h.join(".config"));
    #[cfg(not(unix))]
    let fallback = dirs::config_dir();
    resolve_config_home(xdg, fallback)
}

/// Pure resolver behind [`config_home`], split out so the precedence logic is
/// unit-testable without mutating process-global environment variables.
fn resolve_config_home(
    xdg_config_home: Option<PathBuf>,
    fallback: Option<PathBuf>,
) -> Option<PathBuf> {
    if let Some(xdg) = xdg_config_home
        && xdg.is_absolute()
    {
        return Some(xdg);
    }
    fallback
}

/// Default user config path: `<config_home>/lattice/lattice.toml`, i.e.
/// `~/.config/lattice/lattice.toml` (honouring `$XDG_CONFIG_HOME`). See
/// [`config_home`] for the cross-platform resolution. `None` when no config
/// home resolves (no `$XDG_CONFIG_HOME` override and no `$HOME`).
pub fn default_user_config_path() -> Option<PathBuf> {
    config_home().map(|d| d.join("lattice").join("lattice.toml"))
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
    // Stash the parsed tree before walking. Consumers like
    // `workspace/configuration` walk this by dotted-path; the
    // walker's apply-and-bucket pass below stays unchanged.
    out.raw_tree = table.clone();
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
                // TC.2: a table AT an option's name is that option's VALUE,
                // not a namespace to walk into. Checked before both branches
                // below because a composite option is a leaf — descending into
                // it would apply each of its fields as if it were an option of
                // its own, which is how `[[org.capture-templates]]` used to
                // become a pile of `unknown option` warnings.
                if apply_tree_if_composite(registry, source, out, &dotted, value) {
                    continue;
                }
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
    // ML.5: a TOML **array** for a list-typed option (e.g.
    // `ui.modeline.left = ["core.mode", "core.path"]`) is joined into
    // the delimited form the option's `parse` accepts. For a scalar
    // option (or an unknown key) an array stays the "not applicable"
    // warning it was before — never a panic.
    // TC.2: an array-of-tables (`[[org.capture-templates]]`) reaches here as an
    // Array at the option's name. A composite option takes it whole; ML.5's
    // join-into-a-delimited-string is for the list-of-scalars options that
    // predate schemas and still spell their value as text.
    if apply_tree_if_composite(registry, source, out, dotted, value) {
        return;
    }
    if let toml::Value::Array(items) = value {
        apply_array(registry, source, out, dotted, items);
        return;
    }
    let formatted = match format_scalar(value) {
        Some(s) => s,
        None => {
            // Inline tables at scalar position aren't a known option-
            // value shape; warn so the user notices their config
            // didn't apply.
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
    apply_assignment(registry, source, out, dotted, &formatted);
}

/// Apply a TOML **array** leaf. Only list-typed options
/// ([`crate::ErasedOption::accepts_list`], e.g. the `ui.modeline.*`
/// zone options) accept arrays; the array's scalar elements are joined
/// with `,` into the delimited string the option's
/// [`crate::OptionType::parse`] consumes (ML.5). For a scalar option or
/// an unknown key, an array stays the same "not applicable" warning as
/// before — never a panic. A nested array / inline-table element also
/// warns (list elements must be scalars in v1).
fn apply_array(
    registry: &ConfigRegistry,
    source: &Path,
    out: &mut LoadOutcome,
    dotted: &str,
    items: &[toml::Value],
) {
    let accepts_list = registry
        .lookup(dotted)
        .map(|opt| opt.accepts_list())
        .unwrap_or(false);
    if !accepts_list {
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
    let mut parts = Vec::with_capacity(items.len());
    for item in items {
        match format_scalar(item) {
            Some(s) => parts.push(s),
            None => {
                out.messages.push(LoadMessage {
                    level: LoadMessageLevel::Warning,
                    source: source.to_path_buf(),
                    body: format!(
                        "`{dotted}`: nested list / table values aren't \
                         valid list elements",
                    ),
                });
                return;
            }
        }
    }
    // Element ids never contain commas/spaces, so a comma join is
    // unambiguous and round-trips through the option's `parse`.
    let joined = parts.join(",");
    apply_assignment(registry, source, out, dotted, &joined);
}

/// Drive one `dotted=value` assignment against the registry, capturing
/// every failure mode (unknown option, validation reject, parse error)
/// as a warning. Shared by the scalar + array leaf paths.
fn apply_assignment(
    registry: &ConfigRegistry,
    source: &Path,
    out: &mut LoadOutcome,
    dotted: &str,
    value: &str,
) {
    let assign = format!("{dotted}={value}");
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

/// TC.2 — apply `value` as a whole tree if `dotted` names a registered option
/// whose schema is composite. Returns `true` when it handled the key (applied
/// or warned), `false` when the caller should fall through to its existing
/// scalar / namespace / array handling.
///
/// This is the slice's entire behavioural change, and the reason it is a
/// *predicate on the option* rather than on the TOML shape: what makes
/// `[[org.capture-templates]]` a value and `[completion.per-language]` a
/// namespace is not how they are written — both are tables — but whether an
/// option by that exact name exists and says it has structure.
fn apply_tree_if_composite(
    registry: &ConfigRegistry,
    source: &Path,
    out: &mut LoadOutcome,
    dotted: &str,
    value: &toml::Value,
) -> bool {
    let Some(opt) = registry.lookup(dotted) else {
        return false;
    };
    let schema = opt.schema();
    if !schema.is_composite() {
        return false;
    }
    let tree = match toml_to_config_value(value) {
        Ok(v) => v,
        Err(err) => {
            out.messages.push(LoadMessage {
                level: LoadMessageLevel::Warning,
                source: source.to_path_buf(),
                body: format!("`{dotted}{}`: {}", err.path, err.message),
            });
            return true;
        }
    };
    // Validate here as well as inside `set_value`, because only here is the
    // error still STRUCTURED — the loader can splice the schema path onto the
    // option's dotted name and report
    // `org.capture-templates[2].target.file: expected string, got integer`,
    // where a flattened string would have read `org.capture-templates:
    // [2].target.file: …`. The path is the whole user-facing win of this work;
    // one redundant walk of a cold-path config value is a fair price for it.
    if let Err(err) = crate::schema::validate(&schema, &tree) {
        out.messages.push(LoadMessage {
            level: LoadMessageLevel::Warning,
            source: source.to_path_buf(),
            body: format!("`{dotted}{}`: {}", dot_path(&err.path), err.message),
        });
        return true;
    }
    if let Err(err) = opt.set_value(&tree) {
        out.messages.push(LoadMessage {
            level: LoadMessageLevel::Warning,
            source: source.to_path_buf(),
            body: format!("`{dotted}`: {err}"),
        });
    }
    true
}

/// Join a schema path onto an option name. A record field needs the separating
/// dot (`opt` + `target.file`); an index does not (`opt` + `[2]`).
fn dot_path(path: &str) -> String {
    if path.is_empty() || path.starts_with('[') {
        path.to_string()
    } else {
        format!(".{path}")
    }
}

/// TC.2 — a TOML value as a [`ConfigValue`] tree.
///
/// Pure and schema-blind: it answers "what shape is this", and
/// [`crate::schema::validate`] answers "is that the right shape". Keeping the
/// two apart is what lets the shape error and the schema error carry the same
/// kind of path.
///
/// Floats and datetimes are refused rather than stringified. `ConfigValue` has
/// no kind for either, and quietly turning `1.5` into `"1.5"` would make an
/// option's value depend on the host's float formatter — the sort of thing that
/// works until a locale or a plugin language disagrees. Adding a kind later is
/// additive; guessing now is not.
fn toml_to_config_value(value: &toml::Value) -> Result<ConfigValue, SchemaError> {
    fn go(path: &str, value: &toml::Value) -> Result<ConfigValue, SchemaError> {
        match value {
            toml::Value::String(s) => Ok(ConfigValue::Str(s.clone())),
            toml::Value::Integer(i) => Ok(ConfigValue::Int(*i)),
            toml::Value::Boolean(b) => Ok(ConfigValue::Bool(*b)),
            toml::Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for (i, item) in items.iter().enumerate() {
                    out.push(go(&format!("{path}[{i}]"), item)?);
                }
                Ok(ConfigValue::List(out))
            }
            toml::Value::Table(table) => {
                let mut out = std::collections::BTreeMap::new();
                for (k, v) in table {
                    let child = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    out.insert(k.clone(), go(&child, v)?);
                }
                Ok(ConfigValue::Record(out))
            }
            toml::Value::Float(_) => Err(SchemaError {
                path: path.to_string(),
                message: "floating-point values are not a configuration value shape".to_string(),
            }),
            toml::Value::Datetime(_) => Err(SchemaError {
                path: path.to_string(),
                message: "datetime values are not a configuration value shape".to_string(),
            }),
        }
    }
    go("", value).map_err(|mut e| {
        e.path = dot_path(&e.path);
        e
    })
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

    #[test]
    fn config_home_prefers_absolute_xdg_override() {
        // An absolute $XDG_CONFIG_HOME wins over the platform fallback,
        // on every platform.
        let got = resolve_config_home(
            Some(PathBuf::from("/custom/xdg")),
            Some(PathBuf::from("/home/u/.config")),
        );
        assert_eq!(got, Some(PathBuf::from("/custom/xdg")));
    }

    #[test]
    fn config_home_ignores_relative_or_empty_xdg() {
        // A set-but-relative $XDG_CONFIG_HOME is spec-invalid → fall back.
        let fallback = Some(PathBuf::from("/home/u/.config"));
        assert_eq!(
            resolve_config_home(Some(PathBuf::from("relative/path")), fallback.clone()),
            fallback
        );
        // Empty value → PathBuf::from("") is not absolute → fall back.
        assert_eq!(
            resolve_config_home(Some(PathBuf::from("")), fallback.clone()),
            fallback
        );
    }

    #[test]
    fn config_home_falls_back_when_no_override() {
        let fallback = Some(PathBuf::from("/home/u/.config"));
        assert_eq!(resolve_config_home(None, fallback.clone()), fallback);
    }

    #[test]
    fn config_home_is_none_when_nothing_resolves() {
        assert_eq!(resolve_config_home(None, None), None);
    }

    #[test]
    fn default_user_config_path_ends_with_xdg_lattice_toml() {
        // The public entry point composes <config_home>/lattice/lattice.toml.
        // We can't assert the absolute prefix (env-dependent), but the tail is
        // stable and proves we no longer hard-code the platform-native dir.
        if let Some(p) = default_user_config_path() {
            assert!(
                p.ends_with("lattice/lattice.toml"),
                "expected .../lattice/lattice.toml, got {p:?}"
            );
        }
    }

    fn registry_with_options() -> ConfigRegistry {
        let r = ConfigRegistry::new();
        r.register(ConfigOption::<bool>::new(
            "number",
            true,
            "Show absolute line numbers.",
        ));
        r.register(ConfigOption::<i64>::new("tabstop", 8, "Tab width."));
        r.register(
            ConfigOption::<i64>::builder("scrolloff", 0, "Scroll-off margin.")
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

    // ── TC.2 fixture: a composite-schema option ───────────────────
    //
    // Modelled on org's `capture-templates`, which is the option that
    // proves the point: a LIST of RECORDS, one of which nests another
    // record. Nothing in the workspace has this shape yet (that is
    // phase 3's job), and waiting for it would mean the loader change
    // landed with no test of the case it exists for.

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Template {
        key: String,
        target: String,
        body: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Default)]
    struct Templates(Vec<Template>);

    impl crate::OptionType for Templates {
        // The `:set` text surface. A composite keeps `parse`/`format`
        // round-tripping — the trait's contract does not get a
        // exemption for having structure — over a compact
        // `key>target` form. What the real migration spells here is
        // its own call (typed-configuration.md §2.2); what matters to
        // the loader is that it never touches this path.
        fn parse(s: &str) -> Result<Self, String> {
            if s.is_empty() {
                return Ok(Templates(Vec::new()));
            }
            s.split(';')
                .map(|item| {
                    let (key, target) = item
                        .split_once('>')
                        .ok_or_else(|| format!("expected `key>target`, got `{item}`"))?;
                    Ok(Template {
                        key: key.to_string(),
                        target: target.to_string(),
                        body: None,
                    })
                })
                .collect::<Result<Vec<_>, String>>()
                .map(Templates)
        }

        fn format(&self) -> String {
            self.0
                .iter()
                .map(|t| format!("{}>{}", t.key, t.target))
                .collect::<Vec<_>>()
                .join(";")
        }

        fn type_label() -> &'static str {
            "templates"
        }

        fn schema() -> crate::ConfigSchema {
            use crate::{ConfigSchema, SchemaField};
            ConfigSchema::list(ConfigSchema::record([
                SchemaField::new("key", ConfigSchema::string(), "the key to press"),
                SchemaField::new(
                    "target",
                    ConfigSchema::record([SchemaField::new(
                        "file",
                        ConfigSchema::string(),
                        "where it lands",
                    )]),
                    "where the capture goes",
                ),
                SchemaField::new("body", ConfigSchema::string(), "template body").optional(),
            ]))
        }

        fn to_value(&self) -> ConfigValue {
            ConfigValue::List(
                self.0
                    .iter()
                    .map(|t| {
                        let mut fields = vec![
                            ("key".to_string(), ConfigValue::Str(t.key.clone())),
                            (
                                "target".to_string(),
                                ConfigValue::record([(
                                    "file".to_string(),
                                    ConfigValue::Str(t.target.clone()),
                                )]),
                            ),
                        ];
                        if let Some(b) = &t.body {
                            fields.push(("body".to_string(), ConfigValue::Str(b.clone())));
                        }
                        ConfigValue::record(fields)
                    })
                    .collect(),
            )
        }

        fn from_value(value: &ConfigValue) -> Result<Self, String> {
            let items = value
                .as_list()
                .ok_or_else(|| format!("expected list, got {}", value.kind_label()))?;
            items
                .iter()
                .map(|item| {
                    let key = item
                        .field("key")
                        .and_then(ConfigValue::as_str)
                        .ok_or("missing `key`")?
                        .to_string();
                    let target = item
                        .field("target")
                        .and_then(|t| t.field("file"))
                        .and_then(ConfigValue::as_str)
                        .ok_or("missing `target.file`")?
                        .to_string();
                    let body = item
                        .field("body")
                        .and_then(ConfigValue::as_str)
                        .map(str::to_string);
                    Ok(Template { key, target, body })
                })
                .collect::<Result<Vec<_>, String>>()
                .map(Templates)
        }
    }

    fn registry_with_a_composite_option() -> ConfigRegistry {
        let r = registry_with_options();
        r.register(ConfigOption::<Templates>::new(
            "org.capture-templates",
            Templates::default(),
            "Capture templates.",
        ));
        r
    }

    fn templates_of(r: &ConfigRegistry) -> Templates {
        let opt = r.lookup("org.capture-templates").unwrap();
        <Templates as crate::OptionType>::from_value(&opt.get_value()).unwrap()
    }

    #[test]
    fn an_array_of_tables_lands_as_a_composite_options_value() {
        // The case the blob existed to work around. Before TC.2 this
        // produced "list / inline-table values aren't applicable to
        // scalar options"; org's answer was to make the whole thing a
        // string containing TOML.
        let r = registry_with_a_composite_option();
        let p = write_temp(
            "composite-array-of-tables",
            "[[org.capture-templates]]\n\
             key = \"t\"\n\
             target = { file = \"~/org/refile.org\" }\n\
             body = \"\"\"\n\
             * TODO %?\n\
             \"\"\"\n\
             \n\
             [[org.capture-templates]]\n\
             key = \"n\"\n\
             target = { file = \"~/org/notes.org\" }\n",
        );
        let out = load_file(&r, &p, &[]);
        assert!(out.messages.is_empty(), "messages: {:?}", out.messages);

        let got = templates_of(&r);
        assert_eq!(got.0.len(), 2);
        assert_eq!(got.0[0].key, "t");
        assert_eq!(got.0[0].target, "~/org/refile.org");
        // The multi-line body survives verbatim — the field the blob
        // handled WELL (a `'''` literal preserves newlines), and so
        // the one a tree could plausibly regress. TOML's `"""` eats
        // the newline immediately after the opening delimiter, which
        // is the format's rule and not the tree's doing.
        assert_eq!(got.0[0].body.as_deref(), Some("* TODO %?\n"));
        assert_eq!(got.0[1].key, "n");
        assert_eq!(got.0[1].body, None, "an absent optional field stays absent");
    }

    #[test]
    fn a_table_at_an_options_name_is_its_value_not_a_namespace() {
        // A record-shaped option written as a section. Without the
        // check, `walk_table` descends and applies `key` and `target`
        // as options in their own right — two `unknown option`
        // warnings and no value set.
        let r = registry_with_options();
        r.register(ConfigOption::<Templates>::new(
            "org.one-template",
            Templates::default(),
            "",
        ));
        // A single record still has to satisfy `list<record>`, so this
        // is the WRONG shape — and the point of the assertion is that
        // the loader says so about the option rather than inventing
        // two options that do not exist.
        let p = write_temp(
            "composite-table",
            "[org.one-template]\nkey = \"t\"\ntarget = { file = \"a.org\" }\n",
        );
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1, "messages: {:?}", out.messages);
        let body = &out.messages[0].body;
        assert!(body.contains("org.one-template"), "{body}");
        assert!(body.contains("expected list"), "{body}");
        assert!(
            !body.contains("unknown option"),
            "the fields must not be read as options: {body}"
        );
    }

    #[test]
    fn a_shape_mismatch_is_reported_with_its_path() {
        // The user-facing win of the whole design, and the assertion is
        // on the PATH rather than on rejection: rejecting without
        // saying where is exactly what the hand-rolled parsers did.
        let r = registry_with_a_composite_option();
        let p = write_temp(
            "composite-bad-leaf",
            "[[org.capture-templates]]\n\
             key = \"t\"\n\
             target = { file = \"a.org\" }\n\
             \n\
             [[org.capture-templates]]\n\
             key = \"n\"\n\
             target = { file = 7 }\n",
        );
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1, "messages: {:?}", out.messages);
        let body = &out.messages[0].body;
        assert!(
            body.contains("org.capture-templates[1].target.file"),
            "the path must name the index AND the field: {body}"
        );
        assert!(body.contains("expected string"), "{body}");
        assert!(body.contains("integer"), "{body}");
        // Nothing was committed — a partially-applied list is worse
        // than a refused one, because half a config reads as a bug in
        // the feature rather than a typo in the file.
        assert_eq!(templates_of(&r).0.len(), 0);
    }

    #[test]
    fn a_misspelled_field_names_the_key_and_the_alternatives() {
        let r = registry_with_a_composite_option();
        let p = write_temp(
            "composite-typo",
            "[[org.capture-templates]]\n\
             key = \"t\"\n\
             target = { file = \"a.org\" }\n\
             bodyy = \"oops\"\n",
        );
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1, "messages: {:?}", out.messages);
        let body = &out.messages[0].body;
        assert!(body.contains("org.capture-templates[0].bodyy"), "{body}");
        assert!(body.contains("unknown field"), "{body}");
        assert!(body.contains("body"), "{body}");
    }

    #[test]
    fn a_float_is_refused_by_name_rather_than_stringified() {
        // `ConfigValue` has no float kind. Quietly rendering `1.5` as
        // "1.5" would make the value depend on the host's float
        // formatter, which works until something disagrees about it.
        let r = registry_with_a_composite_option();
        let p = write_temp(
            "composite-float",
            "[[org.capture-templates]]\nkey = 1.5\ntarget = { file = \"a.org\" }\n",
        );
        let out = load_file(&r, &p, &[]);
        assert_eq!(out.messages.len(), 1, "messages: {:?}", out.messages);
        let body = &out.messages[0].body;
        assert!(body.contains("org.capture-templates[0].key"), "{body}");
        assert!(body.contains("floating-point"), "{body}");
    }

    #[test]
    fn scalar_options_are_untouched_by_the_composite_path() {
        // The no-regression half. A table at a SCALAR option's name
        // must still take the old warning, and a structural namespace
        // must still be captured whole — TC.2 adds a branch, it does
        // not re-route the existing ones.
        let r = registry_with_a_composite_option();
        let p = write_temp(
            "composite-no-regression",
            "[completion.per-language.markdown]\nsources = \"buffer\"\n\
             \n[ui]\nseparator = \"|\"\n\
             \n[tabstop]\nnope = 1\n",
        );
        let out = load_file(&r, &p, &["completion.per-language"]);
        assert!(
            out.structural
                .contains_key("completion.per-language.markdown"),
            "a structural namespace is still captured whole: {:?}",
            out.structural.keys().collect::<Vec<_>>()
        );
        assert_eq!(r.lookup("ui.separator").unwrap().get_formatted(), "|");
        // `[tabstop]` is a table at a SCALAR option's name: it walks
        // in and `tabstop.nope` is unknown, exactly as before.
        assert!(
            out.messages
                .iter()
                .any(|m| m.body.contains("unknown option")),
            "messages: {:?}",
            out.messages
        );
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
        let p = write_temp("unknown", "number = false\nbogus.key = 42\ntabstop = 2\n");
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
    fn toml_array_applies_to_a_list_typed_option() {
        // ML.5: a TOML array reaches a list-typed option (ModelineZone),
        // joined into the option's comma-delimited parse form. Helix
        // shape: `[ui.modeline]\nleft = ["core.mode", "core.path"]`.
        let r = registry_with_options();
        r.register(ConfigOption::<crate::ModelineZone>::new(
            "ui.modeline.left",
            crate::ModelineZone::Auto,
            "Left modeline zone.",
        ));
        let p = write_temp(
            "modeline-array",
            "[ui.modeline]\nleft = [\"core.mode\", \"core.path\"]\n",
        );
        let out = load_file(&r, &p, &[]);
        assert!(out.messages.is_empty(), "messages: {:?}", out.messages);
        assert_eq!(
            r.lookup("ui.modeline.left").unwrap().get_formatted(),
            "core.mode,core.path",
        );
    }

    #[test]
    fn empty_toml_array_clears_a_list_typed_zone() {
        // `left = []` is an explicitly-blank zone (distinct from the
        // `auto` default) — applies as an empty id list.
        let r = registry_with_options();
        r.register(ConfigOption::<crate::ModelineZone>::new(
            "ui.modeline.right",
            crate::ModelineZone::Auto,
            "Right modeline zone.",
        ));
        let p = write_temp("modeline-empty", "[ui.modeline]\nright = []\n");
        let out = load_file(&r, &p, &[]);
        assert!(out.messages.is_empty(), "messages: {:?}", out.messages);
        // Empty id list formats back to the empty string.
        assert_eq!(r.lookup("ui.modeline.right").unwrap().get_formatted(), "");
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
        let p = write_temp("ns-scalar", "[completion.per-language]\nbroken = 1\n");
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
        assert!(
            out.structural
                .contains_key("completion.per-language.markdown")
        );
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
            r.lookup("completion.auto_insert_single")
                .unwrap()
                .get_formatted(),
            "false",
        );
        assert!(
            out.structural
                .contains_key("completion.per-language.markdown")
        );
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
    fn load_file_populates_raw_tree_with_full_parsed_table() {
        let r = registry_with_options();
        let p = write_temp(
            "raw-tree",
            "[lsp.rust-analyzer.cargo]\n\
             features = [\"foo\", \"bar\"]\n\
             [lsp.rust-analyzer.checkOnSave]\n\
             enable = true\n",
        );
        let out = load_file(&r, &p, &["lsp"]);
        // Tree carries the full structure even though `lsp` was
        // also handled as a structural namespace (the two
        // surfaces coexist without conflict).
        let features = lookup_dotted_path(&out.raw_tree, "lsp.rust-analyzer.cargo.features")
            .expect("features path");
        let arr = features.as_array().expect("array");
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0].as_str(), Some("foo"));
        assert_eq!(arr[1].as_str(), Some("bar"));
        let enable = lookup_dotted_path(&out.raw_tree, "lsp.rust-analyzer.checkOnSave.enable")
            .expect("enable path");
        assert_eq!(enable.as_bool(), Some(true));
    }

    #[test]
    fn extend_deep_merges_raw_trees_preserving_sibling_keys() {
        // User config has `lsp.rust-analyzer.checkOnSave = true`.
        // Project config has `lsp.rust-analyzer.cargo.features =
        // ["proj"]`. Merged tree carries BOTH -- project's edit
        // doesn't clobber user's sibling key inside the same
        // `[lsp.rust-analyzer]` table.
        let r = registry_with_options();
        let user = write_temp("deep-user", "[lsp.rust-analyzer]\ncheckOnSave = true\n");
        let proj = write_temp(
            "deep-proj",
            "[lsp.rust-analyzer.cargo]\nfeatures = [\"proj\"]\n",
        );
        let mut out = load_file(&r, &user, &["lsp"]);
        out.extend(load_file(&r, &proj, &["lsp"]));
        let check = lookup_dotted_path(&out.raw_tree, "lsp.rust-analyzer.checkOnSave")
            .and_then(|v| v.as_bool());
        assert_eq!(check, Some(true), "user's checkOnSave preserved");
        let features = lookup_dotted_path(&out.raw_tree, "lsp.rust-analyzer.cargo.features")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>());
        assert_eq!(features, Some(vec!["proj"]), "project's features applied");
    }

    #[test]
    fn extend_overrides_scalars_with_later_values() {
        // Both files set the same scalar key -- the later
        // (project) value wins.
        let r = registry_with_options();
        let user = write_temp("scalar-user", "tabstop = 2\n");
        let proj = write_temp("scalar-proj", "tabstop = 8\n");
        let mut out = load_file(&r, &user, &[]);
        out.extend(load_file(&r, &proj, &[]));
        let ts = lookup_dotted_path(&out.raw_tree, "tabstop").unwrap();
        assert_eq!(ts.as_integer(), Some(8));
    }

    #[test]
    fn lookup_dotted_path_returns_none_for_missing_segments() {
        let mut t = toml::Table::new();
        t.insert("a".into(), toml::Value::String("hello".into()));
        assert!(lookup_dotted_path(&t, "missing").is_none());
        // `a` is a string, not a table -- walking past it is None.
        assert!(lookup_dotted_path(&t, "a.deeper").is_none());
        assert_eq!(
            lookup_dotted_path(&t, "a").and_then(|v| v.as_str()),
            Some("hello"),
        );
    }

    #[test]
    fn project_path_helper_lands_at_dot_lattice_config_toml() {
        let p = project_config_path(Path::new("/workspace/foo"));
        assert_eq!(p, PathBuf::from("/workspace/foo/.lattice/config.toml"));
    }
}
