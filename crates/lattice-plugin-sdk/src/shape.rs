//! A Rust struct as a config schema and a config value, plus the arena
//! flattening the WIT seam needs (slice TC.4).
//!
//! Design: `docs/dev/architecture/typed-configuration.md`.
//!
//! TC.3 gave the ABI a way to carry structured configuration: an option
//! declares a schema, values cross as a tree, the host validates one against
//! the other. It also made both cross as an **arena** — a flat node list plus a
//! root index — because WIT has no recursive types. Writing an arena by hand is
//! exactly as unpleasant as it sounds; the `config-guest` fixture does it once,
//! deliberately, to prove the encoding, and no real plugin should.
//!
//! This module is what a real plugin uses instead. `#[derive(ConfigShape)]` on
//! an ordinary struct produces [`ConfigShape::schema`], [`ConfigShape::to_value`]
//! and [`ConfigShape::from_value`]; [`flatten_schema`] / [`flatten_value`] turn
//! either into an arena.
//!
//! **WIT-agnostic, like the rest of the SDK.** The types here are the SDK's own
//! mirrors, not the generated bindings — a proc-macro crate cannot name a
//! per-world WIT type, which is why `#[derive(PluginOption)]` hands back an
//! [`OptionKind`](crate::OptionKind) the plugin maps at the call site. The same
//! one-line-per-plugin tax applies here: a plugin writes one `fn` mapping
//! [`SchemaNode`] / [`ValueNode`] to its generated node types, once, not per
//! option.
//!
//! The design claim this slice makes true is that a guest's parse becomes
//! *total and mechanical* — a walk over a tree the host has already validated —
//! where before it was a bespoke text parser per option. Without the derive that
//! claim is aspirational and the tree is simply a worse blob.
//!
//! # Examples
//!
//! A struct describes itself, round-trips through a [`Value`], and flattens to
//! the arena form that crosses the WIT boundary:
//!
//! ```
//! use lattice_plugin_sdk::ConfigShape;
//! use lattice_plugin_sdk::shape::{
//!     ConfigShape as _, Schema, Value, flatten_value, unflatten_value,
//! };
//!
//! /// Where a capture lands.
//! #[derive(Debug, PartialEq, ConfigShape)]
//! struct Target {
//!     /// The file it is appended to.
//!     file: String,
//!     /// Insert under this headline instead of appending.
//!     headline: Option<String>,
//! }
//!
//! // The schema: a record, fields kebab-cased, `Option<T>` = not required.
//! let Schema::Record(fields) = Target::schema() else { unreachable!() };
//! assert_eq!(fields[0].name, "file");
//! assert!(fields[0].required);
//! assert!(!fields[1].required);
//! assert_eq!(fields[1].doc, "Insert under this headline instead of appending.");
//!
//! // The value: an absent optional field is omitted, not emitted empty.
//! let t = Target { file: "refile.org".into(), headline: None };
//! let v = t.to_value();
//! assert_eq!(v.field("file").and_then(Value::as_str), Some("refile.org"));
//! assert_eq!(v.field("headline"), None);
//! assert_eq!(Target::from_value(&v), Ok(t));
//!
//! // The arena: what actually crosses the boundary, and back.
//! let (nodes, root) = flatten_value(&v);
//! assert_eq!(unflatten_value(&nodes, root), Ok(v));
//! ```

use std::collections::BTreeMap;

/// The leaf kinds. Mirrors `lattice_config::ScalarKind` and the WIT
/// `option-type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScalarKind {
    /// `true` / `false`; carried as [`Value::Bool`].
    Bool,
    /// A signed 64-bit integer; carried as [`Value::Int`].
    Int,
    /// A UTF-8 string; carried as [`Value::Str`].
    Str,
}

/// One field of a [`Schema::Record`]. `doc` is per field because that is what
/// `:describe-option` and `:customize` render beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// The field's key as a config file spells it. The derive kebab-cases the
    /// Rust identifier (`max_depth` → `max-depth`) and strips a raw-identifier
    /// prefix (`r#match` → `match`), so this is the wire name, not the Rust one.
    pub name: String,
    /// The shape the field's value must have.
    pub schema: Schema,
    /// Derived from the Rust type: an `Option<T>` field is optional, everything
    /// else is required. That mapping is the whole reason the derive can decide
    /// this without an attribute — the type already says it.
    pub required: bool,
    /// The field's `///` doc-comment, lines joined and trimmed; empty when the
    /// field has none.
    pub doc: String,
}

