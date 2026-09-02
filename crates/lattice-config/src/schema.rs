//! TC.1 — what an option's value *is shaped like*, as data.
//!
//! Design: [`typed-configuration.md`](../../../docs/dev/architecture/typed-configuration.md).
//!
//! Options have always been typed on the Rust side — `Option<T>` over an
//! [`OptionType`](crate::OptionType). What they have never been is typed to
//! anything that is not Rust. The type-erased surface every runtime-name
//! consumer goes through (`:set`, the TOML loader, plugin introspection,
//! `:describe-option`) exposed a `type_label(): &str` and a formatted `String`,
//! and a label plus a string is not enough to render a composite, validate a
//! field, or write a value back to TOML.
//!
//! [`ConfigSchema`] is that description and [`ConfigValue`] is a value shaped
//! by it. Together they are what makes design §5.12's `:customize` — "a
//! type-aware editing buffer" — possible at all, and what lets a plugin declare
//! a record without the host needing a Rust type for it (WIT has no generics;
//! self-description is the expressible answer).
//!
//! **Additive by construction.** `parse` / `format` keep their round-trip
//! contract and remain the `:set` surface, because a command line is a text
//! surface and typing a record into one is not an improvement. A scalar option
//! behaves identically before and after this module existed — its schema is
//! *derived* from what it already declares rather than written by hand.

use std::collections::BTreeMap;

/// The leaf kinds. Deliberately the three the ABI already carries plus nothing:
/// a float would need a parse/format round-trip that survives every locale and
/// every plugin language's formatter, and no option in the workspace wants one.
/// Adding it later is additive; guessing now is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarKind {
    Bool,
    Int,
    Str,
}

impl ScalarKind {
    /// The word a mismatch message uses. Matches `OptionType::type_label`'s
    /// vocabulary for the three primitives, so the two surfaces do not disagree
    /// about what an integer is called.
    pub fn label(self) -> &'static str {
        match self {
            ScalarKind::Bool => "boolean",
            ScalarKind::Int => "integer",
            ScalarKind::Str => "string",
        }
    }
}

/// One field of a [`ConfigSchema::Record`].
///
/// `doc` is carried per field rather than only per option because that is what
/// `:describe-option` and `:customize` render beside the field — an option-level
/// doc string describing six fields is the wall of prose the schema exists to
/// replace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaField {
    pub name: String,
    pub schema: ConfigSchema,
    /// A missing required field is a validation error naming its path; a
    /// missing optional one is simply absent from the value.
    pub required: bool,
    pub doc: String,
}

impl SchemaField {
    /// A required field. The common case, so it is the short constructor.
    pub fn new(name: impl Into<String>, schema: ConfigSchema, doc: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            schema,
            required: true,
            doc: doc.into(),
        }
    }

    /// Builder: make this field optional.
    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }
}

/// The declared shape of an option's value.
///
/// `Enum` is not sugar for `Scalar(Str)`: it is the difference between
/// `:customize` offering a picker and offering a text field. It is derived from
/// `OptionType::enumerate`, but only for a type that declares that enumeration
/// CLOSED (`enumerate_is_exhaustive`) — several types use `enumerate` as a
/// completion hint over an open space, and describing those as enums would be
/// worse than describing them as strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSchema {
    Scalar(ScalarKind),
    /// A closed set of string forms. Carries the forms in declaration order —
    /// completion and `:customize` both show them in the order the type meant.
    Enum(Vec<String>),
    List(Box<ConfigSchema>),
    Record(Vec<SchemaField>),
}

impl ConfigSchema {
    /// Shorthand constructors, because the common shapes are written often
    /// enough that `ConfigSchema::Scalar(ScalarKind::Str)` becomes noise.
    pub fn string() -> Self {
        ConfigSchema::Scalar(ScalarKind::Str)
    }
    pub fn int() -> Self {
        ConfigSchema::Scalar(ScalarKind::Int)
    }
    pub fn bool() -> Self {
        ConfigSchema::Scalar(ScalarKind::Bool)
    }
    pub fn list(inner: ConfigSchema) -> Self {
        ConfigSchema::List(Box::new(inner))
    }
    pub fn record(fields: impl IntoIterator<Item = SchemaField>) -> Self {
        ConfigSchema::Record(fields.into_iter().collect())
    }

