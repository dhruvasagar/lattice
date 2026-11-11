//! A tool's input as a Rust struct, for the `llm-tools` world (slice AT.5e).
//!
//! Design: `docs/dev/architecture/llm-assistant-tools.md` §5.
//!
//! A tool the assistant can call declares its input as JSON Schema text
//! (`llm-tool.input-schema`) and receives the model's input as JSON text
//! (`call(name, args)`). Written by hand those are two descriptions of one
//! thing, and they drift: a field the schema calls optional that the parse
//! requires fails at runtime, on the model's call, where nobody is looking.
//!
//! This module derives both from one struct. `#[derive(ConfigShape)]` already
//! turns a struct into a schema with a doc per field and an `Option<T>` for
//! what is not required; [`input_schema`] writes that schema as JSON Schema,
//! the field docs as `description`s (which a small model leans on), and
//! [`parse_args`] reads the model's JSON back into the struct, with an error
//! that names the field, which goes back to the model as the call's error.
//!
//! **WIT-agnostic, like the rest of the SDK.** Both functions deal in strings:
//! the guest still implements the world's `tools` and `call` itself, putting
//! [`input_schema`]'s text in its `llm-tool` and passing `args` to
//! [`parse_args`]. Nothing here names a generated type, so it adds no
//! capability that is not on the wire.
//!
//! Field names are the derive's wire names, kebab-cased (`max_depth` becomes
//! `max-depth`), the same in the schema and the parse.
//!
//! # Examples
//!
//! ```
//! use lattice_plugin_sdk::ConfigShape;
//! use lattice_plugin_sdk::llm::{input_schema, parse_args};
//!
//! /// What `org.query` takes.
//! #[derive(Debug, PartialEq, ConfigShape)]
//! struct Query {
//!     /// A match expression, such as `+work/TODO`.
//!     r#match: String,
//!     /// The most entries to return.
//!     limit: Option<i64>,
//! }
//!
//! // In `tools()`: the schema the host offers the model.
//! let schema = input_schema::<Query>().unwrap();
//! assert!(schema.contains(r#""required":["match"]"#));
//!
//! // In `call()`: the model's input, read into the struct.
//! let q: Query = parse_args(r#"{"match": "+work", "limit": null}"#).unwrap();
//! assert_eq!(q, Query { r#match: "+work".into(), limit: None });
//!
//! // A wrong input is an error naming the field, for the model to correct.
//! let e = parse_args::<Query>(r#"{"match": "+work", "limit": "ten"}"#).unwrap_err();
//! assert_eq!(e.to_string(), "limit: expected integer, got string");
//! ```

use std::collections::BTreeMap;

use serde_json::{Map, Value as Json, json};

use crate::shape::{ConfigShape, ScalarKind, Schema, ShapeError, Value};

/// `T`'s schema as JSON Schema text, for `llm-tool.input-schema`.
///
/// # Errors
///
/// A tool's input is a JSON object, so `T` must derive a record (a struct with
/// named fields); any other shape is an error rather than a schema the host
/// would refuse when the plugin loads.
pub fn input_schema<T: ConfigShape>() -> Result<String, ShapeError> {
    match T::schema() {
        schema @ Schema::Record(_) => Ok(to_json_schema(&schema).to_string()),
        _ => Err(ShapeError::new(
            "a tool's input must be a struct with named fields",
        )),
    }
}

/// The model's input (`args` in `call`) read into `T`.
///
/// A `null` field is taken as absent, and empty input as `{}`: models send
/// both for what they mean to leave out.
///
/// # Errors
///
/// Input that is not JSON, or does not fit `T`: a number that is not an
/// integer, a wrong kind, a missing required field, an enum string naming no
/// variant. The [`ShapeError`] carries the path to the field, so returned as
/// the call's error it tells the model what to fix.
pub fn parse_args<T: ConfigShape>(args: &str) -> Result<T, ShapeError> {
    let json: Json = if args.trim().is_empty() {
        Json::Object(Map::new())
    } else {
        serde_json::from_str(args)
            .map_err(|e| ShapeError::new(format!("the input is not JSON: {e}")))?
    };
    T::from_value(&from_json(&json)?)
}