/// The declared shape of a value. Mirrors `lattice_config::ConfigSchema`.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::shape::{ConfigShape, ScalarKind, Schema};
///
/// // The constructors are shorthands for the scalar / list forms.
/// assert_eq!(Schema::int(), Schema::Scalar(ScalarKind::Int));
/// assert_eq!(<Vec<String> as ConfigShape>::schema(), Schema::list(Schema::string()));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Schema {
    /// A single leaf of the given kind.
    Scalar(ScalarKind),
    /// A closed set of strings; a value must be a [`Value::Str`] spelling one
    /// of them. `#[derive(ConfigShape)]` on an all-unit enum produces this,
    /// variants kebab-cased in declaration order — and it is what lets
    /// `:customize` offer a picker instead of a text field.
    Enum(Vec<String>),
    /// A homogeneous list whose every element has the inner shape.
    List(Box<Schema>),
    /// A struct-like record: named fields, each with its own shape, doc and
    /// required flag. Field order is declaration order.
    Record(Vec<Field>),
}

impl Schema {
    /// `Schema::Scalar(ScalarKind::Str)`.
    pub fn string() -> Self {
        Schema::Scalar(ScalarKind::Str)
    }
    /// `Schema::Scalar(ScalarKind::Int)`.
    pub fn int() -> Self {
        Schema::Scalar(ScalarKind::Int)
    }
    /// `Schema::Scalar(ScalarKind::Bool)`.
    pub fn bool() -> Self {
        Schema::Scalar(ScalarKind::Bool)
    }
    /// A list whose elements have shape `inner` (boxes it for you).
    pub fn list(inner: Schema) -> Self {
        Schema::List(Box::new(inner))
    }
}

/// A value shaped by a [`Schema`]. Mirrors `lattice_config::ConfigValue`.
///
/// `Record` is a `BTreeMap` for the same reason the host's is: a value written
/// in TOML (unordered) and the same value built from a struct must compare
/// equal, or "the same config" is two values.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::shape::Value;
///
/// let v = Value::record([
///     ("width".to_string(), Value::Int(80)),
///     ("wrap".to_string(), Value::Bool(true)),
/// ]);
/// assert_eq!(v.field("width").and_then(Value::as_int), Some(80));
/// // Accessors are kind-checked: the wrong kind is `None`, not a coercion.
/// assert_eq!(v.field("wrap").and_then(Value::as_int), None);
/// assert_eq!(v.kind_label(), "record");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// A boolean leaf.
    Bool(bool),
    /// An integer leaf.
    Int(i64),
    /// A string leaf — also how a [`Schema::Enum`] member is carried.
    Str(String),
    /// An ordered list of values.
    List(Vec<Value>),
    /// Named fields, key-ordered (see the type docs for why). An optional field
    /// that is absent has no entry at all.
    Record(BTreeMap<String, Value>),
}

impl Value {
    /// Build a [`Value::Record`] from `(key, value)` pairs. A repeated key keeps
    /// the last value (it is collected into a `BTreeMap`).
    pub fn record(fields: impl IntoIterator<Item = (String, Value)>) -> Self {
        Value::Record(fields.into_iter().collect())
    }

    /// The human name of this value's kind — `"boolean"`, `"integer"`,
    /// `"string"`, `"list"` or `"record"` — as used in [`ShapeError`] messages
    /// (`expected integer, got string`).
    pub fn kind_label(&self) -> &'static str {
        match self {
            Value::Bool(_) => "boolean",
            Value::Int(_) => "integer",
            Value::Str(_) => "string",
            Value::List(_) => "list",
            Value::Record(_) => "record",
        }
    }

    /// The boolean, if this is a [`Value::Bool`]; `None` for any other kind.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
    /// The integer, if this is a [`Value::Int`]; `None` for any other kind.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    /// The string, if this is a [`Value::Str`]; `None` for any other kind.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }
    /// The elements, if this is a [`Value::List`]; `None` for any other kind.
    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(items) => Some(items),
            _ => None,
        }
    }
    /// The field named `name` (its wire, kebab-case spelling), if this is a
    /// [`Value::Record`] that has it. `None` both for a missing field and for
    /// a value that is not a record at all.
    pub fn field(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Record(map) => map.get(name),
            _ => None,
        }
    }
}

