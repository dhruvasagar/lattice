//! AD.3: the reference's examples are real, current code.
//!
//! Examples are regions of guest source (`// @example <target>: <caption>` …
//! `// @end-example`; see `src/examples.rs`). These tests are what make an
//! example trustworthy enough to copy: it is well-formed, it names something
//! that exists in the API, and it lives in a guest that CI compiles against
//! the current `wit/`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lattice_plugin_api::catalog;
use lattice_plugin_api::examples::{Examples, scan};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn examples() -> Examples {
    scan(&repo_root())
}

#[test]
fn every_example_region_is_well_formed() {
    let ex = examples();
    assert!(
        ex.problems.is_empty(),
        "malformed example regions:\n  {}",
        ex.problems.join("\n  ")
    );
}

/// An example whose target does not exist would be rendered nowhere — or,
/// after a rename, keep illustrating a function that is gone. The target
/// grammar: `<interface>`, `<interface>.<function>` (a method as
/// `<resource>.<method>`, the reference's spelling), or `<interface>.<type>`.
#[test]
fn every_example_targets_something_in_the_api() {
    let cat = catalog();
    let mut dangling = Vec::new();
    for e in &examples().items {
        let Some(iface) = cat.interface(e.interface()) else {
            dangling.push(format!(
                "{} ({}): no interface `{}`",
                e.id,
                e.source,
                e.interface()
            ));
            continue;
        };
        let Some(item) = e.item() else { continue };
        let is_function = iface.functions.iter().any(|f| f.display_name() == item);
        let is_type = iface.types.iter().any(|t| t.name == item);
        if !is_function && !is_type {
            let mut names: Vec<String> = iface.functions.iter().map(|f| f.display_name()).collect();
            names.extend(iface.types.iter().map(|t| t.name.clone()));
            dangling.push(format!(
                "{} ({}): `{}` has no function or type `{item}` (it has: {})",
                e.id,
                e.source,
                iface.name,
                names.join(", ")
            ));
        }
    }
    assert!(
        dangling.is_empty(),
        "examples naming something the API does not have:\n  {}",
        dangling.join("\n  ")
    );
}

/// The guests `lattice-plugin-host`'s build script compiles, by name — the
/// `"<name>"` argument of every `build_guest(<path>, "<name>", "<ENV>")`
/// call. That script fails the build when a guest does not compile and the
/// wasm target is installed (as it is in CI), so these are exactly the guests
/// CI proves compile.
fn guests_built_in_ci() -> BTreeSet<String> {
    let build_rs = std::fs::read_to_string(repo_root().join("crates/lattice-plugin-host/build.rs"))
        .expect("read lattice-plugin-host/build.rs");
    build_rs
        .split("build_guest(")
        .skip(1)
        .filter_map(|rest| {
            let call = rest.split(");").next()?;
            let quoted: Vec<&str> = call.split('"').skip(1).step_by(2).collect();
            let [.., name, env] = quoted.as_slice() else {
                return None;
            };
            // The last argument is an ENV_VAR name; anything else is the
            // function's own definition, not a call.
            env.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
                .then(|| name.to_string())
        })
        .collect()
}

#[test]
fn every_example_lives_in_a_guest_ci_compiles() {
    let built = guests_built_in_ci();
    assert!(
        built.len() > 20,
        "parsed suspiciously few build_guest calls from lattice-plugin-host/build.rs: {built:?}"
    );
    let unbuilt: Vec<String> = examples()
        .items
        .iter()
        .filter(|e| !built.contains(e.id.split(':').next().unwrap_or("")))
        .map(|e| format!("{} ({})", e.id, e.source))
        .collect();
    assert!(
        unbuilt.is_empty(),
        "examples in guests lattice-plugin-host/build.rs does not build — add a \
         `build_guest` call for the guest, or move the example:\n  {}",
        unbuilt.join("\n  ")
    );
}

/// Every core plugin is compiled in CI — the property the test above relies
/// on for `plugins/*`, asserted directly so a new plugin cannot land
/// uncompiled even before it carries an example. (`plugins/comment` was
/// compiled only by the release workflow until AD.3.)
#[test]
fn every_core_plugin_is_compiled_in_ci() {
    let built = guests_built_in_ci();
    let unbuilt: Vec<String> = std::fs::read_dir(repo_root().join("plugins"))
        .expect("read plugins/")
        .flatten()
        .filter(|e| e.path().join("Cargo.toml").exists())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| !built.contains(name))
        .collect();
    assert!(
        unbuilt.is_empty(),
        "plugins/ crates lattice-plugin-host/build.rs does not build, so CI never \
         compiles them: {unbuilt:?}"
    );
}
