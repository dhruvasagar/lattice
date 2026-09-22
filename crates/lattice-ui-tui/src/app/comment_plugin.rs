//! The shipped `comment` plugin, driven through the real keystroke path.
//!
//! The comment plan's C.2b gate is "`gcc` round-trips in rust / python / lua
//! buffers", and until this file nothing pressed it. The toggle rules are
//! unit-tested as pure functions inside the plugin, and the loader tests pin
//! that the chord is WIRED — but nothing ran key → operator → guest → edit, so
//! a break anywhere in that chain would have shipped green.
//!
//! Two choices here are load-bearing:
//!
//! - **It lives in `lattice-ui-tui`, over `press_chars`.** A host-side
//!   `dispatch_chord` harness cannot compose an operator: `g` `c` `c` would
//!   fire bare, the buffer would not change, and a test asserting "unchanged"
//!   would pass for the wrong reason (`dispatch-chord-cannot-compose-operators`).
//! - **It loads the shipped component with the shipped `plugin.toml`, and
//!   never enables `comment-mode` by hand.** The manifest's `default_modes` is
//!   what turns the mode on in production; a harness that hand-writes the
//!   manifest or flips the mode itself passes against a plugin whose manifest
//!   forgot it (`plugin-minors-are-inert-until-enabled`).
//!
//! Skips when the component has not been built, the convention every
//! real-plugin test in this repo follows. Build it with
//! `cargo xtask build-core-plugins`.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use std::sync::Arc;

    use lattice_config::ConfigRegistry;
    use lattice_plugin_host::{PluginHost, TrustTier};
    use lattice_plugin_loader::{LoaderServices, PluginLoader};

    use crate::app::App;
    use crate::app::test_helpers::*;

    const PLUGIN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/comment");

    fn comment_wasm() -> Option<Vec<u8>> {
        std::fs::read(format!(
            "{PLUGIN_DIR}/target/wasm32-wasip2/release/comment.wasm"
        ))
        .ok()
    }

    fn body(app: &App) -> String {
        app.editor.document.snapshot().buffer.as_string()
    }

    /// An `App` over `file` holding `text`, with the shipped comment plugin
    /// loaded and `comment-mode` active through its own `default_modes`.
    /// `None` when the component is not built.
    async fn app_with_comment(file: &str, text: &str) -> Option<App> {
        let Some(wasm) = comment_wasm() else {
            eprintln!("SKIP: comment plugin not built (cargo xtask build-core-plugins)");
            return None;
        };
        lattice_host::disable_autoload();

        let base = unique_tempdir();
        let plugin_dir = base.join("plugins").join("comment");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::copy(
            format!("{PLUGIN_DIR}/plugin.toml"),
            plugin_dir.join("plugin.toml"),
        )
        .unwrap();
        std::fs::write(plugin_dir.join("component.wasm"), wasm).unwrap();

        let mut app = app_with_path(text, 20, base.join(file));
        let ed = &app.editor;
        let host = Arc::new(
            PluginHost::with_dirs(base.join("cache"), base.join("data")).expect("host builds"),
        );
        let loader = PluginLoader::with_services(
            host,
            LoaderServices {
                runtime: Some(tokio::runtime::Handle::current()),
                bus: Some(ed.event_bus.clone()),
                command_registry: Some(ed.registry.clone()),
                mode_registry: Some(ed.mode_registry.clone()),
                keymap: Some(ed.keymap.clone()),
                config_registry: ed
                    .services
                    .get::<Arc<ConfigRegistry>>()
                    .map(|h| (*h).clone()),
                operator_chords: ed
                    .services
                    .get::<lattice_mode::OperatorChordWirerHandle>()
                    .map(|h| (*h).clone()),
                help_topics: Some(ed.help_topics.clone()),
                ..Default::default()
            },
        );
        let loaded = loader
            .discover_and_load(&base.join("plugins"), TrustTier::Bundled)
            .await;
        assert_eq!(loaded, 1, "the shipped comment plugin loads");
        assert!(
            settle_mode(&mut app, "comment-mode").await,
            "`comment-mode` activates from the manifest's `default_modes`, not by hand"
        );
        Some(app)
    }

    /// `gcc` comments the cursor line at its indent, and a second `gcc`
    /// restores the buffer byte for byte.
    async fn gcc_round_trips(file: &str, leader: &str) {
        let original = "fn f() {\n    body\n}\n";
        let Some(mut app) = app_with_comment(file, original).await else {
            return;
        };
        press_chars(&mut app, "jgcc");
        assert_eq!(
            body(&app),
            format!("fn f() {{\n    {leader} body\n}}\n"),
            "`gcc` in `{file}` comments the line at its indent with `{leader}`"
        );
        press_chars(&mut app, "gcc");
        assert_eq!(
            body(&app),
            original,
            "a second `gcc` in `{file}` restores it"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gcc_round_trips_in_rust() {
        gcc_round_trips("main.rs", "//").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gcc_round_trips_in_python() {
        gcc_round_trips("main.py", "#").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn gcc_round_trips_in_lua() {
        gcc_round_trips("main.lua", "--").await;
    }

    /// `gc` is an operator, not a line chord: `gcj` takes the motion, and the
    /// leader goes at the range's MINIMUM indent so the block stays aligned.
    #[tokio::test(flavor = "multi_thread")]
    async fn gc_takes_a_motion_and_comments_at_the_minimum_indent() {
        let Some(mut app) = app_with_comment("main.rs", "    a\n        b\nc\n").await else {
            return;
        };
        press_chars(&mut app, "gcj");
        assert_eq!(body(&app), "    // a\n    //     b\nc\n");
    }

    /// A file the plugin has no leader for is left alone, and the user is
    /// told why — a silent no-op on `gcc` reads as a dead chord.
    #[tokio::test(flavor = "multi_thread")]
    async fn gcc_without_a_known_leader_changes_nothing_and_says_so() {
        let original = "some notes\n";
        let Some(mut app) = app_with_comment("notes.txt", original).await else {
            return;
        };
        press_chars(&mut app, "gcc");
        assert_eq!(body(&app), original);
        let echoed = app
            .editor
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default();
        assert!(
            echoed.contains("no comment syntax"),
            "the echo names the reason, got {echoed:?}"
        );
    }
}