/// A failed [`ConfigShape::from_value`], carrying **where**.
///
/// The host validates the tree against the schema before a guest ever sees it,
/// so in practice a guest's `from_value` fails only on a value the host could
/// not have checked — a type the schema calls a string but the guest wants to
/// interpret further (an enum spelled as a string, a path, a duration). Those
/// are exactly the cases where a path matters, so it is carried rather than
/// dropped.
///
/// `Display` renders `path: message`, or just `message` at the root.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::shape::{ConfigShape, Value};
///
/// let v = Value::List(vec![Value::Int(1), Value::Str("two".into())]);
/// let err = <Vec<i64>>::from_value(&v).unwrap_err();
/// assert_eq!(err.path, "[1]");
/// assert_eq!(err.to_string(), "[1]: expected integer, got string");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapeError {
    /// Where the failure is, outermost segment first: field names joined with
    /// `.`, list indices as `[i]` with no dot before them
    /// (`templates[2].target.file`). Empty when the root value itself failed.
    pub path: String,
    /// What went wrong at that location, e.g. `expected integer, got string` or
    /// `required field is missing`.
    pub message: String,
}

impl ShapeError {
    /// An error at the root (empty [`path`](Self::path)); callers above it
    /// prepend their segment with [`under`](Self::under).
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            path: String::new(),
            message: message.into(),
        }
    }

    /// Prepend a segment as this error unwinds back up the walk. The derive
    /// calls it per field, so a leaf failure arrives at the top with the full
    /// path assembled and no field having had to know where it lives.
    ///
    /// A `.` separator is inserted unless the existing path begins with an
    /// index segment (`[`), so indices attach directly to their list.
    ///
    /// # Examples
    ///
    /// ```
    /// use lattice_plugin_sdk::shape::ShapeError;
    ///
    /// // Segments are discovered inside-out, so they are prepended.
    /// let e = ShapeError::new("boom").under("file").under("[0]").under("targets");
    /// assert_eq!(e.path, "targets[0].file");
    /// ```
    pub fn under(mut self, segment: &str) -> Self {
        self.path = if self.path.is_empty() {
            segment.to_string()
        } else if self.path.starts_with('[') {
            format!("{segment}{}", self.path)
        } else {
            format!("{segment}.{}", self.path)
        };
        self
    }
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.path.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

impl std::error::Error for ShapeError {}

/// A type that can describe its own configuration shape and move through it.
///
/// Implemented by `#[derive(ConfigShape)]` for structs, and by hand for the
/// primitives and containers below. The three methods have to agree:
/// `to_value` must produce something `schema` accepts, and `from_value` must
/// invert `to_value`. A type where they disagree is a silently lossy option,
/// which is the failure the SDK's own round-trip test exists to catch.
///
/// Provided impls: `bool`, `i64`, `String`, `Vec<T>` and `Option<T>` (the last
/// meaning "not required" at field position — see its impl). The derive
/// supports structs with named fields (→ [`Schema::Record`]) and all-unit enums
/// (→ [`Schema::Enum`]); tuple structs, data-carrying enums and unions are
/// compile errors. The derive is re-exported at the crate root as
/// `lattice_plugin_sdk::ConfigShape`, beside this trait's path in `shape`.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::ConfigShape;
/// use lattice_plugin_sdk::shape::{ConfigShape as _, Schema, Value};
///
/// /// How a capture is filed.
/// #[derive(Debug, PartialEq, ConfigShape)]
/// enum Disposition { Append, FileUnder }
///
/// assert_eq!(
///     Disposition::schema(),
///     Schema::Enum(vec!["append".into(), "file-under".into()]),
/// );
/// assert_eq!(
///     Disposition::from_value(&Value::Str("file-under".into())),
///     Ok(Disposition::FileUnder),
/// );
/// assert!(Disposition::from_value(&Value::Str("sideways".into())).is_err());
/// ```
pub trait ConfigShape: Sized {
    /// The declared shape, as sent to the host when the option is registered.
    fn schema() -> Schema;
    /// This value as a tree the host can validate against
    /// [`schema`](Self::schema). Must always be accepted by it.
    fn to_value(&self) -> Value;
    /// Read a value back; the inverse of [`to_value`](Self::to_value).
    ///
    /// # Errors
    ///
    /// A [`ShapeError`] carrying the path to the offending node when a kind is
    /// wrong, a required field is missing, or an enum string names no variant.
    fn from_value(value: &Value) -> Result<Self, ShapeError>;
}

