//! The demo script's pre-flight check names every bundled plugin.
//!
//! `docs/media/demo-script.md` opens with a checklist the recording is verified
//! against, and one line of it asserts which plugins `:plugins` should list as
//! `bundled`. That line went stale the moment `comment` shipped as the fourth
//! core plugin: the script still said three, so following it either fails the
//! check against a correct build or teaches you to ignore the check. A
//! pre-flight list that is wrong is worse than no pre-flight list, because it
//! is consulted exactly once, under time pressure, with a camera running.
//!
//! `CORE_PLUGINS` in `xtask/src/main.rs` is the source of truth for what ships
//! — the same constant `the_deb_carries_every_core_plugin` reads — so it is
//! read here rather than copied.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;

fn workspace_root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

/// The plugin names from `xtask`'s `CORE_PLUGINS` literal.
fn core_plugins() -> Vec<String> {
    let src = std::fs::read_to_string(workspace_root().join("xtask/src/main.rs")).unwrap();
    let (_, rest) = src
        .split_once("const CORE_PLUGINS")
        .expect("CORE_PLUGINS exists");
    // `= &[` rather than the first `[`, which belongs to the `&[&str]` type.
    let list = rest
        .split_once("= &[")
        .and_then(|(_, r)| r.split_once(']'))
        .expect("CORE_PLUGINS is a slice literal")
        .0;
    let names: Vec<String> = list
        .split(',')
        .filter_map(|part| {
            let part = part.trim().trim_matches('"');
            (!part.is_empty()).then(|| part.to_owned())
        })
        .collect();
    assert!(
        names.len() >= 4,
        "parsed CORE_PLUGINS as {names:?} — the literal's shape changed"
    );
    names
}

/// The `## Before you record` section — the pre-flight checklist itself, not
/// the whole script, so a plugin merely mentioned in a later beat does not
/// satisfy the check.
fn preflight() -> String {
    let script =
        std::fs::read_to_string(workspace_root().join("docs/media/demo-script.md")).unwrap();
    let (_, rest) = script
        .split_once("## Before you record")
        .expect("demo-script.md has a `## Before you record` section");
    rest.split_once("\n## ")
        .map(|(section, _)| section.to_owned())
        .unwrap_or(rest.to_owned())
}

#[test]
fn the_preflight_checklist_names_every_bundled_plugin() {
    let preflight = preflight();
    let missing: Vec<String> = core_plugins()
        .into_iter()
        .filter(|name| !preflight.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/media/demo-script.md's pre-flight checklist does not name every \
         plugin in xtask's CORE_PLUGINS. Recording against a stale checklist is \
         how `comment` went unnoticed:\n  {}",
        missing.join("\n  ")
    );
}

#[test]
fn the_preflight_checklist_states_the_right_count() {
    let expected = core_plugins().len();
    let words = [
        "", "one", "two", "three", "four", "five", "six", "seven", "eight",
    ];
    let preflight = preflight();
    let spelled = words.get(expected).copied().unwrap_or("");
    assert!(
        preflight.contains(spelled) || preflight.contains(&expected.to_string()),
        "the pre-flight checklist should say there are {expected} bundled \
         plugins (`{spelled}`), so a miscount is caught before the camera is \
         running. CORE_PLUGINS has {expected}."
    );
}
