//! `lattice --scaffold-init` / `--scaffold-plugin NAME` — write a
//! **self-contained, buildable** WASM-component starter (config or plugin) into
//! the lattice config tree.
//!
//! Each scaffold is a complete cargo crate: a `Cargo.toml`, the `plugin.toml` the
//! editor discovers, a minimal `src/lib.rs`, and a `wit/` copy of the editor's
//! own API package (embedded at build time — [`crate::WIT_FILES`]) so it builds
//! with no separate checkout, matched to this editor's version. The user edits
//! `src/lib.rs`, builds to `wasm32-wasip2`, drops the artifact in, and reloads.
//! Refuses to clobber an existing non-empty directory.
//!
//! ## WT.2b — the `wit/` written here is a seed, not the mechanism
//!
//! This copy used to be the *only* one a scaffolded project ever got, and that
//! was the defect: it was made once and never refreshed, so it silently became a
//! fork of an API that kept moving. The refresh now lives in the plugin build
//! service ([`lattice_plugin_loader::build`]), which rewrites `wit/` from the
//! canonical package immediately before it invokes cargo — so the component is
//! always compiled against the API of the process about to instantiate it.
//!
//! The seed still earns its place: `wit_bindgen::generate!` needs the files on
//! disk to expand, so without it rust-analyzer cannot resolve a single symbol in
//! the freshly scaffolded `src/lib.rs` until the editor has built it once. It is
//! a convenience for the first minute of editing, and nothing downstream now
//! depends on it staying current.

use std::path::Path;

use anyhow::{Context, Result, bail};

const CARGO_TOML: &str = r#"# Your lattice config, compiled to a wasm32-wasip2 component.
# A standalone [workspace] so it doesn't inherit an outer cargo toolchain.
[package]
name = "lattice-init"
version = "0.0.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]      # a component, not an rlib

[dependencies]
wit-bindgen = "0.58"

[profile.release]
opt-level = "s"
strip = true

[workspace]
"#;

const PLUGIN_TOML: &str = r#"# The init config manifest. `id` MUST be "init" (the :reload-config target).
# `provides` lists the seams your init.rs registers into — add "grammar" /
# "decorations" / … as you use them, matching the exports in wit/init.wit.
id = "init"
provides = ["config", "keymap", "events"]
"#;

const INIT_WIT: &str = r#"package lattice:plugin-host@0.1.0;

// Your init.rs world — the seams you use. EXPORT the register-* your guest
// implements; IMPORT the host APIs you call. Add/remove seams as needed. Browse
// exact signatures in a running editor with `:describe-plugin-api <seam>`.
// (The comment sits BELOW `package` — only one file per package may doc it.)
world init {
    import config;
    import keymap;
    import events;
    import modes;

    export register-options: func();
    export register-keymap: func();
    export register-events: func();
    export on-event: func(handler: u32, ev: event);
    // A periodic wake you asked for with `events::wake_every(ms)`. Required
    // even if you arm none — the host instantiates this world against the
    // events bindings, which name every export.
    export on-wake: func(id: wake-id);

    use types.{event};
    use events.{wake-id};
}
"#;

const LIB_RS: &str = r####"//! Your lattice config, compiled to a WASM component and loaded at boot.
//! Edit freely, then rebuild + `:reload-config` (see this dir's build steps, or
//! `docs/user/init.md`). Everything below is illustrative — delete what you
//! don't want.

wit_bindgen::generate!({ world: "init", path: "wit" });

// `Guest` is wit-bindgen's trait for THIS world's exports — the `register_*`
// functions the host calls once at load, plus `on_event`. (The name `Guest` is
// fixed by wit-bindgen.) `Config` is your config — the type that implements them.
// `Event` is re-exported at the crate root by the world's `use types.{event}`
// (refer to it unqualified); other types come from their interface module.
use lattice::plugin_host::keymap::BindingMode;
use lattice::plugin_host::types::{EventFilter, EventKind};
use lattice::plugin_host::{config, events, keymap, modes};

struct Config;

impl Guest for Config {
    // IMMEDIATE — option overrides (also settable in lattice.toml or via `:set`).
    fn register_options() {
        config::set_option("tabstop", "4");

        // auto-pair is a CORE plugin, ON by default. Configure it here:
        // config::set_option("auto-pair.style", "manual"); // manual close-key pairing
        // config::set_option("auto-pair.enabled", "false"); // …or turn it off
    }

    // IMMEDIATE — keybindings layered above the builtin vim grammar.
    fn register_keymap() {
        // <C-s> in Normal → an existing command (binds only if the command exists).
        keymap::register_binding(BindingMode::Normal, "<C-s>", "ex:write");
    }

