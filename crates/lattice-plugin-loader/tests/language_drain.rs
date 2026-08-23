//! LG.3c end-to-end: a language plugin discovered on disk loads, and its
//! language becomes indistinguishable from a bundled one — selected by file
//! extension, parsed and highlighted through the ordinary `Syntax` paths, and
//! reversed on unload.
//!
//! Uses the canonical `language-guest` fixture the plugin-host crate builds to
//! a `wasm32-wasip2` component, whose own build.rs compiles a tree-sitter
//! grammar to wasm and bakes it in. Skips when either was not built.
//!
//! The point of the seam is that a plugin language is INDISTINGUISHABLE from a
//! bundled one, so the assertions go through `Lang::detect_from_path` and
//! `Syntax` rather than any plugin-specific side table. If either needed a
//! special case, the seam would have failed at its purpose.
//!
//! The fixture also declares three languages that must be REJECTED — bad
//! grammar bytes, an uncompilable query, and a squat on a bundled name. Those
//! matter as much as the working one: each must cost only itself, leaving the
//! good language registered and the load successful.

#![allow(clippy::unwrap_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;

use lattice_config::ConfigRegistry;
use lattice_grammar::{CommandRegistry, CommandRegistryHandle};
use lattice_help::topics::HelpTopicRegistryHandle;
use lattice_keymap::KeymapHandle;
use lattice_mode::{ModeRegistry, ModeRegistryHandle};
use lattice_picker::PickerRegistryHandle;
use lattice_picker::source::PickerRegistry;
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{LoaderServices, PluginLoader};
use lattice_runtime::EventBus;
use lattice_syntax::{Lang, Syntax};

fn language_guest_wasm() -> Option<Vec<u8>> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../lattice-plugin-host/tests/fixtures/language-guest/target/wasm32-wasip2/release/language_guest.wasm"
    );
    std::fs::read(path).ok()
}

fn write_plugin_dir(root: &std::path::Path, id: &str, provides: &[&str], wasm: &[u8]) {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let list = provides
        .iter()
        .map(|p| format!("\"{p}\""))
        .collect::<Vec<_>>()
        .join(", ");
    std::fs::write(
        dir.join("plugin.toml"),
        format!("id = \"{id}\"\nprovides = [{list}]\n"),
    )
    .unwrap();
    std::fs::write(dir.join("component.wasm"), wasm).unwrap();
}

/// The fixture registers a FIXED language name (`lg3c-md`) into a registry
/// that is process-global, so two tests loading it concurrently genuinely
/// collide: the second registration is refused as already-registered, and
/// either test's unload withdraws what the other is asserting. That is the
/// registry behaving correctly, not a flake — serialise instead of pretending
/// otherwise. (Tests that register their OWN languages use unique names and
/// need none of this.)
fn fixture_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn loader_with_help(base: &std::path::Path) -> (PluginLoader, HelpTopicRegistryHandle) {
    let help: HelpTopicRegistryHandle = lattice_help::topics::builtin_topics().into_handle();
    (loader_inner(base, Some(help.clone())), help)
}

fn loader(base: &std::path::Path) -> PluginLoader {
    loader_inner(base, None)
}

