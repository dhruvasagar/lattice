//! TC.3 — the config schema / value boundary: arena on the wire, tree in the host.
//!
//! Design: `docs/dev/architecture/typed-configuration.md`.
//!
//! **WIT has no recursive types.** A schema and a value are both trees, and the
//! obvious spelling — a variant whose arm holds another variant — does not
//! parse: "type `config-schema` depends on itself". So both cross as an arena:
//! a flat list of nodes plus the index of the root, children referenced by
//! index. This module is where that arena becomes a tree and back.
//!
//! ## What the boundary has to defend against
//!
//! An arena is guest-controlled input in a shape that *invites* two failures no
//! other boundary in this crate has:
//!
//! - **an index that points nowhere** — trivially rejected, and
//! - **an index that points back up the tree**, i.e. a cycle. Following one
//!   naively is unbounded recursion on the host's stack, from a value a plugin
//!   chose. The tree-building walk therefore carries the set of nodes currently
//!   on its own path and refuses a node it is already inside.
//!
//! Both are typed errors, never a trap and never a panic: a malformed
//! declaration registers nothing and the plugin is told so, the same contract
//! `register-option` has for a default that does not parse.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use lattice_config::{ConfigSchema, ConfigValue, ScalarKind, SchemaField};

use crate::config_host::bindings::lattice::plugin_host::config as wit;

/// What went wrong turning an arena into a tree. Rendered into the `false` /
/// warn paths the config seam already uses — a guest never traps over one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArenaError {
    /// A child link points outside the node list.
    OutOfRange { index: u32, len: usize },
    /// A child link points at a node already on the path from the root. Left as
    /// its own variant rather than folded into `OutOfRange` because the two say
    /// different things about the guest that sent it: one is an off-by-one, the
    /// other is a structure that cannot exist.
    Cycle { index: u32 },
}

impl std::fmt::Display for ArenaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArenaError::OutOfRange { index, len } => {
                write!(f, "node index {index} is out of range (arena has {len})")
            }
            ArenaError::Cycle { index } => {
                write!(f, "node index {index} is a cycle — a schema is a tree")
            }
        }
    }
}

impl std::error::Error for ArenaError {}

fn scalar_kind_from_wit(ty: wit::OptionType) -> ScalarKind {
    match ty {
        wit::OptionType::Boolean => ScalarKind::Bool,
        wit::OptionType::Integer => ScalarKind::Int,
        wit::OptionType::String => ScalarKind::Str,
    }
}

fn scalar_kind_to_wit(kind: ScalarKind) -> wit::OptionType {
    match kind {
        ScalarKind::Bool => wit::OptionType::Boolean,
        ScalarKind::Int => wit::OptionType::Integer,
        ScalarKind::Str => wit::OptionType::String,
    }
}

/// Resolve one index against `len`, or say why not.
fn checked(index: u32, len: usize) -> Result<usize, ArenaError> {
    let i = index as usize;
    if i < len {
        Ok(i)
    } else {
        Err(ArenaError::OutOfRange { index, len })
    }
}

/// A guest's schema arena as a [`ConfigSchema`].
pub fn schema_from_wit(arena: &wit::ConfigSchema) -> Result<ConfigSchema, ArenaError> {
    let mut on_path = BTreeSet::new();
    schema_node(arena, arena.root, &mut on_path)
}

fn schema_node(
    arena: &wit::ConfigSchema,
    index: u32,
    on_path: &mut BTreeSet<u32>,
) -> Result<ConfigSchema, ArenaError> {
    let i = checked(index, arena.nodes.len())?;
    // The cycle guard. `on_path` is the chain from the root to here, not every
    // node seen — a schema may legitimately reuse a node from two places (two
    // fields of the same shape), and forbidding that would reject a DAG, which
    // is a perfectly good encoding of a tree.
    if !on_path.insert(index) {
        return Err(ArenaError::Cycle { index });
    }
    let out = match &arena.nodes[i] {
        wit::SchemaNode::Scalar(ty) => ConfigSchema::Scalar(scalar_kind_from_wit(*ty)),
        wit::SchemaNode::EnumOf(forms) => ConfigSchema::Enum(forms.clone()),
        wit::SchemaNode::ListOf(child) => ConfigSchema::list(schema_node(arena, *child, on_path)?),
        wit::SchemaNode::Record(fields) => {
            let mut out = Vec::with_capacity(fields.len());
            for f in fields {
                out.push(SchemaField {
                    name: f.name.clone(),
                    schema: schema_node(arena, f.schema, on_path)?,
                    required: f.required,
                    doc: f.doc.clone(),
                });
            }
            ConfigSchema::Record(out)
        }
    };
    on_path.remove(&index);
    Ok(out)
}

