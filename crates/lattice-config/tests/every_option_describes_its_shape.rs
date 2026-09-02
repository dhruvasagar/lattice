//! TC.1 — the invariants the schema surface has to hold for EVERY option type,
//! not just the three the module's own unit tests reach for.
//!
//! Design: `docs/dev/architecture/typed-configuration.md`.
//!
//! `schema()` / `to_value()` / `from_value()` are defaulted so that no existing
//! option declaration changes. That is what makes the re-base tractable and
//! also what makes it dangerous: a type that overrides one of the three and not
//! the others compiles, registers, and is silently lossy — `set_value` accepts
//! a tree the schema described and `get_value` hands back something that is not
//! it. Nothing else in the crate would notice.
//!
//! So the load-bearing test here is the round-trip, run across a spread of real
//! option types rather than a hand-made one: primitives, an enum-shaped
//! renderer type, and a list-shaped one, because those are the three ways the
//! default implementations resolve differently.

#![allow(clippy::unwrap_used, clippy::panic)]

use lattice_config::option::Option as ConfigOption;
use lattice_config::{
    ConfigSchema, ConfigValue, Decorations, ErasedOption, ExpandHeight, ModelineZone, OptionType,
    RootMarkers, ScalarKind, SignColumn,
};

/// `from_value(to_value(v)) == v` — the contract that makes the tree path and
/// the string path two views of one value rather than two values.
fn assert_round_trips<T: OptionType + PartialEq + std::fmt::Debug>(v: T) {
    let tree = v.to_value();
    let back = T::from_value(&tree).unwrap_or_else(|e| {
        panic!(
            "{}: to_value() produced a tree its own from_value() rejects: {e}",
            T::type_label()
        )
    });
    assert_eq!(
        back,
        v,
        "{} does not round-trip through a tree",
        T::type_label()
    );
}

/// …and the tree it produces must actually satisfy the shape it declares. A
/// type can round-trip perfectly with itself while describing that value as
/// something else entirely, and then every consumer that trusts the schema —
/// the loader, `:describe-option`, `:customize` — is wrong about it.
fn assert_value_matches_schema<T: OptionType>(v: &T) {
    let schema = T::schema();
    let tree = v.to_value();
    if let Err(e) = lattice_config::schema::validate(&schema, &tree) {
        panic!(
            "{}: its own value does not satisfy its own schema ({}): {e}",
            T::type_label(),
            schema.label()
        );
    }
}

#[test]
fn primitives_carry_their_own_kind_rather_than_a_string() {
    // The one thing worth NOT defaulting. A boolean described as
    // `enum["true","false"]` reads fine and would make `:customize` render a
    // two-item picker where a checkbox belongs; an integer described as a
    // string means the TOML loader turns `tabstop = 4` into "4" and parses it
    // back, which is the round-trip TC.2 exists to remove.
    assert_eq!(<bool as OptionType>::schema(), ConfigSchema::bool());
    assert_eq!(<i64 as OptionType>::schema(), ConfigSchema::int());
    assert_eq!(<String as OptionType>::schema(), ConfigSchema::string());

    assert_eq!(true.to_value(), ConfigValue::Bool(true));
    assert_eq!(42i64.to_value(), ConfigValue::Int(42));
    assert_eq!("x".to_string().to_value(), ConfigValue::Str("x".into()));

    for b in [true, false] {
        assert_round_trips(b);
        assert_value_matches_schema(&b);
    }
    for i in [0i64, -7, i64::MAX] {
        assert_round_trips(i);
        assert_value_matches_schema(&i);
    }
    for s in ["", "hello", "with spaces"] {
        assert_round_trips(s.to_string());
        assert_value_matches_schema(&s.to_string());
    }
}

#[test]
fn an_open_enumeration_is_not_mistaken_for_a_closed_one() {
    // The trap this whole flag exists for, pinned by name.
    //
    // `enumerate()` is documented as feeding `:set foo=<Tab>` completion, and
    // three types read that as a HINT rather than a contract: ModelineZone
    // advertises `auto` while accepting any comma list, ExpandHeight advertises
    // `half`/`full` while also accepting a bare number, RootMarkers advertises
    // the defaults while accepting others. Deriving `enum` from `enumerate`
    // alone described all three as closed sets — `:customize` would have
    // offered a one-item picker for an option accepting arbitrary lists, and
    // `lattice.toml` would have rejected every value the option actually wants.
    //
    // Asserted per type rather than "the default is false", because the failure
    // is a type opting in when it should not have, which a test of the default
    // cannot see.
    for (label, exhaustive, schema) in [
        (
            "modeline-zone",
            <ModelineZone as OptionType>::enumerate_is_exhaustive(),
            <ModelineZone as OptionType>::schema(),
        ),
        (
            "expand-height",
            <ExpandHeight as OptionType>::enumerate_is_exhaustive(),
            <ExpandHeight as OptionType>::schema(),
        ),
        (
            "root-markers",
            <RootMarkers as OptionType>::enumerate_is_exhaustive(),
            <RootMarkers as OptionType>::schema(),
        ),
    ] {
        assert!(
            !exhaustive,
            "{label} accepts values its `enumerate()` does not list — it must not claim to be closed"
        );
        assert_eq!(
            schema,
            ConfigSchema::string(),
            "{label} has an open value space, so its schema is a string"
        );
    }
}