    // Subscribe deferred / event-flow hooks (handler ids are yours to choose).
    fn register_events() {
        // SET a plugin's options here: `pre-plugin-loaded` fires after that
        // plugin has declared them and before it reads any, and the load WAITS
        // for this handler. A plugin that reads an option while loading — to
        // build theme elements or a highlight query from it, say — sees your
        // value rather than its default.
        events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::PrePluginLoaded]),
                path_globs: None,
                major_modes: None,
            },
            1,
        );
        // DO things that need the plugin fully loaded here — enabling a mode
        // needs the mode to be registered, which has not happened yet above.
        events::subscribe(
            &EventFilter {
                kinds: Some(vec![EventKind::PluginLoaded]),
                path_globs: None,
                major_modes: None,
            },
            2,
        );
    }

    // React to events. Core plugins like auto-pair are on by default —
    // configure those in `register_options` above, not here.
    fn on_event(handler: u32, ev: Event) {
        // Options, before the plugin can read them. Full names: `set_option`
        // prefixes a bare name with the CALLING plugin's id, so `style` would
        // be looked up as `init.style`.
        if let (1, Event::PrePluginLoaded(name)) = (handler, &ev) {
            if name == "my-plugin" {
                config::set_option("my-plugin.style", "manual");
            }
        }
        if let (2, Event::PluginLoaded(p)) = (handler, ev) {
            if p.name == "my-plugin" {
                modes::enable_mode("my-plugin-mode");
            }
        }
    }

    // Periodic work, off the keystroke path. Arm one with
    // `events::wake_every(60_000)` (it returns the id you get back here) and
    // stop it with `events::cancel_wake(id)`. Leave this empty if you arm none.
    fn on_wake(_id: u32) {}
}

export!(Config);
"####;

// ── plugin scaffold templates (`__NAME__` / `__MODE__` / `__ACTION__` tokens
//    are substituted; the code's own `{}` stay literal) ──────────────────────

const PLUGIN_CARGO_TOML: &str = r#"# A lattice plugin, compiled to a wasm32-wasip2 component.
[package]
name = "__NAME__"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.58"

[profile.release]
opt-level = "s"
strip = true

[workspace]
"#;

const PLUGIN_MANIFEST: &str = r#"# The plugin manifest the editor discovers. `grammar` MUST precede `modes` in
# `provides`: the mode's keymap binds the plugin's OWN grammar action by name,
# resolved at bind time. `default_mode` makes `__MODE__` on by default via the
# auto-registered `__NAME__.enabled` option (`:set __NAME__.enabled=false` off).
# A plugin with more than one on-by-default mode uses `default_modes = [...]`
# instead — one gate, every mode named. A mode that is neither is registered
# but never activates.
id = "__NAME__"
provides = ["grammar", "modes"]
default_mode = "__MODE__"
"#;

const PLUGIN_WORLD_WIT: &str = r#"package lattice:plugin-host@0.1.0;

// Your plugin world — a grammar action + a minor mode that binds a key to it.
// Add seams (config, events, decorations, …) + their exports as you grow it.
world user-plugin {
    import grammar;
    import buffer;
    import tree-sitter;
    import modes;

    export register-grammar: func();
    export grammar-callbacks;
    export register-modes: func();
}
"#;

const PLUGIN_LIB_RS: &str = r####"//! The `__NAME__` lattice plugin. A grammar action (`__ACTION__`) that echoes,
//! plus a minor mode (`__MODE__`) binding `gh` (Normal) to it. Edit freely;
//! rebuild + reinstall (see this dir's build steps). Browse seam signatures with
//! `:describe-plugin-api <seam>`.

wit_bindgen::generate!({ world: "user-plugin", path: "wit" });

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, Args, EchoLevel, EchoPayload, Effect, ExCommandContext,
    MotionContext, MotionResult, OperatorContext, Range, TextObjectContext,
};
use lattice::plugin_host::{grammar, modes};

struct Plugin;

const CB_HELLO: u32 = 1;

impl Guest for Plugin {
    // grammar seam — contribute one action (fired on the mode's chord).
    fn register_grammar() {
        grammar::register_action(
            "__ACTION__",
            "say hello (starter action)",
            &ActionSpec { args_schema: Vec::new() },
            CB_HELLO,
        );
    }

    // modes seam — a minor mode owning an insert/normal keymap. Bindings live at
    // MinorMode(__MODE__), never the builtin layer.
    fn register_modes() {
        modes::register_mode(&ModeDeclaration {
            id: "__MODE__".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Global,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![ModeKeymapBinding {
                binding_mode: BindingMode::Normal,
                chord: "gh".to_string(),
                command: "__ACTION__".to_string(),
            }],
        });
    }
}