fn to_json_schema(schema: &Schema) -> Json {
    match schema {
        Schema::Scalar(ScalarKind::Bool) => json!({"type": "boolean"}),
        Schema::Scalar(ScalarKind::Int) => json!({"type": "integer"}),
        Schema::Scalar(ScalarKind::Str) => json!({"type": "string"}),
        Schema::Enum(names) => json!({"type": "string", "enum": names}),
        Schema::List(inner) => json!({"type": "array", "items": to_json_schema(inner)}),
        Schema::Record(fields) => {
            let mut properties = Map::new();
            for field in fields {
                let mut property = to_json_schema(&field.schema);
                if !field.doc.is_empty()
                    && let Json::Object(p) = &mut property
                {
                    p.insert("description".into(), Json::String(field.doc.clone()));
                }
                properties.insert(field.name.clone(), property);
            }
            let required: Vec<&str> = fields
                .iter()
                .filter(|f| f.required)
                .map(|f| f.name.as_str())
                .collect();
            json!({"type": "object", "properties": properties, "required": required})
        }
    }
}

fn from_json(json: &Json) -> Result<Value, ShapeError> {
    Ok(match json {
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => Value::Int(
            n.as_i64()
                .ok_or_else(|| ShapeError::new(format!("expected integer, got {n}")))?,
        ),
        Json::String(s) => Value::Str(s.clone()),
        Json::Array(items) => Value::List(
            items
                .iter()
                .enumerate()
                .map(|(i, item)| from_json(item).map_err(|e| e.under(&format!("[{i}]"))))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(fields) => {
            let mut record = BTreeMap::new();
            for (name, value) in fields {
                if value.is_null() {
                    continue;
                }
                record.insert(name.clone(), from_json(value).map_err(|e| e.under(name))?);
            }
            Value::Record(record)
        }
        Json::Null => {
            return Err(ShapeError::new("expected a value, got null"));
        }
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::ConfigShape;

    #[derive(Debug, PartialEq, ConfigShape)]
    enum Op {
        Done,
        Schedule,
    }

    /// One change to an entry.
    #[derive(Debug, PartialEq, ConfigShape)]
    struct Change {
        /// The entry's ref, from a query.
        entry_ref: String,
        /// What to do to it.
        op: Op,
        /// Tags to add.
        tags: Vec<String>,
        days: Option<i64>,
    }

    #[test]
    fn the_schema_is_json_schema_with_docs_and_requireds() {
        let schema: Json = serde_json::from_str(&input_schema::<Change>().unwrap()).unwrap();
        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": {
                    "entry-ref": {"type": "string", "description": "The entry's ref, from a query."},
                    "op": {"type": "string", "enum": ["done", "schedule"], "description": "What to do to it."},
                    "tags": {"type": "array", "items": {"type": "string"}, "description": "Tags to add."},
                    "days": {"type": "integer"}
                },
                "required": ["entry-ref", "op", "tags"]
            })
        );
        let e = input_schema::<Vec<String>>().unwrap_err();
        assert!(e.to_string().contains("struct with named fields"), "{e}");
    }

    #[test]
    fn the_models_input_parses_and_its_mistakes_name_the_field() {
        let c: Change = parse_args(
            r#"{"entry-ref": "a.org:12", "op": "schedule", "tags": ["x"], "days": 3, "extra": null}"#,
        )
        .unwrap();
        assert_eq!(
            c,
            Change {
                entry_ref: "a.org:12".into(),
                op: Op::Schedule,
                tags: vec!["x".into()],
                days: Some(3),
            }
        );
        for (args, says) in [
            (r#"{"op": "done", "tags": []}"#, "entry-ref"),
            (
                r#"{"entry-ref": "r", "op": "done", "tags": [1]}"#,
                "tags[0]: expected string",
            ),
            (
                r#"{"entry-ref": "r", "op": "done", "tags": [], "days": 1.5}"#,
                "days: expected integer, got 1.5",
            ),
            (r#"{"entry-ref": "r", "op": "delete", "tags": []}"#, "op"),
            ("{not json", "not JSON"),
        ] {
            let e = parse_args::<Change>(args).unwrap_err().to_string();
            assert!(e.contains(says), "{args}: {e}");
        }
    }

    /// No input at all is an empty object: a tool whose fields are all
    /// optional takes it.
    #[test]
    fn empty_input_is_an_empty_object() {
        #[derive(Debug, PartialEq, ConfigShape)]
        struct Opts {
            limit: Option<i64>,
        }
        assert_eq!(parse_args::<Opts>("").unwrap(), Opts { limit: None });
        assert_eq!(parse_args::<Opts>(" {} ").unwrap(), Opts { limit: None });
    }
}