impl ConfigShape for bool {
    fn schema() -> Schema {
        Schema::bool()
    }
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
    fn from_value(value: &Value) -> Result<Self, ShapeError> {
        value
            .as_bool()
            .ok_or_else(|| ShapeError::new(format!("expected boolean, got {}", value.kind_label())))
    }
}

impl ConfigShape for i64 {
    fn schema() -> Schema {
        Schema::int()
    }
    fn to_value(&self) -> Value {
        Value::Int(*self)
    }
    fn from_value(value: &Value) -> Result<Self, ShapeError> {
        value
            .as_int()
            .ok_or_else(|| ShapeError::new(format!("expected integer, got {}", value.kind_label())))
    }
}

impl ConfigShape for String {
    fn schema() -> Schema {
        Schema::string()
    }
    fn to_value(&self) -> Value {
        Value::Str(self.clone())
    }
    fn from_value(value: &Value) -> Result<Self, ShapeError> {
        value
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ShapeError::new(format!("expected string, got {}", value.kind_label())))
    }
}

impl<T: ConfigShape> ConfigShape for Vec<T> {
    fn schema() -> Schema {
        Schema::list(T::schema())
    }
    fn to_value(&self) -> Value {
        Value::List(self.iter().map(T::to_value).collect())
    }
    fn from_value(value: &Value) -> Result<Self, ShapeError> {
        let items = value
            .as_list()
            .ok_or_else(|| ShapeError::new(format!("expected list, got {}", value.kind_label())))?;
        items
            .iter()
            .enumerate()
            .map(|(i, item)| T::from_value(item).map_err(|e| e.under(&format!("[{i}]"))))
            .collect()
    }
}

/// `Option<T>` is how a field says it is **not required**.
///
/// The schema of an `Option<T>` is `T`'s — optionality is a property of the
/// FIELD, not of the value, which is why [`Field::required`] carries it and
/// this impl does not wrap the schema in anything. A bare `Option<T>` used as a
/// whole option's type therefore describes itself as `T`; `None` is then
/// indistinguishable from absent, which is the correct reading at a leaf and
/// the reason the derive only consults this at field position.
impl<T: ConfigShape> ConfigShape for Option<T> {
    fn schema() -> Schema {
        T::schema()
    }
    fn to_value(&self) -> Value {
        match self {
            Some(v) => v.to_value(),
            // Unreachable through the derive, which omits the field entirely
            // rather than emitting a placeholder. Present because the trait is
            // total, and an empty record is the least surprising stand-in.
            None => Value::Record(BTreeMap::new()),
        }
    }
    fn from_value(value: &Value) -> Result<Self, ShapeError> {
        T::from_value(value).map(Some)
    }
}

// ── Arena flattening ───────────────────────────────────────────────────────
//
// WIT has no recursive types, so a schema and a value cross as a flat node list
// plus a root index. These produce that form in the SDK's own node types; the
// plugin maps them to its generated ones in one function, once.

/// One node of a flattened [`Schema`]. Child links are indices into the node
/// list [`flatten_schema`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaNode {
    /// [`Schema::Scalar`]; a leaf.
    Scalar(ScalarKind),
    /// [`Schema::Enum`]; a leaf carrying the allowed strings.
    Enum(Vec<String>),
    /// [`Schema::List`]; the index of the element schema's node.
    List(u32),
    /// [`Schema::Record`]; one [`FieldNode`] per field, in declaration order.
    Record(Vec<FieldNode>),
}

/// A [`Field`] with its schema as an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldNode {
    /// [`Field::name`] — the wire (kebab-case) key.
    pub name: String,
    /// Index of the field's schema node in the same arena.
    pub schema: u32,
    /// [`Field::required`].
    pub required: bool,
    /// [`Field::doc`].
    pub doc: String,
}