impl GrammarCallbacks for Plugin {
    fn apply_action(
        callback: u32,
        _ctx: ActionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        match callback {
            CB_HELLO => Ok(vec![Effect::Echo(EchoPayload {
                level: EchoLevel::Info,
                text: "hello from __NAME__".to_string(),
            })]),
            other => Err(format!("__NAME__: unknown action callback {other}")),
        }
    }

    // Unused grammar callbacks — return an err (logged, no-op). Fill in as needed.
    fn apply_motion(
        _c: u32,
        _ctx: MotionContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<MotionResult, String> {
        Err("__NAME__: no motions".into())
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("__NAME__: no operators".into())
    }
    fn apply_text_object(
        _c: u32,
        _ctx: TextObjectContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Range, String> {
        Err("__NAME__: no text objects".into())
    }
    fn parse_ex_args(_c: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("__NAME__: no ex-commands".into())
    }
    fn apply_ex_command(
        _c: u32,
        _ctx: ExCommandContext,
        _doc: &Document,
        _tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Effect>, String> {
        Err("__NAME__: no ex-commands".into())
    }
}

export!(Plugin);
"####;

/// Scaffold `~/.config/lattice/init/` (the user config). Returns an error (never
/// a panic) on a missing config home or a non-empty existing config.
pub fn scaffold_init() -> Result<()> {
    let dir = lattice_config::config_home()
        .context("no config home directory on this platform (set $XDG_CONFIG_HOME)")?
        .join("lattice")
        .join("init");
    write_scaffold_init(&dir)?;

    let d = dir.display();
    println!("Created a starter lattice config at {d}\n");
    println!("Next — build it and drop the component in place:\n");
    println!("  rustup target add wasm32-wasip2   # once");
    println!("  cd {d}");
    println!("  cargo build --release --target wasm32-wasip2");
    println!("  cp target/wasm32-wasip2/release/lattice_init.wasm init.wasm\n");
    println!("Then start lattice — or `:reload-config` in a running editor.");
    println!("Edit src/lib.rs to customise; see docs/user/init.md for the seams.");
    Ok(())
}

/// Scaffold a starter plugin project `~/.config/lattice/plugins/<name>/`. The
/// name must be lowercase kebab-case (a valid cargo crate + plugin id).
pub fn scaffold_plugin(name: &str) -> Result<()> {
    validate_plugin_name(name)?;
    let dir = lattice_config::config_home()
        .context("no config home directory on this platform (set $XDG_CONFIG_HOME)")?
        .join("lattice")
        .join("plugins")
        .join(name);
    write_scaffold_plugin(&dir, name)?;

    let d = dir.display();
    let wasm = format!("{}.wasm", name.replace('-', "_"));
    println!("Created a starter plugin `{name}` at {d}\n");
    println!("Next — build it and drop the component in place:\n");
    println!("  rustup target add wasm32-wasip2   # once");
    println!("  cd {d}");
    println!("  cargo build --release --target wasm32-wasip2");
    println!("  cp target/wasm32-wasip2/release/{wasm} {name}.wasm\n");
    println!("Then start lattice — the plugin is discovered from the plugins dir,");
    println!("and `{name}.enabled` (default true) turns its `{name}-mode` on. It");
    println!("binds `gh` (Normal) to a starter action. Edit src/lib.rs to grow it;");
    println!("see docs/user/plugins.md + `:describe-plugin-api <seam>`.");
    Ok(())
}

/// A plugin name must be a valid cargo crate name + WIT-friendly id: lowercase,
/// starting with a letter, only `a-z0-9-`.
fn validate_plugin_name(name: &str) -> Result<()> {
    let ok = name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.ends_with('-');
    if !ok {
        bail!("plugin name `{name}` must be lowercase kebab-case (e.g. `my-plugin`)");
    }
    Ok(())
}

/// Write the init config scaffold into `dir` — the pure filesystem half of
/// [`scaffold_init`], testable against any dir.
fn write_scaffold_init(dir: &Path) -> Result<()> {
    prepare_dir(dir)?;
    write_wit_package(dir)?;
    write(&dir.join("wit").join("init.wit"), INIT_WIT)?;
    write(&dir.join("Cargo.toml"), CARGO_TOML)?;
    write(&dir.join("plugin.toml"), PLUGIN_TOML)?;
    write(&dir.join("src").join("lib.rs"), LIB_RS)?;
    Ok(())
}

/// Write the plugin scaffold for `name` into `dir` — the pure filesystem half of
/// [`scaffold_plugin`], testable against any dir.
fn write_scaffold_plugin(dir: &Path, name: &str) -> Result<()> {
    prepare_dir(dir)?;
    write_wit_package(dir)?;
    let sub = |t: &str| {
        t.replace("__NAME__", name)
            .replace("__MODE__", &format!("{name}-mode"))
            .replace("__ACTION__", &format!("{name}-hello"))
    };
    write(&dir.join("wit").join("plugin.wit"), PLUGIN_WORLD_WIT)?;
    write(&dir.join("Cargo.toml"), &sub(PLUGIN_CARGO_TOML))?;
    write(&dir.join("plugin.toml"), &sub(PLUGIN_MANIFEST))?;
    write(&dir.join("src").join("lib.rs"), &sub(PLUGIN_LIB_RS))?;
    Ok(())
}

/// Create `dir/src` + `dir/wit`, refusing a non-empty existing `dir`.
fn prepare_dir(dir: &Path) -> Result<()> {
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!(
            "{} already exists and is not empty — edit it in place, or remove it first",
            dir.display()
        );
    }
    std::fs::create_dir_all(dir.join("src"))
        .with_context(|| format!("creating {}", dir.display()))?;
    std::fs::create_dir_all(dir.join("wit"))?;
    Ok(())
}

/// Seed `dir/wit/` with the editor's embedded API package.
///
/// A seed, not a sync — see the module docs. The build service refreshes this
/// on every build; what it buys here is a project whose `wit_bindgen::generate!`
/// expands in an IDE before the editor has ever built it.
fn write_wit_package(dir: &Path) -> Result<()> {
    for (name, content) in crate::WIT_FILES {
        std::fs::write(dir.join("wit").join(name), content)
            .with_context(|| format!("writing wit/{name}"))?;
    }
    Ok(())
}

fn write(path: &Path, content: &str) -> Result<()> {
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffolds_a_complete_buildable_layout() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("init");
        write_scaffold_init(&dir).unwrap();

        // The four hand-written files + a non-empty embedded wit package.
        assert!(dir.join("Cargo.toml").exists());
        assert!(dir.join("plugin.toml").exists());
        assert!(dir.join("src/lib.rs").exists());
        assert!(dir.join("wit/init.wit").exists());
        assert!(
            dir.join("wit/types.wit").exists(),
            "the API package is copied"
        );
        assert!(
            !crate::WIT_FILES.is_empty(),
            "wit package embedded at build"
        );

        // The manifest the editor discovers declares the init id.
        let manifest = std::fs::read_to_string(dir.join("plugin.toml")).unwrap();
        assert!(manifest.contains("id = \"init\""));

        // Only one package may carry a doc comment: `init.wit` must NOT lead with
        // one before `package` (the bug that broke the first build).
        let init_wit = std::fs::read_to_string(dir.join("wit/init.wit")).unwrap();
        assert!(
            init_wit.trim_start().starts_with("package "),
            "init.wit starts with `package`, no leading comment"
        );
    }

    #[test]
    fn refuses_to_clobber_a_non_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("init");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep.me"), "existing config").unwrap();
        assert!(
            write_scaffold_init(&dir).is_err(),
            "won't overwrite existing config"
        );
    }

