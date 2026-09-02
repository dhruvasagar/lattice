//! TC.8 — `:describe-option` renders a composite option's declared shape.
//!
//! Design: `docs/dev/architecture/typed-configuration.md`.
//!
//! The payoff that is in scope. Before typed configuration, an option whose
//! value had structure was a string holding TOML, so `:describe-option` printed
//! a wall of it under `current:` and the only thing a reader learnt about the
//! SHAPE was whatever the doc string happened to say in prose. Now the shape is
//! data, and this is where a user sees it.
//!
//! `:customize` — design §5.12's "type-aware editing buffer" — is the larger
//! thing the schema makes possible and is deliberately out of scope. This slice
//! is the smallest one that turns the schema from an internal invariant into
//! something a user can look at.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_config::{ConfigSchema, SchemaField};

/// org's `capture-templates`, reduced to the shape that matters: a list of
/// records, one field of which is a record, one of which is an enum, and one
/// of which is optional.
fn templates_schema() -> ConfigSchema {
    ConfigSchema::list(ConfigSchema::record([
        SchemaField::new("key", ConfigSchema::string(), "The key to press."),
        SchemaField::new(
            "target",
            ConfigSchema::record([
                SchemaField::new("file", ConfigSchema::string(), "Where it lands."),
                SchemaField::new("headline", ConfigSchema::string(), "Insert under this.")
                    .optional(),
            ]),
            "Where the capture goes.",
        ),
        SchemaField::new(
            "disposition",
            ConfigSchema::Enum(vec!["append".into(), "prepend".into()]),
            "Where in the target.",
        ),
        SchemaField::new("body", ConfigSchema::string(), "The template text.").optional(),
    ]))
}

fn described() -> String {
    templates_schema()
        .describe()
        .expect("a composite has a shape worth describing")
        .join("\n")
}

#[test]
fn a_reader_can_see_every_field_it_may_write() {
    // The question a config file actually raises is "what do I write here",
    // and the answer is a list of names. Asserted per name rather than on the
    // whole block, so a rendering change that drops one is a failure that says
    // which.
    let text = described();
    for field in ["key", "target", "file", "headline", "disposition", "body"] {
        assert!(
            text.contains(&format!("{field}")),
            "`{field}` is missing from the shape:\n{text}"
        );
    }
}

#[test]
fn optionality_is_visible_at_a_glance() {
    // The second question a config file raises, and the one a prose doc string
    // was worst at answering — "is this required?" was previously findable only
    // by reading the plugin's source.
    let text = described();
    assert!(text.contains("body?: string"), "{text}");
    assert!(text.contains("headline?: string"), "{text}");
    assert!(
        !text.contains("key?:") && !text.contains("file?:"),
        "a required field must carry no `?`:\n{text}"
    );
}

#[test]
fn nesting_survives_to_the_second_level() {
    // A renderer that stopped at depth one would look correct in every shape
    // that has no depth two — which is most of them, and not the ones this
    // work exists for.
    let text = described();
    let file_line = text
        .lines()
        .find(|l| l.trim_start().starts_with("file:"))
        .unwrap_or_else(|| panic!("no `file` line:\n{text}"));
    let target_line = text
        .lines()
        .find(|l| l.trim_start().starts_with("target:"))
        .unwrap_or_else(|| panic!("no `target` line:\n{text}"));
    let indent = |l: &str| l.len() - l.trim_start().len();
    assert!(
        indent(file_line) > indent(target_line),
        "`file` must sit inside `target`:\n{text}"
    );
}

#[test]
fn an_enum_shows_its_forms_beside_the_field_that_accepts_them() {
    // An enum's whole advantage over a free string is that the answer is
    // finite. A reader who has to go looking for the values has lost it —
    // and the option-level `values:` line cannot help here, because these
    // forms belong to a FIELD several levels down, not to the option.
    let text = described();
    assert!(
        text.contains("disposition: enum — append | prepend"),
        "{text}"
    );
}

#[test]
fn per_field_docs_are_carried_not_dropped() {
    // The reason `SchemaField` has a `doc` at all. An option-level doc string
    // describing six fields in prose is exactly the wall this replaces.
    let text = described();
    assert!(text.contains("The key to press."), "{text}");
    assert!(text.contains("Where it lands."), "{text}");
}

#[test]
fn a_scalar_option_gains_no_block_at_all() {
    // The no-regression half, and it matters more than it looks: almost every
    // option in the editor is a scalar, and a block repeating what `type:`
    // already says would be noise on the common case — which is what stops
    // people reading the uncommon one.
    assert_eq!(ConfigSchema::int().describe(), None);
    assert_eq!(ConfigSchema::string().describe(), None);
    assert_eq!(ConfigSchema::bool().describe(), None);
}