/// One node of a flattened [`Value`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueNode {
    /// [`Value::Bool`].
    Bool(bool),
    /// [`Value::Int`].
    Int(i64),
    /// [`Value::Str`].
    Str(String),
    /// [`Value::List`]; the element nodes' indices, in order.
    List(Vec<u32>),
    /// [`Value::Record`]; `(key, node index)` pairs in key order (the
    /// `BTreeMap`'s iteration order when flattened).
    Record(Vec<(String, u32)>),
}

/// Flatten a schema to `(nodes, root)`.
///
/// Post-order: every child is pushed before its parent, so an index is never
/// handed out for a slot still being built. No dedup — a schema crosses once
/// per option per load, and hashing every subtree to save a few nodes on a
/// cold path is the wrong trade.
///
/// The root is therefore always the last node: `root == nodes.len() - 1`.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::shape::{Schema, SchemaNode, ScalarKind, flatten_schema};
///
/// let (nodes, root) = flatten_schema(&Schema::list(Schema::int()));
/// assert_eq!(nodes, vec![SchemaNode::Scalar(ScalarKind::Int), SchemaNode::List(0)]);
/// assert_eq!(root, 1);
/// ```
pub fn flatten_schema(schema: &Schema) -> (Vec<SchemaNode>, u32) {
    let mut nodes = Vec::new();
    let root = push_schema(&mut nodes, schema);
    (nodes, root)
}

fn push_schema(nodes: &mut Vec<SchemaNode>, schema: &Schema) -> u32 {
    let node = match schema {
        Schema::Scalar(k) => SchemaNode::Scalar(*k),
        Schema::Enum(forms) => SchemaNode::Enum(forms.clone()),
        Schema::List(inner) => SchemaNode::List(push_schema(nodes, inner)),
        Schema::Record(fields) => SchemaNode::Record(
            fields
                .iter()
                .map(|f| FieldNode {
                    name: f.name.clone(),
                    schema: push_schema(nodes, &f.schema),
                    required: f.required,
                    doc: f.doc.clone(),
                })
                .collect(),
        ),
    };
    nodes.push(node);
    (nodes.len() - 1) as u32
}

/// Flatten a value to `(nodes, root)`. See [`flatten_schema`].
pub fn flatten_value(value: &Value) -> (Vec<ValueNode>, u32) {
    let mut nodes = Vec::new();
    let root = push_value(&mut nodes, value);
    (nodes, root)
}

fn push_value(nodes: &mut Vec<ValueNode>, value: &Value) -> u32 {
    let node = match value {
        Value::Bool(b) => ValueNode::Bool(*b),
        Value::Int(i) => ValueNode::Int(*i),
        Value::Str(s) => ValueNode::Str(s.clone()),
        Value::List(items) => ValueNode::List(items.iter().map(|v| push_value(nodes, v)).collect()),
        Value::Record(map) => ValueNode::Record(
            map.iter()
                .map(|(k, v)| (k.clone(), push_value(nodes, v)))
                .collect(),
        ),
    };
    nodes.push(node);
    (nodes.len() - 1) as u32
}

