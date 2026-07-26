//! Integration tests for the `options!` / `groups!` proc macros
//! re-exported from `lattice-config-macros`.
//!
//! These exercise the full expansion path: identifier-to-display-
//! name lowering, doc-comment capture, attribute parsing,
//! `linkme` submission, runtime spec construction.

#![cfg(test)]
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::assertions_on_constants,
    dead_code,
    unsafe_code
)]

use crate::{ConfigRegistry, OptionDecl, OptionGroup};

// Validator function for the test option.
fn validate_test_indent(i: &i64) -> Result<(), String> {
    if (1..=32).contains(i) {
        Ok(())
    } else {
        Err(format!("test-indent out of range: {i}"))
    }
}

// Declare a fixture group via the proc macro.
crate::groups! {
    /// Test fixture group for proc-macro integration tests.
    pub TestProcMacroGroup = "test-proc-macro";
}

// Declare options bound to the fixture group with various
// attribute combinations.
crate::options! {
    namespace = "test-proc";
    group = TestProcMacroGroup;

    /// First option: simple, no attrs.
    pub Simple: bool = true;

    /// Second option: aliases.
    #[aliases("ts2", "tsalt")]
    pub WithAliases: i64 = 4;

    /// Third option: validate.
    #[validate(validate_test_indent)]
    pub WithValidate: i64 = 8;

    /// Fourth option: explicit display name.
    #[name("test-proc.explicit-name")]
    pub ExplicitName: String = String::from("hi");

    /// Fifth option: customizable=false.
    #[customizable(false)]
    pub Internal: bool = false;
}

#[test]
fn group_declaration_emits_typed_constants() {
    assert_eq!(TestProcMacroGroup::NAME, "test-proc-macro");
    // Doc string survives.
    assert!(
        TestProcMacroGroup::DOC.contains("Test fixture group"),
        "DOC was: {:?}",
        TestProcMacroGroup::DOC
    );
}

#[test]
fn option_declaration_emits_typed_constants() {
    assert_eq!(Simple::NAME, "test-proc.simple");
    assert!(Simple::default_value());
    assert!(Simple::DOC.contains("First option"));
    assert!(Simple::CUSTOMIZABLE);

    assert_eq!(WithAliases::NAME, "test-proc.with-aliases");
    assert_eq!(WithAliases::default_value(), 4);

    assert_eq!(ExplicitName::NAME, "test-proc.explicit-name");
    assert_eq!(ExplicitName::default_value(), "hi");

    assert!(!Internal::CUSTOMIZABLE);
}

#[test]
fn option_has_group_binding() {
    use crate::HasGroup;
    assert_eq!(<Simple as HasGroup>::GROUP_NAME, "test-proc-macro");
    assert_eq!(<WithAliases as HasGroup>::GROUP_NAME, "test-proc-macro");
}

#[test]
fn build_spec_carries_alias_and_validator() {
    // Spec construction: aliases honoured, validator wired.
    let spec = WithAliases::build_spec();
    let registry = ConfigRegistry::new();
    let _h = registry.register(spec);
    // Aliases are reachable via the registry's lookup-by-name.
    // PL8.F: `name()` now borrows the (temporary) looked-up option, so own it
    // for the comparison via `.as_deref()`.
    assert_eq!(
        registry
            .lookup("ts2")
            .map(|o| o.name().to_owned())
            .as_deref(),
        Some("test-proc.with-aliases")
    );
    assert_eq!(
        registry
            .lookup("tsalt")
            .map(|o| o.name().to_owned())
            .as_deref(),
        Some("test-proc.with-aliases")
    );

    // Validator: out-of-range write rejected.
    let registry2 = ConfigRegistry::new();
    let _h2 = registry2.register(WithValidate::build_spec());
    let err = registry2
        .parse_and_set_command("test-proc.with-validate=999")
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("out of range"), "msg was {msg}");
}

#[test]
fn linkme_aggregates_test_options() {
    // The 5 test options should appear in OPTION_DECLS.
    let names: Vec<&str> = crate::OPTION_DECLS.iter().map(|m| m.name).collect();
    for expected in [
        "test-proc.simple",
        "test-proc.with-aliases",
        "test-proc.with-validate",
        "test-proc.explicit-name",
    ] {
        assert!(
            names.contains(&expected),
            "linkme slice missing option {expected}; saw {names:?}",
        );
    }
}

#[test]
fn linkme_aggregates_test_group() {
    let names: Vec<&str> = crate::GROUP_DECLS.iter().map(|g| g.name).collect();
    assert!(names.contains(&"test-proc-macro"));
}

#[test]
fn init_from_linkme_registers_every_option() {
    // The boot path walks OPTION_DECLS and registers each.
    // We can't run it on the global registry (other tests
    // would race), so we use a fresh local registry. Note:
    // every options! declaration in the workspace's
    // lattice-config crate is in OPTION_DECLS, so this test
    // boot will register a superset of the test fixtures.
    // We just verify the test fixtures are reachable.
    let registry = ConfigRegistry::new();
    registry.init_from_linkme();

    // Type-keyed reads work post-boot.
    let v = registry
        .get_typed::<Simple>()
        .expect("Simple should be registered after init_from_linkme");
    assert!(*v);

    let v = registry
        .get_typed::<WithAliases>()
        .expect("WithAliases should be registered");
    assert_eq!(*v, 4);

    // Boundary read by name still works.
    let arc = registry
        .lookup("test-proc.simple")
        .expect("name-keyed lookup should succeed");
    assert_eq!(arc.name(), "test-proc.simple");
}

#[test]
fn get_typed_returns_none_before_boot() {
    let registry = ConfigRegistry::new();
    assert!(registry.get_typed::<Simple>().is_none());
}

#[test]
fn overrides_macro_builds_typed_set() {
    let set = crate::overrides! {
        Simple = false,
        WithAliases = 99,
    };
    assert_eq!(set.len(), 2);
    let entries: Vec<_> = set.iter().collect();
    // Each entry's TypeId matches its declaration's TypeId.
    assert_eq!(entries[0].option_type_id, std::any::TypeId::of::<Simple>());
    assert_eq!(
        entries[1].option_type_id,
        std::any::TypeId::of::<WithAliases>()
    );
    // Default priority is Normal.
    assert_eq!(entries[0].priority, crate::OverridePriority::Normal);
}

#[test]
fn overrides_macro_honours_priority_attribute() {
    let set = crate::overrides! {
        #[priority(High)]
        Simple = true,
        #[priority(Low)]
        WithAliases = 1,
        Internal = false,
    };
    let entries: Vec<_> = set.iter().collect();
    assert_eq!(entries[0].priority, crate::OverridePriority::High);
    assert_eq!(entries[1].priority, crate::OverridePriority::Low);
    assert_eq!(entries[2].priority, crate::OverridePriority::Normal);
}

#[test]
fn overrides_macro_resolver_round_trip() {
    // The macro-built set goes through the resolver and the
    // resolved value is reachable via type-keyed read.
    let set = crate::overrides! {
        WithAliases = 7,
    };
    let resolver = crate::Resolver::new();
    let mut resolved = crate::ResolvedOptions::new();
    resolver.resolve_into([&set], &mut resolved);
    let v = resolved.get::<WithAliases>().unwrap();
    assert_eq!(*v, 7);
}

#[test]
fn overrides_macro_empty_works() {
    let set: crate::OptionOverrideSet = crate::overrides! {};
    assert!(set.is_empty());
}