    /// Whether this shape has anything below its top level. The line the TOML
    /// loader and `:describe-option` branch on: a scalar-shaped option keeps
    /// every path it has today, a composite one takes the tree path.
    pub fn is_composite(&self) -> bool {
        matches!(self, ConfigSchema::List(_) | ConfigSchema::Record(_))
    }

    /// A one-line rendering for `:describe-option`'s type column
    /// (`list<record>`, `enum`, `string`). The full field-by-field rendering is
    /// TC.8's; this is what fits where `type_label()` used to go.
    pub fn label(&self) -> String {
        match self {
            ConfigSchema::Scalar(k) => k.label().to_string(),
            ConfigSchema::Enum(_) => "enum".to_string(),
            ConfigSchema::List(inner) => format!("list<{}>", inner.label()),
            ConfigSchema::Record(_) => "record".to_string(),
        }
    }
}

/// A value shaped by a [`ConfigSchema`].
///
/// `Record` is a `BTreeMap` rather than a `Vec<(String, ConfigValue)>` so two
/// values that differ only in field order compare equal — an option's value
/// arriving from `lattice.toml` (which does not preserve order across a
/// round-trip) and the same value built in `init.rs` must not be two different
/// values. The wire form is an association list, because WIT has no map; the
/// conversion is where the ordering stops mattering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValue {
    Bool(bool),
    Int(i64),
    Str(String),
    List(Vec<ConfigValue>),
    Record(BTreeMap<String, ConfigValue>),
}

impl ConfigValue {
    /// Build a record from pairs. The wire and TOML forms both arrive as pairs.
    pub fn record(fields: impl IntoIterator<Item = (String, ConfigValue)>) -> Self {
        ConfigValue::Record(fields.into_iter().collect())
    }

    /// The word a mismatch message uses for what was actually found.
    pub fn kind_label(&self) -> &'static str {
        match self {
            ConfigValue::Bool(_) => "boolean",
            ConfigValue::Int(_) => "integer",
            ConfigValue::Str(_) => "string",
            ConfigValue::List(_) => "list",
            ConfigValue::Record(_) => "record",
        }
    }

    /// Read a scalar back out. `None` on a kind mismatch rather than a panic —
    /// callers are usually walking a tree they have already validated, and the
    /// ones that have not should not be able to crash the editor over a config
    /// file.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ConfigValue::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ConfigValue::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i64> {
        match self {
            ConfigValue::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn as_list(&self) -> Option<&[ConfigValue]> {
        match self {
            ConfigValue::List(items) => Some(items),
            _ => None,
        }
    }
    /// A record's field by name.
    pub fn field(&self, name: &str) -> Option<&ConfigValue> {
        match self {
            ConfigValue::Record(map) => map.get(name),
            _ => None,
        }
    }
}

/// A validation failure, carrying **where** it happened.
///
/// The path is the whole point, and it is what no hand-rolled parser produced:
/// `capture-templates[2].target.file: expected string, got integer` is
/// actionable where "invalid capture template" is not. It is also the concrete
/// thing a user gets out of typed configuration before `:customize` exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    /// Dotted / indexed path from the option's root. Empty at the root itself.
    pub path: String,
    pub message: String,
}

impl std::fmt::Display for SchemaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for SchemaError {}

/// Check `value` against `schema`, reporting the first failure with its path.
///
/// **First failure, not all of them.** A shape mismatch high in the tree makes
/// everything under it meaningless, so a list of twelve consequential errors
/// would bury the one that matters; the loader's contract is to warn and keep
/// going per *key*, and one clear message per key is what serves it.
pub fn validate(schema: &ConfigSchema, value: &ConfigValue) -> Result<(), SchemaError> {
    validate_at("", schema, value)
}

