//! PH7.10 — the config/options seam, driven through a real guest.
//!
//! Instantiates the `config-guest` fixture (a `wasm32-wasip2` `config-plugin`
//! component) via [`PluginHost::spawn_config_plugin`], which drives its
//! `register-options` export against a native [`ConfigRegistry`]. Proves the seam
//! end to end:
//!   - the guest's imported `register-option` calls land three typed options
//!     (bool / integer / string) in the SAME registry core options use,
//!   - the guest's `get-option` reads a value back through the registry (written
//!     to its data-dir mount, `/data/option.log`),
//!   - a plugin option is a first-class registry entry: `:set` parses + sets it
//!     uniformly, and the value round-trips.
//!
//! Skips when the fixture wasn't built (no `wasm32-wasip2` target — see build.rs).

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_mode::CapabilitySet;
use lattice_plugin_host::{PluginBudget, PluginHost, PluginManifest, TrustTier};

const PLUGIN_ID: &str = "config-fixture";

/// The fixture config component path, or `None` when it wasn't built (skip).
fn guest_wasm() -> Option<&'static str> {
    let path = env!("CONFIG_GUEST_WASM");
    (!path.is_empty()).then_some(path)
}

/// The host path the guest's `/data/option.log` maps to for a given data base.
fn option_log(data_base: &std::path::Path) -> PathBuf {
    data_base.join(PLUGIN_ID).join("data").join("option.log")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_declares_options_into_the_shared_registry_end_to_end() {
    let Some(wasm) = guest_wasm() else {
        eprintln!("SKIP: config fixture guest not built (add the wasm32-wasip2 target)");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let data_base = dir.path().join("data");
    let host = PluginHost::with_dirs(dir.path().join("cache"), &data_base).expect("host builds");
    let component = host
        .compile(&std::fs::read(wasm).unwrap())
        .expect("compile config fixture");
    let manifest = PluginManifest::new(PLUGIN_ID, Vec::new(), CapabilitySet::empty());

    // A fresh registry (no linkme core options) — the plugin options are the only
    // entries, so assertions are hermetic.
    let registry = Arc::new(ConfigRegistry::default());

    let (_id, names) = host
        .spawn_config_plugin(
            &component,
            &manifest,
            TrustTier::Bundled,
            PluginBudget::event(),
            &registry,
        )
        .await
        .expect("spawn config plugin");

    // The three declared options were reported (drain of the plugin's
    // contributions) and landed in the shared registry with their mapped types.
    // The guest declared them with SHORT names (`enabled` / `count` / `label`);
    // the host AUTO-NAMESPACES by plugin id, so they register (and report) as
    // `config-fixture.*` — no plugin can collide in the global option namespace.
    assert_eq!(names.len(), 4, "four options declared: {names:?}");
    assert!(names.iter().any(|n| n == "config-fixture.enabled"));
    assert!(names.iter().any(|n| n == "config-fixture.templates"));

    let enabled = registry
        .lookup("config-fixture.enabled")
        .expect("registered");
    assert_eq!(enabled.type_label(), "boolean");
    assert_eq!(enabled.get_formatted(), "true");

    // CI.7: the guest called `set-option("count", "5")` during registration (short
    // name → its own `config-fixture.count` namespace) — the value was overridden
    // through the seam (default was 3).
    let count = registry.lookup("config-fixture.count").expect("registered");
    assert_eq!(count.type_label(), "integer");
    assert_eq!(
        count.get_formatted(),
        "5",
        "set-option overrode the default via the seam"
    );

    let label = registry.lookup("config-fixture.label").expect("registered");
    assert_eq!(label.type_label(), "string");
    assert_eq!(label.get_formatted(), "hello");

    // The guest read `count` back through `get-option` (resolving its own
    // `config-fixture.count`) after the `set-option` — the SET value (5, not the
    // default 3) crossed the round-trip.
    let logged = std::fs::read_to_string(option_log(&data_base)).unwrap_or_default();
    assert!(
        logged.contains("count=5"),
        "get-option returned the set-option value: {logged}"
    );

    // ── TC.3: an option whose value has STRUCTURE ─────────────────────────
    //
    // The guest declared `templates` as `list<record{key, target:record{file},
    // body?}>` — org's `capture-templates` reduced to its shape — through an
    // ARENA, because WIT has no recursive types.
    let templates = registry
        .lookup("config-fixture.templates")
        .expect("the structured option registered");

    // The SHAPE reached the registry, nesting intact. Asserted field by field
    // rather than on a label, because `list<record>` is what a schema that
    // dropped its second level would also report.
    let schema = templates.schema();
    let lattice_config::ConfigSchema::List(inner) = &schema else {
        panic!("expected a list schema, got {}", schema.label());
    };
    let lattice_config::ConfigSchema::Record(fields) = inner.as_ref() else {
        panic!("expected a record element, got {}", inner.label());
    };
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, vec!["key", "target", "body"], "fields, in order");
    assert!(
        !fields[2].required,
        "`body` was declared optional and stayed optional"
    );
    assert_eq!(
        fields[0].doc, "the key to press",
        "per-field docs crossed — what :customize renders beside each field"
    );
    let target = &fields[1].schema;
    let lattice_config::ConfigSchema::Record(target_fields) = target else {
        panic!("`target` must still be a record, got {}", target.label());
    };
    assert_eq!(target_fields[0].name, "file", "the SECOND level survived");

    // The VALUE the guest set through `set-option-value`, read back by the
    // guest through `get-option-value` and flattened with its links resolved —
    // so the assertion covers the arena's structure and not merely its nodes.
    assert!(
        logged.contains("templates=[{key=t,target={file=~/org/refile.org}}]"),
        "the typed round-trip lost or mangled the tree: {logged}"
    );

    // …and the host holds the same value.
    assert_eq!(
        templates.get_value(),
        lattice_config::ConfigValue::List(vec![lattice_config::ConfigValue::record([
            (
                "key".to_string(),
                lattice_config::ConfigValue::Str("t".into())
            ),
            (
                "target".to_string(),
                lattice_config::ConfigValue::record([(
                    "file".to_string(),
                    lattice_config::ConfigValue::Str("~/org/refile.org".into()),
                )]),
            ),
        ])]),
    );

    // A tree that violates the schema is refused — and refused WITHOUT
    // disturbing the value already there. Both halves, because a seam that
    // returned `false` and cleared the option would satisfy the first alone.
    assert!(
        logged.contains("rejected=false"),
        "an ill-shaped tree must be refused: {logged}"
    );
    assert_eq!(
        templates.get_value().as_list().map(<[_]>::len),
        Some(1),
        "the rejected write left the previous value intact"
    );

    // A plugin option is a first-class registry entry: `:set` works uniformly and
    // the value round-trips (this is what `:set config-fixture.count=7` drives).
    registry
        .parse_and_set_command("config-fixture.count=7")
        .expect(":set on a plugin option works");
    assert_eq!(
        registry
            .lookup("config-fixture.count")
            .unwrap()
            .get_formatted(),
        "7"
    );
}
