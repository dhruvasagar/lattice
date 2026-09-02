//! TC.4 — the derive, exercised on org's actual shapes.
//!
//! Design: `docs/dev/architecture/typed-configuration.md`.
//!
//! A unit test of the derive against a toy struct proves the macro expands. It
//! does not prove the thing the slice claims: that a guest's configuration
//! parse becomes *total and mechanical*, replacing a bespoke text parser. So the
//! fixtures here are the real shapes phase 3 has to migrate —
//! `capture-templates` (a list of records with a nested record and an optional
//! field) and an enum-valued field — and the assertions are about the properties
//! a hand-written parser kept getting wrong: optionality, paths, and ordering.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_plugin_sdk::ConfigShape as ConfigShapeDerive;
use lattice_plugin_sdk::shape::{
    ConfigShape, Schema, ShapeError, Value, flatten_schema, flatten_value, unflatten_value,
};

/// Where a capture lands.
#[derive(Debug, Clone, PartialEq, Eq, ConfigShapeDerive)]
struct Target {
    /// The file it is appended to.
    file: String,
    /// Insert under this headline instead of appending.
    headline: Option<String>,
}

/// How a capture is filed.
#[derive(Debug, Clone, PartialEq, Eq, ConfigShapeDerive)]
enum Disposition {
    Append,
    Prepend,
    FileUnder,
}

/// One capture template.
#[derive(Debug, Clone, PartialEq, Eq, ConfigShapeDerive)]
struct Template {
    /// The key to press.
    key: String,
    /// Where the capture goes.
    target: Target,
    /// The body inserted at the target.
    body: Option<String>,
    /// Where in the target it lands.
    disposition: Disposition,
    /// Two words, to pin the wire spelling.
    max_depth: i64,
}

fn one() -> Template {
    Template {
        key: "t".into(),
        target: Target {
            file: "~/org/refile.org".into(),
            headline: None,
        },
        body: Some("* TODO %?\n".into()),
        disposition: Disposition::Append,
        max_depth: 3,
    }
}

#[test]
fn a_struct_describes_itself_as_a_record_of_its_fields() {
    let Schema::Record(fields) = Template::schema() else {
        panic!("a struct is a record");
    };
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    // Declaration order, and KEBAB-CASE: an option's fields should read like
    // every other key in a TOML file, not like Rust identifiers.
    assert_eq!(
        names,
        vec!["key", "target", "body", "disposition", "max-depth"]
    );

    // Optionality comes from the TYPE. That is why there is no
    // `#[shape(optional)]` — a second place to say it is a second place to
    // disagree with the first.
    let required: Vec<bool> = fields.iter().map(|f| f.required).collect();
    assert_eq!(required, vec![true, true, false, true, true]);

    // Per-field docs, from the `///` comments — what `:describe-option` and
    // `:customize` render beside each field.
    assert_eq!(fields[0].doc, "The key to press.");
    assert_eq!(fields[4].doc, "Two words, to pin the wire spelling.");

    // Nesting: `target` is a record in its own right, with its own optional.
    let Schema::Record(target) = &fields[1].schema else {
        panic!("`target` is a record");
    };
    assert_eq!(target[0].name, "file");
    assert!(target[0].required);
    assert!(!target[1].required, "`headline` is Option<String>");
}

#[test]
fn an_all_unit_enum_describes_itself_as_a_closed_set() {
    // The distinction that matters downstream: an enum schema is what lets
    // `:customize` offer a picker rather than a text field.
    assert_eq!(
        Disposition::schema(),
        Schema::Enum(vec!["append".into(), "prepend".into(), "file-under".into()])
    );
    assert_eq!(
        Disposition::from_value(&Value::Str("file-under".into())),
        Ok(Disposition::FileUnder)
    );
}

#[test]
fn an_unknown_enum_form_is_refused_and_shows_the_valid_set() {
    let err = Disposition::from_value(&Value::Str("sideways".into())).unwrap_err();
    assert!(
        err.message.contains("append | prepend | file-under"),
        "{err}"
    );
    assert!(err.message.contains("sideways"), "{err}");
}