fn validate_at(path: &str, schema: &ConfigSchema, value: &ConfigValue) -> Result<(), SchemaError> {
    let mismatch = |expected: &str| {
        Err(SchemaError {
            path: path.to_string(),
            message: format!("expected {expected}, got {}", value.kind_label()),
        })
    };
    match schema {
        ConfigSchema::Scalar(kind) => match (kind, value) {
            (ScalarKind::Bool, ConfigValue::Bool(_))
            | (ScalarKind::Int, ConfigValue::Int(_))
            | (ScalarKind::Str, ConfigValue::Str(_)) => Ok(()),
            _ => mismatch(kind.label()),
        },
        ConfigSchema::Enum(forms) => {
            let Some(got) = value.as_str() else {
                return mismatch("string");
            };
            if forms.iter().any(|f| f == got) {
                Ok(())
            } else {
                Err(SchemaError {
                    path: path.to_string(),
                    // The valid set, inline: an enum's whole advantage over a
                    // free string is that the answer is finite, so a rejection
                    // that does not show it wastes the one thing it knows.
                    message: format!("expected one of {}, got `{got}`", forms.join(" | ")),
                })
            }
        }
        ConfigSchema::List(inner) => {
            let Some(items) = value.as_list() else {
                return mismatch("list");
            };
            for (i, item) in items.iter().enumerate() {
                validate_at(&format!("{path}[{i}]"), inner, item)?;
            }
            Ok(())
        }
        ConfigSchema::Record(fields) => {
            let ConfigValue::Record(map) = value else {
                return mismatch("record");
            };
            for field in fields {
                let child = if path.is_empty() {
                    field.name.clone()
                } else {
                    format!("{path}.{}", field.name)
                };
                match map.get(&field.name) {
                    Some(v) => validate_at(&child, &field.schema, v)?,
                    None if field.required => {
                        return Err(SchemaError {
                            path: child,
                            message: "required field is missing".to_string(),
                        });
                    }
                    None => {}
                }
            }
            // An unknown field is an ERROR, not a warning that scrolls past.
            // A misspelled key in a config file is the single most common way
            // configuration silently does nothing, and the shape is known
            // exactly — so there is no reason to accept it and every reason to
            // say which key and what was expected.
            for key in map.keys() {
                if !fields.iter().any(|f| &f.name == key) {
                    let child = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}.{key}")
                    };
                    let known: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
                    return Err(SchemaError {
                        path: child,
                        message: format!("unknown field; expected one of {}", known.join(" | ")),
                    });
                }
            }
            Ok(())
        }
    }
}

// ── TOML interop ──────────────────────────────────────────────────────────
//
// Lives here rather than in the loader because two callers need it: the loader
// (a composite option's value written natively in `lattice.toml`) and
// `ConfigValue`'s own `OptionType::parse` (the `:set` text surface, below).
// One conversion, so the two homes cannot drift about what a TOML table means.