/// Rebuild a [`Value`] from an arena — the read direction, for a guest turning a
/// `get-option-value` answer back into a tree before `from_value`.
///
/// Range- and cycle-checked, for the host's reasons in reverse: the arena a
/// guest receives is well-formed by construction today, but a guest that
/// assumed so and recursed would be one host bug away from an unbounded walk in
/// wasm, where the failure is a trap the user sees as the plugin crashing.
///
/// # Errors
///
/// A [`ShapeError`] (with an empty path) when any reachable index is out of
/// range or a node is reached again from its own descendants. A node shared by
/// two siblings is *not* a cycle and is accepted.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::shape::{Value, ValueNode, unflatten_value};
///
/// let nodes = [ValueNode::Int(7), ValueNode::List(vec![0, 0])];
/// assert_eq!(
///     unflatten_value(&nodes, 1),
///     Ok(Value::List(vec![Value::Int(7), Value::Int(7)])),
/// );
///
/// // A self-referencing list is refused rather than walked forever.
/// assert!(unflatten_value(&[ValueNode::List(vec![0])], 0).is_err());
/// ```
pub fn unflatten_value(nodes: &[ValueNode], root: u32) -> Result<Value, ShapeError> {
    fn go(nodes: &[ValueNode], i: u32, on_path: &mut Vec<u32>) -> Result<Value, ShapeError> {
        let node = nodes.get(i as usize).ok_or_else(|| {
            ShapeError::new(format!(
                "node index {i} is out of range ({} nodes)",
                nodes.len()
            ))
        })?;
        if on_path.contains(&i) {
            return Err(ShapeError::new(format!("node index {i} is a cycle")));
        }
        on_path.push(i);
        let out = match node {
            ValueNode::Bool(b) => Value::Bool(*b),
            ValueNode::Int(n) => Value::Int(*n),
            ValueNode::Str(s) => Value::Str(s.clone()),
            ValueNode::List(children) => Value::List(
                children
                    .iter()
                    .map(|c| go(nodes, *c, on_path))
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            ValueNode::Record(fields) => {
                let mut map = BTreeMap::new();
                for (k, c) in fields {
                    map.insert(k.clone(), go(nodes, *c, on_path)?);
                }
                Value::Record(map)
            }
        };
        on_path.pop();
        Ok(out)
    }
    go(nodes, root, &mut Vec::new())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn primitives_describe_and_round_trip() {
        assert_eq!(<bool as ConfigShape>::schema(), Schema::bool());
        assert_eq!(<i64 as ConfigShape>::schema(), Schema::int());
        assert_eq!(<String as ConfigShape>::schema(), Schema::string());

        assert_eq!(bool::from_value(&true.to_value()), Ok(true));
        assert_eq!(i64::from_value(&(-7i64).to_value()), Ok(-7));
        assert_eq!(
            String::from_value(&"x".to_string().to_value()),
            Ok("x".to_string())
        );
    }

    #[test]
    fn a_wrong_kind_says_what_it_wanted_and_what_it_got() {
        let err = i64::from_value(&Value::Str("4".into())).unwrap_err();
        assert!(err.message.contains("expected integer"), "{err}");
        assert!(err.message.contains("string"), "{err}");
    }

    #[test]
    fn a_list_failure_carries_the_index() {
        // The whole point of `under`: a leaf that fails deep in a list must
        // arrive at the top knowing where it was, without any element having
        // been told its own position.
        let v = Value::List(vec![Value::Int(1), Value::Int(2), Value::Str("no".into())]);
        let err = <Vec<i64> as ConfigShape>::from_value(&v).unwrap_err();
        assert_eq!(err.path, "[2]");
        assert_eq!(err.to_string(), "[2]: expected integer, got string");
    }

    #[test]
    fn nested_paths_compose_in_reading_order() {
        // `under` prepends, because the walk discovers segments from the
        // inside out. A naive append would spell `file.target[1]`.
        let e = ShapeError::new("boom")
            .under("file")
            .under("target")
            .under("[1]");
        assert_eq!(e.path, "[1].target.file");
    }

    #[test]
    fn an_arena_round_trips_through_flatten_and_back() {
        let value = Value::List(vec![Value::record([
            ("key".to_string(), Value::Str("t".into())),
            (
                "target".to_string(),
                Value::record([("file".to_string(), Value::Str("a.org".into()))]),
            ),
        ])]);
        let (nodes, root) = flatten_value(&value);
        assert_eq!(unflatten_value(&nodes, root).unwrap(), value);
    }

    #[test]
    fn flattening_puts_children_before_their_parent() {
        // Not cosmetic: an index handed out before its slot exists is an arena
        // the host reads as out-of-range, and the guest would have no way to
        // tell which of its options was malformed.
        let (nodes, root) = flatten_value(&Value::List(vec![Value::Int(1), Value::Int(2)]));
        assert_eq!(root as usize, nodes.len() - 1, "the root is pushed last");
        match &nodes[root as usize] {
            ValueNode::List(children) => {
                assert!(
                    children.iter().all(|c| *c < root),
                    "every child index precedes its parent"
                );
            }
            other => panic!("expected a list node, got {other:?}"),
        }
    }

    #[test]
    fn unflattening_refuses_a_bad_arena_rather_than_trusting_it() {
        assert!(unflatten_value(&[], 0).is_err());
        assert!(unflatten_value(&[ValueNode::List(vec![9])], 0).is_err());
        // A cycle: in wasm an unbounded walk is a trap the user sees as the
        // plugin crashing, so the guest checks even though today's host cannot
        // produce one.
        let cyclic = [ValueNode::List(vec![1]), ValueNode::List(vec![0])];
        assert!(unflatten_value(&cyclic, 0).is_err());
    }
}