fn loader_inner(base: &std::path::Path, help: Option<HelpTopicRegistryHandle>) -> PluginLoader {
    let commands: CommandRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(CommandRegistry::new()));
    let pickers: PickerRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(PickerRegistry::new()));
    let modes: ModeRegistryHandle =
        Arc::new(arc_swap::ArcSwap::from_pointee(ModeRegistry::default()));
    let host = Arc::new(
        lattice_plugin_host::PluginHost::with_dirs(base.join("cache"), base.join("data"))
            .expect("host builds"),
    );
    PluginLoader::with_services(
        host,
        LoaderServices {
            runtime: Some(tokio::runtime::Handle::current()),
            bus: Some(Arc::new(EventBus::new())),
            picker_registry: Some(pickers),
            command_registry: Some(commands),
            mode_registry: Some(modes),
            config_registry: Some(Arc::new(ConfigRegistry::default())),
            keymap: Some(KeymapHandle::new()),
            help_topics: help,
            // NOTE: no language handle here, and none is needed — the language
            // registry is process-global, so unlike every other seam this one
            // cannot be left unwired.
            ..Default::default()
        },
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_plugin_language_is_indistinguishable_from_a_bundled_one() {
    let Some(wasm) = language_guest_wasm() else {
        eprintln!("skipping: language-guest wasm not built (no wasm32-wasip2 target)");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    let _guard = fixture_lock().lock().await;
    write_plugin_dir(&plugins_dir, "language-guest", &["language"], &wasm);

    let loader = loader(base.path());
    let n = loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert_eq!(
        n, 1,
        "the language plugin loads even though three of its four languages are rejected"
    );

    // Selected by extension through the ordinary detection path.
    let lang = Lang::detect_from_path(Some(&PathBuf::from("notes.lg3cmd")));
    let Lang::Plugin(name) = lang else {
        panic!("expected the plugin language, got {lang:?}");
    };
    assert_eq!(name.as_str(), "lg3c-md");
    assert_eq!(lang.name(), "lg3c-md");

    // Parses and highlights through the ordinary `Syntax` path. The grammar
    // came from wasm the plugin shipped, and nothing here knows that.
    let mut syntax = Syntax::for_language(lang)
        .expect("registry")
        .expect("a registered plugin language must yield a Syntax");
    syntax.parse("# Title\n\n## Sub\n\ntext\n");
    let lines = syntax.highlight_lines_native(0, 5).expect("highlights");
    assert!(
        lines.iter().any(|l| !l.is_empty()),
        "the plugin's highlights query must produce spans"
    );

    // ── the three rejections ────────────────────────────────────────
    //
    // Each must have cost only itself. That the plugin loaded at all, and
    // that the language above works, is most of the proof; these pin the
    // rest.

    // Bad grammar bytes: not registered.
    assert_eq!(
        Lang::detect_from_path(Some(&PathBuf::from("a.lg3cbad"))),
        Lang::Plain,
        "a language whose grammar is not a wasm module must not register"
    );
    // Uncompilable folds query: not registered. Queries compile at
    // registration precisely so this fails here rather than silently
    // disabling folding later.
    assert_eq!(
        Lang::detect_from_path(Some(&PathBuf::from("a.lg3cbq"))),
        Lang::Plain,
        "a language with an uncompilable query must not register"
    );
    // Squatting a bundled name: refused, and the bundled language untouched.
    assert_eq!(
        Lang::detect_from_path(Some(&PathBuf::from("a.lg3csquat"))),
        Lang::Plain,
        "a language claiming a bundled name must not register"
    );
    assert_eq!(
        Lang::detect_from_path(Some(&PathBuf::from("README.md"))),
        Lang::Markdown,
        "the bundled markdown must be untouched by the squat attempt"
    );
    assert!(
        Syntax::for_language(Lang::Markdown).unwrap().is_some(),
        "bundled markdown must still have its grammar"
    );

    // ── unload ──────────────────────────────────────────────────────
    let report = loader.unload("language-guest").expect("unload succeeds");
    assert_eq!(
        report.languages, 1,
        "the one registered language is withdrawn"
    );

    assert_eq!(
        Lang::detect_from_path(Some(&PathBuf::from("notes.lg3cmd"))),
        Lang::Plain,
        "after unload the extension falls back to plain"
    );
    // A buffer still holding the old `Lang` finds no grammar and renders as
    // plain text — no dangle, and no kind-branch to express it.
    assert!(
        Syntax::for_language(lang).unwrap().is_none(),
        "the grammar must be withdrawn with the language"
    );
}

/// LG.6: one component, two seams, from a world the plugin composed ITSELF.
///
/// A component implements exactly one WIT world, so a plugin providing both
/// `language` and `help` needs a combined world — and bundled plugins get
/// theirs written into lattice's own `wit/` (`auto-pair-plugin` imports six
/// interfaces). An EXTERNAL plugin cannot add a world to someone else's
/// package, which is why org's `:help` page was deferred at LG.6.
///
/// It turns out nothing in lattice needs to change: WIT `include` composes
/// worlds, and wit-bindgen resolves an `inline` package against the
/// interfaces at `path`. The fixture declares its world inline for exactly
/// this reason — a fixture using a lattice-side world would prove the thing
/// external plugins *cannot* do.
///
/// This asserts both drains ran off the one component, which is the part that
/// would silently regress: a composed world that lost an export would still
/// load, just quietly missing half the plugin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_component_can_provide_language_and_help() {
    let Some(wasm) = language_guest_wasm() else {
        eprintln!("skipping: language-guest wasm not built");
        return;
    };

    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");
    let _guard = fixture_lock().lock().await;
    write_plugin_dir(&plugins_dir, "language-guest", &["language", "help"], &wasm);

    let (loader, help) = loader_with_help(base.path());
    assert_eq!(
        loader
            .discover_and_load(&plugins_dir, TrustTier::Bundled)
            .await,
        1,
        "one component, both seams"
    );

    // The language seam ran.
    assert!(
        matches!(
            Lang::detect_from_path(Some(&PathBuf::from("a.lg3cmd"))),
            Lang::Plugin(_)
        ),
        "the language seam must have drained"
    );
    // And so did the help seam, off the same component. An empty topic name
    // lands at the bare plugin id.
    assert!(
        help.load().lookup("language-guest").is_some(),
        "the help seam must have drained from the same component"
    );

    // Teardown of the FIRST seam works; the second seam's contributions do
    // not come back out. That is a real pre-existing bug, not a property of
    // world composition, and it is pinned by the `#[ignore]`d test below
    // rather than asserted away here.
    let report = loader.unload("language-guest").expect("unloads");
    assert_eq!(report.languages, 1, "the first seam reverses");
}

/// **Known-failing: a multi-seam plugin only reverses its FIRST seam.**
///
/// Every `spawn_*` calls `PluginHost::alloc_id`, so a plugin providing N
/// seams gets N host ids — one per wasm instance. The loader keeps only the
/// first (`loaded_id.get_or_insert(id)`) and teardown reverses by that one.
/// Token-based reversals (modes, config options, picker sources, keymap
/// bindings) are unaffected because the drains capture their tokens on the
/// record. **Provenance-keyed ones are not**: help topics, dashboard sections
/// and languages are removed with `unregister_plugin(record.id)`, so anything
/// registered by a later seam stays behind after `:plugin-unload` reports
/// success.
///
/// This is not hypothetical and not new: the bundled `auto-pair` provides
/// `grammar`, `modes`, `config` and `help` in that order, so its `:help`
/// pages leak on unload today. Nothing covered it because the existing
/// `help` teardown test loads a help-ONLY plugin, and `auto_pair.rs` wires a
/// help registry but never unloads.
///
/// Ignored rather than deleted so the gap is runnable
/// (`cargo test -p lattice-plugin-loader -- --ignored`) and impossible to
/// forget. Fixing it means deciding whether a plugin is one identity or many
/// — see the LG.6 note in the slice plan.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "known bug: multi-seam teardown reverses only the first seam"]
async fn a_multi_seam_plugin_reverses_every_seam() {
    let Some(wasm) = language_guest_wasm() else {
        return;
    };
    let base = tempfile::tempdir().unwrap();
    let plugins_dir = base.path().join("plugins");

    let _guard = fixture_lock().lock().await;
    write_plugin_dir(&plugins_dir, "language-guest", &["language", "help"], &wasm);
    let (loader, help) = loader_with_help(base.path());
    loader
        .discover_and_load(&plugins_dir, TrustTier::Bundled)
        .await;
    assert!(help.load().lookup("language-guest").is_some());

    let report = loader.unload("language-guest").expect("unloads");
    assert_eq!(report.languages, 1, "first seam — this part works");
    assert_eq!(
        report.help_topics, 1,
        "the second seam's topics must come back out too"
    );
    assert!(help.load().lookup("language-guest").is_none());
}
