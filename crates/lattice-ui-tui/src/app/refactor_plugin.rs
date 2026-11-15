//! The shipped `refactor` plugin, driven through the real keystroke path
//! (RF.2b).
//!
//! Same two load-bearing choices as `comment_plugin.rs`, for the same
//! reasons: it presses keys through `App::apply` (the path that composes
//! counts and modes), and it loads the shipped component and never enables
//! `refactor-mode` by hand, so a manifest that forgot `default_modes`
//! fails here rather than in a user's editor.
//!
//! Skips when the component has not been built — `lattice-plugin-host`'s
//! build script builds it, as does `cargo xtask build-core-plugins`.

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use std::sync::Arc;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use lattice_config::ConfigRegistry;
    use lattice_grammar::modal::ModalState;
    use lattice_plugin_host::{PluginHost, TrustTier};
    use lattice_plugin_loader::{LoaderServices, PluginLoader};

    use crate::app::App;
    use crate::app::test_helpers::*;

    const PLUGIN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../plugins/refactor");

    fn refactor_wasm() -> Option<Vec<u8>> {
        std::fs::read(format!(
            "{PLUGIN_DIR}/target/wasm32-wasip2/release/refactor.wasm"
        ))
        .ok()
    }

    /// An `App` over `file` holding `text`, parsed as `lang` (or not at all),
    /// with the shipped plugin loaded and `refactor-mode` active through its
    /// own `default_modes`. `None` when the component is not built.
    async fn app_with_refactor(
        file: &str,
        text: &str,
        lang: Option<lattice_syntax::Lang>,
    ) -> Option<App> {
        let Some(wasm) = refactor_wasm() else {
            eprintln!("SKIP: refactor plugin not built (cargo xtask build-core-plugins)");
            return None;
        };
        lattice_host::disable_autoload();

        let base = unique_tempdir();
        let plugin_dir = base.join("plugins").join("refactor");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("component.wasm"), wasm).unwrap();

        let mut app = app_with_path(text, 20, base.join(file));
        if let Some(lang) = lang {
            attach_test_syntax(&mut app, lang);
        }
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
                help_topics: Some(ed.help_topics.clone()),
                ..Default::default()
            },
        );
        let loaded = loader
            .discover_and_load(&base.join("plugins"), TrustTier::Bundled)
            .await;
        assert_eq!(loaded, 1, "the shipped refactor plugin loads");
        assert!(
            settle_mode(&mut app, "refactor-mode").await,
            "`refactor-mode` activates from the manifest's `default_modes`, not by hand"
        );
        Some(app)
    }

    fn alt(app: &mut App, c: char) {
        press(app, KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT));
    }

    /// The text a Visual selection covers, head character included — or
    /// `None` outside Visual.
    fn selected(app: &App) -> Option<String> {
        if !matches!(app.editor.modal, ModalState::Visual(_)) {
            return None;
        }
        let anchor = app.editor.visual_anchor?;
        let head = app.editor.cursor;
        let (start, end) = if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        };
        let text = app.editor.document.snapshot().buffer.as_string();
        let offset = |line: u32, byte: u32| -> usize {
            text.split_inclusive('\n')
                .take(line as usize)
                .map(str::len)
                .sum::<usize>()
                + byte as usize
        };
        let from = offset(start.line, start.byte);
        let to = offset(end.line, end.byte);
        let last = text[to..].chars().next().map_or(0, char::len_utf8);
        Some(text[from..to + last].to_string())
    }

    fn echoed(app: &App) -> String {
        app.editor
            .last_message
            .as_ref()
            .map(|m| m.text.clone())
            .unwrap_or_default()
    }

    const RUST: &str = "fn main() {\n    let x = foo(bar, baz + 1);\n}\n";

    /// `<M-o>` walks outward a named node at a time, `<M-i>` walks back the
    /// same way, and one more `<M-i>` past the first selection returns to
    /// the cursor in Normal. Nothing about it names a Rust node kind.
    #[tokio::test(flavor = "multi_thread")]
    async fn expand_walks_out_and_shrink_walks_back() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz");
        let cursor = app.editor.cursor;

        alt(&mut app, 'o');
        assert_eq!(
            selected(&app).as_deref(),
            Some("baz"),
            "the node at the cursor"
        );
        alt(&mut app, 'o');
        assert_eq!(selected(&app).as_deref(), Some("baz + 1"));
        alt(&mut app, 'o');
        assert_eq!(selected(&app).as_deref(), Some("(bar, baz + 1)"));

        alt(&mut app, 'i');
        assert_eq!(selected(&app).as_deref(), Some("baz + 1"));
        alt(&mut app, 'i');
        assert_eq!(selected(&app).as_deref(), Some("baz"));
        alt(&mut app, 'i');
        assert_eq!(
            app.editor.modal,
            ModalState::Normal,
            "past the first: Normal"
        );
        assert_eq!(app.editor.cursor, cursor, "and the cursor it started from");
        assert_eq!(app.editor.document.snapshot().buffer.as_string(), RUST);
    }

    /// A count grows that many levels, and each is a step back.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_count_expands_that_many_levels() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz3");
        alt(&mut app, 'o');
        assert_eq!(selected(&app).as_deref(), Some("(bar, baz + 1)"));
        alt(&mut app, 'i');
        assert_eq!(selected(&app).as_deref(), Some("baz + 1"));
    }

    /// Changing the selection any other way forgets the way back: `<M-i>`
    /// says so and leaves the selection alone, rather than shrinking to a
    /// place the user did not come from.
    #[tokio::test(flavor = "multi_thread")]
    async fn shrink_after_moving_the_selection_says_so_and_changes_nothing() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        press_chars(&mut app, "jfz");
        alt(&mut app, 'o');
        alt(&mut app, 'o');
        press_chars(&mut app, "h");
        let before = selected(&app);
        alt(&mut app, 'i');
        assert_eq!(selected(&app), before, "the selection is untouched");
        assert!(
            echoed(&app).contains("nothing to shrink back to"),
            "and the user is told why, got {:?}",
            echoed(&app)
        );
    }

    /// `<M-i>` in Normal says there is nothing to shrink. Unbound there, the
    /// Normal lookup's modifier fallback would read it as `i` and drop the
    /// user into Insert — the opposite of what they reached for.
    #[tokio::test(flavor = "multi_thread")]
    async fn shrink_in_normal_says_so_and_does_not_insert() {
        let Some(mut app) =
            app_with_refactor("main.rs", RUST, Some(lattice_syntax::Lang::Rust)).await
        else {
            return;
        };
        alt(&mut app, 'i');
        assert_eq!(app.editor.modal, ModalState::Normal);
        assert!(
            echoed(&app).contains("nothing to shrink"),
            "got {:?}",
            echoed(&app)
        );
    }

    /// The rule is language-agnostic: the same keys walk a Python call.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_keys_walk_python() {
        let src = "x = f(a, b)\n";
        let Some(mut app) =
            app_with_refactor("main.py", src, Some(lattice_syntax::Lang::Python)).await
        else {
            return;
        };
        press_chars(&mut app, "fb");
        alt(&mut app, 'o');
        assert_eq!(selected(&app).as_deref(), Some("b"));
        alt(&mut app, 'o');
        assert_eq!(selected(&app).as_deref(), Some("(a, b)"));
    }

    /// With no syntax tree the key says so instead of doing nothing, and
    /// leaves the buffer in Normal.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_syntax_tree_expand_says_so() {
        let Some(mut app) = app_with_refactor("notes.txt", "some notes\n", None).await else {
            return;
        };
        alt(&mut app, 'o');
        assert_eq!(app.editor.modal, ModalState::Normal);
        assert!(
            echoed(&app).contains("no syntax tree"),
            "got {:?}",
            echoed(&app)
        );
    }
}