    #[test]
    fn scaffolds_a_plugin_with_name_substituted() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("my-plugin");
        write_scaffold_plugin(&dir, "my-plugin").unwrap();

        assert!(dir.join("wit/plugin.wit").exists());
        assert!(
            dir.join("wit/types.wit").exists(),
            "the API package is copied"
        );

        let manifest = std::fs::read_to_string(dir.join("plugin.toml")).unwrap();
        assert!(manifest.contains("id = \"my-plugin\""));
        assert!(manifest.contains("default_mode = \"my-plugin-mode\""));

        let cargo = std::fs::read_to_string(dir.join("Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"my-plugin\""));

        let lib = std::fs::read_to_string(dir.join("src/lib.rs")).unwrap();
        assert!(lib.contains("\"my-plugin-mode\"") && lib.contains("\"my-plugin-hello\""));
        assert!(!lib.contains("__NAME__"), "all tokens substituted");
    }

    #[test]
    fn rejects_bad_plugin_names() {
        assert!(validate_plugin_name("my-plugin").is_ok());
        assert!(validate_plugin_name("foo2").is_ok());
        for bad in ["My-Plugin", "1foo", "foo_bar", "foo bar", "foo-", ""] {
            assert!(
                validate_plugin_name(bad).is_err(),
                "{bad} should be rejected"
            );
        }
    }
}