#[test]
fn a_value_round_trips_through_the_tree() {
    // The contract that makes the derive worth trusting: `from_value` inverts
    // `to_value`. A type where they disagree is a silently lossy option, which
    // nothing downstream would notice.
    let t = one();
    assert_eq!(Template::from_value(&t.to_value()), Ok(t));
}

#[test]
fn an_absent_optional_field_is_omitted_rather_than_emptied() {
    // "Absent" and "present but empty" are different values, and the host's
    // `required` check reads absence. A derive that emitted a placeholder for
    // `None` would make every optional field look present.
    let t = one();
    let v = t.to_value();
    let target = v.field("target").expect("target is present");
    assert!(
        target.field("headline").is_none(),
        "a `None` field must not appear in the tree at all"
    );
    assert!(v.field("body").is_some(), "a `Some` field does appear");

    // …and it round-trips back to `None` rather than to `Some("")`.
    let back = Template::from_value(&v).unwrap();
    assert_eq!(back.target.headline, None);
}

#[test]
fn a_missing_required_field_names_its_path() {
    let mut v = one().to_value();
    let Value::Record(map) = &mut v else {
        panic!("record")
    };
    // Remove a field nested one level down, so the path has to be assembled
    // from two segments discovered at different depths.
    let Some(Value::Record(target)) = map.get_mut("target") else {
        panic!("target")
    };
    target.remove("file");

    let err = Template::from_value(&v).unwrap_err();
    assert_eq!(err.path, "target.file");
    assert!(err.message.contains("required"), "{err}");
}

#[test]
fn a_wrong_typed_leaf_names_its_path_through_a_list() {
    // The composite case: a list of records, one of which has a bad leaf two
    // levels down. This is the message a user of org's `capture-templates`
    // gets, and it is the one no hand-rolled parser produced.
    let mut a = one().to_value();
    let b = one().to_value();
    let Value::Record(map) = &mut a else {
        panic!("record")
    };
    let Some(Value::Record(target)) = map.get_mut("target") else {
        panic!("target")
    };
    target.insert("file".to_string(), Value::Int(7));

    let list = Value::List(vec![b, a]);
    let err = <Vec<Template> as ConfigShape>::from_value(&list).unwrap_err();
    assert_eq!(err.path, "[1].target.file");
    assert_eq!(
        err.to_string(),
        "[1].target.file: expected string, got integer"
    );
}

#[test]
fn a_non_record_is_refused_before_any_field_is_read() {
    // Otherwise the first missing field wins and the message blames a field
    // when the whole value is the wrong kind.
    let err = Template::from_value(&Value::Str("nope".into())).unwrap_err();
    assert_eq!(err.path, "", "the failure is the value itself, not a field");
    assert!(err.message.contains("expected record"), "{err}");
    assert!(err.message.contains("string"), "{err}");
}

#[test]
fn the_derived_shape_survives_the_arena_the_wire_actually_uses() {
    // The derive produces a tree; WIT carries an arena. If flattening lost the
    // nesting, every assertion above would still pass and nothing would work.
    let schema = <Vec<Template> as ConfigShape>::schema();
    let (nodes, root) = flatten_schema(&schema);
    assert!(!nodes.is_empty());
    assert_eq!(root as usize, nodes.len() - 1, "the root is pushed last");

    let value = vec![one(), one()].to_value();
    let (vnodes, vroot) = flatten_value(&value);
    assert_eq!(unflatten_value(&vnodes, vroot).unwrap(), value);
    // …and the whole loop: struct → tree → arena → tree → struct.
    let back =
        <Vec<Template> as ConfigShape>::from_value(&unflatten_value(&vnodes, vroot).unwrap())
            .unwrap();
    assert_eq!(back, vec![one(), one()]);
}

#[test]
fn error_paths_read_outside_in() {
    // `under` prepends, because a walk discovers segments from the inside out.
    // Appending would spell `file.target[1]`, which reads as a different value
    // entirely.
    let e = ShapeError::new("boom")
        .under("file")
        .under("target")
        .under("[1]");
    assert_eq!(e.to_string(), "[1].target.file: boom");
}