/// Join a schema path onto an option name. A record field needs the separating
/// dot (`opt` + `target.file`); an index does not (`opt` + `[2]`).
pub fn dot_path(path: &str) -> String {
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
pub fn toml_to_config_value(value: &toml::Value) -> Result<ConfigValue, SchemaError> {
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

/// A [`ConfigValue`] as TOML. The inverse of [`toml_to_config_value`], used by
/// `ConfigValue`'s `format()` — which is what `:set foo?` echoes and what
/// `:describe-option` shows.
///
/// Total: every `ConfigValue` kind has a TOML counterpart, which is not true in
/// the other direction (floats and datetimes have no `ConfigValue`).
pub fn config_value_to_toml(value: &ConfigValue) -> toml::Value {
    match value {
        ConfigValue::Bool(b) => toml::Value::Boolean(*b),
        ConfigValue::Int(i) => toml::Value::Integer(*i),
        ConfigValue::Str(s) => toml::Value::String(s.clone()),
        ConfigValue::List(items) => {
            toml::Value::Array(items.iter().map(config_value_to_toml).collect())
        }
        ConfigValue::Record(map) => toml::Value::Table(
            map.iter()
                .map(|(k, v)| (k.clone(), config_value_to_toml(v)))
                .collect(),
        ),
    }
}

/// The key `format` wraps a non-record root under. `parse` accepts any name —
/// see [`unwrap_root`].
const ROOT_KEY: &str = "value";

/// The inner value of a single-key wrapper table, if `table` is one.
///
/// A TOML document cannot BE an array, so a list-rooted option has no bare text
/// spelling and needs a wrapper. `format` writes `value = …`; `parse` accepts
/// **any** single key whose payload is an array, not just that one.
///
/// The generosity is deliberate and is what makes the migration painless. A
/// user who wrote `org.capture-templates` as `[[template]]`, or
/// `agenda-sections` as `[[section]]`, keeps writing exactly that: the option's
/// value is now the list itself, and their wrapper name — whatever they chose —
/// unwraps to it. Insisting on `value` would have broken every existing `:set`
/// string for no gain, since the name carries no information the schema does
/// not already have.
///
/// Only an ARRAY payload unwraps. A single-key table whose value is a table is
/// a record with one field, which is a shape an option legitimately has.
fn unwrap_root(table: &toml::Table) -> Option<&toml::Value> {
    if table.len() != 1 {
        return None;
    }
    let (key, value) = table.iter().next()?;
    match value {
        // Any name, for an array: this is the migration path, and the name
        // carries nothing the schema does not already know.
        toml::Value::Array(_) => Some(value),
        // A table under any name is a record with one field — a shape an
        // option legitimately has — so it is left alone.
        toml::Value::Table(_) => None,
        // A scalar root has no bare spelling either, and `format` wraps it
        // under the reserved name. Here the name DOES have to match: a
        // single-field record holding a scalar is far commoner than a
        // scalar-rooted option, so unwrapping every one of those would trade a
        // real case for a rare one.
        _ => (key == ROOT_KEY).then_some(value),
    }
}

/// TC.3 — a `ConfigValue` is itself an option value type.
///
/// This is what a plugin's structured option holds. The shape is NOT here: a
/// plugin declares its schema at registration and it is carried on the option
/// spec ([`crate::option::Option::structured`]), because the schema is metadata
/// about the option — like its doc and its default — rather than data inside
/// the value. A value that carried its own schema could not survive
/// `from_value`, which is a static function with no access to the option it is
/// being set on.
///
/// `parse` / `format` are **TOML text**, which keeps the `:set` contract and
/// costs nothing: the host already has a TOML parser, so a structured option
/// stays settable from the command line and `:set foo?` still echoes something
/// a user can read and paste back. It also means the migration of an option
/// that was a TOML-in-a-string is not a break for anyone who was setting it
/// that way — the text they wrote still parses, it is simply now validated.
impl crate::OptionType for ConfigValue {
    fn parse(s: &str) -> Result<Self, String> {
        let table = toml::from_str::<toml::Table>(s).map_err(|e| format!("expected TOML: {e}"))?;
        // The unwrap half of `format`'s wrapper, below. Symmetric on purpose:
        // `parse(&v.format()) == Ok(v)` is `OptionType`'s contract and a
        // structured option does not get an exemption from it.
        if let Some(inner) = unwrap_root(&table) {
            return toml_to_config_value(inner).map_err(|e| e.to_string());
        }
        toml_to_config_value(&toml::Value::Table(table)).map_err(|e| e.to_string())
    }

    fn format(&self) -> String {
        // A non-table root has no top-level TOML spelling — a document cannot
        // BE an array — so a list-rooted value is rendered under the reserved
        // key `value`, and `parse` unwraps it again.
        //
        // The cost is one ambiguity, named rather than hidden: a RECORD option
        // with exactly one field, and that field a list, cannot round-trip
        // through this text surface — `parse` reads the wrapper as the wrapper
        // it usually is. It is not silent when it happens (the unwrapped tree
        // fails schema validation with a path), `:set` on a composite is
        // deferred surface anyway (typed-configuration.md §2.2), and
        // `lattice.toml` and `set-option-value` both carry the tree natively
        // and never come through here.
        match config_value_to_toml(self) {
            toml::Value::Table(t) => toml::to_string_pretty(&t).unwrap_or_default(),
            other => {
                let mut t = toml::Table::new();
                t.insert(ROOT_KEY.to_string(), other);
                toml::to_string_pretty(&t).unwrap_or_default()
            }
        }
    }

    fn type_label() -> &'static str {
        "structured"
    }

    /// Unknowable statically — the shape belongs to the OPTION, not the type.
    /// Every real structured option is built through
    /// [`crate::option::Option::structured`], which records the declared schema
    /// on the spec and is what `ErasedOption::schema()` answers with. This
    /// fallback exists only so the trait is implementable.
    fn schema() -> crate::ConfigSchema {
        crate::ConfigSchema::string()
    }

    fn to_value(&self) -> ConfigValue {
        self.clone()
    }

    fn from_value(value: &ConfigValue) -> Result<Self, String> {
        Ok(value.clone())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn template_schema() -> ConfigSchema {
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
            SchemaField::new("body", ConfigSchema::string(), "the template body").optional(),
        ]))
    }

    fn template(key: &str, file: ConfigValue) -> ConfigValue {
        ConfigValue::record([
            ("key".to_string(), ConfigValue::Str(key.to_string())),
            (
                "target".to_string(),
                ConfigValue::record([("file".to_string(), file)]),
            ),
        ])
    }

    #[test]
    fn a_well_shaped_tree_validates() {
        let v = ConfigValue::List(vec![template("t", ConfigValue::Str("~/org/in.org".into()))]);
        assert_eq!(validate(&template_schema(), &v), Ok(()));
    }

    #[test]
    fn a_mismatch_names_its_path() {
        // The assertion this module exists for. Rejecting is easy; rejecting
        // with a location is the thing every hand-rolled parser skipped.
        let v = ConfigValue::List(vec![
            template("t", ConfigValue::Str("a".into())),
            template("n", ConfigValue::Str("b".into())),
            template("x", ConfigValue::Int(7)),
        ]);
        let err = validate(&template_schema(), &v).unwrap_err();
        assert_eq!(err.path, "[2].target.file");
        assert!(err.message.contains("expected string"), "{err}");
        assert!(err.message.contains("integer"), "{err}");
        assert_eq!(
            err.to_string(),
            "[2].target.file: expected string, got integer"
        );
    }

    #[test]
    fn a_missing_required_field_names_itself_not_its_parent() {
        let v = ConfigValue::List(vec![ConfigValue::record([(
            "key".to_string(),
            ConfigValue::Str("t".into()),
        )])]);
        let err = validate(&template_schema(), &v).unwrap_err();
        assert_eq!(err.path, "[0].target");
        assert!(err.message.contains("required"), "{err}");
    }

    #[test]
    fn an_optional_field_may_be_absent() {
        // `body` is optional; its absence must not be the same error as
        // `target`'s, or optionality means nothing.
        let v = ConfigValue::List(vec![template("t", ConfigValue::Str("a".into()))]);
        assert_eq!(validate(&template_schema(), &v), Ok(()));
    }

    #[test]
    fn an_unknown_field_is_refused_by_name() {
        // A misspelled key is how configuration silently does nothing. The
        // shape is known exactly, so there is no excuse for accepting it.
        let v = ConfigValue::List(vec![ConfigValue::record([
            ("key".to_string(), ConfigValue::Str("t".into())),
            (
                "target".to_string(),
                ConfigValue::record([("file".to_string(), ConfigValue::Str("a".into()))]),
            ),
            ("bodyy".to_string(), ConfigValue::Str("oops".into())),
        ])]);
        let err = validate(&template_schema(), &v).unwrap_err();
        assert_eq!(err.path, "[0].bodyy");
        assert!(err.message.contains("unknown field"), "{err}");
        assert!(err.message.contains("body"), "{err}");
    }

    #[test]
    fn an_enum_rejection_shows_the_valid_set() {
        let schema = ConfigSchema::Enum(vec!["marker".into(), "indent".into(), "syntax".into()]);
        let err = validate(&schema, &ConfigValue::Str("manual".into())).unwrap_err();
        assert!(err.message.contains("marker | indent | syntax"), "{err}");
        assert!(err.message.contains("manual"), "{err}");
    }

    #[test]
    fn record_field_order_does_not_change_the_value() {
        // Values arrive from TOML (unordered) and from a Rust struct (ordered).
        // If those compared unequal, "the same config" would be two values.
        let a = ConfigValue::record([
            ("key".to_string(), ConfigValue::Str("t".into())),
            ("body".to_string(), ConfigValue::Str("b".into())),
        ]);
        let b = ConfigValue::record([
            ("body".to_string(), ConfigValue::Str("b".into())),
            ("key".to_string(), ConfigValue::Str("t".into())),
        ]);
        assert_eq!(a, b);
    }

    #[test]
    fn labels_read_as_a_type() {
        assert_eq!(template_schema().label(), "list<record>");
        assert_eq!(ConfigSchema::int().label(), "integer");
        assert!(template_schema().is_composite());
        assert!(!ConfigSchema::string().is_composite());
        assert!(!ConfigSchema::Enum(vec!["a".into()]).is_composite());
    }
}