/// A guest's value arena as a [`ConfigValue`].
pub fn value_from_wit(arena: &wit::ConfigValue) -> Result<ConfigValue, ArenaError> {
    let mut on_path = BTreeSet::new();
    value_node(arena, arena.root, &mut on_path)
}

fn value_node(
    arena: &wit::ConfigValue,
    index: u32,
    on_path: &mut BTreeSet<u32>,
) -> Result<ConfigValue, ArenaError> {
    let i = checked(index, arena.nodes.len())?;
    if !on_path.insert(index) {
        return Err(ArenaError::Cycle { index });
    }
    let out = match &arena.nodes[i] {
        wit::ValueNode::Bool(b) => ConfigValue::Bool(*b),
        wit::ValueNode::Int(n) => ConfigValue::Int(*n),
        wit::ValueNode::String(s) => ConfigValue::Str(s.clone()),
        wit::ValueNode::List(children) => {
            let mut out = Vec::with_capacity(children.len());
            for c in children {
                out.push(value_node(arena, *c, on_path)?);
            }
            ConfigValue::List(out)
        }
        wit::ValueNode::Record(fields) => {
            let mut map = BTreeMap::new();
            for (name, child) in fields {
                map.insert(name.clone(), value_node(arena, *child, on_path)?);
            }
            ConfigValue::Record(map)
        }
    };
    on_path.remove(&index);
    Ok(out)
}

/// A [`ConfigValue`] as an arena for the guest.
///
/// Emits one node per tree node — no sharing, no dedup. A `get-option-value`
/// answer is read once and dropped, so the only thing dedup would buy is a
/// smaller copy, at the cost of a hash of every subtree on a path that is
/// already cold.
pub fn value_to_wit(value: &ConfigValue) -> wit::ConfigValue {
    let mut nodes = Vec::new();
    let root = push_value(&mut nodes, value);
    wit::ConfigValue { nodes, root }
}

fn push_value(nodes: &mut Vec<wit::ValueNode>, value: &ConfigValue) -> u32 {
    // Children are pushed BEFORE the parent's own node is reserved, so an index
    // is never handed out for a slot that is still being built.
    let node = match value {
        ConfigValue::Bool(b) => wit::ValueNode::Bool(*b),
        ConfigValue::Int(n) => wit::ValueNode::Int(*n),
        ConfigValue::Str(s) => wit::ValueNode::String(s.clone()),
        ConfigValue::List(items) => {
            let children: Vec<u32> = items.iter().map(|v| push_value(nodes, v)).collect();
            wit::ValueNode::List(children)
        }
        ConfigValue::Record(map) => {
            let fields: Vec<(String, u32)> = map
                .iter()
                .map(|(k, v)| (k.clone(), push_value(nodes, v)))
                .collect();
            wit::ValueNode::Record(fields)
        }
    };
    nodes.push(node);
    (nodes.len() - 1) as u32
}

/// A [`ConfigSchema`] as an arena for the guest. The mirror of
/// [`schema_from_wit`], for `:describe-option`-shaped reads from a plugin.
pub fn schema_to_wit(schema: &ConfigSchema) -> wit::ConfigSchema {
    let mut nodes = Vec::new();
    let root = push_schema(&mut nodes, schema);
    wit::ConfigSchema { nodes, root }
}

