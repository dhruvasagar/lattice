//! The user docs name the config files the loader actually reads.
//!
//! Issue #3: `docs/user/options.md` told users to write
//! `~/.config/lattice/init.toml`, which nothing reads — the loader wants
//! `lattice.toml` — and `terminal-mode.md` said `config.toml`. The first
//! stranger to follow the docs set an option, saw no effect, and had to
//! guess. Nothing failed, because nothing compared the prose to the loader.
//!
//! This does: every `~/.config/lattice/<name>.toml` in `docs/user/` must be
//! the file [`default_user_config_path`] resolves to, and every
//! `.lattice/<name>.toml` the file [`project_config_path`] does. The expected
//! names come from those functions, so renaming a file in the loader fails
//! here until the docs follow.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::Path;

use lattice_config::project_config_path;

/// The file name after `prefix` in every occurrence in `text`, if it is a
/// `.toml` name. `~/.config/lattice/plugins/` and other directories are not
/// config files and are skipped.
fn toml_names_after<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.match_indices(prefix)
        .filter_map(|(at, _)| {
            let rest = &text[at + prefix.len()..];
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'))
                .unwrap_or(rest.len());
            let name = &rest[..end];
            name.ends_with(".toml").then_some(name)
        })
        .collect()
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

#[test]
fn user_docs_name_the_config_files_the_loader_reads() {
    // The user file's NAME, independent of where the config home resolves on
    // this machine (`$XDG_CONFIG_HOME`, `$HOME`, or neither in a sandbox).
    let user = "lattice.toml";
    assert!(
        std::env::var_os("HOME").is_none()
            || lattice_config::default_user_config_path().is_some_and(|p| file_name(&p) == user),
        "the loader's user config file is no longer `{user}` — update this test and the docs"
    );
    let project = file_name(&project_config_path(Path::new("/root")));

    let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/user");
    let mut wrong = Vec::new();
    for entry in std::fs::read_dir(&docs).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let doc = file_name(&path);
        for name in toml_names_after(&text, "~/.config/lattice/") {
            if name != user {
                wrong.push(format!(
                    "{doc}: ~/.config/lattice/{name} (the loader reads {user})"
                ));
            }
        }
        for name in toml_names_after(&text, ".lattice/") {
            if name != project {
                wrong.push(format!(
                    "{doc}: .lattice/{name} (the loader reads {project})"
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "user docs name config files the loader never reads:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn the_scanner_finds_names_and_skips_directories() {
    let text = "see `~/.config/lattice/init.toml`, `~/.config/lattice/plugins/x`, \
                and `.lattice/config.toml`.";
    assert_eq!(
        toml_names_after(text, "~/.config/lattice/"),
        vec!["init.toml"]
    );
    assert_eq!(toml_names_after(text, ".lattice/"), vec!["config.toml"]);
}