#[cfg(test)]
mod config_value_option_type_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::OptionType;

    fn templates() -> ConfigValue {
        ConfigValue::List(vec![ConfigValue::record([
            ("key".to_string(), ConfigValue::Str("t".into())),
            (
                "target".to_string(),
                ConfigValue::record([("file".to_string(), ConfigValue::Str("a.org".into()))]),
            ),
        ])])
    }

    #[test]
    fn a_record_rooted_value_round_trips_as_a_plain_toml_document() {
        let v = ConfigValue::record([
            ("key".to_string(), ConfigValue::Str("t".into())),
            ("count".to_string(), ConfigValue::Int(3)),
            ("on".to_string(), ConfigValue::Bool(true)),
        ]);
        let text = v.format();
        assert!(!text.contains("value"), "a record needs no wrapper: {text}");
        assert_eq!(ConfigValue::parse(&text), Ok(v));
    }

    #[test]
    fn a_list_rooted_value_round_trips_through_the_wrapper() {
        // `OptionType`'s contract is `parse(&v.format()) == Ok(v)`, and a
        // structured option does not get an exemption from it. A TOML document
        // cannot BE an array, so the wrapper is how a list-rooted option has a
        // text form at all — and `parse` has to undo exactly what `format` did.
        let v = templates();
        let text = v.format();
        assert!(
            text.contains("[[value]]"),
            "wrapped under the reserved key: {text}"
        );
        assert_eq!(ConfigValue::parse(&text), Ok(v));
    }

    #[test]
    fn every_kind_round_trips() {
        for v in [
            ConfigValue::Bool(true),
            ConfigValue::Int(-7),
            ConfigValue::Str("hello".into()),
            ConfigValue::List(vec![ConfigValue::Int(1), ConfigValue::Int(2)]),
            templates(),
        ] {
            assert_eq!(ConfigValue::parse(&v.format()), Ok(v.clone()), "{v:?}");
        }
    }

    #[test]
    fn a_toml_string_a_user_already_wrote_still_parses() {
        // The migration promise: an option that WAS a TOML-in-a-string does not
        // break for someone who was setting it that way. The text is unchanged;
        // what is new is that the wrapper unwraps to the list the option now
        // holds, and the result is validated against a schema.
        let text = "[[template]]\nkey = \"t\"\ntarget = { file = \"a.org\" }\n";
        let got = ConfigValue::parse(text).expect("still parses");
        let list = got.as_list().expect("the wrapper unwrapped");
        assert_eq!(list.len(), 1);
        assert_eq!(
            list[0]
                .field("target")
                .and_then(|t| t.field("file"))
                .and_then(ConfigValue::as_str),
            Some("a.org"),
        );
    }

    #[test]
    fn a_wrapper_is_not_unwrapped_when_its_payload_is_a_table() {
        // Only an ARRAY payload is a wrapper. A single-key table whose value is
        // a table is a record with one field, which is a shape an option
        // legitimately has.
        let text = "[value]\nfile = \"a.org\"\n";
        let got = ConfigValue::parse(text).unwrap();
        assert!(
            got.field("value").is_some(),
            "the `value` key survived as a field: {got:?}"
        );
    }

    #[test]
    fn any_wrapper_name_unwraps_so_an_existing_set_string_keeps_working() {
        // The migration nicety, and the reason `parse` is more generous than
        // `format`. Someone who wrote `org.capture-templates` as `[[template]]`
        // — or `agenda-sections` as `[[section]]` — keeps writing exactly that:
        // the option's value is the list itself now, and their wrapper name,
        // whatever they chose, unwraps to it. Insisting on `value` would have
        // broken every existing `:set` string for no gain, since the name
        // carries nothing the schema does not already know.
        for wrapper in ["template", "section", "command", "value"] {
            let text = format!("[[{wrapper}]]\nkey = \"t\"\n");
            let got = ConfigValue::parse(&text)
                .unwrap_or_else(|e| panic!("`{wrapper}` should parse: {e}"));
            let list = got
                .as_list()
                .unwrap_or_else(|| panic!("`{wrapper}` should unwrap to a list, got {got:?}"));
            assert_eq!(list.len(), 1);
            assert_eq!(
                list[0].field("key").and_then(ConfigValue::as_str),
                Some("t")
            );
        }
    }
}