#[test]
fn a_type_whose_enumeration_is_closed_gets_an_enum_schema() {
    // The opt-in half. A type that DOES mean its enumeration as the complete
    // set gets `enum(...)` carrying exactly those forms in order — which is
    // what lets `:customize` offer a picker instead of a text field.
    let forms = <SignColumn as OptionType>::enumerate().expect("signcolumn enumerates");
    assert!(<SignColumn as OptionType>::enumerate_is_exhaustive());
    match <SignColumn as OptionType>::schema() {
        ConfigSchema::Enum(got) => {
            assert_eq!(
                got,
                forms.iter().map(|f| f.to_string()).collect::<Vec<_>>(),
                "the enum schema must carry exactly the completion forms, in order"
            );
        }
        other => panic!("expected an enum schema, got {}", other.label()),
    }

    // …and every enumerated form must parse and satisfy that schema. An
    // enumerate() that advertises a form parse() rejects is a completion menu
    // offering values the option refuses, which this now also catches.
    for form in forms {
        let v = SignColumn::parse(form)
            .unwrap_or_else(|e| panic!("signcolumn advertises `{form}` but rejects it: {e}"));
        assert_value_matches_schema(&v);
        assert_round_trips(v);
    }
}

#[test]
fn a_free_form_type_falls_back_to_string_and_still_round_trips() {
    // `ModelineZone` is a list-shaped value over an open space, so it takes the
    // `scalar(string)` path — its `format()` IS its wire form today. TC.1 does
    // not migrate it to `list<string>` (that is a later slice's call); what it
    // must guarantee is that the fallback is honest rather than lossy.
    for spec in ["", "core.mode", "core.mode,core.path"] {
        let v = ModelineZone::parse(spec).unwrap_or_else(|e| panic!("`{spec}`: {e}"));
        assert_value_matches_schema(&v);
        assert_round_trips(v);
    }
    // …and a closed enum still round-trips through the enum path, which is a
    // different branch of `validate` than the scalar one above.
    let d = Decorations::parse(&Decorations::default().format()).expect("default re-parses");
    assert_value_matches_schema(&d);
    assert_round_trips(d);
}

#[test]
fn the_erased_surface_reports_the_same_shape_as_the_type() {
    // Every runtime-name consumer goes through `ErasedOption`, so a divergence
    // here is a divergence everywhere that matters.
    let o: ConfigOption<i64> = ConfigOption::new("tabstop", 8, "Tab width.");
    let erased: &dyn ErasedOption = &o;

    assert_eq!(erased.schema(), ConfigSchema::Scalar(ScalarKind::Int));
    assert_eq!(erased.get_value(), ConfigValue::Int(8));

    erased
        .set_value(&ConfigValue::Int(4))
        .expect("an int lands");
    assert_eq!(erased.get_value(), ConfigValue::Int(4));
    // The string surface sees the same write — the two paths are views of one
    // value, which is the whole claim of §2.2.
    assert_eq!(erased.get_formatted(), "4");
}

#[test]
fn set_value_refuses_the_wrong_shape_instead_of_coercing_it() {
    // Coercion is what the string ABI did — everything was a string, so
    // everything "worked" and the failure surfaced later as a parse error with
    // no idea where it came from. A typed set that quietly stringified an
    // integer would reintroduce exactly that.
    let o: ConfigOption<i64> = ConfigOption::new("tabstop", 8, "");
    let erased: &dyn ErasedOption = &o;

    let err = erased
        .set_value(&ConfigValue::Str("4".into()))
        .expect_err("a string is not an integer, even when it looks like one");
    assert!(err.contains("expected integer"), "got `{err}`");
    assert!(err.contains("string"), "got `{err}`");
    assert_eq!(
        erased.get_value(),
        ConfigValue::Int(8),
        "the value is untouched"
    );
}

#[test]
fn set_value_runs_the_options_own_validator_too() {
    // The schema check answers "is this the right shape"; the option's
    // validator answers "is this an acceptable value". Both stages have to run
    // on the tree path or a range-checked option would be enforceable through
    // `:set` and not through `lattice.toml`, which is the kind of divergence
    // nobody discovers until it has already been depended on.
    let o: ConfigOption<i64> = ConfigOption::builder("tabstop", 8, "")
        .validate(|i| {
            (1..=32)
                .contains(i)
                .then_some(())
                .ok_or_else(|| format!("out of range: {i}"))
        })
        .build();
    let erased: &dyn ErasedOption = &o;

    erased.set_value(&ConfigValue::Int(4)).expect("in range");
    let err = erased
        .set_value(&ConfigValue::Int(999))
        .expect_err("the validator must run on this path too");
    assert!(err.contains("out of range"), "got `{err}`");
    assert_eq!(
        erased.get_value(),
        ConfigValue::Int(4),
        "the value is untouched"
    );
}