fn push_schema(nodes: &mut Vec<wit::SchemaNode>, schema: &ConfigSchema) -> u32 {
    let node = match schema {
        ConfigSchema::Scalar(k) => wit::SchemaNode::Scalar(scalar_kind_to_wit(*k)),
        ConfigSchema::Enum(forms) => wit::SchemaNode::EnumOf(forms.clone()),
        ConfigSchema::List(inner) => wit::SchemaNode::ListOf(push_schema(nodes, inner)),
        ConfigSchema::Record(fields) => {
            let out: Vec<wit::SchemaField> = fields
                .iter()
                .map(|f| wit::SchemaField {
                    name: f.name.clone(),
                    schema: push_schema(nodes, &f.schema),
                    required: f.required,
                    doc: f.doc.clone(),
                })
                .collect();
            wit::SchemaNode::Record(out)
        }
    };
    nodes.push(node);
    (nodes.len() - 1) as u32
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn templates() -> ConfigSchema {
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
            SchemaField::new("body", ConfigSchema::string(), "body").optional(),
        ]))
    }

    #[test]
    fn a_nested_schema_survives_the_arena_round_trip() {
        // Two levels of record inside a list — the shape the whole design
        // exists for, and the one a flattening is most likely to mangle.
        let schema = templates();
        let round = schema_from_wit(&schema_to_wit(&schema)).unwrap();
        assert_eq!(round, schema);
    }

    #[test]
    fn a_nested_value_survives_the_arena_round_trip() {
        let value = ConfigValue::List(vec![ConfigValue::record([
            ("key".to_string(), ConfigValue::Str("t".into())),
            (
                "target".to_string(),
                ConfigValue::record([("file".to_string(), ConfigValue::Str("a.org".into()))]),
            ),
        ])]);
        let round = value_from_wit(&value_to_wit(&value)).unwrap();
        assert_eq!(round, value);
    }

    #[test]
    fn every_scalar_kind_crosses_as_itself() {
        // A kind that collapsed to a string on the way out would make
        // `set-option-value` on an integer option fail for a value the guest
        // built correctly — and the failure would read as the guest's fault.
        for v in [
            ConfigValue::Bool(true),
            ConfigValue::Bool(false),
            ConfigValue::Int(0),
            ConfigValue::Int(-7),
            ConfigValue::Int(i64::MAX),
            ConfigValue::Str(String::new()),
            ConfigValue::Str("x".into()),
        ] {
            assert_eq!(value_from_wit(&value_to_wit(&v)).unwrap(), v);
        }
        for s in [
            ConfigSchema::bool(),
            ConfigSchema::int(),
            ConfigSchema::string(),
            ConfigSchema::Enum(vec!["a".into(), "b".into()]),
        ] {
            assert_eq!(schema_from_wit(&schema_to_wit(&s)).unwrap(), s);
        }
    }

    #[test]
    fn an_out_of_range_index_is_refused_rather_than_indexed() {
        let arena = wit::ConfigValue {
            nodes: vec![wit::ValueNode::List(vec![9])],
            root: 0,
        };
        assert_eq!(
            value_from_wit(&arena),
            Err(ArenaError::OutOfRange { index: 9, len: 1 })
        );
        // …including the root itself, which is the index nobody remembers to
        // check because it does not arrive through a child link.
        let empty = wit::ConfigValue {
            nodes: vec![],
            root: 0,
        };
        assert!(matches!(
            value_from_wit(&empty),
            Err(ArenaError::OutOfRange { .. })
        ));
    }

    #[test]
    fn a_cycle_is_refused_rather_than_followed() {
        // The failure this boundary exists to prevent: a guest-chosen value
        // that recurses forever on the HOST's stack. A test that only checked
        // for an error would hang before it could fail, so the guard has to be
        // in the walk itself rather than in a depth counter after the fact.
        let arena = wit::ConfigValue {
            nodes: vec![wit::ValueNode::List(vec![1]), wit::ValueNode::List(vec![0])],
            root: 0,
        };
        assert_eq!(value_from_wit(&arena), Err(ArenaError::Cycle { index: 0 }));

        let schema = wit::ConfigSchema {
            nodes: vec![wit::SchemaNode::ListOf(0)],
            root: 0,
        };
        assert_eq!(
            schema_from_wit(&schema),
            Err(ArenaError::Cycle { index: 0 })
        );
    }

    #[test]
    fn sharing_a_node_from_two_places_is_not_a_cycle() {
        // The guard tracks the path from the root, not every node seen. A guest
        // that emits one `string` node and points three fields at it has sent a
        // DAG, which is a perfectly good encoding of a tree — rejecting it
        // would punish exactly the encoding a careful generator produces.
        let arena = wit::ConfigSchema {
            nodes: vec![
                wit::SchemaNode::Scalar(wit::OptionType::String),
                wit::SchemaNode::Record(vec![
                    wit::SchemaField {
                        name: "a".into(),
                        schema: 0,
                        required: true,
                        doc: String::new(),
                    },
                    wit::SchemaField {
                        name: "b".into(),
                        schema: 0,
                        required: true,
                        doc: String::new(),
                    },
                ]),
            ],
            root: 1,
        };
        let got = schema_from_wit(&arena).unwrap();
        assert_eq!(
            got,
            ConfigSchema::record([
                SchemaField::new("a", ConfigSchema::string(), ""),
                SchemaField::new("b", ConfigSchema::string(), ""),
            ])
        );
    }

    #[test]
    fn field_metadata_crosses_intact() {
        // `required` and `doc` are what `:customize` renders and what makes an
        // omitted field an error rather than a default. A round-trip that kept
        // the shape and dropped these would look correct in every structural
        // assertion above.
        let schema = templates();
        let round = schema_from_wit(&schema_to_wit(&schema)).unwrap();
        let ConfigSchema::List(inner) = &round else {
            panic!("expected a list");
        };
        let ConfigSchema::Record(fields) = inner.as_ref() else {
            panic!("expected a record");
        };
        let body = fields.iter().find(|f| f.name == "body").unwrap();
        assert!(!body.required, "the optional field stayed optional");
        let key = fields.iter().find(|f| f.name == "key").unwrap();
        assert!(key.required);
        assert_eq!(key.doc, "the key to press", "per-field docs cross");
    }
}
